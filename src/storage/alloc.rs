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

    pub fn write_into_dev(&mut self, buff: &Vec<u8>, pos: usize, size_in: usize) -> Result<()> {

        if buff.len() > size_in {
            return Err(
                Error::new(ErrorKind::OutOfMemory, "write size overflow"));
        }

        if buff.len() == 0 {
            return Err(
                Error::new(ErrorKind::OutOfMemory, "attempt to write an empty buffer"));
        }

        self._go_to(pos as u64)?;
        self.storage_dev.write_all(buff)?;
        Ok(())

    }

    fn read_obj(&mut self, size: usize) -> Result<Vec<u8>> {

        let mut read_buff: Vec<u8> = vec![0u8; size];

        let res_read = self.storage_dev.read_exact(&mut read_buff);
        if res_read.is_err() { return Err(res_read.err().unwrap()); }

        Ok(read_buff)
    }

    pub fn read_obj_from_pos(&mut self, loc: u64, size: usize) -> Result<Vec<u8>> {
        let disk_move = self._go_to(loc);
        if disk_move.is_err() { return Err(disk_move.err().unwrap()); }
        self.read_obj(size)
    }
   
    pub fn save_superblock(&mut self, superblock: &Vec<u8>, size_in: usize) -> Result<()> {
        self.write_into_dev(superblock, 0, size_in)
    }

    pub fn read_superblock(&mut self, size_in: usize) -> Result<Vec<u8>> {
        self._go_to(0)?;

        let mut buff: Vec<u8> = vec![0u8; size_in];
        let res = self.storage_dev.read_exact(&mut buff);
        if res.is_err() { return Err(res.err().unwrap()); }

        Ok(buff)
    }

}
