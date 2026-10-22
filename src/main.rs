extern crate libc;
extern crate serde;

use std::env;
use std::io::Result;

mod storage;
use crate::storage::TagFs;

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

        "test_alloc" => {
            let res_fs = TagFs::mount(device);

            if res_fs.is_err() {
                let fs_err = res_fs.unwrap();
                panic!("{:?}",fs_err);
            }

            let mut fs = res_fs.unwrap(); 
            let res = fs.write_file(0,String::from("Hello World").into_bytes());
            if res.is_err() {
                let fs_err = res.unwrap();
                panic!("{:?}",fs_err);
            }
        }

        "test_tag" => {
            let res_fs = TagFs::mount(device);

            if res_fs.is_err() {
                let fs_err = res_fs.unwrap();
                panic!("{:?}",fs_err);
            }

            let mut fs = res_fs.unwrap(); 
            let res = fs.register_tag("prova");
            if res.is_err() {
                let fs_err = res.unwrap();
                panic!("{:?}",fs_err);
            }
        }

        _ => {
            eprintln!("Unsupported");
        }
       
    }
}
