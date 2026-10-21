//! A live session's screen as a read returns it (upstream's `ZmxScreen` in `ZmxLead.swift`): the text of the daemon's
//! own terminal, which always has the leader's layout, cut to what a pane's own surface returns.

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
    fn the_last_lines_drop_trailing_blank_rows_first() {
        let text = "> draft\n\nstatus line\n\n   \n";
        assert_eq!(last_lines(text, 2), "\nstatus line");
        assert_eq!(last_lines(text, 50), "> draft\n\nstatus line");
        assert_eq!(last_lines("", 3), "");
    }
}
