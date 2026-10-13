//! The static palette rows and their enablement (upstream's `PaletteCatalog.swift`).
//!
//! [`PaletteCommand::is_enabled`] is the single owner of menu enablement: the menu item, the palette row and the key
//! monitor's built-in dispatch all read it, so a built-in's menu chord and its `keymap.conf` alternatives cannot
//! diverge.

use bitflags::bitflags;
use strum::{EnumIter, EnumString, IntoStaticStr};

use crate::keymap::BuiltinAction;
use crate::model::SplitAxis;

bitflags! {
    /// The yes/no UI facts behind [`PaletteCommand::is_visible`] and [`PaletteCommand::is_enabled`] (upstream's
    /// `PaletteContext` booleans).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct PaletteFacts: u32 {
        const CAN_REMOVE_WORKSPACE = 1;
        const HAS_FLAGGED_SESSIONS = 1 << 1;
        /// The ordinary workspace tree, which workspace stepping keys on. Implies
        /// [`Self::SIDEBAR_SHOWS_WORKSPACE_ROWS`].
        const SIDEBAR_SHOWS_WORKSPACE_TREE = 1 << 2;
        /// The sidebar has workspace rows to fold: the ordinary tree, or flagged mode under the tree layout.
        const SIDEBAR_SHOWS_WORKSPACE_ROWS = 1 << 3;
        const SIDEBAR_SHOWS_FLAGGED_ONLY = 1 << 4;
        const ACTIVE_SESSION_FLAGGED = 1 << 5;
        /// Any workspace is marked in the focus set: membership, not whether the filter is applied.
        const HAS_MARKED_WORKSPACES = 1 << 6;
        /// The current workspace is already marked, so "Add Workspace to Focus" would be a silent no-op.
        const ACTIVE_WORKSPACE_MARKED = 1 << 7;
        /// The current workspace's row shows folded (what the row shows, not the persisted expansion).
        const ACTIVE_WORKSPACE_COLLAPSED = 1 << 8;
        /// More than one workspace is visible, so a workspace step has somewhere to go.
        const CAN_STEP_WORKSPACES = 1 << 9;
        /// More than one window is open; app-global, unlike every other fact here.
        const CAN_STEP_WINDOWS = 1 << 10;
        /// The active session has a split, hidden pane included (`hasSplit`, not `isSplit`).
        const ACTIVE_SESSION_HAS_SPLIT = 1 << 11;
        const HAS_PENDING_CLOSE = 1 << 12;
        const HAS_RECENT_CLOSED = 1 << 13;
        /// The frontmost window has an active session.
        const HAS_ACTIVE_SESSION = 1 << 14;
        /// The frontmost window has a current workspace.
        const HAS_CURRENT_WORKSPACE = 1 << 15;
        /// `remotes.conf` lists a machine to attach from.
        const HAS_REMOTES = 1 << 16;
        const TERMINAL_ZOOM_ACTIVE = 1 << 17;
        const DASHBOARD_OPEN = 1 << 18;
        /// A control picker or a GUI ask holds the window's modal slot.
        const PICKER_ACTIVE = 1 << 19;
    }
}

/// The UI facts behind [`PaletteCommand::is_visible`] (which rows are relevant at all) and
/// [`PaletteCommand::is_enabled`] (which of them can run right now). The default has every fact false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PaletteContext {
    pub facts: PaletteFacts,
    /// The active session's split axis, which names the pane-focus rows.
    pub active_split_axis: Option<SplitAxis>,
}

impl PaletteContext {
    /// A context with `facts` and no split axis.
    #[must_use]
    pub const fn new(facts: PaletteFacts) -> Self {
        Self {
            facts,
            active_split_axis: None,
        }
    }

    /// This context with the active split axis set.
    #[must_use]
    pub const fn with_split_axis(mut self, axis: SplitAxis) -> Self {
        self.active_split_axis = Some(axis);
        self
    }

    /// Whether every fact in `facts` holds.
    #[must_use]
    pub const fn has(&self, facts: PaletteFacts) -> bool {
        self.facts.contains(facts)
    }

    /// Whether the sidebar has workspace rows to fold. The ordinary tree always does, so a caller that only knows
    /// the tree keeps its fold commands.
    #[must_use]
    pub const fn sidebar_shows_workspace_rows(&self) -> bool {
        self.facts
            .intersects(PaletteFacts::SIDEBAR_SHOWS_WORKSPACE_ROWS.union(PaletteFacts::SIDEBAR_SHOWS_WORKSPACE_TREE))
    }

    /// Any cover over the deck. Behind it a keystroke or a menu pick must not mutate what it hides, so all but a
    /// handful of commands go dead; [`PaletteCommand::is_enabled`] owns which.
    #[must_use]
    pub const fn modal_active(&self) -> bool {
        self.facts.intersects(
            PaletteFacts::TERMINAL_ZOOM_ACTIVE
                .union(PaletteFacts::DASHBOARD_OPEN)
                .union(PaletteFacts::PICKER_ACTIVE),
        )
    }
}

/// A static action-palette row, in the order the palette presents them before dynamic rows. The id
/// ([`Self::id`]) is upstream's camel-case raw value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumIter, EnumString, IntoStaticStr)]
#[strum(serialize_all = "camelCase")]
pub enum PaletteCommand {
    NewSession,
    NewWorkspace,
    OpenDirectory,
    RenameSession,
    DuplicateSession,
    RenameWorkspace,
    CloseSession,
    ReopenRecent,
    UndoClose,
    ClearStatus,
    PreviousSession,
    NextSession,
    PreviousAttentionSession,
    NextAttentionSession,
    PreviousWorkspace,
    NextWorkspace,
    PreviousWindow,
    NextWindow,
    FirstSession,
    LastSession,
    ShowAttention,
    ToggleSplit,
    ToggleHorizontalSplit,
    CloseSplit,
    SwapPanes,
    ToggleScratch,
    ToggleTerminalZoom,
    ToggleSidebar,
    ToggleFlag,
    FocusWorkspace,
    Find,
    QuickTerminal,
    Dashboard,
    ToggleFullscreen,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    SelectTheme,
    AttachRemote,
    EditKeymap,
    ReloadKeymap,
    EditHooks,
    ReloadHooks,
    EditGhosttyConfig,
    ReloadConfig,
    DeleteWorkspace,
    ToggleFlaggedView,
    ClearFlagged,
    ClearFocus,
    AddWorkspaceToFocus,
    ToggleWorkspaceFilter,
    ExpandWorkspaces,
    CollapseWorkspaces,
    ToggleWorkspaceCollapse,
    FocusLeftPane,
    FocusRightPane,
}

impl PaletteCommand {
    /// Every command, in palette order.
    pub fn all() -> impl Iterator<Item = Self> {
        <Self as strum::IntoEnumIterator>::iter()
    }

    /// The stable id (upstream's raw value, e.g. `newSession`).
    #[must_use]
    pub fn id(self) -> &'static str {
        self.into()
    }

    /// The command an id names, exactly.
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        id.parse().ok()
    }

    /// The palette command a built-in action runs, if any.
    #[must_use]
    pub fn for_builtin(action: BuiltinAction) -> Option<Self> {
        Self::all().find(|command| command.builtin_action() == Some(action))
    }

    /// Whether the command can run right now: relevance, then the modal cover, then the presence terms.
    #[must_use]
    pub fn is_enabled(self, context: &PaletteContext) -> bool {
        if !self.is_visible(context) || self.is_covered_by_modal(*context) {
            return false;
        }
        match self {
            Self::RenameSession
            | Self::DuplicateSession
            | Self::ClearStatus
            | Self::ToggleFlag
            | Self::ToggleSplit
            | Self::ToggleHorizontalSplit
            | Self::ToggleScratch
            | Self::Find
            | Self::PreviousSession
            | Self::NextSession
            | Self::PreviousAttentionSession
            | Self::NextAttentionSession
            | Self::FirstSession
            | Self::LastSession => context.has(PaletteFacts::HAS_ACTIVE_SESSION),
            Self::SwapPanes => context.has(PaletteFacts::HAS_ACTIVE_SESSION | PaletteFacts::ACTIVE_SESSION_HAS_SPLIT),
            Self::RenameWorkspace
            | Self::FocusWorkspace
            | Self::AddWorkspaceToFocus
            | Self::ToggleWorkspaceCollapse => context.has(PaletteFacts::HAS_CURRENT_WORKSPACE),
            // a step needs somewhere to go, so a lone visible workspace disables rather than no-ops
            Self::PreviousWorkspace | Self::NextWorkspace => {
                context.has(PaletteFacts::HAS_CURRENT_WORKSPACE | PaletteFacts::CAN_STEP_WORKSPACES)
            }
            Self::PreviousWindow | Self::NextWindow => context.has(PaletteFacts::CAN_STEP_WINDOWS),
            Self::AttachRemote => context.has(PaletteFacts::HAS_REMOTES | PaletteFacts::HAS_CURRENT_WORKSPACE),
            _ => true,
        }
    }

    /// Whether a cover blocks the command. The font sizes, the reloads, terminal zoom and Close Session carry no
    /// modal term; Dashboard is spared by its own grid, being that grid's escape hatch; Swap Panes only by a picker.
    fn is_covered_by_modal(self, context: PaletteContext) -> bool {
        match self {
            Self::IncreaseFontSize
            | Self::DecreaseFontSize
            | Self::ResetFontSize
            | Self::ReloadKeymap
            | Self::ReloadHooks
            | Self::ReloadConfig
            | Self::ToggleTerminalZoom
            | Self::CloseSession => false,
            Self::Dashboard => context
                .facts
                .intersects(PaletteFacts::TERMINAL_ZOOM_ACTIVE | PaletteFacts::PICKER_ACTIVE),
            Self::SwapPanes => context.has(PaletteFacts::PICKER_ACTIVE),
            _ => context.modal_active(),
        }
    }

    /// Whether the command's row is worth listing at all. Deliberately wider than [`Self::is_enabled`]: the palette
    /// lists Rename Session with no session open, showing it inert, while the menu item disables.
    #[must_use]
    pub fn is_visible(self, context: &PaletteContext) -> bool {
        match self {
            Self::DeleteWorkspace => context.has(PaletteFacts::CAN_REMOVE_WORKSPACE),
            Self::ToggleFlaggedView => context
                .facts
                .intersects(PaletteFacts::SIDEBAR_SHOWS_FLAGGED_ONLY | PaletteFacts::HAS_FLAGGED_SESSIONS),
            Self::ClearFlagged => context.has(PaletteFacts::HAS_FLAGGED_SESSIONS),
            // the store refuses to enable an empty set, the state the bottom-bar toggle renders disabled in
            Self::ClearFocus | Self::ToggleWorkspaceFilter => context.has(PaletteFacts::HAS_MARKED_WORKSPACES),
            // hidden only where it would be a silent no-op; not gated on sidebar mode, as marking is model state
            Self::AddWorkspaceToFocus => !context.has(PaletteFacts::ACTIVE_WORKSPACE_MARKED),
            // these fold workspace rows, which the flat flagged list does not render
            Self::ExpandWorkspaces | Self::CollapseWorkspaces | Self::ToggleWorkspaceCollapse => {
                context.sidebar_shows_workspace_rows()
            }
            Self::PreviousWorkspace | Self::NextWorkspace => context.has(PaletteFacts::SIDEBAR_SHOWS_WORKSPACE_TREE),
            Self::FocusLeftPane | Self::FocusRightPane | Self::CloseSplit => {
                context.has(PaletteFacts::ACTIVE_SESSION_HAS_SPLIT)
            }
            Self::UndoClose => context.has(PaletteFacts::HAS_PENDING_CLOSE),
            Self::ReopenRecent => context.has(PaletteFacts::HAS_RECENT_CLOSED),
            _ => true,
        }
    }

    /// The row's title in `context`: the toggles name what the command will do now.
    #[must_use]
    pub fn title(self, context: &PaletteContext) -> &'static str {
        let top_bottom = context.active_split_axis == Some(SplitAxis::TopBottom);
        match self {
            Self::ToggleFlag if context.has(PaletteFacts::ACTIVE_SESSION_FLAGGED) => "Unflag Session",
            Self::ToggleFlaggedView if context.has(PaletteFacts::SIDEBAR_SHOWS_FLAGGED_ONLY) => "Show All Sessions",
            Self::ToggleWorkspaceCollapse if context.has(PaletteFacts::ACTIVE_WORKSPACE_COLLAPSED) => {
                "Expand Workspace"
            }
            Self::FocusLeftPane if top_bottom => "Focus Top Pane",
            Self::FocusRightPane if top_bottom => "Focus Bottom Pane",
            _ => self.title_default(),
        }
    }

    /// The title in the default (all-false) context (upstream's `title`).
    #[must_use]
    pub fn title_default(self) -> &'static str {
        match self {
            Self::NewSession => "New Session",
            Self::NewWorkspace => "New Workspace",
            Self::OpenDirectory => "Open Directory…",
            Self::RenameSession => "Rename Session",
            Self::DuplicateSession => "Duplicate Session",
            Self::RenameWorkspace => "Rename Workspace",
            Self::CloseSession => "Close Session",
            Self::ReopenRecent => "Reopen Last Closed Item",
            Self::UndoClose => "Reopen Closed Item",
            Self::ClearStatus => "Clear Status",
            Self::PreviousSession => "Previous Session",
            Self::NextSession => "Next Session",
            Self::PreviousAttentionSession => "Previous Attention Session",
            Self::NextAttentionSession => "Next Attention Session",
            Self::PreviousWorkspace => "Previous Workspace",
            Self::NextWorkspace => "Next Workspace",
            Self::PreviousWindow => "Previous Window",
            Self::NextWindow => "Next Window",
            Self::FirstSession => "First Session",
            Self::LastSession => "Last Session",
            Self::ShowAttention => "Show Attention",
            Self::ToggleSplit => "Toggle Vertical Split",
            Self::ToggleHorizontalSplit => "Toggle Horizontal Split",
            Self::CloseSplit => "Close Split",
            Self::SwapPanes => "Swap Panes",
            Self::ToggleScratch => "Toggle Scratch",
            Self::ToggleTerminalZoom => "Toggle Terminal Zoom",
            Self::ToggleSidebar => "Toggle Sidebar",
            Self::ToggleFlag => "Flag Session",
            Self::FocusWorkspace => "Focus Workspace",
            Self::Find => "Find…",
            Self::QuickTerminal => "Quick Terminal",
            Self::Dashboard => "Dashboard",
            Self::ToggleFullscreen => "Toggle Full Screen",
            Self::IncreaseFontSize => "Increase Font Size",
            Self::DecreaseFontSize => "Decrease Font Size",
            Self::ResetFontSize => "Actual Font Size",
            Self::SelectTheme => "Select Theme…",
            Self::AttachRemote => "Attach Remote…",
            Self::EditKeymap => "Edit Keymap",
            Self::ReloadKeymap => "Reload Keymap",
            Self::EditHooks => "Edit Hooks",
            Self::ReloadHooks => "Reload Hooks",
            Self::EditGhosttyConfig => "Edit ghostty.conf",
            Self::ReloadConfig => "Reload Config",
            Self::DeleteWorkspace => "Delete Workspace",
            Self::ToggleFlaggedView => "Show Flagged Sessions",
            Self::ClearFlagged => "Clear Flagged",
            Self::ClearFocus => "Clear Focus",
            Self::AddWorkspaceToFocus => "Add Workspace to Focus",
            Self::ToggleWorkspaceFilter => "Toggle Workspace Filter",
            Self::ExpandWorkspaces => "Expand Workspaces",
            Self::CollapseWorkspaces => "Collapse Workspaces",
            Self::ToggleWorkspaceCollapse => "Collapse Workspace",
            Self::FocusLeftPane => "Focus Left Pane",
            Self::FocusRightPane => "Focus Right Pane",
        }
    }

    /// The rebindable built-in action behind the row; `None` for a palette-only command.
    #[must_use]
    pub fn builtin_action(self) -> Option<BuiltinAction> {
        Some(match self {
            Self::NewSession => BuiltinAction::NewSession,
            Self::NewWorkspace => BuiltinAction::NewWorkspace,
            Self::OpenDirectory => BuiltinAction::OpenDirectory,
            Self::RenameSession => BuiltinAction::RenameSession,
            Self::DuplicateSession => BuiltinAction::DuplicateSession,
            Self::RenameWorkspace => BuiltinAction::RenameWorkspace,
            Self::CloseSession => BuiltinAction::CloseSession,
            Self::ReopenRecent => BuiltinAction::ReopenRecent,
            Self::UndoClose => BuiltinAction::UndoClose,
            Self::ClearStatus => BuiltinAction::ClearStatus,
            Self::PreviousSession => BuiltinAction::PreviousSession,
            Self::NextSession => BuiltinAction::NextSession,
            Self::PreviousAttentionSession => BuiltinAction::PreviousAttentionSession,
            Self::NextAttentionSession => BuiltinAction::NextAttentionSession,
            Self::PreviousWorkspace => BuiltinAction::PreviousWorkspace,
            Self::NextWorkspace => BuiltinAction::NextWorkspace,
            Self::PreviousWindow => BuiltinAction::PreviousWindow,
            Self::NextWindow => BuiltinAction::NextWindow,
            Self::ToggleWorkspaceCollapse => BuiltinAction::ToggleWorkspaceCollapse,
            Self::FirstSession => BuiltinAction::FirstSession,
            Self::LastSession => BuiltinAction::LastSession,
            Self::ShowAttention => BuiltinAction::ShowAttention,
            Self::ToggleSplit => BuiltinAction::ToggleSplit,
            Self::ToggleHorizontalSplit => BuiltinAction::ToggleHorizontalSplit,
            Self::ToggleScratch => BuiltinAction::ToggleScratch,
            Self::ToggleTerminalZoom => BuiltinAction::ToggleTerminalZoom,
            Self::ToggleSidebar => BuiltinAction::ToggleSidebar,
            Self::ToggleFlag => BuiltinAction::ToggleFlag,
            Self::FocusWorkspace => BuiltinAction::FocusWorkspace,
            Self::Find => BuiltinAction::ToggleSearch,
            Self::QuickTerminal => BuiltinAction::QuickTerminal,
            Self::Dashboard => BuiltinAction::Dashboard,
            Self::ToggleFullscreen => BuiltinAction::ToggleFullscreen,
            Self::IncreaseFontSize => BuiltinAction::IncreaseFontSize,
            Self::DecreaseFontSize => BuiltinAction::DecreaseFontSize,
            Self::ResetFontSize => BuiltinAction::ResetFontSize,
            Self::SelectTheme => BuiltinAction::SelectTheme,
            Self::DeleteWorkspace => BuiltinAction::DeleteWorkspace,
            Self::ToggleFlaggedView => BuiltinAction::ToggleFlaggedView,
            Self::ToggleWorkspaceFilter => BuiltinAction::ToggleWorkspaceFilter,
            Self::FocusLeftPane => BuiltinAction::FocusLeftPane,
            Self::FocusRightPane => BuiltinAction::FocusRightPane,
            Self::AttachRemote => BuiltinAction::AttachRemote,
            Self::EditKeymap
            | Self::ReloadKeymap
            | Self::EditHooks
            | Self::ReloadHooks
            | Self::EditGhosttyConfig
            | Self::ReloadConfig
            | Self::ClearFlagged
            | Self::ClearFocus
            | Self::AddWorkspaceToFocus
            | Self::ExpandWorkspaces
            | Self::CollapseWorkspaces
            | Self::CloseSplit
            | Self::SwapPanes => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use agterm_proto::AskButton;

    use super::*;
    use crate::modal::Modals;
    use crate::model::{AskAlignment, AskStyle, PendingAsk};

    type F = PaletteFacts;

    fn ctx(facts: PaletteFacts) -> PaletteContext {
        PaletteContext::new(facts)
    }

    /// Everything present, nothing covering: every command's menu item is live here.
    const LIVE: PaletteFacts = F::CAN_REMOVE_WORKSPACE
        .union(F::HAS_FLAGGED_SESSIONS)
        .union(F::SIDEBAR_SHOWS_WORKSPACE_TREE)
        .union(F::SIDEBAR_SHOWS_WORKSPACE_ROWS)
        .union(F::HAS_MARKED_WORKSPACES)
        .union(F::CAN_STEP_WORKSPACES)
        .union(F::CAN_STEP_WINDOWS)
        .union(F::ACTIVE_SESSION_HAS_SPLIT)
        .union(F::HAS_PENDING_CLOSE)
        .union(F::HAS_RECENT_CLOSED)
        .union(F::HAS_ACTIVE_SESSION)
        .union(F::HAS_CURRENT_WORKSPACE)
        .union(F::HAS_REMOTES);

    /// The commands whose menu item carries no modal term at all.
    const COVER_PROOF: [PaletteCommand; 8] = [
        PaletteCommand::CloseSession,
        PaletteCommand::ReloadKeymap,
        PaletteCommand::ReloadHooks,
        PaletteCommand::ReloadConfig,
        PaletteCommand::IncreaseFontSize,
        PaletteCommand::DecreaseFontSize,
        PaletteCommand::ResetFontSize,
        PaletteCommand::ToggleTerminalZoom,
    ];

    const NEED_SESSION: [PaletteCommand; 15] = [
        PaletteCommand::RenameSession,
        PaletteCommand::DuplicateSession,
        PaletteCommand::ClearStatus,
        PaletteCommand::ToggleFlag,
        PaletteCommand::ToggleSplit,
        PaletteCommand::ToggleHorizontalSplit,
        PaletteCommand::SwapPanes,
        PaletteCommand::ToggleScratch,
        PaletteCommand::Find,
        PaletteCommand::PreviousSession,
        PaletteCommand::NextSession,
        PaletteCommand::PreviousAttentionSession,
        PaletteCommand::NextAttentionSession,
        PaletteCommand::FirstSession,
        PaletteCommand::LastSession,
    ];

    const FOLDS: [PaletteCommand; 3] = [
        PaletteCommand::ExpandWorkspaces,
        PaletteCommand::CollapseWorkspaces,
        PaletteCommand::ToggleWorkspaceCollapse,
    ];

    #[test]
    fn titles_match_the_palette_source_order() {
        let titles: Vec<&str> = PaletteCommand::all().map(PaletteCommand::title_default).collect();
        assert_eq!(
            titles,
            [
                "New Session",
                "New Workspace",
                "Open Directory…",
                "Rename Session",
                "Duplicate Session",
                "Rename Workspace",
                "Close Session",
                "Reopen Last Closed Item",
                "Reopen Closed Item",
                "Clear Status",
                "Previous Session",
                "Next Session",
                "Previous Attention Session",
                "Next Attention Session",
                "Previous Workspace",
                "Next Workspace",
                "Previous Window",
                "Next Window",
                "First Session",
                "Last Session",
                "Show Attention",
                "Toggle Vertical Split",
                "Toggle Horizontal Split",
                "Close Split",
                "Swap Panes",
                "Toggle Scratch",
                "Toggle Terminal Zoom",
                "Toggle Sidebar",
                "Flag Session",
                "Focus Workspace",
                "Find…",
                "Quick Terminal",
                "Dashboard",
                "Toggle Full Screen",
                "Increase Font Size",
                "Decrease Font Size",
                "Actual Font Size",
                "Select Theme…",
                "Attach Remote…",
                "Edit Keymap",
                "Reload Keymap",
                "Edit Hooks",
                "Reload Hooks",
                "Edit ghostty.conf",
                "Reload Config",
                "Delete Workspace",
                "Show Flagged Sessions",
                "Clear Flagged",
                "Clear Focus",
                "Add Workspace to Focus",
                "Toggle Workspace Filter",
                "Expand Workspaces",
                "Collapse Workspaces",
                "Collapse Workspace",
                "Focus Left Pane",
                "Focus Right Pane",
            ]
        );
        for command in PaletteCommand::all() {
            assert_eq!(command.title(&PaletteContext::default()), command.title_default());
        }
    }

    #[test]
    fn catalog_has_the_expected_static_command_count() {
        assert_eq!(PaletteCommand::all().count(), 56);
    }

    #[test]
    fn ids_round_trip() {
        for command in PaletteCommand::all() {
            assert_eq!(PaletteCommand::from_id(command.id()), Some(command));
        }
        assert_eq!(PaletteCommand::NewSession.id(), "newSession");
        assert_eq!(PaletteCommand::EditGhosttyConfig.id(), "editGhosttyConfig");
        assert_eq!(PaletteCommand::from_id("NewSession"), None);
    }

    #[test]
    fn context_titles_match_toggle_state() {
        use PaletteCommand as C;
        assert_eq!(C::ToggleFlag.title(&ctx(F::empty())), "Flag Session");
        assert_eq!(C::ToggleFlag.title(&ctx(F::ACTIVE_SESSION_FLAGGED)), "Unflag Session");
        assert_eq!(C::ToggleFlaggedView.title(&ctx(F::empty())), "Show Flagged Sessions");
        assert_eq!(
            C::ToggleFlaggedView.title(&ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY)),
            "Show All Sessions"
        );
        let top_bottom = PaletteContext::default().with_split_axis(SplitAxis::TopBottom);
        assert_eq!(C::FocusLeftPane.title(&top_bottom), "Focus Top Pane");
        assert_eq!(C::FocusRightPane.title(&top_bottom), "Focus Bottom Pane");
        let left_right = PaletteContext::default().with_split_axis(SplitAxis::LeftRight);
        assert_eq!(C::FocusLeftPane.title(&left_right), "Focus Left Pane");
        assert_eq!(C::FocusRightPane.title(&left_right), "Focus Right Pane");
        assert_eq!(C::ToggleWorkspaceCollapse.title(&ctx(F::empty())), "Collapse Workspace");
        assert_eq!(
            C::ToggleWorkspaceCollapse.title(&ctx(F::ACTIVE_WORKSPACE_COLLAPSED)),
            "Expand Workspace"
        );
    }

    #[test]
    fn clear_flagged_visible_only_when_something_is_flagged() {
        assert!(!PaletteCommand::ClearFlagged.is_visible(&ctx(F::empty())));
        assert!(PaletteCommand::ClearFlagged.is_visible(&ctx(F::HAS_FLAGGED_SESSIONS)));
    }

    #[test]
    fn flagged_toggle_visible_with_flags_or_while_already_in_flagged_view() {
        assert!(!PaletteCommand::ToggleFlaggedView.is_visible(&ctx(F::empty())));
        assert!(PaletteCommand::ToggleFlaggedView.is_visible(&ctx(F::HAS_FLAGGED_SESSIONS)));
        assert!(PaletteCommand::ToggleFlaggedView.is_visible(&ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY)));
    }

    #[test]
    fn workspace_fold_commands_show_whenever_the_sidebar_has_workspace_rows() {
        let ordinary_tree = ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE | F::SIDEBAR_SHOWS_WORKSPACE_ROWS);
        let flagged_tree = ctx(F::SIDEBAR_SHOWS_WORKSPACE_ROWS | F::SIDEBAR_SHOWS_FLAGGED_ONLY);
        let flagged_flat = ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY);
        for command in FOLDS {
            assert!(command.is_visible(&ordinary_tree));
            assert!(command.is_visible(&flagged_tree));
            assert!(!command.is_visible(&flagged_flat));
        }
        assert!(!PaletteCommand::ToggleWorkspaceCollapse.is_enabled(&ctx(F::SIDEBAR_SHOWS_WORKSPACE_ROWS)));
    }

    // regression: a caller that sets only the tree fact must keep its fold commands.
    #[test]
    fn workspace_fold_commands_stay_visible_for_a_caller_that_omits_the_rows_fact() {
        for command in FOLDS {
            assert!(command.is_visible(&ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE)));
            assert!(!command.is_visible(&ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY)));
        }
    }

    #[test]
    fn workspace_step_commands_show_only_in_the_ordinary_tree() {
        let ordinary_tree = ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE | F::SIDEBAR_SHOWS_WORKSPACE_ROWS);
        let flagged_tree = ctx(F::SIDEBAR_SHOWS_WORKSPACE_ROWS | F::SIDEBAR_SHOWS_FLAGGED_ONLY);
        for command in [PaletteCommand::PreviousWorkspace, PaletteCommand::NextWorkspace] {
            assert!(command.is_visible(&ordinary_tree));
            assert!(!command.is_visible(&flagged_tree));
            assert!(!command.is_visible(&ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY)));
            assert!(!command.is_enabled(&ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE)));
        }
    }

    #[test]
    fn workspace_steps_disable_with_nowhere_to_step() {
        let alone = ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE | F::SIDEBAR_SHOWS_WORKSPACE_ROWS | F::HAS_CURRENT_WORKSPACE);
        let several = ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE | F::CAN_STEP_WORKSPACES | F::HAS_CURRENT_WORKSPACE);
        for command in [PaletteCommand::PreviousWorkspace, PaletteCommand::NextWorkspace] {
            assert!(!command.is_enabled(&alone));
            assert!(command.is_enabled(&several));
            assert!(command.is_visible(&alone), "still listed, just inert");
        }
        assert!(PaletteCommand::ToggleWorkspaceCollapse.is_enabled(&alone));
    }

    #[test]
    fn window_steps_disable_with_one_open_window() {
        for command in [PaletteCommand::PreviousWindow, PaletteCommand::NextWindow] {
            assert!(!command.is_enabled(&ctx(F::empty())));
            assert!(
                command.is_enabled(&ctx(F::CAN_STEP_WINDOWS)),
                "app-global: no session or workspace needed"
            );
            assert!(command.is_visible(&ctx(F::empty())), "still listed, just inert");
            assert!(
                !command.is_enabled(&ctx(F::CAN_STEP_WINDOWS | F::TERMINAL_ZOOM_ACTIVE)),
                "an ordinary modal cover still blocks it"
            );
        }
    }

    #[test]
    fn workspace_and_split_commands_follow_their_predicates() {
        use PaletteCommand as C;
        let none = ctx(F::empty());
        for (command, fact) in [
            (C::DeleteWorkspace, F::CAN_REMOVE_WORKSPACE),
            (C::ClearFocus, F::HAS_MARKED_WORKSPACES),
            (C::FocusLeftPane, F::ACTIVE_SESSION_HAS_SPLIT),
            (C::FocusRightPane, F::ACTIVE_SESSION_HAS_SPLIT),
            (C::CloseSplit, F::ACTIVE_SESSION_HAS_SPLIT),
            (C::UndoClose, F::HAS_PENDING_CLOSE),
            (C::ReopenRecent, F::HAS_RECENT_CLOSED),
        ] {
            assert!(!command.is_visible(&none), "{command:?}");
            assert!(command.is_visible(&ctx(fact)), "{command:?}");
        }
        assert!(C::SwapPanes.is_visible(&none));
        assert!(!C::SwapPanes.is_enabled(&ctx(F::HAS_ACTIVE_SESSION)));
        assert!(C::SwapPanes.is_enabled(&ctx(F::HAS_ACTIVE_SESSION | F::ACTIVE_SESSION_HAS_SPLIT)));
    }

    #[test]
    fn workspace_focus_entries_follow_the_marked_set() {
        use PaletteCommand as C;
        let tree = ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE);
        let flagged = ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY);
        assert!(C::AddWorkspaceToFocus.is_visible(&tree));
        assert!(C::AddWorkspaceToFocus.is_visible(&flagged));
        assert!(!C::AddWorkspaceToFocus.is_visible(&ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE | F::ACTIVE_WORKSPACE_MARKED)));
        assert!(!C::AddWorkspaceToFocus.is_visible(&ctx(F::SIDEBAR_SHOWS_FLAGGED_ONLY | F::ACTIVE_WORKSPACE_MARKED)));
        assert!(C::AddWorkspaceToFocus.is_visible(&ctx(F::SIDEBAR_SHOWS_WORKSPACE_TREE | F::HAS_MARKED_WORKSPACES)));
        assert!(!C::ToggleWorkspaceFilter.is_visible(&ctx(F::empty())));
        assert!(C::ToggleWorkspaceFilter.is_visible(&ctx(F::HAS_MARKED_WORKSPACES)));
        assert_eq!(C::AddWorkspaceToFocus.title(&tree), "Add Workspace to Focus");
        assert_eq!(
            C::ToggleWorkspaceFilter.title(&ctx(F::HAS_MARKED_WORKSPACES)),
            "Toggle Workspace Filter"
        );
    }

    #[test]
    fn hook_commands_mirror_their_keymap_counterparts() {
        use PaletteCommand as C;
        assert_eq!(C::EditHooks.title(&ctx(LIVE)), "Edit Hooks");
        assert_eq!(C::ReloadHooks.title(&ctx(LIVE)), "Reload Hooks");
        assert_eq!(C::EditHooks.builtin_action(), None);
        assert_eq!(C::ReloadHooks.builtin_action(), None);
        for context in [ctx(LIVE), ctx(F::empty())] {
            assert_eq!(C::EditHooks.is_enabled(&context), C::EditKeymap.is_enabled(&context));
            assert_eq!(
                C::ReloadHooks.is_enabled(&context),
                C::ReloadKeymap.is_enabled(&context)
            );
        }
    }

    #[test]
    fn every_command_is_live_when_nothing_is_missing_or_covering() {
        for command in PaletteCommand::all() {
            assert!(command.is_enabled(&ctx(LIVE)), "{command:?} should be live");
        }
    }

    #[test]
    fn session_presence_gates_the_same_commands_the_menu_does() {
        let context = ctx(LIVE - F::HAS_ACTIVE_SESSION);
        for command in PaletteCommand::all() {
            assert_eq!(
                command.is_enabled(&context),
                !NEED_SESSION.contains(&command),
                "{command:?}"
            );
        }
    }

    #[test]
    fn attach_remote_needs_a_configured_remote() {
        let none = ctx(F::HAS_CURRENT_WORKSPACE);
        assert!(PaletteCommand::AttachRemote.is_visible(&none), "stays listed, inert");
        assert!(!PaletteCommand::AttachRemote.is_enabled(&none));
        assert!(PaletteCommand::AttachRemote.is_enabled(&ctx(F::HAS_CURRENT_WORKSPACE | F::HAS_REMOTES)));
        assert!(
            !PaletteCommand::AttachRemote
                .is_enabled(&ctx(F::HAS_CURRENT_WORKSPACE | F::HAS_REMOTES | F::PICKER_ACTIVE))
        );
        assert_eq!(
            PaletteCommand::AttachRemote.builtin_action(),
            Some(BuiltinAction::AttachRemote)
        );
    }

    #[test]
    fn workspace_presence_gates_the_workspace_entries() {
        use PaletteCommand as C;
        let context = ctx(LIVE - F::HAS_CURRENT_WORKSPACE);
        let need_workspace = [
            C::RenameWorkspace,
            C::FocusWorkspace,
            C::AddWorkspaceToFocus,
            C::PreviousWorkspace,
            C::NextWorkspace,
            C::ToggleWorkspaceCollapse,
            C::AttachRemote,
        ];
        for command in PaletteCommand::all() {
            assert_eq!(
                command.is_enabled(&context),
                !need_workspace.contains(&command),
                "{command:?}"
            );
        }
    }

    #[test]
    fn terminal_zoom_leaves_only_the_items_carrying_no_modal_term() {
        let context = ctx((LIVE - F::HAS_REMOTES) | F::TERMINAL_ZOOM_ACTIVE);
        for command in PaletteCommand::all() {
            let expected = COVER_PROOF.contains(&command) || command == PaletteCommand::SwapPanes;
            assert_eq!(command.is_enabled(&context), expected, "{command:?}");
        }
    }

    #[test]
    fn a_pending_picker_leaves_the_same_set() {
        let context = ctx((LIVE - F::HAS_REMOTES) | F::PICKER_ACTIVE);
        for command in PaletteCommand::all() {
            assert_eq!(
                command.is_enabled(&context),
                COVER_PROOF.contains(&command),
                "{command:?}"
            );
        }
    }

    #[test]
    fn a_pending_ask_uses_the_shared_modal_predicate() {
        let window = uuid::Uuid::new_v4();
        let mut modals = Modals::default();
        let ask = PendingAsk {
            id: "ask".to_owned(),
            title: "Continue?".to_owned(),
            message: None,
            buttons: vec![AskButton {
                id: "yes".to_owned(),
                label: "Yes".to_owned(),
                hotkey: None,
            }],
            default_id: None,
            destructive_id: None,
            style: AskStyle::Gui,
            align: AskAlignment::Right,
            width: None,
            anchor: None,
        };
        modals.open_window_ask(window, ask).unwrap();
        let pending = modals.controller(window).unwrap().modal_pending();
        let mut context = ctx(LIVE - F::HAS_REMOTES);
        context.facts.set(F::PICKER_ACTIVE, pending);
        assert!(context.modal_active());
        for command in PaletteCommand::all() {
            assert_eq!(
                command.is_enabled(&context),
                COVER_PROOF.contains(&command),
                "{command:?}"
            );
        }
        modals.resolve_window_ask(
            window,
            agterm_proto::ControlAskResult::outcome(agterm_proto::ControlAskOutcome::Cancelled),
        );
        let pending = modals.controller(window).unwrap().modal_pending();
        let mut cleared = PaletteContext::default();
        cleared.facts.set(F::PICKER_ACTIVE, pending);
        assert!(!cleared.modal_active());
    }

    #[test]
    fn the_open_dashboard_spares_its_own_toggle() {
        let context = ctx((LIVE - F::HAS_REMOTES) | F::DASHBOARD_OPEN);
        for command in PaletteCommand::all() {
            let expected = COVER_PROOF.contains(&command)
                || command == PaletteCommand::Dashboard
                || command == PaletteCommand::SwapPanes;
            assert_eq!(command.is_enabled(&context), expected, "{command:?}");
        }
        assert!(!PaletteCommand::Dashboard.is_enabled(&ctx(F::HAS_ACTIVE_SESSION
            | F::HAS_CURRENT_WORKSPACE
            | F::TERMINAL_ZOOM_ACTIVE
            | F::DASHBOARD_OPEN)));
    }

    #[test]
    fn rows_stay_visible_where_the_menu_item_goes_disabled() {
        let context = ctx(F::empty());
        let rows = NEED_SESSION
            .into_iter()
            .chain([PaletteCommand::RenameWorkspace, PaletteCommand::FocusWorkspace]);
        for command in rows {
            assert!(command.is_visible(&context), "{command:?} stays listed");
            assert!(!command.is_enabled(&context), "{command:?} stays inert");
        }
    }

    #[test]
    fn builtin_mappings_cover_rebindable_commands() {
        use PaletteCommand as C;
        assert_eq!(C::NewSession.builtin_action(), Some(BuiltinAction::NewSession));
        assert_eq!(C::Find.builtin_action(), Some(BuiltinAction::ToggleSearch));
        assert_eq!(
            C::ToggleTerminalZoom.builtin_action(),
            Some(BuiltinAction::ToggleTerminalZoom)
        );
        assert_eq!(C::ResetFontSize.builtin_action(), Some(BuiltinAction::ResetFontSize));
        assert_eq!(C::Dashboard.builtin_action(), Some(BuiltinAction::Dashboard));
        assert_eq!(C::ReopenRecent.builtin_action(), Some(BuiltinAction::ReopenRecent));
        assert_eq!(C::UndoClose.builtin_action(), Some(BuiltinAction::UndoClose));
        assert_eq!(
            C::ToggleWorkspaceFilter.builtin_action(),
            Some(BuiltinAction::ToggleWorkspaceFilter)
        );
        assert_eq!(C::SwapPanes.builtin_action(), None);
        assert_eq!(C::ClearFlagged.builtin_action(), None);
        assert_eq!(C::AddWorkspaceToFocus.builtin_action(), None);
        assert_eq!(C::ExpandWorkspaces.builtin_action(), None);
    }

    #[test]
    fn for_builtin_reverses_the_mapping() {
        for command in PaletteCommand::all() {
            if let Some(action) = command.builtin_action() {
                assert_eq!(PaletteCommand::for_builtin(action), Some(command));
            }
        }
        assert_eq!(
            PaletteCommand::for_builtin(BuiltinAction::ToggleSearch),
            Some(PaletteCommand::Find)
        );
        for action in [
            BuiltinAction::NewWindow,
            BuiltinAction::RenameWindow,
            BuiltinAction::DeleteWindow,
            BuiltinAction::SessionPalette,
            BuiltinAction::CommandPalette,
            BuiltinAction::CustomCommandPalette,
        ] {
            assert_eq!(PaletteCommand::for_builtin(action), None, "{action:?}");
        }
    }
}
