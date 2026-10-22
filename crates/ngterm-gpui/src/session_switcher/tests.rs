//! The walk (ported from the GTK build's `session_switcher.rs` tests) and the keys it reads; the walk through a
//! window's keys and its Control release is in `window/tests.rs`.

use gpui_kit::{Keystroke, TestAppContext};

use super::*;
use crate::terminal::input::Source;

fn ids(count: usize) -> Vec<Uuid> {
    (0..count).map(|_| Uuid::new_v4()).collect()
}

#[test]
fn a_walk_needs_two_sessions_and_starts_on_the_previous_one() {
    let mut switcher = Switcher::default();
    assert!(!switcher.begin(ids(1)));
    assert!(!switcher.is_active());
    let order = ids(3);
    assert!(switcher.begin(order.clone()));
    assert_eq!(switcher.index(), 1);
    assert_eq!(switcher.commit(), Some(order[1]));
    assert!(!switcher.is_active(), "the commit ends the walk");
    assert_eq!(switcher.commit(), None);
}

#[test]
fn the_highlight_wraps_both_ways_and_a_reset_selects_nothing() {
    let mut switcher = Switcher::default();
    let order = ids(3);
    switcher.begin(order.clone());
    switcher.advance(false);
    assert_eq!(switcher.index(), 2);
    switcher.advance(false);
    assert_eq!(switcher.index(), 0, "wraps forward");
    switcher.advance(true);
    assert_eq!(switcher.index(), 2, "wraps back");
    switcher.advance(true);
    assert_eq!(switcher.commit(), Some(order[1]));
    switcher.begin(order);
    switcher.reset();
    assert_eq!(switcher.commit(), None);
}

fn key(source: &str) -> Option<SwitchKey> {
    let keystroke = Keystroke::parse(source).unwrap();
    switch_key(&KeyStroke::new(&keystroke, &Source::Keystroke, false))
}

#[test]
fn ctrl_tab_walks_ctrl_shift_tab_walks_back_and_plain_tab_is_typing() {
    assert_eq!(key("ctrl-tab"), Some(SwitchKey::Tab { reverse: false }));
    assert_eq!(key("ctrl-shift-tab"), Some(SwitchKey::Tab { reverse: true }));
    assert_eq!(key("tab"), None);
    assert_eq!(key("escape"), Some(SwitchKey::Escape));
    assert_eq!(key("ctrl-a"), None);
}

fn candidate(title: &str) -> Candidate {
    Candidate {
        id: Uuid::new_v4(),
        title: title.to_owned(),
        subtitle: "work · ~".to_owned(),
        remote: false,
    }
}

#[gpui_kit::test]
fn a_walk_freezes_its_rows_and_shows_until_it_ends(cx: &mut TestAppContext) {
    let (switcher, cx) =
        cx.add_window_view(|_, _| SessionSwitcher::new(Uuid::new_v4(), crate::state::detached_poster()));
    let shown = |cx: &mut gpui_kit::VisualTestContext| {
        cx.run_until_parked();
        cx.debug_bounds("session-switcher").is_some()
    };
    let walk = Switcher::default();
    assert!(!walk.is_active());
    switcher.update(cx, |switcher, cx| {
        switcher.set_candidates(vec![candidate("a"), candidate("b"), candidate("c")], false);
        assert!(
            switcher.press(SwitchKey::Tab { reverse: false }, cx),
            "consumed while the window takes no UI actions"
        );
        assert!(!switcher.is_active());
        switcher.set_candidates(vec![candidate("a"), candidate("b"), candidate("c")], true);
        assert!(!switcher.press(SwitchKey::Escape, cx), "Escape outside a walk goes on");
        switcher.press(SwitchKey::Tab { reverse: false }, cx);
        // a sync during the walk does not reorder it
        switcher.set_candidates(vec![candidate("z")], true);
        switcher.press(SwitchKey::Tab { reverse: false }, cx);
    });
    assert!(shown(cx), "the panel shows during the walk");
    switcher.read_with(cx, |switcher, _| {
        assert_eq!(switcher.titles(), ["a", "b", "c"], "frozen");
        assert_eq!(switcher.index(), 2);
    });
    switcher.update(cx, |switcher, cx| {
        assert!(switcher.press(SwitchKey::Escape, cx));
    });
    assert!(!shown(cx), "Escape ends it");
}
