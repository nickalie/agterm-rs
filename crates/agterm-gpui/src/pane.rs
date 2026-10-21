//! A session pane's terminal: what it runs ([`PaneSpec`], the GTK build's `deck.rs` part of it), how it starts
//! ([`Launcher`] over `agterm-term`'s [`Pane::launch`]), and what it reports back to the model ([`report`], the GTK
//! build's `connect_terminal`/`connect_reports`).
//!
//! The terminal itself is a [`Terminal`] entity; the core's surface slot holds its [`TerminalSurface`] (the GTK
//! build's `PaneSurface`), whose pane token is the spec's.
//!
//! [`Pane::launch`]: agterm_term::Pane::launch
//! [`Terminal`]: crate::terminal::Terminal
//! [`TerminalSurface`]: crate::terminal::TerminalSurface

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use agterm_core::PaneReport;
use agterm_core::link_policy::ClickOrigin;
use agterm_core::model::{AppStore, SearchReport, Session, StatusKeystroke};
use agterm_core::pane_env::{self, PaneEnv, SessionPane, StatusPane};
use agterm_core::restore::spawn_plan::LaunchSeed;
use agterm_term::{Launch, PaneEvent};
use uuid::Uuid;

use crate::state::{AppState, Poster, post};
use crate::terminal::{Spawn, TerminalEvent, TerminalSpec};

/// A terminal a page runs: one of its session's panes.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneSpec {
    pub window: Uuid,
    pub workspace: Uuid,
    pub session: Uuid,
    /// `AGTERM_PANE`.
    pub pane: StatusPane,
    /// The pane token, `AGTERM_PANE_ID` (upstream's `paneIdentity.uuidString` and its split twin; a fresh one per
    /// scratch terminal, as upstream's `surfaceEnv`).
    pub token: String,
    pub cwd: String,
    /// What the pane runs instead of the login shell.
    pub command: Option<String>,
    /// Hold the pane after the command exits.
    pub wait: bool,
    /// The point size the terminal starts at: the session's persisted size, `None` for the configured one.
    pub font_size: Option<f64>,
}

impl PaneSpec {
    /// The primary pane of `session` in `store`, `None` when the session is not in its tree.
    #[must_use]
    pub fn primary(window: Uuid, store: &AppStore, session: Uuid) -> Option<Self> {
        let workspace = store.workspace_for_session(session)?.id();
        let session = store.session(session)?;
        Some(Self {
            window,
            workspace,
            session: session.id(),
            pane: StatusPane::Left,
            token: pane_env::uuid_string(session.pane_identity),
            cwd: session.effective_cwd().to_owned(),
            command: session.launch.command.clone(),
            wait: session.launch.wait,
            font_size: session.font_size,
        })
    }

    /// The split pane of `session`: its own cwd and creation command; `None` without a split identity.
    #[must_use]
    pub fn split(window: Uuid, workspace: Uuid, session: &Session) -> Option<Self> {
        Some(Self {
            window,
            workspace,
            session: session.id(),
            pane: StatusPane::Right,
            token: pane_env::uuid_string(session.split_pane_identity?),
            cwd: session.cwd_for(StatusPane::Right).to_owned(),
            command: session.split_launch.command.clone(),
            wait: session.split_launch.wait,
            font_size: session.font_size,
        })
    }

    /// A scratch terminal of `session`, in the primary's cwd, running the pending scratch command once (it is taken)
    /// or a login shell.
    #[must_use]
    pub fn scratch(window: Uuid, workspace: Uuid, session: &mut Session) -> Self {
        Self {
            window,
            workspace,
            session: session.id(),
            pane: StatusPane::Scratch,
            token: pane_env::uuid_string(Uuid::new_v4()),
            cwd: session.effective_cwd().to_owned(),
            command: session.scratch_command.take(),
            wait: false,
            font_size: session.font_size,
        }
    }

    /// The pane spec of the session pane carrying `token` in `store` of window `window`: what a replacement
    /// inherits; `None` for a scratch or a pane no longer there.
    #[must_use]
    pub fn of_token(window: Uuid, store: &AppStore, session: Uuid, token: &str) -> Option<Self> {
        let found = store.session(session)?;
        match found.pane_role_for_token(token)? {
            StatusPane::Left => Self::primary(window, store, session),
            StatusPane::Right => Self::split(window, store.workspace_for_session(session)?.id(), found),
            StatusPane::Scratch => None,
        }
    }

    /// Whether the pane's terminal waits for the core's seed (a session pane's, once its session is not dormant),
    /// rather than starting from the spec (the scratch).
    #[must_use]
    pub fn gated(&self) -> bool {
        self.pane != StatusPane::Scratch
    }

    /// The pane's environment: the session, window and workspace ids, `AGTERM_PANE` and the pane token.
    #[must_use]
    pub fn env(&self, context: &PaneContext) -> PaneEnv {
        pane_env::session(&SessionPane {
            session: self.session,
            window: Some(self.window),
            workspace: Some(self.workspace),
            socket_path: &context.socket,
            program_version: &context.program_version,
            pane: Some(self.pane),
            pane_token: Some(&self.token),
        })
    }

    /// Who the terminal is: its token and its start size.
    #[must_use]
    pub fn terminal_spec(&self) -> TerminalSpec {
        #[expect(clippy::cast_possible_truncation, reason = "a point size fits an f32")]
        let font_size = self.font_size.map(|size| size as f32);
        TerminalSpec {
            token: self.token.clone(),
            live: false,
            font_size,
        }
    }

    /// What the terminal runs without a seed: the spec's command (or the login shell) in its cwd, with its
    /// environment.
    #[must_use]
    pub fn launch(&self, context: &PaneContext) -> Launch {
        Launch {
            command: self.command.clone(),
            initial_input: None,
            working_directory: Some(PathBuf::from(&self.cwd)),
            wait_after_command: self.wait,
            env: self.env(context).into_iter().collect(),
            env_remove: Vec::new(),
            live: None,
        }
    }

    /// What the terminal runs with the core's `seed`: the seed's command, input and variables over the pane's own.
    #[must_use]
    pub fn seeded(&self, seed: &LaunchSeed, context: &PaneContext) -> Launch {
        let env: Vec<(String, String)> = self.env(context).into_iter().collect();
        Launch::from_seed(seed, Some(PathBuf::from(&self.cwd)), &env)
    }
}

/// What every shell of this instance is told about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneContext {
    /// `AGTERM_SOCKET`: the owned socket, or `<socket>.unavailable` when another instance owns it.
    pub socket: String,
    /// `TERM_PROGRAM_VERSION`.
    pub program_version: String,
}

/// Starts a launch: the spawn a terminal runs at its first size.
pub type Launcher = Rc<dyn Fn(&Launch) -> Spawn>;

/// What a terminal ran: a command (not the login shell), held after it exits. A held command's end is reported as
/// held (a remote pane's ssh, a replica overlay's job), and every held exit closes on the next key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ran {
    pub command: bool,
    pub wait: bool,
}

impl Ran {
    #[must_use]
    pub fn of(launch: &Launch) -> Self {
        Self {
            command: launch.command.is_some(),
            wait: launch.wait_after_command,
        }
    }
}

/// What the deck records about a pane's terminal beyond the entity: what it ran, shared with its event handler.
pub type RanCell = Rc<Cell<Ran>>;

/// What a terminal event asks of the model, for the pane carrying `token` of `session`.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    Pane(PaneReport),
    Search(SearchReport),
    /// The program ended and the pane closes now.
    Exited,
    /// A held command ended: the pane keeps showing it until a key.
    Held,
    /// The live font size changed.
    FontSize(f64),
    OpenUrl(String),
    /// A key the user pressed in the pane: user activity, and the status glyph it clears.
    Keystroke(StatusKeystroke),
    /// OSC 9/777; the focus sample travels beside it (`post_report`'s `focus`).
    Notification {
        title: String,
        body: String,
    },
}

/// What `event` reports, for a terminal that ran `ran`, on a machine named `hostname`. A key pressed on a lead-covered
/// pane (`covered`) only asks for the takeover: it is neither user activity nor a status keystroke, as in the GTK
/// build and upstream (`PaneLead.consumes` runs before `onUserInput`).
#[must_use]
pub fn report(event: &TerminalEvent, ran: Ran, hostname: &str, points: f32, covered: bool) -> Vec<Report> {
    match event {
        TerminalEvent::Pane(event) => pane_report(event, ran, hostname),
        TerminalEvent::StartSearch(needle) => vec![Report::Search(SearchReport::Started(
            Some(needle.clone()).filter(|needle| !needle.is_empty()),
        ))],
        TerminalEvent::EndSearch => vec![Report::Search(SearchReport::Ended)],
        TerminalEvent::FontSize => vec![Report::FontSize(f64::from(points))],
        TerminalEvent::OpenUrl(url) => vec![Report::OpenUrl(url.clone())],
        TerminalEvent::Keystroke(_) if covered => Vec::new(),
        TerminalEvent::Keystroke(keystroke) => vec![Report::Keystroke(*keystroke)],
        _ => Vec::new(),
    }
}

fn pane_report(event: &PaneEvent, ran: Ran, hostname: &str) -> Vec<Report> {
    match event {
        PaneEvent::Title(title) => vec![Report::Pane(PaneReport::Title(title.clone()))],
        PaneEvent::Pwd(raw) => agterm_term::pwd::reported_path(raw, hostname)
            .map(|path| vec![Report::Pane(PaneReport::Pwd(path))])
            .unwrap_or_default(),
        PaneEvent::SearchTotal(total) => vec![Report::Search(SearchReport::Total(*total))],
        PaneEvent::SearchSelected(selected) => vec![Report::Search(SearchReport::Selected(*selected))],
        PaneEvent::Notification { title, body } => vec![Report::Notification {
            title: title.clone(),
            body: body.clone(),
        }],
        PaneEvent::Exited(status) if !status.held => vec![Report::Exited],
        PaneEvent::Exited(_) if ran.command && ran.wait => vec![Report::Held],
        _ => Vec::new(),
    }
}

/// Posts `report` of the pane carrying `token` of `session` to the model (`focus`: the pane held its window's keyboard
/// and the window was active, sampled when the event came).
pub fn post_report(poster: &Poster, session: Uuid, token: &str, report: Report, focus: crate::state::NotifyFocus) {
    let token = token.to_owned();
    post(poster, move |state: &mut AppState| match report {
        Report::Pane(report) => state.report_pane(session, &token, &report),
        Report::Search(report) => {
            state.core.report_search(session, &token, report);
            state.settle();
        }
        Report::Exited => state.pane_exited(session, &token),
        Report::Held => {
            if state.core.remote_pane_held(session, &token) {
                state.settle();
            }
        }
        Report::FontSize(size) => {
            if state.core.report_font_size(session, &token, size) {
                state.settle();
            }
        }
        Report::OpenUrl(url) => state.follow_link(&url, ClickOrigin::Pane(session)),
        Report::Keystroke(keystroke) => state.pane_keystroke(session, &token, keystroke),
        Report::Notification { title, body } => state.terminal_notification(session, &token, &title, &body, focus),
    });
}

#[cfg(test)]
pub(crate) mod tests;
