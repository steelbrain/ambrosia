//! APFS objects: blocks of the container, each starting with a header that
//! holds its checksum, object ID, transaction ID and type.

use super::{Error, u32_at, u64_at};
use crate::block::{BlockDevice, SECTOR};

/// Storage kinds, in the top bits of a header's type.
pub(crate) const VIRTUAL: u32 = 0;
pub(crate) const PHYSICAL: u32 = 0x4000_0000;

/// Object types, in the low 16 bits of a header's type, and B-tree subtypes.
pub(crate) const NX_SUPERBLOCK: u32 = 0x01;
pub(crate) const BTREE: u32 = 0x02;
pub(crate) const BTREE_NODE: u32 = 0x03;
pub(crate) const OMAP: u32 = 0x0b;
pub(crate) const FS: u32 = 0x0d;
pub(crate) const FSTREE: u32 = 0x0e;

const TYPE_MASK: u32 = 0xffff;

/// The smallest block APFS allows. Every fixed field this reader takes from
/// a superblock or node header lies inside it.
pub(crate) const MIN_BLOCK: usize = 4096;

/// The container's blocks, on the partition that holds them.
pub(crate) struct Disk<D> {
    /// The disk the partition is on.
    pub dev: D,
    /// The partition's first sector.
    first_lba: u64,
    /// The container's block size, in bytes.
    pub block_size: usize,
    /// How many blocks the container has, which no read goes past. It is
    /// `u64::MAX` until [`Disk::set_size`] gives it.
    pub block_count: u64,
}

/// What a reference says about the object it points at.
pub(crate) struct Expect {
    /// The type with its storage kind, as the header holds it.
    pub kind: u32,
    /// The subtype, or `None` where any is allowed.
    pub subtype: Option<u32>,
    /// The object ID: a physical object's is its block.
    pub oid: u64,
    /// The transaction its header must hold.
    pub xid: Xid,
}

/// The transaction an object's header must hold.
pub(crate) enum Xid {
    /// A virtual object's version: the one its object map entry names.
    Exactly(u64),
    /// A physical object: written in the checkpoint read or before.
    AtMost(u64),
}

impl<D: BlockDevice> Disk<D> {
    /// A container on `dev` from sector `first_lba` on. Its size is not
    /// known until its superblock is read, so until [`Disk::set_size`] any
    /// block can be read.
    pub fn new(dev: D, first_lba: u64) -> Self {
        Disk {
            dev,
            first_lba,
            block_size: MIN_BLOCK,
            block_count: u64::MAX,
        }
    }

    /// Sets the container's block size and block count, from its
    /// superblock.
    pub fn set_size(&mut self, block_size: usize, block_count: u64) {
        self.block_size = block_size;
        self.block_count = block_count;
    }

    /// Reads whole blocks from `block` on into `buf`.
    pub fn read_blocks(&mut self, block: u64, buf: &mut [u8]) -> Result<(), Error<D::Error>> {
        let bytes = block
            .checked_mul(self.block_size as u64)
            .filter(|_| self.fits(block, buf.len() as u64))
            .ok_or(Error::OutOfRange { block })?;
        self.read_sectors(bytes, buf)
    }

    /// Reads `buf.len()` bytes from byte `at` of the container on. The
    /// sector-aligned middle goes straight into `buf` in one request; only a
    /// partial first or last sector goes through a sector on the stack.
    pub fn read_bytes(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error<D::Error>> {
        let bs = self.block_size as u64;
        let end = at.checked_add(buf.len() as u64);
        if end.is_none_or(|end| end.div_ceil(bs) > self.block_count) {
            return Err(Error::OutOfRange { block: at / bs });
        }
        let mut done = 0;
        while done < buf.len() {
            let pos = at + done as u64;
            let skip = (pos % SECTOR as u64) as usize;
            let left = buf.len() - done;
            if skip != 0 || left < SECTOR {
                let mut sector = [0u8; SECTOR];
                self.read_sectors(pos - skip as u64, &mut sector)?;
                let n = (SECTOR - skip).min(left);
                buf[done..done + n].copy_from_slice(&sector[skip..skip + n]);
                done += n;
            } else {
                let n = left - left % SECTOR;
                self.read_sectors(pos, &mut buf[done..done + n])?;
                done += n;
            }
        }
        Ok(())
    }

    /// Reads the object in `block` into `buf`, a block long, and checks its
    /// checksum and that its header is what `expect` says.
    pub fn read_object(
        &mut self,
        block: u64,
        buf: &mut [u8],
        expect: &Expect,
    ) -> Result<(), Error<D::Error>> {
        self.read_blocks(block, buf)?;
        check_object(block, buf, expect)
    }

    /// Whether `len` bytes from `block` on lie inside the container.
    fn fits(&self, block: u64, len: u64) -> bool {
        let blocks = len.div_ceil(self.block_size as u64);
        block
            .checked_add(blocks)
            .is_some_and(|end| end <= self.block_count)
    }

    /// Reads sectors from container byte `at`, a multiple of the sector size.
    fn read_sectors(&mut self, at: u64, buf: &mut [u8]) -> Result<(), Error<D::Error>> {
        let block = at / self.block_size as u64;
        let sector = self
            .first_lba
            .checked_add(at / SECTOR as u64)
            .ok_or(Error::OutOfRange { block })?;
        self.dev
            .read(sector, buf)
            .map_err(|error| Error::Read { block, error })
    }
}

/// Checks an object's checksum, then its header against `expect`.
pub(crate) fn check_object<E>(block: u64, obj: &[u8], expect: &Expect) -> Result<(), Error<E>> {
    if !checksum_ok(obj) {
        return Err(Error::Checksum { block });
    }
    let oid = u64_at(obj, 8);
    let xid = u64_at(obj, 16);
    let kind = u32_at(obj, 24);
    let subtype = u32_at(obj, 28);
    let what = if kind != expect.kind {
        "type"
    } else if expect.subtype.is_some_and(|s| s != subtype) {
        "subtype"
    } else if oid != expect.oid {
        "object ID"
    } else if match expect.xid {
        Xid::Exactly(x) => xid != x,
        Xid::AtMost(x) => xid > x,
    } {
        "transaction"
    } else {
        return Ok(());
    };
    Err(Error::WrongObject {
        block,
        what,
        oid,
        xid,
        kind,
        subtype,
    })
}

/// The header's type without its storage kind and flags.
pub(crate) fn object_type(obj: &[u8]) -> u32 {
    u32_at(obj, 24) & TYPE_MASK
}

/// Whether an object's stored checksum matches its bytes.
pub(crate) fn checksum_ok(obj: &[u8]) -> bool {
    u64_at(obj, 0) == fletcher64(&obj[8..])
}

/// APFS's Fletcher-64: two running sums of the little-endian 32-bit words,
/// each modulo 2^32 - 1, folded into the two check words the header
/// stores. `data` is the object without its first 8 bytes, the checksum.
pub(crate) fn fletcher64(data: &[u8]) -> u64 {
    const MOD: u64 = 0xffff_ffff;
    let (mut lo, mut hi) = (0u64, 0u64);
    for word in data.as_chunks::<4>().0 {
        lo = (lo + u64::from(u32::from_le_bytes(*word))) % MOD;
        hi = (hi + lo) % MOD;
    }
    let c1 = MOD - (lo + hi) % MOD;
    let c2 = MOD - (lo + c1) % MOD;
    (c2 << 32) | c1
}

/// Stores an object's checksum, for tests that build objects.
#[cfg(test)]
pub(crate) fn seal(obj: &mut [u8]) {
    let sum = fletcher64(&obj[8..]);
    obj[..8].copy_from_slice(&sum.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fletcher64_small() {
        // Words 1 and 2: the sums are 3 and 4, so the check words are
        // 0xffffffff - 7 and 0xffffffff - ((3 + that) mod 0xffffffff).
        let data = [1, 0, 0, 0, 2, 0, 0, 0];
        assert_eq!(fletcher64(&data), 0x0000_0004_ffff_fff8);
    }

    #[test]
    fn fletcher64_wraps() {
        // Words of 0xffffffff are 0 modulo 2^32 - 1, as are the sums.
        assert_eq!(fletcher64(&[0xff; 16]), 0xffff_ffff_ffff_ffff);
    }

    fn object() -> [u8; MIN_BLOCK] {
        let mut obj = [0u8; MIN_BLOCK];
        obj[8..16].copy_from_slice(&0x402u64.to_le_bytes());
        obj[16..24].copy_from_slice(&7u64.to_le_bytes());
        obj[24..28].copy_from_slice(&(PHYSICAL | OMAP).to_le_bytes());
        obj[100] = 0x5a;
        seal(&mut obj);
        obj
    }

    fn expect() -> Expect {
        Expect {
            kind: PHYSICAL | OMAP,
            subtype: Some(0),
            oid: 0x402,
            xid: Xid::AtMost(7),
        }
    }

    #[test]
    fn checks_object() {
        let obj = object();
        assert!(check_object::<()>(5, &obj, &expect()).is_ok());
        assert_eq!(object_type(&obj), OMAP);
    }

    #[test]
    fn rejects_corrupt_object() {
        let mut obj = object();
        obj[4000] ^= 1;
        assert!(matches!(
            check_object::<()>(5, &obj, &expect()),
            Err(Error::Checksum { block: 5 })
        ));
    }

    #[test]
    fn rejects_wrong_header() {
        let obj = object();
        let wrong = |e: Expect| match check_object::<()>(5, &obj, &e) {
            Err(Error::WrongObject { what, .. }) => what,
            _ => "",
        };
        assert_eq!(
            wrong(Expect {
                kind: OMAP,
                ..expect()
            }),
            "type"
        );
        assert_eq!(
            wrong(Expect {
                subtype: Some(FSTREE),
                ..expect()
            }),
            "subtype"
        );
        assert_eq!(
            wrong(Expect {
                oid: 0x403,
                ..expect()
            }),
            "object ID"
        );
        assert_eq!(
            wrong(Expect {
                xid: Xid::AtMost(6),
                ..expect()
            }),
            "transaction"
        );
        assert_eq!(
            wrong(Expect {
                xid: Xid::Exactly(8),
                ..expect()
            }),
            "transaction"
        );
    }
}
