//! `agterm-session`: agterm's live-session server. Its one command so far is the server itself,
//! `agterm-session server <namespace directory>`, which the client that finds no server runs
//! (`agterm_session::server::spawn::connect_or_start`), detached from itself; `server` passes its arguments on to
//! [`agterm_session::server::command`], which also takes an update handoff's replacement
//! (`server <namespace directory> --handoff-import <channel> <token>`).
//!
//! Exit status: 0 once the server stopped by itself (or handed its sessions over), 1 when it failed, 2 for a command
//! line it does not take, 3 when another server holds the namespace.

use std::ffi::OsString;
use std::process::ExitCode;

use agterm_session::server::{self, EXIT_USAGE};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.split_first() {
        Some((command, rest)) if command == "server" => server::command(rest),
        _ => {
            eprintln!("usage: agterm-session server <namespace directory>");
            ExitCode::from(EXIT_USAGE)
        }
    }
}
