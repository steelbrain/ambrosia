//! Loads and stores of device memory, each one instruction of a fixed form:
//! one general-purpose register, the address in a register, no offset and
//! no writeback.
//!
//! The CPU reports a data abort on that form with a syndrome (ISV=1), so the
//! hypervisor hands the VMM a decoded access. A volatile access promises one
//! access of its size, but not the instruction: the compiler may fold an
//! address update into it, as a pre- or post-indexed form, which the CPU
//! reports without a syndrome. A VMM that doesn't decode such an access
//! itself stops, or injects an abort, which stops the boot
//! (docs/graphics-drivers.md, "Where the module runs").

use core::arch::asm;

/// Reads the 32-bit register at `at`.
///
/// # Safety
///
/// `at` must be aligned to 4 and be a register of device memory that may be
/// read.
pub unsafe fn read32(at: u64) -> u32 {
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
/// `at` must be aligned to 4 and be a register of device memory that may be
/// written with `v`.
pub unsafe fn write32(at: u64, v: u32) {
    // SAFETY: as for read32.
    unsafe {
        asm!("str {v:w}, [{at}]", at = in(reg) at, v = in(reg) v, options(nostack, preserves_flags));
    }
}

/// Reads the 16-bit register at `at`.
///
/// # Safety
///
/// `at` must be aligned to 2 and be a register of device memory that may be
/// read.
pub unsafe fn read16(at: u64) -> u16 {
    let v: u32;
    // SAFETY: as for read32.
    unsafe {
        asm!("ldrh {v:w}, [{at}]", at = in(reg) at, v = out(reg) v, options(nostack, preserves_flags));
    }
    v as u16
}

/// Writes `v` to the 16-bit register at `at`.
///
/// # Safety
///
/// `at` must be aligned to 2 and be a register of device memory that may be
/// written with `v`.
pub unsafe fn write16(at: u64, v: u16) {
    // SAFETY: as for read32.
    unsafe {
        asm!("strh {v:w}, [{at}]", at = in(reg) at, v = in(reg) u32::from(v), options(nostack, preserves_flags));
    }
}
