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
//! line under the pointer, in the frame's rows (within [`REACH`] of the pointer). A path is underlined under the pointer as libghostty underlines it;
//! its click goes to the link policy, which ignores a link with no scheme, as upstream's `LinkPolicy` does. An OSC 8
//! link covers the run of hyperlinked cells around the pointer on its row.

use std::sync::LazyLock;

use fancy_regex::Regex;
use gpui_kit::App;
use ngterm_core::ghostty_config::binds::Mods;
use ngterm_core::link_policy::{self, ClickOrigin, Route};
use ngterm_core::persist::settings::LinkOpenMode;
use ngterm_term::{CellFlags, CellWidth, Frame, GridPoint, Pane};

/// `url.zig`'s parts, in Oniguruma's syntax as fancy-regex reads it (`\/` written `/`). Its one variable-length
/// look-behind, the bare path's `(?<!\$\d*)(?<!\w)`, is the fixed `(?<![$\w])`: a digit is a word character, so
/// `(?<!\w)` already refuses the `$` and digits case but for a `$` right before the match.
mod url_zig {
    pub const SCHEMES: &str =
        r"https?://|mailto:|ftp://|file:|ssh:|git://|ssh://|tel:|magnet:|ipfs://|ipns://|gemini://|gopher://|news:";
    pub const SCHEME_URL_CHARS: &str = r"[\w\-.~:/?#@!$&*+,;=%]";
    pub const PATH_CHARS: &str = r"[\w\-.~:/?#@!$&*+;=%]";
    pub const OPTIONAL_BRACKETED_WORD_SUFFIX: &str = r"(?:[\(\[]\w*[\)\]])?";
    pub const NO_TRAILING_PUNCTUATION: &str = r"(?<![,.])";
    pub const NO_TRAILING_COLON: &str = r"(?<!:)";
    pub const DOTTED_PATH_LOOKAHEAD: &str = r"(?=[\w\-.~:/?#@!$&*+;=%]*\.)";
    pub const NON_DOTTED_PATH_LOOKAHEAD: &str = r"(?![\w\-.~:/?#@!$&*+;=%]*\.)";
    pub const DOTTED_PATH_SPACE_SEGMENTS: &str = r"(?:(?<!:) (?!\w+://)(?!\.{0,2}/)(?!~/)[\w\-.~:/?#@!$&*+;=%]*[/.])*";
    pub const ANY_PATH_SPACE_SEGMENTS: &str = r"(?:(?<!:) (?!\w+://)(?!\.{0,2}/)(?!~/)[\w\-.~:/?#@!$&*+;=%]+)*";

    /// Ghostty's matcher word for word (`scheme_url_branch | rooted_or_relative_path_branch |
    /// bare_relative_path_branch`): the tests hold [`bounded`] to it.
    #[cfg(test)]
    pub fn literal() -> String {
        const IPV6_URL: &str = r"(?:\[[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+\](?::[0-9]+)?)";
        const ROOTED_OR_RELATIVE_PATH_PREFIX: &str =
            r"(?:\.\./|\./|(?<!\w)~/|(?:[\w][\w\-.]*/)*(?<!\w)\$[A-Za-z_]\w*/|\.[\w][\w\-.]*/|(?<![\w~/])/(?!/))";
        const BARE_RELATIVE_PATH_PREFIX: &str = r"(?<![$\w])[\w][\w\-.]*/";
        let scheme = format!(
            "(?:{SCHEMES})(?:{IPV6_URL}|{SCHEME_URL_CHARS}+{OPTIONAL_BRACKETED_WORD_SUFFIX})+{NO_TRAILING_PUNCTUATION}"
        );
        let bare = format!("{DOTTED_PATH_LOOKAHEAD}{BARE_RELATIVE_PATH_PREFIX}{PATH_CHARS}+{NO_TRAILING_COLON}");
        format!("{scheme}|{}|{bare}", rooted(ROOTED_OR_RELATIVE_PATH_PREFIX))
    }

    /// The same matcher written so a backtracking engine cannot run away on it (Oniguruma and fancy-regex both
    /// backtrack, and fancy-regex gives up past its backtracking limit): the same matches, every one (the tests hold
    /// it to [`literal`] over url.zig's cases and many random lines), with each ambiguity that cost exponential or
    /// quadratic backtracking taken out:
    ///
    /// - the IPv6 host's `[:0-9a-fA-F]+(?:[:0-9a-fA-F]*)+` is the `[:0-9a-fA-F]+` it spells;
    /// - the scheme URL's `(?:… | chars+ suffix?)+` takes one character a round, `(?:… | chars suffix?)+`: the same
    ///   strings, without the ways of splitting a run into rounds that the trailing-punctuation look-behind made the
    ///   engine try one by one;
    /// - a `[\w\-.]*` run before the `/` that ends it is atomic (`/` is not in it, so giving some back never helps),
    ///   and so is the `$VAR` prefix's run of such segments (with fewer segments a word character follows, never the
    ///   `$` it needs); that prefix starts only at a word's start (inside a word the word's own start matches first)
    ///   and where a `$` follows the segments;
    /// - the bare path's look-behind comes before its dotted look-ahead, so a start inside a word fails at once.
    pub fn bounded() -> String {
        const IPV6_URL: &str = r"(?:\[[:0-9a-fA-F]+\](?::[0-9]+)?)";
        const ROOTED_OR_RELATIVE_PATH_PREFIX: &str = concat!(
            r"(?:\.\./|\./|(?<!\w)~/|(?<!\w)(?=[\w\-./]*\$)(?>(?:\w(?>[\w\-.]*)/)*)(?<!\w)\$[A-Za-z_]\w*/",
            r"|\.\w(?>[\w\-.]*)/|(?<![\w~/])/(?!/))"
        );
        let scheme = format!(
            "(?:{SCHEMES})(?:{IPV6_URL}|{SCHEME_URL_CHARS}{OPTIONAL_BRACKETED_WORD_SUFFIX})+{NO_TRAILING_PUNCTUATION}"
        );
        let bare = format!(r"(?<![$\w]){DOTTED_PATH_LOOKAHEAD}\w(?>[\w\-.]*)/{PATH_CHARS}+{NO_TRAILING_COLON}");
        format!("{scheme}|{}|{bare}", rooted(ROOTED_OR_RELATIVE_PATH_PREFIX))
    }

    /// The rooted or dot-relative path branch after its `prefix`: a dotted path, or an undotted one with spaces.
    fn rooted(prefix: &str) -> String {
        format!(
            "{prefix}(?:{DOTTED_PATH_LOOKAHEAD}{PATH_CHARS}+{DOTTED_PATH_SPACE_SEGMENTS}{NO_TRAILING_COLON}\
             |{NON_DOTTED_PATH_LOOKAHEAD}{PATH_CHARS}+{ANY_PATH_SPACE_SEGMENTS}{NO_TRAILING_COLON})"
        )
    }
}

/// How far from the pointer, in bytes of the line's text either way, a URL or path is looked for: a longer line is
/// searched only around the pointer, cut at blanks, so its length costs nothing beyond this.
const REACH: usize = 2048;

static URL: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&url_zig::bounded())
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
    let span = covering(URL.as_ref()?, &line.text, pointed)?;
    Some(Link {
        url: line.text[span.clone()].to_owned(),
        kind: LinkKind::Url,
        cells: cells_of(&line, span, frame),
    })
}

/// The span of `regex`'s match in `text` that covers byte `pointed`, the matches taken left to right as Ghostty takes
/// them, within [`REACH`] of the pointer ([`reach`]). Should the engine give up on a search (its backtracking limit),
/// the rest of the line is searched one word at a time, a word it gives up on skipped, so no link after it is lost.
fn covering(regex: &Regex, text: &str, pointed: usize) -> Option<std::ops::Range<usize>> {
    let (mut at, end) = reach(text, pointed)?;
    let text = &text[..end];
    let mut by_word = false;
    while at <= pointed && at < text.len() {
        let until = if by_word { word_end(text, at) } else { text.len() };
        match regex.find_from_pos(&text[..until], at) {
            Ok(Some(found)) => {
                let span = found.range();
                if span.start > pointed {
                    return None;
                }
                if span.contains(&pointed) {
                    return Some(span);
                }
                at = if span.is_empty() {
                    text.ceil_char_boundary(span.end + 1)
                } else {
                    span.end
                };
            }
            Ok(None) if !by_word => return None,
            Err(err) if !by_word => {
                tracing::debug!(%err, "the URL matcher gave up on a line; searching it word by word");
                by_word = true;
            }
            // nothing in this word, or a word the engine gives up on: the next one
            Ok(None) | Err(_) => at = until,
        }
    }
    None
}

/// The byte range of `text` a link through byte `pointed` is looked for in: [`REACH`] either way, a cut end moved to a
/// blank so no link is found cut short (`None` when the pointer's own run of text reaches past it).
fn reach(text: &str, pointed: usize) -> Option<(usize, usize)> {
    let start = match pointed.checked_sub(REACH) {
        None => 0,
        Some(cut) => {
            let cut = text.floor_char_boundary(cut);
            if text[..cut].ends_with(char::is_whitespace) {
                cut
            } else {
                let blank = cut + text[cut..].find(char::is_whitespace)?;
                blank + text[blank..].chars().next().map_or(0, char::len_utf8)
            }
        }
    };
    let end = if pointed + REACH >= text.len() {
        text.len()
    } else {
        let cut = text.ceil_char_boundary(pointed + REACH);
        if text[cut..].starts_with(char::is_whitespace) {
            cut
        } else {
            text[..cut].rfind(char::is_whitespace)?
        }
    };
    (start <= pointed && pointed < end).then_some((start, end))
}

/// Where the word at or after byte `at` of `text` ends (the blanks before it skipped): the next blank, else the end.
fn word_end(text: &str, at: usize) -> usize {
    let word = at
        + text[at..]
            .find(|ch: char| !ch.is_whitespace())
            .unwrap_or(text.len() - at);
    word + text[word..].find(char::is_whitespace).unwrap_or(text.len() - word)
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
        ("../example.py ", "../example.py"),
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
        ("./spaces-end.   ", "./spaces-end."),
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

    /// Every match in `text`, as Ghostty's search iterator finds them one after the other.
    fn all_matches(regex: &Regex, text: &str) -> Vec<(usize, usize)> {
        regex
            .find_iter(text)
            .map(|found| {
                let found = found.unwrap_or_else(|err| panic!("{err} in {text:?}"));
                (found.start(), found.end())
            })
            .collect()
    }

    /// Lines of `pieces` (url.zig's alphabet and the fragments its branches turn on), from a fixed seed.
    fn random_lines(count: usize) -> Vec<String> {
        const PIECES: &[&str] = &[
            "a", "b", "1", "_", "-", ".", "/", "~", ":", "$", " ", ",", "(", ")", "[", "]", "?", "#", "@", "%", "f",
            "\"", "é", "https://", "http://", "mailto:", "../", "./", "$H/", "::", "[::1]", "x.y", "//", "  ", "$PWD/",
        ];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            usize::try_from(seed % 1_000_003).expect("fits")
        };
        (0..count)
            .map(|_| {
                let pieces = next() % 14 + 1;
                (0..pieces).map(|_| PIECES[next() % PIECES.len()]).collect()
            })
            .collect()
    }

    #[test]
    fn the_bounded_matcher_matches_ghosttys_pattern_word_for_word() {
        let literal = fancy_regex::RegexBuilder::new(&url_zig::literal())
            .backtrack_limit(usize::MAX)
            .build()
            .expect("the literal pattern compiles");
        let bounded = URL.as_ref().expect("the matcher compiles");
        let cases = CASES.iter().map(|(input, _)| (*input).to_owned());
        let lines = NO_MATCH.iter().map(|input| (*input).to_owned());
        for text in cases.chain(lines).chain(random_lines(40_000)) {
            assert_eq!(all_matches(bounded, &text), all_matches(&literal, &text), "in {text:?}");
        }
    }

    #[test]
    fn the_matcher_never_gives_up_on_a_line_within_reach() {
        // runs of up to twice the reach, each of which takes Ghostty's pattern as written past fancy-regex's
        // backtracking limit: a bearer token (a run tried at every byte), a scheme run of commas (once taken apart
        // round by round), an unclosed IPv6 host, slash and dash segments
        let token: String = "aZ9_-".repeat(400);
        let lines = [
            (
                format!(
                    "curl -H \"Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.{token}.{token}\" https://api.example.com/v1"
                ),
                "https://api.example.com/v1",
            ),
            (
                format!("https://{} https://example.com", ",".repeat(64)),
                "https://example.com",
            ),
            (format!("http://[{} /tmp/x.txt", ":".repeat(2 * REACH)), "/tmp/x.txt"),
            (format!("{} ./notes.md", "a/".repeat(REACH)), "./notes.md"),
            (format!("{} ./notes.md", "-a/".repeat(REACH / 2)), "./notes.md"),
        ];
        let regex = URL.as_ref().expect("the matcher compiles");
        for (text, link) in &lines {
            let found = all_matches(regex, text);
            assert!(
                found.iter().any(|(start, end)| &text[*start..*end] == *link),
                "{link} in {found:?}"
            );
        }
    }

    #[test]
    fn a_line_the_engine_gives_up_on_is_searched_word_by_word() {
        // Ghostty's pattern as written runs past fancy-regex's backtracking limit on the commas
        let literal = Regex::new(&url_zig::literal()).expect("the literal pattern compiles");
        let text = format!("https://{} https://example.com more", ",".repeat(64));
        assert!(literal.find(&text).is_err(), "the engine gives up on the whole line");
        let pointed = text.find("example").expect("in the text");
        let span = covering(&literal, &text, pointed).expect("the URL after the run is found");
        assert_eq!(&text[span], "https://example.com");
    }

    #[test]
    fn a_link_is_looked_for_within_reach_of_the_pointer_cut_at_blanks() {
        let url = format!("https://example.com/{}", "p".repeat(1500));
        let words = "word ".repeat(1000);
        let text = format!("{words}{url} {words}");
        // the whole URL, wherever on it the pointer is
        let start = text.find("https").expect("in the text");
        for pointed in [start, start + 700, start + url.len() - 1] {
            let span = covering(URL.as_ref().expect("compiles"), &text, pointed).expect("a link");
            assert_eq!(&text[span], url, "with the pointer at {pointed}");
        }
        let (from, to) = reach(&text, start).expect("in reach");
        assert!(from > 0 && to < text.len(), "a long line is cut: {from}..{to}");
        assert!(
            text[..from].ends_with(' ') && text[to..].starts_with(' '),
            "cut at blanks"
        );
        // a run of text longer than the reach around the pointer is no link, never a piece of one
        let run = format!("x https://example.com/{} y", "q".repeat(3 * REACH));
        let middle = run.len() / 2;
        assert_eq!(reach(&run, middle), None);
        assert_eq!(covering(URL.as_ref().expect("compiles"), &run, middle), None);
        // a short line is searched whole
        assert_eq!(reach("see ./notes.md", 6), Some((0, 14)));
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
