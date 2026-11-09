mod superblock;
mod inode;
mod extent;
mod alloc;
mod open;
mod lst;
mod segment;
mod data_structures;

use crate::storage::inode::*;
use crate::storage::extent::*;
use crate::storage::alloc::*;
use crate::storage::data_structures::mem_arena::Arena;

use crate::storage::open::*;

use std::io::{
    Result,
    Error,
    ErrorKind
};

use std::cmp::min;

#[derive(Debug)]
pub struct TagFs {
    alloc: Alloc,
    open: Arena<OpenEntry>
}

impl TagFs {

    fn _alloc_inode(&mut self, i_type: InodeType) -> Result<Inode> {
        let new_inode = Inode::new(i_type);
        Ok(new_inode)
    }

    fn _alloc_root(alloc: &mut Alloc) -> Result<()> {
        let root_inode = Inode::new(InodeType::Tag);

        let alloc_res = alloc.register_inode(&root_inode);
        if alloc_res.is_err() { return Err(alloc_res.err().unwrap()); }

        Ok(())
    }

    pub fn register_tag(&mut self, tag_name: &str) -> Result<()> {

        let mut inode_root = self.alloc.read_root()?;

        let tag_inode: Inode = Inode::new(InodeType::Tag);
        let inode_id: u64 = self.alloc.register_inode(&tag_inode)?;

        if !inode_root.add_tag_element(tag_name, inode_id) { return Err(
            Error::new(
                ErrorKind::InvalidData, "Cannot add tag element in the root"));
        }
        self.alloc.commit_root(&inode_root)
    }

    pub fn open_file(&mut self, path: u128) -> Result<usize> {
        let mut file_ref = OpenEntry::new();
        file_ref.id = path;
        let ret = self.open.insert(file_ref);
        Ok(ret)
    }

    pub fn close_file(&mut self, fd: usize) -> Result<()> {
        if !self.open.delete(fd) { 
            return Err(Error::new(ErrorKind::Other,"Close failed: file is not opened")); 
        }

        Ok(())
    }

    fn _read_inode(&mut self, fd: usize) -> Result<Inode> {

        if fd >= self.open.elements.len() { 
            return Err(Error::new(ErrorKind::Other,"Read failed: file is not opened")); 
        }
        
        match self.open.elements[fd] {
            Some(state) => {
                return self.alloc.read_obj::<Inode>(state.id);
            },

            None => { 
                return Err(Error::new(ErrorKind::Other, format!("Cannot open file with inode id {}",fd)));
            }
        }
    }

    pub fn read_file(&mut self, fd: usize, read_size: usize) -> Result<Vec<u8>> {

        let mut inode: Inode = self._read_inode(fd)?;
        let mut data: Vec<u8> = Vec::new();
        let mut state: OpenEntry = self.open.elements[fd].unwrap(); 

        match self.alloc.file_seek(&inode, state.offset) {
            ExtentType::Direct => {
                let offset_usize: usize = state.offset as usize;
                let inode_payload_usize: usize = INODE_PAYLOAD as usize;
                let end: usize= min(read_size + offset_usize, inode_payload_usize);

                let read_payload: usize = end - offset_usize;

                data.append(&mut inode.payload[offset_usize..end].to_vec());
                self.open.elements[fd] = Option::Some(state);
            },
            ExtentType::Indirect(extent) => { todo!("Indirect extent read handling"); },
            ExtentType::Invalid => { return Err(Error::new(ErrorKind::InvalidData, "Bad read alloc: invalid seek")); }
        }
        
        Ok(data)
    }

    pub fn write_file(&mut self, fd: usize, bytes_in: Vec<u8>) -> Result<()> {

        let mut inode: Inode = self._read_inode(fd)?; 
        let mut state: OpenEntry = self.open.elements[fd].unwrap(); 

        match self.alloc.file_seek(&inode, state.offset) {
            ExtentType::Direct => {
                inode.write(state.offset, &bytes_in[0..(INODE_PAYLOAD - state.offset) as usize].to_vec());
                state.offset += INODE_PAYLOAD - state.offset;
                self.open.elements[fd] = Option::Some(state);
            },
            ExtentType::Indirect(extent) => { todo!("Indirect extent write handling"); }
            ExtentType::Invalid => { return Err(Error::new(ErrorKind::InvalidData, "Bad write alloc: invalid seek")); }
        }

        self.alloc.commit::<Inode>(&inode, state.id)
    }

    pub fn mount(dev_name: &str) -> Result<Self> {

        match Alloc::mount(dev_name) {
            Ok(alloc) => {
                return Ok(Self { alloc: alloc, open: Arena::new() });
            },
            Err(e) => { return Err(e); }
        }

    }

    pub fn format(dev_name: &str) -> Result<()> {

        let mut alloc = Alloc::new(dev_name, true);
        alloc.format_disk()?;
        Self::_alloc_root(&mut alloc)?;

        Ok(())
    }
}
