//! The agent-status rules, ported from upstream's `AgentStatusTests` and `AppStoreStatusTests`: keystroke clearing
//! under each reset mode, the effective sound, the shape and color vocabulary, the pane precedence over a blocked
//! status, and the pane-scoped clear `session.type` runs.

use ngterm_proto::EventKind;
use uuid::Uuid;

use super::support::add;
use crate::model::{
    AgentIndicator, AgentStatus, AppStore, StatusKeystroke, StatusPane, StatusReset, StatusShape, StatusWriteResult,
    is_known_sound, is_valid_color_hex,
};
use crate::persist::AppSettings;

use AgentStatus::{Active, Blocked, Completed, Idle};
use StatusKeystroke::{Interrupt, Other, Submit};
use StatusReset::{Enter, FirstKey, Never};

fn indicator(status: AgentStatus, pane: Option<StatusPane>) -> AgentIndicator {
    AgentIndicator {
        status,
        status_pane: pane,
        ..AgentIndicator::default()
    }
}

/// A store with one session.
fn one_session() -> (AppStore, Uuid) {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let id = add(&mut store, workspace, "/repo");
    (store, id)
}

/// A split session whose LEFT pane holds a blocked status, the two-agent shape the precedence rule exists for.
fn blocked_left_split_session() -> (AppStore, Uuid) {
    let (mut store, id) = one_session();
    store.session_mut(id).unwrap().split.exists = true;
    store.apply_control_status(indicator(Blocked, Some(StatusPane::Left)), id);
    (store, id)
}

fn status(store: &AppStore, id: Uuid) -> &AgentIndicator {
    &store.session(id).unwrap().agent_indicator
}

#[test]
fn cleared_by_keystroke_follows_the_reset_mode_for_attention_and_interrupt_for_active() {
    let table = [
        (Blocked, Other, FirstKey, true),
        (Completed, Other, FirstKey, true),
        (Completed, Submit, FirstKey, true),
        (Completed, Interrupt, FirstKey, true),
        (Blocked, Other, Enter, false),
        (Completed, Other, Enter, false),
        (Completed, Submit, Enter, true),
        (Blocked, Submit, Enter, true),
        (Completed, Interrupt, Enter, false),
        (Blocked, Interrupt, Enter, true),
        (Blocked, Interrupt, Never, false),
        (Blocked, Other, Never, false),
        (Completed, Submit, Never, false),
        (Completed, Interrupt, Never, false),
        (Active, Other, FirstKey, false),
        (Active, Submit, FirstKey, false),
        (Active, Interrupt, FirstKey, true),
        (Active, Interrupt, Enter, true),
        (Active, Interrupt, Never, true),
        (Idle, Interrupt, FirstKey, false),
        (Idle, Submit, Enter, false),
    ];
    for (status, keystroke, reset, cleared) in table {
        assert_eq!(
            status.cleared_by(keystroke, reset),
            cleared,
            "{status:?} {keystroke:?} {reset:?}"
        );
    }
}

#[test]
fn an_indicator_clears_only_from_the_owning_pane_under_the_reset_mode() {
    let right = indicator(Completed, Some(StatusPane::Right));
    assert!(right.cleared_by(StatusPane::Right, Other, FirstKey));
    assert!(!right.cleared_by(StatusPane::Left, Other, FirstKey));
    assert!(!right.cleared_by(StatusPane::Right, Other, Enter));
    assert!(right.cleared_by(StatusPane::Right, Submit, Enter));
    assert!(!right.cleared_by(StatusPane::Right, Submit, Never));
    let untagged = indicator(Blocked, None);
    assert!(untagged.cleared_by(StatusPane::Left, Other, FirstKey));
    assert!(!untagged.cleared_by(StatusPane::Right, Other, FirstKey));
}

#[test]
fn effective_sound_prefers_the_per_call_one_and_defaults_only_for_blocked() {
    assert_eq!(Blocked.effective_sound(Some("Glass"), Some("Sosumi")), Some("Glass"));
    assert_eq!(Active.effective_sound(Some("Glass"), Some("Sosumi")), Some("Glass"));
    assert_eq!(Blocked.effective_sound(None, Some("Sosumi")), Some("Sosumi"));
    for status in [Active, Completed, Idle] {
        assert_eq!(status.effective_sound(None, Some("Sosumi")), None, "{status:?}");
    }
    assert_eq!(Blocked.effective_sound(Some(""), Some("Sosumi")), Some("Sosumi"));
    assert_eq!(Blocked.effective_sound(None, Some("")), None);
    assert_eq!(Blocked.effective_sound(None, None), None);
}

#[test]
fn wire_vocabularies_round_trip() {
    for status in [Idle, Active, Completed, Blocked] {
        assert_eq!(AgentStatus::from_wire(status.wire_name()), Some(status));
    }
    assert_eq!(AgentStatus::from_wire("busy"), None);
    for shape in StatusShape::ALL {
        assert_eq!(StatusShape::from_wire(shape.wire_name()), Some(shape));
    }
    assert_eq!(StatusShape::from_wire("hexagon"), None);
    assert_eq!(
        StatusShape::valid_names_list(),
        "circle|square|triangle|diamond|capsule|star"
    );
    assert!(
        indicator(Blocked, None)
            != AgentIndicator {
                shape: Some(StatusShape::Star),
                ..indicator(Blocked, None)
            },
        "the shape takes part in equality, so a shape-only change emits"
    );
}

#[test]
fn the_status_reset_setting_resolves_unknown_values_to_first_key() {
    let reset = |raw: Option<&str>| {
        AppSettings {
            status_reset: raw.map(str::to_owned),
            ..AppSettings::default()
        }
        .effective_status_reset()
    };
    assert_eq!(reset(None), FirstKey);
    assert_eq!(reset(Some("enter")), Enter);
    assert_eq!(reset(Some("never")), Never);
    assert_eq!(reset(Some("firstKey")), FirstKey);
    assert_eq!(reset(Some("later")), FirstKey);
}

#[test]
fn colors_and_sounds() {
    for valid in ["#ff8800", "FF8800", "#AbCdEf"] {
        assert!(is_valid_color_hex(valid), "{valid}");
    }
    for invalid in [
        "",
        "#",
        "orange",
        "#ff880",
        "#ff88000",
        "##ff8800",
        "#ff88zz",
        "#ｆｆ8800",
    ] {
        assert!(!is_valid_color_hex(invalid), "{invalid}");
    }
    for known in ["default", "beep", "Basso", "Glass"] {
        assert!(is_known_sound(known), "{known}");
    }
    for unknown in ["", "glass", "Gong"] {
        assert!(!is_known_sound(unknown), "{unknown}");
    }
}

// MARK: - pane precedence over a blocked status

#[test]
fn the_right_pane_cannot_replace_the_left_panes_block() {
    for replacement in [Active, Completed, Idle] {
        let (mut store, id) = blocked_left_split_session();
        let stamp = store.session(id).unwrap().status_changed_at;
        let result = store.apply_control_status(indicator(replacement, Some(StatusPane::Right)), id);
        assert_eq!(
            result,
            StatusWriteResult::Refused {
                owner: StatusPane::Left
            }
        );
        assert_eq!(*status(&store, id), indicator(Blocked, Some(StatusPane::Left)));
        assert_eq!(store.session(id).unwrap().status_changed_at, stamp);
    }
}

#[test]
fn the_owning_pane_changes_its_own_block() {
    for replacement in [Active, Completed, Idle] {
        let (mut store, id) = blocked_left_split_session();
        let result = store.apply_control_status(indicator(replacement, Some(StatusPane::Left)), id);
        assert_eq!(result, StatusWriteResult::Applied);
        assert_eq!(status(&store, id).status, replacement);
    }
}

#[test]
fn an_untagged_writer_counts_as_the_left_pane() {
    let (mut store, id) = blocked_left_split_session();
    assert_eq!(
        store.apply_control_status(indicator(Active, None), id),
        StatusWriteResult::Applied
    );
    assert_eq!(status(&store, id).status, Active);
}

#[test]
fn the_other_panes_own_block_replaces_the_standing_one() {
    let (mut store, id) = blocked_left_split_session();
    assert_eq!(
        store.apply_control_status(indicator(Blocked, Some(StatusPane::Right)), id),
        StatusWriteResult::Applied
    );
    assert_eq!(status(&store, id).status_pane, Some(StatusPane::Right));
}

#[test]
fn a_stale_right_tag_on_a_splitless_session_owns_the_block_it_set() {
    let (mut store, id) = one_session();
    store.apply_control_status(indicator(Blocked, Some(StatusPane::Right)), id);
    assert_eq!(status(&store, id).status_pane, Some(StatusPane::Left));
    assert_eq!(
        store.apply_control_status(indicator(Active, Some(StatusPane::Right)), id),
        StatusWriteResult::Applied
    );
    assert_eq!(status(&store, id).status, Active);
}

#[test]
fn a_refused_write_emits_no_status_event() {
    let (mut store, id) = blocked_left_split_session();
    let emitted = |store: &AppStore| {
        store
            .outbox()
            .events
            .iter()
            .filter(|event| event.kind == EventKind::Status)
            .count()
    };
    assert_eq!(emitted(&store), 1, "the accepted write reached the outbox");
    store.apply_control_status(indicator(Active, Some(StatusPane::Right)), id);
    assert_eq!(emitted(&store), 1);
}

// MARK: - typed input

#[test]
fn typed_input_clears_its_own_panes_status_by_the_texts_classification() {
    let (mut store, id) = one_session();
    store.set_agent_indicator(indicator(Blocked, None), id);
    store.note_typed_input(id, StatusPane::Left, "", FirstKey);
    assert_eq!(status(&store, id).status, Blocked, "empty text clears nothing");
    store.note_typed_input(id, StatusPane::Left, "yes", Enter);
    assert_eq!(status(&store, id).status, Blocked, "plain typing under submit-only");
    store.note_typed_input(id, StatusPane::Left, "yes\r\n", Enter);
    assert_eq!(*status(&store, id), AgentIndicator::default(), "a newline submits");

    store.set_agent_indicator(indicator(Completed, None), id);
    store.note_typed_input(id, StatusPane::Scratch, "x", FirstKey);
    assert_eq!(status(&store, id).status, Completed, "another pane's status survives");
    store.note_typed_input(id, StatusPane::Left, "x", FirstKey);
    assert_eq!(status(&store, id).status, Idle);

    store.set_agent_indicator(indicator(Active, None), id);
    store.note_typed_input(id, StatusPane::Left, "\u{1b}\n", FirstKey);
    assert_eq!(status(&store, id).status, Active, "typed text is never an interrupt");
    store.set_agent_indicator(indicator(Blocked, None), id);
    store.note_typed_input(id, StatusPane::Left, "x\n", Never);
    assert_eq!(status(&store, id).status, Blocked);
    store.note_typed_input(Uuid::new_v4(), StatusPane::Left, "x\n", FirstKey);
}

#[test]
fn a_keystroke_clear_reports_whether_it_cleared() {
    let (mut store, id) = one_session();
    store.set_agent_indicator(indicator(Active, Some(StatusPane::Scratch)), id);
    assert!(!store.clear_status_on_input(id, StatusPane::Scratch, Other, FirstKey));
    assert!(store.clear_status_on_input(id, StatusPane::Scratch, Interrupt, Never));
    assert_eq!(status(&store, id).status, Idle);
}
