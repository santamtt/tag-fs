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
use serde::{ Serialize, Deserialize };
use pub_fields::pub_fields;

pub const MAGIC: &[u8; 5] = b"TAGFS";

#[derive(Debug, Serialize, Deserialize)]
#[pub_fields]
#[repr(C,align(4096))]
pub struct SuperBlock {
   magic: [u8; 5],
   timestamp: u64,
   nr_inodes: u32,
   nr_segments: u32,
   root_pos: u64,
   block_size: u64,

   seg_size: u64,
   free_loc: u64,
   chunk_size: u64,
}

impl SuperBlock {

    pub fn save(&self, fd: &mut File) -> Result<()> {

        fd.seek(SeekFrom::Start(0))?; 
        let space_left: usize = self.block_size as usize; 

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

    pub fn serialize(&self) -> Result<Vec<u8>> {

        let res_encoded = bincode::serialize(self).map_err(Error::other);
        res_encoded
    }

    pub fn read(buff_in: &Vec<u8>) -> Result<Self> {
        bincode::deserialize(&buff_in).map_err(Error::other)
    }

}
