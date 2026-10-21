//! The terminal side of Ghostty's terminal-scoped binding actions that change or read the terminal itself
//! (`Surface.performBindingAction` and `Termio.clearScreen` at `LIBGHOSTTY_VT_REV`): `reset`, `clear_screen`,
//! `adjust_selection`, `scroll_to_selection`, and the text of `copy_to_clipboard` and `write_*_file`.
//!
//! `clear_screen` has no counterpart in libghostty-vt's API, so it is written as VT sequences into the terminal (not
//! the program), at a point where the parser is at ground: at once when it is, else by the IO thread at the next
//! ground point of the program's output (`vt_write_until_ground`), so a sequence the program is in the middle of is
//! never cut. Not at a prompt Ghostty removes the rows above the cursor (`eraseActive`, the cursor following its row
//! to the top); here `CSI n S` moves them up (`scrollUp` keeps the cursor's place and pending wrap), `CSI 3 J` drops
//! them with the scrollback, and `CSI n A` takes the cursor back to its row. A documented approximation (plan > Task
//! 13 review fixes): the cursor move ends a pending wrap, a scroll region the program set limits the scroll, and the
//! new rows take the pen's background.

use agterm_ghostty_vt as vt;

use crate::pane::{Pane, Shared, Vt, lock};

/// Ghostty's `adjust_selection` directions.
pub use vt::SelectionAdjust;

/// A text format of Ghostty's formatter (`copy_to_clipboard`, `write_*_file`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TextFormat {
    #[default]
    Plain,
    /// With VT sequences for colors, styles and links.
    Vt,
    Html,
}

/// What [`Pane::format`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatRegion {
    /// The whole screen, scrollback included.
    Screen,
    /// The scrollback only; none on the alternate screen.
    History,
    /// The selection.
    Selection,
}

/// Form feed: the shell repaints its prompt after a clear at the prompt.
const FORM_FEED: u8 = 0x0C;
/// Kitty graphics: delete every image and placement, quietly.
const KITTY_DELETE_ALL: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

impl Pane {
    /// Ghostty's `reset`: a full reset of the terminal (RIS), the program untouched.
    pub fn reset(&self) {
        let shared = self.shared();
        lock(&shared.vt).terminal.reset();
        shared.touch_capture();
        shared.wake();
    }

    /// Ghostty's `clear_screen`: `false`, nothing done, on the alternate screen; otherwise the clear is performed now
    /// or at the program's output's next ground point (module docs).
    pub fn clear_screen(&self) -> bool {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        if vt.terminal.alternate_screen() {
            return false;
        }
        vt.pending_clear = true;
        if vt.terminal.at_ground() {
            apply_clear(shared, &mut vt);
        }
        drop(vt);
        shared.wake();
        true
    }

    /// Ghostty's `adjust_selection`: moves the selection's end and scrolls it into view; `false` with no selection.
    pub fn adjust_selection(&self, adjust: SelectionAdjust) -> bool {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let adjusted = vt.terminal.adjust_selection(adjust).unwrap_or_else(|err| {
            tracing::warn!(%err, "adjusting the selection failed");
            false
        });
        if adjusted {
            scroll_end_into_view(&mut vt.terminal);
            shared.touch_capture();
        }
        drop(vt);
        if adjusted {
            shared.wake();
        }
        adjusted
    }

    /// Ghostty's `scroll_to_selection`: the selection's top-left becomes the first visible row; `false` with no
    /// selection.
    pub fn scroll_to_selection(&self) -> bool {
        let shared = self.shared();
        let mut vt = lock(&shared.vt);
        let Some(selection) = vt.terminal.selection(vt::PointTag::Screen) else {
            return false;
        };
        let top = selection.start.y.min(selection.end.y);
        vt.terminal
            .scroll_viewport(vt::Scroll::Row(usize::try_from(top).unwrap_or(usize::MAX)));
        drop(vt);
        shared.wake();
        true
    }

    /// The text of `region` in `format` (soft wraps joined, `trim` dropping trailing whitespace), formatted under the
    /// VT lock; `None` when the region is missing (no selection, no scrollback), which Ghostty treats as nothing to
    /// do. An existing region may format to an empty string.
    #[must_use]
    pub fn format(&self, region: FormatRegion, format: TextFormat, trim: bool) -> Option<String> {
        let options = vt::FormatOptions {
            format: match format {
                TextFormat::Plain => vt::Format::Plain,
                TextFormat::Vt => vt::Format::Vt,
                TextFormat::Html => vt::Format::Html,
            },
            unwrap: true,
            trim,
            extras: vt::FormatExtras::empty(),
        };
        let mut vt = lock(&self.shared().vt);
        let region = engine_region(&vt.terminal, region)?;
        let bytes = vt
            .terminal
            .format(region, options)
            .map_err(|err| tracing::warn!(%err, "formatting the terminal's text failed"))
            .ok()??;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// The engine's region for `region`; `None` when it does not exist.
fn engine_region(terminal: &vt::Terminal, region: FormatRegion) -> Option<vt::Region> {
    match region {
        FormatRegion::Screen => Some(vt::Region::Screen),
        FormatRegion::Selection => terminal.selection(vt::PointTag::Screen).map(|_| vt::Region::Selection),
        FormatRegion::History => {
            let history = terminal.total_rows().saturating_sub(usize::from(terminal.rows()));
            if terminal.alternate_screen() || history == 0 {
                return None;
            }
            let last = u32::try_from(history - 1).unwrap_or(u32::MAX);
            Some(vt::Region::Range {
                start: vt::Point::new(vt::PointTag::Screen, 0, 0),
                end: vt::Point::new(vt::PointTag::Screen, terminal.cols().saturating_sub(1), last),
                rectangle: false,
            })
        }
    }
}

/// Ghostty scrolls an adjusted selection's end into view: to the top when it is above the viewport, to the bottom row
/// when below.
fn scroll_end_into_view(terminal: &mut vt::Terminal) {
    let Some(selection) = terminal.selection(vt::PointTag::Screen) else {
        return;
    };
    let end = usize::try_from(selection.end.y).unwrap_or(usize::MAX);
    let rows = usize::from(terminal.rows());
    let top = usize::try_from(terminal.viewport_top()).unwrap_or(usize::MAX);
    if (top..top + rows).contains(&end) {
        return;
    }
    let target = if end < top { end } else { end.saturating_sub(rows - 1) };
    terminal.scroll_viewport(vt::Scroll::Row(target));
}

/// Performs a pending clear (module docs), the parser at ground and the VT lock held. On the alternate screen
/// (entered since the clear was asked for) nothing is done.
pub(crate) fn apply_clear(shared: &Shared, vt: &mut Vt) {
    if !std::mem::take(&mut vt.pending_clear) || vt.terminal.alternate_screen() {
        return;
    }
    if let Err(err) = vt.terminal.clear_selection() {
        tracing::warn!(%err, "clearing the selection failed");
    }
    if vt.terminal.cursor_at_prompt() {
        // erase the scrollback, then the whole screen through Ghostty's own complete erase, and let the shell repaint
        vt.terminal.vt_write(b"\x1b[3J\x1b[2J");
        shared.push_input(&[FORM_FEED]);
    } else {
        let (_, row) = vt.terminal.cursor();
        let sequence = if row > 0 {
            format!("\x1b[{row}S\x1b[3J\x1b[{row}A")
        } else {
            "\x1b[3J".to_owned()
        };
        vt.terminal.vt_write(sequence.as_bytes());
        vt.terminal.vt_write(KITTY_DELETE_ALL);
    }
    shared.touch_capture();
}

// the programs are POSIX shell scripts (Windows' forms of a pane are tested in tests/platform.rs)
#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::{GridSize, PaneCommand, PaneConfig};

    const SIZE: GridSize = GridSize {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    };

    fn wait_line(pane: &Pane, row: usize, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let frame = pane.frame().frame;
            if frame.lines().get(row).is_some_and(|line| line == text) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out: {:?}", frame.lines());
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_clear_asked_inside_a_sequence_waits_for_its_end() {
        let dir = tempfile::tempdir().unwrap();
        let command = PaneCommand::new("/bin/sh")
            .arg("-c")
            .arg("stty -echo; printf 'one\\r\\ntwo'; read _; printf '1mafter'; exec sleep 5")
            .cwd(dir.path());
        let pane = Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap();
        wait_line(&pane, 1, "two");
        // the program's output stopped in the middle of an SGR
        lock(&pane.shared().vt).terminal.vt_write(b"\x1b[3");
        assert!(pane.clear_screen());
        assert!(lock(&pane.shared().vt).pending_clear, "the clear waits for ground");
        wait_line(&pane, 0, "one");
        pane.write(b"\n").unwrap();
        // the SGR ended first, then the clear, then the rest of the output at the cursor
        wait_line(&pane, 0, "twoafter");
        assert!(!lock(&pane.shared().vt).pending_clear);
        let red = pane.frame().frame.rows[0].cells()[3].foreground;
        assert!(red.is_some(), "the program's SGR 31 was applied whole");
    }

    #[test]
    fn a_clear_waiting_on_a_clipboard_read_goes_in_with_its_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        // the clipboard read is the last thing the program writes: its BEL ends the batch
        let command = PaneCommand::new("/bin/sh")
            .arg("-c")
            .arg("stty -echo; printf 'one\\r\\ntwo\\033]52;c;?\\007'; exec sleep 5")
            .cwd(dir.path());
        let pane = Pane::spawn(&command, &PaneConfig::new(SIZE)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let read = loop {
            let found = pane.take_events().into_iter().find_map(|event| match event {
                crate::PaneEvent::ClipboardRead(read) => Some(read),
                _ => None,
            });
            if let Some(read) = found {
                break read;
            }
            assert!(Instant::now() < deadline, "no clipboard read");
            std::thread::sleep(Duration::from_millis(5));
        };
        // the IO thread waits for the answer inside the OSC: not at ground, so the clear waits
        assert!(pane.clear_screen());
        assert!(lock(&pane.shared().vt).pending_clear);
        pane.answer_clipboard_read(read.id, crate::ClipboardAnswer::Denied);
        // the dispatch byte reached ground and the clear went in, with no more output from the program
        wait_line(&pane, 0, "two");
        assert!(!lock(&pane.shared().vt).pending_clear);
    }
}
