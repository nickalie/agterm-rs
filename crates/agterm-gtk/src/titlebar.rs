//! The window's header bar (upstream's custom titlebar row, `WindowContentView+Titlebar.swift`, with its zoom and
//! dashboard variants): the sidebar toggle, the title's two lines ([`agterm_core::titlebar::compose`]), and the trailing
//! cluster of recent-sessions and attention popovers, scratch and split toggles, the dashboard, the quick terminal and
//! the custom-commands popover, each gated by its Settings ▸ Interface toggle, with a separator only between two
//! groups of two or more. A terminal zoom or an open dashboard strips the bar to its title and an exit button. The hidden
//! toolbar mode, upstream a bare drag strip under the macOS menu bar, keeps a minimal bar here: the window controls,
//! the drag surface and the primary menu (Linux has no global menu bar, so it is the menu's only mouse path), without
//! the title or the trailing cluster.
//!
//! In GTK idiom the bar is a libadwaita header bar, its buttons flat with symbolic icons and hover texts naming their
//! chords; the split and scratch glyphs, which GNOME's icon theme has no half-filled forms for, are traced with cairo.
//! Every button posts a [`TitlebarAction`] the core runs ([`agterm_core::Core::titlebar_action`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use agterm_core::Core;
use agterm_core::app_actions::ActionEffect;
use agterm_core::keymap::BuiltinAction;
use agterm_core::model::{AgentIndicator, AgentStatus};
use agterm_core::persist::settings::ToolbarMode;
use agterm_core::titlebar::{AttentionBell, SplitGlyph, Titlebar as Model, TitlebarAction, TitlebarComposition};
use gtk::{cairo, gdk};
use uuid::Uuid;

use crate::popovers::{CommandPopover, SessionPopover, SessionPopoverKind};
use crate::row_icon::{Glyph, RowIcon};
use crate::state::{self, AppState, Poster};

/// The CSS class tinting the bell in the blocked status color.
const BELL_BLOCKED_CLASS: &str = "agterm-bell-blocked";
/// The glyph slot of the traced pane glyphs.
const SLOT: i32 = 16;

/// What covers the window: the bar strips down under a zoom or the dashboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cover {
    None,
    /// A terminal zoom: the title and its exit.
    Zoom,
    /// The dashboard: its own title and its exit.
    Dashboard(String),
}

/// The hover text `base`, followed by `chord_label` in parentheses when there is one (upstream's `helpHint`).
#[must_use]
pub fn hint(base: &str, chord_label: Option<&str>) -> String {
    match chord_label {
        Some(label) => format!("{base} ({label})"),
        None => base.to_owned(),
    }
}

/// `action`'s chord as GTK labels accelerators (`Shift+Ctrl+S`), `None` when unbound.
fn chord_label(core: &Core, action: BuiltinAction) -> Option<String> {
    let accel = crate::app_menu::accelerator(core.keymap().equivalent(action)?)?;
    let (key, mods) = gtk::accelerator_parse(&accel)?;
    Some(gtk::accelerator_get_label(key, mods).to_string())
}

/// A pane glyph traced in the button's foreground color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneGlyph {
    Split(SplitGlyph),
    /// The scratch terminal: an inset fill while it shows.
    Scratch(bool),
}

/// A flat header button showing a traced [`PaneGlyph`].
struct GlyphButton {
    button: gtk::Button,
    area: gtk::DrawingArea,
    glyph: Rc<Cell<PaneGlyph>>,
}

impl GlyphButton {
    fn new(glyph: PaneGlyph, name: &str) -> Self {
        let area = gtk::DrawingArea::builder()
            .content_width(SLOT)
            .content_height(SLOT)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let shown = Rc::new(Cell::new(glyph));
        let drawn = Rc::clone(&shown);
        area.set_draw_func(move |area, cr, width, height| {
            cr.translate(f64::from(width - SLOT) / 2.0, f64::from(height - SLOT) / 2.0);
            if let Err(err) = draw_pane_glyph(cr, drawn.get(), &area.color()) {
                tracing::debug!(%err, "drawing a title-bar glyph failed");
            }
        });
        let button = gtk::Button::builder().child(&area).name(name).build();
        Self {
            button,
            area,
            glyph: shown,
        }
    }

    fn set_glyph(&self, glyph: PaneGlyph) {
        if self.glyph.replace(glyph) != glyph {
            self.area.queue_draw();
        }
    }
}

/// Traces `glyph` into the 16x16 slot at the origin: the two-pane frame of `rectangle.split.*` with its shown halves
/// filled, or the scratch's `rectangle` with an inset fill.
fn draw_pane_glyph(cr: &cairo::Context, glyph: PaneGlyph, rgba: &gdk::RGBA) -> Result<(), cairo::Error> {
    cr.set_source_rgba(
        f64::from(rgba.red()),
        f64::from(rgba.green()),
        f64::from(rgba.blue()),
        f64::from(rgba.alpha()),
    );
    cr.set_line_width(1.25);
    let (left, top, width, height) = (1.5, 3.0, 13.0, 10.0);
    cr.rectangle(left, top, width, height);
    cr.stroke()?;
    let (cx, cy) = (left + width / 2.0, top + height / 2.0);
    let inner = (left + 2.0, top + 2.0, width - 4.0, height - 4.0);
    match glyph {
        PaneGlyph::Scratch(active) => {
            if active {
                cr.rectangle(inner.0, inner.1, inner.2, inner.3);
                cr.fill()?;
            }
        }
        PaneGlyph::Split(split) => {
            let stacked = matches!(split, SplitGlyph::BothHorizontal | SplitGlyph::Top | SplitGlyph::Bottom);
            if stacked {
                cr.move_to(left, cy);
                cr.line_to(left + width, cy);
            } else {
                cr.move_to(cx, top);
                cr.line_to(cx, top + height);
            }
            cr.stroke()?;
            let gap = 1.5;
            let first_half = if stacked {
                (inner.0, inner.1, inner.2, cy - gap - inner.1)
            } else {
                (inner.0, inner.1, cx - gap - inner.0, inner.3)
            };
            let second_half = if stacked {
                (inner.0, cy + gap, inner.2, inner.1 + inner.3 - cy - gap)
            } else {
                (cx + gap, inner.1, inner.0 + inner.2 - cx - gap, inner.3)
            };
            let (first, second) = match split {
                SplitGlyph::None => (false, false),
                SplitGlyph::Both | SplitGlyph::BothHorizontal => (true, true),
                SplitGlyph::Left | SplitGlyph::Top => (true, false),
                SplitGlyph::Right | SplitGlyph::Bottom => (false, true),
            };
            for (fill, (x, y, w, h)) in [(first, first_half), (second, second_half)] {
                if fill {
                    cr.rectangle(x, y, w, h);
                    cr.fill()?;
                }
            }
        }
    }
    Ok(())
}

/// The title's two lines: the identity, the remote host and the compact tail on line one, the subtitle on line two.
struct Label {
    widget: gtk::Box,
    title: gtk::Label,
    host: gtk::Box,
    host_text: gtk::Label,
    tail: gtk::Label,
    subtitle: gtk::Label,
}

impl Label {
    fn new() -> Self {
        let title = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let host_text = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(30)
            .build();
        let host = gtk::Box::builder()
            .spacing(4)
            .css_classes(["dim-label"])
            .margin_start(6)
            .build();
        host.append(
            RowIcon::new(Glyph::Remote {
                split: false,
                disconnected: false,
            })
            .widget(),
        );
        host.append(&host_text);
        let tail = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        line.append(&title);
        line.append(&host);
        line.append(&tail);
        let subtitle = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .xalign(0.0)
            .css_classes(["caption", "dim-label"])
            .build();
        let widget = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .margin_start(6)
            .build();
        widget.append(&line);
        widget.append(&subtitle);
        Self {
            widget,
            title,
            host,
            host_text,
            tail,
            subtitle,
        }
    }

    fn show(&self, label: &TitlebarComposition) {
        set_text(&self.title, &label.title);
        self.host.set_visible(label.host.is_some());
        self.host_text.set_text(label.host.as_deref().unwrap_or_default());
        self.host.set_margin_start(if label.title.is_empty() { 0 } else { 6 });
        set_text(&self.tail, &label.tail);
        set_text(&self.subtitle, &label.subtitle);
    }
}

/// Sets `label`'s text, hiding it while empty.
fn set_text(label: &gtk::Label, text: &str) {
    if label.text() != text {
        label.set_text(text);
    }
    label.set_visible(!text.is_empty());
}

/// The header bar and what it shows.
pub struct Titlebar {
    window: Uuid,
    /// The bar itself, painted by [`crate::chrome`].
    header: adw::HeaderBar,
    sidebar_toggle: gtk::Button,
    label: Label,
    /// The trailing cluster, hidden under a cover.
    cluster: gtk::Box,
    recent: SessionPopover,
    attention: SessionPopover,
    jump_divider: gtk::Separator,
    scratch: GlyphButton,
    split: GlyphButton,
    panes_divider: gtk::Separator,
    dashboard: gtk::Button,
    quick: gtk::Button,
    custom: CommandPopover,
    exit_zoom: gtk::Button,
    exit_dashboard: gtk::Button,
    /// The bar last shown and the keymap generation its hover texts read, so an unchanged settle touches no widget.
    shown: RefCell<Option<(Model, Cover, bool, u64)>>,
}

impl Titlebar {
    /// The bar of window `window`, its primary menu showing `menu`, put on top of `toolbar`.
    #[must_use]
    pub fn new(window: Uuid, toolbar: &adw::ToolbarView, menu: &gtk::gio::MenuModel, poster: &Poster) -> Self {
        let flat = |icon: &str, tooltip: &str, name: &str| {
            gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .name(name)
                .build()
        };
        let sidebar_toggle = flat("sidebar-show-symbolic", "Toggle Sidebar", "sidebar-toggle-button");
        let recent = SessionPopover::new(SessionPopoverKind::Recent, window, poster);
        let attention = SessionPopover::new(SessionPopoverKind::Attention, window, poster);
        let scratch = GlyphButton::new(PaneGlyph::Scratch(false), "scratch-toggle");
        let split = GlyphButton::new(PaneGlyph::Split(SplitGlyph::None), "split-toggle");
        let dashboard = flat("view-grid-symbolic", "Dashboard", "dashboard-toggle-button");
        let quick = flat("utilities-terminal-symbolic", "Quick Terminal", "quick-terminal-toggle");
        let custom = CommandPopover::new(window, poster);
        let jump_divider = gtk::Separator::new(gtk::Orientation::Vertical);
        let panes_divider = gtk::Separator::new(gtk::Orientation::Vertical);
        for divider in [&jump_divider, &panes_divider] {
            divider.set_margin_top(10);
            divider.set_margin_bottom(10);
        }
        let cluster = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        cluster.append(recent.widget());
        cluster.append(attention.widget());
        cluster.append(&jump_divider);
        cluster.append(&scratch.button);
        cluster.append(&split.button);
        cluster.append(&panes_divider);
        cluster.append(&dashboard);
        cluster.append(&quick);
        cluster.append(custom.widget());
        let exit_zoom = flat("view-restore-symbolic", "Exit Terminal Zoom", "terminal-zoom-exit");
        let exit_dashboard = flat("window-close-symbolic", "Exit Dashboard", "dashboard-exit");
        let primary = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .menu_model(menu)
            .primary(true)
            .build();
        let label = Label::new();
        // the title starts at the leading edge after the toggle, as upstream's does, rather than centered
        let header = adw::HeaderBar::builder()
            .title_widget(&gtk::Box::new(gtk::Orientation::Horizontal, 0))
            .build();
        header.add_css_class(crate::chrome::BAR_CLASS);
        header.pack_start(&sidebar_toggle);
        header.pack_start(&label.widget);
        header.pack_end(&primary);
        header.pack_end(&exit_zoom);
        header.pack_end(&exit_dashboard);
        header.pack_end(&cluster);
        toolbar.add_top_bar(&header);
        install_css();
        let bar = Self {
            window,
            header,
            sidebar_toggle,
            label,
            cluster,
            recent,
            attention,
            jump_divider,
            scratch,
            split,
            panes_divider,
            dashboard,
            quick,
            custom,
            exit_zoom,
            exit_dashboard,
            shown: RefCell::new(None),
        };
        bar.connect(poster);
        bar
    }

    fn connect(&self, poster: &Poster) {
        let id = self.window;
        let act = |button: &gtk::Button, action: TitlebarAction| {
            let posting = poster.clone();
            button.connect_clicked(move |_| {
                state::post(&posting, move |state: &mut AppState| state.titlebar_action(id, action));
            });
        };
        act(&self.sidebar_toggle, TitlebarAction::ToggleSidebar);
        act(&self.scratch.button, TitlebarAction::ToggleScratch);
        act(&self.split.button, TitlebarAction::ToggleSplit);
        act(&self.dashboard, TitlebarAction::Dashboard);
        act(&self.quick, TitlebarAction::QuickTerminal);
        let posting = poster.clone();
        self.exit_zoom.connect_clicked(move |_| {
            state::post(&posting, move |state: &mut AppState| state.exit_zoom(id));
        });
        let posting = poster.clone();
        self.exit_dashboard.connect_clicked(move |_| {
            state::post(&posting, move |state: &mut AppState| state.exit_dashboard(id));
        });
    }

    /// Renders the window's bar: under `cover` stripped to the title and its exit; `dialog_up` while a dialog holds
    /// the window, which stands the exits down.
    pub fn sync(&self, core: &Core, cover: &Cover, dialog_up: bool) {
        let model = core.titlebar(self.window);
        self.recent.sync(
            core.recent_session_rows(self.window),
            model.recent.is_some_and(|b| b.enabled),
            core,
        );
        self.attention
            .sync(core.attention_rows(), model.attention.is_some_and(|b| b.enabled), core);
        self.custom.sync(core, model.custom_commands.is_some_and(|b| b.enabled));
        set_bell_color(core);
        let key = (model.clone(), cover.clone(), dialog_up, core.keymap_generation());
        if self.shown.borrow().as_ref() == Some(&key) {
            return;
        }
        self.exit_zoom.set_visible(*cover == Cover::Zoom);
        self.exit_dashboard.set_visible(matches!(cover, Cover::Dashboard(_)));
        self.exit_zoom.set_sensitive(!dialog_up);
        self.exit_dashboard.set_sensitive(!dialog_up);
        match cover {
            Cover::Dashboard(title) => self.label.show(&TitlebarComposition {
                title: title.clone(),
                ..TitlebarComposition::default()
            }),
            _ => self.label.show(&model.label),
        }
        let hidden = model.mode == ToolbarMode::Hidden;
        self.label.widget.set_visible(!hidden);
        if hidden {
            self.header.add_css_class(crate::chrome::BAR_HIDDEN_CLASS);
        } else {
            self.header.remove_css_class(crate::chrome::BAR_HIDDEN_CLASS);
        }
        let uncovered = *cover == Cover::None && !hidden;
        self.cluster.set_visible(uncovered);
        self.sidebar_toggle.set_visible(uncovered && model.sidebar_toggle);
        self.sync_buttons(core, &model);
        self.shown.replace(Some(key));
    }

    fn sync_buttons(&self, core: &Core, model: &Model) {
        let label = |action| chord_label(core, action);
        self.sidebar_toggle.set_tooltip_text(Some(&hint(
            "Toggle Sidebar",
            label(BuiltinAction::ToggleSidebar).as_deref(),
        )));
        self.recent
            .show_button(model.recent.map(|button| button.enabled), "Recent sessions (Ctrl+Tab)");
        match model.attention {
            Some(button) => {
                let base = if button.bell == AttentionBell::None {
                    "No sessions need attention"
                } else {
                    "Show sessions that need attention"
                };
                let tooltip = hint(base, label(BuiltinAction::ShowAttention).as_deref());
                self.attention.show_button(Some(button.enabled), &tooltip);
                self.attention.set_blocked(button.bell == AttentionBell::Blocked);
            }
            None => self.attention.show_button(None, ""),
        }
        self.jump_divider.set_visible(model.dividers.after_jump);
        self.panes_divider.set_visible(model.dividers.after_panes);
        self.scratch.button.set_visible(model.scratch.is_some());
        if let Some(scratch) = model.scratch {
            self.scratch.set_glyph(PaneGlyph::Scratch(scratch.active));
            self.scratch.button.set_sensitive(scratch.enabled);
            let base = if scratch.active {
                "Hide scratch terminal"
            } else {
                "Show scratch terminal"
            };
            self.scratch
                .button
                .set_tooltip_text(Some(&hint(base, label(BuiltinAction::ToggleScratch).as_deref())));
        }
        self.split.button.set_visible(model.split.is_some());
        if let Some(split) = model.split {
            self.split.set_glyph(PaneGlyph::Split(split.glyph));
            self.split.button.set_sensitive(split.enabled);
            self.split
                .button
                .set_tooltip_text(Some(&hint(split.hint, label(split.action).as_deref())));
            self.split
                .button
                .update_property(&[gtk::accessible::Property::Description(split.glyph.name())]);
        }
        let plain = |button: &gtk::Button, state: Option<agterm_core::titlebar::Button>, base, action| {
            button.set_visible(state.is_some());
            button.set_sensitive(state.is_some_and(|state| state.enabled));
            button.set_tooltip_text(Some(&hint(base, label(action).as_deref())));
        };
        plain(&self.dashboard, model.dashboard, "Dashboard", BuiltinAction::Dashboard);
        plain(
            &self.quick,
            model.quick_terminal,
            "Quick Terminal",
            BuiltinAction::QuickTerminal,
        );
        let tooltip = hint("Custom commands", label(BuiltinAction::CustomCommandPalette).as_deref());
        self.custom
            .show_button(model.custom_commands.map(|button| button.enabled), &tooltip);
    }
}

thread_local! {
    /// The provider tinting the blocked bell, and the color it was last given.
    static BELL_CSS: RefCell<Option<(gtk::CssProvider, String)>> = const { RefCell::new(None) };
}

/// The display-wide styles of the bar, installed once.
fn install_css() {
    BELL_CSS.with(|css| {
        if css.borrow().is_some() {
            return;
        }
        let Some(display) = gdk::Display::default() else {
            return;
        };
        let provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        css.replace(Some((provider, String::new())));
    });
}

/// Tints the blocked bell in the blocked status color the settings resolve to (upstream's `blockedStatusColor`).
fn set_bell_color(core: &Core) {
    let indicator = AgentIndicator {
        status: AgentStatus::Blocked,
        ..AgentIndicator::default()
    };
    let Some(look) = crate::status_glyph::resolve(&indicator, core.settings()) else {
        return;
    };
    let (red, green, blue) = look.rgb;
    let css = format!(
        ".{BELL_BLOCKED_CLASS} {{ color: rgb({:.0}, {:.0}, {:.0}); }}",
        red * 255.0,
        green * 255.0,
        blue * 255.0
    );
    BELL_CSS.with(|cell| {
        if let Some((provider, current)) = cell.borrow_mut().as_mut()
            && *current != css
        {
            provider.load_from_string(&css);
            current.clone_from(&css);
        }
    });
}

/// The class [`SessionPopover::set_blocked`] puts on the bell.
#[must_use]
pub fn bell_blocked_class() -> &'static str {
    BELL_BLOCKED_CLASS
}

impl AppState {
    /// A title-bar button or popover row of window `window` was used.
    pub fn titlebar_action(&mut self, window: Uuid, action: TitlebarAction) {
        if let ActionEffect::Frontend(left) = self.core.titlebar_action(window, action) {
            tracing::debug!(?left, "a title-bar action left work to the frontend");
        }
        self.settle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hover_texts_name_the_chord_when_bound() {
        assert_eq!(hint("Dashboard", Some("Shift+Ctrl+M")), "Dashboard (Shift+Ctrl+M)");
        assert_eq!(hint("Dashboard", None), "Dashboard");
    }

    #[test]
    fn pane_glyphs_trace_without_error() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, SLOT, SLOT).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let ink = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
        for glyph in [
            SplitGlyph::None,
            SplitGlyph::Both,
            SplitGlyph::BothHorizontal,
            SplitGlyph::Left,
            SplitGlyph::Right,
            SplitGlyph::Top,
            SplitGlyph::Bottom,
        ] {
            draw_pane_glyph(&cr, PaneGlyph::Split(glyph), &ink).unwrap();
        }
        draw_pane_glyph(&cr, PaneGlyph::Scratch(true), &ink).unwrap();
        draw_pane_glyph(&cr, PaneGlyph::Scratch(false), &ink).unwrap();
    }

    /// The filled area's alpha sum in the left and right halves of `glyph`.
    fn ink_by_half(glyph: SplitGlyph) -> (u64, u64) {
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, SLOT, SLOT).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            draw_pane_glyph(&cr, PaneGlyph::Split(glyph), &gdk::RGBA::new(1.0, 1.0, 1.0, 1.0)).unwrap();
        }
        let stride = usize::try_from(surface.stride()).unwrap();
        let data = surface.data().unwrap();
        let (mut left, mut right) = (0, 0);
        for y in 5..11 {
            for x in 3..14 {
                let alpha = u64::from(data[y * stride + x * 4 + 3]);
                if x < 8 {
                    left += alpha;
                } else if x > 8 {
                    right += alpha;
                }
            }
        }
        (left, right)
    }

    fn core(settings: agterm_core::persist::AppSettings) -> (Core, Uuid) {
        let library = agterm_core::model::WindowLibrary::seeded("/home/u");
        let window = library.active_window_id().unwrap();
        let core = Core::new(
            agterm_core::CoreConfig {
                app: agterm_proto::AppIdentity::from_recorded("0.1.0", None),
                capabilities: Vec::new(),
            },
            agterm_core::CoreState {
                library,
                settings,
                persistence: None,
            },
        );
        (core, window)
    }

    #[test]
    fn the_bar_follows_the_toolbar_mode_the_toggles_and_the_cover() {
        crate::gtk_test::on_gtk_thread(bar_follows_mode_and_cover);
    }

    fn bar_follows_mode_and_cover() {
        let (mut core, window) = core(agterm_core::persist::AppSettings {
            toolbar_mode: Some("normal".into()),
            attention_button_enabled: Some(true),
            ..Default::default()
        });
        let toolbar = adw::ToolbarView::new();
        let menu = gtk::gio::Menu::new();
        let bar = Titlebar::new(window, &toolbar, menu.upcast_ref(), &crate::state::detached_poster());
        bar.sync(&core, &Cover::None, false);
        assert!(toolbar.reveals_top_bars());
        assert!(bar.header.has_css_class(crate::chrome::BAR_CLASS));
        assert!(
            !bar.header.has_css_class(crate::chrome::BAR_HIDDEN_CLASS),
            "the hairline shows"
        );
        uncovered_shows_everything_shown(&bar);
        covers_strip_the_bar(&bar, &core);
        core.update_settings(|settings| {
            settings.toolbar_mode = Some("hidden".into());
            settings.hidden_interface_elements = Some(vec!["recentSessions".into()]);
        });
        bar.sync(&core, &Cover::None, false);
        assert!(toolbar.reveals_top_bars(), "hidden mode keeps a minimal bar");
        assert!(
            bar.header.has_css_class(crate::chrome::BAR_HIDDEN_CLASS),
            "no hairline (upstream #368)"
        );
        assert!(
            primary_menu(toolbar.upcast_ref()).is_some_and(|menu| menu.is_visible()),
            "the app menu's only mouse path stays"
        );
        assert!(!bar.label.widget.is_visible(), "no title");
        assert!(
            !bar.cluster.is_visible() && !bar.sidebar_toggle.is_visible(),
            "no buttons"
        );
        core.update_settings(|settings| settings.toolbar_mode = Some("normal".into()));
        bar.sync(&core, &Cover::None, false);
        assert!(bar.label.widget.is_visible() && bar.cluster.is_visible());
        assert!(!bar.header.has_css_class(crate::chrome::BAR_HIDDEN_CLASS));
        assert!(!bar.recent.widget().is_visible());
        assert!(!bar.jump_divider.is_visible(), "one jump button left");
    }

    /// The header's primary menu button, found among `widget`'s descendants.
    fn primary_menu(widget: &gtk::Widget) -> Option<gtk::MenuButton> {
        if let Some(button) = widget.downcast_ref::<gtk::MenuButton>()
            && button.is_primary()
        {
            return Some(button.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = primary_menu(&current) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }

    fn uncovered_shows_everything_shown(bar: &Titlebar) {
        assert!(bar.cluster.is_visible() && bar.sidebar_toggle.is_visible());
        assert_eq!(bar.label.title.text(), "u");
        assert_eq!(bar.label.subtitle.text(), "/home/u", "normal mode's second line");
        assert!(bar.attention.widget().is_visible() && !bar.attention.widget().is_sensitive());
        assert!(!bar.custom.widget().is_visible(), "hidden by default");
        assert!(bar.jump_divider.is_visible() && bar.panes_divider.is_visible());
        assert!(!bar.exit_zoom.is_visible() && !bar.exit_dashboard.is_visible());
    }

    fn covers_strip_the_bar(bar: &Titlebar, core: &Core) {
        bar.sync(core, &Cover::Zoom, false);
        assert!(!bar.cluster.is_visible() && !bar.sidebar_toggle.is_visible());
        assert!(bar.exit_zoom.is_visible());
        bar.sync(core, &Cover::Dashboard("Dashboard".into()), true);
        assert!(bar.exit_dashboard.is_visible() && !bar.exit_dashboard.is_sensitive());
        assert_eq!(bar.label.title.text(), "Dashboard");
        assert!(!bar.label.subtitle.is_visible());
    }

    #[test]
    fn a_hidden_split_fills_only_the_pane_on_screen() {
        let (left, right) = ink_by_half(SplitGlyph::Left);
        assert!(left > right * 4, "{left} vs {right}");
        let (left, right) = ink_by_half(SplitGlyph::Right);
        assert!(right > left * 4, "{left} vs {right}");
        let (left, right) = ink_by_half(SplitGlyph::Both);
        assert!(left > 0 && right > 0);
    }
}
