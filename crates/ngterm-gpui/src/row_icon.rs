//! The sidebar's row and footer glyphs (ported from the GTK build's `row_icon.rs`): the workspace grid, the session
//! terminal and split rectangles, the remote cloud, the footer's flag and grid toggles (upstream's SF Symbols
//! `square.grid.2x2`, `terminal`, `rectangle.split.2x1`/`1x2`, `cloud`, `icloud.slash` and `flag`, picked by
//! `WorkspaceSidebar.swift`'s row icons, `iconForSession` in `WorkspaceSidebar+RowRendering.swift` and the footer
//! toggles of `WindowContentView.swift`), and the controls GTK took from its icon theme: the disclosure chevron, the
//! workspace row's hover "+", and the footer's New Workspace (`rectangle.stack.badge.plus`) and add-session
//! (`plus.rectangle`) symbols.
//!
//! The sidebar spends the outline/filled distinction: a FILL marks a flagged session (or an active footer toggle), a
//! heavier WEIGHT marks a focus-set workspace or a remote split. So the glyphs are traced here ([`rasterize`], through
//! [`ink`]) into a coverage mask with the cut-outs (the terminal's prompt, a filled split's gap, the slash's halo)
//! knocked out to real transparency, and [`icon`] paints the mask tinted with the row's color through GPUI's
//! `paint_alpha_mask` (one texel per device pixel, cached in the sprite atlas). Every glyph sits in the same 16x16
//! slot, so swapping one for another never shifts the row.

pub mod ink;

#[cfg(test)]
mod tests;

use std::f64::consts::PI;
use std::hash::{DefaultHasher, Hash, Hasher};

use gpui_kit::{
    App, Bounds, DevicePixels, Hsla, IntoElement, Pixels, RenderAlphaMaskParams, Styled as _, Window, canvas, point,
    px, size,
};
use ngterm_core::model::SplitAxis;

use self::ink::{Ink, Trace};

/// The glyph's slot, square, in logical pixels (upstream's fixed 16x16 `NSImageView`).
pub const SLOT: f32 = 16.0;
/// [`SLOT`] in the design units every glyph is traced in.
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

/// The disclosure chevron's stroke (`chevron.right`/`chevron.down` at a small point size).
const DISCLOSURE: f64 = 1.5;
/// The "+" strokes (`plus`).
const PLUS: f64 = 1.5;

/// Which glyph an [`icon`] shows; every one fills the same 16x16 slot.
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
        /// The pane arrangement, `None` while the session has no split (a hidden split still shows its arrangement).
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
    /// A workspace row's disclosure: pointing right while folded, down while open.
    Disclosure {
        /// The row is open.
        expanded: bool,
    },
    /// The workspace row's hover "+" (`plus`).
    Plus,
    /// The footer's New Workspace (`rectangle.stack.badge.plus`).
    NewWorkspace,
    /// The footer's add-session menu (`plus.rectangle`).
    AddSession,
}

/// The atlas owner of the sidebar's glyph masks.
const OWNER: &str = "ngterm.sidebar.glyph";

/// `glyph` in a 16x16 slot, tinted `color`.
#[must_use]
pub fn icon(glyph: Glyph, color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            paint_mask(window, bounds, color, Mask::Row(glyph));
        },
    )
    .flex_none()
    .size(px(SLOT))
}

/// What a 16x16 slot paints: a row glyph or a status silhouette (the status glyph shares the painting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Mask {
    Row(Glyph),
    Status(ngterm_core::model::StatusShape),
}

impl Mask {
    fn rasterize(self, side: u32) -> Vec<u8> {
        match self {
            Self::Row(glyph) => rasterize(glyph, side),
            Self::Status(shape) => crate::status_glyph::rasterize(shape, side),
        }
    }
}

/// The slot's side in device pixels at `scale`.
fn device_side(scale: f32) -> u32 {
    let side = (SLOT * scale).round();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a 16-point slot at a display scale: a few dozen pixels"
    )]
    let side = side.clamp(1.0, 1024.0) as u32;
    side
}

/// Paints `mask` centered in `bounds` (a 16x16 slot), tinted `color`, from the sprite atlas (rasterized on a miss).
pub(crate) fn paint_mask(window: &mut Window, bounds: Bounds<Pixels>, color: Hsla, mask: Mask) {
    let side = device_side(window.scale_factor());
    let mut hasher = DefaultHasher::new();
    (mask, side).hash(&mut hasher);
    let params = RenderAlphaMaskParams {
        owner: OWNER,
        id: hasher.finish(),
    };
    let offset = (bounds.size - size(px(SLOT), px(SLOT))) / 2.0;
    let origin = bounds.origin + point(offset.width, offset.height);
    let extent = DevicePixels(i32::try_from(side).unwrap_or(i32::MAX));
    let painted = window.paint_alpha_mask(origin, params, color, || {
        Some((size(extent, extent), mask.rasterize(side)))
    });
    if let Err(err) = painted {
        tracing::debug!(%err, "a sidebar glyph was not painted");
    }
}

/// `glyph`'s coverage on a `side` x `side` canvas (the 16-unit design scaled to fit), row-major.
#[must_use]
pub fn rasterize(glyph: Glyph, side: u32) -> Vec<u8> {
    let mut ink = Ink::new(side, BOX);
    trace(&mut ink, glyph);
    ink.coverage()
}

/// Inks `glyph`.
fn trace(ink: &mut Ink, glyph: Glyph) {
    match glyph {
        Glyph::Workspace { focus_member: false } | Glyph::Grid { filled: false } => grid(ink, 1.5, 14.5, Some(REGULAR)),
        Glyph::Workspace { focus_member: true } => grid(ink, 1.0, 15.0, Some(BLACK)),
        Glyph::Grid { filled: true } => grid(ink, 1.5, 14.5, None),
        Glyph::Session { split: None, flagged } => terminal(ink, flagged),
        Glyph::Session {
            split: Some(axis),
            flagged,
        } => split(ink, axis, flagged),
        Glyph::Remote { split, disconnected } => cloud(ink, if split { BOLD } else { REGULAR }, disconnected),
        Glyph::Flag { filled } => flag(ink, filled),
        Glyph::Disclosure { expanded } => disclosure(ink, expanded),
        Glyph::Plus => plus(ink, (8.0, 8.0), 4.5),
        Glyph::NewWorkspace => new_workspace(ink),
        Glyph::AddSession => add_session(ink),
    }
}

/// Strokes `trace` at `stroke`, or fills it for `None`.
fn ink_with(ink: &mut Ink, trace: Trace, stroke: Option<f64>) {
    match stroke {
        Some(width) => ink.stroke(trace, width),
        None => ink.fill(trace),
    }
}

/// `square.grid.2x2` spanning `low..high` on both axes: four rounded squares, outlined at `stroke` or filled for
/// `None`. An outline stays inside the span.
fn grid(ink: &mut Ink, low: f64, high: f64, stroke: Option<f64>) {
    let gap = 1.75;
    let side = (high - low - gap) / 2.0;
    let inset = stroke.map_or(0.0, |width| width / 2.0);
    let mut trace = Trace::new();
    for x in [low, low + side + gap] {
        for y in [low, low + side + gap] {
            trace.rounded_rect(
                (x + inset, y + inset, side - 2.0 * inset, side - 2.0 * inset),
                1.75 - inset,
            );
        }
    }
    ink_with(ink, trace, stroke);
}

/// `terminal`: the frame with a `>` prompt and a `_` cursor; `terminal.fill` when `filled`, the prompt and cursor
/// knocked out of the solid frame.
fn terminal(ink: &mut Ink, filled: bool) {
    frame(ink, filled);
    let mut prompt = Trace::new();
    prompt.polyline(&CHEVRON);
    prompt.polyline(&CURSOR);
    if filled {
        ink.clear_stroke(prompt, REGULAR, None);
    } else {
        ink.stroke(prompt, REGULAR);
    }
}

/// `rectangle.split.2x1` (left/right) or `rectangle.split.1x2` (top/bottom): the frame with a divider; the `.fill`
/// variant when `filled`, both halves solid with a transparent gap between.
fn split(ink: &mut Ink, axis: SplitAxis, filled: bool) {
    frame(ink, filled);
    let (left, top, width, height, _) = FRAME;
    let (center_x, center_y) = (left + width / 2.0, top + height / 2.0);
    let mut divider = Trace::new();
    if filled {
        let half = SPLIT_GAP / 2.0;
        let (x, y, w, h) = match axis {
            SplitAxis::LeftRight => (center_x - half, top, SPLIT_GAP, height),
            SplitAxis::TopBottom => (left, center_y - half, width, SPLIT_GAP),
        };
        divider.polygon(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)]);
        ink.clear_fill(divider);
        return;
    }
    match axis {
        SplitAxis::LeftRight => divider.polyline(&[(center_x, top), (center_x, top + height)]),
        SplitAxis::TopBottom => divider.polyline(&[(left, center_y), (left + width, center_y)]),
    }
    ink.stroke(divider, REGULAR);
}

/// The terminal and split frame: outlined inside [`FRAME`], or filled.
fn frame(ink: &mut Ink, filled: bool) {
    let (left, top, width, height, radius) = FRAME;
    let inset = if filled { 0.0 } else { REGULAR / 2.0 };
    let mut trace = Trace::new();
    trace.rounded_rect(
        (left + inset, top + inset, width - 2.0 * inset, height - 2.0 * inset),
        radius - inset,
    );
    ink_with(ink, trace, (!filled).then_some(REGULAR));
}

/// `cloud` outlined at `stroke`; `icloud.slash` when `slashed`, the slash set off from the outline by a transparent
/// halo.
fn cloud(ink: &mut Ink, stroke: f64, slashed: bool) {
    let [left, top, right] = CLOUD_BUMPS;
    let left_top = upper_intersection(left, top);
    let top_right = upper_intersection(top, right);
    let mut outline = Trace::new();
    outline.move_to((left.0, CLOUD_BASE));
    arc_between(&mut outline, left, (left.0, CLOUD_BASE), left_top);
    arc_between(&mut outline, top, left_top, top_right);
    arc_between(&mut outline, right, top_right, (right.0, CLOUD_BASE));
    outline.close();
    ink.stroke(outline, stroke);
    if !slashed {
        return;
    }
    let mut slash = Trace::new();
    slash.polyline(&SLASH);
    ink.clear_stroke(slash, stroke + 2.0 * SLASH_HALO, Some(stroke));
}

/// `flag`: the pole and a waving banner, outlined; `flag.fill` when `filled`, the banner solid.
fn flag(ink: &mut Ink, filled: bool) {
    let (pole, pole_top, pole_bottom) = POLE;
    let (right, top, bottom) = BANNER;
    let third = (right - pole) / 3.0;
    let banner = || {
        let mut banner = Trace::new();
        banner.move_to((pole, top));
        banner.curve_to((pole + third, top - 1.5), (pole + 2.0 * third, top + 1.5), (right, top));
        banner.line_to((right, bottom));
        banner.curve_to(
            (pole + 2.0 * third, bottom + 1.5),
            (pole + third, bottom - 1.5),
            (pole, bottom),
        );
        banner.close();
        banner
    };
    // the filled banner is solid without an outline; the outlined one is stroked with the pole
    let mut outline = if filled {
        ink.fill(banner());
        Trace::new()
    } else {
        banner()
    };
    outline.polyline(&[(pole, pole_top), (pole, pole_bottom)]);
    ink.stroke(outline, REGULAR);
}

/// The disclosure chevron: `>` while folded, `v` while open.
fn disclosure(ink: &mut Ink, expanded: bool) {
    let mut trace = Trace::new();
    if expanded {
        trace.polyline(&[(4.5, 6.25), (8.0, 9.75), (11.5, 6.25)]);
    } else {
        trace.polyline(&[(6.25, 4.5), (9.75, 8.0), (6.25, 11.5)]);
    }
    ink.stroke(trace, DISCLOSURE);
}

/// A `+` centered on `center` with arms of `reach`.
fn plus(ink: &mut Ink, (x, y): (f64, f64), reach: f64) {
    let mut trace = Trace::new();
    trace.polyline(&[(x - reach, y), (x + reach, y)]);
    trace.polyline(&[(x, y - reach), (x, y + reach)]);
    ink.stroke(trace, PLUS);
}

/// `rectangle.stack.badge.plus`: a card with a second card's edge behind it, and a solid badge with a knocked-out
/// `+` over its top right corner.
fn new_workspace(ink: &mut Ink) {
    let mut cards = Trace::new();
    cards.rounded_rect((1.0 + REGULAR / 2.0, 5.5, 10.5, 8.5), 2.0);
    cards.polyline(&[(3.5, 3.25), (9.5, 3.25)]);
    ink.stroke(cards, REGULAR);
    let badge = (12.0, 4.0, 3.75);
    let mut halo = Trace::new();
    halo.arc((badge.0, badge.1, badge.2 + 1.0), 0.0, 2.0 * PI);
    ink.clear_fill(halo);
    let mut disc = Trace::new();
    disc.arc(badge, 0.0, 2.0 * PI);
    ink.fill(disc);
    let mut cross = Trace::new();
    cross.polyline(&[(badge.0 - 2.0, badge.1), (badge.0 + 2.0, badge.1)]);
    cross.polyline(&[(badge.0, badge.1 - 2.0), (badge.0, badge.1 + 2.0)]);
    ink.clear_stroke(cross, REGULAR, None);
}

/// `plus.rectangle`: the frame with a `+` inside.
fn add_session(ink: &mut Ink) {
    frame(ink, false);
    let mut cross = Trace::new();
    cross.polyline(&[(5.0, 8.0), (11.0, 8.0)]);
    cross.polyline(&[(8.0, 5.0), (8.0, 11.0)]);
    ink.stroke(cross, REGULAR);
}

/// Continues `trace` clockwise along `circle` (`(center x, center y, radius)`) from the point `from` to the point
/// `to`, both on it.
fn arc_between(trace: &mut Trace, circle: (f64, f64, f64), from: (f64, f64), to: (f64, f64)) {
    let (x, y, _) = circle;
    let angle = |(px, py): (f64, f64)| (py - y).atan2(px - x);
    trace.arc(circle, angle(from), angle(to));
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
