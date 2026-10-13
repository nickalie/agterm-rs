//! The title bar's popovers (upstream's `WindowContentView+RecentSessions.swift` and `+CustomCommands.swift`): the
//! recent-sessions list, the mouse twin of the session switcher; the attention list across every open window with each
//! row's status glyph; and the custom-commands list, the mouse twin of the custom-command palette. Each is a menu button
//! whose popover lists the rows the core composed on the last settle ([`agterm_core::titlebar`]); a click on a row runs
//! it and closes the popover. While one is open the window's auto-follow holds still, and one whose list empties or
//! whose button turns off closes itself.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use agterm_core::Core;
use agterm_core::keymap::CustomCommand;
use agterm_core::titlebar::{CustomCommandRows, SessionRow, TitlebarAction};
use uuid::Uuid;

use crate::row_icon::{Glyph, RowIcon};
use crate::state::{self, AppState, Poster};
use crate::status_glyph::{self, StatusGlyph};

/// The popovers' width (upstream's 320 points).
const WIDTH: i32 = 320;
/// The tallest list before it scrolls (upstream's `attentionRowsCap`, about ten rows).
const MAX_HEIGHT: i32 = 440;

/// Which session list a [`SessionPopover`] shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPopoverKind {
    Recent,
    Attention,
}

impl SessionPopoverKind {
    fn icon(self) -> &'static str {
        match self {
            Self::Recent => "document-open-recent-symbolic",
            Self::Attention => "preferences-system-notifications-symbolic",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Recent => "recent-sessions-button",
            Self::Attention => "attention-button",
        }
    }

    fn row_name(self) -> &'static str {
        match self {
            Self::Recent => "recent-session-row",
            Self::Attention => "attention-session-row",
        }
    }
}

/// The scrolling list a popover holds.
fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(MAX_HEIGHT)
        .width_request(WIDTH)
        .child(child)
        .build()
}

fn list() -> gtk::ListBox {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .activate_on_single_click(true)
        .build();
    list.add_css_class("navigation-sidebar");
    list
}

/// A popover menu button: opening holds the window's auto-follow, closing lifts it.
fn menu_button(icon: &str, name: &str, popover: &gtk::Popover, window: Uuid, poster: &Poster) -> gtk::MenuButton {
    let button = gtk::MenuButton::builder()
        .icon_name(icon)
        .name(name)
        .popover(popover)
        .build();
    let posting = poster.clone();
    popover.connect_show(move |_| {
        state::post(&posting, move |state: &mut AppState| {
            state.titlebar_action(window, TitlebarAction::PopoverOpened);
        });
    });
    let posting = poster.clone();
    popover.connect_closed(move |_| {
        state::post(&posting, move |state: &mut AppState| {
            state.titlebar_action(window, TitlebarAction::PopoverClosed);
        });
    });
    button
}

/// Shows `button` when `state` is `Some`, enabled per its value, with `tooltip`; a popover open under a button that
/// turned off closes.
fn show_button(button: &gtk::MenuButton, popover: &gtk::Popover, state: Option<bool>, tooltip: &str) {
    button.set_visible(state.is_some());
    let enabled = state.unwrap_or(false);
    button.set_sensitive(enabled);
    button.set_tooltip_text(Some(tooltip));
    if !enabled && popover.is_visible() {
        popover.popdown();
    }
}

/// The recent-sessions or attention popover.
pub struct SessionPopover {
    kind: SessionPopoverKind,
    button: gtk::MenuButton,
    popover: gtk::Popover,
    list: gtk::ListBox,
    rows: Rc<RefCell<Vec<SessionRow>>>,
}

impl SessionPopover {
    #[must_use]
    pub fn new(kind: SessionPopoverKind, window: Uuid, poster: &Poster) -> Self {
        let list = list();
        let popover = gtk::Popover::builder().child(&scrolled(&list)).build();
        let button = menu_button(kind.icon(), kind.name(), &popover, window, poster);
        let rows: Rc<RefCell<Vec<SessionRow>>> = Rc::default();
        let chosen = Rc::clone(&rows);
        let posting = poster.clone();
        let closing = popover.clone();
        list.connect_row_activated(move |_, row| {
            let Some(row) = usize::try_from(row.index())
                .ok()
                .and_then(|index| chosen.borrow().get(index).cloned())
                .filter(|row| row.enabled)
            else {
                return;
            };
            closing.popdown();
            let action = match kind {
                SessionPopoverKind::Recent => TitlebarAction::SelectRecent(row.session),
                SessionPopoverKind::Attention => TitlebarAction::SelectAttention {
                    window: row.window,
                    session: row.session,
                },
            };
            state::post(&posting, move |state: &mut AppState| {
                state.titlebar_action(window, action);
            });
        });
        Self {
            kind,
            button,
            popover,
            list,
            rows,
        }
    }

    /// The menu button to pack into the bar.
    #[must_use]
    pub fn widget(&self) -> &gtk::MenuButton {
        &self.button
    }

    /// Lists `rows` (rebuilt only when they changed); an emptied list closes the popover.
    pub fn sync(&self, rows: Vec<SessionRow>, enabled: bool, core: &Core) {
        if rows.is_empty() || !enabled {
            self.popover.popdown();
        }
        if *self.rows.borrow() == rows {
            return;
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for row in &rows {
            self.list.append(&session_row(self.kind, row, core));
        }
        self.rows.replace(rows);
    }

    /// Shows the button (`None` hides it), enabled per its value.
    pub fn show_button(&self, state: Option<bool>, tooltip: &str) {
        show_button(&self.button, &self.popover, state, tooltip);
    }

    /// Tints the bell in the blocked color while a session is blocked.
    pub fn set_blocked(&self, blocked: bool) {
        let class = crate::titlebar::bell_blocked_class();
        if blocked {
            self.button.add_css_class(class);
        } else {
            self.button.remove_css_class(class);
        }
    }
}

/// One two-line session row: an attention row leads with its status glyph, a remote session with the cloud.
fn session_row(kind: SessionPopoverKind, row: &SessionRow, core: &Core) -> gtk::ListBoxRow {
    let line = gtk::Box::builder()
        .spacing(8)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .build();
    if let Some(indicator) = &row.indicator {
        let glyph = StatusGlyph::new();
        glyph.apply(status_glyph::resolve(indicator, core.settings()).as_ref());
        line.append(glyph.widget());
    }
    if row.remote {
        line.append(
            RowIcon::new(Glyph::Remote {
                split: false,
                disconnected: false,
            })
            .widget(),
        );
    }
    let text = gtk::Box::new(gtk::Orientation::Vertical, 1);
    let title = gtk::Label::builder()
        .label(&row.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    let subtitle = gtk::Label::builder()
        .label(&row.subtitle)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["caption", "dim-label"])
        .build();
    text.append(&title);
    text.append(&subtitle);
    line.append(&text);
    gtk::ListBoxRow::builder()
        .child(&line)
        .name(kind.row_name())
        .sensitive(row.enabled)
        .build()
}

/// The custom-commands popover: the most-run commands above a separator, then the rest.
pub struct CommandPopover {
    button: gtk::MenuButton,
    popover: gtk::Popover,
    leading: gtk::ListBox,
    separator: gtk::Separator,
    rest: gtk::ListBox,
    rows: Rc<RefCell<CustomCommandRows>>,
    /// The keymap generation the rows were composed from; `None` before the first.
    generation: Cell<Option<u64>>,
}

impl CommandPopover {
    #[must_use]
    pub fn new(window: Uuid, poster: &Poster) -> Self {
        let (leading, rest) = (list(), list());
        let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        column.append(&leading);
        column.append(&separator);
        column.append(&rest);
        let popover = gtk::Popover::builder().child(&scrolled(&column)).build();
        let button = menu_button("view-more-symbolic", "custom-commands-button", &popover, window, poster);
        let rows: Rc<RefCell<CustomCommandRows>> = Rc::default();
        for (list, top) in [(&leading, true), (&rest, false)] {
            let chosen = Rc::clone(&rows);
            let posting = poster.clone();
            let closing = popover.clone();
            list.connect_row_activated(move |_, row| {
                let rows = chosen.borrow();
                let commands = if top { &rows.leading } else { &rows.rest };
                let Some(id) = usize::try_from(row.index())
                    .ok()
                    .and_then(|index| commands.get(index))
                    .map(|command| command.id)
                else {
                    return;
                };
                closing.popdown();
                let action = TitlebarAction::RunCustomCommand(id);
                state::post(&posting, move |state: &mut AppState| {
                    state.titlebar_action(window, action);
                });
            });
        }
        Self {
            button,
            popover,
            leading,
            separator,
            rest,
            rows,
            generation: Cell::new(None),
        }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::MenuButton {
        &self.button
    }

    /// Whether the rows need composing: the keymap changed since, or the popover is open (opening it settles, so the
    /// run counts are read once per open, as upstream's popover body reads them, not on every settle).
    fn wants_rows(&self, generation: u64) -> bool {
        self.popover.is_visible() || self.generation.get() != Some(generation)
    }

    /// Lists the core's custom commands while the button is `enabled`; a disabled button closes the popover and reads
    /// nothing.
    pub fn sync(&self, core: &Core, enabled: bool) {
        if !enabled {
            self.popover.popdown();
            return;
        }
        let generation = core.keymap_generation();
        if !self.wants_rows(generation) {
            return;
        }
        self.generation.set(Some(generation));
        let rows = core.custom_command_rows();
        if *self.rows.borrow() == rows {
            return;
        }
        for (list, commands, name) in [
            (&self.leading, &rows.leading, "custom-command-top-row"),
            (&self.rest, &rows.rest, "custom-command-row"),
        ] {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            for command in commands {
                list.append(&command_row(command, name));
            }
        }
        let has_leading = !rows.leading.is_empty();
        self.leading.set_visible(has_leading);
        self.separator.set_visible(has_leading);
        self.rows.replace(rows);
    }

    pub fn show_button(&self, state: Option<bool>, tooltip: &str) {
        show_button(&self.button, &self.popover, state, tooltip);
    }
}

/// One command row: its name, and its chord dimmed at the trailing edge.
fn command_row(command: &CustomCommand, name: &str) -> gtk::ListBoxRow {
    let line = gtk::Box::builder()
        .spacing(12)
        .margin_top(4)
        .margin_bottom(4)
        .margin_start(4)
        .margin_end(4)
        .build();
    line.append(
        &gtk::Label::builder()
            .label(&command.name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build(),
    );
    if !command.shortcut.is_empty() {
        line.append(
            &gtk::Label::builder()
                .label(&command.shortcut)
                .css_classes(["caption", "dim-label"])
                .build(),
        );
    }
    gtk::ListBoxRow::builder().child(&line).name(name).build()
}
