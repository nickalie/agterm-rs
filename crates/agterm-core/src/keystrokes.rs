//! The synthetic keystrokes `session.type` sends (upstream's `KeystrokeSegments.swift`): printable runs and Return
//! keypresses, the final Return of a payload held back by [`SUBMIT_GAP`].

use std::time::Duration;

/// One synthetic keystroke group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeystrokeSegment {
    /// A printable run, sent as one key event's text.
    Text(String),
    /// One Return keypress (press and release).
    Return,
}

/// A payload split around its final Return: `head` goes at once, and with `paced_return` one more Return follows
/// after [`SUBMIT_GAP`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PacedKeystrokes {
    pub head: Vec<KeystrokeSegment>,
    pub paced_return: bool,
}

/// The pause between a payload's text and its final Return: Claude Code takes a Return arriving in the same burst
/// as a long text run as pasted content and does not submit (upstream #679).
pub const SUBMIT_GAP: Duration = Duration::from_millis(10);

/// Holds back the final Return of a payload that ends in a line ending and has text before it; earlier Returns stay
/// in `head`, and a payload of Returns alone is not paced.
#[must_use]
pub fn paced(text: &str) -> PacedKeystrokes {
    let mut head = split(text);
    let has_text = head.iter().any(|segment| matches!(segment, KeystrokeSegment::Text(_)));
    let paced_return = has_text && head.last() == Some(&KeystrokeSegment::Return);
    if paced_return {
        head.pop();
    }
    PacedKeystrokes { head, paced_return }
}

/// Normalizes CRLF and CR line endings to LF, then emits every line ending as exactly one Return.
#[must_use]
pub fn split(text: &str) -> Vec<KeystrokeSegment> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let parts: Vec<&str> = normalized.split('\n').collect();
    let mut segments = Vec::with_capacity(parts.len() * 2);
    for (index, part) in parts.iter().enumerate() {
        if !part.is_empty() {
            segments.push(KeystrokeSegment::Text((*part).to_owned()));
        }
        if index + 1 < parts.len() {
            segments.push(KeystrokeSegment::Return);
        }
    }
    segments
}

/// The same keystrokes as typed bytes for a live-session daemon: the runs as UTF-8 and one CR per Return.
#[must_use]
pub fn pty_bytes(text: &str) -> Vec<u8> {
    split(text)
        .into_iter()
        .flat_map(|segment| match segment {
            KeystrokeSegment::Text(run) => run.into_bytes(),
            KeystrokeSegment::Return => vec![0x0D],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::KeystrokeSegment::{Return, Text};
    use super::*;

    fn text(run: &str) -> KeystrokeSegment {
        Text(run.to_owned())
    }

    #[test]
    fn every_line_ending_is_one_return() {
        assert_eq!(split("ls\n"), [text("ls"), Return]);
        assert_eq!(
            split("a\r\nb\rc\n"),
            [text("a"), Return, text("b"), Return, text("c"), Return]
        );
        assert_eq!(split("\n\n"), [Return, Return]);
        assert_eq!(split("plain"), [text("plain")]);
        assert_eq!(split(""), []);
    }

    #[test]
    fn only_a_final_return_after_text_is_paced() {
        let paced_of = |head: Vec<KeystrokeSegment>, paced_return| PacedKeystrokes { head, paced_return };
        assert_eq!(paced("echo hi\n"), paced_of(vec![text("echo hi")], true));
        assert_eq!(paced("a\nb\n"), paced_of(vec![text("a"), Return, text("b")], true));
        assert_eq!(paced("\n"), paced_of(vec![Return], false));
        assert_eq!(paced("no newline"), paced_of(vec![text("no newline")], false));
    }

    #[test]
    fn pty_bytes_send_cr_per_return() {
        assert_eq!(pty_bytes("a\r\nb\n"), b"a\rb\r");
    }
}
