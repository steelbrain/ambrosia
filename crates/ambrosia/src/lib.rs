//! Ambrosia's core: everything it does at boot that doesn't touch the
//! machine. It reads the guest's disks through [`BlockDevice`], unpacks the
//! images iBoot would load, and lays out the kernel, its device tree and its
//! boot arguments in memory. The firmware crate gives it the machine.
//!
//! It is `no_std`, so it runs in the firmware, and it builds for the host
//! too, where its tests read real guest disks.

#![cfg_attr(not(test), no_std)]
#![warn(missing_docs)]

extern crate alloc;

pub mod adt;
pub mod apfs;
pub mod auxkc;
pub mod block;
pub mod boot;
pub mod bootargs;
pub mod bundle;
pub mod config;
pub mod console;
pub mod der;
pub mod elf;
pub mod files;
pub mod gpt;
pub mod graphics;
pub mod im4m;
pub mod img4;
pub mod kext;
pub mod macho;
pub mod nvram;
pub mod pac;
pub mod place;
pub mod plist;
pub mod sha384;
pub mod tree;
pub mod vtdisc;

pub use block::BlockDevice;
