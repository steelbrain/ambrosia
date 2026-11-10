//! A kext Ambrosia places into a kernel collection: an `MH_KEXT_BUNDLE` arm64e
//! Mach-O built against the macOS SDK's Kernel.framework alone, with chained
//! fixups and flat-namespace binds for its KPI imports. This reads its
//! segments, its `kmod_info`, and its fixups, so [`crate::auxkc`] can copy it
//! into the collection, resolve its imports against the collection's kernel
//! symbol table, and make its pointers right (docs/boot-process.md,
//! "Placing the kexts").
//!
//! The fixups are `DYLD_CHAINED_PTR_ARM64E_KERNEL` (stride 4): every pointer
//! carries its auth key, diversity and address-diversity bit inline, and a
//! `next` delta to the following fixup. Any other chained-fixup form is
//! refused.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// What can go wrong while parsing a kext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not a 64-bit little-endian Mach-O.
    BadMagic,
    /// Not an `MH_KEXT_BUNDLE`.
    NotKext,
    /// A load command or table runs past the file.
    Truncated,
    /// A load command shorter than what it holds: under 8 bytes, an
    /// `LC_SEGMENT_64` too short for its sections, or a command whose
    /// offsets placing the kext rewrites too short for them.
    ShortCommand,
    /// The Mach-O header and load commands don't lie in the file bytes of a
    /// segment at file offset 0, from which placing the kext copies them.
    HeaderOutside,
    /// A segment whose bytes run past the file, whose file size exceeds its
    /// memory size, or whose end is past the end of the address space.
    BadSegment,
    /// A chained-fixup page start Ambrosia doesn't handle (multi-start).
    MultiStart,
    /// A chained-fixup form Ambrosia doesn't handle, which the message names.
    /// It reads only what the static linker writes for a kext: 4-byte imports
    /// with plain names, `DYLD_CHAINED_PTR_ARM64E_KERNEL` pointers, and no
    /// addends.
    Unsupported(&'static str),
    /// No `_kmod_info` symbol, or no symbol table to find it in.
    NoKmodInfo,
    /// No `LC_DYLD_CHAINED_FIXUPS`, so no fixups to apply.
    NoFixups,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadMagic => f.write_str("the executable is not a 64-bit little-endian Mach-O"),
            Error::NotKext => f.write_str("the executable is not a kext, an MH_KEXT_BUNDLE"),
            Error::Truncated => {
                f.write_str("a load command, table or name in the executable runs past its end")
            }
            Error::ShortCommand => {
                f.write_str("a load command in the executable is too short for what it holds")
            }
            Error::HeaderOutside => f.write_str(
                "the executable's header and load commands are not in a segment at file offset 0",
            ),
            Error::BadSegment => f.write_str(
                "a segment's contents run past the end of the executable or are larger than \
                 the segment, or the segment ends past the top of the address space",
            ),
            Error::MultiStart => f.write_str(
                "a page of the executable's chained fixups has more than one chain start",
            ),
            Error::Unsupported(what) => write!(
                f,
                "the executable's chained fixups have {what}, which Ambrosia doesn't read"
            ),
            Error::NoKmodInfo => f.write_str("the executable has no _kmod_info symbol"),
            Error::NoFixups => f.write_str("the executable has no chained fixups"),
        }
    }
}

/// A segment to map, as the kext lays it out from its own base.
#[derive(Debug, Clone)]
pub struct Segment {
    /// The segment's name, such as `__TEXT` or `__DATA`.
    pub name: String,
    /// The virtual address the segment is mapped at, from the kext's base.
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

/// One pointer the collection's chained-fixup pass would normally make right.
/// `at` is the kext-relative offset of the 8-byte slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixup {
    /// The kext-relative offset of the 8-byte pointer slot.
    pub at: u64,
    /// What the slot points at.
    pub kind: Kind,
}

/// What a [`Fixup`]'s slot resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Points `target` bytes past the kext's base.
    Rebase {
        /// The byte offset from the kext's base.
        target: u64,
    },
    /// Points at the import of this index.
    Bind {
        /// The index into the kext's imports.
        ordinal: u32,
    },
    /// A signed pointer `target` bytes past the base.
    AuthRebase {
        /// The byte offset from the kext's base.
        target: u64,
        /// The arm64e signing key (0 IA, 1 IB, 2 DA, 3 DB).
        key: u8,
        /// Whether the signature blends in the slot's address.
        addr_div: bool,
        /// The signature's diversity.
        diversity: u16,
    },
    /// A signed pointer at the import of this index.
    AuthBind {
        /// The index into the kext's imports.
        ordinal: u32,
        /// The arm64e signing key (0 IA, 1 IB, 2 DA, 3 DB).
        key: u8,
        /// Whether the signature blends in the slot's address.
        addr_div: bool,
        /// The signature's diversity.
        diversity: u16,
    },
}

/// A parsed `MH_KEXT_BUNDLE`: its segments, fixups, imports and `kmod_info`.
#[derive(Debug, Clone)]
pub struct Kext<'a> {
    data: &'a [u8],
    /// The kext's segments, in load-command order.
    pub segments: Vec<Segment>,
    /// The kext's chained fixups, in order.
    pub fixups: Vec<Fixup>,
    /// The flat-namespace import symbol names, by ordinal.
    pub imports: Vec<String>,
    /// The kext-relative offset of `_kmod_info`.
    pub kmod_info: u64,
    /// The symbol table's `(symoff, nsyms, stroff)`, for [`Kext::exports`].
    symtab: (usize, usize, usize),
}

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_KEXT_BUNDLE: u32 = 11;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
const LC_FUNCTION_STARTS: u32 = 0x26;
const LC_DATA_IN_CODE: u32 = 0x29;
const LC_SEGMENT_SPLIT_INFO: u32 = 0x1e;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;
const LC_CODE_SIGNATURE: u32 = 0x1d;

/// The size of an `LC_SEGMENT_64` with no sections, and of each section.
const SEGMENT_SIZE: u64 = 72;
const SECTION_SIZE: u64 = 80;

/// The size of the load command `cmd`, at the least, as placing a kext reads
/// and rewrites its fields (`place::rewrite_sub_header`): `symtab_command`,
/// and `linkedit_data_command` for the tables it empties. Any command is at
/// least 8 bytes, its `cmd` and `cmdsize`.
fn least_size(cmd: u32) -> u64 {
    match cmd {
        LC_SEGMENT_64 => SEGMENT_SIZE,
        LC_SYMTAB => 24,
        LC_DYLD_CHAINED_FIXUPS
        | LC_FUNCTION_STARTS
        | LC_DATA_IN_CODE
        | LC_SEGMENT_SPLIT_INFO
        | LC_DYLD_EXPORTS_TRIE
        | LC_CODE_SIGNATURE => 16,
        _ => 8,
    }
}

/// The `N` bytes at `at`, or `Truncated` if the file ends first.
fn bytes_at<const N: usize>(d: &[u8], at: usize) -> Result<[u8; N], Error> {
    d.get(at..)
        .and_then(<[u8]>::first_chunk::<N>)
        .copied()
        .ok_or(Error::Truncated)
}
fn u16_at(d: &[u8], at: usize) -> Result<u16, Error> {
    bytes_at(d, at).map(u16::from_le_bytes)
}
fn u32_at(d: &[u8], at: usize) -> Result<u32, Error> {
    bytes_at(d, at).map(u32::from_le_bytes)
}
fn u64_at(d: &[u8], at: usize) -> Result<u64, Error> {
    bytes_at(d, at).map(u64::from_le_bytes)
}

/// The NUL-terminated string at `at`, without its NUL.
fn cstr_at(d: &[u8], at: usize) -> Result<&[u8], Error> {
    let s = d.get(at..).ok_or(Error::Truncated)?;
    let len = s.iter().position(|&b| b == 0).ok_or(Error::Truncated)?;
    Ok(&s[..len])
}

impl<'a> Kext<'a> {
    /// Parses the `MH_KEXT_BUNDLE` at the start of `d`.
    pub fn parse(d: &'a [u8]) -> Result<Kext<'a>, Error> {
        if u32_at(d, 0).map_err(|_| Error::BadMagic)? != MH_MAGIC_64 {
            return Err(Error::BadMagic);
        }
        if u32_at(d, 12)? != MH_KEXT_BUNDLE {
            return Err(Error::NotKext);
        }
        let ncmds = u32_at(d, 16)? as usize;
        let mut segments = Vec::new();
        let mut chained = None;
        let mut symtab = None;
        let mut at = 32;
        for _ in 0..ncmds {
            let cmd = u32_at(d, at)?;
            let size = u32_at(d, at + 4)? as usize;
            if (size as u64) < least_size(cmd) {
                return Err(Error::ShortCommand);
            }
            if at + size > d.len() {
                return Err(Error::Truncated);
            }
            match cmd {
                LC_SEGMENT_64 => {
                    let nsects = u64::from(u32_at(d, at + 64)?);
                    if SEGMENT_SIZE + SECTION_SIZE * nsects > size as u64 {
                        return Err(Error::ShortCommand);
                    }
                    let name = d.get(at + 8..at + 24).ok_or(Error::Truncated)?;
                    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                    let s = Segment {
                        name: String::from_utf8_lossy(&name[..end]).into_owned(),
                        vmaddr: u64_at(d, at + 24)?,
                        vmsize: u64_at(d, at + 32)?,
                        fileoff: u64_at(d, at + 40)?,
                        filesize: u64_at(d, at + 48)?,
                        initprot: u32_at(d, at + 60)?,
                    };
                    // Placing the kext copies each segment's file bytes and
                    // reserves its memory size, so both must make sense.
                    let in_file = s.filesize == 0
                        || s.fileoff
                            .checked_add(s.filesize)
                            .is_some_and(|end| end <= d.len() as u64);
                    if !in_file || s.filesize > s.vmsize || s.vmaddr.checked_add(s.vmsize).is_none()
                    {
                        return Err(Error::BadSegment);
                    }
                    segments.push(s);
                }
                LC_DYLD_CHAINED_FIXUPS => {
                    chained = Some((u32_at(d, at + 8)? as usize, u32_at(d, at + 12)? as usize));
                }
                LC_SYMTAB => {
                    symtab = Some((
                        u32_at(d, at + 8)? as usize,  // symoff
                        u32_at(d, at + 12)? as usize, // nsyms
                        u32_at(d, at + 16)? as usize, // stroff
                    ));
                }
                _ => {}
            }
            at += size;
        }
        // Placing the kext copies the segment at file offset 0, and rewrites
        // the header and load commands in that copy.
        let header_in = |s: &Segment| s.fileoff == 0 && s.filesize >= at as u64;
        if !segments.iter().any(header_in) {
            return Err(Error::HeaderOutside);
        }
        let (fixups, imports) = parse_chained(d, chained.ok_or(Error::NoFixups)?)?;
        let (symoff, nsyms, stroff) = symtab.ok_or(Error::NoKmodInfo)?;
        let kmod_info =
            find_symbol(d, symoff, nsyms, stroff, b"_kmod_info")?.ok_or(Error::NoKmodInfo)?;
        Ok(Kext {
            data: d,
            segments,
            fixups,
            imports,
            kmod_info,
            symtab: (symoff, nsyms, stroff),
        })
    }

    /// The kext's defined, externally visible symbols, as `(name, value)` with
    /// `value` the symbol's address from the kext's base. These are what a
    /// sibling kext in the same collection resolves its imports against: an
    /// `OSBundleLibraries` dependency on another placed kext — a plugin that
    /// imports a base kext's symbols, as Lilu's plugins do — binds to these, as
    /// a within-collection reference, rather than against the Boot KC. Stab
    /// (debug) entries and private externals are excluded, matching what the
    /// static linker lets another image bind.
    pub fn exports(&self) -> Vec<(String, u64)> {
        const N_STAB: u8 = 0xe0;
        const N_PEXT: u8 = 0x10;
        const N_TYPE: u8 = 0x0e;
        const N_EXT: u8 = 0x01;
        const N_SECT: u8 = 0x0e;
        let (symoff, nsyms, stroff) = self.symtab;
        let d = self.data;
        let mut out = Vec::new();
        for i in 0..nsyms {
            let e = symoff + 16 * i;
            let (Ok(strx), Some(&n_type), Ok(value)) =
                (u32_at(d, e), d.get(e + 4), u64_at(d, e + 8))
            else {
                break;
            };
            if n_type & N_STAB != 0
                || n_type & N_EXT == 0
                || n_type & N_PEXT != 0
                || n_type & N_TYPE != N_SECT
                || value == 0
            {
                continue;
            }
            let name_at = stroff + strx as usize;
            let Some(rest) = d.get(name_at..) else {
                continue;
            };
            let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
            if end != 0 {
                out.push((String::from_utf8_lossy(&rest[..end]).into_owned(), value));
            }
        }
        out
    }
}

/// Finds a symbol's value in a Mach-O symbol table (`nlist_64`, 16 bytes).
fn find_symbol(
    d: &[u8],
    symoff: usize,
    nsyms: usize,
    stroff: usize,
    want: &[u8],
) -> Result<Option<u64>, Error> {
    for i in 0..nsyms {
        let e = symoff + 16 * i;
        let strx = u32_at(d, e)? as usize;
        let value = u64_at(d, e + 8)?;
        if cstr_at(d, stroff + strx)? == want {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

const DYLD_CHAINED_IMPORT: u32 = 1;
const DYLD_CHAINED_PTR_ARM64E_KERNEL: u16 = 7;

/// Parses the chained-fixup linkedit: the fixups in order and the import
/// symbol names. Handles `DYLD_CHAINED_PTR_ARM64E_KERNEL` (stride 4) with
/// `DYLD_CHAINED_IMPORT` imports, and refuses any other form rather than
/// misread it.
fn parse_chained(
    d: &[u8],
    (off, _size): (usize, usize),
) -> Result<(Vec<Fixup>, Vec<String>), Error> {
    let starts_off = off + u32_at(d, off + 4)? as usize;
    let imports_off = off + u32_at(d, off + 8)? as usize;
    let syms_off = off + u32_at(d, off + 12)? as usize;
    let imports_count = u32_at(d, off + 16)? as usize;
    if imports_count > 0 {
        // The other import formats are 8 and 16 bytes and carry an addend,
        // which a bind's target would need added.
        if u32_at(d, off + 20)? != DYLD_CHAINED_IMPORT {
            return Err(Error::Unsupported("imports other than DYLD_CHAINED_IMPORT"));
        }
        if u32_at(d, off + 24)? != 0 {
            return Err(Error::Unsupported("compressed import names"));
        }
    }

    // The count is the file's, so it reserves no more than the file can hold.
    let mut imports = Vec::with_capacity(imports_count.min(d.len() / 4));
    for i in 0..imports_count {
        // DYLD_CHAINED_IMPORT: lib_ordinal:8, weak:1, name_offset:23.
        let v = u32_at(d, imports_off + 4 * i)?;
        let name = cstr_at(d, syms_off + (v >> 9) as usize)?;
        imports.push(String::from_utf8_lossy(name).into_owned());
    }

    let seg_count = u32_at(d, starts_off)? as usize;
    let mut fixups = Vec::new();
    for s in 0..seg_count {
        let so = u32_at(d, starts_off + 4 + 4 * s)? as usize;
        if so == 0 {
            continue;
        }
        let base = starts_off + so;
        let page_size = u64::from(u16_at(d, base + 4)?);
        if u16_at(d, base + 6)? != DYLD_CHAINED_PTR_ARM64E_KERNEL {
            return Err(Error::Unsupported(
                "pointers other than DYLD_CHAINED_PTR_ARM64E_KERNEL",
            ));
        }
        let seg_off = u64_at(d, base + 8)?;
        let page_count = u16_at(d, base + 20)? as usize;
        for p in 0..page_count {
            let start = u16_at(d, base + 22 + 2 * p)?;
            if start == 0xffff {
                continue; // DYLD_CHAINED_PTR_START_NONE
            }
            if start & 0x8000 != 0 {
                return Err(Error::MultiStart);
            }
            let mut loc = seg_off
                .checked_add(p as u64 * page_size + u64::from(start))
                .ok_or(Error::Truncated)?;
            loop {
                let raw = u64_at(d, loc as usize)?;
                let auth = raw >> 63 & 1 != 0;
                let bind = raw >> 62 & 1 != 0;
                let next = raw >> 51 & 0x7ff;
                let kind = match (auth, bind) {
                    (false, false) if raw >> 43 & 0xff != 0 => {
                        return Err(Error::Unsupported("a rebase with a high byte"));
                    }
                    (false, false) => Kind::Rebase {
                        target: raw & 0x7ff_ffff_ffff,
                    },
                    (false, true) if raw >> 32 & 0x7_ffff != 0 => {
                        return Err(Error::Unsupported("a bind with an addend"));
                    }
                    (false, true) => Kind::Bind {
                        ordinal: (raw & 0xffff) as u32,
                    },
                    (true, false) => Kind::AuthRebase {
                        target: raw & 0xffff_ffff,
                        key: (raw >> 49 & 3) as u8,
                        addr_div: raw >> 48 & 1 != 0,
                        diversity: (raw >> 32 & 0xffff) as u16,
                    },
                    (true, true) => Kind::AuthBind {
                        ordinal: (raw & 0xffff) as u32,
                        key: (raw >> 49 & 3) as u8,
                        addr_div: raw >> 48 & 1 != 0,
                        diversity: (raw >> 32 & 0xffff) as u16,
                    },
                };
                fixups.push(Fixup { at: loc, kind });
                if next == 0 {
                    break;
                }
                // The ARM64E_KERNEL stride is 4 bytes.
                loc = loc.checked_add(next * 4).ok_or(Error::Truncated)?;
            }
        }
    }
    Ok((fixups, imports))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINKEDIT: usize = 0x4000;
    /// The fixups blob's header fields, from the start of `__LINKEDIT`.
    const IMPORTS_FORMAT: usize = LINKEDIT + 20;
    const SYMBOLS_FORMAT: usize = LINKEDIT + 24;
    /// The one segment's `pointer_format`, in its `starts_in_segment`.
    const POINTER_FORMAT: usize = LINKEDIT + 0x30 + 6;
    /// The chain of four pointer slots in `__TEXT`, 8 bytes apart.
    const SLOTS: usize = 0x1000;

    const REBASE: u64 = 0x3000 | 2 << 51;
    const BIND: u64 = 1 | 2 << 51 | 1 << 62;
    const AUTH_REBASE: u64 = 0x2000 | 0x1234 << 32 | 1 << 48 | 2 << 49 | 2 << 51 | 1 << 63;
    const AUTH_BIND: u64 = 0xbeef << 32 | 3 << 62;

    /// A kext as the static linker lays one out: `__TEXT` holding a chain of a
    /// rebase, a bind, an auth-rebase and an auth-bind, and `__LINKEDIT`
    /// holding its chained fixups, two imports and a `_kmod_info` symbol.
    fn kext() -> Vec<u8> {
        fn put(d: &mut [u8], at: usize, b: &[u8]) {
            d[at..at + b.len()].copy_from_slice(b);
        }
        fn segment(d: &mut [u8], at: usize, name: &[u8], start: u64, size: u64, file: u64) {
            put(d, at, &LC_SEGMENT_64.to_le_bytes());
            put(d, at + 4, &72u32.to_le_bytes());
            put(d, at + 8, name);
            for (o, v) in [(24, start), (32, size), (40, start), (48, file)] {
                put(d, at + o, &v.to_le_bytes());
            }
            put(d, at + 60, &1u32.to_le_bytes());
        }
        let mut d = vec![0u8; LINKEDIT + 0x90];
        put(&mut d, 0, &MH_MAGIC_64.to_le_bytes());
        put(&mut d, 12, &MH_KEXT_BUNDLE.to_le_bytes());
        put(&mut d, 16, &4u32.to_le_bytes());
        segment(&mut d, 32, b"__TEXT", 0, 0x4000, 0x4000);
        segment(&mut d, 104, b"__LINKEDIT", 0x4000, 0x4000, 0x90);
        let lc = 176;
        put(&mut d, lc, &LC_DYLD_CHAINED_FIXUPS.to_le_bytes());
        put(&mut d, lc + 4, &16u32.to_le_bytes());
        put(&mut d, lc + 8, &(LINKEDIT as u32).to_le_bytes());
        put(&mut d, lc + 12, &0x69u32.to_le_bytes());
        put(&mut d, lc + 16, &LC_SYMTAB.to_le_bytes());
        put(&mut d, lc + 20, &24u32.to_le_bytes());
        for (o, v) in [
            (24, LINKEDIT + 0x70),
            (28, 1),
            (32, LINKEDIT + 0x80),
            (36, 12),
        ] {
            put(&mut d, lc + o, &(v as u32).to_le_bytes());
        }

        for (i, w) in [REBASE, BIND, AUTH_REBASE, AUTH_BIND].iter().enumerate() {
            put(&mut d, SLOTS + 8 * i, &w.to_le_bytes());
        }

        // dyld_chained_fixups_header: starts, imports and names, two imports.
        let h = LINKEDIT;
        for (o, v) in [(4, 0x20u32), (8, 0x50), (12, 0x58), (16, 2)] {
            put(&mut d, h + o, &v.to_le_bytes());
        }
        put(&mut d, IMPORTS_FORMAT, &DYLD_CHAINED_IMPORT.to_le_bytes());
        // dyld_chained_starts_in_image: two segments, only __TEXT has fixups.
        put(&mut d, h + 0x20, &2u32.to_le_bytes());
        put(&mut d, h + 0x24, &0x10u32.to_le_bytes());
        // dyld_chained_starts_in_segment: one 16 KiB page, its chain at SLOTS.
        put(&mut d, h + 0x30, &24u32.to_le_bytes());
        put(&mut d, h + 0x34, &0x4000u16.to_le_bytes());
        put(
            &mut d,
            POINTER_FORMAT,
            &DYLD_CHAINED_PTR_ARM64E_KERNEL.to_le_bytes(),
        );
        put(&mut d, h + 0x44, &1u16.to_le_bytes());
        put(&mut d, h + 0x46, &(SLOTS as u16).to_le_bytes());
        // DYLD_CHAINED_IMPORT: flat lookup (0xfe), names at 1 and 8.
        put(&mut d, h + 0x50, &(0xfeu32 | 1 << 9).to_le_bytes());
        put(&mut d, h + 0x54, &(0xfeu32 | 8 << 9).to_le_bytes());
        put(&mut d, h + 0x58, b"\0_IOLog\0_kprintf\0");
        // nlist_64: _kmod_info, external in section 1, at 0x2000.
        put(&mut d, h + 0x70, &1u32.to_le_bytes());
        put(&mut d, h + 0x74, &[0x0f, 1]);
        put(&mut d, h + 0x78, &0x2000u64.to_le_bytes());
        put(&mut d, h + 0x80, b"\0_kmod_info\0");
        d
    }

    #[test]
    fn reads_arm64e_kernel_fixups() {
        let d = kext();
        let k = Kext::parse(&d).unwrap();
        assert_eq!(k.kmod_info, 0x2000);
        assert_eq!(k.imports, ["_IOLog", "_kprintf"]);
        let at = |i: u64| SLOTS as u64 + 8 * i;
        assert_eq!(
            k.fixups,
            [
                Fixup {
                    at: at(0),
                    kind: Kind::Rebase { target: 0x3000 },
                },
                Fixup {
                    at: at(1),
                    kind: Kind::Bind { ordinal: 1 },
                },
                Fixup {
                    at: at(2),
                    kind: Kind::AuthRebase {
                        target: 0x2000,
                        key: 2,
                        addr_div: true,
                        diversity: 0x1234,
                    },
                },
                Fixup {
                    at: at(3),
                    kind: Kind::AuthBind {
                        ordinal: 0,
                        key: 0,
                        addr_div: false,
                        diversity: 0xbeef,
                    },
                },
            ]
        );
    }

    /// A header and load commands that run past the segment at file offset
    /// 0, which placing the kext copies and rewrites them in, are refused.
    #[test]
    fn refuses_a_header_past_its_segment() {
        let mut d = kext();
        // __TEXT's filesize, which its load command, at 32, gives at +48:
        // less than the 216 bytes of the header and load commands.
        d[32 + 48..32 + 56].copy_from_slice(&0xd0u64.to_le_bytes());
        assert_eq!(Kext::parse(&d).unwrap_err(), Error::HeaderOutside);
        // No segment at file offset 0 at all: __TEXT moved to 0x1000.
        let mut d = kext();
        d[32 + 40..32 + 48].copy_from_slice(&0x1000u64.to_le_bytes());
        d[32 + 48..32 + 56].copy_from_slice(&0x3000u64.to_le_bytes());
        assert_eq!(Kext::parse(&d).unwrap_err(), Error::HeaderOutside);
        // Just enough is enough.
        let mut d = kext();
        d[32 + 48..32 + 56].copy_from_slice(&0xd8u64.to_le_bytes());
        assert!(Kext::parse(&d).is_ok());
    }

    /// A load command too short for what it holds is refused: an
    /// `LC_SEGMENT_64` whose sections run past it, a symbol table command
    /// shorter than its fields, and a command under 8 bytes.
    #[test]
    fn refuses_short_commands() {
        let edit = |at: usize, v: u32| {
            let mut d = kext();
            d[at..at + 4].copy_from_slice(&v.to_le_bytes());
            Kext::parse(&d).map(|_| ())
        };
        // __TEXT's nsects, at +64 of its 72-byte command, which holds none.
        assert_eq!(edit(32 + 64, 1), Err(Error::ShortCommand));
        // LC_SYMTAB's cmdsize, 24, at 196.
        assert_eq!(edit(196, 16), Err(Error::ShortCommand));
        // LC_DYLD_CHAINED_FIXUPS's cmdsize, 16, at 180.
        assert_eq!(edit(180, 4), Err(Error::ShortCommand));
        assert_eq!(edit(32 + 64, 0), Ok(()));
    }

    /// Each form the static linker can write but Ambrosia doesn't read is
    /// refused, rather than misread as the form it does.
    #[test]
    fn refuses_other_fixup_forms() {
        let word = |at: usize, w: u64| (at, w.to_le_bytes().to_vec());
        let cases = [
            // DYLD_CHAINED_IMPORT_ADDEND and DYLD_CHAINED_IMPORT_ADDEND64.
            (IMPORTS_FORMAT, 2u32.to_le_bytes().to_vec()),
            (IMPORTS_FORMAT, 3u32.to_le_bytes().to_vec()),
            // zlib-compressed import names.
            (SYMBOLS_FORMAT, 1u32.to_le_bytes().to_vec()),
            // DYLD_CHAINED_PTR_ARM64E, whose stride is 8 bytes.
            (POINTER_FORMAT, 1u16.to_le_bytes().to_vec()),
            // A rebase with a high byte, and a bind with an addend.
            word(SLOTS, REBASE | 0x80 << 43),
            word(SLOTS + 8, BIND | 4 << 32),
        ];
        for (at, bytes) in cases {
            let mut d = kext();
            d[at..at + bytes.len()].copy_from_slice(&bytes);
            assert!(
                matches!(Kext::parse(&d), Err(Error::Unsupported(_))),
                "{at:#x} = {bytes:x?}"
            );
        }
    }
}
