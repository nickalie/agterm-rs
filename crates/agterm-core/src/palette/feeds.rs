//! The rows each palette lists (upstream's `paletteActions`, `paletteSessions`, `paletteThemes`,
//! `paletteCustomCommands` and `paletteAttention`); the attention palette reads the cross-window attention list the
//! title bar's bell shares ([`WindowLibrary::attention_across_windows`]).

use uuid::Uuid;

use super::{PaletteAction, PaletteEntry, PaletteMode, PaletteShortcut};
use crate::Core;
use crate::keymap::{BuiltinAction, CustomCommand};
use crate::model::{AppStore, WindowLibrary};
use crate::palette_catalog::PaletteCommand;
use crate::pane_env::uuid_string;
use crate::theme::ThemeCatalog;

impl Core {
    /// The rows of palette `mode` for the frontmost window, in source order (the search ranks them).
    #[must_use]
    pub fn palette_entries(&self, mode: PaletteMode) -> Vec<PaletteEntry> {
        match mode {
            PaletteMode::Actions => self.palette_actions(),
            PaletteMode::Sessions => self.palette_sessions(),
            PaletteMode::Themes => self.palette_themes(),
            PaletteMode::CustomCommands => self.custom_command_entries(None),
            PaletteMode::Attention => self.palette_attention(),
        }
    }

    /// The id of the row the theme palette opens on: the theme on screen, a no-op preview.
    #[must_use]
    pub fn palette_current_theme_id(&self) -> String {
        ThemeCatalog::id(self.committed_active_theme().as_deref())
    }

    /// A built-in's live chord, as the menu shows it.
    fn shortcut(&self, action: BuiltinAction) -> Option<PaletteShortcut> {
        self.keymap().equivalent(action).cloned().map(PaletteShortcut::Chord)
    }

    /// The action palette: the visible static rows, the window rows, Settings… (the build's own), a Move Session row
    /// per other workspace, then the custom commands badged `custom` (upstream's `paletteActions`).
    fn palette_actions(&self) -> Vec<PaletteEntry> {
        let Some(window) = self.library().active_window_id() else {
            return Vec::new();
        };
        let context = self.palette_context(window);
        let mut entries: Vec<PaletteEntry> = PaletteCommand::all()
            .filter(|command| command.is_visible(&context))
            .map(|command| PaletteEntry {
                id: command.id().to_owned(),
                title: command.title(&context).to_owned(),
                subtitle: None,
                shortcut: command.builtin_action().and_then(|action| self.shortcut(action)),
                badge: None,
                indicator: None,
                action: PaletteAction::Command(command),
            })
            .collect();
        let mut builtins = vec![
            (BuiltinAction::NewWindow, "New Window"),
            (BuiltinAction::RenameWindow, "Rename Window"),
        ];
        if self.library().can_remove_window() {
            builtins.push((BuiltinAction::DeleteWindow, "Delete Window"));
        }
        entries.extend(builtins.into_iter().map(|(action, title)| PaletteEntry {
            id: title.to_owned(),
            title: title.to_owned(),
            subtitle: None,
            shortcut: self.shortcut(action),
            badge: None,
            indicator: None,
            action: PaletteAction::Builtin(action),
        }));
        entries.push(plain_entry(
            "settings".to_owned(),
            "Settings…".to_owned(),
            PaletteAction::Settings,
        ));
        // one "Open Window: <name>" per closed window: the open ones are on screen already
        let library = self.library();
        entries.extend(
            library
                .windows()
                .iter()
                .filter(|info| !library.is_open(info.id))
                .map(|info| {
                    plain_entry(
                        format!("open-window-{}", uuid_string(info.id)),
                        format!("Open Window: {}", info.name),
                        PaletteAction::OpenWindow(info.id),
                    )
                }),
        );
        if let Some(store) = library.store(window) {
            entries.extend(move_session_entries(store));
        }
        entries.extend(self.custom_command_entries(Some("custom")));
        entries
    }

    /// The keymap's custom commands with their shortcut as written, each tagged `badge`.
    fn custom_command_entries(&self, badge: Option<&str>) -> Vec<PaletteEntry> {
        self.keymap()
            .commands()
            .iter()
            .map(|command: &CustomCommand| PaletteEntry {
                id: format!("custom-{}", uuid_string(command.id)),
                title: command.name.clone(),
                subtitle: None,
                shortcut: (!command.shortcut.is_empty()).then(|| PaletteShortcut::Text(command.shortcut.clone())),
                badge: badge.map(str::to_owned),
                indicator: None,
                action: PaletteAction::Custom(command.id),
            })
            .collect()
    }

    /// The frontmost window's navigable sessions (the flagged set in flagged mode, the marked workspaces' under the
    /// focus filter, else all), subtitled `workspace · detail` (upstream's `paletteSessions`).
    fn palette_sessions(&self) -> Vec<PaletteEntry> {
        let Some(store) = self
            .library()
            .active_window_id()
            .and_then(|window| self.library().store(window))
        else {
            return Vec::new();
        };
        store
            .navigable_sessions()
            .into_iter()
            .map(|session| {
                let id = session.id();
                let workspace = store
                    .workspace_for_session(id)
                    .map_or("", |workspace| workspace.name.as_str());
                PaletteEntry {
                    subtitle: Some(format!("{workspace} · {}", session.subtitle_detail())),
                    ..plain_entry(
                        uuid_string(id),
                        session.display_name(),
                        PaletteAction::SelectSession(id),
                    )
                }
            })
            .collect()
    }

    /// Every open window's sessions needing attention, in attention order, each with its status glyph
    /// (upstream's `paletteAttention`).
    fn palette_attention(&self) -> Vec<PaletteEntry> {
        let library = self.library();
        library
            .attention_across_windows()
            .iter()
            .map(|entry| {
                let session = entry.session.id();
                PaletteEntry {
                    subtitle: Some(library.attention_subtitle(entry)),
                    indicator: Some(entry.session.agent_indicator.clone()),
                    ..plain_entry(
                        uuid_string(session),
                        entry.session.display_name(),
                        PaletteAction::Attention {
                            window: entry.window,
                            session,
                        },
                    )
                }
            })
            .collect()
    }

    /// The bundled themes, the one on screen badged `current`; ghostty's built-in row is left out while following the
    /// appearance, as a dual theme needs two named sides (upstream's `paletteThemes`).
    fn palette_themes(&self) -> Vec<PaletteEntry> {
        let current = self.committed_active_theme();
        let following = self.settings().follows_system_appearance();
        self.theme_catalog()
            .entries()
            .into_iter()
            .filter(|entry| !(following && entry.is_default()))
            .map(|entry| PaletteEntry {
                badge: (entry.name == current).then(|| "current".to_owned()),
                ..plain_entry(entry.id, entry.title, PaletteAction::Theme(entry.name))
            })
            .collect()
    }
}

/// A "Move Session to <workspace>" row per workspace other than the selected session's own (upstream skips the
/// session's workspace, not the current one: a fresh workspace is current while the selection sits elsewhere).
fn move_session_entries(store: &AppStore) -> Vec<PaletteEntry> {
    let Some(session) = store.selected_session_id() else {
        return Vec::new();
    };
    let Some(owner) = store.workspace_for_session(session).map(crate::model::Workspace::id) else {
        return Vec::new();
    };
    store
        .workspaces()
        .iter()
        .filter(|workspace| workspace.id() != owner)
        .map(|workspace| {
            plain_entry(
                format!("move-{}", uuid_string(workspace.id())),
                format!("Move Session to {}", workspace.name),
                PaletteAction::MoveSession {
                    session,
                    workspace: workspace.id(),
                },
            )
        })
        .collect()
}

fn plain_entry(id: String, title: String, action: PaletteAction) -> PaletteEntry {
    PaletteEntry {
        id,
        title,
        subtitle: None,
        shortcut: None,
        badge: None,
        indicator: None,
        action,
    }
}

/// Whether `window`'s store holds `session`.
pub(crate) fn window_holds(library: &WindowLibrary, window: Uuid, session: Uuid) -> bool {
    library
        .store(window)
        .is_some_and(|store| store.session(session).is_some())
}
