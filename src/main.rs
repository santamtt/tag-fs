extern crate fuse; 

use std::env;
use fuse::{
    Filesystem
};

#[warn(dead_code)]
struct TagFS{
    dummy: u8
}

impl Filesystem for TagFS {

}

fn main() {
    let fs = TagFS { dummy: 0 };

    let _mount_path = match env::args().nth(1) {
        Some(path) => path,
        _ => { panic!("no mounting point provided") }
    };

    match fuse::mount(fs, &_mount_path, &[]) {
        Ok(mount_fs) => {
            println!("Fs mounted");
        },
        Err(err) => {
            panic!("failed to mount the filesystem, here it is the error: {}",err);
        }
    };
}
