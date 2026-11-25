use serde::{Serialize,Deserialize};

#[repr(C,align(16))]
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Extent {
    pub logical_id: u64, 
    pub phys_block_offset: u32,
    pub length: u32
}

#[derive(Copy, Clone, Debug)]
pub enum ExtentType<'a> {
    Invalid,
    Direct,
    Indirect(&'a Extent)
}

impl Extent {
    pub fn new(logical_id: u64, phys_block_offset: u32, length: u32) -> Self {
        Self {
            logical_id: logical_id, 
            phys_block_offset: phys_block_offset,
            length: length 
        }
    }
}
