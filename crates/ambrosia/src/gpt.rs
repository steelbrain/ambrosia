//! The disk's GUID partition table: the primary header at LBA 1 and its
//! partition entries, each checked against its CRC32. The backup table at the
//! end of the disk is not read, so a disk whose primary table is damaged
//! doesn't boot.

use crate::block::{BlockDevice, SECTOR};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// The partition type of an APFS container.
pub const APFS_TYPE: &str = "7C3457EF-0000-11AA-AA11-00306543ECAC";

/// The partition type of the iBoot system container, the APFS container
/// that keeps the LocalPolicy and the SFR manifest.
pub const ISC_TYPE: &str = "69646961-6700-11AA-AA11-00306543ECAC";

/// The most partition-entry bytes read. Partitioning tools write 128 entries
/// of 128 bytes (16 KiB); this only bounds a corrupt header.
const MAX_ENTRIES_BYTES: usize = 1 << 20;

/// A GUID as GPT stores it: the first three fields little-endian, the rest
/// as bytes. It prints in the usual uppercase form, as in [`APFS_TYPE`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(pub [u8; 16]);

impl Guid {
    /// Whether every byte is zero, which marks an unused partition entry.
    pub fn is_nil(&self) -> bool {
        self.0 == [0; 16]
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:08X}-{:04X}-{:04X}-",
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            u16::from_le_bytes([b[4], b[5]]),
            u16::from_le_bytes([b[6], b[7]]),
        )?;
        for (i, byte) in b[8..].iter().enumerate() {
            if i == 2 {
                f.write_str("-")?;
            }
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// One used entry of the partition table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Partition {
    /// Its index in the entry array, from 0.
    pub index: usize,
    /// What it holds, such as [`APFS_TYPE`].
    pub type_guid: Guid,
    /// The GUID that names this partition alone.
    pub unique_guid: Guid,
    /// The first sector it holds.
    pub first_lba: u64,
    /// The last sector it holds, so that it holds `first_lba..=last_lba`.
    pub last_lba: u64,
    /// Its attribute bits.
    pub attributes: u64,
    /// Its name, decoded from UTF-16 up to the first NUL.
    pub name: String,
}

/// The partition table.
#[derive(Clone, Debug)]
pub struct Gpt {
    /// The GUID that names the disk.
    pub disk_guid: Guid,
    /// The used entries, in table order.
    pub partitions: Vec<Partition>,
}

/// Why the table could not be read.
#[derive(Debug)]
pub enum Error<E> {
    /// The disk failed to read these sectors.
    Read {
        /// The first sector of the read.
        sector: u64,
        /// The disk's error.
        error: E,
    },
    /// LBA 1 does not start with "EFI PART".
    NoSignature,
    /// A field of the header is out of range; the text says which.
    BadHeader(&'static str),
    /// The header's CRC32 does not match its bytes.
    HeaderCrc {
        /// The CRC32 the header holds.
        stored: u32,
        /// The CRC32 of the header's bytes.
        computed: u32,
    },
    /// The entry array's CRC32 does not match its bytes.
    EntriesCrc {
        /// The CRC32 the header gives for the entry array.
        stored: u32,
        /// The CRC32 of the entry array's bytes.
        computed: u32,
    },
    /// A used entry ends before it starts.
    BadPartition {
        /// The entry's index in the array.
        index: usize,
        /// The entry's first sector.
        first: u64,
        /// The entry's last sector, below its first.
        last: u64,
    },
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Read { sector, error } => {
                write!(f, "GPT: read failed at sector {sector}: {error}")
            }
            Error::NoSignature => f.write_str("GPT: no header signature at LBA 1"),
            Error::BadHeader(what) => write!(f, "GPT: bad header: {what}"),
            Error::HeaderCrc { stored, computed } => {
                write!(
                    f,
                    "GPT: header CRC {stored:#010x}, computed {computed:#010x}"
                )
            }
            Error::EntriesCrc { stored, computed } => {
                write!(
                    f,
                    "GPT: entries CRC {stored:#010x}, computed {computed:#010x}"
                )
            }
            Error::BadPartition { index, first, last } => {
                write!(f, "GPT: entry {index} runs from LBA {first} back to {last}")
            }
        }
    }
}

impl Gpt {
    /// Reads and checks the primary table.
    pub fn read<D: BlockDevice>(dev: &mut D) -> Result<Gpt, Error<D::Error>> {
        let mut header = [0u8; SECTOR];
        dev.read(1, &mut header)
            .map_err(|error| Error::Read { sector: 1, error })?;
        if &header[..8] != b"EFI PART" {
            return Err(Error::NoSignature);
        }
        let header_size = u32_at(&header, 12) as usize;
        if !(92..=SECTOR).contains(&header_size) {
            return Err(Error::BadHeader("header size"));
        }
        // The CRC covers the header with its own CRC field taken as zero.
        let stored = u32_at(&header, 16);
        let mut copy = header;
        copy[16..20].fill(0);
        let computed = crc32(&copy[..header_size]);
        if stored != computed {
            return Err(Error::HeaderCrc { stored, computed });
        }
        if u64_at(&header, 24) != 1 {
            return Err(Error::BadHeader("header is not at LBA 1"));
        }

        let entries_lba = u64_at(&header, 72);
        let count = u32_at(&header, 80) as usize;
        let entry_size = u32_at(&header, 84) as usize;
        if entry_size < 128 || !entry_size.is_power_of_two() {
            return Err(Error::BadHeader("entry size"));
        }
        let len = count
            .checked_mul(entry_size)
            .filter(|&len| len <= MAX_ENTRIES_BYTES)
            .ok_or(Error::BadHeader("entry array size"))?;
        let mut entries = vec![0u8; len.div_ceil(SECTOR) * SECTOR];
        dev.read(entries_lba, &mut entries)
            .map_err(|error| Error::Read {
                sector: entries_lba,
                error,
            })?;
        let stored = u32_at(&header, 88);
        let computed = crc32(&entries[..len]);
        if stored != computed {
            return Err(Error::EntriesCrc { stored, computed });
        }

        let mut partitions = Vec::new();
        for (index, e) in entries[..len].chunks_exact(entry_size).enumerate() {
            let type_guid = guid_at(e, 0);
            if type_guid.is_nil() {
                continue;
            }
            let first_lba = u64_at(e, 32);
            let last_lba = u64_at(e, 40);
            if last_lba < first_lba {
                return Err(Error::BadPartition {
                    index,
                    first: first_lba,
                    last: last_lba,
                });
            }
            let units = e[56..128]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .take_while(|&u| u != 0);
            let name = char::decode_utf16(units)
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect();
            partitions.push(Partition {
                index,
                type_guid,
                unique_guid: guid_at(e, 16),
                first_lba,
                last_lba,
                attributes: u64_at(e, 48),
                name,
            });
        }
        Ok(Gpt {
            disk_guid: guid_at(&header, 56),
            partitions,
        })
    }
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64_at(b: &[u8], off: usize) -> u64 {
    u64::from(u32_at(b, off)) | (u64::from(u32_at(b, off + 4)) << 32)
}

fn guid_at(b: &[u8], off: usize) -> Guid {
    let mut g = [0u8; 16];
    g.copy_from_slice(&b[off..off + 16]);
    Guid(g)
}

/// The CRC32 GPT uses: IEEE 802.3, reflected, as zlib computes it.
pub fn crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &byte| {
        CRC_TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8)
    })
}

/// CRC32's table: entry `n` is the remainder of byte `n`.
static CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            bit += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk in memory.
    struct Mem(Vec<u8>);

    impl BlockDevice for Mem {
        type Error = ();
        fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
            let start = sector as usize * SECTOR;
            let src = self.0.get(start..start + buf.len()).ok_or(())?;
            buf.copy_from_slice(src);
            Ok(())
        }
    }

    /// The APFS type GUID as GPT stores it.
    const APFS_BYTES: [u8; 16] = [
        0xEF, 0x57, 0x34, 0x7C, 0x00, 0x00, 0xAA, 0x11, 0xAA, 0x11, 0x00, 0x30, 0x65, 0x43, 0xEC,
        0xAC,
    ];

    /// A 64-sector disk with a table of 128 entries, the first two used.
    fn disk() -> Vec<u8> {
        let mut d = vec![0u8; 64 * SECTOR];
        let entries = 2 * SECTOR;
        let e0 = &mut d[entries..entries + 128];
        e0[..16].copy_from_slice(&APFS_BYTES);
        e0[16..32].copy_from_slice(&[0x11; 16]);
        e0[32..40].copy_from_slice(&40u64.to_le_bytes());
        e0[40..48].copy_from_slice(&47u64.to_le_bytes());
        for (i, c) in "Container".encode_utf16().enumerate() {
            e0[56 + 2 * i..58 + 2 * i].copy_from_slice(&c.to_le_bytes());
        }
        let e1 = &mut d[entries + 128..entries + 256];
        e1[..16].copy_from_slice(&[0x22; 16]);
        e1[32..40].copy_from_slice(&48u64.to_le_bytes());
        e1[40..48].copy_from_slice(&48u64.to_le_bytes());
        let entries_crc = crc32(&d[entries..entries + 128 * 128]);

        let h = &mut d[SECTOR..2 * SECTOR];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&1u64.to_le_bytes());
        h[56..72].copy_from_slice(&[0x33; 16]);
        h[72..80].copy_from_slice(&2u64.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let header_crc = crc32(&h[..92]);
        h[16..20].copy_from_slice(&header_crc.to_le_bytes());
        d
    }

    #[test]
    fn crc32_check_value() {
        // The standard check value of CRC-32/ISO-HDLC.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn guid_text() {
        assert_eq!(Guid(APFS_BYTES).to_string(), APFS_TYPE);
    }

    #[test]
    fn reads_table() {
        let gpt = Gpt::read(&mut Mem(disk())).unwrap();
        assert_eq!(gpt.disk_guid, Guid([0x33; 16]));
        assert_eq!(gpt.partitions.len(), 2);
        let p = &gpt.partitions[0];
        assert_eq!(p.index, 0);
        assert_eq!(p.type_guid.to_string(), APFS_TYPE);
        assert_eq!(p.unique_guid, Guid([0x11; 16]));
        assert_eq!((p.first_lba, p.last_lba), (40, 47));
        assert_eq!(p.name, "Container");
        assert_eq!(gpt.partitions[1].name, "");
        assert_eq!(gpt.partitions[1].index, 1);
    }

    #[test]
    fn rejects_bad_header_crc() {
        let mut d = disk();
        d[SECTOR + 40] ^= 1;
        assert!(matches!(
            Gpt::read(&mut Mem(d)),
            Err(Error::HeaderCrc { .. })
        ));
    }

    #[test]
    fn rejects_bad_entries_crc() {
        let mut d = disk();
        d[2 * SECTOR + 300] ^= 1;
        assert!(matches!(
            Gpt::read(&mut Mem(d)),
            Err(Error::EntriesCrc { .. })
        ));
    }

    #[test]
    fn rejects_missing_signature() {
        let mut d = disk();
        d[SECTOR] = b'X';
        assert!(matches!(Gpt::read(&mut Mem(d)), Err(Error::NoSignature)));
    }

    #[test]
    fn reports_short_disk() {
        let d = disk()[..2 * SECTOR].to_vec();
        assert!(matches!(
            Gpt::read(&mut Mem(d)),
            Err(Error::Read { sector: 2, .. })
        ));
    }
}
