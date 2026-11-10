//! Driver modules' format: an ELF64 little-endian aarch64 static PIE, an
//! `ET_DYN` file as rust-lld links one with `-pie` for
//! `aarch64-unknown-none` (docs/graphics-drivers.md, "The file").
//!
//! Ambrosia loads one anywhere in RAM. It copies the `PT_LOAD` segments to
//! their virtual addresses from a base aligned to their largest `p_align`,
//! zeroes the rest (the bss), and applies the `R_AARCH64_RELATIVE`
//! relocations of the `DT_RELA` table that `PT_DYNAMIC` names. That is all
//! such a module needs. Anything else is refused: a module that needs a
//! dynamic linker (`PT_INTERP`, `DT_NEEDED`, PLT relocations, a relocation
//! against a symbol), thread-local storage, or another relocation format or
//! type. Other program headers (`PT_PHDR`, `PT_GNU_RELRO`, `PT_GNU_STACK`,
//! `PT_GNU_EH_FRAME`) ask nothing of a loader that maps all of the image
//! readable, writable and executable, as Ambrosia's identity map does.

use alloc::vec::Vec;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_TLS: u32 = 7;
const PF_X: u32 = 1;

const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_PLTRELSZ: u64 = 2;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;
const DT_REL: u64 = 17;
const DT_JMPREL: u64 = 23;
const DT_RELR: u64 = 36;

const R_AARCH64_RELATIVE: u32 = 1027;

const EHDR: usize = 64;
const PHDR: usize = 56;
const RELA: usize = 24;

/// Why a module can't be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Not an ELF file.
    NotElf,
    /// An ELF file, but not a 64-bit little-endian aarch64 `ET_DYN`.
    NotPie,
    /// A header or table runs past the end of the file.
    Truncated,
    /// It needs a dynamic linker: `PT_INTERP`, `DT_NEEDED` or PLT
    /// relocations.
    NeedsLinker,
    /// It has thread-local storage.
    Tls,
    /// It has no `PT_LOAD` segment.
    NoSegments,
    /// The program header at this index is a `PT_LOAD` whose file size
    /// exceeds its memory size, whose bytes run past the end of the file, or
    /// whose alignment is not a power of two.
    BadSegment(usize),
    /// The entry point is not in an executable segment.
    BadEntry,
    /// Relocations other than `DT_RELA` with 24-byte entries, or a table
    /// outside the loaded segments' file bytes.
    BadRelocations,
    /// A relocation that is not `R_AARCH64_RELATIVE`, or that names a
    /// symbol.
    Relocation {
        /// The relocation's index in the table.
        index: usize,
        /// Its type, the low 32 bits of its `r_info`.
        kind: u32,
    },
    /// The relocation at this index writes outside the image.
    RelocationOutside(usize),
    /// [`Module::load`] was given memory of the wrong size or alignment.
    BadPlacement,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotElf => f.write_str("not an ELF file"),
            Error::NotPie => f.write_str("not a 64-bit little-endian aarch64 static PIE (ET_DYN)"),
            Error::Truncated => f.write_str("a header or table runs past the end of the file"),
            Error::NeedsLinker => {
                f.write_str("it needs a dynamic linker: PT_INTERP, DT_NEEDED or PLT relocations")
            }
            Error::Tls => f.write_str("it has thread-local storage"),
            Error::NoSegments => f.write_str("it has no PT_LOAD segment"),
            Error::BadSegment(i) => write!(
                f,
                "program header {i} is a PT_LOAD larger in the file than in memory, past the \
                 end of the file, or aligned to other than a power of two"
            ),
            Error::BadEntry => f.write_str("its entry point is not in an executable segment"),
            Error::BadRelocations => f.write_str(
                "its relocations are not one DT_RELA table, of 24-byte entries, in a loaded \
                 segment",
            ),
            Error::Relocation { index, kind } => write!(
                f,
                "relocation {index} is of type {kind}, or names a symbol; the firmware applies \
                 only R_AARCH64_RELATIVE"
            ),
            Error::RelocationOutside(i) => {
                write!(f, "relocation {i} writes outside the loaded image")
            }
            Error::BadPlacement => {
                f.write_str("the memory given for it has the wrong size or alignment")
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

/// A module, checked and ready to load.
#[derive(Debug)]
pub struct Module<'a> {
    elf: &'a [u8],
    segments: Vec<Segment>,
    /// The `DT_RELA` table's bytes, in the file.
    relocations: &'a [u8],
    entry: u64,
    /// The memory it takes, from its base to the end of its last segment.
    pub size: usize,
    /// The alignment its base needs.
    pub align: usize,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// `len` bytes of `b` from `at`, or `Truncated`.
fn range(b: &[u8], at: u64, len: u64) -> Result<&[u8], Error> {
    let end = at.checked_add(len).ok_or(Error::Truncated)?;
    b.get(at as usize..end as usize).ok_or(Error::Truncated)
}

impl<'a> Module<'a> {
    /// Checks `elf` and finds what loading it takes.
    pub fn parse(elf: &'a [u8]) -> Result<Module<'a>, Error> {
        let h = elf.get(..EHDR).ok_or(Error::NotElf)?;
        if &h[..4] != b"\x7fELF" {
            return Err(Error::NotElf);
        }
        // ELFCLASS64, ELFDATA2LSB, EV_CURRENT; ET_DYN, EM_AARCH64.
        if h[4..7] != [2, 1, 1] || u16_at(h, 16) != 3 || u16_at(h, 18) != 183 {
            return Err(Error::NotPie);
        }
        let entry = u64_at(h, 24);
        let (phoff, phentsize, phnum) = (u64_at(h, 32), u16_at(h, 54), u16_at(h, 56));
        if phentsize as usize != PHDR {
            return Err(Error::NotPie);
        }
        let headers = range(elf, phoff, PHDR as u64 * u64::from(phnum))?;

        let mut segments = Vec::new();
        let mut dynamic = None;
        let (mut size, mut align, mut entry_ok) = (0u64, 1u64, false);
        for (i, p) in headers.as_chunks::<PHDR>().0.iter().enumerate() {
            let (kind, flags) = (u32_at(p, 0), u32_at(p, 4));
            let s = Segment {
                offset: u64_at(p, 8),
                vaddr: u64_at(p, 16),
                filesz: u64_at(p, 32),
                memsz: u64_at(p, 40),
            };
            match kind {
                PT_LOAD => {
                    let p_align = u64_at(p, 48).max(1);
                    let end = s.vaddr.checked_add(s.memsz).ok_or(Error::BadSegment(i))?;
                    if s.filesz > s.memsz
                        || range(elf, s.offset, s.filesz).is_err()
                        || !p_align.is_power_of_two()
                    {
                        return Err(Error::BadSegment(i));
                    }
                    size = size.max(end);
                    align = align.max(p_align);
                    entry_ok |= flags & PF_X != 0 && (s.vaddr..end).contains(&entry);
                    segments.push(s);
                }
                PT_DYNAMIC => dynamic = Some(range(elf, s.offset, s.filesz)?),
                PT_INTERP => return Err(Error::NeedsLinker),
                PT_TLS => return Err(Error::Tls),
                _ => {}
            }
        }
        if segments.is_empty() {
            return Err(Error::NoSegments);
        }
        if !entry_ok {
            return Err(Error::BadEntry);
        }

        let (mut rela, mut relasz, mut relaent) = (0, 0, RELA as u64);
        for d in dynamic.unwrap_or(&[]).as_chunks::<16>().0 {
            let (tag, val) = (u64_at(d, 0), u64_at(d, 8));
            match tag {
                DT_NULL => break,
                DT_NEEDED | DT_JMPREL => return Err(Error::NeedsLinker),
                DT_PLTRELSZ if val != 0 => return Err(Error::NeedsLinker),
                DT_REL | DT_RELR => return Err(Error::BadRelocations),
                DT_RELA => rela = val,
                DT_RELASZ => relasz = val,
                DT_RELAENT => relaent = val,
                _ => {}
            }
        }
        if relaent != RELA as u64 || !relasz.is_multiple_of(RELA as u64) {
            return Err(Error::BadRelocations);
        }
        // The table is in a loaded segment's file bytes: read it from there.
        let relocations = if relasz == 0 {
            &[][..]
        } else {
            let s = segments
                .iter()
                .find(|s| rela >= s.vaddr && rela.saturating_add(relasz) <= s.vaddr + s.filesz)
                .ok_or(Error::BadRelocations)?;
            range(elf, s.offset + (rela - s.vaddr), relasz)?
        };
        for (index, r) in relocations.as_chunks::<RELA>().0.iter().enumerate() {
            let info = u64_at(r, 8);
            if info as u32 != R_AARCH64_RELATIVE || info >> 32 != 0 {
                return Err(Error::Relocation {
                    index,
                    kind: info as u32,
                });
            }
            if u64_at(r, 0).checked_add(8).is_none_or(|end| end > size) {
                return Err(Error::RelocationOutside(index));
            }
        }

        Ok(Module {
            elf,
            segments,
            relocations,
            entry,
            size: size as usize,
            align: align as usize,
        })
    }

    /// Loads the module into `image`, its [`size`](Self::size) bytes of
    /// memory at address `base`, aligned to [`align`](Self::align), and
    /// gives the entry point's address.
    pub fn load(&self, image: &mut [u8], base: u64) -> Result<u64, Error> {
        if image.len() != self.size || !base.is_multiple_of(self.align as u64) {
            return Err(Error::BadPlacement);
        }
        image.fill(0);
        for s in &self.segments {
            let from = &self.elf[s.offset as usize..(s.offset + s.filesz) as usize];
            image[s.vaddr as usize..][..from.len()].copy_from_slice(from);
        }
        for r in self.relocations.as_chunks::<RELA>().0 {
            let (at, addend) = (u64_at(r, 0) as usize, u64_at(r, 16));
            image[at..at + 8].copy_from_slice(&base.wrapping_add(addend).to_le_bytes());
        }
        Ok(base + self.entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program header: type, flags, file offset, address, file size,
    /// memory size, alignment.
    type Ph = (u32, u32, u64, u64, u64, u64, u64);

    /// An ELF file of these program headers and this data from offset
    /// 0x1000 on.
    fn elf(entry: u64, phs: &[Ph], data: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; 0x1000];
        v[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        v[16..18].copy_from_slice(&3u16.to_le_bytes());
        v[18..20].copy_from_slice(&183u16.to_le_bytes());
        v[24..32].copy_from_slice(&entry.to_le_bytes());
        v[32..40].copy_from_slice(&(EHDR as u64).to_le_bytes());
        v[54..56].copy_from_slice(&(PHDR as u16).to_le_bytes());
        v[56..58].copy_from_slice(&(phs.len() as u16).to_le_bytes());
        for (i, &(kind, flags, off, vaddr, filesz, memsz, align)) in phs.iter().enumerate() {
            let p = &mut v[EHDR + PHDR * i..][..PHDR];
            p[0..4].copy_from_slice(&kind.to_le_bytes());
            p[4..8].copy_from_slice(&flags.to_le_bytes());
            for (at, x) in [
                (8, off),
                (16, vaddr),
                (24, vaddr),
                (32, filesz),
                (40, memsz),
                (48, align),
            ] {
                p[at..at + 8].copy_from_slice(&x.to_le_bytes());
            }
        }
        v.extend_from_slice(data);
        v
    }

    fn words(ws: &[u64]) -> Vec<u8> {
        ws.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// A module as rust-lld lays one out: read-only data with the
    /// relocations at 0, code at 0x10000, data and bss at 0x20000, and the
    /// dynamic table in the data. `relocs` are (offset, info, addend).
    fn module(relocs: &[(u64, u64, u64)], dynamic: &[(u64, u64)]) -> Vec<u8> {
        let mut ro = words(
            &relocs
                .iter()
                .flat_map(|&(o, i, a)| [o, i, a])
                .collect::<Vec<_>>(),
        );
        ro.resize(0x100, 0);
        let mut text = 0xd65f03c0u32.to_le_bytes().to_vec(); // ret
        text.resize(0x100, 0);
        let mut dt: Vec<u64> = vec![DT_RELA, 0, DT_RELASZ, 24 * relocs.len() as u64];
        dt.extend([DT_RELAENT, 24]);
        dt.extend(dynamic.iter().flat_map(|&(t, v)| [t, v]));
        dt.extend([DT_NULL, 0]);
        let mut data = words(&[0x1111, 0x2222]);
        data.extend(words(&dt));
        let n = data.len() as u64;
        elf(
            0x10000,
            &[
                (PT_LOAD, 4, 0x1000, 0, 0x100, 0x100, 0x10000),
                (PT_LOAD, 5, 0x1100, 0x10000, 0x100, 0x100, 0x10000),
                (PT_LOAD, 6, 0x1200, 0x20000, n, 0x1000, 0x10000),
                (PT_DYNAMIC, 6, 0x1210, 0x20010, n - 16, n - 16, 8),
                (0x6474e551, 6, 0, 0, 0, 0, 0), // PT_GNU_STACK
            ],
            &[ro, text, data].concat(),
        )
    }

    const RELATIVE: u64 = R_AARCH64_RELATIVE as u64;

    #[test]
    fn loads_and_relocates() {
        let f = module(
            &[(0x20000, RELATIVE, 0x10000), (0x20008, RELATIVE, 0x20040)],
            &[],
        );
        let m = Module::parse(&f).unwrap();
        assert_eq!((m.size, m.align), (0x21000, 0x10000));
        let base = 0x1_4000_0000;
        let mut image = vec![0xaa; m.size];
        assert_eq!(m.load(&mut image, base), Ok(base + 0x10000));
        assert_eq!(&image[0x10000..0x10004], &0xd65f03c0u32.to_le_bytes());
        assert_eq!(u64_at(&image, 0x20000), base + 0x10000);
        assert_eq!(u64_at(&image, 0x20008), base + 0x20040);
        // Between the segments and in the bss, zeroes.
        assert!(image[0x100..0x10000].iter().all(|&b| b == 0));
        assert!(image[0x20100..].iter().all(|&b| b == 0));
        // Memory of the wrong size or alignment.
        assert_eq!(m.load(&mut image, base + 0x1000), Err(Error::BadPlacement));
        assert_eq!(m.load(&mut image[1..], base), Err(Error::BadPlacement));
    }

    #[test]
    fn refuses_other_relocations() {
        // R_AARCH64_ABS64 against symbol 1.
        let f = module(&[(0x20000, RELATIVE, 0), (0x20008, 1 << 32 | 257, 0)], &[]);
        assert_eq!(
            Module::parse(&f).unwrap_err(),
            Error::Relocation {
                index: 1,
                kind: 257
            }
        );
        let f = module(&[(0x20000, 1 << 32 | RELATIVE, 0)], &[]);
        assert_eq!(
            Module::parse(&f).unwrap_err(),
            Error::Relocation {
                index: 0,
                kind: 1027
            }
        );
        let f = module(&[(0x20ffc, RELATIVE, 0)], &[]);
        assert_eq!(Module::parse(&f).unwrap_err(), Error::RelocationOutside(0));
        for (tag, want) in [
            (DT_NEEDED, Error::NeedsLinker),
            (DT_JMPREL, Error::NeedsLinker),
            (DT_PLTRELSZ, Error::NeedsLinker),
            (DT_REL, Error::BadRelocations),
            (DT_RELR, Error::BadRelocations),
        ] {
            let f = module(&[], &[(tag, 1)]);
            assert_eq!(Module::parse(&f).unwrap_err(), want, "tag {tag}");
        }
    }

    #[test]
    fn refuses_other_files() {
        let good = module(&[], &[]);
        assert!(Module::parse(&good).is_ok());
        let edit = |at: usize, b: &[u8]| {
            let mut f = good.clone();
            f[at..at + b.len()].copy_from_slice(b);
            Module::parse(&f).unwrap_err()
        };
        assert_eq!(edit(0, b"\x7fELG"), Error::NotElf);
        assert_eq!(edit(4, &[1]), Error::NotPie); // 32-bit
        assert_eq!(edit(5, &[2]), Error::NotPie); // big-endian
        assert_eq!(edit(16, &2u16.to_le_bytes()), Error::NotPie); // ET_EXEC
        assert_eq!(edit(18, &62u16.to_le_bytes()), Error::NotPie); // x86-64
        assert_eq!(edit(24, &0u64.to_le_bytes()), Error::BadEntry); // not code
        assert_eq!(edit(EHDR, &PT_INTERP.to_le_bytes()), Error::NeedsLinker);
        assert_eq!(edit(EHDR, &PT_TLS.to_le_bytes()), Error::Tls);
        // The code segment's file size past its memory size, and its bytes
        // past the file's end.
        assert_eq!(
            edit(EHDR + PHDR + 32, &0x200u64.to_le_bytes()),
            Error::BadSegment(1)
        );
        assert_eq!(
            edit(EHDR + PHDR + 8, &0x10000u64.to_le_bytes()),
            Error::BadSegment(1)
        );
        assert_eq!(
            edit(EHDR + PHDR + 48, &3u64.to_le_bytes()),
            Error::BadSegment(1)
        );
        assert_eq!(edit(56, &100u16.to_le_bytes()), Error::Truncated);
        assert_eq!(Module::parse(&good[..40]).unwrap_err(), Error::NotElf);
    }
}
