//! Just enough DER to walk Image4 containers: tag, length and contents, for
//! definite lengths only, as DER requires.

/// Why an element can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The data ends inside an element.
    Truncated,
    /// A tag number wider than 32 bits.
    BadTag,
    /// A length form DER doesn't allow, or one too long for this machine.
    BadLength,
    /// An INTEGER with no contents, or one read as unsigned that is
    /// negative or wider than 64 bits.
    BadInteger,
    /// An element with another tag where this one was expected.
    Unexpected {
        /// The tag byte expected.
        want: u8,
        /// The tag byte found.
        got: u8,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Truncated => f.write_str("the data ends inside an element"),
            Error::BadTag => f.write_str("a tag number is wider than 32 bits"),
            Error::BadLength => {
                f.write_str("a length is in a form DER doesn't allow, or too large")
            }
            Error::BadInteger => f.write_str("an INTEGER is empty, negative or wider than 64 bits"),
            Error::Unexpected { want, got } => {
                write!(f, "an element has tag {got:#04x} where {want:#04x} belongs")
            }
        }
    }
}

/// The tag byte of a BOOLEAN.
pub const BOOLEAN: u8 = 0x01;
/// The tag byte of an INTEGER.
pub const INTEGER: u8 = 0x02;
/// The tag byte of an OCTET STRING.
pub const OCTET_STRING: u8 = 0x04;
/// The tag byte of an IA5String, an ASCII string.
pub const IA5_STRING: u8 = 0x16;
/// The tag byte of a SEQUENCE, whose constructed bit is set.
pub const SEQUENCE: u8 = 0x30;
/// The tag byte of a SET, whose constructed bit is set.
pub const SET: u8 = 0x31;

/// One element: its tag byte and its contents.
#[derive(Debug, Clone, Copy)]
pub struct Element<'a> {
    /// The first identifier byte: class, constructed bit and, for low tag
    /// numbers, the number.
    pub tag: u8,
    /// The tag number. Image4 manifests tag each property with its
    /// four-character code as the number, in the high-tag-number form.
    pub number: u32,
    /// The contents, after the tag and length.
    pub contents: &'a [u8],
    /// The whole element, tag and length included.
    pub raw: &'a [u8],
}

impl<'a> Element<'a> {
    /// The contents, if the tag is `tag`.
    pub fn expect(self, tag: u8) -> Result<&'a [u8], Error> {
        if self.tag == tag {
            Ok(self.contents)
        } else {
            Err(Error::Unexpected {
                want: tag,
                got: self.tag,
            })
        }
    }

    /// An INTEGER's value, if it is non-negative and fits in 64 bits.
    pub fn uint(self) -> Result<u64, Error> {
        // Two's complement, big-endian: a value whose top bit is set takes
        // a leading zero byte to stay positive.
        let digits = match self.expect(INTEGER)? {
            [] => return Err(Error::BadInteger),
            [first, ..] if first & 0x80 != 0 => return Err(Error::BadInteger),
            [0, rest @ ..] => rest,
            c => c,
        };
        if digits.len() > 8 {
            return Err(Error::BadInteger);
        }
        Ok(digits.iter().fold(0, |v, &b| (v << 8) | u64::from(b)))
    }
}

/// Reads the elements of a constructed element's contents, one at a time.
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    /// A reader of the elements `data` holds, one after another.
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    /// Whether every element has been read.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The next element.
    pub fn element(&mut self) -> Result<Element<'a>, Error> {
        let d = self.data;
        let (&tag, mut rest) = d.split_first().ok_or(Error::Truncated)?;
        let mut number = u32::from(tag & 0x1f);
        // The high-tag-number form: base-128 digits, high bit set on all
        // but the last.
        if number == 0x1f {
            number = 0;
            loop {
                let (&b, r) = rest.split_first().ok_or(Error::Truncated)?;
                rest = r;
                if number >> 25 != 0 {
                    return Err(Error::BadTag);
                }
                number = (number << 7) | u32::from(b & 0x7f);
                if b & 0x80 == 0 {
                    break;
                }
            }
        }
        let (&first, rest) = rest.split_first().ok_or(Error::Truncated)?;
        let (len, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else {
            // The long form: the low bits count the length's big-endian
            // bytes, which follow. With none, it is BER's indefinite form.
            let n = usize::from(first & 0x7f);
            if n == 0 || n > size_of::<usize>() {
                return Err(Error::BadLength);
            }
            let (digits, rest) = rest.split_at_checked(n).ok_or(Error::Truncated)?;
            let len = digits.iter().fold(0, |v, &b| (v << 8) | usize::from(b));
            (len, rest)
        };
        let header = d.len() - rest.len();
        let (contents, after) = rest.split_at_checked(len).ok_or(Error::Truncated)?;
        self.data = after;
        Ok(Element {
            tag,
            number,
            contents,
            raw: &d[..header + len],
        })
    }
}

/// The single element `data` holds.
pub fn parse(data: &[u8]) -> Result<Element<'_>, Error> {
    Reader::new(data).element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_long_lengths() {
        let short = [0x04, 0x02, 0xaa, 0xbb];
        let e = parse(&short).unwrap();
        assert_eq!((e.tag, e.contents), (OCTET_STRING, &[0xaa, 0xbb][..]));
        let mut long = vec![0x04, 0x82, 0x01, 0x00];
        long.extend(core::iter::repeat_n(7u8, 256));
        let e = parse(&long).unwrap();
        assert_eq!(e.contents.len(), 256);
        assert_eq!(e.raw.len(), 260);
    }

    #[test]
    fn truncated() {
        assert_eq!(parse(&[0x04, 0x05, 1, 2]).unwrap_err(), Error::Truncated);
        assert_eq!(parse(&[0x04, 0x82, 1]).unwrap_err(), Error::Truncated);
        assert_eq!(parse(&[0x04, 0x80]).unwrap_err(), Error::BadLength);
        assert_eq!(parse(&[0x04, 0x89, 1]).unwrap_err(), Error::BadLength);
    }

    #[test]
    fn high_tag_numbers() {
        // [PRIVATE 'ECID'], constructed, holding an empty SEQUENCE.
        let fourcc = u32::from_be_bytes(*b"ECID");
        let mut d = vec![0xff];
        for shift in [28, 21, 14, 7] {
            d.push(0x80 | ((fourcc >> shift) & 0x7f) as u8);
        }
        d.push((fourcc & 0x7f) as u8);
        d.extend([0x02, 0x30, 0x00]);
        let e = parse(&d).unwrap();
        assert_eq!((e.tag, e.number), (0xff, fourcc));
        assert_eq!(e.contents, [0x30, 0x00]);
        // Six digits are more than 32 bits.
        let wide = [0xff, 0x81, 0x80, 0x80, 0x80, 0x80, 0x00, 0x00];
        assert_eq!(parse(&wide).unwrap_err(), Error::BadTag);
    }

    #[test]
    fn integers() {
        assert_eq!(parse(&[0x02, 0x01, 0x05]).unwrap().uint().unwrap(), 5);
        assert_eq!(
            parse(&[0x02, 0x02, 0x00, 0xff]).unwrap().uint().unwrap(),
            255
        );
        assert_eq!(
            parse(&[0x02, 0x09, 0, 1, 2, 3, 4, 5, 6, 7, 8])
                .unwrap()
                .uint()
                .unwrap(),
            0x0102_0304_0506_0708
        );
        // -1, empty, and wider than 64 bits.
        for bad in [
            &[0x02, 0x01, 0xff][..],
            &[0x02, 0x00],
            &[0x02, 0x09, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        ] {
            assert_eq!(parse(bad).unwrap().uint().unwrap_err(), Error::BadInteger);
        }
        let e = parse(&[0x16, 0x01, b'a']).unwrap();
        assert_eq!(
            e.uint().unwrap_err(),
            Error::Unexpected {
                want: INTEGER,
                got: IA5_STRING
            }
        );
    }
}
