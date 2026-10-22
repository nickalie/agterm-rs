//! What a built-in action does when its chord fires (upstream's `AppActions.perform(_:in:)`, the `AppActions` bodies
//! behind the menu items, and `PaletteContext` as `AppActions.paletteContext` fills it).
//!
//! An action runs only when the palette row behind it is enabled ([`PaletteCommand::is_enabled`], the one predicate
//! the menu item, the palette row and the keymap share); the actions without a palette row (window management and the
//! three palette launchers) stand down under any cover. The model work happens here, through the same control
//! handlers `ngtermctl` reaches where one exists, the palettes included ([`crate::palette`]); what needs a dialog, a
//! widget or a later task's UI comes back as a [`FrontendAction`] for the frontend.

use ngterm_proto::command::{FontCmd, SessionCmd, SurfaceCmd, WindowCmd, WorkspaceCmd};
use ngterm_proto::{
    Args, Command, ControlAskOutcome, ControlAskResult, ControlPickOutcome, ControlPickResult, Request,
};
use uuid::Uuid;

use crate::Core;
use crate::keymap::BuiltinAction;
use crate::model::{AppStore, SidebarMode};
use crate::palette::PaletteMode;
use crate::palette_catalog::{PaletteCommand, PaletteContext, PaletteFacts};
use crate::pane_env::uuid_string;
use crate::persist::settings::FlaggedViewLayout;
use crate::sidebar_actions::SidebarAction;

/// Where a chord fired: the window it fired in, and whether the quick terminal held the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionOrigin {
    pub window: Uuid,
    pub quick: bool,
}

/// What an action left to the frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrontendAction {
    /// Close Session found nothing to close: the window closes (upstream's `performClose`).
    CloseWindow(Uuid),
    /// Ask before closing `session` (the `confirmCloseSession` setting), then perform
    /// [`SidebarAction::CloseSessions`].
    ConfirmCloseSession { window: Uuid, session: Uuid },
    /// Rename Window: a dialog prefilled with `name`, committed through `window.rename`.
    RenameWindow { window: Uuid, name: String },
    /// Delete Window: confirmed while it holds sessions, then removed.
    DeleteWindow {
        window: Uuid,
        name: String,
        sessions: usize,
    },
    /// The sidebar's inline rename of a session row.
    RenameSession { window: Uuid, session: Uuid },
    /// The sidebar's inline rename of a workspace row.
    RenameWorkspace { window: Uuid, workspace: Uuid },
    /// Delete Workspace: confirmed while it holds sessions, as the row menu does.
    DeleteWorkspace { window: Uuid, workspace: Uuid },
    /// Open Directory…: the directory chooser, then [`SidebarAction::OpenDirectories`].
    OpenDirectory { window: Uuid, workspace: Uuid },
    /// Toggle the window's full screen.
    ToggleFullscreen(Uuid),
    /// Open the Settings popup over the window, or bring the open one to the front (the palette's Settings…).
    OpenSettings(Uuid),
}

/// A config file the menu's Edit items open in an editor overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFile {
    Keymap,
    Hooks,
    Ghostty,
}

/// The editor overlays the Edit items opened, by session and slot generation, so the editor's exit reloads what it
/// edited (upstream's recorded edit session).
#[derive(Debug, Default)]
pub(crate) struct ConfigEdits(Vec<(Uuid, u64, ConfigFile)>);

/// The outcome of [`Core::perform_builtin`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionEffect {
    /// The model did it (commit afterwards), or the action was gated out; the key is consumed either way.
    Done,
    /// The frontend finishes it.
    Frontend(FrontendAction),
}

impl Core {
    /// The UI facts of window `window` behind the palette's and the menu's enablement (upstream's
    /// `AppActions.paletteContext`).
    #[must_use]
    pub fn palette_context(&self, window: Uuid) -> PaletteContext {
        let mut facts = PaletteFacts::empty();
        let library = self.library();
        facts.set(PaletteFacts::CAN_STEP_WINDOWS, library.can_step_windows());
        facts.set(
            PaletteFacts::HAS_RECENT_CLOSED,
            !library.recent_closed_items().is_empty(),
        );
        facts.set(PaletteFacts::TERMINAL_ZOOM_ACTIVE, self.zoom_target(window).is_some());
        facts.set(PaletteFacts::DASHBOARD_OPEN, self.dashboard(window).is_some());
        facts.set(PaletteFacts::PICKER_ACTIVE, self.modal_pending(window));
        facts.set(PaletteFacts::HAS_REMOTES, !self.configured_remotes().is_empty());
        let Some(store) = library.store(window) else {
            return PaletteContext::new(facts);
        };
        facts |= store_facts(store, self.settings().effective_flagged_view_layout());
        let mut context = PaletteContext::new(facts);
        if let Some(axis) = store
            .active_session()
            .filter(|session| session.split.exists)
            .map(|s| s.split.axis)
        {
            context = context.with_split_axis(axis);
        }
        context
    }

    /// Runs built-in `action` for a chord fired at `origin` (upstream's `AppActions.perform(_:in:)`): nothing when its
    /// menu item would be disabled. Commit afterwards.
    pub fn perform_builtin(&mut self, origin: ActionOrigin, action: BuiltinAction) -> ActionEffect {
        let window = origin.window;
        if !self.library().is_open(window) {
            return ActionEffect::Done;
        }
        let context = self.palette_context(window);
        let enabled = match PaletteCommand::for_builtin(action) {
            Some(command) => command.is_enabled(&context),
            None => !context.modal_active(),
        };
        if !enabled {
            return ActionEffect::Done;
        }
        if action == BuiltinAction::CloseSession {
            return self.close_active_session_or_window(origin);
        }
        self.perform_enabled(window, action)
    }

    /// Runs palette command `command` from the app menu in window `window`: a built-in's through
    /// [`Core::perform_builtin`], the menu-only rows here. Nothing when the row is disabled. Commit afterwards.
    pub fn perform_menu_command(&mut self, window: Uuid, command: PaletteCommand) -> ActionEffect {
        if let Some(action) = command.builtin_action() {
            return self.perform_builtin(ActionOrigin { window, quick: false }, action);
        }
        if !self.library().is_open(window) || !command.is_enabled(&self.palette_context(window)) {
            return ActionEffect::Done;
        }
        match command {
            PaletteCommand::ReloadKeymap => {
                self.reload_keymap();
            }
            PaletteCommand::ReloadHooks => {
                self.reload_hooks();
            }
            PaletteCommand::ReloadConfig => self.request_config_reload(),
            PaletteCommand::EditKeymap => self.edit_config_file(window, ConfigFile::Keymap),
            PaletteCommand::EditHooks => self.edit_config_file(window, ConfigFile::Hooks),
            PaletteCommand::EditGhosttyConfig => self.edit_config_file(window, ConfigFile::Ghostty),
            _ => self.perform_store_command(window, command),
        }
        ActionEffect::Done
    }

    fn perform_store_command(&mut self, window: Uuid, command: PaletteCommand) {
        let layout = self.settings().effective_flagged_view_layout();
        let active = self.active_session_of(window);
        let current = self.library().store(window).and_then(AppStore::current_workspace_id);
        if let (PaletteCommand::AddWorkspaceToFocus, Some(workspace)) = (command, current) {
            self.sidebar_act(
                window,
                SidebarAction::SetFocusMembership {
                    workspace,
                    member: true,
                },
            );
            return;
        }
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        match (command, active) {
            (PaletteCommand::ExpandWorkspaces, _) => store.expand_all_workspaces(layout),
            (PaletteCommand::CollapseWorkspaces, _) => store.collapse_other_workspaces(layout),
            (PaletteCommand::ClearFlagged, _) => store.clear_flags(),
            (PaletteCommand::ClearFocus, _) => store.clear_focus(),
            (PaletteCommand::SwapPanes, Some(session)) => {
                if store.swap_panes(session).is_ok() {
                    self.request_pane_focus(session);
                }
            }
            (PaletteCommand::CloseSplit, Some(session)) => store.close_split(session),
            _ => {}
        }
    }

    /// Edit Keymap/Hooks/ghostty.conf (upstream's `editKeymap`): the file, a commented starter when missing, opened in
    /// the user's editor in a 95% overlay over the active session; the editor's exit reloads it.
    fn edit_config_file(&mut self, window: Uuid, file: ConfigFile) {
        let Some(session) = self.active_session_of(window) else {
            return;
        };
        let path = match file {
            ConfigFile::Keymap => {
                let path = self.keymap_path();
                crate::keymap::ensure_starter_keymap(&path, &crate::keymap::linux_defaults());
                path
            }
            ConfigFile::Hooks => {
                let path = self.hooks_path();
                crate::hooks::ensure_starter_hooks(&path);
                path
            }
            ConfigFile::Ghostty => crate::paths::ghostty_config_path(&self.config_dir()),
        };
        let args = Args {
            command: Some(crate::keymap::editor_command(&path.to_string_lossy())),
            size_percent: Some(95),
            ..Args::default()
        };
        // only an editor that actually opened is recorded (upstream records the edit when `openOverlay` succeeds): a
        // refused open leaves the editor already in the slot, and its record, alone.
        let opened = self.try_request(
            Command::Session(SessionCmd::OverlayOpen),
            Some(uuid_string(session)),
            window_args(window, args),
        );
        if !opened {
            return;
        }
        let generation = self
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .filter(|state| state.overlay.program_active())
            .map(|state| state.overlay.generation);
        if let Some(generation) = generation {
            self.config_edits.0.retain(|(edited, _, _)| *edited != session);
            self.config_edits.0.push((session, generation, file));
        }
    }

    /// An overlay of `session` at slot generation `generation` ended: an editor the Edit items opened reloads its file.
    pub(crate) fn config_edit_ended(&mut self, session: Uuid, generation: u64) -> Option<ConfigFile> {
        let index = self
            .config_edits
            .0
            .iter()
            .position(|(edited, opened, _)| *edited == session && *opened == generation)?;
        let (_, _, file) = self.config_edits.0.remove(index);
        match file {
            ConfigFile::Keymap => {
                self.reload_keymap();
            }
            ConfigFile::Hooks => {
                self.reload_hooks();
            }
            ConfigFile::Ghostty => self.request_config_reload(),
        }
        Some(file)
    }

    fn perform_enabled(&mut self, window: Uuid, action: BuiltinAction) -> ActionEffect {
        match action {
            BuiltinAction::NewWindow
            | BuiltinAction::RenameWindow
            | BuiltinAction::DeleteWindow
            | BuiltinAction::PreviousWindow
            | BuiltinAction::NextWindow => self.perform_window_action(window, action),
            BuiltinAction::NewWorkspace
            | BuiltinAction::RenameWorkspace
            | BuiltinAction::DeleteWorkspace
            | BuiltinAction::FocusWorkspace
            | BuiltinAction::ToggleWorkspaceFilter
            | BuiltinAction::PreviousWorkspace
            | BuiltinAction::NextWorkspace
            | BuiltinAction::ToggleWorkspaceCollapse
            | BuiltinAction::ToggleFlaggedView
            | BuiltinAction::ToggleSidebar => self.perform_workspace_action(window, action),
            BuiltinAction::QuickTerminal
            | BuiltinAction::Dashboard
            | BuiltinAction::ToggleFullscreen
            | BuiltinAction::SessionPalette
            | BuiltinAction::CommandPalette
            | BuiltinAction::CustomCommandPalette
            | BuiltinAction::ShowAttention
            | BuiltinAction::SelectTheme
            | BuiltinAction::AttachRemote
            | BuiltinAction::ReopenRecent
            | BuiltinAction::IncreaseFontSize
            | BuiltinAction::DecreaseFontSize
            | BuiltinAction::ResetFontSize => self.perform_app_action(window, action),
            _ => self.perform_session_action(window, action),
        }
    }

    fn perform_window_action(&mut self, window: Uuid, action: BuiltinAction) -> ActionEffect {
        let name = self.library().window_name(Some(window));
        match action {
            BuiltinAction::NewWindow => self.run_request(Command::Window(WindowCmd::New), None, Args::default()),
            BuiltinAction::RenameWindow => return frontend(FrontendAction::RenameWindow { window, name }),
            BuiltinAction::DeleteWindow => {
                if !self.library().can_remove_window() {
                    return ActionEffect::Done;
                }
                let sessions = self.library().store(window).map_or(0, |store| store.sessions().count());
                return frontend(FrontendAction::DeleteWindow { window, name, sessions });
            }
            _ => {
                let to = if action == BuiltinAction::PreviousWindow {
                    "prev"
                } else {
                    "next"
                };
                self.run_request(Command::Window(WindowCmd::Go), None, args_to(to));
            }
        }
        ActionEffect::Done
    }

    fn perform_workspace_action(&mut self, window: Uuid, action: BuiltinAction) -> ActionEffect {
        let current = self.library().store(window).and_then(AppStore::current_workspace_id);
        match (action, current) {
            (BuiltinAction::NewWorkspace, _) => self.sidebar_act(window, SidebarAction::NewWorkspace),
            (BuiltinAction::RenameWorkspace, Some(workspace)) => {
                return frontend(FrontendAction::RenameWorkspace { window, workspace });
            }
            (BuiltinAction::DeleteWorkspace, Some(workspace)) => {
                return frontend(FrontendAction::DeleteWorkspace { window, workspace });
            }
            (BuiltinAction::FocusWorkspace, Some(workspace)) => {
                self.sidebar_act(window, SidebarAction::ToggleFocus(workspace));
            }
            (BuiltinAction::ToggleWorkspaceFilter, _) => self.sidebar_act(window, SidebarAction::ToggleFocusFilter),
            (BuiltinAction::ToggleFlaggedView, _) => self.sidebar_act(window, SidebarAction::ToggleFlaggedView),
            (BuiltinAction::PreviousWorkspace | BuiltinAction::NextWorkspace, _) => {
                let to = if action == BuiltinAction::PreviousWorkspace {
                    "prev"
                } else {
                    "next"
                };
                self.run_request(
                    Command::Workspace(WorkspaceCmd::Go),
                    None,
                    window_args(window, args_to(to)),
                );
            }
            (BuiltinAction::ToggleWorkspaceCollapse, Some(workspace)) => {
                self.toggle_workspace_collapse(window, workspace);
            }
            (BuiltinAction::ToggleSidebar, _) => {
                if let Some(store) = self.library_mut().store_mut(window) {
                    store.toggle_sidebar_visible();
                }
            }
            _ => {}
        }
        ActionEffect::Done
    }

    fn perform_app_action(&mut self, window: Uuid, action: BuiltinAction) -> ActionEffect {
        let launcher = match action {
            BuiltinAction::QuickTerminal => {
                self.toggle_quick_terminal();
                None
            }
            BuiltinAction::Dashboard => {
                self.toggle_dashboard(window);
                None
            }
            BuiltinAction::ReopenRecent => {
                if let Some(owner) = self
                    .library_mut()
                    .reopen_latest_recent_closed_reporting_window(Some(window))
                {
                    self.take_frontmost(owner);
                }
                None
            }
            BuiltinAction::IncreaseFontSize => self.run_font(window, FontCmd::Inc),
            BuiltinAction::DecreaseFontSize => self.run_font(window, FontCmd::Dec),
            BuiltinAction::ResetFontSize => self.run_font(window, FontCmd::Reset),
            BuiltinAction::ToggleFullscreen => Some(FrontendAction::ToggleFullscreen(window)),
            BuiltinAction::SessionPalette => self.launch_palette(window, PaletteMode::Sessions),
            BuiltinAction::CommandPalette => self.launch_palette(window, PaletteMode::Actions),
            BuiltinAction::CustomCommandPalette => self.launch_palette(window, PaletteMode::CustomCommands),
            BuiltinAction::ShowAttention => self.launch_palette(window, PaletteMode::Attention),
            BuiltinAction::SelectTheme => {
                self.open_palette(window, PaletteMode::Themes);
                None
            }
            // File ▸ Attach Remote runs in the core: its pickers are the window's modal slot ([`crate::remote`])
            _ => {
                self.attach_remote(window);
                None
            }
        };
        launcher.map_or(ActionEffect::Done, frontend)
    }

    fn perform_session_action(&mut self, window: Uuid, action: BuiltinAction) -> ActionEffect {
        if action == BuiltinAction::UndoClose {
            if let Some(store) = self.library_mut().store_mut(window) {
                store.undo_pending_close(None);
            }
            return ActionEffect::Done;
        }
        // New Session and Open Directory need only the current workspace (upstream's `newSession`/`openDirectory`),
        // so they still work in a window whose last session closed.
        if action == BuiltinAction::NewSession {
            self.sidebar_act(window, SidebarAction::NewSessionInCurrent);
            return ActionEffect::Done;
        }
        if action == BuiltinAction::OpenDirectory {
            return self
                .library()
                .store(window)
                .and_then(AppStore::current_workspace_id)
                .map_or(ActionEffect::Done, |workspace| {
                    frontend(FrontendAction::OpenDirectory { window, workspace })
                });
        }
        let Some(session) = self.active_session_of(window) else {
            return ActionEffect::Done;
        };
        self.perform_on_session(window, session, action)
    }

    fn perform_on_session(&mut self, window: Uuid, session: Uuid, action: BuiltinAction) -> ActionEffect {
        let target = Some(uuid_string(session));
        let on = |args: Args| window_args(window, args);
        match action {
            BuiltinAction::RenameSession => return frontend(FrontendAction::RenameSession { window, session }),
            BuiltinAction::DuplicateSession => self.sidebar_act(window, SidebarAction::Duplicate(session)),
            BuiltinAction::ClearStatus => self.sidebar_act(window, SidebarAction::ClearStatus(vec![session])),
            BuiltinAction::ToggleFlag => self.sidebar_act(window, SidebarAction::ToggleFlags(vec![session])),
            BuiltinAction::ToggleSplit | BuiltinAction::ToggleHorizontalSplit => {
                self.toggle_split_action(window, session, action == BuiltinAction::ToggleHorizontalSplit);
            }
            BuiltinAction::ToggleScratch => {
                self.run_request(Command::Session(SessionCmd::Scratch), target, on(Args::default()));
            }
            BuiltinAction::ToggleTerminalZoom => {
                self.run_request(Command::Surface(SurfaceCmd::Zoom), None, on(Args::default()));
            }
            BuiltinAction::ToggleSearch => self.toggle_search(window, session),
            BuiltinAction::FocusLeftPane | BuiltinAction::FocusRightPane => {
                let pane = if action == BuiltinAction::FocusLeftPane {
                    "left"
                } else {
                    "right"
                };
                let args = Args {
                    pane: Some(pane.to_owned()),
                    ..Args::default()
                };
                self.run_request(Command::Session(SessionCmd::Focus), target, on(args));
            }
            _ => {
                let to = session_step(action);
                self.run_request(Command::Session(SessionCmd::Go), None, on(args_to(to)));
            }
        }
        ActionEffect::Done
    }

    /// Toggle Split (upstream's `toggleSplit(axis:)`): inert under a full overlay; a shown scratch is dismissed
    /// instead, so the panes come into view first.
    fn toggle_split_action(&mut self, window: Uuid, session: Uuid, horizontal: bool) {
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        let Some(state) = store.session(session) else {
            return;
        };
        if state.full_overlay_active() {
            return;
        }
        if state.scratch_active {
            store.toggle_scratch(session, true);
            return;
        }
        let args = Args {
            axis: Some((if horizontal { "horizontal" } else { "vertical" }).to_owned()),
            ..Args::default()
        };
        self.run_request(
            Command::Session(SessionCmd::Split),
            Some(uuid_string(session)),
            window_args(window, args),
        );
        self.request_pane_focus(session);
    }

    /// Find…: libghostty's search opens on the pane on screen, or the open one ends (the find bar's own toggle).
    fn toggle_search(&mut self, window: Uuid, session: Uuid) {
        let Some(state) = self
            .library_mut()
            .store_mut(window)
            .and_then(|store| store.session_mut(session))
        else {
            return;
        };
        let (pane, action) = match state.search.owner.filter(|_| state.search.active) {
            Some(owner) => (owner, "end_search"),
            None => (state.on_screen_pane(), "start_search"),
        };
        if let Some(surface) = state.pane_slot_mut(pane).get_mut() {
            surface.binding_action(action);
        }
    }

    fn toggle_workspace_collapse(&mut self, window: Uuid, workspace: Uuid) {
        let collapsed = self
            .library()
            .store(window)
            .is_some_and(AppStore::is_current_workspace_collapsed);
        let cmd = if collapsed {
            WorkspaceCmd::Expand
        } else {
            WorkspaceCmd::Collapse
        };
        self.run_request(
            Command::Workspace(cmd),
            Some(uuid_string(workspace)),
            window_args(window, Args::default()),
        );
    }

    /// A palette launcher's chord or menu item: the palette toggles (the theme picker only opens).
    fn launch_palette(&mut self, window: Uuid, mode: PaletteMode) -> Option<FrontendAction> {
        self.toggle_palette(window, mode);
        None
    }

    fn run_font(&mut self, window: Uuid, cmd: FontCmd) -> Option<FrontendAction> {
        self.run_request(Command::Font(cmd), None, window_args(window, Args::default()));
        None
    }

    /// Close Session's ladder (upstream's `closeActiveSession`, then `performClose` when it handled nothing): a pending
    /// dialog, the quick terminal, a zoom, the dashboard, the session's overlay, scratch and focused pane overlay each
    /// go first; then the session, confirmed when the setting asks; with no session, the window.
    fn close_active_session_or_window(&mut self, origin: ActionOrigin) -> ActionEffect {
        let window = origin.window;
        if self.dismiss_pending_modal(window) {
            return ActionEffect::Done;
        }
        if origin.quick {
            let mode = if self.quick_zoomed() { "hide" } else { "" };
            if mode.is_empty() {
                self.dismiss_quick_terminal();
            } else {
                let args = Args {
                    mode: Some(mode.to_owned()),
                    ..Args::default()
                };
                self.run_request(Command::Surface(SurfaceCmd::Zoom), Some("quick".to_owned()), args);
            }
            return ActionEffect::Done;
        }
        if self.zoom_target(window).is_some() {
            self.set_zoom_target(window, None);
            return ActionEffect::Done;
        }
        if self.dashboard(window).is_some() {
            self.toggle_dashboard(window);
            if let Some(session) = self.active_session_of(window) {
                self.request_pane_focus(session);
            }
            return ActionEffect::Done;
        }
        let Some(session) = self.active_session_of(window) else {
            return frontend(FrontendAction::CloseWindow(window));
        };
        if self.close_session_cover(window, session) {
            return ActionEffect::Done;
        }
        if self.settings().confirm_close_session == Some(true) {
            return frontend(FrontendAction::ConfirmCloseSession { window, session });
        }
        self.sidebar_act(window, SidebarAction::CloseSessions(vec![session]));
        ActionEffect::Done
    }

    /// The window's picker cancelled or its GUI ask escaped (upstream's `dismissPendingModal`), else the active
    /// session's terminal ask escaped. Whether one was.
    fn dismiss_pending_modal(&mut self, window: Uuid) -> bool {
        if let Some(id) = self.modals.window_ask(window).map(|ask| ask.id.clone()) {
            self.answer_window_ask(window, &id, ControlAskResult::outcome(ControlAskOutcome::Escaped));
            return true;
        }
        let pick = self
            .modals
            .controller(window)
            .and_then(|slot| slot.pending())
            .map(|pick| pick.id.clone());
        if let Some(id) = pick {
            self.resolve_pick(window, &id, ControlPickResult::outcome(ControlPickOutcome::Cancelled));
            return true;
        }
        let Some(session) = self.active_session_of(window) else {
            return false;
        };
        let ask = self
            .library()
            .store(window)
            .and_then(|store| store.session(session))
            .and_then(|state| state.ask())
            .map(|pending| pending.ask.id.clone());
        ask.is_some_and(|id| {
            self.answer_session_ask(session, &id, ControlAskResult::outcome(ControlAskOutcome::Escaped));
            true
        })
    }

    /// The session's own covers, topmost first: its overlay, its scratch, the focused pane's overlay. Whether one
    /// closed.
    fn close_session_cover(&mut self, window: Uuid, session: Uuid) -> bool {
        let Some(store) = self.library_mut().store_mut(window) else {
            return false;
        };
        let Some(state) = store.session(session) else {
            return false;
        };
        if state.overlay.active {
            return store.close_overlay(session);
        }
        if state.scratch_active {
            store.toggle_scratch(session, true);
            return true;
        }
        match state.focused_overlay_pane() {
            Some(pane) => store.close_pane_overlay(session, pane),
            None => false,
        }
    }

    fn active_session_of(&self, window: Uuid) -> Option<Uuid> {
        self.library().store(window).and_then(AppStore::selected_session_id)
    }

    fn sidebar_act(&mut self, window: Uuid, action: SidebarAction) {
        self.sidebar_action(window, action);
    }

    /// Runs a control command the way `ngtermctl` would, logging a refusal (a chord has nobody to answer).
    pub(crate) fn run_request(&mut self, cmd: Command, target: Option<String>, args: Args) {
        self.try_request(cmd, target, args);
    }

    /// [`Self::run_request`], answering whether the command succeeded.
    fn try_request(&mut self, cmd: Command, target: Option<String>, args: Args) -> bool {
        let request = Request {
            cmd,
            target,
            args: Some(args),
        };
        let response = self.dispatch(&request);
        if !response.ok {
            tracing::debug!(cmd = %cmd, error = ?response.error, "keymap action refused");
        }
        response.ok
    }
}

/// The store-side facts of a palette context.
fn store_facts(store: &AppStore, layout: FlaggedViewLayout) -> PaletteFacts {
    let mut facts = PaletteFacts::empty();
    let rows = store.renders_workspace_rows(layout);
    let active = store.active_session();
    facts.set(PaletteFacts::CAN_REMOVE_WORKSPACE, store.can_remove_workspace());
    facts.set(PaletteFacts::HAS_FLAGGED_SESSIONS, !store.flagged_sessions().is_empty());
    facts.set(
        PaletteFacts::SIDEBAR_SHOWS_WORKSPACE_TREE,
        store.sidebar.mode == SidebarMode::Tree,
    );
    facts.set(PaletteFacts::SIDEBAR_SHOWS_WORKSPACE_ROWS, rows);
    facts.set(
        PaletteFacts::SIDEBAR_SHOWS_FLAGGED_ONLY,
        store.sidebar.mode == SidebarMode::Flagged,
    );
    facts.set(
        PaletteFacts::ACTIVE_SESSION_FLAGGED,
        active.is_some_and(|session| session.flagged),
    );
    facts.set(PaletteFacts::HAS_MARKED_WORKSPACES, !store.focus().ids().is_empty());
    facts.set(
        PaletteFacts::ACTIVE_WORKSPACE_MARKED,
        store.is_current_workspace_focus_member(),
    );
    facts.set(
        PaletteFacts::ACTIVE_WORKSPACE_COLLAPSED,
        store.is_current_workspace_collapsed(),
    );
    facts.set(PaletteFacts::CAN_STEP_WORKSPACES, store.can_step_workspaces());
    facts.set(
        PaletteFacts::ACTIVE_SESSION_HAS_SPLIT,
        active.is_some_and(|session| session.split.exists),
    );
    facts.set(PaletteFacts::HAS_PENDING_CLOSE, store.pending_close_summary().is_some());
    facts.set(PaletteFacts::HAS_ACTIVE_SESSION, active.is_some());
    facts.set(
        PaletteFacts::HAS_CURRENT_WORKSPACE,
        store.current_workspace_id().is_some(),
    );
    facts
}

fn frontend(action: FrontendAction) -> ActionEffect {
    ActionEffect::Frontend(action)
}

fn args_to(to: &str) -> Args {
    Args {
        to: Some(to.to_owned()),
        ..Args::default()
    }
}

fn window_args(window: Uuid, args: Args) -> Args {
    Args {
        window: Some(uuid_string(window)),
        ..args
    }
}

/// The `session.go --to` step a navigation action takes.
fn session_step(action: BuiltinAction) -> &'static str {
    match action {
        BuiltinAction::PreviousSession => "prev",
        BuiltinAction::PreviousAttentionSession => "prev-attention",
        BuiltinAction::NextAttentionSession => "next-attention",
        BuiltinAction::FirstSession => "first",
        BuiltinAction::LastSession => "last",
        _ => "next",
    }
}
