//! Device I/O, checkpoints and the object interface used by `TagFs`.
//!
//! Objects (currently only inodes) are stored in segments as
//! `ObjHeader | bincode payload`, rounded up to the segment allocation unit.
//! Their id is their logical address `(logical segment, unit)`, see
//! `segment::make_obj_id`.
//!
//! Checkpoint protocol (generation g -> g+1, copy c = (g+1) & 1):
//!   1. object data and new segment headers are already written;
//!   2. write dirty SUT bitmaps and the full LST into copy c;
//!   3. flush;
//!   4. write the superblock with generation g+1 into slot c;
//!   5. flush;
//!   6. deferred frees and released segments become allocatable.
//! A crash at any point leaves the previous generation intact.
//!
//! Interim limitation: until inodes live in the CoW B+ tree, `commit`
//! overwrites an object in place (its id must stay stable). The object
//! checksum detects torn writes but the old version is not preserved.

use std::fs::File;
use std::io::{ Error, ErrorKind, Result, Seek, SeekFrom };
use std::os::unix::fs::FileExt;

use serde::{ Serialize, Deserialize };
use serde::de::DeserializeOwned;

use crate::storage::superblock::*;
use crate::storage::inode::*;
use crate::storage::lst::*;
use crate::storage::segment::*;

const OBJ_MAGIC: u32 = u32::from_le_bytes(*b"AOBJ");

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ObjHeader {
    magic: u32,
    /// Payload length in bytes.
    len: u32,
    /// Allocated units (header + payload + padding).
    units: u32,
    /// crc32c of the payload.
    crc: u32,
    /// Generation of the last write.
    generation: u64,
}

const OBJ_HEADER_SIZE: u64 = 24;

#[derive(Debug)]
pub struct Alloc {
    storage_dev: File,
    sup_block: SuperBlock,
    segs: SegmentManager,
}

fn read_vec(dev: &File, pos: u64, size: u64) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; size as usize];
    dev.read_exact_at(&mut buf, pos)?;
    Ok(buf)
}

fn corrupted(msg: String) -> Error { Error::new(ErrorKind::InvalidData, msg) }

impl Alloc {

    /// Opens the device. `enable_trunc` is kept for interface compatibility
    /// but ignored: truncating an image file would shrink it to zero bytes.
    pub fn new(dev_name: &str, enable_trunc: bool) -> Self {
        let _ = enable_trunc;

        let open = File::options()
            .read(true)
            .write(true)
            .open(dev_name);

        if open.is_err() {
            panic!("something went wrong when the device was opened: {}", open.err().unwrap());
        }

        let sup_block = SuperBlock::empty();
        let segs = SegmentManager::new(&sup_block);

        Self {
            storage_dev: open.unwrap(),
            sup_block,
            segs,
        }
    }

    pub fn mount(dev_name: &str) -> Result<Self> {
        let mut alloc = Self::new(dev_name, false);
        let sb = alloc.read_superblock()?;

        if alloc.device_size()? < sb.disk_size {
            return Err(Error::new(ErrorKind::InvalidInput, "device is smaller than the formatted filesystem"));
        }

        let copy = SuperBlock::copy_for(sb.generation);
        let dev = &alloc.storage_dev;

        let lst_buf = read_vec(dev, sb.sgn_table_loc[copy], sb.sgn_table_size)?;
        let lst = Lst::decode(&lst_buf, sb.generation, sb.nr_segments)?;

        let segs = SegmentManager::load(&sb, lst, |seg| {
            read_vec(dev, sb.sut_loc[copy] + seg as u64 * sb.sut_seg_bytes, sb.sut_seg_bytes)
        })?;

        // Cross-check the write-once segment headers against the LST.
        for (seg, entry) in segs.lst().entries.iter().enumerate() {
            if !entry.entry_type.in_use() { continue; }

            let buf = read_vec(dev, entry.phys_location, sb.block_size)?;
            let expected = SegmentHeader::new(seg as u32, entry.entry_type, entry.class_id, entry.unit_shift, entry.birth_gen);
            match SegmentHeader::decode(&buf) {
                Some(mut hdr) => {
                    hdr.checksum = 0;
                    if hdr != expected { return Err(corrupted(format!("segment {}: header does not match LST", seg))); }
                },
                None => return Err(corrupted(format!("segment {}: invalid header", seg))),
            }
        }

        alloc.sup_block = sb;
        alloc.segs = segs;
        Ok(alloc)
    }

    fn device_size(&mut self) -> Result<u64> {
        // Works for both regular files and block devices.
        self.storage_dev.seek(SeekFrom::End(0))
    }

    fn ensure_mounted(&self) -> Result<()> {
        if self.sup_block.check_magic() { Ok(()) } else {
            Err(Error::new(ErrorKind::Other, "filesystem is not formatted/mounted"))
        }
    }

    /// Writes `superblock` into the slot of its generation and flushes it.
    pub fn save_superblock(&mut self, superblock: &SuperBlock) -> Result<()> {
        let buf = superblock.encode()?;
        self.storage_dev.write_all_at(&buf, SuperBlock::slot_for(superblock.generation))?;
        self.storage_dev.sync_data()
    }

    /// Returns the valid superblock with the highest generation.
    pub fn read_superblock(&mut self) -> Result<SuperBlock> {
        let mut best: Option<SuperBlock> = None;

        for slot in 0..SB_SLOTS {
            let buf = read_vec(&self.storage_dev, slot * SB_SLOT_SIZE, SB_SLOT_SIZE)?;
            if let Some(sb) = SuperBlock::decode(&buf) {
                if SuperBlock::slot_for(sb.generation) != slot * SB_SLOT_SIZE { continue; }
                if best.as_ref().is_none_or(|b| sb.generation > b.generation) { best = Some(sb); }
            }
        }

        best.ok_or_else(|| Error::new(ErrorKind::InvalidInput, "Filesystem magic number did not match"))
    }

    pub fn format_disk(&mut self) -> Result<()> {
        self.format_disk_with(FormatOptions::default())
    }

    pub fn format_disk_with(&mut self, opts: FormatOptions) -> Result<()> {
        let disk_size = self.device_size()?;
        let sb = SuperBlock::compute_layout(disk_size, &opts)?;

        // Zero both superblock slots, LST and SUT copies.
        let zeros = vec![0u8; 1 << 20];
        let mut pos = 0;
        while pos < sb.data_start {
            let len = (sb.data_start - pos).min(zeros.len() as u64);
            self.storage_dev.write_all_at(&zeros[..len as usize], pos)?;
            pos += len;
        }

        // Generation 0: empty LST in copy 0, then the superblock.
        let segs = SegmentManager::new(&sb);
        let lst = segs.lst().encode(sb.generation, sb.sgn_table_size)?;
        self.storage_dev.write_all_at(&lst, sb.sgn_table_loc[SuperBlock::copy_for(sb.generation)])?;
        self.storage_dev.sync_data()?;
        self.save_superblock(&sb)?;

        self.sup_block = sb;
        self.segs = segs;
        Ok(())
    }

    /// Makes every change since the last checkpoint durable.
    pub fn checkpoint(&mut self) -> Result<()> {
        self.ensure_mounted()?;

        let generation = self.sup_block.generation + 1;
        let copy = SuperBlock::copy_for(generation);
        let plan = self.segs.prepare_commit();

        for (seg, bytes) in &plan.bitmaps {
            let pos = self.sup_block.sut_loc[copy] + *seg as u64 * self.sup_block.sut_seg_bytes;
            self.storage_dev.write_all_at(bytes, pos)?;
        }

        let lst = self.segs.lst().encode(generation, self.sup_block.sgn_table_size)?;
        self.storage_dev.write_all_at(&lst, self.sup_block.sgn_table_loc[copy])?;
        self.storage_dev.sync_data()?;

        let mut sb = self.sup_block.clone();
        sb.generation = generation;
        self.save_superblock(&sb)?;

        self.sup_block = sb;
        self.segs.finish_commit();
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> { self.checkpoint() }

    pub fn superblock(&self) -> &SuperBlock { &self.sup_block }

    pub fn space_stats(&self) -> SpaceStats { self.segs.stats() }

    /* ------------------------------ objects ------------------------------ */

    fn read_obj_header(&self, id: u128) -> Result<(u64, u8, ObjHeader)> {
        let (seg, unit) = split_obj_id(id)?;
        let (phys, unit_shift, _) = self.segs.locate(seg, unit)?;

        let buf = read_vec(&self.storage_dev, phys, OBJ_HEADER_SIZE)?;
        let hdr: ObjHeader = bincode::deserialize(&buf).map_err(Error::other)?;

        if hdr.magic != OBJ_MAGIC {
            return Err(corrupted(format!("object {:#x}: bad magic", id)));
        }
        if OBJ_HEADER_SIZE + hdr.len as u64 > (hdr.units as u64) << unit_shift
            || !self.segs.is_allocated(seg, unit, hdr.units as u64) {
            return Err(corrupted(format!("object {:#x}: inconsistent header", id)));
        }

        Ok((phys, unit_shift, hdr))
    }

    fn write_obj_at(&self, phys: u64, units: u64, payload: &[u8]) -> Result<()> {
        let hdr = ObjHeader {
            magic: OBJ_MAGIC,
            len: payload.len() as u32,
            units: units as u32,
            crc: crc32c::crc32c(payload),
            generation: self.sup_block.generation + 1,
        };

        let mut buf = bincode::serialize(&hdr).map_err(Error::other)?;
        debug_assert_eq!(buf.len() as u64, OBJ_HEADER_SIZE);
        buf.extend_from_slice(payload);
        self.storage_dev.write_all_at(&buf, phys)
    }

    /// Allocates space for `obj` and writes it, without checkpointing.
    fn store_obj<T: Serialize>(&mut self, kind: LstEntryType, class: ClassId, obj: &T) -> Result<u64> {
        self.ensure_mounted()?;

        let payload = bincode::serialize(obj).map_err(Error::other)?;
        let unit_shift = self.segs.unit_shift_for(kind);
        let units = (OBJ_HEADER_SIZE + payload.len() as u64).div_ceil(1 << unit_shift);

        let alloc = self.segs.allocate(kind, class, units, self.sup_block.generation + 1)?;

        let written = (|| -> Result<()> {
            if let Some((seg_phys, header)) = &alloc.new_segment {
                self.storage_dev.write_all_at(&header.encode(self.sup_block.block_size)?, *seg_phys)?;
            }
            self.write_obj_at(alloc.phys, alloc.units, &payload)
        })();

        if let Err(e) = written {
            self.segs.abort_allocation(&alloc);
            return Err(e);
        }

        Ok(make_obj_id(alloc.seg, alloc.unit))
    }

    /// Stores a new object of type `kind` and returns its id.
    pub fn register_obj<T: Serialize>(&mut self, kind: LstEntryType, class: ClassId, obj: &T) -> Result<u64> {
        let id = self.store_obj(kind, class, obj)?;
        self.checkpoint()?;
        Ok(id)
    }

    /// Stores a new inode and returns its id. The first inode registered on
    /// a fresh filesystem becomes the root.
    pub fn register_inode(&mut self, obj: &Inode) -> Result<u64> {
        let class = self.sup_block.default_class;
        let id = self.store_obj(LstEntryType::Inode, class, obj)?;

        if self.sup_block.root_id == NO_OBJECT { self.sup_block.root_id = id; }
        self.sup_block.nr_inodes += 1;

        self.checkpoint()?;
        Ok(id)
    }

    pub fn read_obj<T: DeserializeOwned>(&mut self, id: u128) -> Result<T> {
        self.ensure_mounted()?;
        let (phys, _, hdr) = self.read_obj_header(id)?;

        let payload = read_vec(&self.storage_dev, phys + OBJ_HEADER_SIZE, hdr.len as u64)?;
        if crc32c::crc32c(&payload) != hdr.crc {
            return Err(corrupted(format!("object {:#x}: checksum mismatch", id)));
        }

        bincode::deserialize(&payload).map_err(Error::other)
    }

    /// Rewrites object `obj_id` in place (see the module documentation).
    pub fn commit<T: Serialize>(&mut self, obj: &T, obj_id: u128) -> Result<()> {
        self.ensure_mounted()?;
        let (phys, unit_shift, hdr) = self.read_obj_header(obj_id)?;

        let payload = bincode::serialize(obj).map_err(Error::other)?;
        if OBJ_HEADER_SIZE + payload.len() as u64 > (hdr.units as u64) << unit_shift {
            return Err(Error::new(ErrorKind::Other,
                format!("object {:#x} grew beyond its allocation", obj_id)));
        }

        self.write_obj_at(phys, hdr.units as u64, &payload)?;
        self.storage_dev.sync_data()
    }

    /// Frees object `obj_id`; its space is reused after the checkpoint.
    pub fn free_obj(&mut self, obj_id: u128) -> Result<()> {
        self.ensure_mounted()?;
        if obj_id == self.sup_block.root_id as u128 {
            return Err(Error::new(ErrorKind::InvalidInput, "cannot free the root inode"));
        }

        let (_, _, hdr) = self.read_obj_header(obj_id)?;
        let (seg, unit) = split_obj_id(obj_id)?;
        let kind = self.segs.lst().get(seg).map(|e| e.entry_type);

        self.segs.free(seg, unit, hdr.units as u64)?;
        if kind == Some(LstEntryType::Inode) {
            self.sup_block.nr_inodes = self.sup_block.nr_inodes.saturating_sub(1);
        }

        self.checkpoint()
    }

    pub fn read_root(&mut self) -> Result<Inode> {
        if self.sup_block.root_id == NO_OBJECT {
            return Err(Error::new(ErrorKind::NotFound, "root inode not allocated"));
        }
        self.read_obj::<Inode>(self.sup_block.root_id as u128)
    }

    pub fn commit_root(&mut self, root: &Inode) -> Result<()> {
        if self.sup_block.root_id == NO_OBJECT {
            return Err(Error::new(ErrorKind::NotFound, "root inode not allocated"));
        }
        self.commit::<Inode>(root, self.sup_block.root_id as u128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{ AtomicU32, Ordering };

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempImage(PathBuf);

    impl TempImage {
        fn new(size: u64) -> Self {
            let path = std::env::temp_dir().join(format!(
                "artfs-test-{}-{}.img", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
            File::create(&path).unwrap().set_len(size).unwrap();
            Self(path)
        }
        fn path(&self) -> &str { self.0.to_str().unwrap() }
    }

    impl Drop for TempImage {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
    }

    // 1 MiB segments, 4 KiB blocks: an inode (~6 KiB) takes 2 units.
    fn small_opts() -> FormatOptions {
        FormatOptions { block_size: 4096, chunk_size: 64 << 10, seg_size: 1 << 20 }
    }

    fn formatted(size: u64) -> (TempImage, Alloc) {
        let img = TempImage::new(size);
        let mut alloc = Alloc::new(img.path(), true);
        alloc.format_disk_with(small_opts()).unwrap();
        (img, alloc)
    }

    #[test]
    fn obj_header_size_is_fixed() {
        let hdr = ObjHeader { magic: 0, len: 0, units: 0, crc: 0, generation: 0 };
        assert_eq!(bincode::serialized_size(&hdr).unwrap(), OBJ_HEADER_SIZE);
    }

    #[test]
    fn format_and_mount() {
        let (img, alloc) = formatted(16 << 20);
        let nr_segments = alloc.superblock().nr_segments;
        assert!(nr_segments > 0);
        drop(alloc);

        let alloc = Alloc::mount(img.path()).unwrap();
        assert_eq!(alloc.superblock().nr_segments, nr_segments);
        assert_eq!(alloc.superblock().generation, 0);
    }

    #[test]
    fn root_roundtrip_across_mounts() {
        let (img, mut alloc) = formatted(16 << 20);

        let root_id = alloc.register_inode(&Inode::new(InodeType::Tag)).unwrap();
        assert_ne!(root_id, NO_OBJECT);

        let mut root = alloc.read_root().unwrap();
        assert!(root.add_tag_element("hello", 42));
        alloc.commit_root(&root).unwrap();
        drop(alloc);

        let mut alloc = Alloc::mount(img.path()).unwrap();
        assert_eq!(alloc.superblock().root_id, root_id);
        let root = alloc.read_root().unwrap();
        assert_eq!(&root.payload[..5], b"hello");
    }

    #[test]
    fn fills_device_then_reports_full() {
        let (img, mut alloc) = formatted(8 << 20);
        let nr_segments = alloc.superblock().nr_segments as u64;
        let inode = Inode::new(InodeType::File);

        let mut ids = std::collections::BTreeSet::new();
        let err = loop {
            match alloc.register_inode(&inode) {
                Ok(id) => assert!(ids.insert(id)),
                Err(e) => break e,
            }
        };
        assert_eq!(err.kind(), ErrorKind::StorageFull);

        // 256 units per segment, 1 for the header, 2 per inode.
        assert_eq!(ids.len() as u64, nr_segments * (255 / 2));
        drop(alloc);

        let mut alloc = Alloc::mount(img.path()).unwrap();
        for &id in &ids { alloc.read_obj::<Inode>(id as u128).unwrap(); }
        assert_eq!(alloc.register_inode(&inode).unwrap_err().kind(), ErrorKind::StorageFull);
    }

    #[test]
    fn freed_space_is_reused_after_checkpoint() {
        let (img, mut alloc) = formatted(8 << 20);
        let inode = Inode::new(InodeType::File);

        let _root = alloc.register_inode(&inode).unwrap();
        let mut ids = Vec::new();
        while let Ok(id) = alloc.register_inode(&inode) { ids.push(id); }

        let victim = ids[ids.len() / 2];
        alloc.free_obj(victim as u128).unwrap();
        assert!(alloc.read_obj::<Inode>(victim as u128).is_err());

        // The hole is reused (same address) by the next allocation.
        assert_eq!(alloc.register_inode(&inode).unwrap(), victim);
        assert_eq!(alloc.register_inode(&inode).unwrap_err().kind(), ErrorKind::StorageFull);
        drop(alloc);

        Alloc::mount(img.path()).unwrap().read_obj::<Inode>(victim as u128).unwrap();
    }

    #[test]
    fn empty_segments_are_released() {
        let (img, mut alloc) = formatted(8 << 20);
        let inode = Inode::new(InodeType::File);

        let _root = alloc.register_inode(&inode).unwrap();
        let mut ids = Vec::new();
        while let Ok(id) = alloc.register_inode(&inode) { ids.push(id); }

        // Empty the first segment that does not contain the root.
        let seg_of = |id: u64| (id >> 32) as u32;
        let target = seg_of(ids[0]) + 1;
        let in_target: Vec<u64> = ids.iter().copied().filter(|&id| seg_of(id) == target).collect();
        for &id in &in_target { alloc.free_obj(id as u128).unwrap(); }

        // It is no longer active (allocations moved on), so it is released.
        assert_eq!(alloc.segs.lst().entries[target as usize].entry_type, LstEntryType::Free);
        drop(alloc);

        let mut alloc = Alloc::mount(img.path()).unwrap();
        assert_eq!(alloc.segs.lst().entries[target as usize].entry_type, LstEntryType::Free);

        // The released segment is reopened for new allocations.
        let id = alloc.register_inode(&inode).unwrap();
        assert_eq!(seg_of(id), target);
    }

    #[test]
    fn mount_falls_back_to_previous_generation() {
        let (img, mut alloc) = formatted(8 << 20);
        let inode = Inode::new(InodeType::File);

        let first = alloc.register_inode(&inode).unwrap();
        let second = alloc.register_inode(&inode).unwrap();
        let generation = alloc.superblock().generation;
        drop(alloc);

        // Corrupt the newest superblock slot.
        let dev = File::options().write(true).open(img.path()).unwrap();
        dev.write_all_at(&[0xFF; 64], SuperBlock::slot_for(generation)).unwrap();
        drop(dev);

        let mut alloc = Alloc::mount(img.path()).unwrap();
        assert_eq!(alloc.superblock().generation, generation - 1);
        alloc.read_obj::<Inode>(first as u128).unwrap();
        assert!(alloc.read_obj::<Inode>(second as u128).is_err());

        // The filesystem keeps working from the older generation.
        alloc.register_inode(&inode).unwrap();
        drop(alloc);
        Alloc::mount(img.path()).unwrap();
    }

    #[test]
    fn tagfs_format_mount_register_tag() {
        use crate::storage::TagFs;

        let img = TempImage::new(3 << 30); // sparse; default 1 GiB segments
        TagFs::format(img.path()).unwrap();

        let mut fs = TagFs::mount(img.path()).unwrap();
        fs.register_tag("prova").unwrap();
        drop(fs);

        let mut fs = TagFs::mount(img.path()).unwrap();
        fs.register_tag("seconda").unwrap();
    }
}
