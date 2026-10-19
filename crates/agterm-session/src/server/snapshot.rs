//! A session's snapshot: the bytes that bring a client's terminal to the state of the session's terminal at one output
//! offset, so that the session's output from that offset on continues it exactly as it continues the session's own.
//!
//! zmx: `serializeTerminalState`, `writeColorOverrides` and `writePwd` (`src/util.zig`) at `ZMX_REV`
//! (`build/pins.env`), with agterm's zmx patch `0003-first-attach-snapshot` (`build/patches/zmx/`: the reset before a
//! snapshot repeating output the client was already sent, and the parser's unfinished sequence after it), licensed
//! under the MIT License (`NOTICE`), modified: ported over libghostty-vt's C API ([`agterm_ghostty_vt`]'s formatter
//! and stream continuation); the continuation is required, never left out (a snapshot without one is unavailable,
//! [`SnapshotError::Unavailable`]); OSC 133 prompt marks are left as the program wrote them (zmx's `redraw=0` rewrite
//! is for an outer terminal, the attach CLI's to make).
//!
//! # The bytes
//!
//! In order:
//!
//! 1. [`ATTACH_RESET`], only for a client that was already sent some of the session's output (a resync): it ends any
//!    sequence that output was cut in and puts back to its default what the cursor would carry into the snapshot's
//!    text;
//! 2. the program's colour overrides (OSC 10, 11, 12), when it set any;
//! 3. the scrollback, when there is any: its cells in VT, styles included, with no terminal state around them, then
//!    the pen reset and a line feed for each of its rows still on the client's screen and each visible row, so every
//!    scrollback row scrolls into the client's scrollback and the visible rows start from a blank, homed screen (zmx's
//!    two phases; zmx clears with `ESC [2J` instead, which a Ghostty client erases rather than scrolls away, losing
//!    the scrollback's last screenful);
//! 4. the visible rows with the terminal state that restores them ([`FormatExtras::RESTORE`]: the modes, the scrolling
//!    region, the keyboard modes, the cursor with its style, hyperlink, protection, Kitty keyboard flags and
//!    charsets), synchronized output (DECSET 2026) left out: replayed, it would hold off a new client's rendering until
//!    its own timeout;
//! 5. the working directory (OSC 7) and the title (OSC 2), which the formatter does not write;
//! 6. the stream continuation: the unfinished escape sequence or UTF-8 character the session's parser stands in, so
//!    the client's parser takes the next output where the session's does.
//!
//! Skipped cells come out as unstyled blanks (Ghostty patch `0004-formatter-unstyled-blanks`), as upstream's
//! re-attach writes them. On the alternate screen the snapshot is that screen's, as zmx's: the primary screen the
//! program will return to is not carried.
//!
//! # Where to cut
//!
//! A snapshot needs the continuation, which the session's terminal tracks, from before its first byte, up to a limit
//! ([`CONTINUATION_LIMIT`] by default, `Terminal::set_continuation_limit`). Past that (a long OSC 52 write, a large Kitty image sent inline) the cut is unavailable until the sequence ends:
//! [`cut_available`] says whether a snapshot can be taken now, and the pane waits for one within a deadline.

use agterm_ghostty_vt::{
    Continuation, Format, FormatExtras, FormatOptions, Mode, Point, PointTag, Region, Rgb, Terminal,
};

/// The longest unfinished sequence the session's terminal keeps for a snapshot: the largest default buffer of the
/// library's APC protocols (`apc.Protocol.maxDefaultBytes`, 65 MiB for a Kitty image), the limit zmx's patch and the
/// library's own snapshots use.
pub const CONTINUATION_LIMIT: usize = 65 * 1024 * 1024;

/// The most snapshot bytes one `SnapshotChunk` carries.
pub const CHUNK_SIZE: usize = 64 * 1024;

/// What a client already sent some of the session's output gets ahead of a snapshot that repeats it (zmx patch
/// `0003-first-attach-snapshot`'s `ATTACH_RESET`). In order: CAN ends any sequence the output was cut in; SGR 0 resets
/// the pen (an erase paints with its background); OSC 8 closes a hyperlink; G0–G3 designate ASCII and SI shifts G0
/// in; DECSCA 1 then 0 drops protection (the 1 switches from ISO protection, which even unprotected erases honor, to
/// DEC); IRM goes; left/right margin mode, origin mode and the scrolling region go, so the cursor homes to the top
/// left; and the screen below it is erased (ED 0: ED 2 would push it into a Ghostty client's scrollback). The snapshot
/// then restores the session's own state. Residues: a string sequence (OSC, DCS, APC) cut there is dispatched early
/// by a Ghostty client; tab stops, Kitty images and a saved cursor the output set stay; output that scrolled is in the
/// client's scrollback and comes again with the snapshot's history.
pub const ATTACH_RESET: &[u8] =
    b"\x18\x1b[0m\x1b]8;;\x1b\\\x1b(B\x1b)B\x1b*B\x1b+B\x0f\x1b[1\"q\x1b[0\"q\x1b[4l\x1b[?69l\x1b[?6l\x1b[r\x1b[H\x1b[J";

/// Why no snapshot was taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// The parser stands in a sequence longer than [`CONTINUATION_LIMIT`], or the terminal tracks none: no
    /// consistent cut now.
    #[error("no consistent snapshot can be cut now: the session's terminal is inside a sequence it cannot hand on")]
    Unavailable,
    /// The formatter failed.
    #[error("the session's terminal could not be formatted: {0}")]
    Format(String),
}

/// Whether a snapshot of `terminal` can be taken now: its continuation is known.
#[must_use]
pub fn cut_available(terminal: &Terminal) -> bool {
    // at ground the continuation is empty: no copy of a long one is made to find out
    (terminal.at_ground() && terminal.continuation_limit() > 0)
        || matches!(terminal.continuation(), Ok(Continuation::Bytes(_)))
}

/// The snapshot of `terminal` (module docs), led by [`ATTACH_RESET`] when `reset`. The terminal is left as it was.
///
/// # Errors
///
/// [`SnapshotError::Unavailable`] when no cut is available ([`cut_available`]); [`SnapshotError::Format`] when the
/// formatter fails.
pub fn capture(terminal: &mut Terminal, reset: bool) -> Result<Vec<u8>, SnapshotError> {
    let continuation = match terminal.continuation() {
        Ok(Continuation::Bytes(bytes)) => bytes,
        Ok(Continuation::Unavailable) | Err(_) => return Err(SnapshotError::Unavailable),
    };
    let mut out = Vec::new();
    if reset {
        out.extend_from_slice(ATTACH_RESET);
    }
    color_overrides(terminal, &mut out);
    let synchronized = terminal.mode(Mode::SYNC_OUTPUT);
    if synchronized {
        set_synchronized(terminal, false)?;
    }
    let cells = scrollback(terminal, &mut out).and_then(|()| visible(terminal, &mut out));
    if synchronized {
        set_synchronized(terminal, true)?;
    }
    cells?;
    let pwd = terminal.pwd();
    if !pwd.is_empty() {
        out.extend_from_slice(format!("\x1b]7;{pwd}\x1b\\").as_bytes());
    }
    let title = terminal.title();
    if !title.is_empty() {
        out.extend_from_slice(format!("\x1b]2;{title}\x07").as_bytes());
    }
    out.extend_from_slice(&continuation);
    Ok(out)
}

fn set_synchronized(terminal: &mut Terminal, value: bool) -> Result<(), SnapshotError> {
    terminal
        .set_mode(Mode::SYNC_OUTPUT, value)
        .map_err(|err| SnapshotError::Format(err.to_string()))
}

/// The foreground, background and cursor colours the program set (OSC 10, 11, 12) where they differ from the
/// terminal's defaults, as zmx writes them. The palette is not carried, as zmx's is not.
fn color_overrides(terminal: &Terminal, out: &mut Vec<u8>) {
    let current = terminal.colors();
    let defaults = terminal.default_colors();
    for (number, color, default) in [
        (10, current.foreground, defaults.foreground),
        (11, current.background, defaults.background),
        (12, current.cursor, defaults.cursor),
    ] {
        if let Some(Rgb { r, g, b }) = color.filter(|color| Some(*color) != default) {
            out.extend_from_slice(format!("\x1b]{number};rgb:{r:02x}/{g:02x}/{b:02x}\x1b\\").as_bytes());
        }
    }
}

/// The scrollback's cells, then what scrolls them all into the client's scrollback and leaves a blank screen with the
/// cursor home; nothing without a scrollback.
fn scrollback(terminal: &mut Terminal, out: &mut Vec<u8>) -> Result<(), SnapshotError> {
    let history = terminal.total_rows().saturating_sub(usize::from(terminal.rows()));
    if history == 0 {
        return Ok(());
    }
    let last_row = u32::try_from(history - 1).unwrap_or(u32::MAX);
    let last_col = terminal.cols().saturating_sub(1);
    let region = Region::Range {
        start: Point::new(PointTag::History, 0, 0),
        end: Point::new(PointTag::History, last_col, last_row),
        rectangle: false,
    };
    let cells = format(terminal, region, FormatExtras::empty())?;
    out.extend_from_slice(&cells);
    out.extend_from_slice(&scroll_away(history, &cells, terminal.rows()));
    Ok(())
}

/// What follows the scrollback's `cells` (its `history` rows, one per line, trailing blank rows trimmed) on a client of
/// `rows` rows: the pen reset (a scrolled-in row takes the pen's background), the trimmed rows, and a line feed per
/// row of the screen, which scrolls exactly the rows written into the scrollback, whether or not they filled the
/// screen, then the cursor home.
fn scroll_away(history: usize, cells: &[u8], rows: u16) -> Vec<u8> {
    let written = cells.windows(2).filter(|pair| pair == b"\r\n").count() + 1;
    let feeds = history.saturating_sub(written) + usize::from(rows);
    let mut out = b"\x1b[0m".to_vec();
    out.extend(std::iter::repeat_n(*b"\r\n", feeds).flatten());
    out.extend_from_slice(b"\x1b[H");
    out
}

/// The visible rows with the state that restores them.
fn visible(terminal: &mut Terminal, out: &mut Vec<u8>) -> Result<(), SnapshotError> {
    let region = Region::Range {
        start: Point::new(PointTag::Active, 0, 0),
        end: Point::new(
            PointTag::Active,
            terminal.cols().saturating_sub(1),
            u32::from(terminal.rows().saturating_sub(1)),
        ),
        rectangle: false,
    };
    out.extend_from_slice(&format(terminal, region, FormatExtras::RESTORE)?);
    Ok(())
}

/// `region` in VT as zmx's formatter writes it: rows as laid out, trailing whitespace trimmed.
fn format(terminal: &mut Terminal, region: Region, extras: FormatExtras) -> Result<Vec<u8>, SnapshotError> {
    let options = FormatOptions {
        format: Format::Vt,
        unwrap: false,
        trim: true,
        extras,
    };
    terminal
        .format(region, options)
        .map_err(|err| SnapshotError::Format(err.to_string()))
        .map(Option::unwrap_or_default)
}

/// `data` in the chunks a snapshot is sent in, each at most [`CHUNK_SIZE`] bytes; none for an empty snapshot.
pub fn chunks(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    data.chunks(CHUNK_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLS: u16 = 40;
    const ROWS: u16 = 6;

    fn tracked() -> Terminal {
        let mut terminal = Terminal::new(COLS, ROWS).unwrap();
        terminal.set_continuation_limit(CONTINUATION_LIMIT).unwrap();
        terminal
    }

    /// A fresh client terminal of the session's size fed `bytes`.
    fn client(bytes: &[u8]) -> Terminal {
        let mut terminal = Terminal::new(COLS, ROWS).unwrap();
        terminal.vt_write(bytes);
        terminal
    }

    fn plain(terminal: &mut Terminal, region: Region) -> String {
        let options = FormatOptions {
            format: Format::Plain,
            unwrap: false,
            trim: true,
            extras: FormatExtras::empty(),
        };
        String::from_utf8(terminal.format(region, options).unwrap().unwrap_or_default()).unwrap()
    }

    /// The cells with their styles, and the cursor, modes and title around them.
    fn state(terminal: &mut Terminal) -> (String, (u16, u16), bool, String) {
        let options = FormatOptions {
            format: Format::Vt,
            unwrap: false,
            trim: true,
            extras: FormatExtras::RESTORE,
        };
        let screen = terminal.format(Region::Viewport, options).unwrap().unwrap_or_default();
        (
            String::from_utf8_lossy(&screen).into_owned(),
            terminal.cursor(),
            terminal.alternate_screen(),
            terminal.title(),
        )
    }

    fn snapshot(terminal: &mut Terminal) -> Vec<u8> {
        capture(terminal, false).unwrap()
    }

    #[test]
    fn a_reattach_shows_the_scrollback_then_the_screen_with_the_cursor_in_place() {
        let mut session = tracked();
        for line in 1..=20 {
            session.vt_write(format!("line {line}\r\n").as_bytes());
        }
        session.vt_write(b"\x1b[1;32mprompt\x1b[0m $ ab\x1b[2D");
        let bytes = snapshot(&mut session);
        let mut client = client(&bytes);
        assert_eq!(state(&mut client), state(&mut session));
        assert_eq!(client.cursor(), (9, 5), "{:?}", String::from_utf8_lossy(&bytes));
        assert_eq!(
            plain(&mut client, Region::Screen),
            plain(&mut session, Region::Screen),
            "the scrollback came along"
        );
        assert!(plain(&mut client, Region::Screen).starts_with("line 1\n"));
        // what follows lands the same way
        session.vt_write(b"X\r\nnext");
        client.vt_write(b"X\r\nnext");
        assert_eq!(state(&mut client), state(&mut session));
    }

    #[test]
    fn every_scrollback_row_comes_along_however_many_and_blank_or_not() {
        // one row, fewer than a screen, more, and blank rows at the scrollback's end
        for written in [
            b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\ng".to_vec(),
            b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9".to_vec(),
            [b"x".as_slice(), &b"\r\n".repeat(13), b"y"].concat(),
            [b"\x1b[44mblue".as_slice(), &b"\r\n".repeat(9), b"z"].concat(),
        ] {
            let mut session = tracked();
            session.vt_write(&written);
            let mut client = client(&snapshot(&mut session));
            assert_eq!(client.total_rows(), session.total_rows(), "{written:?}");
            assert_eq!(
                plain(&mut client, Region::Screen),
                plain(&mut session, Region::Screen),
                "{written:?}"
            );
            assert_eq!(state(&mut client), state(&mut session), "{written:?}");
        }
    }

    #[test]
    fn a_screen_without_scrollback_carries_no_line_feeds_of_its_own() {
        let mut session = tracked();
        session.vt_write(b"one\r\ntwo");
        let bytes = snapshot(&mut session);
        assert!(!bytes.windows(2).any(|pair| pair == b"\r\n\r\n"), "{bytes:?}");
        let mut client = client(&bytes);
        assert_eq!(state(&mut client), state(&mut session));
    }

    #[test]
    fn an_alternate_screen_app_comes_back_on_its_screen() {
        let mut session = tracked();
        session.vt_write(
            b"shell line\r\n$ vim\r\n\x1b[?1049h\x1b[H\x1b[2Jfile text\x1b[3;5H\x1b[7mstatus\x1b[0m\x1b[2;2H",
        );
        let bytes = snapshot(&mut session);
        let mut client = client(&bytes);
        assert!(client.alternate_screen());
        assert_eq!(state(&mut client), state(&mut session));
        assert_eq!(client.cursor(), (1, 1));
        // the app's next output lands the same way on its screen
        session.vt_write(b"typed");
        client.vt_write(b"typed");
        assert_eq!(state(&mut client), state(&mut session));
        // its exit leaves it, to a primary screen the snapshot did not carry (zmx's limit too)
        client.vt_write(b"\x1b[?1049l");
        assert!(!client.alternate_screen());
    }

    #[test]
    fn color_overrides_are_kept_and_defaults_are_not_written() {
        let mut session = tracked();
        session.vt_write(b"\x1b]11;rgb:12/34/56\x07\x1b]12;#abcdef\x07text");
        let bytes = snapshot(&mut session);
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.starts_with("\x1b]11;rgb:12/34/56\x1b\\\x1b]12;rgb:ab/cd/ef\x1b\\"),
            "{text:?}"
        );
        assert!(!text.contains("\x1b]10;"), "{text:?}");
        let client = client(&bytes);
        assert_eq!(client.colors().background, session.colors().background);
        assert_eq!(client.colors().cursor, session.colors().cursor);
        // a theme default is the client's to set, not the snapshot's
        let mut themed = tracked();
        themed
            .set_default_colors(Some(Rgb::new(1, 1, 1)), Some(Rgb::new(2, 2, 2)), None)
            .unwrap();
        themed.vt_write(b"\x1b]10;rgb:09/09/09\x07");
        let text = String::from_utf8_lossy(&snapshot(&mut themed)).into_owned();
        assert!(text.starts_with("\x1b]10;rgb:09/09/09\x1b\\"), "{text:?}");
        assert!(!text.contains("\x1b]11;"), "{text:?}");
    }

    #[test]
    fn synchronized_output_is_not_replayed_and_stays_on_in_the_session() {
        let mut session = tracked();
        session.vt_write(b"\x1b[?2004h\x1b[?2026hhello");
        let bytes = snapshot(&mut session);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\x1b[?2004h"), "{text:?}");
        assert!(!text.contains("2026"), "{text:?}");
        assert!(session.mode(Mode::SYNC_OUTPUT), "the session's own mode is untouched");
        let client = client(&bytes);
        assert!(!client.mode(Mode::SYNC_OUTPUT));
        assert!(client.mode(Mode::dec(2004)));
    }

    #[test]
    fn blank_runs_come_back_unstyled() {
        let mut session = tracked();
        session.vt_write(b"\x1b[41mAB\x1b[3CX\x1b[0m");
        let text = String::from_utf8_lossy(&snapshot(&mut session)).into_owned();
        assert!(text.contains("AB\x1b[0m   \x1b["), "{text:?}");
        assert!(!text.contains("AB   "), "{text:?}");
    }

    #[test]
    fn the_title_and_working_directory_come_along() {
        let mut session = tracked();
        session.vt_write(b"\x1b]2;my title\x07\x1b]7;file://host/tmp/dir\x07text");
        let bytes = snapshot(&mut session);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\x1b]2;my title\x07"), "{text:?}");
        assert!(text.contains("\x1b]7;file://host/tmp/dir\x1b\\"), "{text:?}");
        assert!(!bytes.contains(&0), "no NUL sentinel");
        let client = client(&bytes);
        assert_eq!(client.title(), "my title");
        assert_eq!(client.pwd(), session.pwd());
        // none set, none written
        let mut bare = tracked();
        bare.vt_write(b"text");
        let text = String::from_utf8_lossy(&snapshot(&mut bare)).into_owned();
        assert!(!text.contains("\x1b]2;") && !text.contains("\x1b]7;"), "{text:?}");
    }

    /// The session wrote `prefix`; a client restored from its snapshot and the session both take `suffix`: they end
    /// in the same state.
    fn assert_cut_resumes(prefix: &[u8], suffix: &[u8]) {
        let mut session = tracked();
        session.vt_write(prefix);
        let bytes = snapshot(&mut session);
        let mut client = client(&bytes);
        session.vt_write(suffix);
        client.vt_write(suffix);
        assert_eq!(
            state(&mut client),
            state(&mut session),
            "prefix {prefix:?}, suffix {suffix:?}"
        );
    }

    #[test]
    fn a_cut_mid_sequence_is_restored() {
        assert_cut_resumes(b"plain \x1b[3", b"1mRED\x1b[0m $ ");
        assert_cut_resumes(b"\x1b]2;my ti", b"tle\x07READY $ ");
        assert_cut_resumes(b"price \xe2\x82", b"\xac $ ");
        assert_cut_resumes(b"dcs \x1bP$q", b"m\x1b\\ $ ");
        assert_cut_resumes(b"x\x1b[2\n", b"Cy");
        let mut session = tracked();
        session.vt_write(b"plain \x1b[3");
        assert!(snapshot(&mut session).ends_with(b"\x1b[3"));
    }

    #[test]
    fn an_overlong_sequence_leaves_no_cut_until_it_ends() {
        let mut session = Terminal::new(COLS, ROWS).unwrap();
        session.set_continuation_limit(16).unwrap();
        session.vt_write(b"\x1b]2;");
        session.vt_write(&[b'x'; 64]);
        assert!(!cut_available(&session));
        assert_eq!(capture(&mut session, false), Err(SnapshotError::Unavailable));
        session.vt_write(b"\x07after");
        assert!(cut_available(&session));
        assert!(capture(&mut session, false).is_ok());
        // a terminal that tracks nothing has no cut either
        let mut untracked = Terminal::new(COLS, ROWS).unwrap();
        assert!(!cut_available(&untracked));
        assert_eq!(capture(&mut untracked, false), Err(SnapshotError::Unavailable));
    }

    #[test]
    fn a_resync_resets_what_the_client_was_sent_before_its_snapshot() {
        // the client was sent output up to a cut inside styled text, an open hyperlink and a CSI; then its snapshot
        let sent: &[u8] = b"line\r\n\x1b[1;44m\x1b]8;;http://example.com\x1b\\link\x1b[4h\x1b[3";
        let mut session = tracked();
        session.vt_write(sent);
        session.vt_write(b"2mmore\x1b[0m\x1b]8;;\x1b\\\x1b[4l $ ");
        let mut resynced = client(sent);
        let bytes = capture(&mut session, true).unwrap();
        assert!(bytes.starts_with(ATTACH_RESET));
        resynced.vt_write(&bytes);
        let mut fresh = client(&capture(&mut session, false).unwrap());
        assert_eq!(reset_state(&mut resynced), state(&mut fresh));
        assert_eq!(reset_state(&mut resynced), state(&mut session));
        session.vt_write(b"next");
        resynced.vt_write(b"next");
        assert_eq!(reset_state(&mut resynced), state(&mut session));
    }

    /// [`state`] of a terminal the reset went through, without the ASCII designations of G0–G3 it made: Ghostty
    /// prints ASCII and its default set alike, but its formatter writes a designation that differs from the default
    /// (zmx's test helper `withoutAsciiDesignations`).
    fn reset_state(terminal: &mut Terminal) -> (String, (u16, u16), bool, String) {
        let (mut screen, cursor, alternate, title) = state(terminal);
        for designation in ["\x1b(B", "\x1b)B", "\x1b*B", "\x1b+B"] {
            screen = screen.replace(designation, "");
        }
        (screen, cursor, alternate, title)
    }

    #[test]
    fn chunks_are_bounded() {
        let data = vec![b'x'; CHUNK_SIZE * 2 + 1];
        let sizes: Vec<usize> = chunks(&data).map(<[u8]>::len).collect();
        assert_eq!(sizes, [CHUNK_SIZE, CHUNK_SIZE, 1]);
        assert_eq!(chunks(&[]).count(), 0);
    }
}
