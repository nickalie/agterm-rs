//! The inactive-pane and backdrop mute (upstream's `paneDim`, `backdropWash` and `muteWashOpacity` in
//! `WindowContentView+Detail.swift` and `WindowContentView.swift`): a translucent wash of the background a terminal
//! renders, so background pixels blend background to background and text pixels text to background. The text fades and
//! the background does not darken. One Appearance setting gives both washes their strength, nothing at 0.
//!
//! The unfocused pane of a shown split takes the wash. So does an overlay covering that pane, in the overlay's own
//! background. A floating program's backdrop washes the whole session behind it: the session default over the page,
//! then each laid-out pane's color over that pane, painted opaque and faded once as a group. While a backdrop washes
//! the page the pane wash stays off, since the two would stack to a stronger mute on one pane than on the other.
//!
//! Each page keeps a wash per pane position twice: one right above the terminals, under the lead covers, and one above
//! the pane overlays, under the scratch. That is where upstream's `paneDim` sits on a pane and on its overlay. Neither
//! wash takes the pointer or the keyboard, so a click still focuses the pane under it.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use agterm_core::model::{BackdropWashRegion, OverlayPane, Session};
use agterm_ghostty::Color;
use gtk::prelude::*;

use crate::split::{PanePlan, PaneStack, Slot};
use crate::status_glyph::{self, Rgb};

/// One pane's wash: a solid color at the mute opacity over the pane it is laid out on.
pub struct Wash {
    area: gtk::DrawingArea,
    color: Rc<Cell<Rgb>>,
}

impl Wash {
    /// A hidden wash, out of the pointer's and the keyboard's way.
    #[must_use]
    pub fn new() -> Self {
        let area = inert_area();
        let color = Rc::new(Cell::new((0.0, 0.0, 0.0)));
        let drawn = Rc::clone(&color);
        area.set_draw_func(move |_, cr, _, _| {
            let (red, green, blue) = drawn.get();
            cr.set_source_rgb(red, green, blue);
            if let Err(err) = cr.paint() {
                tracing::debug!(%err, "painting a pane wash failed");
            }
        });
        Self { area, color }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Shows the wash in `color` at `opacity`, or hides it (`None`).
    pub fn show(&self, look: Option<(Rgb, f64)>) {
        let Some((color, opacity)) = look else {
            self.area.set_visible(false);
            return;
        };
        if self.color.replace(color) != color {
            self.area.queue_draw();
        }
        self.area.set_opacity(opacity);
        self.area.set_visible(true);
    }

    /// Whether the wash shows, and in what color at what opacity.
    #[cfg(test)]
    #[must_use]
    pub fn look(&self) -> Option<(Rgb, f64)> {
        self.area.get_visible().then(|| (self.color.get(), self.area.opacity()))
    }
}

/// A page's pane washes: over each pane position's terminal and over its overlay.
pub struct PaneWashes {
    pub primary: Wash,
    pub split: Wash,
    pub primary_overlay: Wash,
    pub split_overlay: Wash,
}

impl PaneWashes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            primary: Wash::new(),
            split: Wash::new(),
            primary_overlay: Wash::new(),
            split_overlay: Wash::new(),
        }
    }

    /// Each wash's widget and the pane position it lies over.
    #[must_use]
    pub fn placed(&self) -> [(gtk::Widget, Slot); 4] {
        [
            (self.primary.widget().clone().upcast(), Slot::Primary),
            (self.split.widget().clone().upcast(), Slot::Split),
            (self.primary_overlay.widget().clone().upcast(), Slot::Primary),
            (self.split_overlay.widget().clone().upcast(), Slot::Split),
        ]
    }

    /// The wash over pane position `slot`'s terminal, or over its overlay.
    #[must_use]
    pub fn wash(&self, slot: Slot, overlay: bool) -> Option<&Wash> {
        match (slot, overlay) {
            (Slot::Primary, false) => Some(&self.primary),
            (Slot::Split, false) => Some(&self.split),
            (Slot::Primary, true) => Some(&self.primary_overlay),
            (Slot::Split, true) => Some(&self.split_overlay),
            (Slot::Scratch, _) => None,
        }
    }

    /// Shows the wash `muted` says and hides the others: the one over pane position `slot`, over its overlay when one
    /// covers the pane, in `color` at `opacity`.
    pub fn show(&self, muted: Option<Muted>) {
        for slot in [Slot::Primary, Slot::Split] {
            for overlay in [false, true] {
                let look = muted
                    .filter(|muted| muted.slot == slot && muted.overlay == overlay)
                    .map(|muted| (muted.color, muted.opacity));
                if let Some(wash) = self.wash(slot, overlay) {
                    wash.show(look);
                }
            }
        }
    }
}

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
pub fn muted(session: &Session, plan: Option<&PanePlan>, opacity: f64, theme: Color) -> Option<Muted> {
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
fn wash_color(hex: Option<&str>, theme: Color) -> Rgb {
    hex.and_then(status_glyph::parse_hex).unwrap_or_else(|| {
        (
            f64::from(theme.r) / 255.0,
            f64::from(theme.g) / 255.0,
            f64::from(theme.b) / 255.0,
        )
    })
}

/// A backdrop wash's regions, back to front: each over the whole page (`None`) or a pane position, in its color.
pub type Regions = Vec<(Option<Slot>, Rgb)>;

/// A floating program's backdrop wash: its regions, back to front, each over the whole page (`None`) or a pane
/// position, painted opaque where the page lays that pane out now; the widget's opacity fades them once as a group.
pub struct BackdropWash {
    area: gtk::DrawingArea,
    regions: Rc<RefCell<Regions>>,
}

impl BackdropWash {
    /// A hidden wash over `stack`'s page, which places the pane regions.
    #[must_use]
    pub fn new(stack: &PaneStack) -> Self {
        let area = inert_area();
        area.set_hexpand(true);
        area.set_vexpand(true);
        let regions: Rc<RefCell<Regions>> = Rc::default();
        let (drawn, weak) = (Rc::clone(&regions), stack.downgrade());
        area.set_draw_func(move |_, cr, width, height| {
            let Some(stack) = weak.upgrade() else { return };
            for (slot, (red, green, blue)) in drawn.borrow().iter() {
                let rect = match slot {
                    None => Some(crate::split::Rect::full(width, height)),
                    Some(slot) => stack.pane_rect(*slot),
                };
                let Some(rect) = rect else { continue };
                cr.set_source_rgb(*red, *green, *blue);
                cr.rectangle(
                    f64::from(rect.x),
                    f64::from(rect.y),
                    f64::from(rect.width),
                    f64::from(rect.height),
                );
                if let Err(err) = cr.fill() {
                    tracing::debug!(%err, "painting a backdrop wash failed");
                }
            }
        });
        Self { area, regions }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Shows `regions` at `opacity`, or hides the wash (`None`, or nothing to fade).
    pub fn show(&self, look: Option<(Regions, f64)>) {
        let Some((regions, opacity)) = look.filter(|(_, opacity)| *opacity > 0.0) else {
            self.area.set_visible(false);
            return;
        };
        if *self.regions.borrow() != regions {
            *self.regions.borrow_mut() = regions;
            self.area.queue_draw();
        }
        self.area.set_opacity(opacity);
        self.area.set_visible(true);
    }

    /// Whether the wash shows, and its regions at what opacity.
    #[cfg(test)]
    #[must_use]
    pub fn look(&self) -> Option<(Regions, f64)> {
        self.area
            .get_visible()
            .then(|| (self.regions.borrow().clone(), self.area.opacity()))
    }
}

/// The backdrop wash of `session`'s page laid out by `plan`, over the `theme` background: its regions while
/// [`PanePlan::backdrop`] says a floating program washes the page, else `None`.
#[must_use]
pub fn backdrop(session: &Session, plan: Option<&PanePlan>, theme: Color) -> Option<Regions> {
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

/// A drawing area that never takes the pointer or the keyboard, hidden.
fn inert_area() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_can_target(false);
    area.set_can_focus(false);
    area.set_focusable(false);
    area.set_visible(false);
    area
}

#[cfg(test)]
mod tests {
    use agterm_core::model::{BackgroundWatermark, OverlayPane, WatermarkKind};

    use super::*;
    use crate::split::tests::{pane_overlay, program, session, split};

    const THEME: Color = Color {
        r: 0x10,
        g: 0x20,
        b: 0x30,
    };
    const THEME_RGB: Rgb = (16.0 / 255.0, 32.0 / 255.0, 48.0 / 255.0);

    fn color(hex: &str) -> BackgroundWatermark {
        BackgroundWatermark {
            kind: WatermarkKind::Color,
            image_path: None,
            text: None,
            color_hex: Some(hex.to_owned()),
            opacity: None,
            fit: None,
            position: None,
            repeats: None,
        }
    }

    fn rgb(red: u8, green: u8, blue: u8) -> Rgb {
        (
            f64::from(red) / 255.0,
            f64::from(green) / 255.0,
            f64::from(blue) / 255.0,
        )
    }

    fn muted_of(session: &Session, opacity: f64) -> Option<Muted> {
        muted(session, Some(&PanePlan::layout(session, None)), opacity, THEME)
    }

    #[test]
    fn the_pane_wash_blends_toward_the_background_the_pane_renders() {
        let mut shown = session(Some(split(true, true)), false);
        assert_eq!(
            muted_of(&shown, 0.4),
            Some(Muted {
                slot: Slot::Primary,
                overlay: false,
                color: THEME_RGB,
                opacity: 0.4,
            }),
            "the theme background without a solid color"
        );
        shown.background_watermark = Some(color("#201414"));
        assert_eq!(
            muted_of(&shown, 0.4).map(|muted| muted.color),
            Some(rgb(0x20, 0x14, 0x14))
        );
        shown.pane_backgrounds.left = Some(color("#0a0b0c"));
        assert_eq!(
            muted_of(&shown, 0.4).map(|muted| muted.color),
            Some(rgb(0x0a, 0x0b, 0x0c)),
            "a pane override wins over the session default"
        );
        assert_eq!(muted_of(&shown, 0.0), None, "strength 0 renders nothing");
        assert_eq!(
            muted(&shown, None, 0.4, THEME),
            None,
            "nothing off screen or under the dashboard"
        );
    }

    #[test]
    fn an_overlay_wash_blends_toward_the_overlays_own_background() {
        let mut overlaid = session(Some(split(true, true)), false);
        overlaid.pane_backgrounds.left = Some(color("#201414"));
        pane_overlay(&mut overlaid, OverlayPane::Left, 1);
        let wash = muted_of(&overlaid, 0.4).expect("a wash");
        assert!(wash.overlay && wash.slot == Slot::Primary);
        assert_eq!(wash.color, THEME_RGB, "the overlay never inherits the pane's color");
        let set = |hex: &str, session: &mut Session| {
            if let Some(overlay) = session.pane_overlays.slot_mut(OverlayPane::Left).overlay.as_mut() {
                overlay.background_color = Some(hex.to_owned());
            }
        };
        set("#0a0b0c", &mut overlaid);
        assert_eq!(
            muted_of(&overlaid, 0.4).map(|muted| muted.color),
            Some(rgb(0x0a, 0x0b, 0x0c))
        );
        set("teal", &mut overlaid);
        assert_eq!(
            muted_of(&overlaid, 0.4).map(|muted| muted.color),
            Some(THEME_RGB),
            "a color the renderer refuses leaves the theme"
        );
    }

    #[test]
    fn the_backdrop_washes_each_laid_out_pane_over_the_session_default() {
        let mut floating = session(Some(split(true, true)), false);
        floating.background_watermark = Some(color("#201414"));
        floating.pane_backgrounds.right = Some(color("#0a0b0c"));
        assert_eq!(
            backdrop(&floating, Some(&PanePlan::layout(&floating, None)), THEME),
            None,
            "no program, no backdrop"
        );
        program(&mut floating, Some(60));
        let plan = PanePlan::layout(&floating, None);
        assert_eq!(
            backdrop(&floating, Some(&plan), THEME),
            Some(vec![
                (None, rgb(0x20, 0x14, 0x14)),
                (Some(Slot::Primary), rgb(0x20, 0x14, 0x14)),
                (Some(Slot::Split), rgb(0x0a, 0x0b, 0x0c)),
            ])
        );
        assert_eq!(backdrop(&floating, None, THEME), None, "nothing off screen");
    }
}
