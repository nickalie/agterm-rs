//! `agterm-session`: agterm's live-session server, `agterm-session server <namespace directory>`, which the client that
//! finds no server runs (`agterm_session::server::spawn::connect_or_start`), detached from itself; and its clients'
//! commands (`attach`, `list`, `kill`, `screen`, `type`, `print`, `version`: [`agterm_session::cli`]).
//!
//! The server's exit status: 0 once it stopped by itself, 1 when it failed, 2 for a command line it does not take, 3
//! when another server holds the namespace. The other commands' are [`agterm_session::cli`]'s.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use agterm_session::cli;
use agterm_session::server::{self, EXIT_FAILED, EXIT_STOPPED, EXIT_TAKEN, ServerConfig};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [command, directory] if command == "server" => serve(Path::new(directory)),
        _ => cli::run(&args),
    }
}

fn serve(directory: &Path) -> ExitCode {
    // the directory is made absolute before the server leaves the working directory it was started in
    let result = std::path::absolute(directory).and_then(|directory| {
        std::env::set_current_dir(server::working_directory(&directory)?)?;
        server::run(&directory, &ServerConfig::default())
    });
    match result {
        Ok(_) => ExitCode::from(EXIT_STOPPED),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            eprintln!("agterm-session server: {error}");
            ExitCode::from(EXIT_TAKEN)
        }
        Err(error) => {
            eprintln!("agterm-session server: {error}");
            ExitCode::from(EXIT_FAILED)
        }
    }
}
