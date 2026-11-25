use crate::storage::TagFs;
use crate::storage::{ FileKind, FileStat };
use crate::storage::errno;

use core::time::Duration;

use fuser::Filesystem;
use fuser::{ FileAttr, FileType, FileHandle };
use fuser::{ Request, INodeNo, Errno, Generation};
use fuser::{ ReplyAttr, ReplyEntry, ReplyDirectory };

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

}
