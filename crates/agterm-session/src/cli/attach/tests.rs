use super::*;
use crate::attach_tty::input::Answer;
use crate::client::Delegated;
use crate::protocol::{DelegatedReply, DelegatedRequest, GridSize};
use crate::server::replies::DELEGATION_WAIT;

#[test]
fn the_exit_status_is_the_programs_and_unknown_is_success() {
    let exited = |code| ExitStatus::Exited { code, signal: None };
    assert_eq!(exit_code(&exited(0)), 0);
    assert_eq!(exit_code(&exited(7)), 7);
    assert_eq!(exit_code(&exited(0xC000_013A)), EXIT_FAILED);
    assert_eq!(exit_code(&ExitStatus::Unknown), EXIT_OK);
}

#[test]
fn the_budget_holds_the_reader_back_until_output_is_written() {
    let budget = Arc::new(Budget::default());
    budget.take(OUTPUT_BUDGET + 1);
    let (sender, received) = mpsc::channel();
    let waiting = Arc::clone(&budget);
    let reader = thread::spawn(move || {
        waiting.take(10);
        let _ = sender.send(());
    });
    assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    budget.give(OUTPUT_BUDGET + 1);
    assert!(received.recv_timeout(Duration::from_secs(10)).is_ok());
    let _ = reader.join();
}

/// The attach loop's input and relay at injected instants, driven as the loop drives them: an event, then what is
/// due; with no event, a wake at each deadline on the way.
struct Sim {
    relay: Relay,
    keys: Keys,
    now: Instant,
    typed: Vec<u8>,
    /// The relay's queries to the outer terminal.
    written: Vec<u8>,
    replies: Vec<(u64, DelegatedReply)>,
    detached: bool,
}

impl Sim {
    /// A client in a terminal whose probe was answered.
    fn new(detach_key: bool) -> Self {
        let now = Instant::now();
        let mut relay = Relay::new(None, GridSize { cols: 80, rows: 24 }, None);
        let mut fx = Effects::default();
        relay.start(true, now, &mut fx);
        relay.answer(Answer::PrimaryAttributes, now, &mut fx);
        assert!(!relay.probing());
        Self {
            relay,
            keys: Keys::new(detach_key),
            now,
            typed: Vec::new(),
            written: Vec::new(),
            replies: Vec::new(),
            detached: false,
        }
    }

    /// One whose clipboard read was asked and ran out of time: it stays unresolved.
    fn unresolved() -> Self {
        let mut sim = Self::new(true);
        assert!(sim.ask(1));
        sim.idle(DELEGATION_WAIT);
        assert_eq!(sim.replies, [(1, DelegatedReply::Denied)]);
        assert!(!sim.relay.awaiting());
        assert!(sim.relay.clipboard_asked());
        sim.replies.clear();
        sim
    }

    fn effects(&mut self, fx: Effects) {
        self.written.extend(fx.stdout);
        for outgoing in fx.outgoing {
            if let Outgoing::Answer(request, reply) = outgoing {
                self.replies.push((request.id, reply));
            }
        }
    }

    fn tokens(&mut self, tokens: Vec<Token>) {
        let mut fx = Effects::default();
        for token in tokens {
            match token {
                Token::Input(bytes) => self.typed.extend(bytes),
                Token::Answer(answer) => self.relay.answer(answer, self.now, &mut fx),
                Token::Detach => self.detached = true,
            }
        }
        self.effects(fx);
    }

    fn due_at(&mut self, at: Instant) {
        self.now = at;
        let mut fx = Effects::default();
        let tokens = due(&mut self.relay, &mut self.keys, at, &mut fx);
        self.effects(fx);
        self.tokens(tokens);
    }

    /// `wait` passes with no event.
    fn idle(&mut self, wait: Duration) {
        let until = self.now + wait;
        for _ in 0..100 {
            let next = [self.relay.next_deadline(), self.keys.quiet(&self.relay)]
                .into_iter()
                .flatten()
                .min();
            match next {
                Some(next) if next <= until => self.due_at(next.max(self.now)),
                _ => return self.due_at(until),
            }
        }
        panic!("a deadline that never passes");
    }

    /// A read of the outer terminal now.
    fn read(&mut self, bytes: &[u8]) {
        let tokens = self.keys.read(bytes, self.now, &self.relay);
        self.tokens(tokens);
        self.due_at(self.now);
    }

    /// The session reads the clipboard: whether the outer terminal was asked.
    fn ask(&mut self, id: u64) -> bool {
        let request = Delegated {
            id,
            epoch: 1,
            request: DelegatedRequest::ClipboardRead {
                selection: "c".to_owned(),
                list_only: false,
            },
        };
        let mut fx = Effects::default();
        self.relay.delegated(request, self.now, &mut fx);
        let asked = fx.stdout.ends_with(b"\x1b]52;c;?\x1b\\");
        self.effects(fx);
        self.due_at(self.now);
        asked
    }
}

fn text(text: &str) -> DelegatedReply {
    DelegatedReply::Clipboard {
        text: Some(text.to_owned()),
    }
}

/// A pause past the escape time, and one past the wait for a cut answer.
const PAUSES: [Duration; 2] = [
    INPUT_QUIET.saturating_add(Duration::from_millis(10)),
    ANSWER_QUIET.saturating_add(Duration::from_secs(1)),
];

#[test]
fn a_late_clipboard_answer_cut_by_pauses_is_typed_nowhere_and_the_next_read_is_asked() {
    for reattach in [false, true] {
        for pause in PAUSES {
            let what = format!("reattach {reattach}, pause {pause:?}");
            let mut sim = Sim::new(true);
            assert!(sim.ask(1));
            if reattach {
                sim.relay.reattached();
            } else {
                sim.idle(DELEGATION_WAIT);
                assert_eq!(sim.replies, [(1, DelegatedReply::Denied)], "{what}");
            }
            assert!(!sim.relay.awaiting(), "{what}");
            // the user agreed late, and the answer comes over a slow link, its ST cut too
            for part in [b"\x1b]52;c;aGVs".as_slice(), b"bG8", b"=\x1b", b"\\"] {
                sim.read(part);
                sim.idle(pause);
            }
            assert_eq!(String::from_utf8_lossy(&sim.typed), "", "{what}");
            assert!(!sim.relay.clipboard_asked(), "{what}: the late answer was taken");
            sim.replies.clear();
            assert!(sim.ask(2), "{what}: the next read is asked");
            sim.read(b"\x1b]52;c;d29y");
            sim.idle(pause);
            sim.read(b"bGQ=\x07x");
            assert_eq!(sim.replies, [(2, text("world"))], "{what}");
            assert_eq!(sim.typed, b"x", "{what}");
        }
    }
}

#[test]
fn a_live_reads_answer_waits_through_a_long_pause() {
    let mut sim = Sim::new(true);
    assert!(sim.ask(1));
    sim.read(b"\x1b]52");
    sim.idle(PAUSES[1]);
    sim.read(b";c;aGVs");
    sim.idle(PAUSES[1]);
    sim.read(b"bG8=\x1b\\");
    assert_eq!(sim.replies, [(1, text("hello"))]);
    assert_eq!(sim.typed, b"");
    assert!(!sim.relay.clipboard_asked());
}

#[test]
fn an_answer_begun_before_the_deadline_and_ended_after_it_is_discarded() {
    let mut sim = Sim::new(true);
    assert!(sim.ask(1));
    sim.idle(DELEGATION_WAIT.saturating_sub(Duration::from_millis(500)));
    sim.read(b"\x1b]52;c;aGVs");
    sim.idle(PAUSES[1]);
    assert_eq!(
        sim.replies,
        [(1, DelegatedReply::Denied)],
        "denied at its deadline, the answer still held"
    );
    sim.read(b"bG8=\x07");
    assert_eq!(sim.replies, [(1, DelegatedReply::Denied)]);
    assert_eq!(sim.typed, b"");
    assert!(sim.ask(2));
}

#[test]
fn an_oversized_clipboard_answer_is_discarded_up_to_its_end() {
    let mut sim = Sim::new(true);
    assert!(sim.ask(1));
    sim.read(b"\x1b]52;c;");
    let chunk = vec![b'A'; 1024 * 1024];
    for _ in 0..=crate::attach_tty::input::STRING_LIMIT / chunk.len() {
        sim.read(&chunk);
    }
    sim.idle(PAUSES[1]);
    sim.read(b"AAAA\x1b\\x");
    assert_eq!(sim.typed, b"x");
    assert_eq!(sim.replies, [(1, DelegatedReply::Denied)]);
    assert!(sim.ask(2), "the read is resolved");
}

#[test]
fn a_key_inside_a_clipboard_answer_drops_what_was_framed_and_is_read_on_its_own() {
    for read_live in [true, false] {
        let fresh = || {
            let mut sim = if read_live { Sim::new(true) } else { Sim::unresolved() };
            if read_live {
                assert!(sim.ask(1));
            }
            sim.read(b"\x1b]52;c;aGVs");
            sim.idle(PAUSES[1]);
            sim
        };
        // the detach key detaches
        let mut sim = fresh();
        sim.read(b"\x1c");
        assert!(sim.detached, "live {read_live}");
        assert_eq!(sim.typed, b"", "live {read_live}");
        // Enter and an arrow reach the program, the read stays asked
        for key in [b"\r".as_slice(), b"\x1b[A"] {
            let mut sim = fresh();
            sim.read(key);
            assert_eq!(sim.typed, key, "live {read_live}");
            assert!(sim.relay.clipboard_asked(), "live {read_live}");
            assert_eq!(sim.relay.awaiting(), read_live);
            // what comes after it is no framed answer any more: typed (the residual the input's module docs name)
            sim.read(b"bG8=");
            sim.idle(PAUSES[0]);
            assert_eq!(&sim.typed[key.len()..], b"bG8=", "live {read_live}");
        }
    }
    // with the detach key off, Ctrl+\ is a key like Enter
    let mut sim = Sim::new(false);
    assert!(sim.ask(1));
    sim.read(b"\x1b]52;c;aGVs\x1c");
    assert!(!sim.detached);
    assert_eq!(sim.typed, b"\x1c");
}

#[test]
fn keys_and_pastes_keep_their_holds_while_a_clipboard_read_is_unresolved() {
    // Alt+] and an answer-shaped Alt+] 5, a clipboard header cut before its 52: typed after the escape time
    for keys in [b"\x1b]".as_slice(), b"\x1b]5", b"\x1bPx"] {
        let mut sim = Sim::unresolved();
        sim.read(keys);
        assert_eq!(sim.typed, b"");
        sim.idle(PAUSES[0]);
        assert_eq!(sim.typed, keys, "{:?}", String::from_utf8_lossy(keys));
    }
    // readline's M-_ and Enter, vim's Escape, P and an arrow, an OSC fragment and the arrow ending it: typed at once
    for keys in [b"\x1b_\r".as_slice(), b"\x1bPx\x1b[A", b"\x1b]11;xy\x1b[A"] {
        let mut sim = Sim::unresolved();
        sim.read(keys);
        assert_eq!(sim.typed, keys, "{:?}", String::from_utf8_lossy(keys));
    }
    // a cut paste end waits for the rest of the paste
    let mut sim = Sim::unresolved();
    sim.read(b"\x1b[200~abc\x1b[20");
    sim.idle(PAUSES[1]);
    assert_eq!(sim.typed, b"\x1b[200~abc");
    sim.read(b"1~");
    assert_eq!(sim.typed, b"\x1b[200~abc\x1b[201~");
    // with no read asked, what only starts like a clipboard answer is typed after the escape time
    let mut sim = Sim::new(true);
    sim.read(b"\x1b]52;c;aGVs");
    sim.idle(PAUSES[0]);
    assert_eq!(sim.typed, b"\x1b]52;c;aGVs");
}
