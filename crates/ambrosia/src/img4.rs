//! Image4, the container iBoot's boot objects come in, as far as Ambrosia
//! needs it: the payload of an IM4P, the type that names it, and the
//! manifest (IM4M) an IMG4 carries. Ambrosia checks no signatures, since it
//! is the firmware that would.
//!
//! - IMG4: `SEQUENCE { "IMG4", IM4P, [0] IM4M, [1] IM4R (optional) }`
//! - IM4P: `SEQUENCE { "IM4P", type, description, OCTET STRING payload,
//!   KBAG (optional), SEQUENCE { INTEGER algorithm, INTEGER size } (optional) }`
//!
//! A payload is LZFSE (it starts with an LZFSE block magic, `bvx…`), LZSS
//! (it starts with a `complzss` header), or the object itself.

use crate::der::{self, Element, Reader};
use alloc::vec::Vec;

/// Why a boot object can't be read or unpacked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The container's DER is malformed.
    Der(der::Error),
    /// A container that doesn't start with the magic its kind needs.
    NotImage4,
    /// A payload encrypted with a key bag, which Ambrosia can't open.
    Encrypted,
    /// A payload whose unpacked size is unknown or doesn't match its header.
    BadSize,
    /// The caller's buffer is too small for the unpacked payload.
    TooSmall {
        /// The unpacked size.
        need: usize,
    },
    /// The heap can't hold the unpacked payload.
    NoMemory {
        /// The unpacked size.
        need: usize,
    },
    /// The compressed payload is corrupt.
    Corrupt,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Der(e) => write!(f, "its DER is malformed: {e}"),
            Error::NotImage4 => f.write_str("not an IMG4 or IM4P file"),
            Error::Encrypted => f.write_str("its payload is encrypted"),
            Error::BadSize => f.write_str(
                "its payload's unpacked size is unknown or doesn't match what it unpacks to",
            ),
            Error::TooSmall { need } => {
                write!(
                    f,
                    "its payload unpacks to {need} bytes, more than its space"
                )
            }
            Error::NoMemory { need } => write!(
                f,
                "its payload unpacks to {need} bytes, more than the heap holds"
            ),
            Error::Corrupt => f.write_str("its compressed payload is corrupt"),
        }
    }
}

impl From<der::Error> for Error {
    fn from(e: der::Error) -> Self {
        Error::Der(e)
    }
}

/// How a payload is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packing {
    /// Not packed: the payload is the object.
    Raw,
    /// LZFSE blocks.
    Lzfse,
    /// LZSS, after a `complzss` header.
    Lzss,
}

/// An IM4P: one boot object.
#[derive(Debug, Clone, Copy)]
pub struct Payload<'a> {
    /// Its four-character type, such as `krnl` or `dtre`.
    pub kind: [u8; 4],
    /// Its description, free text, such as the name and version of the
    /// project that built it.
    pub description: &'a [u8],
    /// The payload as stored, packed as `packing` says.
    pub data: &'a [u8],
    /// How `data` is packed.
    pub packing: Packing,
    /// The unpacked size: the data's length if it is raw, and otherwise
    /// what the LZSS header or the IM4P gives, if either does.
    pub size: Option<usize>,
}

/// An IMG4: a payload and the manifest it was personalised with.
#[derive(Debug, Clone, Copy)]
pub struct Image<'a> {
    /// The IM4P.
    pub payload: Payload<'a>,
    /// The IM4M, whole, as its DER bytes.
    pub manifest: Option<&'a [u8]>,
}

/// The next element's contents, if it is an IA5String.
fn string<'a>(r: &mut Reader<'a>) -> Result<&'a [u8], Error> {
    Ok(r.element()?.expect(der::IA5_STRING)?)
}

/// The IM4P that `e` is.
fn payload(e: Element<'_>) -> Result<Payload<'_>, Error> {
    let mut r = Reader::new(e.expect(der::SEQUENCE)?);
    if string(&mut r)? != b"IM4P" {
        return Err(Error::NotImage4);
    }
    let kind: [u8; 4] = string(&mut r)?.try_into().map_err(|_| Error::NotImage4)?;
    let description = string(&mut r)?;
    let data = r.element()?.expect(der::OCTET_STRING)?;
    let mut size = None;
    while !r.is_empty() {
        let e = r.element()?;
        match e.tag {
            // A key bag: the payload is encrypted.
            der::OCTET_STRING => return Err(Error::Encrypted),
            der::SEQUENCE => {
                let mut info = Reader::new(e.contents);
                let _algorithm = info.element()?.uint()?;
                let n = info.element()?.uint()?;
                size = Some(usize::try_from(n).map_err(|_| Error::BadSize)?);
            }
            _ => {}
        }
    }
    let packing = if data.starts_with(b"bvx") {
        Packing::Lzfse
    } else if data.starts_with(b"complzss") {
        size = Some(lzss_header(data)?.0);
        Packing::Lzss
    } else {
        size = Some(data.len());
        Packing::Raw
    };
    Ok(Payload {
        kind,
        description,
        data,
        packing,
        size,
    })
}

/// Parses a bare IM4P file.
pub fn parse_im4p(data: &[u8]) -> Result<Payload<'_>, Error> {
    payload(der::parse(data)?)
}

/// Parses an IMG4 file.
pub fn parse_img4(data: &[u8]) -> Result<Image<'_>, Error> {
    let mut r = Reader::new(der::parse(data)?.expect(der::SEQUENCE)?);
    if string(&mut r)? != b"IMG4" {
        return Err(Error::NotImage4);
    }
    let payload = payload(r.element()?)?;
    let mut manifest = None;
    while !r.is_empty() {
        let e = r.element()?;
        // [0], constructed: the manifest.
        if e.tag == 0xa0 {
            manifest = Some(der::parse(e.contents)?.raw);
        }
    }
    Ok(Image { payload, manifest })
}

/// Parses either kind of file.
pub fn parse(data: &[u8]) -> Result<Payload<'_>, Error> {
    match parse_img4(data) {
        Ok(image) => Ok(image.payload),
        Err(Error::NotImage4) => parse_im4p(data),
        Err(e) => Err(e),
    }
}

impl Payload<'_> {
    /// Unpacks the payload into `out`, and gives its length.
    pub fn unpack_into(&self, out: &mut [u8]) -> Result<usize, Error> {
        let need = self.size.ok_or(Error::BadSize)?;
        if out.len() < need {
            return Err(Error::TooSmall { need });
        }
        let out = &mut out[..need];
        // `size` is what parsing found for raw and LZSS payloads; a payload
        // built another way may disagree with its data.
        match self.packing {
            Packing::Raw if self.data.len() == need => out.copy_from_slice(self.data),
            Packing::Raw => return Err(Error::BadSize),
            Packing::Lzfse => lzfse(self.data, out)?,
            Packing::Lzss => {
                let (size, body) = lzss_header(self.data)?;
                if size != need || lzss(body, out) != need {
                    return Err(Error::BadSize);
                }
            }
        }
        Ok(need)
    }

    /// Unpacks the payload into a new buffer.
    pub fn unpack(&self) -> Result<Vec<u8>, Error> {
        // The size comes from the file, so it may be more than the heap holds.
        let need = self.size.ok_or(Error::BadSize)?;
        let mut out = Vec::new();
        out.try_reserve_exact(need)
            .map_err(|_| Error::NoMemory { need })?;
        out.resize(need, 0);
        self.unpack_into(&mut out)?;
        Ok(out)
    }
}

/// Decodes an LZFSE stream that unpacks to exactly `out.len()` bytes.
fn lzfse(data: &[u8], out: &mut [u8]) -> Result<(), Error> {
    use compcol::{Decoder, Status};
    let mut d = compcol::lzfse::Decoder::new();
    let (mut consumed, mut written) = (0, 0);
    loop {
        // Once `out` is full, what is left must be the end of the stream.
        let mut spare = [0u8; 1];
        let full = written == out.len();
        let dst = if full {
            &mut spare[..]
        } else {
            &mut out[written..]
        };
        let (p, status) = d
            .decode(&data[consumed..], dst)
            .map_err(|_| Error::Corrupt)?;
        if full && p.written != 0 {
            return Err(Error::BadSize);
        }
        consumed += p.consumed;
        written += p.written;
        match status {
            Status::StreamEnd if written == out.len() => return Ok(()),
            Status::StreamEnd => return Err(Error::BadSize),
            _ if p.consumed == 0 && p.written == 0 => return Err(Error::Corrupt),
            _ => {}
        }
    }
}

/// The `complzss` header: magic, Adler-32, unpacked size, packed size, all
/// big-endian, then padding to 0x180 bytes. Gives the unpacked size and the
/// packed stream.
fn lzss_header(data: &[u8]) -> Result<(usize, &[u8]), Error> {
    let be = |at: usize| -> Result<usize, Error> {
        let field = data.get(at..).and_then(<[u8]>::first_chunk);
        Ok(u32::from_be_bytes(*field.ok_or(Error::BadSize)?) as usize)
    };
    let (size, packed) = (be(12)?, be(16)?);
    let body = data.get(0x180..).and_then(|d| d.get(..packed));
    Ok((size, body.ok_or(Error::BadSize)?))
}

/// The LZSS kernelcaches have used since before Image4: a 4 KiB window that
/// starts filled with spaces, at 0xfee; a flag byte for each eight items,
/// whose set bits are literals; a match is 12 bits of window position and 4
/// of length minus 3. Gives the bytes written.
fn lzss(data: &[u8], out: &mut [u8]) -> usize {
    const N: usize = 4096;
    const F: usize = 18;
    const THRESHOLD: usize = 2;
    let mut window = [b' '; N];
    let mut r = N - F;
    let (mut i, mut o) = (0, 0);
    let mut flags: u32 = 0;
    while o < out.len() {
        flags >>= 1;
        if flags & 0x100 == 0 {
            let Some(&f) = data.get(i) else { break };
            i += 1;
            flags = u32::from(f) | 0xff00;
        }
        if flags & 1 != 0 {
            let Some(&c) = data.get(i) else { break };
            i += 1;
            out[o] = c;
            o += 1;
            window[r] = c;
            r = (r + 1) & (N - 1);
        } else {
            let (Some(&a), Some(&b)) = (data.get(i), data.get(i + 1)) else {
                break;
            };
            i += 2;
            let pos = a as usize | ((b as usize & 0xf0) << 4);
            let len = (b as usize & 0x0f) + THRESHOLD + 1;
            for k in 0..len {
                if o == out.len() {
                    break;
                }
                let c = window[(pos + k) & (N - 1)];
                out[o] = c;
                o += 1;
                window[r] = c;
                r = (r + 1) & (N - 1);
            }
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    fn der(tag: u8, contents: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
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

    fn im4p(kind: &[u8; 4], payload: &[u8], extra: &[u8]) -> Vec<u8> {
        let mut c = der(der::IA5_STRING, b"IM4P");
        c.extend(der(der::IA5_STRING, kind));
        c.extend(der(der::IA5_STRING, b"test"));
        c.extend(der(der::OCTET_STRING, payload));
        c.extend_from_slice(extra);
        der(der::SEQUENCE, &c)
    }

    #[test]
    fn raw_payload() {
        let f = im4p(b"dtre", b"hello", &[]);
        let p = parse(&f).unwrap();
        assert_eq!(&p.kind, b"dtre");
        assert_eq!(p.packing, Packing::Raw);
        assert_eq!(p.unpack().unwrap(), b"hello");
        let short = Payload { size: Some(3), ..p };
        assert_eq!(short.unpack().unwrap_err(), Error::BadSize);
    }

    #[test]
    fn img4_with_manifest() {
        let p = im4p(b"krnl", b"abc", &[]);
        let m = der(der::SEQUENCE, &der(der::IA5_STRING, b"IM4M"));
        let mut c = der(der::IA5_STRING, b"IMG4");
        c.extend(p);
        c.extend(der(0xa0, &m));
        let f = der(der::SEQUENCE, &c);
        let image = parse_img4(&f).unwrap();
        assert_eq!(&image.payload.kind, b"krnl");
        assert_eq!(image.manifest, Some(&m[..]));
    }

    #[test]
    fn encrypted_payload() {
        let kbag = der(der::OCTET_STRING, &[1, 2, 3]);
        let f = im4p(b"krnl", b"x", &kbag);
        assert_eq!(parse(&f).unwrap_err(), Error::Encrypted);
    }

    #[test]
    fn lzfse_payload() {
        // An LZFSE stream of one uncompressed block, then the end marker.
        let mut s = b"bvx-".to_vec();
        s.extend(5u32.to_le_bytes());
        s.extend(b"hello");
        s.extend(b"bvx$");
        let mut info = der(der::INTEGER, &[1]);
        info.extend(der(der::INTEGER, &[5]));
        let f = im4p(b"krnl", &s, &der(der::SEQUENCE, &info));
        let p = parse(&f).unwrap();
        assert_eq!((p.packing, p.size), (Packing::Lzfse, Some(5)));
        assert_eq!(p.unpack().unwrap(), b"hello");
        let mut small = [0u8; 4];
        assert_eq!(
            p.unpack_into(&mut small).unwrap_err(),
            Error::TooSmall { need: 5 }
        );
        let huge = Payload {
            size: Some(usize::MAX),
            ..p
        };
        assert_eq!(
            huge.unpack().unwrap_err(),
            Error::NoMemory { need: usize::MAX }
        );
    }

    #[test]
    fn lzss_payload() {
        // "abcabcabc": three literals, then a match of six at window 0xfee.
        let body = [0b0000_0111, b'a', b'b', b'c', 0xee, 0xf3];
        let mut s = b"complzss".to_vec();
        s.extend(0u32.to_be_bytes());
        s.extend(9u32.to_be_bytes());
        s.extend((body.len() as u32).to_be_bytes());
        s.resize(0x180, 0);
        s.extend(body);
        let f = im4p(b"krnl", &s, &[]);
        let p = parse(&f).unwrap();
        assert_eq!((p.packing, p.size), (Packing::Lzss, Some(9)));
        assert_eq!(p.unpack().unwrap(), b"abcabcabc");
        let short = Payload { size: Some(3), ..p };
        assert_eq!(short.unpack().unwrap_err(), Error::BadSize);
        // A header cut off in its sizes, or before the packed stream ends.
        for len in [14, 0x180 + body.len() - 1] {
            let f = im4p(b"krnl", &s[..len], &[]);
            assert_eq!(parse(&f).unwrap_err(), Error::BadSize);
        }
    }
}
