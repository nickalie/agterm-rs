//! A launched pane's foreground process group (`agterm_term::Pane::foreground_pid`, what libghostty's
//! `foreground_pid` was for the GTK build) read through this crate's process table, as core's restore capture and the
//! `tree` read it: a command pane's program past its `/bin/sh -c` wrapper, a job typed into a login shell.

use std::time::{Duration, Instant};

use agterm_app::process_table::ProcTable;
use agterm_core::command_restore::PaneForeground;
use agterm_core::ghostty_config::GhosttyConfig;
use agterm_core::restore::capture::ForegroundReader;
use agterm_term::{GridSize, Host, Launch, Pane, PaneConfig};

const SIZE: GridSize = GridSize {
    cols: 80,
    rows: 24,
    cell_width: 8,
    cell_height: 16,
};

/// A launch isolated from any agterm the tests run under.
fn launch(command: Option<&str>, initial_input: Option<&str>) -> Launch {
    Launch {
        command: command.map(str::to_owned),
        initial_input: initial_input.map(str::to_owned),
        env_remove: std::env::vars_os()
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .filter(|name| name.starts_with("AGTERM_") || name.starts_with("ZMX_"))
            .collect(),
        ..Launch::default()
    }
}

fn host() -> Host {
    Host::new(&GhosttyConfig::default(), Some("/bin/sh".to_owned()), None, None)
}

/// Poll `read` until it answers `want`.
fn wait_for<T: PartialEq + std::fmt::Debug>(want: &T, mut read: impl FnMut() -> T) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let got = read();
        if &got == want {
            return;
        }
        assert!(Instant::now() < deadline, "still {got:?}, waiting for {want:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_command_panes_program_is_read_past_its_wrapper() -> Result<(), Box<dyn std::error::Error>> {
    let pane = Pane::launch(&launch(Some("sleep 30"), None), &host(), &PaneConfig::new(SIZE))?;
    let table = ProcTable::new();
    let reader = ForegroundReader::new(&table, None);
    let running = || {
        let group = pane.foreground_pid().and_then(|pid| i32::try_from(pid).ok())?;
        reader.running(group)
    };
    wait_for(
        &Some(PaneForeground::Program(vec!["sleep".to_owned(), "30".to_owned()])),
        running,
    );
    Ok(())
}

#[test]
fn a_job_typed_into_the_login_shell_is_its_foreground_command() -> Result<(), Box<dyn std::error::Error>> {
    let pane = Pane::launch(&launch(None, Some("sleep 31\n")), &host(), &PaneConfig::new(SIZE))?;
    let table = ProcTable::new();
    let reader = ForegroundReader::new(&table, None);
    let command = || {
        let group = pane.foreground_pid().and_then(|pid| i32::try_from(pid).ok())?;
        reader.command(group)
    };
    wait_for(&Some(vec!["sleep".to_owned(), "31".to_owned()]), command);
    // the job's end gives the foreground back to the shell, which no capture replays
    pane.write(b"\x03")?;
    wait_for(&None, command);
    let shell = pane.pid().and_then(|pid| i32::try_from(pid).ok());
    wait_for(&shell, || pane.foreground_pid().and_then(|pid| i32::try_from(pid).ok()));
    Ok(())
}
