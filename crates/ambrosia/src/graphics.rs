//! The graphics driver interface, revision 1: how Ambrosia starts a driver
//! module, and what the module gives back. docs/graphics-drivers.md is the
//! guide for module authors.
//!
//! A module is an ELF file in the configuration bundle, which `Graphics` >
//! `Driver` in `config.plist` names (`config.rs`; the format is `elf.rs`'s).
//! Ambrosia loads it, calls its entry point once, before it reads the
//! guest's disks, and never calls into it again. The module finds its device
//! on the PCIe bus, gives it addresses from the PCIe window, sets a mode, and
//! returns the framebuffer. Ambrosia then draws its log there, as an early
//! console, and hands the framebuffer to the kernel.
//!
//! The types are `#[repr(C)]`, and the entry point and the log callback are
//! `extern "C"`, so a module can be written in any language. In C:
//!
//! ```c
//! struct services {                  // offset
//!     uint32_t revision;             //  0: 1
//!     uint32_t reserved;             //  4: 0
//!     uint64_t ecam_base;            //  8
//!     uint64_t ecam_size;            // 16
//!     uint64_t window_base;          // 24
//!     uint64_t window_size;          // 32
//!     void (*log)(const struct services *services,
//!                 const uint8_t *text, size_t len);   // 40
//! };                                 // 48 bytes
//!
//! struct framebuffer {               // offset
//!     uint64_t base;                 //  0
//!     uint32_t width;                //  8
//!     uint32_t height;               // 12
//!     uint32_t stride;               // 16
//!     uint32_t reserved;             // 20
//! };                                 // 24 bytes
//!
//! int32_t entry(const struct services *services, struct framebuffer *out);
//! ```
//!
//! This file uses nothing but `core`, so a module written in Rust can include
//! it as it is (`drivers/bochs-display` does).

/// The interface's revision, which [`Services::revision`] carries. A later
/// revision only adds fields at the end of [`Services`].
pub const REVISION: u32 = 1;

/// What Ambrosia gives the module. All addresses are physical. The module
/// runs with the MMU on and an identity map, so they are its pointers too.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Services {
    /// [`REVISION`].
    pub revision: u32,
    /// Zero.
    pub reserved: u32,
    /// The PCIe ECAM: bus 0's configuration space, 4 KiB a function, at
    /// `ecam_base + (device << 15) + (function << 12)`.
    pub ecam_base: u64,
    /// The ECAM's size in bytes; no function lies past it.
    pub ecam_size: u64,
    /// The PCIe memory window, from which the module may give its own
    /// device's BARs addresses. The kernel assigns every other BAR from the
    /// same window, and keeps the module's. It lies below 4 GiB, but a
    /// 32-bit BAR must not reach 0xffffffff: QEMU never maps one there, and
    /// the kernel moves it (docs/graphics-drivers.md, "Where the BARs go").
    pub window_base: u64,
    /// The PCIe memory window's size in bytes.
    pub window_size: u64,
    /// Writes `len` bytes of UTF-8 at `text` as one line of Ambrosia's log,
    /// to the UART and, once a framebuffer is up, to the screen. Ambrosia
    /// adds the line's prefix and its end. `text` must point at `len`
    /// readable bytes, and may be null when `len` is 0.
    pub log: unsafe extern "C" fn(services: &Services, text: *const u8, len: usize),
}

/// The display the module set up, which it writes for Ambrosia when it
/// returns 0. Ambrosia zeroes it before the call.
///
/// The pixel format is fixed: 32 bits, `0x00RRGGBB` little-endian, so blue
/// is the first byte in memory. It is the format the kernel's console takes
/// for depth 32.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Framebuffer {
    /// Its physical address: inside the PCIe window, in one of the device's
    /// BARs, which the module left decoding.
    pub base: u64,
    /// The visible width, in pixels.
    pub width: u32,
    /// The visible height, in pixels.
    pub height: u32,
    /// Bytes from one row to the next: at least `4 * width`, and a multiple
    /// of 4.
    pub stride: u32,
    /// Zero.
    pub reserved: u32,
}

/// The module's entry point, its ELF header's `e_entry`. It returns 0 when
/// it drives a display and has filled `out`, and anything else when it found
/// no device it drives, or failed; then Ambrosia boots with no console.
pub type Entry = unsafe extern "C" fn(services: &Services, out: &mut Framebuffer) -> i32;

// The C layout above, checked.
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(offset_of!(Services, ecam_base) == 8);
    assert!(offset_of!(Services, window_base) == 24);
    assert!(offset_of!(Services, log) == 40);
    assert!(size_of::<Services>() == 48);
    assert!(offset_of!(Framebuffer, width) == 8);
    assert!(offset_of!(Framebuffer, stride) == 16);
    assert!(size_of::<Framebuffer>() == 24);
};
