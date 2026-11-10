//! The boot, from the files Ambrosia read to what the kernel finds at entry:
//! the kernel collection, trust caches, device tree, manifests and boot
//! arguments placed in RAM, as iBoot places them.
//!
//! The layout, upward from a random page in RAM's first 32 MiB:
//!
//! | what | size |
//! |---|---|
//! | Auxiliary KC, if a kext is added | its own, to 16 KiB |
//! | trust caches | theirs, to 16 KiB |
//! | kernel collection | its span |
//! | SEPFW (empty) | 16 KiB |
//! | device tree | its own, to 16 KiB |
//! | boot arguments | 16 KiB |
//!
//! That is the kernel's static memory, from `physBase`, the start of RAM, to
//! `topOfKernelData`, and it must end below `limit`. The rest of RAM is the
//! kernel's, except the carveouts at its top: the framebuffer, the panic log
//! and the manifests (docs/boot-process.md, "The memory layout").
//!
//! The boot arguments' video fields give the kernel's console the display a
//! graphics driver module set up, if there is one, and otherwise that
//! framebuffer in RAM, which nothing shows. Its carveout stays either way.

use crate::adt::{self, Node};
use crate::bootargs::{self, BootArgs, Video};
use crate::config::{self, Config};
use crate::graphics::Framebuffer;
use crate::img4::{self, Payload};
use crate::kext;
use crate::macho::{self, Header};
use crate::pac;
use crate::sha384::Sha384;
use crate::tree::{self, Machine, Objects, PlacedKext, Placement};
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

const PAGE: u64 = 0x4000;
/// The size of the blocks the kernel maps itself with.
const BLOCK: u64 = 32 << 20;

/// Rounds `n` up to a whole number of 16 KiB pages, or `None` on overflow.
fn page_up(n: u64) -> Option<u64> {
    n.checked_next_multiple_of(PAGE)
}

/// The RAM the boot writes, which the firmware gives.
pub trait Memory {
    /// The bytes at `[pa, pa + len)`, or `None` if they aren't RAM the boot
    /// may write.
    fn bytes(&mut self, pa: u64, len: usize) -> Option<&mut [u8]>;
}

/// The files the boot reads, whole, as the guest's disks hold them.
pub struct Files<'a> {
    /// The UUID of the install's volume group, which names its directories
    /// in both Preboot volumes.
    pub volume_group: &'a str,
    /// The name of the boot directory, as `/<volume_group>/boot/active`
    /// gives it.
    pub boot_dir: &'a str,
    /// `System/Library/Caches/apticket.der`
    pub manifest: &'a [u8],
    /// `System/Library/Caches/com.apple.kernelcaches/kernelcache`
    pub kernel: &'a [u8],
    /// `usr/standalone/firmware/devicetree.img4`
    pub device_tree: &'a [u8],
    /// `usr/standalone/firmware/root_hash.img4`
    pub root_hash: &'a [u8],
    /// `usr/standalone/firmware/base_system_root_hash.img4`
    pub base_root_hash: &'a [u8],
    /// `usr/standalone/firmware/FUD/StaticTrustCache.img4`
    pub static_trust_cache: &'a [u8],
    /// `usr/standalone/firmware/FUD/BaseSystemTrustCache.img4`
    pub base_trust_cache: &'a [u8],
    /// The LocalPolicy, from the system container's Preboot.
    pub policy: &'a [u8],
    /// `SFR/current/apticket.der`, from the system container's Preboot.
    pub sfr_manifest: &'a [u8],
    /// The aux drive's NVRAM region.
    pub nvram_region: &'a [u8],
}

/// What can go wrong while preparing the boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A named IMG4 image could not be parsed or unpacked.
    Image(&'static str, img4::Error),
    /// The device tree template could not be parsed.
    Tree(adt::Error),
    /// Filling the device tree failed.
    Populate(tree::Error),
    /// The kernel collection's Mach-O header could not be parsed.
    Kernel(macho::Error),
    /// The image's IMG4 carries no manifest where one is needed.
    NoManifest(&'static str),
    /// The guest's NVRAM region holds no valid bank, or the device tree
    /// template names no NVRAM region.
    NoNvram,
    /// The layout doesn't fit in RAM, or below the firmware's heap.
    NoRoom,
    /// A patch names an entry the kernel collection doesn't have.
    NoEntry(String),
    /// A kext the configuration names can't be placed: its bundle
    /// identifier, and why.
    Kext(String, KextError),
    /// The AuxKC's load commands, a fileset entry for each kext among them,
    /// don't fit in its header page.
    NoHeaderRoom,
    /// Building the Auxiliary KC failed for no one kext; the string says how.
    Place(&'static str),
    /// The prelink info has no `_PrelinkInfoDictionary` array to add to.
    NoPrelinkArray,
    /// An address or size computation overflowed or underflowed, from a
    /// malformed kernel collection or a RAM layout that doesn't fit. The
    /// string names the computation.
    Overflow(&'static str),
    /// Reading a donor vtable's PAC discriminators for the handover failed.
    Vtdisc(crate::vtdisc::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Image(name, e) => write!(f, "{name}: {e}"),
            Error::Tree(e) => write!(f, "the device tree template doesn't parse: {e}"),
            Error::Populate(e) => write!(f, "{e}"),
            Error::Kernel(e) => write!(f, "the kernel collection can't be read: {e}"),
            Error::NoManifest(name) => write!(f, "{name} carries no manifest"),
            Error::NoNvram => f.write_str(
                "the device tree template names no NVRAM region, or the aux disk's NVRAM \
                 region holds no valid bank",
            ),
            Error::NoRoom => f.write_str(
                "the layout doesn't fit in RAM, or the kexts and trust caches don't fit below \
                 the kernel collection",
            ),
            Error::NoEntry(id) => write!(
                f,
                "Kernel.Patch names {id}, which is not an entry of the kernel collection"
            ),
            Error::Kext(id, e) => write!(f, "kext {id}: {e}"),
            Error::NoHeaderRoom => f.write_str(
                "the Auxiliary KC's load commands, a fileset entry for each kext among them, \
                 don't fit in its 16 KiB header",
            ),
            Error::Place(what) => write!(f, "can't build the Auxiliary KC: {what}"),
            Error::NoPrelinkArray => f.write_str(
                "the prelink info has no _PrelinkInfoDictionary array to add the kexts to",
            ),
            Error::Overflow(what) => write!(f, "an address or size is out of range: {what}"),
            Error::Vtdisc(e) => write!(f, "{e}"),
        }
    }
}

/// Why a kext can't be placed in the Auxiliary KC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KextError {
    /// Its executable is not one Ambrosia reads.
    Exe(kext::Error),
    /// It imports a symbol, named here, that neither the kernel collection
    /// nor the other placed kexts define.
    NoSymbol(String),
    /// It has a segment with contents, named here, that the Auxiliary KC
    /// doesn't hold.
    Segment(String),
    /// Its layout, fixups or Info.plist can't be placed; the string says how.
    Place(&'static str),
}

impl fmt::Display for KextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KextError::Exe(e) => write!(f, "{e}"),
            KextError::NoSymbol(name) => write!(
                f,
                "it imports {name}, which neither the kernel collection nor the other kexts \
                 define"
            ),
            KextError::Segment(name) => write!(
                f,
                "it has a segment named {name}; Ambrosia places only __TEXT, __TEXT_EXEC, \
                 __DATA_CONST, __OBJC_CONST and __DATA"
            ),
            KextError::Place(what) => f.write_str(what),
        }
    }
}

/// Where the kernel starts, and what the firmware must clean to the point
/// of coherency before it turns its caches off.
#[derive(Debug, Clone, Copy)]
pub struct Handoff {
    /// The physical address of the kernel's entry point, where the firmware
    /// jumps once the collection is placed and its memory clean.
    pub entry: u64,
    /// The physical address of the boot arguments page.
    pub boot_args: u64,
    /// The `(start, len)` physical ranges the firmware must clean to the
    /// point of coherency before it turns its caches off: the kernel's
    /// static memory, and the carveouts at the top of RAM.
    pub written: [(u64, u64); 2],
}

/// The framebuffer iBoot carves out of RAM: 640 by 1136 at 32 bits, drawn at
/// twice its size. Bits 16 to 23 of the depth hold the scale less 1.
const FB_WIDTH: u64 = 640;
const FB_HEIGHT: u64 = 1136;
const FB_DEPTH: u64 = 32 | (1 << 16);

/// The boot arguments' video fields: the display a graphics driver module
/// set up, at depth 32 and scale 1, or else iBoot's framebuffer at `vram_at`.
fn video(display: Option<&Framebuffer>, vram_at: u64) -> Video {
    match display {
        Some(fb) => Video {
            base: fb.base,
            display: 1,
            row_bytes: u64::from(fb.stride),
            width: u64::from(fb.width),
            height: u64::from(fb.height),
            depth: 32,
        },
        None => Video {
            base: vram_at,
            display: 1,
            row_bytes: FB_WIDTH * 4,
            width: FB_WIDTH,
            height: FB_HEIGHT,
            depth: FB_DEPTH,
        },
    }
}

fn payload<'a>(name: &'static str, file: &'a [u8]) -> Result<Payload<'a>, Error> {
    img4::parse(file).map_err(|e| Error::Image(name, e))
}

fn unpack(name: &'static str, file: &[u8]) -> Result<Vec<u8>, Error> {
    payload(name, file)?
        .unpack()
        .map_err(|e| Error::Image(name, e))
}

fn write(mem: &mut impl Memory, pa: u64, data: &[u8]) -> Result<(), Error> {
    mem.bytes(pa, data.len())
        .ok_or(Error::NoRoom)?
        .copy_from_slice(data);
    Ok(())
}

/// The boot's entropy: the VM's random value and a timer reading, hashed.
/// The VMM draws the random value once per VM process, so the timer is what
/// tells boots of one process apart.
pub fn seed(rnd: u32, timer: u64) -> [u8; 48] {
    let mut h = Sha384::new();
    h.update(b"Ambrosia boot seed");
    h.update(&rnd.to_le_bytes());
    h.update(&timer.to_le_bytes());
    h.finalize()
}

/// The pointer authentication keys the kernel and every process run on,
/// from the boot's seed: the five 128-bit keys, IA, IB, DA, DB and GA, each
/// as its low then its high half. XNU on vmapple never sets the keys
/// (docs/boot-process.md, "The pointer authentication keys"), so these are
/// the guest's for its life.
pub fn pac_keys(seed: &[u8; 48]) -> [u64; 10] {
    let mut bytes = [0u8; 96];
    for (i, half) in bytes.as_chunks_mut::<48>().0.iter_mut().enumerate() {
        let mut h = Sha384::new();
        h.update(b"Ambrosia PAC keys");
        h.update(seed);
        h.update(&[i as u8]);
        half.copy_from_slice(&h.finalize());
    }
    let mut keys = [0u64; 10];
    for (k, b) in keys.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *k = u64::from_le_bytes(*b);
    }
    keys
}

/// NVRAM's banks: two, as `/chosen/nvram-bank-count` gives them.
const NVRAM_BANKS: usize = 2;

/// The variable macOS's avp-rtc driver keeps its clock's offset to calendar
/// time in, in microseconds as `0x%llx`, in NVRAM's `common` partition. It
/// stores 1 for an offset of 0, which timed would take for a reset clock.
const RTC_OFFSET: &[u8] = b"com.apple.System.rtc-offset";

/// Where the aux drive keeps NVRAM, in bytes from its start: the template's
/// `/product/util` `regions` table lists the aux drive's regions as (id,
/// offset, size) triples of 32 bits, and NVRAM is region 2.
pub fn nvram_location(device_tree: &[u8]) -> Result<(u64, u64), Error> {
    let template = unpack("devicetree", device_tree)?;
    let (dt, _) = Node::parse(&template).map_err(Error::Tree)?;
    let regions = dt
        .find("product/util")
        .and_then(|n| n.get("regions"))
        .ok_or(Error::NoNvram)?;
    regions
        .as_chunks::<12>()
        .0
        .iter()
        .map(|t| {
            let w = |i: usize| u64::from(u32::from_le_bytes(t[i..i + 4].try_into().unwrap()));
            (w(0), w(4), w(8))
        })
        .find(|&(id, _, _)| id == 2)
        .map(|(_, off, size)| (off, size))
        .ok_or(Error::NoNvram)
}

/// The serialized trust caches the kernel reads from the memory map's
/// `TrustCache`: a count, each module's offset, then the modules.
fn trust_caches(modules: &[&[u8]]) -> Vec<u8> {
    let header = 4 + 4 * modules.len();
    let mut v = Vec::with_capacity(header + modules.iter().map(|m| m.len()).sum::<usize>());
    v.extend((modules.len() as u32).to_le_bytes());
    let mut at = header;
    for m in modules {
        v.extend((at as u32).to_le_bytes());
        at += m.len();
    }
    for m in modules {
        v.extend_from_slice(m);
    }
    v
}

/// Places everything for the kernel, below `limit`, and gives the handoff.
/// `config` gives the kernel's command line, the kexts to place and the
/// patches to apply. `seed` is the entropy the kernel's random seed and the
/// KASLR slide come from. `log` takes progress lines for the boot log.
pub fn prepare(
    m: &Machine,
    f: &Files<'_>,
    config: &Config<'_>,
    mem: &mut impl Memory,
    limit: u64,
    seed: &[u8; 48],
    log: &mut dyn FnMut(fmt::Arguments<'_>),
) -> Result<Handoff, Error> {
    let ram_end = m.ram_base + m.ram_size;

    let template = unpack("devicetree", f.device_tree)?;
    let (mut dt, _) = Node::parse(&template).map_err(Error::Tree)?;
    let root_hash = unpack("root_hash", f.root_hash)?;
    let base_root_hash = unpack("base_system_root_hash", f.base_root_hash)?;
    let tc = trust_caches(&[
        &unpack("StaticTrustCache", f.static_trust_cache)?,
        &unpack("BaseSystemTrustCache", f.base_trust_cache)?,
    ]);
    let policy = img4::parse_img4(f.policy)
        .map_err(|e| Error::Image("LocalPolicy", e))?
        .manifest
        .ok_or(Error::NoManifest("LocalPolicy"))?;

    let banks = NVRAM_BANKS;
    let nvram = crate::nvram::current(f.nvram_region, banks).ok_or(Error::NoNvram)?;
    log(format_args!(
        "NVRAM bank {} of {banks}, generation {}",
        nvram.index, nvram.generation
    ));
    // macOS keeps the offset from avp-rtc's counter to calendar time in
    // NVRAM, from the first time the guest sets its clock. Until then the
    // offset is zero, and timed takes the clock for reset and sets it back
    // to the last write to the guest's disk. The machine's counter counts
    // calendar time, so a guest without an offset gets the one its driver
    // stores for zero.
    let mut nvram_data = nvram.data.to_vec();
    if m.avp_rtc
        && dt.find("arm-io/avp-rtc").is_some()
        && crate::nvram::add_if_absent(&mut nvram_data, RTC_OFFSET, b"0x1")
    {
        log(format_args!("NVRAM has no RTC offset: set it to zero"));
    }

    let kernel = payload("kernelcache", f.kernel)?;
    let kc_size = kernel
        .size
        .ok_or(Error::Image("kernelcache", img4::Error::BadSize))? as u64;

    // The carveouts at the top of RAM: the manifests, the panic log, then
    // the framebuffer. The kernel's memory ends below them.
    let manifests = [f.manifest, policy, f.sfr_manifest];
    let manifests_len = page_up(manifests.iter().map(|x| x.len() as u64 + 64).sum())
        .ok_or(Error::Overflow("manifests region"))?;
    let manifests_at = ram_end
        .checked_sub(manifests_len)
        .ok_or(Error::Overflow("manifests region below RAM end"))?;
    let pram_at = manifests_at
        .checked_sub(tree::PRAM_SIZE)
        .ok_or(Error::Overflow("panic log below the manifests"))?;
    let fb_len = page_up(FB_WIDTH * 4 * FB_HEIGHT).ok_or(Error::Overflow("framebuffer"))?;
    let vram_at = pram_at
        .checked_sub(fb_len)
        .ok_or(Error::Overflow("framebuffer below the panic log"))?;

    let objects = Objects {
        volume_group: f.volume_group,
        boot_dir: f.boot_dir,
        manifest: f.manifest,
        policy,
        sfr_manifest: f.sfr_manifest,
        root_hash: &root_hash,
        base_root_hash: &base_root_hash,
        nvram: &nvram_data,
        nvram_bank: nvram.index,
        nvram_banks: banks as u32,
        nvram_bank_size: nvram_data.len() as u32,
    };

    // Placeholder entries for the `/chosen/ambrosia` handover's per-kext nodes,
    // for the sizing pass below. The tree's size doesn't depend on the addresses
    // in them, which aren't computed yet.
    let sizing_kexts: Vec<PlacedKext<'_>> = config
        .kexts
        .iter()
        .filter(|k| k.executable.is_some())
        .map(|k| PlacedKext {
            id: &k.id,
            base: 0,
            size: 0,
        })
        .collect();

    // The kernel maps itself in 32 MiB blocks, so its virtual and physical
    // addresses must agree modulo 32 MiB, as iBoot's do. The trust caches go
    // right below the collection, in its block: 26.6.2 and later panic on
    // trust caches above the collection. The device tree goes above it:
    // early on, 14.6.1 maps nothing below the collection, and 26.6.2 nothing
    // below its block. So the collection goes at a random page in RAM's
    // first 32 MiB (a block, since RAM starts on one) that leaves room for
    // the trust caches, and the slide (below) matches it there.
    let random = |at: usize| u64::from_le_bytes(seed[at..at + 8].try_into().unwrap());
    let phys_base = m.ram_base;
    // Every configured kext with an executable rides in one Auxiliary KC,
    // placed below the Boot KC so its lowest segment lies below the Boot KC's
    // (the two guard panics in arm_vm_init). Reserve page-aligned room for the
    // whole collection in the block below it, under the trust caches.
    let aux_kexts: Vec<crate::auxkc::KextInput<'_>> = config
        .kexts
        .iter()
        .filter_map(|k| {
            k.executable.map(|e| crate::auxkc::KextInput {
                id: k.id.as_str(),
                info: k.info,
                exe: e,
            })
        })
        .collect();
    let aux_reserve = if aux_kexts.is_empty() {
        0
    } else {
        page_up(crate::auxkc::size(&aux_kexts)?).ok_or(Error::Overflow("AuxKC reserve"))?
    };
    let tc_room = page_up(tc.len() as u64).ok_or(Error::Overflow("trust caches"))?;
    let below = tc_room
        .checked_add(aux_reserve)
        .ok_or(Error::Overflow("reserve below the collection"))?;
    if below >= BLOCK {
        return Err(Error::NoRoom);
    }
    let kc_at = phys_base + below + random(32) % ((BLOCK - below) / PAGE) * PAGE;
    let tc_at = kc_at - tc_room;
    let aux_at = tc_at - aux_reserve;
    let sepfw_at = kc_at
        .checked_add(page_up(kc_size).ok_or(Error::Overflow("kernel collection size"))?)
        .ok_or(Error::Overflow("kernel collection end"))?;
    // The kernel's static memory must end below `limit` (checked in full
    // below); the collection is unpacked first, so check it now.
    if sepfw_at > limit {
        return Err(Error::NoRoom);
    }
    let dt_at = sepfw_at + PAGE;

    // The kernel collection, unpacked where it runs.
    let kc = mem.bytes(kc_at, kc_size as usize).ok_or(Error::NoRoom)?;
    kernel
        .unpack_into(kc)
        .map_err(|e| Error::Image("kernelcache", e))?;
    let h = Header::parse(kc).map_err(Error::Kernel)?;
    let (lo, _) = h.vm_range();
    log(format_args!(
        "kernel collection: {} entries, {:#x} bytes at {:#x}",
        h.entries.len(),
        kc_size,
        kc_at
    ));

    // KASLR: a slide from 32 MiB to 512 MiB, of 16 KiB pages, that puts the
    // collection's virtual address where its physical one is in its block.
    // Computed now because the AuxKC's load address depends on it.
    let slide = kc_at.wrapping_sub(lo) % BLOCK + (1 + random(40) % 15) * BLOCK;
    let kc_virt = lo + slide;
    let virt_base = kc_virt - (kc_at - phys_base);

    // The donor vtable PAC discriminators to export (Kernel >
    // ExportVtableDiscriminators), read inline from the Boot KC's chained-fixup
    // words before the kernel consumes them, for the /chosen/ambrosia handover.
    let vtables = if config.export_vtable_disc.is_empty() {
        Vec::new()
    } else {
        let syms = h.all_symbols(kc).map_err(Error::Kernel)?;
        crate::vtdisc::export(kc, &syms, slide, &config.export_vtable_disc)
            .map_err(Error::Vtdisc)?
    };
    if !vtables.is_empty() {
        let slots: usize = vtables.iter().map(|t| t.slots.len()).sum();
        log(format_args!(
            "vtable-disc: {} donor vtable(s), {slots} slot(s) in the handover",
            vtables.len()
        ));
    }
    if let (Some(_), Some(name)) = (&m.display, &m.display_node) {
        log(format_args!(
            "display node /arm-io/{name}, a display-subsystem"
        ));
    }

    // Measure the device tree, now that the handover's nodes are known, from a
    // copy filled with placeholder addresses: its byte length is what the real
    // fill must fit, and the kernel's boot arguments follow it in memory. The
    // memory map names an AuxKC only when one is placed, so the placeholder
    // has one when the real fill will.
    let mut sized = dt.clone();
    let placeholder = Placement {
        aux_kc: (0, aux_reserve),
        ..Placement::default()
    };
    tree::populate(
        &mut sized,
        m,
        &objects,
        &placeholder,
        &sizing_kexts,
        &vtables,
        seed,
    )
    .map_err(Error::Populate)?;
    let dt_len = page_up(sized.size() as u64).ok_or(Error::Overflow("device tree"))?;
    drop(sized);
    let args_at = dt_at + dt_len;
    let top = args_at + PAGE;
    if top > limit || top > vram_at {
        return Err(Error::NoRoom);
    }

    // The PAC spins are in the collection's executable segments.
    let mut spins = 0;
    for s in h.segments.iter().filter(|s| s.initprot & 4 != 0) {
        let Some(at) = s.vmaddr.checked_sub(lo).map(|a| a as usize) else {
            continue;
        };
        if let Some(code) = at
            .checked_add(s.vmsize as usize)
            .and_then(|end| kc.get_mut(at..end))
        {
            spins += pac::skip_spins(code);
        }
    }
    log(format_args!("{spins} PAC spins made nops"));

    // The configuration's patches, each over the segments of the entry it
    // names, but the __LINKEDIT they all share.
    for p in &config.patches {
        let e = h
            .entries
            .iter()
            .find(|e| e.id == p.identifier)
            .ok_or_else(|| Error::NoEntry(p.identifier.clone()))?;
        let at = e
            .vmaddr
            .checked_sub(lo)
            .ok_or(Error::Kernel(macho::Error::Truncated))?;
        let entry = Header::parse(kc.get(at as usize..).unwrap_or(&[])).map_err(Error::Kernel)?;
        let mut done = 0;
        for s in entry.segments.iter().filter(|s| s.name != "__LINKEDIT") {
            let limit = match p.count {
                0 => 0,
                n if done < n => n - done,
                _ => break,
            };
            let at = s.vmaddr.wrapping_sub(lo) as usize;
            if let Some(bytes) = kc.get_mut(at..at.saturating_add(s.vmsize as usize)) {
                done += config::apply(p, bytes, limit);
            }
        }
        log(format_args!("patch for {}: {done} replaced", p.identifier));
    }

    // Build the Auxiliary KC for every configured kext, from the Boot KC's
    // identity and symbols: the kexts' binds resolve against the whole
    // collection, the kernel and its kexts both, and the AuxKC's _BootKCID is
    // the Boot KC's LC_UUID. An AuxKC is linked at base zero. The kernel
    // rebases every structural address by the collection's load address, which
    // it takes from phystokv(AuxKC-mach_header): aux_va, the linear-map virtual
    // address of aux_at. So the collection's vmaddrs are offsets from its
    // start, and each kext's own pointers are left as raw kernel-cache chain
    // words for the kernel's deferred AuxKC loader to rebase and sign: a rebase
    // targets this collection by its zero-based offset, a bind the Boot KC by
    // the resolved symbol's offset from `lo`. The handover records where each
    // kext landed. This is the last read of the Boot KC in `kc`, so the borrow
    // ends here and the AuxKC can be written to RAM below.
    let aux_va = aux_at - phys_base + virt_base;
    let (aux_image, placed): (Option<Vec<u8>>, Vec<PlacedKext<'_>>) = if aux_kexts.is_empty() {
        (None, Vec::new())
    } else {
        let boot_uuid = crate::auxkc::boot_kc_uuid(kc)
            .ok_or(Error::Place("the kernel collection has no LC_UUID"))?;
        let symbols = Header::parse(kc)
            .map_err(Error::Kernel)?
            .all_symbols(kc)
            .map_err(Error::Kernel)?;
        let aux = crate::auxkc::build(&aux_kexts, aux_va, lo, &boot_uuid, &symbols, log)?;
        // The handover: each placed kext's slid sub-header address and the span
        // of its segments, read from the AuxKC's fileset entry. The kernel loads
        // the AuxKC at aux_va, so a kext's runtime header is aux_va plus its
        // base-zero sub-header address.
        let ah = Header::parse(&aux).map_err(Error::Kernel)?;
        let mut placed: Vec<PlacedKext<'_>> = Vec::with_capacity(aux_kexts.len());
        for ki in &aux_kexts {
            let missing = |what| Error::Kext(ki.id.into(), KextError::Place(what));
            let e = ah
                .entries
                .iter()
                .find(|e| e.id == ki.id)
                .ok_or_else(|| missing("the Auxiliary KC has no fileset entry for it"))?;
            let sub = Header::parse(aux.get(e.fileoff as usize..).unwrap_or(&[]))
                .map_err(Error::Kernel)?;
            let mut mapped = sub.segments.iter().filter(|s| s.vmsize > 0);
            let first = mapped
                .next()
                .ok_or_else(|| missing("it has no segments in the Auxiliary KC"))?;
            let (mut lo_va, mut hi_va) = (first.vmaddr, first.vmaddr + first.vmsize);
            for s in mapped {
                lo_va = lo_va.min(s.vmaddr);
                hi_va = hi_va.max(s.vmaddr + s.vmsize);
            }
            placed.push(PlacedKext {
                id: ki.id,
                base: aux_va + e.vmaddr,
                size: hi_va - lo_va,
            });
        }
        log(format_args!(
            "AuxKC placed at phys {aux_at:#x}, virt {aux_va:#x}, {:#x} bytes, {} kext(s)",
            aux.len(),
            placed.len(),
        ));
        (Some(aux), placed)
    };
    if let Some(aux) = &aux_image {
        // The collection must fit the room reserved for it from `size`, or it
        // would overrun up into the trust caches and the Boot KC placed just
        // above it. `size` and `build` lay out the same segments, so this holds;
        // the check keeps a difference between them from corrupting memory.
        if aux.len() as u64 > aux_reserve {
            return Err(Error::Overflow("AuxKC larger than its reserve"));
        }
        write(mem, aux_at, aux)?;
    }

    let entry = h.entry.ok_or(Error::Kernel(macho::Error::NoEntry))?;
    let entry_pa = kc_at
        .checked_add(
            entry
                .checked_sub(lo)
                .ok_or(Error::Overflow("entry below the collection"))?,
        )
        .ok_or(Error::Overflow("entry above RAM"))?;

    // The kernel collection's parts, as the memory map gives them: read-only
    // from its start up to the first writable segment, writable up to
    // __LINKEDIT, then __LINKEDIT.
    let first_rw = h
        .segments
        .iter()
        .filter(|s| s.initprot & 2 != 0)
        .map(|s| s.vmaddr)
        .min()
        .unwrap_or(lo);
    let le = h
        .segment("__LINKEDIT")
        .map(|s| (s.vmaddr, s.vmsize))
        .unwrap_or((lo.saturating_add(kc_size), 0));
    let pa = |va: u64| {
        kc_at
            .checked_add(
                va.checked_sub(lo)
                    .ok_or(Error::Overflow("address below the collection"))?,
            )
            .ok_or(Error::Overflow("address above RAM"))
    };
    let kc_rw_size =
        le.0.checked_sub(first_rw)
            .ok_or(Error::Overflow("__LINKEDIT below the writable segments"))?;
    let (kc_rw_pa, kc_le_pa) = (pa(first_rw)?, pa(le.0)?);

    // The manifests, 64-byte aligned in their carveout.
    let mut man_at = manifests_at;
    let mut man_ranges = [(0, 0); 3];
    for (i, man) in manifests.iter().enumerate() {
        write(mem, man_at, man)?;
        man_ranges[i] = (man_at, man.len() as u64);
        man_at = (man_at + man.len() as u64).next_multiple_of(64);
    }

    let at = Placement {
        device_tree: (dt_at, dt_len),
        trust_cache: (tc_at, tc_room),
        boot_args: (args_at, PAGE),
        sepfw: (sepfw_at, 0x2000),
        kc_ro: (kc_at, first_rw - lo),
        kc_rw: (kc_rw_pa, kc_rw_size),
        kc_le: (kc_le_pa, le.1),
        kc_entry: entry + slide,
        kc_virt,
        manifest: man_ranges[0],
        policy: man_ranges[1],
        sfr_manifest: man_ranges[2],
        manifests_region: (manifests_at, manifests_len),
        pram: (pram_at, tree::PRAM_SIZE),
        vram: (vram_at, fb_len),
        kernel_memory: (phys_base, vram_at - phys_base),
        aux_kc: match &aux_image {
            Some(aux) => (aux_at, aux.len() as u64),
            None => (0, 0),
        },
    };
    tree::populate(&mut dt, m, &objects, &at, &placed, &vtables, seed).map_err(Error::Populate)?;
    let dt_bytes = dt.serialize();
    let dt_mem = mem.bytes(dt_at, dt_len as usize).ok_or(Error::NoRoom)?;
    dt_mem.fill(0);
    dt_mem
        .get_mut(..dt_bytes.len())
        .ok_or(Error::Overflow("device tree larger than measured"))?
        .copy_from_slice(&dt_bytes);

    let tc_mem = mem
        .bytes(tc_at, at.trust_cache.1 as usize)
        .ok_or(Error::NoRoom)?;
    tc_mem.fill(0);
    tc_mem[..tc.len()].copy_from_slice(&tc);
    mem.bytes(sepfw_at, PAGE as usize)
        .ok_or(Error::NoRoom)?
        .fill(0);
    mem.bytes(vram_at, fb_len as usize)
        .ok_or(Error::NoRoom)?
        .fill(0);
    mem.bytes(pram_at, tree::PRAM_SIZE as usize)
        .ok_or(Error::NoRoom)?
        .fill(0);

    let args = BootArgs {
        virt_base,
        phys_base,
        mem_size: vram_at - phys_base,
        top_of_kernel_data: top,
        video: video(m.display.as_ref(), vram_at),
        machine_type: 0,
        device_tree: virt_base + (dt_at - phys_base),
        device_tree_length: dt_len as u32,
        boot_flags: 0,
        mem_size_actual: 0,
    };
    let args_mem = mem.bytes(args_at, PAGE as usize).ok_or(Error::NoRoom)?;
    args_mem.fill(0);
    args.write(
        &config.command_line,
        (&mut args_mem[..bootargs::SIZE]).try_into().unwrap(),
    );
    log(format_args!(
        "physBase {phys_base:#x} virtBase {virt_base:#x} top {top:#x} memSize {:#x} slide {slide:#x}",
        args.mem_size
    ));

    Ok(Handoff {
        entry: entry_pa,
        boot_args: args_at,
        written: [(phys_base, top - phys_base), (vram_at, ram_end - vram_at)],
    })
}

/// Where `needle` first appears in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The prelink dict for a placed kext: its Info.plist's own keys, plus the
/// addresses the kernel needs. The Info.plist's `IOKitPersonalities` and
/// `OSBundleLibraries` carry through, so the kernel matches and starts it.
pub(crate) fn prelink_dict(
    info: &[u8],
    load_addr: u64,
    size: u64,
    kmod: u64,
) -> Result<String, KextError> {
    let text =
        core::str::from_utf8(info).map_err(|_| KextError::Place("its Info.plist is not UTF-8"))?;
    let no_dict = || KextError::Place("its Info.plist has no top-level <dict>");
    let open = text.find("<dict>").ok_or_else(no_dict)? + "<dict>".len();
    let close = text
        .rfind("</dict>")
        .filter(|&close| close >= open)
        .ok_or_else(no_dict)?;
    let inner = &text[open..close];
    Ok(alloc::format!(
        "<dict>{inner}\
         <key>_PrelinkExecutableLoadAddr</key><integer>{load}</integer>\
         <key>_PrelinkExecutableSourceAddr</key><integer>{load}</integer>\
         <key>_PrelinkExecutableSize</key><integer>{size}</integer>\
         <key>_PrelinkKmodInfo</key><integer>{kmod}</integer></dict>",
        load = load_addr as i64,
        kmod = kmod as i64,
    ))
}

/// Inserts the placed kexts' dicts at the front of `_PrelinkInfoDictionary`,
/// in the prelink info's slack. The info is XML text ended by a NUL.
pub(crate) fn insert_prelink(
    kc: &mut [u8],
    prelink: (u64, u64),
    dicts: &[String],
) -> Result<(), Error> {
    let (fo, size) = (prelink.0 as usize, prelink.1 as usize);
    let seg = fo
        .checked_add(size)
        .and_then(|end| kc.get_mut(fo..end))
        .ok_or(Error::NoPrelinkArray)?;
    // The text runs up to its NUL, and the array must be in it.
    let used = seg.iter().position(|&b| b == 0).unwrap_or(seg.len());
    let text = &seg[..used];
    let marker = find(text, b"_PrelinkInfoDictionary").ok_or(Error::NoPrelinkArray)?;
    let array = find(&text[marker..], b"<array>").ok_or(Error::NoPrelinkArray)? + marker;
    let at = array + "<array>".len();
    let add: usize = dicts.iter().map(|d| d.len()).sum();
    if used + add > seg.len() {
        return Err(Error::NoRoom);
    }
    seg.copy_within(at..used, at + add);
    let mut w = at;
    for d in dicts {
        seg[w..w + d.len()].copy_from_slice(d.as_bytes());
        w += d.len();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pac_keys_differ() {
        let k = pac_keys(&seed(0x6a5a_78a3, 1));
        assert_eq!(k, pac_keys(&seed(0x6a5a_78a3, 1)));
        assert_ne!(k, pac_keys(&seed(0x6a5a_78a3, 2)));
        for (i, a) in k.iter().enumerate() {
            assert!(k[i + 1..].iter().all(|b| b != a));
        }
    }

    #[test]
    fn video_fields() {
        let fb = Framebuffer {
            base: 0x5000_0000,
            width: 1024,
            height: 768,
            stride: 4096,
            reserved: 0,
        };
        assert_eq!(
            video(Some(&fb), 0x2_fdc0_0000),
            Video {
                base: 0x5000_0000,
                display: 1,
                row_bytes: 4096,
                width: 1024,
                height: 768,
                depth: 32
            }
        );
        assert_eq!(
            video(None, 0x2_fdc0_0000),
            Video {
                base: 0x2_fdc0_0000,
                display: 1,
                row_bytes: 2560,
                width: 640,
                height: 1136,
                depth: 0x1_0020
            }
        );
    }

    #[test]
    fn prelink_dict_carries_personalities_and_addresses() {
        let info = br#"<?xml version="1.0"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>ai.aneesiqbal.ambrosia.test.kext</string>
<key>IOKitPersonalities</key><dict><key>t</key><dict/></dict>
</dict></plist>"#;
        let d = prelink_dict(info, 0xfffffe000bdb0000, 0x1c000, 0xfffffe000bdb80b0).unwrap();
        assert!(d.starts_with("<dict>") && d.ends_with("</dict>"));
        assert!(d.contains("IOKitPersonalities") && d.contains("ai.aneesiqbal.ambrosia.test.kext"));
        assert!(d.contains(&alloc::format!(
            "<integer>{}</integer>",
            0xfffffe000bdb0000u64 as i64
        )));
        assert!(d.contains("_PrelinkKmodInfo"));
    }

    #[test]
    fn prelink_dict_needs_a_dict() {
        for info in [
            &b"<plist></plist>"[..],
            b"</dict><dict>",
            b"<dict>\xff</dict>",
        ] {
            assert!(
                matches!(prelink_dict(info, 0, 0, 0), Err(KextError::Place(_))),
                "{}",
                String::from_utf8_lossy(info)
            );
        }
    }

    #[test]
    fn insert_prelink_adds_entries() {
        let mut seg = alloc::vec![0u8; 256];
        let plist = b"<key>_PrelinkInfoDictionary</key><array><dict><key>old</key></dict></array>";
        seg[..plist.len()].copy_from_slice(plist);
        insert_prelink(&mut seg, (0, 256), &["<dict>NEW</dict>".into()]).unwrap();
        let text = core::str::from_utf8(&seg[..seg.iter().position(|&b| b == 0).unwrap()]).unwrap();
        assert_eq!(
            text,
            "<key>_PrelinkInfoDictionary</key><array><dict>NEW</dict><dict><key>old</key></dict></array>"
        );
        // An array past the text's end is none.
        let mut seg = alloc::vec![0u8; 256];
        seg[..12].copy_from_slice(b"<plist>\0<key");
        seg[12..12 + plist.len()].copy_from_slice(plist);
        assert_eq!(
            insert_prelink(&mut seg, (0, 256), &["<dict/>".into()]),
            Err(Error::NoPrelinkArray)
        );
    }

    /// Some of the lines the docs quote, as the firmware prints them.
    #[test]
    fn error_messages() {
        let placing = |e| Error::Kext("ai.aneesiqbal.ambrosia.test.kext".into(), e).to_string();
        assert_eq!(
            placing(KextError::Exe(kext::Error::NoKmodInfo)),
            "kext ai.aneesiqbal.ambrosia.test.kext: the executable has no _kmod_info symbol"
        );
        assert_eq!(
            placing(KextError::Exe(kext::Error::Unsupported(
                "a bind with an addend"
            ))),
            "kext ai.aneesiqbal.ambrosia.test.kext: the executable's chained fixups have a \
             bind with an addend, which Ambrosia doesn't read"
        );
        assert_eq!(
            placing(KextError::NoSymbol("_IOLog".into())),
            "kext ai.aneesiqbal.ambrosia.test.kext: it imports _IOLog, which neither the \
             kernel collection nor the other kexts define"
        );
        assert_eq!(
            Error::NoEntry("com.example.none".into()).to_string(),
            "Kernel.Patch names com.example.none, which is not an entry of the kernel \
             collection"
        );
        assert_eq!(
            Error::Image("kernelcache", img4::Error::Encrypted).to_string(),
            "kernelcache: its payload is encrypted"
        );
        assert_eq!(
            Error::Populate(tree::Error::DisplayNodeTaken).to_string(),
            "/arm-io already has a node named as Graphics.DisplayNode"
        );
    }

    #[test]
    fn trust_cache_header() {
        let v = trust_caches(&[&[1, 2, 3, 4], &[5, 6]]);
        assert_eq!(&v[..12], &[2, 0, 0, 0, 12, 0, 0, 0, 16, 0, 0, 0]);
        assert_eq!(&v[12..], &[1, 2, 3, 4, 5, 6]);
    }
}
