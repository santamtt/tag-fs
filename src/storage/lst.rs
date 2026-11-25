//! Logical Segment Table.
//!
//! Every on-disk pointer references a *logical* segment id. The LST maps each
//! logical id to the physical location of the segment and stores the
//! per-segment metadata that changes over time (usage, class, ...). Moving a
//! whole segment (GC, tiering, device evacuation) only requires changing
//! `phys_location` here.
//!
//! On-disk copy format: `LstHeader` followed by `nr_entries` fixed-size
//! `LstEntry` records, padded to the block size.

use serde::{ Serialize, Deserialize };
use pub_fields::pub_fields;

use std::io::{ Error, ErrorKind, Result };

pub const LST_MAGIC: [u8; 4] = *b"ALST";

/// What a logical segment currently stores.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LstEntryType {
    /// Standalone inode objects (block granularity). Interim until inodes
    /// live in the B+ tree.
    Inode,
    /// File data extents (block granularity).
    Extent,
    /// Logical id retired, physical segment awaiting reclamation. Reserved
    /// for GC/relocation; never produced while GC is disabled.
    Dead,
    /// Unused logical id.
    Free,
    /// B+ tree nodes (chunk granularity).
    Node,
}

impl LstEntryType {
    pub fn in_use(&self) -> bool { !matches!(self, LstEntryType::Free | LstEntryType::Dead) }
}

#[pub_fields]
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LstEntry {
    /// Physical byte offset of the segment (meaningless when `Free`).
    phys_location: u64,
    entry_type: LstEntryType,
    /// Placement class (decides stream and GC policy).
    class_id: u32,
    /// Allocation unit of the segment = `1 << unit_shift` bytes.
    unit_shift: u8,
    /// Allocated units, excluding the reserved header unit.
    used_units: u64,
    /// Generation in which the segment was opened.
    birth_gen: u64,
    /// crc32c of this segment's bitmap in the SUT copy paired with this LST copy.
    usage_crc: u32,
}

impl LstEntry {
    pub fn new() -> Self {
        Self {
            phys_location: 0,
            entry_type: LstEntryType::Free,
            class_id: 0,
            unit_shift: 0,
            used_units: 0,
            birth_gen: 0,
            usage_crc: 0,
        }
    }

    pub fn encoded_size() -> u64 {
        bincode::serialized_size(&LstEntry::new()).expect("LstEntry is serializable")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct LstHeader {
    magic: [u8; 4],
    generation: u64,
    nr_entries: u32,
    crc: u32,
}

/// Size reserved for the LST header (fixed, so entries start at a known offset).
pub const LST_HEADER_SIZE: u64 = 64;

#[derive(Debug, Clone)]
pub struct Lst {
    pub entries: Vec<LstEntry>,
}

impl Lst {

    pub fn new(nr_segments: u32) -> Self {
        Self { entries: vec![LstEntry::new(); nr_segments as usize] }
    }

    pub fn len(&self) -> usize { self.entries.len() }

    pub fn get(&self, seg: u32) -> Option<&LstEntry> { self.entries.get(seg as usize) }

    pub fn get_mut(&mut self, seg: u32) -> Option<&mut LstEntry> { self.entries.get_mut(seg as usize) }

    /// Serializes a full LST copy, padded to `copy_size` bytes.
    pub fn encode(&self, generation: u64, copy_size: u64) -> Result<Vec<u8>> {
        let mut body: Vec<u8> = Vec::with_capacity(self.entries.len() * LstEntry::encoded_size() as usize);
        for entry in &self.entries {
            body.extend(bincode::serialize(entry).map_err(Error::other)?);
        }

        let header = LstHeader {
            magic: LST_MAGIC,
            generation,
            nr_entries: self.entries.len() as u32,
            crc: crc32c::crc32c(&body),
        };

        let mut buf = bincode::serialize(&header).map_err(Error::other)?;
        buf.resize(LST_HEADER_SIZE as usize, 0);
        buf.extend(body);

        if buf.len() as u64 > copy_size {
            return Err(Error::other("LST does not fit in its reserved area"));
        }
        buf.resize(copy_size as usize, 0);
        Ok(buf)
    }

    /// Decodes an LST copy, checking magic, generation, size and checksum.
    pub fn decode(buf: &[u8], expected_gen: u64, nr_segments: u32) -> Result<Self> {
        let corrupted = |msg: &str| Error::new(ErrorKind::InvalidData, format!("LST: {}", msg));

        let header: LstHeader = bincode::deserialize(buf).map_err(Error::other)?;
        if header.magic != LST_MAGIC { return Err(corrupted("bad magic")); }
        if header.generation != expected_gen { return Err(corrupted("generation mismatch")); }
        if header.nr_entries != nr_segments { return Err(corrupted("entry count mismatch")); }

        let entry_size = LstEntry::encoded_size() as usize;
        let start = LST_HEADER_SIZE as usize;
        let end = start + entry_size * nr_segments as usize;
        if end > buf.len() { return Err(corrupted("truncated table")); }

        let body = &buf[start..end];
        if crc32c::crc32c(body) != header.crc { return Err(corrupted("checksum mismatch")); }

        let mut entries = Vec::with_capacity(nr_segments as usize);
        for raw in body.chunks_exact(entry_size) {
            entries.push(bincode::deserialize::<LstEntry>(raw).map_err(Error::other)?);
        }

        Ok(Self { entries })
    }
}
