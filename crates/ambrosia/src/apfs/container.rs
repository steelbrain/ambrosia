//! The container: its superblock, as of the latest valid checkpoint, its
//! object map, and its volumes' superblocks.

use super::btree::Omap;
use super::object::{
    Disk, Expect, FS, MIN_BLOCK, NX_SUPERBLOCK, VIRTUAL, Xid, checksum_ok, object_type,
};
use super::volume::Volume;
use super::{Error, Uuid, u16_at, u32_at, u64_at, uuid_at};
use crate::block::BlockDevice;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

/// The container superblock's object ID.
const NX_OID: u64 = 1;
/// The most volumes a container has.
const MAX_VOLUMES: usize = 100;
/// The largest block size APFS allows.
const MAX_BLOCK: usize = 65536;
/// The most checkpoint descriptor blocks this reader looks through, far more
/// than a container has; this only bounds a corrupt superblock.
const MAX_DESC_BLOCKS: u32 = 1 << 16;
/// How many descriptor blocks one disk request reads.
const DESC_CHUNK: usize = 64;
/// `nx_xp_desc_blocks`'s top bit: the area is a B-tree, not a ring of blocks.
const DESC_IN_TREE: u32 = 0x8000_0000;

/// Container incompatible features.
const NX_VERSION2: u64 = 0x2;
const NX_FUSION: u64 = 0x100;

/// A volume's role, from its superblock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// No role.
    None,
    /// The system: the OS's root file system.
    System,
    /// Users' home directories.
    User,
    /// A recovery system.
    Recovery,
    /// Swap space for virtual memory.
    Vm,
    /// What a boot reads before it mounts the system: on macOS, each
    /// system's boot objects.
    Preboot,
    /// Used by the OS installer.
    Installer,
    /// A system's writable data, beside its read-only System volume.
    Data,
    /// Used by radio firmware.
    Baseband,
    /// Used by software update.
    Update,
    /// xART: used to manage the OS's access to secure user data.
    Xart,
    /// Firmware data.
    Hardware,
    /// Time Machine backups.
    Backup,
    /// A role this reader does not name, as its superblock holds it.
    Other(u16),
}

impl Role {
    fn from_raw(raw: u16) -> Role {
        match raw {
            0x000 => Role::None,
            0x001 => Role::System,
            0x002 => Role::User,
            0x004 => Role::Recovery,
            0x008 => Role::Vm,
            0x010 => Role::Preboot,
            0x020 => Role::Installer,
            0x040 => Role::Data,
            0x080 => Role::Baseband,
            0x0c0 => Role::Update,
            0x100 => Role::Xart,
            0x140 => Role::Hardware,
            0x180 => Role::Backup,
            other => Role::Other(other),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Role::Other(raw) => write!(f, "role {raw:#x}"),
            named => fmt::Debug::fmt(named, f),
        }
    }
}

/// A volume, as its superblock describes it.
#[derive(Clone, Debug)]
pub struct VolumeInfo {
    /// Its name, up to the first NUL, with bytes that are not UTF-8
    /// replaced.
    pub name: String,
    /// Its role.
    pub role: Role,
    /// Its UUID.
    pub uuid: Uuid,
    /// The volume group it belongs to, if any. A macOS system's System and
    /// Data volumes share one, and its Preboot volume keeps that system's
    /// boot files in a directory named for it.
    pub group: Option<Uuid>,
    /// Its index among the container's volumes.
    pub index: u32,
    /// The transaction that wrote the superblock, as of which the volume's
    /// tree is read.
    pub(crate) xid: u64,
    /// Its incompatible features.
    pub(crate) incompat: u64,
    /// Its flags, which say whether it is encrypted.
    pub(crate) fs_flags: u64,
    /// The block of its object map.
    pub(crate) omap: u64,
    /// The object type of its file-system tree, with its storage kind.
    pub(crate) root_tree_type: u32,
    /// The virtual object ID of its file-system tree's root.
    pub(crate) root_tree: u64,
}

/// An APFS container on a partition.
pub struct Container<D> {
    pub(crate) disk: Disk<D>,
    /// The checkpoint's transaction, the newest any object read can hold.
    pub(crate) xid: u64,
    uuid: Uuid,
    volumes: Vec<VolumeInfo>,
}

impl<D: BlockDevice> Container<D> {
    /// Opens the container that starts at sector `first_lba` of `dev`, and
    /// reads every volume's superblock. A volume superblock that fails its
    /// checks fails the open.
    pub fn open(dev: D, first_lba: u64) -> Result<Self, Error<D::Error>> {
        let mut disk = Disk::new(dev, first_lba);

        // Block 0's copy of the superblock gives the block size and where
        // the checkpoints are.
        let mut sb = vec![0u8; MIN_BLOCK];
        disk.read_blocks(0, &mut sb)?;
        if !is_nx_superblock(&sb) {
            return Err(Error::NotApfs);
        }
        let block_size = u32_at(&sb, 36);
        let bs = block_size as usize;
        if !bs.is_power_of_two() || !(MIN_BLOCK..=MAX_BLOCK).contains(&bs) {
            return Err(Error::BlockSize(block_size));
        }
        if bs > MIN_BLOCK {
            sb.resize(bs, 0);
            disk.set_size(bs, u64::MAX);
            disk.read_blocks(0, &mut sb)?;
        }
        if !checksum_ok(&sb) {
            return Err(Error::Checksum { block: 0 });
        }
        disk.set_size(bs, u64_at(&sb, 40));

        // Block 0's copy can be older than the latest checkpoint's.
        let xid = read_latest_superblock(&mut disk, &mut sb)?;
        disk.set_size(bs, u64_at(&sb, 40));

        let incompat = u64_at(&sb, 64);
        if incompat & NX_VERSION2 == 0 {
            return Err(Error::Unsupported("container format before APFS 2"));
        }
        if incompat & NX_FUSION != 0 {
            return Err(Error::Unsupported("Fusion container"));
        }

        let mut omap = Omap::open(&mut disk, u64_at(&sb, 160), xid)?;
        let max_volumes = u32_at(&sb, 180) as usize;
        if max_volumes > MAX_VOLUMES {
            return Err(Error::BadSuperblock("volume count"));
        }
        let mut block = vec![0u8; bs];
        let mut volumes = Vec::new();
        for i in 0..max_volumes {
            let oid = u64_at(&sb, 184 + 8 * i);
            if oid != 0 {
                volumes.push(read_volume(&mut disk, &mut omap, oid, xid, &mut block)?);
            }
        }
        Ok(Container {
            disk,
            xid,
            uuid: uuid_at(&sb, 72),
            volumes,
        })
    }

    /// The container's UUID.
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// The container's block size, in bytes.
    pub fn block_size(&self) -> usize {
        self.disk.block_size
    }

    /// The transaction of the checkpoint read.
    pub fn xid(&self) -> u64 {
        self.xid
    }

    /// The container's volumes, in superblock order.
    pub fn volumes(&self) -> &[VolumeInfo] {
        &self.volumes
    }

    /// Opens volume `index` of [`Container::volumes`].
    pub fn volume(&mut self, index: usize) -> Result<Volume<'_, D>, Error<D::Error>> {
        let info = self
            .volumes
            .get(index)
            .cloned()
            .ok_or(Error::NoSuchVolume(index))?;
        Volume::open(self, info)
    }
}

/// Whether `b` starts with a container superblock's header and magic. Its
/// checksum is checked apart.
fn is_nx_superblock(b: &[u8]) -> bool {
    object_type(b) == NX_SUPERBLOCK && u64_at(b, 8) == NX_OID && &b[32..36] == b"NXSB"
}

/// Reads the latest checkpoint's superblock into `sb`, which holds block 0's
/// copy, and returns its transaction. The latest is the one in the
/// checkpoint descriptor area with the newest transaction and a valid
/// checksum.
fn read_latest_superblock<D: BlockDevice>(
    disk: &mut Disk<D>,
    sb: &mut [u8],
) -> Result<u64, Error<D::Error>> {
    let desc_blocks = u32_at(sb, 104);
    if desc_blocks & DESC_IN_TREE != 0 {
        return Err(Error::Unsupported("checkpoint descriptors in a B-tree"));
    }
    if desc_blocks == 0 || desc_blocks > MAX_DESC_BLOCKS {
        return Err(Error::BadSuperblock("checkpoint descriptor blocks"));
    }
    let desc_base = u64_at(sb, 112);
    let desc_blocks = desc_blocks as usize;
    let block_size = u32_at(sb, 36);
    let bs = disk.block_size;
    // Freed on return, before the caller allocates anything else, so a bump
    // heap, as the firmware's is, takes it back.
    let mut chunk = vec![0u8; bs * desc_blocks.min(DESC_CHUNK)];
    let mut newest = None;
    for first in (0..desc_blocks).step_by(DESC_CHUNK) {
        let n = (desc_blocks - first).min(DESC_CHUNK);
        let at = desc_base.saturating_add(first as u64);
        disk.read_blocks(at, &mut chunk[..n * bs])?;
        for block in chunk[..n * bs].chunks_exact(bs) {
            let xid = u64_at(block, 16);
            if is_nx_superblock(block)
                && u32_at(block, 36) == block_size
                && newest.is_none_or(|n| xid > n)
                && checksum_ok(block)
            {
                newest = Some(xid);
                sb.copy_from_slice(block);
            }
        }
    }
    newest.ok_or(Error::NoCheckpoint)
}

/// Reads the superblock of the volume whose virtual object ID is `oid`.
fn read_volume<D: BlockDevice>(
    disk: &mut Disk<D>,
    omap: &mut Omap,
    oid: u64,
    xid: u64,
    buf: &mut [u8],
) -> Result<VolumeInfo, Error<D::Error>> {
    let (block, version) = omap.lookup(disk, oid, xid)?;
    let expect = Expect {
        kind: VIRTUAL | FS,
        subtype: None,
        oid,
        xid: Xid::Exactly(version),
    };
    disk.read_object(block, buf, &expect)?;
    if &buf[32..36] != b"APSB" {
        return Err(Error::BadSuperblock("volume superblock magic"));
    }
    let name = &buf[704..960];
    let name = name.split(|&b| b == 0).next().unwrap_or(name);
    let group = uuid_at(buf, 1008);
    Ok(VolumeInfo {
        name: String::from_utf8_lossy(name).into_owned(),
        role: Role::from_raw(u16_at(buf, 964)),
        uuid: uuid_at(buf, 240),
        group: (group.0 != [0; 16]).then_some(group),
        index: u32_at(buf, 36),
        xid: version,
        incompat: u64_at(buf, 56),
        fs_flags: u64_at(buf, 264),
        omap: u64_at(buf, 128),
        root_tree_type: u32_at(buf, 116),
        root_tree: u64_at(buf, 136),
    })
}
