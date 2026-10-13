//! The reply of `zmx screen` (upstream's `ZmxScreen` in `ZmxLead.swift`): a line of six numbers (revision, columns,
//! rows, cursor column, cursor row, alternate screen), then the text of the daemon's own terminal, which always has the
//! leader's layout.

use agterm_host::LiveScreen;

/// The screen `output` carries; `None` for a malformed reply, a zmx without the query included, never a blank screen.
#[must_use]
pub fn parse_screen(output: &str) -> Option<LiveScreen> {
    let (header, text) = output.split_once('\n').unwrap_or((output, ""));
    let fields: Vec<u64> = header
        .split(' ')
        .filter(|field| !field.is_empty())
        .map(|field| field.parse().ok())
        .collect::<Option<_>>()?;
    let [revision, columns, rows, cursor_column, cursor_row, alternate] = fields.as_slice() else {
        return None;
    };
    let narrow = |value: &u64| u32::try_from(*value).ok();
    Some(LiveScreen {
        revision: *revision,
        columns: narrow(columns)?,
        rows: narrow(rows)?,
        cursor_column: narrow(cursor_column)?,
        cursor_row: narrow(cursor_row)?,
        alternate: *alternate != 0,
        text: text.to_owned(),
    })
}

/// The last `count` content lines of `text`, trailing blank rows dropped first, matching what a pane's own surface
/// returns for the same request.
#[must_use]
pub fn last_lines(text: &str, count: usize) -> String {
    let mut rows: Vec<&str> = text.split('\n').collect();
    while rows.last().is_some_and(|row| row.chars().all(char::is_whitespace)) {
        rows.pop();
    }
    rows[rows.len().saturating_sub(count)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_parses_the_header_and_keeps_the_text_verbatim() {
        let screen = parse_screen("42 89 60 2 17 1\n> draft\n\nstatus line\n\n   \n").unwrap();
        assert_eq!(
            screen,
            LiveScreen {
                revision: 42,
                columns: 89,
                rows: 60,
                cursor_column: 2,
                cursor_row: 17,
                alternate: true,
                text: "> draft\n\nstatus line\n\n   \n".to_owned(),
            }
        );
        assert_eq!(last_lines(&screen.text, 2), "\nstatus line");
        assert_eq!(last_lines(&screen.text, 50), "> draft\n\nstatus line");
        assert_eq!(last_lines("", 3), "");
    }

    #[test]
    fn a_blank_screen_is_an_empty_text_not_a_failure() {
        assert_eq!(parse_screen("0 80 24 0 0 0\n").unwrap().text, "");
        assert_eq!(parse_screen("0 80 24 0 0 0").unwrap().text, "");
    }

    #[test]
    fn a_malformed_reply_is_refused() {
        for output in [
            "",
            "80 24\ntext",
            "a b c d e f\ntext",
            "1 2 3 4 5 -6\ntext",
            "1 4294967296 3 4 5 0\ntext",
            "error: session is unresponsive",
        ] {
            assert_eq!(parse_screen(output), None, "{output}");
        }
    }
}
