//! `agterm-session`: agterm's live-session server. Its one command so far is the server itself,
//! `agterm-session server <namespace directory>`, which the client that finds no server runs
//! (`agterm_session::server::spawn::connect_or_start`), detached from itself.
//!
//! Exit status: 0 once the server stopped by itself, 1 when it failed, 2 for a command line it does not take, 3 when
//! another server holds the namespace.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::ExitCode;

use agterm_session::server::{self, EXIT_FAILED, EXIT_STOPPED, EXIT_TAKEN, EXIT_USAGE, ServerConfig};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [command, directory] if command == "server" => serve(Path::new(directory)),
        _ => {
            eprintln!("usage: agterm-session server <namespace directory>");
            ExitCode::from(EXIT_USAGE)
        }
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
