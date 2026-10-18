//! The generated Unicode tables the lookups read: Ghostty's build-time generator output, checked in.
//!
//! Ghostty generates them at build time (`props-unigen`, `symbols-unigen`, the comptime grapheme precompute, uucode's
//! own tables); here they are source files, written by a generator and never edited (the zig-free-core plan's Task 6
//! ports the generator into this crate). Each file names its inputs. The checksums below pin every value, so a
//! changed entry fails the tests on every platform; a regeneration updates them in the same change.
//!
//! rustty: `crates/rustty-unicode/src/generated.rs` and `generated/checksum.rs` at `RUSTTY_REV` (MIT).
//! Changes: the checksums also cover the `Properties` stage and the case folding table, which rustty left out.

pub(crate) mod case_folding_data;
pub(crate) mod grapheme_data;
pub(crate) mod props_data;
pub(crate) mod symbols_data;

#[cfg(test)]
mod tests {
    use super::{case_folding_data, grapheme_data, props_data, symbols_data};
    use crate::unicode::props::Properties;

    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    /// FNV-1a over `bytes`, continuing from `hash`.
    fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fn fnv_u16(seed: u64, values: &[u16]) -> u64 {
        values.iter().fold(seed, |h, v| fnv1a(h, &v.to_le_bytes()))
    }

    fn fnv_u32(seed: u64, values: &[u32]) -> u64 {
        values.iter().fold(seed, |h, v| fnv1a(h, &v.to_le_bytes()))
    }

    /// One `Properties` as the four bytes of its fields, in declaration order.
    fn properties_bytes(p: Properties) -> [u8; 4] {
        [
            p.width,
            u8::from(p.width_zero_in_grapheme),
            p.grapheme_break as u8,
            u8::from(p.emoji_vs_base),
        ]
    }

    // rustty's three checksums, pinned when its tables matched Ghostty's generator output (its differential gate
    // against uucode): equal values here prove the copy is rustty's tables, entry for entry.

    #[test]
    fn grapheme_data_checksum() {
        assert_eq!(
            fnv1a(FNV_OFFSET, &grapheme_data::DATA),
            0x3c5c_3746_da10_ce7d,
            "a grapheme table entry changed; only a regeneration updates this"
        );
    }

    #[test]
    fn symbols_data_checksum() {
        let mut h = fnv_u16(FNV_OFFSET, &symbols_data::STAGE1);
        h = fnv_u16(h, &symbols_data::STAGE2);
        for &b in &symbols_data::STAGE3 {
            h = fnv1a(h, &[u8::from(b)]);
        }
        assert_eq!(
            h, 0x130b_1f00_7d79_a82c,
            "a symbols table entry changed; only a regeneration updates this"
        );
    }

    #[test]
    fn props_data_checksum() {
        let mut h = fnv_u16(FNV_OFFSET, &props_data::STAGE1);
        h = fnv_u16(h, &props_data::STAGE2);
        assert_eq!(
            h, 0x0f41_cf4e_8302_848f,
            "a props table entry changed; only a regeneration updates this"
        );
    }

    // agterm's additions, pinned from the same copy once the three above held.

    #[test]
    fn props_stage3_checksum() {
        let h = props_data::STAGE3
            .iter()
            .fold(FNV_OFFSET, |h, p| fnv1a(h, &properties_bytes(*p)));
        assert_eq!(
            h, PROPS_STAGE3_CHECKSUM,
            "a Properties entry changed; only a regeneration updates this"
        );
    }

    #[test]
    fn case_folding_checksum() {
        let h = case_folding_data::CASE_FOLDING_FULL
            .iter()
            .fold(FNV_OFFSET, |h, (cp, mapping)| {
                fnv_u32(fnv1a(h, &cp.to_le_bytes()), mapping)
            });
        assert_eq!(
            h, CASE_FOLDING_CHECKSUM,
            "a case folding entry changed; only a regeneration updates this"
        );
    }

    const PROPS_STAGE3_CHECKSUM: u64 = 0xe427_19b2_7396_b866;
    const CASE_FOLDING_CHECKSUM: u64 = 0xe7ff_c069_8341_9f5d;
}
