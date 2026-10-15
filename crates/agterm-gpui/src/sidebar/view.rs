//! How the sidebar is drawn (upstream's `SidebarCellView`, `SidebarRowView`, `BadgeView` and the cell builder in
//! `WorkspaceSidebar+RowRendering.swift`; `WindowContentView.swift`'s `sidebarColumn`, `sidebarDivider` and
//! `bottomBar`): the column in the sidebar color, its rows (a workspace row's disclosure, the leading glyph, the name
//! or the rename field, a workspace row's hover "+", the agent-status glyph and the unseen badge; the selection pill
//! behind a selected row), the drop highlight, the empty flagged view's hint, the footer, and the divider with its
//! grab handle overhanging the terminal.
//!
//! The element ids are upstream's accessibility ids: `workspace-row`/`session-row` on the name,
//! `workspace-icon`/`session-icon`, `edit-field`, `workspace-add-session`, `notify-badge`, `agent-status`, and the
//! footer's `add-session`, `focus-filter-toggle`, `flagged-view-toggle`.

use std::rc::Rc;

use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input};
use gpui_kit::component::menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, CursorStyle, DismissEvent, DragMoveEvent, ElementId,
    EmptyView, ExternalPaths, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, deferred, div,
    prelude::FluentBuilder as _, px, rgb,
};
use uuid::Uuid;

use super::drag::{DragPayload, Highlight};
use super::look::{
    BADGE_FONT_SIZE, BADGE_GAP, BADGE_HEIGHT, BADGE_RED, BADGE_TEXT_ROOM, CONTENT_INSET, DISABLED_ALPHA, DISCLOSURE,
    DIVIDER_HANDLE, FOOTER_BUTTON, FOOTER_GAP, FOOTER_PADDING, GAP, Look, NESTED_INDENT, PILL_INSET_X, PILL_INSET_Y,
    PILL_RADIUS, color,
};
use super::menu;
use super::rows::badge_text;
use super::{DividerDrag, EMPTY_FLAGGED, FooterAction, RowDrag, RowKind, Sidebar, SidebarRow, context};
use crate::row_icon::{self, Glyph, SLOT};
use crate::status_glyph;

/// The drop highlight's color: macOS `systemBlue`, as the outline view's drop indicator.
const DROP_ACCENT: u32 = 0x000A_84FF;
/// The drop line's thickness.
const DROP_LINE: f32 = 2.0;
/// The empty flagged view's hint: its distance from the top and the sides, and its text's opacity.
const HINT_INSETS: (f32, f32) = (40.0, 16.0);
const HINT_ALPHA: f64 = 0.55;

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.settle_drags(cx);
        let root = div().id("sidebar").flex_none().h_full();
        if !self.shown {
            return root.into_any_element();
        }
        #[expect(clippy::cast_possible_truncation, reason = "a sidebar width is a few hundred points")]
        let width = self.width() as f32;
        root.flex()
            .flex_row()
            .on_drag_move::<DividerDrag>(cx.listener(|this, event: &DragMoveEvent<DividerDrag>, window, cx| {
                cx.set_active_drag_cursor_style(CursorStyle::ResizeLeftRight, window);
                this.divider_moved(f64::from(event.event.position.x), f64::from(event.bounds.origin.x), cx);
            }))
            .child(self.column(width, window, cx))
            .child(self.divider())
            .into_any_element()
    }
}

impl Sidebar {
    /// The sidebar's column: the rows over the footer, in the sidebar color.
    fn column(&self, width: f32, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let look = self.look;
        let key = window.is_window_active();
        let rows: Vec<AnyElement> = (0..self.rows.len())
            .map(|index| self.row_element(index, key, window, cx))
            .collect();
        let list = div()
            .id("sidebar-list")
            .size_full()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_drag_move::<RowDrag>(cx.listener(|this, event: &DragMoveEvent<RowDrag>, _, cx| {
                let position = event.event.position;
                if !event.bounds.contains(&position) {
                    if this.drag.over {
                        this.end_drag(false, cx);
                    }
                    return;
                }
                let payload = event.drag(cx).payload.clone();
                this.rows_dragged_over(&payload, f64::from(position.y), cx);
            }))
            .on_drop::<RowDrag>(cx.listener(|this, drag: &RowDrag, window, cx| {
                let y = f64::from(window.mouse_position().y);
                this.rows_dropped(&drag.payload, y, cx);
            }))
            .on_drag_move::<ExternalPaths>(cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                let position = event.event.position;
                if event.bounds.contains(&position) {
                    this.folders_dragged_over(f64::from(position.y), cx);
                } else if this.drag.over {
                    this.end_drag(false, cx);
                }
            }))
            .on_drop::<ExternalPaths>(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                let y = f64::from(window.mouse_position().y);
                this.folders_dropped(paths, y, window, cx);
            }))
            .children(rows);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .bg(color(look.colors.sidebar, 1.0))
            .text_color(look.text(false))
            .text_size(px(look.font_size))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(list)
                    .when(self.context.empty_flagged, |area| area.child(empty_hint(look))),
            )
            .child(self.footer(cx))
    }

    /// Row `index`.
    fn row_element(&self, index: usize, key: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let row = &self.rows[index];
        let id = row.id;
        let look = self.look;
        let selected = matches!(row.kind, RowKind::Session { selected: true, .. });
        let workspace = row.is_workspace();
        let mut element = div()
            .id(ElementId::Uuid(id))
            .debug_selector(|| format!("row:{}", row.label))
            .relative()
            .flex_none()
            .w_full()
            .h(px(look.row_height()))
            .px(px(CONTENT_INSET))
            .text_color(look.text(selected))
            .when(selected, |element| element.child(pill(look.pill(key))))
            .child(self.row_content(row, selected, window, cx))
            .children(self.drop_marker(index))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                if event.standard_click() {
                    this.row_clicked(id, event.click_count(), event.modifiers(), window, cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, _| this.secondary_pressed(id)),
            );
        if workspace {
            element = element.on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_hovered(id, *hovered, cx);
            }));
        }
        if self.context.drop.tree && self.editing.is_none() {
            let payload = if workspace {
                DragPayload {
                    workspace: true,
                    ids: vec![id],
                }
            } else {
                DragPayload {
                    workspace: false,
                    ids: context::dragged_sessions(&self.context, id),
                }
            };
            let drag = RowDrag {
                payload,
                label: row.label.clone(),
                look,
            };
            element = element.on_drag(drag, |drag, _, _, cx| {
                let preview = DragPreview {
                    label: drag.label.clone().into(),
                    look: drag.look,
                };
                cx.new(|_| preview)
            });
        }
        let sidebar = cx.entity().downgrade();
        element
            .context_menu(move |popup, window, cx| {
                let Some(entity) = sidebar.upgrade() else {
                    return popup;
                };
                let entries = entity.update(cx, |this, _| {
                    this.menu_opened();
                    this.menu_entries(id)
                });
                // the subscription needs the menu built; it lives as long as the menu
                let closing = sidebar.clone();
                cx.defer_in(window, move |_, _, cx| {
                    cx.subscribe_self(move |_, _: &DismissEvent, cx| {
                        if let Some(entity) = closing.upgrade() {
                            entity.update(cx, |this, _| this.menu_closed());
                        }
                    })
                    .detach();
                });
                let target = sidebar.clone();
                let perform: menu::Perform = Rc::new(move |action, window, cx| {
                    if let Some(entity) = target.upgrade() {
                        entity.update(cx, |this, cx| this.perform(action, window, cx));
                    }
                });
                menu::build(popup, entries, &perform, window, cx)
            })
            .into_any_element()
    }

    /// A row's content, leading to trailing.
    fn row_content(&self, row: &SidebarRow, selected: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let look = self.look;
        let icon_color = look.icon(selected);
        let workspace = row.is_workspace();
        let shows_add = workspace && self.hovered == Some(row.id) && self.context.row_add_session;
        let status = row.status.as_ref();
        let trailing = trailing_width(window, row, status.is_some(), shows_add);
        let lead = match row.kind {
            RowKind::Workspace { .. } | RowKind::Session { nested: true, .. } => NESTED_INDENT,
            RowKind::Session { nested: false, .. } => 0.0,
        };
        #[expect(clippy::cast_possible_truncation, reason = "a sidebar width is a few hundred points")]
        let available = self.width() as f32 - 2.0 * CONTENT_INSET - lead - SLOT - GAP - trailing;
        let content = div().flex().flex_row().items_center().size_full().min_w(px(0.));
        let content = match row.kind {
            RowKind::Workspace { expanded } => content
                .child(Self::disclosure(row.id, expanded, icon_color, cx))
                .child(div().flex_none().w(px(NESTED_INDENT - DISCLOSURE))),
            RowKind::Session { nested: true, .. } => content.pl(px(NESTED_INDENT)),
            RowKind::Session { nested: false, .. } => content,
        };
        content
            .child(leading_icon(row, icon_color))
            .child(div().flex_none().w(px(GAP)))
            .child(self.name(row, available, window, cx))
            .child(div().flex_none().w(px(GAP)))
            .when(shows_add, |content| {
                content.child(Self::add_button(row.id, icon_color, cx))
            })
            .when_some(status, |content, look| {
                content.child(status_glyph::glyph(look, status_glyph::animations_enabled(cx)))
            })
            .when(row.badge > 0, |content| content.child(badge(row.badge)))
            .into_any_element()
    }

    /// The name, or the rename field while this row is renamed; the full name shows on hover only while the row is too
    /// narrow for it (upstream's `updateNameTooltip`).
    fn name(&self, row: &SidebarRow, available: f32, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if let Some(editing) = self.editing.as_ref().filter(|editing| editing.id == row.id) {
            return div()
                .flex_1()
                .min_w(px(0.))
                .debug_selector(|| "edit-field".to_owned())
                .on_action(cx.listener(|this, _: &Escape, _, cx| this.finish_rename(false, cx)))
                // a press anywhere else commits, as the field losing the keyboard does: the disclosure, the hover "+"
                // and the rows take no focus, and the list they reshape would otherwise cancel the edit
                .on_mouse_down_out(cx.listener(|this, _, _, cx| this.finish_rename(true, cx)))
                .child(
                    Input::new(&editing.input)
                        .id("edit-field")
                        .accessibility_id("edit-field")
                        .h(px(self.look.row_height() - 4.0)),
                )
                .into_any_element();
        }
        let workspace = row.is_workspace();
        let weight = if workspace {
            FontWeight::MEDIUM
        } else {
            FontWeight::NORMAL
        };
        let label: SharedString = row.label.clone().into();
        let truncated = text_width(window, &label, self.look.font_size, weight) > available;
        div()
            .id(if workspace { "workspace-row" } else { "session-row" })
            .flex_1()
            .min_w(px(0.))
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .font_weight(weight)
            .child(label.clone())
            .when(truncated, |name| {
                name.tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            })
            .into_any_element()
    }

    /// A workspace row's disclosure: opens or folds the row at once, whatever the whole-row setting.
    fn disclosure(id: Uuid, expanded: bool, color: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("disclosure")
            .debug_selector(move || format!("disclosure:{id}"))
            .flex_none()
            .size(px(DISCLOSURE))
            .child(row_icon::icon(Glyph::Disclosure { expanded }, color))
            .tooltip(move |window, cx| Tooltip::new(if expanded { "Collapse" } else { "Expand" }).build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.cancel_toggle();
                this.toggle(id);
            }))
    }

    /// A workspace row's hover "+": a session in that workspace.
    fn add_button(id: Uuid, color: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("workspace-add-session")
            .debug_selector(move || format!("workspace-add-session:{id}"))
            .flex_none()
            .size(px(SLOT))
            .child(row_icon::icon(Glyph::Plus, color))
            .tooltip(|window, cx| Tooltip::new("New Session").build(window, cx))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.add_session_clicked(id);
            }))
    }

    /// Where a drop would land on row `index`: a line above or below it, or a frame around it.
    fn drop_marker(&self, index: usize) -> Option<AnyElement> {
        let accent: Hsla = rgb(DROP_ACCENT).into();
        let marker = match self.drag.highlight? {
            Highlight::OnRow(on) if on == index => div()
                .absolute()
                .left(px(PILL_INSET_X))
                .right(px(PILL_INSET_X))
                .top(px(PILL_INSET_Y))
                .bottom(px(PILL_INSET_Y))
                .rounded(px(PILL_RADIUS))
                .border(px(DROP_LINE))
                .border_color(accent),
            Highlight::Above(above) if above == index => {
                div().absolute().left_0().right_0().top_0().h(px(DROP_LINE)).bg(accent)
            }
            Highlight::Below(below) if below == index => div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(px(DROP_LINE))
                .bg(accent),
            _ => return None,
        };
        Some(marker.into_any_element())
    }

    /// The footer (upstream's `bottomBar`): New Workspace and the add-session menu on the left, the focus filter and
    /// the flagged view toggles on the right, each hidden by its Interface setting.
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.context.footer;
        let ink = self.look.text(false);
        let sidebar = cx.entity().downgrade();
        let run = move |action: FooterAction, _: &mut Context<Self>| {
            let sidebar = sidebar.clone();
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                if let Some(entity) = sidebar.upgrade() {
                    entity.update(cx, |this, cx| this.footer_action(action, window, cx));
                }
            }
        };
        let focus_tip = if state.filter_on {
            "Show all workspaces"
        } else {
            "Show only focused workspaces"
        };
        let flagged_tip = if state.flagged_mode {
            "Show all sessions"
        } else {
            "Show flagged sessions"
        };
        // no entering an empty flagged view; the way back to the tree always works
        let flagged_usable = state.flagged_mode || state.has_flagged;
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(FOOTER_GAP))
            .px(px(FOOTER_PADDING.0))
            .py(px(FOOTER_PADDING.1))
            .when(state.new_workspace, |footer| {
                footer.child(
                    footer_button("new-workspace", Glyph::NewWorkspace, ink, "New Workspace")
                        .accessibility_label("New Workspace")
                        .on_click(run(FooterAction::NewWorkspace, cx)),
                )
            })
            .when(state.new_session, |footer| {
                footer.child(Self::add_session_menu(ink, cx))
            })
            .child(div().flex_1())
            .when(state.focus_filter, |footer| {
                footer.child(
                    footer_button(
                        "focus-filter-toggle",
                        Glyph::Grid {
                            filled: state.filter_on,
                        },
                        ink,
                        focus_tip,
                    )
                    .accessibility_label("Toggle Workspace Filter")
                    // the filter's on/off, upstream's accessibility value
                    .toggled(state.filter_on)
                    // nothing marked, nothing to filter to: the store refuses an empty set too
                    .disabled(!state.has_members)
                    .when(!state.has_members, |button| button.opacity(DISABLED_ALPHA))
                    .on_click(run(FooterAction::ToggleFocusFilter, cx)),
                )
            })
            .when(state.flagged_view, |footer| {
                footer.child(
                    footer_button(
                        "flagged-view-toggle",
                        Glyph::Flag {
                            filled: state.flagged_mode,
                        },
                        ink,
                        flagged_tip,
                    )
                    .accessibility_label("Toggle Flagged View")
                    .toggled(state.flagged_mode)
                    .disabled(!flagged_usable)
                    .when(!flagged_usable, |button| button.opacity(DISABLED_ALPHA))
                    .on_click(run(FooterAction::ToggleFlaggedView, cx)),
                )
            })
    }

    /// The footer's add-session menu: New Session and Open Directory…, opening upward.
    fn add_session_menu(ink: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar = cx.entity().downgrade();
        footer_button("add-session", Glyph::AddSession, ink, "New Session")
            .accessibility_label("Add session")
            .dropdown_menu_with_anchor(gpui_kit::Anchor::BottomLeft, move |popup: PopupMenu, _, _| {
                popup
                    .item(footer_item(&sidebar, "New Session", FooterAction::NewSession))
                    .item(footer_item(&sidebar, "Open Directory…", FooterAction::OpenDirectory))
            })
    }

    /// The 1-point divider and its wider grab handle, drawn over the terminal's edge (upstream's `sidebarDivider`).
    fn divider(&self) -> impl IntoElement {
        let overhang = (DIVIDER_HANDLE - 1.0) / 2.0;
        div()
            .relative()
            .flex_none()
            .w(px(1.))
            .h_full()
            .bg(self.look.separator())
            .child(
                deferred(
                    div()
                        .id("sidebar-divider")
                        .debug_selector(|| "sidebar-divider".to_owned())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-overhang))
                        .w(px(DIVIDER_HANDLE))
                        .cursor_ew_resize()
                        .on_drag(DividerDrag, |_, _, _, cx| cx.new(|_| EmptyView)),
                )
                .with_priority(1),
            )
    }
}

/// An item of the footer's add-session menu, running `action`.
fn footer_item(sidebar: &WeakEntity<Sidebar>, label: &'static str, action: FooterAction) -> PopupMenuItem {
    let sidebar = sidebar.clone();
    PopupMenuItem::new(label).on_click(move |_, window, cx| {
        if let Some(entity) = sidebar.upgrade() {
            entity.update(cx, |this, cx| this.footer_action(action, window, cx));
        }
    })
}

/// A footer control: a borderless 24x22 button with a glyph, which never takes the keyboard.
fn footer_button(id: &'static str, glyph: Glyph, ink: Hsla, tooltip: &'static str) -> Button {
    Button::new(id)
        .debug_selector(move || id.to_owned())
        .ghost()
        .tab_stop(false)
        .p_0()
        .w(px(FOOTER_BUTTON.0))
        .h(px(FOOTER_BUTTON.1))
        .tooltip(tooltip)
        .child(row_icon::icon(glyph, ink))
}

/// The selection pill behind a selected row.
fn pill(fill: Hsla) -> impl IntoElement {
    div()
        .absolute()
        .left(px(PILL_INSET_X))
        .right(px(PILL_INSET_X))
        .top(px(PILL_INSET_Y))
        .bottom(px(PILL_INSET_Y))
        .rounded(px(PILL_RADIUS))
        .bg(fill)
}

/// A row's leading glyph, with a remote row's stream notice as its hover text.
fn leading_icon(row: &SidebarRow, color: Hsla) -> impl IntoElement {
    let tooltip = row.icon_tooltip.clone().map(SharedString::from);
    div()
        .id(if row.is_workspace() {
            "workspace-icon"
        } else {
            "session-icon"
        })
        .flex_none()
        .size(px(SLOT))
        .child(row_icon::icon(row.icon, color))
        .when_some(tooltip, |icon, tooltip| {
            icon.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        })
}

/// The unseen-notification badge: a red capsule with the count in white, a single digit a circle.
fn badge(count: usize) -> impl IntoElement {
    div().id("notify-badge").flex_none().pl(px(BADGE_GAP)).child(
        div()
            .flex()
            .items_center()
            .justify_center()
            .h(px(BADGE_HEIGHT))
            .min_w(px(BADGE_HEIGHT))
            .px(px(BADGE_TEXT_ROOM / 2.0))
            .rounded(px(BADGE_HEIGHT / 2.0))
            .bg(color(BADGE_RED, 1.0))
            .text_color(gpui_kit::white())
            .text_size(px(BADGE_FONT_SIZE))
            .font_weight(FontWeight::BOLD)
            .child(badge_text(count)),
    )
}

/// The width of what trails a row's name: the gap, the hover "+", the status glyph and the badge.
fn trailing_width(window: &Window, row: &SidebarRow, status: bool, add: bool) -> f32 {
    let mut width = GAP;
    if add {
        width += SLOT;
    }
    if status {
        width += SLOT;
    }
    if row.badge > 0 {
        let text = badge_text(row.badge);
        let capsule =
            (text_width(window, &text.into(), BADGE_FONT_SIZE, FontWeight::BOLD) + BADGE_TEXT_ROOM).max(BADGE_HEIGHT);
        width += capsule + BADGE_GAP;
    }
    width
}

/// `text`'s width in the window's font at `size` and `weight`.
fn text_width(window: &Window, text: &SharedString, size: f32, weight: FontWeight) -> f32 {
    let mut run = window.text_style().to_run(text.len());
    run.font.weight = weight;
    let line = window.text_system().shape_line(text.clone(), px(size), &[run], None);
    f32::from(line.width)
}

/// The empty flagged view's hint, over the list and never taking the pointer.
fn empty_hint(look: Look) -> impl IntoElement {
    div()
        .absolute()
        .top(px(HINT_INSETS.0))
        .left(px(HINT_INSETS.1))
        .right(px(HINT_INSETS.1))
        .flex()
        .flex_col()
        .items_center()
        .text_color(color(look.colors.foreground, HINT_ALPHA))
        .children(EMPTY_FLAGGED.lines().map(|line| div().text_center().child(line)))
}

/// What a row drag shows under the pointer: the row's label on a pill in the sidebar's colors.
struct DragPreview {
    label: SharedString,
    look: Look,
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(2.))
            .rounded(px(PILL_RADIUS))
            .bg(self.look.pill(true))
            .text_color(self.look.text(false))
            .text_size(px(self.look.font_size))
            .whitespace_nowrap()
            .child(self.label.clone())
    }
}
