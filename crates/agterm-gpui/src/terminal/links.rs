//! Links in a terminal (Ghostty's `linkAtPos`, `link-url`, `link-osc8`): an OSC 8 hyperlink under the pointer, or a
//! URL detected in the text. Both need Ctrl (Super) and no other modifier held, as Ghostty's defaults ask
//! (`ctrlOrSuper` for OSC 8, `hover_mods` for the URL matcher): then the link is underlined while hovered, the pointer
//! becomes a hand, and a click (a press and release on the link without a drag) opens it — through
//! [`super::TerminalEvent::OpenUrl`], which the frontend routes through the link policy ([`follow_link`] is the
//! default route: the desktop's handler for a web or mail link, the file manager for a local file, nothing for the
//! rest).
//!
//! URLs and paths are found with Ghostty's default matcher (`src/config/url.zig` at `LIBGHOSTTY_VT_REV`, MIT: its
//! scheme URL branch, its rooted or dot-relative path branch and its bare relative path branch) over the soft-wrapped
//! line under the pointer, in the frame's rows. A path is underlined under the pointer as libghostty underlines it;
//! its click goes to the link policy, which ignores a link with no scheme, as upstream's `LinkPolicy` does. An OSC 8
//! link covers the run of hyperlinked cells around the pointer on its row.

use std::sync::LazyLock;

use agterm_core::ghostty_config::binds::Mods;
use agterm_core::link_policy::{self, ClickOrigin, Route};
use agterm_core::persist::settings::LinkOpenMode;
use agterm_term::{CellFlags, CellWidth, Frame, GridPoint, Pane};
use fancy_regex::Regex;
use gpui_kit::App;

/// `url.zig`'s parts, in Oniguruma's syntax as fancy-regex reads it (`\/` written `/`). Its one variable-length
/// look-behind, the bare path's `(?<!\$\d*)(?<!\w)`, is the fixed `(?<![$\w])`: a digit is a word character, so
/// `(?<!\w)` already refuses the `$` and digits case but for a `$` right before the match.
mod url_zig {
    pub const SCHEMES: &str =
        r"https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:";
    pub const IPV6_URL: &str = r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)";
    pub const SCHEME_URL_CHARS: &str = r"[\w\-.~:/?#@!$&*+,;=%]";
    pub const PATH_CHARS: &str = r"[\w\-.~:/?#@!$&*+;=%]";
    pub const OPTIONAL_BRACKETED_WORD_SUFFIX: &str = r"(?:[\(\[]\w*[\)\]])?";
    pub const NO_TRAILING_PUNCTUATION: &str = r"(?<![,.])";
    pub const NO_TRAILING_COLON: &str = r"(?<!:)";
    pub const TRAILING_SPACES_AT_EOL: &str = r"(?: +(?= *$))?";
    pub const DOTTED_PATH_LOOKAHEAD: &str = r"(?=[\w\-.~:/?#@!$&*+;=%]*\.)";
    pub const NON_DOTTED_PATH_LOOKAHEAD: &str = r"(?![\w\-.~:/?#@!$&*+;=%]*\.)";
    pub const DOTTED_PATH_SPACE_SEGMENTS: &str = r"(?:(?<!:) (?!\w+://)(?!\.{0,2}/)(?!~/)[\w\-.~:/?#@!$&*+;=%]*[/.])*";
    pub const ANY_PATH_SPACE_SEGMENTS: &str = r"(?:(?<!:) (?!\w+://)(?!\.{0,2}/)(?!~/)[\w\-.~:/?#@!$&*+;=%]+)*";
    pub const ROOTED_OR_RELATIVE_PATH_PREFIX: &str =
        r"(?:\.\./|\./|(?<!\w)~/|(?:[\w][\w\-.]*/)*(?<!\w)\$[A-Za-z_]\w*/|\.[\w][\w\-.]*/|(?<![\w~/])/(?!/))";
    pub const BARE_RELATIVE_PATH_PREFIX: &str = r"(?<![$\w])[\w][\w\-.]*/";
}

/// Ghostty's whole matcher: `scheme_url_branch | rooted_or_relative_path_branch | bare_relative_path_branch`.
fn url_pattern() -> String {
    use url_zig::{
        ANY_PATH_SPACE_SEGMENTS, BARE_RELATIVE_PATH_PREFIX, DOTTED_PATH_LOOKAHEAD, DOTTED_PATH_SPACE_SEGMENTS,
        IPV6_URL, NO_TRAILING_COLON, NO_TRAILING_PUNCTUATION, NON_DOTTED_PATH_LOOKAHEAD,
        OPTIONAL_BRACKETED_WORD_SUFFIX, PATH_CHARS, ROOTED_OR_RELATIVE_PATH_PREFIX, SCHEME_URL_CHARS, SCHEMES,
        TRAILING_SPACES_AT_EOL,
    };
    let scheme = format!(
        "(?:{SCHEMES})(?:{IPV6_URL}|{SCHEME_URL_CHARS}+{OPTIONAL_BRACKETED_WORD_SUFFIX})+{NO_TRAILING_PUNCTUATION}"
    );
    let rooted = format!(
        "{ROOTED_OR_RELATIVE_PATH_PREFIX}(?:{DOTTED_PATH_LOOKAHEAD}{PATH_CHARS}+{DOTTED_PATH_SPACE_SEGMENTS}\
         {NO_TRAILING_COLON}{TRAILING_SPACES_AT_EOL}|{NON_DOTTED_PATH_LOOKAHEAD}{PATH_CHARS}+\
         {ANY_PATH_SPACE_SEGMENTS}{NO_TRAILING_COLON}{TRAILING_SPACES_AT_EOL})"
    );
    let bare = format!(
        "{DOTTED_PATH_LOOKAHEAD}{BARE_RELATIVE_PATH_PREFIX}{PATH_CHARS}+{NO_TRAILING_COLON}{TRAILING_SPACES_AT_EOL}"
    );
    format!("{scheme}|{rooted}|{bare}")
}

static URL: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&url_pattern())
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

/// The URL or path on viewport cell (`col`, `row`).
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
        // a match the backtracking engine gives up on (its limit) is no link
        let found = found.ok()?;
        let span = found.range();
        if found.as_str().is_empty() || !span.contains(&pointed) {
            return None;
        }
        Some(Link {
            url: found.as_str().to_owned(),
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

    fn first_match(text: &str) -> Option<String> {
        let regex = URL.as_ref().expect("the matcher compiles");
        regex
            .find(text)
            .expect("the matcher runs")
            .map(|found| found.as_str().to_owned())
    }

    /// Ghostty's `url.zig` `test "url regex"`, every case: the first match in each input.
    const CASES: &[(&str, &str)] = &[
        ("hello https://example.com world", "https://example.com"),
        ("https://example.com/foo(bar) more", "https://example.com/foo(bar)"),
        (
            "https://example.com/foo(bar)baz more",
            "https://example.com/foo(bar)baz",
        ),
        ("Link inside (https://example.com) parens", "https://example.com"),
        ("Link period https://example.com. More text.", "https://example.com"),
        (
            "Link trailing comma https://example.com, more text.",
            "https://example.com",
        ),
        (
            "Link in double quotes \"https://example.com\" and more",
            "https://example.com",
        ),
        (
            "Link in single quotes 'https://example.com' and more",
            "https://example.com",
        ),
        (
            "some file with https://google.com https://duckduckgo.com links.",
            "https://google.com",
        ),
        (
            "and links in it. links https://yahoo.com mailto:test@example.com ssh://1.2.3.4",
            "https://yahoo.com",
        ),
        ("also match http://example.com non-secure links", "http://example.com"),
        ("match tel://+12123456789 phone numbers", "tel://+12123456789"),
        (
            "match with query url https://example.com?query=1&other=2 and more text.",
            "https://example.com?query=1&other=2",
        ),
        (
            "url with dashes [mode 2027](https://github.com/contour-terminal/terminal-unicode-core) for better unicode support",
            "https://github.com/contour-terminal/terminal-unicode-core",
        ),
        ("dot.http://example.com", "http://example.com"),
        (
            "weird characters https://example.com/~user/?query=1&other=2#hash and more",
            "https://example.com/~user/?query=1&other=2#hash",
        ),
        (
            "square brackets https://example.com/[foo] and more",
            "https://example.com/[foo]",
        ),
        (
            "[13]:TooManyStatements: TempFile#assign_temp_file_to_entity has approx 7 statements [https://example.com/docs/Too-Many-Statements.md]",
            "https://example.com/docs/Too-Many-Statements.md",
        ),
        ("match ftp://example.com ftp links", "ftp://example.com"),
        ("match file://example.com file links", "file://example.com"),
        ("match ssh://example.com ssh links", "ssh://example.com"),
        ("match git://example.com git links", "git://example.com"),
        ("/tmp/test.txt http://www.google.com", "/tmp/test.txt"),
        ("match tel:+18005551234 tel links", "tel:+18005551234"),
        (
            "match magnet:?xt=urn:btih:1234567890 magnet links",
            "magnet:?xt=urn:btih:1234567890",
        ),
        ("match ipfs://QmSomeHashValue ipfs links", "ipfs://QmSomeHashValue"),
        ("match ipns://QmSomeHashValue ipns links", "ipns://QmSomeHashValue"),
        ("match gemini://example.com gemini links", "gemini://example.com"),
        ("match gopher://example.com gopher links", "gopher://example.com"),
        (
            "match news:comp.infosystems.www.servers.unix news links",
            "news:comp.infosystems.www.servers.unix",
        ),
        (
            "/Users/ghostty.user/code/example.py",
            "/Users/ghostty.user/code/example.py",
        ),
        (
            "/Users/ghostty.user/code/../example.py",
            "/Users/ghostty.user/code/../example.py",
        ),
        (
            "/Users/ghostty.user/code/../example.py hello world",
            "/Users/ghostty.user/code/../example.py",
        ),
        ("../example.py", "../example.py"),
        ("../example.py ", "../example.py "),
        ("first time ../example.py contributor ", "../example.py"),
        (
            "[link](/home/user/ghostty.user/example)",
            "/home/user/ghostty.user/example",
        ),
        ("Serving HTTP on :: port 8000 (http://[::]:8000/)", "http://[::]:8000/"),
        (
            "IPv6 address https://[2001:db8::1]:8080/path",
            "https://[2001:db8::1]:8080/path",
        ),
        ("IPv6 localhost http://[::1]:3000", "http://[::1]:3000"),
        (
            "Complex IPv6 https://[2001:db8:85a3:8d3:1319:8a2e:370:7348]:443/",
            "https://[2001:db8:85a3:8d3:1319:8a2e:370:7348]:443/",
        ),
        (
            "IPv6 with path https://[2001:db8::1]/path/to/resource",
            "https://[2001:db8::1]/path/to/resource",
        ),
        (
            "IPv6 with query https://[2001:db8::1]:8080/api?param=value&other=123",
            "https://[2001:db8::1]:8080/api?param=value&other=123",
        ),
        ("IPv6 compressed http://[2001:db8::]:80/", "http://[2001:db8::]:80/"),
        (
            "IPv6 multiple zeros http://[2001:0:0:0:0:0:0:1]",
            "http://[2001:0:0:0:0:0:0:1]",
        ),
        (
            "IPv6 link-local https://[fe80::1234:5678:9abc]",
            "https://[fe80::1234:5678:9abc]",
        ),
        ("IPv6 multicast http://[ff02::1]/stream", "http://[ff02::1]/stream"),
        (
            "IPv6 in markdown [link](http://[2001:db8::1]/docs)",
            "http://[2001:db8::1]/docs",
        ),
        ("./spaces-end.   ", "./spaces-end.   "),
        ("./space middle", "./space middle"),
        ("../test folder/file.txt", "../test folder/file.txt"),
        ("/tmp/test folder/file.txt", "/tmp/test folder/file.txt"),
        ("/tmp/test  folder/file.txt", "/tmp/test"),
        (
            "diff --git a/src/font/shaper/harfbuzz.zig b/src/font/shaper/harfbuzz.zig",
            "a/src/font/shaper/harfbuzz.zig",
        ),
        ("/tmp/foo /tmp/bar", "/tmp/foo"),
        ("/tmp/foo.txt /tmp/bar.txt", "/tmp/foo.txt"),
        ("src/config/url.zig", "src/config/url.zig"),
        ("app/folder/file.rb:1", "app/folder/file.rb:1"),
        ("modified:   src/config/url.zig", "src/config/url.zig"),
        ("lib/ghostty/terminal.zig:42:10", "lib/ghostty/terminal.zig:42:10"),
        ("some-pkg/src/file.txt more text", "some-pkg/src/file.txt"),
        ("src/foo.c,baz.txt", "src/foo.c"),
        ("~/foo/bar.txt", "~/foo/bar.txt"),
        ("open ~/Documents/notes.md please", "~/Documents/notes.md"),
        ("~/.config/ghostty/config", "~/.config/ghostty/config"),
        ("directory: ~/src/ghostty-org/ghostty", "~/src/ghostty-org/ghostty"),
        ("$HOME/src/config/url.zig", "$HOME/src/config/url.zig"),
        ("project dir: $PWD/src/ghostty/main.zig", "$PWD/src/ghostty/main.zig"),
        ("foo/$BAR/baz", "foo/$BAR/baz"),
        (".foo/bar/$VAR", ".foo/bar/$VAR"),
        (".config/ghostty/config", ".config/ghostty/config"),
        (
            "loaded from .local/share/ghostty/state.db now",
            ".local/share/ghostty/state.db",
        ),
        ("../some/where", "../some/where"),
        (
            "  - shared/src/foo/SomeItem.m:12, shared/src/",
            "shared/src/foo/SomeItem.m:12",
        ),
        ("foo.local/share", "foo.local/share"),
        ("2024/report.txt", "2024/report.txt"),
        ("./foo bar,baz", "./foo bar"),
        ("/tmp/foo bar,baz", "/tmp/foo bar"),
        ("./.config/ghostty: Needs upstream (main)", "./.config/ghostty"),
        ("./Downloads: Operation not permitted", "./Downloads"),
    ];

    /// Its `no_match_cases`.
    const NO_MATCH: &[&str] = &[
        "input/output",
        "foo/bar",
        "$10/bar",
        "$10/$20",
        "$10/bar.txt",
        "foo/bar,baz.txt",
        "foo$BAR/baz.txt",
        "foo~/bar.txt",
        "// foo bar",
        "//foo",
        "no link here",
    ];

    #[test]
    fn the_matcher_finds_ghosttys_urls_and_paths() {
        for (input, expected) in CASES {
            assert_eq!(first_match(input).as_deref(), Some(*expected), "in {input:?}");
        }
        for input in NO_MATCH {
            assert_eq!(first_match(input), None, "in {input:?}");
        }
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
