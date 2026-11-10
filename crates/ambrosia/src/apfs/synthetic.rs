//! A small container built in memory, to test what a real guest disk does
//! not show: picking the newest valid checkpoint, object map versions, a
//! walk across leaves, a file with holes read in odd-sized pieces, and the
//! errors that corrupt structures give.

use super::btree::tests::node;
use super::object::{BTREE, BTREE_NODE, FS, FSTREE, OMAP, PHYSICAL, VIRTUAL, seal};
use super::{Container, Error, Kind, Role};
use crate::block::{BlockDevice, SECTOR};
use alloc::vec;
use alloc::vec::Vec;

const BS: usize = 4096;
/// The container starts this many sectors into the disk.
const FIRST_LBA: u64 = 8;
const BLOCKS: usize = 24;

/// Blocks of the container.
const DESC: usize = 1;
const DESC_CORRUPT: usize = 2;
const NX_OMAP: usize = 3;
const NX_OMAP_TREE: usize = 4;
const VOL_SB: usize = 5;
const VOL_OMAP: usize = 6;
const VOL_OMAP_TREE: usize = 7;
const FS_ROOT: usize = 8;
const FS_LEAF_A: usize = 9;
const FS_LEAF_B: usize = 10;
const DATA_A: u64 = 20;
const DATA_C: u64 = 22;

/// Virtual object IDs.
const VOL_OID: u64 = 0x400;
const ROOT_OID: u64 = 0x402;
const LEAF_A_OID: u64 = 0x403;
const LEAF_B_OID: u64 = 0x404;

/// The checkpoint's transaction, and the file's inode and size.
const XID: u64 = 5;
const FILE: u64 = 0x10;
const FILE_SIZE: u64 = 3 * BS as u64 + 100;

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

fn put(b: &mut [u8], at: usize, bytes: &[u8]) {
    b[at..at + bytes.len()].copy_from_slice(bytes);
}

fn header(b: &mut [u8], oid: u64, xid: u64, kind: u32, subtype: u32) {
    put(b, 8, &oid.to_le_bytes());
    put(b, 16, &xid.to_le_bytes());
    put(b, 24, &kind.to_le_bytes());
    put(b, 28, &subtype.to_le_bytes());
}

/// A root node's tree info.
fn info(b: &mut [u8], flags: u32, key: u32, val: u32, nodes: u64) {
    let i = BS - 40;
    put(b, i, &flags.to_le_bytes());
    put(b, i + 4, &(BS as u32).to_le_bytes());
    put(b, i + 8, &key.to_le_bytes());
    put(b, i + 12, &val.to_le_bytes());
    put(b, i + 32, &nodes.to_le_bytes());
}

fn nx_superblock(xid: u64) -> Vec<u8> {
    let mut b = vec![0u8; BS];
    header(&mut b, 1, xid, 0x8000_0000 | 0x01, 0);
    put(&mut b, 32, b"NXSB");
    put(&mut b, 36, &(BS as u32).to_le_bytes());
    put(&mut b, 40, &(BLOCKS as u64).to_le_bytes());
    put(&mut b, 64, &2u64.to_le_bytes());
    put(&mut b, 72, &[0xc0; 16]);
    put(&mut b, 104, &2u32.to_le_bytes());
    put(&mut b, 112, &(DESC as u64).to_le_bytes());
    put(&mut b, 160, &(NX_OMAP as u64).to_le_bytes());
    put(&mut b, 180, &100u32.to_le_bytes());
    put(&mut b, 184, &VOL_OID.to_le_bytes());
    seal(&mut b);
    b
}

fn omap(block: usize, tree: usize) -> Vec<u8> {
    let mut b = vec![0u8; BS];
    header(&mut b, block as u64, XID, PHYSICAL | OMAP, 0);
    put(&mut b, 40, &(PHYSICAL | BTREE).to_le_bytes());
    put(&mut b, 48, &(tree as u64).to_le_bytes());
    seal(&mut b);
    b
}

/// An object map's tree: one leaf of (oid, xid) to block.
fn omap_tree(block: usize, map: &[(u64, u64, u64)]) -> Vec<u8> {
    let entries: Vec<([u8; 16], [u8; 16])> = map
        .iter()
        .map(|&(oid, xid, paddr)| {
            let mut k = [0u8; 16];
            k[..8].copy_from_slice(&oid.to_le_bytes());
            k[8..].copy_from_slice(&xid.to_le_bytes());
            let mut v = [0u8; 16];
            v[4..8].copy_from_slice(&(BS as u32).to_le_bytes());
            v[8..].copy_from_slice(&paddr.to_le_bytes());
            (k, v)
        })
        .collect();
    let refs: Vec<(&[u8], &[u8])> = entries.iter().map(|(k, v)| (&k[..], &v[..])).collect();
    let mut b = node(BS, 0, true, true, &refs);
    header(&mut b, block as u64, XID, PHYSICAL | BTREE, OMAP);
    info(&mut b, 0x10, 16, 16, 1);
    seal(&mut b);
    b
}

fn volume_superblock() -> Vec<u8> {
    let mut b = vec![0u8; BS];
    header(&mut b, VOL_OID, XID, VIRTUAL | FS, 0);
    put(&mut b, 32, b"APSB");
    put(&mut b, 56, &1u64.to_le_bytes()); // case-insensitive
    put(&mut b, 116, &(VIRTUAL | BTREE).to_le_bytes());
    put(&mut b, 128, &(VOL_OMAP as u64).to_le_bytes());
    put(&mut b, 136, &ROOT_OID.to_le_bytes());
    put(&mut b, 240, &[0xab; 16]);
    put(&mut b, 264, &1u64.to_le_bytes()); // unencrypted
    put(&mut b, 704, b"Test\0");
    put(&mut b, 964, &0x10u16.to_le_bytes()); // Preboot
    put(&mut b, 1008, &[0xcd; 16]);
    seal(&mut b);
    b
}

fn key(id: u64, ty: u64) -> Vec<u8> {
    (id | ty << 60).to_le_bytes().to_vec()
}

fn inode(parent: u64, mode: u16, size: Option<u64>) -> Vec<u8> {
    let mut v = vec![0u8; 92];
    put(&mut v, 0, &parent.to_le_bytes());
    put(&mut v, 80, &mode.to_le_bytes());
    if let Some(size) = size {
        // A 4-byte field before the data stream, so the stream's data
        // starts after the first field's padding.
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&48u16.to_le_bytes());
        v.extend_from_slice(&[3, 0, 4, 0, 8, 0, 40, 0]);
        v.extend_from_slice(&[0x77; 8]);
        let mut stream = [0u8; 40];
        stream[..8].copy_from_slice(&size.to_le_bytes());
        v.extend_from_slice(&stream);
    }
    v
}

/// An inode record's value with its private ID set to its own ID.
fn inode_of(id: u64, parent: u64, mode: u16, size: Option<u64>) -> Vec<u8> {
    let mut v = inode(parent, mode, size);
    put(&mut v, 8, &id.to_le_bytes());
    v
}

fn dir_rec(parent: u64, name: &str, id: u64, dtype: u16) -> (Vec<u8>, Vec<u8>) {
    let mut k = key(parent, 9);
    let len = name.len() as u32 + 1;
    k.extend_from_slice(&(len | 0x1234 << 10).to_le_bytes());
    k.extend_from_slice(name.as_bytes());
    k.push(0);
    let mut v = vec![0u8; 18];
    put(&mut v, 0, &id.to_le_bytes());
    put(&mut v, 16, &dtype.to_le_bytes());
    (k, v)
}

fn extent(id: u64, logical: u64, len: u64, phys: u64) -> (Vec<u8>, Vec<u8>) {
    let mut k = key(id, 8);
    k.extend_from_slice(&logical.to_le_bytes());
    let mut v = vec![0u8; 24];
    put(&mut v, 0, &len.to_le_bytes());
    put(&mut v, 8, &phys.to_le_bytes());
    (k, v)
}

fn fs_node(oid: u64, level: u16, root: bool, records: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let refs: Vec<(&[u8], &[u8])> = records.iter().map(|(k, v)| (&k[..], &v[..])).collect();
    let mut b = node(BS, level, root, false, &refs);
    let kind = if root { BTREE } else { BTREE_NODE };
    header(&mut b, oid, XID, VIRTUAL | kind, FSTREE);
    if root {
        info(&mut b, 0, 0, 0, 3);
    }
    seal(&mut b);
    b
}

fn pattern(seed: u8) -> Vec<u8> {
    (0..BS).map(|i| (i as u8).wrapping_mul(7) ^ seed).collect()
}

/// The file's data: a block, a block with no extent, a block whose extent
/// has no block, and the first 100 bytes of a last block.
fn file_data() -> Vec<u8> {
    let mut d = pattern(0xa0);
    d.resize(3 * BS, 0);
    d.extend_from_slice(&pattern(0xc0)[..100]);
    d
}

fn image() -> Vec<u8> {
    let mut blocks = vec![vec![0u8; BS]; BLOCKS];
    blocks[0] = nx_superblock(XID - 1);
    blocks[DESC] = nx_superblock(XID);
    // A newer checkpoint whose superblock fails its checksum.
    blocks[DESC_CORRUPT] = nx_superblock(XID + 4);
    blocks[DESC_CORRUPT][500] ^= 1;
    blocks[NX_OMAP] = omap(NX_OMAP, NX_OMAP_TREE);
    blocks[NX_OMAP_TREE] = omap_tree(NX_OMAP_TREE, &[(VOL_OID, XID, VOL_SB as u64)]);
    blocks[VOL_SB] = volume_superblock();
    blocks[VOL_OMAP] = omap(VOL_OMAP, VOL_OMAP_TREE);
    // Leaf B has an older version and a newer one past the volume's
    // transaction; both blocks are empty, so reading either fails.
    blocks[VOL_OMAP_TREE] = omap_tree(
        VOL_OMAP_TREE,
        &[
            (ROOT_OID, XID, FS_ROOT as u64),
            (LEAF_A_OID, XID, FS_LEAF_A as u64),
            (LEAF_B_OID, XID - 2, 11),
            (LEAF_B_OID, XID, FS_LEAF_B as u64),
            (LEAF_B_OID, XID + 2, 12),
        ],
    );
    blocks[FS_ROOT] = fs_node(
        ROOT_OID,
        1,
        true,
        &[
            (key(2, 3), LEAF_A_OID.to_le_bytes().to_vec()),
            (key(FILE, 3), LEAF_B_OID.to_le_bytes().to_vec()),
        ],
    );
    blocks[FS_LEAF_A] = fs_node(
        LEAF_A_OID,
        0,
        false,
        &[
            (key(2, 3), inode_of(2, 1, 0o040755, None)),
            dir_rec(2, "file.bin", FILE, 8),
            dir_rec(2, "Dir", 0x11, 4),
        ],
    );
    blocks[FS_LEAF_B] = fs_node(
        LEAF_B_OID,
        0,
        false,
        &[
            (key(FILE, 3), inode_of(FILE, 2, 0o100644, Some(FILE_SIZE))),
            extent(FILE, 0, BS as u64, DATA_A),
            extent(FILE, 2 * BS as u64, BS as u64, 0),
            extent(FILE, 3 * BS as u64, BS as u64, DATA_C),
            (key(0x11, 3), inode_of(0x11, 2, 0o040755, None)),
            dir_rec(0x11, "inner", 0x12, 8),
            (key(0x12, 3), inode_of(0x12, 0x11, 0o100644, None)),
        ],
    );
    blocks[DATA_A as usize] = pattern(0xa0);
    blocks[DATA_C as usize] = pattern(0xc0);

    let mut disk = vec![0u8; FIRST_LBA as usize * SECTOR];
    for b in blocks {
        disk.extend_from_slice(&b);
    }
    disk
}

/// Changes a block of the image and stores its checksum again.
fn reseal(disk: &mut [u8], block: usize, change: impl FnOnce(&mut [u8])) {
    let at = FIRST_LBA as usize * SECTOR + block * BS;
    let b = &mut disk[at..at + BS];
    change(b);
    seal(b);
}

#[test]
fn reads_container() {
    let mut c = Container::open(Mem(image()), FIRST_LBA).unwrap();
    assert_eq!(c.xid(), XID);
    assert_eq!(c.uuid().0, [0xc0; 16]);
    let v = &c.volumes()[0];
    assert_eq!((v.name.as_str(), v.role), ("Test", Role::Preboot));
    assert_eq!(v.uuid.0, [0xab; 16]);
    assert_eq!(v.group.unwrap().0, [0xcd; 16]);
    assert!(matches!(c.volume(1), Err(Error::NoSuchVolume(1))));

    let mut vol = c.volume(0).unwrap();
    let root = vol.root().unwrap();
    assert_eq!((root.id, root.parent, root.kind), (2, 1, Kind::Dir));
    let file = vol.stat(FILE).unwrap();
    assert_eq!(
        (file.kind, file.size, file.mode),
        (Kind::File, FILE_SIZE, 0o100644)
    );
    assert_eq!(vol.stat(0x12).unwrap().size, 0);
    assert!(matches!(vol.stat(0x13), Err(Error::NoInode { id: 0x13 })));
}

#[test]
fn looks_up_paths() {
    let mut c = Container::open(Mem(image()), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    // The root directory's entries end in leaf A, so listing it walks on
    // into leaf B to see that.
    let root = vol.root().unwrap();
    let names: Vec<_> = vol
        .read_dir(&root)
        .unwrap()
        .into_iter()
        .map(|e| (e.name, e.id, e.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("file.bin".into(), FILE, Kind::File),
            ("Dir".into(), 0x11, Kind::Dir)
        ]
    );

    assert_eq!(vol.lookup("FILE.BIN").unwrap().id, FILE);
    assert_eq!(vol.lookup("/").unwrap().id, 2);
    let inner = vol.lookup("/dir/INNER").unwrap();
    assert_eq!((inner.id, inner.parent, inner.size), (0x12, 0x11, 0));
    assert_eq!(vol.read(&inner).unwrap(), []);
    assert!(matches!(
        vol.lookup("/nope"),
        Err(Error::NotFound { dir: 2, .. })
    ));
    assert!(matches!(
        vol.lookup("file.bin/x"),
        Err(Error::NotADirectory { id: FILE })
    ));
}

#[test]
fn reads_file_with_holes() {
    let mut c = Container::open(Mem(image()), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    let file = vol.stat(FILE).unwrap();
    let want = file_data();
    assert_eq!(vol.read(&file).unwrap(), want);

    for offset in [0, 1, 511, 513, 4000, 4096, 8190, 12287, 12387, 12388, 99999] {
        for len in [1, 100, 512, 600, 5000, 20000] {
            let mut buf = vec![0x5a; len];
            let n = vol.read_at(&file, offset as u64, &mut buf).unwrap();
            let end = (offset + len).min(want.len());
            let expect = want.get(offset..end).unwrap_or(&[]);
            assert_eq!(&buf[..n], expect, "read at {offset} for {len}");
        }
    }
}

#[test]
fn compressed_file_is_an_error() {
    let mut disk = image();
    // Set UF_COMPRESSED in the file's inode record.
    let record = inode_of(FILE, 2, 0o100644, Some(FILE_SIZE));
    let flags = find(&disk, FS_LEAF_B, &record) + 68;
    reseal(&mut disk, FS_LEAF_B, |b| b[flags] |= 0x20);
    let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    let file = vol.stat(FILE).unwrap();
    assert!(file.is_compressed());
    assert!(matches!(
        vol.read(&file),
        Err(Error::Compressed { id: FILE })
    ));
}

#[test]
fn file_larger_than_memory_is_an_error() {
    let mut disk = image();
    // The data stream's size, after the inode's fixed part, the extended
    // fields' count and length, two field descriptors and the first field.
    let record = inode_of(FILE, 2, 0o100644, Some(FILE_SIZE));
    let size = find(&disk, FS_LEAF_B, &record) + 92 + 4 + 8 + 8;
    reseal(&mut disk, FS_LEAF_B, |b| {
        b[size..size + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    });
    let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    let file = vol.stat(FILE).unwrap();
    assert_eq!(file.size, u64::MAX);
    assert!(matches!(
        vol.read(&file),
        Err(Error::TooLarge {
            id: FILE,
            size: u64::MAX
        })
    ));
}

#[test]
fn overlapping_extents_are_an_error() {
    let mut disk = image();
    // Move the hole's extent to byte 0, inside the first block's extent.
    let (key, _) = extent(FILE, 2 * BS as u64, BS as u64, 0);
    let logical = find(&disk, FS_LEAF_B, &key) + 8;
    reseal(&mut disk, FS_LEAF_B, |b| {
        b[logical..logical + 8].fill(0);
    });
    let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    let file = vol.stat(FILE).unwrap();
    assert!(matches!(
        vol.read(&file),
        Err(Error::BadRecord {
            id: FILE,
            what: "file extents overlap or are not whole blocks"
        })
    ));
}

/// Where `bytes` start in a block of the image, from the block's start.
fn find(disk: &[u8], block: usize, bytes: &[u8]) -> usize {
    let at = FIRST_LBA as usize * SECTOR + block * BS;
    disk[at..at + BS]
        .windows(bytes.len())
        .position(|w| w == bytes)
        .unwrap()
}

#[test]
fn corrupt_node_is_an_error() {
    let mut disk = image();
    disk[FIRST_LBA as usize * SECTOR + FS_LEAF_B * BS + 300] ^= 1;
    let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    assert!(matches!(
        vol.stat(FILE),
        Err(Error::Checksum { block }) if block == FS_LEAF_B as u64
    ));
}

#[test]
fn tree_larger_than_it_says_is_an_error() {
    let mut disk = image();
    // One node, but any record takes the root and a leaf.
    reseal(&mut disk, FS_ROOT, |b| {
        b[BS - 8..].copy_from_slice(&1u64.to_le_bytes());
    });
    let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    assert!(matches!(
        vol.root(),
        Err(Error::TreeTooBig { root: ROOT_OID })
    ));
}

#[test]
fn tree_with_more_nodes_than_blocks_is_an_error() {
    let set_nodes = |nodes: u64| {
        let mut disk = image();
        reseal(&mut disk, FS_ROOT, |b| {
            b[BS - 8..].copy_from_slice(&nodes.to_le_bytes());
        });
        let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
        c.volume(0).map(|_| ())
    };
    assert!(set_nodes(BLOCKS as u64).is_ok());
    assert!(matches!(
        set_nodes(BLOCKS as u64 + 1),
        Err(Error::BadNode { block, what: "more nodes than the container has blocks" })
            if block == FS_ROOT as u64
    ));
}

#[test]
fn child_pointing_at_root_is_an_error() {
    let mut disk = image();
    // The root's second child, the value nearest the start of the area.
    let at = BS - 40 - 16;
    reseal(&mut disk, FS_ROOT, |b| {
        b[at..at + 8].copy_from_slice(&ROOT_OID.to_le_bytes());
    });
    let mut c = Container::open(Mem(disk), FIRST_LBA).unwrap();
    let mut vol = c.volume(0).unwrap();
    assert!(matches!(
        vol.stat(FILE),
        Err(Error::WrongObject { what: "type", .. })
    ));
}

#[test]
fn no_valid_checkpoint_is_an_error() {
    let mut disk = image();
    disk[FIRST_LBA as usize * SECTOR + DESC * BS + 500] ^= 1;
    assert!(matches!(
        Container::open(Mem(disk), FIRST_LBA),
        Err(Error::NoCheckpoint)
    ));
}

#[test]
fn not_apfs_is_an_error() {
    assert!(matches!(
        Container::open(Mem(image()), 0),
        Err(Error::NotApfs)
    ));
}
