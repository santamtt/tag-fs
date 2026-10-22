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

use std::mem::size_of;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct Alloc{
    storage_dev: File,
    sup_block: SuperBlock
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
            timestamp: 0,
            nr_inodes: 0,
            nr_segments: 0,
            root_id: 0,
            block_size: 0,

            seg_size: 0,
            free_loc: 0,

            chunk_size: 0,
            obj_table: 0,
            obj_id: 0
        };


        Self {
            storage_dev: open.unwrap(),
            sup_block: sup_block
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

    fn _direct_read<T: DeserializeOwned>(&mut self, loc: u64) -> Result<T> {
        let disk_move = self._go_to(loc)?;

        let size = size_of::<T>();
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
        self._direct_read::<SuperBlock>(0)
    }

    pub fn format_disk(&mut self) -> Result<()> {

        let local_block_size: u64 = 4096; // bytes (4 kb)
        let local_chunk_size: u64 = 16*local_block_size;
        let local_seg_size: u64 = 512*local_block_size; // 2 Mb

        let sup_block = SuperBlock {
            magic: *MAGIC,
            timestamp: 0,
            nr_inodes: 0,
            nr_segments: 1,
            block_size: local_block_size,
            seg_size: local_seg_size,

            root_id: 0,
            free_loc: 2*local_seg_size,

            chunk_size: local_chunk_size,
            obj_table: local_seg_size,

            obj_id: 0
        };

        self.save_superblock(&sup_block)?;
        self.sup_block = sup_block;
        Ok(())
    }

    fn _access_obj_table(&mut self, obj_id: u64) -> Result<()> {
        let obj_reg: u64 = obj_id + self.sup_block.obj_table;

        if obj_id >= self.sup_block.seg_size {
            let err_msg: &str =
                "in function _access_obj_table: cannot allocate more objects in the device: object table is full";

            return Err(Error::new(
                    ErrorKind::StorageFull, err_msg));
        }

        self._go_to(obj_reg)
    }

    fn _get_obj_pos(&mut self, id: u64) -> Result<u64>{
        self._access_obj_table(id)?;
        let mut addr_read: Vec<u8> = vec![0u8;8];
        self.storage_dev.read_exact(&mut addr_read)?;

        Ok(u64::from_ne_bytes(addr_read[0..=7].try_into().unwrap()))
    }

    fn _update_obj_pos(&mut self, obj_id: u64, new_loc: u64) -> Result<()> {
        self._access_obj_table(obj_id)?;
        self.storage_dev.write_all(&new_loc.to_ne_bytes())
    }

    pub fn read_obj<T: DeserializeOwned>(&mut self, id: u64) -> Result<T> {
        let obj_pos: u64 = self._get_obj_pos(id)?;
        self._direct_read::<T>(obj_pos)
    }

    pub fn register_obj<T: Serialize>(&mut self, obj: &T) -> Result<u64> {

        let curr_obj_id: u64 = self.sup_block.obj_id;
        self._access_obj_table(curr_obj_id)?;

        self.storage_dev.write_all(&self.sup_block.free_loc.to_ne_bytes())?;

        self._direct_write::<T>(obj, self.sup_block.free_loc)?;

        self.sup_block.obj_id += 1;
        self.sup_block.free_loc += size_of::<T>() as u64;

        self._update_superblock()?;
        Ok(curr_obj_id)

    }

    pub fn commit<T: Serialize>(&mut self, obj: &T, obj_id: u64) -> Result<()> {
        let new_obj_pos: u64 = self.sup_block.free_loc;
        self._direct_write::<T>(obj, self.sup_block.free_loc)?;
        self.sup_block.free_loc += size_of::<T>() as u64;
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
