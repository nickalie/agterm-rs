//! Links in a terminal (Ghostty's `linkAtPos`, `link-url`, `link-osc8`): an OSC 8 hyperlink under the pointer, or a
//! URL detected in the text. Both need Ctrl (Super) and no other modifier held, as Ghostty's defaults ask
//! (`ctrlOrSuper` for OSC 8, `hover_mods` for the URL matcher): then the link is underlined while hovered, the pointer
//! becomes a hand, and a click (a press and release on the link without a drag) opens it — through
//! [`super::TerminalEvent::OpenUrl`], which the frontend routes through the link policy ([`follow_link`] is the
//! default route: the desktop's handler for a web or mail link, the file manager for a local file, nothing for the
//! rest).
//!
//! URLs are found with Ghostty's scheme matcher (`src/config/url.zig`, MIT; the path branches are left out: the link
//! policy ignores a link with no scheme) over the soft-wrapped line under the pointer, in the frame's rows. Its
//! look-behind against trailing punctuation is applied after matching. An OSC 8 link covers the run of hyperlinked
//! cells around the pointer on its row.

use std::sync::LazyLock;

use agterm_core::ghostty_config::binds::Mods;
use agterm_core::link_policy::{self, ClickOrigin, Route};
use agterm_core::persist::settings::LinkOpenMode;
use agterm_term::{CellFlags, CellWidth, Frame, GridPoint, Pane};
use gpui_kit::App;
use regex::Regex;

/// Ghostty's scheme URL branch (`url.zig`), its trailing `(?<![,.])` applied by [`trim`].
const URL_PATTERN: &str = concat!(
    r"(?:https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:)",
    r"(?:(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)|[\w\-.~:/?#@!$&*+,;=%]+(?:[\(\[]\w*[\)\]])?)+",
);

static URL: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(URL_PATTERN)
        .map_err(|err| tracing::error!(%err, "the URL matcher does not compile"))
        .ok()
});

/// What kind of link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// An OSC 8 hyperlink.
    Osc8,
    /// A URL found in the text.
    Url,
}

/// A link on the viewport: where it opens and the cells it covers (viewport row, first and last column).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub kind: LinkKind,
    pub cells: Vec<(u16, u16, u16)>,
}

impl Link {
    /// Whether the link covers viewport cell (`col`, `row`).
    #[must_use]
    pub fn covers(&self, col: u16, row: u16) -> bool {
        self.cells
            .iter()
            .any(|(at, start, end)| *at == row && (*start..=*end).contains(&col))
    }
}

/// Whether `mods` make links live: Ctrl or Super alone (Ghostty's `ctrlOrSuper`). The caller drops Shift first while
/// the program has the mouse and may not capture it (Ghostty's `mouseModsWithCapture`).
#[must_use]
pub fn activates(mods: Mods) -> bool {
    let mods = mods.binding();
    mods == Mods::CTRL || mods == Mods::SUPER
}

/// The link under viewport cell `at` while `mods` make links live: an OSC 8 hyperlink first, then a URL when `urls`
/// (`link-url`).
#[must_use]
pub fn link_at(pane: &Pane, frame: &Frame, at: GridPoint, mods: Mods, urls: bool) -> Option<Link> {
    if !activates(mods) {
        return None;
    }
    let (col, row) = (at.x, u16::try_from(at.y).ok()?);
    osc8_at(pane, frame, col, row).or_else(|| if urls { url_at(frame, col, row) } else { None })
}

/// The OSC 8 hyperlink on viewport cell (`col`, `row`): the run of cells around it carrying the same link.
#[must_use]
pub fn osc8_at(pane: &Pane, frame: &Frame, col: u16, row: u16) -> Option<Link> {
    let cells = frame.rows.get(usize::from(row))?.cells();
    let at = usize::from(col);
    if !cells
        .get(at)
        .is_some_and(|cell| cell.flags.contains(CellFlags::HYPERLINK))
    {
        return None;
    }
    let url = pane.hyperlink(GridPoint::viewport(col, u32::from(row)))?;
    // a neighbour of another link (or none) ends the run
    let linked = |index: usize| {
        cells
            .get(index)
            .is_some_and(|cell| cell.flags.contains(CellFlags::HYPERLINK))
            && u16::try_from(index)
                .ok()
                .and_then(|x| pane.hyperlink(GridPoint::viewport(x, u32::from(row))))
                == Some(url.clone())
    };
    let start = (0..=at).rev().take_while(|index| linked(*index)).last().unwrap_or(at);
    let end = (at..cells.len())
        .take_while(|index| linked(*index))
        .last()
        .unwrap_or(at);
    Some(Link {
        url,
        kind: LinkKind::Osc8,
        cells: vec![(row, u16::try_from(start).ok()?, u16::try_from(end).ok()?)],
    })
}

/// The soft-wrapped line through viewport row `row`: its text and, for each cell's text, where it starts in it.
struct Line {
    text: String,
    /// `(byte offset, row, column)`, in order.
    cells: Vec<(usize, u16, u16)>,
}

fn line_through(frame: &Frame, row: usize) -> Option<Line> {
    let rows = &frame.rows;
    rows.get(row)?;
    let mut first = row;
    while first > 0 && rows[first - 1].wrapped() {
        first -= 1;
    }
    let mut last = row;
    while last + 1 < rows.len() && rows[last].wrapped() {
        last += 1;
    }
    let mut line = Line {
        text: String::new(),
        cells: Vec::new(),
    };
    for (index, row) in rows.iter().enumerate().take(last + 1).skip(first) {
        let at_row = u16::try_from(index).ok()?;
        for (col, cell) in row.cells().iter().enumerate() {
            if matches!(cell.width, CellWidth::SpacerTail | CellWidth::SpacerHead) {
                continue;
            }
            line.cells.push((line.text.len(), at_row, u16::try_from(col).ok()?));
            match row.text(col) {
                "" => line.text.push(' '),
                text => line.text.push_str(text),
            }
        }
    }
    Some(line)
}

/// A match with its trailing `,` and `.` dropped (Ghostty's `(?<![,.])`).
fn trim(text: &str) -> &str {
    text.trim_end_matches([',', '.'])
}

/// The URL on viewport cell (`col`, `row`).
#[must_use]
pub fn url_at(frame: &Frame, col: u16, row: u16) -> Option<Link> {
    let line = line_through(frame, usize::from(row))?;
    // the pointer on a wide character's spacer belongs to the character
    let pointed = line
        .cells
        .iter()
        .filter(|(_, at_row, at_col)| *at_row == row && *at_col <= col)
        .map(|(offset, ..)| *offset)
        .next_back()?;
    let regex = URL.as_ref()?;
    regex.find_iter(&line.text).find_map(|found| {
        let url = trim(found.as_str());
        let span = found.start()..found.start() + url.len();
        if url.is_empty() || !span.contains(&pointed) {
            return None;
        }
        Some(Link {
            url: url.to_owned(),
            kind: LinkKind::Url,
            cells: cells_of(&line, span, frame),
        })
    })
}

/// The viewport cells a byte span of `line` covers, per row.
fn cells_of(line: &Line, span: std::ops::Range<usize>, frame: &Frame) -> Vec<(u16, u16, u16)> {
    let mut cells: Vec<(u16, u16, u16)> = Vec::new();
    for (_, row, col) in line.cells.iter().filter(|(offset, ..)| span.contains(offset)) {
        let wide = frame
            .rows
            .get(usize::from(*row))
            .and_then(|r| r.cells().get(usize::from(*col)))
            .is_some_and(|cell| cell.width == CellWidth::Wide);
        let end = if wide { col + 1 } else { *col };
        match cells.last_mut() {
            Some((at, _, last)) if at == row => *last = end,
            _ => cells.push((*row, *col, end)),
        }
    }
    cells
}

/// Carries out what the link policy decides for a clicked link (the GTK build's `follow_link`): the desktop's handler
/// for a web or mail link, the file manager for a local file, nothing for anything else. A session overlay route
/// opens in the browser here; the frontend's own route shows it over its session.
pub fn follow_link(raw: &str, mode: LinkOpenMode, origin: ClickOrigin, cx: &App) {
    match link_policy::route(raw, mode, origin) {
        Route::Browser(url) | Route::Overlay { url, .. } => cx.open_url(&url),
        Route::Reveal(path) => cx.reveal_path(&path),
        Route::Ignore => tracing::debug!(raw, "link click ignored by the link policy"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_drop_trailing_punctuation_and_keep_balanced_parentheses() {
        let regex = URL.as_ref().unwrap();
        let found = |text: &str| regex.find(text).map(|m| trim(m.as_str()).to_owned());
        assert_eq!(
            found("see https://example.com/a.").as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(found("(https://example.com)").as_deref(), Some("https://example.com"));
        assert_eq!(
            found("https://en.wikipedia.org/wiki/Rust_(video_game) x").as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(video_game)")
        );
        assert_eq!(found("mail mailto:a@b.c, now").as_deref(), Some("mailto:a@b.c"));
        assert_eq!(found("no link here"), None);
    }

    #[test]
    fn only_ctrl_or_super_alone_make_links_live() {
        assert!(activates(Mods::CTRL));
        assert!(activates(Mods::SUPER | Mods::CAPS_LOCK));
        assert!(!activates(Mods::empty()));
        assert!(!activates(Mods::CTRL | Mods::SHIFT));
        assert!(!activates(Mods::CTRL | Mods::SUPER));
    }
}
