//! Writes a cpio archive in the "newc" format, the one the firmware reads
//! (`ambrosia::bundle`), so that the same entries always give the same
//! bytes.
//!
//! Each member is a 110-byte ASCII header of a magic and 13 fields of 8
//! hexadecimal digits, its NUL-terminated path padded to 4 bytes, and its data
//! padded to 4 bytes. The archive ends with a member named `TRAILER!!!`.
//! Nothing about the machine that wrote it goes in: every member has mtime 0,
//! uid and gid 0, device numbers 0, and an inode number that is its place in
//! the archive.

/// The path of the member that ends an archive.
const TRAILER: &str = "TRAILER!!!";

/// A directory's mode: `S_IFDIR`, `rwxr-xr-x`.
const DIR: u32 = 0o040_755;
/// A file's mode: `S_IFREG`, `rw-r--r--`.
const FILE: u32 = 0o100_644;
/// An executable file's mode: `S_IFREG`, `rwxr-xr-x`.
const EXECUTABLE: u32 = 0o100_755;

/// One member of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its path in the archive: no leading `/` or `./`, `/` between parts.
    pub path: String,
    /// What it is.
    pub kind: Kind,
}

/// What an [`Entry`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A directory, mode 0755.
    Dir,
    /// A regular file: mode 0755 if `executable`, else 0644.
    File {
        /// Whether its mode lets everyone execute it.
        executable: bool,
        /// Its contents.
        data: Vec<u8>,
    },
}

impl Entry {
    /// A directory at `path`.
    pub fn dir(path: impl Into<String>) -> Entry {
        Entry {
            path: path.into(),
            kind: Kind::Dir,
        }
    }

    /// A file at `path` holding `data`.
    pub fn file(path: impl Into<String>, executable: bool, data: Vec<u8>) -> Entry {
        Entry {
            path: path.into(),
            kind: Kind::File { executable, data },
        }
    }
}

/// The archive of `entries`, in their order, and its trailer.
///
/// Each file must be smaller than 4 GiB, the most a header's size field
/// holds. The bundle's size limit, which [`crate::check`] applies, is far
/// smaller.
pub fn write(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let ino = u32::try_from(i + 1).expect("fewer than 2^32 entries");
        match &e.kind {
            Kind::Dir => member(&mut out, ino, DIR, 2, &e.path, &[]),
            Kind::File { executable, data } => {
                let mode = if *executable { EXECUTABLE } else { FILE };
                member(&mut out, ino, mode, 1, &e.path, data);
            }
        }
    }
    member(&mut out, 0, 0, 1, TRAILER, &[]);
    out
}

/// Appends one member to `out`.
fn member(out: &mut Vec<u8>, ino: u32, mode: u32, nlink: u32, path: &str, data: &[u8]) {
    let size = u32::try_from(data.len()).expect("a file under 4 GiB");
    let name_size = u32::try_from(path.len() + 1).expect("a path under 4 GiB");
    // ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor,
    // rdevmajor, rdevminor, namesize, check.
    let fields = [ino, mode, 0, 0, nlink, 0, size, 0, 0, 0, 0, name_size, 0];
    out.extend_from_slice(b"070701");
    for f in fields {
        out.extend_from_slice(format!("{f:08x}").as_bytes());
    }
    out.extend_from_slice(path.as_bytes());
    out.push(0);
    pad4(out);
    out.extend_from_slice(data);
    pad4(out);
}

/// Pads `out` with NULs to a multiple of 4 bytes.
fn pad4(out: &mut Vec<u8>) {
    out.resize(out.len().next_multiple_of(4), 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ambrosia::bundle::Bundle;

    #[test]
    fn writes_what_the_firmware_reads() {
        let a = write(&[
            Entry::file("config.plist", false, b"<dict/>".to_vec()),
            Entry::dir("Drivers"),
            Entry::file("Drivers/d.elf", true, vec![1, 2, 3, 4, 5]),
        ]);
        assert_eq!(a.len() % 4, 0);
        let b = Bundle::parse(&a).unwrap();
        assert_eq!(b.len, a.len());
        let files: Vec<_> = b.files.iter().map(|f| (f.path, f.data)).collect();
        assert_eq!(
            files,
            [
                ("config.plist", &b"<dict/>"[..]),
                ("Drivers/d.elf", &[1, 2, 3, 4, 5][..])
            ]
        );
    }

    #[test]
    fn header_fields() {
        let a = write(&[Entry::file("config.plist", true, b"abc".to_vec())]);
        let h = std::str::from_utf8(&a[..110]).unwrap();
        let field = |i: usize| &h[6 + 8 * i..14 + 8 * i];
        assert_eq!(&h[..6], "070701");
        assert_eq!(field(0), "00000001"); // ino
        assert_eq!(field(1), "000081ed"); // mode 0100755
        assert_eq!(field(2), "00000000"); // uid
        assert_eq!(field(3), "00000000"); // gid
        assert_eq!(field(5), "00000000"); // mtime
        assert_eq!(field(6), "00000003"); // filesize
        assert_eq!(field(11), "0000000d"); // namesize, with the NUL
        // 110 + 13 is 123, padded to 124; the data, padded to 128.
        assert_eq!(&a[110..124], b"config.plist\0\0");
        assert_eq!(&a[124..128], b"abc\0");
        let trailer = std::str::from_utf8(&a[128..238]).unwrap();
        assert_eq!(&trailer[14..22], "00000000"); // mode 0
        assert_eq!(&a[238..249], b"TRAILER!!!\0");
        assert_eq!(a.len(), 252);
    }

    #[test]
    fn modes() {
        let a = write(&[
            Entry::file("config.plist", false, Vec::new()),
            Entry::dir("Kexts"),
        ]);
        let mode = |at: usize| {
            std::str::from_utf8(&a[at + 14..at + 22])
                .unwrap()
                .to_owned()
        };
        assert_eq!(mode(0), "000081a4"); // 0100644
        // config.plist's member: 110 + 13, padded to 124, and no data.
        assert_eq!(mode(124), "000041ed"); // 040755
    }
}
