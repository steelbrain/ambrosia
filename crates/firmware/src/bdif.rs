//! The machine's firmware disk interface, vmapple's "backdoor" at 0x30000000
//! (QEMU's `hw/vmapple/bdif.c`). It reads the `root` and `aux` drives a
//! request at a time: the firmware writes a request's address to the drive's
//! command register, and the machine has filled the buffer and the status
//! byte when the store returns.

use ambrosia::block::{BlockDevice, SECTOR};
use core::ptr::{addr_of_mut, write_volatile};

const BASE: usize = 0x3000_0000;
const REG_CMD: usize = 0x408;

/// Reads go in requests of up to 16 MiB; the machine refuses one over
/// 128 MiB.
const MAX_REQUEST: usize = 16 << 20;

const FLAGS_READ: u32 = 0x0003_0001;
const STATUS_OK: u8 = 0;

/// A drive the interface reads. Its value is the offset of the drive's
/// registers from the interface's base.
#[derive(Clone, Copy)]
pub enum Drive {
    /// The guest's disk, with its partitions and volumes.
    Root = 0x0000_0000,
    /// The aux disk, which keeps the NVRAM.
    Aux = 0x0001_0000,
}

/// One buffer of a request: its address, its length and what the machine
/// does with it.
#[repr(C)]
struct Cmd {
    addr: u64,
    len: u32,
    flags: u32,
}

/// A request, as the machine reads it from the address written to the
/// command register: the sector to start at, the buffer to fill, and the
/// status byte to set.
#[repr(C)]
struct Request {
    sector: Cmd,
    data: Cmd,
    status: Cmd,
}

/// The sector a request starts at, in the layout the machine reads.
#[repr(C)]
struct Sector {
    pad: [u32; 2],
    sector: u32,
    pad2: u32,
}

/// A request with the sector and status it points to.
#[repr(C, align(64))]
struct Slot {
    req: Request,
    sector: Sector,
    status: u8,
}

static mut SLOT: Slot = Slot {
    req: Request {
        sector: Cmd {
            addr: 0,
            len: 0,
            flags: 0,
        },
        data: Cmd {
            addr: 0,
            len: 0,
            flags: 0,
        },
        status: Cmd {
            addr: 0,
            len: 0,
            flags: 0,
        },
    },
    sector: Sector {
        pad: [0; 2],
        sector: 0,
        pad2: 0,
    },
    status: 0,
};

/// A request the machine failed, by the sector it started at.
#[derive(Debug)]
pub struct ReadError {
    sector: u64,
}

impl core::fmt::Display for ReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the request at sector {} failed", self.sector)
    }
}

/// A drive, read as a [`BlockDevice`].
pub struct Disk(pub Drive);

impl BlockDevice for Disk {
    type Error = ReadError;

    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ReadError> {
        let mut done = 0;
        while done < buf.len() {
            let len = (buf.len() - done).min(MAX_REQUEST);
            let at = sector + (done / SECTOR) as u64;
            let sector32 = u32::try_from(at).map_err(|_| ReadError { sector: at })?;
            // SAFETY: `SLOT` is only used here, on one CPU, and `read` is
            // never reentered, so the reference is the only one. The
            // identity map makes each pointer its own physical address, as
            // the machine reads it. The machine reads the request during the
            // store to the command register, which is device memory in the
            // map, and before the store returns writes `len` bytes into
            // `buf[done..]`, which holds at least that many, and the status.
            unsafe {
                let s = &mut *addr_of_mut!(SLOT);
                s.sector.sector = sector32;
                s.status = 0xff;
                s.req.sector = Cmd {
                    addr: &s.sector as *const Sector as u64,
                    len: size_of::<Sector>() as u32,
                    flags: 0,
                };
                s.req.data = Cmd {
                    addr: buf[done..].as_mut_ptr() as u64,
                    len: len as u32,
                    flags: FLAGS_READ,
                };
                s.req.status = Cmd {
                    addr: &s.status as *const u8 as u64,
                    len: 1,
                    flags: 0,
                };
                core::arch::asm!("dsb sy");
                write_volatile(
                    (BASE + self.0 as usize + REG_CMD) as *mut u64,
                    &s.req as *const Request as u64,
                );
                core::arch::asm!("dsb sy");
                if core::ptr::read_volatile(&s.status) != STATUS_OK {
                    return Err(ReadError { sector: at });
                }
            }
            done += len;
        }
        Ok(())
    }
}
