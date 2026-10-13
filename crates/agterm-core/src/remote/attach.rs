//! `zmx.attach` and File ▸ Attach Remote (upstream's `attachRemoteSession` in `ControlServer+Zmx.swift` and
//! `AppActions+RemoteAttach.swift`): a local session attached to one of another machine's live sessions, each pane an
//! ssh into the origin's zmx daemon, marked remote, never persisted.
//!
//! The remote is resolved again before anything is created, so a session gone since the list was taken fails rather
//! than handing back a fresh shell wearing its name; everything that can fail is checked before the model is touched.

use std::collections::HashMap;
use std::time::Duration;

use agterm_proto::command::SessionCmd;
use agterm_proto::{
    Args, Command, ControlPickOutcome, ControlRemoteSession, ControlRemoteTree, ControlResult, PickItem, Request,
    Response,
};
use uuid::Uuid;

use super::ssh::{PaneLabel, attach_pane_command};
use super::tree::{self, RemoteCommandResult};
use super::viewer::{Answer, run_off_thread, take_answer};
use crate::Core;
use crate::commands::Outcome;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::modal::{MAX_PICK_ITEMS, PendingPick};
use crate::model::{AppStore, NewSession, RemoteBinding, RemoteOrigin, SplitAxis};
use crate::pane_env::uuid_string;
use crate::resolve::{Resolver, trimmed};
use crate::zmx::lead::LeadAttachment;

/// How often a waiting `zmx.attach` checks for the far side's answer.
const ATTACH_POLL: Duration = Duration::from_millis(50);
/// How long an attach's error HUD stays up (upstream's `remoteAttachErrorSeconds`).
const ERROR_SECONDS: f64 = 5.0;
/// A progress panel is timed, so a soft-closed session drops it (upstream's `remoteAttachProgressSeconds`).
const PROGRESS_SECONDS: f64 = 30.0;

/// What a finished remote read says: the far side's own refusal from stdout first, since its agtermctl prints it there
/// and exits nonzero; ssh's failures reach stderr. An ok-looking payload from a nonzero process is never accepted
/// (upstream's `remoteTree(host:)` remote form).
///
/// # Errors
///
/// The refusal or failure as the caller reads it.
pub(crate) fn read_remote_tree(result: &RemoteCommandResult, host: &str) -> Result<ControlRemoteTree, String> {
    if result.status != 0 {
        let stderr = result.stderr.trim();
        let detail = tree::remote_error(&result.stdout).or_else(|| (!stderr.is_empty()).then(|| stderr.to_owned()));
        return Err(detail.unwrap_or_else(|| format!("the remote command failed on {host}")));
    }
    tree::decode(&result.stdout)
        .map(|remote| remote.stamped(host))
        .map_err(|error| error.message())
}

/// Starts reading `host`'s attachable sessions off the main thread.
///
/// # Errors
///
/// `invalid host`: the host is not echoed, as it is refused for carrying control characters a terminal would print.
pub(crate) fn start_tree_read(core: &Core, host: &str) -> Result<Answer, ControlError> {
    let argv = tree::tree_command(host).map_err(|_| ControlError::rejected("invalid host"))?;
    run_off_thread(&core.remote.runner, argv, tree::TREE_DEADLINE)
        .ok_or_else(|| ControlError::rejected(format!("the remote command failed on {host}")))
}

/// Starts `zmx.attach HOST SESSION [--window]` (upstream's dispatcher arm): no `active` default, as the target names a
/// session on another machine, and a target with control characters is refused before it can reach an error message.
pub(crate) fn begin_attach(core: &mut Core, request: &Request) -> Dispatched {
    let args = request.args.as_ref();
    let Some(host) = trimmed(args.and_then(|args| args.host.as_deref())).map(str::to_owned) else {
        return Dispatched::done(Err(ControlError::rejected("zmx.attach requires a host")));
    };
    let Some(session) = trimmed(request.target.as_deref()).map(str::to_owned) else {
        return Dispatched::done(Err(ControlError::rejected("zmx.attach requires a remote session")));
    };
    if !tree::is_plain(&session) {
        return Dispatched::done(Err(ControlError::rejected("invalid remote session")));
    }
    let window = trimmed(args.and_then(|args| args.window.as_deref())).map(str::to_owned);
    match start_tree_read(core, &host) {
        Ok(answer) => poll_attach(answer, AttachRequest { host, session, window }),
        Err(error) => Dispatched::done(Err(error)),
    }
}

/// One attach as asked.
#[derive(Debug, Clone)]
pub(crate) struct AttachRequest {
    pub(crate) host: String,
    pub(crate) session: String,
    pub(crate) window: Option<String>,
}

fn poll_attach(answer: Answer, request: AttachRequest) -> Dispatched {
    Dispatched::Pending {
        after: ATTACH_POLL,
        resume: Box::new(move |core: &mut Core| match take_answer(&answer) {
            Some(result) => Dispatched::done(core.finish_attach(&result, &request)),
            None => poll_attach(answer, request),
        }),
    }
}

impl Core {
    /// Creates the local session attached to `request`'s remote session once its host's list arrived (upstream's
    /// `attachRemoteSession`).
    pub(crate) fn finish_attach(&mut self, result: &RemoteCommandResult, request: &AttachRequest) -> Outcome {
        let remote_tree = read_remote_tree(result, &request.host).map_err(ControlError::rejected)?;
        self.attach_from(&remote_tree, request)
    }

    fn attach_from(&mut self, remote_tree: &ControlRemoteTree, request: &AttachRequest) -> Outcome {
        let host = request.host.as_str();
        // by id only: names are mutable and not unique, and `zmx tree` prints the id for exactly this hand-off
        let remote = remote_tree
            .sessions
            .iter()
            .find(|candidate| candidate.id == request.session)
            .ok_or_else(|| ControlError::rejected(format!("no attachable session {} on {host}", request.session)))?;
        let (left, right) = panes_by_role(remote)
            .ok_or_else(|| ControlError::rejected(format!("{host} reported panes agterm cannot address")))?;
        // attaching is the user asking for the session HERE, so every pane claims the lead at once
        let leads = (LeadAttachment::new(true), LeadAttachment::new(true));
        let command = |daemon: &str, pane: &'static str, lead: &LeadAttachment| {
            let label = PaneLabel {
                session: &remote.name,
                pane,
            };
            attach_pane_command(host, &remote_tree.endpoint, daemon, label, Some(lead))
        };
        let unaddressable = || ControlError::rejected(format!("{host} reported a session agterm cannot address"));
        let primary = command(&left, "left", &leads.0).map_err(|_| unaddressable())?;
        let split = right
            .as_deref()
            .map(|daemon| command(daemon, "right", &leads.1))
            .transpose()
            .map_err(|_| unaddressable())?;
        let window = Resolver::new(self.library()).open_placement_window(request.window.as_deref())?;
        // the LOCAL working directory, not the remote one: a path on the far side may not exist here
        let home = self.library().home().to_owned();
        let store = self.library_mut().store_mut(window).ok_or(ControlError::NoOpenWindow)?;
        let workspace = store
            .current_workspace_id()
            .ok_or_else(|| ControlError::rejected("no window to attach into"))?;
        let created = store
            .add_session(
                workspace,
                NewSession {
                    command: Some(primary),
                    name: Some(remote.name.clone()),
                    wait: true,
                    remote_host: Some(host.to_owned()),
                    ..NewSession::at_cwd(home)
                },
            )
            .ok_or_else(|| ControlError::rejected("could not create the session"))?;
        let daemons = install_panes(store, created, &left, right.as_deref(), split, remote);
        let origin = RemoteOrigin {
            host: host.to_owned(),
            endpoint: remote_tree.endpoint.clone(),
            session_name: remote.name.clone(),
        };
        let identities: Vec<Uuid> = daemons.keys().copied().collect();
        store.bind_remote(
            RemoteBinding::new(remote.id.clone(), daemons, remote_tree.presentation, Some(origin)),
            created,
        );
        let primary_identity = store.session(created).map(|session| session.pane_identity);
        for identity in identities {
            let lead = if Some(identity) == primary_identity {
                leads.0.clone()
            } else {
                leads.1.clone()
            };
            self.zmx.leads.begin(identity, lead, false);
        }
        // the row's created event fired before the binding existed
        self.start_remote_presentation(created);
        self.request_pane_focus(created);
        Ok(ControlResult::id(uuid_string(created)))
    }
}

/// The left and the optional right daemon, by role and never by position: a payload with two lefts or no left fails
/// rather than quietly becoming one pane, or the wrong one.
fn panes_by_role(remote: &ControlRemoteSession) -> Option<(String, Option<String>)> {
    let mut roles: HashMap<&str, &str> = HashMap::new();
    for pane in &remote.panes {
        if !matches!(pane.pane.as_str(), "left" | "right") || roles.insert(&pane.pane, &pane.daemon).is_some() {
            return None;
        }
    }
    let left = roles.get("left")?;
    Some(((*left).to_owned(), roles.get("right").map(|right| (*right).to_owned())))
}

/// Gives the new session its split when the remote has one, and returns the daemon each local pane attaches to.
fn install_panes(
    store: &mut AppStore,
    created: Uuid,
    left: &str,
    right: Option<&str>,
    split: Option<String>,
    remote: &ControlRemoteSession,
) -> HashMap<Uuid, String> {
    if let Some(split) = split
        && let Some(session) = store.session_mut(created)
    {
        session.split_launch.command = Some(split);
        session.split_launch.wait = true;
        let axis = remote
            .split_axis
            .as_deref()
            .and_then(SplitAxis::from_wire)
            .unwrap_or_default();
        store.set_split_visibility(created, true, Some(axis));
    }
    let Some(session) = store.session(created) else {
        return HashMap::new();
    };
    let mut daemons = HashMap::from([(session.pane_identity, left.to_owned())]);
    if let (Some(right), Some(local)) = (right, session.split_pane_identity) {
        daemons.insert(local, right.to_owned());
    }
    daemons
}

// MARK: - File ▸ Attach Remote

/// Where one Attach Remote is.
#[derive(Debug)]
enum Step {
    /// The window's picker asks which configured machine to attach from.
    ChoosingHost { pick: String },
    /// The machine's sessions are being listed.
    Listing { host: String, answer: Answer },
    /// The window's picker asks which session.
    ChoosingSession { host: String, pick: String },
    /// The chosen session is being attached; its label names it in the progress panel.
    Attaching { request: AttachRequest, answer: Answer },
}

/// One File ▸ Attach Remote in progress (upstream's `attachRemote` task with its `RemoteAttachHud`).
#[derive(Debug)]
pub(crate) struct AttachFlow {
    window: Uuid,
    /// The session the progress and error panels go over, the window's selection when the flow started.
    hud_session: Option<Uuid>,
    /// The slot generation of the panel this flow opened; one another caller posted meanwhile is not its to close.
    owned_hud: Option<u64>,
    step: Step,
}

/// One remote session as a picker row: its name, then where it lives, its context, its cwd and what runs in it
/// (upstream's `remotePickItem`).
fn pick_item(session: &ControlRemoteSession) -> PickItem {
    let programs: Vec<String> = session
        .panes
        .iter()
        .filter_map(|pane| pane.foreground.as_ref()?.first())
        .map(|program| program.rsplit('/').next().unwrap_or(program).to_owned())
        .collect();
    let parts = [
        format!("{}/{}", session.window_name, session.workspace_name),
        session.context.clone().unwrap_or_default(),
        session.cwd.clone(),
        programs.join(" | "),
    ];
    let subtitle: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .filter(|part| !part.is_empty())
        .collect();
    PickItem {
        id: session.id.clone(),
        label: session.name.clone(),
        subtitle: Some(subtitle.join("  ·  ")),
    }
}

impl Core {
    /// The hosts `remotes.conf` lists; an unreadable file reads as none.
    #[must_use]
    pub fn configured_remotes(&self) -> Vec<super::remotes::RemoteEntry> {
        let path = self.config_dir().join("remotes.conf");
        match super::remotes::load_remotes(&path) {
            Ok((remotes, _)) => remotes.entries,
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "remotes.conf could not be read");
                Vec::new()
            }
        }
    }

    /// File ▸ Attach Remote in window `window` (upstream's `attachRemote()`): with one configured machine it lists that
    /// machine's sessions at once, with several the window's picker asks which first. Commit afterwards.
    pub fn attach_remote(&mut self, window: Uuid) {
        if !self.library().is_open(window) || self.modal_pending(window) {
            return;
        }
        let remotes = self.configured_remotes();
        let Some(first) = remotes.first() else {
            return;
        };
        let hud_session = self.library().store(window).and_then(AppStore::selected_session_id);
        if remotes.len() == 1 {
            let host = first.destination.clone();
            self.begin_listing(window, hud_session, host);
            return;
        }
        let items = remotes
            .iter()
            .map(|remote| PickItem {
                id: remote.destination.clone(),
                label: remote.label.clone(),
                subtitle: (remote.label != remote.destination).then(|| remote.destination.clone()),
            })
            .collect();
        let Some(pick) = self.open_internal_pick(window, items, "Attach from which Mac?") else {
            return;
        };
        self.remote.viewer.attach_flows.push(AttachFlow {
            window,
            hud_session,
            owned_hud: None,
            step: Step::ChoosingHost { pick },
        });
    }

    fn open_internal_pick(&mut self, window: Uuid, items: Vec<PickItem>, prompt: &str) -> Option<String> {
        let id = uuid_string(Uuid::new_v4());
        let pick = PendingPick {
            id: id.clone(),
            items,
            prompt: Some(prompt.to_owned()),
            query: None,
            allow_custom: false,
            selection: None,
        };
        self.modals.open_pick(window, pick).ok()?;
        self.sync_modals();
        Some(id)
    }

    fn begin_listing(&mut self, window: Uuid, hud_session: Option<Uuid>, host: String) {
        let mut flow = AttachFlow {
            window,
            hud_session,
            owned_hud: None,
            step: Step::ChoosingHost { pick: String::new() },
        };
        self.attach_progress(&mut flow, &format!("listing sessions on {host}…"));
        match start_tree_read(self, &host) {
            Ok(answer) => {
                flow.step = Step::Listing { host, answer };
                self.remote.viewer.attach_flows.push(flow);
            }
            Err(error) => self.attach_failed(&mut flow, &format!("{host}: {error}")),
        }
    }

    /// Moves every Attach Remote on: a picker answered, a listing or an attach arrived.
    pub(crate) fn advance_attach_flows(&mut self) {
        let flows = std::mem::take(&mut self.remote.viewer.attach_flows);
        for flow in flows {
            if let Some(flow) = self.advance_attach_flow(flow) {
                self.remote.viewer.attach_flows.push(flow);
            }
        }
    }

    /// One flow's next step; `None` once it ended.
    fn advance_attach_flow(&mut self, mut flow: AttachFlow) -> Option<AttachFlow> {
        match std::mem::replace(&mut flow.step, Step::ChoosingHost { pick: String::new() }) {
            Step::ChoosingHost { pick } => match self.picked(flow.window, &pick) {
                Picked::Waiting => {
                    flow.step = Step::ChoosingHost { pick };
                    Some(flow)
                }
                Picked::Chosen(host, _) => {
                    self.begin_listing(flow.window, flow.hud_session, host);
                    None
                }
                Picked::Ended => None,
            },
            Step::Listing { host, answer } => {
                let Some(result) = take_answer(&answer) else {
                    flow.step = Step::Listing { host, answer };
                    return Some(flow);
                };
                self.listed(flow, &host, &result)
            }
            Step::ChoosingSession { host, pick } => match self.picked(flow.window, &pick) {
                Picked::Waiting => {
                    flow.step = Step::ChoosingSession { host, pick };
                    Some(flow)
                }
                Picked::Chosen(session, label) => self.start_attach(flow, host, session, &label),
                Picked::Ended => None,
            },
            Step::Attaching { request, answer } => {
                let Some(result) = take_answer(&answer) else {
                    flow.step = Step::Attaching { request, answer };
                    return Some(flow);
                };
                match self.finish_attach(&result, &request) {
                    Ok(_) => self.attach_close_hud(&mut flow),
                    Err(error) => self.attach_failed(&mut flow, &error.to_string()),
                }
                None
            }
        }
    }

    /// The listing arrived: the window's picker asks which session to attach.
    fn listed(&mut self, mut flow: AttachFlow, host: &str, result: &RemoteCommandResult) -> Option<AttachFlow> {
        let sessions = match read_remote_tree(result, host) {
            Ok(remote) => remote.sessions,
            Err(error) => {
                self.attach_failed(&mut flow, &format!("{host}: {error}"));
                return None;
            }
        };
        if sessions.is_empty() || sessions.len() > MAX_PICK_ITEMS {
            let reason = if sessions.is_empty() {
                format!("nothing to attach on {host}")
            } else {
                format!("{host} offers too many sessions")
            };
            self.attach_failed(&mut flow, &reason);
            return None;
        }
        self.attach_close_hud(&mut flow);
        // the window may have closed during the ssh wait, and a picker no window renders would never resolve
        if !self.library().is_open(flow.window) {
            return None;
        }
        let items = sessions.iter().map(pick_item).collect();
        let pick = self.open_internal_pick(flow.window, items, &format!("Attach from {host}"))?;
        flow.step = Step::ChoosingSession {
            host: host.to_owned(),
            pick,
        };
        Some(flow)
    }

    fn start_attach(&mut self, mut flow: AttachFlow, host: String, session: String, label: &str) -> Option<AttachFlow> {
        self.attach_progress(&mut flow, &format!("attaching {label}…"));
        match start_tree_read(self, &host) {
            Ok(answer) => {
                flow.step = Step::Attaching {
                    request: AttachRequest {
                        host,
                        session,
                        window: Some(uuid_string(flow.window)),
                    },
                    answer,
                };
                Some(flow)
            }
            Err(error) => {
                self.attach_failed(&mut flow, &error.to_string());
                None
            }
        }
    }

    /// Picker `pick`'s answer in window `window`.
    fn picked(&self, window: Uuid, pick: &str) -> Picked {
        let result = self
            .modals
            .controller(window)
            .and_then(|controller| controller.result(pick))
            .or_else(|| self.modals.retained_pick(pick).map(|(_, result)| result));
        match result {
            Some(result) if result.result == ControlPickOutcome::Pending => Picked::Waiting,
            Some(result) if result.result == ControlPickOutcome::Picked => match result.id {
                Some(id) => {
                    let label = result.label.unwrap_or_else(|| id.clone());
                    Picked::Chosen(id, label)
                }
                None => Picked::Ended,
            },
            _ => Picked::Ended,
        }
    }

    // MARK: - the attach's panels

    fn attach_hud_message(text: &str) -> String {
        crate::keymap::failure::message("Attach Remote", text)
    }

    /// Opens the attach's panel over the session it started from; whether it opened.
    fn attach_open_hud(&mut self, flow: &mut AttachFlow, text: &str, hide_after: f64, spinner: bool) -> bool {
        let Some(session) = flow.hud_session else {
            return false;
        };
        let args = Args {
            message: Some(Self::attach_hud_message(text)),
            spinner: spinner.then(|| "bar".to_owned()),
            hide_after: Some(hide_after),
            window: Some(uuid_string(flow.window)),
            ..Args::default()
        };
        let response: Response = self.dispatch(&Request {
            cmd: Command::Session(SessionCmd::HudOpen),
            target: Some(uuid_string(session)),
            args: Some(args),
        });
        if !response.ok {
            return false;
        }
        flow.owned_hud = self
            .library()
            .store_for_session(session)
            .and_then(|store| store.session(session))
            .filter(|found| found.overlay.hud_active())
            .map(|found| found.overlay.generation);
        true
    }

    fn attach_progress(&mut self, flow: &mut AttachFlow, text: &str) {
        self.attach_open_hud(flow, text, PROGRESS_SECONDS, true);
    }

    /// Closes the panel this flow opened, never one another caller posted meanwhile.
    fn attach_close_hud(&mut self, flow: &mut AttachFlow) {
        let (Some(session), Some(owned)) = (flow.hud_session, flow.owned_hud.take()) else {
            return;
        };
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return;
        };
        let current = store
            .session(session)
            .filter(|found| found.overlay.hud_active())
            .map(|found| found.overlay.generation);
        if current == Some(owned) {
            store.close_hud(session);
        }
    }

    /// The attach failed: an error panel over its session, else a banner (upstream's `RemoteAttachHud.fail`).
    fn attach_failed(&mut self, flow: &mut AttachFlow, text: &str) {
        tracing::info!(reason = text, "attach remote failed");
        if self.attach_open_hud(flow, text, ERROR_SECONDS, false) {
            return;
        }
        self.attach_close_hud(flow);
        self.post_app_banner(
            "command-failure:Attach Remote",
            "Command failed",
            format!("Attach Remote ({text})"),
        );
    }
}

/// A picker's answer as a flow reads it.
enum Picked {
    Waiting,
    /// The picked row's id and label.
    Chosen(String, String),
    /// Dismissed, or gone with its window.
    Ended,
}
