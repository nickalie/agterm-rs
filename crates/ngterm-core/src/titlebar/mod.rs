//! The window's title bar and its popovers, host-free (upstream's `TitlebarComposition.swift`,
//! `InterfaceElement.titlebarGroupDividers`, `WindowContentView+Titlebar.swift`, `+RecentSessions.swift`,
//! `+CustomCommands.swift` and `WindowLibrary+Attention.swift`): what the bar's two lines read, which buttons it shows
//! in which state, the rows of the recent-sessions, attention and custom-commands popovers, and what choosing one does.
//! The frontend owns only layout, glyphs and truncation.

use uuid::Uuid;

use crate::Core;
use crate::app_actions::{ActionEffect, ActionOrigin};
use crate::keymap::{BuiltinAction, CustomCommand};
use crate::model::{AgentIndicator, AgentStatus, AppStore, Session, SplitAxis, WindowLibrary, attention_order};
use crate::pane_env::uuid_string;
use crate::persist::settings::{InterfaceElement, ToolbarMode};

#[cfg(test)]
mod tests;

/// Joins the workspace, session and window names.
const IDENTITY_SEPARATOR: &str = " — ";
/// Sits between the identity and the context on a compact bar's single row.
const CONTEXT_SEPARATOR: &str = " · ";
/// Characters of the workspace name kept before an ellipsis, so an uncapped prefix cannot push the session name off.
pub const WORKSPACE_NAME_LIMIT: usize = 24;
/// The title with no session selected.
pub const UNTITLED: &str = "Ngterm";
/// How many recent sessions the popover lists (upstream's `SessionSwitcher.maxCandidates`).
pub const RECENT_SESSIONS_LIMIT: usize = 10;
/// How many most-run commands lead the custom-commands popover, and the command count above which they do.
pub const MOST_USED_COMMAND_LIMIT: usize = 5;

/// What the title bar has to work with, each part already resolved: one hidden by its interface toggle is `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitlebarParts {
    /// The workspace holding the active session; capped and dropped when blank by [`compose`].
    pub workspace_name: Option<String>,
    /// The active session's display name ("Ngterm" with none).
    pub session_name: Option<String>,
    /// The window's user-set name; `None` for an automatic one.
    pub window_name: Option<String>,
    /// The session's effective context.
    pub context: Option<String>,
    /// The focused pane's terminal title or its cwd.
    pub detail: String,
    pub remote_host: Option<String>,
}

/// The title bar's two lines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitlebarComposition {
    /// Line one's workspace, session and window identity.
    pub title: String,
    /// The SSH target after the identity, styled and truncated apart.
    pub host: Option<String>,
    /// The compact bar's context, its separator included when another part precedes it.
    pub tail: String,
    /// Line two; empty outside the normal bar.
    pub subtitle: String,
}

/// Lays `parts` out for `mode`: compact puts the context last on one row so it truncates first, normal gives it line
/// two in place of the detail, hidden composes nothing.
#[must_use]
pub fn compose(parts: &TitlebarParts, mode: ToolbarMode) -> TitlebarComposition {
    let identity = [
        parts.workspace_name.as_deref().and_then(capped_workspace_name),
        parts.session_name.clone(),
        parts.window_name.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(IDENTITY_SEPARATOR);
    match mode {
        ToolbarMode::Hidden => TitlebarComposition::default(),
        ToolbarMode::Compact => {
            let context = parts.context.as_deref().unwrap_or_default();
            let separator = if identity.is_empty() && parts.remote_host.is_none() {
                ""
            } else {
                CONTEXT_SEPARATOR
            };
            TitlebarComposition {
                title: identity,
                host: parts.remote_host.clone(),
                tail: if context.is_empty() {
                    String::new()
                } else {
                    format!("{separator}{context}")
                },
                subtitle: String::new(),
            }
        }
        ToolbarMode::Normal => TitlebarComposition {
            title: identity,
            host: parts.remote_host.clone(),
            tail: String::new(),
            subtitle: parts.context.clone().unwrap_or_else(|| parts.detail.clone()),
        },
    }
}

/// `None` for a blank name; past [`WORKSPACE_NAME_LIMIT`] characters it is cut with an ellipsis.
fn capped_workspace_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= WORKSPACE_NAME_LIMIT {
        return Some(trimmed.to_owned());
    }
    Some(trimmed.chars().take(WORKSPACE_NAME_LIMIT).chain(['…']).collect())
}

/// Where the trailing cluster draws a separator: only between two groups that each still show two or more buttons,
/// or across an empty middle group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GroupDividers {
    /// Between the jump group (recent, attention) and the pane group (scratch, split).
    pub after_jump: bool,
    /// Between the pane group and the window group (dashboard, quick terminal, custom commands).
    pub after_panes: bool,
}

/// The dividers for groups of `jump`, `panes` and `window` shown buttons.
#[must_use]
pub fn group_dividers(jump: usize, panes: usize, window: usize) -> GroupDividers {
    GroupDividers {
        after_jump: jump >= 2 && panes >= 2,
        after_panes: (panes >= 2 && window >= 2) || (jump >= 2 && window >= 2 && panes == 0),
    }
}

/// The split button's glyph: filled halves are the shown panes, the outline a hidden one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitGlyph {
    /// No split: an empty two-pane outline.
    None,
    /// Both panes side by side.
    Both,
    /// Both panes stacked.
    BothHorizontal,
    /// Hidden split, the left (primary) pane showing.
    Left,
    Right,
    Top,
    Bottom,
}

impl SplitGlyph {
    /// The glyph for `session` (none without one).
    #[must_use]
    pub fn of(session: Option<&Session>) -> Self {
        let Some(session) = session.filter(|session| session.split.exists) else {
            return Self::None;
        };
        let stacked = session.split.axis == SplitAxis::TopBottom;
        match (session.split.shown, session.split.focused, stacked) {
            (true, _, false) => Self::Both,
            (true, _, true) => Self::BothHorizontal,
            (false, true, false) => Self::Right,
            (false, true, true) => Self::Bottom,
            (false, false, false) => Self::Left,
            (false, false, true) => Self::Top,
        }
    }

    /// The upstream accessibility value, one per state.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Both => "both",
            Self::BothHorizontal => "both-horizontal",
            Self::Left => "left",
            Self::Right => "right",
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

/// The attention bell's state across every open window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionBell {
    /// Nothing needs attention: dimmed and disabled.
    None,
    Attention,
    /// A session is blocked: filled in the blocked color.
    Blocked,
}

/// A title-bar button: whether it is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Button {
    pub enabled: bool,
}

/// The split button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitButton {
    pub glyph: SplitGlyph,
    /// The hover text before its chord.
    pub hint: &'static str,
    /// The action whose chord the hover text shows: the horizontal toggle on a stacked split.
    pub action: BuiltinAction,
    pub enabled: bool,
}

/// The scratch button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchButton {
    /// The scratch terminal shows (filled glyph).
    pub active: bool,
    pub enabled: bool,
}

/// The bell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionButton {
    pub bell: AttentionBell,
    pub enabled: bool,
}

/// The whole title bar of one window: each button `None` while its interface toggle hides it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Titlebar {
    pub mode: ToolbarMode,
    pub label: TitlebarComposition,
    pub sidebar_toggle: bool,
    pub recent: Option<Button>,
    pub attention: Option<AttentionButton>,
    pub scratch: Option<ScratchButton>,
    pub split: Option<SplitButton>,
    pub dashboard: Option<Button>,
    pub quick_terminal: Option<Button>,
    pub custom_commands: Option<Button>,
    pub dividers: GroupDividers,
}

/// One row of the recent-sessions or attention popover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The window holding the session.
    pub window: Uuid,
    pub session: Uuid,
    pub title: String,
    /// "`workspace` · detail", led by the window name for an attention row while several windows are open.
    pub subtitle: String,
    pub remote: bool,
    /// The status glyph an attention row leads with.
    pub indicator: Option<AgentIndicator>,
    /// A row whose window sits under a cover is inert.
    pub enabled: bool,
}

/// The custom-commands popover: the most-run commands (once the keymap holds more than [`MOST_USED_COMMAND_LIMIT`])
/// above the rest, both in keymap order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomCommandRows {
    pub leading: Vec<CustomCommand>,
    pub rest: Vec<CustomCommand>,
}

/// What a title-bar button or popover row asks of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlebarAction {
    ToggleSidebar,
    ToggleScratch,
    /// Toggle the split on the session's own axis (upstream's `toggleCurrentSplit`).
    ToggleSplit,
    Dashboard,
    QuickTerminal,
    /// A recent-sessions row.
    SelectRecent(Uuid),
    /// An attention row: the window holding the session, and the session.
    SelectAttention {
        window: Uuid,
        session: Uuid,
    },
    /// A custom-commands row, by the command's id.
    RunCustomCommand(Uuid),
    /// A popover opened: auto-follow holds still until it closes.
    PopoverOpened,
    PopoverClosed,
}

/// One row of the cross-window attention list: a non-idle session and the open window holding it.
#[derive(Debug, Clone, Copy)]
pub struct AttentionEntry<'a> {
    pub window: Uuid,
    pub session: &'a Session,
}

impl WindowLibrary {
    /// Every open window's non-idle sessions in one attention order, each with its window.
    #[must_use]
    pub fn attention_across_windows(&self) -> Vec<AttentionEntry<'_>> {
        let mut entries: Vec<AttentionEntry<'_>> = self
            .windows()
            .iter()
            .filter_map(|info| self.store(info.id).map(|store| (info.id, store)))
            .flat_map(|(window, store)| {
                store
                    .attention_sessions()
                    .into_iter()
                    .map(move |session| AttentionEntry { window, session })
            })
            .collect();
        entries.sort_by(|a, b| attention_order(a.session, b.session));
        entries
    }

    /// The subtitle every attention row shares: "`workspace` · detail", led by the window name once more than one
    /// window is open.
    #[must_use]
    pub fn attention_subtitle(&self, entry: &AttentionEntry<'_>) -> String {
        let detail = session_subtitle(self.store(entry.window), entry.session);
        if self.open_ids().len() > 1 {
            format!("{} · {detail}", self.window_name(Some(entry.window)))
        } else {
            detail
        }
    }
}

/// "`workspace` · switcher detail" for `session` of `store`.
fn session_subtitle(store: Option<&AppStore>, session: &Session) -> String {
    let workspace = store
        .and_then(|store| store.workspace_for_session(session.id()))
        .map(|workspace| workspace.name.clone())
        .unwrap_or_default();
    format!("{workspace} · {}", session.switcher_detail())
}

impl Core {
    /// Window `window`'s title bar under the current settings.
    #[must_use]
    pub fn titlebar(&self, window: Uuid) -> Titlebar {
        let settings = self.settings();
        let shows = |element| !settings.is_interface_element_hidden(element);
        let mode = settings.effective_toolbar_mode();
        let store = self.library().store(window);
        let active = store.and_then(AppStore::active_session);
        let has_session = active.is_some();
        let modal = self.modal_pending(window);
        let recent = shows(InterfaceElement::RecentSessions).then(|| Button {
            enabled: !modal
                && store.is_some_and(|store| !store.navigable_recent_sessions(RECENT_SESSIONS_LIMIT).is_empty()),
        });
        let attention = settings
            .attention_button_enabled
            .unwrap_or(false)
            .then(|| self.attention_button(modal));
        let scratch = shows(InterfaceElement::Scratch).then(|| ScratchButton {
            active: active.is_some_and(|session| session.scratch_active),
            enabled: has_session,
        });
        let split = shows(InterfaceElement::Split).then(|| split_button(active));
        let dashboard = shows(InterfaceElement::Dashboard).then_some(Button { enabled: has_session });
        let quick_terminal = shows(InterfaceElement::QuickTerminal).then_some(Button { enabled: !modal });
        let custom_commands = shows(InterfaceElement::CustomCommands).then(|| Button {
            enabled: has_session && !modal && !self.keymap().commands().is_empty(),
        });
        let count = |buttons: &[bool]| buttons.iter().filter(|shown| **shown).count();
        let dividers = group_dividers(
            count(&[recent.is_some(), attention.is_some()]),
            count(&[scratch.is_some(), split.is_some()]),
            count(&[dashboard.is_some(), quick_terminal.is_some(), custom_commands.is_some()]),
        );
        Titlebar {
            mode,
            label: compose(&self.titlebar_parts(window), mode),
            sidebar_toggle: shows(InterfaceElement::SidebarToggle),
            recent,
            attention,
            scratch,
            split,
            dashboard,
            quick_terminal,
            custom_commands,
            dividers,
        }
    }

    fn attention_button(&self, modal: bool) -> AttentionButton {
        let entries = self.library().attention_across_windows();
        let bell = if entries.is_empty() {
            AttentionBell::None
        } else if entries
            .iter()
            .any(|entry| entry.session.agent_indicator.status == AgentStatus::Blocked)
        {
            AttentionBell::Blocked
        } else {
            AttentionBell::Attention
        };
        AttentionButton {
            bell,
            enabled: bell != AttentionBell::None && !modal,
        }
    }

    /// The title bar's parts for window `window`, each gated by its interface toggle. The workspace is the active
    /// session's own, not the current one: selecting an empty workspace keeps the previous session selected.
    #[must_use]
    pub fn titlebar_parts(&self, window: Uuid) -> TitlebarParts {
        let settings = self.settings();
        let shows = |element| !settings.is_interface_element_hidden(element);
        let store = self.library().store(window);
        let active = store.and_then(AppStore::active_session);
        TitlebarParts {
            workspace_name: shows(InterfaceElement::WorkspaceName)
                .then(|| store.zip(active))
                .flatten()
                .and_then(|(store, session)| store.workspace_for_session(session.id()))
                .map(|workspace| workspace.name.clone()),
            session_name: shows(InterfaceElement::SessionName)
                .then(|| active.map_or_else(|| UNTITLED.to_owned(), Session::display_name)),
            window_name: shows(InterfaceElement::WindowName)
                .then(|| self.library().custom_window_name(window).map(str::to_owned))
                .flatten(),
            context: shows(InterfaceElement::SessionContext)
                .then(|| active.and_then(Session::effective_context).map(str::to_owned))
                .flatten(),
            detail: active.map(Session::subtitle_detail).unwrap_or_default(),
            remote_host: shows(InterfaceElement::RemoteHost)
                .then(|| active.and_then(Session::remote_host).map(str::to_owned))
                .flatten(),
        }
    }

    /// The recent-sessions popover of window `window`: its most recently used other sessions in the visible scope.
    #[must_use]
    pub fn recent_session_rows(&self, window: Uuid) -> Vec<SessionRow> {
        let Some(store) = self.library().store(window) else {
            return Vec::new();
        };
        store
            .navigable_recent_sessions(RECENT_SESSIONS_LIMIT)
            .into_iter()
            .filter_map(|id| store.session(id))
            .map(|session| SessionRow {
                window,
                session: session.id(),
                title: session.display_name(),
                subtitle: session_subtitle(Some(store), session),
                remote: session.remote_host().is_some(),
                indicator: None,
                enabled: true,
            })
            .collect()
    }

    /// The attention popover: every open window's sessions needing attention, a row inert while its window is covered.
    #[must_use]
    pub fn attention_rows(&self) -> Vec<SessionRow> {
        let library = self.library();
        library
            .attention_across_windows()
            .iter()
            .map(|entry| SessionRow {
                window: entry.window,
                session: entry.session.id(),
                title: entry.session.display_name(),
                subtitle: library.attention_subtitle(entry),
                remote: entry.session.remote_host().is_some(),
                indicator: Some(entry.session.agent_indicator.clone()),
                enabled: self.can_select_attention(entry.window, entry.session.id()),
            })
            .collect()
    }

    /// The custom-commands popover's rows.
    #[must_use]
    pub fn custom_command_rows(&self) -> CustomCommandRows {
        let commands = self.keymap().commands();
        if commands.len() <= MOST_USED_COMMAND_LIMIT {
            return CustomCommandRows {
                leading: Vec::new(),
                rest: commands.to_vec(),
            };
        }
        let usage = self.custom_command_usage();
        let leading: Vec<Uuid> = usage
            .most_used(commands, MOST_USED_COMMAND_LIMIT)
            .into_iter()
            .map(|command| command.id)
            .collect();
        let (leading, rest) = commands
            .iter()
            .cloned()
            .partition(|command| leading.contains(&command.id));
        CustomCommandRows { leading, rest }
    }

    /// The custom commands' run counts; empty when the model is kept in memory only.
    fn custom_command_usage(&self) -> crate::keymap::usage::CustomCommandUsage {
        self.persistence
            .as_ref()
            .map(|persistence| crate::keymap::usage::UsageStore::new(persistence.directory()).load())
            .unwrap_or_default()
    }

    /// Runs a title-bar button or popover row for window `window`. Commit afterwards.
    pub fn titlebar_action(&mut self, window: Uuid, action: TitlebarAction) -> ActionEffect {
        let builtin = |core: &mut Self, action| core.perform_builtin(ActionOrigin { window, quick: false }, action);
        match action {
            TitlebarAction::ToggleSidebar => builtin(self, BuiltinAction::ToggleSidebar),
            TitlebarAction::ToggleScratch => builtin(self, BuiltinAction::ToggleScratch),
            TitlebarAction::ToggleSplit => {
                let active = self.library().store(window).and_then(AppStore::active_session);
                builtin(self, split_button(active).action)
            }
            TitlebarAction::Dashboard => builtin(self, BuiltinAction::Dashboard),
            TitlebarAction::QuickTerminal => builtin(self, BuiltinAction::QuickTerminal),
            TitlebarAction::SelectRecent(session) => {
                self.select_recent(window, session);
                ActionEffect::Done
            }
            TitlebarAction::SelectAttention { window, session } => {
                self.select_attention(window, session);
                ActionEffect::Done
            }
            TitlebarAction::RunCustomCommand(id) => {
                self.run_popover_command(window, id);
                ActionEffect::Done
            }
            TitlebarAction::PopoverOpened => {
                self.suppress_auto_follow(window);
                ActionEffect::Done
            }
            TitlebarAction::PopoverClosed => {
                self.resume_auto_follow(window);
                ActionEffect::Done
            }
        }
    }

    /// A recent-sessions row: user activity, the session selected and given the keyboard.
    fn select_recent(&mut self, window: Uuid, session: Uuid) -> bool {
        if self.modal_pending(window) {
            return false;
        }
        let Some(store) = self.library_mut().store_mut(window) else {
            return false;
        };
        if store.session(session).is_none() {
            return false;
        }
        store.select_session(Some(session));
        self.note_window_activity(window);
        self.request_pane_focus(session);
        true
    }

    /// A custom-commands row: run for the active session's focused pane, as the palette does, then the terminal takes
    /// the keyboard back. Nothing runs under a cover (upstream's `runCustomCommand` gates on `uiActionsEnabled`).
    fn run_popover_command(&mut self, window: Uuid, id: Uuid) -> bool {
        if !self.ui_actions_enabled(window) {
            return false;
        }
        let Some(command) = self
            .keymap()
            .commands()
            .iter()
            .find(|command| command.id == id)
            .cloned()
        else {
            return false;
        };
        let Some(session) = self
            .library()
            .store(window)
            .and_then(AppStore::active_session)
            .map(Session::id)
        else {
            return false;
        };
        self.note_window_activity(window);
        if let Some(reason) = self.run_custom_command(&command, window, session) {
            tracing::info!(name = %command.name, %reason, session = %uuid_string(session), "custom command did not start");
        }
        self.request_pane_focus(session);
        true
    }
}

/// The split button for `active`: its glyph, hover text and the chord its hover text shows.
fn split_button(active: Option<&Session>) -> SplitButton {
    let glyph = SplitGlyph::of(active);
    let stacked = active.is_some_and(|session| session.split.exists && session.split.axis == SplitAxis::TopBottom);
    let hint = match active.map(|session| (session.split.exists, session.split.shown)) {
        Some((true, true)) => "Hide split",
        Some((true, false)) => "Show split",
        _ => "Split right",
    };
    SplitButton {
        glyph,
        hint,
        action: if stacked {
            BuiltinAction::ToggleHorizontalSplit
        } else {
            BuiltinAction::ToggleSplit
        },
        enabled: active.is_some(),
    }
}
