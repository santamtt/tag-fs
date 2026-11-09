use serde::{ Serialize, Deserialize };
use pub_fields::pub_fields;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[repr(C,align(8))]
pub enum LstEntryType {
    Inode,
    Extent,
    Dead,
    Free
}

#[pub_fields]
#[repr(C,align(32))]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LstEntry {
    phys_location: u64,
    entry_type: LstEntryType,
    checksum: u64
}

impl LstEntry {
    pub fn new() -> Self {
        Self {
            phys_location: 0,
            entry_type: LstEntryType::Free,
            checksum: 0
        }
    }
}
