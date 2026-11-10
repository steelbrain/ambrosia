//! Building an Auxiliary kernel collection (`MH_FILESET`) that holds the
//! configured kexts, which the boot places below the Boot KC for the kernel to
//! load with SIP on (docs/internals/replacing-iboot.md, "The Auxiliary kernel
//! collection"). A third-party kext normally goes in the Auxiliary KC;
//! Ambrosia builds that collection itself, in memory, and advertises it in
//! `/chosen/memory-map` as `AuxKC-mach_header`, exactly as the Boot KC is
//! advertised, so the kernel's deferred AuxKC loader finds and starts the
//! kexts with nothing done in the guest.
//!
//! The collection holds one or more kexts, their segments coalesced by name
//! into one top-level segment each (`__TEXT`, `__TEXT_EXEC`, `__DATA_CONST`,
//! `__DATA`, in that order), as any kernel collection coalesces its members:
//! all the kexts' `__TEXT_EXEC` go into the one `__TEXT_EXEC`, mapped
//! executable, and so on, so `arm_vm_prot_init` maps each by name and the
//! kernel's fileset loader, which resolves a sub-kext's segments against the
//! top-level segments by name, finds exactly one per name. A kext's own
//! segments are therefore spread across the shared segments. Each kext's
//! cross-segment `adrp`s are re-encoded for its placement, and each gets a
//! `_PrelinkInfoDictionary` entry and an `LC_FILESET_ENTRY` that name it. The
//! collection's `_BootKCID` is set to the loaded Boot KC's `LC_UUID`
//! (`validateKCFileSetUUID`), and its `_PrelinkKCID` to the collection's own.
//!
//! Unlike a Boot-KC placement, a kext's pointers are **not** pre-signed. The
//! AuxKC is started by the kernel's deferred in-memory-AuxKC loader, which walks
//! the collection's own chained-fixups table and rebases and signs every pointer
//! there itself (`DYLD_CHAINED_PTR_64_KERNEL_CACHE`: `basePointers[cacheLevel] +
//! target`, then a PAC sign for an `isAuth` slot). So Ambrosia emits one real
//! chained-fixups table covering every kext — each kext's rebases and binds as a
//! zero-based kernel-cache rebase word, `cacheLevel` 3 (this Auxiliary KC) for a
//! pointer into itself and `cacheLevel` 0 (the Boot KC) for a bind to a kernel
//! symbol — and leaves the slots as raw chain words for the kernel to sign.
//! A slot signed here would have the loader authenticate a pointer it did not
//! sign, which traps (FEAT_FPAC) when the kext starts.

use crate::boot::{Error, KextError, insert_prelink, prelink_dict};
use crate::kext::{self};
use crate::place::{KMOD_INFO_FIELDS, KMOD_INFO_OUTSIDE, Placement, Seg, place_off, place_one};
use crate::plist;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

const PAGE: u64 = 0x4000;
const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_UUID: u32 = 0x1b;
const LC_FILESET_ENTRY: u32 = 0x8000_0035;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
const MH_FILESET: u32 = 12;

fn put_u32(d: &mut [u8], at: usize, v: u32) {
    d[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_u64(d: &mut [u8], at: usize, v: u64) {
    d[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn get_u32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}
/// `n` rounded up to whole pages, or `None` on overflow.
fn page_up(n: u64) -> Option<u64> {
    n.checked_next_multiple_of(PAGE)
}

/// The Boot KC's `LC_UUID` (16 bytes), read from its header.
pub fn boot_kc_uuid(kc: &[u8]) -> Option<[u8; 16]> {
    let ncmds = get_u32(kc, 16) as usize;
    let mut at = 32;
    for _ in 0..ncmds {
        if get_u32(kc, at) == LC_UUID {
            return kc.get(at + 8..at + 24).map(|s| s.try_into().unwrap());
        }
        at += get_u32(kc, at + 4) as usize;
    }
    None
}

/// The boot's error for kext `id`'s `e`.
fn kext_error(id: &str, e: KextError) -> Error {
    Error::Kext(id.into(), e)
}

/// Base64 of raw bytes, for a plist `<data>`.
fn base64(d: &[u8]) -> String {
    let mut s = String::new();
    plist::base64_encode(&mut s, d);
    s
}

/// One kext to place in the Auxiliary KC: its bundle identifier, `Info.plist`
/// and `MH_KEXT_BUNDLE` executable, as the configuration named them.
pub struct KextInput<'a> {
    /// The kext's `CFBundleIdentifier`.
    pub id: &'a str,
    /// The kext's `Info.plist`.
    pub info: &'a [u8],
    /// The kext's `MH_KEXT_BUNDLE` executable.
    pub exe: &'a [u8],
}

/// The canonical order of the collection's top-level segments, one of each
/// name. A kernel collection coalesces every member's segments by name into
/// these, and the kernel's fileset loader resolves a sub-kext's segments
/// against them by name — so there is exactly one segment per name, never one
/// per kext.
const COLL_ORDER: [&str; 4] = ["__TEXT", "__TEXT_EXEC", "__DATA_CONST", "__DATA"];

/// Why a kext can't be placed whose mach header is in no placed segment.
const NO_HEADER_SEGMENT: &str = "it has no segment at file offset 0";
/// Why a kext can't be placed one of whose fixups is outside its placed
/// segments.
const FIXUP_OUTSIDE: KextError =
    KextError::Place("a chained fixup's slot or target is outside the segments Ambrosia places");

/// The top-level collection segment a kext segment is coalesced into, or `None`
/// for a segment Ambrosia does not place. `__OBJC_CONST` rides in
/// `__DATA_CONST`.
fn collseg_name(name: &str) -> Option<&'static str> {
    match name {
        "__TEXT" => Some("__TEXT"),
        "__TEXT_EXEC" => Some("__TEXT_EXEC"),
        "__DATA_CONST" | "__OBJC_CONST" => Some("__DATA_CONST"),
        "__DATA" => Some("__DATA"),
        _ => None,
    }
}

/// The segments of `k` the collection holds: those with contents, but
/// `__LINKEDIT`, in load-command order.
fn mapped<'k>(k: &'k kext::Kext<'_>) -> impl Iterator<Item = &'k kext::Segment> {
    k.segments
        .iter()
        .filter(|s| s.filesize > 0 && s.name != "__LINKEDIT")
}

/// Parses the kext `ki` and checks what laying it out needs of it, as
/// [`size`] and [`build`] both do: its mapped segments start with the one at
/// file offset 0, which holds its mach header, each is one the collection
/// holds, and the `kmod_info` fields placing it writes lie in one of them.
fn parse<'a>(ki: &KextInput<'a>) -> Result<kext::Kext<'a>, Error> {
    let k = kext::Kext::parse(ki.exe).map_err(|e| kext_error(ki.id, KextError::Exe(e)))?;
    if mapped(&k).next().is_none_or(|s| s.fileoff != 0) {
        return Err(kext_error(
            ki.id,
            KextError::Place("its first segment with contents is not at file offset 0"),
        ));
    }
    if let Some(s) = mapped(&k).find(|s| collseg_name(&s.name).is_none()) {
        return Err(kext_error(ki.id, KextError::Segment(s.name.clone())));
    }
    // As `place::place_span` finds them, by file offset over the segment's
    // memory size.
    let holds_kmod_info = |s: &kext::Segment| {
        k.kmod_info
            .checked_sub(s.fileoff)
            .and_then(|into| into.checked_add(KMOD_INFO_FIELDS))
            .is_some_and(|end| end <= s.vmsize)
    };
    if !mapped(&k).any(holds_kmod_info) {
        return Err(kext_error(ki.id, KMOD_INFO_OUTSIDE));
    }
    Ok(k)
}

/// The bytes `build` lays the kexts' mapped segments across, coalesced by name
/// in [`COLL_ORDER`], including the one leading page of `__TEXT` for the
/// collection header. The addresses don't change it. It mirrors `build`'s
/// layout, reserving each segment's full `vmsize` so the reserve matches the
/// collection `build` emits: a segment with a zerofill tail (`vmsize` >
/// `filesize`, bss) takes its whole extent, as `build` lays it out and the
/// kernel materializes it. The collection is placed just below the trust
/// caches and the Boot KC, so a reserve smaller than the built collection
/// would let it overrun into them.
fn segments_size(kexts: &[KextInput<'_>]) -> Result<u64, Error> {
    let parsed: Vec<kext::Kext<'_>> = kexts.iter().map(parse).collect::<Result<_, _>>()?;
    let mut off = 0u64;
    for &oname in &COLL_ORDER {
        if oname == "__TEXT" {
            // The collection header and load commands take __TEXT's first page.
            off = off
                .checked_add(PAGE)
                .ok_or(Error::Overflow("AuxKC header page"))?;
        }
        for k in &parsed {
            for s in mapped(k) {
                if collseg_name(&s.name) != Some(oname) {
                    continue;
                }
                // vmsize, matching build's `page_up(s.vmsize)`: the collection
                // reserves a segment's zerofill tail, not just its file bytes.
                off = page_up(s.vmsize)
                    .and_then(|n| off.checked_add(n))
                    .ok_or(Error::Overflow("AuxKC segment layout"))?;
            }
        }
    }
    Ok(off)
}

/// The `__PRELINK_INFO` reserve: the base plist plus one dict per kext, each
/// carrying its `Info.plist`'s inner keys and a few address integers. Sized by
/// content (address-independent), rounded up to whole pages, at least two.
fn prelink_reserve(kexts: &[KextInput<'_>]) -> Result<u64, Error> {
    let mut need = 1024u64; // the base plist skeleton and the empty array
    for ki in kexts {
        need = need
            .checked_add(ki.info.len() as u64)
            .and_then(|n| n.checked_add(512))
            .ok_or(Error::Overflow("AuxKC prelink reserve"))?;
    }
    let pages = need
        .checked_next_multiple_of(PAGE)
        .ok_or(Error::Overflow("AuxKC prelink reserve"))?;
    Ok(pages.max(2 * PAGE))
}

/// The byte length [`build`] produces for these kexts, computed from the layout
/// alone (addresses don't change it: `__LINKEDIT` is one page, `__PRELINK_INFO`
/// is sized from the kexts' `Info.plist`s). It refuses a kext `build` would
/// refuse for its own layout, as `build` does; what it can't find is what
/// depends on the kernel collection, such as an import nothing defines.
pub fn size(kexts: &[KextInput<'_>]) -> Result<u64, Error> {
    let segs = segments_size(kexts)?;
    let pi = prelink_reserve(kexts)?;
    segs.checked_add(PAGE)
        .and_then(|o| o.checked_add(pi))
        .ok_or(Error::Overflow("AuxKC size"))
}

/// A top-level segment of the collection we emit.
struct CollSeg {
    name: &'static str,
    initprot: u32,
    fileoff: u64,
    filesize: u64,
    nsects: u32,
}

/// `DYLD_CHAINED_PTR_64_KERNEL_CACHE`, the pointer format a kernel collection's
/// chained fixups use.
const KC_POINTER_FORMAT: u16 = 8;
/// The `basePointers` index for the Boot KC (`com.apple.kernel` symbols).
const KC_LEVEL_BOOT: u64 = 0;
/// The `basePointers` index for the Auxiliary KC (this collection's own
/// pointers), from `pexpert/gen/kcformat.c`.
const KC_LEVEL_AUX: u64 = 3;

/// Encodes one `dyld_chained_ptr_64_kernel_cache_rebase` word, less its `next`
/// field: `target`:30 (the offset the kernel adds to `basePointers[level]`),
/// `cacheLevel`:2, `diversity`:16, `addrDiv`:1, `key`:2, `next`:12, `isAuth`:1.
fn kc_word(target: u64, level: u64, auth: bool, key: u8, addr_div: bool, diversity: u16) -> u64 {
    (target & 0x3fff_ffff)
        | (level << 30)
        | (u64::from(diversity) << 32)
        | (u64::from(addr_div) << 48)
        | ((u64::from(key) & 3) << 49)
        | (u64::from(auth) << 63)
}

/// Where a bind's symbol lives, as `(target, cacheLevel)` for [`kc_word`].
/// Resolved first against the Boot KC's kernel symbols (`cacheLevel` 0, the
/// symbol's offset from the Boot KC base, which the kernel adds to
/// `basePointers[0]`); failing that, against the symbols a sibling placed kext
/// exports (`cacheLevel` 3, the export's collection address, which the kernel
/// adds to `basePointers[3]`, the AuxKC's own base) — this is how an
/// `OSBundleLibraries` dependency between two placed kexts resolves, as a
/// within-collection reference. Both targets must fit the 30-bit `target` field.
fn bind_target(
    name: &str,
    symbols: &BTreeMap<String, u64>,
    exports: &BTreeMap<String, u64>,
    boot_lo: u64,
) -> Result<(u64, u64), KextError> {
    if let Some(&unslid) = symbols.get(name) {
        let off = unslid.checked_sub(boot_lo).ok_or(KextError::Place(
            "a kernel symbol it imports is below the kernel collection's base",
        ))?;
        if off >= (1 << 30) {
            return Err(KextError::Place(
                "a kernel symbol it imports is 1 GiB or more above the kernel collection's base",
            ));
        }
        Ok((off, KC_LEVEL_BOOT))
    } else if let Some(&target) = exports.get(name) {
        if target >= (1 << 30) {
            return Err(KextError::Place(
                "a symbol it imports from another kext is 1 GiB or more into the Auxiliary KC",
            ));
        }
        Ok((target, KC_LEVEL_AUX))
    } else {
        Err(KextError::NoSymbol(name.into()))
    }
}

/// The kernel-cache fixup word for a bind at `ordinal`. `auth`/`key`/`addr_div`/
/// `diversity` carry the import's signing schema; an `auth` bind, Boot-KC or
/// sibling, is signed by the kernel's deferred loader like any other slot.
#[allow(clippy::too_many_arguments)]
fn bind_word(
    k: &kext::Kext<'_>,
    symbols: &BTreeMap<String, u64>,
    exports: &BTreeMap<String, u64>,
    ordinal: u32,
    boot_lo: u64,
    auth: bool,
    key: u8,
    addr_div: bool,
    diversity: u16,
) -> Result<u64, KextError> {
    let name = k.imports.get(ordinal as usize).ok_or(KextError::Place(
        "a chained fixup binds an import it doesn't have",
    ))?;
    let (target, level) = bind_target(name, symbols, exports, boot_lo)?;
    Ok(kc_word(target, level, auth, key, addr_div, diversity))
}

/// A built chained-fixups table: `blob` is the `LC_DYLD_CHAINED_FIXUPS` payload
/// for `__LINKEDIT`, `slots` the `(collection file offset, word)` pairs to write
/// over the placed pointers.
struct Chain {
    blob: Vec<u8>,
    slots: Vec<(u64, u64)>,
}

/// One kext's fixups, as (slot's collection file offset, word without
/// `next`), for [`build_chain`].
fn kext_fixups(
    p: &Placement,
    k: &kext::Kext<'_>,
    symbols: &BTreeMap<String, u64>,
    exports: &BTreeMap<String, u64>,
    boot_lo: u64,
) -> Result<Vec<(u64, u64)>, KextError> {
    let mut fx = Vec::with_capacity(k.fixups.len());
    for f in &k.fixups {
        let pos = place_off(p, f.at).ok_or(FIXUP_OUTSIDE)?.1;
        let word = match f.kind {
            kext::Kind::Rebase { target } => {
                let t = place_off(p, target).ok_or(FIXUP_OUTSIDE)?.0;
                kc_word(t, KC_LEVEL_AUX, false, 0, false, 0)
            }
            kext::Kind::AuthRebase {
                target,
                key,
                addr_div,
                diversity,
            } => {
                let t = place_off(p, target).ok_or(FIXUP_OUTSIDE)?.0;
                kc_word(t, KC_LEVEL_AUX, true, key, addr_div, diversity)
            }
            kext::Kind::Bind { ordinal } => {
                bind_word(k, symbols, exports, ordinal, boot_lo, false, 0, false, 0)?
            }
            kext::Kind::AuthBind {
                ordinal,
                key,
                addr_div,
                diversity,
            } => bind_word(
                k, symbols, exports, ordinal, boot_lo, true, key, addr_div, diversity,
            )?,
        };
        fx.push((pos, word));
    }
    Ok(fx)
}

/// Builds the collection's chained-fixups table (`LC_DYLD_CHAINED_FIXUPS`
/// payload) covering every placed kext's pointers, and the raw kernel-cache word
/// each pointer slot takes. Returns `(blob, slots)`: `blob` is the table for
/// `__LINKEDIT`, `slots` the `(collection file offset, word)` pairs to write
/// over the placed pointers. The kernel's deferred loader walks the table,
/// rebases each word by `basePointers[cacheLevel]`, and signs an `isAuth` slot.
///
/// A rebase or auth-rebase points into this collection (`cacheLevel` 3, target
/// the slot's zero-based collection offset); a bind points into the Boot KC
/// (`cacheLevel` 0, target the kernel symbol's Boot-KC offset). The AuxKC is
/// base-zero, so a slot's file offset equals its zero-based collection address.
/// `kexts` gives each kext's bundle identifier and placement; their slots
/// share one table, threaded per collection page, since the kernel walks the
/// whole collection's fixups in one pass.
fn build_chain(
    coll: &[CollSeg],
    kexts: &[(&str, &Placement, &kext::Kext<'_>)],
    symbols: &BTreeMap<String, u64>,
    exports: &BTreeMap<String, u64>,
    boot_lo: u64,
) -> Result<Chain, Error> {
    // Every kext fixup as (slot's collection file offset, word without `next`).
    let mut fx: Vec<(u64, u64)> = Vec::new();
    for &(id, p, k) in kexts {
        fx.extend(kext_fixups(p, k, symbols, exports, boot_lo).map_err(|e| kext_error(id, e))?);
    }
    fx.sort_unstable_by_key(|&(pos, _)| pos);

    // dyld_chained_starts_in_image: seg_count, then seg_info_offset[seg_count].
    // The pool of per-segment starts follows, 8-aligned (segment_offset is u64).
    let seg_count = coll.len();
    let pool_base = (4 + 4 * seg_count).next_multiple_of(8);
    let mut seg_info_off = vec![0u32; seg_count];
    let mut pool: Vec<u8> = Vec::new();
    let mut slots: Vec<(u64, u64)> = Vec::new();
    for (i, c) in coll.iter().enumerate() {
        let seg_fx: Vec<(u64, u64)> = fx
            .iter()
            .copied()
            .filter(|&(pos, _)| pos >= c.fileoff && pos < c.fileoff + c.filesize)
            .collect();
        if seg_fx.is_empty() {
            continue;
        }
        while !(pool_base + pool.len()).is_multiple_of(8) {
            pool.push(0);
        }
        seg_info_off[i] = (pool_base + pool.len()) as u32;
        let page_count = (c.filesize / PAGE) as usize;
        let mut page_start = vec![0xffffu16; page_count];
        for (pg, start) in page_start.iter_mut().enumerate() {
            let page_lo = c.fileoff + pg as u64 * PAGE;
            let mut in_page: Vec<(u64, u64)> = seg_fx
                .iter()
                .copied()
                .filter(|&(pos, _)| pos >= page_lo && pos < page_lo + PAGE)
                .collect();
            if in_page.is_empty() {
                continue;
            }
            in_page.sort_unstable_by_key(|&(pos, _)| pos);
            *start = (in_page[0].0 - page_lo) as u16;
            for w in 0..in_page.len() {
                let (pos, word) = in_page[w];
                let next = if w + 1 < in_page.len() {
                    (in_page[w + 1].0 - pos) / 4
                } else {
                    0
                };
                slots.push((pos, word | ((next & 0xfff) << 51)));
            }
        }
        // dyld_chained_starts_in_segment.
        let size = 22 + 2 * page_count;
        let mut s = vec![0u8; size];
        put_u32(&mut s, 0, size as u32); // size
        s[4..6].copy_from_slice(&(PAGE as u16).to_le_bytes()); // page_size
        s[6..8].copy_from_slice(&KC_POINTER_FORMAT.to_le_bytes());
        put_u64(&mut s, 8, c.fileoff); // segment_offset (base-zero == vmaddr)
        s[20..22].copy_from_slice(&(page_count as u16).to_le_bytes()); // page_count
        for (pg, st) in page_start.iter().enumerate() {
            s[22 + 2 * pg..24 + 2 * pg].copy_from_slice(&st.to_le_bytes());
        }
        pool.extend_from_slice(&s);
    }

    // The payload: a dyld_chained_fixups_header, the starts_in_image, the pool,
    // and a one-byte empty symbol string. Imports are empty (binds are encoded
    // as kernel-cache rebases into the Boot KC).
    let starts_off = 0x20usize;
    let imports_off = starts_off + pool_base + pool.len();
    let mut blob = vec![0u8; imports_off + 1];
    put_u32(&mut blob, 0, 0); // fixups_version
    put_u32(&mut blob, 4, starts_off as u32); // starts_offset
    put_u32(&mut blob, 8, imports_off as u32); // imports_offset
    put_u32(&mut blob, 12, imports_off as u32); // symbols_offset
    put_u32(&mut blob, 16, 0); // imports_count
    put_u32(&mut blob, 20, 1); // imports_format (DYLD_CHAINED_IMPORT)
    put_u32(&mut blob, 24, 0); // symbols_format
    put_u32(&mut blob, starts_off, seg_count as u32); // starts_in_image.seg_count
    for (i, &o) in seg_info_off.iter().enumerate() {
        put_u32(&mut blob, starts_off + 4 + 4 * i, o);
    }
    blob[starts_off + pool_base..starts_off + pool_base + pool.len()].copy_from_slice(&pool);
    Ok(Chain { blob, slots })
}

/// A kext parsed and placed into the collection: kept so its segments can be
/// copied, its fixups chained and its prelink dict and fileset entry written
/// after the whole layout is known.
struct Placed<'a> {
    k: kext::Kext<'a>,
    info: &'a [u8],
    exe: &'a [u8],
    id: &'a str,
    p: Placement,
}

/// Builds a minimal AuxKC holding the given kexts (one or more). Each kext's
/// mapped segments are laid out in turn at their own zero-based offsets, and it
/// gets its own fileset entry and prelink dict; one chained-fixups table covers
/// every kext's pointers.
///
/// An AuxKC is linked at base zero, and the kernel rebases it by the
/// collection's load address. `slide` is that load address (the linear-map
/// virtual address of the AuxKC, `aux_va`), which the log gives each kext's
/// slid address from. `boot_lo` is the Boot KC's unslid base, from which a
/// bind's kernel-cache target offset is taken; `symbols` maps each kernel
/// symbol to its unslid address. `boot_uuid` is the loaded Boot KC's
/// `LC_UUID`, which the collection's `_BootKCID` carries. `log` takes a
/// progress line.
pub fn build(
    kexts: &[KextInput<'_>],
    slide: u64,
    boot_lo: u64,
    boot_uuid: &[u8; 16],
    symbols: &BTreeMap<String, u64>,
    log: &mut dyn FnMut(fmt::Arguments<'_>),
) -> Result<Vec<u8>, Error> {
    if kexts.is_empty() {
        return Err(Error::Place("no kexts to place"));
    }

    // Parse every kext, then lay their mapped segments into the collection
    // coalesced by name: all kexts' __TEXT into one top-level __TEXT, all
    // __TEXT_EXEC into one __TEXT_EXEC, and so on, in COLL_ORDER. The kernel's
    // fileset loader resolves a sub-kext's segments against the top-level
    // segments by name, so there must be exactly one segment per name; a kext's
    // own segments are therefore spread across the shared segments, as in any
    // kernel collection. The collection header and load commands take __TEXT's
    // first page, ahead of the first kext's mach header.
    let mut placed: Vec<Placed<'_>> = Vec::with_capacity(kexts.len());
    for ki in kexts {
        placed.push(Placed {
            k: parse(ki)?,
            info: ki.info,
            exe: ki.exe,
            id: ki.id,
            p: Placement {
                segs: Vec::new(),
                load_addr: 0,
                code_size: 0,
                kmod_addr: 0,
            },
        });
    }

    // Lay out the coalesced top-level segments. `placements` is written here
    // and moved back into `placed` below; keeping it separate lets the layout
    // read each kext's segments (in `placed`) while writing its placement.
    let mut coll: Vec<CollSeg> = Vec::new();
    let mut off = 0u64;
    let mut placements: Vec<Placement> = placed
        .iter()
        .map(|_| Placement {
            segs: Vec::new(),
            load_addr: 0,
            code_size: 0,
            kmod_addr: 0,
        })
        .collect();
    // Per-kext (lo, hi) vmaddr span, for code_size (kmod_info.size).
    let mut span: Vec<(u64, u64)> = vec![(u64::MAX, 0u64); placed.len()];
    for &oname in &COLL_ORDER {
        let seg_start = off;
        if oname == "__TEXT" {
            off = off
                .checked_add(PAGE)
                .ok_or(Error::Overflow("AuxKC header page"))?;
        }
        let mut initprot: Option<u32> = None;
        for ki in 0..placed.len() {
            for s in mapped(&placed[ki].k) {
                if collseg_name(&s.name) != Some(oname) {
                    continue;
                }
                let content_off = off;
                // place_one copies exe[fileoff..fileoff+filesize]; the collection
                // reserves the full vmsize, so a segment's zerofill tail (bss) is
                // real, zeroed space and a rebase into it resolves.
                placements[ki].segs.push(Seg {
                    fileoff: s.fileoff,
                    filesize: s.filesize,
                    vmsize: s.vmsize,
                    pos: content_off,
                    va: content_off,
                });
                span[ki].0 = span[ki].0.min(content_off);
                span[ki].1 = span[ki].1.max(content_off.saturating_add(s.vmsize));
                initprot.get_or_insert(s.initprot);
                off = page_up(s.vmsize)
                    .and_then(|n| off.checked_add(n))
                    .ok_or(Error::Overflow("AuxKC segment layout"))?;
            }
        }
        if off > seg_start {
            coll.push(CollSeg {
                name: oname,
                initprot: initprot.unwrap_or(1),
                fileoff: seg_start,
                filesize: off - seg_start,
                nsects: 0,
            });
        }
    }
    for ki in 0..placed.len() {
        let id = placed[ki].id;
        placements[ki].load_addr = place_off(&placements[ki], 0)
            .ok_or_else(|| kext_error(id, KextError::Place(NO_HEADER_SEGMENT)))?
            .0;
        placements[ki].code_size = span[ki].1 - span[ki].0;
        placements[ki].kmod_addr = place_off(&placements[ki], placed[ki].k.kmod_info)
            .ok_or_else(|| kext_error(id, KMOD_INFO_OUTSIDE))?
            .0;
    }
    for (pk, pl) in placed.iter_mut().zip(placements) {
        pk.p = pl;
    }

    // Each placed kext's defined-external symbols, at their collection
    // addresses. A placed kext's import of a symbol another placed kext exports
    // (an OSBundleLibraries dependency — a plugin resolving against a base kext)
    // binds against these, as a within-collection reference, rather than against
    // the Boot KC. The symbol's value is a vmaddr from the kext's base, which
    // `place_off` maps to its collection address exactly as a rebase target.
    let mut exports: BTreeMap<String, u64> = BTreeMap::new();
    for pk in &placed {
        for (name, value) in pk.k.exports() {
            if let Some((va, _)) = place_off(&pk.p, value) {
                exports.entry(name).or_insert(va);
            }
        }
    }

    // __LINKEDIT: the shared chained-fixups blob and an empty symbol table.
    let le_off = off;
    coll.push(CollSeg {
        name: "__LINKEDIT",
        initprot: 1,
        fileoff: le_off,
        filesize: PAGE,
        nsects: 0,
    });
    off = off
        .checked_add(PAGE)
        .ok_or(Error::Overflow("AuxKC linkedit"))?;

    // __PRELINK_INFO: the prelink dictionary array, sized from the kexts'
    // Info.plists (its length depends on them, not on addresses, so the total
    // collection size stays independent of the addresses in it).
    let pi_off = off;
    let pi_size = prelink_reserve(kexts)?;
    coll.push(CollSeg {
        name: "__PRELINK_INFO",
        initprot: 3,
        fileoff: pi_off,
        filesize: pi_size,
        nsects: 1,
    });
    off = off
        .checked_add(pi_size)
        .ok_or(Error::Overflow("AuxKC prelink"))?;

    // The collection's chained-fixups table over every kext's pointers. The
    // slots are filled with raw kernel-cache rebase words (below, after
    // placement); the kernel's deferred loader rebases and signs them.
    let chain_kexts: Vec<(&str, &Placement, &kext::Kext<'_>)> =
        placed.iter().map(|pk| (pk.id, &pk.p, &pk.k)).collect();
    let Chain {
        blob: cf_blob,
        slots: cf_slots,
    } = build_chain(&coll, &chain_kexts, symbols, &exports, boot_lo)?;
    if le_off + cf_blob.len() as u64 + 1 > pi_off {
        return Err(Error::Place(
            "the kexts' chained fixups don't fit in its 16 KiB __LINKEDIT",
        ));
    }

    let total = off as usize;
    let mut new = vec![0u8; total];

    // Collection mach header, in __TEXT's first page. cputype/cpusubtype come
    // from the first kext (all the kexts are arm64e).
    let exe0 = placed[0].exe;
    put_u32(&mut new, 0, 0xfeed_facf); // MH_MAGIC_64
    put_u32(&mut new, 4, get_u32(exe0, 4)); // cputype (arm64)
    put_u32(&mut new, 8, get_u32(exe0, 8)); // cpusubtype (arm64e)
    put_u32(&mut new, 12, MH_FILESET);

    // Lay out the load commands.
    let mut lc: Vec<u8> = Vec::new();
    let seg_lc = |c: &CollSeg, sect: Option<(u64, u64)>| -> Vec<u8> {
        let nsects = c.nsects as usize;
        let len = 72 + 80 * nsects;
        let mut b = vec![0u8; len];
        put_u32(&mut b, 0, LC_SEGMENT_64);
        put_u32(&mut b, 4, len as u32);
        b[8..8 + c.name.len()].copy_from_slice(c.name.as_bytes());
        put_u64(&mut b, 24, c.fileoff); // vmaddr (unslid, base zero)
        put_u64(&mut b, 32, c.filesize); // vmsize
        put_u64(&mut b, 40, c.fileoff); // fileoff
        put_u64(&mut b, 48, c.filesize); // filesize
        put_u32(&mut b, 56, c.initprot); // maxprot
        put_u32(&mut b, 60, c.initprot); // initprot
        put_u32(&mut b, 64, c.nsects);
        if let Some((secoff, secsize)) = sect {
            let so = 72;
            b[so..so + 6].copy_from_slice(b"__info");
            b[so + 16..so + 16 + c.name.len()].copy_from_slice(c.name.as_bytes());
            put_u64(&mut b, so + 32, secoff); // addr
            put_u64(&mut b, so + 40, secsize); // size
            put_u32(&mut b, so + 48, secoff as u32); // offset
        }
        b
    };
    for c in &coll {
        let sect = if c.nsects == 1 {
            Some((c.fileoff, c.filesize))
        } else {
            None
        };
        lc.extend_from_slice(&seg_lc(c, sect));
    }
    // One LC_FILESET_ENTRY per kext: its mach header sits at the kext's placed
    // __TEXT content offset (base-zero, so vmaddr == fileoff).
    for pk in &placed {
        let (va, fo) = place_off(&pk.p, 0)
            .ok_or_else(|| kext_error(pk.id, KextError::Place(NO_HEADER_SEGMENT)))?;
        let name = pk.id.as_bytes();
        let sz = (32 + name.len() + 1).next_multiple_of(8);
        let mut b = vec![0u8; sz];
        put_u32(&mut b, 0, LC_FILESET_ENTRY);
        put_u32(&mut b, 4, sz as u32);
        put_u64(&mut b, 8, va); // vmaddr (unslid) of the kext header
        put_u64(&mut b, 16, fo); // fileoff of the kext header
        put_u32(&mut b, 24, 32); // entry_id offset
        b[32..32 + name.len()].copy_from_slice(name);
        lc.extend_from_slice(&b);
    }
    // LC_DYLD_CHAINED_FIXUPS: the shared chained-fixups table, in __LINKEDIT.
    let cf_size = cf_blob.len() as u64;
    {
        let mut b = vec![0u8; 16];
        put_u32(&mut b, 0, LC_DYLD_CHAINED_FIXUPS);
        put_u32(&mut b, 4, 16);
        put_u32(&mut b, 8, le_off as u32); // dataoff
        put_u32(&mut b, 12, cf_size as u32); // datasize
        lc.extend_from_slice(&b);
    }
    // LC_SYMTAB: empty, after the chained-fixups blob in __LINKEDIT.
    {
        let mut b = vec![0u8; 24];
        put_u32(&mut b, 0, LC_SYMTAB);
        put_u32(&mut b, 4, 24);
        put_u32(&mut b, 8, (le_off + cf_size) as u32); // symoff
        put_u32(&mut b, 12, 0); // nsyms
        put_u32(&mut b, 16, (le_off + cf_size) as u32); // stroff
        put_u32(&mut b, 20, 1); // strsize
        lc.extend_from_slice(&b);
    }
    // The collection's own identity, distinct from the Boot KC's. The kernel
    // reads it both from the header's LC_UUID and, when it registers the AuxKC
    // for deferred kext processing, from the prelink info's _PrelinkKCID (as a
    // 16-byte OSData); the two must agree or the registration rejects the KC
    // ("The Aux KC has no UUID in _PrelinkKCID").
    let aux_uuid = {
        let mut u = *boot_uuid;
        u[0] ^= 0xa5; // derive a different uuid
        u
    };
    // LC_UUID.
    {
        let mut b = vec![0u8; 24];
        put_u32(&mut b, 0, LC_UUID);
        put_u32(&mut b, 4, 24);
        b[8..24].copy_from_slice(&aux_uuid);
        lc.extend_from_slice(&b);
    }

    let ncmds = coll.len() as u32 + placed.len() as u32 /* fileset per kext */
        + 2 /* chained, symtab */ + 1 /* uuid */;
    put_u32(&mut new, 16, ncmds);
    put_u32(&mut new, 20, lc.len() as u32);
    if 32 + lc.len() > PAGE as usize {
        return Err(Error::NoHeaderRoom);
    }
    new[32..32 + lc.len()].copy_from_slice(&lc);

    // Write the shared chained-fixups table into __LINKEDIT.
    new[le_off as usize..le_off as usize + cf_blob.len()].copy_from_slice(&cf_blob);

    // Place each kext: copy its segments, re-encode its adrps, write kmod_info,
    // and rewrite its sub-header.
    for pk in &placed {
        place_one(&mut new, &pk.p, &pk.k, pk.exe).map_err(|e| kext_error(pk.id, e))?;
    }

    // Write each placed pointer slot's raw kernel-cache rebase word, threaded
    // into the chain, for the kernel's deferred loader to rebase and sign.
    for &(pos, word) in &cf_slots {
        put_u64(&mut new, pos as usize, word);
    }

    // The deferred AuxKC loader reads the prelink info's `_PrelinkKmodInfo` and
    // the load addresses as zero-based offsets and adds the collection's load
    // address (`aux_va`) itself to reach the runtime pointers: it computes
    // `kmod_info = base + _PrelinkKmodInfo`, so an absolute value would be
    // added twice and wrap. `place_one` already wrote `kmod_info.address` as
    // the zero-based link address, which is what the loader wants, so leave it.
    // Build one prelink dict per kext with its zero-based load and kmod offsets.
    let dicts: Vec<String> = placed
        .iter()
        .map(|pk| {
            prelink_dict(pk.info, pk.p.load_addr, pk.p.code_size, pk.p.kmod_addr)
                .map_err(|e| kext_error(pk.id, e))
        })
        .collect::<Result<_, _>>()?;
    let bootkcid = base64(boot_uuid);
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\
<plist version=\"1.0\"><dict>\
<key>_BootKCID</key><data>{bootkcid}</data>\
<key>_PrelinkKCID</key><data>{prelinkkcid}</data>\
<key>_PrelinkInfoDictionary</key><array></array>\
</dict></plist>\0",
        prelinkkcid = base64(&aux_uuid),
    );
    let mut plist = vec![0u8; pi_size as usize];
    if xml.len() > plist.len() {
        return Err(Error::NoPrelinkArray);
    }
    plist[..xml.len()].copy_from_slice(xml.as_bytes());
    insert_prelink(&mut plist, (0, pi_size), &dicts)?;
    new[pi_off as usize..(pi_off + pi_size) as usize].copy_from_slice(&plist);

    log(format_args!(
        "AuxKC: {} bytes, {} kext(s), {} fixups, BootKCID <data>{bootkcid}</data>",
        new.len(),
        placed.len(),
        cf_slots.len(),
    ));
    for pk in &placed {
        log(format_args!(
            "AuxKC: kext {} at {:#x} (slid {:#x}), code {:#x}, kmod {:#x}",
            pk.id,
            pk.p.load_addr,
            pk.p.load_addr + slide,
            pk.p.code_size,
            pk.p.kmod_addr,
        ));
    }
    Ok(new)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bind resolves against the Boot KC first (cacheLevel 0, offset from the
    /// Boot KC base), then against a sibling placed kext's exports (cacheLevel 3,
    /// the export's collection address), and errors when neither has it.
    #[test]
    fn bind_target_boot_then_sibling_exports() {
        let boot_lo = 0xffff_fe00_0700_0000;
        let mut symbols = BTreeMap::new();
        symbols.insert("_IOLog".to_string(), boot_lo + 0x1234);
        // A name the kernel also defines wins over a sibling export of the same
        // name, keeping the Boot-KC KPI resolution unchanged.
        symbols.insert("_shared".to_string(), boot_lo + 0x40);
        let export = "_ai_aneesiqbal_ambrosia_test_export";
        let mut exports = BTreeMap::new();
        exports.insert(export.to_string(), 0x8764u64);
        exports.insert("_shared".to_string(), 0x99u64);

        // Boot-KC symbol -> (offset from boot_lo, level 0).
        assert_eq!(
            bind_target("_IOLog", &symbols, &exports, boot_lo).unwrap(),
            (0x1234, KC_LEVEL_BOOT)
        );
        // Sibling export -> (collection address, level 3).
        assert_eq!(
            bind_target(export, &symbols, &exports, boot_lo).unwrap(),
            (0x8764, KC_LEVEL_AUX)
        );
        // Boot KC wins a name collision.
        assert_eq!(
            bind_target("_shared", &symbols, &exports, boot_lo).unwrap(),
            (0x40, KC_LEVEL_BOOT)
        );
        // Neither has it.
        assert!(matches!(
            bind_target("_missing", &symbols, &exports, boot_lo),
            Err(KextError::NoSymbol(_))
        ));
        // A kernel symbol below the Boot KC base is rejected, not wrapped.
        symbols.insert("_below".to_string(), boot_lo - 8);
        assert!(matches!(
            bind_target("_below", &symbols, &exports, boot_lo),
            Err(KextError::Place(_))
        ));
        // Targets must fit the 30-bit kernel-cache target field.
        exports.insert("_far".to_string(), 1 << 30);
        assert!(matches!(
            bind_target("_far", &symbols, &exports, boot_lo),
            Err(KextError::Place(_))
        ));
    }

    /// A minimal `MH_KEXT_BUNDLE` with a `__TEXT` and a `__DATA` whose `vmsize`
    /// exceeds its `filesize` by `data_bss` (a zerofill tail), no fixups and no
    /// imports, and a `_kmod_info` symbol in `__DATA`. Enough for `size` and
    /// `build` to lay it out.
    fn synth_kext(data_bss: u64) -> Vec<u8> {
        const SEG: usize = 72;
        let data_fs: u64 = 0x4000;
        let data_vs = data_fs + data_bss;
        // __LINKEDIT content: chained-fixups blob (no starts), one nlist, strtab.
        let le_off: u64 = 0x8000;
        let chain_len: u64 = 0x28; // 0x20 header area + seg_count(1) + offset(0)
        let sym_off = le_off + chain_len;
        let str_off = sym_off + 16;
        let strtab: &[u8] = b"\0_kmod_info\0";
        let le_size = chain_len + 16 + strtab.len() as u64;
        let total = (le_off + le_size) as usize;
        let mut d = vec![0u8; total];

        put_u32(&mut d, 0, 0xfeed_facf); // MH_MAGIC_64
        put_u32(&mut d, 4, 0x0100_000c); // cputype arm64
        put_u32(&mut d, 8, 0x8000_0002); // cpusubtype arm64e
        put_u32(&mut d, 12, 11); // MH_KEXT_BUNDLE
        put_u32(&mut d, 16, 5); // ncmds
        put_u32(&mut d, 20, (3 * SEG + 24 + 16) as u32); // sizeofcmds

        let mut seg = |at: usize, name: &str, va: u64, vs: u64, fo: u64, fs: u64, prot: u32| {
            put_u32(&mut d, at, LC_SEGMENT_64);
            put_u32(&mut d, at + 4, SEG as u32);
            d[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
            put_u64(&mut d, at + 24, va);
            put_u64(&mut d, at + 32, vs);
            put_u64(&mut d, at + 40, fo);
            put_u64(&mut d, at + 48, fs);
            put_u32(&mut d, at + 56, prot);
            put_u32(&mut d, at + 60, prot);
        };
        let mut at = 32;
        // vmaddr == fileoff at each segment's start; __DATA's bss tail pushes
        // __LINKEDIT's vmaddr past its fileoff, as a real kext's does.
        seg(at, "__TEXT", 0, 0x4000, 0, 0x4000, 5);
        at += SEG;
        seg(at, "__DATA", 0x4000, data_vs, 0x4000, data_fs, 3);
        at += SEG;
        seg(
            at,
            "__LINKEDIT",
            0x4000 + data_vs,
            0x4000,
            le_off,
            le_size,
            1,
        );
        at += SEG;
        // LC_SYMTAB
        put_u32(&mut d, at, LC_SYMTAB);
        put_u32(&mut d, at + 4, 24);
        put_u32(&mut d, at + 8, sym_off as u32); // symoff
        put_u32(&mut d, at + 12, 1); // nsyms
        put_u32(&mut d, at + 16, str_off as u32); // stroff
        put_u32(&mut d, at + 20, strtab.len() as u32);
        at += 24;
        // LC_DYLD_CHAINED_FIXUPS
        put_u32(&mut d, at, LC_DYLD_CHAINED_FIXUPS);
        put_u32(&mut d, at + 4, 16);
        put_u32(&mut d, at + 8, le_off as u32); // dataoff
        put_u32(&mut d, at + 12, chain_len as u32); // datasize

        // Chained-fixups header: starts at +0x20, no imports.
        let c = le_off as usize;
        put_u32(&mut d, c, 1); // fixups_version
        put_u32(&mut d, c + 4, 0x20); // starts_offset
        put_u32(&mut d, c + 8, 0x1c); // imports_offset
        put_u32(&mut d, c + 12, 0x1c); // symbols_offset
        put_u32(&mut d, c + 16, 0); // imports_count
        put_u32(&mut d, c + 0x20, 1); // seg_count
        put_u32(&mut d, c + 0x24, 0); // seg_info_offset[0] = 0 (no starts)

        // One nlist: _kmod_info at strx 1, value 0x4000 (its vmaddr in __DATA).
        let s = sym_off as usize;
        put_u32(&mut d, s, 1); // n_strx
        d[s + 4] = 0x0e; // n_type N_SECT (not external)
        d[s + 5] = 2; // n_sect __DATA
        put_u64(&mut d, s + 8, 0x4000); // n_value
        d[str_off as usize..str_off as usize + strtab.len()].copy_from_slice(strtab);
        d
    }

    /// The reserve `size` computes must equal the collection `build` emits, so
    /// the placed AuxKC fits its reservation, including for a kext whose
    /// `__DATA` has a zerofill tail: both count the segment's `vmsize`, not its
    /// `filesize`.
    #[test]
    fn size_matches_build_with_a_zerofill_tail() {
        let exe = synth_kext(0x4000); // __DATA vmsize 0x8000, filesize 0x4000
        let info =
            b"<plist><dict><key>CFBundleIdentifier</key><string>ai.aneesiqbal.ambrosia.test.z</string></dict></plist>";
        let kexts = vec![KextInput {
            id: "ai.aneesiqbal.ambrosia.test.z",
            info,
            exe: &exe,
        }];
        let built = build(
            &kexts,
            0xffff_fe00_0600_0000,
            0xffff_fe00_0700_0000,
            &[0u8; 16],
            &BTreeMap::new(),
            &mut |_a| {},
        )
        .unwrap();
        assert_eq!(size(&kexts).unwrap(), built.len() as u64);

        // The reserve tracks the bss: a larger zerofill tail reserves more.
        let bigger_exe = synth_kext(0x8000);
        let bigger = vec![KextInput {
            id: "ai.aneesiqbal.ambrosia.test.z",
            info,
            exe: &bigger_exe,
        }];
        assert!(size(&bigger).unwrap() > size(&kexts).unwrap());
    }

    /// A kext whose segments or symbols don't fit its file is refused with an
    /// error, not a panic.
    #[test]
    fn malformed_kexts() {
        let good = synth_kext(0);
        assert!(kext::Kext::parse(&good).is_ok());
        let edit = |at: usize, v: &[u8]| {
            let mut k = good.clone();
            k[at..at + v.len()].copy_from_slice(v);
            kext::Kext::parse(&k).map(|_| ()).unwrap_err()
        };
        // __DATA's load command is the second, at 104.
        let (vmsize, fileoff) = (104 + 32, 104 + 40);
        for (at, v, want) in [
            // Its bytes past the file's end.
            (fileoff, 0x10_0000u64, kext::Error::BadSegment),
            // Its file size past its memory size.
            (vmsize, 0x1000, kext::Error::BadSegment),
            // Its memory past the end of the address space.
            (vmsize, u64::MAX, kext::Error::BadSegment),
        ] {
            assert_eq!(edit(at, &v.to_le_bytes()), want, "{at} {v:#x}");
        }
        // The one symbol's name past the file's end.
        assert_eq!(
            edit(0x8028, &0x10_0000u32.to_le_bytes()),
            kext::Error::Truncated
        );
    }

    /// A `kmod_info` whose address and size fields would run past the end of
    /// the segment that holds it is refused, not written past that segment.
    #[test]
    fn kmod_info_past_its_segment() {
        let id = "ai.aneesiqbal.ambrosia.test.k";
        let info = b"<plist><dict><key>CFBundleIdentifier</key>\
                     <string>ai.aneesiqbal.ambrosia.test.k</string></dict></plist>";
        // `_kmod_info`'s value, in synth_kext's one nlist, at 0x8030: in
        // __DATA, which runs from 0x4000 to 0x8000.
        let build_at = |kmod_info: u64| {
            let mut exe = synth_kext(0);
            exe[0x8030..0x8038].copy_from_slice(&kmod_info.to_le_bytes());
            let kexts = [KextInput {
                id,
                info,
                exe: &exe,
            }];
            build(&kexts, 0, 0, &[0; 16], &BTreeMap::new(), &mut |_| {})
        };
        // Its fields end at +0xac, so the last place it fits is 0x8000 - 0xac.
        assert!(build_at(0x8000 - 0xac).is_ok());
        let e = build_at(0x8000 - 0xab).unwrap_err();
        assert!(
            matches!(&e, Error::Kext(k, KextError::Place(_)) if k == id),
            "{e:?}"
        );
        assert_eq!(
            e.to_string(),
            "kext ai.aneesiqbal.ambrosia.test.k: its _kmod_info is not wholly inside a segment \
             Ambrosia places"
        );
    }

    /// `size` refuses a kext that `build` refuses for its own layout, with the
    /// same error, so a check that sizes the layout finds it before the boot.
    #[test]
    fn size_refuses_what_build_refuses() {
        let id = "ai.aneesiqbal.ambrosia.test.s";
        let info = b"<plist><dict><key>CFBundleIdentifier</key>\
                     <string>ai.aneesiqbal.ambrosia.test.s</string></dict></plist>";
        let refused = |edit: fn(&mut Vec<u8>)| {
            let mut exe = synth_kext(0);
            edit(&mut exe);
            let kexts = [KextInput {
                id,
                info,
                exe: &exe,
            }];
            let e = build(&kexts, 0, 0, &[0; 16], &BTreeMap::new(), &mut |_| {}).unwrap_err();
            assert_eq!(size(&kexts).unwrap_err(), e);
            e
        };
        // __DATA, whose load command is the second, at 104, renamed.
        assert_eq!(
            refused(|d| d[104 + 8..104 + 14].copy_from_slice(b"__KLD\0")),
            Error::Kext(id.into(), KextError::Segment("__KLD".into()))
        );
        // `_kmod_info`, at 0x8030, 8 bytes before __DATA's end.
        assert_eq!(
            refused(|d| d[0x8030..0x8038].copy_from_slice(&0x7ff8u64.to_le_bytes())),
            Error::Kext(id.into(), KMOD_INFO_OUTSIDE)
        );
    }

    /// Reconstructs the AuxKC to a file for offline inspection with otool. Run
    /// with: AMBROSIA_AUXKC_KEXT=.../Hello.kext/Contents/MacOS/Hello
    /// AMBROSIA_AUXKC_INFO=.../Hello.kext/Contents/Info.plist
    /// AMBROSIA_AUXKC_OUT=/tmp/auxkc.bin cargo test -p ambrosia auxkc_dump -- --ignored --nocapture
    /// (examples/hello-kext/build.sh builds Hello.kext).
    #[test]
    #[ignore]
    fn auxkc_dump() {
        let exe = std::fs::read(std::env::var("AMBROSIA_AUXKC_KEXT").unwrap()).unwrap();
        let info = std::fs::read(std::env::var("AMBROSIA_AUXKC_INFO").unwrap()).unwrap();
        let out = std::env::var("AMBROSIA_AUXKC_OUT").unwrap();
        // An optional second kext, to inspect a multi-kext collection.
        let exe2 = std::env::var("AMBROSIA_AUXKC_KEXT2")
            .ok()
            .map(|p| std::fs::read(p).unwrap());
        let info2 = std::env::var("AMBROSIA_AUXKC_INFO2")
            .ok()
            .map(|p| std::fs::read(p).unwrap());

        let mut symbols = BTreeMap::new();
        for kbytes in [Some(&exe), exe2.as_ref()].into_iter().flatten() {
            let k = kext::Kext::parse(kbytes).unwrap();
            for (i, imp) in k.imports.iter().enumerate() {
                symbols
                    .entry(imp.clone())
                    .or_insert(0xffff_fe00_0700_0000 + i as u64 * 8);
            }
        }
        let boot_uuid = [0x11u8; 16];
        let slide = 0xffff_fe00_0600_0000;
        // The fake kernel symbols sit just above this base, so their Boot-KC
        // offsets are small and positive.
        let boot_lo = 0xffff_fe00_0700_0000;
        let mut kexts = vec![KextInput {
            id: "ai.aneesiqbal.ambrosia.example.hello",
            info: &info,
            exe: &exe,
        }];
        if let (Some(e2), Some(i2)) = (exe2.as_ref(), info2.as_ref()) {
            kexts.push(KextInput {
                id: "ai.aneesiqbal.ambrosia.example.hello2",
                info: i2,
                exe: e2,
            });
        }
        let aux = build(&kexts, slide, boot_lo, &boot_uuid, &symbols, &mut |a| {
            std::eprintln!("{a}");
        })
        .unwrap();
        std::fs::write(&out, &aux).unwrap();
        std::eprintln!("wrote {} bytes to {out}", aux.len());
    }
}
