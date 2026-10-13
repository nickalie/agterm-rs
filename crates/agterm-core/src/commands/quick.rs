//! `quick`, `quick.type` and `quick.text`: the app's one quick terminal (upstream's `QuickTerminalController` in
//! `agterm/Views/QuickTerminal.swift`, `ControlServer+AppCommands.swift` > Quick terminal, and the validation of
//! `ControlDispatcher.swift` `dispatchQuickCommand`), plus the `quick` target of `surface.cursor` and `surface.zoom`
//! ([`super::surface_target`]).
//!
//! The quick terminal belongs to no window: one panel per app, summoned over whatever is on screen, so its state is
//! app-wide and `tree.quickVisible` reads the same in every window. The frontend creates its terminal lazily on the
//! first show and installs it here ([`Core::install_quick_surface`]); hiding keeps the shell alive, so a
//! shown-then-hidden quick terminal stays typable and readable, while `surface.*` address it only while shown. Its
//! shell exiting drops the terminal, and the next show spawns a fresh one.
//!
//! A control `quick show` pins the panel: losing the keyboard dismisses only a panel the user summoned, because a
//! script's next command runs while focus is still settling (upstream's `dismissOnFocusLoss: false`).

use agterm_host::{Surface, TextScope};
use agterm_proto::command::QuickCmd;
use agterm_proto::{Command, ControlResult, Request, Response};

use super::surface::{POLL_INTERVAL, buffer_extent, last_content_lines};
use super::typing::ATTEMPTS;
use super::{Outcome, ToggleMode, args_of};
use crate::Core;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::model::SurfaceSlot;

/// The quick terminal's id among the `surface.*` targets and in `tree.zoomedSurface`.
pub(crate) const QUICK: &str = "quick";

/// The quick terminal's state, app-wide.
#[derive(Debug, Default)]
pub struct QuickState {
    /// The panel is on screen (upstream's `isVisible`).
    visible: bool,
    /// The panel fills its screen (`surface.zoom --target quick`); cleared on hide.
    zoomed: bool,
    /// A control show pinned the panel, so losing focus does not dismiss it; cleared on hide and shell exit.
    pinned: bool,
    /// The long-lived terminal, created by the frontend on the first show and kept across hide and show.
    surface: SurfaceSlot,
}

impl QuickState {
    /// Shows the panel; a `pinned` show (the control path) keeps it up when it loses focus, and pins an already
    /// visible panel too.
    fn show(&mut self, pinned: bool) {
        self.pinned |= pinned;
        self.visible = true;
    }

    /// Hides the panel, ending its zoom and pin; the terminal stays.
    pub(crate) fn hide(&mut self) {
        self.visible = false;
        self.zoomed = false;
        self.pinned = false;
    }
}

impl Core {
    /// Whether the quick terminal panel is shown.
    #[must_use]
    pub fn quick_visible(&self) -> bool {
        self.quick.visible
    }

    /// Whether the shown quick terminal panel fills its screen.
    #[must_use]
    pub fn quick_zoomed(&self) -> bool {
        self.quick.zoomed
    }

    /// Whether the quick terminal has a terminal installed (shown once and its shell still running).
    #[must_use]
    pub fn quick_has_surface(&self) -> bool {
        self.quick.surface.is_present()
    }

    /// Installs the terminal the frontend created for the quick terminal; one already there is torn down (it never
    /// should be).
    pub fn install_quick_surface(&mut self, surface: Box<dyn Surface>) {
        if let Some(previous) = self.quick.surface.replace(surface) {
            tracing::error!("the quick terminal got a second terminal");
            previous.teardown();
        }
    }

    /// The user's toggle (a keybinding or a titlebar button, upstream's `toggle()`): hides a shown panel, else shows
    /// one that dismisses itself when it loses focus. Refused with no open window, as upstream's `canShow`. Commit
    /// afterwards.
    pub fn toggle_quick_terminal(&mut self) {
        if self.quick.visible {
            self.quick.hide();
        } else if !self.library().open_ids().is_empty() && !self.active_window_modal_pending() {
            self.quick.show(false);
        }
    }

    /// The panel lost the keyboard: a panel the user summoned hides, a pinned one stays (upstream's
    /// `handleResignKey`). Commit afterwards.
    pub fn quick_lost_focus(&mut self) {
        if self.quick.visible && !self.quick.pinned {
            self.quick.hide();
        }
    }

    /// The user dismissed the panel itself (its window was asked to close): it hides, its shell kept. Commit
    /// afterwards.
    pub fn dismiss_quick_terminal(&mut self) {
        self.quick.hide();
    }

    /// The quick terminal's shell exited: the panel goes with its terminal, so the next show spawns a fresh shell
    /// (upstream's `handleShellExit`). Commit afterwards.
    pub fn quick_shell_exited(&mut self) {
        self.quick.hide();
        self.quick.surface.teardown();
    }

    /// A keystroke in the quick terminal counts as user activity in the active window (upstream's quick-terminal
    /// `onUserInput` → `activeStore.noteUserActivity`), so an idle auto-follow cannot reshuffle the selection behind
    /// the panel while the user types. Arm the auto-follow timers afterwards.
    pub fn note_quick_input(&mut self) {
        if let Some(window) = self.library().active_window_id() {
            self.note_window_activity(window);
        }
    }

    /// Where a fresh quick terminal starts its shell: the active session's directory, through the remote rule, else
    /// home (upstream's `quickTerminalCwd`).
    #[must_use]
    pub fn quick_terminal_cwd(&self) -> String {
        let home = self.library().home();
        self.library()
            .active_store()
            .and_then(|store| store.active_session())
            .map_or_else(
                || home.to_owned(),
                |session| session.local_working_directory(session.effective_cwd(), home),
            )
    }
}

/// Answers `quick` (show, hide or toggle); a bare `ok`. `quick.type` and `quick.text` start through [`begin`].
pub(crate) fn dispatch(
    core: &mut Core,
    cmd: QuickCmd,
    request: &Request,
) -> Result<Option<ControlResult>, ControlError> {
    match cmd {
        QuickCmd::Visibility => set_visibility(core, args_of(request).mode.as_deref()).map(|()| None),
        // `commands::deferred` starts these; never routed here
        QuickCmd::Type | QuickCmd::Text => Err(ControlError::NotSupported(Command::Quick(cmd))),
    }
}

/// Upstream's `setQuickTerminal`: refused with no open window, as the panel must not outlive the last one; `show` runs
/// even over a visible panel, which is what pins one the user summoned.
fn set_visibility(core: &mut Core, mode: Option<&str>) -> Result<(), ControlError> {
    if core.library().open_ids().is_empty() {
        return Err(ControlError::NoOpenWindow);
    }
    let mode = ToggleMode::parse_with(mode, "show", "hide")
        .ok_or_else(|| ControlError::rejected(format!("invalid quick mode: {}", mode.unwrap_or("toggle"))))?;
    if mode.desired_value(core.quick.visible) {
        // not gated on the panel being hidden: `show` also carries the pin
        if let Some(error) = core
            .library()
            .active_window_id()
            .and_then(|id| core.modals.pending_modal_error(id))
        {
            return Err(ControlError::rejected(error));
        }
        core.quick.show(true);
    } else {
        core.quick.hide();
    }
    Ok(())
}

/// Whether the quick terminal is shown (`tree.quickVisible`).
pub(crate) fn visible(core: &Core) -> bool {
    core.quick.visible
}

/// Whether the quick terminal is zoomed (`tree.zoomedSurface` reads `quick` then, whatever the window's zoom).
pub(crate) fn zoomed(core: &Core) -> bool {
    core.quick.zoomed
}

fn not_available() -> ControlError {
    ControlError::rejected(format!("surface not available: {QUICK}"))
}

/// `surface.zoom --target quick` (upstream's `QuickTerminalController.setZoom`): the panel fills its screen instead of
/// a window, so no window's zoom holds it. Refused while the panel is not shown.
pub(crate) fn set_zoom(core: &mut Core, mode: ToggleMode) -> Outcome {
    if !core.quick.visible {
        return Err(not_available());
    }
    core.quick.zoomed = mode.desired_value(core.quick.zoomed);
    Ok(ControlResult::id(QUICK))
}

/// The quick terminal for `surface.cursor quick`: only while shown, as hiding keeps the terminal alive and a panel
/// shown once must not stay readable forever.
pub(crate) fn cursor_surface(core: &Core) -> Result<&dyn Surface, ControlError> {
    core.quick
        .surface
        .get()
        .filter(|_| core.quick.visible)
        .ok_or_else(not_available)
}

// MARK: - quick.type and quick.text

/// What a waiting `quick.type` or `quick.text` does once the terminal is up.
enum Job {
    /// Types the text as keystrokes.
    Type(String),
    /// Reads the buffer: the screen, or with `all` the scrollback too, or the last `lines` content lines of it.
    Text { all: bool, lines: Option<usize> },
}

impl Job {
    /// The answer once the probes ran out with the terminal still not up.
    fn unready(&self) -> ControlError {
        match self {
            Self::Type(_) => ControlError::rejected("quick terminal not realized"),
            Self::Text { .. } => ControlError::rejected("failed to read surface buffer"),
        }
    }

    /// Runs the job on `surface`; `None` while it is not realized.
    fn run(&self, surface: &mut dyn Surface) -> Option<Dispatched> {
        match self {
            Self::Type(text) => surface.type_text(text).then(|| Dispatched::Done(Response::success())),
            Self::Text { all, lines } => {
                let scope = if *all || lines.is_some() {
                    TextScope::All
                } else {
                    TextScope::Screen
                };
                let text = surface.read_text(scope)?;
                let text = match lines {
                    Some(lines) => last_content_lines(&text, *lines),
                    None => text,
                };
                Some(Dispatched::done(Ok(ControlResult::text(text))))
            }
        }
    }
}

/// Starts `quick.type` or `quick.text` (upstream's `typeQuick` and `readQuickText`): `quick show` flips visibility
/// before the frontend creates and realizes the terminal, so a shown terminal not up yet is probed again every
/// [`POLL_INTERVAL`], [`ATTEMPTS`] times; one never shown fails at once.
pub(crate) fn begin(core: &mut Core, request: &Request) -> Dispatched {
    match start(core, request) {
        Ok(job) => step(core, job, 0),
        Err(error) => Dispatched::Done(Response::from(error)),
    }
}

/// The argument checks, ahead of the window check as upstream's dispatcher validates before the app side runs.
fn start(core: &Core, request: &Request) -> Result<Job, ControlError> {
    let args = args_of(request);
    let job = if request.cmd == Command::Quick(QuickCmd::Type) {
        let text = args
            .text
            .clone()
            .ok_or_else(|| ControlError::rejected("quick.type requires text"))?;
        if text.contains('\0') {
            return Err(ControlError::rejected("text must not contain a NUL byte"));
        }
        Job::Type(text)
    } else {
        let (all, lines) = buffer_extent(args)?;
        Job::Text { all, lines }
    };
    if core.library().open_ids().is_empty() {
        return Err(ControlError::NoOpenWindow);
    }
    Ok(job)
}

/// Probe `attempt` (0 is the first, made at once).
fn step(core: &mut Core, job: Job, attempt: u32) -> Dispatched {
    if let Some(answer) = probe(core, &job) {
        return answer;
    }
    if attempt >= ATTEMPTS {
        return Dispatched::Done(Response::from(job.unready()));
    }
    Dispatched::Pending {
        after: POLL_INTERVAL,
        resume: Box::new(move |core: &mut Core| step(core, job, attempt + 1)),
    }
}

/// One probe: the answer once the terminal ran the job, or at once for a terminal never shown; `None` to keep waiting
/// (a terminal not realized yet, or a shown panel whose terminal the frontend has not created).
fn probe(core: &mut Core, job: &Job) -> Option<Dispatched> {
    let visible = core.quick.visible;
    match core.quick.surface.get_mut() {
        Some(surface) => job.run(surface),
        None if !visible => Some(Dispatched::Done(Response::from(ControlError::rejected(
            "quick terminal not open",
        )))),
        None => None,
    }
}
