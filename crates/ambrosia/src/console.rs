//! The early console: Ambrosia's log, drawn as text on the framebuffer a
//! graphics driver module gives (`graphics.rs`).
//!
//! The glyphs are font8x8's basic Latin ones, from the `font8x8` crate. The
//! bitmaps are in the public domain (Marcel Sondaar's font, as Daniel
//! Hepper's `font8x8_basic.h` carries it), and the crate is MIT-licensed.
//! Each is drawn at twice its size, 16 by 16 pixels, light grey on black: 64
//! columns by 48 rows at 1024 by 768.
//!
//! The framebuffer is device memory, so the console only writes it, a pixel
//! at a time. It never scrolls, which would read the whole screen back for
//! each line. A line too long for the screen wraps, and at the bottom the
//! console carries on from the top. Each row it moves to is cleared with
//! the row after it, so the newest line is followed by a blank one.

use crate::graphics::Framebuffer;
use core::fmt;
use font8x8::legacy::BASIC_LEGACY;

/// Where the console draws: the framebuffer's pixels, each set by its index
/// in 32-bit words from the framebuffer's base.
pub trait Pixels {
    /// Sets the pixel at `index` to `value`, in the framebuffer's format.
    fn set(&mut self, index: usize, value: u32);
}

const SCALE: usize = 2;
/// A character cell's width and height, in pixels.
const CELL: usize = 8 * SCALE;
const FOREGROUND: u32 = 0x00c0_c0c0;
const BACKGROUND: u32 = 0;

/// Why a module's framebuffer can't be drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unusable {
    /// It holds less than one character cell.
    TooSmall,
    /// Its base or stride is not a multiple of 4, or its stride is less
    /// than 4 bytes a pixel.
    BadLayout,
    /// It is not wholly inside the PCIe window.
    OutsideWindow,
}

impl fmt::Display for Unusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unusable::TooSmall => write!(
                f,
                "it is smaller than one character cell, {CELL} by {CELL} pixels"
            ),
            Unusable::BadLayout => f.write_str(
                "its base or stride is not a multiple of 4, or its stride is less than 4 \
                 bytes a pixel",
            ),
            Unusable::OutsideWindow => f.write_str("it is not wholly inside the PCIe window"),
        }
    }
}

/// Checks that Ambrosia can draw on `fb`: it must be in `window`, the PCIe
/// window as (base, size), where the module put its device's BARs.
pub fn check(fb: &Framebuffer, (base, size): (u64, u64)) -> Result<(), Unusable> {
    if (fb.width as usize) < CELL || (fb.height as usize) < CELL {
        return Err(Unusable::TooSmall);
    }
    if !fb.base.is_multiple_of(4)
        || !fb.stride.is_multiple_of(4)
        || u64::from(fb.stride) < 4 * u64::from(fb.width)
    {
        return Err(Unusable::BadLayout);
    }
    let len = u64::from(fb.stride) * u64::from(fb.height);
    match fb.base.checked_add(len) {
        Some(end) if fb.base >= base && end <= base + size => Ok(()),
        _ => Err(Unusable::OutsideWindow),
    }
}

/// The console, drawing through `P`.
pub struct Console<P> {
    pixels: P,
    width: usize,
    /// The stride in pixels.
    stride: usize,
    cols: usize,
    rows: usize,
    col: usize,
    row: usize,
}

impl<P: Pixels> Console<P> {
    /// A console on `fb`, which [`check`] passed, drawn through `pixels`. It
    /// clears the screen.
    pub fn new(pixels: P, fb: &Framebuffer) -> Self {
        let (width, height) = (fb.width as usize, fb.height as usize);
        let mut c = Console {
            pixels,
            width,
            stride: fb.stride as usize / 4,
            cols: width / CELL,
            rows: height / CELL,
            col: 0,
            row: 0,
        };
        for y in 0..height {
            c.fill(y, BACKGROUND);
        }
        c
    }

    fn fill(&mut self, y: usize, value: u32) {
        for x in 0..self.width {
            self.pixels.set(y * self.stride + x, value);
        }
    }

    fn clear_row(&mut self, row: usize) {
        for y in row * CELL..(row + 1) * CELL {
            self.fill(y, BACKGROUND);
        }
    }

    fn newline(&mut self) {
        self.col = 0;
        self.row = (self.row + 1) % self.rows;
        self.clear_row(self.row);
        self.clear_row((self.row + 1) % self.rows);
    }

    /// Draws `c`, a character of basic Latin, at the cursor.
    fn glyph(&mut self, c: u8) {
        if self.col == self.cols {
            self.newline();
        }
        let (x0, y0) = (self.col * CELL, self.row * CELL);
        // Each byte is a row of the glyph, its lowest bit the leftmost pixel.
        for (gy, bits) in BASIC_LEGACY[c as usize & 0x7f].iter().enumerate() {
            for y in y0 + gy * SCALE..y0 + (gy + 1) * SCALE {
                for x in 0..CELL {
                    let on = bits >> (x / SCALE) & 1 != 0;
                    let v = if on { FOREGROUND } else { BACKGROUND };
                    self.pixels.set(y * self.stride + x0 + x, v);
                }
            }
        }
        self.col += 1;
    }
}

impl<P: Pixels> fmt::Write for Console<P> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            match ch {
                '\n' => self.newline(),
                '\t' => self.glyph(b' '),
                ' '..='~' => self.glyph(ch as u8),
                _ => self.glyph(b'?'),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    struct Screen(Vec<u32>);

    impl Pixels for Screen {
        fn set(&mut self, index: usize, value: u32) {
            self.0[index] = value;
        }
    }

    /// 4 columns by 3 rows, with 8 pixels of padding a row.
    const FB: Framebuffer = Framebuffer {
        base: 0x8000_0000,
        width: 64,
        height: 48,
        stride: 72 * 4,
        reserved: 0,
    };
    const PADDING: u32 = 0x00ff_0000;

    fn at(s: &Screen, x: usize, y: usize) -> u32 {
        s.0[y * 72 + x]
    }

    /// The glyph in character cell (col, row), as its rows of bits.
    fn cell(s: &Screen, col: usize, row: usize) -> [u8; 8] {
        let mut g = [0u8; 8];
        for (gy, bits) in g.iter_mut().enumerate() {
            for gx in 0..8 {
                let (x, y) = (col * 16 + gx * 2, row * 16 + gy * 2);
                let v = at(s, x, y);
                // A glyph's pixel is all of its 2 by 2 square.
                assert_eq!(
                    [at(s, x + 1, y), at(s, x, y + 1), at(s, x + 1, y + 1)],
                    [v; 3]
                );
                *bits |= u8::from(v == FOREGROUND) << gx;
            }
        }
        g
    }

    fn glyph(c: u8) -> [u8; 8] {
        BASIC_LEGACY[c as usize]
    }

    #[test]
    fn draws_and_wraps() {
        let mut c = Console::new(Screen(vec![PADDING; 72 * 48]), &FB);
        write!(c, "Ab\u{e9}\t").unwrap();
        let s = &c.pixels;
        assert_eq!(cell(s, 0, 0), glyph(b'A'));
        assert_eq!(cell(s, 1, 0), glyph(b'b'));
        assert_eq!(cell(s, 2, 0), glyph(b'?'));
        assert_eq!(cell(s, 3, 0), [0; 8]);
        // The fifth character wraps to the next row.
        write!(c, "cde\nfg").unwrap();
        let s = &c.pixels;
        assert_eq!(cell(s, 0, 1), glyph(b'c'));
        assert_eq!(cell(s, 2, 1), glyph(b'e'));
        assert_eq!(cell(s, 1, 2), glyph(b'g'));
        // Starting the last row cleared the first, which comes after it.
        assert_eq!(cell(s, 0, 0), [0; 8]);
        // The next line goes at the top, and clears the row after it.
        write!(c, "\nh").unwrap();
        let s = &c.pixels;
        assert_eq!(cell(s, 0, 0), glyph(b'h'));
        assert_eq!(cell(s, 0, 1), [0; 8]);
        assert_eq!(cell(s, 1, 2), glyph(b'g'));
        // The padding past the width is never written.
        assert!((0..48).all(|y| at(s, 64, y) == PADDING && at(s, 63, y) != PADDING));
    }

    #[test]
    fn checks_the_framebuffer() {
        let window = (0x5000_0000, 0xb000_0000);
        assert_eq!(check(&FB, window), Ok(()));
        let bad = |f: fn(&mut Framebuffer)| {
            let mut fb = FB;
            f(&mut fb);
            check(&fb, window).unwrap_err()
        };
        assert_eq!(bad(|f| f.height = 15), Unusable::TooSmall);
        assert_eq!(bad(|f| f.stride = 63 * 4), Unusable::BadLayout);
        assert_eq!(bad(|f| f.stride = 72 * 4 + 2), Unusable::BadLayout);
        assert_eq!(bad(|f| f.base += 2), Unusable::BadLayout);
        assert_eq!(bad(|f| f.base = 0x1_0000_0000), Unusable::OutsideWindow);
        assert_eq!(bad(|f| f.base = 0xffff_f000), Unusable::OutsideWindow);
        assert_eq!(bad(|f| f.base = 0x4000_0000), Unusable::OutsideWindow);
        assert_eq!(bad(|f| f.base = u64::MAX - 3), Unusable::OutsideWindow);
    }
}
