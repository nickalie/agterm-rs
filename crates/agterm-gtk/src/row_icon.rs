//! The sidebar's row and footer glyphs: the workspace grid, the session terminal and split rectangles, the remote
//! cloud, and the footer's flag and grid toggles (upstream's SF Symbols `square.grid.2x2`, `terminal`,
//! `rectangle.split.2x1`/`1x2`, `cloud`, `icloud.slash` and `flag`, picked by `WorkspaceSidebar.swift`'s row icons,
//! `iconForSession` in `WorkspaceSidebar+RowRendering.swift` and the footer toggles of `WindowContentView.swift`).
//!
//! GNOME's symbolic icon theme has no outline/filled pairs for these, and the sidebar spends exactly that distinction:
//! a FILL marks a flagged session (or an active footer toggle), a heavier WEIGHT marks a focus-set workspace or a remote
//! split. So [`draw`] traces them with cairo, in the widget's CSS foreground color so `dim-label`, opacity and the
//! selection colors apply, and knocks the cut-outs (the terminal's prompt, a filled split's gap, the slash's halo) out
//! to real transparency. Every glyph sits in the same 16x16 slot, so swapping one for another never shifts the row.

use std::cell::Cell;
use std::f64::consts::PI;
use std::rc::Rc;

use agterm_core::model::SplitAxis;
use gtk::prelude::*;
use gtk::{cairo, gdk};

/// The glyph's slot, square, in logical pixels (upstream's fixed 16x16 `NSImageView`).
const SLOT: i32 = 16;
/// [`SLOT`] in the design units every glyph is traced in: the slot is a `BOX` x `BOX` square at scale 1.
const BOX: f64 = 16.0;

/// The `regular` symbol weight's stroke.
const REGULAR: f64 = 1.25;
/// The `bold` symbol weight's stroke: a remote split.
const BOLD: f64 = 1.75;
/// The `black` symbol weight's stroke: a focus-set workspace.
const BLACK: f64 = 2.25;

/// The terminal and split frame: left, top, width, height and corner radius.
const FRAME: (f64, f64, f64, f64, f64) = (1.0, 2.5, 14.0, 11.0, 2.75);
/// The transparent gap between a filled split's two halves.
const SPLIT_GAP: f64 = 1.5;
/// The terminal's `>` prompt: top, tip and bottom.
const CHEVRON: [(f64, f64); 3] = [(4.5, 5.75), (7.0, 8.0), (4.5, 10.25)];
/// The terminal's `_` cursor: start and end.
const CURSOR: [(f64, f64); 2] = [(8.5, 10.25), (11.5, 10.25)];

/// The cloud's three bumps as `(center x, center y, radius)`: left, top, right; the left and right ones rest on
/// [`CLOUD_BASE`].
const CLOUD_BUMPS: [(f64, f64, f64); 3] = [(4.25, 10.0, 2.75), (8.0, 7.25, 4.0), (11.5, 9.75, 3.0)];
/// The cloud's flat bottom edge.
const CLOUD_BASE: f64 = 12.75;
/// The `icloud.slash` stroke: from the top left to the bottom right.
const SLASH: [(f64, f64); 2] = [(2.5, 2.0), (13.5, 14.0)];
/// The transparent halo either side of the slash, where it crosses the cloud.
const SLASH_HALO: f64 = 1.0;

/// The flag's pole: x, top and bottom.
const POLE: (f64, f64, f64) = (3.25, 1.5, 14.5);
/// The flag's banner: right edge x, top y, bottom y.
const BANNER: (f64, f64, f64) = (13.0, 2.5, 9.5);

/// Which glyph a [`RowIcon`] shows; every one fills the same 16x16 slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyph {
    /// A workspace row (`square.grid.2x2`): four outlined squares, at a much heavier stroke (`.black` weight, still
    /// outlined) while the workspace is a member of the focus set.
    Workspace {
        /// The workspace is in the focus set.
        focus_member: bool,
    },
    /// A local session row: `terminal` while unsplit, else `rectangle.split.2x1` (left/right panes) or
    /// `rectangle.split.1x2` (top/bottom panes); each swaps to its filled variant while the session is flagged (tree
    /// mode only: the flagged view passes `false`, every row there being flagged).
    Session {
        /// The pane arrangement, `None` while unsplit (or the split is hidden).
        split: Option<SplitAxis>,
        /// The session is flagged.
        flagged: bool,
    },
    /// A remote session row (`cloud`): bold while split, without telling the axis apart, and slashed
    /// (`icloud.slash`) while its presentation stream is down. Never filled: a fill reads as a flag.
    Remote {
        /// The session is split.
        split: bool,
        /// The presentation stream is not up.
        disconnected: bool,
    },
    /// The footer's flagged-view toggle: `flag`, filled (`flag.fill`) while the flagged view shows.
    Flag {
        /// The flagged view is on.
        filled: bool,
    },
    /// The footer's focus-filter toggle: `square.grid.2x2`, filled (`square.grid.2x2.fill`) while the filter applies.
    Grid {
        /// The focus filter is on.
        filled: bool,
    },
}

/// A 16x16 sidebar glyph drawn in its widget's CSS foreground color. Cloning it hands out another handle on the same
/// widget.
#[derive(Clone)]
pub struct RowIcon {
    area: gtk::DrawingArea,
    glyph: Rc<Cell<Glyph>>,
}

impl RowIcon {
    /// A widget showing `glyph`, centered in a fixed 16x16 slot.
    #[must_use]
    pub fn new(glyph: Glyph) -> Self {
        let area = gtk::DrawingArea::builder()
            .content_width(SLOT)
            .content_height(SLOT)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let glyph = Rc::new(Cell::new(glyph));
        let drawn = Rc::clone(&glyph);
        area.set_draw_func(move |area, cr, width, height| {
            cr.translate((f64::from(width) - BOX) / 2.0, (f64::from(height) - BOX) / 2.0);
            if let Err(err) = draw(cr, drawn.get(), BOX, &area.color()) {
                tracing::debug!(%err, "drawing a sidebar glyph failed");
            }
        });
        Self { area, glyph }
    }

    /// The widget to pack into a row or the footer.
    #[must_use]
    pub fn widget(&self) -> &gtk::Widget {
        self.area.upcast_ref()
    }

    /// Shows `glyph`; redraws only when it differs from the one shown. The slot keeps its size either way.
    pub fn set_glyph(&self, glyph: Glyph) {
        if self.glyph.replace(glyph) != glyph {
            self.area.queue_draw();
        }
    }

    /// The glyph shown.
    #[must_use]
    #[cfg(test)]
    pub fn glyph(&self) -> Glyph {
        self.glyph.get()
    }
}

/// Draws `glyph` in `rgba` into the `size` x `size` square at the user-space origin of `cr`, the 16-unit design scaled
/// to fit. Cut-outs are transparent, so whatever lies beneath shows through; the context's state is left as found.
///
/// # Errors
///
/// A cairo error from the context (a failed surface allocation, a context already in an error state).
pub fn draw(cr: &cairo::Context, glyph: Glyph, size: f64, rgba: &gdk::RGBA) -> Result<(), cairo::Error> {
    cr.save()?;
    let drawn = draw_scaled(cr, glyph, size, rgba);
    cr.restore()?;
    drawn
}

/// [`draw`] inside its save/restore: traces the glyph into a group, so the cut-outs clear only the glyph's own ink, and
/// paints the group.
fn draw_scaled(cr: &cairo::Context, glyph: Glyph, size: f64, rgba: &gdk::RGBA) -> Result<(), cairo::Error> {
    cr.scale(size / BOX, size / BOX);
    cr.set_source_rgba(
        f64::from(rgba.red()),
        f64::from(rgba.green()),
        f64::from(rgba.blue()),
        f64::from(rgba.alpha()),
    );
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.push_group();
    let traced = trace(cr, glyph);
    cr.pop_group_to_source()?;
    traced?;
    cr.paint()
}

/// Inks `glyph` in the current source.
fn trace(cr: &cairo::Context, glyph: Glyph) -> Result<(), cairo::Error> {
    match glyph {
        Glyph::Workspace { focus_member: false } | Glyph::Grid { filled: false } => grid(cr, 1.5, 14.5, Some(REGULAR)),
        Glyph::Workspace { focus_member: true } => grid(cr, 1.0, 15.0, Some(BLACK)),
        Glyph::Grid { filled: true } => grid(cr, 1.5, 14.5, None),
        Glyph::Session { split: None, flagged } => terminal(cr, flagged),
        Glyph::Session {
            split: Some(axis),
            flagged,
        } => split(cr, axis, flagged),
        Glyph::Remote { split, disconnected } => cloud(cr, if split { BOLD } else { REGULAR }, disconnected),
        Glyph::Flag { filled } => flag(cr, filled),
    }
}

/// `square.grid.2x2` spanning `low..high` on both axes: four rounded squares, outlined at `stroke` or filled for
/// `None`. An outline stays inside the span.
fn grid(cr: &cairo::Context, low: f64, high: f64, stroke: Option<f64>) -> Result<(), cairo::Error> {
    let gap = 1.75;
    let side = (high - low - gap) / 2.0;
    let inset = stroke.map_or(0.0, |width| width / 2.0);
    for x in [low, low + side + gap] {
        for y in [low, low + side + gap] {
            rounded_rect(
                cr,
                (x + inset, y + inset, side - 2.0 * inset, side - 2.0 * inset),
                1.75 - inset,
            );
        }
    }
    ink(cr, stroke)
}

/// `terminal`: the frame with a `>` prompt and a `_` cursor; `terminal.fill` when `filled`, the prompt and cursor
/// knocked out of the solid frame.
fn terminal(cr: &cairo::Context, filled: bool) -> Result<(), cairo::Error> {
    frame(cr, filled)?;
    if filled {
        cr.set_operator(cairo::Operator::Clear);
    }
    cr.set_line_width(REGULAR);
    polyline(cr, &CHEVRON);
    polyline(cr, &CURSOR);
    let stroked = cr.stroke();
    cr.set_operator(cairo::Operator::Over);
    stroked
}

/// `rectangle.split.2x1` (left/right) or `rectangle.split.1x2` (top/bottom): the frame with a divider; the `.fill`
/// variant when `filled`, both halves solid with a transparent gap between.
fn split(cr: &cairo::Context, axis: SplitAxis, filled: bool) -> Result<(), cairo::Error> {
    frame(cr, filled)?;
    let (left, top, width, height, _) = FRAME;
    let (center_x, center_y) = (left + width / 2.0, top + height / 2.0);
    if filled {
        let half = SPLIT_GAP / 2.0;
        match axis {
            SplitAxis::LeftRight => cr.rectangle(center_x - half, top, SPLIT_GAP, height),
            SplitAxis::TopBottom => cr.rectangle(left, center_y - half, width, SPLIT_GAP),
        }
        cr.set_operator(cairo::Operator::Clear);
        let cleared = cr.fill();
        cr.set_operator(cairo::Operator::Over);
        return cleared;
    }
    match axis {
        SplitAxis::LeftRight => polyline(cr, &[(center_x, top), (center_x, top + height)]),
        SplitAxis::TopBottom => polyline(cr, &[(left, center_y), (left + width, center_y)]),
    }
    ink(cr, Some(REGULAR))
}

/// The terminal and split frame: outlined inside [`FRAME`], or filled.
fn frame(cr: &cairo::Context, filled: bool) -> Result<(), cairo::Error> {
    let (left, top, width, height, radius) = FRAME;
    let inset = if filled { 0.0 } else { REGULAR / 2.0 };
    rounded_rect(
        cr,
        (left + inset, top + inset, width - 2.0 * inset, height - 2.0 * inset),
        radius - inset,
    );
    ink(cr, (!filled).then_some(REGULAR))
}

/// `cloud` outlined at `stroke`; `icloud.slash` when `slashed`, the slash set off from the outline by a transparent
/// halo.
fn cloud(cr: &cairo::Context, stroke: f64, slashed: bool) -> Result<(), cairo::Error> {
    let [left, top, right] = CLOUD_BUMPS;
    let left_top = upper_intersection(left, top);
    let top_right = upper_intersection(top, right);
    cr.move_to(left.0, CLOUD_BASE);
    arc_between(cr, left, (left.0, CLOUD_BASE), left_top);
    arc_between(cr, top, left_top, top_right);
    arc_between(cr, right, top_right, (right.0, CLOUD_BASE));
    cr.close_path();
    ink(cr, Some(stroke))?;
    if !slashed {
        return Ok(());
    }
    polyline(cr, &SLASH);
    cr.set_line_width(stroke + 2.0 * SLASH_HALO);
    cr.set_operator(cairo::Operator::Clear);
    let cleared = cr.stroke_preserve();
    cr.set_operator(cairo::Operator::Over);
    cleared?;
    ink(cr, Some(stroke))
}

/// `flag`: the pole and a waving banner, outlined; `flag.fill` when `filled`, the banner solid.
fn flag(cr: &cairo::Context, filled: bool) -> Result<(), cairo::Error> {
    let (pole, pole_top, pole_bottom) = POLE;
    let (right, top, bottom) = BANNER;
    let third = (right - pole) / 3.0;
    cr.move_to(pole, top);
    cr.curve_to(pole + third, top - 1.5, pole + 2.0 * third, top + 1.5, right, top);
    cr.line_to(right, bottom);
    cr.curve_to(
        pole + 2.0 * third,
        bottom + 1.5,
        pole + third,
        bottom - 1.5,
        pole,
        bottom,
    );
    cr.close_path();
    if filled {
        cr.fill()?;
    }
    polyline(cr, &[(pole, pole_top), (pole, pole_bottom)]);
    ink(cr, Some(REGULAR))
}

/// Strokes the current path at `stroke`, or fills it for `None`.
fn ink(cr: &cairo::Context, stroke: Option<f64>) -> Result<(), cairo::Error> {
    match stroke {
        Some(width) => {
            cr.set_line_width(width);
            cr.stroke()
        }
        None => cr.fill(),
    }
}

/// Traces the rectangle `(left, top, width, height)` with corners of `radius`, as a new sub-path.
fn rounded_rect(cr: &cairo::Context, (left, top, width, height): (f64, f64, f64, f64), radius: f64) {
    let radius = radius.clamp(0.0, width.min(height) / 2.0);
    let (right, bottom) = (left + width, top + height);
    cr.new_sub_path();
    cr.arc(right - radius, top + radius, radius, -PI / 2.0, 0.0);
    cr.arc(right - radius, bottom - radius, radius, 0.0, PI / 2.0);
    cr.arc(left + radius, bottom - radius, radius, PI / 2.0, PI);
    cr.arc(left + radius, top + radius, radius, PI, 3.0 * PI / 2.0);
    cr.close_path();
}

/// Traces an open polyline through `points`, as a new sub-path.
fn polyline(cr: &cairo::Context, points: &[(f64, f64)]) {
    let mut points = points.iter();
    if let Some(&(x, y)) = points.next() {
        cr.move_to(x, y);
    }
    for &(x, y) in points {
        cr.line_to(x, y);
    }
}

/// Continues the path clockwise along `circle` (`(center x, center y, radius)`) from the point `from` to the point
/// `to`, both on it.
fn arc_between(cr: &cairo::Context, circle: (f64, f64, f64), from: (f64, f64), to: (f64, f64)) {
    let (x, y, radius) = circle;
    let angle = |(px, py): (f64, f64)| (py - y).atan2(px - x);
    cr.arc(x, y, radius, angle(from), angle(to));
}

/// Where the outlines of two overlapping circles (`(center x, center y, radius)`) cross, the upper of the two points.
fn upper_intersection(first: (f64, f64, f64), second: (f64, f64, f64)) -> (f64, f64) {
    let ((x1, y1, r1), (x2, y2, r2)) = (first, second);
    let (dx, dy) = (x2 - x1, y2 - y1);
    let distance = dx.hypot(dy);
    let along = (r1 * r1 - r2 * r2 + distance * distance) / (2.0 * distance);
    let across = (r1 * r1 - along * along).max(0.0).sqrt();
    let (base_x, base_y) = (x1 + along * dx / distance, y1 + along * dy / distance);
    let (off_x, off_y) = (-dy * across / distance, dx * across / distance);
    let (a, b) = ((base_x + off_x, base_y + off_y), (base_x - off_x, base_y - off_y));
    if a.1 <= b.1 { a } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtk_test::on_gtk_thread;

    /// A rendered glyph's alpha plane, `side` x `side`.
    struct Alpha {
        side: usize,
        alpha: Vec<u8>,
    }

    impl Alpha {
        fn at(&self, x: usize, y: usize) -> u8 {
            self.alpha[y * self.side + x]
        }

        /// The pixels at least half opaque.
        fn opaque(&self) -> usize {
            self.alpha.iter().filter(|&&alpha| alpha >= 128).count()
        }

        /// The total coverage.
        fn ink(&self) -> u64 {
            self.alpha.iter().map(|&alpha| u64::from(alpha)).sum()
        }
    }

    /// Renders `glyph` at `pixels` pixels square, framed by an empty `margin` on every side, in opaque black.
    fn render_framed(glyph: Glyph, pixels: i32, margin: i32) -> Alpha {
        let extent = pixels + 2 * margin;
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, extent, extent).expect("an image surface");
        {
            let cr = cairo::Context::new(&surface).expect("a cairo context");
            cr.translate(f64::from(margin), f64::from(margin));
            draw(&cr, glyph, f64::from(pixels), &gdk::RGBA::BLACK).expect("the glyph draws");
        }
        surface.flush();
        let stride = usize::try_from(surface.stride()).expect("a positive stride");
        let side = usize::try_from(extent).expect("a positive extent");
        let data = surface.data().expect("the surface's pixels");
        let mut alpha = Vec::with_capacity(side * side);
        for y in 0..side {
            for x in 0..side {
                let offset = y * stride + x * 4;
                let pixel = u32::from_ne_bytes(data[offset..offset + 4].try_into().expect("four bytes"));
                alpha.push(u8::try_from(pixel >> 24).expect("the alpha byte"));
            }
        }
        Alpha { side, alpha }
    }

    fn render(glyph: Glyph, size: i32) -> Alpha {
        render_framed(glyph, size, 0)
    }

    fn every_glyph() -> Vec<Glyph> {
        let mut glyphs = Vec::new();
        for on in [false, true] {
            glyphs.push(Glyph::Workspace { focus_member: on });
            glyphs.push(Glyph::Flag { filled: on });
            glyphs.push(Glyph::Grid { filled: on });
            for split in [None, Some(SplitAxis::LeftRight), Some(SplitAxis::TopBottom)] {
                glyphs.push(Glyph::Session { split, flagged: on });
            }
            for disconnected in [false, true] {
                glyphs.push(Glyph::Remote {
                    split: on,
                    disconnected,
                });
            }
        }
        glyphs
    }

    #[test]
    fn every_glyph_renders_ink_and_differs_from_every_other() {
        // the footer's unfilled grid is the workspace row's symbol; every other pair is a distinct symbol
        let glyphs: Vec<Glyph> = every_glyph()
            .into_iter()
            .filter(|&glyph| glyph != Glyph::Grid { filled: false })
            .collect();
        let rendered: Vec<Alpha> = glyphs.iter().map(|&glyph| render(glyph, 16)).collect();
        for (glyph, alpha) in glyphs.iter().zip(&rendered) {
            assert!(alpha.opaque() > 20, "{glyph:?} renders some ink");
        }
        for (i, first) in rendered.iter().enumerate() {
            for (j, second) in rendered.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    first.alpha, second.alpha,
                    "{:?} and {:?} look alike",
                    glyphs[i], glyphs[j]
                );
            }
        }
        assert!(render(Glyph::Grid { filled: false }, 16).opaque() > 20);
    }

    #[test]
    fn nothing_draws_outside_the_slot() {
        // the slot is pixels 4..20 of a 24-pixel square
        let slot = 4..20;
        for glyph in every_glyph() {
            let framed = render_framed(glyph, 16, 4);
            let outside = (0..framed.side * framed.side)
                .map(|index| (index % framed.side, index / framed.side))
                .filter(|(x, y)| !slot.contains(x) || !slot.contains(y));
            for (x, y) in outside {
                assert_eq!(framed.at(x, y), 0, "{glyph:?} inks ({x}, {y}) outside the slot");
            }
        }
    }

    #[test]
    fn a_flagged_session_fills_its_icon() {
        for split in [None, Some(SplitAxis::LeftRight), Some(SplitAxis::TopBottom)] {
            let plain = render(Glyph::Session { split, flagged: false }, 16);
            let flagged = render(Glyph::Session { split, flagged: true }, 16);
            assert!(
                flagged.opaque() > plain.opaque() * 3 / 2,
                "{split:?}: the flagged icon is filled"
            );
        }
    }

    #[test]
    fn the_filled_terminal_knocks_its_prompt_out() {
        // 4x scale: the pixel at (26, 30) covers design point (6.625, 7.625), on the chevron's upper stroke
        let filled = render(
            Glyph::Session {
                split: None,
                flagged: true,
            },
            64,
        );
        let outline = render(
            Glyph::Session {
                split: None,
                flagged: false,
            },
            64,
        );
        assert_eq!(filled.at(26, 30), 0, "the chevron is see-through in terminal.fill");
        assert_eq!(outline.at(26, 30), 255, "the chevron is inked in terminal");
        // design point (12.5, 5.5): solid frame in the fill, empty inside the outline
        assert_eq!(filled.at(50, 22), 255);
        assert_eq!(outline.at(50, 22), 0);
    }

    #[test]
    fn a_filled_split_keeps_a_transparent_gap_on_its_divider() {
        // 4x scale: design point (8.125, 4.625) sits on the left/right divider, (4.625, 8.125) on the top/bottom one
        let cases = [(SplitAxis::LeftRight, (32, 18)), (SplitAxis::TopBottom, (18, 32))];
        for (axis, (x, y)) in cases {
            let filled = render(
                Glyph::Session {
                    split: Some(axis),
                    flagged: true,
                },
                64,
            );
            let outline = render(
                Glyph::Session {
                    split: Some(axis),
                    flagged: false,
                },
                64,
            );
            assert_eq!(filled.at(x, y), 0, "{axis:?}: the halves stand apart");
            assert_eq!(outline.at(x, y), 255, "{axis:?}: the outline draws the divider");
            assert!(
                filled.at(y, x) == 255 && outline.at(y, x) == 0,
                "{axis:?}: no divider across"
            );
        }
    }

    #[test]
    fn a_focus_member_workspace_is_heavier_but_not_filled() {
        let plain = render(Glyph::Workspace { focus_member: false }, 64);
        let member = render(Glyph::Workspace { focus_member: true }, 64);
        assert!(member.ink() > plain.ink() * 3 / 2, "the focus member's grid is heavier");
        // the center of the top-left square (design (4.125, 4.125)) stays hollow, unlike the filled footer grid
        assert_eq!(member.at(16, 16), 0);
        assert_eq!(render(Glyph::Grid { filled: true }, 64).at(16, 16), 255);
    }

    #[test]
    fn the_footer_toggles_fill_when_on() {
        for (off, on) in [
            (Glyph::Flag { filled: false }, Glyph::Flag { filled: true }),
            (Glyph::Grid { filled: false }, Glyph::Grid { filled: true }),
        ] {
            assert!(
                render(on, 16).opaque() > render(off, 16).opaque() * 6 / 5,
                "{on:?} is filled"
            );
        }
        // 4x scale: design point (8.125, 6.125) is inside the banner, solid only in flag.fill
        assert_eq!(render(Glyph::Flag { filled: true }, 64).at(32, 24), 255);
        assert_eq!(render(Glyph::Flag { filled: false }, 64).at(32, 24), 0);
        assert_eq!(
            render(Glyph::Grid { filled: false }, 16).alpha,
            render(Glyph::Workspace { focus_member: false }, 16).alpha,
            "the footer's grid is the workspace row's"
        );
    }

    #[test]
    fn a_remote_split_is_bolder_and_a_disconnected_cloud_is_slashed() {
        for disconnected in [false, true] {
            let single = render(
                Glyph::Remote {
                    split: false,
                    disconnected,
                },
                64,
            );
            let split = render(
                Glyph::Remote {
                    split: true,
                    disconnected,
                },
                64,
            );
            assert!(split.ink() > single.ink() * 5 / 4, "a split cloud is bolder");
        }
        // design point (3.125, 2.625): on the slash, clear of the cloud
        let connected = render(
            Glyph::Remote {
                split: false,
                disconnected: false,
            },
            64,
        );
        let slashed = render(
            Glyph::Remote {
                split: false,
                disconnected: true,
            },
            64,
        );
        assert_eq!(connected.at(12, 10), 0);
        assert_eq!(slashed.at(12, 10), 255);
        // the cloud's inside stays empty either way (design (8.125, 9.625))
        assert_eq!(connected.at(32, 38), 0);
    }

    #[test]
    fn drawing_leaves_the_context_as_found() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 16, 16).expect("an image surface");
        let cr = cairo::Context::new(&surface).expect("a cairo context");
        cr.set_line_width(7.0);
        for glyph in every_glyph() {
            draw(&cr, glyph, 16.0, &gdk::RGBA::BLACK).expect("the glyph draws");
        }
        assert!((cr.line_width() - 7.0).abs() < f64::EPSILON);
        assert_eq!(cr.operator(), cairo::Operator::Over);
        assert_eq!(cr.matrix(), cairo::Matrix::identity());
    }

    #[test]
    fn the_widget_keeps_a_16_pixel_slot_and_swaps_glyphs() {
        on_gtk_thread(|| {
            let icon = RowIcon::new(Glyph::Workspace { focus_member: false });
            let widget = icon.widget().clone();
            let natural = |orientation| widget.measure(orientation, -1).1;
            assert_eq!(natural(gtk::Orientation::Horizontal), SLOT);
            assert_eq!(natural(gtk::Orientation::Vertical), SLOT);

            let flagged = Glyph::Session {
                split: Some(SplitAxis::TopBottom),
                flagged: true,
            };
            icon.set_glyph(flagged);
            assert_eq!(icon.glyph(), flagged);
            assert_eq!(icon.clone().glyph(), flagged, "a clone shares the glyph");
            assert_eq!(natural(gtk::Orientation::Horizontal), SLOT, "a swap keeps the slot");
        });
    }
}
