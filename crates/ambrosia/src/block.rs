//! Disks, as Ambrosia reads them: whole 512-byte sectors.

/// The size of a sector, in bytes.
pub const SECTOR: usize = 512;

/// A disk that reads whole sectors.
pub trait BlockDevice {
    /// The error a failed read gives.
    type Error: core::fmt::Debug;

    /// Reads `buf.len() / SECTOR` sectors, from sector `sector` on, into
    /// `buf`, whose length callers keep a multiple of [`SECTOR`]. Sector
    /// numbers come from the disk's own tables unchecked, so a read the disk
    /// can't serve, such as one past its end, gives an error, not a panic.
    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), Self::Error>;
}

/// A borrowed disk reads as the disk does, so one disk can serve each of its
/// partitions in turn.
impl<D: BlockDevice + ?Sized> BlockDevice for &mut D {
    type Error = D::Error;

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), Self::Error> {
        (**self).read(sector, buf)
    }
}
