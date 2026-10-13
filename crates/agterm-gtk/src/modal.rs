//! The frame the native picker and the ask dialogs draw in: a dimming backdrop and one panel placed over it
//! (upstream's `CommandPalette` scrim and panel in `Palette.swift`, `AskDialogView`'s dim layer and centered panel).
//!
//! [`ModalFrame`] is a container widget laying its two children out by a [`PanelRule`]: a palette's panel sits near the
//! top of the frame over the whole-frame scrim, an ask's panel is centered in its anchor (the frame itself, or a
//! rectangle a resolver reports, followed frame by frame) over a dim layer covering that anchor. The geometry is a pure
//! function of the sizes ([`ask_panel_width`], [`ask_panel_rect`], [`palette_rect`]), testable without a display.
//!
//! The shared look of the panels lives here too: one CSS provider with the dim layer, the native panel (a popover's
//! backing, upstream's material panel) and the picker's rows.

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gdk, glib};

use crate::split::Rect;

/// The dim layer's style class.
pub const DIM_CLASS: &str = "agterm-modal-dim";
/// The native (rounded, popover-backed) panel's style class.
pub const PANEL_CLASS: &str = "agterm-modal-panel";
/// A palette row's badge (`custom`, `current`): a small capsule.
pub const BADGE_CLASS: &str = "agterm-palette-badge";

const CSS: &str = "
.agterm-modal-dim { background-color: alpha(black, 0.2); }
.agterm-modal-panel {
  background-color: @popover_bg_color;
  color: @popover_fg_color;
  border-radius: 12px;
  box-shadow: 0 0 0 1px alpha(white, 0.1), 0 4px 24px alpha(black, 0.4);
}
.agterm-modal-panel button.agterm-ask-destructive { color: @destructive_color; }
.agterm-pick-list { background: transparent; }
.agterm-pick-list > row { border-radius: 0; }
.agterm-pick-list > row:selected { background-color: alpha(@accent_bg_color, 0.25); color: inherit; }
.agterm-palette-badge { background-color: alpha(@window_fg_color, 0.15); border-radius: 999px; padding: 2px 6px; }
.agterm-search-bar { border-radius: 8px; padding: 6px 10px; }
";

thread_local! {
    static CSS_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs the panels' shared look on the default display, once.
pub fn install_css() {
    if CSS_INSTALLED.with(std::cell::Cell::get) {
        return;
    }
    let Some(display) = gdk::Display::default() else { return };
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    CSS_INSTALLED.with(|installed| installed.set(true));
}

// MARK: - geometry

/// Clearance kept between a panel edge and the window edge (upstream's `InterfaceMetrics.panelMargin`).
const PANEL_MARGIN: f64 = 16.0;
/// The width a palette prefers not to shrink below (upstream's `minimumPanelWidth`).
const MINIMUM_PANEL_WIDTH: f64 = 280.0;
/// The height below which a palette stops shrinking (upstream's `minimumPanelHeight`).
const MINIMUM_PANEL_HEIGHT: f64 = 160.0;
/// The palette's width at the default interface size (upstream's `panelWidthAtDefaultFontSize`).
const PALETTE_WIDTH: f64 = 520.0;
/// How far down the frame the palette starts (upstream's `topInsetFraction`).
const PALETTE_TOP_FRACTION: f64 = 0.12;
/// A content-sized ask's widest panel, in terminal cells (upstream's `72 * terminalCell`).
const ASK_MAX_CELLS: f64 = 72.0;

/// How a [`ModalFrame`] places its panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelRule {
    /// An ask's panel, centered in its anchor over a dim layer covering the anchor (upstream's `AskDialogView`).
    Ask {
        /// The caller's width in percent of the anchor; `None` sizes the panel to its content.
        width_percent: Option<i64>,
        /// The terminal cell the content-sized cap is counted in.
        terminal_cell: f64,
        /// The dialog's spacing unit, kept clear above and below the panel.
        cell: f64,
    },
    /// A palette's panel near the top of the frame, centered over the terminal area `inset` points right of the
    /// frame's left edge (the sidebar), over a scrim covering the whole frame.
    Palette { inset: f64 },
}

/// A content-sized or fixed-percent ask panel's width in an anchor `anchor_width` wide (upstream's `panelWidth` and its
/// `ViewThatFits`): a percent of the anchor, else the content's `natural` width up to the cap (90% of the anchor or 72
/// terminal cells, whichever is less), never below the content's `minimum`.
#[must_use]
pub fn ask_panel_width(
    width_percent: Option<i64>,
    anchor_width: f64,
    natural: f64,
    minimum: f64,
    terminal_cell: f64,
) -> f64 {
    let anchor_width = anchor_width.max(0.0);
    let width = match width_percent {
        // a percent within 10...100, so the conversion is exact
        #[expect(clippy::cast_precision_loss, reason = "a percent within 10...100")]
        Some(percent) => anchor_width * percent.clamp(10, 100) as f64 / 100.0,
        None => natural.min((anchor_width * 0.9).min(ASK_MAX_CELLS * terminal_cell)),
    };
    width.max(minimum)
}

/// An ask panel `width` wide whose content wants `natural_height`, centered in `anchor` and kept a `cell` clear of its
/// top and bottom (upstream's `panelHeight`), never below the content's `minimum_height`.
#[must_use]
pub fn ask_panel_rect(anchor: Rect, width: f64, natural_height: f64, minimum_height: f64, cell: f64) -> Rect {
    let room = (f64::from(anchor.height) - 2.0 * cell).max(0.0);
    let height = natural_height.min(room).max(minimum_height);
    Rect {
        x: anchor.x + pixels((f64::from(anchor.width) - width) / 2.0),
        y: anchor.y + pixels((f64::from(anchor.height) - height) / 2.0),
        width: pixels(width),
        height: pixels(height),
    }
}

/// The palette's width in a frame `width` wide with the terminal area `inset` from its left (upstream's
/// `fittedPanelWidth`): the ideal width, shrunk to what the terminal area leaves, never past the frame.
#[must_use]
pub fn palette_width(width: f64, inset: f64) -> f64 {
    let ceiling = (width - 2.0 * PANEL_MARGIN).max(0.0);
    let fits_terminal_area = width - inset - 2.0 * PANEL_MARGIN;
    PALETTE_WIDTH
        .min(MINIMUM_PANEL_WIDTH.max(fits_terminal_area))
        .min(ceiling)
}

/// How far right of the frame's center a palette `panel` wide sits, so it centers over the terminal area without
/// leaving the frame (upstream's `panelOffset`).
#[must_use]
pub fn palette_offset(panel: f64, width: f64, inset: f64) -> f64 {
    (inset / 2.0).min((width - panel) / 2.0).max(0.0)
}

/// The palette panel's rectangle in a `width`×`height` frame, for content wanting `natural_height` at that width.
#[must_use]
pub fn palette_rect(width: f64, height: f64, inset: f64, natural_height: f64, minimum_height: f64) -> Rect {
    let panel = palette_width(width, inset);
    let cap = MINIMUM_PANEL_HEIGHT.max(height * (1.0 - PALETTE_TOP_FRACTION) - PANEL_MARGIN);
    Rect {
        x: pixels((width - panel) / 2.0 + palette_offset(panel, width, inset)),
        y: pixels(height * PALETTE_TOP_FRACTION),
        width: pixels(panel),
        height: pixels(natural_height.min(cap).max(minimum_height)),
    }
}

/// `value` rounded to whole pixels, saturating.
#[must_use]
pub fn pixels(value: f64) -> i32 {
    let value = value.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX));
    #[expect(clippy::cast_possible_truncation, reason = "rounded and clamped into i32")]
    let pixels = value as i32;
    pixels
}

// MARK: - the frame widget

/// Resolves an ask's anchor in the frame's coordinates at layout time; `None` while it is not on screen.
pub type AnchorFn = Box<dyn Fn(&gtk::Widget) -> Option<Rect>>;

/// What [`ModalFrame`] lays out.
#[derive(Default)]
struct FrameLayout {
    backdrop: Option<gtk::Widget>,
    panel: Option<gtk::Widget>,
    rule: Option<PanelRule>,
    /// The anchor resolver; `None` anchors to the whole frame.
    anchor: Option<AnchorFn>,
    /// The anchor the last layout used, which the frame clock compares against.
    placed: Option<Rect>,
}

impl FrameLayout {
    fn anchor(&self, frame: &gtk::Widget, width: i32, height: i32) -> Option<Rect> {
        match &self.anchor {
            Some(resolve) => resolve(frame),
            None => Some(Rect::full(width, height)),
        }
    }

    fn allocate(&mut self, frame: &gtk::Widget, width: i32, height: i32) {
        let anchor = self.anchor(frame, width, height);
        self.placed = anchor;
        let (Some(backdrop), Some(panel), Some(rule)) = (&self.backdrop, &self.panel, self.rule) else {
            return;
        };
        let (dim, placed) = match anchor {
            Some(anchor) => match rule {
                PanelRule::Palette { inset } => (
                    Rect::full(width, height),
                    place_palette(panel, f64::from(width), f64::from(height), inset),
                ),
                PanelRule::Ask { .. } => (anchor, place_ask(panel, anchor, rule)),
            },
            None => (Rect::default(), Rect::default()),
        };
        allocate(backdrop, dim);
        allocate(panel, placed);
    }
}

fn allocate(widget: &gtk::Widget, rect: Rect) {
    widget.size_allocate(&gtk::Allocation::new(rect.x, rect.y, rect.width, rect.height), -1);
}

/// The panel's minimum and natural height at `width`.
fn heights(panel: &gtk::Widget, width: i32) -> (f64, f64) {
    let (minimum, natural, _, _) = panel.measure(gtk::Orientation::Vertical, width);
    (f64::from(minimum), f64::from(natural))
}

fn place_palette(panel: &gtk::Widget, width: f64, height: f64, inset: f64) -> Rect {
    let panel_width = pixels(palette_width(width, inset));
    let (minimum, natural) = heights(panel, panel_width);
    palette_rect(width, height, inset, natural, minimum)
}

fn place_ask(panel: &gtk::Widget, anchor: Rect, rule: PanelRule) -> Rect {
    let PanelRule::Ask {
        width_percent,
        terminal_cell,
        cell,
    } = rule
    else {
        return Rect::default();
    };
    let (minimum, natural, _, _) = panel.measure(gtk::Orientation::Horizontal, -1);
    let width = ask_panel_width(
        width_percent,
        f64::from(anchor.width),
        f64::from(natural),
        f64::from(minimum),
        terminal_cell,
    );
    let (minimum_height, natural_height) = heights(panel, pixels(width));
    ask_panel_rect(anchor, width, natural_height, minimum_height, cell)
}

mod imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    use super::FrameLayout;

    #[derive(Default)]
    pub struct ModalFrame {
        pub(super) layout: RefCell<FrameLayout>,
        pub(super) tick: RefCell<Option<gtk::TickCallbackId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ModalFrame {
        const NAME: &'static str = "AgtermModalFrame";
        type Type = super::ModalFrame;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ModalFrame {
        fn dispose(&self) {
            if let Some(tick) = self.tick.borrow_mut().take() {
                tick.remove();
            }
            let widget = self.obj();
            while let Some(child) = widget.first_child() {
                child.unparent();
            }
            let mut layout = self.layout.borrow_mut();
            layout.backdrop = None;
            layout.panel = None;
            layout.anchor = None;
        }
    }

    impl WidgetImpl for ModalFrame {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (0, 0, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            self.layout
                .borrow_mut()
                .allocate(self.obj().upcast_ref(), width, height);
        }
    }
}

glib::wrapper! {
    /// A backdrop and a panel laid out by a [`PanelRule`] (module docs).
    pub struct ModalFrame(ObjectSubclass<imp::ModalFrame>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ModalFrame {
    /// A frame over `backdrop` showing `panel` by `rule`; it fills whatever its parent gives it.
    #[must_use]
    pub fn new(backdrop: &impl IsA<gtk::Widget>, panel: &impl IsA<gtk::Widget>, rule: PanelRule) -> Self {
        install_css();
        let frame: Self = glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .build();
        frame.set_overflow(gtk::Overflow::Hidden);
        backdrop.set_parent(&frame);
        panel.set_parent(&frame);
        {
            let mut layout = frame.imp().layout.borrow_mut();
            layout.backdrop = Some(backdrop.clone().upcast());
            layout.panel = Some(panel.clone().upcast());
            layout.rule = Some(rule);
        }
        frame
    }

    /// Changes how the panel is placed.
    pub fn set_rule(&self, rule: PanelRule) {
        let mut layout = self.imp().layout.borrow_mut();
        if layout.rule != Some(rule) {
            layout.rule = Some(rule);
            drop(layout);
            self.queue_allocate();
        }
    }

    /// Anchors the panel to what `anchor` resolves at layout time, re-laid out on any frame the anchor moved (the
    /// anchored area is laid out elsewhere, so nothing else tells the frame); `None` anchors to the whole frame.
    pub fn set_anchor(&self, anchor: Option<AnchorFn>) {
        if let Some(tick) = self.imp().tick.borrow_mut().take() {
            tick.remove();
        }
        let tracking = anchor.is_some();
        self.imp().layout.borrow_mut().anchor = anchor;
        if tracking {
            let tick = self.add_tick_callback(|frame, _| {
                let moved = frame.imp().layout.try_borrow().is_ok_and(|layout| {
                    layout.anchor(frame.upcast_ref(), frame.width(), frame.height()) != layout.placed
                });
                if moved {
                    frame.queue_allocate();
                }
                glib::ControlFlow::Continue
            });
            *self.imp().tick.borrow_mut() = Some(tick);
        }
        self.queue_allocate();
    }
}

/// The rectangle of `widget` in `frame`'s coordinates while it is on screen.
#[must_use]
pub fn bounds_in(widget: &gtk::Widget, frame: &gtk::Widget) -> Option<Rect> {
    if !widget.is_mapped() {
        return None;
    }
    let bounds = widget.compute_bounds(frame)?;
    Some(Rect {
        x: pixels(f64::from(bounds.x())),
        y: pixels(f64::from(bounds.y())),
        width: pixels(f64::from(bounds.width())),
        height: pixels(f64::from(bounds.height())),
    })
}

/// Keeps `child` scrolled into `scroller`'s view (upstream's `scrollTo` with no anchor: nothing moves while the child
/// is visible, an edge reveals the minimum). A child not laid out yet is revealed once it is.
pub fn reveal(scroller: &gtk::ScrolledWindow, child: &gtk::Widget) {
    let Some(content) = scroller.child() else { return };
    if child.height() == 0 {
        let (scroller, child) = (scroller.downgrade(), child.downgrade());
        content.add_tick_callback(move |_, _| {
            if let (Some(scroller), Some(child)) = (scroller.upgrade(), child.upgrade())
                && child.height() > 0
            {
                reveal(&scroller, &child);
            }
            glib::ControlFlow::Break
        });
        return;
    }
    // a viewport draws its child scrolled, so the child's own coordinates are the adjustment's
    let shown = content
        .downcast_ref::<gtk::Viewport>()
        .and_then(gtk::Viewport::child)
        .unwrap_or(content);
    let Some(bounds) = child.compute_bounds(&shown) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    let (top, bottom) = (f64::from(bounds.y()), f64::from(bounds.y() + bounds.height()));
    let (value, page) = (adjustment.value(), adjustment.page_size());
    if top < value {
        adjustment.set_value(top);
    } else if bottom > value + page {
        adjustment.set_value(bottom - page);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: f64, expected: f64) -> bool {
        (actual - expected).abs() < 1e-9
    }

    fn anchor(width: i32, height: i32) -> Rect {
        Rect {
            x: 10,
            y: 20,
            width,
            height,
        }
    }

    #[test]
    fn a_content_sized_ask_takes_its_natural_width_up_to_the_cap() {
        // 90% of a 400-wide anchor is 360, under 72 cells of 8
        assert!(close(ask_panel_width(None, 400.0, 200.0, 50.0, 8.0), 200.0));
        assert!(close(ask_panel_width(None, 400.0, 900.0, 50.0, 8.0), 360.0));
        // 72 cells of 8 = 576, under 90% of 2000
        assert!(close(ask_panel_width(None, 2000.0, 900.0, 50.0, 8.0), 576.0));
        assert!(
            close(ask_panel_width(None, 100.0, 900.0, 150.0, 8.0), 150.0),
            "never under its minimum"
        );
    }

    #[test]
    fn a_fixed_width_ask_is_a_clamped_percent_of_its_anchor() {
        assert!(close(ask_panel_width(Some(50), 800.0, 100.0, 10.0, 8.0), 400.0));
        assert!(close(ask_panel_width(Some(5), 800.0, 100.0, 10.0, 8.0), 80.0));
        assert!(close(ask_panel_width(Some(150), 800.0, 100.0, 10.0, 8.0), 800.0));
    }

    #[test]
    fn an_ask_panel_centers_in_its_anchor_a_cell_clear_of_its_edges() {
        let rect = ask_panel_rect(anchor(400, 300), 200.0, 100.0, 20.0, 8.0);
        assert_eq!(
            rect,
            Rect {
                x: 110,
                y: 120,
                width: 200,
                height: 100
            }
        );
        let tall = ask_panel_rect(anchor(400, 300), 200.0, 900.0, 20.0, 8.0);
        assert_eq!(
            (tall.y, tall.height),
            (28, 284),
            "capped to the anchor less a cell each side"
        );
    }

    #[test]
    fn the_palette_fits_the_terminal_area_and_centers_over_it() {
        assert!(close(palette_width(1200.0, 0.0), 520.0));
        // the sidebar leaves 1000 - 221 - 32 = 747: the ideal fits
        assert!(close(palette_width(1000.0, 221.0), 520.0));
        // a narrow terminal area shrinks it, not under 280
        assert!(close(palette_width(700.0, 221.0), 447.0));
        assert!(close(palette_width(500.0, 300.0), 280.0));
        // never past the frame less its margins
        assert!(close(palette_width(250.0, 0.0), 218.0));
        assert!(close(palette_offset(520.0, 1000.0, 221.0), 110.5));
        assert!(
            close(palette_offset(520.0, 600.0, 221.0), 40.0),
            "kept inside the frame"
        );
        let rect = palette_rect(1000.0, 600.0, 0.0, 300.0, 50.0);
        assert_eq!(
            rect,
            Rect {
                x: 240,
                y: 72,
                width: 520,
                height: 300
            }
        );
        let short = palette_rect(1000.0, 100.0, 0.0, 300.0, 50.0);
        assert_eq!(short.height, 160, "the minimum cap");
    }
}
