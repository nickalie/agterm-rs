//! `remotes.conf`, the hosts offered for `zmx tree`/attach (upstream's `Remotes.swift`): one `destination [label...]`
//! per line. Only a whole-line `#` is a comment; the rest of a line after the destination is its label verbatim. A bad
//! line is diagnosed and skipped, and later lines still parse.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use super::tree::is_host;
use crate::keymap::KeymapDiagnostic;

/// One line of `remotes.conf`. The destination is its identity; the label equals it when the line gives none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    pub destination: String,
    pub label: String,
    pub line: usize,
}

/// The parsed `remotes.conf`, entries in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Remotes {
    pub entries: Vec<RemoteEntry>,
}

/// Foundation's `.whitespaces`: horizontal blanks only, never a line break.
fn is_blank(ch: char) -> bool {
    ch.is_whitespace()
        && !matches!(
            ch,
            '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
        )
}

/// Parses `remotes.conf` (upstream's `parseRemotesConf`).
#[must_use]
pub fn parse_remotes_conf(text: &str) -> (Remotes, Vec<KeymapDiagnostic>) {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut diagnostics = Vec::new();
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    for (index, raw) in normalized.split('\n').enumerate() {
        let line_number = index + 1;
        let line = raw.trim_matches(is_blank);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let destination: String = line.chars().take_while(|ch| !ch.is_whitespace()).collect();
        if !is_host(&destination) {
            diagnostics.push(KeymapDiagnostic::new(
                line_number,
                "invalid destination; remote skipped",
            ));
            continue;
        }
        let rest = line[destination.len()..].trim_matches(is_blank);
        if rest
            .chars()
            .any(|ch| (u32::from(ch) < 0x20 && ch != '\t') || u32::from(ch) == 0x7f)
        {
            diagnostics.push(KeymapDiagnostic::new(
                line_number,
                format!("label for '{destination}' has control characters; remote skipped"),
            ));
            continue;
        }
        if !seen.insert(destination.clone()) {
            diagnostics.push(KeymapDiagnostic::new(
                line_number,
                format!("remote '{destination}' is already defined; remote skipped"),
            ));
            continue;
        }
        let label = if rest.is_empty() {
            destination.clone()
        } else {
            rest.to_owned()
        };
        entries.push(RemoteEntry {
            destination,
            label,
            line: line_number,
        });
    }
    (Remotes { entries }, diagnostics)
}

/// Reads `remotes.conf` at `path` (upstream's `RemotesFile.load`). A missing file is an empty list.
///
/// # Errors
///
/// The file exists and cannot be read, which is never mistaken for "no remotes".
pub fn load_remotes(path: &Path) -> io::Result<(Remotes, Vec<KeymapDiagnostic>)> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_remotes_conf(&text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((Remotes::default(), Vec::new())),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(destination: &str, label: &str, line: usize) -> RemoteEntry {
        RemoteEntry {
            destination: destination.to_owned(),
            label: label.to_owned(),
            line,
        }
    }

    #[test]
    fn a_bare_destination_labels_itself_and_the_rest_is_the_label_verbatim() {
        let (remotes, diagnostics) = parse_remotes_conf("studio.local\n");
        assert!(diagnostics.is_empty());
        assert_eq!(remotes.entries, [entry("studio.local", "studio.local", 1)]);
        let (remotes, diagnostics) = parse_remotes_conf("me@192.168.1.33 \t  Mac Studio  #2 \n");
        assert!(diagnostics.is_empty());
        assert_eq!(remotes.entries, [entry("me@192.168.1.33", "Mac Studio  #2", 1)]);
    }

    #[test]
    fn blank_and_whole_line_comments_are_ignored_and_crlf_is_normalized() {
        let text = "# header\r\n\r\n   # indented comment\r\nstudio.local\r\n\r\nmini office mini\r\n";
        let (remotes, diagnostics) = parse_remotes_conf(text);
        assert!(diagnostics.is_empty());
        assert_eq!(
            remotes.entries,
            [
                entry("studio.local", "studio.local", 4),
                entry("mini", "office mini", 6)
            ]
        );
        assert_eq!(parse_remotes_conf(""), (Remotes::default(), Vec::new()));
    }

    #[test]
    fn a_destination_ssh_would_misread_is_skipped_with_its_line() {
        for line in ["-oProxyCommand=evil host", "bad\u{1b}[31mhost", "ho\u{7f}st label"] {
            let (remotes, diagnostics) = parse_remotes_conf(&format!("good\n{line}\nalso-good\n"));
            assert_eq!(
                remotes
                    .entries
                    .iter()
                    .map(|entry| entry.destination.as_str())
                    .collect::<Vec<_>>(),
                ["good", "also-good"]
            );
            assert_eq!(
                diagnostics,
                [KeymapDiagnostic::new(2, "invalid destination; remote skipped")]
            );
        }
    }

    #[test]
    fn a_label_with_control_characters_is_skipped_and_tabs_kept() {
        let (remotes, diagnostics) = parse_remotes_conf("studio \u{1b}[31mred\nmini\ttabbed\tlabel\n");
        assert_eq!(remotes.entries, [entry("mini", "tabbed\tlabel", 2)]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 1);
        assert!(diagnostics[0].message.contains("control characters"));
    }

    #[test]
    fn a_repeated_destination_keeps_the_first_and_labels_may_repeat() {
        let (remotes, diagnostics) = parse_remotes_conf("studio first\nmini first\nstudio second");
        assert_eq!(
            remotes.entries,
            [entry("studio", "first", 1), entry("mini", "first", 2)]
        );
        assert_eq!(
            diagnostics,
            [KeymapDiagnostic::new(
                3,
                "remote 'studio' is already defined; remote skipped"
            )]
        );
    }

    #[test]
    fn loading_reads_a_file_and_takes_a_missing_one_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remotes.conf");
        assert_eq!(load_remotes(&path).unwrap().0, Remotes::default());
        std::fs::write(&path, "studio\n").unwrap();
        assert_eq!(load_remotes(&path).unwrap().0.entries, [entry("studio", "studio", 1)]);
        let unreadable = dir.path().join("dir.conf");
        std::fs::create_dir(&unreadable).unwrap();
        assert!(load_remotes(&unreadable).is_err());
    }
}
