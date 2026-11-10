//! APFS B-trees, and the object map, the B-tree that gives a virtual object's
//! block.
//!
//! A node is a block: a header, a table of contents, keys growing up from the
//! table, and values growing down from the node's end (from just before the
//! tree's info, in the root). An index node's values are its children's
//! object IDs. Nodes have no sibling links, so a walk keeps the path from the
//! root, one block buffer per level, in a [`Cursor`] it reuses.

use super::object::{BTREE, BTREE_NODE, Disk, Expect, OMAP, PHYSICAL, VIRTUAL, Xid};
use super::{Error, u16_at, u32_at, u64_at};
use crate::block::BlockDevice;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

/// Node flags.
const NODE_ROOT: u16 = 0x1;
const NODE_LEAF: u16 = 0x2;
const NODE_FIXED: u16 = 0x4;

/// Tree flags, in the root's info.
const TREE_PHYSICAL: u32 = 0x10;
const TREE_NOHEADER: u32 = 0x100;

/// A node's header, after the object header, ends here; the table of
/// contents' offset counts from here.
const NODE_HEADER: usize = 56;
/// The tree's info at the end of the root node.
const TREE_INFO: usize = 40;
/// A table-of-contents offset that marks a key with no value.
const NO_VALUE: u16 = 0xffff;
/// Deeper than any tree a container holds: a 4 KiB node holds tens of
/// entries at least, so this many levels address more blocks than exist.
const MAX_DEPTH: u16 = 16;

/// One node, parsed from its block.
pub(crate) struct Node<'a> {
    buf: &'a [u8],
    /// How far above the leaves it is: 0 for a leaf.
    pub level: u16,
    /// How many entries it has.
    pub nkeys: usize,
    /// The key and value sizes of a tree with fixed-size entries.
    fixed: Option<(usize, usize)>,
    /// Where the table of contents starts.
    toc: usize,
    /// Where the key area starts, after the table of contents.
    keys: usize,
    /// Where the value area ends.
    vals: usize,
}

impl<'a> Node<'a> {
    /// Parses a node. `fixed` is the tree's entry sizes if its entries have
    /// fixed sizes, and `root` whether the node is the tree's root.
    pub fn parse(
        buf: &'a [u8],
        fixed: Option<(usize, usize)>,
        root: bool,
    ) -> Result<Self, &'static str> {
        let flags = u16_at(buf, 32);
        let level = u16_at(buf, 34);
        let nkeys = u32_at(buf, 36) as usize;
        let toc = NODE_HEADER + u16_at(buf, 40) as usize;
        let keys = toc + u16_at(buf, 42) as usize;
        let vals = buf.len() - if root { TREE_INFO } else { 0 };
        if (flags & NODE_ROOT != 0) != root {
            return Err("root flag disagrees with the node's place");
        }
        if (flags & NODE_LEAF != 0) != (level == 0) {
            return Err("leaf flag disagrees with the level");
        }
        if level >= MAX_DEPTH {
            return Err("level too deep");
        }
        if keys > vals {
            return Err("table of contents runs past the node");
        }
        if (flags & NODE_FIXED != 0) != fixed.is_some() {
            return Err("entry layout differs from the tree's");
        }
        let entry = if fixed.is_some() { 4 } else { 8 };
        if nkeys.saturating_mul(entry) > keys - toc {
            return Err("more keys than the table of contents holds");
        }
        Ok(Node {
            buf,
            level,
            nkeys,
            fixed,
            toc,
            keys,
            vals,
        })
    }

    /// Entry `i`'s key and value. The trees this reader walks have keys of 8
    /// bytes or more and a value for every key.
    pub fn entry(&self, i: usize) -> Result<(&'a [u8], &'a [u8]), &'static str> {
        if i >= self.nkeys {
            return Err("entry past the last key");
        }
        let (k_off, k_len, v_off, v_len) = match self.fixed {
            Some((k_len, v_len)) => {
                let t = self.toc + 4 * i;
                // An index node's values are child object IDs, whatever the
                // size of a leaf's values.
                let v_len = if self.level == 0 { v_len } else { 8 };
                (u16_at(self.buf, t), k_len, u16_at(self.buf, t + 2), v_len)
            }
            None => {
                let t = self.toc + 8 * i;
                let b = self.buf;
                let len = |off| usize::from(u16_at(b, off));
                (u16_at(b, t), len(t + 2), u16_at(b, t + 4), len(t + 6))
            }
        };
        let k_start = self.keys + usize::from(k_off);
        if k_start + k_len > self.vals {
            return Err("key outside the node");
        }
        if k_len < 8 {
            return Err("key shorter than 8 bytes");
        }
        let key = &self.buf[k_start..k_start + k_len];
        if v_off == NO_VALUE {
            return Err("key without a value");
        }
        // A value's offset counts back from the end of the value area.
        let v_start = self
            .vals
            .checked_sub(usize::from(v_off))
            .filter(|&s| s >= self.keys && s + v_len <= self.vals)
            .ok_or("value outside the node")?;
        Ok((key, &self.buf[v_start..v_start + v_len]))
    }

    /// The object ID of an index node's child `i`. A hashed tree's values
    /// carry a hash after it, which this reader does not check.
    pub fn child(&self, i: usize) -> Result<u64, &'static str> {
        let (_, value) = self.entry(i)?;
        if value.len() < 8 {
            return Err("child reference too short");
        }
        Ok(u64_at(value, 0))
    }
}

/// How a tree's nodes are found: physical nodes by their object ID, which is
/// their block, and virtual ones through an object map.
pub(crate) enum Storage<'a> {
    /// Nodes written in transaction `max_xid` or before.
    Physical { max_xid: u64 },
    /// Nodes `omap` maps, as of transaction `xid`.
    Virtual { omap: &'a mut Omap, xid: u64 },
}

/// What a walk does after a record.
#[derive(PartialEq, Eq)]
pub(crate) enum Flow {
    /// On to the next record.
    Continue,
    /// End the walk.
    Stop,
}

/// A B-tree, as its root's info describes it.
pub(crate) struct Tree {
    /// The root's object ID.
    pub root: u64,
    /// The subtype every node's header holds: the kind of tree.
    subtype: u32,
    /// The key and value sizes of a tree with fixed-size entries.
    fixed: Option<(usize, usize)>,
    /// How many nodes the tree says it has, which bounds a walk.
    nodes: u64,
}

/// The block buffers for one walk at a time, one per level, reused.
#[derive(Default)]
pub(crate) struct Cursor {
    frames: Vec<Frame>,
}

/// One level of a walk's path: a node and where the walk is in it.
struct Frame {
    /// The node's block's bytes.
    buf: Vec<u8>,
    /// The block the node was read from.
    block: u64,
    /// The entry the walk is at.
    index: usize,
}

impl Cursor {
    /// The frame for `depth`, made the first time a walk goes that deep.
    fn frame(&mut self, depth: usize, block_size: usize) -> &mut Frame {
        while self.frames.len() <= depth {
            self.frames.push(Frame {
                buf: vec![0; block_size],
                block: 0,
                index: 0,
            });
        }
        &mut self.frames[depth]
    }
}

impl Tree {
    /// Opens the tree whose root is `root`, of kind `subtype`, and reads the
    /// root's info.
    pub fn open<D: BlockDevice>(
        disk: &mut Disk<D>,
        cursor: &mut Cursor,
        storage: &mut Storage<'_>,
        root: u64,
        subtype: u32,
    ) -> Result<Tree, Error<D::Error>> {
        let frame = cursor.frame(0, disk.block_size);
        load(disk, storage, subtype, root, true, frame)?;
        let block = frame.block;
        let buf = &frame.buf;
        let info = &buf[buf.len() - TREE_INFO..];
        let flags = u32_at(info, 0);
        let node_size = u32_at(info, 4) as usize;
        let fixed = (u16_at(buf, 32) & NODE_FIXED != 0)
            .then(|| (u32_at(info, 8) as usize, u32_at(info, 12) as usize));
        let bad = |what| Error::BadNode { block, what };
        if node_size != disk.block_size {
            return Err(Error::Unsupported("B-tree nodes not a block long"));
        }
        if flags & TREE_NOHEADER != 0 {
            return Err(Error::Unsupported("B-tree nodes without headers"));
        }
        let physical = matches!(storage, Storage::Physical { .. });
        if (flags & TREE_PHYSICAL != 0) != physical {
            return Err(bad("tree's storage differs from its reference's"));
        }
        if fixed.is_some_and(|(k, v)| k < 8 || k > node_size || v > node_size) {
            return Err(bad("entry sizes out of range"));
        }
        // Every node is a block of the container, so the tree has no more
        // nodes than it has blocks. The count is each walk's budget of node
        // reads, and a corrupt one would let a walk of a tree whose index
        // nodes repeat children run on for up to 2^64 reads.
        let nodes = u64_at(info, 32);
        if nodes > disk.block_count {
            return Err(bad("more nodes than the container has blocks"));
        }
        Ok(Tree {
            root,
            subtype,
            fixed,
            nodes,
        })
    }

    /// Walks the tree's records in order, from the first whose key is not
    /// less than a target on. `cmp` orders a record's key against the
    /// target, and `visit` takes each record until it says to stop.
    ///
    /// The descent goes into the last child whose key is not greater than
    /// the target. Every record before that child is less than the target,
    /// so the first record not less than it is in that child or after it,
    /// and the walk forward finds it.
    pub fn walk<D, C, V>(
        &self,
        disk: &mut Disk<D>,
        cursor: &mut Cursor,
        storage: &mut Storage<'_>,
        cmp: C,
        mut visit: V,
    ) -> Result<(), Error<D::Error>>
    where
        D: BlockDevice,
        C: Fn(&[u8]) -> Ordering,
        V: FnMut(&mut Disk<D>, &[u8], &[u8]) -> Result<Flow, Error<D::Error>>,
    {
        let bs = disk.block_size;
        let mut budget = self.nodes;

        let mut depth = 0;
        let mut oid = self.root;
        let mut parent_level = None;
        loop {
            let frame = cursor.frame(depth, bs);
            self.read(disk, storage, &mut budget, oid, parent_level, frame)?;
            let (index, child) = {
                let node = self.node(frame, depth == 0)?;
                let block = frame.block;
                let bad = |what| Error::BadNode { block, what };
                let key = |i| node.entry(i).map(|(k, _)| k).map_err(bad);
                if node.level == 0 {
                    let mut i = 0;
                    while i < node.nkeys && cmp(key(i)?) == Ordering::Less {
                        i += 1;
                    }
                    (i, None)
                } else {
                    if node.nkeys == 0 {
                        return Err(bad("index node with no children"));
                    }
                    let mut i = 0;
                    while i + 1 < node.nkeys && cmp(key(i + 1)?) != Ordering::Greater {
                        i += 1;
                    }
                    (i, Some((node.child(i).map_err(bad)?, node.level)))
                }
            };
            frame.index = index;
            depth += 1;
            match child {
                Some((child, level)) => {
                    oid = child;
                    parent_level = Some(level);
                }
                None => break,
            }
        }

        // Walk forward: visit the leaf's records, and when a node runs out,
        // go back up and down into its parent's next child.
        loop {
            let top = depth - 1;
            let step = {
                let frame = &cursor.frames[top];
                let node = self.node(frame, top == 0)?;
                let bad = |what| Error::BadNode {
                    block: frame.block,
                    what,
                };
                if frame.index >= node.nkeys {
                    Step::Up
                } else if node.level == 0 {
                    let (key, value) = node.entry(frame.index).map_err(bad)?;
                    if visit(disk, key, value)? == Flow::Stop {
                        return Ok(());
                    }
                    Step::Next
                } else {
                    Step::Down(node.child(frame.index).map_err(bad)?, node.level)
                }
            };
            match step {
                Step::Up if top == 0 => return Ok(()),
                Step::Up => {
                    depth -= 1;
                    cursor.frames[top - 1].index += 1;
                }
                Step::Next => cursor.frames[top].index += 1,
                Step::Down(child, level) => {
                    let frame = cursor.frame(depth, bs);
                    self.read(disk, storage, &mut budget, child, Some(level), frame)?;
                    depth += 1;
                }
            }
        }
    }

    /// Reads node `oid` into `frame`, counting it against the walk's
    /// `budget`. `parent_level` is the level of the node that points at it,
    /// or `None` for the root.
    fn read<D: BlockDevice>(
        &self,
        disk: &mut Disk<D>,
        storage: &mut Storage<'_>,
        budget: &mut u64,
        oid: u64,
        parent_level: Option<u16>,
        frame: &mut Frame,
    ) -> Result<(), Error<D::Error>> {
        *budget = budget
            .checked_sub(1)
            .ok_or(Error::TreeTooBig { root: self.root })?;
        load(
            disk,
            storage,
            self.subtype,
            oid,
            parent_level.is_none(),
            frame,
        )?;
        let level = u16_at(&frame.buf, 34);
        if parent_level.is_some_and(|p| p.checked_sub(1) != Some(level)) {
            return Err(Error::BadNode {
                block: frame.block,
                what: "child level is not one below its parent's",
            });
        }
        Ok(())
    }

    /// Parses the node in `frame`, the tree's root if `root`.
    fn node<'a, E>(&self, frame: &'a Frame, root: bool) -> Result<Node<'a>, Error<E>> {
        Node::parse(&frame.buf, self.fixed, root).map_err(|what| Error::BadNode {
            block: frame.block,
            what,
        })
    }
}

/// Where a walk goes after an entry.
enum Step {
    /// Up to the parent: this node has no entries left.
    Up,
    /// On to the leaf's next record.
    Next,
    /// Down into this child of a node at this level.
    Down(u64, u16),
}

/// Reads node `oid` of a tree of kind `subtype` into `frame`, and checks it
/// is the node the reference names.
fn load<D: BlockDevice>(
    disk: &mut Disk<D>,
    storage: &mut Storage<'_>,
    subtype: u32,
    oid: u64,
    root: bool,
    frame: &mut Frame,
) -> Result<(), Error<D::Error>> {
    let (block, storage_kind, xid) = match storage {
        Storage::Physical { max_xid } => (oid, PHYSICAL, Xid::AtMost(*max_xid)),
        Storage::Virtual { omap, xid } => {
            let (block, version) = omap.lookup(disk, oid, *xid)?;
            (block, VIRTUAL, Xid::Exactly(version))
        }
    };
    let kind = if root { BTREE } else { BTREE_NODE };
    let expect = Expect {
        kind: kind | storage_kind,
        subtype: Some(subtype),
        oid,
        xid,
    };
    frame.block = block;
    frame.index = 0;
    disk.read_object(block, &mut frame.buf, &expect)
}

/// An object map: virtual object ID and transaction to block.
pub(crate) struct Omap {
    tree: Tree,
    cursor: Cursor,
    /// The newest transaction the map's own blocks can hold.
    max_xid: u64,
}

/// An object map value's flags.
const OMAP_DELETED: u32 = 0x1;
const OMAP_ENCRYPTED: u32 = 0x4;
const OMAP_NOHEADER: u32 = 0x8;

impl Omap {
    /// Opens the object map object in `block`, written in transaction
    /// `max_xid` or before.
    pub fn open<D: BlockDevice>(
        disk: &mut Disk<D>,
        block: u64,
        max_xid: u64,
    ) -> Result<Omap, Error<D::Error>> {
        let mut buf = vec![0; disk.block_size];
        let expect = Expect {
            kind: PHYSICAL | OMAP,
            subtype: None,
            oid: block,
            xid: Xid::AtMost(max_xid),
        };
        disk.read_object(block, &mut buf, &expect)?;
        if u32_at(&buf, 40) != PHYSICAL | BTREE {
            return Err(Error::Unsupported("object map tree not physical"));
        }
        let mut cursor = Cursor::default();
        let mut storage = Storage::Physical { max_xid };
        let tree = Tree::open(disk, &mut cursor, &mut storage, u64_at(&buf, 48), OMAP)?;
        if tree.fixed != Some((16, 16)) {
            return Err(Error::BadNode {
                block: tree.root,
                what: "object map entries not 16 bytes",
            });
        }
        Ok(Omap {
            tree,
            cursor,
            max_xid,
        })
    }

    /// The block of virtual object `oid` as of transaction `xid`, and the
    /// transaction that wrote that version: the entry for `oid` with the
    /// newest transaction not after `xid`.
    pub fn lookup<D: BlockDevice>(
        &mut self,
        disk: &mut Disk<D>,
        oid: u64,
        xid: u64,
    ) -> Result<(u64, u64), Error<D::Error>> {
        let mut found = None;
        let mut storage = Storage::Physical {
            max_xid: self.max_xid,
        };
        // Keys sort by object ID, then transaction. Start at the object's
        // oldest entry and keep the last one not after `xid`.
        self.tree.walk(
            disk,
            &mut self.cursor,
            &mut storage,
            |key| (u64_at(key, 0), u64_at(key, 8)).cmp(&(oid, 0)),
            |_, key, value| {
                if u64_at(key, 0) != oid || u64_at(key, 8) > xid {
                    return Ok(Flow::Stop);
                }
                found = Some((u64_at(key, 8), u32_at(value, 0), u64_at(value, 8)));
                Ok(Flow::Continue)
            },
        )?;
        let not_mapped = Error::NotMapped { oid, xid };
        let (version, flags, block) = found.ok_or(not_mapped)?;
        if flags & OMAP_DELETED != 0 {
            return Err(Error::NotMapped { oid, xid });
        }
        if flags & OMAP_ENCRYPTED != 0 {
            return Err(Error::Unsupported("encrypted object"));
        }
        if flags & OMAP_NOHEADER != 0 {
            return Err(Error::Unsupported("object without a header"));
        }
        Ok((block, version))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a node: `entries` are key and value bytes, laid out as APFS
    /// does, keys from the table's end up and values from the end down.
    pub(crate) fn node(
        size: usize,
        level: u16,
        root: bool,
        fixed: bool,
        entries: &[(&[u8], &[u8])],
    ) -> Vec<u8> {
        let mut b = vec![0u8; size];
        let mut flags = if level == 0 { NODE_LEAF } else { 0 };
        if root {
            flags |= NODE_ROOT;
        }
        if fixed {
            flags |= NODE_FIXED;
        }
        let entry = if fixed { 4 } else { 8 };
        let toc_len = entry * entries.len().max(1);
        b[32..34].copy_from_slice(&flags.to_le_bytes());
        b[34..36].copy_from_slice(&level.to_le_bytes());
        b[36..40].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        b[42..44].copy_from_slice(&(toc_len as u16).to_le_bytes());
        let keys = NODE_HEADER + toc_len;
        let vals = size - if root { TREE_INFO } else { 0 };
        let (mut k_off, mut v_off) = (0usize, 0usize);
        for (i, (k, v)) in entries.iter().enumerate() {
            b[keys + k_off..keys + k_off + k.len()].copy_from_slice(k);
            v_off += v.len();
            b[vals - v_off..vals - v_off + v.len()].copy_from_slice(v);
            let t = NODE_HEADER + entry * i;
            let put = |b: &mut [u8], at: usize, x: usize| {
                b[at..at + 2].copy_from_slice(&(x as u16).to_le_bytes());
            };
            put(&mut b, t, k_off);
            if fixed {
                put(&mut b, t + 2, v_off);
            } else {
                put(&mut b, t + 2, k.len());
                put(&mut b, t + 4, v_off);
                put(&mut b, t + 6, v.len());
            }
            k_off += k.len();
        }
        b
    }

    #[test]
    fn parses_variable_leaf() {
        let b = node(
            4096,
            0,
            false,
            false,
            &[(b"key one!", b"v1"), (b"key two, longer", b"value 2")],
        );
        let n = Node::parse(&b, None, false).unwrap();
        assert_eq!((n.level, n.nkeys), (0, 2));
        assert_eq!(n.entry(0).unwrap(), (&b"key one!"[..], &b"v1"[..]));
        assert_eq!(
            n.entry(1).unwrap(),
            (&b"key two, longer"[..], &b"value 2"[..])
        );
        assert!(n.entry(2).is_err());
    }

    #[test]
    fn parses_fixed_root_index() {
        let k = [7u8; 16];
        let b = node(4096, 1, true, true, &[(&k, &0x1234u64.to_le_bytes())]);
        let n = Node::parse(&b, Some((16, 16)), true).unwrap();
        assert_eq!(n.entry(0).unwrap().0, &k);
        // An index node's values are 8-byte child IDs.
        assert_eq!(n.child(0).unwrap(), 0x1234);
    }

    #[test]
    fn rejects_bad_layouts() {
        let b = node(4096, 0, false, false, &[(b"12345678", b"v")]);
        assert!(Node::parse(&b, None, true).is_err(), "root flag");
        assert!(Node::parse(&b, Some((16, 16)), false).is_err(), "fixed");

        let mut bad = b.clone();
        bad[34] = 1;
        assert!(Node::parse(&bad, None, false).is_err(), "leaf at level 1");

        let mut bad = b.clone();
        bad[36..40].copy_from_slice(&1000u32.to_le_bytes());
        assert!(Node::parse(&bad, None, false).is_err(), "key count");

        let mut bad = b.clone();
        bad[42..44].copy_from_slice(&0xfff0u16.to_le_bytes());
        assert!(Node::parse(&bad, None, false).is_err(), "table size");

        let mut bad = b.clone();
        bad[NODE_HEADER + 4..NODE_HEADER + 6].copy_from_slice(&5000u16.to_le_bytes());
        let n = Node::parse(&bad, None, false).unwrap();
        assert!(n.entry(0).is_err(), "value offset");

        let mut bad = b;
        bad[NODE_HEADER..NODE_HEADER + 2].copy_from_slice(&4090u16.to_le_bytes());
        let n = Node::parse(&bad, None, false).unwrap();
        assert!(n.entry(0).is_err(), "key offset");
    }
}
