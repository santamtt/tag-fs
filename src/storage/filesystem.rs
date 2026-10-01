use crate::storage::superblock::*;
use crate::storage::inode::*;
use crate::storage::node::*;

#[derive(Debug)]
pub struct TagFs {
    pub sup_block: SuperBlock,
    dev: File
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

    fn check_magic(magic_in: &[u8;5]) -> bool { magic_in == MAGIC }

    fn _access_block(&mut self, block_index: usize, offset: u16) -> Result<()> {
        if block_index < 1 {
            return Err(Error::new(ErrorKind::OutOfMemory, "ERROR: bad block index"));
        }

        let curr_pos: i64 = self.dev.stream_position()? as i64;
        let move_pos: i64 = ((self.sup_block.block_size*block_index as u64) as i64) - curr_pos; 
        let final_pos: i64 = move_pos+(offset as i64);

        if offset >= self.sup_block.block_size as u16 {
            return Err(Error::new(ErrorKind::OutOfMemory, "ERROR: invalid block offset"));
        }

        self.dev.seek_relative(final_pos)?;
        Ok(())
    }

    pub fn write_into_block(&mut self, data: &Vec<u8>, block_index: usize, offset: u16) -> Result<()> {

        let res = match self._access_block(block_index, offset) {
            Ok(()) => { self.dev.write_all(data) }
            Err(err) => { Err(err) }
        };

        res
    }

    pub fn read_from_block(&mut self, buffer: &mut Vec<u8>, block_index: usize, offset: u16) -> Result<()> {

        let res = match self._access_block(block_index, offset) {
            Ok(()) => { self.dev.read_exact(buffer) }
            Err(err) => { Err(err) }
        };

        res
    }

    fn _alloc_inode(&mut self, i_type: InodeType) -> Result<Inode> {
        let new_inode = Inode::new(i_type, 0);
        Ok(new_inode)
    }

    fn _alloc_root(sup_block: &SuperBlock, fd: &mut File) -> Result<()> {
        
        Self::_go_to(fd, sup_block.root_pos)?;
        let mut root_inode = Inode::new(InodeType::File, 0);
        let serialized_root = root_inode.serialize();
        if serialized_root.is_err() {
            return Err(Error::new(
                    ErrorKind::InvalidData, "Cannot serialize root inode"));
        }

        fd.write_all(&serialized_root.unwrap())
    }

    pub fn _read_root(&mut self) -> Result<()> {
        Self::_go_to(&mut self.dev, self.sup_block.root_pos)?;
        let mut disk_inode: Vec<u8> = vec![0u8; self.sup_block.block_size as usize];
        self.dev.read_exact(&mut disk_inode)?;
        let res_inode = Inode::deserialize(&disk_inode);


        println!("{:?}",inode.payload);

        Ok(())
    }

    pub fn write_file(&mut self, bytes_in: Vec<u8>) -> Result<()> {

        let mut res_inode = self._alloc_inode(InodeType::File);

        match res_inode {
            Ok(mut inode) => {
                inode.write(&bytes_in);
                return Ok(());
            },
            Err(e) => {
                return Err(e);
            }
        }
    }

    pub fn alloc_tag(&mut self) -> Result<()> {
        let _  = self._alloc_inode(InodeType::Tag);
        Ok(())
    }

    pub fn register_tag(&mut self, tag_name: &str, inode_id: u64) -> Result<()> {

        self.dev.seek(SeekFrom::Start(self.sup_block.root_pos as u64))?;

        Ok(())
    }

    fn _go_to(fd: &mut File, next_pos: u64) -> Result<()> {

        let curr_pos: i64 = fd.stream_position()? as i64;
        let i64_next_pos: i64 = next_pos as i64;
        let final_pos = i64_next_pos - curr_pos;

        fd.seek_relative(final_pos)
    }

    pub fn mount(dev_name: &str) -> Result<Self> {
        let mut device = Self::load_device(dev_name, false);

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

    pub fn format(dev_name: &str) -> Result<()> {

        let mut fd = Self::load_device(dev_name, true);
        fd.seek(SeekFrom::Start(0))?;

        let local_block_size: u64 = 4096; // bytes (4 kb)
        let local_chunk_size: u64 = 16*local_block_size as u64;
        let local_seg_size: u64 = 512*local_block_size; // 2 Mb

        let sup_block = SuperBlock {
            magic: *MAGIC,
            timestamp: 0,
            nr_inodes: 0,
            nr_segments: 2,
            root_pos: local_seg_size as u64 + local_block_size,
            block_size: local_block_size,

            seg_size: local_seg_size,
            free_loc: local_seg_size,

            chunk_size: local_chunk_size,
        };

        // write superblock
        sup_block.save(&mut fd)?;
        Self::_alloc_root(&sup_block, &mut fd)?;

        Ok(())
    }
}
