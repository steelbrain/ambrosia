//! The log: vmapple's UART, a PL011 at 0x20010000, which iBoot prints to as
//! well, and the early console once a graphics driver gives one
//! (`screen.rs`). QEMU's PL011 needs no setup to transmit.

use core::fmt::{self, Write};
use core::ptr::{read_volatile, write_volatile};

const BASE: usize = 0x2001_0000;
const DR: usize = 0x00;
const FR: usize = 0x18;
const FR_TXFF: u32 = 1 << 5;

/// The UART, written a byte at a time.
pub struct Uart;

impl Uart {
    fn put(&mut self, byte: u8) {
        // SAFETY: the PL011's flag and data registers, aligned 32-bit
        // registers that are device memory both with the MMU off and in the
        // identity map. Reading the flags has no effect, and writing the
        // data register sends the byte.
        unsafe {
            while read_volatile((BASE + FR) as *const u32) & FR_TXFF != 0 {}
            write_volatile((BASE + DR) as *mut u32, u32::from(byte));
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.put(b'\r');
            }
            self.put(b);
        }
        Ok(())
    }
}

/// Writes log text to the UART, and to the screen once the early console
/// is up. The `log!` macro calls it.
pub fn print(args: fmt::Arguments<'_>) {
    let _ = Uart.write_fmt(args);
    crate::screen::print(args);
}

/// Logs a line, with Ambrosia's prefix: to the UART, and to the screen once
/// the early console is up.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::uart::print(format_args!("ambrosia: {}\n", format_args!($($arg)*)))
    };
}
