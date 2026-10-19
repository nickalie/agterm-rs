//! `ngtermctl version` (`agtermctlKit/MiscCommands.swift` `Version`): the serving app's `version (commit)`, then this
//! CLI's own executable.

use std::path::PathBuf;
use std::{env, fs};

use agterm_core::paths::Env;
use agterm_proto::{Command, Request};

use crate::{BasicOptions, Failure, RequestCommand};

/// `ngtermctl version [--socket <socket>] [--json]`.
#[derive(Debug, clap::Args)]
#[command(about = "Print the version of the app serving the socket.")]
pub struct Version {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Version {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        Request::new(Command::Version)
    }
}

impl Version {
    /// The app's version, then `client: <path>` unless `--json`, so a caller can tell which CLI answered.
    pub fn run(&self, env: &dyn Env) -> Result<(), Failure> {
        self.default_run(env)?;
        if !self.options.json
            && let Some(path) = client_path()
        {
            println!("client: {}", path.display());
        }
        Ok(())
    }
}

/// The running executable's real path: the kernel's answer (`/proc/self/exe`), not `argv[0]`, resolved through
/// symlinks since an installed CLI is usually one.
pub fn client_path() -> Option<PathBuf> {
    env::current_exe().and_then(fs::canonicalize).ok()
}
