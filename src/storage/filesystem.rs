use crate::storage::superblock::*;

#[derive(Debug)]
pub struct TagFs {
    sup_block: SuperBlock,
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


    pub fn format(dev_name: &str) -> Result<()> {
        let mut fd = Self::load_device(dev_name, true);

        if BLOCK_SIZE < BOOT_SECTOR_SIZE{
            panic!("block size is insufficient for storage purposes");
        }

        let sup_block = SuperBlock {
            magic: *MAGIC,
            nr_inodes: 0,
            nr_blocks: 0,
            nr_tags: 0
        };

        fd.write_all(&[0u8; 1024])?; // padding for boot sector

        // write superblock
        sup_block.save(&mut fd)?;

        // Pad the rest of Block 0 with zeros
        let current_pos = fd.stream_position()? as usize;
        let padding = vec![0u8; BLOCK_SIZE - current_pos];
        fd.write_all(&padding)?;

        Ok(())
    }

    fn check_magic(magic_in: &[u8;5]) -> bool { magic_in == MAGIC }

    fn _access_block(&mut self, block_index: usize, offset: u16) -> Result<()> {
        if block_index < 1 {
            //println!("[DEBUG] {}", self.tot_size/BLOCK_SIZE); 
            return Err(Error::new(ErrorKind::OutOfMemory, "ERROR: bad block index"));
        }

        let curr_pos: i64 = self.dev.stream_position()? as i64;
        let move_pos: i64 = ((BLOCK_SIZE*block_index) as i64) - curr_pos; 
        let final_pos: i64 = move_pos+(offset as i64);

        if offset >= BLOCK_SIZE as u16 {
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

    pub fn mount(dev_name: &str) -> Result<Self> {
        let mut device = Self::load_device(dev_name, false);
        let mut boot_tmp: [u8;1024] = [0u8;1024]; 

        let _ = device.read_exact(&mut boot_tmp)?;
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
}
