use crate::storage::{SetAttr, TagFs};
use crate::storage::{ FileKind, FileStat };
use crate::storage::errno;

use core::time::Duration;
use std::time::SystemTime;

use fuser::{Filesystem, FopenFlags, TimeOrNow};
use fuser::{ FileAttr, FileType, FileHandle };
use fuser::{ Request, INodeNo, Errno, Generation};
use fuser::{ ReplyAttr, ReplyEntry, ReplyDirectory, ReplyCreate };

use std::ffi::{ OsStr };

fn get_attr_fuse(attr: &FileStat) -> FileAttr {
    FileAttr {
        ino: INodeNo(attr.ino),
        size: attr.size,
        blocks: attr.blocks,
        atime: attr.atime,
        mtime: attr.mtime,
        ctime: attr.ctime,
        crtime: attr.ctime,          // creation time: macOS only, not stored by TagFs
        kind: match attr.kind {
            FileKind::Directory => FileType::Directory,
            FileKind::RegularFile => FileType::RegularFile,
        },
        perm: attr.perm,
        nlink: attr.nlink,
        uid: attr.uid,
        gid: attr.gid,
        rdev: 0,                  // only meaningful for device files
        blksize: attr.blksize,
        flags: 0,                 // macOS only
    }

}

fn get_kind_fuse(kind: &FileKind) -> FileType {
     match kind {
        FileKind::Directory => FileType::Directory,
        FileKind::RegularFile => FileType::RegularFile
    }
}

fn convert_timestamp(timestamp_in: &Option<TimeOrNow>) -> Option<SystemTime> {
    match timestamp_in {
        Some(timestamp) => {

            let mut time_ret: SystemTime = SystemTime::now();

            if let TimeOrNow::SpecificTime(time) = *timestamp {
                time_ret = time;
            }

            Option::Some(time_ret)
        }, None => {
            Option::None
        }
    }
}

impl Filesystem for TagFs {

    fn lookup(&self, _r: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        match self.lookup(parent.0, name_str) {
            Ok(attr) => {
                let ttl = Duration::from_secs(1);
                let fuse_attr = get_attr_fuse(&attr);
                reply.entry(&ttl, &fuse_attr, Generation(0));
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let ino_attr: SetAttr =  SetAttr {
            mode: mode,
            uid: uid,
            gid: gid,
            size: size,
            atime: convert_timestamp(&atime),
            mtime: convert_timestamp(&mtime),
        };

        match self.setattr(ino.0, &ino_attr) {
            Ok(attr) => {
                let ttl = Duration::from_secs(1);
                let fuse_attr = get_attr_fuse(&attr);
                reply.attr(&ttl, &fuse_attr);
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn getattr(&self, _r: &Request, parent: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.getattr(parent.0) {
            Ok(attr) => {
                let ttl = Duration::from_secs(1);
                let fuse_attr: FileAttr = get_attr_fuse(&attr);
                reply.attr(&ttl, &fuse_attr);
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

// directories methods

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: fuser::OpenFlags, reply: fuser::ReplyOpen) {
        match self.getattr(ino.0) {
            Ok(st) if st.kind == FileKind::Directory => reply.opened(FileHandle(0), FopenFlags::empty()),
            Ok(_) => reply.error(Errno::from_i32(libc::ENOTDIR)),
            Err(e) => reply.error(Errno::from_i32(errno(&e))),
        }
    }

    fn mkdir(&self, req: &Request, parent: INodeNo, name: &OsStr, mode: u32, umask: u32, reply: ReplyEntry) {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };
        
        match self.mkdir(parent.0, name_str, mode & !umask, req.uid(), req.gid()) {
            Ok(attr) => {
                let ttl = Duration::from_secs(1);
                let fuse_attr = get_attr_fuse(&attr);
                reply.entry(&ttl, &fuse_attr, Generation(0));
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn readdir(&self, _req: &Request, ino: INodeNo, _fh: FileHandle, offset: u64, mut reply: ReplyDirectory) {
        match self.readdir(ino.0) {
            Ok(entries) => {
                
                for (i, entry) in entries.iter().enumerate().skip(offset as usize) {
                // offset passed to add() = offset of the *next* entry
                    if reply.add(INodeNo(entry.ino), (i + 1) as u64, get_kind_fuse(&entry.kind), &entry.name) {
                        break; // reply buffer full; kernel will call again with this offset
                    }
                }
                reply.ok();
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn rmdir(&self, _req: &Request, _parent: INodeNo, _name: &OsStr, _reply: fuser::ReplyEmpty) {
        todo!();
    }

    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _flags: fuser::OpenFlags,
        reply: fuser::ReplyEmpty,
    )
    {
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: fuser::OpenFlags, reply: fuser::ReplyOpen) {
        match self.open(ino.0, flags.0) {
            Ok(fd) => {
                let fuse_fd = FileHandle(fd);
                reply.opened(fuse_fd, FopenFlags::empty());
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn create(
        &self,
        req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    )
    {
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                reply.error(Errno::ENOENT);
                return;
            }
        };

        match self.create(parent.0, name_str, mode | !umask, flags, req.uid(), req.gid()) {
            Ok((attr,fd)) => {
                let ttl = Duration::from_secs(1);
                reply.created(&ttl, &get_attr_fuse(&attr), Generation(0), FileHandle(fd), FopenFlags::empty());
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        _size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _reply: fuser::ReplyData,
    )
    {
        todo!();
    }


    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _offset: u64,
        data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: fuser::ReplyWrite,
    )
    {
        match self.write(fh.0, data) {
            Ok(written) => {
                reply.written(written);
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    )
    {
        match self.release(fh.0) {
            Ok(()) => {
                reply.ok();
            },
            Err(e) => {
                reply.error(Errno::from_i32(errno(&e)));
            }
        }
    }

}
