use std::io::{
    Result,
    Error,
    ErrorKind
};

use libc::*;
pub use serde::{ Serialize, Deserialize };

#[derive(Copy, Clone)]
#[derive(Serialize, Deserialize)]
pub enum InodeType {
    Tag,
    File
}

#[repr(C,align(4096))]
#[derive(Serialize, Deserialize)]
pub struct Inode {
    id: u64,
    generation: u64,
    size_bytes: u64,

    atime_sec: u64,
    mtime_sec: u64,
    ctime_sec: u64,

    mode: u16,
    i_type: InodeType,
    depth: u8,
    flags: u32,

    uid: u32,
    gid: u32,
    nr_ext: u32,

    link_count: u16,

    atime_nsec: u32,
    mtime_nsec: u32,
    ctime_nsec: u32,

    checksum: u32,

    extents: Vec<u8>,

    indirect_ext_block: u64,
    indirect_ext_count: u64,

    pub payload: Vec<u8> 
}

impl Inode{

    pub fn new(i_type: InodeType, id: u64) -> Self {

        let local_gid = unsafe { libc::getegid() };
        let local_uid = unsafe { libc::geteuid() };

        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        unsafe {
            libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        };

        let new_inode = Inode {
            id: id,
            generation: 0,
            size_bytes: 0,

            atime_sec: ts.tv_sec as u64,
            mtime_sec: ts.tv_sec as u64,
            ctime_sec: ts.tv_sec as u64,

            mode: 0x0644,
            i_type: i_type,
            depth: 0,
            flags: 0,

            uid: local_uid,
            gid: local_gid,
            nr_ext: 0,

            link_count: 0,

            atime_nsec: ts.tv_nsec as u32,
            mtime_nsec: ts.tv_nsec as u32,
            ctime_nsec: ts.tv_nsec as u32,

            checksum: 0,
            extents: vec![0;128],

            indirect_ext_block: 0,
            indirect_ext_count: 0,

            payload: vec![0u8;3847]
        };

        new_inode
    }


    fn _check_permissions(&self) -> bool {

        let my_gid = unsafe { libc::getegid() };
        let my_uid = unsafe { libc::geteuid() };
        let mut res = true;
        let mut shift = 8;

        if my_uid != self.uid {
            shift = if my_gid == self.gid { 4 } else { 0 };
        }

        let mask: u16 = 0x2 << shift;

        self.mode & mask == mask 
    }

    pub fn serialize(&self) -> Result<Vec<u8>>{
        let res_encoded = bincode::serialize(self).map_err(Error::other);

        if res_encoded.is_err(){
            return Err(Error::new(
                    ErrorKind::InvalidData,
                    "Superblock cannot be serialized correctly"
            ));
        }

        res_encoded 
    }

    pub fn deserialize(bytes_in: &Vec<u8>) -> Result<Self>{

        let res_inode: Inode = bincode::deserialize(bytes_in).map_err(Error::other)

        res_inode
    }

    pub fn write(&mut self, bytes_in: &Vec<u8>) -> bool {


        if !self._check_permissions() { return false; }

        if self.depth == 0 {

            for i in 0..bytes_in.len() {
                self.payload[i] = bytes_in[i];
            }

            return true;
        } 
       
        false
    }

}
