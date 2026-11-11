//! An Ambrosia graphics driver module for QEMU's `bochs-display`, PCI
//! 1234:1111, and the example of the interface (docs/graphics-drivers.md).
//!
//! It finds the device on bus 0, gives its two BARs addresses at the start
//! of the PCIe window, turns its memory decoding on, and sets a 1024 by 768
//! mode of 32 bits through the Bochs VBE ("DISPI") registers in BAR 2. BAR 0
//! is the framebuffer. The device is QEMU's (`hw/display/bochs-display.c`),
//! so this works on any QEMU machine with it on bus 0.
//!
//! `make bochs-display` builds it as target/bochs-display.elf, and `make
//! bochs-display-bundle` a configuration bundle that starts it,
//! target/bochs-display.cpio. QEMU loads that with
//! `-device loader,file=target/bochs-display.cpio,addr=0x120000000,force-raw=on`,
//! beside `-device bochs-display,romfile=`.

#![no_std]
#![no_main]

#[path = "../../../crates/ambrosia/src/graphics.rs"]
mod graphics;
mod mmio;

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicPtr, Ordering};
use graphics::{Framebuffer, Services};

const VENDOR: u32 = 0x1234;
const DEVICE: u32 = 0x1111;
const WIDTH: u16 = 1024;
const HEIGHT: u16 = 768;

/// The entry point's result when it drives a display and has filled `out`.
const DRIVES_A_DISPLAY: i32 = 0;
/// Its result when bus 0 has no bochs-display.
const NO_DEVICE: i32 = 1;
/// Its result when it found the device but could not set it up.
const FAILED: i32 = 2;

/// The configuration space's command register, and its memory decoding bit.
const COMMAND: u64 = 0x04;
const MEMORY_SPACE: u16 = 1 << 1;
/// The first BAR's offset in configuration space.
const BARS: u64 = 0x10;

/// BAR 2 holds the DISPI registers, 16 bits each, from 0x500, by index.
const DISPI: u64 = 0x500;
const ID: u64 = 0x0;
const XRES: u64 = 0x1;
const YRES: u64 = 0x2;
const BPP: u64 = 0x3;
const ENABLE: u64 = 0x4;
const VIRT_WIDTH: u64 = 0x6;
const X_OFFSET: u64 = 0x8;
const Y_OFFSET: u64 = 0x9;
const VIDEO_MEMORY_64K: u64 = 0xa;
const ENABLED: u16 = 0x01;
const LFB_ENABLED: u16 = 0x40;
/// The IDs the DISPI interface reports, one per revision.
const IDS: core::ops::RangeInclusive<u16> = 0xb0c0..=0xb0c5;
/// QEMU's extended register for the framebuffer's byte order, in BAR 2.
const BYTE_ORDER: u64 = 0x604;
const LITTLE_ENDIAN: u32 = 0x1e1e_1e1e;

/// The services, for the panic handler's log line.
static SERVICES: AtomicPtr<Services> = AtomicPtr::new(core::ptr::null_mut());

/// One line of text, for [`Services::log`].
struct Line {
    buf: [u8; 128],
    len: usize,
}

impl Write for Line {
    /// Adds as much of `s` as fits, cut at a character boundary so that the
    /// line stays UTF-8.
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let n = s.floor_char_boundary(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

fn log(s: &Services, args: fmt::Arguments<'_>) {
    let mut line = Line {
        buf: [0; 128],
        len: 0,
    };
    let _ = line.write_fmt(args);
    // SAFETY: the line's bytes, as many as it holds.
    unsafe { (s.log)(s, line.buf.as_ptr(), line.len) };
}

/// One PCI function's configuration space, by its address in the ECAM.
#[derive(Clone, Copy)]
struct ConfigSpace(u64);

impl ConfigSpace {
    fn read32(self, off: u64) -> u32 {
        // SAFETY: an aligned register of the ECAM, which Services gives.
        unsafe { mmio::read32(self.0 + off) }
    }
    fn write32(self, off: u64, v: u32) {
        // SAFETY: as for read32.
        unsafe { mmio::write32(self.0 + off, v) }
    }
    fn read16(self, off: u64) -> u16 {
        // SAFETY: as for read32.
        unsafe { mmio::read16(self.0 + off) }
    }
    fn write16(self, off: u64, v: u16) {
        // SAFETY: as for read32.
        unsafe { mmio::write16(self.0 + off, v) }
    }

    /// The size of memory BAR `i`, and whether it is 64-bit, or `None` if
    /// it is an I/O BAR or not there. Memory decoding must be off.
    fn bar_size(self, i: u64) -> Option<(u64, bool)> {
        let at = BARS + 4 * i;
        let old = self.read32(at);
        if old & 1 != 0 {
            return None;
        }
        let wide = (old >> 1) & 3 == 2;
        self.write32(at, !0);
        let mut mask = u64::from(self.read32(at) & !0xf) | 0xffff_ffff_0000_0000;
        self.write32(at, old);
        if wide {
            let old_high = self.read32(at + 4);
            self.write32(at + 4, !0);
            mask = (mask & 0xffff_ffff) | u64::from(self.read32(at + 4)) << 32;
            self.write32(at + 4, old_high);
        }
        let size = (!mask).wrapping_add(1);
        (mask & 0xffff_fff0 != 0).then_some((size, wide))
    }

    fn set_bar(self, i: u64, wide: bool, at: u64) {
        self.write32(BARS + 4 * i, at as u32);
        if wide {
            self.write32(BARS + 4 * i + 4, (at >> 32) as u32);
        }
    }
}

/// The device on bus 0, by its configuration space and its device and
/// function numbers.
fn find(s: &Services) -> Option<(ConfigSpace, u64, u64)> {
    for dev in 0..32u64 {
        for func in 0..8u64 {
            let off = dev << 15 | func << 12;
            if off + 0x1000 > s.ecam_size {
                return None;
            }
            let c = ConfigSpace(s.ecam_base + off);
            if c.read32(0) == DEVICE << 16 | VENDOR {
                return Some((c, dev, func));
            }
        }
    }
    None
}

fn dispi_read(regs: u64, index: u64) -> u16 {
    // SAFETY: a DISPI register in the device's BAR 2, which decodes.
    unsafe { mmio::read16(regs + DISPI + 2 * index) }
}

fn dispi_write(regs: u64, index: u64, v: u16) {
    // SAFETY: as for dispi_read.
    unsafe { mmio::write16(regs + DISPI + 2 * index, v) }
}

// The entry point has the interface's signature.
const _: graphics::Entry = driver_entry;

/// The module's entry point (docs/graphics-drivers.md, "The entry point"),
/// which `.cargo/config.toml` makes the ELF's: it sets the device up and
/// fills `out` with its framebuffer.
#[unsafe(no_mangle)]
pub extern "C" fn driver_entry(s: &Services, out: &mut Framebuffer) -> i32 {
    SERVICES.store(s as *const Services as *mut Services, Ordering::Relaxed);
    if s.revision < graphics::REVISION {
        return FAILED;
    }
    let Some((c, dev, func)) = find(s) else {
        log(s, format_args!("bochs-display: no device on bus 0"));
        return NO_DEVICE;
    };

    // Size both BARs with decoding off, then place them at the start of the
    // window, each aligned to its size, the larger first. (Not at its end:
    // QEMU never maps a 32-bit BAR that reaches 0xffffffff.)
    let command = c.read16(COMMAND);
    c.write16(COMMAND, command & !MEMORY_SPACE);
    let (Some((fb_size, fb_wide)), Some((regs_size, regs_wide))) = (c.bar_size(0), c.bar_size(2))
    else {
        log(s, format_args!("bochs-display: BAR 0 or 2 is not memory"));
        return FAILED;
    };
    let fb = s.window_base.next_multiple_of(fb_size);
    let regs = (fb + fb_size).next_multiple_of(regs_size);
    if regs + regs_size > s.window_base + s.window_size {
        log(
            s,
            format_args!("bochs-display: its BARs don't fit the window"),
        );
        return FAILED;
    }
    c.set_bar(0, fb_wide, fb);
    c.set_bar(2, regs_wide, regs);
    c.write16(COMMAND, command | MEMORY_SPACE);

    if !IDS.contains(&dispi_read(regs, ID)) {
        log(
            s,
            format_args!("bochs-display: no DISPI interface in BAR 2"),
        );
        return FAILED;
    }
    let vram = u64::from(dispi_read(regs, VIDEO_MEMORY_64K)) * 0x10000;
    let stride = u32::from(WIDTH) * 4;
    if u64::from(stride * u32::from(HEIGHT)) > vram.min(fb_size) {
        log(
            s,
            format_args!("bochs-display: {vram:#x} bytes of video memory is too little"),
        );
        return FAILED;
    }
    dispi_write(regs, ENABLE, 0);
    dispi_write(regs, XRES, WIDTH);
    dispi_write(regs, YRES, HEIGHT);
    dispi_write(regs, BPP, 32);
    dispi_write(regs, VIRT_WIDTH, WIDTH);
    dispi_write(regs, X_OFFSET, 0);
    dispi_write(regs, Y_OFFSET, 0);
    // SAFETY: QEMU's extended register in BAR 2.
    unsafe { mmio::write32(regs + BYTE_ORDER, LITTLE_ENDIAN) };
    dispi_write(regs, ENABLE, ENABLED | LFB_ENABLED);

    log(
        s,
        format_args!(
            "bochs-display at 00:{dev:02x}.{func}: {WIDTH}x{HEIGHT}, framebuffer {fb:#x}, registers {regs:#x}"
        ),
    );
    *out = Framebuffer {
        base: fb,
        width: u32::from(WIDTH),
        height: u32::from(HEIGHT),
        stride,
        reserved: 0,
    };
    DRIVES_A_DISPLAY
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    // SAFETY: the pointer is null until the entry point stores the services
    // Ambrosia passed it, which live until the entry point returns. Nothing
    // in the module runs after that, so a panic finds null or live
    // services.
    if let Some(s) = unsafe { SERVICES.load(Ordering::Relaxed).as_ref() } {
        log(s, format_args!("bochs-display: {info}"));
    }
    loop {
        core::hint::spin_loop();
    }
}
