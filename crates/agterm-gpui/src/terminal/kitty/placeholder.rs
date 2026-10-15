//! Where a run of Kitty Unicode placeholder cells draws its part of the image: Ghostty's `Placement.renderPlacement`
//! and `grid` (`src/terminal/kitty/graphics_unicode.zig` at `LIBGHOSTTY_VT_REV`, MIT, Copyright Mitchell Hashimoto and
//! Ghostty contributors), and its `placeholderTarget` over the frame's virtual placements.
//!
//! The image is fitted, aspect kept and centered, into the virtual placement's grid (`c=`/`r=`, or the image's own
//! size in cells); each run draws the slice of that grid its cells name.
//!
//! ⚠️ libghostty-vt's C API reports a placement id without its namespace (a client's `p=` or one the terminal
//! assigned), so ids resolve by number: a named id can match a terminal-assigned placement Ghostty would not match,
//! and with none named the image's virtual placement with the lowest number wins where Ghostty prefers a client's
//! first. Relative placements (`P=`) rooted at a virtual placement are not drawn: the C API gives them no position,
//! root or chain offsets. Both are parity gaps tracked in `docs/backlog/kitty-virtual-placement-c-api-gaps.md`.

use agterm_term::{KittyPlacement, KittyView, SourceRect, VirtualPlacement};

/// What [`render`] places, in the terminal's pixels: offsets from the run's first cell, the source rectangle in the
/// image's pixels, the drawn size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtualRender {
    pub offset_x: u32,
    pub offset_y: u32,
    pub source: SourceRect,
    pub width: u32,
    pub height: u32,
}

/// The placement a run of placeholders shows (Ghostty's `placeholderTarget`): the image's placement with the id the
/// run names, wherever it is placed, or with none named its virtual placement with the lowest id.
#[must_use]
pub fn target<'a>(view: &'a KittyView, run: &VirtualPlacement) -> Option<&'a KittyPlacement> {
    let mut of_image = view
        .placements
        .iter()
        .filter(|placement| placement.image.id == run.image_id);
    if run.placement_id > 0 {
        of_image.find(|placement| placement.placement_id == run.placement_id)
    } else {
        of_image
            .filter(|placement| placement.is_virtual)
            .min_by_key(|placement| placement.placement_id)
    }
}

/// The grid the image is fitted into: the placement's cells, or as many as the image needs at its own size.
fn grid(target: &KittyPlacement, image: (u32, u32), cell: (u32, u32)) -> Option<(u32, u32)> {
    let fit = |asked: u32, size: u32, cell: u32| {
        if asked > 0 { asked } else { size.div_ceil(cell.max(1)) }
    };
    let columns = fit(target.columns, image.0, cell.0);
    let rows = fit(target.rows, image.1, cell.1);
    // Ghostty's cell counts are u16
    (u16::try_from(columns).is_ok() && u16::try_from(rows).is_ok() && columns > 0 && rows > 0)
        .then_some((columns, rows))
}

/// One axis of the source rectangle and destination (Ghostty's per-axis branches of `renderPlacement`).
struct Axis {
    /// The image's size on this axis.
    image: f64,
    /// The fitted image's padding on each side, in image pixels.
    pad: f64,
    /// The padded image's size.
    scaled: f64,
    /// Image pixels to placement pixels.
    scale: f64,
}

impl Axis {
    /// The run's slice `(source start, source length)` in padded-image pixels and its drawn length → the source
    /// start and length in image pixels, the offset into the run and the drawn length.
    fn clip(&self, mut start: f64, mut length: f64, mut drawn: f64) -> (f64, f64, f64, f64) {
        let mut offset = 0.0;
        if start < self.pad {
            // the slice starts in the padding before the image
            let skip = self.pad - start;
            length -= skip;
            offset = skip;
            drawn -= skip * self.scale;
            start = 0.0;
            if length > self.image {
                length = self.image;
                drawn = self.image * self.scale;
            }
        } else if start + length > self.scaled - self.pad {
            // the slice ends in the padding after the image
            start -= self.pad;
            length = self.scaled - self.pad - start;
            length -= self.pad;
            drawn = length * self.scale;
        } else {
            start -= self.pad;
        }
        (start, length, offset * self.scale, drawn)
    }
}

/// Ghostty's `renderPlacement` of `run` for the image of `image` pixels and its `target`, at `cell` pixels a cell;
/// `None` when nothing of the image falls in the run.
#[must_use]
pub fn render(
    run: &VirtualPlacement,
    target: &KittyPlacement,
    image: (u32, u32),
    cell: (u32, u32),
) -> Option<VirtualRender> {
    let (columns, rows) = grid(target, image, cell)?;
    let (image_width, image_height) = (f64::from(image.0), f64::from(image.1));
    let grid_width = f64::from(columns * cell.0);
    let grid_height = f64::from(rows * cell.1);
    // fit the image into the grid, aspect kept, centered on the other axis
    let (scale, x_pad, y_pad) = if image_width * grid_height > image_height * grid_width {
        let scale = grid_width / image_width.max(1.0);
        (scale, 0.0, (grid_height - image_height * scale) / 2.0)
    } else {
        let scale = grid_height / image_height.max(1.0);
        (scale, (grid_width - image_width * scale) / 2.0, 0.0)
    };
    let x = Axis {
        image: image_width,
        pad: x_pad / scale,
        scaled: image_width + 2.0 * x_pad / scale,
        scale,
    };
    let y = Axis {
        image: image_height,
        pad: y_pad / scale,
        scaled: image_height + 2.0 * y_pad / scale,
        scale,
    };
    let fraction = |part: u32, whole: u32| f64::from(part) / f64::from(whole);
    let (source_y, source_height, offset_y, height) = y.clip(
        y.scaled * fraction(run.row, rows),
        y.scaled * fraction(run.height, rows),
        f64::from(run.height * cell.1),
    );
    let (source_x, source_width, offset_x, width) = x.clip(
        x.scaled * fraction(run.col, columns),
        x.scaled * fraction(run.width, columns),
        f64::from(run.width * cell.0),
    );
    if source_width <= 0.0 || source_height <= 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "pixel values of a grid of u16 cells, rounded as Ghostty rounds them"
    )]
    let whole = |value: f64| value.round().max(0.0) as u32;
    Some(VirtualRender {
        offset_x: whole(offset_x),
        offset_y: whole(offset_y),
        source: SourceRect {
            x: whole(source_x),
            y: whole(source_y),
            width: whole(source_width),
            height: whole(source_height),
        },
        width: whole(width),
        height: whole(height),
    })
}
