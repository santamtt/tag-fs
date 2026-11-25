//! Segment management (no-GC allocator).
//!
//! - Every segment starts with a write-once `SegmentHeader` in its first
//!   allocation unit. All mutable per-segment state lives outside the
//!   segment: in the LST (usage counters, class, location) and in the SUT
//!   (allocation bitmap), both double buffered and committed at checkpoint.
//! - Allocation policy (GC disabled):
//!     1. allocate from the active segment of the stream `(kind, class)`;
//!     2. otherwise best-fit among the stream's segments, reusing holes;
//!     3. otherwise open a new segment (free logical id + free physical slot);
//!     4. otherwise ask the class GC policy (`Off` => out of space).
//! - Frees are deferred: freed units become allocatable only after the
//!   checkpoint that records them is durable. Fully empty, non-active
//!   segments are released back to the free pool at checkpoint.
//!
//! Extension points for GC/relocation: `GcPolicy`, step 4 of `allocate`,
//! the `Dead` LST state, and the logical/physical split (`free_logical` /
//! `free_phys`) which already allows moving a segment by changing its
//! `phys_location` in the LST.

use std::collections::{ BTreeMap, BTreeSet };
use std::io::{ Error, ErrorKind, Result };

use serde::{ Serialize, Deserialize };
use pub_fields::pub_fields;

use crate::storage::lst::*;
use crate::storage::superblock::SuperBlock;

pub const SEG_MAGIC: [u8; 4] = *b"ASEG";

/// Placement class: every object belongs to exactly one class, which decides
/// its write stream and its GC policy.
pub type ClassId = u32;
pub const DEFAULT_CLASS: ClassId = 0;

/// GC policy of a placement class. Only `Off` is implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GcPolicy {
    /// Never relocate: space is reclaimed only by reusing holes.
    Off,
    /// Relocate only on explicit user request (not implemented yet).
    Manual,
    /// Background relocation driven by thresholds (not implemented yet).
    Auto,
}

/* ----------------------------- object ids ------------------------------ */

/// Object ids are `(logical segment id << 32) | unit index`. Unit 0 of every
/// segment holds the segment header, so id 0 is never a valid object.
pub fn make_obj_id(seg: u32, unit: u64) -> u64 {
    debug_assert!(unit <= u32::MAX as u64);
    ((seg as u64) << 32) | unit
}

pub fn split_obj_id(id: u128) -> Result<(u32, u64)> {
    if id >> 64 != 0 {
        return Err(Error::new(ErrorKind::InvalidInput, format!("object id {:#x} out of range", id)));
    }
    let id = id as u64;
    let unit = id & 0xFFFF_FFFF;
    if unit == 0 {
        return Err(Error::new(ErrorKind::InvalidInput, format!("object id {:#x} is not valid", id)));
    }
    Ok(((id >> 32) as u32, unit))
}

/* ---------------------------- segment header --------------------------- */

#[pub_fields]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SegmentHeader {
    magic: [u8; 4],
    logical_id: u32,
    kind: LstEntryType,
    class_id: ClassId,
    unit_shift: u8,
    birth_gen: u64,
    checksum: u32,
}

impl SegmentHeader {

    pub fn new(logical_id: u32, kind: LstEntryType, class_id: ClassId, unit_shift: u8, birth_gen: u64) -> Self {
        Self { magic: SEG_MAGIC, logical_id, kind, class_id, unit_shift, birth_gen, checksum: 0 }
    }

    pub fn encode(&self, block_size: u64) -> Result<Vec<u8>> {
        let mut hdr = self.clone();
        hdr.checksum = 0;
        hdr.checksum = crc32c::crc32c(&bincode::serialize(&hdr).map_err(Error::other)?);

        let mut buf = bincode::serialize(&hdr).map_err(Error::other)?;
        buf.resize(block_size as usize, 0);
        Ok(buf)
    }

    pub fn decode(buf: &[u8]) -> Option<Self> {
        let mut hdr: SegmentHeader = bincode::deserialize(buf).ok()?;
        if hdr.magic != SEG_MAGIC { return None; }

        let stored = hdr.checksum;
        hdr.checksum = 0;
        if crc32c::crc32c(&bincode::serialize(&hdr).ok()?) != stored { return None; }

        hdr.checksum = stored;
        Some(hdr)
    }
}

/* ----------------------------- usage bitmap ---------------------------- */

/// One bit per allocation unit of a segment (1 = allocated).
#[derive(Debug, Clone)]
pub struct UsageBitmap {
    words: Vec<u64>,
    units: u64,
}

impl UsageBitmap {

    pub fn new(units: u64) -> Self {
        Self { words: vec![0u64; units.div_ceil(64) as usize], units }
    }

    pub fn units(&self) -> u64 { self.units }

    pub fn get(&self, i: u64) -> bool {
        i < self.units && (self.words[(i / 64) as usize] >> (i % 64)) & 1 == 1
    }

    pub fn set_range(&mut self, start: u64, len: u64, value: bool) {
        debug_assert!(start + len <= self.units);
        for i in start..start + len {
            let word = &mut self.words[(i / 64) as usize];
            if value { *word |= 1 << (i % 64); } else { *word &= !(1 << (i % 64)); }
        }
    }

    pub fn all_set(&self, start: u64, len: u64) -> bool {
        start + len <= self.units && (start..start + len).all(|i| self.get(i))
    }

    pub fn count_ones(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }

    /// Little-endian serialization padded to `size` bytes.
    pub fn to_bytes(&self, size: u64) -> Vec<u8> {
        let mut buf: Vec<u8> = self.words.iter().flat_map(|w| w.to_le_bytes()).collect();
        buf.resize(size as usize, 0);
        buf
    }

    pub fn from_bytes(bytes: &[u8], units: u64) -> Result<Self> {
        let nr_words = units.div_ceil(64) as usize;
        if bytes.len() < nr_words * 8 {
            return Err(Error::new(ErrorKind::InvalidData, "usage bitmap truncated"));
        }

        let words: Vec<u64> = bytes[..nr_words * 8]
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        let bitmap = Self { words, units };

        // Bits past the end of the segment must be zero.
        let tail = nr_words as u64 * 64;
        if (units..tail).any(|i| (bitmap.words[(i / 64) as usize] >> (i % 64)) & 1 == 1) {
            return Err(Error::new(ErrorKind::InvalidData, "usage bitmap has bits past segment end"));
        }
        Ok(bitmap)
    }
}

/* ------------------------------ free space ----------------------------- */

/// Free extents of one segment, in allocation units, indexed both by start
/// (for coalescing) and by size (for best-fit).
#[derive(Debug, Clone, Default)]
pub struct FreeSpace {
    by_start: BTreeMap<u64, u64>,
    by_size: BTreeSet<(u64, u64)>,
}

impl FreeSpace {

    pub fn new() -> Self { Self::default() }

    pub fn from_bitmap(bitmap: &UsageBitmap) -> Self {
        let mut free = Self::new();
        let mut run_start: Option<u64> = None;

        for i in 0..bitmap.units() {
            match (bitmap.get(i), run_start) {
                (false, None) => run_start = Some(i),
                (true, Some(start)) => { free.insert(start, i - start); run_start = None; },
                _ => {}
            }
        }
        if let Some(start) = run_start { free.insert(start, bitmap.units() - start); }

        free
    }

    fn remove(&mut self, start: u64, len: u64) {
        self.by_start.remove(&start);
        self.by_size.remove(&(len, start));
    }

    /// Inserts a free extent, merging it with adjacent ones.
    pub fn insert(&mut self, mut start: u64, mut len: u64) {
        if len == 0 { return; }

        if let Some((&prev_start, &prev_len)) = self.by_start.range(..start).next_back() {
            debug_assert!(prev_start + prev_len <= start, "overlapping free extents");
            if prev_start + prev_len == start {
                self.remove(prev_start, prev_len);
                start = prev_start;
                len += prev_len;
            }
        }

        if let Some(&next_len) = self.by_start.get(&(start + len)) {
            self.remove(start + len, next_len);
            len += next_len;
        }

        self.by_start.insert(start, len);
        self.by_size.insert((len, start));
    }

    /// Best fit: smallest free extent that can hold `len` units.
    pub fn alloc_best_fit(&mut self, len: u64) -> Option<u64> {
        let &(size, start) = self.by_size.range((len, 0)..).next()?;
        self.remove(start, size);
        if size > len { self.insert(start + len, size - len); }
        Some(start)
    }

    pub fn largest(&self) -> u64 { self.by_size.last().map(|&(size, _)| size).unwrap_or(0) }

    pub fn total(&self) -> u64 { self.by_start.values().sum() }

    pub fn nr_extents(&self) -> usize { self.by_start.len() }
}

/* --------------------------- segment manager --------------------------- */

#[derive(Debug)]
struct SegRuntime {
    bitmap: UsageBitmap,
    free: FreeSpace,
}

/// Result of an allocation: `units` units starting at `unit` in logical
/// segment `seg`, physically at byte `phys`.
#[derive(Debug, Clone)]
pub struct Allocation {
    pub seg: u32,
    pub unit: u64,
    pub units: u64,
    pub unit_shift: u8,
    pub phys: u64,
    /// Set when the allocation opened a new segment: its header must be
    /// written at the given physical offset before the next checkpoint.
    pub new_segment: Option<(u64, SegmentHeader)>,
}

/// Work to do on disk for a checkpoint: bitmaps to write into the SUT copy
/// of the new generation (the LST is always written in full).
#[derive(Debug, Default)]
pub struct CommitPlan {
    pub bitmaps: Vec<(u32, Vec<u8>)>,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    nr_segments: u32,
    seg_size: u64,
    data_start: u64,
    block_shift: u8,
    chunk_shift: u8,
    sut_seg_bytes: u64,
}

impl Geometry {
    fn from_sb(sb: &SuperBlock) -> Self {
        Self {
            nr_segments: sb.nr_segments,
            seg_size: sb.seg_size,
            data_start: sb.data_start,
            block_shift: sb.block_shift(),
            chunk_shift: sb.chunk_shift(),
            sut_seg_bytes: sb.sut_seg_bytes,
        }
    }

    fn phys_of_slot(&self, slot: u32) -> u64 { self.data_start + slot as u64 * self.seg_size }

    fn slot_of_phys(&self, phys: u64) -> Option<u32> {
        if phys < self.data_start || (phys - self.data_start) % self.seg_size != 0 { return None; }
        let slot = (phys - self.data_start) / self.seg_size;
        if slot >= self.nr_segments as u64 { return None; }
        Some(slot as u32)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SpaceStats {
    pub segments_in_use: u32,
    pub segments_free: u32,
    pub used_bytes: u64,
    pub free_bytes_in_segments: u64,
}

type StreamKey = (LstEntryType, ClassId);

#[derive(Debug)]
pub struct SegmentManager {
    geo: Geometry,
    lst: Lst,
    rt: Vec<Option<SegRuntime>>,

    free_logical: BTreeSet<u32>,
    free_phys: BTreeSet<u32>,

    /// (kind, class, largest free extent, seg): replaces the old SegTree.
    holes: BTreeSet<(LstEntryType, ClassId, u64, u32)>,
    /// Active (currently filled) segment of each stream.
    active: BTreeMap<StreamKey, u32>,
    policies: BTreeMap<ClassId, GcPolicy>,

    /// Frees waiting for the next checkpoint.
    pending_free: Vec<(u32, u64, u64)>,
    /// Frees recorded by the checkpoint in progress.
    committing_free: Vec<(u32, u64, u64)>,
    /// Segments released by the checkpoint in progress: (seg, slot, kind, class).
    committing_release: Vec<(u32, u32, LstEntryType, ClassId)>,

    /// Segments whose bitmap changed since the last checkpoint / in the previous one.
    dirty_cur: BTreeSet<u32>,
    dirty_prev: BTreeSet<u32>,
}

impl SegmentManager {

    /// Manager for a freshly formatted device (every segment free).
    pub fn new(sb: &SuperBlock) -> Self {
        let geo = Geometry::from_sb(sb);
        let n = geo.nr_segments;

        Self {
            geo,
            lst: Lst::new(n),
            rt: (0..n).map(|_| None).collect(),
            free_logical: (0..n).collect(),
            free_phys: (0..n).collect(),
            holes: BTreeSet::new(),
            active: BTreeMap::new(),
            policies: BTreeMap::from([(DEFAULT_CLASS, GcPolicy::Off)]),
            pending_free: Vec::new(),
            committing_free: Vec::new(),
            committing_release: Vec::new(),
            dirty_cur: BTreeSet::new(),
            dirty_prev: BTreeSet::new(),
        }
    }

    /// Rebuilds the in-memory state from a committed LST copy.
    /// `read_bitmap(seg)` must return the SUT bitmap of `seg` from the copy
    /// paired with that LST.
    pub fn load<F>(sb: &SuperBlock, lst: Lst, mut read_bitmap: F) -> Result<Self>
    where F: FnMut(u32) -> Result<Vec<u8>> {

        let corrupted = |seg: u32, msg: &str| Error::new(
            ErrorKind::InvalidData, format!("segment {}: {}", seg, msg));

        let mut mgr = Self::new(sb);
        if lst.len() != mgr.geo.nr_segments as usize {
            return Err(Error::new(ErrorKind::InvalidData, "LST size does not match superblock"));
        }
        mgr.lst = lst;

        for seg in 0..mgr.geo.nr_segments {
            let entry = mgr.lst.entries[seg as usize].clone();

            if entry.entry_type == LstEntryType::Free { continue; }
            mgr.free_logical.remove(&seg);

            let slot = mgr.geo.slot_of_phys(entry.phys_location)
                .ok_or_else(|| corrupted(seg, "invalid physical location"))?;
            if !mgr.free_phys.remove(&slot) {
                return Err(corrupted(seg, "physical slot used twice"));
            }

            if entry.entry_type == LstEntryType::Dead { continue; }

            if entry.unit_shift != mgr.unit_shift_for(entry.entry_type) {
                return Err(corrupted(seg, "unexpected allocation unit"));
            }

            let bytes = read_bitmap(seg)?;
            if crc32c::crc32c(&bytes) != entry.usage_crc {
                return Err(corrupted(seg, "usage bitmap checksum mismatch"));
            }

            let bitmap = UsageBitmap::from_bytes(&bytes, mgr.units_per_segment(entry.unit_shift))?;
            if !bitmap.get(0) || bitmap.count_ones() - 1 != entry.used_units {
                return Err(corrupted(seg, "usage bitmap does not match LST counters"));
            }

            let free = FreeSpace::from_bitmap(&bitmap);
            mgr.holes.insert((entry.entry_type, entry.class_id, free.largest(), seg));
            mgr.rt[seg as usize] = Some(SegRuntime { bitmap, free });

            // A crash may have left the inactive SUT copy half written: the
            // first checkpoint after mount rewrites every bitmap.
            mgr.dirty_prev.insert(seg);
        }

        Ok(mgr)
    }

    pub fn lst(&self) -> &Lst { &self.lst }

    pub fn unit_shift_for(&self, kind: LstEntryType) -> u8 {
        match kind {
            LstEntryType::Node => self.geo.chunk_shift,
            _ => self.geo.block_shift,
        }
    }

    pub fn units_per_segment(&self, unit_shift: u8) -> u64 { self.geo.seg_size >> unit_shift }

    pub fn policy(&self, class: ClassId) -> GcPolicy {
        self.policies.get(&class).copied().unwrap_or(GcPolicy::Off)
    }

    pub fn set_policy(&mut self, class: ClassId, policy: GcPolicy) { self.policies.insert(class, policy); }

    pub fn stats(&self) -> SpaceStats {
        let mut stats = SpaceStats::default();
        stats.segments_free = self.free_logical.len().min(self.free_phys.len()) as u32;

        for (seg, rt) in self.rt.iter().enumerate() {
            if let Some(rt) = rt {
                let shift = self.lst.entries[seg].unit_shift;
                stats.segments_in_use += 1;
                stats.used_bytes += self.lst.entries[seg].used_units << shift;
                stats.free_bytes_in_segments += rt.free.total() << shift;
            }
        }
        stats
    }

    /// Physical location of unit `unit` of segment `seg`, checking that it
    /// is allocated. Returns `(phys, unit_shift, kind)`.
    pub fn locate(&self, seg: u32, unit: u64) -> Result<(u64, u8, LstEntryType)> {
        let not_found = || Error::new(ErrorKind::NotFound,
            format!("no allocated object at segment {} unit {}", seg, unit));

        let entry = self.lst.get(seg).ok_or_else(not_found)?;
        let rt = self.rt.get(seg as usize).and_then(|r| r.as_ref()).ok_or_else(not_found)?;
        if unit == 0 || !rt.bitmap.get(unit) { return Err(not_found()); }

        Ok((entry.phys_location + (unit << entry.unit_shift), entry.unit_shift, entry.entry_type))
    }

    pub fn is_allocated(&self, seg: u32, unit: u64, units: u64) -> bool {
        match self.rt.get(seg as usize) {
            Some(Some(rt)) => unit > 0 && rt.bitmap.all_set(unit, units),
            _ => false,
        }
    }

    fn reindex_holes(&mut self, seg: u32, old_largest: u64) {
        let entry = &self.lst.entries[seg as usize];
        let (kind, class) = (entry.entry_type, entry.class_id);
        let new_largest = self.rt[seg as usize].as_ref().map(|rt| rt.free.largest()).unwrap_or(0);

        self.holes.remove(&(kind, class, old_largest, seg));
        self.holes.insert((kind, class, new_largest, seg));
    }

    fn try_alloc_in(&mut self, seg: u32, units: u64) -> Option<Allocation> {
        let rt = self.rt[seg as usize].as_mut()?;
        let old_largest = rt.free.largest();
        let unit = rt.free.alloc_best_fit(units)?;
        rt.bitmap.set_range(unit, units, true);

        let entry = &mut self.lst.entries[seg as usize];
        entry.used_units += units;
        let (phys_location, unit_shift) = (entry.phys_location, entry.unit_shift);

        self.reindex_holes(seg, old_largest);
        self.dirty_cur.insert(seg);

        Some(Allocation {
            seg,
            unit,
            units,
            unit_shift,
            phys: phys_location + (unit << unit_shift),
            new_segment: None,
        })
    }

    fn open_segment(&mut self, kind: LstEntryType, class: ClassId, birth_gen: u64) -> Option<(u32, SegmentHeader)> {
        let seg = *self.free_logical.first()?;
        let slot = *self.free_phys.first()?;
        self.free_logical.remove(&seg);
        self.free_phys.remove(&slot);

        let unit_shift = self.unit_shift_for(kind);
        let units = self.units_per_segment(unit_shift);

        let entry = &mut self.lst.entries[seg as usize];
        entry.phys_location = self.geo.phys_of_slot(slot);
        entry.entry_type = kind;
        entry.class_id = class;
        entry.unit_shift = unit_shift;
        entry.used_units = 0;
        entry.birth_gen = birth_gen;
        entry.usage_crc = 0;

        // Unit 0 is reserved for the segment header.
        let mut bitmap = UsageBitmap::new(units);
        bitmap.set_range(0, 1, true);
        let mut free = FreeSpace::new();
        free.insert(1, units - 1);

        self.holes.insert((kind, class, free.largest(), seg));
        self.rt[seg as usize] = Some(SegRuntime { bitmap, free });
        self.active.insert((kind, class), seg);
        self.dirty_cur.insert(seg);

        Some((seg, SegmentHeader::new(seg, kind, class, unit_shift, birth_gen)))
    }

    /// Allocates `units` contiguous units for an object of type `kind` in
    /// placement class `class`. `birth_gen` is recorded if a segment is opened.
    pub fn allocate(&mut self, kind: LstEntryType, class: ClassId, units: u64, birth_gen: u64) -> Result<Allocation> {
        if !kind.in_use() {
            return Err(Error::new(ErrorKind::InvalidInput, "cannot allocate in a free/dead segment"));
        }

        let max_units = self.units_per_segment(self.unit_shift_for(kind)) - 1;
        if units == 0 || units > max_units {
            return Err(Error::new(ErrorKind::InvalidInput,
                format!("cannot allocate {} units (max {} per segment)", units, max_units)));
        }

        // 1. active segment of the stream
        if let Some(&seg) = self.active.get(&(kind, class)) {
            if let Some(alloc) = self.try_alloc_in(seg, units) { return Ok(alloc); }
        }

        // 2. best fit among the stream's segments (hole reuse)
        let candidate = self.holes
            .range((kind, class, units, 0)..=(kind, class, u64::MAX, u32::MAX))
            .next()
            .map(|&(_, _, _, seg)| seg);

        if let Some(seg) = candidate {
            let alloc = self.try_alloc_in(seg, units).expect("hole index out of sync");
            self.active.insert((kind, class), seg);
            return Ok(alloc);
        }

        // 3. new segment
        if let Some((seg, header)) = self.open_segment(kind, class, birth_gen) {
            let mut alloc = self.try_alloc_in(seg, units).expect("fresh segment must fit");
            alloc.new_segment = Some((self.lst.entries[seg as usize].phys_location, header));
            return Ok(alloc);
        }

        // 4. out of space: this is where a GC policy would kick in.
        match self.policy(class) {
            GcPolicy::Off => Err(Error::new(ErrorKind::StorageFull,
                format!("no space left for {} units of {:?} (class {}, GC off)", units, kind, class))),
            GcPolicy::Manual | GcPolicy::Auto => Err(Error::new(ErrorKind::StorageFull,
                "no space left and GC is not implemented yet")),
        }
    }

    /// Undoes an allocation that was never committed (e.g. the write failed).
    pub fn abort_allocation(&mut self, alloc: &Allocation) {
        let Some(rt) = self.rt[alloc.seg as usize].as_mut() else { return; };
        let old_largest = rt.free.largest();
        rt.bitmap.set_range(alloc.unit, alloc.units, false);
        rt.free.insert(alloc.unit, alloc.units);
        self.lst.entries[alloc.seg as usize].used_units -= alloc.units;
        self.reindex_holes(alloc.seg, old_largest);
    }

    /// Frees `units` units at `unit` of `seg`. The space is reusable only
    /// after the next checkpoint is durable.
    pub fn free(&mut self, seg: u32, unit: u64, units: u64) -> Result<()> {
        if units == 0 || !self.is_allocated(seg, unit, units) {
            return Err(Error::new(ErrorKind::InvalidInput,
                format!("freeing unallocated range: segment {} unit {} len {}", seg, unit, units)));
        }

        let overlaps = self.pending_free.iter().any(|&(s, u, n)|
            s == seg && unit < u + n && u < unit + units);
        if overlaps {
            return Err(Error::new(ErrorKind::InvalidInput, "double free"));
        }

        self.pending_free.push((seg, unit, units));
        Ok(())
    }

    /// First phase of a checkpoint: applies deferred frees and segment
    /// releases to the persistent image and returns the bitmaps to write.
    /// Must be followed by `finish_commit` once the checkpoint is durable.
    pub fn prepare_commit(&mut self) -> CommitPlan {
        for (seg, unit, units) in std::mem::take(&mut self.pending_free) {
            let rt = self.rt[seg as usize].as_mut().expect("pending free in unused segment");
            rt.bitmap.set_range(unit, units, false);
            self.lst.entries[seg as usize].used_units -= units;
            self.dirty_cur.insert(seg);
            self.committing_free.push((seg, unit, units));
        }

        // Release empty segments that are not being filled.
        let active: BTreeSet<u32> = self.active.values().copied().collect();
        for seg in 0..self.geo.nr_segments {
            let entry = &mut self.lst.entries[seg as usize];
            if self.rt[seg as usize].is_none() || entry.used_units != 0 || active.contains(&seg) { continue; }

            let slot = self.geo.slot_of_phys(entry.phys_location).expect("valid physical location");
            self.committing_release.push((seg, slot, entry.entry_type, entry.class_id));
            *entry = LstEntry::new();
        }

        let mut plan = CommitPlan::default();
        let targets: BTreeSet<u32> = self.dirty_cur.union(&self.dirty_prev).copied().collect();
        for seg in targets {
            let entry = &mut self.lst.entries[seg as usize];
            if !entry.entry_type.in_use() { continue; }

            let rt = self.rt[seg as usize].as_ref().expect("in-use segment without runtime state");
            let bytes = rt.bitmap.to_bytes(self.geo.sut_seg_bytes);
            entry.usage_crc = crc32c::crc32c(&bytes);
            plan.bitmaps.push((seg, bytes));
        }

        plan
    }

    /// Second phase of a checkpoint: the freed space becomes allocatable.
    pub fn finish_commit(&mut self) {
        for (seg, slot, kind, class) in std::mem::take(&mut self.committing_release) {
            if let Some(rt) = self.rt[seg as usize].take() {
                self.holes.remove(&(kind, class, rt.free.largest(), seg));
            }
            self.free_logical.insert(seg);
            self.free_phys.insert(slot);
        }

        for (seg, unit, units) in std::mem::take(&mut self.committing_free) {
            let Some(rt) = self.rt[seg as usize].as_mut() else { continue; };
            let old_largest = rt.free.largest();
            rt.free.insert(unit, units);
            self.reindex_holes(seg, old_largest);
        }

        self.dirty_prev = std::mem::take(&mut self.dirty_cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_best_fit_and_coalescing() {
        let mut fs = FreeSpace::new();
        fs.insert(10, 5);
        fs.insert(30, 2);
        fs.insert(15, 5); // merges with [10, 15)
        assert_eq!(fs.nr_extents(), 2);
        assert_eq!(fs.largest(), 10);

        assert_eq!(fs.alloc_best_fit(2), Some(30)); // exact fit preferred
        assert_eq!(fs.alloc_best_fit(4), Some(10));
        assert_eq!(fs.total(), 6);
        assert_eq!(fs.alloc_best_fit(7), None);
    }

    #[test]
    fn bitmap_roundtrip_and_free_space() {
        let mut bm = UsageBitmap::new(100);
        bm.set_range(0, 1, true);
        bm.set_range(10, 20, true);
        let bytes = bm.to_bytes(4096);
        let bm2 = UsageBitmap::from_bytes(&bytes, 100).unwrap();
        assert_eq!(bm2.count_ones(), 21);

        let fs = FreeSpace::from_bitmap(&bm2);
        assert_eq!(fs.total(), 79);
        assert_eq!(fs.largest(), 70);
    }

    #[test]
    fn obj_id_roundtrip() {
        let id = make_obj_id(7, 42);
        assert_eq!(split_obj_id(id as u128).unwrap(), (7, 42));
        assert!(split_obj_id(0).is_err());
        assert!(split_obj_id(1u128 << 64).is_err());
    }
}
