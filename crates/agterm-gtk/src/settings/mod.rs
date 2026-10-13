//! The Settings dialog (upstream's `SettingsView.swift` over `SettingsModel.swift`): upstream's six tabs as the pages
//! of one libadwaita preferences dialog, General, Appearance, Interface, Notifications, Agent Status and Key Mapping,
//! opened from the app menu over the frontmost window.
//!
//! Every control edit goes to the main loop as an [`agterm_core::settings_model::SettingsChange`], which the core
//! validates, writes to `settings.json` and applies; the settle that follows refreshes the open dialog from the
//! settings ([`sync`]), so a change from elsewhere (`theme.set`, `sidebar.flagged-layout`, `browser.links`) shows here
//! too. A refresh sets the widgets with their handlers muted, so it never posts an edit back.

mod agent_status;
mod appearance;
mod general;
mod interface;
mod key_mapping;
mod notifications;
mod rows;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use agterm_core::Core;
use agterm_core::keymap::KeymapDiagnostic;
use agterm_core::persist::AppSettings;
use agterm_core::settings_model::SettingsChange;

use crate::state::{self, AppState, Poster};

/// What the dialog's controls show: the settings and the few facts beside them.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub settings: AppSettings,
    /// The terminals render the dark side; picks which theme slot the theme pickers write.
    pub dark: bool,
    /// The resolved config directory holding `keymap.conf` and `hooks.conf`.
    pub config_dir: String,
    pub keymap_diagnostics: Vec<String>,
    pub hooks_diagnostics: Vec<String>,
    /// Why `live` restore cannot run on this host, when it cannot.
    pub live_unavailable: Option<String>,
}

/// What the pickers offer, read once when the dialog opens (upstream's `SettingsCatalog`, read at view construction).
pub struct Catalogs {
    /// The system's monospaced font families.
    pub fonts: Vec<String>,
    /// The bundled themes.
    pub themes: Rc<[String]>,
}

impl Snapshot {
    #[must_use]
    pub fn of(core: &Core, dark: bool) -> Self {
        Self {
            settings: core.settings().clone(),
            dark,
            config_dir: core.config_dir().to_string_lossy().into_owned(),
            keymap_diagnostics: core.keymap_diagnostics().iter().map(diagnostic_line).collect(),
            hooks_diagnostics: core.hooks_diagnostics().iter().map(diagnostic_line).collect(),
            live_unavailable: core.restore_launch().live_unavailable_reason.clone(),
        }
    }
}

/// A diagnostic as one line (upstream's `diagnosticLine`): `line N: message`, the bare message for a whole-file one.
#[must_use]
pub fn diagnostic_line(diagnostic: &KeymapDiagnostic) -> String {
    if diagnostic.line > 0 {
        format!("line {}: {}", diagnostic.line, diagnostic.message)
    } else {
        diagnostic.message.clone()
    }
}

/// Re-sets one control from a snapshot.
type Refresh = Box<dyn Fn(&Snapshot)>;

thread_local! {
    /// The open dialog, if any.
    static OPEN: RefCell<Option<Rc<Open>>> = const { RefCell::new(None) };
    /// Set while a refresh writes the widgets: their handlers post nothing then.
    static REFRESHING: Cell<bool> = const { Cell::new(false) };
}

/// The open dialog and how to refresh it.
struct Open {
    dialog: adw::PreferencesDialog,
    refreshers: Vec<Refresh>,
    last: RefCell<Option<Snapshot>>,
}

impl Open {
    fn refresh(&self, snapshot: Snapshot) {
        if self.last.borrow().as_ref() == Some(&snapshot) {
            return;
        }
        REFRESHING.with(|flag| flag.set(true));
        for refresh in &self.refreshers {
            refresh(&snapshot);
        }
        REFRESHING.with(|flag| flag.set(false));
        *self.last.borrow_mut() = Some(snapshot);
    }
}

/// Whether a widget handler is running for a refresh rather than for the user.
fn refreshing() -> bool {
    REFRESHING.with(Cell::get)
}

/// Sends one edit to the core; nothing while a refresh sets the widgets.
pub fn send(poster: &Poster, change: SettingsChange) {
    if refreshing() {
        return;
    }
    state::post(poster, move |state: &mut AppState| {
        state.core.change_settings(change);
        state.settle();
    });
}

/// Runs `task` on the main loop and settles; nothing while a refresh sets the widgets.
pub fn act(poster: &Poster, task: impl FnOnce(&mut Core) + Send + 'static) {
    if refreshing() {
        return;
    }
    state::post(poster, move |state: &mut AppState| {
        task(&mut state.core);
        state.settle();
    });
}

/// What the pages are built with: the poster their edits go through, and the refreshers they register.
pub struct Rows {
    poster: Poster,
    refreshers: Vec<Refresh>,
}

impl Rows {
    #[must_use]
    pub fn new(poster: Poster) -> Self {
        Self {
            poster,
            refreshers: Vec::new(),
        }
    }

    #[must_use]
    pub fn poster(&self) -> &Poster {
        &self.poster
    }

    /// Registers how a control follows the settings.
    pub fn refresh(&mut self, refresh: impl Fn(&Snapshot) + 'static) {
        self.refreshers.push(Box::new(refresh));
    }
}

/// The dialog's pages in upstream's tab order; their refreshers come back in `rows`.
#[must_use]
pub fn pages(rows: &mut Rows, catalogs: &Catalogs) -> [adw::PreferencesPage; 6] {
    [
        general::page(rows),
        appearance::page(rows, catalogs),
        interface::page(rows),
        notifications::page(rows),
        agent_status::page(rows),
        key_mapping::page(rows),
    ]
}

/// The dialog over [`pages`], opening on General.
#[must_use]
pub fn build(rows: &mut Rows, catalogs: &Catalogs) -> adw::PreferencesDialog {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Settings");
    dialog.set_search_enabled(true);
    for page in pages(rows, catalogs) {
        dialog.add(&page);
    }
    dialog.set_visible_page_name("general");
    dialog
}

/// Opens the Settings dialog over the frontmost window, or raises the open one (File ▸ Settings).
pub fn present(state: &AppState) {
    let Some(parent) = state.frontmost_gtk_window() else {
        return;
    };
    if let Some(open) = OPEN.with(|open| open.borrow().clone()) {
        open.dialog.present(Some(&parent));
        return;
    }
    let mut rows = Rows::new(state.poster().clone());
    let catalogs = Catalogs {
        fonts: monospace_families(parent.upcast_ref()),
        themes: state.core.theme_catalog().names().into(),
    };
    let dialog = build(&mut rows, &catalogs);
    let open = Rc::new(Open {
        dialog: dialog.clone(),
        refreshers: rows.refreshers,
        last: RefCell::new(None),
    });
    open.refresh(Snapshot::of(&state.core, crate::ghostty_app::is_dark()));
    OPEN.with(|slot| *slot.borrow_mut() = Some(open));
    dialog.connect_closed(|_| drop(OPEN.with(|slot| slot.borrow_mut().take())));
    dialog.present(Some(&parent));
}

/// Brings the open dialog in line with the settings; called after every settle.
pub fn sync(core: &Core) {
    let Some(open) = OPEN.with(|open| open.borrow().clone()) else {
        return;
    };
    open.refresh(Snapshot::of(core, crate::ghostty_app::is_dark()));
}

/// The monospaced font families the font picker offers, sorted case-insensitively (upstream's
/// `SettingsCatalog.monospacedFontFamilies`).
fn monospace_families(widget: &gtk::Widget) -> Vec<String> {
    let mut families: Vec<String> = widget
        .pango_context()
        .list_families()
        .iter()
        .filter(|family| family.is_monospace())
        .map(|family| family.name().to_string())
        .collect();
    families.sort_by_key(|name| name.to_lowercase());
    families.dedup();
    families
}

#[cfg(test)]
mod tests;
