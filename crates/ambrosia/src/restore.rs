//! The restore bundle: the IPSW's restore objects, handed to the firmware
//! the way the configuration bundle is, to boot the installer's restore OS in
//! place of the installed system. Apple's `restored` then writes the guest's
//! LocalPolicies, so a disk an offline writer laid out gets policies no SEP
//! had to sign. Ambrosia runs no AVPBooter, iBSS or iBEC; it does iBEC's part,
//! placing these objects and entering the restore kernel.
//!
//! The restore kernelcache and device tree are the installed system's own
//! images, so the restore boot reuses the macOS kernel-collection load, the
//! pointer-authentication spins and the device-tree trimming. What differs is
//! the root: a ramdisk mounted as `md0`, a version 1 trust cache, the restore
//! boot arguments, and the restore manifest and policy, which are signed by
//! the production chain and need no configuration patches.
//!
//! The bundle carries every member but the ramdisk, which rides on its own
//! because it is hundreds of megabytes. The firmware takes the restore path
//! when the bundle holds [`KERNELCACHE`].

use crate::bundle::Bundle;

/// The restore kernelcache, an IM4P (`krnl`, lzfse): the image the installed
/// system boots.
pub const KERNELCACHE: &str = "restore-kernelcache.im4p";
/// The restore device tree, an IM4P (`dtre`, lzfse): the installed system's
/// template.
pub const DEVICE_TREE: &str = "restore-devicetree.im4p";
/// The restore trust cache, an IM4P (`rtsc`): a version 1 trust cache.
pub const TRUST_CACHE: &str = "restore-trustcache.im4p";
/// The restore ramdisk, an IM4P (`rdsk`) whose payload is the raw APFS
/// container mounted as `md0`. It is handed over on its own, not in the
/// archive, and the firmware places its payload below the kernel's data.
pub const RAMDISK: &str = "restore-ramdisk.im4p";
/// The restore AP ticket, the DER of an IM4M. Its hash is the boot manifest
/// hash, and the restore ticket is the SFR manifest.
pub const APTICKET: &str = "apticket.der";
/// The restore LocalPolicy, an IMG4: the empty `lpol` payload with its ticket.
pub const POLICY: &str = "restore-policy.img4";
/// The 32-byte AP nonce the ticket is bound to.
pub const NONCE: &str = "nonce";
/// The kernel's command line for the restore boot.
pub const BOOT_ARGS: &str = "boot-args";

/// Whether `bundle` is a restore bundle: it carries the restore kernelcache,
/// which a configuration bundle never does.
pub fn present(bundle: &Bundle<'_>) -> bool {
    bundle.file(KERNELCACHE).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::archive;

    #[test]
    fn present_only_for_restore() {
        let restore = archive(&[
            (KERNELCACHE, 0o100644, b"\x30\x82"),
            (BOOT_ARGS, 0o100644, b"rd=md0 -restore"),
            (NONCE, 0o100644, &[0; 32]),
        ]);
        assert!(present(&Bundle::parse(&restore).unwrap()));

        let config = archive(&[("config.plist", 0o100644, b"<dict/>")]);
        assert!(!present(&Bundle::parse(&config).unwrap()));
    }
}
