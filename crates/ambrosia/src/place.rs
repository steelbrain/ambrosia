//! Placing a parsed kext into a kernel collection: the segment-placement
//! machinery [`crate::auxkc`] uses to lay a kext's image where the collection
//! maps it and rewrite its sub-header.
//!
//! A [`Placement`] records where each of a kext's mapped segments lands: its
//! file position and virtual address in the collection. [`place_off`] maps a
//! kext-relative offset through it, and [`place_one`] does the placement:
//! copies the segments, re-encodes the cross-segment `adrp`+`add`/`ldr` pairs
//! the compiler computed for the kext's own layout, writes `kmod_info`, and
//! rewrites the sub-header to the placed addresses (docs/boot-process.md,
//! "Placing the kexts"). The kext's pointers are the collection's chained
//! fixups, which [`crate::auxkc`] writes and the kernel applies.

use crate::boot::KextError;
use crate::kext;
use alloc::vec::Vec;

/// `MH_DYLIB_IN_CACHE`: the Mach-O is part of a shared cache or kernel
/// collection, so the kernel slides its segments in place instead of
/// relocating it as a standalone image.
const MH_DYLIB_IN_CACHE: u32 = 0x8000_0000;

const LC_SEGMENT_64: u32 = 0x19;
const LC_SYMTAB: u32 = 0x2;
const LC_DYSYMTAB: u32 = 0xb;
const LC_DYLD_CHAINED_FIXUPS: u32 = 0x8000_0034;
const LC_FUNCTION_STARTS: u32 = 0x26;
const LC_DATA_IN_CODE: u32 = 0x29;
const LC_SEGMENT_SPLIT_INFO: u32 = 0x1e;
const LC_DYLD_EXPORTS_TRIE: u32 = 0x8000_0033;
const LC_CODE_SIGNATURE: u32 = 0x1d;

/// The offsets in the packed arm64e `kmod_info` of its `address` and `size`
/// fields, which placing a kext writes, and the end of `size`.
const KMOD_ADDRESS: u64 = 0x9c;
const KMOD_SIZE: u64 = 0xa4;
/// The bytes of a kext's `kmod_info` that must lie in one of its placed
/// segments: up to the end of its `size` field.
pub(crate) const KMOD_INFO_FIELDS: u64 = KMOD_SIZE + 8;

/// Why a kext can't be placed whose load commands run past the copy of its
/// segment at file offset 0, which [`crate::kext::Kext::parse`] rules out.
const PAST_HEADER: KextError = KextError::Exe(kext::Error::HeaderOutside);
/// Why a kext can't be placed one of whose load commands is too short for
/// the fields it rewrites, which [`crate::kext::Kext::parse`] rules out.
const SHORT: KextError = KextError::Exe(kext::Error::ShortCommand);
/// Why a kext can't be placed whose `kmod_info` fields don't lie in the
/// segment that holds it.
pub(crate) const KMOD_INFO_OUTSIDE: KextError =
    KextError::Place("its _kmod_info is not wholly inside a segment Ambrosia places");
/// Why a kext can't be placed one of whose segments runs past the
/// executable, or past its place in the collection, which
/// [`crate::kext::Kext::parse`] and the layout rule out.
const SEGMENT_OUTSIDE: KextError = KextError::Place(
    "a segment of it runs past the end of the executable or of its place in the Auxiliary KC",
);

fn get_u32(d: &[u8], at: usize) -> Option<u32> {
    d.get(at..)?.first_chunk().copied().map(u32::from_le_bytes)
}
fn get_u64(d: &[u8], at: usize) -> Option<u64> {
    d.get(at..)?.first_chunk().copied().map(u64::from_le_bytes)
}
fn put_u32(d: &mut [u8], at: usize, v: u32) -> Option<()> {
    *d.get_mut(at..)?.first_chunk_mut()? = v.to_le_bytes();
    Some(())
}
fn put_u64(d: &mut [u8], at: usize, v: u64) -> Option<()> {
    *d.get_mut(at..)?.first_chunk_mut()? = v.to_le_bytes();
    Some(())
}

/// The `len` bytes at `at` in `d`, if `d` holds them.
fn span_mut(d: &mut [u8], at: u64, len: u64) -> Option<&mut [u8]> {
    let at = usize::try_from(at).ok()?;
    d.get_mut(at..at.checked_add(usize::try_from(len).ok()?)?)
}

/// Where one of a kext's mapped segments lands in the collection. A fixup
/// offset — a slot location or a rebase target — is an offset from the kext's
/// base, matched against `[fileoff, fileoff + vmsize)`: `vmsize`, not
/// `filesize`, so a pointer into a segment's zerofill tail (bss, which the file
/// does not hold) still resolves. The collection reserves `vmsize`, so that
/// tail is real, zeroed space.
pub(crate) struct Seg {
    /// Original file offset in the kext; equals the segment's base-relative
    /// vmaddr, which is what a fixup offset into this segment carries.
    pub(crate) fileoff: u64,
    /// Original file size: how many bytes `place_one` copies.
    pub(crate) filesize: u64,
    /// Original VM size: the segment's extent, covering any zerofill tail.
    pub(crate) vmsize: u64,
    /// Placed file position in the collection.
    pub(crate) pos: u64,
    /// Placed vmaddr (unslid) in the collection.
    pub(crate) va: u64,
}

/// A kext's placement: each of its mapped segments, at its new address and file
/// position in the collection.
pub(crate) struct Placement {
    pub(crate) segs: Vec<Seg>,
    /// The kext's base (its `__TEXT`'s placed vmaddr).
    pub(crate) load_addr: u64,
    /// The span of the kext's placed segments, for the start-function range
    /// check and for `kmod_info.size`.
    pub(crate) code_size: u64,
    /// The placed vmaddr of the kext's `kmod_info`.
    pub(crate) kmod_addr: u64,
}

/// Where a kext-relative offset `o` lands, given a placement: the placed vmaddr
/// and the file position in the new collection.
pub(crate) fn place_off(p: &Placement, o: u64) -> Option<(u64, u64)> {
    p.segs.iter().find_map(|s| {
        if o >= s.fileoff && o - s.fileoff < s.vmsize {
            Some((s.va + (o - s.fileoff), s.pos + (o - s.fileoff)))
        } else {
            None
        }
    })
}

/// Where the `len` bytes at kext-relative offset `o` land, as their file
/// position in the new collection, if they lie in one placed segment.
pub(crate) fn place_span(p: &Placement, o: u64, len: u64) -> Option<u64> {
    p.segs.iter().find_map(|s| {
        let into = o.checked_sub(s.fileoff)?;
        (into.checked_add(len)? <= s.vmsize).then_some(s.pos + into)
    })
}

/// Copies one kext's segments into their placed positions, re-encodes its
/// `adrp`s, writes its `kmod_info`, and rewrites its sub-header to the placed
/// addresses. Its pointer slots are left for [`crate::auxkc`] to fill with
/// the collection's chained-fixup words.
pub(crate) fn place_one(
    new: &mut [u8],
    p: &Placement,
    k: &kext::Kext<'_>,
    exe: &[u8],
) -> Result<(), KextError> {
    // Copy each mapped segment's bytes to its placed file position.
    for s in &p.segs {
        let from = usize::try_from(s.fileoff)
            .ok()
            .and_then(|at| exe.get(at..at.checked_add(s.filesize as usize)?))
            .ok_or(SEGMENT_OUTSIDE)?;
        span_mut(new, s.pos, s.filesize)
            .ok_or(SEGMENT_OUTSIDE)?
            .copy_from_slice(from);
    }
    // Re-encode the kext's cross-segment `adrp`+`add`/`ldr` pairs: the segments
    // land at new relative distances, so a page reference the compiler computed
    // for the kext's own layout no longer reaches.
    relink_adrp(new, p, k)?;
    // kmod_info.address and .size describe the kext's code to the kernel. A
    // prelinked kext carries its unslid link address here; the kernel slides
    // it. The fields are at +0x9c (address) and +0xa4 (size) in the packed
    // arm64e kmod_info, which must lie in the segment that holds it, in the
    // space the collection reserves for that segment.
    let kmod_info = place_span(p, k.kmod_info, KMOD_INFO_FIELDS)
        .and_then(|at| span_mut(new, at, KMOD_INFO_FIELDS))
        .ok_or(KMOD_INFO_OUTSIDE)?;
    put_u64(kmod_info, KMOD_ADDRESS as usize, p.load_addr).ok_or(KMOD_INFO_OUTSIDE)?;
    put_u64(kmod_info, KMOD_SIZE as usize, p.code_size).ok_or(KMOD_INFO_OUTSIDE)?;

    // Rewrite the kext's sub-header to the placed addresses, and empty the
    // tables the kernel's per-entry slide pass would otherwise walk through
    // the kext's own (unplaced) __LINKEDIT (a near-null base, a data abort).
    rewrite_sub_header(new, p)?;
    Ok(())
}

/// Re-encodes the `adrp` page immediates in the kext's executable segments for
/// the placed layout. Coalescing moves a kext's segments to new relative
/// positions, so a page-relative `adrp` the compiler computed for the kext's
/// own layout no longer reaches its target.
///
/// An `adrp` loads a 4 KiB-aligned page into a register; a later `add`/`ldr`/
/// `str` adds the in-page offset. Only the page needs re-encoding: it maps
/// through the placement to its collection page, and the collection's load
/// slide then cancels (both sides are page-relative). The companion offset is
/// left alone — every segment lands 16 KiB-aligned, so an address's low 12 bits
/// are preserved and the companion still reaches. This needs no pairing of the
/// `adrp` with its companion, which a compiler may schedule far away or past a
/// branch; pointer references, by contrast, are the chained fixups, applied
/// separately. An `adrp` whose page is not in one of the kext's placed segments
/// is left unchanged.
fn relink_adrp(new: &mut [u8], p: &Placement, k: &kext::Kext<'_>) -> Result<(), KextError> {
    for seg in k
        .segments
        .iter()
        .filter(|s| s.initprot & 4 != 0 && s.filesize > 0)
    {
        let (base_va, base_pos) = place_off(p, seg.vmaddr).ok_or(KextError::Place(
            "an executable segment of it is not one Ambrosia places",
        ))?;
        let code = span_mut(new, base_pos, seg.filesize).ok_or(SEGMENT_OUTSIDE)?;
        for (i, word) in code.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let i = 4 * i as u64;
            let w = u32::from_le_bytes(*word);
            // ADRP (bit 31 set); ADR (bit 31 clear) is not page-relative.
            if w & 0x9f00_0000 == 0x9000_0000 {
                let imm = (((w >> 5) & 0x7ffff) << 2) | ((w >> 29) & 3);
                let imm = i64::from((imm as i32) << 11 >> 11); // sign-extend 21 bits
                let pc = seg.vmaddr.wrapping_add(i);
                let page = (pc & !0xfff).wrapping_add((imm << 12) as u64);
                // The page's collection address; a page outside the kext's
                // placed segments is left as is.
                if let Some((new_page, _)) = place_off(p, page) {
                    let new_pc = base_va + i;
                    let delta = ((new_page & !0xfff).wrapping_sub(new_pc & !0xfff) as i64) >> 12;
                    let adrp = (w & !((3 << 29) | (0x7ffff << 5)))
                        | (((delta & 3) as u32) << 29)
                        | ((((delta >> 2) & 0x7ffff) as u32) << 5);
                    *word = adrp.to_le_bytes();
                }
            }
        }
    }
    Ok(())
}

/// Rewrites a placed kext's sub-header (at its `__TEXT`'s placed position) so
/// its segment addresses and file offsets are the placed ones, and empties the
/// symbol and linkedit tables (the kext is resolved already; the kernel finds
/// it by its fileset entry and prelink dict).
///
/// The sub-header is marked `MH_DYLIB_IN_CACHE` and its segment addresses are
/// written *unslid* (the placed link addresses), exactly as `kcgen` leaves a
/// kext it coalesces into a collection. That flag tells the kernel the Mach-O
/// is already in the collection: it adds the collection's slide to each segment
/// address in place, so at run time they match the slid, signed
/// `kmod_info.start` the kernel validates against. Without the flag the kernel
/// relocates the kext as standalone and the addresses come out wrong (zero),
/// and the start-function range check fails. File offsets, which the slide does
/// not touch, stay as placed.
fn rewrite_sub_header(new: &mut [u8], p: &Placement) -> Result<(), KextError> {
    // The kext's mach header is at file offset 0, in its __TEXT segment; find
    // that segment's placed file position (the segments are not placed in the
    // kext's own order, since data may be laid down before code). Its load
    // commands follow it in that segment's copy, `h`, and every read and
    // write below stays in it.
    let text = p
        .segs
        .iter()
        .find(|s| s.fileoff == 0)
        .ok_or(KextError::Place("it has no segment at file offset 0"))?;
    let base = text.pos;
    let h = span_mut(new, base, text.filesize).ok_or(SEGMENT_OUTSIDE)?;
    // Mark the Mach-O as already in the collection (MH_DYLIB_IN_CACHE), so the
    // kernel slides its segment addresses rather than relocating it.
    let flags = get_u32(h, 24).ok_or(PAST_HEADER)?;
    put_u32(h, 24, flags | MH_DYLIB_IN_CACHE).ok_or(PAST_HEADER)?;
    let ncmds = get_u32(h, 16).ok_or(PAST_HEADER)?;
    let mut at = 32usize;
    for _ in 0..ncmds {
        let cmd = get_u32(h, at).ok_or(PAST_HEADER)?;
        let size = get_u32(h, at + 4).ok_or(PAST_HEADER)? as usize;
        if size < 8 {
            return Err(SHORT);
        }
        // The command, which `Kext::parse` has checked holds its fields.
        let c = at
            .checked_add(size)
            .and_then(|end| h.get_mut(at..end))
            .ok_or(PAST_HEADER)?;
        match cmd {
            LC_SEGMENT_64 => {
                // Match the segment to its placement by its original file
                // offset; a segment we did not place (empty, or __LINKEDIT)
                // collapses to the kext's base so it stays inside the extent.
                let fileoff = get_u64(c, 40).ok_or(SHORT)?;
                let placed = p.segs.iter().find(|s| s.fileoff == fileoff);
                let (pos, va) = placed.map_or((base, p.load_addr), |s| (s.pos, s.va));
                put_u64(c, 24, va).ok_or(SHORT)?; // vmaddr (unslid; the kernel slides it)
                put_u64(c, 40, pos).ok_or(SHORT)?; // fileoff
                if placed.is_none() {
                    put_u64(c, 32, 0).ok_or(SHORT)?; // vmsize
                    put_u64(c, 48, 0).ok_or(SHORT)?; // filesize
                }
                let nsects = get_u32(c, 64).ok_or(SHORT)?;
                for i in 0..nsects as usize {
                    let so = 72 + 80 * i;
                    let secoff = get_u64(c, so + 32).ok_or(SHORT)?.wrapping_sub(fileoff);
                    // addr (unslid), and offset
                    put_u64(c, so + 32, va.wrapping_add(secoff)).ok_or(SHORT)?;
                    let offset = match placed {
                        Some(_) => pos.wrapping_add(secoff) as u32,
                        None => 0,
                    };
                    put_u32(c, so + 48, offset).ok_or(SHORT)?;
                }
            }
            LC_SYMTAB => {
                for fld in [8, 12, 16, 20] {
                    put_u32(c, fld, 0).ok_or(SHORT)?;
                }
            }
            LC_DYSYMTAB => c.get_mut(8..).ok_or(SHORT)?.fill(0),
            LC_FUNCTION_STARTS
            | LC_DATA_IN_CODE
            | LC_SEGMENT_SPLIT_INFO
            | LC_DYLD_EXPORTS_TRIE
            | LC_CODE_SIGNATURE => {
                put_u32(c, 8, 0).ok_or(SHORT)?; // dataoff
                put_u32(c, 12, 0).ok_or(SHORT)?; // datasize
            }
            LC_DYLD_CHAINED_FIXUPS => put_u32(c, 0, 0).ok_or(SHORT)?, // neutralize
            _ => {}
        }
        at += size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// A fixup offset into a segment's zerofill tail, past `filesize` but
    /// within `vmsize`, maps through the placement, because the collection
    /// reserves the segment's full virtual size: a rebase from a kext's data
    /// into its own bss resolves.
    #[test]
    fn place_off_reaches_zerofill_tail() {
        let p = Placement {
            segs: vec![Seg {
                fileoff: 0x28000,
                filesize: 0x10000,
                vmsize: 0x1c000,
                pos: 0x1000,
                va: 0x5000,
            }],
            load_addr: 0x5000,
            code_size: 0x1c000,
            kmod_addr: 0x5000,
        };
        // The initialized part maps.
        assert_eq!(place_off(&p, 0x28000), Some((0x5000, 0x1000)));
        // A target in the bss tail (fileoff + 0x12c78: past filesize 0x10000,
        // within vmsize 0x1c000) maps to the reserved, zeroed space.
        assert_eq!(
            place_off(&p, 0x28000 + 0x12c78),
            Some((0x5000 + 0x12c78, 0x1000 + 0x12c78))
        );
        // The last byte of the segment maps; one past its vmsize does not.
        assert_eq!(
            place_off(&p, 0x28000 + 0x1bfff),
            Some((0x5000 + 0x1bfff, 0x1000 + 0x1bfff))
        );
        assert_eq!(place_off(&p, 0x28000 + 0x1c000), None);
        // Below the segment is out of range.
        assert_eq!(place_off(&p, 0x27fff), None);
    }
}
