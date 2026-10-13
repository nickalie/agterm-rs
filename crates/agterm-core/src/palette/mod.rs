//! The command palettes (upstream's `PaletteController` and the palette half of `AppActions+Palette.swift`): one
//! app-global palette, drawn in the frontmost window, in one of five modes, each listing its own rows
//! ([`feeds`]), ranked by the fuzzy search ([`search`]) and run here ([`run`]).
//!
//! The palette owns the keyboard while it is open, so it opens only where the frontmost window takes UI actions (no
//! terminal zoom, no dashboard, no picker or GUI ask in its modal slot, upstream's `uiActionsEnabled`), and every
//! commit closes it once that stops holding ([`Core::commit`] → `settle_palette`): a `pick.open` or GUI `ask.open` on
//! the frontmost window, a zoom or the dashboard there, or a window with a pending pick coming to the front (upstream's
//! `palette.close()` call sites). While it is open the frontmost window's auto-follow waits; its close gives the
//! keyboard back to the active session.
//!
//! The theme palette previews the selected theme live: entering it captures both theme slots, a preview writes the
//! slot on screen without saving `settings.json`, a run commits it, and leaving the palette any other way puts both
//! slots back (upstream's `beginThemePreview`/`previewTheme`/`commitThemePreview`/`cancelThemePreview`).

pub mod feeds;
pub mod run;
pub mod search;

use uuid::Uuid;

use crate::Core;
use crate::keymap::Chord;
use crate::model::AgentIndicator;
use crate::palette_catalog::PaletteCommand;

pub use search::{filter_entries, palette_search_keys, pick_custom_row_label};

/// Which palette is open (upstream's `PaletteMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaletteMode {
    /// The app's commands.
    Actions,
    /// The frontmost window's navigable sessions.
    Sessions,
    /// The bundled themes, previewed live.
    Themes,
    /// The keymap's custom commands alone.
    CustomCommands,
    /// Every open window's sessions that need attention.
    Attention,
}

impl PaletteMode {
    /// The search field's placeholder (upstream's `placeholder`).
    #[must_use]
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Actions => "Run an action…",
            Self::Sessions => "Go to session…",
            Self::Themes => "Select a theme…",
            Self::CustomCommands => "Run a custom command…",
            Self::Attention => "Go to a session that needs attention…",
        }
    }
}

/// A row's right-aligned shortcut hint: a built-in's live chord, or a custom command's shortcut as the user spelled it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteShortcut {
    Chord(Chord),
    Text(String),
}

/// What running a row does (upstream's `PaletteItem.run` closures, as data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteAction {
    /// A static action row.
    Command(PaletteCommand),
    /// New, Rename or Delete Window: a built-in no palette command owns.
    Builtin(crate::keymap::BuiltinAction),
    /// Opens a closed window.
    OpenWindow(Uuid),
    /// Moves the frontmost window's selected session to another workspace.
    MoveSession { session: Uuid, workspace: Uuid },
    /// Runs the keymap's custom command with this id.
    Custom(Uuid),
    /// Selects a session of the frontmost window.
    SelectSession(Uuid),
    /// Selects a session needing attention in its window, raising that window.
    Attention { window: Uuid, session: Uuid },
    /// Commits a theme (`None` is ghostty's built-in colors).
    Theme(Option<String>),
}

/// One palette row (upstream's `PaletteItem`); its enablement is asked of live state, never stored
/// ([`Core::palette_entry_enabled`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub shortcut: Option<PaletteShortcut>,
    /// A small trailing tag (`custom`, `current`).
    pub badge: Option<String>,
    /// A leading status glyph: the attention palette's rows only.
    pub indicator: Option<AgentIndicator>,
    pub action: PaletteAction,
}

/// The theme slots as the theme palette found them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThemePreview {
    theme: Option<String>,
    dark: Option<String>,
}

/// The palette's state.
#[derive(Debug, Default)]
pub(crate) struct PaletteRuntime {
    mode: Option<PaletteMode>,
    /// Bumped on every open and mode change, so the frontend builds a fresh palette (an empty query, the selection on
    /// the first row) and drops what an older one posts.
    generation: u64,
    /// The window whose auto-follow the open palette holds.
    suppressed: Option<Uuid>,
    preview: Option<ThemePreview>,
}

impl Core {
    /// The open palette's mode; `None` while it is closed.
    #[must_use]
    pub fn palette_mode(&self) -> Option<PaletteMode> {
        self.palette.mode
    }

    /// The open palette's generation: a new one for every open and mode change.
    #[must_use]
    pub fn palette_generation(&self) -> u64 {
        self.palette.generation
    }

    /// Whether window `window` draws the open palette: the frontmost window, with no picker or GUI ask in its slot.
    #[must_use]
    pub fn palette_holds(&self, window: Uuid) -> bool {
        self.palette.mode.is_some() && self.library().active_window_id() == Some(window) && !self.modal_pending(window)
    }

    /// Whether window `window` takes UI actions (upstream's `uiActionsEnabled(for:)`): open, not zoomed, no dashboard
    /// open and nothing in its modal slot.
    #[must_use]
    pub fn ui_actions_enabled(&self, window: Uuid) -> bool {
        self.library().is_open(window)
            && self.zoom_target(window).is_none()
            && self.dashboard(window).is_none()
            && !self.modal_pending(window)
    }

    /// The palette launchers (upstream's `toggleSessionPalette`/`toggleActionPalette`/`toggleCustomCommandPalette`/
    /// `toggleAttentionPalette`): the same mode again closes it, another one switches, all in frontmost window
    /// `window` while it takes UI actions. Commit afterwards.
    pub fn toggle_palette(&mut self, window: Uuid, mode: PaletteMode) {
        if !self.palette_launchable(window) {
            return;
        }
        let next = (self.palette.mode != Some(mode)).then_some(mode);
        self.set_palette_mode(next);
    }

    /// Opens `mode` in frontmost window `window` whatever is open (upstream's `palette.open`, the theme picker's and
    /// the action palette's Show Attention launcher), while the window takes UI actions. Commit afterwards.
    pub fn open_palette(&mut self, window: Uuid, mode: PaletteMode) {
        if self.palette_launchable(window) {
            self.set_palette_mode(Some(mode));
        }
    }

    /// Closes the palette (Escape, its scrim, a run), an uncommitted theme preview reverted. Commit afterwards.
    pub fn close_palette(&mut self) {
        self.set_palette_mode(None);
    }

    fn palette_launchable(&self, window: Uuid) -> bool {
        self.library().active_window_id() == Some(window) && self.ui_actions_enabled(window)
    }

    /// Closes the palette once the frontmost window stops taking UI actions, and keeps auto-follow held in the window
    /// that draws it. Run by every commit.
    pub(crate) fn settle_palette(&mut self) {
        if self.palette.mode.is_none() {
            return;
        }
        let front = self.library().active_window_id();
        if !front.is_some_and(|window| self.ui_actions_enabled(window)) {
            self.close_palette();
            return;
        }
        if self.palette.suppressed != front {
            self.hold_auto_follow(front);
        }
    }

    fn set_palette_mode(&mut self, mode: Option<PaletteMode>) {
        let before = self.palette.mode;
        if before == mode {
            return;
        }
        if before == Some(PaletteMode::Themes) {
            self.cancel_theme_preview();
        }
        self.palette.mode = mode;
        if mode.is_none() {
            self.hold_auto_follow(None);
            // upstream's `focusActiveSession` on close: the terminal takes the keyboard back from the field
            if let Some(session) = self
                .library()
                .active_window_id()
                .and_then(|window| self.library().store(window))
                .and_then(crate::model::AppStore::selected_session_id)
            {
                self.request_pane_focus(session);
            }
            return;
        }
        self.palette.generation += 1;
        if before.is_none() {
            let front = self.library().active_window_id();
            self.hold_auto_follow(front);
        }
        if mode == Some(PaletteMode::Themes) {
            self.begin_theme_preview();
        }
    }

    /// Moves the palette's auto-follow hold to `window`, or lifts it.
    fn hold_auto_follow(&mut self, window: Option<Uuid>) {
        if let Some(previous) = self.palette.suppressed.take() {
            self.resume_auto_follow(previous);
        }
        if let Some(window) = window {
            self.suppress_auto_follow(window);
            self.palette.suppressed = Some(window);
        }
    }

    // MARK: - the live theme preview

    /// Whether the theme on screen is the dark slot (upstream's `rendersDarkSlot`).
    fn renders_dark_slot(&self) -> bool {
        self.settings().follows_system_appearance() && self.applied_dark()
    }

    /// The theme on screen, the badged row of the theme palette: the one the palette found while it previews.
    pub(crate) fn committed_active_theme(&self) -> Option<String> {
        let dark = self.applied_dark();
        match &self.palette.preview {
            Some(original) => {
                let mut settings = self.settings().clone();
                settings.theme.clone_from(&original.theme);
                settings.dark_theme.clone_from(&original.dark);
                settings.active_theme(dark).map(str::to_owned)
            }
            None => self.settings().active_theme(dark).map(str::to_owned),
        }
    }

    /// The theme slots to persist while a preview has the live ones: the slots the palette found, so a settings write
    /// mid-preview cannot leak an uncommitted theme (upstream's `committedTheme`).
    pub(crate) fn previewed_slots_to_persist(&self) -> Option<(Option<String>, Option<String>)> {
        self.palette
            .preview
            .as_ref()
            .map(|original| (original.theme.clone(), original.dark.clone()))
    }

    /// Runs `write`, an explicit theme change (`theme.set`), around an open theme preview: the previewed slot goes back
    /// first, the change lands and saves as asked, and the preview starts over from the new slots, so neither the save
    /// nor a later Escape undoes it (upstream's `persistAndApply` moves `committedTheme` the same way).
    pub(crate) fn with_theme_preview_rebased(&mut self, write: impl FnOnce(&mut Self)) {
        let original = self.palette.preview.take();
        if let Some(original) = &original {
            let settings = self.settings_unsaved();
            settings.theme.clone_from(&original.theme);
            settings.dark_theme.clone_from(&original.dark);
        }
        write(self);
        if original.is_some() {
            self.begin_theme_preview();
        }
    }

    fn begin_theme_preview(&mut self) {
        if self.palette.preview.is_none() {
            let settings = self.settings();
            self.palette.preview = Some(ThemePreview {
                theme: settings.theme.clone(),
                dark: settings.dark_theme.clone(),
            });
        }
    }

    /// The theme palette's selection moved to `name` (ghostty's built-in for `None`): it applies live, unsaved, to the
    /// slot on screen. Nothing outside an open theme palette. Commit afterwards.
    pub fn preview_palette_theme(&mut self, name: Option<&str>) {
        if self.palette.mode != Some(PaletteMode::Themes) || self.palette.preview.is_none() {
            return;
        }
        self.set_visible_theme_slot(name.map(str::to_owned));
        self.apply_settings();
    }

    fn set_visible_theme_slot(&mut self, name: Option<String>) {
        let dark = self.renders_dark_slot();
        let settings = self.settings_unsaved();
        if dark {
            settings.dark_theme = name;
        } else {
            settings.theme = name;
        }
    }

    /// Persists the previewed theme: only the slot on screen keeps it, the other goes back to what the palette found
    /// (upstream's `commitTheme(nonActiveOriginal:)`).
    fn commit_theme_preview(&mut self) {
        let Some(original) = self.palette.preview.take() else {
            return;
        };
        let dark = self.renders_dark_slot();
        let settings = self.settings_unsaved();
        let restored = if dark {
            std::mem::replace(&mut settings.theme, original.theme.clone()) != original.theme
        } else {
            std::mem::replace(&mut settings.dark_theme, original.dark.clone()) != original.dark
        };
        if restored {
            self.apply_settings();
        }
        self.save_settings();
    }

    /// Puts both slots back as the palette found them (upstream's `cancelThemePreview`).
    fn cancel_theme_preview(&mut self) {
        let Some(original) = self.palette.preview.take() else {
            return;
        };
        let settings = self.settings_unsaved();
        settings.theme = original.theme;
        settings.dark_theme = original.dark;
        self.apply_settings();
    }
}
