//! A page's popups, drawn by the app above the page (`WebKitGTK` drew them itself; an offscreen WPE page cannot): a
//! `<select>`'s rows under the select, and the context menu at the right-click. The context menu holds the items
//! `WebKitGTK`'s menu offers a page, minus those the overlay refuses (new windows, downloads, the inspector, opening a
//! link in place): Copy Link Address, Copy Image Address and Copy Media Address over a link, an image or media; Cut,
//! Copy, Paste and Select All in a field; Copy over selected text; else Back, Forward and Reload. A click outside a
//! popup or Escape closes it unanswered; the arrows move its highlight and Return picks, while it holds the page's keys.

use agterm_wpe::{ContextMenu, EditingCommand, MenuToken, OptionKind, OptionMenu};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Bounds, Context, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels,
    SharedString, StatefulInteractiveElement as _, Styled as _, anchored, deferred, div, point, px,
};

use super::panel::PanelColors;
use super::view::PageView;

/// What a context menu item does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Editing(EditingCommand),
    Back,
    Forward,
    Reload,
    /// Puts this text on the desktop's clipboard.
    CopyText(String),
}

/// One row of a popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub enabled: bool,
    /// A row of an option group, drawn indented.
    pub indented: bool,
    /// An `<optgroup>`'s heading, never picked.
    pub heading: bool,
    pub action: Action,
}

/// A page's popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Menu {
    Options {
        token: MenuToken,
        rows: Vec<Row>,
        /// The select's area in the page.
        anchor: (i32, i32, i32),
        highlighted: Option<usize>,
    },
    Context {
        items: Vec<Row>,
        at: (i32, i32),
        highlighted: Option<usize>,
    },
}

impl Menu {
    /// The popup of a `<select>`, the selected row highlighted.
    #[must_use]
    pub fn options(menu: &OptionMenu) -> Self {
        let rows: Vec<Row> = menu
            .items
            .iter()
            .map(|item| Row {
                label: item.label.clone(),
                enabled: item.enabled && item.kind != OptionKind::GroupLabel,
                indented: item.kind == OptionKind::GroupChild,
                heading: item.kind == OptionKind::GroupLabel,
                action: Action::Reload,
            })
            .collect();
        let highlighted = menu.items.iter().position(|item| item.selected);
        Self::Options {
            token: menu.token,
            rows,
            anchor: (menu.area.x, menu.area.y + menu.area.height, menu.area.width),
            highlighted,
        }
    }

    /// The context menu for what lies under a right-click, with the page's history (`can_go_back`, `can_go_forward`);
    /// `None` when it would hold nothing.
    #[must_use]
    pub fn context(menu: &ContextMenu, (can_go_back, can_go_forward): (bool, bool)) -> Option<Self> {
        let items = context_items(menu, can_go_back, can_go_forward);
        (!items.is_empty()).then_some(Self::Context {
            items,
            at: (menu.x, menu.y),
            highlighted: None,
        })
    }

    fn rows(&self) -> &[Row] {
        match self {
            Self::Options { rows, .. } | Self::Context { items: rows, .. } => rows,
        }
    }

    fn highlight(&mut self) -> &mut Option<usize> {
        match self {
            Self::Options { highlighted, .. } | Self::Context { highlighted, .. } => highlighted,
        }
    }

    /// The highlighted row, when it can be picked.
    #[must_use]
    pub fn highlighted(&self) -> Option<usize> {
        let highlighted = match self {
            Self::Options { highlighted, .. } | Self::Context { highlighted, .. } => *highlighted,
        }?;
        self.rows()
            .get(highlighted)
            .filter(|row| row.enabled)
            .map(|_| highlighted)
    }

    /// Moves the highlight to the next (`down`) or previous enabled row, staying put at either end.
    pub fn step(&mut self, down: bool) {
        let enabled: Vec<usize> = self
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.enabled)
            .map(|(index, _)| index)
            .collect();
        let current = *self.highlight();
        let next = match (current, down) {
            (None, true) => enabled.first().copied(),
            (None, false) => enabled.last().copied(),
            (Some(at), true) => enabled.iter().copied().find(|&index| index > at).or(Some(at)),
            (Some(at), false) => enabled.iter().copied().rev().find(|&index| index < at).or(Some(at)),
        };
        *self.highlight() = next;
    }
}

/// The context menu's items for `menu` (module docs).
fn context_items(menu: &ContextMenu, can_go_back: bool, can_go_forward: bool) -> Vec<Row> {
    let row = |label: &str, enabled: bool, action: Action| Row {
        label: label.to_owned(),
        enabled,
        indented: false,
        heading: false,
        action,
    };
    let mut items = Vec::new();
    if let Some(link) = &menu.link {
        items.push(row("Copy Link Address", true, Action::CopyText(link.clone())));
    }
    if let Some(image) = &menu.image {
        items.push(row("Copy Image Address", true, Action::CopyText(image.clone())));
    }
    if let Some(media) = &menu.media {
        items.push(row("Copy Media Address", true, Action::CopyText(media.clone())));
    }
    if menu.editable {
        items.push(row("Cut", menu.selection, Action::Editing(EditingCommand::Cut)));
        items.push(row("Copy", menu.selection, Action::Editing(EditingCommand::Copy)));
        items.push(row("Paste", true, Action::Editing(EditingCommand::Paste)));
        items.push(row("Select All", true, Action::Editing(EditingCommand::SelectAll)));
    } else if menu.selection {
        items.push(row("Copy", true, Action::Editing(EditingCommand::Copy)));
    }
    if items.is_empty() {
        items.push(row("Back", can_go_back, Action::Back));
        items.push(row("Forward", can_go_forward, Action::Forward));
        items.push(row("Reload", true, Action::Reload));
    }
    items
}

/// The popup drawn above everything at its place in the page laid out at `bounds`, in the page's `colors`.
pub fn element(menu: &Menu, bounds: Bounds<Pixels>, colors: &PanelColors, cx: &mut Context<PageView>) -> AnyElement {
    #[expect(clippy::cast_precision_loss, reason = "page coordinates are far below 2^24")]
    let (x, y, width) = match menu {
        Menu::Options { anchor, .. } => (anchor.0 as f32, anchor.1 as f32, anchor.2 as f32),
        Menu::Context { at, .. } => (at.0 as f32, at.1 as f32, 180.),
    };
    let highlighted = match menu {
        Menu::Options { highlighted, .. } | Menu::Context { highlighted, .. } => *highlighted,
    };
    let rows: Vec<AnyElement> = menu
        .rows()
        .iter()
        .enumerate()
        .map(|(index, row)| row_element(index, row, highlighted == Some(index), colors, cx))
        .collect();
    let panel = div()
        .id("html-popup")
        .debug_selector(|| "html-popup".to_owned())
        .occlude()
        .min_w(px(width.max(120.)))
        .max_h(px(360.))
        .overflow_y_scroll()
        .py(px(4.))
        .rounded(px(6.))
        .bg(colors.strip())
        .border_1()
        .border_color(colors.ink(0.18))
        .shadow_md()
        .text_size(px(13.))
        .text_color(colors.ink(1.0))
        .on_mouse_down_out(cx.listener(|view, _, _, cx| {
            view.close_menu();
            cx.notify();
        }))
        .children(rows);
    deferred(
        anchored()
            .position(bounds.origin + point(px(x), px(y)))
            .snap_to_window()
            .child(panel),
    )
    .with_priority(1)
    .into_any_element()
}

fn row_element(
    index: usize,
    row: &Row,
    highlighted: bool,
    colors: &PanelColors,
    cx: &mut Context<PageView>,
) -> AnyElement {
    let enabled = row.enabled;
    div()
        .id(SharedString::from(format!("html-popup-row-{index}")))
        .debug_selector(move || format!("html-popup-row-{index}"))
        .px(px(if row.indented { 22. } else { 10. }))
        .py(px(3.))
        .when(row.heading, |row| row.text_color(colors.ink(0.55)).text_size(px(12.)))
        .when(!enabled && !row.heading, |row| row.text_color(colors.ink(0.35)))
        .when(highlighted, |row| row.bg(colors.ink(0.12)))
        .when(enabled, |row| {
            row.hover(|style| style.bg(gpui_kit::hsla(0., 0., 0.5, 0.15)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |view, _, _, cx| view.pick(index, cx)))
        })
        .child(SharedString::from(row.label.clone()))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use agterm_wpe::{Area, OptionItem, ViewId};

    use super::*;

    fn hit() -> ContextMenu {
        ContextMenu {
            view: ViewId::from_raw(1),
            x: 5,
            y: 6,
            link: None,
            image: None,
            media: None,
            editable: false,
            selection: false,
        }
    }

    fn labels(menu: &Menu) -> Vec<(&str, bool)> {
        menu.rows()
            .iter()
            .map(|row| (row.label.as_str(), row.enabled))
            .collect()
    }

    #[test]
    fn a_plain_page_offers_its_history_and_reload() {
        let menu = Menu::context(&hit(), (true, false)).unwrap();
        assert_eq!(labels(&menu), [("Back", true), ("Forward", false), ("Reload", true)]);
    }

    #[test]
    fn a_link_a_field_and_a_selection_offer_their_own_items_and_nothing_refused() {
        let link = ContextMenu {
            link: Some("https://a.test/".to_owned()),
            ..hit()
        };
        let menu = Menu::context(&link, (true, true)).unwrap();
        assert_eq!(labels(&menu), [("Copy Link Address", true)]);
        assert_eq!(menu.rows()[0].action, Action::CopyText("https://a.test/".to_owned()));
        let field = ContextMenu {
            editable: true,
            ..hit()
        };
        assert_eq!(
            labels(&Menu::context(&field, (false, false)).unwrap()),
            [("Cut", false), ("Copy", false), ("Paste", true), ("Select All", true)]
        );
        let selected = ContextMenu {
            selection: true,
            ..hit()
        };
        assert_eq!(
            labels(&Menu::context(&selected, (false, false)).unwrap()),
            [("Copy", true)]
        );
        let all = Menu::context(
            &ContextMenu {
                link: Some("l".to_owned()),
                image: Some("i".to_owned()),
                media: Some("m".to_owned()),
                ..hit()
            },
            (false, false),
        )
        .unwrap();
        for refused in [
            "Open Link",
            "Open Link in New Window",
            "Download Linked File",
            "Inspect Element",
        ] {
            assert!(all.rows().iter().all(|row| row.label != refused), "{refused}");
        }
    }

    #[test]
    fn a_select_popup_highlights_its_selected_row_and_steps_over_disabled_ones() {
        let item = |label: &str, kind, enabled, selected| OptionItem {
            label: label.to_owned(),
            tooltip: None,
            kind,
            enabled,
            selected,
        };
        let mut menu = Menu::options(&OptionMenu {
            token: MenuToken::from_raw(3),
            view: ViewId::from_raw(1),
            area: Area {
                x: 10,
                y: 20,
                width: 100,
                height: 30,
            },
            items: vec![
                item("one", OptionKind::Option, true, true),
                item("group", OptionKind::GroupLabel, false, false),
                item("two", OptionKind::GroupChild, false, false),
                item("three", OptionKind::GroupChild, true, false),
            ],
        });
        assert_eq!(menu.highlighted(), Some(0));
        let Menu::Options { anchor, .. } = &menu else {
            unreachable!()
        };
        assert_eq!(*anchor, (10, 50, 100), "the popup opens under the select");
        menu.step(true);
        assert_eq!(
            menu.highlighted(),
            Some(3),
            "the heading and the disabled row are stepped over"
        );
        menu.step(true);
        assert_eq!(menu.highlighted(), Some(3), "it stays at the end");
        menu.step(false);
        assert_eq!(menu.highlighted(), Some(0));
        assert!(menu.rows()[1].heading && menu.rows()[2].indented);
    }
}
