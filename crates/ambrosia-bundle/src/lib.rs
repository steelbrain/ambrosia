//! Builds, checks and lists Ambrosia's configuration bundles: cpio archives
//! in the "newc" format whose first file is config.plist, which QEMU's
//! generic loader puts in the guest's RAM at 0x120000000.
//!
//! [`create`] writes a bundle the same way every time and checks it before
//! giving it; [`check`] checks one that exists. Both read the archive with
//! the firmware's own readers, from the `ambrosia` crate, so that a bundle
//! they accept is one the firmware reads and lays out. What depends on the
//! guest's kernel collection, such as the symbols a kext imports, shows only
//! at boot.

#![warn(missing_docs)]

pub mod check;
pub mod create;
mod error;
pub mod newc;

pub use check::{Summary, check, read};
pub use create::{Inputs, create};
pub use error::Error;
