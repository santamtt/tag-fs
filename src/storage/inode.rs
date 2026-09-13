use std::io::{
    Result,
    Error,
    ErrorKind
};

use pub_fields::pub_fields;
use serde::{ Serialize, Deserialize };

// See Andrew S. Tanembaum "Modern Operating System" Textbook
#[pub_fields]
#[derive(Serialize, Deserialize)]
pub struct Inode {
    i_mode: u16,
    i_links_count: u16, // nr directory entries pointing to this inode
    i_uid: u16,
    i_gid: u16,
    i_size_lo: u32,
    i_block: Vec<u8>, // address of the data blocks
    i_atime: u32,
    i_mtime: u32,
    i_ctime: u32,
    i_dtime: u32,
    i_blocks_lo: u32,
    i_flags: u32,
    i_osd1: u32,
    i_generation: u32,
    i_file_acl_lo: u32,
    i_size_high: u32,
    i_extra_size: u16,
    i_checksum_hi: u16,
    i_ctime_extra: u32,
    i_mtime_extra: u32,
    i_atime_extra: u32,
    i_crtime: u32,
    i_crtime_extra: u32,
    i_version_hi: u32,
    i_projid: u32
}

impl Inode{
    
    pub fn check_i_block_size(&self) -> Result<()>{
        if self.i_block.len() != 60 {
            return Err(Error::new(
                    ErrorKind::OutOfMemory,
                    "inode i_block size is not equal to 60"));
        }

        Ok(())
    }

}
