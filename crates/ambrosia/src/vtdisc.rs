//! Exporting a donor class's vtable PAC discriminators for injected code.
//!
//! A framework that synthesises a subclass of a kernel class in the Auxiliary
//! KC — as a graphics kext does for an `IOMobileFramebuffer` consumer — must
//! re-sign each inherited C++ vtable slot for the subclass object's slot
//! address. On arm64e a vtable method pointer is signed with key `IA`, address
//! diversity on (`addrDiv = 1`), and a 16-bit **type** discriminator blended
//! in, derived from the method's function-type mangling rather than its
//! symbol. The running vtable holds only the signed pointer, from which the
//! discriminator can't be recovered; the 16-bit value survives only in the
//! Boot KC's `DYLD_CHAINED_PTR_64_KERNEL_CACHE` word for that slot, which the
//! kernel consumes while it applies fixups. So Ambrosia reads it at placement,
//! from the Boot KC on disk, and hands it over under
//! `/chosen/ambrosia/vtable-disc`.
//!
//! Ambrosia names no class: the configuration lists the donor vtable symbols
//! (`__ZTV…`) under `Kernel` > `ExportVtableDiscriminators`, and the core
//! reads whatever it's given, knowing nothing of what they are.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::macho::{self, Header};

/// A method slot's authenticated-pointer parameters, read inline from the
/// donor vtable's `DYLD_CHAINED_PTR_64_KERNEL_CACHE` word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// The vtable word index: 2 for the first method, after the Itanium
    /// vtable's offset-to-top (0) and typeinfo (1) words.
    pub slot: u16,
    /// The 16-bit type discriminator blended into the pointer's signature
    /// (word bits 32..47).
    pub div: u16,
    /// The signing key, word bits 49..50: 0 = IA, 1 = IB, 2 = DA, 3 = DB.
    pub key: u8,
    /// Whether the slot's storage address is blended into the modifier, word
    /// bit 48.
    pub addr_div: u8,
}

/// One donor vtable's per-slot discriminators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    /// The vtable symbol, e.g. `__ZTV25IOMobileFramebufferLegacy`.
    pub symbol: String,
    /// The slid virtual address of the vtable's word 0.
    pub base: u64,
    /// The first method slot's word index, always 2.
    pub first_slot: u16,
    /// One record per method slot, from `first_slot` up, in order.
    pub slots: Vec<Slot>,
}

/// Why a requested export can't be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The Boot KC's Mach-O header didn't parse.
    Kernel(macho::Error),
    /// A configured symbol isn't in the Boot KC's symbol table.
    NoSymbol(String),
    /// A symbol's address isn't in any Boot KC segment's file data.
    Unmapped(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Kernel(e) => write!(f, "the kernel collection can't be read: {e}"),
            Error::NoSymbol(s) => write!(
                f,
                "Kernel.ExportVtableDiscriminators names {s}, which the kernel collection \
                 doesn't define"
            ),
            Error::Unmapped(s) => write!(
                f,
                "Kernel.ExportVtableDiscriminators names {s}, which is not in the file data of \
                 a segment of the kernel collection"
            ),
        }
    }
}

/// The most of a vtable Ambrosia reads when no later symbol bounds it, as a
/// byte span: far above any real class's vtable.
const CAP: u64 = 0x1_0000;

/// Reads the discriminator table for each `wanted` symbol from the Boot KC
/// `kc`, whose dense symbol map is `symbols` (as [`Header::all_symbols`] gives
/// it) and whose load slide is `slide`.
///
/// A vtable runs from its symbol's address to the next symbol above it — the
/// map is dense, so that bound is tight — less its trailing zero words. Word 0
/// is offset-to-top and word 1 the typeinfo pointer, so the method slots are
/// the rest.
pub fn export(
    kc: &[u8],
    symbols: &BTreeMap<String, u64>,
    slide: u64,
    wanted: &[String],
) -> Result<Vec<Table>, Error> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let h = Header::parse(kc).map_err(Error::Kernel)?;
    let mut tables = Vec::with_capacity(wanted.len());
    for symbol in wanted {
        let &va = symbols
            .get(symbol)
            .ok_or_else(|| Error::NoSymbol(symbol.clone()))?;
        let seg = h
            .segments
            .iter()
            .find(|s| s.vmaddr <= va && va - s.vmaddr < s.filesize)
            .ok_or_else(|| Error::Unmapped(symbol.clone()))?;
        // The vtable's byte span: to the next symbol above it, but never past
        // the segment's file data or the read cap.
        let cap = va.saturating_add(CAP);
        let end = symbols
            .values()
            .copied()
            .filter(|&v| v > va)
            .min()
            .unwrap_or(cap)
            .min(cap)
            .min(seg.vmaddr.saturating_add(seg.filesize));
        let base_off = seg
            .fileoff
            .checked_add(va - seg.vmaddr)
            .ok_or_else(|| Error::Unmapped(symbol.clone()))? as usize;
        let nwords = ((end - va) / 8) as usize;
        let mut words: Vec<u64> = (0..nwords)
            .map(|i| u64le(kc, base_off.saturating_add(i * 8)))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| Error::Unmapped(symbol.clone()))?;
        while words.last() == Some(&0) {
            words.pop();
        }
        let slots = words
            .iter()
            .enumerate()
            .skip(2)
            .map(|(i, &w)| Slot {
                slot: i as u16,
                div: (w >> 32) as u16,
                key: ((w >> 49) & 3) as u8,
                addr_div: ((w >> 48) & 1) as u8,
            })
            .collect();
        tables.push(Table {
            symbol: symbol.clone(),
            base: va.wrapping_add(slide),
            first_slot: 2,
            slots,
        });
    }
    Ok(tables)
}

/// A table's packed `slots` blob for `/chosen/ambrosia/vtable-disc`: 8 bytes
/// per method slot, `{ u16 slot, u16 div, u8 key, u8 addr_div, u8 pad[2] }`,
/// all little-endian, in slot order.
pub fn blob(t: &Table) -> Vec<u8> {
    let mut b = Vec::with_capacity(t.slots.len() * 8);
    for s in &t.slots {
        b.extend_from_slice(&s.slot.to_le_bytes());
        b.extend_from_slice(&s.div.to_le_bytes());
        b.push(s.key);
        b.push(s.addr_div);
        b.extend_from_slice(&[0, 0]);
    }
    b
}

/// The little-endian `u64` at `o` in `d`, or `None` past its end.
fn u64le(d: &[u8], o: usize) -> Option<u64> {
    d.get(o..)?.first_chunk().copied().map(u64::from_le_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec;

    // Mach-O constants, kept private in macho.rs, mirrored here for the fixture.
    const MH_MAGIC_64: u32 = 0xfeed_facf;
    const MH_FILESET: u32 = 0xc;
    const LC_SEGMENT_64: u32 = 0x19;
    const LC_SYMTAB: u32 = 0x2;
    const LC_FILESET_ENTRY: u32 = 0x8000_0035;

    fn seg(name: &str, vmaddr: u64, fileoff: u64, size: u64) -> Vec<u8> {
        let mut c = vec![];
        c.extend(LC_SEGMENT_64.to_le_bytes());
        c.extend(72u32.to_le_bytes());
        let mut n = [0u8; 16];
        n[..name.len()].copy_from_slice(name.as_bytes());
        c.extend(n);
        for v in [vmaddr, size, fileoff, size] {
            c.extend(v.to_le_bytes());
        }
        c.extend([0u8; 16]);
        c
    }

    fn fileset_entry(id: &str, fileoff: u64) -> Vec<u8> {
        let mut c = vec![];
        c.extend(LC_FILESET_ENTRY.to_le_bytes());
        c.extend(0u32.to_le_bytes());
        c.extend(0u64.to_le_bytes());
        c.extend(fileoff.to_le_bytes());
        c.extend(32u32.to_le_bytes());
        c.extend(0u32.to_le_bytes());
        c.extend(id.as_bytes());
        c.push(0);
        while c.len() % 8 != 0 {
            c.push(0);
        }
        let size = c.len() as u32;
        c[4..8].copy_from_slice(&size.to_le_bytes());
        c
    }

    fn submacho(symoff: u32, nsyms: u32, stroff: u32, strsize: u32) -> Vec<u8> {
        let mut d = vec![];
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

    fn nlist(strx: u32, value: u64) -> Vec<u8> {
        let mut e = vec![];
        e.extend(strx.to_le_bytes());
        e.push(0x0f); // N_SECT | N_EXT
        e.push(1);
        e.extend(0u16.to_le_bytes());
        e.extend(value.to_le_bytes());
        e
    }

    fn header(cmds: &[Vec<u8>]) -> Vec<u8> {
        let size: usize = cmds.iter().map(Vec::len).sum();
        let mut d = vec![];
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

    #[test]
    fn export_reads_vtable_slots() {
        // A minimal MH_FILESET: a __DATA_CONST segment holds a 5-word vtable
        // (offset-to-top, typeinfo, two method auth-fixup words, a trailing
        // zero), and a fileset entry's symtab names the vtable and the symbol
        // after it, which bounds the read.
        const V: u64 = 0xffff_0000_0001_0000;
        let word_a: u64 = (1 << 63) | (1 << 48) | (0x1234 << 32) | 0x100;
        let word_b: u64 = (1 << 63) | (1 << 48) | (0x5678 << 32) | 0x200;
        let vtable = [0u64, 0, word_a, word_b, 0];
        let strtab: &[u8] = b"\0__ZTVtest\0_next\0";

        // Place [header+cmds][submacho][symtab][strtab][vtable data], computing
        // each absolute offset, then rebuild the commands with the real offsets.
        let hdr_len = 32 + seg("__DATA_CONST", V, 0, 0).len() + fileset_entry("k", 0).len();
        let submacho_at = hdr_len;
        let symtab_at = submacho_at + submacho(0, 0, 0, 0).len();
        let strtab_at = symtab_at + 2 * 16;
        let data_at = (strtab_at + strtab.len() + 7) & !7;

        let mut d = header(&[
            seg("__DATA_CONST", V, data_at as u64, (vtable.len() * 8) as u64),
            fileset_entry("k", submacho_at as u64),
        ]);
        assert_eq!(d.len(), submacho_at);
        d.extend(submacho(
            symtab_at as u32,
            2,
            strtab_at as u32,
            strtab.len() as u32,
        ));
        d.extend(nlist(1, V)); // "__ZTVtest" at strtab offset 1
        d.extend(nlist(11, V + (vtable.len() as u64) * 8)); // "_next" at offset 11
        assert_eq!(d.len(), strtab_at);
        d.extend(strtab);
        while d.len() < data_at {
            d.push(0);
        }
        for w in vtable {
            d.extend(w.to_le_bytes());
        }

        let syms = Header::parse(&d).unwrap().all_symbols(&d).unwrap();
        let tables = export(&d, &syms, 0x1000, &[String::from("__ZTVtest")]).unwrap();
        assert_eq!(tables.len(), 1);
        let t = &tables[0];
        assert_eq!(t.symbol, "__ZTVtest");
        assert_eq!(t.base, V + 0x1000); // slid
        assert_eq!(t.first_slot, 2);
        // The trailing zero word is trimmed; the two method slots remain.
        assert_eq!(t.slots.len(), 2);
        let s = |i: usize| {
            (
                t.slots[i].slot,
                t.slots[i].div,
                t.slots[i].key,
                t.slots[i].addr_div,
            )
        };
        assert_eq!(s(0), (2, 0x1234, 0, 1));
        assert_eq!(s(1), (3, 0x5678, 0, 1));

        // The blob packs { u16 slot, u16 div, u8 key, u8 addr_div, u8 pad[2] }
        // little-endian, in slot order.
        let b = blob(t);
        assert_eq!(b.len(), 16);
        assert_eq!(&b[0..8], &[0x02, 0x00, 0x34, 0x12, 0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&b[8..16], &[0x03, 0x00, 0x78, 0x56, 0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn missing_symbol_is_an_error() {
        let d = header(&[seg("__DATA_CONST", 0x1000, 0x100, 0x100)]);
        let syms = Header::parse(&d).unwrap().all_symbols(&d).unwrap();
        assert_eq!(
            export(&d, &syms, 0, &[String::from("__ZTVgone")]),
            Err(Error::NoSymbol(String::from("__ZTVgone")))
        );
    }
}
