//! The clipboard writes a client that fell behind gets back with its resync.
//!
//! A client whose queue overflowed is not sent the output it fell behind on (`super::clients`): its resync's
//! snapshot brings its terminal to the session's state, but a clipboard write in that output is no state, so the
//! client's terminal would never apply it, though `ngtermctl clipboard set` (the server's print) or the program had
//! it written and the api answered `Done`. So the pane keeps, per clipboard location, the latest write its terminal
//! completed (OSC 52, OSC 1337 Copy or OSC 5522, every one reaching libghostty-vt's clipboard write effect,
//! [`Recorder`]), with the output offset of the end of the read it completed in and the order it completed in
//! ([`Recovery`]). A client's resync snapshot, and the session's last snapshot for a client that fell behind, carry
//! each location's latest write that completed past where that client's copy of the stream stopped
//! (`Clients::missed_since`), re-encoded as OSC 52 writes, in the order they completed, right after the snapshot's
//! reset: before its parser continuation, which leaves the client's parser inside a sequence. A fresh attach gets none.
//!
//! The contract: a client that fell behind ends with each clipboard's latest value the session wrote while it was
//! behind; values one overwrote meanwhile are not written to it. Only what a frontend applies is kept: a write with a
//! `text/plain` representation (a frontend ignores the others, so one leaves the location's entry as it was); one
//! whose text exceeds [`TEXT_LIMIT`] empties its location's entry instead, so a stale value is never written in its
//! place. A write in the read the client's copy stopped in is written again (the read is the unit of the queue),
//! which a terminal that asks before a write asks once more. The entries do not outlast an update handoff, which
//! disconnects every client anyway.

use std::sync::{Mutex, PoisonError};

use agterm_ghostty_vt as vt;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// The most text a kept write holds: an OSC 52 write the size `ngtermctl clipboard set` takes at most, which the
/// frontend's terminal parses whole.
pub const TEXT_LIMIT: usize = 6 * 1024 * 1024;

/// A clipboard write the session's terminal completed: its location and its text, `None` for one too large to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub location: vt::ClipboardLocation,
    pub text: Option<Vec<u8>>,
}

/// What the terminal's clipboard write effect records, taken by the pane after each read it feeds.
#[derive(Debug, Default)]
pub struct Recorder {
    writes: Mutex<Vec<Written>>,
}

impl Recorder {
    /// Records `request` when a frontend would apply it (it has a `text/plain` representation).
    pub fn record(&self, request: &vt::ClipboardWriteRequest<'_>) {
        let Some(text) = request
            .contents
            .iter()
            .find(|content| content.mime == b"text/plain" || content.mime.starts_with(b"text/plain;"))
        else {
            return;
        };
        let text = (text.data.len() <= TEXT_LIMIT).then(|| text.data.to_vec());
        self.lock().push(Written {
            location: request.location,
            text,
        });
    }

    /// The writes recorded since the last take, in order.
    #[must_use]
    pub fn take(&self) -> Vec<Written> {
        std::mem::take(&mut *self.lock())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Written>> {
        self.writes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One location's latest write.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// When it completed, among every write kept.
    ordinal: u64,
    /// The output offset of the end of the read it completed in.
    end: u64,
    /// The write as OSC 52.
    sequence: Vec<u8>,
}

/// Each clipboard location's latest write.
#[derive(Debug, Default)]
pub struct Recovery {
    /// Standard, selection, primary.
    entries: [Option<Entry>; 3],
    next: u64,
}

impl Recovery {
    /// `writes`, completed in order in the read that ended at the output offset `end`.
    pub fn completed(&mut self, writes: Vec<Written>, end: u64) {
        for written in writes {
            let slot = &mut self.entries[index(written.location)];
            *slot = written.text.map(|text| Entry {
                ordinal: self.next,
                end,
                sequence: osc_52(written.location, &text),
            });
            self.next += 1;
        }
    }

    /// The writes a client whose copy of the stream stopped at `missed_since` did not get: each location's latest that
    /// completed past it, in the order they completed, one after another.
    #[must_use]
    pub fn since(&self, missed_since: u64) -> Vec<u8> {
        let mut entries: Vec<&Entry> = self
            .entries
            .iter()
            .flatten()
            .filter(|entry| entry.end > missed_since)
            .collect();
        entries.sort_by_key(|entry| entry.ordinal);
        entries
            .into_iter()
            .flat_map(|entry| entry.sequence.iter().copied())
            .collect()
    }
}

fn index(location: vt::ClipboardLocation) -> usize {
    match location {
        vt::ClipboardLocation::Standard => 0,
        vt::ClipboardLocation::Selection => 1,
        vt::ClipboardLocation::Primary => 2,
    }
}

/// The OSC 52 write of `text` to `location`, ending in BEL.
fn osc_52(location: vt::ClipboardLocation, text: &[u8]) -> Vec<u8> {
    let name = match location {
        vt::ClipboardLocation::Standard => "c",
        vt::ClipboardLocation::Selection => "s",
        vt::ClipboardLocation::Primary => "p",
    };
    format!("\x1b]52;{name};{}\x07", STANDARD.encode(text)).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(location: vt::ClipboardLocation, text: &str) -> Written {
        Written {
            location,
            text: Some(text.as_bytes().to_vec()),
        }
    }

    #[test]
    fn a_client_gets_each_locations_latest_write_past_its_stop_in_completion_order() {
        let mut recovery = Recovery::default();
        recovery.completed(vec![written(vt::ClipboardLocation::Standard, "old")], 10);
        recovery.completed(
            vec![
                written(vt::ClipboardLocation::Primary, "b"),
                written(vt::ClipboardLocation::Selection, "a"),
            ],
            20,
        );
        recovery.completed(vec![written(vt::ClipboardLocation::Standard, "new")], 30);
        // "b" is Yg==, "a" YQ==: the primary write completed before the selection one, in the same read
        assert_eq!(
            String::from_utf8(recovery.since(10)).unwrap(),
            "\x1b]52;p;Yg==\x07\x1b]52;s;YQ==\x07\x1b]52;c;bmV3\x07"
        );
        assert_eq!(String::from_utf8(recovery.since(20)).unwrap(), "\x1b]52;c;bmV3\x07");
        assert!(recovery.since(30).is_empty(), "nothing past the end");
        // a stop inside the read a write completed in gets it again
        assert_eq!(recovery.since(29), recovery.since(20));
    }

    #[test]
    fn the_order_of_two_locations_in_one_read_is_the_order_they_completed_in() {
        let mut recovery = Recovery::default();
        recovery.completed(
            vec![
                written(vt::ClipboardLocation::Selection, "a"),
                written(vt::ClipboardLocation::Primary, "b"),
            ],
            5,
        );
        assert_eq!(
            String::from_utf8(recovery.since(0)).unwrap(),
            "\x1b]52;s;YQ==\x07\x1b]52;p;Yg==\x07"
        );
    }

    #[test]
    fn a_write_too_large_to_keep_empties_its_location() {
        let mut recovery = Recovery::default();
        recovery.completed(vec![written(vt::ClipboardLocation::Standard, "kept")], 1);
        recovery.completed(
            vec![Written {
                location: vt::ClipboardLocation::Standard,
                text: None,
            }],
            2,
        );
        assert!(recovery.since(0).is_empty(), "never the stale value in its place");
    }

    #[test]
    fn the_recorder_keeps_a_text_write_and_ignores_one_without_text() {
        let recorder = Recorder::default();
        let image = vt::ClipboardWriteRequest {
            location: vt::ClipboardLocation::Standard,
            contents: vec![vt::ClipboardData {
                mime: b"image/png",
                data: b"\x89PNG",
            }],
            name: b"",
        };
        recorder.record(&image);
        let clear = vt::ClipboardWriteRequest {
            location: vt::ClipboardLocation::Standard,
            contents: Vec::new(),
            name: b"",
        };
        recorder.record(&clear);
        let text = vt::ClipboardWriteRequest {
            location: vt::ClipboardLocation::Primary,
            contents: vec![
                vt::ClipboardData {
                    mime: b"image/png",
                    data: b"\x89PNG",
                },
                vt::ClipboardData {
                    mime: b"text/plain;charset=utf-8",
                    data: b"hi",
                },
            ],
            name: b"",
        };
        recorder.record(&text);
        assert_eq!(recorder.take(), [written(vt::ClipboardLocation::Primary, "hi")]);
        assert!(recorder.take().is_empty());
        let large = vec![b'x'; TEXT_LIMIT + 1];
        recorder.record(&vt::ClipboardWriteRequest {
            location: vt::ClipboardLocation::Standard,
            contents: vec![vt::ClipboardData {
                mime: b"text/plain",
                data: &large,
            }],
            name: b"",
        });
        assert_eq!(
            recorder.take(),
            [Written {
                location: vt::ClipboardLocation::Standard,
                text: None
            }]
        );
    }
}
