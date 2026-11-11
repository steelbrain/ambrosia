//! The Ambrosia image. vmapple runs it in place of AVPBooter, from its
//! firmware region at 0x00100000, on the boot CPU at EL1 with the MMU off.

#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod uart;
mod bdif;
mod driver;
mod heap;
mod keys;
mod mmio;
mod mmu;
mod screen;

use ambrosia::boot::{self, Memory};
use ambrosia::bundle::{self, Bundle};
use ambrosia::config::Config;
use ambrosia::files;
use ambrosia::tree::Machine;
use core::arch::{asm, global_asm};

global_asm!(include_str!("start.s"));

// The linker script reaches the linker through .cargo/config.toml, which
// cargo doesn't watch. Including it here makes cargo relink when it changes.
const _: &str = include_str!("../link.ld");

unsafe extern "C" {
    fn enter_kernel(entry: u64, boot_args: u64) -> !;
}

/// Where the configuration bundle is, if one is given: QEMU's generic loader
/// puts it there (`-device loader,file=<bundle>,addr=0x120000000,force-raw=on`).
/// It is 512 MiB into RAM, above what the boot lays out, and may run up to
/// the heap at 1 GiB.
const BUNDLE_AT: u64 = mmu::RAM_BASE + (512 << 20);
// That space is the most the bundle's reader takes, `bundle::MAX_LEN`.
const _: () = assert!(BUNDLE_AT + bundle::MAX_LEN as u64 == mmu::RAM_BASE + (1 << 30));

#[global_allocator]
static HEAP: heap::Bump = heap::Bump::new();

/// vmapple's PCIe ECAM, buses 0 to 255: base and size.
const ECAM: (u64, u64) = (0x4000_0000, 0x1000_0000);

/// vmapple's PCIe MMIO window: base and size. The kernel places every BAR
/// below 4 GiB, aligned to its size, so the window runs from the end of ECAM
/// up to RAM, which starts at 4 GiB.
const PCIE_WINDOW: (u64, u64) = (0x5000_0000, mmu::RAM_BASE - 0x5000_0000);

/// Whether the machine has avp-rtc, the clock macOS keeps its offset to
/// calendar time with, at 0x30240000: its ID register, at +0x40, reads
/// 0xf001, the value on which iBoot keeps it in the device tree. A machine
/// without it reads 0 there, and the guest keeps the PL031
/// (docs/machine.md, "avp-rtc").
fn avp_rtc() -> bool {
    // SAFETY: device memory in the identity map; the read has no effect.
    unsafe { mmio::read32(0x3024_0040) == 0xf001 }
}

/// The machine's config block (QEMU's `hw/vmapple/cfg.c`).
mod cfg {
    use alloc::string::String;

    const BASE: usize = 0x0040_0000;

    fn read<T: Copy>(off: usize) -> T {
        // SAFETY: the config block is 64 KiB of RAM the machine fills.
        unsafe { core::ptr::read_volatile((BASE + off) as *const T) }
    }

    /// A NUL-padded string field of 32 bytes.
    fn string(off: usize) -> String {
        let b: [u8; 32] = read(off);
        let end = b.iter().position(|&c| c == 0).unwrap_or(32);
        String::from_utf8_lossy(&b[..end]).into()
    }

    /// The number of CPUs the machine has.
    pub fn nr_cpus() -> u32 {
        read(0x04)
    }
    /// The machine's ECID, its unique chip identifier.
    pub fn ecid() -> u64 {
        read(0x18)
    }
    /// The size of guest RAM in bytes.
    pub fn ram_size() -> u64 {
        read(0x20)
    }
    /// The machine's random seed, drawn once per VM process.
    pub fn rnd() -> u32 {
        read(0x38)
    }
    /// The machine's serial number.
    pub fn serial() -> String {
        string(0x500)
    }
    /// The machine's region code.
    pub fn region() -> String {
        string(0x520)
    }
    /// The machine's model string, such as `VM0001`.
    pub fn model() -> String {
        string(0x540)
    }
    /// The SoC name, such as `Apple M1 (Virtual)`.
    pub fn soc_name() -> String {
        string(0x584)
    }
}

/// Guest RAM, as the boot writes it: identity mapped, less the stretch the
/// firmware itself still uses.
struct Ram {
    start: u64,
    end: u64,
    /// `[start, end)` of RAM the boot may not write: the bundle, which the
    /// configuration borrows from, and the heap.
    kept: (u64, u64),
}

impl Memory for Ram {
    fn bytes(&mut self, pa: u64, len: usize) -> Option<&mut [u8]> {
        let end = pa.checked_add(len as u64)?;
        if pa < self.start || end > self.end || (pa < self.kept.1 && end > self.kept.0) {
            return None;
        }
        // SAFETY: RAM the firmware owns until the kernel runs, outside the
        // bundle and the heap, which are all it reads or writes otherwise;
        // the boot asks for each range while it holds no other slice of it.
        Some(unsafe { core::slice::from_raw_parts_mut(pa as *mut u8, len) })
    }
}

fn timer() -> u64 {
    let t: u64;
    // SAFETY: reads the virtual counter.
    unsafe { asm!("mrs {}, cntvct_el0", out(reg) t) };
    t
}

#[unsafe(no_mangle)]
extern "C" fn main() -> ! {
    log!("Ambrosia {}", env!("CARGO_PKG_VERSION"));
    let ram_base = mmu::RAM_BASE;
    let ram_end = ram_base + cfg::ram_size();
    log!(
        "{} CPUs, RAM {:#x}-{:#x}, ECID {:#x}",
        cfg::nr_cpus(),
        ram_base,
        ram_end,
        cfg::ecid()
    );
    mmu::enable(ram_end);

    // The heap is a GiB from RAM's second GiB on: the kernel's static memory
    // fits below it, and the carveouts at the top of RAM above it. A guest
    // with less than the firmware needs would wrap `ram_end - 64 MiB`, so it
    // saturates; `HEAP.init` then gets an empty range and every allocation
    // fails, which halts the boot rather than corrupting memory.
    let heap_start = ram_base + (1 << 30);
    let heap_end = (heap_start + (1 << 30)).min(ram_end.saturating_sub(64 << 20));
    // SAFETY: called once, here, before the first allocation.
    unsafe { HEAP.init(heap_start as usize, heap_end as usize) };

    // The bundle's stretch of RAM, from BUNDLE_AT to the heap, cut short if
    // RAM ends first. Nothing writes it while the configuration borrows from
    // it: the heap is above it, and `Ram` refuses the boot any range in it.
    let bundle_len = heap_start.min(ram_end).saturating_sub(BUNDLE_AT);
    // SAFETY: mapped RAM, as above, which nothing changes while the slice
    // lives.
    let bundle_mem =
        unsafe { core::slice::from_raw_parts(BUNDLE_AT as *const u8, bundle_len as usize) };
    let config = match Bundle::find(bundle_mem).map(|b| b.map(|b| (Config::read(&b), b.len))) {
        None => Config::default(),
        Some(Ok((Ok(c), len))) => {
            log!(
                "configuration: {len} bytes, {} kexts, {} patches, graphics driver {}",
                c.kexts.len(),
                c.patches.len(),
                c.driver.as_ref().map_or("none", |d| &d.path)
            );
            c
        }
        Some(Ok((Err(e), _))) => {
            log!("bad configuration: {e}");
            halt()
        }
        Some(Err(e)) => {
            log!("bad configuration bundle: {e}");
            halt()
        }
    };

    // The graphics driver, before any disk read, so the early console shows
    // the rest of the boot.
    let framebuffer = match &config.driver {
        None => None,
        Some(d) => match driver::start(d, ECAM, PCIE_WINDOW) {
            Ok(fb) => fb,
            Err(e) => {
                log!("bad graphics driver {}: {e}", d.path);
                halt()
            }
        },
    };
    if let Some(fb) = &framebuffer {
        // SAFETY: `driver::start` returns a framebuffer only once it has
        // passed `console::check` for the PCIe window.
        unsafe { screen::start(fb) };
        log!(
            "Ambrosia {}: early console, {}x{} at {:#x}",
            env!("CARGO_PKG_VERSION"),
            fb.width,
            fb.height,
            fb.base
        );
    }

    let m = Machine {
        ram_base,
        ram_size: ram_end - ram_base,
        pcie_window: PCIE_WINDOW,
        cpus: cfg::nr_cpus(),
        ecid: cfg::ecid(),
        rnd: cfg::rnd(),
        serial: cfg::serial(),
        model: cfg::model(),
        region: cfg::region(),
        soc_name: cfg::soc_name(),
        display: framebuffer,
        display_node: config.display_node.clone(),
        avp_rtc: avp_rtc(),
    };

    let found = match files::read_all(
        &mut bdif::Disk(bdif::Drive::Root),
        &mut bdif::Disk(bdif::Drive::Aux),
        &mut |a| log!("{a}"),
    ) {
        Ok(f) => f,
        Err(e) => {
            log!("can't read the boot objects: {e}");
            halt()
        }
    };
    let seed = boot::seed(m.rnd, timer());
    let mut ram = Ram {
        start: ram_base,
        end: ram_end,
        kept: (BUNDLE_AT, heap_end.max(heap_start)),
    };

    // The kernel and every process run on these keys. The firmware uses no
    // pointer authentication itself, so changing the keys under it is
    // harmless.
    keys::set(&boot::pac_keys(&seed));
    let handoff = match boot::prepare(
        &m,
        &found.files(),
        &config,
        &mut ram,
        BUNDLE_AT,
        &seed,
        &mut |a| log!("{a}"),
    ) {
        Ok(h) => h,
        Err(e) => {
            log!("boot failed: {e}");
            halt()
        }
    };

    for (start, len) in handoff.written {
        mmu::clean(start, len);
    }
    let (start, high) = HEAP.used();
    mmu::clean(start as u64, (high - start) as u64);
    log!("entering the kernel at {:#x}", handoff.entry);
    // SAFETY: the kernel is placed and its memory clean; this never returns.
    unsafe { enter_kernel(handoff.entry, handoff.boot_args) }
}

fn halt() -> ! {
    loop {
        // SAFETY: waits for an interrupt, which never comes.
        unsafe { asm!("wfi") };
    }
}

/// Reports an exception the image took, from `start.s`'s vectors.
#[unsafe(no_mangle)]
extern "C" fn exception(index: u64, regs: *const [u64; 31]) -> ! {
    let (esr, elr, far): (u64, u64, u64);
    // SAFETY: reads the exception's syndrome registers.
    unsafe {
        asm!("mrs {}, esr_el1", out(reg) esr);
        asm!("mrs {}, elr_el1", out(reg) elr);
        asm!("mrs {}, far_el1", out(reg) far);
    }
    log!("exception {index}: ESR {esr:#x} ELR {elr:#x} FAR {far:#x}");
    // SAFETY: the vector saved x0 to x30 there.
    let regs = unsafe { &*regs };
    for (i, pair) in regs.chunks(2).enumerate() {
        match pair {
            [a, b] => log!("x{:<2} {a:#018x}  x{:<2} {b:#018x}", 2 * i, 2 * i + 1),
            [a] => log!("x{:<2} {a:#018x}", 2 * i),
            _ => {}
        }
    }
    halt()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    log!("panic: {info}");
    halt()
}
