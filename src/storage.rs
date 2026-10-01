mod superblock;
mod inode;
mod node;
mod alloc; 

use crate::storage::superblock::*;
use crate::storage::inode::*;
use crate::storage::node::*;
use crate::storage::alloc::*;

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
use std::mem::size_of;

#[derive(Debug)]
pub struct TagFs {
    pub sup_block: SuperBlock,
    alloc: Alloc
}

impl TagFs {

    fn check_magic(magic_in: &[u8;5]) -> bool { magic_in == MAGIC }

    fn _alloc_inode(&mut self, i_type: InodeType) -> Result<Inode> {
        let new_inode = Inode::new(i_type, 0);
        Ok(new_inode)
    }

    fn _alloc_root(sup_block: &SuperBlock, alloc: &mut Alloc) -> Result<()> {
        let mut root_inode = Inode::new(InodeType::File, 0);
        alloc.direct_write::<Inode>(&root_inode, sup_block.root_pos)
    }

    pub fn _read_root(&mut self) -> Result<()> {
        let disk_inode = self.alloc.direct_read_from_pos::<Inode>(self.sup_block.root_pos);
        if disk_inode.is_err() { return Err(disk_inode.err().unwrap()); }
        let res_inode = disk_inode.unwrap();
        Ok(())
    }

    pub fn write_file(&mut self, bytes_in: Vec<u8>) -> Result<()> {

        let mut res_inode = self._alloc_inode(InodeType::File);

        match res_inode {
            Ok(mut inode) => {
                inode.write(&bytes_in);
                return Ok(());
            },
            Err(e) => {
                return Err(e);
            }
        }
    }

    pub fn mount(dev_name: &str) -> Result<Self> {

        let mut alloc = Alloc::new(dev_name, false);

        let res_sup_block = alloc.read_superblock();
        if res_sup_block.is_err() { return Err(res_sup_block.err().unwrap()); }
        let sup_block = res_sup_block.unwrap();

        if !Self::check_magic(&sup_block.magic) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Filesystem magic number did not match"));
        }

        let res_ok = Self {
            sup_block: sup_block,
            alloc: alloc 
        };
        
        Ok(res_ok)
    }

    pub fn format(dev_name: &str) -> Result<()> {

        let mut alloc = Alloc::new(dev_name, true);

        let local_block_size: u64 = 4096; // bytes (4 kb)
        let local_chunk_size: u64 = 16*local_block_size as u64;
        let local_seg_size: u64 = 512*local_block_size; // 2 Mb

        let sup_block = SuperBlock {
            magic: *MAGIC,
            timestamp: 0,
            nr_inodes: 0,
            nr_segments: 2,
            root_pos: local_seg_size as u64 + local_block_size,
            block_size: local_block_size,

            seg_size: local_seg_size,
            free_loc: local_seg_size,

            chunk_size: local_chunk_size,
        };

        // write superblock
        alloc.save_superblock(&sup_block)?;
        Self::_alloc_root(&sup_block, &mut alloc)?;

        Ok(())
    }
}
