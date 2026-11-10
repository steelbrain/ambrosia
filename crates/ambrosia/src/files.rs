//! The boot objects, read from the guest's disks where iBoot finds them.
//!
//! The root disk's GPT has a main APFS container and an iBoot system
//! container. In the main one, the System volume's volume group names the
//! macOS install, and its Preboot volume keeps the install's boot objects
//! under `/<group>/boot/<hash>/`, where `/<group>/boot/active` names the
//! hash. The iBoot system container's Preboot volume keeps the install's
//! LocalPolicy under `/<group>/LocalPolicy/` and the SFR manifest under
//! `/SFR/current/`. The aux disk keeps NVRAM, where the device tree template
//! says.

use crate::apfs::{self, Container, Role, Volume};
use crate::block::{BlockDevice, SECTOR};
use crate::boot::{self, Files};
use crate::gpt::{self, APFS_TYPE, Gpt, ISC_TYPE};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// The most bytes a boot object may hold. The largest, the kernel
/// collection, is tens of MiB; this only bounds a corrupt inode's size, which
/// sizes the buffer the file is read into.
const MAX_FILE_LEN: u64 = 256 << 20;

/// The most bytes of NVRAM region read from the aux disk. The templates give
/// a region of 1 MiB; this only bounds a corrupt template's, whose length
/// sizes the buffer the region is read into.
const MAX_NVRAM_LEN: u64 = 16 << 20;

/// The boot objects, read whole. A path is in the boot directory,
/// `/<volume_group>/boot/<boot_dir>/` of the main container's Preboot
/// volume, unless its field says otherwise.
pub struct BootFiles {
    /// The UUID of the install's volume group, which names its directories
    /// in both Preboot volumes.
    pub volume_group: String,
    /// The name of the boot directory, as `/<group>/boot/active` gives it.
    pub boot_dir: String,
    /// `System/Library/Caches/apticket.der`, the boot manifest.
    pub manifest: Vec<u8>,
    /// `System/Library/Caches/com.apple.kernelcaches/kernelcache`, the
    /// kernel collection.
    pub kernel: Vec<u8>,
    /// `usr/standalone/firmware/devicetree.img4`, the device tree template.
    pub device_tree: Vec<u8>,
    /// `usr/standalone/firmware/root_hash.img4`, the System volume's root
    /// hash.
    pub root_hash: Vec<u8>,
    /// `usr/standalone/firmware/base_system_root_hash.img4`, the base
    /// system's root hash.
    pub base_root_hash: Vec<u8>,
    /// `usr/standalone/firmware/FUD/StaticTrustCache.img4`, a trust cache.
    pub static_trust_cache: Vec<u8>,
    /// `usr/standalone/firmware/FUD/BaseSystemTrustCache.img4`, the base
    /// system's trust cache.
    pub base_trust_cache: Vec<u8>,
    /// The install's LocalPolicy, from `/<group>/LocalPolicy/` in the iBoot
    /// system container's Preboot volume.
    pub policy: Vec<u8>,
    /// `/SFR/current/apticket.der` in the iBoot system container's Preboot
    /// volume, the SFR manifest.
    pub sfr_manifest: Vec<u8>,
    /// The aux disk's NVRAM region, every bank of it.
    pub nvram_region: Vec<u8>,
}

impl BootFiles {
    /// The files, borrowed, as [`boot::prepare`] takes them.
    pub fn files(&self) -> Files<'_> {
        Files {
            volume_group: &self.volume_group,
            boot_dir: &self.boot_dir,
            manifest: &self.manifest,
            kernel: &self.kernel,
            device_tree: &self.device_tree,
            root_hash: &self.root_hash,
            base_root_hash: &self.base_root_hash,
            static_trust_cache: &self.static_trust_cache,
            base_trust_cache: &self.base_trust_cache,
            policy: &self.policy,
            sfr_manifest: &self.sfr_manifest,
            nvram_region: &self.nvram_region,
        }
    }
}

/// Why the boot objects could not be read.
#[derive(Debug)]
pub enum Error<E> {
    /// The root disk's partition table can't be read.
    Gpt(gpt::Error<E>),
    /// An APFS container, volume, directory or file can't be read.
    Apfs(apfs::Error<E>),
    /// The root disk has no partition of this kind, or more than one.
    Partition(&'static str),
    /// The container has no volume of this role, or more than one.
    Volume(Role),
    /// The main container's System volume is in no volume group.
    NoGroup,
    /// `/<group>/boot/active` doesn't name a boot directory.
    BadActive,
    /// `/<group>/LocalPolicy/` has no policy but the recovery one's, or more
    /// than one.
    Policy,
    /// A file is larger than 256 MiB, more than any boot object.
    TooLarge {
        /// The file's path in its volume.
        path: String,
        /// The file's size in bytes, as its inode gives it.
        size: u64,
    },
    /// The device tree template can't be unpacked, or gives no NVRAM region
    /// on the aux disk.
    Nvram(boot::Error),
    /// The template's NVRAM region doesn't start and end on sector
    /// boundaries, or is larger than 16 MiB.
    BadNvramRegion {
        /// Where the region starts, in bytes from the aux disk's start.
        at: u64,
        /// The region's length in bytes.
        len: u64,
    },
    /// The aux disk failed to read NVRAM.
    Aux(E),
}

impl<E: fmt::Display> fmt::Display for Error<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Gpt(e) => write!(f, "{e}"),
            Error::Apfs(e) => write!(f, "{e}"),
            Error::Partition(kind) => write!(f, "the disk has no single {kind} partition"),
            Error::Volume(role) => write!(f, "the container has no single {role} volume"),
            Error::NoGroup => f.write_str("the System volume is in no volume group"),
            Error::BadActive => f.write_str("boot/active names no boot directory"),
            Error::Policy => f.write_str("LocalPolicy holds no single policy"),
            Error::TooLarge { path, size } => write!(
                f,
                "{path} is {size:#x} bytes, larger than {MAX_FILE_LEN:#x} bytes"
            ),
            Error::Nvram(e) => write!(f, "no NVRAM region: {e}"),
            Error::BadNvramRegion { at, len } => write!(
                f,
                "the NVRAM region at {at:#x}, {len:#x} bytes, is not whole sectors \
                 or is larger than {MAX_NVRAM_LEN:#x} bytes"
            ),
            Error::Aux(e) => write!(f, "the aux disk failed to read NVRAM: {e}"),
        }
    }
}

impl<E> From<apfs::Error<E>> for Error<E> {
    fn from(e: apfs::Error<E>) -> Self {
        Error::Apfs(e)
    }
}

/// The first sector of the table's one partition of type `kind`. `what`
/// names the type for the error if there is none or more than one.
fn partition<E>(table: &Gpt, kind: &str, what: &'static str) -> Result<u64, Error<E>> {
    let mut found = table
        .partitions
        .iter()
        .filter(|p| p.type_guid.to_string() == kind);
    match (found.next(), found.next()) {
        (Some(p), None) => Ok(p.first_lba),
        _ => Err(Error::Partition(what)),
    }
}

/// The index of the container's one volume of this role.
fn volume<D: BlockDevice>(c: &Container<D>, role: Role) -> Result<usize, Error<D::Error>> {
    let mut found = c
        .volumes()
        .iter()
        .enumerate()
        .filter(|(_, v)| v.role == role);
    match (found.next(), found.next()) {
        (Some((i, _)), None) => Ok(i),
        _ => Err(Error::Volume(role)),
    }
}

/// The whole file at `path` in volume `v`.
fn read<D: BlockDevice>(v: &mut Volume<'_, D>, path: &str) -> Result<Vec<u8>, Error<D::Error>> {
    let file = v.lookup(path)?;
    if file.size > MAX_FILE_LEN {
        return Err(Error::TooLarge {
            path: path.to_string(),
            size: file.size,
        });
    }
    Ok(v.read(&file)?)
}

/// Reads the boot objects of the one macOS install on `root`, and NVRAM
/// from `aux`. `log` takes progress lines for the boot log.
pub fn read_all<D: BlockDevice>(
    root: &mut D,
    aux: &mut D,
    log: &mut dyn FnMut(fmt::Arguments<'_>),
) -> Result<BootFiles, Error<D::Error>> {
    let table = Gpt::read(root).map_err(Error::Gpt)?;
    let main_at = partition(&table, APFS_TYPE, "APFS")?;
    let isc_at = partition(&table, ISC_TYPE, "iBoot system container")?;

    let mut main = Container::open(&mut *root, main_at)?;
    let system = volume(&main, Role::System)?;
    let group = main.volumes()[system].group.ok_or(Error::NoGroup)?;
    let volume_group = group.to_string();
    let mut preboot = main.volume(volume(&main, Role::Preboot)?)?;
    let active = read(&mut preboot, &format!("/{volume_group}/boot/active"))?;
    let boot_dir = core::str::from_utf8(&active)
        .ok()
        .map(str::trim)
        .filter(|h| !h.is_empty() && !h.contains('/'))
        .ok_or(Error::BadActive)?
        .to_string();
    log(format_args!(
        "macOS {volume_group}, boot objects {boot_dir}"
    ));
    let dir = format!("/{volume_group}/boot/{boot_dir}");
    let mut boot_file = |path: &str| read(&mut preboot, &format!("{dir}/{path}"));
    let fw = "usr/standalone/firmware";
    let manifest = boot_file("System/Library/Caches/apticket.der")?;
    let kernel = boot_file("System/Library/Caches/com.apple.kernelcaches/kernelcache")?;
    let device_tree = boot_file(&format!("{fw}/devicetree.img4"))?;
    let root_hash = boot_file(&format!("{fw}/root_hash.img4"))?;
    let base_root_hash = boot_file(&format!("{fw}/base_system_root_hash.img4"))?;
    let static_trust_cache = boot_file(&format!("{fw}/FUD/StaticTrustCache.img4"))?;
    let base_trust_cache = boot_file(&format!("{fw}/FUD/BaseSystemTrustCache.img4"))?;

    // The policy is named for its nonce's hash. Beside it is the one
    // recoveryOS boots with.
    let mut isc = Container::open(&mut *root, isc_at)?;
    let mut isc_preboot = isc.volume(volume(&isc, Role::Preboot)?)?;
    let policies = format!("/{volume_group}/LocalPolicy");
    let listing = isc_preboot.lookup(&policies)?;
    let mut names = isc_preboot
        .read_dir(&listing)?
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.ends_with(".img4") && !n.ends_with(".recovery.img4"));
    let name = match (names.next(), names.next()) {
        (Some(n), None) => n,
        _ => return Err(Error::Policy),
    };
    let policy = read(&mut isc_preboot, &format!("{policies}/{name}"))?;
    let sfr_manifest = read(&mut isc_preboot, "/SFR/current/apticket.der")?;

    let (at, len) = boot::nvram_location(&device_tree).map_err(Error::Nvram)?;
    let nvram_region = read_nvram(aux, at, len)?;

    Ok(BootFiles {
        volume_group,
        boot_dir,
        manifest,
        kernel,
        device_tree,
        root_hash,
        base_root_hash,
        static_trust_cache,
        base_trust_cache,
        policy,
        sfr_manifest,
        nvram_region,
    })
}

/// Reads the NVRAM region, `len` bytes from byte `at` of the aux disk.
fn read_nvram<D: BlockDevice>(aux: &mut D, at: u64, len: u64) -> Result<Vec<u8>, Error<D::Error>> {
    let sector = SECTOR as u64;
    if !at.is_multiple_of(sector) || !len.is_multiple_of(sector) || len > MAX_NVRAM_LEN {
        return Err(Error::BadNvramRegion { at, len });
    }
    let mut region = vec![0; len as usize];
    aux.read(at / sector, &mut region).map_err(Error::Aux)?;
    Ok(region)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disk in memory.
    struct Mem(Vec<u8>);

    impl BlockDevice for Mem {
        type Error = ();
        fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), ()> {
            let start = sector as usize * SECTOR;
            let src = self.0.get(start..start + buf.len()).ok_or(())?;
            buf.copy_from_slice(src);
            Ok(())
        }
    }

    #[test]
    fn reads_the_nvram_region() {
        let disk: Vec<u8> = (0..4 * SECTOR).map(|i| (i / SECTOR) as u8).collect();
        let region = read_nvram(&mut Mem(disk), SECTOR as u64, 2 * SECTOR as u64).unwrap();
        assert_eq!(region, [[1; SECTOR], [2; SECTOR]].concat());
    }

    #[test]
    fn rejects_a_bad_nvram_region() {
        let mut aux = Mem(vec![0; 4 * SECTOR]);
        let sector = SECTOR as u64;
        // Off a sector boundary at either end, one sector past the bound,
        // and the largest region the template's 32-bit fields can give.
        for (at, len) in [
            (1, sector),
            (sector, 100),
            (0, MAX_NVRAM_LEN + sector),
            (0, 0xffff_fe00),
        ] {
            assert!(matches!(
                read_nvram(&mut aux, at, len),
                Err(Error::BadNvramRegion { .. })
            ));
        }
        // A region past the disk's end is the disk's to refuse.
        assert!(matches!(
            read_nvram(&mut aux, 4 * sector, sector),
            Err(Error::Aux(()))
        ));
    }
}
