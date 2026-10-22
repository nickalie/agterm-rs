//! The title bar's popovers (upstream's `WindowContentView+RecentSessions.swift` and `+CustomCommands.swift`; port of
//! the GTK build's `popovers.rs`): the recent-sessions list, the mouse twin of the session switcher; the attention list
//! across every open window with each row's status glyph; and the custom-commands list, the mouse twin of the
//! custom-command palette, the most-run commands above a separator. Each lists the rows the core composed
//! ([`ngterm_core::titlebar`]); a click on a row (or Return on the highlighted one) runs it and closes the popover.
//!
//! One popover is open per window at a time, under the button that opened it ([`Popovers::toggle`]). While it is open
//! the window's auto-follow holds still (`PopoverOpened`/`PopoverClosed`), it holds the keyboard (Up and Down move the
//! highlight, Return runs it, Escape closes), and a press anywhere outside it closes it and goes no further (a GTK
//! popover's autohide). One whose list empties or whose button turns off closes itself. Once it closes the window's
//! sync gives the keyboard back to the shown terminal. Element ids are upstream's: `recent-session-row`,
//! `attention-session-row`, `custom-command-top-row`, `custom-command-row`.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Div, FocusHandle, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ParentElement as _, Pixels, Point, Render, Role, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use ngterm_core::Core;
use ngterm_core::chrome::ChromeColors;
use ngterm_core::keymap::CustomCommand;
use ngterm_core::titlebar::{CustomCommandRows, SessionRow, TitlebarAction};
use uuid::Uuid;

use crate::modal::{self, NavKey};
use crate::row_icon::{self, Glyph};
use crate::state::{AppState, Poster, post};
use crate::status_glyph;
pub use crate::titlebar::PopoverKind;

/// The popovers' width (upstream's 320 points).
const WIDTH: f32 = 320.;
/// The tallest list before it scrolls (upstream's `attentionRowsCap`, about ten rows).
const MAX_HEIGHT: f32 = 440.;
/// Clearance between the popover and the window edge.
const MARGIN: f32 = 8.;

/// What a popover row does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Row {
    Session(SessionRow),
    /// A custom command; whether it is one of the most-run ones above the separator.
    Command(CustomCommand, bool),
}

impl Row {
    fn enabled(&self) -> bool {
        match self {
            Self::Session(row) => row.enabled,
            Self::Command(..) => true,
        }
    }
}

/// The open popover: which, and the point under its button it hangs from (window coordinates).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Open {
    kind: PopoverKind,
    anchor: Point<Pixels>,
}

/// A window's popovers (module docs).
pub struct Popovers {
    window: Uuid,
    poster: Poster,
    open: Option<Open>,
    recent: Vec<SessionRow>,
    attention: Vec<SessionRow>,
    commands: CustomCommandRows,
    /// Whether each button is on, as the last sync saw it.
    enabled: [bool; 3],
    /// The keyboard's row.
    highlighted: Option<usize>,
    /// The keymap generation the command rows were composed from; `None` before the first.
    generation: Option<u64>,
    settings: ngterm_core::persist::settings::AppSettings,
    focus: FocusHandle,
}

fn slot(kind: PopoverKind) -> usize {
    match kind {
        PopoverKind::Recent => 0,
        PopoverKind::Attention => 1,
        PopoverKind::CustomCommands => 2,
    }
}

impl Popovers {
    #[must_use]
    pub fn new(window: Uuid, poster: Poster, cx: &mut Context<Self>) -> Self {
        Self {
            window,
            poster,
            open: None,
            recent: Vec::new(),
            attention: Vec::new(),
            commands: CustomCommandRows::default(),
            enabled: [false; 3],
            highlighted: None,
            generation: None,
            settings: ngterm_core::persist::settings::AppSettings::default(),
            focus: cx.focus_handle(),
        }
    }

    /// The popover open, if any.
    #[must_use]
    pub fn open_kind(&self) -> Option<PopoverKind> {
        self.open.map(|open| open.kind)
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    #[must_use]
    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// The rows of `kind`, in order (the most-run commands first).
    fn rows(&self, kind: PopoverKind) -> Vec<Row> {
        match kind {
            PopoverKind::Recent => self.recent.iter().cloned().map(Row::Session).collect(),
            PopoverKind::Attention => self.attention.iter().cloned().map(Row::Session).collect(),
            PopoverKind::CustomCommands => self
                .commands
                .leading
                .iter()
                .map(|command| Row::Command(command.clone(), true))
                .chain(
                    self.commands
                        .rest
                        .iter()
                        .map(|command| Row::Command(command.clone(), false)),
                )
                .collect(),
        }
    }

    /// The labels of the open popover's rows, in order (tests).
    #[must_use]
    pub fn row_titles(&self) -> Vec<String> {
        self.open
            .map(|open| {
                self.rows(open.kind)
                    .into_iter()
                    .map(|row| match row {
                        Row::Session(row) => row.title,
                        Row::Command(command, _) => command.name,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The button of `kind` was clicked: its popover opens hanging from `anchor` (another one closing), or closes
    /// when it is the one open.
    pub fn toggle(&mut self, kind: PopoverKind, anchor: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        if self.open_kind() == Some(kind) {
            self.close(cx);
            return;
        }
        if !self.enabled[slot(kind)] {
            return;
        }
        let reopened = self.open.is_some();
        self.open = Some(Open { kind, anchor });
        self.highlighted = None;
        window.focus(&self.focus, cx);
        if !reopened {
            let id = self.window;
            post(&self.poster, move |state: &mut AppState| {
                state.titlebar_action(id, TitlebarAction::PopoverOpened);
            });
        }
        cx.notify();
    }

    /// Closes the open popover; the window's auto-follow resumes and its sync hands the keyboard back.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_none() {
            return;
        }
        self.highlighted = None;
        let id = self.window;
        post(&self.poster, move |state: &mut AppState| {
            state.titlebar_action(id, TitlebarAction::PopoverClosed);
        });
        cx.notify();
    }

    /// Lists the core's rows: the session lists on every sync, the custom commands once per keymap change and while
    /// the popover is open (upstream's popover body reads the run counts as it opens). An emptied list or a button
    /// turned off closes its popover.
    pub fn sync(&mut self, core: &Core, cx: &mut Context<Self>) {
        let model = core.titlebar(self.window);
        self.enabled = [
            model.recent.is_some_and(|button| button.enabled),
            model.attention.is_some_and(|button| button.enabled),
            model.custom_commands.is_some_and(|button| button.enabled),
        ];
        self.settings = core.settings().clone();
        let recent = core.recent_session_rows(self.window);
        let attention = core.attention_rows();
        let generation = core.keymap_generation();
        let commands_open = self.open_kind() == Some(PopoverKind::CustomCommands);
        if (commands_open || self.generation != Some(generation)) && self.enabled[2] {
            self.generation = Some(generation);
            self.commands = core.custom_command_rows();
        }
        let changed = recent != self.recent || attention != self.attention;
        self.recent = recent;
        self.attention = attention;
        if let Some(open) = self.open
            && (!self.enabled[slot(open.kind)] || self.rows(open.kind).is_empty())
        {
            self.close(cx);
        }
        if changed {
            cx.notify();
        }
    }

    /// Runs row `index` of the open popover and closes it; a disabled row does nothing.
    fn activate(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(open) = self.open else { return };
        let Some(row) = self.rows(open.kind).into_iter().nth(index).filter(Row::enabled) else {
            return;
        };
        let action = match row {
            Row::Session(row) if open.kind == PopoverKind::Recent => TitlebarAction::SelectRecent(row.session),
            Row::Session(row) => TitlebarAction::SelectAttention {
                window: row.window,
                session: row.session,
            },
            Row::Command(command, _) => TitlebarAction::RunCustomCommand(command.id),
        };
        self.close(cx);
        let id = self.window;
        post(&self.poster, move |state: &mut AppState| {
            state.titlebar_action(id, action);
        });
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.open else { return };
        let Some(key) = modal::nav_key(&event.keystroke) else {
            return;
        };
        cx.stop_propagation();
        let count = self.rows(open.kind).len();
        match key {
            NavKey::Forward if count > 0 => {
                self.highlighted = Some(self.highlighted.map_or(0, |index| (index + 1) % count));
            }
            NavKey::Backward if count > 0 => {
                self.highlighted = Some(self.highlighted.map_or(count - 1, |index| (index + count - 1) % count));
            }
            NavKey::Activate | NavKey::Space => {
                if let Some(index) = self.highlighted {
                    self.activate(index, cx);
                }
            }
            NavKey::Cancel => self.close(cx),
            NavKey::Forward | NavKey::Backward | NavKey::Letter(_) => {}
        }
        cx.notify();
    }

    fn render_row(
        &self,
        kind: PopoverKind,
        index: usize,
        row: &Row,
        colors: ChromeColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let highlighted = self.highlighted == Some(index);
        let hover = crate::chrome::color(colors.foreground, 0.1);
        let base = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .mx_1()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .when(highlighted, |row| row.bg(crate::chrome::color(modal::ACCENT, 0.25)))
            .when(row.enabled(), |row| {
                row.hover(move |style| style.bg(hover)).cursor_pointer()
            })
            .when(!row.enabled(), |row| row.opacity(0.45));
        let content = match row {
            Row::Session(session) => {
                let id = match kind {
                    PopoverKind::Attention => "attention-session-row",
                    _ => "recent-session-row",
                };
                let glyph = session
                    .indicator
                    .as_ref()
                    .and_then(|indicator| status_glyph::resolve(indicator, &self.settings))
                    .map(|look| status_glyph::glyph(&look, status_glyph::animations_enabled(cx)).into_any_element());
                let row = base
                    .id((id, index))
                    .accessibility_id(id)
                    .role(Role::MenuItem)
                    .aria_label(SharedString::from(session.title.clone()));
                session_line(row.children(glyph), session, colors)
            }
            Row::Command(command, top) => {
                let id = if *top {
                    "custom-command-top-row"
                } else {
                    "custom-command-row"
                };
                let row = base
                    .id((id, index))
                    .accessibility_id(id)
                    .role(Role::MenuItem)
                    .aria_label(SharedString::from(command.name.clone()));
                command_line(row, command, colors)
            }
        };
        content
            .on_click(cx.listener(move |popovers, _, _, cx| popovers.activate(index, cx)))
            .into_any_element()
    }
}

/// A session row's two lines after its glyph: the cloud for a remote session, the title, the subtitle dimmed.
fn session_line(line: Stateful<Div>, session: &SessionRow, colors: ChromeColors) -> Stateful<Div> {
    let ink = crate::chrome::color(colors.foreground, 1.0);
    let dim = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
    line.when(session.remote, |line| {
        line.child(row_icon::icon(
            Glyph::Remote {
                split: false,
                disconnected: false,
            },
            ink,
        ))
    })
    .child(
        div()
            .flex()
            .flex_col()
            .min_w(px(0.))
            .flex_1()
            .child(div().truncate().child(SharedString::from(session.title.clone())))
            .child(
                div()
                    .truncate()
                    .text_size(px(11.))
                    .text_color(dim)
                    .child(SharedString::from(session.subtitle.clone())),
            ),
    )
}

/// A command row: its name, and its chord dimmed at the trailing edge.
fn command_line(line: Stateful<Div>, command: &CustomCommand, colors: ChromeColors) -> Stateful<Div> {
    let dim = crate::chrome::color(colors.foreground, crate::chrome::DIM_ALPHA);
    line.child(
        div()
            .flex_1()
            .min_w(px(0.))
            .truncate()
            .child(SharedString::from(command.name.clone())),
    )
    .when(!command.shortcut.is_empty(), |line| {
        line.child(
            div()
                .text_size(px(11.))
                .text_color(dim)
                .child(SharedString::from(command.shortcut.clone())),
        )
    })
}

impl Render for Popovers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = self.open else {
            return div().id("popover-layer");
        };
        let colors = crate::chrome::current(cx);
        let rows = self.rows(open.kind);
        let separator = crate::chrome::separator(&colors);
        let mut list: Vec<AnyElement> = Vec::new();
        for (index, row) in rows.iter().enumerate() {
            let leading_ends =
                matches!(row, Row::Command(_, false)) && index > 0 && matches!(rows[index - 1], Row::Command(_, true));
            if leading_ends {
                list.push(div().my_1().h(px(1.)).bg(separator).into_any_element());
            }
            list.push(self.render_row(open.kind, index, row, colors, cx));
        }
        let viewport = window.viewport_size();
        let left = (f32::from(open.anchor.x) - WIDTH / 2.)
            .min(f32::from(viewport.width) - WIDTH - MARGIN)
            .max(MARGIN);
        div()
            .id("popover-layer")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|popovers, _, _, cx| {
                    popovers.close(cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|popovers, _, _, cx| {
                    popovers.close(cx);
                    cx.stop_propagation();
                }),
            )
            .child(
                modal::panel(colors)
                    .id("popover")
                    .key_context("NgtermPopover")
                    .track_focus(&self.focus)
                    .on_key_down(cx.listener(Self::key_down))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .absolute()
                    .left(px(left))
                    .top(open.anchor.y + px(4.))
                    .w(px(WIDTH))
                    .max_h(px(MAX_HEIGHT))
                    .overflow_y_scroll()
                    .py_1()
                    .text_size(px(13.))
                    .children(list),
            )
    }
}

#[cfg(test)]
mod tests;
