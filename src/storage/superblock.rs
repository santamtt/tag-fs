use serde::{ Serialize, Deserialize };
use pub_fields::pub_fields;

pub const MAGIC: &[u8; 5] = b"ArtFS";

#[derive(Debug, Serialize, Deserialize)]
#[pub_fields]
#[repr(C,align(4096))]
pub struct SuperBlock {
   magic: [u8; 5],
   generation: u64,
   nr_inodes: u32,

   nr_segments: u32,
   root_id: u64,
   block_size: u64,

   seg_size: u64,
   free_loc: u64,
   chunk_size: u64,

   sgn_table_loc: u64,
   sgn_table_size: u64,
   sgn_id: u64
}

impl SuperBlock {
    pub fn check_magic(&self) -> bool { self.magic == *MAGIC }
}
