extern crate libc;

use std::fs::*;
use std::io::{Result, Read, Write, Seek};
use std::env;

const MAGIC: &[u8; 5] = b"TAGFS";
const BLOCK_SIZE: u64 = 4096;

#[warn(dead_code)]
struct TagFs {
    size: u32,
    tags: Vec<u8>
}

impl TagFs {

    fn load_device(dev_name: &str, enable_trunc: bool) -> File {
        let open = File::options()
            .read(true)
            .write(true)
            .truncate(enable_trunc)
            .open(dev_name);
        
        if open.is_err() {
            panic!("something went wrong: {}",open.err().unwrap());
        }

        open.unwrap()
    }

    pub fn format(dev_name: &str) -> Result<()> {
        let mut fd = Self::load_device(dev_name, true);
        let res = fd.write_all(MAGIC)?;

        fd.write_all(&5u64.to_le_bytes())?; // size = 0

        // Pad the rest of Block 0 with zeros
        let current_pos = fd.stream_position()?;
        let padding = vec![0u8; (BLOCK_SIZE - current_pos) as usize];
        fd.write_all(&padding)?;

        Ok(())
    }

    fn check_magic(device: &mut File) -> bool {
        let mut read_buff: [u8;5] = [0u8;5];
        let magic_read = device.read_exact(&mut read_buff);
        let mut compare = false;

        if magic_read.is_ok() {
           compare = &read_buff == MAGIC; 
        } else {
            let magic_error = magic_read.err().unwrap();
            eprintln!("Failed to load filesystem magic number: {}", magic_error);
        }

        compare
    }

    fn mount(dev_name: &str) -> Option<Self> {
        let mut device = Self::load_device(dev_name, false);
        if !Self::check_magic(&mut device){ return Option::None; }

        let mut read_buff: [u8;8] = [0u8; 8];
        let size_read = device.read_exact(&mut read_buff);
        
        if size_read.is_ok(){
        println!("{:?}",read_buff);
        }
        return Option::Some(Self { size: 0, tags: Vec::new() });
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
                Err(err) => eprintln!("Error: {}", err)
            }
        },
        "mount" => {
            let fs = TagFs::mount(device);
        },
        _ => {
            eprintln!("Unsupported");
        }
    }
}
