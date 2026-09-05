extern crate libc;
extern crate serde;

use std::env;
use std::fs::*;
use std::io::{
    Read, 
    Seek, 
    Write,
    Result,
    Error,
    ErrorKind
};

use serde::{ Serialize, Deserialize };

const BOOT_SECTOR_SIZE:usize = 1024; // Kb
const MAGIC: &[u8; 5] = b"TAGFS";
const BLOCK_SIZE: usize = 4096; // Kb
const BLOCK_GROUP_SIZE: usize = 128*(1 << 20); // Mb
const BLOCKS_PER_GROUP: usize = BLOCK_GROUP_SIZE/BLOCK_SIZE;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SuperBlock {
    magic: [u8; 5],
    nr_inodes: u32,
    nr_blocks: u32,
    nr_tags: u32,
}

#[derive(Debug)]
struct TagFs {
    sup_block: SuperBlock,
    dev: File
}

impl SuperBlock {
    pub fn save(&self, fd: &mut File) -> Result<()> {
        
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

impl TagFs {

    fn load_device(dev_name: &str, enable_trunc: bool) -> File {
        let open = File::options()
            .read(true)
            .write(true)
            .truncate(enable_trunc)
            .open(dev_name);

        if open.is_err() {
            panic!("something went wrong: {}", open.err().unwrap());
        }

        open.unwrap()
    }


    pub fn format(dev_name: &str) -> Result<()> {
        let mut fd = Self::load_device(dev_name, true);

        if BLOCK_SIZE < BOOT_SECTOR_SIZE{
            panic!("block size is insufficient for storage purposes");
        }

        let sup_block = SuperBlock {
            magic: *MAGIC,
            nr_inodes: 0,
            nr_blocks: 0,
            nr_tags: 0
        };

        fd.write_all(&[0u8; 1024])?; // padding for boot sector

        // write superblock
        sup_block.save(&mut fd)?;

        // Pad the rest of Block 0 with zeros
        let current_pos = fd.stream_position()? as usize;
        let padding = vec![0u8; BLOCK_SIZE - current_pos];
        fd.write_all(&padding)?;

        Ok(())
    }

    fn check_magic(magic_in: &[u8;5]) -> bool { magic_in == MAGIC }

    fn _access_block(&mut self, block_index: usize, offset: u16) -> Result<()> {
        if block_index < 1 {
            //println!("[DEBUG] {}", self.tot_size/BLOCK_SIZE); 
            return Err(Error::new(ErrorKind::OutOfMemory, "ERROR: bad block index"));
        }

        let curr_pos: i64 = self.dev.stream_position()? as i64;
        let move_pos: i64 = ((BLOCK_SIZE*block_index) as i64) - curr_pos; 
        let final_pos: i64 = move_pos+(offset as i64);

        if offset >= BLOCK_SIZE as u16 {
            return Err(Error::new(ErrorKind::OutOfMemory, "ERROR: invalid block offset"));
        }

        self.dev.seek_relative(final_pos)?;
        Ok(())
    }

    fn write_into_block(&mut self, data: &Vec<u8>, block_index: usize, offset: u16) -> Result<()> {

        let res = match self._access_block(block_index, offset) {
            Ok(()) => { self.dev.write_all(data) }
            Err(err) => { Err(err) }
        };

        res
    }

    fn read_from_block(&mut self, buffer: &mut Vec<u8>, block_index: usize, offset: u16) -> Result<()> {

        let res = match self._access_block(block_index, offset) {
            Ok(()) => { self.dev.read_exact(buffer) }
            Err(err) => { Err(err) }
        };

        res
    }

    fn mount(dev_name: &str) -> Result<Self> {
        let mut device = Self::load_device(dev_name, false);
        let mut boot_tmp: [u8;1024] = [0u8;1024]; 

        let _ = device.read_exact(&mut boot_tmp)?;
        let sup_block = SuperBlock::read(&mut device)?;

        if !Self::check_magic(&sup_block.magic) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Filesystem magic number did not match"));
        }

        let res_ok = Self {
            sup_block: sup_block,
            dev: device
        };
        
        Ok(res_ok)
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() != 3 {
        panic!("device must be passed as parameter");
    }

    let op = (&args[1]).as_str();
    let device = (&args[2]).as_str();

    match op {
        "format" => {
            let res: Result<()> = TagFs::format(device);
            match res {
                Ok(()) => println!("Ok"),
                Err(err) => eprintln!("Error: {}", err),
            }
        }

        "mount" => {
            let res_fs = TagFs::mount(device);

            if res_fs.is_err() {
                let fs_err = res_fs.unwrap();
                panic!("{:?}",fs_err);
            }

            println!("Filesystem mounted");
        }

        "test_write" => {
            let res_fs = TagFs::mount(device);

            if res_fs.is_err() {
                let fs_err = res_fs.unwrap();
                panic!("{:?}",fs_err);
            }

            let mut fs = res_fs.unwrap(); 

            let prova = vec![0x8;1024];
            let res = fs.write_into_block(&prova,1,0);
            println!("{:?}", res);
        }

        "test_read" => {
            let res_fs = TagFs::mount(device);

            if res_fs.is_err() {
                let fs_err = res_fs.unwrap();
                panic!("{:?}",fs_err);
            }

            let mut fs = res_fs.unwrap();
            let mut read_buff = vec![0x0;1024];

            let _ = fs.read_from_block(&mut read_buff,1,0);
            println!("{:?}", read_buff);
        }

        _ => {
            eprintln!("Unsupported");
        }
       
    }
}
