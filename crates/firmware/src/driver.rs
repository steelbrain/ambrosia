//! Starts the graphics driver module the configuration names
//! (`ambrosia::graphics`), which gives the early console.

use crate::mmu;
use alloc::alloc::{Layout, alloc_zeroed};
use alloc::string::String;
use ambrosia::config::Driver;
use ambrosia::console;
use ambrosia::elf::{self, Module};
use ambrosia::graphics::{Entry, Framebuffer, REVISION, Services};
use core::fmt;

/// Why a graphics driver module could not be started.
pub enum Error {
    /// The module is not one Ambrosia loads.
    Elf(elf::Error),
    /// The heap can't hold the module.
    NoMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Elf(e) => write!(f, "{e}"),
            Error::NoMemory => f.write_str("it is too big for the heap"),
        }
    }
}

/// [`Services::log`]: a line of the module's in the boot log.
unsafe extern "C" fn log(_: &Services, text: *const u8, len: usize) {
    // A module written in C may pass a null `text` for an empty line, which
    // a slice may not be made from.
    let bytes = if len == 0 {
        &[][..]
    } else {
        // SAFETY: the interface asks the module for `len` readable bytes at
        // `text`, which it does not change during the call.
        unsafe { core::slice::from_raw_parts(text, len) }
    };
    log!("{}", String::from_utf8_lossy(bytes));
}

/// Loads the module and calls it, with the machine's ECAM and PCIe window
/// as (base, size). Gives its framebuffer, if it drives a display and the
/// framebuffer passes `console::check`; a module that drives nothing is no
/// error.
pub fn start(
    d: &Driver<'_>,
    ecam: (u64, u64),
    window: (u64, u64),
) -> Result<Option<Framebuffer>, Error> {
    let m = Module::parse(d.elf).map_err(Error::Elf)?;
    // The heap is RAM, which the identity map leaves executable. Nothing
    // frees the image: Ambrosia never calls the module again, and the
    // kernel takes over the heap's memory.
    let layout = Layout::from_size_align(m.size, m.align).map_err(|_| Error::NoMemory)?;
    // SAFETY: the layout's size is not zero: `Module::parse` accepts a module
    // only if its entry point lies inside a segment, below `m.size`.
    let base = unsafe { alloc_zeroed(layout) };
    if base.is_null() {
        return Err(Error::NoMemory);
    }
    // SAFETY: `m.size` bytes at `base`, just allocated and zeroed, so
    // initialised, and used through nothing else.
    let image = unsafe { core::slice::from_raw_parts_mut(base, m.size) };
    let entry = m.load(image, base as u64).map_err(Error::Elf)?;
    mmu::sync_code(base as u64, m.size as u64);
    log!(
        "graphics driver {}: {:#x} bytes at {:#x}",
        d.path,
        m.size,
        base as u64
    );

    let services = Services {
        revision: REVISION,
        reserved: 0,
        ecam_base: ecam.0,
        ecam_size: ecam.1,
        window_base: window.0,
        window_size: window.1,
        log,
    };
    let mut fb = Framebuffer::default();
    // SAFETY: the module is loaded, relocated and its code synced; its entry
    // point has the interface's signature, which the module promises.
    let status = unsafe {
        let entry = core::mem::transmute::<*const (), Entry>(entry as *const ());
        entry(&services, &mut fb)
    };
    if status != 0 {
        log!("graphics driver: no display ({status})");
        return Ok(None);
    }
    if let Err(e) = console::check(&fb, window) {
        log!(
            "graphics driver: its framebuffer, {}x{} at {:#x} with stride {}, can't be used: {e}",
            fb.width,
            fb.height,
            fb.base,
            fb.stride
        );
        return Ok(None);
    }
    Ok(Some(fb))
}
