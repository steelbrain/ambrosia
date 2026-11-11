//! The early console: once a graphics driver module has given a
//! framebuffer, the log is drawn on it as well as sent to the UART
//! (`ambrosia::console`).

use ambrosia::console::{Console, Pixels};
use ambrosia::graphics::Framebuffer;
use core::cell::UnsafeCell;
use core::fmt::{self, Write};

/// The framebuffer's pixels, in device memory.
struct Fb(*mut u32);

impl Pixels for Fb {
    fn set(&mut self, index: usize, value: u32) {
        // SAFETY: the console's indices stay inside the framebuffer it was
        // made on, and `start` takes only a framebuffer that
        // `console::check` found inside the PCIe window. The identity map
        // has the window as device memory, and the module left the
        // framebuffer's BAR decoding. The index is of 32-bit pixels, so
        // the address is aligned to 4.
        unsafe { crate::mmio::write32(self.0.add(index) as usize, value) }
    }
}

/// The console, once `start` has made it.
struct Screen(UnsafeCell<Option<Console<Fb>>>);

// SAFETY: the firmware runs on one CPU, with interrupts masked, so no two
// threads ever reach the console.
unsafe impl Sync for Screen {}

static SCREEN: Screen = Screen(UnsafeCell::new(None));

/// Starts the console on `fb` and clears the screen.
///
/// # Safety
///
/// `fb` must have passed [`ambrosia::console::check`] for the PCIe window it
/// lies in, so its base points at a framebuffer of its width, height and
/// stride that the console may write.
pub unsafe fn start(fb: &Framebuffer) {
    let c = Console::new(Fb(fb.base as *mut u32), fb);
    // SAFETY: one CPU, and `print` holds no reference to the console
    // between calls, so this write is the only access.
    unsafe { *SCREEN.0.get() = Some(c) };
}

/// Draws log text, if the console is up.
pub fn print(args: fmt::Arguments<'_>) {
    // The console is taken out while it draws, so a line logged meanwhile,
    // by an exception or a panic in the drawing, goes only to the UART.
    // SAFETY: one CPU; the reference lives only for the take.
    let Some(mut c) = (unsafe { (*SCREEN.0.get()).take() }) else {
        return;
    };
    let _ = c.write_fmt(args);
    // SAFETY: as above.
    unsafe { *SCREEN.0.get() = Some(c) };
}
