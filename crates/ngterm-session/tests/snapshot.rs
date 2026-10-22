//! Attaching to a running pane on real PTYs: the snapshot a client starts from, and the in-band resync of a client
//! that fell behind. Each client feeds what it is sent into a terminal of its own, as a client's would, which must
//! end where the session's terminal stands: scrollback, screen, styles and cursor. On every OS, except where noted:
//! the tests that need a program to write escape sequences of its own (an alternate screen, a sequence cut in two)
//! run on Unix, as Windows' pseudoconsole re-renders what a program writes; the snapshot's own unit tests cover those
//! on every OS.
//!
//! Every session gets the test's environment without the live ngterm's `NGTERM_*`, `AGTERM_*` and `ZMX_*` variables,
//! and a namespace directory of its own (only named in its environment: no server runs).

#[expect(dead_code, reason = "the helpers serve the pane, lead and snapshot tests alike")]
mod support;

use std::time::{Duration, Instant};

use ngterm_ghostty_vt::{Format, FormatExtras, FormatOptions, Region, Terminal};
use ngterm_session::protocol::{LeadRole, Managed};
use ngterm_session::server::clients::{ClientEvent, RecvError, ResyncBudget};
use ngterm_session::server::pane::{Pane, ResyncError};
use ngterm_session::server::snapshot::RESYNC_RESET;
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
        rows: String::from_utf8_lossy(&rows).into_owned(),
        cursor: terminal.cursor(),
        alternate: terminal.alternate_screen(),
    }
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
fn lines_then_prompt() -> ngterm_session::protocol::SessionSpec {
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
    use ngterm_session::server::pane::AttachError;
    use ngterm_session::server::snapshot::SnapshotError;

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
        Some(AttachError::Unavailable(SnapshotError::Unavailable)),
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
    assert!(data.starts_with(RESYNC_RESET));
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

/// The slow client's events after its role: its overflow, the session's last snapshot (epoch 2, its first dropped
/// unsent) and the exit; its terminal then shows the session's final one.
fn assert_last_snapshot(pane: &Pane, slow: &mut Client) -> TestResult {
    let events: Vec<_> = slow
        .events
        .iter()
        .filter(|event| !matches!(event, ClientEvent::Role { .. }))
        .collect();
    let [
        ClientEvent::Overflowed { .. },
        ClientEvent::Snapshot { epoch: 2, data, .. },
        ClientEvent::Exit(_),
    ] = events.as_slice()
    else {
        return Err(format!("no last snapshot between the overflow and the exit: {:?}", slow.events).into());
    };
    assert!(data.starts_with(RESYNC_RESET));
    let terminal = slow.terminal.as_mut().ok_or("the client got no snapshot")?;
    assert_eq!(view(terminal), pane.with_terminal(view));
    Ok(())
}

#[test]
fn a_client_behind_when_the_session_ends_gets_its_last_snapshot_before_the_exit() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut slow, mut watcher) = slow_session("last", &dir, ResyncBudget::default())?;
    flood_past(&pane, &mut watcher, 1..=3)?;
    quit(&pane)?;
    watcher.finish();
    // its writer comes to the overflow only after the end: no resync then, the last snapshot is queued instead
    assert_eq!(pane.resync(slow.id()), Err(ResyncError::Ended));
    slow.finish();
    assert_last_snapshot(&pane, &mut slow)?;
    let terminal = slow.terminal.as_mut().ok_or("no terminal")?;
    assert!(support::shown(view(terminal).rows.as_bytes()).contains(&flood_end(3)));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_session_ending_while_a_resync_waits_for_a_cut_gives_the_last_snapshot() -> TestResult {
    use ngterm_session::server::snapshot;

    let dir = tempfile::tempdir()?;
    let mut config = config("waiting", dir.path(), shell());
    config.client_queue_limit = 16 * 1024;
    config.continuation_limit = 64;
    config.snapshot_wait = PATIENCE;
    let (pane, creator) = Pane::spawn(config)?;
    let (mut slow, mut watcher) = (Client::new(creator), Client::attach(&pane)?);
    flood_past(&pane, &mut watcher, 1..=3)?;
    // a title longer than the terminal hands on, left open: no cut until the program ends (the shell's `read` takes
    // the doubled backslash for one)
    type_line(
        &pane,
        "printf '\\\\033]2;'; i=0; while [ $i -lt 20 ]; do printf xxxxxxxxxx; i=$((i+1)); done; read x; exit 3",
    )?;
    let id = slow.id();
    let deadline = Instant::now() + PATIENCE;
    while pane.with_terminal(|terminal| snapshot::cut_available(terminal)) {
        assert!(
            Instant::now() < deadline,
            "the program never wrote its title: {:?}",
            String::from_utf8_lossy(&watcher.output[watcher.output.len().saturating_sub(400)..])
        );
        watcher.receive(Duration::from_millis(50));
    }
    std::thread::scope(|scope| -> TestResult {
        let resync = scope.spawn(|| pane.resync(id));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!resync.is_finished(), "the resync waits for a cut");
        type_line(&pane, "")?;
        assert_eq!(
            resync.join().map_err(|_| "the resync panicked")?,
            Err(ResyncError::Ended)
        );
        Ok(())
    })?;
    assert_eq!(slow.finish(), watcher.finish());
    assert_last_snapshot(&pane, &mut slow)?;
    Ok(())
}

// -- the clipboard writes a client that fell behind gets back

/// The OSC 52 write of `text` to the clipboard `name` (`c`, `s` or `p`), as `ngtermctl clipboard set` prints one.
fn clipboard_write(name: char, text: &str) -> Vec<u8> {
    use base64::Engine as _;
    format!(
        "\x1b]52;{name};{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    )
    .into_bytes()
}

/// The bytes the snapshot `data` carries between its reset and its own state: what the client got back.
fn recovered(data: &[u8]) -> TestResult<Vec<u8>> {
    let after = data
        .strip_prefix(RESYNC_RESET)
        .ok_or("the snapshot does not start with its reset")?;
    let mut recovered = Vec::new();
    let mut rest = after;
    while let Some(write) = rest.strip_prefix(b"\x1b]52;") {
        let end = write.iter().position(|byte| *byte == 0x07).ok_or("an unended write")?;
        recovered.extend_from_slice(&rest[..=(5 + end)]);
        rest = &write[end + 1..];
    }
    Ok(recovered)
}

/// The last snapshot the client received.
fn last_snapshot(client: &Client) -> TestResult<Vec<u8>> {
    client
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            ClientEvent::Snapshot { data, .. } => Some(data.clone()),
            _ => None,
        })
        .ok_or_else(|| format!("no snapshot: {:?}", client.events).into())
}

#[test]
fn a_resync_gives_a_client_each_clipboards_latest_write_it_missed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut slow, mut watcher) = slow_session("clipboard", &dir, ResyncBudget::default())?;
    // printed while it read nothing yet, then overwritten while it was behind: only the latest is its to get
    pane.print(&clipboard_write('c', "first"))?;
    flood_past(&pane, &mut watcher, 1..=2)?;
    pane.print(&clipboard_write('c', "latest"))?;
    pane.print(&clipboard_write('p', "primary"))?;
    flood_past(&pane, &mut watcher, 3..=3)?;
    slow.drain();
    pane.resync(slow.id())?;
    slow.receive(PATIENCE);
    let mut expected = clipboard_write('c', "latest");
    expected.extend(clipboard_write('p', "primary"));
    assert_eq!(
        String::from_utf8_lossy(&recovered(&last_snapshot(&slow)?)?),
        String::from_utf8_lossy(&expected),
        "each clipboard's latest, in the order they were written"
    );
    // the client that kept up was sent each write once, as printed, and a later attach's snapshot carries none
    let sent = String::from_utf8_lossy(&watcher.output).into_owned();
    for text in ["first", "latest", "primary"] {
        let write = String::from_utf8(clipboard_write(if text == "primary" { 'p' } else { 'c' }, text))?;
        assert_eq!(sent.matches(&write).count(), 1, "{write:?} in {sent:?}");
    }
    let mut later = Client::attach(&pane)?;
    later.receive(PATIENCE);
    let fresh = last_snapshot(&later)?;
    assert!(!fresh.starts_with(RESYNC_RESET) && !fresh.windows(5).any(|window| window == b"\x1b]52;"));
    // the resynced client reads on from its snapshot
    type_line(&pane, &tagged("after"))?;
    slow.expect("got:after");
    quit(&pane)?;
    for client in [&mut slow, &mut watcher, &mut later] {
        client.finish();
    }
    Ok(())
}

#[test]
fn a_resync_snapshot_dropped_unsent_leaves_the_missed_writes_to_the_next() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut slow, mut watcher) = slow_session("redrop", &dir, ResyncBudget::default())?;
    flood_past(&pane, &mut watcher, 1..=2)?;
    pane.print(&clipboard_write('c', "missed"))?;
    pane.resync(slow.id())?;
    // the resync's snapshot is dropped unsent with the flood after it
    flood_past(&pane, &mut watcher, 3..=4)?;
    slow.drain();
    pane.resync(slow.id())?;
    slow.receive(PATIENCE);
    assert_eq!(recovered(&last_snapshot(&slow)?)?, clipboard_write('c', "missed"));
    quit(&pane)?;
    slow.finish();
    watcher.finish();
    Ok(())
}

#[test]
fn a_client_behind_when_the_session_ends_gets_the_writes_it_missed_in_its_last_snapshot() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut slow, mut watcher) = slow_session("lastclip", &dir, ResyncBudget::default())?;
    flood_past(&pane, &mut watcher, 1..=2)?;
    pane.print(&clipboard_write('s', "selected"))?;
    quit(&pane)?;
    watcher.finish();
    slow.finish();
    assert_last_snapshot(&pane, &mut slow)?;
    assert_eq!(recovered(&last_snapshot(&slow)?)?, clipboard_write('s', "selected"));
    Ok(())
}

/// The program's own clipboard writes are kept as printed ones are (a Unix program: Windows' pseudoconsole keeps OSC
/// 52 to itself).
#[cfg(unix)]
#[test]
fn a_programs_own_clipboard_write_is_given_back_too() -> TestResult {
    let dir = tempfile::tempdir()?;
    let (pane, mut slow, mut watcher) = slow_session("programclip", &dir, ResyncBudget::default())?;
    flood_past(&pane, &mut watcher, 1..=2)?;
    // "bW9pbg==" is "moin"; then a line the watcher waits for (the shell's `read` takes a doubled backslash for one)
    type_line(&pane, "printf '\\\\033]52;c;bW9pbg==\\\\007'; echo written\"\"-now")?;
    watcher.expect("written-now");
    slow.drain();
    pane.resync(slow.id())?;
    slow.receive(PATIENCE);
    assert_eq!(recovered(&last_snapshot(&slow)?)?, clipboard_write('c', "moin"));
    quit(&pane)?;
    slow.finish();
    watcher.finish();
    Ok(())
}
