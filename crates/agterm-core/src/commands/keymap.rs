//! `keymap.reload`, `keymap.list` and `keymap.run` (upstream's `ControlDispatcher` keymap arms and the app's
//! `reloadKeymap`/`listKeymap`/`runCustomCommand`), the keymap the app holds (`SettingsModel.keymap`), and the spawn
//! half of the custom-command runner (`CustomCommandRunner.run`/`spawn`/`report`): a command runs detached as
//! `/bin/sh -c` with the session's `{AGT_X}` tokens expanded and exported, and a failure posts a banner and, for
//! `--error-hud`, a panel over the session.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agterm_host::{LaunchSpec, ProcessExit};
use agterm_proto::command::{KeymapCmd, SessionCmd};
use agterm_proto::{Args, Command, ControlKeymapMenuItem, ControlResult, Request};
use uuid::Uuid;

use crate::Core;
use crate::error::ControlError;
use crate::keymap::failure;
use crate::keymap::{
    CommandContext, CommandPane, CustomCommand, Keymap, KeymapDiagnostic, SurfaceOwner, TypingSurface,
    ensure_starter_keymap, linux_defaults, load_keymap, project,
};
use crate::model::{OverlayPane, Session, StatusPane};
use crate::pane_env::uuid_string;
use crate::resolve::Resolver;

/// How long a failure panel stays up (upstream's `CustomCommandRunner.failureHudSeconds`).
pub const FAILURE_HUD_SECONDS: f64 = 10.0;

/// Reads the live menu's accelerators for `keymap.list`.
pub type MenuReader = Box<dyn Fn() -> Vec<ControlKeymapMenuItem>>;

/// The keymap the app holds and the last load's diagnostics.
pub(crate) struct KeymapRuntime {
    keymap: Keymap,
    diagnostics: Vec<KeymapDiagnostic>,
    /// Bumped on every load, so the frontend rebuilds its matcher and menu.
    generation: u64,
    menu_reader: Option<MenuReader>,
}

impl Default for KeymapRuntime {
    fn default() -> Self {
        Self {
            keymap: Keymap::empty(&linux_defaults()),
            diagnostics: Vec::new(),
            generation: 0,
            menu_reader: None,
        }
    }
}

impl fmt::Debug for KeymapRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeymapRuntime")
            .field("generation", &self.generation)
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

impl Core {
    /// The `keymap.conf` the app reads.
    #[must_use]
    pub fn keymap_path(&self) -> PathBuf {
        self.config_dir().join("keymap.conf")
    }

    /// The parsed keymap.
    #[must_use]
    pub fn keymap(&self) -> &Keymap {
        &self.keymap.keymap
    }

    /// The last load's diagnostics.
    #[must_use]
    pub fn keymap_diagnostics(&self) -> &[KeymapDiagnostic] {
        &self.keymap.diagnostics
    }

    /// Changes whenever the keymap was loaded again: the frontend rebuilds its matcher and menu when it moved.
    #[must_use]
    pub fn keymap_generation(&self) -> u64 {
        self.keymap.generation
    }

    /// Installs the reader of the live menu's accelerators `keymap.list` reports.
    pub fn set_menu_reader(&mut self, reader: MenuReader) {
        self.keymap.menu_reader = Some(reader);
    }

    /// The launch step: writes the commented starter `keymap.conf` when none exists and loads it (upstream's
    /// `SettingsModel` init); its problems post a banner, as upstream's scene task does.
    pub fn load_keymap_at_launch(&mut self) {
        ensure_starter_keymap(&self.keymap_path(), &linux_defaults());
        self.reload_keymap();
    }

    /// Re-reads `keymap.conf` (File > Reload Keymap, `keymap.reload`); diagnostics post a banner. The diagnostic count.
    pub fn reload_keymap(&mut self) -> usize {
        let (keymap, diagnostics) = load_keymap(&self.keymap_path(), &linux_defaults());
        self.keymap.keymap = keymap;
        self.keymap.diagnostics = diagnostics;
        self.keymap.generation += 1;
        let count = self.keymap.diagnostics.len();
        if count > 0 {
            let body = format!(
                "{count} issue{} — see Settings ▸ Key Mapping",
                if count == 1 { "" } else { "s" }
            );
            self.post_app_banner("keymap-diagnostics", "Keymap", body);
        }
        count
    }

    /// Runs `command` for session `session` of window `window` as the palette and `keymap.run` do: the pane is the
    /// focused one (the split when it holds focus and exists), its selection read from that pane. Why it did not
    /// start, `None` once it has.
    pub fn run_custom_command(&mut self, command: &CustomCommand, window: Uuid, session: Uuid) -> Option<String> {
        let pane = self
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .map(|state| {
                if state.split.focused && state.split_surface.is_present() {
                    CommandPane::Right
                } else {
                    CommandPane::Left
                }
            })?;
        self.run_custom_command_in(command, window, session, pane, None)
    }

    /// Runs `command` for a chord typed in `pane` of session `session` (upstream's `runFromKeybind`), `selection`
    /// being what the typing surface had selected (`None` reads the pane's own).
    pub fn run_custom_command_in(
        &mut self,
        command: &CustomCommand,
        window: Uuid,
        session: Uuid,
        pane: CommandPane,
        selection: Option<String>,
    ) -> Option<String> {
        let context = self.command_context(window, session, pane, selection)?;
        let cwd = self
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .map(|state| state.local_working_directory(&context.session_pwd, self.library().home()));
        self.spawn_custom_command(command, &context, cwd)
    }

    /// Runs `command` fired by a chord typed in window `window` (upstream's `handleKeyDown` `.fired` arm): context from
    /// the terminal that held the keyboard (`runFromKeybind`/`runFromSessionlessSurface`), so a chord typed in a
    /// zoomed, split, scratch or overlay terminal acts for that terminal's session and pane and reads its selection;
    /// with no session terminal holding it (the quick terminal, a page, a HUD, an emptied window) the active session's
    /// focused pane as the palette runs it, else the sessionless launcher (`runNoSurface`).
    pub fn run_custom_command_from_keybind(
        &mut self,
        command: &CustomCommand,
        window: Option<Uuid>,
        typing: Option<TypingSurface>,
    ) {
        if let Some(typing) = typing
            && let Some((window, session, pane)) = self.typing_pane(window, &typing.owner)
        {
            let selection = typing.selection.unwrap_or_default();
            let _ = self.run_custom_command_in(command, window, session, pane, Some(selection));
            return;
        }
        let active = self.library().active_window_id();
        let session = active
            .and_then(|window| self.library().store(window))
            .and_then(|store| store.active_session())
            .map(Session::id);
        match (active, session) {
            (Some(window), Some(session)) => {
                let _ = self.run_custom_command(command, window, session);
            }
            _ => self.run_custom_command_sessionless(command, active),
        }
    }

    /// The window, session and `$AGT_PANE` of a typing terminal; `None` when no open window holds it any more. The
    /// window it was typed in is asked first, as a session id can repeat across windows. A pane terminal names its
    /// pane by its token's current role; an overlay names the pane underneath it (upstream's `sessionlessPane`): the
    /// scratch whenever one is up, else a pane overlay's own pane, else the session's focused pane.
    fn typing_pane(&self, window: Option<Uuid>, owner: &SurfaceOwner) -> Option<(Uuid, Uuid, CommandPane)> {
        let session = owner.session();
        let library = self.library();
        let holds = |window: &Uuid| {
            library
                .store(*window)
                .is_some_and(|store| store.session(session).is_some())
        };
        let window = window
            .filter(holds)
            .or_else(|| library.open_ids().into_iter().find(holds))?;
        let state = library.store(window)?.session(session)?;
        let pane = match owner {
            SurfaceOwner::Pane { token, .. } => match state.pane_role_for_token(token)? {
                StatusPane::Left => CommandPane::Left,
                StatusPane::Right => CommandPane::Right,
                StatusPane::Scratch => CommandPane::Scratch,
            },
            SurfaceOwner::Overlay { generation, pane, .. } => {
                let overlay_pane = if *pane {
                    Some(state.pane_overlays.pane_of(*generation)?)
                } else if state.overlay.generation == *generation && state.overlay.program_active() {
                    None
                } else {
                    return None;
                };
                let right =
                    overlay_pane.map_or_else(|| state.focuses_split(), |pane| matches!(pane, OverlayPane::Right));
                if state.scratch_active {
                    CommandPane::Scratch
                } else if right {
                    CommandPane::Right
                } else {
                    CommandPane::Left
                }
            }
        };
        Some((window, session, pane))
    }

    /// A chord in a window with no session to act for (upstream's `spawnSessionless`): only a command naming no
    /// session-scoped token runs, with the window and socket tokens alone.
    pub fn run_custom_command_sessionless(&mut self, command: &CustomCommand, window: Option<Uuid>) {
        if CommandContext::references_session_scoped_context(&command.command) {
            tracing::info!(name = %command.name, "custom command references session context but no session is active; ignored");
            return;
        }
        let context = CommandContext {
            window_id: window.map(uuid_string).unwrap_or_default(),
            window_name: self.library().window_name(window),
            socket: self.child_socket(),
            ..CommandContext::default()
        };
        let _ = self.spawn_custom_command(command, &context, None);
    }

    /// Every `{AGT_X}` value for a fire in `pane` of `session` (upstream's `context(for:in:selectionSurface:pane:)`).
    fn command_context(
        &self,
        window: Uuid,
        session: Uuid,
        pane: CommandPane,
        selection: Option<String>,
    ) -> Option<CommandContext> {
        let store = self.library().store(window)?;
        let state = store.session(session)?;
        let role = status_pane(pane);
        let slot = state.pane_slot(role);
        let workspace = store.workspace_for_session(session);
        Some(CommandContext {
            session_id: uuid_string(session),
            session_name: state.display_name(),
            session_pwd: state.cwd_for(role).to_owned(),
            session_host: state
                .remote_host()
                .map(crate::model::naming::sanitized)
                .unwrap_or_default(),
            workspace_id: workspace
                .map(|workspace| uuid_string(workspace.id()))
                .unwrap_or_default(),
            workspace_name: workspace.map(|workspace| workspace.name.clone()).unwrap_or_default(),
            window_id: uuid_string(window),
            window_name: self.library().window_name(Some(window)),
            pane,
            pane_id: slot
                .get()
                .map(|surface| surface.pane_token().to_owned())
                .unwrap_or_default(),
            selection: selection
                .or_else(|| slot.get().and_then(agterm_host::Surface::read_selection))
                .unwrap_or_default(),
            socket: self.child_socket(),
        })
    }

    /// Starts the expanded command detached (upstream's `spawn(_:context:cwd:)`); a vanished working directory or a
    /// launch error is reported at once, a non-zero exit when it comes. Why it did not start, `None` once it has.
    fn spawn_custom_command(
        &mut self,
        command: &CustomCommand,
        context: &CommandContext,
        cwd: Option<String>,
    ) -> Option<String> {
        let session = context.session_id.clone();
        if let Some(cwd) = cwd.as_deref().filter(|cwd| !cwd.is_empty() && !Path::new(cwd).exists()) {
            let reason = format!(
                "session directory {} no longer exists",
                crate::model::naming::sanitized(cwd)
            );
            tracing::error!(name = %command.name, %reason, "custom command not started");
            self.report_command_failure(command, &reason, None, &session);
            return Some(reason);
        }
        let Some(host) = self.processes.host.clone() else {
            let reason = "custom commands are not available".to_owned();
            self.report_command_failure(command, &reason, None, &session);
            return Some(reason);
        };
        let mut env = context.environment();
        env.push(("PATH".to_owned(), self.child_path()));
        let spec = LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".to_owned(), context.expand(&command.command)],
            env,
            cwd: cwd.filter(|cwd| !cwd.is_empty()).map(PathBuf::from),
            stdin: None,
            capture_stderr: command.error_hud,
        };
        let failed = command.clone();
        let poster = Arc::clone(&host.poster);
        let on_exit = Box::new(move |exit: ProcessExit| {
            if exit.status == 0 {
                return;
            }
            poster(Box::new(move |core: &mut Core| {
                let detail = failure::detail(&exit.stderr_tail);
                core.report_command_failure(&failed, &format!("exit {}", exit.status), detail, &session);
            }));
        });
        match host.launcher.launch(&spec, on_exit) {
            Ok(_) => {
                self.record_command_usage(command);
                None
            }
            Err(err) => {
                let reason = err.to_string();
                tracing::error!(name = %command.name, %reason, "custom command failed to spawn");
                self.report_command_failure(command, &reason, None, &context.session_id);
                Some(reason)
            }
        }
    }

    /// A banner, obeying the notification setting, and for an `--error-hud` command a panel over its session that
    /// hides itself after [`FAILURE_HUD_SECONDS`] (upstream's `report(command:reason:detail:sessionID:)`). A rejected
    /// pane placement falls back to the whole session; other refusals are only logged.
    pub(crate) fn report_command_failure(
        &mut self,
        command: &CustomCommand,
        reason: &str,
        detail: Option<String>,
        session: &str,
    ) {
        self.post_app_banner(
            format!("command-failure:{}", command.name),
            "Command failed",
            format!("{} ({reason})", command.name),
        );
        if !command.error_hud || session.is_empty() {
            return;
        }
        let args = Args {
            message: Some(failure::message(&command.name, reason)),
            detail,
            position: Some(command.error_position.wire_name().to_owned()),
            hide_after: Some(FAILURE_HUD_SECONDS),
            pane: command.error_pane.map(|pane| pane.wire_name().to_owned()),
            ..Args::default()
        };
        let request = |args: Args| Request {
            cmd: Command::Session(SessionCmd::HudOpen),
            target: Some(session.to_owned()),
            args: Some(args),
        };
        let mut response = self.dispatch(&request(args.clone()));
        if !response.ok && args.pane.is_some() {
            tracing::info!(name = %command.name, error = ?response.error, "failure panel placement refused; showing it session-wide");
            response = self.dispatch(&request(Args { pane: None, ..args }));
        }
        if !response.ok {
            tracing::info!(name = %command.name, %reason, error = ?response.error, "custom command failed; no failure panel");
        }
    }

    /// Counts one run toward the custom-commands popover's most-used section.
    fn record_command_usage(&self, command: &CustomCommand) {
        if let Some(persistence) = self.persistence.as_ref() {
            crate::keymap::usage::UsageStore::new(persistence.directory()).record(command);
        }
    }
}

/// The model's pane role of a command pane.
fn status_pane(pane: CommandPane) -> StatusPane {
    match pane {
        CommandPane::Left => StatusPane::Left,
        CommandPane::Right => StatusPane::Right,
        CommandPane::Scratch => StatusPane::Scratch,
    }
}

/// Answers `keymap.*`.
pub(crate) fn dispatch(core: &mut Core, cmd: KeymapCmd, request: &Request) -> Result<ControlResult, ControlError> {
    match cmd {
        KeymapCmd::Reload => Ok(ControlResult::count(
            i64::try_from(core.reload_keymap()).unwrap_or(i64::MAX),
        )),
        KeymapCmd::List => {
            let menu = core.keymap.menu_reader.as_ref().map(|read| read());
            let path = core.keymap_path().to_string_lossy().into_owned();
            Ok(ControlResult::keymap(project(
                &core.keymap.keymap,
                &core.keymap.diagnostics,
                &path,
                menu,
            )))
        }
        KeymapCmd::Run => run(core, request),
    }
}

/// `keymap.run`: the command by its exact name, run for the addressed session.
fn run(core: &mut Core, request: &Request) -> Result<ControlResult, ControlError> {
    let args = request.args.as_ref();
    // matched exactly: trimming would make a name differing only by spaces unreachable
    let name = args
        .and_then(|args| args.name.as_deref())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ControlError::rejected("keymap.run requires a command name"))?;
    let command = core
        .keymap()
        .commands()
        .iter()
        .find(|command| command.name == name)
        .cloned()
        .ok_or_else(|| ControlError::rejected(format!("no custom command named {name}")))?;
    if core.processes.host.is_none() {
        return Err(ControlError::rejected("custom commands are not available"));
    }
    let window = args.and_then(|args| args.window.as_deref());
    let resolver = Resolver::new(core.library());
    let target = resolver.session(request.target.as_deref(), window)?;
    let session = target.session;
    if let Some(failure) = core.run_custom_command(&command, target.window, session) {
        return Err(ControlError::rejected(format!("{name} did not start: {failure}")));
    }
    Ok(ControlResult::id(uuid_string(session)))
}
