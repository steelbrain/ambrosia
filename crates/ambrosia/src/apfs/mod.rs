//! A read-only APFS reader, enough to read the boot files in a container's
//! Preboot volume.
//!
//! [`Container::open`] reads a container from its partition: the latest valid
//! checkpoint's superblock, the container's object map, and each volume's
//! superblock. [`Container::volume`] opens one volume's file-system tree, and
//! [`Volume`] looks up paths, lists directories, and reads files, straight
//! into the caller's buffer.
//!
//! It handles what a macOS guest's disk uses and nothing more. Encrypted and
//! sealed volumes, snapshots, compressed files and writing are left out, and
//! each gives an [`Error`] rather than wrong data. Every object read is
//! checked against its Fletcher-64 checksum and against the type, object ID
//! and transaction its reference implies, and every walk is bounded, so a
//! corrupt disk gives an error rather than a hang.

mod btree;
mod container;
mod object;
mod volume;

#[cfg(test)]
mod synthetic;

pub use container::{Container, Role, VolumeInfo};
pub use volume::{DirEntry, Inode, Kind, Volume};

use alloc::string::String;
use core::fmt;

/// A UUID as APFS stores it: 16 bytes in text order. It prints in the usual
/// uppercase form, as `diskutil` does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Uuid(
    /// The 16 bytes, in text order.
    pub [u8; 16],
);

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, byte) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                f.write_str("-")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Why a read failed. Blocks are numbered from the container's start, in the
/// container's block size.
#[derive(Debug)]
pub enum Error<E> {
    /// The disk failed to read this block.
    Read {
        /// The block the read starts in.
        block: u64,
        /// The disk's error.
        error: E,
    },
    /// A reference points past the container's last block.
    OutOfRange {
        /// The block the reference starts in.
        block: u64,
    },
    /// Block 0 holds no APFS container superblock.
    NotApfs,
    /// The container's block size is not one APFS allows.
    BlockSize(u32),
    /// A superblock field is out of range.
    BadSuperblock(&'static str),
    /// The object in this block fails its Fletcher-64 checksum.
    Checksum {
        /// The object's block.
        block: u64,
    },
    /// The object in this block is not the one its reference names.
    WrongObject {
        /// The object's block.
        block: u64,
        /// Which header field differs from the reference: "type",
        /// "subtype", "object ID" or "transaction".
        what: &'static str,
        /// The object ID the header holds.
        oid: u64,
        /// The transaction the header holds.
        xid: u64,
        /// The type the header holds, with its storage kind and flags.
        kind: u32,
        /// The subtype the header holds.
        subtype: u32,
    },
    /// No block of the checkpoint descriptor area holds a valid superblock.
    NoCheckpoint,
    /// The object map has no live entry for this virtual object as of this
    /// transaction.
    NotMapped {
        /// The virtual object's ID.
        oid: u64,
        /// The transaction it was looked up as of.
        xid: u64,
    },
    /// A B-tree node in this block is laid out wrong.
    BadNode {
        /// The node's block.
        block: u64,
        /// What is wrong with it.
        what: &'static str,
    },
    /// A walk of the B-tree with this root read more nodes than the tree
    /// says it has, so the tree reaches a node twice or its count is wrong.
    TreeTooBig {
        /// The object ID of the tree's root.
        root: u64,
    },
    /// A file-system record of this object is malformed.
    BadRecord {
        /// The ID of the object the record belongs to.
        id: u64,
        /// What is wrong with it.
        what: &'static str,
    },
    /// The disk uses something this reader leaves out.
    Unsupported(&'static str),
    /// [`Container::volume`] was given an index past the volume list.
    NoSuchVolume(usize),
    /// The file system has no inode with this ID.
    NoInode {
        /// The inode ID looked up.
        id: u64,
    },
    /// The directory with this ID has no entry of this name.
    NotFound {
        /// The directory's inode ID.
        dir: u64,
        /// The name looked up.
        name: String,
    },
    /// A path goes through this object, which is not a directory.
    NotADirectory {
        /// The object's inode ID.
        id: u64,
    },
    /// This file's data is compressed (decmpfs), which this reader leaves
    /// out.
    Compressed {
        /// The file's inode ID.
        id: u64,
    },
    /// This file is larger than the memory left to read it into.
    TooLarge {
        /// The file's inode ID.
        id: u64,
        /// Its size in bytes, from its inode.
        size: u64,
    },
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { block, error } => {
                write!(f, "APFS: read failed at block {block:#x}: {error}")
            }
            Error::OutOfRange { block } => write!(f, "APFS: block {block:#x} is past the end"),
            Error::NotApfs => f.write_str("APFS: no container superblock"),
            Error::BlockSize(size) => write!(f, "APFS: block size {size}"),
            Error::BadSuperblock(what) => write!(f, "APFS: bad superblock: {what}"),
            Error::Checksum { block } => write!(f, "APFS: block {block:#x} fails its checksum"),
            Error::WrongObject {
                block,
                what,
                oid,
                xid,
                kind,
                subtype,
            } => write!(
                f,
                "APFS: block {block:#x} holds object {oid:#x} xid {xid:#x} \
                 type {kind:#x}/{subtype:#x}: wrong {what}"
            ),
            Error::NoCheckpoint => f.write_str("APFS: no valid checkpoint"),
            Error::NotMapped { oid, xid } => {
                write!(f, "APFS: object {oid:#x} is not mapped at xid {xid:#x}")
            }
            Error::BadNode { block, what } => write!(f, "APFS: B-tree node {block:#x}: {what}"),
            Error::TreeTooBig { root } => {
                write!(f, "APFS: B-tree {root:#x} has more nodes than it says")
            }
            Error::BadRecord { id, what } => write!(f, "APFS: record of object {id:#x}: {what}"),
            Error::Unsupported(what) => write!(f, "APFS: unsupported: {what}"),
            Error::NoSuchVolume(index) => write!(f, "APFS: no volume {index}"),
            Error::NoInode { id } => write!(f, "APFS: no inode {id:#x}"),
            Error::NotFound { dir, name } => {
                write!(f, "APFS: no {name:?} in directory {dir:#x}")
            }
            Error::NotADirectory { id } => write!(f, "APFS: object {id:#x} is not a directory"),
            Error::Compressed { id } => write!(f, "APFS: file {id:#x} is compressed"),
            Error::TooLarge { id, size } => {
                write!(
                    f,
                    "APFS: file {id:#x} of {size} bytes does not fit in memory"
                )
            }
        }
    }
}

/// Little-endian fields at a byte offset. Callers check that the bytes are
/// there: either the buffer is a whole block and the offset a fixed one well
/// inside the smallest block, or they checked the record's length first.
fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from(u32_at(b, off)) | (u64::from(u32_at(b, off + 4)) << 32)
}

/// The UUID at a byte offset, under the same rule.
fn uuid_at(b: &[u8], off: usize) -> Uuid {
    let mut u = [0u8; 16];
    u.copy_from_slice(&b[off..off + 16]);
    Uuid(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_text() {
        let u = Uuid([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54,
            0x32, 0x10,
        ]);
        assert_eq!(u.to_string(), "01234567-89AB-CDEF-FEDC-BA9876543210");
    }
}
