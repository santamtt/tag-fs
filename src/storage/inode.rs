use std::io::{
    Result,
    Error,
    ErrorKind
};

use crate::storage::extent::*;

use libc::{
    getegid,
    geteuid,
    clock_gettime
};
use serde::{ Serialize, Deserialize };

#[derive(Copy, Clone, Debug)]
#[derive(Serialize, Deserialize)]
pub enum InodeType {
    Tag,
    File
}

pub const INODE_PAYLOAD: u64 = 3847;

#[repr(C,align(4096))]
#[derive(Debug, Serialize, Deserialize)]
pub struct Inode {
    _unused: u64,
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

    pub extents: Vec<Extent>,

    indirect_ext_block: u64,
    indirect_ext_count: u64,

    pub payload: Vec<u8>
}

impl Inode{

    pub fn new(i_type: InodeType) -> Self {

        let local_gid = unsafe { getegid() };
        let local_uid = unsafe { geteuid() };

        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        unsafe {
            clock_gettime(libc::CLOCK_REALTIME, &mut ts);
        };

        let new_inode = Inode {
            _unused: 0,
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
            extents: vec![Extent::new(0,0,0);128],

            indirect_ext_block: 0,
            indirect_ext_count: 0,

            payload: vec![0u8;INODE_PAYLOAD as usize]
        };

        new_inode
    }

    fn _check_write_permissions(&self) -> bool {

        let my_gid = unsafe { getegid() };
        let my_uid = unsafe { geteuid() };
        let mut shift = 8;

        if my_uid != self.uid {
            shift = if my_gid == self.gid { 4 } else { 0 };
        }

        let mask: u16 = 0x2 << shift;

        self.mode & mask == mask
    }

    pub fn write(&mut self, start_pos: u64, bytes_in: &Vec<u8>) -> bool {

        if !(self._check_write_permissions() && start_pos < INODE_PAYLOAD) { return false; }

        for i in 0..bytes_in.len() {
            self.payload[i] = bytes_in[i];
        }

        true
    }

    fn _append(&mut self, entry: &Vec<u8>) -> bool {

        if self.depth == 0 {
            let size_bytes: usize = self.size_bytes as usize;
            for i in 0..entry.len() {
                self.payload[i+size_bytes] = entry[i];
            }

            self.size_bytes += entry.len() as u64;
            return true;
        }

        false
    }

    pub fn add_tag_element(&mut self, tag_name: &str, inode_id: u64) -> bool {

        match self.i_type {
            InodeType::Tag => {
                if tag_name.len() > 120 { return false; }
                let mut serialized_entry: Vec<u8> = tag_name.as_bytes().to_vec();
                let mut serialized_id = inode_id.to_ne_bytes().to_vec();

                serialized_entry.append(&mut serialized_id);
                return self._append(&serialized_entry);
            },
            _ => { return false; }
        }
    }
    
    pub fn seek(&self, pos: u64, block_size: u64) -> Option<&Extent> {
        if pos > self.size_bytes {
            return Option::None
        }

        Option::None
    }

}
