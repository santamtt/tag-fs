extern crate libc;
extern crate serde;

mod storage;
mod fuse;

use fuser::{ Config, MountOption };
use crate::storage::TagFs;

use std::env;
use std::io::Result;

fn main() -> Result<()> {
    let mut config = Config::default();
    config.mount_options = vec![MountOption::FSName("artfs".into()), MountOption::RW];
    fuser::mount(TagFs::mount("test_disk.img")?, "exec_mount/", &config).unwrap(); // blocks until unmounted 
    Ok(())
}
