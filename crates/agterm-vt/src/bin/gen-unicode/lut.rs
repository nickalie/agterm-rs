//! The generator of the three-level lookup tables the library reads (`unicode::lut`), after
//! <https://here-be-braces.com/fast-lookup-of-unicode-properties/>.
//!
//! Ghostty: `src/unicode/lut.zig`'s `Generator` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-unicode/src/lut.rs`'s `Generator` at `RUSTTY_REV` (MIT).
//! Changes: the context is a closure, and equality is `PartialEq` (`Properties.eql` compares every field, as the
//! derived equality does). Blocks are deduplicated by a `HashMap` where Ghostty hashes them with Wyhash: only the
//! first index each block is stored at matters, and that does not depend on the hash. `writeZig` has no counterpart:
//! the tables are written as Rust (`write`), and [`Tables::get`] reads them back for the generator's own check.

use std::collections::HashMap;

/// The domain the tables cover: every `u21`, `0..=0x1F_FFFF`.
pub(crate) const MAX_CODE_POINT: u32 = 0x1F_FFFF;

const BLOCK_SIZE: usize = 256;

/// The three stages of a generated table.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Tables<Elem> {
    pub(crate) stage1: Vec<u16>,
    pub(crate) stage2: Vec<u16>,
    pub(crate) stage3: Vec<Elem>,
}

/// Why a table could not be generated (`error.BlockTooLarge`, `error.Stage2TooLarge`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GenerateError {
    BlockTooLarge,
    Stage2TooLarge,
}

impl std::fmt::Display for GenerateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BlockTooLarge => "more distinct values than a stage2 entry can index",
            Self::Stage2TooLarge => "more stage2 entries than a stage1 entry can index",
        })
    }
}

/// Generates the tables mapping every code point of the `u21` domain to `get(cp)`.
pub(crate) fn generate<Elem: Copy + PartialEq>(get: impl Fn(u32) -> Elem) -> Result<Tables<Elem>, GenerateError> {
    let mut blocks: HashMap<[u16; BLOCK_SIZE], u16> = HashMap::new();
    let mut tables = Tables {
        stage1: Vec::new(),
        stage2: Vec::new(),
        stage3: Vec::new(),
    };
    let mut block = [0_u16; BLOCK_SIZE];
    let mut block_len = 0;
    for cp in 0..=MAX_CODE_POINT {
        // each distinct mapping gets one stage3 entry, in the order first seen
        let elem = get(cp);
        let index = if let Some(index) = tables.stage3.iter().position(|item| *item == elem) {
            index
        } else {
            tables.stage3.push(elem);
            tables.stage3.len() - 1
        };
        block[block_len] = u16::try_from(index).map_err(|_| GenerateError::BlockTooLarge)?;
        block_len += 1;

        // a block is finalized when it is full or the code points run out
        if block_len < BLOCK_SIZE && cp != MAX_CODE_POINT {
            continue;
        }
        block[block_len..].fill(0);
        let start = if let Some(&start) = blocks.get(&block) {
            start
        } else {
            let start = u16::try_from(tables.stage2.len()).map_err(|_| GenerateError::Stage2TooLarge)?;
            blocks.insert(block, start);
            tables.stage2.extend_from_slice(&block[..block_len]);
            start
        };
        tables.stage1.push(start);
        block_len = 0;
    }
    Ok(tables)
}

impl<Elem: Copy> Tables<Elem> {
    /// The mapping of a code point of the `u21` domain, read as the library reads it (`TablesRef::get`). `None` for a
    /// code point the stages do not cover.
    pub(crate) fn get(&self, cp: u32) -> Option<Elem> {
        let high = usize::try_from(cp >> 8).ok()?;
        let low = usize::try_from(cp & 0xFF).ok()?;
        let block = usize::from(*self.stage1.get(high)?);
        let index = usize::from(*self.stage2.get(block + low)?);
        self.stage3.get(index).copied()
    }
}

#[cfg(test)]
mod tests;
