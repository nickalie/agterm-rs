//! Kitty graphics Unicode placeholders: the cells (U+10EEEE plus row, column and image-id diacritics) through which a
//! virtual placement (`U=1`) is shown, and the runs of them a renderer draws.
//!
//! Ported from Ghostty (<https://github.com/ghostty-org/ghostty>, MIT, Copyright Mitchell Hashimoto and Ghostty
//! contributors) at `LIBGHOSTTY_VT_REV`: `src/terminal/kitty/graphics_unicode.zig` (`IncompletePlacement`,
//! `PlacementIterator`, the diacritics table, which is Kitty's `rowcolumn-diacritics.txt`). libghostty-vt's C API
//! gives a virtual placement no position (its cells are the position), so the frame carries what each placeholder
//! cell says ([`KittyPlaceholder`], decoded with the row while its style's raw colors are at hand: an id is a palette
//! index or a 24-bit color, which the frame's resolved colors lose), and [`virtual_placements`] joins them into runs
//! as Ghostty's iterator does.

use std::sync::Arc;

use crate::render::Row;

/// The placeholder character.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// One placeholder cell, as written: Ghostty's `IncompletePlacement`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KittyPlaceholder {
    pub col: u16,
    /// The image id's low 24 bits, from the foreground color (a palette index or a 24-bit color; none is 0).
    pub image_id_low: u32,
    /// The image id's high 8 bits, from the third diacritic.
    pub image_id_high: Option<u8>,
    /// From the underline color; `None` when it is unset or 0.
    pub placement_id: Option<u32>,
    /// The image row and column this cell shows, from the first and second diacritics.
    pub row: Option<u32>,
    pub column: Option<u32>,
}

/// A run of placeholder cells on one viewport row showing consecutive cells of one image row: Ghostty's `Placement`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VirtualPlacement {
    /// The run's first cell on the viewport.
    pub viewport_row: u16,
    pub viewport_col: u16,
    pub image_id: u32,
    /// 0: the image's preferred virtual placement.
    pub placement_id: u32,
    /// The first image cell the run shows (0-based).
    pub row: u32,
    pub col: u32,
    /// The run's size in cells (one row).
    pub width: u32,
    pub height: u32,
}

/// A raw style color as Ghostty's `colorToId` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdColor {
    None,
    Palette(u8),
    Rgb(u8, u8, u8),
}

impl IdColor {
    fn id(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Palette(index) => u32::from(index),
            Self::Rgb(r, g, b) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        }
    }
}

/// The placeholder a cell's grapheme `text` and raw colors make, or `None` when the cell is not one. Diacritics that
/// are not in the table count as absent (Kitty's behavior), past the third they are ignored.
pub(crate) fn decode(col: u16, text: &str, foreground: IdColor, underline: IdColor) -> Option<KittyPlaceholder> {
    let mut chars = text.chars();
    if chars.next() != Some(PLACEHOLDER) {
        return None;
    }
    let mut marks = chars.map(index);
    let row = marks.next().flatten();
    let column = marks.next().flatten();
    let image_id_high = marks.next().flatten().and_then(|high| u8::try_from(high).ok());
    let placement = underline.id();
    Some(KittyPlaceholder {
        col,
        image_id_low: foreground.id(),
        image_id_high,
        placement_id: (placement != 0).then_some(placement),
        row,
        column,
    })
}

/// The row/column value of a diacritic.
fn index(mark: char) -> Option<u32> {
    DIACRITICS
        .binary_search(&u32::from(mark))
        .ok()
        .and_then(|index| u32::try_from(index).ok())
}

/// A run being built (Ghostty's `IncompletePlacement` with its width).
struct Run {
    first: KittyPlaceholder,
    row: u32,
    col: u32,
    width: u32,
}

impl Run {
    fn start(first: KittyPlaceholder) -> Self {
        Self {
            first,
            row: first.row.unwrap_or(0),
            col: first.column.unwrap_or(0),
            width: 1,
        }
    }

    /// Kitty's continuation rule: the same ids, and the row and column (when given) continue this run.
    fn can_append(&self, next: &KittyPlaceholder) -> bool {
        self.first.image_id_low == next.image_id_low
            && self.first.placement_id == next.placement_id
            && next.row.is_none_or(|row| row == self.row)
            && next.column.is_none_or(|col| col == self.col + self.width)
            && next
                .image_id_high
                .is_none_or(|high| Some(high) == self.first.image_id_high)
    }

    fn complete(&self, viewport_row: u16) -> VirtualPlacement {
        VirtualPlacement {
            viewport_row,
            viewport_col: self.first.col,
            image_id: self.first.image_id_low | (u32::from(self.first.image_id_high.unwrap_or(0)) << 24),
            placement_id: self.first.placement_id.unwrap_or(0),
            row: self.row,
            col: self.col,
            width: self.width,
            height: 1,
        }
    }
}

/// The virtual placements the placeholder cells of `rows` (a viewport, top to bottom) show: Ghostty's
/// `PlacementIterator`. A run ends at a cell that is not a placeholder or one that does not continue it.
#[must_use]
pub fn virtual_placements(rows: &[Arc<Row>]) -> Vec<VirtualPlacement> {
    let mut placements = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let Ok(viewport_row) = u16::try_from(index) else {
            break;
        };
        let mut run: Option<Run> = None;
        let mut next_col = None;
        for placeholder in row.placeholders() {
            // a cell between two placeholders ends the run
            if next_col != Some(placeholder.col)
                && let Some(done) = run.take()
            {
                placements.push(done.complete(viewport_row));
            }
            next_col = placeholder.col.checked_add(1);
            match run.as_mut() {
                Some(current) if current.can_append(placeholder) => current.width += 1,
                Some(current) => {
                    placements.push(current.complete(viewport_row));
                    run = Some(Run::start(*placeholder));
                }
                None => run = Some(Run::start(*placeholder)),
            }
        }
        if let Some(done) = run {
            placements.push(done.complete(viewport_row));
        }
    }
    placements
}

/// The diacritics whose index is a row, column or high id byte, sorted (Kitty's `rowcolumn-diacritics.txt`).
#[rustfmt::skip]
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

#[cfg(test)]
mod tests {
    use super::*;

    const ROW_0: char = '\u{0305}';
    const ROW_1: char = '\u{030D}';
    const ROW_2: char = '\u{030E}';
    const ROW_3: char = '\u{0310}';

    fn cell(marks: &[char]) -> String {
        std::iter::once(PLACEHOLDER).chain(marks.iter().copied()).collect()
    }

    fn placeholder(col: u16, marks: &[char]) -> KittyPlaceholder {
        decode(col, &cell(marks), IdColor::None, IdColor::None).unwrap()
    }

    fn rows(cells: Vec<Vec<KittyPlaceholder>>) -> Vec<Arc<Row>> {
        cells
            .into_iter()
            .map(|placeholders| Arc::new(Row::with_placeholders(placeholders)))
            .collect()
    }

    #[test]
    fn the_table_is_kittys_sorted_list() {
        assert!(DIACRITICS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(index(ROW_0), Some(0));
        assert_eq!(index(ROW_3), Some(3));
        assert_eq!(index('\u{1D244}'), Some(296));
        assert_eq!(index('a'), None);
    }

    #[test]
    fn ids_come_from_the_colors_and_the_third_diacritic() {
        assert_eq!(decode(0, "x", IdColor::None, IdColor::None), None, "not a placeholder");
        let palette = decode(3, &cell(&[ROW_0, ROW_0]), IdColor::Palette(42), IdColor::Palette(21)).unwrap();
        assert_eq!(
            palette,
            KittyPlaceholder {
                col: 3,
                image_id_low: 42,
                image_id_high: None,
                placement_id: Some(21),
                row: Some(0),
                column: Some(0),
            }
        );
        let rgb = decode(0, &cell(&[ROW_1, ROW_2, ROW_2]), IdColor::Rgb(0, 0, 1), IdColor::None).unwrap();
        assert_eq!((rgb.image_id_low, rgb.placement_id), (1, None));
        assert_eq!((rgb.row, rgb.column, rgb.image_id_high), (Some(1), Some(2), Some(2)));
        // Ghostty's "specifying image id with high bits": 42 | 2 << 24
        let high = decode(0, &cell(&[ROW_0, ROW_0, ROW_2]), IdColor::Palette(42), IdColor::None).unwrap();
        let placed = virtual_placements(&rows(vec![vec![high]]));
        assert_eq!(placed[0].image_id, 33_554_474);
        // an unknown diacritic counts as absent
        let unknown = decode(0, &cell(&['a', ROW_1]), IdColor::Rgb(1, 2, 3), IdColor::None).unwrap();
        assert_eq!(
            (unknown.row, unknown.column, unknown.image_id_low),
            (None, Some(1), 0x01_0203)
        );
    }

    #[test]
    fn runs_continue_on_the_next_column_and_break_on_a_jump_or_a_gap() {
        let at = |cells: Vec<KittyPlaceholder>| -> Vec<(u16, u32, u32, u32)> {
            virtual_placements(&rows(vec![cells]))
                .iter()
                .map(|placement| (placement.viewport_col, placement.row, placement.col, placement.width))
                .collect()
        };
        // explicit columns 0, 1, 2
        assert_eq!(
            at(vec![
                placeholder(0, &[ROW_0, ROW_0]),
                placeholder(1, &[ROW_0, ROW_1]),
                placeholder(2, &[ROW_0, ROW_2]),
            ]),
            [(0, 0, 0, 3)]
        );
        // no column, then no diacritic at all: continuations
        assert_eq!(
            at(vec![
                placeholder(0, &[ROW_0, ROW_0]),
                placeholder(1, &[ROW_0]),
                placeholder(2, &[])
            ]),
            [(0, 0, 0, 3)]
        );
        // a jump from column 0 to column 2 starts a run
        assert_eq!(
            at(vec![placeholder(0, &[ROW_0, ROW_0]), placeholder(1, &[ROW_0, ROW_2])]),
            [(0, 0, 0, 1), (1, 0, 2, 1)]
        );
        // a cell that is not a placeholder ends the run; one starting mid-row starts at its own column
        assert_eq!(
            at(vec![
                placeholder(3, &[ROW_0, ROW_0]),
                placeholder(4, &[ROW_0, ROW_1]),
                placeholder(6, &[])
            ]),
            [(3, 0, 0, 2), (6, 0, 0, 1)]
        );
        // another row of the image: a new run
        assert_eq!(
            at(vec![placeholder(0, &[ROW_0]), placeholder(1, &[ROW_1])]),
            [(0, 0, 0, 1), (1, 1, 0, 1)]
        );
    }

    #[test]
    fn every_viewport_row_is_scanned() {
        let placed = virtual_placements(&rows(vec![
            vec![placeholder(0, &[ROW_0, ROW_0]), placeholder(1, &[ROW_0, ROW_1])],
            Vec::new(),
            vec![placeholder(0, &[ROW_1, ROW_0]), placeholder(1, &[ROW_1, ROW_1])],
        ]));
        let rows: Vec<(u16, u32, u32)> = placed.iter().map(|p| (p.viewport_row, p.row, p.width)).collect();
        assert_eq!(rows, [(0, 0, 2), (2, 1, 2)]);
        assert!(placed.iter().all(|placement| placement.height == 1));
    }
}
