//! The generator's tables read back every code point's value, share identical blocks, and refuse what a `u16`
//! cannot index.

use super::{GenerateError, MAX_CODE_POINT, generate};

/// Ghostty's `test "unicode props: tables match uucode"`, on a mapping of its own: every code point of the `u21`
/// domain reads back the value it was generated from.
#[test]
fn every_code_point_reads_back_its_value() {
    let value = |cp: u32| match cp {
        0x41..=0x5A => 1_u8,
        0x4E00..=0x9FFF => 2,
        0x11_0000.. => 3,
        _ => 0,
    };
    let tables = generate(value).unwrap();
    assert_eq!(tables.stage1.len(), 0x20_0000 >> 8);
    for cp in 0..=MAX_CODE_POINT {
        assert_eq!(tables.get(cp), Some(value(cp)), "U+{cp:X}");
    }
}

/// Values get stage3 entries in the order first seen, and identical blocks one stage2 copy.
#[test]
fn values_and_blocks_are_stored_once() {
    let tables = generate(|cp| cp == 0x100 || cp == 0x1_0000).unwrap();
    assert_eq!(tables.stage3, [false, true]);
    // two distinct blocks: all false, and true at its first entry
    assert_eq!(tables.stage2.len(), 512);
    assert_eq!(tables.stage1[0], 0);
    assert_eq!(tables.stage1[1], 256);
    assert_eq!(tables.stage1[2], 0);
    assert_eq!(tables.stage1[0x100], 256);
    assert_eq!(tables.stage1.iter().filter(|&&s| s == 256).count(), 2);
    assert_eq!(&tables.stage2[256..259], &[1, 0, 0]);
}

/// More distinct blocks than a stage1 entry can point at.
#[test]
fn too_many_distinct_blocks_are_refused() {
    // block `high` holds the low bits of `high` in its first sixteen entries: 8192 distinct blocks
    let result = generate(|cp| ((cp >> 8) >> (cp & 0xF)) & 1 == 1);
    assert_eq!(result, Err(GenerateError::Stage2TooLarge));
}

/// 256 distinct blocks: the last one starts at an index a `u16` holds, but stage2 ends one past `u16::MAX`.
#[test]
fn a_stage2_longer_than_a_u16_counts_is_refused() {
    let result = generate(|cp| (cp >> 8) & 0xFF);
    assert_eq!(result, Err(GenerateError::Stage2TooLarge));
}

#[test]
fn a_code_point_past_the_stages_reads_as_none() {
    let tables = generate(|_| 7_u8).unwrap();
    assert_eq!(tables.get(MAX_CODE_POINT), Some(7));
    assert_eq!(tables.get(MAX_CODE_POINT + 1), None);
}
