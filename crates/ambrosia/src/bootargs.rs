//! XNU's arm64 boot arguments, `boot_args` revision 3, version 2, which the
//! firmware passes in x0 at kernel entry.
//!
//! The structure has two layouts, which differ in the length of the command
//! line and so in where the two fields after it, `bootFlags` and
//! `memSizeActual`, lie. The 12.6 and 13.6 kernels copy 736 bytes of it,
//! with a 608-byte command line and the two fields at 720 and 728. The
//! kernels from 14.6.1 to 27.2.1 copy 1152 bytes, with a 1024-byte command
//! line and the two fields at 1136 and 1144. [`BootArgs::write`] fills one
//! block that reads right in both: the command line ends within the shorter
//! field, the shorter layout's two fields are zero, as they lie inside the
//! longer layout's command line, and the longer layout's carry the values.

/// The length of the command line field in the shorter layout, which bounds
/// the command line in both.
pub const COMMAND_LINE: usize = 608;

/// Where the longer layout keeps `memSizeActual`. Its `bootFlags`, at 1136,
/// is zero.
const MEM_SIZE_ACTUAL: usize = 1144;

/// The framebuffer the kernel's early console draws on, if any: XNU's
/// `Boot_Video`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Video {
    /// The framebuffer's physical address, `v_baseAddr`.
    pub base: u64,
    /// The display code, `v_display`.
    pub display: u64,
    /// The bytes from the start of one row to the next, `v_rowBytes`.
    pub row_bytes: u64,
    /// The width in pixels, `v_width`.
    pub width: u64,
    /// The height in pixels, `v_height`.
    pub height: u64,
    /// The bits per pixel in the low byte, and the scale less 1 in bits 16
    /// to 23, `v_depth`.
    pub depth: u64,
}

/// XNU's `boot_args`, without the revision, the version and the command
/// line, which [`BootArgs::write`] adds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BootArgs {
    /// The kernel virtual address `phys_base` maps to.
    pub virt_base: u64,
    /// Where the kernel's memory starts.
    pub phys_base: u64,
    /// How much memory the kernel has from `phys_base` on.
    pub mem_size: u64,
    /// The end of what the firmware placed for the kernel.
    pub top_of_kernel_data: u64,
    /// The framebuffer for the kernel's early console.
    pub video: Video,
    /// The machine type, `machineType`.
    pub machine_type: u32,
    /// The device tree, at its kernel virtual address.
    pub device_tree: u64,
    /// The device tree's length in bytes.
    pub device_tree_length: u32,
    /// The size of all of RAM, `memSizeActual`, which must equal the device
    /// tree's `/chosen/dram-size`: the 27.2.1 kernel panics where they
    /// differ. Only the longer layout carries it. The 12.6 and 13.6 kernels
    /// read 0, which has them take `mem_size`.
    pub mem_size_actual: u64,
}

/// The bytes [`BootArgs::write`] fills, the longer layout's size.
pub const SIZE: usize = 1152;

impl BootArgs {
    /// Writes the structure, with `command_line`, into `out`, in both
    /// layouts at once. The command line is cut to 607 bytes, to keep its
    /// NUL within the shorter layout's field. `bootFlags` is 0 in both.
    pub fn write(&self, command_line: &str, out: &mut [u8; SIZE]) {
        out.fill(0);
        let mut put = |at: usize, b: &[u8]| out[at..at + b.len()].copy_from_slice(b);
        put(0, &3u16.to_le_bytes());
        put(2, &2u16.to_le_bytes());
        put(8, &self.virt_base.to_le_bytes());
        put(16, &self.phys_base.to_le_bytes());
        put(24, &self.mem_size.to_le_bytes());
        put(32, &self.top_of_kernel_data.to_le_bytes());
        let v = self.video;
        for (i, f) in [v.base, v.display, v.row_bytes, v.width, v.height, v.depth]
            .iter()
            .enumerate()
        {
            put(40 + 8 * i, &f.to_le_bytes());
        }
        put(88, &self.machine_type.to_le_bytes());
        put(96, &self.device_tree.to_le_bytes());
        put(104, &self.device_tree_length.to_le_bytes());
        let cl = command_line.as_bytes();
        put(108, &cl[..cl.len().min(COMMAND_LINE - 1)]);
        // The shorter layout's bootFlags and memSizeActual, at 720 and 728,
        // stay zero: in the longer layout they are command line, past its NUL.
        put(MEM_SIZE_ACTUAL, &self.mem_size_actual.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let a = BootArgs {
            virt_base: 0xfffffe00_16a04000,
            phys_base: 0x70a04000,
            mem_size: 0x1_feb38000,
            top_of_kernel_data: 0x783a8000,
            device_tree: 0xfffffe00_194a8000,
            device_tree_length: 0x8c000,
            mem_size_actual: 0x2_0000_0000,
            ..Default::default()
        };
        let mut b = [0u8; SIZE];
        a.write("-v", &mut b);
        let u64_at = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
        assert_eq!(u16::from_le_bytes([b[2], b[3]]), 2);
        assert_eq!(u64_at(8), a.virt_base);
        assert_eq!(u64_at(16), a.phys_base);
        assert_eq!(u64_at(32), a.top_of_kernel_data);
        assert_eq!(u64_at(0x60), a.device_tree);
        assert_eq!(
            u32::from_le_bytes(b[0x68..0x6c].try_into().unwrap()),
            0x8c000
        );
        assert_eq!(&b[108..111], b"-v\0");
        // The shorter layout: bootFlags and memSizeActual are 0.
        assert_eq!((u64_at(720), u64_at(728)), (0, 0));
        // The longer layout: bootFlags is 0, memSizeActual all of RAM, and
        // its command line is zero past the NUL.
        assert_eq!((u64_at(1136), u64_at(1144)), (0, 0x2_0000_0000));
        assert!(b[111..1136].iter().all(|&x| x == 0));
    }

    /// A command line too long for the shorter layout's field is cut to keep
    /// its NUL there, so neither layout's fields after it are touched.
    #[test]
    fn long_command_line() {
        let a = BootArgs {
            mem_size_actual: 7,
            ..Default::default()
        };
        let mut b = [0u8; SIZE];
        a.write(&"x".repeat(1000), &mut b);
        assert!(b[108..108 + 607].iter().all(|&x| x == b'x'));
        assert!(b[108 + 607..1144].iter().all(|&x| x == 0));
        assert_eq!(u64::from_le_bytes(b[1144..].try_into().unwrap()), 7);
    }
}
