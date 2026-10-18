//! The three-level lookup table from a code point to its mapping, after
//! <https://here-be-braces.com/fast-lookup-of-unicode-properties/>.
//!
//! The code point's high bits (`cp >> 8`) index `stage1`, which gives the start of its 256-entry block in `stage2`;
//! the low byte indexes the block, which gives the index of the mapping in `stage3`. Identical blocks are stored
//! once, so the tables stay small.
//!
//! Ghostty: `src/unicode/lut.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-unicode/src/lut.rs` at `RUSTTY_REV` (MIT).
//! Changes: only the lookup (`Tables.get`) is here; the `Generator` is the table generator's
//! (`src/bin/gen-unicode/lut.rs`), and `writeZig` has no counterpart: the generator writes the tables as Rust. The
//! stages are fixed-size arrays, so the `stage1` index needs no check, and the other two are indexed safely: rustty's
//! unchecked reads, which relied on the generator's invariant, are checked reads here, and a test holds the invariant
//! for every generated table. A code point is a `u32` masked to Zig's `u21`.

/// A three-level lookup table over `'static` generated stages, from a code point to an `Elem`. The stage lengths are
/// in the type, so a table cannot be paired with stages of another shape.
#[derive(Clone, Copy)]
pub(crate) struct TablesRef<Elem: 'static, const S1: usize, const S2: usize, const S3: usize> {
    pub(crate) stage1: &'static [u16; S1],
    pub(crate) stage2: &'static [u16; S2],
    pub(crate) stage3: &'static [Elem; S3],
}

/// The code point domain of the tables: Zig's `u21`. Every generated table covers it whole (`stage1` has
/// `0x20_0000 >> 8` entries).
const CODE_POINT_MASK: u32 = 0x1F_FFFF;

impl<Elem: Copy, const S1: usize, const S2: usize, const S3: usize> TablesRef<Elem, S1, S2, S3> {
    /// The mapping for a code point. Ghostty's parameter is a `u21`; a wider value is masked to its low 21 bits,
    /// which changes nothing for any value Ghostty can pass.
    #[inline]
    pub(crate) fn get(&self, cp: u32) -> Elem {
        let cp = cp & CODE_POINT_MASK;
        let high = (cp >> 8) as usize;
        let low = (cp & 0xFF) as usize;
        let block = usize::from(self.stage1[high]);
        self.stage3[usize::from(self.stage2[block + low])]
    }
}

#[cfg(test)]
mod tests {
    use super::TablesRef;
    use crate::unicode::generated::{props_data, symbols_data};
    use crate::unicode::{props_table, symbols_table};

    /// The invariant [`TablesRef::get`] relies on, for every generated table: each `stage1` entry plus the largest
    /// low byte (0xFF) is inside `stage2`, and each `stage2` entry is inside `stage3`. A table that broke it would
    /// panic on a lookup instead of answering.
    #[test]
    fn generated_tables_uphold_the_lookup_invariant() {
        fn check<Elem>(stage1: &[u16], stage2: &[u16], stage3: &[Elem]) {
            assert_eq!(stage1.len(), 0x20_0000 >> 8, "stage1 covers the u21 domain");
            for &s1 in stage1 {
                assert!(usize::from(s1) + 0xFF < stage2.len());
            }
            for &s2 in stage2 {
                assert!(usize::from(s2) < stage3.len());
            }
        }
        check(&props_data::STAGE1, &props_data::STAGE2, &props_data::STAGE3);
        check(&symbols_data::STAGE1, &symbols_data::STAGE2, &symbols_data::STAGE3);
    }

    /// The lookup is Ghostty's plain three-stage indexing for every code point of the `u21` domain, and a value
    /// above it aliases its masked value.
    #[test]
    fn get_is_the_three_stage_indexing_over_the_u21_domain() {
        fn reference<Elem: Copy>(stage1: &[u16], stage2: &[u16], stage3: &[Elem], cp: u32) -> Elem {
            let high = usize::try_from(cp >> 8).unwrap();
            let low = usize::try_from(cp & 0xFF).unwrap();
            stage3[usize::from(stage2[usize::from(stage1[high]) + low])]
        }

        let props = props_table::table();
        let symbols = symbols_table::table();
        for cp in 0..=0x1F_FFFF_u32 {
            let expected = reference(&props_data::STAGE1, &props_data::STAGE2, &props_data::STAGE3, cp);
            assert_eq!(props.get(cp), expected, "props at {cp:#x}");
            let expected = reference(&symbols_data::STAGE1, &symbols_data::STAGE2, &symbols_data::STAGE3, cp);
            assert_eq!(symbols.get(cp), expected, "symbols at {cp:#x}");
        }
        for cp in [0_u32, 0x41, 0x10_FFFF, 0x1F_FFFF] {
            assert_eq!(props.get(cp | 0x20_0000), props.get(cp));
            assert_eq!(symbols.get(cp | 0x20_0000), symbols.get(cp));
            assert_eq!(props.get(cp | 0xFFE0_0000), props.get(cp));
        }
    }

    /// A table is its stages: a hand-built one answers from them.
    #[test]
    fn a_small_table_answers_from_its_stages() {
        static STAGE1: [u16; 2] = [0, 256];
        static STAGE2: [u16; 512] = {
            let mut s = [0; 512];
            s[0x41] = 1;
            s[256] = 2;
            s
        };
        static STAGE3: [u8; 3] = [10, 20, 30];
        let table = TablesRef {
            stage1: &STAGE1,
            stage2: &STAGE2,
            stage3: &STAGE3,
        };
        assert_eq!(table.get(0x00), 10);
        assert_eq!(table.get(0x41), 20);
        assert_eq!(table.get(0x100), 30);
        assert_eq!(table.get(0x1FF), 10);
    }
}
