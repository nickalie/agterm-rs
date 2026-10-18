//! `remotes.conf`, the hosts offered for `live tree`/attach (upstream's `Remotes.swift`): one `destination [label...]`
//! per line. Only a whole-line `#` is a comment; the rest of a line after the destination is its label verbatim. A bad
//! line is diagnosed and skipped, and later lines still parse.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

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

/// `remotes.conf` as the app follows it (upstream's `RemotesWatcher` without the file watch, which is the frontend's): the
/// entries File ▸ Attach Remote lists and how many problems the file has, both read again on every refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemotesState {
    path: PathBuf,
    entries: Vec<RemoteEntry>,
    issue_count: usize,
}

impl RemotesState {
    /// A state reading `path`, loaded at once without a report.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        let mut state = Self::default();
        state.watch(path);
        state
    }

    /// The file this state reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The entries of the last read; an unreadable file reads as none.
    #[must_use]
    pub fn entries(&self) -> &[RemoteEntry] {
        &self.entries
    }

    /// How many problems the file had at the last read: its parse diagnostics, or one when it exists and cannot be
    /// read.
    #[must_use]
    pub fn issue_count(&self) -> usize {
        self.issue_count
    }

    /// Switches to another file, as a config directory change does; never reported.
    pub fn watch(&mut self, path: PathBuf) {
        self.path = path;
        self.refresh(false);
    }

    /// Reads the file again. The issue count when `reporting` and the read turned a clean file into one with problems,
    /// once per such episode: a file that stays broken is not reported again.
    pub fn refresh(&mut self, reporting: bool) -> Option<usize> {
        let (entries, issues) = match load_remotes(&self.path) {
            Ok((remotes, diagnostics)) => (remotes.entries, diagnostics.len()),
            Err(error) => {
                tracing::debug!(path = %self.path.display(), %error, "remotes.conf could not be read");
                (Vec::new(), 1)
            }
        };
        let turned_bad = self.issue_count == 0 && issues > 0;
        self.entries = entries;
        self.issue_count = issues;
        (reporting && turned_bad).then_some(issues)
    }
}

/// The body of the `remotes.conf` problems banner (upstream's `notifyRemotesDiagnostics`).
#[must_use]
pub fn diagnostics_banner_body(count: usize) -> String {
    format!("{count} issue{} in remotes.conf", if count == 1 { "" } else { "s" })
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

    fn destinations(state: &RemotesState) -> Vec<&str> {
        state.entries().iter().map(|entry| entry.destination.as_str()).collect()
    }

    /// Writes `text` to `path` as an editor saving by replacement does: a sibling renamed over it.
    fn replace(path: &Path, text: &str) {
        let staged = path.with_extension("tmp");
        std::fs::write(&staged, text).unwrap();
        std::fs::rename(&staged, path).unwrap();
    }

    #[test]
    fn a_broken_file_at_start_is_counted_but_not_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remotes.conf");
        std::fs::write(&path, "studio\n-bad\n").unwrap();
        let mut state = RemotesState::at(path);
        assert_eq!(destinations(&state), ["studio"]);
        assert_eq!(state.issue_count(), 1);
        assert_eq!(state.refresh(true), None, "the file was already broken");
    }

    #[test]
    fn one_report_per_clean_to_broken_episode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remotes.conf");
        std::fs::write(&path, "studio\n").unwrap();
        let mut state = RemotesState::at(path.clone());
        assert_eq!(state.issue_count(), 0);
        let edit = |state: &mut RemotesState, text: &str, issues: usize| {
            replace(&path, text);
            let report = state.refresh(true);
            assert_eq!(state.issue_count(), issues, "{text:?}");
            report
        };
        let mut reported = Vec::new();
        reported.extend(edit(&mut state, "studio\n-bad\n", 1));
        reported.extend(edit(&mut state, "studio\n-bad\n-worse\nstudio dup\n", 3));
        reported.extend(edit(&mut state, "studio\n-bad\n", 1));
        assert_eq!(reported, [1], "a file that stays broken is not reported again");
        reported.extend(edit(&mut state, "studio\n", 0));
        reported.extend(edit(&mut state, "-bad\n-worse\n", 2));
        assert_eq!(reported, [1, 2]);
    }

    #[test]
    fn a_quiet_refresh_reports_nothing_but_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remotes.conf");
        let mut state = RemotesState::at(path.clone());
        std::fs::write(&path, "-bad\n").unwrap();
        assert_eq!(state.refresh(false), None);
        assert_eq!(state.issue_count(), 1);
        assert_eq!(state.refresh(true), None, "the quiet read already counted the episode");
    }

    #[test]
    fn a_file_that_cannot_be_read_is_one_issue_and_no_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remotes.conf");
        std::fs::write(&path, "studio\n").unwrap();
        let mut state = RemotesState::at(path.clone());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(state.refresh(true), Some(1));
        assert!(state.entries().is_empty());
        assert_eq!(state.issue_count(), 1);
    }

    #[test]
    fn the_entries_follow_every_kind_of_edit_after_a_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remotes.conf");
        let mut state = RemotesState::at(path.clone());
        assert!(state.entries().is_empty(), "no file lists nothing");
        std::fs::write(&path, "studio Mac Studio\n-bad\n").unwrap();
        state.refresh(true);
        assert_eq!(destinations(&state), ["studio"], "a created file");
        assert_eq!(state.entries()[0].label, "Mac Studio");
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, b"mini\n").unwrap();
        drop(file);
        state.refresh(true);
        assert_eq!(destinations(&state), ["studio", "mini"], "an in-place append");
        replace(&path, "mini\n");
        state.refresh(true);
        assert_eq!(destinations(&state), ["mini"], "an atomic replace");
        std::fs::remove_file(&path).unwrap();
        state.refresh(true);
        assert!(state.entries().is_empty(), "a deleted file");
        assert_eq!(state.issue_count(), 0, "a missing file is no problem");
    }

    #[test]
    fn watching_another_file_reads_it_and_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a.conf");
        let second = dir.path().join("b.conf");
        std::fs::write(&first, "studio\n").unwrap();
        std::fs::write(&second, "mini\n-bad\n").unwrap();
        let mut state = RemotesState::at(first);
        state.watch(second.clone());
        assert_eq!(state.path(), second);
        assert_eq!(destinations(&state), ["mini"]);
        assert_eq!(state.issue_count(), 1);
        assert_eq!(state.refresh(true), None);
    }

    #[test]
    fn the_banner_counts_its_issues() {
        assert_eq!(diagnostics_banner_body(1), "1 issue in remotes.conf");
        assert_eq!(diagnostics_banner_body(2), "2 issues in remotes.conf");
    }
}
