//! The client against the real patched zmx (`scripts/build-zmx.sh`), in a private socket directory. Runs when
//! `AGTERM_TEST_ZMX` names the binary (`$(scripts/build-zmx.sh --print-prefix)/bin/zmx`); `cargo test` never needs zig,
//! so without it the test returns at once.

use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use agterm_core::zmx::daemon_name;
use agterm_host::{KillOutcome, SessionHost};
use agterm_zmx::ZmxClient;
use uuid::Uuid;

#[test]
fn the_client_lists_reads_types_into_and_kills_a_real_daemon() -> Result<(), Box<dyn std::error::Error>> {
    let Some(zmx) = std::env::var_os("AGTERM_TEST_ZMX").map(PathBuf::from) else {
        eprintln!("AGTERM_TEST_ZMX is unset: skipping the real zmx test");
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let socket_directory = dir.path().to_string_lossy().into_owned();
    let client = ZmxClient::new(&zmx, socket_directory.clone());
    assert!(client.is_available());
    assert_eq!(client.list()?, Vec::new(), "an empty namespace is an answer");

    let name = daemon_name(Uuid::new_v4());
    let status = Command::new(&zmx)
        .args(["run", &name, "-d", "/bin/sh"])
        .env("ZMX_DIR", &socket_directory)
        .env("SHELL", "/bin/sh")
        .status()?;
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(5);
    let record = loop {
        if let Some(record) = client.list()?.into_iter().find(|record| record.name == name) {
            break record;
        }
        assert!(Instant::now() < deadline, "the daemon never listed");
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(record.clients, Some(0));
    assert!(record.leader_pid.is_some_and(|pid| pid > 0));

    client.type_input(&name, b"echo agterm-zmx-$((40+2))\r")?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = client.screen(&name, false)?;
        assert!(screen.columns > 0 && screen.rows > 0);
        if screen.text.contains("agterm-zmx-42") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "typed input never reached the screen: {:?}",
            screen.text
        );
        thread::sleep(Duration::from_millis(50));
    }

    assert_eq!(client.kill(&name, true), KillOutcome::Killed);
    assert!(client.list()?.iter().all(|record| record.name != name));
    assert!(
        client.screen(&name, false).is_err(),
        "a gone daemon is never a blank screen"
    );
    Ok(())
}

/// The pids of every process whose environment names `ZMX_DIR=<socket_directory>`, as the conformance harness's leftover
/// check reads them.
fn processes_in(socket_directory: &str) -> Vec<u32> {
    let marker = format!("ZMX_DIR={socket_directory}");
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|environ| {
                environ
                    .split(|byte| *byte == 0)
                    .any(|variable| variable == marker.as_bytes())
            })
        })
        .collect()
}

#[test]
fn the_stop_leaves_no_process_in_the_instance_s_namespace() -> Result<(), Box<dyn std::error::Error>> {
    let Some(zmx) = std::env::var_os("AGTERM_TEST_ZMX").map(PathBuf::from) else {
        eprintln!("AGTERM_TEST_ZMX is unset: skipping the real zmx test");
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let socket_directory = dir.path().to_string_lossy().into_owned();
    let client = ZmxClient::new(&zmx, socket_directory.clone()).with_sweeper(agterm_zmx::sweep::Sweeper::live());
    let names: Vec<String> = (0..2).map(|_| daemon_name(Uuid::new_v4())).collect();
    for name in &names {
        let status = Command::new(&zmx)
            .args(["run", name, "-d", "/bin/sh"])
            .env("ZMX_DIR", &socket_directory)
            .env("SHELL", "/bin/sh")
            .status()?;
        assert!(status.success());
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while client
        .list()?
        .iter()
        .filter(|record| names.contains(&record.name))
        .count()
        < names.len()
    {
        assert!(Instant::now() < deadline, "the daemons never listed");
        thread::sleep(Duration::from_millis(50));
    }
    // a foreground job in each shell, as a pane's program runs
    for name in &names {
        client.type_input(name, b"sleep 600\r")?;
    }
    thread::sleep(Duration::from_millis(300));
    assert!(!processes_in(&socket_directory).is_empty());

    // what the pane finalizer and the harness's reap run: one forced batch over the claimed names
    assert!(SessionHost::kill_batch(&client, &names));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let left = processes_in(&socket_directory);
        if left.is_empty() {
            break;
        }
        assert!(Instant::now() < deadline, "processes left under ZMX_DIR: {left:?}");
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(client.list()?, Vec::new());
    Ok(())
}
