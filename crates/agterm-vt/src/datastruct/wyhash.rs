//! Wyhash, bit for bit as Zig's standard library computes it.
//!
//! Zig: `lib/std/hash/wyhash.zig` (Zig 0.15), which Ghostty's hash maps and hyperlink hashing use.
//! rustty: `crates/rustty-zigstd/src/wyhash.rs` at `RUSTTY_REV` (MIT).
//! Changes: lint-clean names and checked conversions; the algorithm and its test vectors are unchanged. The digest is
//! not an implementation detail: the page's hash maps live in page memory, so a key's hash decides its slot,
//! fingerprint and probe order, and keeping Zig's hash keeps Ghostty's page layout.

const SECRET: [u64; 4] = [
    0xa076_1d64_78bd_642f,
    0xe703_7ed1_a0b4_28db,
    0x8ebc_6af0_9c88_c6e3,
    0x5899_65cc_7537_4cc3,
];

/// The iterative Wyhash state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wyhash {
    a: u64,
    b: u64,
    state: [u64; 3],
    total_len: usize,
    buf: [u8; 48],
    buf_len: usize,
}

impl Wyhash {
    /// A state seeded with `seed`.
    pub(crate) fn init(seed: u64) -> Self {
        let state = seed ^ mix(seed ^ SECRET[0], SECRET[1]);
        Self {
            a: 0,
            b: 0,
            state: [state; 3],
            total_len: 0,
            buf: [0; 48],
            buf_len: 0,
        }
    }

    /// Feeds `input`; any split of the input into updates gives the same digest.
    pub(crate) fn update(&mut self, input: &[u8]) {
        self.total_len += input.len();
        if input.len() <= 48 - self.buf_len {
            self.buf[self.buf_len..self.buf_len + input.len()].copy_from_slice(input);
            self.buf_len += input.len();
            return;
        }

        let mut i = 0;
        if self.buf_len > 0 {
            i = 48 - self.buf_len;
            self.buf[self.buf_len..].copy_from_slice(&input[..i]);
            let block = self.buf;
            self.round(&block);
            self.buf_len = 0;
        }
        while i + 48 < input.len() {
            self.round(block_at(input, i));
            i += 48;
        }

        let remaining = &input[i..];
        if remaining.len() < 16 && i >= 48 {
            let rem = 16 - remaining.len();
            self.buf[48 - rem..].copy_from_slice(&input[i - rem..i]);
        }
        self.buf[..remaining.len()].copy_from_slice(remaining);
        self.buf_len = remaining.len();
    }

    /// The digest of everything fed so far; the state is unchanged, so more can be fed after.
    pub(crate) fn finalize(&self) -> u64 {
        let mut input: &[u8] = &self.buf[..self.buf_len];
        let mut copy = self.shallow_copy();
        let mut scratch = [0u8; 16];
        if self.total_len <= 16 {
            copy.small_key(input);
        } else {
            let mut offset = 0;
            if self.buf_len < 16 {
                let rem = 16 - self.buf_len;
                scratch[..rem].copy_from_slice(&self.buf[48 - rem..]);
                scratch[rem..rem + self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
                input = &scratch;
                offset = rem;
            }
            copy.final0();
            copy.final1(input, offset);
        }
        copy.final2()
    }

    fn shallow_copy(&self) -> Self {
        Self {
            buf: [0; 48],
            buf_len: 0,
            ..*self
        }
    }

    fn small_key(&mut self, input: &[u8]) {
        debug_assert!(input.len() <= 16);
        if input.len() >= 4 {
            let end = input.len() - 4;
            let quarter = (input.len() >> 3) << 2;
            self.a = (read(4, input) << 32) | read(4, &input[quarter..]);
            self.b = (read(4, &input[end..]) << 32) | read(4, &input[end - quarter..]);
        } else if !input.is_empty() {
            self.a = (u64::from(input[0]) << 16)
                | (u64::from(input[input.len() >> 1]) << 8)
                | u64::from(input[input.len() - 1]);
            self.b = 0;
        } else {
            self.a = 0;
            self.b = 0;
        }
    }

    fn round(&mut self, input: &[u8; 48]) {
        for i in 0..3 {
            let a = read(8, &input[8 * (2 * i)..]);
            let b = read(8, &input[8 * (2 * i + 1)..]);
            self.state[i] = mix(a ^ SECRET[i + 1], b ^ self.state[i]);
        }
    }

    fn final0(&mut self) {
        self.state[0] ^= self.state[1] ^ self.state[2];
    }

    /// `input_lb` must be at least 16 bytes for the final read; the bytes before `start_pos` are lookback.
    fn final1(&mut self, input_lb: &[u8], start_pos: usize) {
        debug_assert!(input_lb.len() >= 16);
        debug_assert!(input_lb.len() - start_pos <= 48);
        let input = &input_lb[start_pos..];
        let mut i = 0;
        while i + 16 < input.len() {
            self.state[0] = mix(
                read(8, &input[i..]) ^ SECRET[1],
                read(8, &input[i + 8..]) ^ self.state[0],
            );
            i += 16;
        }
        self.a = read(8, &input_lb[input_lb.len() - 16..]);
        self.b = read(8, &input_lb[input_lb.len() - 8..]);
    }

    fn final2(&mut self) -> u64 {
        self.a ^= SECRET[1];
        self.b ^= self.state[0];
        mum(&mut self.a, &mut self.b);
        mix(self.a ^ SECRET[0] ^ self.total_len as u64, self.b ^ SECRET[1])
    }

    /// The one-shot digest of `input` with `seed`.
    pub(crate) fn hash(seed: u64, input: &[u8]) -> u64 {
        let mut state = Self::init(seed);
        if input.len() <= 16 {
            state.small_key(input);
        } else {
            let mut i = 0;
            if input.len() >= 48 {
                while i + 48 < input.len() {
                    state.round(block_at(input, i));
                    i += 48;
                }
                state.final0();
            }
            state.final1(input, i);
        }
        state.total_len = input.len();
        state.final2()
    }
}

/// The 48-byte block of `input` at `i`, which the caller keeps in bounds.
fn block_at(input: &[u8], i: usize) -> &[u8; 48] {
    let block: &[u8] = &input[i..i + 48];
    block.try_into().unwrap_or(&[0; 48])
}

/// The little-endian integer of the first `bytes` (at most 8) bytes of `data`.
fn read(bytes: usize, data: &[u8]) -> u64 {
    debug_assert!(bytes <= 8);
    let mut buf = [0u8; 8];
    buf[..bytes].copy_from_slice(&data[..bytes]);
    u64::from_le_bytes(buf)
}

/// The 128-bit product of `a` and `b`, low half into `a`, high half into `b`.
#[expect(clippy::cast_possible_truncation, reason = "the halves of a 128-bit product")]
fn mum(a: &mut u64, b: &mut u64) {
    let x = u128::from(*a).wrapping_mul(u128::from(*b));
    *a = x as u64;
    *b = (x >> 64) as u64;
}

fn mix(a: u64, b: u64) -> u64 {
    let (mut a, mut b) = (a, b);
    mum(&mut a, &mut b);
    a ^ b
}

/// Feeds Rust's `Hash` into Wyhash; for integers this is their native-endian bytes, Zig's `std.mem.asBytes`.
impl core::hash::Hasher for Wyhash {
    fn write(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }

    fn finish(&self) -> u64 {
        self.finalize()
    }
}

#[cfg(test)]
mod tests {
    use core::hash::{Hash, Hasher};

    use super::Wyhash;

    #[test]
    fn test_vectors() {
        let vectors: [(u64, u64, &[u8]); 7] = [
            (0x0409_638e_e2bd_e459, 0, b""),
            (0xa841_2d09_1b5f_e0a9, 1, b"a"),
            (0x32dd_92e4_b291_5153, 2, b"abc"),
            (0x8619_1240_89a3_a16b, 3, b"message digest"),
            (0x7a43_afb6_1d7f_5f40, 4, b"abcdefghijklmnopqrstuvwxyz"),
            (
                0xff42_329b_90e5_0d58,
                5,
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            ),
            (
                0xc39c_ab13_b115_aad3,
                6,
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            ),
        ];
        for (expected, seed, input) in vectors {
            assert_eq!(Wyhash::hash(seed, input), expected);
        }
    }

    #[test]
    fn smhasher() {
        let mut buf = [0u8; 256];
        let mut all = [0u8; 256 * 8];
        for i in 0..256usize {
            buf[i] = u8::try_from(i).unwrap();
            let h = Wyhash::hash(u64::try_from(256 - i).unwrap(), &buf[..i]);
            all[i * 8..i * 8 + 8].copy_from_slice(&h.to_le_bytes());
        }
        assert_eq!(Wyhash::hash(0, &all) & 0xFFFF_FFFF, 0xBD5E_840C);
    }

    #[test]
    fn iterative_api() {
        let buf = [0u8; 528];
        let mut len = 0;
        let mut hasher = Wyhash::init(0);
        for i in 1..32 {
            let direct = Wyhash::hash(0, &buf[..len + i]);
            hasher.update(&buf[len..len + i]);
            let f1 = hasher.finalize();
            let f2 = hasher.finalize();
            assert_eq!(f1, f2, "finalize is idempotent");
            assert_eq!(f1, direct, "iterative matches direct");
            len += i;
        }
    }

    /// `SplitMix64`, the draws of the differential test.
    fn splitmix(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    #[test]
    fn differential_against_zig() {
        // (k, seed, digest) for the `k`th input, from Zig's `std.hash.Wyhash` over the same SplitMix64 inputs
        let rows: [(usize, u64, u64); 25] = [
            (0, 0, 0x0409_638e_e2bd_e459),
            (1, 0, 0x069a_5c31_5a3d_f180),
            (3, 0, 0xbd38_a3f4_2b32_aecd),
            (4, 0, 0x078a_be22_5607_e127),
            (8, 0, 0xca6b_af7b_a8c1_c2c6),
            (15, 0, 0x3a27_cdba_9081_4fdd),
            (16, 0, 0x6152_564e_0cd8_9363),
            (17, 0, 0x3e15_ab60_fbc1_3d38),
            (47, 0, 0x7e5b_64ec_926e_1fe2),
            (48, 0, 0xa320_60d8_0387_f7ce),
            (49, 0, 0x8773_0997_8877_aa51),
            (95, 0, 0x7ea5_bed2_f124_8abd),
            (96, 0, 0xa196_869a_a021_bbe9),
            (97, 0, 0x443b_14e4_34df_dffd),
            (144, 0, 0x4e25_db19_f4d4_9aae),
            (145, 0, 0x7689_a8d2_84b4_b4b5),
            (200, 0, 0x5aa8_5eba_a5ed_668a),
            (300, 0, 0x3e80_9dd3_7697_d664),
            (301, 0x1a7e_44f7_add4_1273, 0x2ec8_976a_1b40_d84a),
            (305, 0x7b23_e450_a17d_5d1b, 0xa897_9d7e_1abd_df65),
            (349, 0x2d63_c9e8_4c37_1461, 0xcb78_0feb_2d3c_bdeb),
            (350, 0x340d_ed0f_b6a9_b7be, 0x41b2_07c7_dde5_831c),
            (397, 0xb27e_d942_accf_5f44, 0x40f7_d00b_3399_bfd2),
            (450, 0x3997_3b12_5dce_b591, 0x3bf6_a15b_c5e4_f6cc),
            (601, 0x4241_4865_7410_51df, 0x94e6_aec8_03fe_ecd8),
        ];
        let mut rng = 0x5eed_0082;
        let mut checked = 0;
        for k in 0..602usize {
            let len = k % 301;
            let seed = if k < 301 { 0 } else { splitmix(&mut rng) };
            let buf: Vec<u8> = (0..len).map(|_| splitmix(&mut rng).to_le_bytes()[0]).collect();
            let one = Wyhash::hash(seed, &buf);
            let mut wh = Wyhash::init(seed);
            let mut i = 0;
            while i < len {
                let step = usize::try_from(1 + splitmix(&mut rng) % 64).unwrap();
                let end = len.min(i + step);
                wh.update(&buf[i..end]);
                i = end;
            }
            assert_eq!(wh.finalize(), one, "k={k}");
            if let Some(&(_, want_seed, want)) = rows.iter().find(|r| r.0 == k) {
                assert_eq!(seed, want_seed, "k={k}");
                assert_eq!(one, want, "k={k}");
                checked += 1;
            }
        }
        assert_eq!(checked, rows.len());
    }

    #[test]
    fn seed0_vectors() {
        let vectors: [(u64, &[u8]); 7] = [
            (0x0409_638e_e2bd_e459, b""),
            (0x28d2_0533_09d2_8531, b"a"),
            (0x02a4_f1d7_cb51_6c72, b"abc"),
            (0x41d0_32e1_df79_b67e, b"message digest"),
            (0x774f_a8c2_1ed6_acd2, b"abcdefghijklmnopqrstuvwxyz"),
            (
                0x0369_bcbe_3f0f_0c0d,
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
            ),
            (
                0x4866_2e17_cabf_ab37,
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
            ),
        ];
        for (expected, input) in vectors {
            assert_eq!(Wyhash::hash(0, input), expected);
        }
    }

    #[test]
    fn u32_as_bytes_vectors() {
        // `std.hash.Wyhash.hash(0, std.mem.asBytes(&key))`, the page maps' automatic hash for `u32` keys
        let vectors: [(u32, u64); 8] = [
            (0, 0x14b8_3016_dc46_0955),
            (1, 0xd145_1a1b_34d5_fb64),
            (2, 0xb52d_3031_bc13_56f4),
            (3, 0x3579_1793_81f3_5bc6),
            (4, 0x63b2_677c_5d80_f97a),
            (8, 0xe850_de3f_7e60_b2fa),
            (16, 0x052a_446e_fa88_3433),
            (24, 0xa226_4b2b_07f9_9c5a),
        ];
        for (key, expected) in vectors {
            assert_eq!(Wyhash::hash(0, &key.to_ne_bytes()), expected);
            let mut hasher = Wyhash::init(0);
            key.hash(&mut hasher);
            assert_eq!(hasher.finish(), expected);
        }
    }

    #[test]
    fn iterative_matches_one_shot() {
        let mut big = [0u8; 200];
        for (i, b) in big.iter_mut().enumerate() {
            *b = u8::try_from(i % 251).unwrap();
        }
        assert_eq!(Wyhash::hash(0, &big), 0xb964_93db_7846_fe8c);
        assert_eq!(Wyhash::hash(12345, &big), 0x9ef0_0d3d_0756_bdab);
        let mut wh = Wyhash::init(0);
        wh.update(&big[..37]);
        wh.update(&big[37..100]);
        wh.update(&big[100..]);
        assert_eq!(wh.finalize(), 0xb964_93db_7846_fe8c);
        assert_eq!(wh.finalize(), 0xb964_93db_7846_fe8c);

        let mut input = b"Z".repeat(48);
        input.extend_from_slice(b"01234567890abcdefg");
        for i in 0..17 {
            let payload = &input[..input.len() - i];
            let mut wh = Wyhash::init(0);
            wh.update(payload);
            assert_eq!(wh.finalize(), Wyhash::hash(0, payload));
        }
    }
}
