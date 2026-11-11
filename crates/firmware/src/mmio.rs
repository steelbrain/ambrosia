//! Loads and stores of device memory, each one instruction of a fixed form:
//! one general-purpose register, the address in a register, no offset and
//! no writeback.
//!
//! The CPU reports a data abort on that form with a syndrome (ISV=1), which
//! gives the register, the size and the direction, so the hypervisor hands
//! the VMM a decoded access. A volatile access promises one access of its
//! size, but not the instruction: the compiler may fold an address update
//! into it, as a pre- or post-indexed form, and the CPU reports that form
//! without a syndrome, which the VMM must then decode itself
//! (docs/machine.md, "MMIO accesses without a syndrome, under HVF"). The
//! assembly here leaves the compiler no such choice.

use core::arch::asm;

/// Reads the 32-bit register at `at`.
///
/// # Safety
///
/// `at` must be aligned to 4 and be a register of device memory in the
/// identity map that may be read.
pub unsafe fn read32(at: usize) -> u32 {
    let v: u32;
    // SAFETY: the caller's. The asm may touch memory, so the compiler keeps
    // it in order with the accesses around it, as it would a volatile one.
    unsafe {
        asm!("ldr {v:w}, [{at}]", at = in(reg) at, v = out(reg) v, options(nostack, preserves_flags));
    }
    v
}

/// Writes `v` to the 32-bit register at `at`.
///
/// # Safety
///
/// `at` must be aligned to 4 and be a register of device memory in the
/// identity map that may be written with `v`.
pub unsafe fn write32(at: usize, v: u32) {
    // SAFETY: as for read32.
    unsafe {
        asm!("str {v:w}, [{at}]", at = in(reg) at, v = in(reg) v, options(nostack, preserves_flags));
    }
}

/// Writes `v` to the 64-bit register at `at`.
///
/// # Safety
///
/// `at` must be aligned to 8 and be a register of device memory in the
/// identity map that may be written with `v`.
pub unsafe fn write64(at: usize, v: u64) {
    // SAFETY: as for read32.
    unsafe {
        asm!("str {v:x}, [{at}]", at = in(reg) at, v = in(reg) v, options(nostack, preserves_flags));
    }
}
