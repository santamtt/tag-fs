//! Superblock and on-disk layout computation.
//!
//! Disk layout (all offsets in bytes):
//!
//! ```text
//! 0          SB_SLOT_SIZE      2*SB_SLOT_SIZE
//! | SB slot 0 | SB slot 1 | LST copy 0 | LST copy 1 | SUT copy 0 | SUT copy 1 | pad | seg 0 | seg 1 | ...
//!                                                                               ^ data_start (chunk aligned)
//! ```
//!
//! - Two superblock slots: generation `g` is written to slot `g & 1`. On mount
//!   the valid slot (magic + checksum) with the highest generation wins.
//! - LST (Logical Segment Table) and SUT (Segment Usage Table, one allocation
//!   bitmap per logical segment) are double buffered the same way: a
//!   checkpoint with generation `g` writes copy `g & 1`, and the superblock is
//!   written only after that copy is durable.

use serde::{ Serialize, Deserialize };
use pub_fields::pub_fields;

use std::io::{ Error, ErrorKind, Result };

use crate::storage::lst::{ LST_HEADER_SIZE, LstEntry };

pub const MAGIC: &[u8; 5] = b"ArtFS";
pub const FORMAT_VERSION: u32 = 1;

/// Fixed size of a superblock slot, independent of the block size so the
/// superblock can be located before the block size is known.
pub const SB_SLOT_SIZE: u64 = 4096;
pub const SB_SLOTS: u64 = 2;

/// Object id meaning "no object" (unit 0 of every segment is its header, so
/// no real object can have this id).
pub const NO_OBJECT: u64 = 0;

#[derive(Debug, Clone, Copy)]
pub struct FormatOptions {
    pub block_size: u64,
    pub chunk_size: u64,
    pub seg_size: u64,
}

impl Default for FormatOptions {
    fn default() -> Self {
        let block_size: u64 = 4096;          // 4 KiB
        Self {
            block_size,
            chunk_size: block_size << 4,     // 64 KiB
            seg_size: block_size << 18,      // 1 GiB
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[pub_fields]
pub struct SuperBlock {
    magic: [u8; 5],
    version: u32,
    generation: u64,
    nr_inodes: u32,

    /// Number of logical segment ids == number of physical segment slots.
    nr_segments: u32,
    /// Object id of the root inode (`NO_OBJECT` until the first inode exists).
    root_id: u64,
    block_size: u64,

    seg_size: u64,
    chunk_size: u64,
    /// Size of the device when it was formatted.
    disk_size: u64,

    /// Location of the two LST copies.
    sgn_table_loc: [u64; 2],
    /// Size in bytes of one LST copy (block aligned).
    sgn_table_size: u64,

    /// Location of the two SUT copies.
    sut_loc: [u64; 2],
    /// Bytes reserved for the bitmap of one segment inside a SUT copy.
    sut_seg_bytes: u64,

    /// Physical offset of segment slot 0 (chunk aligned).
    data_start: u64,
    /// Placement class used when the caller does not specify one.
    default_class: u32,

    checksum: u32,
}

fn round_up(v: u64, align: u64) -> u64 { v.div_ceil(align) * align }

fn invalid(msg: &str) -> Error { Error::new(ErrorKind::InvalidInput, msg.to_string()) }

impl SuperBlock {

    pub fn empty() -> Self {
        Self {
            magic: [0u8; 5],
            version: 0,
            generation: 0,
            nr_inodes: 0,
            nr_segments: 0,
            root_id: NO_OBJECT,
            block_size: 0,
            seg_size: 0,
            chunk_size: 0,
            disk_size: 0,
            sgn_table_loc: [0; 2],
            sgn_table_size: 0,
            sut_loc: [0; 2],
            sut_seg_bytes: 0,
            data_start: 0,
            default_class: 0,
            checksum: 0,
        }
    }

    pub fn check_magic(&self) -> bool { self.magic == *MAGIC }

    /// Computes the layout of a fresh filesystem on a device of `disk_size` bytes.
    pub fn compute_layout(disk_size: u64, opts: &FormatOptions) -> Result<Self> {
        let FormatOptions { block_size, chunk_size, seg_size } = *opts;

        if !block_size.is_power_of_two() || block_size < 512 {
            return Err(invalid("block size must be a power of two >= 512"));
        }
        if !chunk_size.is_power_of_two() || chunk_size < block_size {
            return Err(invalid("chunk size must be a power of two >= block size"));
        }
        if !seg_size.is_power_of_two() || seg_size < 2 * chunk_size {
            return Err(invalid("segment size must be a power of two >= 2 chunks"));
        }
        // Unit indexes are stored in the lower 32 bits of an object id.
        if seg_size / block_size > u32::MAX as u64 {
            return Err(invalid("segment size too large for 32-bit unit indexes"));
        }

        let max_segs = (disk_size / seg_size).min(u32::MAX as u64);

        let lst_bytes = round_up(
            LST_HEADER_SIZE + max_segs * LstEntry::encoded_size(), block_size);
        let sut_seg_bytes = round_up((seg_size / block_size).div_ceil(8), block_size);
        let sut_bytes = max_segs * sut_seg_bytes;

        let lst0 = round_up(SB_SLOTS * SB_SLOT_SIZE, block_size);
        let lst1 = lst0 + lst_bytes;
        let sut0 = lst1 + lst_bytes;
        let sut1 = sut0 + sut_bytes;
        let data_start = round_up(sut1 + sut_bytes, chunk_size);

        let nr_segments = disk_size.saturating_sub(data_start) / seg_size;
        if nr_segments == 0 {
            return Err(invalid("device too small for the requested segment size"));
        }

        Ok(Self {
            magic: *MAGIC,
            version: FORMAT_VERSION,
            generation: 0,
            nr_inodes: 0,
            nr_segments: nr_segments as u32,
            root_id: NO_OBJECT,
            block_size,
            seg_size,
            chunk_size,
            disk_size,
            sgn_table_loc: [lst0, lst1],
            sgn_table_size: lst_bytes,
            sut_loc: [sut0, sut1],
            sut_seg_bytes,
            data_start,
            default_class: 0,
            checksum: 0,
        })
    }

    /// Serializes the superblock (with checksum) into a full slot buffer.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut sb = self.clone();
        sb.checksum = 0;
        let raw = bincode::serialize(&sb).map_err(Error::other)?;
        sb.checksum = crc32c::crc32c(&raw);

        let mut buf = bincode::serialize(&sb).map_err(Error::other)?;
        if buf.len() as u64 > SB_SLOT_SIZE {
            return Err(Error::other("superblock larger than its slot"));
        }
        buf.resize(SB_SLOT_SIZE as usize, 0);
        Ok(buf)
    }

    /// Decodes a slot buffer; returns `None` if the slot does not contain a
    /// valid superblock (bad magic, version or checksum).
    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut sb: SuperBlock = bincode::deserialize(buf).ok()?;
        if !sb.check_magic() || sb.version != FORMAT_VERSION { return None; }

        let stored = sb.checksum;
        sb.checksum = 0;
        let raw = bincode::serialize(&sb).ok()?;
        if crc32c::crc32c(&raw) != stored { return None; }

        sb.checksum = stored;
        Some(sb)
    }

    /// Superblock slot used by a given generation.
    pub fn slot_for(generation: u64) -> u64 { (generation & 1) * SB_SLOT_SIZE }

    /// LST/SUT copy used by a given generation.
    pub fn copy_for(generation: u64) -> usize { (generation & 1) as usize }

    /// Physical byte offset of physical segment slot `slot`.
    pub fn phys_of_slot(&self, slot: u32) -> u64 { self.data_start + slot as u64 * self.seg_size }

    /// Physical segment slot at byte offset `phys`.
    pub fn slot_of_phys(&self, phys: u64) -> Option<u32> {
        if phys < self.data_start || (phys - self.data_start) % self.seg_size != 0 { return None; }
        let slot = (phys - self.data_start) / self.seg_size;
        if slot >= self.nr_segments as u64 { return None; }
        Some(slot as u32)
    }

    pub fn block_shift(&self) -> u8 { self.block_size.trailing_zeros() as u8 }
    pub fn chunk_shift(&self) -> u8 { self.chunk_size.trailing_zeros() as u8 }
}
