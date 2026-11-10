//! The configuration bundle: Ambrosia's `config.plist`, the kexts it names
//! and the driver modules, as one cpio archive in the "newc" format, the
//! one `cpio -o -H newc` writes. QEMU's generic loader puts it in RAM
//! before the firmware starts (docs/configuration.md, "The bundle").
//!
//! Each member is a 110-byte ASCII header, its NUL-terminated path padded to
//! 4 bytes, and its data padded to 4 bytes. The archive ends with a member
//! named `TRAILER!!!`. Only regular files are kept. Directories, links and
//! the rest are skipped, since a bundle is files under paths.

use alloc::vec::Vec;

/// The file every bundle starts with. Its being first tells a bundle from
/// whatever else RAM holds.
pub const CONFIG: &str = "config.plist";

/// The most bytes a bundle may take: the 512 MiB from 0x120000000, where
/// QEMU's generic loader puts it, up to the firmware's heap.
pub const MAX_LEN: usize = 512 << 20;

const HEADER: usize = 110;
const MAGIC: &[u8; 6] = b"070701";
const TRAILER: &str = "TRAILER!!!";
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

/// One file of the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File<'a> {
    /// Its path in the archive, without a leading `./`.
    pub path: &'a str,
    /// Its contents.
    pub data: &'a [u8],
}

/// A parsed bundle, borrowing the archive's bytes.
#[derive(Debug)]
pub struct Bundle<'a> {
    /// The regular files, in the archive's order. The first is
    /// `config.plist`.
    pub files: Vec<File<'a>>,
    /// The archive's length, through its trailer.
    pub len: usize,
}

/// Why a bundle can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The member at this offset has no newc header.
    BadHeader(usize),
    /// The member at this offset runs past the end of the memory given.
    Truncated(usize),
    /// The path of the member at this offset is not UTF-8.
    BadPath(usize),
    /// The first file is not `config.plist`.
    NoConfig,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::BadHeader(at) => {
                write!(f, "the archive member at byte {at} has no newc header")
            }
            Error::Truncated(at) => write!(
                f,
                "the archive member at byte {at} runs past the end of the bundle's space"
            ),
            Error::BadPath(at) => write!(
                f,
                "the archive member at byte {at} has a path that is not UTF-8"
            ),
            Error::NoConfig => write!(f, "the archive's first file is not {CONFIG}"),
        }
    }
}

/// A header field's value: 8 hexadecimal digits.
fn hex(field: &[u8]) -> Option<u32> {
    field
        .iter()
        .try_fold(0, |n, &c| Some(n << 4 | char::from(c).to_digit(16)?))
}

/// `n`, rounded up to the archive's 4-byte alignment.
fn pad4(n: usize) -> usize {
    n.next_multiple_of(4)
}

impl<'a> Bundle<'a> {
    /// The bundle at the start of `mem`, or `None` if `mem` doesn't start
    /// with a cpio member, which means no bundle was given.
    pub fn find(mem: &'a [u8]) -> Option<Result<Bundle<'a>, Error>> {
        mem.starts_with(MAGIC).then(|| Bundle::parse(mem))
    }

    /// Parses the archive at the start of `mem`.
    pub fn parse(mem: &'a [u8]) -> Result<Bundle<'a>, Error> {
        let mut files = Vec::new();
        let mut at = 0;
        loop {
            let h = mem.get(at..at + HEADER).ok_or(Error::Truncated(at))?;
            if &h[..6] != MAGIC {
                return Err(Error::BadHeader(at));
            }
            // The 13 fields after the magic, 8 hex digits each: inode, mode,
            // uid, gid, links, mtime, size, four device numbers, the path's
            // length with its NUL, and a checksum.
            let field = |i: usize| hex(&h[6 + 8 * i..14 + 8 * i]).ok_or(Error::BadHeader(at));
            let mode = field(1)?;
            let size = field(6)? as usize;
            let name_len = field(11)? as usize;
            if name_len == 0 {
                return Err(Error::BadHeader(at));
            }
            // The header is within `mem` and the fields are 32-bit, so the
            // sums below can't overflow a 64-bit usize.
            let name = mem
                .get(at + HEADER..at + HEADER + name_len - 1)
                .ok_or(Error::Truncated(at))?;
            let path = core::str::from_utf8(name).map_err(|_| Error::BadPath(at))?;
            let data_at = pad4(at + HEADER + name_len);
            let data = mem
                .get(data_at..data_at + size)
                .ok_or(Error::Truncated(at))?;
            let next = pad4(data_at + size);
            if path == TRAILER {
                if files.first().map(|f: &File<'_>| f.path) != Some(CONFIG) {
                    return Err(Error::NoConfig);
                }
                return Ok(Bundle { files, len: next });
            }
            if mode & S_IFMT == S_IFREG {
                let path = path.strip_prefix("./").unwrap_or(path);
                files.push(File { path, data });
            }
            at = next;
        }
    }

    /// The file at `path`, if the bundle has it.
    pub fn file(&self, path: &str) -> Option<&'a [u8]> {
        self.files.iter().find(|f| f.path == path).map(|f| f.data)
    }

    /// The configuration's bytes: the first file's, which
    /// [`Bundle::parse`] makes sure is `config.plist`.
    pub fn config(&self) -> &'a [u8] {
        self.files[0].data
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::format;

    /// A newc archive of these members: (path, mode, data).
    pub(crate) fn archive(members: &[(&str, u32, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut add = |path: &str, mode: u32, data: &[u8]| {
            out.extend(format!(
                "070701{:08x}{mode:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
                1, 0, 0, 1, 0, data.len(), 0, 0, 0, 0, path.len() + 1, 0
            ).bytes());
            out.extend(path.bytes());
            out.push(0);
            while out.len() % 4 != 0 {
                out.push(0);
            }
            out.extend(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        };
        for (p, m, d) in members {
            add(p, *m, d);
        }
        add(TRAILER, 0, b"");
        out
    }

    #[test]
    fn reads_files() {
        let a = archive(&[
            ("./config.plist", 0o100644, b"<dict/>"),
            ("./Kexts", 0o040755, b""),
            ("./Kexts/T.kext/Contents/Info.plist", 0o100644, b"abc"),
            (
                "./Kexts/T.kext/Contents/MacOS/T",
                0o100755,
                b"\x01\x02\x03\x04\x05",
            ),
        ]);
        let mut mem = a.clone();
        mem.extend([0xaa; 64]);
        let b = Bundle::find(&mem).unwrap().unwrap();
        assert_eq!(b.len, a.len());
        assert_eq!(b.config(), b"<dict/>");
        assert_eq!(b.files.len(), 3);
        assert_eq!(
            b.file("Kexts/T.kext/Contents/MacOS/T"),
            Some(&[1, 2, 3, 4, 5][..])
        );
        assert_eq!(b.file("Kexts"), None);
    }

    #[test]
    fn errors() {
        assert!(Bundle::find(&[0; 256]).is_none());
        let a = archive(&[("x", 0o100644, b"1"), ("config.plist", 0o100644, b"")]);
        assert_eq!(Bundle::parse(&a).unwrap_err(), Error::NoConfig);
        let a = archive(&[("config.plist", 0o100644, b"0123456789")]);
        assert_eq!(Bundle::parse(&a[..120]).unwrap_err(), Error::Truncated(0));
        // Fields of hexadecimal digits only: no other letter, and no sign.
        for c in *b"g+" {
            let mut a = archive(&[("config.plist", 0o100644, b"")]);
            a[14] = c;
            assert_eq!(Bundle::parse(&a).unwrap_err(), Error::BadHeader(0));
        }
    }
}
