//! The lead cover over a live pane whose attach client does not lead (upstream's `Views/PaneLeadCover.swift` and the
//! key handling of `Ghostty/PaneLead.swift`; ported from the GTK build's `pane_lead.rs`): the terminal draws output
//! laid out for another client's grid, so the pane shows an opaque cover saying so, and a key press on it takes the
//! pane over in a fresh terminal. A click on the cover gives the pane the keyboard, so the next press can take it over.
//!
//! The core decides what a cover says ([`Core::pane_lead_cover`]). A remote pane waiting to be attached again holds
//! the keyboard too (a key press retries now) and shows what ssh said on its last failed probe, uncovered (upstream's
//! `RemoteReconnectNote`). The cover takes the terminal's colors, as upstream's (`resolvedTerminalColor`,
//! `resolvedChromeText`).

use agterm_core::chrome::Rgb;
use agterm_core::{Core, LeadCover};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, FocusHandle, FontWeight, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, PathBuilder, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    canvas, div, point, px,
};

use crate::mute::color;
use crate::split::Slot;

/// What the cover asks for under its title while no takeover is on its way.
pub const TAKEOVER_HINT: &str = "Press any key to use it here";

/// What one pane needs over its terminal now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneCover {
    pub cover: Option<LeadCover>,
    /// Key presses go to the core rather than the terminal: covered, or waiting to be attached again.
    pub holds_keys: bool,
    /// What ssh said on the last failed reconnect probe.
    pub note: Option<String>,
}

impl PaneCover {
    /// Whether anything shows over the pane while it is laid out and the page is on screen.
    #[must_use]
    pub fn shows(&self) -> bool {
        self.cover.is_some() || self.note.is_some()
    }
}

/// What the pane carrying `token` needs now, from the core's lead book and reconnect book.
#[must_use]
pub fn cover(core: &Core, token: &str) -> PaneCover {
    let cover = core.pane_lead_cover(token);
    let holds_keys = cover.is_some() || core.reconnect_waiting(token);
    let note = core.pane_reconnect_note(token);
    PaneCover {
        cover,
        holds_keys,
        note,
    }
}

/// The pane role an element id names: `left` or `right`.
fn role(slot: Slot) -> &'static str {
    match slot {
        Slot::Split => "right",
        Slot::Primary | Slot::Scratch => "left",
    }
}

/// The cover drawn over the pane at `slot` for `cover` (`None` when nothing shows), in the terminal's `background` and
/// `foreground`: the cover takes the pointer and a click focuses `terminal`; a lone note takes nothing.
#[must_use]
pub fn element(
    cover: &PaneCover,
    slot: Slot,
    background: Rgb,
    foreground: Rgb,
    terminal: &FocusHandle,
) -> Option<AnyElement> {
    if !cover.shows() {
        return None;
    }
    let id = SharedString::from(format!("pane-lead-cover-{}", role(slot)));
    let mut root = div()
        .id(id.clone())
        .accessibility_id(id)
        .role(gpui_kit::Role::Group)
        .size_full()
        .relative();
    if let Some(lead) = &cover.cover {
        let focus = terminal.clone();
        root = root
            .bg(color(background, 1.0))
            .occlude()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&focus, cx);
                cx.stop_propagation();
            })
            .child(card(lead, foreground));
    }
    if let Some(note) = &cover.note {
        root = root.child(reconnect_note(note, slot));
    }
    Some(root.into_any_element())
}

/// The cover's centered column: the icon, the title and, unless a takeover is on its way, the hint.
fn card(lead: &LeadCover, foreground: Rgb) -> impl IntoElement {
    let text = color(foreground, 1.0);
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(16.))
        .text_color(text)
        .child(icon(foreground))
        .child(
            div()
                .text_size(px(14.))
                .font_weight(FontWeight::MEDIUM)
                .child(lead.title),
        )
        .when_some(lead.offers_takeover.then_some(TAKEOVER_HINT), |column, hint| {
            column.child(div().text_size(px(12.)).opacity(0.7).child(hint))
        })
}

/// Upstream's `rectangle.on.rectangle.slash`, drawn: two overlapping frames struck through.
fn icon(foreground: Rgb) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            let stroke = color(foreground, 1.0);
            let origin = bounds.origin;
            let frame = |x: f32, y: f32, width: f32, height: f32| {
                let mut path = PathBuilder::stroke(px(1.5));
                path.move_to(origin + point(px(x), px(y)));
                path.line_to(origin + point(px(x + width), px(y)));
                path.line_to(origin + point(px(x + width), px(y + height)));
                path.line_to(origin + point(px(x), px(y + height)));
                path.close();
                path
            };
            for path in [frame(2., 2., 18., 13.), frame(8., 9., 18., 13.)] {
                if let Ok(path) = path.build() {
                    window.paint_path(path, stroke);
                }
            }
            let mut slash = PathBuilder::stroke(px(1.5));
            slash.move_to(origin + point(px(1.), px(1.)));
            slash.line_to(origin + point(px(27.), px(23.)));
            if let Ok(slash) = slash.build() {
                window.paint_path(slash, stroke);
            }
        },
    )
    .w(px(28.))
    .h(px(24.))
}

/// What ssh said, in a strip along the pane's bottom (upstream's `RemoteReconnectNote`): never takes the pointer.
fn reconnect_note(reason: &str, slot: Slot) -> impl IntoElement {
    div().absolute().left_0().right_0().bottom_0().child(
        div()
            .id(SharedString::from(format!("remote-reconnect-note-{}", role(slot))))
            .accessibility_id(SharedString::from(format!("remote-reconnect-note-{}", role(slot))))
            .role(gpui_kit::Role::Status)
            .w_full()
            .px(px(8.))
            .py(px(2.))
            .bg(color(Rgb::new(0xff, 0xff, 0x00), 1.0))
            .text_color(color(Rgb::BLACK, 1.0))
            .text_size(px(12.))
            .font_family(crate::terminal::DEFAULT_FAMILY)
            .whitespace_nowrap()
            .overflow_hidden()
            .text_ellipsis()
            .child(format!("ssh: {reason}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cover_shows_while_covered_or_noted() {
        assert!(!PaneCover::default().shows());
        let covered = PaneCover {
            cover: Some(LeadCover {
                title: "In use from another machine",
                offers_takeover: true,
            }),
            holds_keys: true,
            note: None,
        };
        assert!(covered.shows());
        let noted = PaneCover {
            note: Some("Connection refused".to_owned()),
            ..PaneCover::default()
        };
        assert!(noted.shows() && !noted.holds_keys);
    }

    #[test]
    fn element_ids_name_the_pane_role() {
        assert_eq!(role(Slot::Primary), "left");
        assert_eq!(role(Slot::Split), "right");
    }
}
