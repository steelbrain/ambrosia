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
use crate::img4::{self, Payload};

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

/// The AP nonce is 32 bytes, the SHA-384-halved value the LocalPolicy binds
/// to.
const NONCE_LEN: usize = 32;

/// Whether `bundle` is a restore bundle: it carries the restore kernelcache,
/// which a configuration bundle never does.
pub fn present(bundle: &Bundle<'_>) -> bool {
    bundle.file(KERNELCACHE).is_some()
}

/// Why a restore bundle can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A member the restore boot needs is missing: the member's name.
    Missing(&'static str),
    /// A member that must be an IM4P isn't one: the member's name and why.
    Payload(&'static str, img4::Error),
    /// The nonce is the wrong length: the member's name and the length found.
    BadNonce(&'static str, usize),
    /// The boot arguments are not UTF-8: the member's name.
    BadBootArgs(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Missing(name) => {
                write!(f, "the restore bundle has no {name}")
            }
            Error::Payload(name, why) => {
                write!(f, "the restore {name} is not a valid IM4P: {why}")
            }
            Error::BadNonce(name, len) => write!(
                f,
                "the restore {name} is {len} bytes, not the {NONCE_LEN} an AP nonce needs"
            ),
            Error::BadBootArgs(name) => {
                write!(f, "the restore {name} is not UTF-8")
            }
        }
    }
}

/// The restore objects the firmware places, borrowing the bundle's bytes and
/// the ramdisk, which rides on its own because it is hundreds of megabytes.
/// The four images are IM4P payloads, parsed but not yet unpacked; the rest
/// are handed over whole, the way iBEC would receive them.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// The restore kernelcache's payload (`krnl`, lzfse).
    pub kernelcache: Payload<'a>,
    /// The restore device tree's payload (`dtre`, lzfse).
    pub device_tree: Payload<'a>,
    /// The restore trust cache's payload (`rtsc`), a version 1 trust cache.
    pub trust_cache: Payload<'a>,
    /// The ramdisk's payload (`rdsk`), the raw APFS container mounted as
    /// `md0`.
    pub ramdisk: Payload<'a>,
    /// The restore AP ticket, the DER of an IM4M.
    pub apticket: &'a [u8],
    /// The restore LocalPolicy, an IMG4.
    pub policy: &'a [u8],
    /// The 32-byte AP nonce the ticket is bound to.
    pub nonce: &'a [u8],
    /// The kernel's command line.
    pub boot_args: &'a str,
}

impl<'a> Input<'a> {
    /// Gathers the restore objects from `bundle` and the separately loaded
    /// `ramdisk`, checking that every member is present, that the four images
    /// parse as IM4Ps, that the nonce is the right length and that the boot
    /// arguments are text. It does not unpack the images or verify the
    /// ticket; the firmware does that as it places them.
    pub fn from(bundle: &Bundle<'a>, ramdisk: &'a [u8]) -> Result<Input<'a>, Error> {
        let member = |name| bundle.file(name).ok_or(Error::Missing(name));
        let image = |name| -> Result<Payload<'a>, Error> {
            img4::parse_im4p(member(name)?).map_err(|why| Error::Payload(name, why))
        };
        let kernelcache = image(KERNELCACHE)?;
        let device_tree = image(DEVICE_TREE)?;
        let trust_cache = image(TRUST_CACHE)?;
        let ramdisk = img4::parse_im4p(ramdisk).map_err(|why| Error::Payload(RAMDISK, why))?;
        let apticket = member(APTICKET)?;
        let policy = member(POLICY)?;
        let nonce = member(NONCE)?;
        if nonce.len() != NONCE_LEN {
            return Err(Error::BadNonce(NONCE, nonce.len()));
        }
        let boot_args =
            core::str::from_utf8(member(BOOT_ARGS)?).map_err(|_| Error::BadBootArgs(BOOT_ARGS))?;
        Ok(Input {
            kernelcache,
            device_tree,
            trust_cache,
            ramdisk,
            apticket,
            policy,
            nonce,
            boot_args,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::tests::archive;
    use crate::der;
    use alloc::vec::Vec;

    /// A DER element: `tag`, its length, then `contents`.
    fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
        let mut v = alloc::vec![tag];
        if contents.len() < 0x80 {
            v.push(contents.len() as u8);
        } else {
            let n = contents.len().to_be_bytes();
            let n = &n[n.iter().position(|&b| b != 0).unwrap()..];
            v.push(0x80 | n.len() as u8);
            v.extend_from_slice(n);
        }
        v.extend_from_slice(contents);
        v
    }

    /// A minimal IM4P of `kind` with a raw `payload`.
    fn im4p(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut c = tlv(der::IA5_STRING, b"IM4P");
        c.extend(tlv(der::IA5_STRING, kind));
        c.extend(tlv(der::IA5_STRING, b"test"));
        c.extend(tlv(der::OCTET_STRING, payload));
        tlv(der::SEQUENCE, &c)
    }

    /// A restore bundle whose members are all present and well-formed, with
    /// the ramdisk returned on its own as the firmware loads it.
    fn good() -> (Vec<u8>, Vec<u8>) {
        let archive = archive(&[
            (KERNELCACHE, 0o100644, &im4p(b"krnl", b"kc")),
            (DEVICE_TREE, 0o100644, &im4p(b"dtre", b"dt")),
            (TRUST_CACHE, 0o100644, &im4p(b"rtsc", b"tc")),
            (APTICKET, 0o100644, b"ticket"),
            (POLICY, 0o100644, b"policy"),
            (NONCE, 0o100644, &[7; NONCE_LEN]),
            (BOOT_ARGS, 0o100644, b"rd=md0 -restore"),
        ]);
        (archive, im4p(b"rdsk", b"ramdisk"))
    }

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

    #[test]
    fn input_gathers_members() {
        let (archive, ramdisk) = good();
        let bundle = Bundle::parse(&archive).unwrap();
        let input = Input::from(&bundle, &ramdisk).unwrap();
        assert_eq!(&input.kernelcache.kind, b"krnl");
        assert_eq!(input.kernelcache.data, b"kc");
        assert_eq!(&input.device_tree.kind, b"dtre");
        assert_eq!(&input.trust_cache.kind, b"rtsc");
        assert_eq!(&input.ramdisk.kind, b"rdsk");
        assert_eq!(input.ramdisk.data, b"ramdisk");
        assert_eq!(input.apticket, b"ticket");
        assert_eq!(input.policy, b"policy");
        assert_eq!(input.nonce, &[7; NONCE_LEN]);
        assert_eq!(input.boot_args, "rd=md0 -restore");
    }

    #[test]
    fn input_reports_a_missing_member() {
        for drop in [
            KERNELCACHE,
            DEVICE_TREE,
            TRUST_CACHE,
            APTICKET,
            POLICY,
            NONCE,
            BOOT_ARGS,
        ] {
            let (archive, ramdisk) = good();
            // Rebuild the bundle without the dropped member.
            let bundle = Bundle::parse(&archive).unwrap();
            let kept: Vec<_> = bundle
                .files
                .iter()
                .filter(|f| f.path != drop)
                .map(|f| (f.path, 0o100644u32, f.data))
                .collect();
            let archive = crate::bundle::tests::archive(&kept);
            let bundle = Bundle::parse(&archive).unwrap();
            assert_eq!(
                Input::from(&bundle, &ramdisk).unwrap_err(),
                Error::Missing(drop)
            );
        }
    }

    #[test]
    fn input_rejects_a_bad_image() {
        let ramdisk = im4p(b"rdsk", b"ramdisk");
        // A kernelcache that is valid DER but not an IM4P.
        let archive = archive(&[
            (KERNELCACHE, 0o100644, &[0x30, 0x02, 0x00, 0x00]),
            (DEVICE_TREE, 0o100644, &im4p(b"dtre", b"dt")),
            (TRUST_CACHE, 0o100644, &im4p(b"rtsc", b"tc")),
            (APTICKET, 0o100644, b"ticket"),
            (POLICY, 0o100644, b"policy"),
            (NONCE, 0o100644, &[7; NONCE_LEN]),
            (BOOT_ARGS, 0o100644, b"rd=md0 -restore"),
        ]);
        let bundle = Bundle::parse(&archive).unwrap();
        assert!(matches!(
            Input::from(&bundle, &ramdisk).unwrap_err(),
            Error::Payload(KERNELCACHE, _)
        ));
    }

    #[test]
    fn input_checks_the_nonce_length() {
        let (archive, ramdisk) = good();
        let bundle = Bundle::parse(&archive).unwrap();
        let short: Vec<_> = bundle
            .files
            .iter()
            .map(|f| {
                (
                    f.path,
                    0o100644u32,
                    if f.path == NONCE {
                        &[7u8; 16][..]
                    } else {
                        f.data
                    },
                )
            })
            .collect();
        let archive = crate::bundle::tests::archive(&short);
        let bundle = Bundle::parse(&archive).unwrap();
        assert_eq!(
            Input::from(&bundle, &ramdisk).unwrap_err(),
            Error::BadNonce(NONCE, 16)
        );
    }
}
