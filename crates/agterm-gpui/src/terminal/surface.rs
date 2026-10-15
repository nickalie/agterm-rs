//! [`TerminalSurface`]: the core's [`Surface`] over a terminal, every method the GTK build's `PaneSurface` implements
//! (`agterm-gtk/src/pane.rs`, `surface.rs`), with libghostty's work done through `agterm-term` and core's ghostty
//! action table (`agterm_core::ghostty_config::binds`).
//!
//! No method has a GPUI context: what needs the app (the clipboard, events for the frontend) is queued on the
//! terminal's pump (module docs of [`super`]), and a binding action is accepted for a realized terminal whatever it
//! then does, as the GTK build's `binding_action` returned.

use std::path::PathBuf;
use std::rc::Rc;
use std::str::FromStr as _;

use agterm_core::ghostty_config::binds::{
    self, Action, CopyFormat, Direction, Mods, Performer, PhysicalKey, WriteAction, WriteFormat, WriteScreen,
};
use agterm_core::keystrokes::{self, KeystrokeSegment, SUBMIT_GAP};
use agterm_host::{Surface, TextScope};
use agterm_term::{
    ClipboardLocation, FormatRegion, KeyInput, Pane, Scroll, SearchStep, SelectionAdjust, SelectionText, TextFormat,
    TextRegion,
};

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

    /// Read-only (`toggle_readonly`), nothing is typed and the call still succeeds, as libghostty's key path drops
    /// the input of a read-only surface.
    fn type_text(&mut self, text: &str) -> bool {
        let Some(pane) = self.pane() else {
            return false;
        };
        if self.shared.is_readonly() {
            return true;
        }
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

/// The terminal side of Ghostty's `performBindingAction` at `GHOSTTY_REV` (Task 8's action table): every
/// terminal-scoped action, with libghostty's performed/not-performed answer, which decides whether a `performable:`
/// binding lets its key through. App-scoped actions and the window, tab and split actions are the frontend's (Task 14
/// routes them) and are not performed here; so is `copy_title_to_clipboard`, which libghostty hands to the apprt and
/// neither the GTK build's nor upstream's handles.
struct Actions<'a> {
    shared: &'a Shared,
}

impl Performer for Actions<'_> {
    fn perform(&mut self, action: &Action) -> bool {
        let Some(pane) = self.shared.pane().filter(|_| self.shared.is_realized()) else {
            return false;
        };
        if let Some(bytes) = action.pty_bytes() {
            // a read-only terminal writes nothing for the user, and the action still counts as performed
            if !self.shared.is_readonly() && pane.send(&bytes).is_err() {
                tracing::debug!(target: "agterm::surface", "the terminal's input queue refused a binding's bytes");
            }
            return true;
        }
        self.font(action)
            .or_else(|| self.search(&pane, action))
            .or_else(|| self.clipboard(&pane, action))
            .or_else(|| self.screen_file(&pane, action))
            .or_else(|| self.terminal(&pane, action))
            .unwrap_or_else(|| scroll(&pane, action, self.page()))
    }

    /// The sequence keys belong to the keyboard (Task 14); nothing is queued here.
    fn flush_sequence(&mut self) {}
}

impl Actions<'_> {
    fn queue(&self, effect: Effect) -> bool {
        self.shared.queue(effect);
        true
    }

    /// The font size actions (Ghostty's clamps); the views follow on the next frame. Always performed.
    fn font(&self, action: &Action) -> Option<bool> {
        let points = self.shared.font_points();
        let points = match action {
            Action::IncreaseFontSize(delta) => points + delta.clamp(0.0, 255.0),
            Action::DecreaseFontSize(delta) => points - delta.clamp(0.0, 255.0),
            Action::ResetFontSize => self.shared.original_font_points(),
            Action::SetFontSize(points) => *points,
            _ => return None,
        };
        self.shared.set_font_points(points);
        Some(true)
    }

    /// The search actions: `end_search` and an empty `search:` are performed only when a search was open (so the
    /// default `performable:escape=end_search` lets Escape through otherwise), the UI is told of both ends anyway.
    fn search(&self, pane: &Pane, action: &Action) -> Option<bool> {
        Some(match action {
            Action::Search(needle) => pane.search(needle),
            Action::StartSearch => self.queue(Effect::Emit(TerminalEvent::StartSearch(String::new()))),
            // Ghostty's `selectionString`: untrimmed; an existing selection counts even when its text is empty
            Action::SearchSelection => match pane.format(FormatRegion::Selection, TextFormat::Plain, false) {
                Some(needle) => self.queue(Effect::Emit(TerminalEvent::StartSearch(needle))),
                None => false,
            },
            Action::EndSearch => {
                let performed = pane.search("");
                self.queue(Effect::Emit(TerminalEvent::EndSearch));
                performed
            }
            Action::NavigateSearch(direction) => pane.navigate_search(match direction {
                Direction::Next => SearchStep::Next,
                Direction::Previous => SearchStep::Previous,
            }),
            _ => return None,
        })
    }

    fn clipboard(&self, pane: &Pane, action: &Action) -> Option<bool> {
        Some(match action {
            Action::CopyToClipboard(format) => self.copy(pane, *format),
            Action::CopyUrlToClipboard => match self.shared.hovered_url() {
                Some(url) => self.queue(Effect::Copy(url, ClipboardLocation::Standard)),
                None => false,
            },
            Action::CopyTitleToClipboard => false,
            Action::PasteFromClipboard => self.queue(Effect::Paste(ClipboardLocation::Standard)),
            Action::PasteFromSelection => self.queue(Effect::Paste(ClipboardLocation::Selection)),
            _ => return None,
        })
    }

    /// `copy_to_clipboard`: Ghostty's `copySelectionToClipboards`, performed whenever there is a selection: plain and
    /// VT text as text, `html` as `text/html` alone, `mixed` as both (the HTML offer is our `gpui-pre-linux` patch).
    fn copy(&self, pane: &Pane, format: CopyFormat) -> bool {
        let trim = self.shared.config().copy_trim;
        let text_format = match format {
            CopyFormat::Plain | CopyFormat::Mixed => TextFormat::Plain,
            CopyFormat::Vt => TextFormat::Vt,
            CopyFormat::Html => TextFormat::Html,
        };
        let Some(text) = pane.format(FormatRegion::Selection, text_format, trim) else {
            return false;
        };
        let effect = match format {
            CopyFormat::Html => Effect::CopyHtml { text: None, html: text },
            CopyFormat::Mixed => match pane.format(FormatRegion::Selection, TextFormat::Html, trim) {
                Some(html) => Effect::CopyHtml { text: Some(text), html },
                None => Effect::Copy(text, ClipboardLocation::Standard),
            },
            CopyFormat::Plain | CopyFormat::Vt => Effect::Copy(text, ClipboardLocation::Standard),
        };
        self.queue(effect);
        if self.shared.config().clear_selection_on_copy
            && let Err(err) = pane.clear_selection()
        {
            tracing::warn!(target: "agterm::surface", %err, "clearing the selection after a copy failed");
        }
        true
    }

    /// `write_screen_file`, `write_scrollback_file`, `write_selection_file`: Ghostty's `writeScreenFile`.
    fn screen_file(&self, pane: &Pane, action: &Action) -> Option<bool> {
        let (region, write) = match action {
            Action::WriteScreenFile(write) => (FormatRegion::Screen, write),
            Action::WriteScrollbackFile(write) => (FormatRegion::History, write),
            Action::WriteSelectionFile(write) => (FormatRegion::Selection, write),
            _ => return None,
        };
        Some(self.write_screen_file(pane, region, *write))
    }

    /// Writes `region` into a new file of its own temporary directory and copies, pastes (the path typed into the
    /// program) or opens it. With no such region (no selection, no scrollback) nothing happens, and the action still
    /// counts as performed, as in Ghostty; `false` only when the file cannot be written.
    fn write_screen_file(&self, pane: &Pane, region: FormatRegion, write: WriteScreen) -> bool {
        let format = match write.format {
            WriteFormat::Plain => TextFormat::Plain,
            WriteFormat::Vt => TextFormat::Vt,
            WriteFormat::Html => TextFormat::Html,
        };
        let Some(text) = pane.format(region, format, false) else {
            return true;
        };
        let path = match screen_file(region, format, &text) {
            Ok(path) => path.to_string_lossy().into_owned(),
            Err(err) => {
                tracing::warn!(target: "agterm::surface", %err, "writing the screen file failed");
                return false;
            }
        };
        match write.action {
            WriteAction::Copy => self.queue(Effect::Copy(path, ClipboardLocation::Standard)),
            WriteAction::Open => self.queue(Effect::Emit(TerminalEvent::OpenUrl(path))),
            WriteAction::Paste => {
                if !self.shared.is_readonly() && pane.write(path.as_bytes()).is_err() {
                    tracing::debug!(target: "agterm::surface", "the terminal's input queue refused the file's path");
                }
                true
            }
        }
    }

    /// The actions on the terminal's screen, selection and modes.
    fn terminal(&self, pane: &Pane, action: &Action) -> Option<bool> {
        Some(match action {
            Action::Reset => {
                pane.reset();
                true
            }
            Action::ClearScreen => pane.clear_screen(),
            Action::SelectAll => {
                // Ghostty's `setSelectionAndCopy`: a selection made is copied per `copy-on-select`
                if pane.select_all().unwrap_or(false) {
                    self.shared.copy_on_select();
                }
                true
            }
            Action::AdjustSelection(adjust) => pane.adjust_selection(selection_adjust(*adjust)),
            Action::ScrollToSelection => pane.scroll_to_selection(),
            Action::ScrollToRow(row) => {
                pane.scroll(Scroll::Row(usize::try_from(*row).unwrap_or(usize::MAX)));
                true
            }
            Action::ToggleMouseReporting => {
                pane.set_mouse_reporting(!pane.mouse_reporting());
                true
            }
            Action::ToggleReadonly => {
                let readonly = !self.shared.is_readonly();
                self.shared.set_readonly(readonly);
                self.queue(Effect::Emit(TerminalEvent::Readonly(readonly)))
            }
            _ => return None,
        })
    }

    /// The viewport's height in rows.
    fn page(&self) -> isize {
        self.shared
            .laid
            .get()
            .map_or(1, |laid| isize::try_from(laid.grid.rows).unwrap_or(isize::MAX))
    }
}

fn selection_adjust(adjust: binds::SelectionAdjust) -> SelectionAdjust {
    use binds::SelectionAdjust as Core;
    match adjust {
        Core::Left => SelectionAdjust::Left,
        Core::Right => SelectionAdjust::Right,
        Core::Up => SelectionAdjust::Up,
        Core::Down => SelectionAdjust::Down,
        Core::PageUp => SelectionAdjust::PageUp,
        Core::PageDown => SelectionAdjust::PageDown,
        Core::Home => SelectionAdjust::Home,
        Core::End => SelectionAdjust::End,
        Core::BeginningOfLine => SelectionAdjust::BeginningOfLine,
        Core::EndOfLine => SelectionAdjust::EndOfLine,
    }
}

/// `<region>.<txt|html>` in a new temporary directory of its own, readable by the user only; the directory is kept
/// for whoever the path goes to (Ghostty's `TempDir` retained).
fn screen_file(region: FormatRegion, format: TextFormat, text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = tempfile::Builder::new().prefix("agterm-").tempdir()?.keep();
    let name = match region {
        FormatRegion::Screen => "screen",
        FormatRegion::History => "history",
        FormatRegion::Selection => "selection",
    };
    let extension = if format == TextFormat::Html { "html" } else { "txt" };
    let path = dir.join(format!("{name}.{extension}"));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?
        .write_all(text.as_bytes())?;
    Ok(path)
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
