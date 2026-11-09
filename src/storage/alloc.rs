use std::fs::*;
use std::io::{
    Read,
    Write,
    Seek,
    SeekFrom,
    Result,
    Error,
    ErrorKind
};

use crate::storage::superblock::*;
use crate::storage::inode::*;
use crate::storage::extent::*;
use crate::storage::lst::*;
use crate::storage::segment::*;

use std::mem::size_of;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct Alloc{
    storage_dev: File,
    sup_block: SuperBlock,
    lst: Vec<LstEntry>,
    seg_tree: SegTree
}

impl Alloc {

    pub fn new(dev_name: &str, enable_trunc: bool) -> Self {

        let open = File::options()
            .read(true)
            .write(true)
            .truncate(enable_trunc)
            .open(dev_name);

        if open.is_err() {
            panic!("something went wrong when the device was opened: {}", open.err().unwrap());
        }

        let sup_block = SuperBlock {
            magic: [0u8;5],
            generation: 0,
            nr_inodes: 0,
            root_id: 0,
            block_size: 0,

            seg_size: 0,
            free_loc: 0,
            nr_segments: 0,

            chunk_size: 0,
            sgn_table_loc: 0,
            sgn_table_size: 0,
            sgn_id: 0
        };


        Self {
            storage_dev: open.unwrap(),
            sup_block: sup_block,
            lst: Vec::new(),
            seg_tree: SegTree::new()
        }
    }

    pub fn mount(dev_name: &str) -> Result<Self> {

        let mut alloc_ret: Self = Self::new(dev_name, false);
        let sup_block: SuperBlock = alloc_ret.read_superblock()?;

        if !sup_block.check_magic() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Filesystem magic number did not match"));
        }

        alloc_ret._load_lst()?;
        alloc_ret.sup_block = sup_block;
        Ok(alloc_ret)
    }

    fn _go_to(&mut self, next_pos: u64) -> Result<()> {
        let curr_pos: i64 = self.storage_dev.stream_position()? as i64;
        let i64_next_pos: i64 = next_pos as i64;
        let final_pos = i64_next_pos - curr_pos;

        self.storage_dev.seek_relative(final_pos)
    }

    fn _direct_write<T: Serialize>(&mut self, obj: &T, pos: u64) -> Result<()> {

        self._go_to(pos as u64)?;
        let encoded_obj = bincode::serialize(obj).map_err(Error::other)?;

        self.storage_dev.write_all(&encoded_obj)?;
        Ok(())

    }

    fn _direct_read<T: DeserializeOwned>(&mut self, loc: u64, size: usize) -> Result<T> {
        let disk_move = self._go_to(loc)?;

        let mut read_buff: Vec<u8> = vec![0u8; size];

        let res_read = self.storage_dev.read_exact(&mut read_buff)?;
        let read_slice = &read_buff.as_slice();

        bincode::deserialize(read_slice).map_err(Error::other)
    }

    pub fn save_superblock(&mut self, superblock: &SuperBlock) -> Result<()> {
        self._direct_write::<SuperBlock>(superblock, 0)
    }

    fn _update_superblock(&mut self) -> Result<()>{
        self._go_to(0)?;
        let encoded_supblock = bincode::serialize(&self.sup_block).map_err(Error::other)?;

        self.storage_dev.write_all(&encoded_supblock)?;
        Ok(())
    }

    pub fn read_extent(&mut self, extent: &Extent) -> Result<Vec<u8>> {
        let disk_move = self._go_to(extent.phys_block_offset as u64)?;
        
        let data_size = (extent.length*self.sup_block.block_size as u32) as usize;
        let mut read_buff: Vec<u8> = vec![0u8; data_size];
        let res_read = self.storage_dev.read_exact(&mut read_buff)?;

        bincode::deserialize(&read_buff.as_slice()).map_err(Error::other)
    }

    pub fn read_superblock(&mut self) -> Result<SuperBlock> {
        self._direct_read::<SuperBlock>(0, size_of::<SuperBlock>())
    }

    pub fn format_disk(&mut self) -> Result<()> {

        let local_block_size: u64 = 4096; // bytes (4 Kib)
        let local_chunk_size: u64 = local_block_size << 4; // 64 Kib
        let local_seg_size: u64 = local_block_size << 18; // 1 Gib 

        let sup_block = SuperBlock {
            magic: *MAGIC,
            generation: 0,
            nr_inodes: 0,
            nr_segments: 0,
            block_size: local_block_size,
            seg_size: local_seg_size,

            root_id: 0,
            free_loc: 2*local_seg_size,

            chunk_size: local_chunk_size,
            sgn_table_loc: local_seg_size,
            sgn_table_size: local_seg_size,

            sgn_id: 0
        };

        self.save_superblock(&sup_block)?;
        self.sup_block = sup_block;
        Ok(())
    }

    fn _load_lst(&mut self) -> Result<()> {

        let seg_size: u64 = self.sup_block.seg_size;
        let lst_pos: u64 = if self.sup_block.generation & 0x1 == 0x1 { seg_size >> 1 } else { seg_size }; 

        self.lst = self._direct_read::<Vec<LstEntry>>(lst_pos, (self.sup_block.nr_segments as usize)*size_of::<LstEntry>())?;

        Ok(())

    }

    fn _commit_lst(&mut self) -> Result<()> {

        let seg_size: u64 = self.sup_block.seg_size;
        let lst_pos: u64 = if self.sup_block.generation & 0x1 == 0x1 { seg_size >> 1 } else { seg_size }; 
        let lst_ref = self.lst.clone();
        self._direct_write::<Vec<LstEntry>>(&lst_ref, lst_pos)?;
        
        Ok(())
    }

    fn _get_sgn_id(&self, id: u128) -> u64 {
        let mask: u128 = 0xFFFFFFFF;
        (id >> 64 & mask) as u64
    }

    fn _get_obj_pos(&mut self, id: u128) -> Result<u64>{
        todo!("Not implemented yet");
    }

    fn _update_sgn_pos(&mut self, sgn_id: u64, new_loc: u64) -> Result<()> {
        self.lst[sgn_id as usize].phys_location = new_loc;
        self.sup_block.generation += 1;

        if self.sup_block.generation % 10 == 0 {
            return self._commit_lst();
        }

        Ok(())
    }

    fn _update_obj_pos(&mut self, obj_id: u128, new_loc: u64) -> Result<()> {
        todo!("Not implemented yet");
    }

    fn _register_sgn(&mut self) -> Result<u64> {

        self.lst.push(LstEntry::new());
        self.sup_block.nr_segments += 1;
        let curr_sgn_id: u64 = self.lst.len() as u64; 

        if self.sup_block.generation % 10 == 0{
    
            self._commit_lst()?;
            self._update_superblock()?;
        }

        Ok(curr_sgn_id)

    }

    pub fn read_obj<T: DeserializeOwned>(&mut self, id: u128) -> Result<T> {
        let obj_pos: u64 = self._get_obj_pos(id)?;
        self._direct_read::<T>(obj_pos, size_of::<T>())
    }

    pub fn register_inode(&mut self, obj: &Inode) -> Result<u64> {
        Ok(0)
    }

    pub fn commit<T: Serialize>(&mut self, obj: &T, obj_id: u128) -> Result<()> {
        let new_obj_pos: u64 = self.sup_block.free_loc;
        self._direct_write::<T>(obj, self.sup_block.free_loc)?;
        self.sup_block.free_loc += size_of::<T>() as u64;
        self.sup_block.generation += 1;
        self._update_superblock()?;
        self._update_obj_pos(obj_id, new_obj_pos)
    }

    pub fn read_root(&mut self) -> Result<Inode> { self.read_obj::<Inode>(0) }
    pub fn commit_root(&mut self, root: &Inode) -> Result<()> { self.commit::<Inode>(&root,0) }

    pub fn file_seek<'a>(&self, inode: &'a Inode, pos: u64) -> ExtentType<'a> {

        if pos < INODE_PAYLOAD { return ExtentType::Direct; }

        let extent_opt = inode.seek(pos, self.sup_block.block_size);
        let extent_type = match extent_opt {
            Some(extent) => { ExtentType::Indirect(extent) },
            None => { ExtentType::Invalid }
        };

        extent_type
    }

}
