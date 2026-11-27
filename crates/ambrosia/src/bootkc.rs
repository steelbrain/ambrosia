//! Placing kexts in the Boot KC itself, as OpenCore injects kexts into an x86
//! kernel collection: for a kext the kernel must have before user space. The
//! Auxiliary KC ([`crate::auxkc`]) is loaded only once early user space runs,
//! which is too late for a driver the boot needs, such as one the root or data
//! volume's mount waits on. A Boot KC's kexts are known to the kernel from its
//! first moments: its early pass over the collection's chained fixups rebases
//! and signs their pointers with everyone else's, and IOKit matching starts
//! them as it starts Apple's.
//!
//! A kext's segments go in the free tails of the collection's own segments of
//! the same names (`__TEXT`, `__TEXT_EXEC`, `__DATA_CONST`, `__DATA`), from a
//! 4 KiB page boundary on plus the segment's own offset in its page, so that
//! the in-page offsets its `adrp` pairs carry still hold ([`crate::place`]
//! re-encodes the pages). Its pointers join the collection's chains: each is a
//! kernel-cache rebase at `cacheLevel` 0, and a page's existing chain is
//! extended to the kext's first slot in it. It gets an `LC_FILESET_ENTRY` in
//! the collection's header and a `_PrelinkInfoDictionary` entry with its
//! unslid addresses, as every Boot KC kext has.

use crate::auxkc::{KextInput, Own, collseg_name, kext_fixups, mapped, parse};
use crate::boot::{Error, KextError, insert_prelink, prelink_dict};
use crate::macho::Header;
use crate::place::{KMOD_INFO_FIELDS, KMOD_INFO_OUTSIDE, Placement, Seg, place_off, place_one};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

const PAGE_4K: u64 = 0x1000;
const LC_SEGMENT_64: u32 = 0x19;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
/// `DYLD_CHAINED_PTR_64_KERNEL_CACHE`.
const KC_POINTER_FORMAT: u16 = 8;
/// A page with no chain.
const START_NONE: u16 = 0xffff;
/// A page with several chains, which this doesn't extend.
const START_MULTI: u16 = 0x8000;
/// Room kept after the collection's load commands for the entries added.
const LOAD_COMMAND_ROOM: u64 = 0x400;
/// The collection segments a kext's segments go in.
const SEGMENTS: [&str; 4] = ["__TEXT", "__TEXT_EXEC", "__DATA_CONST", "__DATA"];

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    d.get(at..)?.first_chunk().copied().map(u16::from_le_bytes)
}
fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..)?.first_chunk().copied().map(u32::from_le_bytes)
}
fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    d.get(at..)?.first_chunk().copied().map(u64::from_le_bytes)
}
fn put(d: &mut [u8], at: usize, v: &[u8]) -> Option<()> {
    d.get_mut(at..at.checked_add(v.len())?)?.copy_from_slice(v);
    Some(())
}

fn bad(what: &'static str) -> Error {
    Error::Place(what)
}

/// A collection segment's span and where its free tail starts.
struct Free {
    name: &'static str,
    end: u64,
    next: u64,
}

/// Where the used part of the collection segment `[lo_va, hi_va)` ends: past
/// the last section of every fileset member's segment in it (its memory size
/// if it has none: the linker rounds the last member's segment up to the
/// collection segment's end, over the padding), and past its last non-zero
/// byte.
fn used_end(kc: &[u8], lo: u64, h: &Header, lo_va: u64, hi_va: u64) -> Result<u64, Error> {
    let truncated = || bad("a fileset member's header is truncated");
    let mut end = lo_va;
    for e in &h.entries {
        let fo = e.fileoff as usize;
        let ncmds = u32_at(kc, fo + 16).ok_or_else(truncated)? as usize;
        let mut at = fo + 32;
        for _ in 0..ncmds {
            let cmd = u32_at(kc, at).ok_or_else(truncated)?;
            let size = u32_at(kc, at + 4).ok_or_else(truncated)? as usize;
            if cmd == LC_SEGMENT_64 {
                let va = u64_at(kc, at + 24).ok_or_else(truncated)?;
                let vs = u64_at(kc, at + 32).ok_or_else(truncated)?;
                let nsects = u32_at(kc, at + 64).ok_or_else(truncated)? as usize;
                if vs > 0 && va >= lo_va && va < hi_va {
                    let mut seg_end = if nsects == 0 { va + vs } else { va };
                    for i in 0..nsects {
                        let sect = at + 72 + 80 * i;
                        let addr = u64_at(kc, sect + 32).ok_or_else(truncated)?;
                        let len = u64_at(kc, sect + 40).ok_or_else(truncated)?;
                        seg_end = seg_end.max(addr + len);
                    }
                    end = end.max(seg_end);
                }
            }
            at += size;
        }
    }
    let (a, b) = ((lo_va - lo) as usize, (hi_va - lo) as usize);
    let bytes = kc
        .get(a..b)
        .ok_or(bad("a Boot KC segment runs past the collection"))?;
    if let Some(last) = bytes.iter().rposition(|&c| c != 0) {
        end = end.max(lo_va + last as u64 + 1);
    }
    Ok(end)
}

/// A fileset member's segment: its address and size, by name.
fn member_segment(kc: &[u8], fo: usize, name: &str) -> Option<(u64, u64)> {
    let ncmds = u32_at(kc, fo + 16)? as usize;
    let mut at = fo + 32;
    for _ in 0..ncmds {
        let size = u32_at(kc, at + 4)? as usize;
        if u32_at(kc, at)? == LC_SEGMENT_64 {
            let n = kc.get(at + 8..at + 24)?;
            let len = n.iter().position(|&b| b == 0).unwrap_or(16);
            if &n[..len] == name.as_bytes() {
                return Some((u64_at(kc, at + 24)?, u64_at(kc, at + 32)?));
            }
        }
        at += size;
    }
    None
}

/// Where in the collection's `__TEXT_EXEC` kext code may go: the part the
/// kernel maps executable for its kexts, `segPLKTEXTEXEC` in XNU's
/// arm_vm_init.c. With the kernel's own `__TEXT_EXEC` at the segment's
/// start, the kexts' code follows the kernel's `__LAST`; otherwise (macOS 26)
/// it comes before the kernel's code. Past either is the kernel's own code
/// and `__LAST`, which it maps non-executable once it has booted.
fn kext_text_exec(kc: &[u8], h: &Header, seg: (u64, u64)) -> Result<(u64, u64), Error> {
    let k = h
        .entries
        .iter()
        .find(|e| e.id == "com.apple.kernel")
        .ok_or(bad("the Boot KC has no com.apple.kernel"))?;
    let fo = k.fileoff as usize;
    let text = member_segment(kc, fo, "__TEXT_EXEC").ok_or(bad("the kernel has no __TEXT_EXEC"))?;
    if text.0 == seg.0 {
        let last = member_segment(kc, fo, "__LAST").ok_or(bad("the kernel has no __LAST"))?;
        Ok((last.0 + last.1, seg.0 + seg.1))
    } else {
        Ok((seg.0, text.0))
    }
}

/// The collection's chained-fixups table: its file offset and size.
fn chained_fixups(kc: &[u8]) -> Result<(usize, usize), Error> {
    let ncmds = u32_at(kc, 16).ok_or(bad("the Boot KC's header is truncated"))? as usize;
    let mut at = 32;
    for _ in 0..ncmds {
        let cmd = u32_at(kc, at).ok_or(bad("the Boot KC's load commands are truncated"))?;
        let size = u32_at(kc, at + 4).ok_or(bad("the Boot KC's load commands are truncated"))?;
        if cmd == LC_DYLD_CHAINED_FIXUPS {
            let off = u32_at(kc, at + 8).ok_or(bad("bad LC_DYLD_CHAINED_FIXUPS"))?;
            let len = u32_at(kc, at + 12).ok_or(bad("bad LC_DYLD_CHAINED_FIXUPS"))?;
            return Ok((off as usize, len as usize));
        }
        at += size as usize;
    }
    Err(bad("the Boot KC has no LC_DYLD_CHAINED_FIXUPS"))
}

/// The collection's `LC_SEGMENT_64`s in load-command order, as the chained
/// fixups' segment indexes count them: (file offset, file size).
fn segment_order(kc: &[u8]) -> Vec<(u64, u64)> {
    let ncmds = u32_at(kc, 16).unwrap_or(0) as usize;
    let (mut at, mut out) = (32, Vec::new());
    for _ in 0..ncmds {
        let (Some(cmd), Some(size)) = (u32_at(kc, at), u32_at(kc, at + 4)) else {
            break;
        };
        if cmd == LC_SEGMENT_64 {
            out.push((
                u64_at(kc, at + 40).unwrap_or(0),
                u64_at(kc, at + 48).unwrap_or(0),
            ));
        }
        at += size as usize;
    }
    out
}

/// Threads `fx`, (file offset, word without `next`), into the collection's
/// chains: per page, the kext slots chain to each other in order, and the
/// page's last existing slot to the first of them, or the page starts with
/// them if it had none.
fn join_chains(kc: &mut [u8], mut fx: Vec<(u64, u64)>) -> Result<(), Error> {
    let (table, len) = chained_fixups(kc)?;
    let segs = segment_order(kc);
    let t = |o: usize| table + o;
    let truncated = || bad("the Boot KC's chained fixups are truncated");
    if table + len > kc.len() {
        return Err(truncated());
    }
    let starts = u32_at(kc, t(4)).ok_or_else(truncated)? as usize;
    let seg_count = u32_at(kc, t(starts)).ok_or_else(truncated)? as usize;
    fx.sort_unstable_by_key(|&(pos, _)| pos);
    let mut i = 0;
    while i < fx.len() {
        let pos = fx[i].0;
        let si = segs
            .iter()
            .position(|&(o, s)| pos >= o && pos < o + s)
            .filter(|&si| si < seg_count)
            .ok_or(bad("a kext slot is in no Boot KC segment"))?;
        let info = u32_at(kc, t(starts + 4 + 4 * si)).ok_or_else(truncated)? as usize;
        if info == 0 {
            return Err(bad(
                "a kext slot is in a Boot KC segment without chained fixups",
            ));
        }
        let s = t(starts + info);
        let page_size = u64::from(u16_at(kc, s + 4).ok_or_else(truncated)?);
        if u16_at(kc, s + 6) != Some(KC_POINTER_FORMAT) || page_size == 0 {
            return Err(bad(
                "the Boot KC's chained fixups are not kernel-cache pointers",
            ));
        }
        let seg_off = u64_at(kc, s + 8).ok_or_else(truncated)?;
        let page_count = u64::from(u16_at(kc, s + 20).ok_or_else(truncated)?);
        let page = (pos - seg_off) / page_size;
        if page >= page_count {
            return Err(bad("a kext slot is past its segment's chained-fixup pages"));
        }
        let page_lo = seg_off + page * page_size;
        // This page's kext slots.
        let mut j = i;
        while j < fx.len() && fx[j].0 < page_lo + page_size {
            j += 1;
        }
        let ps_at = s + 22 + 2 * page as usize;
        let ps = u16_at(kc, ps_at).ok_or_else(truncated)?;
        let first = fx[i].0;
        if ps == START_NONE {
            put(kc, ps_at, &((first - page_lo) as u16).to_le_bytes()).ok_or_else(truncated)?;
        } else if ps & START_MULTI != 0 {
            return Err(bad("a Boot KC page the kext shares has several chains"));
        } else {
            let mut cur = page_lo + u64::from(ps);
            loop {
                let w = u64_at(kc, cur as usize).ok_or_else(truncated)?;
                let next = (w >> 51) & 0xfff;
                if next == 0 {
                    if first <= cur || (first - cur) / 4 > 0xfff {
                        return Err(bad("a kext slot can't join its page's chain"));
                    }
                    let w = w | (((first - cur) / 4) << 51);
                    put(kc, cur as usize, &w.to_le_bytes()).ok_or_else(truncated)?;
                    break;
                }
                cur += 4 * next;
            }
        }
        for k in i..j {
            let (pos, word) = fx[k];
            let next = if k + 1 < j {
                (fx[k + 1].0 - pos) / 4
            } else {
                0
            };
            put(
                kc,
                pos as usize,
                &(word | ((next & 0xfff) << 51)).to_le_bytes(),
            )
            .ok_or_else(truncated)?;
        }
        i = j;
    }
    Ok(())
}

/// Adds an `LC_FILESET_ENTRY` naming `id`, whose header is at `va`.
fn add_fileset_entry(kc: &mut [u8], lo: u64, id: &str, va: u64) -> Result<(), Error> {
    let short = || bad("the Boot KC's header has no room for another fileset entry");
    let ncmds = u32_at(kc, 16).ok_or_else(short)?;
    let sizeofcmds = u32_at(kc, 20).ok_or_else(short)?;
    let at = 32 + sizeofcmds as usize;
    let size = (32 + id.len() + 1).next_multiple_of(8);
    let mut c = alloc::vec![0u8; size];
    c[0..4].copy_from_slice(&LC_FILESET_ENTRY.to_le_bytes());
    c[4..8].copy_from_slice(&(size as u32).to_le_bytes());
    c[8..16].copy_from_slice(&va.to_le_bytes());
    c[16..24].copy_from_slice(&(va - lo).to_le_bytes());
    c[24..28].copy_from_slice(&32u32.to_le_bytes());
    c[32..32 + id.len()].copy_from_slice(id.as_bytes());
    if kc
        .get(at..at + size)
        .is_none_or(|room| room.iter().any(|&b| b != 0))
    {
        return Err(short());
    }
    put(kc, at, &c).ok_or_else(short)?;
    put(kc, 16, &(ncmds + 1).to_le_bytes()).ok_or_else(short)?;
    put(kc, 20, &(sizeofcmds + size as u32).to_le_bytes()).ok_or_else(short)?;
    Ok(())
}

/// A kext placed in the Boot KC.
pub struct Placed {
    /// Its `CFBundleIdentifier`.
    pub id: String,
    /// Its header's unslid address.
    pub base: u64,
    /// The span of its segments.
    pub size: u64,
}

/// Places `kexts` in the Boot KC `kc`, whose file offsets are its virtual
/// addresses less `lo`, binding their imports against `symbols` (unslid).
/// Gives the placed kexts and their exports at their unslid addresses.
pub fn inject(
    kc: &mut [u8],
    lo: u64,
    kexts: &[KextInput<'_>],
    symbols: &BTreeMap<String, u64>,
    log: &mut dyn FnMut(fmt::Arguments<'_>),
) -> Result<(Vec<Placed>, BTreeMap<String, u64>), Error> {
    let h = Header::parse(kc).map_err(Error::Kernel)?;
    let sizeofcmds = u64::from(u32_at(kc, 20).ok_or(bad("the Boot KC's header is truncated"))?);
    let mut free: Vec<Free> = Vec::new();
    for name in SEGMENTS {
        let s = h
            .segments
            .iter()
            .find(|s| s.name == name)
            .ok_or(bad("the Boot KC lacks a segment a kext goes in"))?;
        let (from, to) = if name == "__TEXT_EXEC" {
            kext_text_exec(kc, &h, (s.vmaddr, s.vmsize))?
        } else {
            (s.vmaddr, s.vmaddr + s.vmsize)
        };
        let mut used = used_end(kc, lo, &h, from, to)?;
        if name == "__TEXT" {
            used = used.max(lo + 32 + sizeofcmds + LOAD_COMMAND_ROOM);
        }
        free.push(Free {
            name,
            end: to,
            next: used,
        });
    }

    // Lay each kext's segments in the tails.
    let mut parsed = Vec::with_capacity(kexts.len());
    for ki in kexts {
        let k = parse(ki)?;
        let mut p = Placement {
            segs: Vec::new(),
            load_addr: 0,
            code_size: 0,
            kmod_addr: 0,
        };
        let (mut lo_va, mut hi_va) = (u64::MAX, 0);
        for s in mapped(&k) {
            let name = collseg_name(&s.name).ok_or_else(|| {
                Error::Kext(
                    ki.id.into(),
                    KextError::Place("it has a segment no collection holds"),
                )
            })?;
            let f = free
                .iter_mut()
                .find(|f| f.name == name)
                .ok_or(bad("no such segment"))?;
            let mut va = f.next.next_multiple_of(PAGE_4K) + s.fileoff % PAGE_4K;
            if va < f.next {
                va += PAGE_4K;
            }
            // Its contents, not the linker's whole pages: the tails are
            // smaller than a page, and a copy past them would overwrite what
            // follows.
            let end = va + s.extent;
            if end > f.end {
                return Err(Error::Kext(
                    ki.id.into(),
                    KextError::Place("the Boot KC has no room left for it in a segment"),
                ));
            }
            f.next = end;
            p.segs.push(Seg {
                fileoff: s.fileoff,
                filesize: s.filesize.min(s.extent),
                vmsize: s.extent,
                pos: va - lo,
                va,
            });
            lo_va = lo_va.min(va);
            hi_va = hi_va.max(end);
        }
        let id = ki.id;
        p.load_addr = place_off(&p, 0)
            .ok_or_else(|| Error::Kext(id.into(), KextError::Place("its header is not placed")))?
            .0;
        p.code_size = hi_va - lo_va;
        p.kmod_addr = place_off(&p, k.kmod_info)
            .filter(|_| place_off(&p, k.kmod_info + KMOD_INFO_FIELDS - 1).is_some())
            .ok_or_else(|| Error::Kext(id.into(), KMOD_INFO_OUTSIDE))?
            .0;
        parsed.push((ki, k, p));
    }

    // Every placed kext's exports, unslid, and as Boot-KC targets for binds
    // between them.
    let mut exports: BTreeMap<String, u64> = BTreeMap::new();
    for (_, k, p) in &parsed {
        for (name, value) in k.exports() {
            if let Some((va, _)) = place_off(p, value) {
                exports.entry(name).or_insert(va);
            }
        }
    }
    let targets: BTreeMap<String, u64> = exports
        .iter()
        .map(|(n, &va)| (n.clone(), va - lo))
        .collect();

    let own = Own { level: 0, base: lo };
    let mut fx = Vec::new();
    let mut placed = Vec::with_capacity(parsed.len());
    let mut dicts = Vec::with_capacity(parsed.len());
    for (ki, k, p) in &parsed {
        let id = ki.id;
        place_one(kc, p, k, ki.exe).map_err(|e| Error::Kext(id.into(), e))?;
        fx.extend(
            kext_fixups(p, k, symbols, &targets, lo, own).map_err(|e| Error::Kext(id.into(), e))?,
        );
        add_fileset_entry(kc, lo, id, p.load_addr)?;
        dicts.push(
            prelink_dict(ki.info, p.load_addr, p.code_size, p.kmod_addr)
                .map_err(|e| Error::Kext(id.into(), e))?,
        );
        log(format_args!(
            "Boot KC: kext {id} at {:#x}, {:#x} bytes, kmod {:#x}",
            p.load_addr, p.code_size, p.kmod_addr
        ));
        placed.push(Placed {
            id: id.into(),
            base: p.load_addr,
            size: p.code_size,
        });
    }
    join_chains(kc, fx)?;
    let pi = h
        .segments
        .iter()
        .find(|s| s.name == "__PRELINK_INFO")
        .ok_or(Error::NoPrelinkArray)?;
    insert_prelink(kc, (pi.fileoff, pi.filesize), &dicts)?;
    Ok((placed, exports))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A collection of one 32 KiB `__DATA` segment at 0x4000 (two 16 KiB
    /// pages) whose chained fixups, at 0xc000, start page 0 at 0x10 and
    /// leave page 1 without a chain; page 0's chain is 0x4010 -> 0x4020.
    fn collection() -> Vec<u8> {
        let mut kc = vec![0u8; 0x10000];
        put(&mut kc, 16, &2u32.to_le_bytes());
        put(&mut kc, 20, &(72u32 + 16).to_le_bytes());
        // LC_SEGMENT_64 __DATA
        put(&mut kc, 32, &LC_SEGMENT_64.to_le_bytes());
        put(&mut kc, 36, &72u32.to_le_bytes());
        put(&mut kc, 40, b"__DATA");
        put(&mut kc, 56, &0x4000u64.to_le_bytes()); // vmaddr
        put(&mut kc, 64, &0x8000u64.to_le_bytes()); // vmsize
        put(&mut kc, 72, &0x4000u64.to_le_bytes()); // fileoff
        put(&mut kc, 80, &0x8000u64.to_le_bytes()); // filesize
        // LC_DYLD_CHAINED_FIXUPS
        put(&mut kc, 104, &LC_DYLD_CHAINED_FIXUPS.to_le_bytes());
        put(&mut kc, 108, &16u32.to_le_bytes());
        put(&mut kc, 112, &0xc000u32.to_le_bytes());
        put(&mut kc, 116, &0x100u32.to_le_bytes());
        // dyld_chained_fixups_header: starts at +0x20
        put(&mut kc, 0xc004, &0x20u32.to_le_bytes());
        // starts_in_image: one segment, its starts at +8 from the image's
        put(&mut kc, 0xc020, &1u32.to_le_bytes());
        put(&mut kc, 0xc024, &8u32.to_le_bytes());
        let s = 0xc028;
        put(&mut kc, s, &26u32.to_le_bytes());
        put(&mut kc, s + 4, &0x4000u16.to_le_bytes());
        put(&mut kc, s + 6, &KC_POINTER_FORMAT.to_le_bytes());
        put(&mut kc, s + 8, &0x4000u64.to_le_bytes());
        put(&mut kc, s + 20, &2u16.to_le_bytes());
        put(&mut kc, s + 22, &0x10u16.to_le_bytes());
        put(&mut kc, s + 24, &START_NONE.to_le_bytes());
        put(&mut kc, 0x4010, &(0x111u64 | (4 << 51)).to_le_bytes());
        put(&mut kc, 0x4020, &0x222u64.to_le_bytes());
        kc
    }

    fn word(kc: &[u8], at: usize) -> u64 {
        u64_at(kc, at).unwrap()
    }

    #[test]
    fn joins_a_pages_chain_and_starts_an_empty_one() {
        let mut kc = collection();
        join_chains(
            &mut kc,
            vec![(0x8008, 0x333), (0x4108, 0x555), (0x4100, 0x444)],
        )
        .unwrap();
        // The page's last slot now leads to the kext's first, which leads on.
        assert_eq!(word(&kc, 0x4010), 0x111 | (4 << 51));
        assert_eq!(word(&kc, 0x4020), 0x222 | (((0x4100 - 0x4020) / 4) << 51));
        assert_eq!(word(&kc, 0x4100), 0x444 | (2 << 51));
        assert_eq!(word(&kc, 0x4108), 0x555);
        // Page 1 had no chain: it starts with the kext's slot.
        assert_eq!(u16_at(&kc, 0xc028 + 24), Some(8));
        assert_eq!(word(&kc, 0x8008), 0x333);
    }

    #[test]
    fn refuses_a_slot_before_the_pages_chain_ends() {
        let mut kc = collection();
        assert!(join_chains(&mut kc, vec![(0x4018, 0x444)]).is_err());
    }

    #[test]
    fn refuses_a_slot_outside_the_chained_segments() {
        let mut kc = collection();
        assert!(join_chains(&mut kc, vec![(0x100, 0x444)]).is_err());
    }

    #[test]
    fn adds_a_fileset_entry() {
        let mut kc = collection();
        add_fileset_entry(
            &mut kc,
            0xffff_0000_0000_0000,
            "org.example.k",
            0xffff_0000_0000_9000,
        )
        .unwrap();
        assert_eq!(u32_at(&kc, 16), Some(3));
        let at = 32 + 88;
        assert_eq!(u32_at(&kc, at), Some(LC_FILESET_ENTRY));
        assert_eq!(u32_at(&kc, at + 4), Some(48));
        assert_eq!(u32_at(&kc, 20), Some(88 + 48));
        assert_eq!(u64_at(&kc, at + 8), Some(0xffff_0000_0000_9000));
        assert_eq!(u64_at(&kc, at + 16), Some(0x9000));
        assert_eq!(&kc[at + 32..at + 45], b"org.example.k");
        // The header has no room past bytes that are not zero.
        let mut full = collection();
        put(&mut full, 32 + 88 + 40, &[1]);
        assert!(add_fileset_entry(&mut full, 0, "org.example.k", 0x9000).is_err());
    }

    #[test]
    fn finds_a_members_segment() {
        let kc = collection();
        assert_eq!(member_segment(&kc, 0, "__DATA"), Some((0x4000, 0x8000)));
        assert_eq!(member_segment(&kc, 0, "__TEXT"), None);
    }
}
