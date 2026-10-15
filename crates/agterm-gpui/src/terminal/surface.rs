//! [`TerminalSurface`]: the core's [`Surface`] over a terminal, every method the GTK build's `PaneSurface` implements
//! (`agterm-gtk/src/pane.rs`, `surface.rs`), with libghostty's work done through `agterm-term` and core's ghostty
//! action table (`agterm_core::ghostty_config::binds`).
//!
//! No method has a GPUI context: what needs the app (the clipboard, events for the frontend) is queued on the
//! terminal's pump (module docs of [`super`]), and a binding action is accepted for a realized terminal whatever it
//! then does, as the GTK build's `binding_action` returned.

use std::rc::Rc;
use std::str::FromStr as _;

use agterm_core::ghostty_config::binds::{Action, CopyFormat, Direction, Mods, Performer, PhysicalKey};
use agterm_core::keystrokes::{self, KeystrokeSegment, SUBMIT_GAP};
use agterm_host::{Surface, TextScope};
use agterm_term::{ClipboardLocation, KeyInput, Pane, Scroll, SearchStep, SelectionText, TextRegion};

use super::{Effect, Shared, TerminalEvent};

/// A terminal as the core's surface slot holds it.
#[derive(Debug, Clone)]
pub struct TerminalSurface {
    shared: Rc<Shared>,
}

impl TerminalSurface {
    #[must_use]
    pub fn new(shared: Rc<Shared>) -> Self {
        Self { shared }
    }

    #[must_use]
    pub fn shared(&self) -> &Rc<Shared> {
        &self.shared
    }

    /// The pane while the terminal is realized.
    fn pane(&self) -> Option<Rc<Pane>> {
        self.shared.pane().filter(|_| self.shared.is_realized())
    }

    /// Performs a ghostty binding action on this terminal; whether it was performed (a `performable:` binding of an
    /// unsupported or idle action lets its key through). The keyboard's binds (Task 14) perform through this too.
    #[must_use]
    pub fn perform(&self, action: &Action) -> bool {
        Actions { shared: &self.shared }.perform(action)
    }
}

/// Sends `text` as one key press typing it (an unidentified key, as libghostty's `text_key` sent it).
fn type_run(pane: &Pane, text: &str) {
    let key = KeyInput {
        unshifted: None,
        ..KeyInput::press(PhysicalKey::UNIDENTIFIED, Mods::empty(), text)
    };
    if let Err(err) = pane.key(&key) {
        tracing::warn!(target: "agterm::surface", %err, "typing into the terminal failed");
    }
}

/// One Return keypress, press then release, through the key encoder (upstream's `sendReturn`).
fn press_return(pane: &Pane) {
    let Some(enter) = PhysicalKey::new("enter") else {
        return;
    };
    let press = KeyInput::press(enter, Mods::empty(), "");
    let release = KeyInput {
        action: agterm_core::ghostty_config::binds::KeyAction::Release,
        ..press
    };
    for key in [press, release] {
        if let Err(err) = pane.key(&key) {
            tracing::warn!(target: "agterm::surface", %err, "typing Return into the terminal failed");
        }
    }
}

impl Surface for TerminalSurface {
    fn pane_token(&self) -> &str {
        &self.shared.spec().token
    }

    fn is_realized(&self) -> bool {
        self.shared.is_realized()
    }

    /// A live pane is a client of its daemon once its terminal exists (the attach runs from the pane's start).
    fn is_live_session(&self) -> bool {
        self.shared.spec().live && self.shared.is_realized()
    }

    /// As in the GTK build: the reports of every pane already reach the session.
    fn promote_to_primary(&mut self) {}

    fn type_text(&mut self, text: &str) -> bool {
        let Some(pane) = self.pane() else {
            return false;
        };
        // a live composition goes in first, as typed, or the injected text would land before it
        if let Some(preedit) = self.shared.preedit() {
            type_run(&pane, &preedit);
            self.shared.set_preedit(None);
        }
        let paced = keystrokes::paced(text);
        for segment in &paced.head {
            match segment {
                KeystrokeSegment::Text(run) => type_run(&pane, run),
                KeystrokeSegment::Return => press_return(&pane),
            }
        }
        if paced.paced_return {
            // blocking, as the GTK build: a deferred Return could be overtaken by another injection or a key
            std::thread::sleep(SUBMIT_GAP);
            press_return(&pane);
        }
        true
    }

    fn read_text(&self, scope: TextScope) -> Option<String> {
        let region = match scope {
            TextScope::Screen => TextRegion::Viewport,
            TextScope::All => TextRegion::All,
        };
        self.pane()?.read_text(region).ok()
    }

    fn read_selection(&self) -> Option<String> {
        self.pane()?
            .selection_text(SelectionText::Raw)
            .filter(|text| !text.is_empty())
    }

    fn cursor_column(&self) -> Option<u32> {
        Some(u32::from(self.pane()?.cursor_column()))
    }

    fn binding_action(&mut self, action: &str) -> bool {
        if !self.shared.is_realized() {
            return false;
        }
        match Action::from_str(action) {
            Ok(action) => {
                if !self.perform(&action) {
                    tracing::debug!(target: "agterm::surface", action = action.name(), "binding action not performed");
                }
            }
            Err(err) => tracing::warn!(target: "agterm::surface", action, ?err, "unknown binding action"),
        }
        true
    }

    fn foreground_pid(&self) -> Option<u32> {
        self.pane()?.foreground_pid()
    }

    fn font_size(&self) -> Option<f64> {
        self.shared.is_realized().then(|| f64::from(self.shared.font_points()))
    }

    fn cell_size(&self) -> Option<(f64, f64)> {
        self.shared.is_realized().then(|| self.shared.cell_size())
    }

    fn teardown(self: Box<Self>) {
        self.shared.teardown();
    }
}

/// The terminal side of ghostty's `performBindingAction` (Task 8's action table): what the GTK build's libghostty did
/// for each terminal-scoped action this build supports. App-scoped actions and the window, tab and split actions are
/// the frontend's (Task 14 routes them); anything not performed here returns false.
struct Actions<'a> {
    shared: &'a Shared,
}

impl Performer for Actions<'_> {
    fn perform(&mut self, action: &Action) -> bool {
        let Some(pane) = self.shared.pane().filter(|_| self.shared.is_realized()) else {
            return false;
        };
        if let Some(bytes) = action.pty_bytes() {
            return pane.send(&bytes).is_ok();
        }
        match action {
            Action::CopyToClipboard(format) => self.copy(&pane, *format),
            Action::PasteFromClipboard => self.queue(Effect::Paste(ClipboardLocation::Standard)),
            Action::PasteFromSelection => self.queue(Effect::Paste(ClipboardLocation::Selection)),
            Action::SelectAll => pane.select_all().unwrap_or(false),
            Action::IncreaseFontSize(delta) => self.font(self.shared.font_points() + delta.clamp(0.0, 255.0)),
            Action::DecreaseFontSize(delta) => self.font(self.shared.font_points() - delta.clamp(0.0, 255.0)),
            Action::ResetFontSize => self.font(self.shared.original_font_points()),
            Action::SetFontSize(points) => self.font(*points),
            Action::Search(needle) => {
                pane.search(needle);
                true
            }
            Action::StartSearch => self.queue(Effect::Emit(TerminalEvent::StartSearch)),
            Action::EndSearch => {
                pane.search("");
                self.queue(Effect::Emit(TerminalEvent::EndSearch))
            }
            Action::NavigateSearch(direction) => pane.navigate_search(match direction {
                Direction::Next => SearchStep::Next,
                Direction::Previous => SearchStep::Previous,
            }),
            _ => scroll(&pane, action, self.page()),
        }
    }

    /// The sequence keys belong to the keyboard (Task 14); nothing is queued here.
    fn flush_sequence(&mut self) {}
}

impl Actions<'_> {
    fn queue(&self, effect: Effect) -> bool {
        self.shared.queue(effect);
        true
    }

    /// A font size change (the views follow on the next frame); performed even when the size stays (Ghostty's
    /// actions always are).
    fn font(&self, points: f32) -> bool {
        self.shared.set_font_points(points);
        true
    }

    /// `copy_to_clipboard`: the selection's text, trimmed as Ghostty copies it; nothing without a selection.
    fn copy(&self, pane: &Pane, _format: CopyFormat) -> bool {
        match pane.selection_text(SelectionText::Copy) {
            Some(text) if !text.is_empty() => self.queue(Effect::Copy(text, ClipboardLocation::Standard)),
            _ => false,
        }
    }

    /// The viewport's height in rows.
    fn page(&self) -> isize {
        self.shared
            .laid
            .get()
            .map_or(1, |laid| isize::try_from(laid.grid.rows).unwrap_or(isize::MAX))
    }
}

/// The viewport scrolls of ghostty's action table.
fn scroll(pane: &Pane, action: &Action, rows_per_page: isize) -> bool {
    let scroll = match action {
        Action::ScrollToTop => Scroll::Top,
        Action::ScrollToBottom => Scroll::Bottom,
        Action::ScrollPageUp => Scroll::Rows(-rows_per_page),
        Action::ScrollPageDown => Scroll::Rows(rows_per_page),
        Action::ScrollPageLines(lines) => Scroll::Rows(isize::from(*lines)),
        Action::ScrollPageFractional(fraction) => Scroll::Rows(fractional_page(*fraction, rows_per_page)),
        Action::JumpToPrompt(prompts) => Scroll::Prompts(isize::from(*prompts)),
        _ => return false,
    };
    pane.scroll(scroll);
    true
}

/// `scroll_page_fractional`: a fraction of the page, rounded toward zero as Ghostty's `@intFromFloat`.
fn fractional_page(fraction: f32, page: isize) -> isize {
    #[expect(clippy::cast_precision_loss, reason = "a page of rows is far below 2^24")]
    let rows = fraction * page as f32;
    #[expect(clippy::cast_possible_truncation, reason = "a fraction of a page fits an isize")]
    let rows = rows.trunc() as isize;
    rows
}
