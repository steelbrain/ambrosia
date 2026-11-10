//! An identity map, so the image runs with caches on and may make unaligned
//! accesses, which device memory, and so memory with the MMU off, faults on.
//!
//! RAM and the firmware's own regions are normal write-back memory, and the
//! rest is device memory. The kernel expects the MMU and caches off at entry,
//! so `clean` and `enter_kernel` undo this before the jump.

use core::arch::asm;

/// Where vmapple's RAM starts: at 4 GiB, above the PCIe window.
pub const RAM_BASE: u64 = 0x1_0000_0000;

const GIB: u64 = 1 << 30;
const MIB2: u64 = 2 << 20;

// Descriptor bits, for the 4 KiB granule.
const VALID: u64 = 1 << 0;
const TABLE: u64 = 1 << 1;
const ATTR_DEVICE: u64 = 0 << 2;
const ATTR_NORMAL: u64 = 1 << 2;
const INNER_SHAREABLE: u64 = 3 << 8;
const ACCESS: u64 = 1 << 10;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;

const NORMAL: u64 = VALID | ATTR_NORMAL | INNER_SHAREABLE | ACCESS;
const DEVICE: u64 = VALID | ATTR_DEVICE | ACCESS | PXN | UXN;

/// MAIR_EL1: attribute 0 is Device-nGnRnE, attribute 1 normal write-back.
const MAIR: u64 = 0xff << 8;

#[repr(C, align(4096))]
struct Table([u64; 512]);

/// Level 1 covers 512 GiB, 1 GiB an entry; the first two GiB, which hold
/// the firmware's own regions among devices, go down to level 2.
static mut L1: Table = Table([0; 512]);
static mut L2: [Table; 2] = [Table([0; 512]), Table([0; 512])];

/// Maps RAM from [`RAM_BASE`] up to `ram_end`, turns the MMU and caches on.
pub fn enable(ram_end: u64) {
    // SAFETY: the tables are only written here, before the MMU uses them.
    let (l1, l2) = unsafe {
        (
            &mut *core::ptr::addr_of_mut!(L1),
            &mut *core::ptr::addr_of_mut!(L2),
        )
    };
    for (gib, table) in l2.iter_mut().enumerate() {
        for (i, entry) in table.0.iter_mut().enumerate() {
            let pa = gib as u64 * GIB + i as u64 * MIB2;
            // The firmware region and the config block are RAM in the first
            // 2 MiB blocks; the rest is devices and the PCIe window.
            let normal = pa < 0x0060_0000;
            *entry = pa | if normal { NORMAL } else { DEVICE };
        }
        l1.0[gib] = table.0.as_ptr() as u64 | VALID | TABLE;
    }
    for gib in 2..512u64 {
        let pa = gib * GIB;
        if pa < ram_end {
            l1.0[gib as usize] = pa | if pa < RAM_BASE { DEVICE } else { NORMAL };
        }
    }

    // T0SZ 25: a 39-bit space from level 1. Walks are inner shareable and
    // cached; TTBR1 is unused. IPS is the CPU's physical address size.
    let parange: u64;
    // SAFETY: reads an ID register.
    unsafe { asm!("mrs {}, id_aa64mmfr0_el1", out(reg) parange) };
    let tcr = 25 | (1 << 8) | (1 << 10) | (3 << 12) | (1 << 23) | ((parange & 7) << 32);
    // SAFETY: the tables map the code running now at its own address.
    unsafe {
        asm!(
            "msr mair_el1, {mair}",
            "msr tcr_el1, {tcr}",
            "msr ttbr0_el1, {ttbr}",
            "dsb ish",
            "tlbi vmalle1",
            "dsb ish",
            "isb",
            "mrs {t}, sctlr_el1",
            "orr {t}, {t}, #(1 << 0)",
            "orr {t}, {t}, #(1 << 2)",
            "orr {t}, {t}, #(1 << 12)",
            "msr sctlr_el1, {t}",
            "isb",
            mair = in(reg) MAIR,
            tcr = in(reg) tcr,
            ttbr = in(reg) l1.0.as_ptr() as u64,
            t = out(reg) _,
        );
    }
}

/// The smallest data and instruction cache line sizes, in bytes, from
/// CTR_EL0's DminLine and IminLine, each log2 of the line in 4-byte words.
fn cache_lines() -> (u64, u64) {
    let ctr: u64;
    // SAFETY: reads the cache type register.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr) };
    (4 << ((ctr >> 16) & 0xf), 4 << (ctr & 0xf))
}

/// Cleans and invalidates `[start, start + len)` to the point of coherency,
/// so memory holds what the caches did once they are off, and no dirty line
/// is left to be written back over what the kernel writes with them off.
pub fn clean(start: u64, len: u64) {
    let (line, _) = cache_lines();
    let mut a = start & !(line - 1);
    while a < start + len {
        // SAFETY: a cache maintenance operation on a mapped address.
        unsafe { asm!("dc civac, {}", in(reg) a) };
        a += line;
    }
    // SAFETY: a barrier.
    unsafe { asm!("dsb sy") };
}

/// Makes code just written to `[start, start + len)` safe to run: cleans
/// it to the point of unification and invalidates the instruction cache
/// over it.
pub fn sync_code(start: u64, len: u64) {
    let (dline, iline) = cache_lines();
    let mut a = start & !(dline - 1);
    while a < start + len {
        // SAFETY: a cache maintenance operation on a mapped address.
        unsafe { asm!("dc cvau, {}", in(reg) a) };
        a += dline;
    }
    // SAFETY: a barrier.
    unsafe { asm!("dsb ish") };
    let mut a = start & !(iline - 1);
    while a < start + len {
        // SAFETY: as above.
        unsafe { asm!("ic ivau, {}", in(reg) a) };
        a += iline;
    }
    // SAFETY: barriers.
    unsafe { asm!("dsb ish", "isb") };
}
