//! One sidebar row's widgets (upstream's `SidebarCellView` and its builder in `WorkspaceSidebar+RowRendering.swift`):
//! a workspace row's disclosure arrow, the leading glyph, the name (a label, or the inline rename field), a workspace
//! row's hover "+", the agent-status glyph and the unseen badge. Built once per row and updated in place, so a content
//! change never restarts a blink or drops a rename.
//!
//! The names upstream's accessibility tree carries are the widget names: `workspace-row`/`session-row` on the name,
//! `workspace-icon`/`session-icon`, `edit-field`, `workspace-add-session`, `notify-badge`, `agent-status`.

use gtk::prelude::*;

use super::rows::{RowKind, SidebarRow, badge_text};
use crate::row_icon::RowIcon;
use crate::status_glyph::StatusGlyph;

/// How far a nested session row's content starts past its workspace row's: the disclosure arrow's width.
const NESTED_INDENT: i32 = 22;

/// A row's widgets.
pub struct RowWidget {
    row: gtk::ListBoxRow,
    arrow: Option<gtk::Button>,
    icon: RowIcon,
    stack: gtk::Stack,
    label: gtk::Label,
    entry: gtk::Entry,
    add: Option<gtk::Button>,
    glyph: StatusGlyph,
    badge: gtk::Label,
}

impl RowWidget {
    /// Builds the widgets of `row`.
    #[must_use]
    pub fn build(row: &SidebarRow) -> Self {
        let workspace = row.is_workspace();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let arrow = workspace.then(|| {
            let arrow = gtk::Button::builder()
                .icon_name("pan-end-symbolic")
                .focusable(false)
                .valign(gtk::Align::Center)
                .build();
            arrow.add_css_class("flat");
            arrow.add_css_class("agterm-disclosure");
            content.append(&arrow);
            arrow
        });
        if let RowKind::Session { nested: true, .. } = row.kind {
            content.set_margin_start(NESTED_INDENT);
        }
        let icon = RowIcon::new(row.icon);
        icon.widget()
            .set_widget_name(if workspace { "workspace-icon" } else { "session-icon" });
        icon.widget().add_css_class("dim-label");
        icon.widget().set_valign(gtk::Align::Center);
        content.append(icon.widget());
        let label = name_label(workspace);
        let entry = gtk::Entry::builder()
            .name("edit-field")
            .hexpand(true)
            .width_chars(1)
            .build();
        let stack = gtk::Stack::builder().hexpand(true).hhomogeneous(false).build();
        stack.add_named(&label, Some("label"));
        stack.add_named(&entry, Some("edit"));
        stack.set_visible_child_name("label");
        content.append(&stack);
        let add = workspace.then(|| {
            let add = gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .name("workspace-add-session")
                .tooltip_text("New Session")
                .focusable(false)
                .valign(gtk::Align::Center)
                .visible(false)
                .build();
            add.add_css_class("flat");
            add.add_css_class("agterm-row-add");
            add.update_property(&[gtk::accessible::Property::Label("New Session")]);
            content.append(&add);
            add
        });
        let glyph = StatusGlyph::new();
        content.append(glyph.widget());
        let badge = gtk::Label::builder()
            .name("notify-badge")
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        badge.add_css_class("agterm-badge");
        badge.add_css_class("numeric");
        content.append(&badge);
        let list_row = gtk::ListBoxRow::builder()
            .child(&content)
            .selectable(!workspace)
            .focusable(false)
            .build();
        let widget = Self {
            row: list_row,
            arrow,
            icon,
            stack,
            label,
            entry,
            add,
            glyph,
            badge,
        };
        widget.update(row);
        widget
    }

    /// Brings the widgets in line with `row`, which has this row's shape.
    pub fn update(&self, row: &SidebarRow) {
        self.label.set_label(&row.label);
        self.icon.set_glyph(row.icon);
        self.icon.widget().set_tooltip_text(row.icon_tooltip.as_deref());
        if let (Some(arrow), RowKind::Workspace { expanded }) = (&self.arrow, row.kind) {
            arrow.set_icon_name(if expanded {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            });
            arrow.set_tooltip_text(Some(if expanded { "Collapse" } else { "Expand" }));
        }
        self.glyph.apply(row.status.as_ref());
        self.badge.set_visible(row.badge > 0);
        if row.badge > 0 {
            let text = badge_text(row.badge);
            self.badge.set_label(&text);
            self.badge.update_property(&[gtk::accessible::Property::Label(&text)]);
        }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::ListBoxRow {
        &self.row
    }

    /// A workspace row's disclosure arrow.
    #[must_use]
    pub fn arrow(&self) -> Option<&gtk::Button> {
        self.arrow.as_ref()
    }

    /// A workspace row's hover "+".
    #[must_use]
    pub fn add_button(&self) -> Option<&gtk::Button> {
        self.add.as_ref()
    }

    /// The inline rename field.
    #[must_use]
    pub fn entry(&self) -> &gtk::Entry {
        &self.entry
    }

    /// Swaps the name for the rename field seeded with `text`, all of it selected, and gives it the keyboard.
    pub fn begin_edit(&self, text: &str) {
        self.entry.set_text(text);
        self.stack.set_visible_child_name("edit");
        self.entry.grab_focus();
        self.entry.select_region(0, -1);
    }

    /// Puts the name back in place of the rename field.
    pub fn end_edit(&self) {
        self.stack.set_visible_child_name("label");
    }

    /// The rename field's text.
    #[must_use]
    pub fn edited_text(&self) -> String {
        self.entry.text().to_string()
    }

    /// The name the row shows.
    #[cfg(test)]
    pub fn shown_name(&self) -> String {
        self.label.label().to_string()
    }
}

/// The row's name: truncated at the end, with the full name as its tooltip only while truncated (upstream's
/// `updateNameTooltip`).
fn name_label(workspace: bool) -> gtk::Label {
    let label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .name(if workspace { "workspace-row" } else { "session-row" })
        .has_tooltip(true)
        .build();
    label.add_css_class("agterm-row-name");
    if workspace {
        label.add_css_class("heading");
    }
    label.connect_query_tooltip(|label, _, _, _, tooltip| {
        if !label.layout().is_ellipsized() {
            return false;
        }
        tooltip.set_text(Some(&label.label()));
        true
    });
    label
}
