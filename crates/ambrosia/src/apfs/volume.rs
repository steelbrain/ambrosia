//! A volume: its object map and file-system tree, read as of the volume's
//! transaction.
//!
//! The file-system tree's records are keyed by an object ID and a record
//! type, then fields of that type. An inode's record holds its mode, flags
//! and, in an extended field, its data stream's size; its data is in file
//! extent records keyed by the stream's ID and logical offset; and a
//! directory's entries are directory records keyed by the directory's ID
//! and the entry's name.

use super::btree::{Cursor, Flow, Omap, Storage, Tree};
use super::container::{Container, VolumeInfo};
use super::object::{BTREE, Disk, FSTREE, VIRTUAL};
use super::{Error, u16_at, u32_at, u64_at};
use crate::block::BlockDevice;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Record types, in a key's top 4 bits.
const TYPE_INODE: u8 = 3;
const TYPE_FILE_EXTENT: u8 = 8;
const TYPE_DIR_REC: u8 = 9;
const ID_MASK: u64 = (1 << 60) - 1;

/// The root directory's inode.
const ROOT_DIR: u64 = 2;

/// Volume incompatible features.
const CASE_INSENSITIVE: u64 = 0x1;
const NORMALIZATION_INSENSITIVE: u64 = 0x8;
const SEALED: u64 = 0x20;
/// The features this reader handles: names compared without case or
/// normalization, which only changes how directory records are keyed.
const KNOWN_INCOMPAT: u64 = CASE_INSENSITIVE | NORMALIZATION_INSENSITIVE;
/// The volume flag that says it is not encrypted.
const UNENCRYPTED: u64 = 0x1;

/// An inode's fixed part, before its extended fields.
const INODE_FIXED: usize = 92;
/// The extended field that holds an inode's data stream.
const EXT_DSTREAM: u8 = 8;
/// `bsd_flags`: the data is compressed into an extended attribute or the
/// resource fork, through decmpfs.
const UF_COMPRESSED: u32 = 0x20;

/// A file extent's length, below its flags.
const EXTENT_LEN_MASK: u64 = (1 << 56) - 1;

/// What an inode is, from its mode or a directory entry's type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A regular file.
    File,
    /// A directory.
    Dir,
    /// A symbolic link.
    Symlink,
    /// Another kind, as the BSD `DT_*` value of its type.
    Other(u16),
}

impl Kind {
    fn from_dtype(dtype: u16) -> Kind {
        match dtype {
            4 => Kind::Dir,
            8 => Kind::File,
            10 => Kind::Symlink,
            other => Kind::Other(other),
        }
    }
}

/// An inode, as [`Volume::stat`] reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inode {
    /// Its ID, the object ID its records are keyed by.
    pub id: u64,
    /// The ID of the directory it is in.
    pub parent: u64,
    /// What it is, from its mode.
    pub kind: Kind,
    /// Its BSD mode: its type and permission bits.
    pub mode: u16,
    /// The size of its data in bytes: 0 for a directory.
    pub size: u64,
    /// Its BSD flags, as `chflags` sets them.
    pub bsd_flags: u32,
    /// The ID its file extents are keyed by.
    stream: u64,
}

impl Inode {
    /// Whether its data is compressed through decmpfs, which
    /// [`Volume::read_at`] does not read.
    pub fn is_compressed(&self) -> bool {
        self.bsd_flags & UF_COMPRESSED != 0
    }
}

/// An entry of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// Its name, without the stored NUL, with bytes that are not UTF-8
    /// replaced.
    pub name: String,
    /// Its inode.
    pub id: u64,
    /// What it is, from the entry's type.
    pub kind: Kind,
}

/// An open volume. It borrows its container, which holds the disk.
pub struct Volume<'c, D: BlockDevice> {
    disk: &'c mut Disk<D>,
    info: VolumeInfo,
    omap: Omap,
    tree: Tree,
    cursor: Cursor,
    /// Whether directory records' keys carry a name hash.
    hashed: bool,
    /// Whether names compare without case.
    fold_case: bool,
}

impl<'c, D: BlockDevice> Volume<'c, D> {
    /// Opens the volume `info` describes, in `container`: its object map and
    /// its file-system tree. An encrypted or sealed volume, or one with a
    /// feature this reader does not know, gives [`Error::Unsupported`].
    pub(crate) fn open(
        container: &'c mut Container<D>,
        info: VolumeInfo,
    ) -> Result<Self, Error<D::Error>> {
        if info.fs_flags & UNENCRYPTED == 0 {
            return Err(Error::Unsupported("encrypted volume"));
        }
        if info.incompat & SEALED != 0 {
            return Err(Error::Unsupported("sealed volume"));
        }
        if info.incompat & !KNOWN_INCOMPAT != 0 {
            return Err(Error::Unsupported("volume feature"));
        }
        if info.root_tree_type != VIRTUAL | BTREE {
            return Err(Error::Unsupported("file-system tree not virtual"));
        }
        let disk = &mut container.disk;
        let mut omap = Omap::open(disk, info.omap, container.xid)?;
        let mut cursor = Cursor::default();
        let mut storage = Storage::Virtual {
            omap: &mut omap,
            xid: info.xid,
        };
        let tree = Tree::open(disk, &mut cursor, &mut storage, info.root_tree, FSTREE)?;
        Ok(Volume {
            disk,
            hashed: info.incompat & (CASE_INSENSITIVE | NORMALIZATION_INSENSITIVE) != 0,
            fold_case: info.incompat & CASE_INSENSITIVE != 0,
            info,
            omap,
            tree,
            cursor,
        })
    }

    /// The volume, as its superblock describes it.
    pub fn info(&self) -> &VolumeInfo {
        &self.info
    }

    /// The root directory.
    pub fn root(&mut self) -> Result<Inode, Error<D::Error>> {
        self.stat(ROOT_DIR)
    }

    /// Reads inode `id`.
    pub fn stat(&mut self, id: u64) -> Result<Inode, Error<D::Error>> {
        let mut inode = None;
        self.records(id, TYPE_INODE, |_, key, value| {
            if key.len() != 8 {
                return Err(Error::BadRecord {
                    id,
                    what: "inode key length",
                });
            }
            inode = Some(parse_inode(id, value).map_err(|what| Error::BadRecord { id, what })?);
            Ok(Flow::Stop)
        })?;
        inode.ok_or(Error::NoInode { id })
    }

    /// Looks up `path`, a `/`-separated path from the root directory.
    ///
    /// Names are compared byte for byte, or on a case-insensitive volume
    /// with ASCII letters folded. That finds every ASCII name as macOS
    /// would. A name with other characters is found only in the form it is
    /// stored in, since matching it as APFS does takes Unicode's case and
    /// normalization tables, which the paths a boot looks up do not need.
    pub fn lookup(&mut self, path: &str) -> Result<Inode, Error<D::Error>> {
        let mut id = ROOT_DIR;
        let mut kind = Kind::Dir;
        for name in path.split('/').filter(|c| !c.is_empty()) {
            if kind != Kind::Dir {
                return Err(Error::NotADirectory { id });
            }
            let fold = self.fold_case;
            let mut found = None;
            self.entries(id, |entry_name, entry_id, entry_kind| {
                let same = if fold {
                    entry_name.eq_ignore_ascii_case(name.as_bytes())
                } else {
                    entry_name == name.as_bytes()
                };
                if same {
                    found = Some((entry_id, entry_kind));
                    return Flow::Stop;
                }
                Flow::Continue
            })?;
            (id, kind) = found.ok_or_else(|| Error::NotFound {
                dir: id,
                name: name.into(),
            })?;
        }
        self.stat(id)
    }

    /// Lists a directory's entries, in the order the tree keeps them.
    pub fn read_dir(&mut self, dir: &Inode) -> Result<Vec<DirEntry>, Error<D::Error>> {
        if dir.kind != Kind::Dir {
            return Err(Error::NotADirectory { id: dir.id });
        }
        let mut list = Vec::new();
        self.entries(dir.id, |name, id, kind| {
            list.push(DirEntry {
                name: String::from_utf8_lossy(name).into_owned(),
                id,
                kind,
            });
            Flow::Continue
        })?;
        Ok(list)
    }

    /// Reads `file`'s data from byte `offset` on into `buf`, and returns how
    /// many bytes it read: all of `buf`, or up to the file's end. Each
    /// extent's part goes straight into `buf` in one disk request, apart from
    /// partial sectors at the ends of the range. A hole reads as zeroes.
    pub fn read_at(
        &mut self,
        file: &Inode,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<usize, Error<D::Error>> {
        if file.is_compressed() {
            return Err(Error::Compressed { id: file.id });
        }
        let Some(left) = file.size.checked_sub(offset) else {
            return Ok(0);
        };
        let len = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let buf = &mut buf[..len];
        let end = offset + len as u64;
        let bs = self.disk.block_size as u64;
        let id = file.id;
        let bad = |what| Error::BadRecord { id, what };

        // `pos` is the next byte of the file to fill, and `prev_end` the end
        // of the extent before, which the next must not start before.
        let mut pos = offset;
        let mut prev_end = 0;
        self.records(file.stream, TYPE_FILE_EXTENT, |disk, key, value| {
            if key.len() != 16 || value.len() < 24 {
                return Err(bad("file extent record length"));
            }
            let logical = u64_at(key, 8);
            let len_and_flags = u64_at(value, 0);
            let ext_len = len_and_flags & EXTENT_LEN_MASK;
            let phys = u64_at(value, 8);
            if len_and_flags & !EXTENT_LEN_MASK != 0 {
                return Err(Error::Unsupported("file extent flags"));
            }
            if logical < prev_end
                || !logical.is_multiple_of(bs)
                || ext_len == 0
                || !ext_len.is_multiple_of(bs)
            {
                return Err(bad("file extents overlap or are not whole blocks"));
            }
            let ext_end = logical
                .checked_add(ext_len)
                .ok_or(bad("file extent too long"))?;
            prev_end = ext_end;
            if ext_end <= pos {
                return Ok(Flow::Continue);
            }
            if logical >= end {
                return Ok(Flow::Stop);
            }
            if logical > pos {
                buf[(pos - offset) as usize..(logical - offset) as usize].fill(0);
                pos = logical;
            }
            let stop = ext_end.min(end);
            let dst = &mut buf[(pos - offset) as usize..(stop - offset) as usize];
            if phys == 0 {
                // An extent with no block is a hole.
                dst.fill(0);
            } else {
                let at = phys
                    .checked_mul(bs)
                    .and_then(|at| at.checked_add(pos - logical))
                    .ok_or(Error::OutOfRange { block: phys })?;
                disk.read_bytes(at, dst)?;
            }
            pos = stop;
            Ok(if pos == end {
                Flow::Stop
            } else {
                Flow::Continue
            })
        })?;
        // Past the last extent is a hole too.
        buf[(pos - offset) as usize..].fill(0);
        Ok(len)
    }

    /// Reads all of `file`'s data.
    pub fn read(&mut self, file: &Inode) -> Result<Vec<u8>, Error<D::Error>> {
        // The size is the inode's, so a corrupt one can ask for more memory
        // than there is: that gives an error rather than an abort.
        let too_large = || Error::TooLarge {
            id: file.id,
            size: file.size,
        };
        let size = usize::try_from(file.size).map_err(|_| too_large())?;
        let mut data = Vec::new();
        data.try_reserve_exact(size).map_err(|_| too_large())?;
        data.resize(size, 0);
        self.read_at(file, 0, &mut data)?;
        Ok(data)
    }

    /// Calls `visit` with each entry of directory `dir`: its name, without
    /// the stored NUL, its inode and its kind, until `visit` says to stop.
    fn entries<F>(&mut self, dir: u64, mut visit: F) -> Result<(), Error<D::Error>>
    where
        F: FnMut(&[u8], u64, Kind) -> Flow,
    {
        let hashed = self.hashed;
        let bad = |what| Error::BadRecord { id: dir, what };
        self.records(dir, TYPE_DIR_REC, |_, key, value| {
            let name = entry_name(key, hashed).map_err(bad)?;
            if value.len() < 18 {
                return Err(bad("directory record too short"));
            }
            let kind = Kind::from_dtype(u16_at(value, 16) & 0xf);
            Ok(visit(name, u64_at(value, 0), kind))
        })
    }

    /// Walks object `id`'s records of type `ty`, in key order.
    fn records<F>(&mut self, id: u64, ty: u8, mut visit: F) -> Result<(), Error<D::Error>>
    where
        F: FnMut(&mut Disk<D>, &[u8], &[u8]) -> Result<Flow, Error<D::Error>>,
    {
        let want = (id, ty);
        let mut storage = Storage::Virtual {
            omap: &mut self.omap,
            xid: self.info.xid,
        };
        // The target is the first possible key of the object's records of
        // this type, so a record with the same ID and type is after it.
        self.tree.walk(
            self.disk,
            &mut self.cursor,
            &mut storage,
            |key| key_head(key).cmp(&want).then(Ordering::Greater),
            |disk, key, value| match key_head(key).cmp(&want) {
                Ordering::Less => Ok(Flow::Continue),
                Ordering::Equal => visit(disk, key, value),
                Ordering::Greater => Ok(Flow::Stop),
            },
        )
    }
}

/// A file-system key's object ID and record type, which keys sort by first.
fn key_head(key: &[u8]) -> (u64, u8) {
    let head = u64_at(key, 0);
    (head & ID_MASK, (head >> 60) as u8)
}

/// A directory record key's name, without its NUL. A hashed key holds the
/// name's length and hash in 32 bits, the length in the low 10; a plain one
/// holds the length in 16.
fn entry_name(key: &[u8], hashed: bool) -> Result<&[u8], &'static str> {
    let (start, len) = if hashed {
        if key.len() < 12 {
            return Err("directory record key too short");
        }
        (12, (u32_at(key, 8) & 0x3ff) as usize)
    } else {
        if key.len() < 10 {
            return Err("directory record key too short");
        }
        (10, usize::from(u16_at(key, 8)))
    };
    if key.len() != start + len || len == 0 || key[start + len - 1] != 0 {
        return Err("directory record name length");
    }
    Ok(&key[start..start + len - 1])
}

/// Parses inode `id`'s record value.
fn parse_inode(id: u64, v: &[u8]) -> Result<Inode, &'static str> {
    if v.len() < INODE_FIXED {
        return Err("inode record too short");
    }
    let mode = u16_at(v, 80);
    Ok(Inode {
        id,
        parent: u64_at(v, 0),
        kind: Kind::from_dtype(mode >> 12),
        mode,
        size: stream_size(&v[INODE_FIXED..])?,
        bsd_flags: u32_at(v, 68),
        stream: u64_at(v, 8),
    })
}

/// The data stream size in an inode's extended fields, or 0 if it has none.
/// The fields are a count and the bytes used, a 4-byte type, flags and size
/// for each field, then each field's data padded to 8 bytes.
fn stream_size(xf: &[u8]) -> Result<u64, &'static str> {
    if xf.is_empty() {
        return Ok(0);
    }
    if xf.len() < 4 {
        return Err("inode extended fields too short");
    }
    let count = usize::from(u16_at(xf, 0));
    let used = usize::from(u16_at(xf, 2));
    // The data follows the descriptors with no gap, and fills the record.
    let mut data = 4 + 4 * count;
    if data + used != xf.len() {
        return Err("inode extended fields' length");
    }
    for i in 0..count {
        let ty = xf[4 + 4 * i];
        let size = usize::from(u16_at(xf, 4 + 4 * i + 2));
        let field = xf
            .get(data..data + size)
            .ok_or("inode extended field past its record")?;
        if ty == EXT_DSTREAM {
            if size < 8 {
                return Err("inode data stream too short");
            }
            return Ok(u64_at(field, 0));
        }
        data += size.next_multiple_of(8);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Extended fields holding one data stream field, of `size` bytes.
    fn dstream_fields(size: u16) -> Vec<u8> {
        let mut xf = vec![1, 0, 40, 0, EXT_DSTREAM, 0];
        xf.extend_from_slice(&size.to_le_bytes());
        xf.extend_from_slice(&0x1234u64.to_le_bytes());
        xf.resize(8 + 40, 0);
        xf
    }

    #[test]
    fn reads_stream_size() {
        assert_eq!(stream_size(&dstream_fields(40)), Ok(0x1234));
        assert_eq!(stream_size(&[]), Ok(0));
    }

    #[test]
    fn rejects_bad_extended_fields() {
        let xf = dstream_fields(40);
        assert!(stream_size(&xf[..3]).is_err(), "no room for the count");
        assert!(stream_size(&xf[..47]).is_err(), "used length");
        let mut many = xf.clone();
        many[..2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(stream_size(&many).is_err(), "descriptors past the record");
        assert!(stream_size(&dstream_fields(48)).is_err(), "field past it");
        assert!(stream_size(&dstream_fields(4)).is_err(), "short stream");
    }

    #[test]
    fn reads_entry_names() {
        let mut hashed = vec![0u8; 8];
        hashed.extend_from_slice(&(4u32 | 0xabc << 10).to_le_bytes());
        hashed.extend_from_slice(b"abc\0");
        assert_eq!(entry_name(&hashed, true), Ok(&b"abc"[..]));
        assert!(entry_name(&hashed[..11], true).is_err(), "short key");
        assert!(entry_name(&hashed[..15], true).is_err(), "name past key");

        let mut plain = vec![0u8; 8];
        plain.extend_from_slice(&4u16.to_le_bytes());
        plain.extend_from_slice(b"abc\0");
        assert_eq!(entry_name(&plain, false), Ok(&b"abc"[..]));
        assert!(entry_name(&plain[..9], false).is_err(), "short key");
        let mut no_nul = plain.clone();
        no_nul[13] = b'd';
        assert!(entry_name(&no_nul, false).is_err(), "no NUL");
        let mut empty = vec![0u8; 8];
        empty.extend_from_slice(&0u16.to_le_bytes());
        assert!(entry_name(&empty, false).is_err(), "no name");
    }

    #[test]
    fn rejects_short_inode() {
        assert!(parse_inode(5, &[0; INODE_FIXED - 1]).is_err());
        assert_eq!(parse_inode(5, &[0; INODE_FIXED]).unwrap().size, 0);
    }
}
