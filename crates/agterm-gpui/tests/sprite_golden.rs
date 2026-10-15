//! The procedural sprites against Ghostty's own goldens (plan > Task 15).
//!
//! `tests/golden/` holds Ghostty's sprite test sheets (`src/font/sprite/testdata/` at `LIBGHOSTTY_VT_REV` of
//! build/pins.env, MIT, Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors; `tests/golden/LICENSE`): for each
//! block of 256 code points, a 16 x 16 grid of padded cells (a quarter of the cell on every side) at four cell sizes,
//! `U+<first>...U+<last>-<width>x<height>+<thickness>.png`, 8-bit coverage. Ghostty's `testDrawRanges` draws them with
//! `Metrics.calc` of a face `width` wide, `ascent` above and `descent` below the baseline, no line gap, and an
//! underline `thickness` thick (the box lines' thickness).
//!
//! Each family's cells are rebuilt from this build's masks and compared pixel by pixel at all four sizes (even and odd
//! cells, even and odd lines). Families drawn from rectangles (blocks, braille, octants, ...) must match exactly.
//! Families with paths may differ where tiny-skia's anti-aliasing differs from z2d's, within budgets measured at this
//! revision: the mean coverage difference over the family's pixels, and the share of pixels off by more than
//! [`STRAY`] (a figure in the wrong place rather than a softer edge).

use agterm_gpui::terminal::sprite::{self, Mask};
use agterm_gpui::terminal::{CellMetrics, FaceMetrics};

/// One of Ghostty's four golden sizes.
struct Size {
    width: u32,
    ascent: u32,
    descent: u32,
    thickness: u32,
}

const SIZES: [Size; 4] = [
    Size {
        width: 18,
        ascent: 30,
        descent: 6,
        thickness: 4,
    },
    Size {
        width: 12,
        ascent: 20,
        descent: 4,
        thickness: 3,
    },
    Size {
        width: 11,
        ascent: 19,
        descent: 2,
        thickness: 2,
    },
    Size {
        width: 9,
        ascent: 15,
        descent: 2,
        thickness: 1,
    },
];

impl Size {
    fn height(&self) -> u32 {
        self.ascent + self.descent
    }

    /// Ghostty's `Metrics.calc` for the face `testDrawRanges` describes, through this build's port of it.
    fn metrics(&self) -> CellMetrics {
        CellMetrics::calc(&FaceMetrics {
            cell_width: f64::from(self.width),
            ascent: f64::from(self.ascent),
            descent: -f64::from(self.descent),
            line_gap: 0.0,
            underline_thickness: Some(f64::from(self.thickness)),
            strikethrough_thickness: Some(f64::from(self.thickness)),
            ..FaceMetrics::default()
        })
    }

    /// A padded cell's size.
    fn stride(&self) -> (usize, usize) {
        let (width, height) = (self.width, self.height());
        (to_usize(width + 2 * (width / 4)), to_usize(height + 2 * (height / 4)))
    }

    fn label(&self) -> String {
        format!("{}x{}+{}", self.width, self.height(), self.thickness)
    }
}

fn to_usize(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A coverage difference past which a pixel is a stray: a figure out of place, not an edge anti-aliased differently.
const STRAY: u8 = 96;

/// A family of sprites, the code points it draws and how far it may stray from Ghostty's pixels.
struct Family {
    name: &'static str,
    ranges: &'static [(u32, u32)],
    /// The mean coverage difference allowed over the family's pixels, as a share of full coverage (0: exact).
    mean: f64,
    /// The share of the family's pixels allowed to be strays.
    strays: f64,
}

const fn exact(name: &'static str, ranges: &'static [(u32, u32)]) -> Family {
    Family {
        name,
        ranges,
        mean: 0.0,
        strays: 0.0,
    }
}

/// The budgets are about twice the worst measured at `LIBGHOSTTY_VT_REV` (the 9x17+1 cell, thickness 1, is the worst
/// for every family with paths: there a pixel of anti-aliasing is most of a line).
const FAMILIES: &[Family] = &[
    exact("box drawing lines", &[(0x2500, 0x256c), (0x2574, 0x257f)]),
    Family {
        name: "box drawing arcs, diagonals",
        ranges: &[(0x256d, 0x2573)],
        mean: 0.012,
        strays: 0.0,
    },
    exact("blocks", &[(0x2580, 0x259f)]),
    Family {
        name: "geometric shapes",
        ranges: &[(0x25e2, 0x25e5), (0x25f8, 0x25fa), (0x25ff, 0x25ff)],
        mean: 0.0025,
        strays: 0.0,
    },
    exact("braille", &[(0x2800, 0x28ff)]),
    Family {
        name: "powerline",
        ranges: &[(0xe0b0, 0xe0bf), (0xe0d2, 0xe0d2), (0xe0d4, 0xe0d4)],
        mean: 0.01,
        strays: 0.0,
    },
    Family {
        name: "branch",
        ranges: &[(0xf5d0, 0xf60d)],
        mean: 0.012,
        strays: 0.005,
    },
    Family {
        name: "legacy computing",
        ranges: &[(0x1fb00, 0x1fbaf), (0x1fbbd, 0x1fbbf), (0x1fbce, 0x1fbef)],
        mean: 0.004,
        strays: 0.001,
    },
    exact("octants", &[(0x1cd00, 0x1cde5)]),
    Family {
        name: "legacy computing supplement",
        ranges: &[
            (0x1cc1b, 0x1cc1e),
            (0x1cc21, 0x1cc3f),
            (0x1ce00, 0x1ce01),
            (0x1ce0b, 0x1ce0c),
            (0x1ce16, 0x1ce19),
            (0x1ce51, 0x1ceaf),
        ],
        mean: 0.003,
        strays: 0.0003,
    },
];

/// A decoded golden sheet: 8-bit coverage, row-major.
struct Sheet {
    width: usize,
    pixels: Vec<u8>,
}

fn sheet(base: u32, size: &Size) -> TestResult<Sheet> {
    let name = format!("U+{base:X}...U+{:X}-{}.png", base + 0xff, size.label());
    let path = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    let file = std::fs::File::open(&path).map_err(|err| format!("{path}: {err}"))?;
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info()?;
    let mut buffer = vec![0; reader.output_buffer_size().ok_or("a golden too large to decode")?];
    let info = reader.next_frame(&mut buffer)?;
    assert_eq!(info.color_type, png::ColorType::Grayscale, "{name}");
    assert_eq!(info.bit_depth, png::BitDepth::Eight, "{name}");
    let width = to_usize(info.width);
    buffer.truncate(width * to_usize(info.height));
    Ok(Sheet { width, pixels: buffer })
}

/// This build's padded cell for `cp`: the mask placed where its trim left it.
fn cell(cp: u32, size: &Size, metrics: &CellMetrics) -> Vec<u8> {
    let (stride_x, stride_y) = size.stride();
    let mut pixels = vec![0; stride_x * stride_y];
    let Some(mask) = sprite::rasterize(cp, metrics, 1) else {
        return pixels;
    };
    let origin = |padding: u32, offset: i32| i64::from(padding) + i64::from(offset);
    let (left, top) = (origin(size.width / 4, mask.left), origin(size.height() / 4, mask.top));
    for y in 0..mask.height {
        for x in 0..mask.width {
            let (px, py) = (left + i64::from(x), top + i64::from(y));
            let (Ok(px), Ok(py)) = (usize::try_from(px), usize::try_from(py)) else {
                continue;
            };
            if px < stride_x && py < stride_y {
                pixels[py * stride_x + px] = mask.at(x, y);
            }
        }
    }
    pixels
}

/// The golden's padded cell for `cp`.
fn golden_cell(sheet: &Sheet, cp: u32, size: &Size) -> Vec<u8> {
    let (stride_x, stride_y) = size.stride();
    let index = to_usize(cp & 0xff);
    let (x0, y0) = ((index % 16) * stride_x, (index / 16) * stride_y);
    (0..stride_y)
        .flat_map(|y| {
            let start = (y0 + y) * sheet.width + x0;
            sheet.pixels[start..start + stride_x].iter().copied()
        })
        .collect()
}

/// How far a family is from the goldens at one size.
#[derive(Debug, Default)]
struct Diff {
    pixels: usize,
    /// The sum of the coverage differences.
    total: u64,
    strays: usize,
    /// The code points with the largest differences, and theirs.
    worst: Vec<(u32, u64)>,
}

impl Diff {
    #[expect(clippy::cast_precision_loss, reason = "pixel counts and sums are far below 2^52")]
    fn mean(&self) -> f64 {
        self.total as f64 / 255.0 / self.pixels as f64
    }

    #[expect(clippy::cast_precision_loss, reason = "pixel counts are far below 2^52")]
    fn stray_share(&self) -> f64 {
        self.strays as f64 / self.pixels as f64
    }
}

fn compare(family: &Family, size: &Size) -> TestResult<Diff> {
    let metrics = size.metrics();
    let mut diff = Diff::default();
    let mut sheets: Vec<(u32, Sheet)> = Vec::new();
    for &(min, max) in family.ranges {
        for cp in min..=max {
            let base = cp & !0xff;
            let loaded = if let Some(index) = sheets.iter().position(|(sheet_base, _)| *sheet_base == base) {
                index
            } else {
                sheets.push((base, sheet(base, size)?));
                sheets.len() - 1
            };
            let golden = golden_cell(&sheets[loaded].1, cp, size);
            let ours = cell(cp, size, &metrics);
            let deltas: Vec<u8> = ours.iter().zip(&golden).map(|(a, b)| a.abs_diff(*b)).collect();
            let total: u64 = deltas.iter().map(|delta| u64::from(*delta)).sum();
            diff.pixels += deltas.len();
            diff.total += total;
            diff.strays += deltas.iter().filter(|delta| **delta > STRAY).count();
            if total > 0 {
                diff.worst.push((cp, total));
            }
        }
    }
    diff.worst.sort_by_key(|(_, total)| std::cmp::Reverse(*total));
    diff.worst.truncate(6);
    Ok(diff)
}

#[test]
fn every_family_matches_ghosttys_goldens_at_every_size() -> TestResult {
    let mut failures = Vec::new();
    for family in FAMILIES {
        for size in &SIZES {
            let diff = compare(family, size)?;
            let (mean, strays) = (diff.mean(), diff.stray_share());
            println!(
                "{:<28} {:<9} mean {:>7.4}% (budget {:.2}%), strays {:>7.4}% (budget {:.2}%) of {} pixels, worst {:X?}",
                family.name,
                size.label(),
                mean * 100.0,
                family.mean * 100.0,
                strays * 100.0,
                family.strays * 100.0,
                diff.pixels,
                diff.worst
            );
            if mean > family.mean || strays > family.strays {
                failures.push(format!(
                    "{} at {}: mean {:.4}%, strays {:.4}%",
                    family.name,
                    size.label(),
                    mean * 100.0,
                    strays * 100.0
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "sprites strayed from Ghostty's goldens: {failures:#?}"
    );
    Ok(())
}

#[test]
fn sprites_are_deterministic() {
    for size in &SIZES {
        let metrics = size.metrics();
        for family in FAMILIES {
            for &(min, max) in family.ranges {
                for cp in min..=max {
                    let first: Option<Mask> = sprite::rasterize(cp, &metrics, 1);
                    assert_eq!(
                        first,
                        sprite::rasterize(cp, &metrics, 1),
                        "U+{cp:X} at {}",
                        size.label()
                    );
                }
            }
        }
    }
}
