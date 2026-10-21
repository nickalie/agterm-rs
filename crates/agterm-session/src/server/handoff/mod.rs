//! The update handoff: a server hands every live session over to a server of another build, and the sessions' programs
//! keep running, with their terminals, through the change.
//!
//! herdr: `src/server/handoff.rs` (the token-authenticated private socket, the JSON manifest, `SCM_RIGHTS` batches of
//! 64, the validated, restored, ready, committed and owned steps with their timeouts) and
//! `src/server/headless/{lifecycle,bootstrap}.rs` (`perform_live_handoff`, `run_handoff_import_server`: the clients
//! disconnected, the panes paused, the replacement spawned, the rollback before the commit) at `HERDR_REV`
//! (`build/pins.env`), licensed under the Apache License 2.0, modified as below. The design was settled with Codex
//! (the run's `discussions/task-21-handoff-design`).
//!
//! # The request
//!
//! A client finding a server of another build ([`crate::server::spawn::connect_or_start`]) asks it, on a control
//! connection, to hand over to the replacement executable ([`crate::protocol::HandoffRequest`]); the answer comes once
//! the handoff committed (`ok`) or failed before its commit. A server advertises
//! [`crate::protocol::CAPABILITY_HANDOFF_REQUEST`] when this host can hand over ([`Support`]: on Windows only behind
//! the `ConPTY` layout gate, `windows.rs`). One handoff runs at a time, and a failed one is not run again: a request
//! for the build and protocol of a handoff this server failed is answered at once with that failure's reason, before
//! anything is fenced ([`Failures`]), so a client process that did not see the first attempt (each pane's attach runs
//! in a process of its own) disconnects nobody by asking. Settled with Codex (the run's
//! `discussions/task-25-handoff-retry-across-processes`).
//!
//! # The steps
//!
//! The old server (the exporter):
//!
//! 1. fences the api and attach connections (the server's operation lock, taken exclusively, and a new connection
//!    generation, so no operation decoded before it runs after it), disconnects them, and drops new ones until the
//!    handoff ends (their clients retry, and find whichever server serves then);
//! 2. binds a private channel (an owner-only endpoint under the namespace directory, `handoff-<pid>`) and starts the
//!    replacement detached, as `agterm-session server <namespace> --handoff-import <channel> <token>`;
//! 3. accepts the replacement on the channel, checks its pid where the OS tells it and its token, and reads its
//!    identity: build, protocol and libghostty-vt pin ([`Identity`]);
//! 4. cuts every pane ([`Pane::freeze`], [`Pane::quiesce`], [`Pane::cut`]; a pane whose program is ending is left
//!    out): the terminal goes as libghostty-vt's binary snapshot when both pins are equal, else as the formatter's VT
//!    restoration ([`VtEncoding`]; a pane the formatter cannot carry fails the handoff, and the client offers the
//!    reset);
//! 5. sends the manifest ([`Manifest`], one JSON line) and each pane's terminal and replies as raw bytes after it;
//!    the replacement answers `validated` once it checked the manifest against itself;
//! 6. passes the endpoint and the PTYs (`unix.rs`: the listening socket, the locked lock file and the masters by
//!    `SCM_RIGHTS`; `windows.rs`: the pseudoconsoles' handles, the programs' processes and jobs duplicated into the
//!    replacement's process, their values in a JSON line, the pipe taken over beside); the replacement answers
//!    `restored` once every pane is rebuilt, inert ([`Pane::import`]), and `ready` once it holds the endpoint and its
//!    accept thread runs, waiting for the commit to accept (so nothing it could fail at is left after the commit);
//! 7. commits by writing `committed`: from the moment that line is written whole, the sessions are the
//!    replacement's, whatever follows (a missing `owned` only logs). Its PTY actors let go of the PTYs
//!    ([`Pane::hand_over`]), it gives up the socket file, answers the request and stops.
//!
//! The replacement (the importer, [`receive`] and `run_import`) does the other half and, on `committed`, makes every
//! pane its own ([`Pane::activate`]), claims the endpoint, opens its accept thread, and answers `owned`.
//!
//! The exporter's steps up to its commit all fit in [`EXPORT_DEADLINE`] from the request (each step's own timeout cut
//! to the time left), so its answer comes within [`ANSWER_WITHIN`], which the client's wait exceeds
//! ([`crate::server::spawn::HANDOFF_TIMEOUT`]).
//!
//! # Rollback
//!
//! Until `committed` is written whole, any failure (a step's [`STEP_TIMEOUT`], a refusal, the replacement's death)
//! rolls back: the replacement is ended and waited for, every pane resumes ([`Pane::resume`]), the connections are
//! taken again, and the request is answered failed, the client then offering the reset. The endpoint was never
//! released, so nothing binds again; the replacement's copies of the PTYs ended with it. A failed write of
//! `committed` means the replacement could not have read the line whole, and it only acts on the whole line.
//!
//! Lost in a handoff, by design: the attached clients' streams (they reattach, from a snapshot) and their reply
//! ownership; a clipboard read waiting for an answer (denied); the Kitty answers a barrier waited for (it ends
//! empty). And at this libghostty-vt pin, from the terminal: Kitty images, their placements and glyph registrations
//! (neither encoding carries them), and across a pin change the saved cursor (the formatter has no extra for it); see
//! `pane/handoff.rs`.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as sys;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(windows)]
pub use windows::{SupportMatrix, gate};

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use agterm_ghostty_vt as vt;
use serde::{Deserialize, Serialize};

use super::Shared;
use super::pane::{Cut, Limits, Pane};
use crate::endpoint::{self, Endpoint, Listener, Stream};
use crate::protocol::{ControlError, ErrorCode, GridSize, HandoffRequest, PROTOCOL_VERSION};

/// The manifest's version: an importer refuses any other.
pub const HANDOFF_VERSION: u32 = 1;
/// How long each step waits for the other side (herdr's `READY_TIMEOUT`).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the exporter waits for `owned` after its commit (herdr's `OWNED_ACK_TIMEOUT`).
pub const OWNED_TIMEOUT: Duration = Duration::from_millis(500);
/// How long a pane's PTY actor has to quiesce.
const QUIESCE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the exporter waits for the disconnected clients to leave the panes.
const DETACH_WAIT: Duration = Duration::from_secs(2);
/// How long a rollback waits for the replacement it ended to be gone.
const REAP_WAIT: Duration = Duration::from_secs(5);
/// How long the exporter's steps up to its commit take at most, all of them: each step waits its own timeout or the
/// time left, whichever is less.
pub const EXPORT_DEADLINE: Duration = Duration::from_secs(120);
/// How long a handoff request waits for its answer at most, on the server's side: the steps, then the `owned` after a
/// commit or the reaping of a rollback.
pub const ANSWER_WITHIN: Duration = EXPORT_DEADLINE.saturating_add(REAP_WAIT).saturating_add(OWNED_TIMEOUT);
/// The longest line either side reads (the manifest's, the handles').
const MAX_LINE: usize = 16 * 1024 * 1024;
/// The most bytes one pane's terminal and replies take on the channel.
const MAX_PANE_PAYLOAD: u64 = 1024 * 1024 * 1024;

/// What a replacement that refuses the manifest answers in place of `validated`, followed by its reason.
const REFUSED: &str = "refused: ";

/// The argument that makes `agterm-session server <namespace>` an importer: `--handoff-import <channel> <token>`.
pub const IMPORT_FLAG: &str = "--handoff-import";

/// Whether this server can hand its sessions over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// Not on this host, with the reason the request is refused with.
    Refused(String),
}

impl Support {
    /// This host's: always on Unix; on Windows only where the `ConPTY` layout gate passes.
    #[must_use]
    pub fn of_this_host() -> Self {
        sys::support()
    }
}

/// How a pane's terminal crosses the handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VtEncoding {
    /// libghostty-vt's binary snapshot: the screens, history, modes and unfinished sequence, but no Kitty images,
    /// placements or glyph registrations (the codec ignores them at this pin); both servers run the same
    /// libghostty-vt pin ([`vt::PIN`]).
    Binary,
    /// The formatter's VT restoration with its extras and the unfinished sequence (the attach snapshot), replayed into
    /// a fresh terminal: across a pin change, for a session on its primary screen. Neither the saved cursor nor Kitty
    /// images pass.
    Formatter,
}

/// One session as the manifest names it; its terminal (`vt_len` bytes) and its replies not yet written
/// (`replies_len` bytes) follow the manifest line, pane after pane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    pub name: String,
    pub pid: u32,
    /// Whole seconds since the Unix epoch.
    pub created_at: u64,
    pub tracked_env: Vec<(String, String)>,
    /// The program's `TERM`, the terminal's terminfo name.
    #[serde(default)]
    pub term: Option<String>,
    pub size: GridSize,
    /// The output offset the cut stands at.
    pub offset: u64,
    pub revision: u64,
    pub vt: VtEncoding,
    pub vt_len: u64,
    pub replies_len: u64,
}

/// The handoff's manifest: versioned, the source's identity, what the replacement must be, the panes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub source_build: String,
    pub source_protocol: u32,
    pub source_pin: String,
    pub expected_build: String,
    pub expected_protocol: u32,
    pub panes: Vec<PaneState>,
}

/// What the replacement says of itself on the channel, after its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub build: String,
    pub protocol: u32,
    /// Its libghostty-vt pin ([`vt::PIN`]).
    pub pin: String,
    pub pid: u32,
}

impl Identity {
    /// This process's, as a server of `build`.
    #[must_use]
    pub fn of_this_process(build: &str) -> Self {
        Self {
            build: build.to_owned(),
            protocol: PROTOCOL_VERSION,
            pin: vt::PIN.to_owned(),
            pid: std::process::id(),
        }
    }
}

impl Manifest {
    /// Whether the replacement `identity` takes this manifest: the version it reads, and the build and protocol the
    /// request expected, and a binary snapshot only from its own pin.
    ///
    /// # Errors
    ///
    /// Why not, worded for the request's failure.
    pub fn check(&self, identity: &Identity) -> Result<(), String> {
        if self.version != HANDOFF_VERSION {
            return Err(format!(
                "the handoff manifest is version {}, the replacement reads {HANDOFF_VERSION}",
                self.version
            ));
        }
        if self.expected_protocol != identity.protocol {
            return Err(format!(
                "the handoff expected protocol {}, the replacement speaks {}",
                self.expected_protocol, identity.protocol
            ));
        }
        if self.expected_build != identity.build {
            return Err(format!(
                "the handoff expected build {:?}, the replacement is {:?}",
                self.expected_build, identity.build
            ));
        }
        if self.panes.iter().any(|pane| pane.vt == VtEncoding::Binary) && self.source_pin != identity.pin {
            return Err(format!(
                "binary terminal snapshots of libghostty-vt {} cannot be read by libghostty-vt {}",
                self.source_pin, identity.pin
            ));
        }
        Ok(())
    }
}

/// The encoding a source of pin `ours` sends to a replacement of pin `theirs`.
#[must_use]
pub fn encoding_for(ours: &str, theirs: &str) -> VtEncoding {
    if ours == theirs {
        VtEncoding::Binary
    } else {
        VtEncoding::Formatter
    }
}

fn failed(message: impl Into<String>) -> ControlError {
    ControlError::new(ErrorCode::HandoffFailed, message)
}

/// The channel of the handoff `pid` (the exporter's) runs in `namespace`.
#[must_use]
pub fn channel(namespace: &Path, pid: u32) -> PathBuf {
    namespace.join(format!("handoff-{pid}"))
}

/// Hand every session of the server `shared` over to the replacement `request` names (module docs). `Ok` once
/// committed: the caller answers the request, then stops the server.
pub(super) fn export(shared: &Arc<Shared>, request: &HandoffRequest) -> Result<(), ControlError> {
    if let Support::Refused(reason) = &shared.handoff {
        return Err(super::no_handoff(reason));
    }
    shared
        .failed_handoffs
        .begin(&shared.handing_off, request)
        .map_err(failed)?;
    let mut exporter = Exporter {
        shared,
        request,
        deadline: Instant::now() + EXPORT_DEADLINE,
        cut: Vec::new(),
        replacement: None,
        channel: None,
    };
    let result = exporter.run();
    if let Err(message) = &result {
        tracing::warn!(%message, "the update handoff failed; this server keeps its sessions");
        exporter.roll_back();
        shared.failed_handoffs.fail(&shared.handing_off, request, message);
    }
    if let Some(channel) = &exporter.channel {
        let _ = std::fs::remove_dir_all(channel);
    }
    result.map_err(failed)
}

/// The handoffs a server failed, each by the build and protocol it was asked to hand over to, with its reason; never
/// forgotten while the server runs (time passing does not make a failed handoff work, and a new build is another
/// request). A handoff begins and a failed one ends under the same lock, so a request racing a failing handoff finds
/// it running or failed, never a gap to begin it again in.
#[derive(Debug, Default)]
pub(super) struct Failures {
    failed: Mutex<Vec<(String, u32, String)>>,
}

impl Failures {
    /// Begins the handoff `request` asks for, setting `handing_off`: refused with the earlier reason when one to the
    /// same build and protocol failed, or while one runs.
    pub(super) fn begin(&self, handing_off: &AtomicBool, request: &HandoffRequest) -> Result<(), String> {
        let failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        let earlier = failed
            .iter()
            .find(|(build, protocol, _)| *build == request.expected_build && *protocol == request.expected_protocol);
        if let Some((_, _, reason)) = earlier {
            return Err(reason.clone());
        }
        if handing_off.swap(true, Ordering::AcqRel) {
            return Err("a handoff of this server is already in progress".to_owned());
        }
        Ok(())
    }

    /// Ends the handoff `request` asked for, rolled back after it failed for `reason`: remembered, then `handing_off`
    /// cleared, so connections are admitted again.
    pub(super) fn fail(&self, handing_off: &AtomicBool, request: &HandoffRequest, reason: &str) {
        let mut failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        failed.push((
            request.expected_build.clone(),
            request.expected_protocol,
            reason.to_owned(),
        ));
        handing_off.store(false, Ordering::Release);
    }
}

/// The exporter's state, for its rollback.
struct Exporter<'a> {
    shared: &'a Arc<Shared>,
    request: &'a HandoffRequest,
    /// When its steps up to the commit must be over ([`EXPORT_DEADLINE`]).
    deadline: Instant,
    /// The panes held at the cut, each with whether its PTY actor quiesced.
    cut: Vec<(Arc<Pane>, bool)>,
    replacement: Option<super::spawn::Spawned>,
    channel: Option<PathBuf>,
}

impl Exporter<'_> {
    fn run(&mut self) -> Result<(), String> {
        let shared = self.shared;
        let _fence = shared.fence();
        wait_for_detach(&shared.panes.all(), self.step(DETACH_WAIT)?);
        let stream = self.connect_replacement()?;
        let identity: Identity = read_json(&stream, self.step(STEP_TIMEOUT)?, "the replacement's identity")?;
        let encoding = encoding_for(vt::PIN, &identity.pin);
        let cuts = self.cut_panes(encoding)?;
        let manifest = Manifest {
            version: HANDOFF_VERSION,
            source_build: shared.identity.build.clone(),
            source_protocol: PROTOCOL_VERSION,
            source_pin: vt::PIN.to_owned(),
            expected_build: self.request.expected_build.clone(),
            expected_protocol: self.request.expected_protocol,
            panes: cuts.iter().map(|cut| cut.state.clone()).collect(),
        };
        self.sending(&stream, || send_manifest(&stream, &manifest, &cuts))?;
        expect(&stream, "validated", self.step(STEP_TIMEOUT)?)?;
        let listener = shared
            .listener()
            .ok_or("the server's endpoint is gone; nothing to hand over")?;
        let panes: Vec<Arc<Pane>> = self.cut.iter().map(|(pane, _)| Arc::clone(pane)).collect();
        let replacement = self.replacement.as_ref().ok_or("the replacement is gone")?;
        self.sending(&stream, || {
            sys::send_handles(&stream, &listener, &panes, replacement).map_err(|err| err.to_string())
        })?;
        expect(&stream, "restored", self.step(STEP_TIMEOUT)?)?;
        expect(&stream, "ready", self.step(STEP_TIMEOUT)?)?;
        // the commit: once this line is written whole the sessions are the replacement's
        write_line(&stream, "committed").map_err(|err| format!("the commit could not be sent: {err}"))?;
        tracing::info!(panes = panes.len(), "update handoff committed");
        if let Err(err) = expect(&stream, "owned", OWNED_TIMEOUT) {
            tracing::warn!(%err, "the replacement did not confirm it owns the sessions; the commit stands");
        }
        listener.relinquish();
        for pane in &panes {
            pane.hand_over();
        }
        self.cut.clear();
        self.replacement = None;
        Ok(())
    }

    /// How long a step waits: `most`, cut to the time left before the deadline.
    fn step(&self, most: Duration) -> Result<Duration, String> {
        step_within(self.deadline, most)
    }

    /// `send` on `stream` under a watchdog that disconnects the stream at the step's timeout (a peer that stops
    /// reading would hold a write forever).
    fn sending(&self, stream: &Arc<Stream>, send: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let watchdog = Deadline::start(stream, self.step(STEP_TIMEOUT)?);
        match send() {
            Err(_) if watchdog.expired() => Err("the replacement did not read what was sent in time".to_owned()),
            outcome => outcome,
        }
    }

    /// Steps 2 and 3: the channel bound, the replacement started and accepted, its pid and token checked.
    fn connect_replacement(&mut self) -> Result<Arc<Stream>, String> {
        let shared = self.shared;
        let channel = Endpoint::new(channel(&shared.namespace, std::process::id()));
        self.channel = Some(channel.directory().to_path_buf());
        let listener = channel
            .bind()
            .map_err(|err| format!("the handoff channel {channel} could not be bound: {err}"))?;
        let token = uuid::Uuid::new_v4().to_string();
        let replacement = super::spawn::spawn_import(
            Path::new(&self.request.import_exe),
            &shared.namespace,
            channel.directory(),
            &token,
        )
        .map_err(|err| {
            format!(
                "the replacement {} could not be started: {err}",
                self.request.import_exe
            )
        })?;
        let pid = replacement.pid();
        self.replacement = Some(replacement);
        let stream = Arc::new(accept_within(listener, &channel, self.step(STEP_TIMEOUT)?)?);
        if let Some(peer) = sys::peer_pid(&stream)
            && peer != pid
        {
            return Err(format!(
                "the handoff channel was answered by pid {peer}, not the replacement (pid {pid})"
            ));
        }
        let presented = read_line(&stream, self.step(STEP_TIMEOUT)?)?;
        if presented != token {
            return Err("the replacement presented the wrong handoff token".to_owned());
        }
        Ok(stream)
    }

    /// Step 4: every pane held at its cut (module docs).
    fn cut_panes(&mut self, encoding: VtEncoding) -> Result<Vec<Cut>, String> {
        let mut cuts = Vec::new();
        for pane in self.shared.panes.all() {
            if !pane.freeze() {
                tracing::debug!(session = %pane.name(), "a live session ending as the handoff began is left out");
                continue;
            }
            self.cut.push((Arc::clone(&pane), false));
            pane.quiesce(self.step(QUIESCE_TIMEOUT)?)
                .map_err(|err| format!("the live session {:?} did not quiesce: {err}", pane.name()))?;
            if let Some(last) = self.cut.last_mut() {
                last.1 = true;
            }
            cuts.push(pane.cut(encoding)?);
        }
        Ok(cuts)
    }

    /// Before the commit: the replacement ended and waited for, every pane resumed.
    fn roll_back(&mut self) {
        if let Some(mut replacement) = self.replacement.take() {
            replacement.terminate();
            let deadline = Instant::now() + REAP_WAIT;
            while replacement.try_exit().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        for (pane, quiesced) in self.cut.drain(..) {
            pane.resume(quiesced);
        }
    }
}

/// How long a step of an exporter whose steps must end by `deadline` waits: `most`, or the time left if less.
///
/// # Errors
///
/// The deadline passed.
fn step_within(deadline: Instant, most: Duration) -> Result<Duration, String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(format!(
            "the handoff did not reach its commit within {}s",
            EXPORT_DEADLINE.as_secs()
        ));
    }
    Ok(left.min(most))
}

/// Until every pane in `panes` has no client attached, or `wait` passed.
fn wait_for_detach(panes: &[Arc<Pane>], wait: Duration) {
    let deadline = Instant::now() + wait;
    while panes.iter().any(|pane| pane.clients() > 0) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
}

/// The manifest line, then every pane's terminal and replies.
fn send_manifest(stream: &Stream, manifest: &Manifest, cuts: &[Cut]) -> Result<(), String> {
    let line = serde_json::to_string(manifest).map_err(|err| format!("the manifest could not be encoded: {err}"))?;
    let mut writer = stream;
    let sent = write_line(stream, &line).and_then(|()| {
        for cut in cuts {
            writer.write_all(&cut.vt)?;
            writer.write_all(&cut.replies)?;
        }
        writer.flush()
    });
    sent.map_err(|err| format!("the manifest could not be sent: {err}"))
}

/// The replacement's connection on the channel `listener` listens on, within `timeout`. The accept runs on a thread
/// of its own, which owns the listener and drops it once it accepted; past the timeout a connection made from here
/// wakes it, and the channel is given up.
fn accept_within(listener: Listener, channel: &Endpoint, timeout: Duration) -> Result<Stream, String> {
    let (done, accepted) = mpsc::channel();
    thread::Builder::new()
        .name("agterm-session-handoff-accept".to_owned())
        .spawn(move || {
            let _ = done.send(listener.accept());
        })
        .map_err(|err| format!("no thread to accept the replacement on: {err}"))?;
    match accepted.recv_timeout(timeout) {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(err)) => Err(format!(
            "accepting the replacement on the handoff channel failed: {err}"
        )),
        Err(_) => {
            let _waker = channel.connect_timeout(Duration::from_secs(1));
            Err(format!(
                "the replacement did not connect to the handoff channel within {}s",
                timeout.as_secs()
            ))
        }
    }
}

/// What the importer took over, inert until its commit; it answers `ready` once it can serve (`run_import`).
pub(super) struct Taken {
    pub(super) stream: Arc<Stream>,
    pub(super) listener: Listener,
    pub(super) panes: Vec<Pane>,
}

/// The importer's half up to `ready`, which the caller answers ([`Taken`]): connect to the exporter's `channel`,
/// present `token` and this server's identity, check the manifest, rebuild every pane inert and take the endpoint
/// `endpoint` over.
///
/// # Errors
///
/// Any step failed; nothing was taken (the panes built so far are dropped, ending nothing).
pub(super) fn receive(
    build: &str,
    endpoint: &Endpoint,
    channel: &Path,
    token: &str,
    limits: Limits,
) -> io::Result<Taken> {
    let stream = Arc::new(Endpoint::new(channel).connect()?);
    write_line(&stream, token)?;
    let identity = Identity::of_this_process(build);
    write_line(&stream, &serde_json::to_string(&identity).map_err(io::Error::other)?)?;
    let manifest: Manifest = read_json(&stream, STEP_TIMEOUT, "the handoff manifest").map_err(io::Error::other)?;
    // read whole before any refusal, so the exporter's send ends and it reads the reason
    let payloads = read_payloads(&stream, &manifest.panes, STEP_TIMEOUT)?;
    if let Err(reason) = manifest.check(&identity) {
        // the exporter fails the request with the reason; it is read before this end closes (a Windows pipe drops
        // what its reader has not read once the writer is gone)
        if write_line(&stream, &format!("{REFUSED}{reason}")).is_ok() {
            endpoint::close(&stream);
        }
        return Err(io::Error::other(reason));
    }
    write_line(&stream, "validated")?;
    let handles = sys::receive_handles(&stream, &manifest.panes, STEP_TIMEOUT)?;
    let mut panes = Vec::with_capacity(manifest.panes.len());
    for ((state, (vt, replies)), pty) in manifest.panes.iter().zip(payloads).zip(handles.ptys) {
        panes.push(Pane::import(state, &vt, &replies, pty, limits)?);
    }
    write_line(&stream, "restored")?;
    let listener = sys::take_endpoint(endpoint, handles.endpoint)?;
    Ok(Taken {
        stream,
        listener,
        panes,
    })
}

/// The importer's commit: whether `committed` came, whole, within [`STEP_TIMEOUT`].
pub(super) fn committed(stream: &Arc<Stream>) -> bool {
    match read_line(stream, STEP_TIMEOUT) {
        Ok(line) if line == "committed" => true,
        Ok(line) => {
            tracing::warn!(%line, "the handoff source sent something other than its commit");
            false
        }
        Err(err) => {
            tracing::warn!(%err, "the handoff source did not commit");
            false
        }
    }
}

/// Every pane's terminal and replies, as the manifest sizes them.
fn read_payloads(stream: &Arc<Stream>, panes: &[PaneState], timeout: Duration) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let _deadline = Deadline::start(stream, timeout);
    let mut reader = &**stream;
    panes
        .iter()
        .map(|pane| {
            if pane.vt_len.saturating_add(pane.replies_len) > MAX_PANE_PAYLOAD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("the live session {:?} is too large to take over", pane.name),
                ));
            }
            let mut take = |len: u64| -> io::Result<Vec<u8>> {
                let mut bytes = vec![0; usize::try_from(len).map_err(io::Error::other)?];
                reader.read_exact(&mut bytes)?;
                Ok(bytes)
            };
            Ok((take(pane.vt_len)?, take(pane.replies_len)?))
        })
        .collect()
}

/// `line` and its newline.
pub(super) fn write_line(stream: &Stream, line: &str) -> io::Result<()> {
    let mut writer = stream;
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// The next line, without its newline, read a byte at a time (nothing past it is consumed: raw payloads and
/// descriptors follow), within `timeout`.
pub(super) fn read_line(stream: &Arc<Stream>, timeout: Duration) -> Result<String, String> {
    let deadline = Deadline::start(stream, timeout);
    let mut reader = &**stream;
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match reader.read(&mut byte) {
            // the deadline's disconnect reads as the channel's end on Unix
            Ok(0) | Err(_) if deadline.expired() => {
                return Err(format!(
                    "the handoff peer did not answer within {}s",
                    timeout.as_secs_f32()
                ));
            }
            Ok(0) => return Err("the handoff peer closed the channel".to_owned()),
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if line.len() >= MAX_LINE => return Err("a handoff line exceeded its limit".to_owned()),
            Ok(_) => line.push(byte[0]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(format!("reading the handoff channel failed: {err}")),
        }
    }
    drop(deadline);
    String::from_utf8(line).map_err(|_| "a handoff line was not UTF-8".to_owned())
}

/// The next line, which must be `expected`; a refusal ([`REFUSED`]) fails with the replacement's reason.
fn expect(stream: &Arc<Stream>, expected: &str, timeout: Duration) -> Result<(), String> {
    match read_line(stream, timeout)? {
        line if line == expected => Ok(()),
        line => match line.strip_prefix(REFUSED) {
            Some(reason) => Err(format!("the replacement refused the handoff: {reason}")),
            None => Err(format!("the replacement answered {line:?} where {expected:?} was due")),
        },
    }
}

/// The next line, as JSON of `T` (`what` names it in an error).
fn read_json<T: for<'de> Deserialize<'de>>(stream: &Arc<Stream>, timeout: Duration, what: &str) -> Result<T, String> {
    let line = read_line(stream, timeout)?;
    serde_json::from_str(&line).map_err(|err| format!("{what} did not parse: {err}"))
}

/// A watchdog on a channel read: past its timeout it disconnects the stream (again every 100 ms, so a read started
/// after the first disconnect is woken too, as a Windows pipe needs), which fails the read; dropped, it stops.
pub(super) struct Deadline {
    stop: Option<mpsc::Sender<()>>,
    fired: Arc<AtomicBool>,
    watchdog: Option<thread::JoinHandle<()>>,
}

impl Deadline {
    pub(super) fn start(stream: &Arc<Stream>, timeout: Duration) -> Self {
        let (stop, stopped) = mpsc::channel::<()>();
        let fired = Arc::new(AtomicBool::new(false));
        let (stream, flag) = (Arc::clone(stream), Arc::clone(&fired));
        let watchdog = thread::Builder::new()
            .name("agterm-session-handoff-deadline".to_owned())
            .spawn(move || {
                let mut wait = timeout;
                while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(wait) {
                    flag.store(true, Ordering::Release);
                    endpoint::disconnect(&stream);
                    wait = Duration::from_millis(100);
                }
            })
            .ok();
        Self {
            stop: Some(stop),
            fired,
            watchdog,
        }
    }

    fn expired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(watchdog) = self.watchdog.take() {
            let _ = watchdog.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, vt: VtEncoding) -> PaneState {
        PaneState {
            name: name.to_owned(),
            pid: 42,
            created_at: 1_700_000_000,
            tracked_env: vec![("PWD".to_owned(), "/tmp".to_owned())],
            term: Some("xterm-ghostty".to_owned()),
            size: GridSize { cols: 80, rows: 24 },
            offset: 1234,
            revision: 7,
            vt,
            vt_len: 10,
            replies_len: 0,
        }
    }

    fn manifest(panes: Vec<PaneState>) -> Manifest {
        Manifest {
            version: HANDOFF_VERSION,
            source_build: "old".to_owned(),
            source_protocol: PROTOCOL_VERSION,
            source_pin: "rev a".to_owned(),
            expected_build: "new".to_owned(),
            expected_protocol: PROTOCOL_VERSION,
            panes,
        }
    }

    fn identity(pin: &str) -> Identity {
        Identity {
            build: "new".to_owned(),
            protocol: PROTOCOL_VERSION,
            pin: pin.to_owned(),
            pid: 9,
        }
    }

    #[test]
    fn the_manifest_round_trips_as_one_json_line() {
        let manifest = manifest(vec![pane("a", VtEncoding::Binary), pane("b", VtEncoding::Formatter)]);
        let line = serde_json::to_string(&manifest).unwrap();
        assert!(!line.contains('\n'));
        assert!(
            line.contains(r#""vt":"binary""#) && line.contains(r#""vt":"formatter""#),
            "{line}"
        );
        assert_eq!(serde_json::from_str::<Manifest>(&line).unwrap(), manifest);
        // a pane without its optional TERM reads as none
        let mut value = serde_json::to_value(pane("c", VtEncoding::Binary)).unwrap();
        value.as_object_mut().unwrap().remove("term");
        assert_eq!(serde_json::from_value::<PaneState>(value).unwrap().term, None);
    }

    #[test]
    fn the_replacement_takes_only_a_manifest_meant_for_it() {
        let binary = manifest(vec![pane("a", VtEncoding::Binary)]);
        assert_eq!(binary.check(&identity("rev a")), Ok(()));
        let pin = binary.check(&identity("rev b")).unwrap_err();
        assert!(
            pin.contains("libghostty-vt rev a cannot be read by libghostty-vt rev b"),
            "{pin}"
        );
        // the formatter crosses pins
        let formatter = manifest(vec![pane("a", VtEncoding::Formatter)]);
        assert_eq!(formatter.check(&identity("rev b")), Ok(()));
        let mut other = identity("rev a");
        other.build = "newer".to_owned();
        assert!(binary.check(&other).unwrap_err().contains(r#"expected build "new""#));
        let mut protocol = identity("rev a");
        protocol.protocol += 1;
        assert!(binary.check(&protocol).unwrap_err().contains("expected protocol"));
        let mut version = binary.clone();
        version.version += 1;
        assert!(
            version
                .check(&identity("rev a"))
                .unwrap_err()
                .contains("manifest is version")
        );
    }

    #[test]
    fn the_binary_snapshot_goes_only_between_equal_pins() {
        assert_eq!(encoding_for("rev a", "rev a"), VtEncoding::Binary);
        assert_eq!(encoding_for("rev a", "rev b"), VtEncoding::Formatter);
        assert_eq!(encoding_for(vt::PIN, vt::PIN), VtEncoding::Binary);
    }

    #[test]
    fn the_channel_is_a_directory_of_the_namespace_named_after_the_exporter() {
        assert_eq!(
            channel(Path::new("/tmp/ngterm-live-x"), 77),
            Path::new("/tmp/ngterm-live-x").join("handoff-77")
        );
    }

    /// A connected pair over an endpoint in a temporary directory.
    fn pair() -> (tempfile::TempDir, Arc<Stream>, Arc<Stream>) {
        let directory = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(directory.path().join("channel"));
        let listener = endpoint.bind().unwrap();
        let accepting = thread::spawn(move || listener.accept().unwrap());
        let client = endpoint.connect().unwrap();
        let server = accepting.join().unwrap();
        (directory, Arc::new(server), Arc::new(client))
    }

    #[test]
    fn lines_cross_whole_and_nothing_past_one_is_read() {
        let (_directory, server, client) = pair();
        write_line(&client, "token").unwrap();
        write_line(&client, r#"{"a":1}"#).unwrap();
        (&*client).write_all(b"raw").unwrap();
        assert_eq!(read_line(&server, STEP_TIMEOUT).unwrap(), "token");
        assert_eq!(read_line(&server, STEP_TIMEOUT).unwrap(), r#"{"a":1}"#);
        let mut raw = [0_u8; 3];
        (&*server).read_exact(&mut raw).unwrap();
        assert_eq!(&raw, b"raw");
        drop(client);
        assert!(read_line(&server, STEP_TIMEOUT).unwrap_err().contains("closed"));
    }

    #[test]
    fn a_step_waits_its_own_timeout_or_the_time_left() {
        let far = Instant::now() + Duration::from_secs(60);
        assert_eq!(step_within(far, STEP_TIMEOUT), Ok(STEP_TIMEOUT));
        let near = step_within(Instant::now() + Duration::from_secs(5), STEP_TIMEOUT).unwrap();
        assert!(
            near <= Duration::from_secs(5) && near > Duration::from_secs(4),
            "{near:?}"
        );
        let past = step_within(Instant::now(), STEP_TIMEOUT).unwrap_err();
        assert!(past.contains("did not reach its commit within 120s"), "{past}");
        // the client waits longer than the server can take to answer
        assert!(crate::server::spawn::HANDOFF_TIMEOUT > ANSWER_WITHIN);
    }

    #[test]
    fn a_silent_peer_fails_the_read_at_its_deadline() {
        let (_directory, server, _client) = pair();
        let started = Instant::now();
        let error = read_line(&server, Duration::from_millis(200)).unwrap_err();
        assert!(error.contains("did not answer"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    }

    fn request(build: &str, protocol: u32) -> HandoffRequest {
        HandoffRequest {
            import_exe: "/opt/ngterm/bin/agterm-session".to_owned(),
            expected_build: build.to_owned(),
            expected_protocol: protocol,
        }
    }

    #[test]
    fn a_failed_handoff_is_refused_again_with_its_reason_and_another_build_or_protocol_begins() {
        let failures = Failures::default();
        let handing_off = AtomicBool::new(false);
        let asked = request("new", PROTOCOL_VERSION);
        assert_eq!(failures.begin(&handing_off, &asked), Ok(()));
        assert!(handing_off.load(Ordering::Acquire));
        let running = failures.begin(&handing_off, &asked).unwrap_err();
        assert!(running.contains("already in progress"), "{running}");
        failures.fail(&handing_off, &asked, "the replacement refused the manifest");
        assert!(!handing_off.load(Ordering::Acquire), "connections are admitted again");

        // the same build and protocol, from any executable: the earlier reason, as it was, and nothing begun
        let moved = HandoffRequest {
            import_exe: "/elsewhere/agterm-session".to_owned(),
            ..asked.clone()
        };
        for again in [&asked, &moved] {
            assert_eq!(
                failures.begin(&handing_off, again),
                Err("the replacement refused the manifest".to_owned())
            );
            assert!(!handing_off.load(Ordering::Acquire));
        }
        // another protocol of the same build, or another build: a handoff of its own
        let other_protocol = request("new", PROTOCOL_VERSION + 1);
        assert_eq!(failures.begin(&handing_off, &other_protocol), Ok(()));
        failures.fail(&handing_off, &other_protocol, "refused too");
        assert_eq!(
            failures.begin(&handing_off, &request("newer", PROTOCOL_VERSION)),
            Ok(())
        );
    }

    /// Requests racing failing handoffs to one build begin it once: each failure is remembered before the next request
    /// could begin it.
    #[test]
    fn requests_racing_a_failing_handoff_begin_it_once() {
        const RACERS: usize = 8;
        let failures = Arc::new(Failures::default());
        let handing_off = Arc::new(AtomicBool::new(false));
        let start = Arc::new(std::sync::Barrier::new(RACERS));
        let racers: Vec<_> = (0..RACERS)
            .map(|_| {
                let (failures, handing_off, start) =
                    (Arc::clone(&failures), Arc::clone(&handing_off), Arc::clone(&start));
                thread::spawn(move || {
                    start.wait();
                    race(&failures, &handing_off)
                })
            })
            .collect();
        let begun: u32 = racers.into_iter().map(|racer| racer.join().unwrap()).sum();
        assert_eq!(begun, 1);
    }

    /// Asks for one handoff over and over, failing each one it begins; how many it began.
    fn race(failures: &Failures, handing_off: &AtomicBool) -> u32 {
        let asked = request("new", PROTOCOL_VERSION);
        let mut begun = 0;
        for _ in 0..2_000 {
            if failures.begin(handing_off, &asked).is_ok() {
                begun += 1;
                failures.fail(handing_off, &asked, "failed");
            }
        }
        begun
    }
}
