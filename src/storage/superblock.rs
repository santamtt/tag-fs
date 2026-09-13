pub use std::fs::*;
pub use std::io::{
    Read, 
    Write,
    Seek, 
    SeekFrom,
    Result,
    Error,
    ErrorKind
};
pub use std::mem::size_of;

pub use serde::{ Serialize, Deserialize };

use pub_fields::pub_fields;

pub const BOOT_SECTOR_SIZE:usize = 1024; // Kb
pub const MAGIC: &[u8; 5] = b"TAGFS";
pub const BLOCK_SIZE: usize = 4096; // Kb
pub const INODE_TABLE_SIZE: usize = 2*(1 << 30); // Gb

const BLOCK_GROUP_SIZE: usize = 128*(1 << 20); // Mb
const BLOCKS_PER_GROUP: usize = BLOCK_GROUP_SIZE/BLOCK_SIZE;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[pub_fields]
pub struct SuperBlock {
   magic: [u8; 5],
   nr_inodes: u32,
   nr_blocks: u32,
   nr_tags: u32,
   root_pos: u32,
   free_i_bitmap: u64,
   free_i_loc: u64,
   free_d_loc: u64
}

impl SuperBlock {
    pub fn save(&self, fd: &mut File) -> Result<()> {

        fd.seek(SeekFrom::Start(1024))?; 
        let space_left: usize = BLOCK_SIZE - BOOT_SECTOR_SIZE; 

        let res_encoded = bincode::serialize(self).map_err(|e| {
            Error::new(ErrorKind::InvalidData,e);
        });

        if res_encoded.is_err(){
            return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Superblock cannot be serialized correctly"
            ));
        }

        let encoded: Vec<u8> = res_encoded.unwrap();


        if encoded.len() > space_left {
            return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Superblock exceeds block size"
            ));
        }

        let mut block_buffer = vec![0u8; space_left];
        block_buffer[..encoded.len()].copy_from_slice(&encoded);
        fd.write_all(&block_buffer)?;
        fd.flush()?;

        Ok(())
    }

    pub fn read(fd: &mut File) -> Result<Self> {
        let sup_size: usize = BLOCK_SIZE - BOOT_SECTOR_SIZE;

        let mut tmp_buff: Vec<u8> =  vec![0u8; sup_size];
        fd.read_exact(&mut tmp_buff)?;

        bincode::deserialize(&tmp_buff).map_err(Error::other)
    }
}
