extern crate libc;
extern crate serde;

use std::env;
use std::io::Result;

mod storage;
use crate::storage::filesystem::TagFs;

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
