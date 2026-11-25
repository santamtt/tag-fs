//! TagFs: tag-based namespace on top of the segment allocator, shaped for FUSE.
//!
//! Namespace model:
//!   - the root (FUSE inode `ROOT_INO`) lists tags (directories) and untagged files;
//!   - every tag lists the files that carry it (tags are flat: no nested tags);
//!   - a file listed in several tags is like a hard link (`nlink` = number of entries).
//!
//! Directory content (root and tags) is stored inline in the inode payload as
//! a sequence of records `[kind: u8][name len: u8][name][object id: u64 LE]`.
//! File data is stored inline in the payload (at most `MAX_FILE_SIZE` bytes)
//! until extent management is implemented.
//!
//! FUSE compatibility:
//!   - every public method takes `&self` (fuser >= 0.16 calls `Filesystem`
//!     methods through `&self`), so the state uses interior mutability;
//!   - inode numbers are FUSE inode numbers: `ROOT_INO` (1) is the root,
//!     every other number is the object id of the inode;
//!   - errors are `io::Error`s whose kind maps to an errno through `errno()`.
//!
//! Locking: every operation takes all locks in the fixed order
//! alloc -> root -> open -> orphans (see `Ctx`), so operations are serialized
//! and cannot deadlock. Finer-grained locking can be introduced later.

mod superblock;
mod inode;
mod extent;
mod alloc;
mod open;
mod lst;
mod segment;
mod data_structures;

use crate::storage::inode::*;
use crate::storage::alloc::*;
use crate::storage::data_structures::mem_arena::Arena;

use crate::storage::open::*;

use std::collections::BTreeSet;
use std::io::{
    Result,
    Error,
    ErrorKind
};
use std::sync::{ Mutex, MutexGuard };
use std::time::{ Duration, SystemTime, UNIX_EPOCH };

use core::cmp::{ max, min };

/// FUSE inode number of the root directory.
pub const ROOT_INO: u64 = 1;
/// Maximum length of a file or tag name, in bytes.
pub const MAX_NAME_LEN: usize = 255;
/// Maximum file size while data is stored inline in the inode.
pub const MAX_FILE_SIZE: u64 = INODE_PAYLOAD;
/// Block size reported to `stat`.
pub const BLKSIZE: u32 = 4096;

const DIR_PERM: u16 = 0o755;
const KIND_FILE: u8 = 0;
const KIND_DIR: u8 = 1;

/* ------------------------------------------------------------------------- */
/*                         FUSE-facing data types                             */
/* ------------------------------------------------------------------------- */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Directory,
    RegularFile
}

/// Attributes of an inode, with the same fields as `fuser::FileAttr`.
#[derive(Debug, Clone)]
pub struct FileStat {
    pub ino: u64,
    pub kind: FileKind,
    pub size: u64,
    /// Size in 512-byte blocks.
    pub blocks: u64,
    pub perm: u16,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub atime: SystemTime,
    pub mtime: SystemTime,
    pub ctime: SystemTime,
    pub blksize: u32,
}

/// One `readdir` entry. `readdir` always starts with "." and "..".
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub ino: u64,
    pub kind: FileKind,
    pub name: String,
}

/// Changes requested by `setattr` (`None` = leave unchanged).
#[derive(Debug, Clone, Default)]
pub struct SetAttr {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
}

/// Values for `statfs`, with the same meaning as `fuser::ReplyStatfs::statfs`.
#[derive(Debug, Clone, Copy)]
pub struct StatFs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

/// errno corresponding to an error returned by `TagFs`.
pub fn errno(e: &Error) -> i32 {
    if let Some(code) = e.raw_os_error() { return code; }

    match e.kind() {
        ErrorKind::NotFound => libc::ENOENT,
        ErrorKind::AlreadyExists => libc::EEXIST,
        ErrorKind::NotADirectory => libc::ENOTDIR,
        ErrorKind::IsADirectory => libc::EISDIR,
        ErrorKind::DirectoryNotEmpty => libc::ENOTEMPTY,
        ErrorKind::StorageFull => libc::ENOSPC,
        ErrorKind::FileTooLarge => libc::EFBIG,
        ErrorKind::TooManyLinks => libc::EMLINK,
        ErrorKind::InvalidFilename => libc::ENAMETOOLONG,
        ErrorKind::InvalidInput => libc::EINVAL,
        ErrorKind::PermissionDenied => libc::EPERM,
        ErrorKind::Unsupported => libc::EOPNOTSUPP,
        _ => libc::EIO,
    }
}

/* ------------------------------------------------------------------------- */
/*                               helpers                                      */
/* ------------------------------------------------------------------------- */

fn err(kind: ErrorKind, msg: impl Into<String>) -> Error { Error::new(kind, msg.into()) }

fn lock<T>(m: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    m.lock().map_err(|_| Error::other("TagFs state poisoned by a previous panic"))
}

fn now() -> (u64, u32) { from_system_time(SystemTime::now()) }

fn from_system_time(t: SystemTime) -> (u64, u32) {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    (d.as_secs(), d.subsec_nanos())
}

fn to_system_time((sec, nsec): (u64, u32)) -> SystemTime { UNIX_EPOCH + Duration::new(sec, nsec) }

fn kind_of(inode: &Inode) -> FileKind {
    match inode.inode_type() {
        InodeType::Tag => FileKind::Directory,
        InodeType::File => FileKind::RegularFile,
    }
}

fn touch(inode: &mut Inode) {
    let t = now();
    inode.set_mtime(t);
    inode.set_ctime(t);
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(err(ErrorKind::InvalidInput, format!("invalid name {:?}", name)));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(err(ErrorKind::InvalidFilename, format!("name longer than {} bytes", MAX_NAME_LEN)));
    }
    Ok(())
}

/* --------------------------- directory records --------------------------- */

#[derive(Debug, Clone)]
struct DirRecord {
    name: String,
    id: u64,
    kind: FileKind,
}

fn find(records: &[DirRecord], name: &str) -> Option<usize> {
    records.iter().position(|r| r.name == name)
}

fn decode_dir(inode: &Inode) -> Result<Vec<DirRecord>> {
    let corrupted = || err(ErrorKind::InvalidData,
        "corrupted directory (formatted with an older layout? reformat the device)");

    let size = inode.size_bytes() as usize;
    if size > inode.payload.len() { return Err(corrupted()); }
    let data = &inode.payload[..size];

    let mut records = Vec::new();
    let mut pos = 0;
    while pos < size {
        if pos + 2 > size { return Err(corrupted()); }

        let kind = match data[pos] {
            KIND_FILE => FileKind::RegularFile,
            KIND_DIR => FileKind::Directory,
            _ => return Err(corrupted()),
        };
        let len = data[pos + 1] as usize;
        let end = pos + 2 + len + 8;
        if len == 0 || end > size { return Err(corrupted()); }

        let name = std::str::from_utf8(&data[pos + 2..pos + 2 + len]).map_err(|_| corrupted())?;
        let id = u64::from_le_bytes(data[pos + 2 + len..end].try_into().unwrap());

        records.push(DirRecord { name: name.to_string(), id, kind });
        pos = end;
    }

    Ok(records)
}

fn encode_dir(inode: &mut Inode, records: &[DirRecord]) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    for r in records {
        buf.push(if r.kind == FileKind::Directory { KIND_DIR } else { KIND_FILE });
        buf.push(r.name.len() as u8);
        buf.extend_from_slice(r.name.as_bytes());
        buf.extend_from_slice(&r.id.to_le_bytes());
    }

    if buf.len() > inode.payload.len() {
        return Err(err(ErrorKind::StorageFull, "directory is full"));
    }

    let old_size = min(inode.size_bytes() as usize, inode.payload.len());
    inode.payload[..buf.len()].copy_from_slice(&buf);
    if old_size > buf.len() { inode.payload[buf.len()..old_size].fill(0); }
    inode.set_size_bytes(buf.len() as u64);
    Ok(())
}

/* ------------------------------------------------------------------------- */
/*                                 TagFs                                      */
/* ------------------------------------------------------------------------- */

#[derive(Debug)]
pub struct TagFs {
    alloc: Mutex<Alloc>,
    open: Mutex<Arena<OpenEntry>>,
    /// Cached root inode, always in sync with its on-disk copy.
    root: Mutex<Inode>,
    /// Object id of the root inode.
    root_id: u64,
    /// Unlinked files still open: freed on their last `release`.
    orphans: Mutex<BTreeSet<u64>>,
}

/// All the state of a `TagFs`, locked for the duration of one operation.
struct Ctx<'a> {
    alloc: MutexGuard<'a, Alloc>,
    root: MutexGuard<'a, Inode>,
    open: MutexGuard<'a, Arena<OpenEntry>>,
    orphans: MutexGuard<'a, BTreeSet<u64>>,
    root_id: u64,
}

impl<'a> Ctx<'a> {

    fn to_id(&self, ino: u64) -> u64 { if ino == ROOT_INO { self.root_id } else { ino } }

    fn to_ino(&self, id: u64) -> u64 { if id == self.root_id { ROOT_INO } else { id } }

    fn load(&mut self, id: u64) -> Result<Inode> {
        if id == self.root_id { return Ok(self.root.clone()); }
        self.alloc.read_obj::<Inode>(id as u128)
    }

    fn store(&mut self, id: u64, inode: &Inode) -> Result<()> {
        if id == self.root_id {
            self.alloc.commit_root(inode)?;
            *self.root = inode.clone();
            Ok(())
        } else {
            self.alloc.commit::<Inode>(inode, id as u128)
        }
    }

    fn load_dir(&mut self, id: u64) -> Result<(Inode, Vec<DirRecord>)> {
        let inode = self.load(id)?;
        if kind_of(&inode) != FileKind::Directory {
            return Err(err(ErrorKind::NotADirectory, "not a directory"));
        }
        let records = decode_dir(&inode)?;
        Ok((inode, records))
    }

    fn stat_of(&self, id: u64, inode: &Inode) -> FileStat {
        let kind = kind_of(inode);
        let size = inode.size_bytes();

        let nlink = match kind {
            FileKind::Directory => 2 + decode_dir(inode)
                .map(|r| r.iter().filter(|r| r.kind == FileKind::Directory).count() as u32)
                .unwrap_or(0),
            FileKind::RegularFile => max(inode.link_count() as u32, 1),
        };

        FileStat {
            ino: self.to_ino(id),
            kind,
            size,
            blocks: size.div_ceil(512),
            perm: inode.perm(),
            nlink,
            uid: inode.uid(),
            gid: inode.gid(),
            atime: to_system_time(inode.atime()),
            mtime: to_system_time(inode.mtime()),
            ctime: to_system_time(inode.ctime()),
            blksize: BLKSIZE,
        }
    }

    fn stat(&mut self, id: u64) -> Result<FileStat> {
        let inode = self.load(id)?;
        Ok(self.stat_of(id, &inode))
    }

    /// Allocates and persists a new inode; returns its object id.
    fn new_inode(&mut self, i_type: InodeType, mode: u32, uid: u32, gid: u32) -> Result<u64> {
        let mut inode = Inode::new(i_type);
        inode.set_perm((mode & 0o777) as u16);
        inode.set_owner(uid, gid);
        inode.set_link_count(match i_type { InodeType::File => 1, InodeType::Tag => 2 });
        self.alloc.register_inode(&inode)
    }

    /// Writes `records` into directory `dir_id` and persists it.
    fn store_dir(&mut self, dir_id: u64, mut dir: Inode, records: &[DirRecord]) -> Result<()> {
        encode_dir(&mut dir, records)?;
        touch(&mut dir);
        self.store(dir_id, &dir)
    }

    fn is_open(&self, id: u64) -> bool {
        self.open.elements.iter().flatten().any(|e| e.id == id as u128)
    }

    /// Frees an inode that is no longer referenced, or defers it while open.
    fn release_inode(&mut self, id: u64) -> Result<()> {
        if self.is_open(id) {
            self.orphans.insert(id);
            Ok(())
        } else {
            self.alloc.free_obj(id as u128)
        }
    }

    /// Removes one directory entry's worth of references from `id`.
    fn drop_link(&mut self, id: u64) -> Result<()> {
        let mut inode = self.load(id)?;
        if kind_of(&inode) == FileKind::Directory { return self.release_inode(id); }

        let links = inode.link_count().saturating_sub(1);
        if links == 0 { return self.release_inode(id); }

        inode.set_link_count(links);
        inode.set_ctime(now());
        self.store(id, &inode)
    }

    /// Checks that `dest` can be replaced by an entry of kind `src_kind`.
    fn check_replace(&mut self, src_kind: FileKind, dest: &DirRecord) -> Result<()> {
        match (src_kind, dest.kind) {
            (FileKind::RegularFile, FileKind::Directory) =>
                Err(err(ErrorKind::IsADirectory, "cannot replace a tag with a file")),
            (FileKind::Directory, FileKind::RegularFile) =>
                Err(err(ErrorKind::NotADirectory, "cannot replace a file with a tag")),
            (FileKind::Directory, FileKind::Directory) => {
                let (_, children) = self.load_dir(dest.id)?;
                if children.is_empty() { Ok(()) } else {
                    Err(err(ErrorKind::DirectoryNotEmpty, "destination tag is not empty"))
                }
            },
            (FileKind::RegularFile, FileKind::RegularFile) => Ok(()),
        }
    }

    fn new_handle(&mut self, id: u64, flags: i32) -> u64 {
        let mut entry = OpenEntry::new();
        entry.id = id as u128;
        entry.flags = flags as u32;
        entry.refcount = 1;
        self.open.insert(entry) as u64
    }

    fn handle(&self, fh: u64) -> Result<OpenEntry> {
        self.open.elements.get(fh as usize).and_then(|e| *e)
            .ok_or_else(|| Error::from_raw_os_error(libc::EBADF))
    }
}

impl TagFs {

    fn ctx(&self) -> Result<Ctx<'_>> {
        let alloc = lock(&self.alloc)?;
        let root = lock(&self.root)?;
        let open = lock(&self.open)?;
        let orphans = lock(&self.orphans)?;
        Ok(Ctx { alloc, root, open, orphans, root_id: self.root_id })
    }

    /* ------------------------------ lifecycle ------------------------------ */

    fn _alloc_root(alloc: &mut Alloc) -> Result<()> {
        let mut root_inode = Inode::new(InodeType::Tag);
        root_inode.set_perm(DIR_PERM);
        root_inode.set_link_count(2);
        alloc.register_inode(&root_inode)?;
        Ok(())
    }

    pub fn format(dev_name: &str) -> Result<()> {
        let mut alloc = Alloc::new(dev_name, true);
        alloc.format_disk()?;
        Self::_alloc_root(&mut alloc)
    }

    pub fn mount(dev_name: &str) -> Result<Self> {
        let mut alloc = Alloc::mount(dev_name)?;
        let root = alloc.read_root()?;
        if kind_of(&root) != FileKind::Directory {
            return Err(err(ErrorKind::InvalidData, "root inode is not a directory"));
        }
        let root_id = alloc.superblock().root_id;

        Ok(Self {
            alloc: Mutex::new(alloc),
            open: Mutex::new(Arena::new()),
            root: Mutex::new(root),
            root_id,
            orphans: Mutex::new(BTreeSet::new()),
        })
    }

    /// Frees unlinked files that are still open and makes everything durable.
    /// Call it from `Filesystem::destroy`.
    pub fn unmount(&self) -> Result<()> {
        let mut c = self.ctx()?;
        let orphans: Vec<u64> = std::mem::take(&mut *c.orphans).into_iter().collect();
        for id in orphans { c.alloc.free_obj(id as u128)?; }
        c.alloc.sync()
    }

    /// Makes every completed operation durable (checkpoint).
    pub fn fsync(&self) -> Result<()> { self.ctx()?.alloc.sync() }

    pub fn root_ino(&self) -> u64 { ROOT_INO }

    /// Clone of the cached root inode.
    pub fn root_inode(&self) -> Result<Inode> { Ok(lock(&self.root)?.clone()) }

    /* ------------------------------ attributes ----------------------------- */

    pub fn getattr(&self, ino: u64) -> Result<FileStat> {
        let mut c = self.ctx()?;
        let id = c.to_id(ino);
        c.stat(id)
    }

    pub fn setattr(&self, ino: u64, attr: &SetAttr) -> Result<FileStat> {
        let mut c = self.ctx()?;
        let id = c.to_id(ino);
        let mut inode = c.load(id)?;
        let t = now();

        if let Some(size) = attr.size {
            if kind_of(&inode) == FileKind::Directory {
                return Err(err(ErrorKind::IsADirectory, "cannot truncate a tag"));
            }
            if size > MAX_FILE_SIZE {
                return Err(err(ErrorKind::FileTooLarge, format!("files are limited to {} bytes", MAX_FILE_SIZE)));
            }
            let old = inode.size_bytes();
            if size < old { inode.payload[size as usize..old as usize].fill(0); }
            inode.set_size_bytes(size);
            inode.set_mtime(t);
        }

        if let Some(mode) = attr.mode { inode.set_perm((mode & 0o777) as u16); }
        if attr.uid.is_some() || attr.gid.is_some() {
            let (uid, gid) = (attr.uid.unwrap_or(inode.uid()), attr.gid.unwrap_or(inode.gid()));
            inode.set_owner(uid, gid);
        }
        if let Some(atime) = attr.atime { inode.set_atime(from_system_time(atime)); }
        if let Some(mtime) = attr.mtime { inode.set_mtime(from_system_time(mtime)); }
        inode.set_ctime(t);

        c.store(id, &inode)?;
        Ok(c.stat_of(id, &inode))
    }

    pub fn statfs(&self) -> Result<StatFs> {
        let c = self.ctx()?;
        let sb = c.alloc.superblock();
        let space = c.alloc.space_stats();

        let bsize = sb.block_size;
        let total_bytes = sb.nr_segments as u64 * sb.seg_size;
        let free_bytes = space.segments_free as u64 * sb.seg_size + space.free_bytes_in_segments;
        // A serialized inode currently takes two blocks.
        let ffree = free_bytes / (2 * bsize);

        Ok(StatFs {
            blocks: total_bytes / bsize,
            bfree: free_bytes / bsize,
            bavail: free_bytes / bsize,
            files: sb.nr_inodes as u64 + ffree,
            ffree,
            bsize: bsize as u32,
            namelen: MAX_NAME_LEN as u32,
            frsize: bsize as u32,
        })
    }

    /* ------------------------------ namespace ------------------------------ */

    pub fn lookup(&self, parent: u64, name: &str) -> Result<FileStat> {
        let mut c = self.ctx()?;
        let pid = c.to_id(parent);
        let (_, records) = c.load_dir(pid)?;
        let idx = find(&records, name)
            .ok_or_else(|| err(ErrorKind::NotFound, format!("{:?} not found", name)))?;
        c.stat(records[idx].id)
    }

    /// Resolves a `/`-separated path relative to the root.
    pub fn disk_lookup(&self, path: &str) -> Result<FileStat> {
        let mut stat = self.getattr(ROOT_INO)?;
        for component in path.split('/').filter(|c| !c.is_empty()) {
            stat = self.lookup(stat.ino, component)?;
        }
        Ok(stat)
    }

    pub fn readdir(&self, ino: u64) -> Result<Vec<DirEntry>> {
        let mut c = self.ctx()?;
        let id = c.to_id(ino);
        let (_, records) = c.load_dir(id)?;

        let mut entries = vec![
            DirEntry { ino, kind: FileKind::Directory, name: ".".to_string() },
            DirEntry { ino: ROOT_INO, kind: FileKind::Directory, name: "..".to_string() },
        ];
        entries.extend(records.into_iter().map(|r| DirEntry { ino: c.to_ino(r.id), kind: r.kind, name: r.name }));
        Ok(entries)
    }

    /// Creates a tag. Tags can only be created in the root.
    pub fn mkdir(&self, parent: u64, name: &str, mode: u32, uid: u32, gid: u32) -> Result<FileStat> {
        check_name(name)?;
        let mut c = self.ctx()?;
        let pid = c.to_id(parent);
        let (dir, mut records) = c.load_dir(pid)?;

        if pid != c.root_id {
            return Err(err(ErrorKind::Unsupported, "tags cannot be nested"));
        }
        if find(&records, name).is_some() {
            return Err(err(ErrorKind::AlreadyExists, format!("{:?} already exists", name)));
        }

        let id = c.new_inode(InodeType::Tag, mode, uid, gid)?;
        records.push(DirRecord { name: name.to_string(), id, kind: FileKind::Directory });
        if let Err(e) = c.store_dir(pid, dir, &records) {
            let _ = c.alloc.free_obj(id as u128);
            return Err(e);
        }
        c.stat(id)
    }

    /// Deletes an empty tag.
    pub fn rmdir(&self, parent: u64, name: &str) -> Result<()> {
        let mut c = self.ctx()?;
        let pid = c.to_id(parent);
        let (dir, mut records) = c.load_dir(pid)?;

        let idx = find(&records, name)
            .ok_or_else(|| err(ErrorKind::NotFound, format!("{:?} not found", name)))?;
        if records[idx].kind != FileKind::Directory {
            return Err(err(ErrorKind::NotADirectory, format!("{:?} is not a tag", name)));
        }

        let tag_id = records[idx].id;
        let (_, children) = c.load_dir(tag_id)?;
        if !children.is_empty() {
            return Err(err(ErrorKind::DirectoryNotEmpty, format!("tag {:?} is not empty", name)));
        }

        records.remove(idx);
        c.store_dir(pid, dir, &records)?;
        c.release_inode(tag_id)
    }

    /// Creates and opens a regular file; returns its attributes and handle.
    pub fn create(&self, parent: u64, name: &str, mode: u32, flags: i32, uid: u32, gid: u32) -> Result<(FileStat, u64)> {
        check_name(name)?;
        let mut c = self.ctx()?;
        let pid = c.to_id(parent);
        let (dir, mut records) = c.load_dir(pid)?;

        if find(&records, name).is_some() {
            return Err(err(ErrorKind::AlreadyExists, format!("{:?} already exists", name)));
        }

        let id = c.new_inode(InodeType::File, mode, uid, gid)?;
        records.push(DirRecord { name: name.to_string(), id, kind: FileKind::RegularFile });
        if let Err(e) = c.store_dir(pid, dir, &records) {
            let _ = c.alloc.free_obj(id as u128);
            return Err(e);
        }

        let fh = c.new_handle(id, flags);
        Ok((c.stat(id)?, fh))
    }

    /// Removes a file entry (i.e. removes the tag from the file). The file is
    /// deleted when its last entry is removed and it is no longer open.
    pub fn unlink(&self, parent: u64, name: &str) -> Result<()> {
        let mut c = self.ctx()?;
        let pid = c.to_id(parent);
        let (dir, mut records) = c.load_dir(pid)?;

        let idx = find(&records, name)
            .ok_or_else(|| err(ErrorKind::NotFound, format!("{:?} not found", name)))?;
        if records[idx].kind == FileKind::Directory {
            return Err(err(ErrorKind::IsADirectory, format!("{:?} is a tag", name)));
        }

        let id = records.remove(idx).id;
        c.store_dir(pid, dir, &records)?;
        c.drop_link(id)
    }

    /// Adds an entry for file `ino` in `newparent` (i.e. adds a tag to the file).
    pub fn link(&self, ino: u64, newparent: u64, newname: &str) -> Result<FileStat> {
        check_name(newname)?;
        let mut c = self.ctx()?;
        let id = c.to_id(ino);
        let mut inode = c.load(id)?;

        if kind_of(&inode) == FileKind::Directory {
            return Err(err(ErrorKind::PermissionDenied, "hard links to tags are not allowed"));
        }

        let npid = c.to_id(newparent);
        let (dir, mut records) = c.load_dir(npid)?;
        if find(&records, newname).is_some() {
            return Err(err(ErrorKind::AlreadyExists, format!("{:?} already exists", newname)));
        }
        if records.iter().any(|r| r.id == id) {
            return Err(err(ErrorKind::AlreadyExists, "the file is already in this directory"));
        }

        let links = inode.link_count().checked_add(1)
            .ok_or_else(|| err(ErrorKind::TooManyLinks, "too many links"))?;

        // Increase the link count first: after a crash the file is leaked
        // rather than freed while still referenced.
        let old_links = inode.link_count();
        inode.set_link_count(links);
        inode.set_ctime(now());
        c.store(id, &inode)?;

        records.push(DirRecord { name: newname.to_string(), id, kind: FileKind::RegularFile });
        if let Err(e) = c.store_dir(npid, dir, &records) {
            inode.set_link_count(old_links);
            let _ = c.store(id, &inode);
            return Err(e);
        }

        Ok(c.stat_of(id, &inode))
    }

    /// Renames/moves an entry. Files can move between any directories, tags
    /// only inside the root. An existing destination is replaced unless
    /// `no_replace` is set (`RENAME_NOREPLACE`).
    pub fn rename(&self, parent: u64, name: &str, newparent: u64, newname: &str, no_replace: bool) -> Result<()> {
        check_name(newname)?;
        let mut c = self.ctx()?;
        let pid = c.to_id(parent);
        let npid = c.to_id(newparent);

        let (dir, mut records) = c.load_dir(pid)?;
        let idx = find(&records, name)
            .ok_or_else(|| err(ErrorKind::NotFound, format!("{:?} not found", name)))?;
        let src = records[idx].clone();

        if src.kind == FileKind::Directory && npid != c.root_id {
            return Err(err(ErrorKind::Unsupported, "tags cannot be nested"));
        }
        if pid == npid && name == newname { return Ok(()); }

        let mut victim: Option<DirRecord> = None;

        if pid == npid {
            if let Some(j) = find(&records, newname) {
                let dest = records[j].clone();
                if no_replace { return Err(err(ErrorKind::AlreadyExists, format!("{:?} already exists", newname))); }
                c.check_replace(src.kind, &dest)?;
                records.remove(j);
                victim = Some(dest);
            }
            let idx = find(&records, name).expect("source entry still present");
            records[idx].name = newname.to_string();
            c.store_dir(pid, dir, &records)?;
        } else {
            let (ndir, mut nrecords) = c.load_dir(npid)?;

            if let Some(j) = find(&nrecords, newname) {
                let dest = nrecords[j].clone();
                if no_replace { return Err(err(ErrorKind::AlreadyExists, format!("{:?} already exists", newname))); }
                if dest.id == src.id { return Ok(()); } // same file: POSIX says do nothing
                c.check_replace(src.kind, &dest)?;
                nrecords.remove(j);
                victim = Some(dest);
            }
            if nrecords.iter().any(|r| r.id == src.id) {
                return Err(err(ErrorKind::AlreadyExists, "the file is already in the destination directory"));
            }

            records.remove(idx);
            nrecords.push(DirRecord { name: newname.to_string(), ..src.clone() });

            // Check that both directories fit before writing either of them.
            let (mut dir, mut ndir) = (dir, ndir);
            encode_dir(&mut dir, &records)?;
            encode_dir(&mut ndir, &nrecords)?;
            touch(&mut dir);
            touch(&mut ndir);

            // Destination first: after a crash the file is in both places
            // rather than in none.
            c.store(npid, &ndir)?;
            c.store(pid, &dir)?;
        }

        let mut moved = c.load(src.id)?;
        moved.set_ctime(now());
        c.store(src.id, &moved)?;

        match victim {
            Some(v) => c.drop_link(v.id),
            None => Ok(()),
        }
    }

    /* -------------------------------- data --------------------------------- */

    /// Opens a regular file and returns a handle for `read`/`write`/`release`.
    pub fn open(&self, ino: u64, flags: i32) -> Result<u64> {
        let mut c = self.ctx()?;
        let id = c.to_id(ino);
        let inode = c.load(id)?;
        if kind_of(&inode) == FileKind::Directory {
            return Err(err(ErrorKind::IsADirectory, "cannot open a tag as a file"));
        }
        Ok(c.new_handle(id, flags))
    }

    pub fn release(&self, fh: u64) -> Result<()> {
        let mut c = self.ctx()?;
        let entry = c.handle(fh)?;
        c.open.delete(fh as usize);

        let id = entry.id as u64;
        if c.orphans.contains(&id) && !c.is_open(id) {
            c.orphans.remove(&id);
            c.alloc.free_obj(id as u128)?;
        }
        Ok(())
    }

    pub fn read(&self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>> {
        let mut c = self.ctx()?;
        let entry = c.handle(fh)?;
        let inode = c.load(entry.id as u64)?;

        let len = inode.size_bytes();
        let start = min(offset, len) as usize;
        let end = min(offset.saturating_add(size as u64), len) as usize;
        Ok(inode.payload[start..end].to_vec())
    }

    /// Writes at `offset`; returns the number of bytes written (short when
    /// the write crosses `MAX_FILE_SIZE`).
    pub fn write(&self, fh: u64, offset: u64, data: &[u8]) -> Result<u32> {
        let mut c = self.ctx()?;
        let entry = c.handle(fh)?;
        let id = entry.id as u64;
        let mut inode = c.load(id)?;

        if data.is_empty() { return Ok(0); }
        if offset >= MAX_FILE_SIZE {
            return Err(err(ErrorKind::FileTooLarge, format!("files are limited to {} bytes", MAX_FILE_SIZE)));
        }

        let written = min(data.len() as u64, MAX_FILE_SIZE - offset) as usize;
        let start = offset as usize;
        inode.payload[start..start + written].copy_from_slice(&data[..written]);
        inode.set_size_bytes(max(inode.size_bytes(), offset + written as u64));
        touch(&mut inode);

        c.store(id, &inode)?;
        Ok(written as u32)
    }

    /* -------------------------- legacy interface --------------------------- */

    /// Creates a tag in the root, owned by the current user.
    pub fn register_tag(&self, tag_name: &str) -> Result<()> {
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        self.mkdir(ROOT_INO, tag_name, DIR_PERM as u32, uid, gid).map(|_| ())
    }

    /// Opens the inode with object id `path`; the returned descriptor keeps a
    /// cursor used by `read_file`/`write_file`.
    pub fn open_file(&self, path: u128) -> Result<usize> {
        let id = u64::try_from(path).map_err(|_| err(ErrorKind::InvalidInput, "invalid inode id"))?;
        let ino = if id == self.root_id { ROOT_INO } else { id };
        Ok(self.open(ino, libc::O_RDWR)? as usize)
    }

    pub fn close_file(&self, fd: usize) -> Result<()> { self.release(fd as u64) }

    pub fn read_file(&self, fd: usize, read_size: usize) -> Result<Vec<u8>> {
        let offset = self.ctx()?.handle(fd as u64)?.offset;
        let data = self.read(fd as u64, offset, read_size.min(u32::MAX as usize) as u32)?;

        let mut c = self.ctx()?;
        if let Some(Some(entry)) = c.open.elements.get_mut(fd) { entry.offset += data.len() as u64; }
        Ok(data)
    }

    pub fn write_file(&self, fd: usize, bytes_in: Vec<u8>) -> Result<()> {
        let offset = self.ctx()?.handle(fd as u64)?.offset;
        let written = self.write(fd as u64, offset, &bytes_in)?;

        let mut c = self.ctx()?;
        if let Some(Some(entry)) = c.open.elements.get_mut(fd) { entry.offset += written as u64; }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::PathBuf;
    use std::sync::atomic::{ AtomicU32, Ordering };

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct TempImage(PathBuf);

    impl TempImage {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "tagfs-test-{}-{}.img", std::process::id(), COUNTER.fetch_add(1, Ordering::SeqCst)));
            File::create(&path).unwrap().set_len(3 << 30).unwrap(); // sparse
            Self(path)
        }
        fn path(&self) -> &str { self.0.to_str().unwrap() }
    }

    impl Drop for TempImage {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
    }

    fn fresh() -> (TempImage, TagFs) {
        let img = TempImage::new();
        TagFs::format(img.path()).unwrap();
        let fs = TagFs::mount(img.path()).unwrap();
        (img, fs)
    }

    fn names(fs: &TagFs, ino: u64) -> Vec<String> {
        fs.readdir(ino).unwrap().into_iter().map(|e| e.name).collect()
    }

    #[test]
    fn root_attributes() {
        let (_img, fs) = fresh();
        let st = fs.getattr(ROOT_INO).unwrap();
        assert_eq!(st.ino, ROOT_INO);
        assert_eq!(st.kind, FileKind::Directory);
        assert_eq!(st.perm, 0o755);
        assert_eq!(st.nlink, 2);
        assert_eq!(names(&fs, ROOT_INO), vec![".", ".."]);
    }

    #[test]
    fn tags_and_files() {
        let (_img, fs) = fresh();

        let tag = fs.mkdir(ROOT_INO, "cats", 0o755, 1000, 1000).unwrap();
        assert_eq!(tag.kind, FileKind::Directory);
        assert_eq!(fs.getattr(ROOT_INO).unwrap().nlink, 3);
        assert_eq!(fs.mkdir(ROOT_INO, "cats", 0o755, 0, 0).unwrap_err().kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs.mkdir(tag.ino, "nested", 0o755, 0, 0).unwrap_err().kind(), ErrorKind::Unsupported);

        let (file, fh) = fs.create(tag.ino, "tom.txt", 0o640, libc::O_RDWR, 1000, 1000).unwrap();
        assert_eq!(file.perm, 0o640);
        assert_eq!(fs.write(fh, 0, b"hello world").unwrap(), 11);
        assert_eq!(fs.write(fh, 6, b"tagfs").unwrap(), 5);
        assert_eq!(fs.read(fh, 0, 100).unwrap(), b"hello tagfs");
        assert_eq!(fs.read(fh, 6, 3).unwrap(), b"tag");
        fs.release(fh).unwrap();

        assert_eq!(fs.lookup(tag.ino, "tom.txt").unwrap().size, 11);
        assert_eq!(fs.disk_lookup("/cats/tom.txt").unwrap().ino, file.ino);
        assert_eq!(fs.lookup(tag.ino, "nope").unwrap_err().kind(), ErrorKind::NotFound);
        assert_eq!(fs.rmdir(ROOT_INO, "cats").unwrap_err().kind(), ErrorKind::DirectoryNotEmpty);
        assert_eq!(names(&fs, tag.ino), vec![".", "..", "tom.txt"]);
    }

    #[test]
    fn links_rename_and_unlink() {
        let (_img, fs) = fresh();
        let cats = fs.mkdir(ROOT_INO, "cats", 0o755, 0, 0).unwrap();
        let pets = fs.mkdir(ROOT_INO, "pets", 0o755, 0, 0).unwrap();

        let (file, fh) = fs.create(cats.ino, "tom", 0o644, 0, 0, 0).unwrap();
        fs.release(fh).unwrap();

        // Second tag on the same file.
        let st = fs.link(file.ino, pets.ino, "tom").unwrap();
        assert_eq!(st.nlink, 2);
        assert_eq!(fs.link(file.ino, pets.ino, "other").unwrap_err().kind(), ErrorKind::AlreadyExists);

        // Rename inside a tag, then move to the root.
        fs.rename(cats.ino, "tom", cats.ino, "thomas", false).unwrap();
        assert_eq!(names(&fs, cats.ino), vec![".", "..", "thomas"]);
        fs.rename(cats.ino, "thomas", ROOT_INO, "thomas", false).unwrap();
        assert_eq!(names(&fs, cats.ino), vec![".", ".."]);
        assert!(names(&fs, ROOT_INO).contains(&"thomas".to_string()));

        // Renaming a tag.
        fs.rename(ROOT_INO, "cats", ROOT_INO, "felines", false).unwrap();
        assert!(fs.lookup(ROOT_INO, "felines").is_ok());

        // Removing one entry keeps the file, removing the last deletes it.
        fs.unlink(pets.ino, "tom").unwrap();
        assert_eq!(fs.getattr(file.ino).unwrap().nlink, 1);
        fs.unlink(ROOT_INO, "thomas").unwrap();
        assert!(fs.getattr(file.ino).is_err());

        fs.rmdir(ROOT_INO, "felines").unwrap();
        assert_eq!(fs.getattr(ROOT_INO).unwrap().nlink, 3); // only "pets" left
    }

    #[test]
    fn rename_replaces_destination() {
        let (_img, fs) = fresh();
        let (a, fh_a) = fs.create(ROOT_INO, "a", 0o644, 0, 0, 0).unwrap();
        let (b, fh_b) = fs.create(ROOT_INO, "b", 0o644, 0, 0, 0).unwrap();
        fs.release(fh_a).unwrap();
        fs.release(fh_b).unwrap();

        assert_eq!(fs.rename(ROOT_INO, "a", ROOT_INO, "b", true).unwrap_err().kind(), ErrorKind::AlreadyExists);
        fs.rename(ROOT_INO, "a", ROOT_INO, "b", false).unwrap();
        assert_eq!(fs.lookup(ROOT_INO, "b").unwrap().ino, a.ino);
        assert!(fs.getattr(b.ino).is_err()); // replaced file was freed
    }

    #[test]
    fn unlinked_open_file_survives_until_release() {
        let (_img, fs) = fresh();
        let (file, fh) = fs.create(ROOT_INO, "tmp", 0o644, 0, 0, 0).unwrap();
        fs.write(fh, 0, b"data").unwrap();

        fs.unlink(ROOT_INO, "tmp").unwrap();
        assert_eq!(fs.read(fh, 0, 10).unwrap(), b"data");
        fs.release(fh).unwrap();
        assert!(fs.getattr(file.ino).is_err());
    }

    #[test]
    fn setattr_truncate_and_limits() {
        let (_img, fs) = fresh();
        let (file, fh) = fs.create(ROOT_INO, "f", 0o644, 0, 0, 0).unwrap();
        fs.write(fh, 0, b"0123456789").unwrap();

        let st = fs.setattr(file.ino, &SetAttr { size: Some(4), mode: Some(0o600), ..Default::default() }).unwrap();
        assert_eq!((st.size, st.perm), (4, 0o600));
        // Extending again reads zeros, not the old bytes.
        fs.setattr(file.ino, &SetAttr { size: Some(8), ..Default::default() }).unwrap();
        assert_eq!(fs.read(fh, 0, 100).unwrap(), b"0123\0\0\0\0");

        assert_eq!(fs.write(fh, MAX_FILE_SIZE - 2, b"abcd").unwrap(), 2);
        assert_eq!(fs.write(fh, MAX_FILE_SIZE, b"x").unwrap_err().kind(), ErrorKind::FileTooLarge);
        assert_eq!(errno(&fs.write(fh, MAX_FILE_SIZE, b"x").unwrap_err()), libc::EFBIG);
        assert_eq!(errno(&fs.read(999, 0, 1).unwrap_err()), libc::EBADF);
        fs.release(fh).unwrap();
    }

    #[test]
    fn persists_across_mounts() {
        let (img, fs) = fresh();
        let tag = fs.mkdir(ROOT_INO, "train", 0o755, 0, 0).unwrap();
        let (file, fh) = fs.create(tag.ino, "sample.bin", 0o644, 0, 0, 0).unwrap();
        fs.write(fh, 0, b"payload").unwrap();
        fs.release(fh).unwrap();
        fs.unmount().unwrap();
        drop(fs);

        let fs = TagFs::mount(img.path()).unwrap();
        let st = fs.disk_lookup("train/sample.bin").unwrap();
        assert_eq!((st.ino, st.size), (file.ino, 7));
        let fh = fs.open(st.ino, libc::O_RDONLY).unwrap();
        assert_eq!(fs.read(fh, 0, 100).unwrap(), b"payload");
        assert!(fs.statfs().unwrap().bfree > 0);
    }

    #[test]
    fn legacy_interface() {
        let (_img, fs) = fresh();
        fs.register_tag("prova").unwrap();
        let (file, fh) = fs.create(ROOT_INO, "f", 0o644, 0, 0, 0).unwrap();
        fs.release(fh).unwrap();

        let fd = fs.open_file(file.ino as u128).unwrap();
        fs.write_file(fd, b"Hello ".to_vec()).unwrap();
        fs.write_file(fd, b"World".to_vec()).unwrap();
        fs.close_file(fd).unwrap();

        let fd = fs.open_file(file.ino as u128).unwrap();
        assert_eq!(fs.read_file(fd, 5).unwrap(), b"Hello");
        assert_eq!(fs.read_file(fd, 100).unwrap(), b" World");
        fs.close_file(fd).unwrap();
    }
}
