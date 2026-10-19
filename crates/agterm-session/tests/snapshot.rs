//! Attaching to a running pane on real PTYs: the snapshot a client starts from, and the in-band resync of a client
//! that fell behind. Each client feeds what it is sent into a terminal of its own, as a client's would, which must
//! end where the session's terminal stands: scrollback, screen, styles and cursor. On every OS, except where noted:
//! the tests that need a program to write escape sequences of its own (an alternate screen, a sequence cut in two)
//! run on Unix, as Windows' pseudoconsole re-renders what a program writes; the snapshot's own unit tests cover those
//! on every OS.
//!
//! Every session gets the test's environment without the live agterm's `AGTERM_*` and `ZMX_*` variables, and a
//! namespace directory of its own (only named in its environment: no server runs).

#[expect(dead_code, reason = "the helpers serve the pane, lead and snapshot tests alike")]
mod support;

use std::time::{Duration, Instant};

use agterm_ghostty_vt::{Format, FormatExtras, FormatOptions, Region, Terminal};
use agterm_session::protocol::{LeadRole, Managed};
use agterm_session::server::clients::{ClientEvent, RecvError, ResyncBudget};
use agterm_session::server::pane::{AttachError, Pane, ResyncError};
use agterm_session::server::snapshot::{ATTACH_RESET, SnapshotError};
use support::{Client, PATIENCE, SIZE, TestResult, config, quit, script, shell, tagged, type_line};

/// What a terminal shows: every row with its styles, scrollback included, the cursor and the active screen.
#[derive(Debug, Clone, PartialEq, Eq)]
struct View {
    rows: String,
    cursor: (u16, u16),
    alternate: bool,
}

fn view(terminal: &mut Terminal) -> View {
    let options = FormatOptions {
        format: Format::Vt,
        unwrap: false,
        trim: true,
        extras: FormatExtras::empty(),
    };
    let rows = terminal
        .format(Region::Screen, options)
        .ok()
        .flatten()
        .unwrap_or_default();
    View {
        rows: without_ascii_designations(&String::from_utf8_lossy(&rows)),
        cursor: terminal.cursor(),
        alternate: terminal.alternate_screen(),
    }
}

/// `text` without the ASCII designations of G0–G3 a resync's reset makes: Ghostty prints ASCII and its default set
/// alike, but its formatter writes a designation that differs from the default.
fn without_ascii_designations(text: &str) -> String {
    ["\x1b(B", "\x1b)B", "\x1b*B", "\x1b+B"]
        .iter()
        .fold(text.to_owned(), |text, designation| text.replace(designation, ""))
}

/// Read `client` up to the session's output now and compare what its terminal shows with the session's own terminal,
/// at one offset: retried while the program is still writing.
fn assert_converged(pane: &Pane, client: &mut Client) -> TestResult<View> {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let offset = pane.offset();
        client.drain();
        while client.next < offset && Instant::now() < deadline {
            client.receive(Duration::from_millis(100));
        }
        let server = pane.with_terminal(view);
        if pane.offset() == offset && client.next == offset {
            let terminal = client.terminal.as_mut().ok_or("the client got no snapshot")?;
            let shown = view(terminal);
            assert_eq!(shown, server, "the client's terminal differs from the session's");
            return Ok(shown);
        }
        assert!(Instant::now() < deadline, "the session never settled");
    }
}

/// A program that prints 60 numbered lines, then a prompt, and waits for a line.
fn lines_then_prompt() -> agterm_session::protocol::SessionSpec {
    if cfg!(windows) {
        script("(for /l %i in (1,1,60) do @echo line-%i) & set /p x=prompt^> ")
    } else {
        script("i=1; while [ $i -le 60 ]; do echo line-$i; i=$((i+1)); done; printf 'prompt> '; read x")
    }
}

#[test]
fn a_reattach_shows_the_scrollback_then_the_screen_with_the_cursor_in_place() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("reattach", dir.path(), lines_then_prompt()))?;
    let mut first = Client::new(creator);
    first.expect("prompt>");
    assert!(pane.detach(first.id()));
    let mut later = Client::attach(&pane)?;
    later.receive(PATIENCE);
    assert!(
        matches!(later.events.first(), Some(ClientEvent::Snapshot { epoch: 1, .. })),
        "{:?}",
        later.events
    );
    let shown = assert_converged(&pane, &mut later)?;
    let plain = support::shown(shown.rows.as_bytes());
    assert!(plain.contains("line-1\r\n") || plain.contains("line-1\n"), "{plain:?}");
    assert!(plain.contains("line-60") && plain.contains("prompt>"), "{plain:?}");
    assert_eq!(shown.cursor.1, SIZE.rows - 1, "the prompt sits on the last row");
    assert!(
        shown.cursor.0 >= 7,
        "the cursor is after the prompt: {:?}",
        shown.cursor
    );
    type_line(&pane, "")?;
    later.finish();
    Ok(())
}

#[test]
fn a_session_with_no_output_yet_sends_an_empty_snapshot() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, creator) = Pane::spawn(config("quiet", dir.path(), shell()))?;
    let mut creator = Client::new(creator);
    let mut later = Client::attach(&pane)?;
    for client in [&mut creator, &mut later] {
        client.receive(PATIENCE);
        assert!(
            matches!(
                client.events.first(),
                Some(ClientEvent::Snapshot { epoch: 1, offset: 0, data, .. }) if data.is_empty()
            ),
            "{:?}",
            client.events
        );
    }
    quit(&pane)?;
    creator.finish();
    later.finish();
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_alternate_screen_app_comes_back_on_its_screen() -> TestResult {
    let dir = tempfile::tempdir()?;
    let app = script(
        "printf 'before\\n'; printf '\\033[?1049h\\033[H\\033[2Jfull screen\\033[5;10H\\033[7mhere\\033[0m'; read x; \
         printf '\\033[?1049l'",
    );
    let (pane, creator) = Pane::spawn(config("alternate", dir.path(), app))?;
    let mut first = Client::new(creator);
    first.expect("here");
    let mut later = Client::attach(&pane)?;
    let shown = assert_converged(&pane, &mut later)?;
    assert!(shown.alternate, "the client is on the alternate screen");
    assert_eq!(shown.cursor, (13, 4));
    type_line(&pane, "")?;
    later.finish();
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_cut_mid_sequence_is_restored() -> TestResult {
    let dir = tempfile::tempdir()?;
    // the program stops inside a CSI, then finishes it once a line comes
    let cut = script("printf 'plain \\033[3'; read x; printf '1mRED\\033[0m done'; read y");
    let (pane, creator) = Pane::spawn(config("cut", dir.path(), cut))?;
    let mut first = Client::new(creator);
    let deadline = Instant::now() + PATIENCE;
    while pane.offset() < b"plain \x1b[3".len() as u64 {
        assert!(Instant::now() < deadline, "the program never wrote its half sequence");
        first.receive(Duration::from_millis(50));
    }
    let mut later = Client::attach(&pane)?;
    later.receive(PATIENCE);
    let Some(ClientEvent::Snapshot { data, .. }) = later.events.first() else {
        panic!("no snapshot first: {:?}", later.events);
    };
    assert!(
        data.ends_with(b"\x1b[3"),
        "the snapshot hands the cut sequence on: {data:?}"
    );
    type_line(&pane, "")?;
    later.expect("done");
    let shown = assert_converged(&pane, &mut later)?;
    assert!(
        shown.rows.contains("38;5;1mRED"),
        "the cut sequence styled what followed: {:?}",
        shown.rows
    );
    type_line(&pane, "")?;
    later.finish();
    Ok(())
}

#[cfg(unix)]
#[test]
fn without_a_cut_an_attach_waits_then_fails_without_harm() -> TestResult {
    let dir = tempfile::tempdir()?;
    // a title longer than the terminal hands on, left open until a line comes
    let open = script(
        "printf '\\033]2;'; i=0; while [ $i -lt 20 ]; do printf xxxxxxxxxx; i=$((i+1)); done; read x; printf '\\007ok'; read y",
    );
    let mut config = config("open", dir.path(), open);
    config.continuation_limit = 64;
    config.snapshot_wait = WAIT;
    config.managed = Some(managed("creator", false));
    let (pane, creator) = Pane::spawn(config)?;
    let mut creator = Client::new(creator);
    let deadline = Instant::now() + PATIENCE;
    while pane.offset() < 204 {
        assert!(Instant::now() < deadline, "the program never wrote its title");
        creator.receive(Duration::from_millis(50));
    }
    let started = Instant::now();
    let refused = pane.try_attach(SIZE, Some(&managed("claimer", true)));
    assert!(started.elapsed() >= WAIT, "it waited for a cut");
    assert_eq!(
        refused.err(),
        Some(AttachError::Unavailable(SnapshotError::Unavailable))
    );
    assert_eq!(pane.clients(), 1, "nothing was attached");
    assert_eq!(pane.leader(), Some(creator.id()), "the claim changed nothing");
    // a cut that comes within the wait lets an attach through
    let offset = std::thread::scope(|scope| -> TestResult<u64> {
        let attach = scope.spawn(|| pane.try_attach(SIZE, None).map(|attached| attached.offset));
        std::thread::sleep(WAIT / 10);
        type_line(&pane, "")?;
        Ok(attach.join().map_err(|_| "the attach panicked")??)
    })?;
    assert!(offset > 204, "the attach came after the title's end");
    creator.expect("ok");
    type_line(&pane, "")?;
    creator.finish();
    Ok(())
}

/// How long the open-title session's attaches wait for a cut.
#[cfg(unix)]
const WAIT: Duration = Duration::from_secs(2);

fn managed(nonce: &str, claim: bool) -> Managed {
    Managed {
        nonce: nonce.to_owned(),
        claim,
    }
}

/// The lines a flood chunk prints, each about 60 bytes: one chunk fits in the tests' 16 KiB client queue, two do not.
const FLOOD_LINES: usize = 150;

/// The line that makes [`shell`] print [`FLOOD_LINES`] numbered lines, then [`flood_end`] of `chunk` (which the echoed
/// line does not hold).
fn flood(chunk: usize) -> String {
    if cfg!(windows) {
        format!(
            "(for /l %i in (1,1,{FLOOD_LINES}) do @echo line-%i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx) \
             & echo chunk-{chunk}^-end"
        )
    } else {
        format!(
            "i=0; while [ $i -lt {FLOOD_LINES} ]; do echo line-$i-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; \
             i=$((i+1)); done; echo chunk-{chunk}\"\"-end"
        )
    }
}

fn flood_end(chunk: usize) -> String {
    format!("chunk-{chunk}-end")
}

/// Flood the session with `chunks`, each read by `watcher` before the next is asked for, while the client that is to
/// fall behind reads nothing.
fn flood_past(pane: &Pane, watcher: &mut Client, chunks: std::ops::RangeInclusive<usize>) -> TestResult {
    for chunk in chunks {
        type_line(pane, &flood(chunk))?;
        watcher.expect(&flood_end(chunk));
    }
    Ok(())
}

/// A session of [`shell`] whose managed creator, given a 16 KiB queue and `budget`, leads it, with a watcher
/// attached.
fn slow_session(name: &str, dir: &tempfile::TempDir, budget: ResyncBudget) -> TestResult<(Pane, Client, Client)> {
    let mut config = config(name, dir.path(), shell());
    config.client_queue_limit = 16 * 1024;
    config.resync_budget = budget;
    config.managed = Some(managed("slow", true));
    let (pane, creator) = Pane::spawn(config)?;
    let watcher = Client::attach(&pane)?;
    Ok((pane, Client::new(creator), watcher))
}

#[test]
fn a_slow_client_is_resynced_in_band_and_keeps_its_role() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut slow, mut watcher) = slow_session("resync", &dir, ResyncBudget::default())?;
    flood_past(&pane, &mut watcher, 1..=3)?;
    slow.drain();
    assert!(
        matches!(
            slow.events.as_slice(),
            [
                ClientEvent::Role {
                    role: LeadRole::Leader,
                    generation: 1
                },
                ClientEvent::Overflowed { offset: 0 }
            ]
        ),
        "its role stayed, what it was not sent went: {:?}",
        slow.events
    );
    pane.resync(slow.id())?;
    slow.receive(PATIENCE);
    let Some(ClientEvent::Snapshot { epoch, data, .. }) = slow.events.last() else {
        panic!("no snapshot after the resync: {:?}", slow.events);
    };
    assert_eq!(*epoch, 2, "its first snapshot's epoch was replaced, never sent");
    assert!(data.starts_with(ATTACH_RESET));
    assert!(pane.accepts_reply(slow.id(), 2));
    assert!(
        !pane.accepts_reply(slow.id(), 1),
        "an answer of the superseded epoch is dropped"
    );
    // a resync of a client that did not overflow does nothing
    pane.resync(slow.id())?;
    assert_eq!(slow.epoch, 2);
    type_line(&pane, &tagged("after"))?;
    slow.expect("got:after");
    let shown = assert_converged(&pane, &mut slow)?;
    assert!(support::shown(shown.rows.as_bytes()).contains(&flood_end(3)));
    assert_eq!(pane.role(slow.id()), Some(LeadRole::Leader), "it kept the lead");
    assert_eq!(slow.roles(), [(LeadRole::Leader, 1)], "and was told no other role");
    quit(&pane)?;
    slow.finish();
    watcher.finish();
    Ok(())
}

#[test]
fn a_client_past_its_resync_budget_is_disconnected() -> TestResult {
    let dir = tempfile::tempdir()?;
    let budget = ResyncBudget {
        resyncs: 1,
        window: Duration::from_secs(3600),
    };
    let (pane, mut slow, mut watcher) = slow_session("budget", &dir, budget)?;
    flood_past(&pane, &mut watcher, 1..=3)?;
    pane.resync(slow.id())?;
    // it still reads nothing: the resync's snapshot is dropped unsent with the rest
    flood_past(&pane, &mut watcher, 4..=6)?;
    assert_eq!(pane.resync(slow.id()), Err(ResyncError::OverBudget));
    assert_eq!(pane.clients(), 1, "only the watcher is left");
    assert_eq!(pane.role(slow.id()), None);
    assert_ne!(pane.leader(), Some(slow.id()), "it left the lead");
    while slow.receive(Duration::ZERO) {}
    assert_eq!(slow.subscription.try_recv(), Err(RecvError::Closed));
    assert!(
        !slow
            .events
            .iter()
            .any(|event| matches!(event, ClientEvent::Output { .. } | ClientEvent::Snapshot { .. })),
        "{:?}",
        slow.events
    );
    assert_eq!(pane.resync(slow.id()), Err(ResyncError::Detached));
    quit(&pane)?;
    watcher.finish();
    assert_eq!(pane.resync(slow.id()), Err(ResyncError::Ended));
    Ok(())
}
