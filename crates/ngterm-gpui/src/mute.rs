//! The inactive-pane and backdrop mute (upstream's `paneDim`, `backdropWash` and `muteWashOpacity` in
//! `WindowContentView+Detail.swift` and `WindowContentView.swift`; ported from the GTK build's `mute.rs`): a
//! translucent wash of the background a terminal renders, so background pixels blend background to background and text
//! pixels text to background. The text fades and the background does not darken. One Appearance setting gives both
//! washes their strength, nothing at 0.
//!
//! The unfocused pane of a shown split takes the wash. So does an overlay covering that pane, in the overlay's own
//! background. A floating program's backdrop washes the whole session behind it: the session default over the page,
//! then each laid-out pane's color over that pane, painted opaque and faded once as a group. While a backdrop washes
//! the page the pane wash stays off, since the two would stack to a stronger mute on one pane than on the other.
//!
//! A wash lies right above its pane's terminal (under the lead covers), or above the pane's overlay; neither takes the
//! pointer or the keyboard, so a click still focuses the pane under it. GPUI fades each primitive on its own rather
//! than a group, so the backdrop's regions are first flattened into disjoint rectangles ([`flatten`]), each painted in
//! the color on top there: fading those one by one equals fading the stack once.

use gpui_kit::{Bounds, Hsla, Pixels, Rgba, Window, fill, point, px, size};
use ngterm_core::chrome::Rgb;
use ngterm_core::model::{BackdropWashRegion, OverlayPane, Session};

use crate::split::{Geometry, PanePlan, Panes, Rect, Slot, pane_rect};

/// The wash one page shows over its inactive pane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Muted {
    pub slot: Slot,
    /// The wash lies over the pane's overlay rather than its terminal.
    pub overlay: bool,
    pub color: Rgb,
    pub opacity: f64,
}

/// The pane wash of `session`'s page laid out by `plan` (`None` under the dashboard or off screen), at `opacity` over
/// the `theme` background: over the pane [`PanePlan::muted`] names, in the background its overlay or its terminal
/// renders.
#[must_use]
pub fn muted(session: &Session, plan: Option<&PanePlan>, opacity: f64, theme: Rgb) -> Option<Muted> {
    if opacity <= 0.0 {
        return None;
    }
    let plan = plan?;
    let slot = plan.muted()?;
    let pane = match slot {
        Slot::Primary => OverlayPane::Left,
        Slot::Split => OverlayPane::Right,
        Slot::Scratch => return None,
    };
    let overlay = plan.overlays.pane(slot).is_some();
    let hex = session.pane_wash_color_hex(pane);
    Some(Muted {
        slot,
        overlay,
        color: wash_color(hex, theme),
        opacity,
    })
}

/// The color a wash blends toward: the `#rrggbb` a surface renders, else the theme background.
#[must_use]
pub fn wash_color(hex: Option<&str>, theme: Rgb) -> Rgb {
    hex.map(str::trim)
        .filter(|hex| ngterm_core::model::is_valid_color_hex(hex))
        .and_then(Rgb::from_hex)
        .unwrap_or(theme)
}

/// A backdrop wash's regions, back to front: each over the whole page (`None`) or a pane position, in its color.
pub type Regions = Vec<(Option<Slot>, Rgb)>;

/// The backdrop wash of `session`'s page laid out by `plan`, over the `theme` background: its regions while
/// [`PanePlan::backdrop`] says a floating program washes the page, else `None`.
#[must_use]
pub fn backdrop(session: &Session, plan: Option<&PanePlan>, theme: Rgb) -> Option<Regions> {
    plan.filter(|plan| plan.backdrop())?;
    let regions = session
        .backdrop_wash_regions()
        .into_iter()
        .map(|BackdropWashRegion { pane, color_hex }| {
            (pane.map(Slot::of_overlay), wash_color(color_hex.as_deref(), theme))
        })
        .collect();
    Some(regions)
}

/// What a page's mute draws now: the pane wash, and the backdrop's regions at their opacity.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Washes {
    pub pane: Option<Muted>,
    pub backdrop: Option<(Regions, f64)>,
}

impl Washes {
    /// The pane wash over pane position `slot`'s terminal (`overlay` false) or its overlay: its color and opacity.
    #[must_use]
    pub fn wash(&self, slot: Slot, overlay: bool) -> Option<(Rgb, f64)> {
        self.pane
            .filter(|muted| muted.slot == slot && muted.overlay == overlay)
            .map(|muted| (muted.color, muted.opacity))
    }
}

/// The regions of a backdrop flattened into disjoint rectangles of a `width`×`height` page laid out by `panes` and
/// `geometry`, each in the color on top there; a region over a pane the page does not lay out is skipped.
#[must_use]
pub fn flatten(
    regions: &[(Option<Slot>, Rgb)],
    panes: Panes,
    geometry: &Geometry,
    width: i32,
    height: i32,
) -> Vec<(Rect, Rgb)> {
    let placed: Vec<(Rect, Rgb)> = regions
        .iter()
        .filter_map(|(slot, color)| {
            let rect = match slot {
                None => Some(Rect::full(width, height)),
                Some(slot) => pane_rect(panes, geometry, *slot),
            };
            rect.filter(|rect| !rect.is_empty()).map(|rect| (rect, *color))
        })
        .collect();
    let mut flat = Vec::new();
    let mut covered: Vec<Rect> = Vec::new();
    for (rect, color) in placed.into_iter().rev() {
        let mut visible = vec![rect];
        for over in &covered {
            visible = visible.into_iter().flat_map(|piece| subtract(piece, *over)).collect();
        }
        flat.extend(visible.into_iter().map(|piece| (piece, color)));
        covered.push(rect);
    }
    flat
}

/// `rect` without what `cut` covers: up to four disjoint pieces.
fn subtract(rect: Rect, cut: Rect) -> Vec<Rect> {
    let (left, top) = (rect.x.max(cut.x), rect.y.max(cut.y));
    let (right, bottom) = (
        (rect.x + rect.width).min(cut.x + cut.width),
        (rect.y + rect.height).min(cut.y + cut.height),
    );
    if left >= right || top >= bottom {
        return vec![rect];
    }
    let pieces = [
        // above and below the cut, full width
        Rect {
            height: top - rect.y,
            ..rect
        },
        Rect {
            y: bottom,
            height: rect.y + rect.height - bottom,
            ..rect
        },
        // left and right of it, between those
        Rect {
            y: top,
            width: left - rect.x,
            height: bottom - top,
            ..rect
        },
        Rect {
            x: right,
            y: top,
            width: rect.x + rect.width - right,
            height: bottom - top,
        },
    ];
    pieces.into_iter().filter(|piece| !piece.is_empty()).collect()
}

/// A color at `alpha` as GPUI paints it.
#[must_use]
pub fn color(rgb: Rgb, alpha: f64) -> Hsla {
    #[expect(clippy::cast_possible_truncation, reason = "an alpha within 0...1")]
    let a = alpha.clamp(0.0, 1.0) as f32;
    Rgba {
        r: f32::from(rgb.r) / 255.0,
        g: f32::from(rgb.g) / 255.0,
        b: f32::from(rgb.b) / 255.0,
        a,
    }
    .into()
}

/// Paints a backdrop's flattened rectangles inside `bounds` at `opacity`.
pub fn paint_backdrop(window: &mut Window, bounds: Bounds<Pixels>, flat: &[(Rect, Rgb)], opacity: f64) {
    for (rect, rgb) in flat {
        #[expect(clippy::cast_precision_loss, reason = "page coordinates are far below 2^24")]
        let (x, y, width, height) = (rect.x as f32, rect.y as f32, rect.width as f32, rect.height as f32);
        let quad = Bounds {
            origin: bounds.origin + point(px(x), px(y)),
            size: size(px(width), px(height)),
        };
        window.paint_quad(fill(quad, color(*rgb, opacity)));
    }
}

#[cfg(test)]
mod tests;
