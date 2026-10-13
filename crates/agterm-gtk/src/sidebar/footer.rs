//! The sidebar footer (upstream's `bottomBar` in `WindowContentView.swift`): New Workspace and the add-session menu
//! (New Session, Open Directory…) on the left, the focus filter and the flagged view toggles on the right, each
//! hideable through the interface settings. The add-session menu's items and the toggles act through the closures the
//! sidebar hands in.

use std::rc::Rc;

use gtk::gio;
use gtk::prelude::*;

use super::context::FooterState;
use crate::row_icon::{Glyph, RowIcon};

/// What the footer's controls ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterAction {
    NewWorkspace,
    NewSession,
    OpenDirectory,
    ToggleFocusFilter,
    ToggleFlaggedView,
}

pub struct Footer {
    root: gtk::Box,
    new_workspace: gtk::Button,
    add_session: gtk::MenuButton,
    focus: gtk::Button,
    focus_icon: RowIcon,
    flagged: gtk::Button,
    flagged_icon: RowIcon,
}

impl Footer {
    /// The footer, its controls running `perform`.
    #[must_use]
    pub fn new(perform: impl Fn(FooterAction) + 'static) -> Self {
        let perform: Rc<dyn Fn(FooterAction)> = Rc::new(perform);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        root.add_css_class("agterm-footer");
        root.set_margin_start(6);
        root.set_margin_end(6);
        root.set_margin_top(4);
        root.set_margin_bottom(4);
        let new_workspace = gtk::Button::builder()
            .icon_name("tab-new-symbolic")
            .tooltip_text("New Workspace")
            .focusable(false)
            .build();
        new_workspace.add_css_class("flat");
        new_workspace.update_property(&[gtk::accessible::Property::Label("New Workspace")]);
        let run = Rc::clone(&perform);
        new_workspace.connect_clicked(move |_| run(FooterAction::NewWorkspace));
        root.append(&new_workspace);

        let add_session = add_session_menu(&perform);
        root.append(&add_session);
        root.append(&gtk::Box::builder().hexpand(true).build());

        let focus_icon = RowIcon::new(Glyph::Grid { filled: false });
        let focus = toggle_button("focus-filter-toggle", "Toggle Workspace Filter", &focus_icon);
        let run = Rc::clone(&perform);
        focus.connect_clicked(move |_| run(FooterAction::ToggleFocusFilter));
        root.append(&focus);
        let flagged_icon = RowIcon::new(Glyph::Flag { filled: false });
        let flagged = toggle_button("flagged-view-toggle", "Toggle Flagged View", &flagged_icon);
        let run = Rc::clone(&perform);
        flagged.connect_clicked(move |_| run(FooterAction::ToggleFlaggedView));
        root.append(&flagged);
        Self {
            root,
            new_workspace,
            add_session,
            focus,
            focus_icon,
            flagged,
            flagged_icon,
        }
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    /// Shows `state`: which controls show, the toggles' glyphs, usability and hover text.
    pub fn update(&self, state: FooterState) {
        self.new_workspace.set_visible(state.new_workspace);
        self.add_session.set_visible(state.new_session);

        self.focus.set_visible(state.focus_filter);
        self.focus_icon.set_glyph(Glyph::Grid {
            filled: state.filter_on,
        });
        // nothing marked, nothing to filter to: the store refuses an empty set too
        self.focus.set_sensitive(state.has_members);
        self.focus.set_tooltip_text(Some(if state.filter_on {
            "Show all workspaces"
        } else {
            "Show only focused workspaces"
        }));
        self.focus
            .update_property(&[gtk::accessible::Property::ValueText(if state.filter_on {
                "on"
            } else {
                "off"
            })]);

        self.flagged.set_visible(state.flagged_view);
        self.flagged_icon.set_glyph(Glyph::Flag {
            filled: state.flagged_mode,
        });
        // no entering an empty flagged view; the way back to the tree always works
        self.flagged.set_sensitive(state.flagged_mode || state.has_flagged);
        self.flagged.set_tooltip_text(Some(if state.flagged_mode {
            "Show all sessions"
        } else {
            "Show flagged sessions"
        }));
    }
}

fn toggle_button(name: &str, label: &str, icon: &RowIcon) -> gtk::Button {
    let button = gtk::Button::builder()
        .name(name)
        .child(icon.widget())
        .focusable(false)
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// The add-session menu button: New Session and Open Directory….
fn add_session_menu(perform: &Rc<dyn Fn(FooterAction)>) -> gtk::MenuButton {
    let group = gio::SimpleActionGroup::new();
    for (name, action) in [
        ("new-session", FooterAction::NewSession),
        ("open-directory", FooterAction::OpenDirectory),
    ] {
        let entry = gio::SimpleAction::new(name, None);
        let run = Rc::clone(perform);
        entry.connect_activate(move |_, _| run(action));
        group.add_action(&entry);
    }
    let menu = gio::Menu::new();
    menu.append(Some("New Session"), Some("footer.new-session"));
    menu.append(Some("Open Directory…"), Some("footer.open-directory"));
    let button = gtk::MenuButton::builder()
        .icon_name("list-add-symbolic")
        .name("add-session")
        .tooltip_text("New Session")
        .menu_model(&menu)
        .focusable(false)
        .build();
    button.add_css_class("flat");
    button.update_property(&[gtk::accessible::Property::Label("Add session")]);
    button.insert_action_group("footer", Some(&group));
    button
}
