//! The Settings popup (upstream's `SettingsView.swift` over `SettingsModel.swift`; ported from the GTK build's
//! `settings/`): upstream's six tabs in its order, General, Appearance, Interface, Notifications, Agent Status and Key
//! Mapping, opening on General. Each tab is a page module ([`general`], [`appearance`], [`interface`],
//! [`notifications`], [`agent_status`], [`key_mapping`]) that describes its sections and controls from a [`Snapshot`]
//! of what the core holds; [`form`] draws them.
//!
//! Upstream's Settings scene is a window of its own; here, at the user's request, it is a modal popup inside the agterm
//! window it was opened from (a separate window drifts off to a tiling compositor's next column): the window
//! (`crate::window`) draws it centered over a dimmed layer that takes every click, sized like upstream's 540x680 window
//! and shrinking to fit a smaller one ([`panel_size`]). It holds the window's keyboard while it is up (the key monitor
//! leaves its [`KEY_CONTEXT`] alone, as it does an alert's); Escape, its close button and a click outside it close it,
//! and the keyboard goes back to the window's terminal. There is one at a time in the app, as there is one Settings
//! window upstream: asking for it again while it is up brings its window to the front with the popup holding the
//! keyboard; a new one opens over the window whose app menu asked (the frontmost one when that window is no longer
//! on screen). It goes with its window.
//!
//! The core owns every setting: a control's edit is one [`SettingsChange`] (or a core call, the two Reloads) posted to
//! the main loop through [`Edits`], which the core validates, writes to `settings.json` and applies. The windows' sync
//! after every task hands the open popup a fresh snapshot ([`SettingsView::sync`]), so a control always shows what
//! the core holds after a write, and a change made elsewhere (`theme.set`, `sidebar.flagged-layout`,
//! `browser.links`, a config reload, a desktop light/dark flip) shows in the open popup too. The controls that keep
//! state of their own (the sliders, the color wells) are set from the snapshot whenever it changes and after every
//! edit, so an edit the core refused or normalised snaps back to the stored value.
//!
//! The popup paints in the chrome's colors (`crate::chrome`), like the app's other surfaces; its text sizes are fixed,
//! as upstream's grouped form is (the Interface font size is the palettes' and the switcher's, not Settings').

mod agent_status;
mod appearance;
mod draw;
pub mod form;
mod general;
mod interface;
mod key_mapping;
mod notifications;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;

use agterm_core::Core;
use agterm_core::keymap::KeymapDiagnostic;
use agterm_core::model::AgentStatus;
use agterm_core::persist::AppSettings;
use agterm_core::settings_model::SettingsChange;
use gpui_kit::assets::IconName;
use gpui_kit::component::color_picker::{ColorPickerEvent, ColorPickerState};
use gpui_kit::component::slider::{SliderEvent, SliderState};
use gpui_kit::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Hsla, IntoElement,
    Pixels, Render, Rgba, ScrollHandle, Size, Subscription, WeakEntity, Window, px, size,
};

pub use self::form::{Action, Control, Item, Picker, Run, Section, SliderKey};
use crate::state::{self, AppState, Poster};

/// The popup's size (upstream's Settings window, `.frame(width: 540, height: 680)`).
pub const PANEL_SIZE: (f32, f32) = (540.0, 680.0);
/// The clearance the popup keeps from its window's edges (the palettes' `InterfaceMetrics.panelMargin`).
pub const PANEL_MARGIN: f32 = 16.0;
/// The popup's title.
pub const TITLE: &str = "Settings";
/// The key context the popup holds the keyboard in, which the key monitor leaves alone.
pub const KEY_CONTEXT: &str = "AgtermSettings";

/// The popup's size in a window whose content is `viewport`: upstream's window size, shrunk to leave the margin on
/// every side of a smaller window.
#[must_use]
pub fn panel_size(viewport: Size<Pixels>) -> Size<Pixels> {
    let (width, height) = PANEL_SIZE;
    let fit = |ideal: f32, room: Pixels| px(ideal.min(f32::from(room) - 2.0 * PANEL_MARGIN).max(0.0));
    size(fit(width, viewport.width), fit(height, viewport.height))
}

/// One tab, in upstream's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    General,
    Appearance,
    Interface,
    Notifications,
    AgentStatus,
    KeyMapping,
}

impl Tab {
    /// Upstream's tab order.
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Appearance,
        Self::Interface,
        Self::Notifications,
        Self::AgentStatus,
        Self::KeyMapping,
    ];

    /// The tab's label.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Interface => "Interface",
            Self::Notifications => "Notifications",
            Self::AgentStatus => "Agent Status",
            Self::KeyMapping => "Key Mapping",
        }
    }

    /// The page's name (the GTK build's page names), which its tab's element id carries: `settings-tab-<name>`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Interface => "interface",
            Self::Notifications => "notifications",
            Self::AgentStatus => "agent-status",
            Self::KeyMapping => "key-mapping",
        }
    }

    /// The tab's glyph, Lucide's closest to upstream's SF Symbols (`gearshape`, `paintbrush`, `macwindow`, `bell`,
    /// `smallcircle.filled.circle`, `keyboard`).
    #[must_use]
    pub fn glyph(self) -> IconName {
        match self {
            Self::General => IconName::Settings,
            Self::Appearance => IconName::Paintbrush,
            Self::Interface => IconName::AppWindowMac,
            Self::Notifications => IconName::Bell,
            Self::AgentStatus => IconName::CircleDot,
            Self::KeyMapping => IconName::Keyboard,
        }
    }

    /// The page's sections over `snapshot`.
    #[must_use]
    pub fn sections(self, snapshot: &Snapshot, catalogs: &Catalogs) -> Vec<Section> {
        match self {
            Self::General => general::sections(snapshot),
            Self::Appearance => appearance::sections(snapshot, catalogs),
            Self::Interface => interface::sections(snapshot),
            Self::Notifications => notifications::sections(snapshot),
            Self::AgentStatus => agent_status::sections(snapshot),
            Self::KeyMapping => key_mapping::sections(snapshot),
        }
    }
}

/// The glyphs the tabs and the controls draw from Lucide's catalog beyond gpui-kit's own component icons, served with
/// the app's assets (`crate::titlebar::Icons`).
pub const GLYPHS: [IconName; 4] = [
    IconName::Paintbrush,
    IconName::AppWindowMac,
    IconName::CircleDot,
    IconName::Keyboard,
];

gpui_kit::assets::icon_assets!(pub(crate) SettingsIcons, [Paintbrush, AppWindowMac, CircleDot, Keyboard]);

/// What the popup's controls show: the settings and the few facts beside them, read from the core.
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// The active session's focused directory, where a directory chooser starts when no directory is set yet
    /// (upstream's `DirectoryPanelDefaults.url(paths:…, activeSessionCwd)`).
    pub session_cwd: Option<String>,
}

impl Snapshot {
    /// What `core` holds now, with the terminals on the `dark` side.
    #[must_use]
    pub fn of(core: &Core, dark: bool) -> Self {
        let library = core.library();
        let session_cwd = library
            .active_window_id()
            .and_then(|window| library.store(window))
            .and_then(agterm_core::model::AppStore::active_session)
            .map(|session| session.focused_cwd().to_owned());
        Self {
            settings: core.settings().clone(),
            dark,
            config_dir: core.config_dir().to_string_lossy().into_owned(),
            keymap_diagnostics: core.keymap_diagnostics().iter().map(diagnostic_line).collect(),
            hooks_diagnostics: core.hooks_diagnostics().iter().map(diagnostic_line).collect(),
            live_unavailable: core.restore_launch().live_unavailable_reason.clone(),
            session_cwd,
        }
    }

    /// Where a directory chooser starts: the first of `preferred` and the active session's directory that is a
    /// directory here.
    #[must_use]
    pub fn chooser_start(&self, preferred: Option<&str>) -> Option<std::path::PathBuf> {
        [preferred, self.session_cwd.as_deref()]
            .into_iter()
            .flatten()
            .map(std::path::PathBuf::from)
            .find(|path| path.is_dir())
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

/// What the pickers offer (upstream's `SettingsCatalog`, read at view construction).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalogs {
    /// The system's monospaced font families, sorted case-insensitively.
    pub fonts: Rc<[String]>,
    /// The bundled themes.
    pub themes: Rc<[String]>,
}

/// The system's monospaced font families, sorted case-insensitively without duplicates (upstream's
/// `SettingsCatalog.monospacedFontFamilies`), read once per run: the font database is loaded off the main thread.
#[must_use]
pub fn monospace_families() -> Vec<String> {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();
    FAMILIES
        .get_or_init(|| {
            let mut db = cosmic_text::fontdb::Database::new();
            db.load_system_fonts();
            let mut families: Vec<String> = db
                .faces()
                .filter(|face| face.monospaced)
                .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
                .collect();
            families.sort_by_key(|name| name.to_lowercase());
            families.dedup();
            families
        })
        .clone()
}

/// Where the controls' edits go: the main loop, and the popup's note that an edit is on its way (the next sync then
/// sets every stateful control from the snapshot, even an unchanged one).
#[derive(Clone)]
pub struct Edits {
    poster: Poster,
    stale: Rc<Cell<bool>>,
}

impl Edits {
    #[must_use]
    pub fn new(poster: Poster) -> Self {
        Self {
            poster,
            stale: Rc::default(),
        }
    }

    /// Sends one edit to the core, then settles.
    pub fn send(&self, change: SettingsChange) {
        self.stale.set(true);
        state::post(&self.poster, move |state: &mut AppState| {
            state.core.change_settings(change);
            state.settle();
        });
    }

    /// Runs `task` on the core (a Reload), then settles.
    pub fn act(&self, task: fn(&mut Core)) {
        self.stale.set(true);
        state::post(&self.poster, move |state: &mut AppState| {
            task(&mut state.core);
            state.settle();
        });
    }

    /// Runs what a button does: an edit, a core call, or a directory chooser over `window` whose pick is an edit.
    pub fn run(&self, run: &Run, window: &Window, cx: &mut App) {
        match run {
            Run::Change(change) => self.send(change.clone()),
            Run::Core(task) => self.act(*task),
            Run::Choose { request, change } => {
                let picked = crate::folder_chooser::choose(window.window_handle(), request.clone(), cx);
                let (edits, change) = (self.clone(), *change);
                cx.spawn(async move |_| {
                    if let Some(path) = picked.await {
                        edits.send(change(path.to_string_lossy().into_owned()));
                    }
                })
                .detach();
            }
        }
    }

    /// Whether an edit went out since the last call.
    fn take_stale(&self) -> bool {
        self.stale.replace(false)
    }
}

/// The statuses that draw a glyph, in upstream's row order.
pub const GLYPH_STATUSES: [AgentStatus; 3] = [AgentStatus::Active, AgentStatus::Blocked, AgentStatus::Completed];

/// The popup's view: the tabs and the shown page over the last snapshot.
pub struct SettingsView {
    focus: FocusHandle,
    edits: Edits,
    snapshot: Snapshot,
    catalogs: Catalogs,
    tab: Tab,
    sliders: Vec<(SliderKey, Entity<SliderState>)>,
    /// The glyph rows' color wells, by [`GLYPH_STATUSES`].
    wells: Vec<(AgentStatus, Entity<ColorPickerState>)>,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// The view over `snapshot`, opening on General; its edits go to `poster`.
    pub fn new(
        poster: Poster,
        snapshot: Snapshot,
        catalogs: Catalogs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let sliders = SliderKey::ALL
            .into_iter()
            .map(|key| {
                let (min, max) = key.bounds();
                let state = cx.new(|_| {
                    SliderState::new()
                        .min(narrow(min))
                        .max(narrow(max))
                        .step(1.0)
                        .default_value(narrow(key.value(&snapshot.settings)))
                });
                subscriptions.push(cx.subscribe(&state, move |view: &mut Self, _, event: &SliderEvent, _| {
                    if let SliderEvent::Change(value) = event {
                        view.edits.send(key.change(f64::from(value.end())));
                    }
                }));
                (key, state)
            })
            .collect();
        let wells = GLYPH_STATUSES
            .into_iter()
            .map(|status| {
                let rgb = agent_status::glyph_rgb(&snapshot.settings, status);
                let state = cx.new(|cx| ColorPickerState::new(window, cx).default_value(hsla(rgb)));
                subscriptions.push(
                    cx.subscribe(&state, move |view: &mut Self, _, event: &ColorPickerEvent, _| {
                        let ColorPickerEvent::Change(Some(color)) = event else {
                            return;
                        };
                        view.edits.send(agent_status::color_change(status, hex_of(*color)));
                    }),
                );
                (status, state)
            })
            .collect();
        Self {
            focus: cx.focus_handle(),
            edits: Edits::new(poster),
            snapshot,
            catalogs,
            tab: Tab::General,
            sliders,
            wells,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    /// The popup over what the core held (`opening`), its font picker filled once the families are read.
    pub fn open(opening: Opening, window: &mut Window, cx: &mut App) -> Entity<Self> {
        let Opening {
            snapshot,
            themes,
            poster,
        } = opening;
        let catalogs = Catalogs {
            fonts: Rc::default(),
            themes,
        };
        let view = cx.new(|cx| Self::new(poster, snapshot, catalogs, window, cx));
        load_fonts(view.downgrade(), cx);
        view
    }

    /// Closes the popup: whoever shows it takes it down ([`DismissEvent`]).
    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    /// Whether the popup, or a control in it, holds `window`'s keyboard.
    #[must_use]
    pub fn holds_keyboard(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The snapshot the controls show.
    #[must_use]
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The tab shown.
    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// The shown page's sections.
    #[must_use]
    pub fn sections(&self) -> Vec<Section> {
        self.tab.sections(&self.snapshot, &self.catalogs)
    }

    /// Where the controls' edits go.
    #[must_use]
    pub fn edits(&self) -> &Edits {
        &self.edits
    }

    /// The slider state behind `key`.
    #[must_use]
    pub fn slider(&self, key: SliderKey) -> Option<&Entity<SliderState>> {
        self.sliders
            .iter()
            .find(|(shown, _)| *shown == key)
            .map(|(_, state)| state)
    }

    /// The color well of `status`'s glyph row.
    #[must_use]
    pub fn well(&self, status: AgentStatus) -> Option<&Entity<ColorPickerState>> {
        self.wells
            .iter()
            .find(|(shown, _)| *shown == status)
            .map(|(_, state)| state)
    }

    /// Shows `tab`, scrolled to its top.
    pub fn select(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.scroll.set_offset(gpui_kit::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// The font families arrived (read off the main thread).
    pub fn set_fonts(&mut self, fonts: Vec<String>, cx: &mut Context<Self>) {
        let fonts: Rc<[String]> = fonts.into();
        if self.catalogs.fonts != fonts {
            self.catalogs.fonts = fonts;
            cx.notify();
        }
    }

    /// Brings the controls in line with what the core holds: every one when the snapshot changed or an edit went out.
    pub fn sync(&mut self, snapshot: Snapshot, window: &mut Window, cx: &mut Context<Self>) {
        let stale = self.edits.take_stale();
        if !stale && snapshot == self.snapshot {
            return;
        }
        self.snapshot = snapshot;
        let settings = &self.snapshot.settings;
        for (key, state) in &self.sliders {
            let value = narrow(key.value(settings));
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
        // a well is rewritten only when the core holds another color than it committed (a change made elsewhere, an
        // edit the core refused): rewriting it with its own color would round its sliders through `#rrggbb` mid-drag
        // (a grey loses its hue) and drop a preview or hex the user is still typing
        for (status, state) in &self.wells {
            let color = hsla(agent_status::glyph_rgb(settings, *status));
            if state.read(cx).value().map(hex_of) != Some(hex_of(color)) {
                state.update(cx, |state, cx| state.set_value(color, window, cx));
            }
        }
        cx.notify();
    }
}

impl EventEmitter<DismissEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        draw::panel(self, cx)
    }
}

/// A slider position as the slider state takes it.
#[expect(clippy::cast_possible_truncation, reason = "a slider position within 0...100")]
fn narrow(value: f64) -> f32 {
    value as f32
}

/// A status tint as GPUI's color.
fn hsla(rgb: crate::status_glyph::Rgb) -> Hsla {
    crate::status_glyph::hsla(rgb)
}

/// A picked color as the settings store it: `#rrggbb`, lower-case.
#[must_use]
pub fn hex_of(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let channel = |value: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0...255 first"
        )]
        let byte = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
        byte
    };
    format!("#{:02x}{:02x}{:02x}", channel(rgba.r), channel(rgba.g), channel(rgba.b))
}

/// What a popup opens over, read from the state before it opens.
pub struct Opening {
    pub snapshot: Snapshot,
    /// The bundled themes, read from their directory now.
    pub themes: Rc<[String]>,
    pub poster: Poster,
}

impl Opening {
    #[must_use]
    pub fn of(state: &AppState) -> Self {
        Self {
            snapshot: Snapshot::of(&state.core, state.applied_dark()),
            themes: state.core.theme_catalog().names().to_vec().into(),
            poster: state.poster().clone(),
        }
    }
}

/// Reads the font families off the main thread and hands them to the popup's font picker.
fn load_fonts(view: WeakEntity<SettingsView>, cx: &mut App) {
    let families = cx.background_executor().spawn(async { monospace_families() });
    cx.spawn(async move |cx| {
        let fonts = families.await;
        if view.update(cx, |view, cx| view.set_fonts(fonts, cx)).is_err() {
            tracing::debug!("the Settings popup closed before its fonts were read");
        }
    })
    .detach();
}

#[cfg(test)]
mod tests;
