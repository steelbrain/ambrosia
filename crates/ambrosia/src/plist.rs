//! Property lists in Apple's XML form: Ambrosia's configuration, each kext's
//! Info.plist, and the kernel collection's prelink info.
//!
//! The parser takes what Apple's tools and the kernel's own serializer
//! write: the XML declaration, a DOCTYPE, comments, the `plist` element or a
//! bare value, the usual entities, and the kernel's `ID`/`IDREF` attributes,
//! with which the prelink info shares a value between places (an element
//! with `IDREF="n"` stands for the earlier one with `ID="n"`). Integers are
//! decimal or `0x` hexadecimal, and may be negative.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A property list value. A dictionary keeps its keys in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A dictionary: its keys and their values.
    Dict(Vec<(String, Value)>),
    /// An array.
    Array(Vec<Value>),
    /// A string, its entities decoded.
    String(String),
    /// An integer, kept as its 64 bits: a negative one in two's complement.
    Integer(u64),
    /// A real, kept as written.
    Real(String),
    /// A boolean, `true` or `false`.
    Bool(bool),
    /// Data, decoded from its base64.
    Data(Vec<u8>),
    /// A date, kept as written.
    Date(String),
}

impl Value {
    /// The value under `key`, if this is a dictionary that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The integer's 64 bits, if this is one.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The data's bytes, if this is data.
    pub fn as_data(&self) -> Option<&[u8]> {
        match self {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }

    /// The array's values, if this is an array.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// Why a document didn't parse, and the byte offset where it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The byte offset in the document.
    pub at: usize,
    /// What is wrong there.
    pub what: &'static str,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} at byte {}", self.what, self.at)
    }
}

/// The deepest nesting accepted, so a hostile document can't exhaust the
/// stack. A value an `IDREF` copies counts at the depth it is copied to.
const MAX_DEPTH: usize = 64;

/// Parses a document, or a bare value, into its one value.
pub fn parse(text: &[u8]) -> Result<Value, Error> {
    let mut p = Parser {
        s: text,
        at: 0,
        ids: Vec::new(),
        copy_budget: text.len(),
    };
    p.misc()?;
    let tag = p.open()?;
    let value = if tag.name == b"plist" {
        if tag.empty {
            return Err(p.err("empty plist"));
        }
        p.misc()?;
        let inner = p.open()?;
        let v = p.value(inner, 0)?;
        p.misc()?;
        p.close(b"plist")?;
        v
    } else {
        p.value(tag, 0)?
    };
    p.misc()?;
    if p.at != p.s.len() {
        return Err(p.err("text after the value"));
    }
    Ok(value)
}

/// An opening or empty-element tag, with the attributes the parser uses.
struct Tag<'a> {
    name: &'a [u8],
    /// Its `ID` attribute.
    id: Option<&'a [u8]>,
    /// Its `IDREF` attribute.
    idref: Option<&'a [u8]>,
    /// Whether it is an empty-element tag, `<x/>`, which has no contents
    /// and no closing tag.
    empty: bool,
}

/// The document being parsed, and the parser's place in it.
struct Parser<'a> {
    s: &'a [u8],
    /// The offset of the next byte to read.
    at: usize,
    /// The values given an `ID`, for `IDREF`s later in the document.
    ids: Vec<(&'a [u8], Value)>,
    /// How much more `IDREF`s may copy, as [`measure`] counts it. A copy
    /// can hold earlier copies, so without a limit a short document could
    /// unfold into an exponentially large value. It starts at the
    /// document's length, which bounds what the document itself can hold.
    copy_budget: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, what: &'static str) -> Error {
        Error { at: self.at, what }
    }

    fn rest(&self) -> &'a [u8] {
        &self.s[self.at..]
    }

    fn skip_ws(&mut self) {
        while self.at < self.s.len() && self.s[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    /// Skips to just past `end`.
    fn skip_past(&mut self, end: &[u8]) -> Result<(), Error> {
        let pos = find(self.rest(), end).ok_or_else(|| self.err("unterminated markup"))?;
        self.at += pos + end.len();
        Ok(())
    }

    /// Skips whitespace, the XML declaration, DOCTYPE and comments.
    fn misc(&mut self) -> Result<(), Error> {
        loop {
            self.skip_ws();
            let r = self.rest();
            if r.starts_with(b"<?") {
                self.skip_past(b"?>")?;
            } else if r.starts_with(b"<!--") {
                self.skip_past(b"-->")?;
            } else if r.starts_with(b"<!DOCTYPE") {
                self.skip_past(b">")?;
            } else {
                return Ok(());
            }
        }
    }

    /// An opening (or empty-element) tag and its attributes.
    fn open(&mut self) -> Result<Tag<'a>, Error> {
        if self.rest().first() != Some(&b'<') || self.rest().get(1) == Some(&b'/') {
            return Err(self.err("expected an element"));
        }
        self.at += 1;
        let name = self.name()?;
        let mut tag = Tag {
            name,
            id: None,
            idref: None,
            empty: false,
        };
        loop {
            self.skip_ws();
            match self.rest() {
                [b'/', b'>', ..] => {
                    self.at += 2;
                    tag.empty = true;
                    return Ok(tag);
                }
                [b'>', ..] => {
                    self.at += 1;
                    return Ok(tag);
                }
                [] => return Err(self.err("unterminated tag")),
                _ => {
                    let attr = self.name()?;
                    self.skip_ws();
                    if self.rest().first() != Some(&b'=') {
                        return Err(self.err("attribute without a value"));
                    }
                    self.at += 1;
                    self.skip_ws();
                    let quote = *self.rest().first().ok_or_else(|| self.err("attribute"))?;
                    if quote != b'"' && quote != b'\'' {
                        return Err(self.err("unquoted attribute"));
                    }
                    self.at += 1;
                    let len = self
                        .rest()
                        .iter()
                        .position(|&c| c == quote)
                        .ok_or_else(|| self.err("unterminated attribute"))?;
                    let value = &self.rest()[..len];
                    self.at += len + 1;
                    match attr {
                        b"ID" => tag.id = Some(value),
                        b"IDREF" => tag.idref = Some(value),
                        _ => {}
                    }
                }
            }
        }
    }

    fn name(&mut self) -> Result<&'a [u8], Error> {
        let len = self
            .rest()
            .iter()
            .position(|&c| !(c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b':'))
            .unwrap_or(self.rest().len());
        if len == 0 {
            return Err(self.err("expected a name"));
        }
        let name = &self.rest()[..len];
        self.at += len;
        Ok(name)
    }

    fn close(&mut self, name: &[u8]) -> Result<(), Error> {
        self.skip_ws();
        let r = self.rest();
        if r.starts_with(b"</") && r[2..].starts_with(name) && r.get(2 + name.len()) == Some(&b'>')
        {
            self.at += 3 + name.len();
            Ok(())
        } else {
            Err(self.err("expected a closing tag"))
        }
    }

    /// The text up to the element's closing tag, with entities decoded.
    fn text(&mut self, tag: &Tag<'_>) -> Result<String, Error> {
        if tag.empty {
            return Ok(String::new());
        }
        let start = self.at;
        let len = self
            .rest()
            .iter()
            .position(|&c| c == b'<')
            .ok_or_else(|| self.err("unterminated text"))?;
        self.at += len;
        let raw = core::str::from_utf8(&self.s[start..start + len]).map_err(|_| Error {
            at: start,
            what: "text is not UTF-8",
        })?;
        let text = unescape(raw).ok_or(Error {
            at: start,
            what: "bad entity",
        })?;
        self.close(tag.name)?;
        Ok(text)
    }

    /// The value the element `tag` opens, through its closing tag.
    fn value(&mut self, tag: Tag<'a>, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return Err(self.err("nested too deeply"));
        }
        if let Some(r) = tag.idref {
            let v = self
                .ids
                .iter()
                .rev()
                .find(|(id, _)| *id == r)
                .map(|(_, v)| v)
                .ok_or_else(|| self.err("IDREF to no ID"))?;
            let (levels, size) = measure(v);
            if depth + levels > MAX_DEPTH + 1 {
                return Err(self.err("nested too deeply"));
            }
            self.copy_budget = self
                .copy_budget
                .checked_sub(size)
                .ok_or_else(|| self.err("IDREFs copy too much"))?;
            let v = v.clone();
            if !tag.empty {
                self.close(tag.name)?;
            }
            return Ok(v);
        }
        let id = tag.id;
        let v = match tag.name {
            b"dict" => {
                let mut d = Vec::new();
                if !tag.empty {
                    loop {
                        self.misc()?;
                        if self.rest().starts_with(b"</") {
                            self.close(b"dict")?;
                            break;
                        }
                        let k = self.open()?;
                        if k.name != b"key" {
                            return Err(self.err("expected a key"));
                        }
                        let key = self.text(&k)?;
                        self.misc()?;
                        let t = self.open()?;
                        d.push((key, self.value(t, depth + 1)?));
                    }
                }
                Value::Dict(d)
            }
            b"array" => {
                let mut a = Vec::new();
                if !tag.empty {
                    loop {
                        self.misc()?;
                        if self.rest().starts_with(b"</") {
                            self.close(b"array")?;
                            break;
                        }
                        let t = self.open()?;
                        a.push(self.value(t, depth + 1)?);
                    }
                }
                Value::Array(a)
            }
            b"string" => Value::String(self.text(&tag)?),
            b"integer" => {
                let t = self.text(&tag)?;
                Value::Integer(integer(t.trim()).ok_or_else(|| self.err("bad integer"))?)
            }
            b"real" => Value::Real(self.text(&tag)?),
            b"date" => Value::Date(self.text(&tag)?),
            b"data" => {
                let t = self.text(&tag)?;
                Value::Data(base64_decode(&t).ok_or_else(|| self.err("bad base64"))?)
            }
            b"true" | b"false" => {
                if !tag.empty {
                    self.close(tag.name)?;
                }
                Value::Bool(tag.name == b"true")
            }
            _ => return Err(self.err("unknown element")),
        };
        if let Some(id) = id {
            self.ids.push((id, v.clone()));
        }
        Ok(v)
    }
}

/// How many levels of elements `v` spans, and its size: one for each value
/// in it, plus the bytes of its keys, strings and data. It recurses as deep
/// as `v` is, which the parser keeps within `MAX_DEPTH` levels.
fn measure(v: &Value) -> (usize, usize) {
    match v {
        Value::Dict(d) => d.iter().fold((1, 1), |(levels, size), (k, v)| {
            let (l, s) = measure(v);
            (levels.max(1 + l), size + k.len() + s)
        }),
        Value::Array(a) => a.iter().fold((1, 1), |(levels, size), v| {
            let (l, s) = measure(v);
            (levels.max(1 + l), size + s)
        }),
        Value::String(s) | Value::Real(s) | Value::Date(s) => (1, 1 + s.len()),
        Value::Data(d) => (1, 1 + d.len()),
        Value::Integer(_) | Value::Bool(_) => (1, 1),
    }
}

/// The offset of the first `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// An `integer` element's text: decimal or `0x` hexadecimal, after an
/// optional sign. A negative one is kept in two's complement, so it can be
/// no lower than `i64::MIN`.
fn integer(t: &str) -> Option<u64> {
    let (neg, t) = match t.strip_prefix('-') {
        Some(t) => (true, t),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let n = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => digits(hex, 16)?,
        None => digits(t, 10)?,
    };
    if neg {
        (n <= 1 << 63).then(|| n.wrapping_neg())
    } else {
        Some(n)
    }
}

/// The number `s` writes in `radix`, if `s` is all digits: `from_str_radix`
/// alone would take a sign too.
fn digits(s: &str, radix: u32) -> Option<u64> {
    if !s.bytes().all(|c| char::from(c).is_digit(radix)) {
        return None;
    }
    u64::from_str_radix(s, radix).ok()
}

/// `raw` with its entities and character references decoded, or `None` if
/// one is unknown or has no `;`.
fn unescape(raw: &str) -> Option<String> {
    if !raw.contains('&') {
        return Some(raw.to_string());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let semi = rest[amp..].find(';')? + amp;
        let entity = &rest[amp + 1..semi];
        let c = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = match entity.strip_prefix("#x") {
                    Some(hex) => digits(hex, 16)?,
                    None => digits(entity.strip_prefix('#')?, 10)?,
                };
                char::from_u32(u32::try_from(code).ok()?)?
            }
        };
        out.push(c);
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// The base64 alphabet, in the order of the values its characters stand for.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A `data` element's text: base64, with whitespace anywhere.
fn base64_decode(t: &str) -> Option<Vec<u8>> {
    // Padding ends the data: only more padding and whitespace may follow.
    let (t, padding) = t.split_at(t.find('=').unwrap_or(t.len()));
    if !padding
        .bytes()
        .all(|c| c == b'=' || c.is_ascii_whitespace())
    {
        return None;
    }
    let mut out = Vec::with_capacity(t.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for c in t.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = BASE64.iter().position(|&b| b == c)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    // One character past the last whole byte holds too few bits for one.
    (bits != 6).then_some(out)
}

/// Appends `d` to `out` in base64, padded.
pub(crate) fn base64_encode(out: &mut String, d: &[u8]) {
    for chunk in d.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    /// `v` as a document: the XML declaration, the DOCTYPE and the
    /// `plist` element, as Apple's tools write it.
    fn to_xml(v: &Value) -> String {
        let mut out = String::from(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \
             \"-//Apple//DTD PLIST 1.0//EN\" \
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n",
        );
        write_xml(v, &mut out, 0);
        out.push_str("</plist>\n");
        out
    }

    /// `v` as one XML element and its contents, indented by `depth`
    /// tabs, with a newline after it.
    fn write_xml(v: &Value, out: &mut String, depth: usize) {
        let pad = |out: &mut String, depth: usize| {
            for _ in 0..depth {
                out.push('\t');
            }
        };
        pad(out, depth);
        match v {
            Value::Dict(d) if d.is_empty() => out.push_str("<dict/>\n"),
            Value::Dict(d) => {
                out.push_str("<dict>\n");
                for (k, v) in d {
                    pad(out, depth + 1);
                    out.push_str("<key>");
                    escape(out, k);
                    out.push_str("</key>\n");
                    write_xml(v, out, depth + 1);
                }
                pad(out, depth);
                out.push_str("</dict>\n");
            }
            Value::Array(a) if a.is_empty() => out.push_str("<array/>\n"),
            Value::Array(a) => {
                out.push_str("<array>\n");
                for v in a {
                    write_xml(v, out, depth + 1);
                }
                pad(out, depth);
                out.push_str("</array>\n");
            }
            Value::String(s) => {
                out.push_str("<string>");
                escape(out, s);
                out.push_str("</string>\n");
            }
            Value::Integer(n) => {
                let _ = writeln!(out, "<integer>{n:#x}</integer>");
            }
            Value::Real(r) => {
                out.push_str("<real>");
                escape(out, r);
                out.push_str("</real>\n");
            }
            Value::Bool(true) => out.push_str("<true/>\n"),
            Value::Bool(false) => out.push_str("<false/>\n"),
            Value::Data(d) => {
                out.push_str("<data>");
                base64_encode(out, d);
                out.push_str("</data>\n");
            }
            Value::Date(d) => {
                out.push_str("<date>");
                escape(out, d);
                out.push_str("</date>\n");
            }
        }
    }

    /// Appends `s` to `out`, with the characters that markup gives meaning to
    /// as entities.
    fn escape(out: &mut String, s: &str) {
        for c in s.chars() {
            match c {
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '&' => out.push_str("&amp;"),
                _ => out.push(c),
            }
        }
    }

    const INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<!-- a comment -->
	<key>CFBundleIdentifier</key>
	<string>org.example.test&amp;kext</string>
	<key>IOKitPersonalities</key>
	<dict>
		<key>Test</key>
		<dict>
			<key>IOProbeScore</key>
			<integer>-1</integer>
			<key>IOPCIMatch</key>
			<string>0x52471234</string>
		</dict>
	</dict>
	<key>OSBundleRequired</key>
	<string/>
	<key>Blob</key>
	<data>
	AAEC/w==
	</data>
	<key>Flags</key>
	<array>
		<true/>
		<false/>
		<integer>0x10</integer>
	</array>
	<key>Empty</key>
	<dict/>
</dict>
</plist>
"#;

    #[test]
    fn info_plist() {
        let v = parse(INFO.as_bytes()).unwrap();
        assert_eq!(
            v.get("CFBundleIdentifier").unwrap().as_str(),
            Some("org.example.test&kext")
        );
        let test = v.get("IOKitPersonalities").unwrap().get("Test").unwrap();
        assert_eq!(test.get("IOProbeScore").unwrap().as_u64(), Some(u64::MAX));
        assert_eq!(v.get("OSBundleRequired").unwrap().as_str(), Some(""));
        assert_eq!(v.get("Blob").unwrap().as_data(), Some(&[0, 1, 2, 0xff][..]));
        assert_eq!(
            v.get("Flags").unwrap(),
            &Value::Array(vec![
                Value::Bool(true),
                Value::Bool(false),
                Value::Integer(16)
            ])
        );
        assert_eq!(v.get("Empty").unwrap(), &Value::Dict(vec![]));
        // What it writes reads back the same.
        assert_eq!(parse(to_xml(&v).as_bytes()).unwrap(), v);
    }

    #[test]
    fn ids_and_bare_values() {
        // The kernel's own serializer shares values by ID, and writes the
        // prelink info as a bare dict.
        let v = parse(
            br#"<dict><key>A</key><string ID="1">x</string><key>B</key><string IDREF="1"/><key>N</key><integer size="64" ID="2">0xfffffe0007004000</integer><key>M</key><integer IDREF="2"/></dict>"#,
        )
        .unwrap();
        assert_eq!(v.get("B").unwrap().as_str(), Some("x"));
        assert_eq!(v.get("M").unwrap().as_u64(), Some(0xffff_fe00_0700_4000));
    }

    #[test]
    fn idrefs_are_limited() {
        // Each array holds a copy of the one before it, so each is a level
        // deeper than the last, though none is written nested deeply.
        let mut deep = String::from("<array><array ID=\"0\"/>");
        for i in 1..100 {
            deep += &format!("<array ID=\"{i}\"><array IDREF=\"{}\"/></array>", i - 1);
        }
        deep += "</array>";
        assert_eq!(
            parse(deep.as_bytes()).unwrap_err().what,
            "nested too deeply"
        );
        // Each array holds two copies of the one before it, so each is twice
        // as large as the last.
        let mut wide = String::from("<array><string ID=\"0\">abcdefgh</string>");
        for i in 1..64 {
            let p = i - 1;
            wide +=
                &format!("<array ID=\"{i}\"><array IDREF=\"{p}\"/><array IDREF=\"{p}\"/></array>");
        }
        wide += "</array>";
        assert_eq!(
            parse(wide.as_bytes()).unwrap_err().what,
            "IDREFs copy too much"
        );
    }

    #[test]
    fn errors() {
        for bad in [
            &b"<dict><key>A</key></dict>"[..],
            b"<dict><string>x</string></dict>",
            b"<integer>12q</integer>",
            b"<string>a</strin>",
            b"<string IDREF=\"9\"/>",
            b"<dict></dict> trailing",
            b"<string>&bogus;</string>",
            b"<string>&#+65;</string>",
            b"<integer>0x+10</integer>",
            b"<integer>-+1</integer>",
            b"<integer>-0x8000000000000001</integer>",
            b"<data>AA==AA</data>",
            b"<data>AAAAA</data>",
        ] {
            assert!(parse(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
        let deep = "<array>".repeat(100) + &"</array>".repeat(100);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn numbers() {
        for (text, want) in [
            ("<integer>-0x8000000000000000</integer>", i64::MIN as u64),
            ("<integer>+12</integer>", 12),
            ("<integer> 0XfF </integer>", 255),
        ] {
            assert_eq!(parse(text.as_bytes()).unwrap(), Value::Integer(want));
        }
        assert_eq!(
            parse(b"<string>&#65;&#x42;</string>").unwrap(),
            Value::String("AB".into())
        );
    }

    #[test]
    fn base64_round_trip() {
        for n in 0..10 {
            let d: Vec<u8> = (0..n).map(|i| (i * 37) as u8).collect();
            let mut s = String::new();
            base64_encode(&mut s, &d);
            assert_eq!(base64_decode(&s).unwrap(), d);
        }
        assert_eq!(base64_decode("A A\n= =\n").unwrap(), [0]);
    }
}
