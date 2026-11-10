//! The kernel collection's Mach-O header: a 64-bit `MH_FILESET` whose
//! segments the firmware places, and whose `LC_UNIXTHREAD` gives the
//! kernel's entry point.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// What can go wrong while parsing the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a 64-bit little-endian Mach-O.
    BadMagic,
    /// A load command that runs past the header or the file.
    Truncated,
    /// No `LC_UNIXTHREAD` with an arm64 thread state.
    NoEntry,
    /// The entry point lies in no segment.
    EntryOutsideSegments,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Error::BadMagic => "not a 64-bit little-endian Mach-O",
            Error::Truncated => {
                "a load command, symbol or name runs past the end of its header or file"
            }
            Error::NoEntry => "no entry point, an LC_UNIXTHREAD with an arm64 thread state",
            Error::EntryOutsideSegments => "its entry point is in no segment",
        })
    }
}

const MH_MAGIC_64: u32 = 0xfeed_facf;
/// `filetype` of a Mach-O that is a set of files, as a kernel collection is.
pub const MH_FILESET: u32 = 12;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_UNIXTHREAD: u32 = 0x5;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;
const ARM_THREAD_STATE64: u32 = 6;

/// An `LC_SEGMENT_64` load command: a span of the file mapped into memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The segment's name, such as `__TEXT` or `__LINKEDIT`.
    pub name: String,
    /// The virtual address the segment is mapped at.
    pub vmaddr: u64,
    /// The size of the mapping in memory.
    pub vmsize: u64,
    /// The byte offset of the segment's contents in the file.
    pub fileoff: u64,
    /// The size of the segment's contents in the file.
    pub filesize: u64,
    /// The protection the segment starts with: read 1, write 2, execute 4.
    pub initprot: u32,
}

/// An `LC_FILESET_ENTRY` load command: one member of an `MH_FILESET`, the
/// kernel or one of its kexts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesetEntry {
    /// The member's identifier, such as `com.apple.kernel`.
    pub id: String,
    /// The virtual address of the member's own Mach-O header.
    pub vmaddr: u64,
    /// The byte offset of the member's own Mach-O header in the file.
    pub fileoff: u64,
}

/// A Mach-O header and the load commands the firmware reads from it.
#[derive(Debug, Clone)]
pub struct Header {
    /// The CPU family the file is built for.
    pub cputype: u32,
    /// The CPU subtype the file is built for.
    pub cpusubtype: u32,
    /// The file's kind, `MH_FILESET` for a kernel collection.
    pub filetype: u32,
    /// The file's segments, in the order its load commands give them.
    pub segments: Vec<Segment>,
    /// The fileset's members, empty unless the file is an `MH_FILESET`.
    pub entries: Vec<FilesetEntry>,
    /// The entry point, at the file's link address, if it has one: a kernel
    /// collection and its kernel do, its kexts don't.
    pub entry: Option<u64>,
}

fn u32_at(d: &[u8], at: usize) -> Result<u32, Error> {
    let b = d
        .get(at..)
        .and_then(<[u8]>::first_chunk::<4>)
        .ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes(*b))
}

fn u64_at(d: &[u8], at: usize) -> Result<u64, Error> {
    let b = d
        .get(at..)
        .and_then(<[u8]>::first_chunk::<8>)
        .ok_or(Error::Truncated)?;
    Ok(u64::from_le_bytes(*b))
}

fn cstr(d: &[u8]) -> String {
    let end = d.iter().position(|&b| b == 0).unwrap_or(d.len());
    String::from_utf8_lossy(&d[..end]).into()
}

impl Header {
    /// Parses the header and load commands at the start of `d`.
    pub fn parse(d: &[u8]) -> Result<Header, Error> {
        if u32_at(d, 0).map_err(|_| Error::BadMagic)? != MH_MAGIC_64 {
            return Err(Error::BadMagic);
        }
        let mut h = Header {
            cputype: u32_at(d, 4)?,
            cpusubtype: u32_at(d, 8)?,
            filetype: u32_at(d, 12)?,
            segments: Vec::new(),
            entries: Vec::new(),
            entry: None,
        };
        let ncmds = u32_at(d, 16)? as usize;
        let sizeofcmds = u32_at(d, 20)? as usize;
        let end = 32usize.checked_add(sizeofcmds).ok_or(Error::Truncated)?;
        if end > d.len() {
            return Err(Error::Truncated);
        }
        let mut at = 32;
        for _ in 0..ncmds {
            let cmd = u32_at(d, at)?;
            let size = u32_at(d, at + 4)? as usize;
            if size < 8 || at + size > end {
                return Err(Error::Truncated);
            }
            let c = &d[at..at + size];
            match cmd {
                LC_SEGMENT_64 => h.segments.push(Segment {
                    name: cstr(c.get(8..24).ok_or(Error::Truncated)?),
                    vmaddr: u64_at(c, 24)?,
                    vmsize: u64_at(c, 32)?,
                    fileoff: u64_at(c, 40)?,
                    filesize: u64_at(c, 48)?,
                    initprot: u32_at(c, 60)?,
                }),
                LC_FILESET_ENTRY => {
                    let name = u32_at(c, 24)? as usize;
                    h.entries.push(FilesetEntry {
                        id: cstr(c.get(name..).ok_or(Error::Truncated)?),
                        vmaddr: u64_at(c, 8)?,
                        fileoff: u64_at(c, 16)?,
                    });
                }
                // arm_thread_state64: x0 to x28, fp, lr, sp, pc, cpsr.
                LC_UNIXTHREAD if u32_at(c, 8)? == ARM_THREAD_STATE64 => {
                    h.entry = Some(u64_at(c, 16 + 32 * 8)?);
                }
                _ => {}
            }
            at += size;
        }
        // The entry point, if there is one, must land in a segment: the
        // firmware turns it into a physical address relative to the lowest
        // segment, and runs the kernel there.
        if let Some(e) = h.entry {
            let in_segment = h
                .segments
                .iter()
                .any(|s| s.vmaddr <= e && e - s.vmaddr < s.vmsize);
            if !in_segment {
                return Err(Error::EntryOutsideSegments);
            }
        }
        Ok(h)
    }

    /// The lowest address any segment starts at, and the highest it ends at.
    pub fn vm_range(&self) -> (u64, u64) {
        let lo = self.segments.iter().map(|s| s.vmaddr).min().unwrap_or(0);
        let hi = self
            .segments
            .iter()
            .map(|s| s.vmaddr.saturating_add(s.vmsize))
            .max()
            .unwrap_or(0);
        (lo, hi)
    }

    /// The segment with the given name, if the file has one.
    pub fn segment(&self, name: &str) -> Option<&Segment> {
        self.segments.iter().find(|s| s.name == name)
    }

    /// The external symbols of the whole collection: the union of every
    /// fileset entry's symbols. A real kext binds against the kernel and its
    /// kexts both — `IOCatalogue::addDrivers` lives in `com.apple.kernel`, but
    /// a graphics kext imports from `IOPCIFamily` and `IOGraphicsFamily`, each
    /// a separate member — so a placed kext resolves against this union, as the
    /// kernel's kext linker binds flat-namespace imports against the whole
    /// collection. On a name defined in more than one member, `com.apple.kernel`
    /// wins (it is parsed last and overwrites); among the other members, the
    /// last in fileset order wins. `d` is the whole collection.
    pub fn all_symbols(&self, d: &[u8]) -> Result<BTreeMap<String, u64>, Error> {
        let mut syms = BTreeMap::new();
        // Others first, then the kernel, so the kernel's definition overwrites
        // any duplicate and wins deterministically.
        for e in self.entries.iter().filter(|e| e.id != "com.apple.kernel") {
            entry_symbols(d, e.fileoff as usize, &mut syms)?;
        }
        for e in self.entries.iter().filter(|e| e.id == "com.apple.kernel") {
            entry_symbols(d, e.fileoff as usize, &mut syms)?;
        }
        Ok(syms)
    }
}

/// Reads one fileset entry's external symbols into `syms`, as the linked
/// (unslid) address of each. `d` is the whole collection; `fo` is the entry's
/// header offset in it. A later entry's definition of a name overwrites an
/// earlier one.
fn entry_symbols(d: &[u8], fo: usize, syms: &mut BTreeMap<String, u64>) -> Result<(), Error> {
    let ncmds = u32_at(d, fo.saturating_add(16))? as usize;
    let mut at = fo.saturating_add(32);
    for _ in 0..ncmds {
        let cmd = u32_at(d, at)?;
        let size = u32_at(d, at + 4)? as usize;
        if size < 8 {
            return Err(Error::Truncated);
        }
        if cmd == LC_SYMTAB {
            let symoff = u32_at(d, at + 8)? as usize;
            let nsyms = u32_at(d, at + 12)? as usize;
            let stroff = u32_at(d, at + 16)? as usize;
            for i in 0..nsyms {
                let e = symoff + 16 * i;
                let strx = stroff + u32_at(d, e)? as usize;
                let n_type = *d.get(e + 4).ok_or(Error::Truncated)?;
                let value = u64_at(d, e + 8)?;
                if n_type & 0xe0 != 0 || value == 0 {
                    continue; // a stab, or no address
                }
                let name = d.get(strx..).ok_or(Error::Truncated)?;
                let len = name.iter().position(|&b| b == 0).ok_or(Error::Truncated)?;
                if let Ok(name) = core::str::from_utf8(&name[..len]) {
                    syms.insert(name.into(), value);
                }
            }
        }
        at += size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(cmds: &[Vec<u8>]) -> Vec<u8> {
        let size: usize = cmds.iter().map(Vec::len).sum();
        let mut d = Vec::new();
        for v in [
            MH_MAGIC_64,
            0x0100_000c,
            2,
            MH_FILESET,
            cmds.len() as u32,
            size as u32,
            0,
            0,
        ] {
            d.extend(v.to_le_bytes());
        }
        for c in cmds {
            d.extend(c);
        }
        d
    }

    fn segment(name: &str, vmaddr: u64, vmsize: u64) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend(LC_SEGMENT_64.to_le_bytes());
        c.extend(72u32.to_le_bytes());
        let mut n = [0u8; 16];
        n[..name.len()].copy_from_slice(name.as_bytes());
        c.extend(n);
        for v in [vmaddr, vmsize, vmaddr & 0xffff, vmsize] {
            c.extend(v.to_le_bytes());
        }
        c.extend([0u8; 16]);
        c
    }

    fn thread(pc: u64) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend(LC_UNIXTHREAD.to_le_bytes());
        c.extend((16 + 34 * 8u32).to_le_bytes());
        c.extend(ARM_THREAD_STATE64.to_le_bytes());
        c.extend(68u32.to_le_bytes());
        for i in 0..34u64 {
            c.extend(if i == 32 { pc } else { 0 }.to_le_bytes());
        }
        c
    }

    #[test]
    fn segments_and_entry() {
        let d = header(&[
            thread(0xffff_0000_0000_1480),
            segment("__TEXT", 0xffff_0000_0000_0000, 0x4000),
            segment("__LINKEDIT", 0xffff_0000_0001_0000, 0x8000),
        ]);
        let h = Header::parse(&d).unwrap();
        assert_eq!(h.filetype, MH_FILESET);
        assert_eq!(h.entry, Some(0xffff_0000_0000_1480));
        assert_eq!(h.vm_range(), (0xffff_0000_0000_0000, 0xffff_0000_0001_8000));
        assert_eq!(h.segment("__LINKEDIT").unwrap().vmsize, 0x8000);
    }

    /// An `LC_FILESET_ENTRY` command naming a member at `fileoff` in the file.
    fn fileset_entry(id: &str, vmaddr: u64, fileoff: u64) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend(LC_FILESET_ENTRY.to_le_bytes());
        c.extend(0u32.to_le_bytes()); // cmdsize, filled below
        c.extend(vmaddr.to_le_bytes());
        c.extend(fileoff.to_le_bytes());
        c.extend(32u32.to_le_bytes()); // the id string's offset in the command
        c.extend(0u32.to_le_bytes()); // reserved
        c.extend(id.as_bytes());
        c.push(0);
        while c.len() % 8 != 0 {
            c.push(0);
        }
        let size = c.len() as u32;
        c[4..8].copy_from_slice(&size.to_le_bytes());
        c
    }

    /// A member sub-header carrying one `LC_SYMTAB`, with the table's offsets
    /// taken as absolute positions in the whole collection.
    fn submacho(symoff: u32, nsyms: u32, stroff: u32, strsize: u32) -> Vec<u8> {
        let mut d = Vec::new();
        for v in [MH_MAGIC_64, 0x0100_000c, 2, 11, 1, 24, 0, 0] {
            d.extend(v.to_le_bytes());
        }
        d.extend(LC_SYMTAB.to_le_bytes());
        d.extend(24u32.to_le_bytes());
        d.extend(symoff.to_le_bytes());
        d.extend(nsyms.to_le_bytes());
        d.extend(stroff.to_le_bytes());
        d.extend(strsize.to_le_bytes());
        d
    }

    /// An `nlist_64` for a defined external symbol (`N_SECT | N_EXT`).
    fn nlist(strx: u32, value: u64) -> Vec<u8> {
        let mut e = Vec::new();
        e.extend(strx.to_le_bytes());
        e.push(0x0f); // n_type: N_SECT | N_EXT
        e.push(1); // n_sect
        e.extend(0u16.to_le_bytes()); // n_desc
        e.extend(value.to_le_bytes());
        e
    }

    #[test]
    fn all_symbols_unions_entries_kernel_wins() {
        // Two members below the kernel's symbols sit after the load commands,
        // then the two symbol tables and a shared string table. The kernel is
        // listed first in the fileset, to prove it wins a duplicate regardless
        // of order.
        let (sub_a, sub_b) = (144u64, 200u64);
        let (sym_a, sym_b, strtab) = (256u32, 288u32, 320u32);
        let mut d = header(&[
            fileset_entry("com.apple.kernel", 0, sub_a),
            fileset_entry("com.apple.kpi.iokit", 0, sub_b),
        ]);
        assert_eq!(d.len() as u64, sub_a);
        d.extend(submacho(sym_a, 2, strtab, 16));
        assert_eq!(d.len() as u64, sub_b);
        d.extend(submacho(sym_b, 2, strtab, 16));
        assert_eq!(d.len() as u32, sym_a);
        d.extend(nlist(1, 0x1000)); // _foo
        d.extend(nlist(6, 0xaaaa)); // _dup, the kernel's definition
        assert_eq!(d.len() as u32, sym_b);
        d.extend(nlist(11, 0x2000)); // _bar
        d.extend(nlist(6, 0xbbbb)); // _dup, the member's definition
        assert_eq!(d.len() as u32, strtab);
        d.extend(b"\0_foo\0_dup\0_bar\0");

        let h = Header::parse(&d).unwrap();
        // The union sees both entries' symbols, and the kernel's _dup wins
        // the duplicate.
        let all = h.all_symbols(&d).unwrap();
        assert_eq!(all.get("_foo"), Some(&0x1000));
        assert_eq!(all.get("_bar"), Some(&0x2000));
        assert_eq!(all.get("_dup"), Some(&0xaaaa));
    }

    /// A symbol whose name lies past the file is an error, not a panic.
    #[test]
    fn symbol_name_past_the_file() {
        let mut d = header(&[fileset_entry("com.apple.kernel", 0, 88)]);
        assert_eq!(d.len(), 88);
        d.extend(submacho(144, 1, 160, 1));
        d.extend(nlist(0x1_0000, 0x1000));
        d.push(0);
        let h = Header::parse(&d).unwrap();
        assert_eq!(h.all_symbols(&d).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn errors() {
        assert_eq!(Header::parse(&[0; 32]).unwrap_err(), Error::BadMagic);
        let d = header(&[segment("__TEXT", 0, 0x4000)]);
        assert_eq!(Header::parse(&d).unwrap().entry, None);
        assert_eq!(Header::parse(&d[..60]).unwrap_err(), Error::Truncated);
    }
}
