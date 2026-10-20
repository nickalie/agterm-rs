//! `agterm-session`: agterm's live-session server, `agterm-session server <namespace directory>`, which the client that
//! finds no server runs (`agterm_session::server::spawn::connect_or_start`), detached from itself, or an update
//! handoff's replacement, `server <namespace directory> --handoff-import <channel> <token>` (both
//! [`agterm_session::server::command`]); and its clients' commands (`attach`, `list`, `kill`, `screen`, `type`,
//! `print`, `version`: [`agterm_session::cli`]), which also say what is wrong with any other `server` line.
//!
//! The server's exit status: 0 once it stopped by itself (or handed its sessions over), 1 when it failed, 2 for a
//! command line it does not take, 3 when another server holds the namespace. The other commands' are
//! [`agterm_session::cli`]'s.

use std::ffi::OsString;
use std::process::ExitCode;

use agterm_session::cli;
use agterm_session::server::{self, handoff};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [command, rest @ ..] if command == "server" && is_server_line(rest) => server::command(rest),
        _ => cli::run(&args),
    }
}

/// Whether `rest`, what follows `server`, is a line [`server::command`] runs: the namespace directory, alone or with
/// a handoff's import.
fn is_server_line(rest: &[OsString]) -> bool {
    match rest {
        [_] => true,
        [_, flag, _, _] => flag == handoff::IMPORT_FLAG,
        _ => false,
    }
}
