use crate::storage::superblock::*;
use crate::storage::inode::*;

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

    fn find_next_free(&mut self, bitmap: &Vec<u8>, ) { 

        let mut new_byte_pos: usize = 0;
        let mut new_bit_pos: u8 = 0;
        let mut is_full: bool = true;

        'check: for byte_index in 0..bitmap.len() {
            for i in 0..=8 {
                let bit: bool = (bitmap[byte_index] >> i) & 0x1 != 0;

                if bit { 
                    is_full = false; 
                    new_byte_pos = byte_index;
                    new_bit_pos = i;
                    break 'check
                } 
            }
        }

        if is_full {
            self.sup_block.free_i_bitmap = 0;
            self.sup_block.free_i_loc = 0; 
            return ; 
        }

        let inode_size: usize = size_of::<Inode>(); 
        let inode_pos: usize = (BLOCK_SIZE << 1)+(size_of::<u8>()*new_byte_pos+new_bit_pos as usize)*inode_size;

        self.sup_block.free_i_bitmap = (BLOCK_SIZE + new_byte_pos) as u64;
        self.sup_block.free_i_loc = inode_pos as u64; 
    }

    fn write_inode(&mut self, inode: &Inode) -> Result<()> {

        // check first if inode i_block size is correct
        inode.check_i_block_size()?;
        if self.sup_block.free_i_bitmap == 0 && self.sup_block.free_i_loc == 0 { 
            return Err(Error::new(
                    ErrorKind::InvalidData,
                    "cannot allocate inode: no space left"
            ));
        }

        let curr_pos: usize = self.dev.stream_position()? as usize;
        let inode_table_pos: usize = BOOT_SECTOR_SIZE + size_of::<SuperBlock>(); 

        if curr_pos != inode_table_pos {
            self.dev.seek(SeekFrom::Start(inode_table_pos as u64))?;
        }

        // retrieve inode bitmap from the
        // disk, and use it to find the first
        // free position 
        let mut bitmap: Vec<u8> = vec![0u8; BLOCK_SIZE];
        self.dev.read_exact(&mut bitmap)?;

        let byte_pos: usize = (self.sup_block.free_i_bitmap / 8) as usize;
        let bit_pos: u8 = (self.sup_block.free_i_bitmap % 8) as u8;

        // calculate the inode position on the
        // disk, save it and update the bitmap

        let inode_size: usize = size_of::<Inode>(); 
        let inode_pos: usize = (size_of::<u8>()*byte_pos+bit_pos as usize)*inode_size;

        // TODO: substitute with relative
        self.dev.seek(SeekFrom::Start(inode_pos as u64))?;

        let res_encoded = bincode::serialize(inode).map_err(|e| {
            Error::new(ErrorKind::InvalidData,e);
        });

        if res_encoded.is_err(){
            return Err(Error::new(
                    ErrorKind::InvalidData,
                    "inode cannot be serialized correctly"
            ));
        }
        
        let mask_update: u8 = 0x1 << bit_pos; 
        bitmap[byte_pos] |= mask_update;

        // TODO: substitute with relative
        self.find_next_free(&mut bitmap);
        self.dev.seek(SeekFrom::Start(inode_table_pos as u64))?;
        self.dev.write_all(&bitmap)?;
        self.sup_block.nr_inodes += 1;

        self.sup_block.save(&mut self.dev)?;

        Ok(())
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

    pub fn format(dev_name: &str) -> Result<()> {
        let mut fd = Self::load_device(dev_name, true);

        if BLOCK_SIZE < BOOT_SECTOR_SIZE {
          	return Err(Error::new(
            	ErrorKind::OutOfMemory, "block size is insufficient for storage purposes"));
        }

        let inode_bitmap_pos : usize = size_of::<SuperBlock>() + BOOT_SECTOR_SIZE;
        let inode_pos : usize = inode_bitmap_pos + BLOCK_SIZE; 

        let sup_block = SuperBlock {
            magic: *MAGIC,
            nr_inodes: 0,
            nr_blocks: 1,
            nr_tags: 0,
            root_pos: 0,
            free_i_bitmap: inode_bitmap_pos as u64,
            free_i_loc: inode_pos as u64,
            free_d_loc: 0
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
}
