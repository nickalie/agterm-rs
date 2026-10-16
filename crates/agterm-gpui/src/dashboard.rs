//! The dashboard grid (upstream's `Views/DashboardView.swift` and `Views/WindowContentView+Dashboard.swift`; port of
//! the GTK build's `dashboard.rs`): while a window's dashboard is open its deck shows every member pane's own terminal
//! at that pane's grid cell, view-only, on an opaque backdrop in the terminal's background, each cell framed, named by
//! a chip on its bottom edge and the highlighted one ringed.
//!
//! A cell hosts the member's own terminal view (the deck page's owner view, never a second rendering): the page stops
//! drawing it while the grid is open, and laid out in its cell it resizes its terminal to the cell, as upstream's
//! reparented surface does ("view-only promises no input, not no process resize"). The terminals take no input and no
//! focus: a transparent layer over each cell takes the clicks and the grid's own focus holds the keyboard, which the
//! arrows walk the highlight with, Enter enters, Esc closes, and every other key is swallowed so nothing reaches a
//! terminal behind (the keymap's chords still run first, as upstream's menu key equivalents do). A click rings the
//! clicked cell at once, then enters it after [`CLICK_ENTER_DELAY`] unless another click or key moved the highlight
//! meanwhile.
//!
//! A fixed or auto font mode gives every member terminal a transient font size while it is in the grid, put back when
//! it leaves ([`FontOverrides`]); its size reports are not the pane's meanwhile. A member pane under an overlay shows
//! the overlay's cover over its cell (upstream's `DashboardOverlayCover`). The chip carries the session's agent status
//! (upstream's `DashboardCaptionPill`): the status color with contrasting text, pulsing while the status blinks.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use agterm_core::Core;
use agterm_core::chrome::Rgb;
use agterm_core::dashboard::{DashboardController, DashboardCover, DashboardMember, Direction};
use agterm_core::model::{AppStore, OverlayPane, Session, SplitAxis};
use agterm_core::persist::AppSettings;
use agterm_core::zoom::ZoomSurface;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Animation, AnimationExt as _, AnyElement, Hsla, InteractiveElement as _, IntoElement, Keystroke, MouseButton,
    ParentElement as _, SharedString, Styled as _, div, px,
};
use uuid::Uuid;

use crate::state::{AppState, Poster, post};
use crate::terminal::Shared;

/// The gap between cells and around the grid (upstream's `gridSpacing`).
pub const GAP: f32 = 12.0;
/// A cell's corner radius (upstream's `cellCornerRadius`).
pub const CELL_RADIUS: f32 = 6.0;
/// The highlighted cell's ring (upstream's `highlightLineWidth`).
pub const RING: f32 = 1.5;
/// How far a name chip hangs below its cell's bottom edge, so it straddles the frame line (upstream's
/// `captionBottomOffset`).
pub const CAPTION_DROP: f32 = 8.0;
/// The chip's inset from its cell's right edge.
pub const CAPTION_INSET: f32 = 6.0;
/// An unselected idle chip's text opacity (upstream's `unselectedCaptionTextOpacity`).
pub const UNSELECTED_CAPTION: f64 = 0.55;
/// An idle chip's fill: the foreground over the backdrop at this opacity (the theme carries no selection color here).
pub const IDLE_FILL: f64 = 0.16;
/// The pulsing wash's peak and half period (upstream's `washPeakOpacity`, `pulseDuration`).
pub const WASH_PEAK: f32 = 0.75;
pub const PULSE_HALF: Duration = Duration::from_millis(450);
/// How long a clicked cell's ring flashes before the click enters it (upstream's `dashboardClickEnterDelay`).
pub const CLICK_ENTER_DELAY: Duration = Duration::from_millis(180);

/// The pane marker after a cell's name: which pane of a split session it hosts, by the split's axis; nothing for a
/// session without a split.
#[must_use]
pub fn pane_marker(member: DashboardMember, session: &Session) -> &'static str {
    let top_bottom = session.split.axis == SplitAxis::TopBottom;
    match (member.surface, top_bottom) {
        (ZoomSurface::Split, true) => " ▼",
        (ZoomSurface::Split, false) => " ▶",
        _ if !session.split.exists => "",
        (_, true) => " ▲",
        (_, false) => " ◀",
    }
}

/// The grid's rows of `count` cells: each row's member indexes, `None` for the clear fillers that keep a ragged last
/// row's cells the size of the full rows above (upstream's `cellSlot`).
#[must_use]
pub fn rows(count: usize) -> Vec<Vec<Option<usize>>> {
    let count = count.min(agterm_core::dashboard::MAX_CELLS);
    if count == 0 {
        return Vec::new();
    }
    let (cols, rows) = agterm_core::dashboard::grid(count);
    (0..rows)
        .map(|row| {
            (0..cols)
                .map(|col| Some(row * cols + col).filter(|index| *index < count))
                .collect()
        })
        .collect()
}

/// What the deck renders of an open dashboard.
#[derive(Debug, Clone, PartialEq)]
pub struct GridModel {
    /// The cells, row-major.
    pub members: Vec<DashboardMember>,
    pub highlighted: Option<DashboardMember>,
    /// The font size the members take, `None` to leave each at its own.
    pub font_size: Option<f64>,
}

impl GridModel {
    /// Window `window`'s open dashboard.
    #[must_use]
    pub fn of(core: &Core, window: Uuid) -> Option<Self> {
        let controller = core.dashboard(window)?;
        Some(Self {
            members: controller.members().to_vec(),
            highlighted: controller.highlighted(),
            font_size: controller.applied_font_size(),
        })
    }

    /// The token of the terminal each cell hosts, in cell order (`None` for a pane with no terminal yet).
    #[must_use]
    pub fn tokens(&self, store: Option<&AppStore>) -> Vec<Option<String>> {
        self.members
            .iter()
            .map(|member| member_token(store?, *member))
            .collect()
    }

    /// What each cell draws over its terminal, read from `store` and the settings.
    #[must_use]
    pub fn cells(&self, store: Option<&AppStore>, settings: &AppSettings) -> Vec<ChromeCell> {
        self.members
            .iter()
            .map(|member| {
                let session = store.and_then(|store| store.session(member.session));
                let caption = session
                    .map(|session| format!("{}{}", session.display_name(), pane_marker(*member, session)))
                    .unwrap_or_default();
                let pane = if member.surface == ZoomSurface::Split {
                    OverlayPane::Right
                } else {
                    OverlayPane::Left
                };
                ChromeCell {
                    member: *member,
                    caption,
                    highlighted: self.highlighted == Some(*member),
                    cover: session.and_then(|session| session.dashboard_cover(pane)),
                    status: session
                        .and_then(|session| crate::status_glyph::resolve(&session.agent_indicator, settings)),
                }
            })
            .collect()
    }
}

/// The token of the terminal `member` names in `store`.
fn member_token(store: &AppStore, member: DashboardMember) -> Option<String> {
    let session = store.session(member.session)?;
    let token = session.pane_slot(member.surface.pane()?).get()?.pane_token();
    (!token.is_empty()).then(|| token.to_owned())
}

/// What the grid draws over one cell.
#[derive(Debug, Clone, PartialEq)]
pub struct ChromeCell {
    pub member: DashboardMember,
    /// The session's name and the pane marker.
    pub caption: String,
    pub highlighted: bool,
    /// What hides the member pane's terminal in its session's own view, which the cell then names instead.
    pub cover: Option<DashboardCover>,
    /// The session's agent status the chip carries; `None` while idle.
    pub status: Option<crate::status_glyph::GlyphLook>,
}

/// A key the grid acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridKey {
    Move(Direction),
    Enter,
    Close,
}

/// The grid's reading of an unmodified key press; every other key is swallowed.
#[must_use]
pub fn grid_key(stroke: &Keystroke) -> Option<GridKey> {
    let modifiers = &stroke.modifiers;
    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
        return None;
    }
    match stroke.key.as_str() {
        "left" => Some(GridKey::Move(Direction::Left)),
        "right" => Some(GridKey::Move(Direction::Right)),
        "up" => Some(GridKey::Move(Direction::Up)),
        "down" => Some(GridKey::Move(Direction::Down)),
        "enter" | "kp_enter" => Some(GridKey::Enter),
        "escape" => Some(GridKey::Close),
        _ => None,
    }
}

// MARK: - fonts

/// One terminal's transient grid font size and the size it had before.
struct FontOverride {
    shared: Rc<Shared>,
    original: f32,
    applied: f32,
}

/// The transient font sizes the grid gives its member terminals (upstream's `dashboardFontOverride`): set while a
/// terminal sits in a cell under a fixed or auto mode, re-asserted when something else moved it (a config reload), and
/// put back to the size it had when it leaves the grid or the grid closes. A terminal not started yet gets its size on
/// a later sync. While overridden a terminal's size reports are not the pane's, nor is the revert's
/// ([`Shared::set_font_override`]).
#[derive(Default)]
pub struct FontOverrides {
    by_token: HashMap<String, FontOverride>,
}

impl FontOverrides {
    /// Gives `members` (token and terminal) the font `size` (none: their own) and puts every other overridden terminal
    /// back.
    pub fn sync(&mut self, members: &[(String, Rc<Shared>)], size: Option<f64>) {
        #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
        let size = size.map(|size| size as f32);
        let wanted: Vec<&(String, Rc<Shared>)> = match size {
            Some(_) => members.iter().filter(|(_, shared)| shared.is_realized()).collect(),
            None => Vec::new(),
        };
        self.by_token.retain(|token, entry| {
            let stays = wanted
                .iter()
                .any(|(known, shared)| known == token && Rc::ptr_eq(shared, &entry.shared));
            if !stays {
                entry.shared.clear_font_override(entry.original);
            }
            stays
        });
        let Some(size) = size else { return };
        for (token, shared) in wanted {
            let entry = self.by_token.entry(token.clone()).or_insert_with(|| FontOverride {
                shared: Rc::clone(shared),
                original: shared.font_points(),
                applied: size,
            });
            entry.applied = size;
            entry.shared.set_font_override(size);
        }
    }

    /// The tokens overridden now.
    #[must_use]
    pub fn tokens(&self) -> Vec<&str> {
        self.by_token.keys().map(String::as_str).collect()
    }

    /// The size an overridden terminal took, by token.
    #[must_use]
    pub fn applied(&self, token: &str) -> Option<f32> {
        self.by_token.get(token).map(|entry| entry.applied)
    }
}

// MARK: - drawing

/// The colors the grid draws with: the terminal's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Look {
    pub background: Rgb,
    pub foreground: Rgb,
}

fn color(rgb: Rgb, alpha: f64) -> Hsla {
    crate::chrome::color(rgb, alpha)
}

/// One cell: the member's terminal view (or nothing while it has none), its lead cover, its overlay cover, the frame,
/// the chip and the click target.
pub struct CellParts {
    pub index: usize,
    pub chrome: ChromeCell,
    pub terminal: Option<AnyElement>,
    pub lead: Option<AnyElement>,
}

/// The grid over the deck: its rows of cells on the opaque backdrop. `cells` come row-major; a click posts through
/// `poster` for window `window`; `animate` lets a blinking chip pulse.
pub fn grid(window: Uuid, cells: Vec<CellParts>, look: Look, poster: &Poster, animate: bool) -> AnyElement {
    let count = cells.len();
    let mut cells: Vec<Option<CellParts>> = cells.into_iter().map(Some).collect();
    let rows = rows(count).into_iter().map(|row| {
        let slots = row
            .into_iter()
            .map(|slot| match slot.and_then(|index| cells.get_mut(index)?.take()) {
                Some(parts) => cell(window, parts, look, poster, animate),
                None => div().flex_1().into_any_element(),
            });
        div()
            .flex()
            .flex_row()
            .flex_1()
            .min_h(px(0.))
            .gap(px(GAP))
            .children(slots)
    });
    div()
        .id("dashboard")
        .debug_selector(|| "dashboard".to_owned())
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .gap(px(GAP))
        .p(px(GAP))
        .bg(color(look.background, 1.0))
        .children(rows)
        .into_any_element()
}

fn cell(window: Uuid, parts: CellParts, look: Look, poster: &Poster, animate: bool) -> AnyElement {
    let CellParts {
        index,
        chrome,
        terminal,
        lead,
    } = parts;
    let member = chrome.member;
    let poster = poster.clone();
    let ring = if chrome.highlighted {
        color(look.foreground, 1.0)
    } else {
        color(look.foreground, 0.12)
    };
    let selector = format!("dashboard-cell-{index}");
    let covered = chrome.cover.is_some();
    div()
        .id(("dashboard-cell", index))
        .debug_selector(move || selector.clone())
        .relative()
        .flex_1()
        .min_w(px(0.))
        .min_h(px(0.))
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(px(CELL_RADIUS))
                .overflow_hidden()
                .bg(color(look.background, 1.0))
                .children(terminal.map(|terminal| div().size_full().child(terminal)))
                .when(!covered, |content| {
                    content.children(lead.map(|lead| div().absolute().inset_0().child(lead)))
                })
                .children(chrome.cover.as_ref().map(|cover| overlay_cover(cover, look))),
        )
        .child(
            // the transparent hit target above the terminal: the terminals take no click
            div()
                .absolute()
                .inset_0()
                .occlude()
                .rounded(px(CELL_RADIUS))
                .border(px(if chrome.highlighted { RING } else { 1.0 }))
                .border_color(ring)
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    clicked(&poster, window, member);
                    cx.stop_propagation();
                }),
        )
        .when(chrome.highlighted, |cell| {
            cell.child(
                div()
                    .debug_selector(|| "dashboard-highlighted".to_owned())
                    .absolute()
                    .inset_0(),
            )
        })
        .child(caption(&chrome, look, animate))
        .into_any_element()
}

/// The chip on the cell's bottom-right frame line: the name and pane marker, the status color with contrasting text
/// while the session is not idle (pulsing while it blinks), else the muted idle fill.
fn caption(chrome: &ChromeCell, look: Look, animate: bool) -> AnyElement {
    let (fill, text, wash) = if let Some(status) = &chrome.status {
        let fill = crate::status_glyph::hsla(status.rgb);
        let (text, wash) = if luminance(status.rgb) > 0.5 {
            (gpui_kit::black(), gpui_kit::white())
        } else {
            (gpui_kit::white(), gpui_kit::black())
        };
        (fill, text, (status.blink && animate).then_some(wash))
    } else {
        let alpha = if chrome.highlighted { 1.0 } else { UNSELECTED_CAPTION };
        (color(look.foreground, IDLE_FILL), color(look.foreground, alpha), None)
    };
    let label = SharedString::from(chrome.caption.clone());
    let pill = div()
        .relative()
        .max_w_full()
        .rounded_full()
        .overflow_hidden()
        // the backdrop under the fill, so a translucent fill never lets the frame line show through
        .bg(color(look.background, 1.0))
        .child(div().absolute().inset_0().bg(fill))
        .children(wash.map(pulse))
        .child(
            div()
                .relative()
                .px(px(6.))
                .py(px(1.))
                .text_size(px(11.))
                .text_color(text)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis_middle()
                .child(label),
        );
    div()
        .absolute()
        .left(px(CAPTION_INSET))
        .right(px(CAPTION_INSET))
        .bottom(px(-CAPTION_DROP))
        .flex()
        .flex_row()
        .justify_end()
        .child(pill)
        .into_any_element()
}

/// The pulsing wash over a blinking chip's fill: the fill stays opaque, the wash pulses on top.
fn pulse(wash: Hsla) -> AnyElement {
    div()
        .id("dashboard-caption-wash")
        .absolute()
        .inset_0()
        .bg(wash)
        .with_animation(
            "dashboard-caption-pulse",
            Animation::new(PULSE_HALF * 2).repeat().with_max_fps(30.0),
            |wash, delta| wash.opacity(pulse_at(delta)),
        )
        .into_any_element()
}

/// The wash's opacity `delta` of the way through a pulse: up to [`WASH_PEAK`] and back, eased.
#[must_use]
pub fn pulse_at(delta: f32) -> f32 {
    let phase = if delta < 0.5 { delta * 2.0 } else { (1.0 - delta) * 2.0 };
    let eased = 0.5 - 0.5 * (std::f32::consts::PI * phase).cos();
    WASH_PEAK * eased
}

/// A color's relative luminance (sRGB weights), which picks black or white text over it.
fn luminance((r, g, b): crate::status_glyph::Rgb) -> f64 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// The stand-in for a covered member pane (upstream's `DashboardOverlayCover`): the kind of cover, its app-derived
/// source, and a page's own title as a separate dimmed line.
fn overlay_cover(cover: &DashboardCover, look: Look) -> AnyElement {
    let (kind, source, title) = match cover {
        DashboardCover::Page { identity, title } => ("HTML overlay", Some(identity.clone()), title.clone()),
        DashboardCover::Program { command } => ("Program overlay", command.clone(), None),
    };
    let line = |text: String, id: &'static str| {
        div()
            .debug_selector(move || id.to_owned())
            .max_w_full()
            .whitespace_nowrap()
            .overflow_hidden()
            .child(text)
    };
    div()
        .absolute()
        .inset_0()
        .bg(color(look.background, 1.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .p_4()
        .text_color(color(look.foreground, 1.0))
        .child(line(kind.to_owned(), "dashboard-overlay-cover").text_size(px(13.)))
        .children(source.map(|source| {
            line(source, "dashboard-overlay-cover-source")
                .text_size(px(12.))
                .text_ellipsis_middle()
        }))
        .children(title.map(|title| {
            line(title, "dashboard-overlay-cover-title")
                .text_size(px(12.))
                .text_ellipsis()
                .text_color(color(look.foreground, UNSELECTED_CAPTION))
        }))
        .into_any_element()
}

/// A click highlights the cell at once, then enters it after the flash if it is still the highlighted one.
fn clicked(poster: &Poster, window: Uuid, member: DashboardMember) {
    post(poster, move |state: &mut AppState| {
        state.dashboard_click(window, member);
    });
    let Some(main_loop) = poster.upgrade() else {
        return;
    };
    let entering = Box::new(move |state: &mut AppState| state.dashboard_click_enter(window, member));
    if main_loop.post_after(CLICK_ENTER_DELAY, entering).is_err() {
        tracing::debug!("dashboard: the click's enter was dropped, the loop is closed");
    }
}

// MARK: - the actions

/// The grid's actions, each a main-loop task ending in a settle.
impl AppState {
    /// A key on window `window`'s grid: the arrows walk the highlight, Enter enters it, Esc closes.
    pub fn dashboard_key(&mut self, window: Uuid, key: GridKey) {
        if self.is_quitting() {
            return;
        }
        match key {
            GridKey::Move(direction) => self.core.dashboard_move(window, direction),
            GridKey::Enter => {
                let highlighted = self.core.dashboard(window).and_then(DashboardController::highlighted);
                if let Some(member) = highlighted {
                    self.core.dashboard_enter(window, member);
                }
            }
            GridKey::Close => self.core.dashboard_close(window),
        }
        self.settle();
    }

    /// A click on `member`'s cell: its ring flashes.
    pub fn dashboard_click(&mut self, window: Uuid, member: DashboardMember) {
        if self.is_quitting() {
            return;
        }
        self.core.dashboard_highlight(window, member);
        self.settle();
    }

    /// The flash of a click ran out: the cell is entered unless another click or key moved the highlight, or the grid
    /// closed.
    pub fn dashboard_click_enter(&mut self, window: Uuid, member: DashboardMember) {
        if self.is_quitting() {
            return;
        }
        let still = self
            .core
            .dashboard(window)
            .is_some_and(|grid| grid.highlighted() == Some(member));
        if still {
            self.core.dashboard_enter(window, member);
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests;
