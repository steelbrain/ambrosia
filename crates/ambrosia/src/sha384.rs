//! SHA-384 (FIPS 180-4), which names boot objects: iBoot finds them by their
//! manifests' hashes, and the device tree carries those hashes. Ambrosia
//! also hashes its boot seed and PAC keys with it. It is SHA-512 with its own
//! initial values, cut to 48 bytes.

/// The initial hash value: the first 64 bits of the fractional parts of the
/// square roots of the ninth to sixteenth primes.
const IV: [u64; 8] = [
    0xcbbb9d5dc1059ed8,
    0x629a292a367cd507,
    0x9159015a3070dd17,
    0x152fecd8f70e5939,
    0x67332667ffc00b31,
    0x8eb44a8768581511,
    0xdb0c2e0d64f98fa7,
    0x47b5481dbefa4fa4,
];
/// The round constants: the first 64 bits of the fractional parts of the
/// cube roots of the first 80 primes.
const K: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

/// A SHA-384 hash being computed, fed in pieces with [`Sha384::update`].
#[derive(Clone)]
pub struct Sha384 {
    state: [u64; 8],
    block: [u8; 128],
    /// How many bytes of `block` are filled.
    filled: usize,
    /// The message's length so far, in bytes.
    length: u128,
}

impl Default for Sha384 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha384 {
    /// A hash of the empty message, to feed with [`update`](Self::update).
    pub fn new() -> Self {
        Self {
            state: IV,
            block: [0; 128],
            filled: 0,
            length: 0,
        }
    }

    /// Adds `data` to the message.
    pub fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u128;
        while !data.is_empty() {
            let n = data.len().min(128 - self.filled);
            self.block[self.filled..self.filled + n].copy_from_slice(&data[..n]);
            self.filled += n;
            data = &data[n..];
            if self.filled == 128 {
                compress(&mut self.state, &self.block);
                self.filled = 0;
            }
        }
    }

    /// The hash of the message: the padding (a 1 bit, zeros, and the
    /// message's length in bits as 128 bits), then the first six words of the
    /// state, big-endian.
    pub fn finalize(mut self) -> [u8; 48] {
        let bits = self.length * 8;
        self.block[self.filled] = 0x80;
        self.block[self.filled + 1..].fill(0);
        if self.filled >= 112 {
            compress(&mut self.state, &self.block);
            self.block.fill(0);
        }
        self.block[112..].copy_from_slice(&bits.to_be_bytes());
        compress(&mut self.state, &self.block);
        let mut out = [0; 48];
        for (o, w) in out.as_chunks_mut::<8>().0.iter_mut().zip(self.state) {
            *o = w.to_be_bytes();
        }
        out
    }
}

/// The SHA-384 hash of `data`.
pub fn sha384(data: &[u8]) -> [u8; 48] {
    let mut h = Sha384::new();
    h.update(data);
    h.finalize()
}

/// Runs the compression function over one 128-byte block.
fn compress(state: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 80];
    for (w, b) in w.iter_mut().zip(block.as_chunks::<8>().0) {
        *w = u64::from_be_bytes(*b);
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for (k, w) in K.iter().zip(w) {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*k)
            .wrapping_add(w);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;
    use alloc::{format, vec};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn nist_vectors() {
        assert_eq!(
            hex(&sha384(b"")),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"
        );
        assert_eq!(
            hex(&sha384(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            hex(&sha384(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "3391fdddfc8dc7393707a65b1b4709397cf8b1d162af05abfe8f450de5f36bc6b0455a8520bc4e6f5fe95b1fe3c8452b"
        );
        assert_eq!(
            hex(&sha384(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "09330c33f71147e83d192fc782cd1b4753111b173b3b05d22fa08086e3b0f712fcc7c71a557e2db966c3e9fa91746039"
        );
        assert_eq!(
            hex(&sha384(&vec![b'a'; 1_000_000])),
            "9d0e1809716474cb086e834e310a4a1ced149e9c00f248527972cec5704c2a5b07b8b3dc38ecc4ebae97ddd87f3d8985"
        );
    }

    /// Lengths either side of where the padding needs a second block.
    #[test]
    fn padding_boundaries() {
        for (n, want) in [
            (
                111,
                "435770712c611be7293a66dd0dc8d1450dc7ff7337bfe115bf058ef2eb9bed09cee85c26963a5bcc0905dc2df7cc6a76",
            ),
            (
                112,
                "3e0cbf3aee0e3aa70415beae1bd12dd7db821efa446440f12132edffce76f635e53526a111491e75ee8e27b9700eec20",
            ),
            (
                127,
                "485709a91f9846d3eb9824335106532fde8bfd5d770408e0899d99db5bf85b79ba04630d1e6d13f9f336915daf7c1d1d",
            ),
            (
                128,
                "f809b88323411f24a6f152e5e9d9d1b5466b77e0f3c7550f8b242c31b6e7b99bcb45bdecb6124bc23283db3b9fc4f5b3",
            ),
            (
                129,
                "3d27644477d56bfcf9142bee2cdeebb13dd5fac6b1cf3e0418a9ecaeae92d32e041c9596a0d25c3812546c78281e8964",
            ),
            (
                255,
                "059fb9a5bb7d90988188e1a3d9034d6ae4449d97b056c536c1e5b41259ff4f46a9ee5d16ae14ea815dc1749754114f52",
            ),
            (
                256,
                "983980373213482dd5c9a5a424db89418e3344c459fa31a356e42eaa28544ca01b9839f6593c9e5d79fd439b5da6ebef",
            ),
        ] {
            assert_eq!(hex(&sha384(&vec![0; n])), want, "{n} zero bytes");
        }
    }

    /// Feeding a message in pieces gives the hash of the whole.
    #[test]
    fn pieces() {
        let data: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        for split in [0, 1, 64, 127, 128, 129, 200, 300] {
            let mut h = Sha384::new();
            h.update(&data[..split]);
            h.update(&data[split..]);
            assert_eq!(h.finalize(), sha384(&data), "split at {split}");
        }
        for size in [1, 3, 100, 127, 129] {
            let mut h = Sha384::new();
            for piece in data.chunks(size) {
                h.update(piece);
            }
            assert_eq!(h.finalize(), sha384(&data), "pieces of {size}");
        }
    }
}
