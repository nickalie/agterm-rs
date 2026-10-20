//! Running a palette row (upstream's `PaletteItem.runIfEnabled` over the closures `AppActions+Palette.swift` builds).
//!
//! A row's enablement is asked of live state when it renders and again when it runs, so a row that went inert while
//! the palette sat open neither runs nor closes the palette. A row that runs closes the palette first, so a launcher
//! row (Show Attention, Select Theme…) can open the next palette; a theme row commits its preview before the close.

use agterm_proto::command::WindowCmd;
use agterm_proto::{Args, Command};
use uuid::Uuid;

use super::{PaletteAction, PaletteMode, feeds::window_holds};
use crate::Core;
use crate::app_actions::{ActionEffect, ActionOrigin, FrontendAction};
use crate::model::AppStore;
use crate::palette_catalog::PaletteCommand;
use crate::pane_env::uuid_string;
use crate::windows::WindowRequest;

impl Core {
    /// Whether `action`, a row of the palette window `window` draws, can run now. The static rows read the menu's
    /// predicate; an attention row asks its own window's modal gate (upstream's `canSelectAttention`); the rest are
    /// gated when they run.
    #[must_use]
    pub fn palette_entry_enabled(&self, window: Uuid, action: &PaletteAction) -> bool {
        match action {
            PaletteAction::Command(command) => command.is_enabled(&self.palette_context(window)),
            PaletteAction::Attention { window, session } => self.can_select_attention(*window, *session),
            _ => true,
        }
    }

    /// Runs the palette row `action` from window `window`: `None` when it is inert (the palette stays open), else
    /// what the frontend finishes. Commit afterwards.
    pub fn run_palette_entry(&mut self, window: Uuid, action: &PaletteAction) -> Option<ActionEffect> {
        if self.palette.mode.is_none() || !self.palette_entry_enabled(window, action) {
            return None;
        }
        if let PaletteAction::Theme(name) = action {
            self.preview_palette_theme(name.as_deref());
            self.commit_theme_preview();
            self.close_palette();
            return Some(ActionEffect::Done);
        }
        self.close_palette();
        Some(match action {
            PaletteAction::Command(command) => self.run_palette_command(window, *command),
            PaletteAction::Builtin(builtin) => self.perform_builtin(ActionOrigin { window, quick: false }, *builtin),
            PaletteAction::Settings => ActionEffect::Frontend(FrontendAction::OpenSettings(window)),
            other => {
                self.run_palette_item(window, other);
                ActionEffect::Done
            }
        })
    }

    /// A static row as the palette dispatches it (upstream's `dispatch(_:)`): Close Session never closes the window,
    /// and the two launchers open their palette rather than toggle it.
    fn run_palette_command(&mut self, window: Uuid, command: PaletteCommand) -> ActionEffect {
        match command {
            PaletteCommand::CloseSession
                if self
                    .library()
                    .store(window)
                    .and_then(AppStore::selected_session_id)
                    .is_none() =>
            {
                ActionEffect::Done
            }
            PaletteCommand::ShowAttention => {
                self.open_palette(window, PaletteMode::Attention);
                ActionEffect::Done
            }
            PaletteCommand::SelectTheme => {
                self.open_palette(window, PaletteMode::Themes);
                ActionEffect::Done
            }
            _ => self.perform_menu_command(window, command),
        }
    }

    /// The dynamic rows, each behind the modal gate of the window it acts in.
    fn run_palette_item(&mut self, window: Uuid, action: &PaletteAction) {
        if let PaletteAction::Attention { window, session } = action {
            self.select_attention(*window, *session);
            return;
        }
        if !self.ui_actions_enabled(window) {
            return;
        }
        match action {
            PaletteAction::OpenWindow(id) => {
                self.run_request(
                    Command::Window(WindowCmd::Select),
                    Some(uuid_string(*id)),
                    Args::default(),
                );
            }
            PaletteAction::MoveSession { session, workspace } => {
                if let Some(store) = self.library_mut().store_mut(window) {
                    store.move_session(*session, *workspace, None);
                }
            }
            PaletteAction::Custom(id) => self.run_palette_custom_command(window, *id),
            PaletteAction::SelectSession(session) if window_holds(self.library(), window, *session) => {
                self.pick_session(window, *session);
            }
            _ => {}
        }
    }

    /// A picked custom command runs for the active session's focused pane, else sessionless (upstream's
    /// `runCustomCommand`). A command the keymap no longer holds does nothing.
    fn run_palette_custom_command(&mut self, window: Uuid, id: Uuid) {
        let Some(command) = self
            .keymap()
            .commands()
            .iter()
            .find(|command| command.id == id)
            .cloned()
        else {
            return;
        };
        let session = self.library().store(window).and_then(AppStore::selected_session_id);
        match session {
            Some(session) => {
                if let Some(reason) = self.run_custom_command(&command, window, session) {
                    tracing::info!(name = %command.name, reason, "palette custom command did not start");
                }
            }
            None => self.run_custom_command_sessionless(&command, Some(window)),
        }
    }

    /// Whether the attention row for `session` in `window` can run: that window takes UI actions and still holds the
    /// session (upstream's `canSelectAttention`).
    #[must_use]
    pub fn can_select_attention(&self, window: Uuid, session: Uuid) -> bool {
        self.ui_actions_enabled(window) && window_holds(self.library(), window, session)
    }

    /// Selects `session` in `window`, raising that window first when it is not the frontmost one, and lands the
    /// keyboard on the pane that asked for attention (upstream's `selectAttention`): the attention palette's rows and
    /// the title bar's attention popover. Whether it ran. Commit afterwards.
    pub fn select_attention(&mut self, window: Uuid, session: Uuid) -> bool {
        if !self.can_select_attention(window, session) {
            return false;
        }
        if self.library().active_window_id() != Some(window) {
            self.request_window(WindowRequest::Raise(window));
            self.take_frontmost(window);
        }
        self.pick_session(window, session);
        true
    }

    /// A user's pick of `session` in `window` (the session palette, an attention row): activity noted, so auto-follow
    /// grants its full idle grace, the session selected and its blocked pane revealed, all in that window (a legacy
    /// snapshot may hold one session id in two open windows).
    fn pick_session(&mut self, window: Uuid, session: Uuid) {
        let Some(store) = self.library_mut().store_mut(window) else {
            return;
        };
        let indicator = store.select_session(Some(session));
        self.note_window_activity(window);
        self.reveal_blocked_pane_in(window, session, indicator.as_ref());
    }

    /// The Ctrl-Tab switcher's candidates in window `window`: up to `limit` most recently selected sessions of the
    /// navigable set, most recent first (upstream's `SessionSwitcher.begin`).
    #[must_use]
    pub fn switcher_candidates(&self, window: Uuid, limit: usize) -> Vec<Uuid> {
        let Some(store) = self.library().store(window) else {
            return Vec::new();
        };
        let navigable = store.navigable_session_ids();
        store.session_recency().top(limit, |id| navigable.contains(id))
    }

    /// The Ctrl-Tab switcher committed `session` in window `window` (upstream's `SessionSwitcher.commit`): activity
    /// noted and the session selected, while the window takes UI actions. Commit afterwards.
    pub fn switch_to_session(&mut self, window: Uuid, session: Uuid) {
        if !self.ui_actions_enabled(window) || !window_holds(self.library(), window, session) {
            return;
        }
        self.note_user_activity(session);
        if let Some(store) = self.library_mut().store_mut(window) {
            store.select_session(Some(session));
        }
    }
}
