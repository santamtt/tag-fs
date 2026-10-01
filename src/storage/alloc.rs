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

use crate::storage::SuperBlock;

use std::mem::size_of;
use serde::{ Serialize, Deserialize };
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct Alloc{
    storage_dev: File
}

impl Alloc {

    pub fn new(dev_name: &str, enable_trunc: bool) -> Self {

        let open = File::options()
            .read(true)
            .write(true)
            .truncate(enable_trunc)
            .open(dev_name);

        if open.is_err() {
            panic!("something went wrong: {}", open.err().unwrap());
        }

        Self {
            storage_dev: open.unwrap()
        }
    }

    fn _go_to(&mut self, next_pos: u64) -> Result<()> {

        let curr_pos: i64 = self.storage_dev.stream_position()? as i64;
        let i64_next_pos: i64 = next_pos as i64;
        let final_pos = i64_next_pos - curr_pos;

        self.storage_dev.seek_relative(final_pos)
    }

    pub fn direct_write<T: Serialize>(&mut self, obj: &T, pos: u64) -> Result<()> {

        self._go_to(pos as u64)?;

        let res = bincode::serialize(obj).map_err(Error::other);
        if res.is_err() { return Err(res.err().unwrap()); }

        self.storage_dev.write_all(&res.unwrap())?;
        Ok(())

    }

    fn direct_read<T: DeserializeOwned>(&mut self) -> Result<T> {

        let size = size_of::<T>();
        let mut read_buff: Vec<u8> = vec![0u8; size];

        let res_read = self.storage_dev.read_exact(&mut read_buff);
        if res_read.is_err() { return Err(res_read.err().unwrap()); }
        let read_slice = &read_buff.as_slice();

        bincode::deserialize(read_slice).map_err(Error::other)
    }

    pub fn direct_read_from_pos<T: DeserializeOwned>(&mut self, loc: u64) -> Result<T> {
        let disk_move = self._go_to(loc);
        self.direct_read()
    }
   
    pub fn save_superblock(&mut self, superblock: &SuperBlock) -> Result<()> {
        self.direct_write::<SuperBlock>(superblock, 0)
    }

    pub fn read_superblock(&mut self) -> Result<SuperBlock> {
        self._go_to(0)?;

        let mut buff: Vec<u8> = vec![0u8; size_of::<SuperBlock>()];
        let res = self.storage_dev.read_exact(&mut buff);
        if res.is_err() { return Err(res.err().unwrap()); }

        bincode::deserialize(&buff).map_err(Error::other)
    }



}
