//! The `ngterm-session` binary's client commands, each test in a private namespace that only `NGTERM_LIVE_DIR` names,
//! every child run with the test's environment without the live ngterm's `NGTERM_*`, `AGTERM_*` and `ZMX_*` variables:
//! the api commands' output and exit status, and `attach` as a terminal client on a real PTY, the test standing in for
//! the outer terminal (it answers the client's probe, and reads what reaches it): a round trip with a resize, detach
//! and reattach, the gone guard, the role title on a lead change, `NGTERM_LIVE_SESSION`, and (on Unix, where no
//! pseudoconsole answers the program's queries itself, `server::replies`) the reply owner: split queries with two
//! clients attached, the outer terminal's colours, an OSC 52 read relayed.
//!
//! Every session a test leaves is killed through the api at its end, after which the namespace's server stops by
//! itself.

#[expect(
    dead_code,
    reason = "the helpers serve the pane, lead, snapshot, attach, api, client, host and cli tests alike"
)]
mod support;

#[expect(dead_code, reason = "the helpers serve the client, host and cli tests alike")]
mod peer;

#[expect(dead_code, reason = "the helpers serve the cli and e2e tests alike")]
mod binary;

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use binary::{Answers, COLS, Namespace, ROWS, shell_argv, spec, stderr, stdout, with_shell};
use ngterm_session::protocol::{AttachRequest, GridSize, Role};
use peer::Peer;
use support::{TestResult, tagged};

/// The argv of the gone guard (`ngterm-core::remote::ssh`): a session that is gone runs it and fails.
fn gone_guard() -> Vec<&'static str> {
    if cfg!(windows) {
        vec!["cmd.exe", "/d", "/c", "echo ngterm: remote session is gone& exit /b 1"]
    } else {
        vec![
            "/bin/sh",
            "-c",
            "printf '%s\\n' 'ngterm: remote session is gone'; exit 1",
        ]
    }
}

#[test]
fn the_api_commands_print_and_exit_as_documented() -> TestResult {
    let namespace = Namespace::new()?;
    without_a_session(&namespace)?;
    namespace.create("cmd", spec(shell_argv()))?;
    with_a_session(&namespace)?;
    let killed = namespace.run(&["kill", "--force", "cmd", "other"], b"", &[])?;
    assert_eq!(killed.status.code(), Some(1), "one of the two was no session");
    assert_eq!(stdout(&killed), "killed session cmd\n");
    assert!(
        stderr(&killed).contains("no live session named other"),
        "{}",
        stderr(&killed)
    );
    assert_eq!(stdout(&namespace.run(&["list", "--short"], b"", &[])?), "");
    Ok(())
}

/// The commands where no server runs: nothing to list, no session to read or kill, usage errors.
fn without_a_session(namespace: &Namespace) -> TestResult {
    let version = namespace.run(&["version"], b"", &[])?;
    assert_eq!(version.status.code(), Some(0));
    assert!(stdout(&version).starts_with("ngterm-session\t"), "{}", stdout(&version));
    // no server: no session, and nothing started
    let empty = namespace.run(&["list"], b"", &[])?;
    assert_eq!(empty.status.code(), Some(0));
    assert_eq!(stdout(&empty), "");
    assert!(
        stderr(&empty).starts_with("no sessions found in "),
        "{}",
        stderr(&empty)
    );
    let missing = namespace.run(&["screen", "none"], b"", &[])?;
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("no live session named none"),
        "{}",
        stderr(&missing)
    );
    assert_eq!(namespace.run(&["kill", "none"], b"", &[])?.status.code(), Some(1));
    assert_eq!(namespace.run(&["bogus"], b"", &[])?.status.code(), Some(2));
    assert_eq!(namespace.run(&["screen"], b"", &[])?.status.code(), Some(2));
    let unset = namespace.run(&["list"], b"", &[("NGTERM_LIVE_DIR", "")])?;
    assert_eq!(unset.status.code(), Some(2));
    assert!(
        stderr(&unset).contains("NGTERM_LIVE_DIR is not set"),
        "{}",
        stderr(&unset)
    );
    Ok(())
}

/// The commands on the session `cmd`: listed, typed and printed into, its screen read.
fn with_a_session(namespace: &Namespace) -> TestResult {
    let listed = namespace.run(&["list"], b"", &[])?;
    assert_eq!(listed.status.code(), Some(0));
    assert!(stdout(&listed).starts_with("name=cmd\tpid="), "{}", stdout(&listed));
    assert_eq!(stdout(&namespace.run(&["list", "--short"], b"", &[])?), "cmd\n");
    // the prefix comes before every name
    let prefixed = namespace.run(&["screen", "d"], b"", &[("NGTERM_LIVE_SESSION_PREFIX", "cm")])?;
    assert_eq!(prefixed.status.code(), Some(0), "{}", stderr(&prefixed));

    let typed = namespace.run(&["type", "cmd"], format!("{}\r", tagged("typed")).as_bytes(), &[])?;
    assert_eq!(typed.status.code(), Some(0), "{}", stderr(&typed));
    namespace.screen_until("cmd", |text| text.contains("got:typed"));
    assert_eq!(namespace.run(&["type", "cmd"], b"", &[])?.status.code(), Some(1));
    let printed = namespace.run(&["print", "cmd", "printed", "words"], b"", &[])?;
    assert_eq!(printed.status.code(), Some(0), "{}", stderr(&printed));
    namespace.screen_until("cmd", |text| text.contains("printed words"));
    let from_stdin = namespace.run(&["print", "cmd"], b"from-stdin", &[])?;
    assert_eq!(from_stdin.status.code(), Some(0), "{}", stderr(&from_stdin));
    namespace.screen_until("cmd", |text| text.contains("from-stdin"));

    let screen = namespace.run(&["screen", "--all", "cmd"], b"", &[])?;
    assert_eq!(screen.status.code(), Some(0));
    let text = stdout(&screen);
    let (header, body) = text.split_once('\n').ok_or("no header")?;
    let fields: Vec<&str> = header.split(' ').collect();
    assert_eq!(fields.len(), 6, "{header}");
    assert_eq!(&fields[1..3], [COLS.to_string(), ROWS.to_string()]);
    assert!(body.contains("got:typed") && body.contains("from-stdin"), "{body}");
    Ok(())
}

#[test]
fn attach_round_trips_resizes_detaches_and_reattaches_to_its_snapshot() -> TestResult {
    let namespace = Namespace::new()?;
    let args = with_shell("trip");
    // an inherited session variable set empty is unset: the attach goes ahead
    let unset = [("NGTERM_LIVE_SESSION", "")];
    let mut first = namespace.attach(&args, &unset, &Answers::basic())?;
    first.type_line(&tagged("pong"))?;
    first.expect("got:pong");
    assert_eq!(namespace.session_names(), ["trip"]);

    first.resize(30, 100);
    namespace.screen_until("trip", |text| text.starts_with("100x30\n"));

    // Ctrl+\ detaches and leaves the session running
    first.write(b"\x1c")?;
    assert_eq!(first.wait()?, 0);
    assert_eq!(namespace.session_names(), ["trip"]);

    // a second attach ignores its argv (create-only) and shows the session's screen
    let mut second = namespace.attach(&["trip", "never-run"], &[], &Answers::basic())?;
    second.expect("got:pong");
    second.type_line(&tagged("again"))?;
    second.expect("got:again");
    // the session's program ends: the client exits with its status
    second.type_line("exit")?;
    if cfg!(unix) {
        // the read loop ends at the end of its input
        second.write(b"\x04")?;
    }
    assert_eq!(second.wait()?, 0);
    assert!(namespace.session_names().is_empty());
    Ok(())
}

#[test]
fn the_detach_key_goes_to_the_program_when_turned_off() -> TestResult {
    let namespace = Namespace::new()?;
    let off = [("NGTERM_LIVE_NO_DETACH_KEY", "1")];
    if cfg!(windows) {
        let args = with_shell("keyed");
        let mut client = namespace.attach(&args, &off, &Answers::basic())?;
        client.type_line(&tagged("ready"))?;
        client.expect("got:ready");
        // cmd shows the Ctrl+\ it got as ^\
        client.write(b"\x1c")?;
        namespace.screen_until("keyed", |text| text.contains("^\\"));
        client.write(b"\r")?;
        client.type_line(&tagged("still"))?;
        client.expect("got:still");
        assert!(client.child.try_wait()?.is_none(), "Ctrl+\\ detached");
    } else {
        // the program reads its terminal raw, so Ctrl+\ is a byte and no SIGQUIT
        let script = "stty raw -echo; printf 'READY\\r\\n'; \
                      b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); printf 'GOT:%s\\r\\n' \"$b\"; \
                      sleep 600";
        let mut client = namespace.attach(&["keyed", "/bin/sh", "-c", script], &off, &Answers::basic())?;
        client.expect("READY");
        client.write(b"\x1c")?;
        client.expect("GOT:1c");
        assert!(client.child.try_wait()?.is_none(), "Ctrl+\\ detached");
    }
    Ok(())
}

/// Escape or an Alt chord and the keys after it, bunched as a slow link hands them over: what only starts like an
/// answer is typed once the input stays quiet, no answer being due, and a control after it is typed at once.
#[cfg(unix)]
#[test]
fn keys_that_start_like_an_answer_reach_the_program() -> TestResult {
    let namespace = Namespace::new()?;
    let script = "stty raw -echo; printf 'READY\\r\\n'; \
                  b=$(dd bs=1 count=10 2>/dev/null | od -An -tx1 | tr -d ' \\n'); printf 'GOT:%s\\r\\n' \"$b\"; \
                  sleep 600";
    let client = namespace.attach(&["bunched", "/bin/sh", "-c", script], &[], &Answers::basic())?;
    client.expect("READY");
    // Alt+_ and G, the start of a Kitty graphics answer, then a pause past the escape time
    client.write(b"\x1b_G")?;
    std::thread::sleep(Duration::from_millis(300));
    // readline's M-_ and Enter, then Alt+] and 1, the start of an OSC answer, ended by Backspace
    client.write(b"\x1b_\r\x1b]1\x7f")?;
    client.expect("GOT:1b5f471b5f0d1b5d317f");
    Ok(())
}

#[test]
fn a_gone_session_runs_the_guard_and_the_attach_fails() -> TestResult {
    let namespace = Namespace::new()?;
    let mut args = vec!["vanished"];
    args.extend(gone_guard());
    let mut client = namespace.attach(&args, &[], &Answers::basic())?;
    if cfg!(unix) {
        // on Windows the pseudoconsole in front of the client repaints what it shows, and may not paint a line the
        // client's reset at its exit clears at once
        client.expect("ngterm: remote session is gone");
    }
    assert_eq!(client.wait()?, 1, "the guard's status");
    assert!(namespace.session_names().is_empty());
    Ok(())
}

/// The far end of an attach from another machine: the line `ngterm-core`'s ssh attach has the far machine's shell run
/// (the last word of `remote::ssh::attach_command`, run by `sh -c` as sshd runs it), under the attach contract's
/// variables of a pane the account may carry, attaches the session it names and types into it, its end leaving the
/// session running; for a session that is gone it runs the guard and fails, creating nothing. Unix only: the line is
/// a POSIX shell's, as the far side of an ssh attach is.
#[cfg(unix)]
#[test]
fn the_remote_attach_line_attaches_its_session_and_fails_for_a_gone_one() -> TestResult {
    let namespace = Namespace::new()?;
    let endpoint = ngterm_proto::ControlLiveEndpoint {
        executable: namespace.executable.to_string_lossy().into_owned(),
        socket_directory: namespace.directory.to_string_lossy().into_owned(),
    };
    let remote_line = |daemon: &str| -> TestResult<String> {
        let argv = ngterm_core::remote::ssh::attach_command("far.example", &endpoint, daemon, None)
            .map_err(|error| format!("{error:?}"))?;
        Ok(argv.last().ok_or("no remote command line")?.clone())
    };
    let carried = [
        ("NGTERM_LIVE_SESSION", "outer"),
        ("NGTERM_LIVE_SESSION_PREFIX", "outer."),
    ];
    let name = ngterm_core::live::daemon_name(uuid::Uuid::new_v4());
    namespace.create(&name, spec(shell_argv()))?;
    let mut client = namespace.on_terminal("/bin/sh", &["-c", &remote_line(&name)?], &carried, &Answers::basic())?;
    client.type_line(&tagged("far"))?;
    client.expect("got:far");
    namespace.screen_until(&name, |text| text.contains("got:far"));
    client.kill()?;
    assert_eq!(namespace.session_names(), std::slice::from_ref(&name));

    let gone = ngterm_core::live::daemon_name(uuid::Uuid::new_v4());
    let mut client = namespace.on_terminal("/bin/sh", &["-c", &remote_line(&gone)?], &carried, &Answers::basic())?;
    client.expect("ngterm: remote session is gone");
    assert_eq!(client.wait()?, 1, "the guard's status");
    assert_eq!(namespace.session_names(), [name]);
    Ok(())
}

#[test]
fn an_attach_inside_a_session_is_refused() -> TestResult {
    let namespace = Namespace::new()?;
    let refused = namespace.run(&["attach", "nested"], b"", &[("NGTERM_LIVE_SESSION", "outer")])?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        stderr(&refused).contains("inside the live session outer"),
        "{}",
        stderr(&refused)
    );
    assert!(namespace.session_names().is_empty());
    Ok(())
}

/// Unix only: a Windows pseudoconsole between the client and the test passes on only the latest title it was given
/// when it paints, so a role title that the session's own title (its snapshot's) follows at once never reaches the
/// test there.
#[cfg(unix)]
#[test]
fn a_managed_attach_writes_its_role_title_on_every_lead_change() -> TestResult {
    let namespace = Namespace::new()?;
    let args = with_shell("led");
    let first = namespace.attach(
        &args,
        &[("NGTERM_LIVE_MANAGED", "na"), ("NGTERM_LIVE_MANAGED_CLAIM", "1")],
        &Answers::basic(),
    )?;
    first.expect_bytes(b"ngterm-live-role;na:leader:");
    let second = namespace.attach(
        &["led"],
        &[("NGTERM_LIVE_MANAGED", "nb"), ("NGTERM_LIVE_MANAGED_CLAIM", "1")],
        &Answers::basic(),
    )?;
    second.expect_bytes(b"ngterm-live-role;nb:leader:");
    first.expect_bytes(b"ngterm-live-role;na:follower:");
    // an unmanaged attach writes none
    let third = namespace.attach(&["led"], &[], &Answers::basic())?;
    third.type_line(&tagged("third"))?;
    first.expect("got:third");
    assert!(!String::from_utf8_lossy(&third.output()).contains("live-role"));
    Ok(())
}

/// The reply owner's tests: the session's program writes queries, prints what comes back as `REPLY<n>:<hex>` lines,
/// and is run by `/bin/sh` with its terminal raw and reads timed (`stty min 0 time 30`, `dd`, `od`).
#[cfg(unix)]
mod owner {
    use super::*;
    use crate::binary::Outer;

    /// A script that waits for a typed line, then writes each query (a `printf` format, which may hold a `sleep`
    /// between two halves) and prints the answer's bytes.
    fn probe_script(queries: &[&str]) -> String {
        use std::fmt::Write as _;
        let mut script = String::from(
            "printf 'READY\\r\\n'; IFS= read -r go; stty raw -echo min 0 time 30; \
             reply() { dd bs=4096 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'; }; ",
        );
        for (index, query) in queries.iter().enumerate() {
            let _ = write!(script, "{query}; r=$(reply); printf 'REPLY{index}:%s\\r\\n' \"$r\"; ");
        }
        script.push_str("printf 'DONE\\r\\n'; stty sane; sleep 600");
        script
    }

    /// The ESCs in `bytes`: one per answer (two for one ended by ST).
    fn escapes(bytes: &[u8]) -> usize {
        String::from_utf8_lossy(bytes).matches('\x1b').count()
    }

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2)
            .filter_map(|at| u8::from_str_radix(hex.get(at * 2..at * 2 + 2)?, 16).ok())
            .collect()
    }

    /// The answer the program printed for query `index`.
    fn reply(client: &Outer, index: usize) -> Vec<u8> {
        let prefix = format!("REPLY{index}:");
        client.expect(&prefix);
        let text = support::wait_for(
            &format!("the whole {prefix} line"),
            || client.text(),
            |text| {
                text.split_once(prefix.as_str())
                    .is_some_and(|(_, rest)| rest.contains('\n'))
            },
        );
        let (_, rest) = text.split_once(prefix.as_str()).unwrap_or_default();
        unhex(rest.lines().next().unwrap_or_default().trim())
    }

    fn attach_probe(namespace: &Namespace, name: &str, queries: &[&str], answers: &Answers) -> TestResult<Outer> {
        let script = probe_script(queries);
        let client = namespace.attach(&[name, "/bin/sh", "-c", &script], &[], answers)?;
        client.expect("READY");
        Ok(client)
    }

    #[test]
    fn split_queries_reach_no_outer_terminal_and_get_one_answer() -> TestResult {
        let namespace = Namespace::new()?;
        let queries = [
            // DA2 cut after its prefix, DECRQM inside its parameters, XTGETTCAP inside its string
            "printf '\\033[>'; sleep 0.3; printf 'c'",
            "printf '\\033[?20'; sleep 0.3; printf '26$p'",
            "printf '\\033P+q54'; sleep 0.3; printf '4e\\033\\\\'",
        ];
        let first = attach_probe(&namespace, "split", &queries, &Answers::basic())?;
        let second = namespace.attach(&["split"], &[], &Answers::basic())?;
        second.expect("READY");
        first.type_line("go")?;
        first.expect("DONE");
        second.expect("DONE");
        let da2 = reply(&first, 0);
        assert!(
            da2.starts_with(b"\x1b[>") && da2.ends_with(b"c"),
            "{:?}",
            String::from_utf8_lossy(&da2)
        );
        assert_eq!(escapes(&da2), 1, "one answer");
        let decrqm = reply(&first, 1);
        assert!(
            decrqm.starts_with(b"\x1b[?2026;") && decrqm.ends_with(b"$y"),
            "{decrqm:?}"
        );
        assert_eq!(escapes(&decrqm), 1, "one answer");
        let xtgettcap = reply(&first, 2);
        assert!(
            xtgettcap.to_ascii_lowercase().starts_with(b"\x1bp1+r544e="),
            "{:?}",
            String::from_utf8_lossy(&xtgettcap)
        );
        assert_eq!(escapes(&xtgettcap), 2, "one answer, ended by ST");
        for client in [&first, &second] {
            let output = String::from_utf8_lossy(&client.output()).into_owned();
            for query in ["\x1b[>c", "26$p", "\x1bP+q", "+q544e"] {
                assert!(!output.contains(query), "{query:?} reached an outer terminal");
            }
        }
        Ok(())
    }

    #[test]
    fn the_outer_terminals_colours_answer_osc_10_and_11() -> TestResult {
        let namespace = Namespace::new()?;
        let answers = Answers {
            foreground: Some("rgb:1111/2222/3333"),
            background: Some("rgb:4444/5555/6666"),
            ..Answers::default()
        };
        let queries = ["printf '\\033]10;?\\033\\\\'", "printf '\\033]11;?\\007'"];
        let client = attach_probe(&namespace, "themed", &queries, &answers)?;
        client.type_line("go")?;
        client.expect("DONE");
        let foreground = String::from_utf8_lossy(&reply(&client, 0)).into_owned();
        assert!(foreground.contains("]10;rgb:1111/2222/3333"), "{foreground:?}");
        let background = String::from_utf8_lossy(&reply(&client, 1)).into_owned();
        assert!(background.contains("]11;rgb:4444/5555/6666"), "{background:?}");
        Ok(())
    }

    #[test]
    fn an_osc_52_read_is_relayed_to_the_outer_terminal_and_back() -> TestResult {
        use base64::Engine as _;
        let namespace = Namespace::new()?;
        let answers = Answers {
            clipboard: Some("clip-text"),
            ..Answers::default()
        };
        let client = attach_probe(&namespace, "clip", &["printf '\\033]52;c;?\\007'"], &answers)?;
        client.type_line("go")?;
        client.expect("DONE");
        let answer = String::from_utf8_lossy(&reply(&client, 0)).into_owned();
        let data = base64::engine::general_purpose::STANDARD.encode("clip-text");
        assert!(answer.contains(&format!("]52;c;{data}")), "{answer:?}");
        // the read reached the outer terminal from the client, not from the program
        assert!(String::from_utf8_lossy(&client.output()).contains("\x1b]52;c;?\x1b\\"));
        Ok(())
    }

    /// An outer terminal that asks its user first answers the read late, and over a slow link the answer can come in
    /// two reads with a pause between them: the client waits for it, holds the cut answer, and types none of it.
    #[test]
    fn a_late_osc_52_answer_cut_by_a_pause_is_relayed_whole() -> TestResult {
        use base64::Engine as _;
        let namespace = Namespace::new()?;
        let client = attach_probe(&namespace, "late", &["printf '\\033]52;c;?\\007'"], &Answers::basic())?;
        client.type_line("go")?;
        client.expect_bytes(b"\x1b]52;c;?\x1b\\");
        let data = base64::engine::general_purpose::STANDARD.encode("clip-text");
        let (head, tail) = data.split_at(4);
        // the user's consent takes a while; the pause inside the answer is past the input's escape time
        std::thread::sleep(Duration::from_millis(400));
        client.write(format!("\x1b]52;c;{head}").as_bytes())?;
        std::thread::sleep(Duration::from_millis(300));
        client.write(format!("{tail}\x1b\\").as_bytes())?;
        client.expect("DONE");
        let answer = reply(&client, 0);
        let text = String::from_utf8_lossy(&answer).into_owned();
        assert!(text.contains(&format!("]52;c;{data}")), "{text:?}");
        assert_eq!(
            escapes(&answer),
            1 + usize::from(text.ends_with('\\')),
            "one answer, nothing typed: {text:?}"
        );
        Ok(())
    }
}

/// A scripted server for the reattach tests: the first attach is sent a snapshot and output, then its connection ends
/// without the session's exit; a later one is answered as `again` says. Every attach request is recorded.
fn dropping_peer(
    again: fn(&mut ngterm_session::endpoint::Stream),
) -> TestResult<(Peer, Arc<Mutex<Vec<AttachRequest>>>)> {
    use ngterm_session::protocol::{AttachClientMessage, PROTOCOL_VERSION, read_frame};
    let requests: Arc<Mutex<Vec<AttachRequest>>> = Arc::default();
    let recorded = Arc::clone(&requests);
    let peer = Peer::start(move |mut stream, role| {
        if role != Role::Attach {
            return;
        }
        let Ok(AttachClientMessage::Attach(request)) =
            read_frame(&mut stream).and_then(|frame| frame.message::<AttachClientMessage>(PROTOCOL_VERSION))
        else {
            return;
        };
        let first = {
            let mut requests = recorded.lock().unwrap_or_else(PoisonError::into_inner);
            requests.push(request);
            requests.len() == 1
        };
        if first {
            send_snapshot(&mut stream, b"first-snap", b"first-output");
            // the connection ends here, the session's exit unsent
        } else {
            again(&mut stream);
        }
    })?;
    Ok((peer, requests))
}

/// A snapshot holding `snapshot`, then `output`.
fn send_snapshot(stream: &mut ngterm_session::endpoint::Stream, snapshot: &[u8], output: &[u8]) {
    use ngterm_session::protocol::{AttachServerMessage, write_message};
    let size = GridSize { cols: COLS, rows: ROWS };
    let offset = 100;
    for message in [
        AttachServerMessage::ReplyOwner {
            owner: true,
            generation: 1,
        },
        AttachServerMessage::SnapshotBegin { epoch: 1, offset, size },
        AttachServerMessage::SnapshotChunk {
            epoch: 1,
            data: snapshot.to_vec(),
        },
        AttachServerMessage::SnapshotEnd { epoch: 1 },
        AttachServerMessage::Output {
            offset,
            data: output.to_vec(),
        },
    ] {
        if write_message(stream, &message).is_err() {
            return;
        }
    }
}

#[test]
fn a_connection_that_ends_without_the_exit_is_attached_again_from_a_reset() -> TestResult {
    let (peer, requests) = dropping_peer(|stream| {
        use ngterm_session::protocol::{AttachServerMessage, ExitStatus, write_message};
        send_snapshot(stream, b"second-snap", b"second-output");
        std::thread::sleep(Duration::from_millis(300));
        let _ = write_message(
            stream,
            &AttachServerMessage::Exit {
                status: ExitStatus::Exited { code: 3, signal: None },
            },
        );
        // the exit is read before the connection's end
        std::thread::sleep(Duration::from_secs(2));
    })?;
    let namespace = Namespace::of_peer(peer.directory());
    let mut client = namespace.attach(
        &["dropped", "never-run"],
        &[("NGTERM_LIVE_MANAGED", "nd"), ("NGTERM_LIVE_MANAGED_CLAIM", "1")],
        &Answers::basic(),
    )?;
    assert_eq!(client.wait()?, 3, "{:?}", client.text());
    client.expect("second-snap");
    if cfg!(unix) {
        // the bytes as written (a pseudoconsole repaints them on Windows): the first output, the reset with the
        // colour scheme reports turned on again after its RIS, the second snapshot
        let output = String::from_utf8_lossy(&client.output()).into_owned();
        let second = output.find("second-snap").ok_or("no second snapshot")?;
        let reset = String::from_utf8_lossy(ngterm_session::server::snapshot::RESYNC_RESET).replacen(
            "\x1bc",
            "\x1bc\x1b[?2031h",
            1,
        );
        let reset_at = output.find(&reset).ok_or("no reset")?;
        assert!(
            output.find("first-output").is_some_and(|first| first < reset_at) && reset_at < second,
            "{output:?}"
        );
    }
    let requests = requests.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].create.is_some());
    assert_eq!(requests[1].name, "dropped");
    assert!(requests[1].create.is_none(), "a reattach creates nothing");
    assert_eq!(
        requests[1].managed.as_ref().map(|managed| managed.nonce.as_str()),
        Some("nd")
    );
    Ok(())
}

#[test]
fn a_reattach_to_a_gone_session_fails() -> TestResult {
    let (peer, _) = dropping_peer(|stream| {
        use ngterm_session::protocol::{AttachRefusal, AttachServerMessage, write_message};
        let _ = write_message(
            stream,
            &AttachServerMessage::Refused {
                reason: AttachRefusal::Gone,
                message: "no such session".to_owned(),
            },
        );
    })?;
    let namespace = Namespace::of_peer(peer.directory());
    let mut client = namespace.attach(&["vanishing"], &[], &Answers::basic())?;
    assert_eq!(client.wait()?, 1);
    if cfg!(unix) {
        // printed as the client exits, which a pseudoconsole may not paint before it closes (Windows)
        client.expect("the live session vanishing is gone");
    }
    Ok(())
}

/// A path that names no directory yet is fine for every command: the server makes it.
#[test]
fn an_attach_starts_the_namespaces_server() -> TestResult {
    let namespace = Namespace::new()?;
    assert!(!Path::new(&namespace.directory).exists());
    let args = with_shell("started");
    let mut client = namespace.attach(&args, &[], &Answers::basic())?;
    client.type_line(&tagged("up"))?;
    client.expect("got:up");
    client.write(b"\x1c")?;
    assert_eq!(client.wait()?, 0);
    assert_eq!(namespace.session_names(), ["started"]);
    Ok(())
}
