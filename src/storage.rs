mod superblock;
mod inode;
mod extent;
mod alloc;
mod open;

use crate::storage::inode::*;
use crate::storage::extent::*;
use crate::storage::alloc::*;

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
    open: Vec<OpenEntry>
}

impl TagFs {

    fn _alloc_inode(&mut self, i_type: InodeType) -> Result<Inode> {
        let new_inode = Inode::new(i_type);
        Ok(new_inode)
    }

    fn _alloc_root(alloc: &mut Alloc) -> Result<()> {
        let root_inode = Inode::new(InodeType::Tag);

        let alloc_res = alloc.register_obj::<Inode>(&root_inode);
        if alloc_res.is_err() { return Err(alloc_res.err().unwrap()); }

        Ok(())
    }

    pub fn register_tag(&mut self, tag_name: &str) -> Result<()> {

        let mut inode_root = self.alloc.read_root()?;

        let tag_inode: Inode = Inode::new(InodeType::Tag);

        let alloc_inode = self.alloc.register_obj::<Inode>(&tag_inode);
        if alloc_inode.is_err() { return Err(alloc_inode.err().unwrap()); }
        let inode_id: u64 = alloc_inode.unwrap();

        if !inode_root.add_tag_element(tag_name, inode_id) { return Err(
            Error::new(
                ErrorKind::InvalidData, "Cannot add tag element in the root"));
        }
        self.alloc.commit_root(&inode_root)
    }

    pub fn open_file(&mut self, path: u64) -> Result<usize> {
        let mut file_ref = OpenEntry::new();
        file_ref.id = path;
        self.open.push(file_ref);
        Ok(self.open.len() - 1)
    }

    pub fn close_file(&mut self, fd: usize) -> Result<()> {
        if self.open.len() < fd { 
            return Err(Error::new(ErrorKind::Other,"Close failed: file is not opened")); 
        }

        self.open.remove(fd);
        Ok(())
    }

    pub fn read_file(&mut self, fd: usize, read_size: usize) -> Result<Vec<u8>> {

        if fd >= self.open.len() { 
            return Err(Error::new(ErrorKind::Other,"Read failed: file is not opened")); 
        }
        
        let state: &mut OpenEntry = &mut self.open[fd];
        let inode: Inode = self.alloc.read_obj::<Inode>(state.id)?;

        let mut data: Vec<u8> = Vec::new();

        match self.alloc.file_seek(&inode, state.offset) {
            ExtentType::Direct => {
                let offset_usize: usize = state.offset as usize;
                let inode_payload_usize: usize = INODE_PAYLOAD as usize;
                let end: usize= min(read_size + offset_usize, inode_payload_usize);

                let read_payload: usize = end - offset_usize;

                data.append(&mut inode.payload[offset_usize..end].to_vec());
            },
            ExtentType::Indirect(extent) => { todo!("Indirect extent read handling"); },
            ExtentType::Invalid => { return Err(Error::new(ErrorKind::InvalidData, "Bad read alloc: invalid seek")); }
        }
        
        Ok(data)
    }

    pub fn write_file(&mut self, fd: usize, bytes_in: Vec<u8>) -> Result<()> {

        if fd >= self.open.len() { 
            return Err(Error::new(ErrorKind::Other,"Write failed: file is not opened")); 
        }
        
        let state: &mut OpenEntry = &mut self.open[fd];
        let mut inode: Inode = self.alloc.read_obj::<Inode>(state.id)?;
        
        match self.alloc.file_seek(&inode, state.offset) {
            ExtentType::Direct => {
                inode.write(state.offset, &bytes_in[0..(INODE_PAYLOAD - state.offset) as usize].to_vec());
                state.offset += INODE_PAYLOAD - state.offset;
            },
            ExtentType::Indirect(extent) => { todo!("Indirect extent write handling"); }
            ExtentType::Invalid => { return Err(Error::new(ErrorKind::InvalidData, "Bad write alloc: invalid seek")); }
        }

        self.alloc.commit::<Inode>(&inode, state.id)
    }

    pub fn mount(dev_name: &str) -> Result<Self> {

        match Alloc::mount(dev_name) {
            Ok(alloc) => {
                return Ok(Self { alloc: alloc, open: vec![OpenEntry::new();3] });
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
