//! Image4 manifests (IM4M): the properties iBoot copies into the device tree.
//!
//! An IM4M is SEQUENCE { "IM4M", INTEGER version, SET body, OCTET STRING
//! signature, certificates }. The body holds one MANB, a set of entries, each
//! tagged with its four-character code: MANP, the manifest's own properties,
//! and one entry per boot object (`krnl`, `dtre`, …) with that object's
//! properties. A property is SEQUENCE { IA5String code, value }, tagged with
//! its code too.

use crate::der::{self, Reader};
use alloc::vec::Vec;

/// A property's value, as the manifest's DER holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value<'a> {
    /// An INTEGER, which must be non-negative and fit in 64 bits.
    Int(u64),
    /// A BOOLEAN.
    Bool(bool),
    /// An OCTET STRING's bytes, such as a digest.
    Data(&'a [u8]),
    /// An IA5String's bytes, such as a version.
    String(&'a [u8]),
}

/// One property of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Property<'a> {
    /// Its four-character code, such as `ECID` or `DGST`.
    pub code: [u8; 4],
    /// Its value.
    pub value: Value<'a>,
}

/// One entry of the manifest body: MANP or a boot object.
#[derive(Debug, Clone)]
pub struct Entry<'a> {
    /// Its four-character code: `MANP`, or the boot object's type, such as
    /// `krnl`.
    pub code: [u8; 4],
    /// Its properties, in the manifest's order.
    pub properties: Vec<Property<'a>>,
}

/// A parsed IM4M, borrowing its bytes.
#[derive(Debug, Clone)]
pub struct Manifest<'a> {
    /// The whole IM4M, as its DER bytes.
    pub raw: &'a [u8],
    /// The body's entries, in the manifest's order.
    pub entries: Vec<Entry<'a>>,
}

/// Why a manifest can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Its DER is malformed.
    Der(der::Error),
    /// Its DER doesn't have the IM4M's layout: an element, magic or code
    /// is not the one the layout puts there.
    NotManifest,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Der(e) => write!(f, "its DER is malformed: {e}"),
            Error::NotManifest => f.write_str("its DER is not laid out as an IM4M"),
        }
    }
}

impl From<der::Error> for Error {
    fn from(e: der::Error) -> Self {
        Error::Der(e)
    }
}

/// A four-character code, from an IA5String's contents.
fn code(s: &[u8]) -> Result<[u8; 4], Error> {
    s.try_into().map_err(|_| Error::NotManifest)
}

/// Reads `SEQUENCE { IA5String code, rest }` and gives the code and a reader
/// for the rest.
fn named<'a>(contents: &'a [u8]) -> Result<([u8; 4], Reader<'a>), Error> {
    let mut r = Reader::new(der::parse(contents)?.expect(der::SEQUENCE)?);
    let c = code(r.element()?.expect(der::IA5_STRING)?)?;
    Ok((c, r))
}

/// The property in `contents`, those of the element tagged with its code.
fn property<'a>(contents: &'a [u8]) -> Result<Property<'a>, Error> {
    let (code, mut r) = named(contents)?;
    let v = r.element()?;
    let value = match v.tag {
        der::INTEGER => Value::Int(v.uint()?),
        der::BOOLEAN => Value::Bool(v.contents.iter().any(|&b| b != 0)),
        der::OCTET_STRING => Value::Data(v.contents),
        der::IA5_STRING => Value::String(v.contents),
        _ => return Err(Error::NotManifest),
    };
    Ok(Property { code, value })
}

impl<'a> Manifest<'a> {
    /// Reads the IM4M at the start of `raw`.
    pub fn parse(raw: &'a [u8]) -> Result<Manifest<'a>, Error> {
        let outer = der::parse(raw)?;
        let mut r = Reader::new(outer.expect(der::SEQUENCE)?);
        if r.element()?.expect(der::IA5_STRING)? != b"IM4M" {
            return Err(Error::NotManifest);
        }
        let _version = r.element()?.uint()?;
        let mut body = Reader::new(r.element()?.expect(der::SET)?);
        let manb = body.element()?;
        let (c, mut manb) = named(manb.contents)?;
        if &c != b"MANB" {
            return Err(Error::NotManifest);
        }
        let mut set = Reader::new(manb.element()?.expect(der::SET)?);
        let mut entries = Vec::new();
        while !set.is_empty() {
            let e = set.element()?;
            let (code, mut er) = named(e.contents)?;
            let mut props = Reader::new(er.element()?.expect(der::SET)?);
            let mut properties = Vec::new();
            while !props.is_empty() {
                properties.push(property(props.element()?.contents)?);
            }
            entries.push(Entry { code, properties });
        }
        Ok(Manifest {
            raw: outer.raw,
            entries,
        })
    }

    /// The entry with `code`: `MANP`, or a boot object's.
    pub fn entry(&self, code: &[u8; 4]) -> Option<&Entry<'a>> {
        self.entries.iter().find(|e| &e.code == code)
    }

    /// The manifest's own property `code`, from MANP.
    pub fn get(&self, code: &[u8; 4]) -> Option<Value<'a>> {
        self.entry(b"MANP")?.get(code)
    }
}

impl<'a> Entry<'a> {
    /// The entry's property `code`.
    pub fn get(&self, code: &[u8; 4]) -> Option<Value<'a>> {
        self.properties
            .iter()
            .find(|p| &p.code == code)
            .map(|p| p.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn der(tag: &[u8], contents: &[u8]) -> Vec<u8> {
        let mut v = tag.to_vec();
        if contents.len() < 0x80 {
            v.push(contents.len() as u8);
        } else {
            v.push(0x82);
            v.extend((contents.len() as u16).to_be_bytes());
        }
        v.extend_from_slice(contents);
        v
    }

    /// The identifier of a constructed private element numbered `code`.
    fn private(code: &[u8; 4]) -> Vec<u8> {
        let n = u32::from_be_bytes(*code);
        let mut v = vec![0xff];
        for shift in [28, 21, 14, 7] {
            v.push(0x80 | ((n >> shift) & 0x7f) as u8);
        }
        v.push((n & 0x7f) as u8);
        v
    }

    fn prop(code: &[u8; 4], value: &[u8]) -> Vec<u8> {
        let mut s = der(&[der::IA5_STRING], code);
        s.extend_from_slice(value);
        der(&private(code), &der(&[der::SEQUENCE], &s))
    }

    fn entry(code: &[u8; 4], props: &[Vec<u8>]) -> Vec<u8> {
        let mut s = der(&[der::IA5_STRING], code);
        s.extend(der(&[der::SET], &props.concat()));
        der(&private(code), &der(&[der::SEQUENCE], &s))
    }

    fn manifest(entries: &[Vec<u8>]) -> Vec<u8> {
        // MANB holds a SET of entries, as an entry holds a SET of properties.
        let manb = entry(b"MANB", entries);
        let mut m = der(&[der::IA5_STRING], b"IM4M");
        m.extend(der(&[der::INTEGER], &[0]));
        m.extend(der(&[der::SET], &manb));
        m.extend(der(&[der::OCTET_STRING], &[0; 4]));
        der(&[der::SEQUENCE], &m)
    }

    #[test]
    fn properties() {
        let m = manifest(&[
            entry(
                b"MANP",
                &[
                    prop(b"ECID", &der(&[der::INTEGER], &[0x00, 0xbe, 0xef])),
                    prop(b"CPRO", &der(&[der::BOOLEAN], &[0xff])),
                    prop(b"apmv", &der(&[der::IA5_STRING], b"27.0.1")),
                ],
            ),
            entry(
                b"krnl",
                &[prop(b"DGST", &der(&[der::OCTET_STRING], &[7; 48]))],
            ),
        ]);
        let mut padded = m.clone();
        padded.extend([0; 16]);
        let p = Manifest::parse(&padded).unwrap();
        assert_eq!(p.raw, &m[..]);
        assert_eq!(p.get(b"ECID"), Some(Value::Int(0xbeef)));
        assert_eq!(p.get(b"CPRO"), Some(Value::Bool(true)));
        assert_eq!(p.get(b"apmv"), Some(Value::String(b"27.0.1")));
        let k = p.entry(b"krnl").unwrap();
        assert_eq!(k.get(b"DGST"), Some(Value::Data(&[7; 48])));
    }
}
