//! The [`SessionHost`] over the session server ([`crate::client`]), in place of `agterm-zmx`'s `ZmxClient`: the same
//! operations with the same error kinds and outcomes, over api calls instead of zmx invocations.
//!
//! - **The namespace.** Every call reaches the server of the instance's namespace only (the directory the host was
//!   made with). The host never starts a server: one that does not run holds no session, which is an empty listing,
//!   a kill that found nothing to kill ([`KillOutcome::StaleSocket`], the outcome the server's own `Gone` maps to),
//!   and a screen or a typing that fails.
//! - **Failures are never answers.** A listing that could not be read is an error, never an empty namespace; a screen
//!   that could not be read is an error, never a blank screen; a kill that could not be sent is
//!   [`KillOutcome::Failed`] with the reason. Every call is bounded by the client's timeout
//!   ([`crate::client::DEFAULT_TIMEOUT`], zmx's per invocation) and never retried: a second typing after an unclear
//!   failure would type twice.
//! - **The error texts** are zmx's in live-session wording: `live session list failed: <reason>`,
//!   `live session screen failed: <reason>`, `live session type failed: <reason>` (`ZmxClient`'s `zmx list failed`,
//!   `zmx screen failed`, `zmx type failed`; its `zmx list output was incomplete` and `zmx screen answered no screen`
//!   have no counterpart, since the server's replies are typed and a malformed one is a failed call). No core or suite
//!   assertion reads a [`HostError`]'s text: `agterm-core` turns each failure into its own control error
//!   (`commands::live::inventory`'s `could not read the live session list` and
//!   `could not read the live session screen of <name>`, `commands::surface`'s and `commands::typing`'s read and typing
//!   refusals, `commands::restart_live`'s kill outcomes), which the suite asserts through its vocabulary
//!   (`conformance/agterm_conformance/vocabulary.py`); only the fakes' own test reads a fake's text.
//! - **The foreground job** of a killed session's shell is hung up by the server (`server::api`), so the host does not
//!   sweep after a kill as `ZmxClient` did. A restart's own job calls ([`SessionHost::foreground_job`],
//!   [`SessionHost::job_running`], [`SessionHost::end_job`]) read and signal this machine's process table directly
//!   ([`crate::job::Sweeper`], the rules the server's api runs too): the restart reads the job before the kill, and
//!   waits for it and ends it after, when the server may already have stopped with its last session.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agterm_core::live::support::is_executable_file;
use agterm_host::{HostError, KillOutcome, LiveScreen, LiveSessionRecord, ProcessRecord, SessionHost};

use crate::client::{Client, ClientError, unexpected};
use crate::job::{Signal, Sweeper};
use crate::protocol::{ApiRequest, ApiResponse, KillResult};

/// The session host over the server of one namespace.
pub struct ServerHost {
    executable: PathBuf,
    client: Client,
    sweeper: Sweeper,
}

impl ServerHost {
    /// A host of the namespace `directory`, whose sessions run in the server `executable` starts.
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, directory: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            client: Client::new(directory),
            sweeper: Sweeper::live(),
        }
    }

    /// This host with every call bounded by `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.client = self.client.with_timeout(timeout);
        self
    }

    /// This host reading and signalling a restart's job through `sweeper`.
    #[must_use]
    pub fn with_sweeper(mut self, sweeper: Sweeper) -> Self {
        self.sweeper = sweeper;
        self
    }

    /// The `agterm-session` executable.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The client its calls go through.
    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// One kill call for `names`, one result per name; `None` without a server.
    fn kill_names(&self, names: Vec<String>, force: bool) -> Result<Option<Vec<KillResult>>, ClientError> {
        match self.client.call(ApiRequest::Kill { names, force })? {
            None => Ok(None),
            Some(ApiResponse::Killed(results)) => Ok(Some(results)),
            Some(other) => Err(unexpected("a kill", &other)),
        }
    }
}

/// `error` as the host's error for `operation` (`list`, `screen`, `type`), logged.
fn failed(operation: &str, name: Option<&str>, error: &ClientError) -> HostError {
    tracing::error!(name, %error, "live session {operation} failed");
    HostError(format!("live session {operation} failed: {error}"))
}

impl SessionHost for ServerHost {
    fn is_available(&self) -> bool {
        is_executable_file(&self.executable)
    }

    fn list(&self) -> Result<Vec<LiveSessionRecord>, HostError> {
        match self.client.call(ApiRequest::List) {
            Ok(None) => Ok(Vec::new()),
            Ok(Some(ApiResponse::Sessions(sessions))) => Ok(sessions.into_iter().map(Into::into).collect()),
            Ok(Some(other)) => Err(failed("list", None, &unexpected("a list", &other))),
            Err(error) => Err(failed("list", None, &error)),
        }
    }

    /// One session killed; without a server, or without a session of that name, nothing was left to kill.
    fn kill(&self, name: &str, force: bool) -> KillOutcome {
        match self.kill_names(vec![name.to_owned()], force) {
            Ok(None) => KillOutcome::StaleSocket,
            Ok(Some(results)) => results.into_iter().find(|result| result.name == name).map_or_else(
                || KillOutcome::Failed(format!("the live session server answered no kill result for {name}")),
                |result| result.outcome.into(),
            ),
            Err(error) => {
                tracing::error!(name, %error, "live session kill failed");
                KillOutcome::Failed(error.to_string())
            }
        }
    }

    /// Every distinct name of `names` in one forced kill call; true unless the call failed or a kill failed. Without
    /// a server, or without a name, nothing is left to kill, which is true.
    fn kill_batch(&self, names: &[String]) -> bool {
        let mut seen = BTreeSet::new();
        let unique: Vec<String> = names
            .iter()
            .filter(|name| seen.insert(name.as_str()))
            .cloned()
            .collect();
        if unique.is_empty() {
            return true;
        }
        match self.kill_names(unique, true) {
            Ok(None) => true,
            Ok(Some(results)) => {
                let failed: Vec<&KillResult> = results
                    .iter()
                    .filter(|result| matches!(result.outcome, crate::protocol::KillOutcome::Failed(_)))
                    .collect();
                for result in &failed {
                    tracing::error!(name = result.name, outcome = ?result.outcome, "live session kill failed");
                }
                failed.is_empty()
            }
            Err(error) => {
                tracing::error!(names = names.join(","), %error, "live session kill failed");
                false
            }
        }
    }

    fn screen(&self, name: &str, all: bool) -> Result<LiveScreen, HostError> {
        let request = ApiRequest::Screen {
            name: name.to_owned(),
            all,
        };
        match self.client.call(request) {
            Ok(Some(ApiResponse::Screen(screen))) => Ok(screen.into()),
            Ok(None) => Err(failed(
                "screen",
                Some(name),
                &ClientError::NoServer(self.client.endpoint().to_string()),
            )),
            Ok(Some(other)) => Err(failed("screen", Some(name), &unexpected("a screen read", &other))),
            Err(error) => Err(failed("screen", Some(name), &error)),
        }
    }

    /// `Ok` means the server queued the bytes, not that the program read them.
    fn type_input(&self, name: &str, bytes: &[u8]) -> Result<(), HostError> {
        let request = ApiRequest::Type {
            name: name.to_owned(),
            data: bytes.to_vec(),
        };
        match self.client.call(request) {
            Ok(Some(ApiResponse::Done)) => Ok(()),
            Ok(None) => Err(failed(
                "type",
                Some(name),
                &ClientError::NoServer(self.client.endpoint().to_string()),
            )),
            Ok(Some(other)) => Err(failed("type", Some(name), &unexpected("typing", &other))),
            Err(error) => Err(failed("type", Some(name), &error)),
        }
    }

    fn foreground_job(&self, shell: i32) -> Option<Vec<ProcessRecord>> {
        self.sweeper.foreground_job(shell)
    }

    fn job_running(&self, job: &[ProcessRecord]) -> bool {
        self.sweeper.is_running(job)
    }

    fn end_job(&self, job: &[ProcessRecord]) {
        self.sweeper.send(Signal::Kill, job);
    }
}
