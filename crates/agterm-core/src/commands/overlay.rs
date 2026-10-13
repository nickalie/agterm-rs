//! `session.overlay.*`: a caller's program in the session-wide overlay slot or covering one split pane, and the remote
//! overlay jobs' claim. Validation and its messages as upstream's `ControlDispatcher+Overlay.swift` (every check before
//! the target resolves), the effects as `agterm/Control/ControlServer+SessionActions.swift` (`openSessionOverlay`,
//! `closeSessionOverlay`, `resizeSessionOverlay`, `sessionOverlayResult`), `ControlServer+SurfaceIO.swift`
//! (`overlayReadSurface`) and `ControlServer+OverlayJobs.swift`.
//!
//! The slot is shared with the HUD ([`super::hud`]): a program replaces a HUD, the HUD is refused over a program, and
//! the read commands refuse a HUD with their own messages. HTML pages are validated here as upstream does, but no page
//! can open yet: they join with the HTML overlay task.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use agterm_host::TextScope;
use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, ControlResult, Request};
use uuid::Uuid;

use super::hud::PANE_NOT_VISIBLE;
use super::surface::{buffer_extent, last_content_lines};
use super::{Outcome, args_of, session_store, with_id};
use crate::Core;
use crate::error::{ControlError, Noun};
use crate::html::{self, HtmlNavigation};
use crate::model::{OverlayPane, OverlayProgram, PaneOverlayOpenFailure, Session, is_valid_color_hex};
use crate::overlay_jobs::{EXPIRY_SLACK, LAUNCH_WINDOW, OverlayJobFrame, OverlayJobOutcome, OverlayJobs, START_WINDOW};
use crate::resolve::{ACTIVE, trimmed};

// MARK: - messages

/// `session.overlay.*` against a slot holding nothing.
const NO_OVERLAY: &str = "no overlay";
const ALREADY_OPEN: &str = "overlay already open";
const STILL_RUNNING: &str = "overlay still running";
const NO_RESULT: &str = "no overlay result";
const NOT_REALIZED: &str = "overlay not realized";
/// Upstream's `OverlayHudError`.
const HUD_NO_RESULT: &str = "no overlay result: the slot holds a hud";
const HUD_NO_READ: &str = "no overlay to read: the slot holds a hud";
const HUD_FULL_RESIZE: &str = "a hud is always floating: pass --size-percent, not --full";
const HUD_WRITE_FAILED: &str = "could not write the hud message";
/// Upstream's `PaneOverlayError`.
const PANE_ALREADY_OPEN: &str = "pane overlay already open";
const INVALID_PANE: &str = "session.overlay: --pane must be left or right";
const PANE_SIZE_CONFLICT: &str = "session.overlay.open: --pane is mutually exclusive with --size-percent";
const PANE_RESIZE_UNSUPPORTED: &str = "session.overlay.resize: --pane is not supported (pane overlays are always full)";
/// Upstream's `OverlayHtmlError`.
const COMMAND_AND_HTML: &str = "session.overlay.open takes a command or --html, not both";
const COMMAND_AND_URL: &str = "session.overlay.open takes a command or --url, not both";
const HTML_AND_URL: &str = "session.overlay.open takes --html or --url, not both";
const WAIT_WITH_HTML: &str = "session.overlay.open: --wait cannot be combined with --html";
const WAIT_WITH_URL: &str = "session.overlay.open: --wait cannot be combined with --url";
const CWD_WITH_URL: &str = "session.overlay.open: --cwd cannot be combined with --url";
const INVALID_URL: &str = "session.overlay.open: --url must be an absolute http or https URL";
const NAVIGATION_WITHOUT_PAGE: &str = "session.overlay.open: --navigation requires --html or --url";
const JAVASCRIPT_WITHOUT_PAGE: &str = "session.overlay.open: --js requires --html or --url";
const CHROMELESS_REQUIRES_FILE: &str = "session.overlay.open: --chromeless requires --html";
const CHROMELESS_WITH_NAVIGATION: &str = "session.overlay.open: --chromeless cannot be combined with --navigation";
const PERSISTENT_REQUIRES_URL: &str = "session.overlay.open: --persistent requires --url";
const BROWSE_REQUIRES_URL: &str = "session.overlay.open: --browse requires --url";
const NOT_HTML: &str = "the overlay is not an html page";
const NAVIGATION: &str = "session.overlay.navigate requires back, forward, browser or finder";
const SUBMIT_VALUE: &str = "session.overlay.submit requires a value";
const INVALID_PAGE_ID: &str = "invalid page id";
const UNKNOWN_PAGE: &str = "no such page";
/// A page that passed validation: this build shows none yet (the HTML overlay task brings `WebKitGTK`).
const PAGES_UNAVAILABLE: &str = "session.overlay.open: html overlays are not available in this build";
/// Upstream's job claim refusals.
const JOB_REQUIRED: &str = "session.overlay.job.run requires a job id";
const INVALID_JOB: &str = "invalid job id";
const JOB_NOT_CLAIMABLE: &str = "job not claimable";

/// The `--size-percent` range error of `command` (`open` or `resize`).
fn size_range(command: &str) -> ControlError {
    ControlError::rejected(format!("session.overlay.{command}: --size-percent must be 1...100"))
}

// MARK: - dispatch

/// Answers a `session.overlay.*` command.
pub(crate) fn dispatch(core: &mut Core, cmd: SessionCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let target = request.target.as_deref();
    let window = args.window.as_deref();
    match cmd {
        SessionCmd::OverlayOpen => open(core, target, args),
        SessionCmd::OverlayClose => close(core, target, window, parse_overlay_pane(args.pane.as_deref())?),
        SessionCmd::OverlayResize => resize(core, target, args),
        SessionCmd::OverlayResult => result(core, target, args),
        SessionCmd::OverlaySubmit => {
            if args.value.is_none() {
                return Err(ControlError::rejected(SUBMIT_VALUE));
            }
            page_command(core, target, window, parse_overlay_pane(args.pane.as_deref())?)
        }
        SessionCmd::OverlayReload => page_command(core, target, window, parse_overlay_pane(args.pane.as_deref())?),
        SessionCmd::OverlayNavigate => {
            args.to
                .as_deref()
                .and_then(HtmlNavigation::from_wire)
                .ok_or_else(|| ControlError::rejected(NAVIGATION))?;
            page_command(core, target, window, parse_overlay_pane(args.pane.as_deref())?)
        }
        SessionCmd::OverlayCopy => copy(core, target, window, parse_overlay_pane(args.pane.as_deref())?),
        SessionCmd::OverlayText => text(core, target, args),
        SessionCmd::OverlayJobRun => claim_job(core, target),
        other => Err(ControlError::NotSupported(agterm_proto::Command::Session(other))),
    }
}

/// The `session.overlay.*` pane selector (upstream's `parseOverlayPane`): absent keeps the session-wide slot, the
/// pane aliases scope to one pane, anything else (the scratch included) is refused.
fn parse_overlay_pane(raw: Option<&str>) -> Result<Option<OverlayPane>, ControlError> {
    raw.map(|raw| OverlayPane::from_control_name(raw).ok_or_else(|| ControlError::rejected(INVALID_PANE)))
        .transpose()
}

/// What `session.overlay.open` shows (upstream's `OverlayContent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayContent {
    Program,
    Page,
}

/// Sorts a program from a page and refuses the flag combinations either cannot take (upstream's `overlayContent`).
fn overlay_content(command: &str, args: &Args, home: &str) -> Result<OverlayContent, ControlError> {
    let reject = |message: &str| Err(ControlError::rejected(message));
    let set = |flag: Option<bool>| flag == Some(true);
    match (args.html.as_deref(), args.url.as_deref()) {
        (None, None) => {
            if set(args.navigation) {
                return reject(NAVIGATION_WITHOUT_PAGE);
            }
            if set(args.javascript) {
                return reject(JAVASCRIPT_WITHOUT_PAGE);
            }
            if set(args.chromeless) {
                return reject(CHROMELESS_REQUIRES_FILE);
            }
            if set(args.persistent) {
                return reject(PERSISTENT_REQUIRES_URL);
            }
            if set(args.browse) {
                return reject(BROWSE_REQUIRES_URL);
            }
            if command.is_empty() {
                return reject("session.overlay.open requires a command");
            }
            Ok(OverlayContent::Program)
        }
        (Some(_), Some(_)) => reject(HTML_AND_URL),
        (Some(file), None) => {
            if !command.is_empty() {
                return reject(COMMAND_AND_HTML);
            }
            if set(args.wait) {
                return reject(WAIT_WITH_HTML);
            }
            if set(args.chromeless) && set(args.navigation) {
                return reject(CHROMELESS_WITH_NAVIGATION);
            }
            if set(args.persistent) {
                return reject(PERSISTENT_REQUIRES_URL);
            }
            if set(args.browse) {
                return reject(BROWSE_REQUIRES_URL);
            }
            if let Some(error) = html::grant_error(file, args.cwd.as_deref(), home) {
                return reject(&format!("session.overlay.open: {error}"));
            }
            Ok(OverlayContent::Page)
        }
        (None, Some(url)) => {
            if !command.is_empty() {
                return reject(COMMAND_AND_URL);
            }
            if set(args.wait) {
                return reject(WAIT_WITH_URL);
            }
            if args.cwd.is_some() {
                return reject(CWD_WITH_URL);
            }
            if set(args.chromeless) {
                return reject(CHROMELESS_REQUIRES_FILE);
            }
            if html::web_url(url).is_none() {
                return reject(INVALID_URL);
            }
            Ok(OverlayContent::Page)
        }
    }
}

// MARK: - open, close, resize

/// `session.overlay.open` (upstream's dispatcher arm, then `openSessionOverlay`): a program in the session-wide slot,
/// floating at `--size-percent` or covering the session, or in one pane's slot with `--pane`. Opening never selects
/// the session; `--follow` does.
fn open(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let command = args.command.clone().unwrap_or_default();
    let content = overlay_content(&command, args, core.library().home())?;
    if let Some(color) = args.color.as_deref()
        && !is_valid_color_hex(color)
    {
        return Err(ControlError::rejected(format!("invalid color: {color} (#rrggbb)")));
    }
    let pane = parse_overlay_pane(args.pane.as_deref())?;
    if pane.is_some() && args.size_percent.is_some() {
        return Err(ControlError::rejected(PANE_SIZE_CONFLICT));
    }
    if args.size_percent.is_some_and(|percent| !(1..=100).contains(&percent)) {
        return Err(size_range("open"));
    }
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    if content == OverlayContent::Page {
        return Err(ControlError::rejected(PAGES_UNAVAILABLE));
    }
    let program = OverlayProgram {
        command,
        cwd: args.cwd.clone(),
        wait: args.wait.unwrap_or(false),
        size_percent: args.size_percent,
        background_color: args.color.clone(),
    };
    match pane {
        Some(pane) => store
            .open_pane_overlay(id, pane, program)
            .map_err(|failure| pane_open_failure(failure, target))?,
        None if !store.open_overlay(id, program) => return Err(ControlError::rejected(ALREADY_OPEN)),
        None => {}
    }
    // both kinds run in the session's eager page whatever is selected, so opening needs no select: `--follow` is one
    if args.follow == Some(true) {
        store.select_session(Some(id));
    }
    Ok(with_id(id))
}

/// The error of a refused pane overlay open (upstream's `paneOverlayFailure`).
fn pane_open_failure(failure: PaneOverlayOpenFailure, target: Option<&str>) -> ControlError {
    match failure {
        PaneOverlayOpenFailure::UnknownSession => ControlError::NotFound {
            noun: Noun::Session,
            target: target.unwrap_or(ACTIVE).to_owned(),
        },
        PaneOverlayOpenFailure::AlreadyOpen => ControlError::rejected(PANE_ALREADY_OPEN),
        PaneOverlayOpenFailure::PaneNotVisible => ControlError::rejected(PANE_NOT_VISIBLE),
    }
}

/// `session.overlay.close` (upstream's `closeSessionOverlay`): a pane's overlay with `--pane`, else the session-wide
/// slot, a HUD in it included as a courtesy, since the slot is shared.
fn close(core: &mut Core, target: Option<&str>, window: Option<&str>, pane: Option<OverlayPane>) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let closed = match pane {
        Some(pane) => store.close_pane_overlay(id, pane),
        None => store.close_overlay(id),
    };
    if !closed {
        return Err(ControlError::rejected(NO_OVERLAY));
    }
    Ok(with_id(id))
}

/// `session.overlay.resize` (upstream's dispatcher arm, then `resizeSessionOverlay`): a program floats at the percent or
/// covers the session with `--full`; a HUD takes the percent as its width, never full, and is measured again for the
/// height that width needs, a refused body write putting the size back.
fn resize(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    // pane overlays are always full, so any `--pane` is refused, valid spelling or not
    if args.pane.is_some() {
        return Err(ControlError::rejected(PANE_RESIZE_UNSUPPORTED));
    }
    let full = args.full == Some(true);
    let size = match (full, args.size_percent) {
        (true, Some(_)) => {
            return Err(ControlError::rejected(
                "session.overlay.resize: --full is mutually exclusive with --size-percent",
            ));
        }
        (false, None) => {
            return Err(ControlError::rejected(
                "session.overlay.resize requires --size-percent or --full",
            ));
        }
        (_, Some(percent)) if !(1..=100).contains(&percent) => return Err(size_range("resize")),
        (_, percent) => percent,
    };
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let slot = store.session(id).map(|session| &session.overlay);
    let hud = slot.is_some_and(crate::model::OverlaySlot::hud_active);
    if size.is_none() && hud {
        return Err(ControlError::rejected(HUD_FULL_RESIZE));
    }
    let previous = slot.and_then(|slot| Some((slot.hud_panel_size()?, slot.hud.as_ref()?.resized_width_percent)));
    if !store.resize_overlay(id, size) {
        return Err(ControlError::rejected(NO_OVERLAY));
    }
    if hud && !core.refresh_hud(id) {
        if let (Some((size, forced)), Some(store)) = (previous, core.library_mut().store_for_session_mut(id)) {
            store.set_hud_size(id, size, forced);
        }
        return Err(ControlError::rejected(HUD_WRITE_FAILED));
    }
    Ok(with_id(id))
}

// MARK: - result and the page commands

/// `session.overlay.result` (upstream's `sessionOverlayResult`): the last program's exit status once it ended; a
/// `--page` reads a page's outcome instead.
fn result(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    if let Some(page) = args.page.as_deref() {
        parse_uuid(page).ok_or_else(|| ControlError::rejected(INVALID_PAGE_ID))?;
        // no page has opened in this build, so none has an outcome to read
        return Err(ControlError::rejected(UNKNOWN_PAGE));
    }
    let pane = parse_overlay_pane(args.pane.as_deref())?;
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    let (running, exit_code) = match pane {
        Some(pane) => (
            session.pane_overlay(pane).is_some(),
            session.pane_overlays.slot(pane).exit_code,
        ),
        None => (session.overlay.program_active(), session.overlay.exit_code),
    };
    if running {
        return Err(ControlError::rejected(STILL_RUNNING));
    }
    // a HUD opened after a program clears its result; the painter itself never reports a status
    if exit_code.is_none() && pane.is_none() && session.overlay.hud_active() {
        return Err(ControlError::rejected(HUD_NO_RESULT));
    }
    let status = exit_code.ok_or_else(|| ControlError::rejected(NO_RESULT))?;
    Ok(ControlResult {
        exit_code: Some(i64::from(status)),
        ..with_id(id)
    })
}

/// A UUID as Foundation's `UUID(uuidString:)` takes it: the hyphenated form, either case.
fn parse_uuid(raw: &str) -> Option<Uuid> {
    (raw.len() == 36).then(|| Uuid::parse_str(raw).ok()).flatten()
}

/// `session.overlay.submit`, `.reload` and `.navigate` once validated (upstream's `htmlOverlayCommandFailure`): the slot
/// they address must hold a page. No page can open in this build, so an occupied slot holds a program.
fn page_command(core: &mut Core, target: Option<&str>, window: Option<&str>, pane: Option<OverlayPane>) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let session = store
        .session(id)
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    let occupied = match pane {
        Some(pane) => session.pane_overlay(pane).is_some(),
        None => session.overlay.active && !session.overlay.hud_active(),
    };
    Err(ControlError::rejected(if occupied { NOT_HTML } else { NO_OVERLAY }))
}

// MARK: - copy and text

/// The terminal `session.overlay.copy` and `.text` read (upstream's `overlayReadSurface`): the pane's own overlay with
/// `pane`, else the session-wide slot's program; a HUD is refused, and a slot whose terminal is not up yet names the
/// overlay rather than the session.
fn overlay_read_surface(
    session: &Session,
    pane: Option<OverlayPane>,
) -> Result<&dyn agterm_host::Surface, ControlError> {
    let (occupied, slot) = if let Some(pane) = pane {
        (
            session.pane_overlay(pane).is_some(),
            &session.pane_overlays.slot(pane).surface,
        )
    } else {
        if session.overlay.hud_active() {
            return Err(ControlError::rejected(HUD_NO_READ));
        }
        (session.overlay.active, &session.overlay.surface)
    };
    if !occupied {
        return Err(ControlError::rejected(NO_OVERLAY));
    }
    slot.get()
        .filter(|surface| surface.is_realized())
        .ok_or_else(|| ControlError::rejected(NOT_REALIZED))
}

/// `session.overlay.copy` (upstream's `copySessionOverlaySelection`): the overlay's selection as text, which
/// `session.copy` cannot reach, never through the clipboard.
fn copy(core: &mut Core, target: Option<&str>, window: Option<&str>, pane: Option<OverlayPane>) -> Outcome {
    let (store, id) = session_store(core, target, window)?;
    let session = store
        .session(id)
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    let text = overlay_read_surface(session, pane)?
        .read_selection()
        .ok_or_else(|| ControlError::rejected("no selection"))?;
    Ok(ControlResult::text(text))
}

/// `session.overlay.text` (upstream's `readSessionOverlayText`): the covering program's drawn screen, as `session.text`
/// reads a pane. The extent is checked before the pane, so the same flags fail first on the same error there and here.
fn text(core: &mut Core, target: Option<&str>, args: &Args) -> Outcome {
    let (all, lines) = buffer_extent(args)?;
    let pane = parse_overlay_pane(args.pane.as_deref())?;
    let (store, id) = session_store(core, target, args.window.as_deref())?;
    let session = store
        .session(id)
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    let surface = overlay_read_surface(session, pane)?;
    let scope = if all || lines.is_some() {
        TextScope::All
    } else {
        TextScope::Screen
    };
    let text = surface
        .read_text(scope)
        .ok_or_else(|| ControlError::rejected("failed to read surface buffer"))?;
    Ok(ControlResult::text(match lines {
        Some(lines) => last_content_lines(&text, lines),
        None => text,
    }))
}

// MARK: - the frontend's hooks

impl Core {
    /// The terminal of pane overlay `generation` of session `session` ended (upstream's pane overlay `onExit`): the
    /// overlay closes wherever it sits now, a swap or promotion having moved it. Whether it closed; commit afterwards.
    pub fn pane_overlay_exited(&mut self, session: Uuid, generation: u64) -> bool {
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        let pane = store
            .session(session)
            .and_then(|found| found.pane_overlays.pane_of(generation));
        pane.is_some_and(|pane| store.close_pane_overlay(session, pane))
    }

    /// The user focused pane overlay `generation` of session `session` (upstream's pane overlay `onFocusChange`): the
    /// split's focus follows the pane it covers. Whether the focus moved; commit afterwards.
    pub fn pane_overlay_focused(&mut self, session: Uuid, generation: u64) -> bool {
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        let Some(pane) = store
            .session(session)
            .and_then(|found| found.pane_overlays.pane_of(generation))
        else {
            return false;
        };
        let role = match pane {
            OverlayPane::Left => crate::model::StatusPane::Left,
            OverlayPane::Right => crate::model::StatusPane::Right,
        };
        store.note_pane_focus(session, role)
    }

    /// Drops every pane overlay that can never come up (upstream's deck re-running `dropUnrealizedPaneOverlays` on a
    /// layout or zoom change): its pane is not laid out, no zoom claims it, and its terminal never realized.
    pub(crate) fn drop_stranded_pane_overlays(&mut self) {
        for window in self.library().open_ids() {
            let zoom = self.zoom_target(window);
            let Some(store) = self.library_mut().store_mut(window) else {
                continue;
            };
            for session in store.sessions_mut() {
                let id = session.id();
                session.drop_unrealized_pane_overlays(|pane| {
                    zoom.is_some_and(|zoom| zoom.session == id && zoom.surface.overlay_pane() == Some(pane))
                });
            }
        }
    }
}

// MARK: - remote overlay jobs

/// One helper connection of a claimed job, as the frontend hands it over: lines go out without their newline.
pub trait JobChannel {
    /// Queues one line; false when the connection cannot take it (gone or stalled).
    fn send(&self, line: Vec<u8>) -> bool;
    /// Closes the connection.
    fn shutdown(&self);
}

/// The overlay jobs' table and their helpers' connections (upstream's `overlayJobs`, `overlayJobStreams`,
/// `pendingJobCancels`).
#[derive(Default)]
pub(crate) struct JobRuntime {
    pub(crate) jobs: OverlayJobs,
    /// The adopted connection of each claimed job, with the serial of its adoption.
    streams: HashMap<String, (u64, Box<dyn JobChannel>)>,
    next_stream: u64,
    /// Cancels that reached a claimed job before its connection was adopted.
    pending_cancels: HashSet<String>,
    /// Jobs the table asked to cancel through their helper, until the core delivers it.
    cancel_requests: Rc<RefCell<Vec<String>>>,
    /// Expiry runs the frontend owes the table, after each delay.
    expiries: Vec<Duration>,
}

impl fmt::Debug for JobRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRuntime")
            .field("jobs", &self.jobs)
            .field("streams", &self.streams.keys().collect::<Vec<_>>())
            .field("pending_cancels", &self.pending_cancels)
            .field("expiries", &self.expiries)
            .finish_non_exhaustive()
    }
}

/// `session.overlay.job.run` (upstream's `claimOverlayJob`): the request is the claim, so an `ok` here is the one
/// winner of the race against the job's launch deadline; the frontend then hands the connection to
/// [`Core::adopt_overlay_job_stream`].
fn claim_job(core: &mut Core, target: Option<&str>) -> Outcome {
    let job = trimmed(target).ok_or_else(|| ControlError::rejected(JOB_REQUIRED))?;
    parse_uuid(job).ok_or_else(|| ControlError::rejected(INVALID_JOB))?;
    let requests = Rc::clone(&core.jobs.cancel_requests);
    let owned = job.to_owned();
    let hook = Box::new(move || requests.borrow_mut().push(owned.clone()));
    if core.jobs.jobs.claim(job, hook).is_none() {
        return Err(ControlError::rejected(JOB_NOT_CLAIMABLE));
    }
    core.jobs.expiries.push(START_WINDOW + EXPIRY_SLACK);
    Ok(ControlResult::id(job))
}

impl Core {
    /// Takes over the connection of a job whose claim was just answered `ok`, and sends its launch context (upstream's
    /// `adoptOverlayJobStream`). A job that ended between its claim and now must not launch: its connection closes.
    /// Returns the adoption's serial, which the connection's reports carry.
    pub fn adopt_overlay_job_stream(&mut self, job: &str, channel: Box<dyn JobChannel>) -> u64 {
        self.jobs.next_stream += 1;
        let serial = self.jobs.next_stream;
        let context = self.jobs.jobs.job(job).and_then(|found| match found.state {
            crate::overlay_jobs::OverlayJobState::Claimed { .. } => Some(found.context.clone()),
            _ => None,
        });
        let Some(context) = context else {
            channel.shutdown();
            return serial;
        };
        send_frame(channel.as_ref(), &OverlayJobFrame::Context { context });
        if self.jobs.pending_cancels.remove(job) {
            send_frame(channel.as_ref(), &OverlayJobFrame::Cancel);
        }
        self.jobs.streams.insert(job.to_owned(), (serial, channel));
        serial
    }

    /// One line from a job's helper (upstream's `OverlayJobStream.receive`): its start or its outcome. A line that is
    /// not a frame, or a frame only the origin sends, closes the connection.
    pub fn overlay_job_line(&mut self, job: &str, serial: u64, line: &[u8]) {
        if self.jobs.streams.get(job).is_none_or(|(current, _)| *current != serial) {
            return;
        }
        let jobs = &mut self.jobs.jobs;
        match OverlayJobFrame::parse(line) {
            Ok(OverlayJobFrame::Started) => jobs.started(job),
            Ok(OverlayJobFrame::Exited { code }) => {
                jobs.finish(job, OverlayJobOutcome::Exited(code));
            }
            Ok(OverlayJobFrame::Canceled) => {
                jobs.finish(job, OverlayJobOutcome::Canceled);
            }
            Ok(OverlayJobFrame::LaunchFailed { reason }) => {
                tracing::info!(%reason, "remote overlay job failed to launch");
                jobs.finish(job, OverlayJobOutcome::LaunchFailed);
            }
            Ok(OverlayJobFrame::Context { .. } | OverlayJobFrame::Cancel) | Err(_) => {
                tracing::warn!("closing a job helper connection on a bad frame");
                if let Some((_, channel)) = self.jobs.streams.get(job) {
                    channel.shutdown();
                }
            }
        }
    }

    /// A job's helper connection closed (upstream's `OverlayJobStream.closed`): a job it never reported an outcome for
    /// ends `unknown`.
    pub fn overlay_job_closed(&mut self, job: &str, serial: u64) {
        self.jobs.jobs.helper_gone(job);
        if self
            .jobs
            .streams
            .get(job)
            .is_some_and(|(current, _)| *current == serial)
        {
            self.jobs.streams.remove(job);
        }
    }

    /// Cancels job `id` (upstream's `OverlayJobs.cancel`, its hook reaching the helper through `cancelJobHelper`).
    /// False when the job is over or unknown.
    pub fn cancel_overlay_job(&mut self, id: &str) -> bool {
        let cancelled = self.jobs.jobs.cancel(id);
        let requests = std::mem::take(&mut *self.jobs.cancel_requests.borrow_mut());
        for job in requests {
            match self.jobs.streams.get(&job) {
                Some((_, channel)) => send_frame(channel.as_ref(), &OverlayJobFrame::Cancel),
                None => {
                    self.jobs.pending_cancels.insert(job);
                }
            }
        }
        cancelled
    }

    /// Runs the job table's expiry, at a deadline the frontend armed from [`Core::take_overlay_job_expiries`].
    pub fn expire_overlay_jobs(&mut self) {
        self.jobs.jobs.expire();
    }

    /// The expiry runs armed since the last take, each after its delay (upstream's `scheduleOverlayJobExpiry`).
    pub fn take_overlay_job_expiries(&mut self) -> Vec<Duration> {
        std::mem::take(&mut self.jobs.expiries)
    }

    /// Arms the expiry a newly handed-out job needs once its launch window ends.
    #[expect(
        dead_code,
        reason = "remote overlays register jobs; they join with the remote session tasks"
    )]
    pub(crate) fn arm_job_launch_expiry(&mut self) {
        self.jobs.expiries.push(LAUNCH_WINDOW + EXPIRY_SLACK);
    }
}

/// Sends `frame` on `channel`; a connection that cannot take it is closed (upstream's `OverlayJobStream.send`).
fn send_frame(channel: &dyn JobChannel, frame: &OverlayJobFrame) {
    let sent = serde_json::to_vec(frame).is_ok_and(|line| channel.send(line));
    if !sent {
        tracing::error!("closing a job helper connection that cannot take a frame");
        channel.shutdown();
    }
}
