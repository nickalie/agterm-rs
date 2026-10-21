//! The session server end to end, on every OS, through the `agterm-session` binary and the client library together:
//! the server is started by the binary's `attach`, detached from it and from the test, and every later step reaches
//! it over its endpoint as agterm's own clients do. A session is created, attached to, typed into, read with `screen`
//! and detached from; a client killed with its whole process tree leaves the session and the server running, and a
//! reattach through the library starts from a snapshot that shows the very screen `screen` read; `type` reaches the
//! attached client, the client's input reaches `screen`; a binary of another build hands the server over on its
//! attach, keeping the screen; `print` reaches an attached client; `list`, `kill` (a program that ignores the hangup
//! outlives it, on Unix) and `kill --force` end it all, and the server stops by itself.
//!
//! Each test has a private namespace that only `AGTERM_LIVE_DIR` names, every child the test's environment without
//! the live agterm's `AGTERM_*` and `ZMX_*` variables (`binary::Namespace`); its sessions are killed at its end.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach, api, client, host, cli and e2e tests alike"
)]
mod support;

#[expect(dead_code, reason = "the helpers serve the cli and e2e tests alike")]
mod binary;

use std::path::Path;

use agterm_ghostty_vt::Terminal;
use agterm_session::client::{AttachEvent, AttachReader, Attachment, Client};
use agterm_session::protocol::{AttachRequest, GridSize, ReplyMetadata, Screen, Welcome};
use agterm_session::server::api::read_screen;
use binary::{Answers, COLS, Namespace, ROWS, spec, stage, stderr, stdout, with_shell};
use support::{TestResult, shown, tagged};

/// The session the round trip keeps alive.
const NAME: &str = "e2e";

/// What a terminal's screen shows, as `screen` prints it without its revision: `cols rows x y alternate`, then the
/// text.
fn view(screen: &Screen) -> String {
    format!(
        "{} {} {} {} {}\n{}",
        screen.columns,
        screen.rows,
        screen.cursor_column,
        screen.cursor_row,
        u8::from(screen.alternate),
        screen.text
    )
}

/// `screen [--all] <name>` through the binary, until its text holds `needle`; what it printed without the revision
/// (as [`view`] puts it).
fn screen_showing(namespace: &Namespace, name: &str, all: bool, needle: &str) -> String {
    let mut args = vec!["screen"];
    if all {
        args.push("--all");
    }
    args.push(name);
    support::wait_for(
        &format!("{needle:?} on the screen of {name}"),
        || {
            namespace
                .run(&args, b"", &[])
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| Some(stdout(&output).split_once(' ')?.1.to_owned()))
                .unwrap_or_default()
        },
        |screen| screen.split_once('\n').is_some_and(|(_, text)| text.contains(needle)),
    )
}

/// Waits for `list` through the binary to show `name` with `clients` clients attached.
fn wait_for_clients(namespace: &Namespace, name: &str, clients: u32) {
    let prefix = format!("name={name}\t");
    let count = format!("\tclients={clients}\t");
    support::wait_for(
        &format!("{name} with {clients} clients in the list"),
        || {
            namespace
                .run(&["list"], b"", &[])
                .map(|output| stdout(&output))
                .unwrap_or_default()
        },
        |list| {
            list.lines()
                .any(|line| line.starts_with(&prefix) && line.contains(&count))
        },
    );
}

/// `list --short` through the binary.
fn short_list(namespace: &Namespace) -> TestResult<String> {
    let listed = namespace.run(&["list", "--short"], b"", &[])?;
    assert_eq!(listed.status.code(), Some(0), "{}", stderr(&listed));
    Ok(stdout(&listed))
}

/// The running server, asked through the library, and the same server as the binary's `version` reports it.
fn running_server(namespace: &Namespace) -> TestResult<Welcome> {
    let server = namespace.server()?.ok_or("no server runs")?;
    let version = namespace.run(&["version"], b"", &[])?;
    assert_eq!(version.status.code(), Some(0), "{}", stderr(&version));
    let line = format!("server\t{} (pid {})\n", server.build, server.pid);
    assert!(stdout(&version).contains(&line), "{}", stdout(&version));
    Ok(server)
}

/// Whether a process `pid` runs on this host.
fn runs(pid: u32) -> TestResult<bool> {
    let table = agterm_session::job::read_table().ok_or("no process table")?;
    Ok(table.iter().any(|entry| u32::try_from(entry.record.pid) == Ok(pid)))
}

/// Waits for the process `pid` to end.
fn wait_for_exit(pid: u32, what: &str) {
    support::wait_for(what, || runs(pid).unwrap_or(true), |running| !running);
}

/// The text a terminal shows of what `reader` sends until `done` holds for it.
fn read_until(reader: &mut AttachReader, done: impl Fn(&str) -> bool) -> TestResult<String> {
    let mut bytes = Vec::new();
    while !done(&shown(&bytes)) {
        match reader.recv()? {
            Some(AttachEvent::Output { data, .. } | AttachEvent::SnapshotChunk(data)) => bytes.extend_from_slice(&data),
            Some(AttachEvent::Exit(status)) => return Err(format!("the session ended: {status:?}").into()),
            Some(_) => {}
            None => return Err(format!("the stream ended: {:?}", shown(&bytes)).into()),
        }
    }
    Ok(shown(&bytes))
}

fn enter() -> &'static str {
    if cfg!(windows) { "\r" } else { "\n" }
}

#[test]
fn a_session_outlives_its_clients_and_an_update_through_the_binary_and_the_library() -> TestResult {
    let staged = tempfile::tempdir()?;
    let mut namespace = Namespace::new()?;
    namespace.executable = stage(staged.path(), "e2e-a")?;
    assert!(namespace.server()?.is_none(), "a server before the first attach");

    let server = attach_type_read_and_detach(&namespace)?;
    let screen = attach_and_kill_the_client(&namespace, &server)?;
    let attachment = reattach_to_the_same_screen(&namespace, &screen)?;
    type_both_ways(&namespace, attachment)?;
    // the update before `print`: on Windows the new server resizes the pseudoconsole it took over, which repaints
    // its own screen over what `print` wrote into the session's terminal and never into the pseudoconsole's
    hand_over_on_an_attach_of_another_build(&namespace, staged.path(), &server)?;
    print_into_an_attached_client(&namespace)?;

    let killed = namespace.run(&["kill", NAME], b"", &[])?;
    assert_eq!(killed.status.code(), Some(0), "{}", stderr(&killed));
    assert_eq!(stdout(&killed), format!("killed session {NAME}\n"));
    assert_eq!(short_list(&namespace)?, "");
    Ok(())
}

/// The binary's `attach` starts the server and creates the session; typed into, read with `screen` and detached from
/// with Ctrl+\, the session runs on with no client. The server, which runs in a process of its own; its welcome.
fn attach_type_read_and_detach(namespace: &Namespace) -> TestResult<Welcome> {
    let mut first = namespace.attach(&with_shell(NAME), &[], &Answers::basic())?;
    first.type_line(&tagged("one"))?;
    first.expect("got:one");
    let server = running_server(namespace)?;
    assert_eq!(server.build, "e2e-a");
    assert_ne!(server.pid, std::process::id(), "the server runs in the test");
    assert_ne!(server.pid, first.child.pid(), "the server is the attach");

    let screen = screen_showing(namespace, NAME, false, "got:one");
    let header: Vec<&str> = screen.lines().next().unwrap_or_default().split(' ').collect();
    assert_eq!(&header[..2], [COLS.to_string(), ROWS.to_string()], "{screen}");

    first.write(b"\x1c")?;
    assert_eq!(first.wait()?, 0, "the detach's status");
    wait_for_clients(namespace, NAME, 0);
    assert_eq!(short_list(namespace)?, format!("{NAME}\n"));
    Ok(server)
}

/// Another attach through the binary, typed into, then killed with its process tree: the session and its server
/// (the same process) run on, the client gone from the list. The screen then, as [`screen_showing`] reads it.
fn attach_and_kill_the_client(namespace: &Namespace, server: &Welcome) -> TestResult<String> {
    let mut second = namespace.attach(&[NAME], &[], &Answers::basic())?;
    second.expect("got:one");
    second.type_line(&tagged("two"))?;
    second.expect("got:two");
    wait_for_clients(namespace, NAME, 1);

    second.kill()?;
    wait_for_clients(namespace, NAME, 0);
    assert_eq!(running_server(namespace)?.pid, server.pid, "another server serves");
    Ok(screen_showing(namespace, NAME, false, "got:two"))
}

/// An attach to the session through the library at the outer terminal's grid; the terminal its snapshot builds.
fn attach_through_the_library(namespace: &Namespace) -> TestResult<(Attachment, Terminal)> {
    let mut attachment = Client::new(&namespace.directory).attach(AttachRequest {
        name: NAME.to_owned(),
        create: None,
        size: GridSize { cols: COLS, rows: ROWS },
        managed: None,
        metadata: ReplyMetadata::default(),
    })?;
    let mut terminal: Option<Terminal> = None;
    loop {
        match attachment.reader.recv()? {
            Some(AttachEvent::SnapshotBegin { size, .. }) => {
                terminal = Some(Terminal::new(size.cols, size.rows).map_err(|err| format!("{err:?}"))?);
            }
            Some(AttachEvent::SnapshotChunk(data)) => {
                terminal.as_mut().ok_or("a chunk before its snapshot")?.vt_write(&data);
            }
            Some(AttachEvent::SnapshotEnd) => break,
            Some(AttachEvent::Exit(status)) => return Err(format!("the session ended: {status:?}").into()),
            Some(_) => {}
            None => return Err("the stream ended before its snapshot".into()),
        }
    }
    Ok((attachment, terminal.ok_or("no snapshot")?))
}

/// A reattach through the library: its snapshot, fed into a terminal of the client's own, shows `screen`, the screen
/// the binary read before. The attachment.
fn reattach_to_the_same_screen(namespace: &Namespace, screen: &str) -> TestResult<Attachment> {
    let (attachment, mut terminal) = attach_through_the_library(namespace)?;
    let shown = read_screen(&mut terminal, 0, false).map_err(|err| format!("{err:?}"))?;
    assert_eq!(view(&shown), screen, "the reattach's screen");
    Ok(attachment)
}

/// `type` through the binary reaches the attached library client, and the client's input reaches `screen`; then it
/// detaches.
fn type_both_ways(namespace: &Namespace, mut attachment: Attachment) -> TestResult {
    let typed = namespace.run(&["type", NAME], format!("{}\r", tagged("three")).as_bytes(), &[])?;
    assert_eq!(typed.status.code(), Some(0), "{}", stderr(&typed));
    read_until(&mut attachment.reader, |text| text.contains("got:three"))?;

    attachment
        .writer
        .input(format!("{}{}", tagged("four"), enter()).as_bytes())?;
    read_until(&mut attachment.reader, |text| text.contains("got:four"))?;
    screen_showing(namespace, NAME, false, "got:four");

    attachment.writer.detach()?;
    while attachment.reader.recv()?.is_some() {}
    wait_for_clients(namespace, NAME, 0);
    Ok(())
}

/// The binary staged again as another build: its attach hands the server over to it, and the session, its screen
/// and its scrollback carry over to the new server; the old one ends. Detached at the end.
fn hand_over_on_an_attach_of_another_build(namespace: &Namespace, root: &Path, old: &Welcome) -> TestResult {
    let before = screen_showing(namespace, NAME, true, "got:four");
    assert_eq!(stage(root, "e2e-b")?, namespace.executable);

    let mut third = namespace.attach(&[NAME], &[], &Answers::basic())?;
    third.expect("got:four");
    let new = running_server(namespace)?;
    assert_eq!(new.build, "e2e-b");
    assert_ne!(new.pid, old.pid, "the old server serves");
    wait_for_exit(old.pid, "the old server's end");
    assert_eq!(
        screen_showing(namespace, NAME, true, "got:four"),
        before,
        "the screen after the update"
    );

    third.type_line(&tagged("five"))?;
    third.expect("got:five");
    third.write(b"\x1c")?;
    assert_eq!(third.wait()?, 0, "the detach's status");
    wait_for_clients(namespace, NAME, 0);
    Ok(())
}

/// `print` through the binary reaches a client attached through the library, as its program's output would, and
/// `screen`; then the client detaches.
fn print_into_an_attached_client(namespace: &Namespace) -> TestResult {
    let (mut attachment, _) = attach_through_the_library(namespace)?;
    let printed = namespace.run(&["print", NAME, "printed", "by-cli"], b"", &[])?;
    assert_eq!(printed.status.code(), Some(0), "{}", stderr(&printed));
    read_until(&mut attachment.reader, |text| text.contains("printed by-cli"))?;
    screen_showing(namespace, NAME, false, "printed by-cli");
    attachment.writer.detach()?;
    while attachment.reader.recv()?.is_some() {}
    Ok(())
}

/// The program of a session that ignores the hangup (Unix) and runs each line typed into it.
fn stubborn_argv() -> Vec<String> {
    if cfg!(windows) {
        binary::shell_argv()
    } else {
        ["/bin/sh", "-c", "trap '' HUP; while read line; do eval \"$line\"; done"]
            .map(str::to_owned)
            .to_vec()
    }
}

#[test]
fn kill_and_kill_force_end_the_listed_sessions_and_the_server_stops() -> TestResult {
    let namespace = Namespace::new()?;
    let server = start_from_an_attach_killed_at_once(&namespace)?;
    namespace.create("stubborn", spec(stubborn_argv()))?;
    list_both_unattached(&namespace)?;

    let plain = namespace.run(&["kill", "plain"], b"", &[])?;
    assert_eq!(plain.status.code(), Some(0), "{}", stderr(&plain));
    assert_eq!(stdout(&plain), "killed session plain\n");
    assert_eq!(short_list(&namespace)?, "stubborn\n");
    if cfg!(unix) {
        let refused = namespace.run(&["kill", "stubborn"], b"", &[])?;
        assert_eq!(refused.status.code(), Some(1), "the hangup ended it");
        assert!(
            stderr(&refused).contains("did not end after its hangup"),
            "{}",
            stderr(&refused)
        );
        assert_eq!(short_list(&namespace)?, "stubborn\n");
    }
    let forced = namespace.run(&["kill", "--force", "stubborn"], b"", &[])?;
    assert_eq!(forced.status.code(), Some(0), "{}", stderr(&forced));
    assert_eq!(stdout(&forced), "killed session stubborn\n");
    assert_eq!(short_list(&namespace)?, "");

    // no session and no client: the server stops by itself
    wait_for_exit(server.pid, "the idle server's end");
    assert!(namespace.server()?.is_none());
    Ok(())
}

/// The session `plain` created by the binary's `attach`, which starts the server and is then killed with its process
/// tree: the server, detached from it, runs on. Its welcome.
fn start_from_an_attach_killed_at_once(namespace: &Namespace) -> TestResult<Welcome> {
    let mut starter = namespace.attach(&with_shell("plain"), &[], &Answers::basic())?;
    starter.type_line(&tagged("plain"))?;
    starter.expect("got:plain");
    let server = running_server(namespace)?;
    starter.kill()?;
    wait_for_clients(namespace, "plain", 0);
    assert_eq!(running_server(namespace)?.pid, server.pid, "another server serves");
    Ok(server)
}

/// `list` shows `plain` and `stubborn`, in that order, each with its program's pid and no client.
fn list_both_unattached(namespace: &Namespace) -> TestResult {
    let listed = namespace.run(&["list"], b"", &[])?;
    assert_eq!(listed.status.code(), Some(0), "{}", stderr(&listed));
    let lines: Vec<String> = stdout(&listed).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    for (line, name) in lines.iter().zip(["plain", "stubborn"]) {
        assert!(line.starts_with(&format!("name={name}\tpid=")), "{line}");
        assert!(line.contains("\tclients=0\tcreated="), "{line}");
    }
    Ok(())
}
