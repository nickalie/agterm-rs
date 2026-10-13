//! A page's panel: the app-drawn identity strip the page cannot cover, with the navigation buttons of `--navigation`,
//! over the web view, and the failure shown over a page that did not load (upstream's `HtmlOverlayView`). A chromeless
//! page has no strip.

use agterm_core::html::{HtmlLoadState, HtmlNavigation, HtmlOverlay, HtmlOverlayTheme, HtmlReloadTarget};
use agterm_core::model::is_valid_color_hex;
use gtk::prelude::*;
use gtk::{gdk, pango};
use uuid::Uuid;
use webkit6::WebView;

use super::files;
use super::policy::{self, StripItem};
use super::registry;
use crate::state::{self, AppState, Poster};

/// The theme background when the terminal's is unknown (upstream's sRGB 0.157, 0.173, 0.204).
const FALLBACK_BACKGROUND: &str = "#282c34";
const FALLBACK_FOREGROUND: &str = "#ffffff";
/// The strip's height (upstream's 28 points).
const STRIP_HEIGHT: i32 = 28;
/// How many characters of the identity hold their place before the page's title gives up width.
const IDENTITY_CHARS: usize = 24;

/// The colors a page is drawn in: the terminal's, the overlay's own background first.
pub(super) struct PanelColors {
    /// The terminal's background, which the strip sits on.
    terminal: String,
    /// The page's background: the overlay's own, else the terminal's.
    background: String,
    foreground: String,
    dark: bool,
}

impl PanelColors {
    /// The colors under the overlay's `background` (`#rrggbb`), the theme's when it has none or an invalid one.
    pub(super) fn new(background: Option<&str>) -> Self {
        let hex = |color: agterm_ghostty::Color| format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);
        let terminal = crate::ghostty_app::background_color().map_or_else(|| FALLBACK_BACKGROUND.to_owned(), hex);
        let foreground = crate::ghostty_app::foreground_color().map_or_else(|| FALLBACK_FOREGROUND.to_owned(), hex);
        let background = background.filter(|color| is_valid_color_hex(color)).map_or_else(
            || terminal.clone(),
            |color| format!("#{}", color.trim_start_matches('#')),
        );
        let dark = rgb(&background).is_none_or(|(r, g, b)| agterm_core::html::theme::is_dark(r, g, b));
        Self {
            terminal,
            background,
            foreground,
            dark,
        }
    }

    /// The terminal theme as the page sees it (upstream's `HtmlOverlayRegistry.theme`). The palette is not read from
    /// libghostty yet, so a page gets no `--agterm-color-N`.
    pub(super) fn theme(&self) -> HtmlOverlayTheme {
        HtmlOverlayTheme::new(&self.background, &self.foreground, self.dark, &[])
    }
}

/// The components of a `#rrggbb` color, each 0...1.
fn rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let digits = hex.strip_prefix('#')?;
    let channel = |at: usize| {
        digits
            .get(at..at + 2)
            .and_then(|pair| u8::from_str_radix(pair, 16).ok())
            .map(|value| f64::from(value) / 255.0)
    };
    Some((channel(0)?, channel(2)?, channel(4)?))
}

/// The strip's widgets that follow the model.
struct Strip {
    back: Option<gtk::Button>,
    forward: Option<gtk::Button>,
    identity: gtk::Label,
    dot: gtk::Label,
    title: gtk::Label,
}

/// One page's panel.
pub(super) struct Panel {
    root: gtk::Box,
    strip: Option<Strip>,
    failure: gtk::Box,
    error: gtk::Label,
    provider: gtk::CssProvider,
}

impl Panel {
    /// The panel of `overlay` around `web_view`, drawn in `colors`; its buttons act on the page by id.
    pub(super) fn new(overlay: &HtmlOverlay, colors: &PanelColors, web_view: &WebView, poster: &Poster) -> Self {
        let class = format!("agterm-page-{}", overlay.id.simple());
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&stylesheet(&class, colors));
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class(&class);
        root.add_css_class("agterm-page");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let strip = policy::strip_items(overlay).map(|items| {
            let (bar, strip) = build_strip(overlay.id, &items, poster);
            root.append(&bar);
            let separator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            separator.add_css_class("agterm-page-separator");
            separator.set_height_request(1);
            root.append(&separator);
            strip
        });
        let (failure, error) = failure_view();
        let content = gtk::Overlay::new();
        content.set_child(Some(web_view));
        content.add_overlay(&failure);
        content.set_hexpand(true);
        content.set_vexpand(true);
        root.append(&content);
        files::refuse_file_drops(&root);
        let panel = Self {
            root,
            strip,
            failure,
            error,
            provider,
        };
        panel.update(overlay);
        panel
    }

    pub(super) fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    /// Shows the model's value: history buttons, the identity and title, the failure.
    pub(super) fn update(&self, overlay: &HtmlOverlay) {
        if let Some(strip) = &self.strip {
            let current = overlay.current.as_ref();
            if let Some(back) = &strip.back {
                back.set_sensitive(current.is_some_and(|current| current.can_go_back));
            }
            if let Some(forward) = &strip.forward {
                forward.set_sensitive(current.is_some_and(|current| current.can_go_forward));
            }
            let identity = overlay.identity();
            let chars = identity.chars().count().min(IDENTITY_CHARS);
            strip.identity.set_width_chars(i32::try_from(chars).unwrap_or(0));
            strip.identity.set_label(&identity);
            strip
                .identity
                .set_tooltip_text(Some(&policy::identity_tooltip(overlay)));
            let title = current.and_then(|current| current.title.as_deref());
            strip.title.set_label(title.unwrap_or_default());
            strip.title.set_tooltip_text(title);
            strip.title.set_visible(title.is_some());
            strip.dot.set_visible(title.is_some());
        }
        self.failure.set_visible(overlay.load_state == HtmlLoadState::Failed);
        self.error.set_label(overlay.load_error.as_deref().unwrap_or_default());
        self.error.set_visible(overlay.load_error.is_some());
    }

    /// Takes the panel out of its parent and its style off the display: the page is gone.
    pub(super) fn release(&self) {
        crate::overlay::unparent(self.root.upcast_ref());
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_remove_provider_for_display(&display, &self.provider);
        }
    }
}

/// The page's style: its background behind the transparent web view and the failure, the strip on the terminal's
/// background with a faint wash of the text color, the title and separator dimmed (upstream's opacities).
fn stylesheet(class: &str, colors: &PanelColors) -> String {
    let PanelColors {
        terminal,
        background,
        foreground,
        ..
    } = colors;
    format!(
        ".{class} {{ background-color: {background}; color: {foreground}; }}
.{class} .agterm-page-strip {{ background-image: linear-gradient(alpha({foreground}, 0.06), alpha({foreground}, 0.06));
  background-color: {terminal}; font-size: 12px; padding: 0 10px; }}
.{class} .agterm-page-strip button {{ color: {foreground}; min-width: 22px; min-height: 20px; padding: 0;
  margin: 0; }}
.{class} .agterm-page-strip button:disabled {{ color: alpha({foreground}, 0.3); }}
.{class} .agterm-page-dot {{ color: alpha({foreground}, 0.4); }}
.{class} .agterm-page-title {{ color: alpha({foreground}, 0.55); }}
.{class} .agterm-page-separator {{ background-color: alpha({foreground}, 0.1); }}
.{class} .agterm-page-failure {{ background-color: {background}; color: {foreground}; padding: 24px; }}
.{class} .agterm-page-error {{ font-size: 12px; }}
"
    )
}

/// The strip and the widgets that follow the model, its buttons acting on page `id`.
fn build_strip(id: Uuid, items: &[StripItem], poster: &Poster) -> (gtk::Box, Strip) {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    bar.add_css_class("agterm-page-strip");
    bar.set_height_request(STRIP_HEIGHT);
    let identity = gtk::Label::new(None);
    identity.set_ellipsize(pango::EllipsizeMode::Middle);
    identity.set_single_line_mode(true);
    let dot = gtk::Label::new(Some("·"));
    dot.add_css_class("agterm-page-dot");
    let title = gtk::Label::new(None);
    title.add_css_class("agterm-page-title");
    title.set_ellipsize(pango::EllipsizeMode::End);
    title.set_single_line_mode(true);
    let mut strip = Strip {
        back: None,
        forward: None,
        identity,
        dot,
        title,
    };
    for item in items {
        if *item == StripItem::Identity {
            let labels = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            labels.set_hexpand(true);
            labels.set_halign(gtk::Align::Center);
            labels.append(&strip.identity);
            labels.append(&strip.dot);
            labels.append(&strip.title);
            bar.append(&labels);
            continue;
        }
        let button = strip_button(id, *item, poster);
        bar.append(&button);
        match item {
            StripItem::Back => strip.back = Some(button),
            StripItem::Forward => strip.forward = Some(button),
            _ => {}
        }
    }
    (bar, strip)
}

/// One of the strip's flat icon buttons; it never takes the keyboard from the page.
fn strip_button(id: Uuid, item: StripItem, poster: &Poster) -> gtk::Button {
    let (icon, tooltip) = match item {
        StripItem::Back => ("go-previous-symbolic", "Back"),
        StripItem::Forward => ("go-next-symbolic", "Forward"),
        StripItem::Reload => ("view-refresh-symbolic", "Reload"),
        StripItem::Browser => ("web-browser-symbolic", "Open in Browser"),
        StripItem::Files => ("folder-open-symbolic", "Show in Files"),
        StripItem::CopyLink => ("edit-copy-symbolic", "Copy Link"),
        StripItem::Close | StripItem::Identity => ("window-close-symbolic", "Close"),
    };
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button.set_focus_on_click(false);
    button.set_focusable(false);
    button.set_valign(gtk::Align::Center);
    let poster = poster.clone();
    button.connect_clicked(move |_| act(id, item, &poster));
    button
}

/// What a strip button does: history and hand-offs go to the page's web view, reload and close to the model.
fn act(id: Uuid, item: StripItem, poster: &Poster) {
    let navigation = match item {
        StripItem::Back => HtmlNavigation::Back,
        StripItem::Forward => HtmlNavigation::Forward,
        StripItem::Browser => HtmlNavigation::Browser,
        StripItem::Files => HtmlNavigation::Finder,
        StripItem::CopyLink => return registry::copy_link(id),
        StripItem::Reload => {
            return state::post(poster, move |state: &mut AppState| {
                if state.core.reload_html_overlay(id, HtmlReloadTarget::Current) {
                    state.settle();
                }
            });
        }
        StripItem::Close | StripItem::Identity => {
            return state::post(poster, move |state: &mut AppState| {
                if state.core.close_html_overlay(id) {
                    state.settle();
                }
            });
        }
    };
    if let Err(error) = registry::navigate(id, navigation) {
        tracing::debug!(%error, "page navigation refused");
    }
}

/// The failure shown over the page so a failed load never reads as a blank one; a reload replaces it with the
/// loading state.
fn failure_view() -> (gtk::Box, gtk::Label) {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.set_halign(gtk::Align::Center);
    column.set_valign(gtk::Align::Center);
    column.set_vexpand(true);
    let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
    icon.set_pixel_size(24);
    column.append(&icon);
    let heading = gtk::Label::new(Some("The page could not be loaded"));
    heading.add_css_class("heading");
    column.append(&heading);
    let error = gtk::Label::new(None);
    error.add_css_class("agterm-page-error");
    error.set_selectable(true);
    error.set_wrap(true);
    error.set_justify(gtk::Justification::Center);
    column.append(&error);
    let failure = gtk::Box::new(gtk::Orientation::Vertical, 0);
    failure.add_css_class("agterm-page-failure");
    failure.append(&column);
    failure.set_visible(false);
    (failure, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors_read_as_components() {
        assert_eq!(rgb("#ff0000"), Some((1.0, 0.0, 0.0)));
        assert_eq!(rgb("#zz0000"), None);
        assert_eq!(rgb("ff0000"), None);
    }
}
