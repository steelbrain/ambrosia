//! NVRAM, as the aux drive keeps it: banks of the same size, each a CHRP-style
//! partition image whose header carries an Adler-32 checksum, at +0x10 over
//! the bank from +0x14 on, and a generation count at +0x14. The current bank
//! is the valid one with the highest generation. iBoot hands it to the kernel
//! whole, as `/chosen/nvram-proxy-data`.

/// The bank to hand the kernel: its index, generation and bytes.
#[derive(Debug, Clone, Copy)]
pub struct Bank<'a> {
    /// Its place in the region, from 0.
    pub index: u32,
    /// Its generation count, from +0x14.
    pub generation: u32,
    /// All its bytes, header included.
    pub data: &'a [u8],
}

/// Adler-32, as zlib computes it. The sums are reduced modulo 65521 every
/// 5552 bytes, the most `b` can take in from reduced sums without passing
/// `u32::MAX`.
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

/// The current bank of an NVRAM region holding `count` banks, or `None` if
/// no bank is valid.
pub fn current(region: &[u8], count: usize) -> Option<Bank<'_>> {
    let size = region.len() / count.max(1);
    if size < 0x20 {
        return None;
    }
    region
        .chunks_exact(size)
        .enumerate()
        .filter(|(_, b)| {
            b[0] == 0x5a && &b[4..9] == b"nvram" && adler32(&b[0x14..]) == u32_at(b, 0x10)
        })
        .max_by_key(|(_, b)| u32_at(b, 0x14))
        .map(|(i, b)| Bank {
            index: i as u32,
            generation: u32_at(b, 0x14),
            data: b,
        })
}

/// The partition macOS keeps its own variables in, beside `system`. A
/// partition is a 16-byte header, `sig`, a checksum of the header, its
/// length in 16-byte units with the header, and its name in 12 bytes, then
/// NUL-terminated `name=value` entries, then zeroes.
const COMMON: &[u8] = b"common";

/// The body of the bank's `common` partition, if it has one.
fn common(bank: &mut [u8]) -> Option<&mut [u8]> {
    let mut at = 0;
    while at + 16 <= bank.len() {
        let units = u16::from_le_bytes([bank[at + 2], bank[at + 3]]) as usize;
        let end = at + units * 16;
        if units == 0 || end > bank.len() {
            return None;
        }
        let name = &bank[at + 4..at + 16];
        if name.split(|&b| b == 0).next() == Some(COMMON) {
            return Some(&mut bank[at + 16..end]);
        }
        at = end;
    }
    None
}

/// Adds `name=value` to the bank's `common` partition if no variable there
/// has `name`, and updates the bank's checksum. Returns whether it added
/// it; it doesn't if the variable is there, or if the bank has no `common`
/// partition or no room left in it.
pub fn add_if_absent(bank: &mut [u8], name: &[u8], value: &[u8]) -> bool {
    let Some(body) = common(bank) else {
        return false;
    };
    // The entries run up to the first empty one.
    let mut end = 0;
    for entry in body.split(|&b| b == 0) {
        if entry.is_empty() {
            break;
        }
        if entry
            .strip_prefix(name)
            .is_some_and(|v| v.first() == Some(&b'='))
        {
            return false;
        }
        end += entry.len() + 1;
    }
    let len = name.len() + 1 + value.len();
    // Keep a NUL after the new entry, to end the list.
    if end + len + 1 > body.len() {
        return false;
    }
    body[end..end + name.len()].copy_from_slice(name);
    body[end + name.len()] = b'=';
    body[end + name.len() + 1..end + len].copy_from_slice(value);
    body[end + len] = 0;
    let sum = adler32(&bank[0x14..]);
    bank[0x10..0x14].copy_from_slice(&sum.to_le_bytes());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bank(size: usize, generation: u32) -> Vec<u8> {
        let mut b = vec![0u8; size];
        b[0] = 0x5a;
        b[4..9].copy_from_slice(b"nvram");
        b[0x14..0x18].copy_from_slice(&generation.to_le_bytes());
        b[0x40] = 7;
        let sum = adler32(&b[0x14..]);
        b[0x10..0x14].copy_from_slice(&sum.to_le_bytes());
        b
    }

    #[test]
    fn newest_valid_bank() {
        let mut region = bank(0x100, 5);
        region.extend(bank(0x100, 9));
        assert_eq!(current(&region, 2).unwrap().index, 1);
        // A corrupt newer bank loses to a valid older one.
        region[0x100 + 0x40] ^= 1;
        let b = current(&region, 2).unwrap();
        assert_eq!((b.index, b.generation), (0, 5));
        region[0x40] ^= 1;
        assert!(current(&region, 2).is_none());
    }

    /// A bank as the aux drive holds one: the header partition, `common`,
    /// then `system`, each body given.
    fn partitioned(common: &[u8], system: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        let mut at = 0;
        for (sig, units, name, body) in [
            (0x5a, 2, &b"nvram"[..], &b""[..]),
            (0x71, 0x10, b"common", common),
            (0x70, 0x0e, b"system", system),
        ] {
            b[at] = sig;
            b[at + 2..at + 4].copy_from_slice(&(units as u16).to_le_bytes());
            b[at + 4..at + 4 + name.len()].copy_from_slice(name);
            if sig != 0x5a {
                b[at + 16..at + 16 + body.len()].copy_from_slice(body);
            }
            at += units * 16;
        }
        let sum = adler32(&b[0x14..]);
        b[0x10..0x14].copy_from_slice(&sum.to_le_bytes());
        b
    }

    const NAME: &[u8] = b"com.apple.System.rtc-offset";

    #[test]
    fn adds_a_variable_once() {
        let mut b = partitioned(b"a=1\0b=2\0", b"rtc=0x5\0");
        assert!(add_if_absent(&mut b, NAME, b"0x1"));
        assert_eq!(
            &b[0x30..0x30 + 41],
            b"a=1\0b=2\0com.apple.System.rtc-offset=0x1\0\0"
        );
        // The checksum covers the change, so the bank stays current.
        assert!(current(&b, 1).is_some());
        assert!(!add_if_absent(&mut b, NAME, b"0x2"));
        // `system` is untouched.
        assert_eq!(&b[0x130..0x139], b"rtc=0x5\0\0");
    }

    #[test]
    fn keeps_the_guests_own_value() {
        let mut b = partitioned(b"com.apple.System.rtc-offset=0x948f\0", b"");
        let before = b.clone();
        assert!(!add_if_absent(&mut b, NAME, b"0x1"));
        assert_eq!(b, before);
        // A longer name is another variable.
        let mut b = partitioned(b"com.apple.System.rtc-offset2=0x7\0", b"");
        assert!(add_if_absent(&mut b, NAME, b"0x1"));
    }

    #[test]
    fn needs_room_and_a_common_partition() {
        let full = [b'x'; 0xf0 - 4];
        let mut b = partitioned(&[&full[..], b"=1\0"].concat(), b"");
        assert!(!add_if_absent(&mut b, NAME, b"0x1"));
        let mut b = bank(0x100, 1);
        assert!(!add_if_absent(&mut b, NAME, b"0x1"));
    }

    #[test]
    fn adler() {
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }
}
