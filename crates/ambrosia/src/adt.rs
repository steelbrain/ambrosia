//! Apple's flattened device tree, which iBoot hands the kernel, as a tree
//! Ambrosia can edit and write out again.
//!
//! A node is a property count and a child count, both 32-bit little-endian,
//! then its properties, then its children. A property is a 32-byte
//! NUL-padded name, a 32-bit length whose top bit is a flag, and its value
//! padded to 4 bytes. In the template a firmware starts from, the flag marks
//! a property the firmware is to fill in.

use alloc::string::String;
use alloc::vec::Vec;

/// Why a device tree didn't parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The tree ends inside a node or property.
    Truncated {
        /// The offset of the field that runs past the end.
        at: usize,
    },
    /// A property name that isn't NUL-terminated within 32 bytes, or isn't
    /// UTF-8.
    BadName {
        /// The offset of the name.
        at: usize,
    },
    /// Deeper nesting than any real tree has.
    TooBig,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated { at } => {
                write!(f, "it ends inside a node or property, at byte {at}")
            }
            Error::BadName { at } => write!(
                f,
                "the property name at byte {at} is not UTF-8 ended by a NUL within 32 bytes"
            ),
            Error::TooBig => f.write_str("its nodes nest deeper than any real tree's"),
        }
    }
}

/// A property name's field, which holds the name and its NUL.
const NAME_LEN: usize = 32;
/// The length's top bit, the property's flag.
const FLAG: u32 = 1 << 31;
/// The deepest nesting [`Node::parse`] takes, below the root.
const MAX_DEPTH: usize = 32;

/// A property: its name, its flag and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// The name, without its NUL. The tree holds at most 31 bytes of it.
    pub name: String,
    /// The length's top bit, which in a template marks a property the
    /// firmware is to fill in.
    pub flag: bool,
    /// The value, without its padding.
    pub value: Vec<u8>,
}

/// A node: its properties, in the tree's order, and its children.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// The node's properties.
    pub properties: Vec<Property>,
    /// The nodes below this one.
    pub children: Vec<Node>,
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, Error> {
    let b = data
        .get(at..)
        .and_then(<[u8]>::first_chunk)
        .ok_or(Error::Truncated { at })?;
    Ok(u32::from_le_bytes(*b))
}

impl Node {
    /// Parses a tree, and gives it with the length it took.
    pub fn parse(data: &[u8]) -> Result<(Node, usize), Error> {
        let mut at = 0;
        let node = Self::parse_at(data, &mut at, 0)?;
        Ok((node, at))
    }

    fn parse_at(data: &[u8], at: &mut usize, depth: usize) -> Result<Node, Error> {
        if depth > MAX_DEPTH {
            return Err(Error::TooBig);
        }
        let nprops = u32_at(data, *at)?;
        let nchildren = u32_at(data, *at + 4)?;
        *at += 8;
        // Each property takes at least 36 bytes and each child 8, so counts
        // the rest of the data can't hold are corrupt.
        let least = u64::from(nprops) * 36 + u64::from(nchildren) * 8;
        if least > (data.len() - *at) as u64 {
            return Err(Error::Truncated { at: *at });
        }
        let mut node = Node {
            properties: Vec::with_capacity(nprops as usize),
            children: Vec::with_capacity(nchildren as usize),
        };
        for _ in 0..nprops {
            let raw = data
                .get(*at..*at + NAME_LEN)
                .ok_or(Error::Truncated { at: *at })?;
            let end = raw
                .iter()
                .position(|&b| b == 0)
                .ok_or(Error::BadName { at: *at })?;
            let name = core::str::from_utf8(&raw[..end]).map_err(|_| Error::BadName { at: *at })?;
            let len = u32_at(data, *at + NAME_LEN)?;
            let start = *at + NAME_LEN + 4;
            let n = (len & !FLAG) as usize;
            let padded = n.next_multiple_of(4);
            let value = data
                .get(start..start + padded)
                .ok_or(Error::Truncated { at: start })?;
            node.properties.push(Property {
                name: name.into(),
                flag: len & FLAG != 0,
                value: value[..n].to_vec(),
            });
            *at = start + padded;
        }
        for _ in 0..nchildren {
            node.children.push(Self::parse_at(data, at, depth + 1)?);
        }
        Ok(node)
    }

    /// The tree's bytes.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.size());
        self.write(&mut out);
        out
    }

    /// The length [`serialize`](Self::serialize) gives.
    pub fn size(&self) -> usize {
        8 + self
            .properties
            .iter()
            .map(|p| NAME_LEN + 4 + p.value.len().next_multiple_of(4))
            .sum::<usize>()
            + self.children.iter().map(Node::size).sum::<usize>()
    }

    /// Appends the node's bytes to `out`. A name longer than 31 bytes is cut
    /// to 31, to leave room for its NUL.
    fn write(&self, out: &mut Vec<u8>) {
        out.extend((self.properties.len() as u32).to_le_bytes());
        out.extend((self.children.len() as u32).to_le_bytes());
        for p in &self.properties {
            let mut name = [0u8; NAME_LEN];
            let n = p.name.len().min(NAME_LEN - 1);
            name[..n].copy_from_slice(&p.name.as_bytes()[..n]);
            out.extend(name);
            let len = p.value.len() as u32 | if p.flag { FLAG } else { 0 };
            out.extend(len.to_le_bytes());
            out.extend(&p.value);
            out.resize(out.len().next_multiple_of(4), 0);
        }
        for c in &self.children {
            c.write(out);
        }
    }

    /// The node's `name` property, without its NUL.
    pub fn name(&self) -> &str {
        self.get("name")
            .and_then(|v| core::str::from_utf8(v.split(|&b| b == 0).next()?).ok())
            .unwrap_or("")
    }

    /// The value of the property `name`.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.property(name).map(|p| &p.value[..])
    }

    /// The property `name`, with its flag.
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties.iter().find(|p| p.name == name)
    }

    /// The property `name`'s first 32 bits.
    pub fn get_u32(&self, name: &str) -> Option<u32> {
        Some(u32::from_le_bytes(
            self.get(name)?.get(..4)?.try_into().ok()?,
        ))
    }

    /// Sets the property `name` to `value`, adding it at the end if the node
    /// has none, and clears its flag.
    pub fn set(&mut self, name: &str, value: &[u8]) {
        match self.properties.iter_mut().find(|p| p.name == name) {
            Some(p) => {
                p.value.clear();
                p.value.extend_from_slice(value);
                p.flag = false;
            }
            None => self.properties.push(Property {
                name: name.into(),
                flag: false,
                value: value.to_vec(),
            }),
        }
    }

    /// Sets the property `name` to `v`, as 32 bits.
    pub fn set_u32(&mut self, name: &str, v: u32) {
        self.set(name, &v.to_le_bytes());
    }

    /// Sets the property `name` to `v`, as 64 bits.
    pub fn set_u64(&mut self, name: &str, v: u64) {
        self.set(name, &v.to_le_bytes());
    }

    /// Sets a string property, with its NUL.
    pub fn set_str(&mut self, name: &str, s: &str) {
        let mut v = Vec::with_capacity(s.len() + 1);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        self.set(name, &v);
    }

    /// Removes the property `name`, if the node has it.
    pub fn remove(&mut self, name: &str) {
        self.properties.retain(|p| p.name != name);
    }

    /// The child named `name`.
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name() == name)
    }

    /// The child named `name`, to edit.
    pub fn child_mut(&mut self, name: &str) -> Option<&mut Node> {
        self.children.iter_mut().find(|c| c.name() == name)
    }

    /// The node at a path of names below this one, such as
    /// `"chosen/memory-map"`.
    pub fn find(&self, path: &str) -> Option<&Node> {
        path.split('/')
            .filter(|s| !s.is_empty())
            .try_fold(self, |n, part| n.child(part))
    }

    /// The node at a path of names below this one, to edit.
    pub fn find_mut(&mut self, path: &str) -> Option<&mut Node> {
        path.split('/')
            .filter(|s| !s.is_empty())
            .try_fold(self, |n, part| n.child_mut(part))
    }

    /// Removes the child named `name`, and gives it, if the node has one.
    pub fn remove_child(&mut self, name: &str) -> Option<Node> {
        let i = self.children.iter().position(|c| c.name() == name)?;
        Some(self.children.remove(i))
    }

    /// Visits this node and every node below it, depth first, and stops at
    /// the first error `f` gives.
    pub fn walk_mut<E>(&mut self, f: &mut impl FnMut(&mut Node) -> Result<(), E>) -> Result<(), E> {
        f(self)?;
        for c in &mut self.children {
            c.walk_mut(f)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Node {
        let mut root = Node::default();
        root.set_str("name", "device-tree");
        let mut chosen = Node::default();
        chosen.set_str("name", "chosen");
        chosen.set_u32("secure-boot", 1);
        let mut map = Node::default();
        map.set_str("name", "memory-map");
        map.set("DeviceTree", &[0; 16]);
        chosen.children.push(map);
        root.children.push(chosen);
        root
    }

    #[test]
    fn round_trip() {
        let t = sample();
        let bytes = t.serialize();
        assert_eq!(bytes.len(), t.size());
        let (back, len) = Node::parse(&bytes).unwrap();
        assert_eq!(len, bytes.len());
        assert_eq!(back, t);
        assert_eq!(back.find("chosen").unwrap().get_u32("secure-boot"), Some(1));
        assert!(back.find("chosen/memory-map").is_some());
        assert!(back.find("chosen/nothing").is_none());
    }

    #[test]
    fn flag_and_padding() {
        let mut t = sample();
        t.properties.push(Property {
            name: "placeholder".into(),
            flag: true,
            value: vec![1, 2, 3],
        });
        let bytes = t.serialize();
        let (back, _) = Node::parse(&bytes).unwrap();
        let p = back.property("placeholder").unwrap();
        assert!(p.flag);
        assert_eq!(p.value, [1, 2, 3]);
        let mut back = back;
        back.set("placeholder", &[4]);
        assert!(!back.property("placeholder").unwrap().flag);
    }

    #[test]
    fn corrupt_counts() {
        let mut bytes = sample().serialize();
        bytes[4] = 0xff;
        assert!(Node::parse(&bytes).is_err());
        assert!(Node::parse(&bytes[..20]).is_err());
    }

    #[test]
    fn walk_stops_at_error() {
        let mut t = sample();
        let mut seen = Vec::new();
        let r = t.walk_mut(&mut |n| {
            seen.push(n.name().to_string());
            if n.name() == "chosen" {
                Err("stop")
            } else {
                Ok(())
            }
        });
        assert_eq!(r, Err("stop"));
        assert_eq!(seen, ["device-tree", "chosen"]);
    }

    /// A tree whose last value ends without its padding is cut short, and
    /// parsing it gives no length past its end.
    #[test]
    fn unpadded_end() {
        let mut t = Node::default();
        t.set("odd", &[1, 2, 3]);
        let bytes = t.serialize();
        assert_eq!(bytes.len(), 48);
        assert_eq!(Node::parse(&bytes).unwrap().1, 48);
        assert_eq!(Node::parse(&bytes[..47]), Err(Error::Truncated { at: 44 }));
    }

    /// Nesting past `MAX_DEPTH` is refused rather than recursed into.
    #[test]
    fn depth() {
        let nested = |levels: usize| {
            let mut n = Node::default();
            for _ in 1..levels {
                n = Node {
                    properties: Vec::new(),
                    children: vec![n],
                };
            }
            n.serialize()
        };
        assert!(Node::parse(&nested(MAX_DEPTH + 1)).is_ok());
        assert_eq!(Node::parse(&nested(MAX_DEPTH + 2)), Err(Error::TooBig));
    }
}
