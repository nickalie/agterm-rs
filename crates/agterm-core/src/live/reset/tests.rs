//! Upstream's `LiveResetTests.swift`.

use super::*;
use crate::live::inventory::{OwnerWindowState, PaneClaim, PaneRole};

const SESSION_A: Uuid = Uuid::from_u128(0xa);
const SESSION_B: Uuid = Uuid::from_u128(0xb);
const PANE_A: Uuid = Uuid::from_u128(0xa1);
const PANE_A_SPLIT: Uuid = Uuid::from_u128(0xa2);
const PANE_B: Uuid = Uuid::from_u128(0xb1);
const CUTOFF: u64 = 1000;

fn claim(pane: Uuid, role: PaneRole, session: Uuid) -> PaneClaim {
    PaneClaim {
        pane_identity: pane,
        pane: role,
        pending_close: false,
        window_id: Uuid::from_u128(0xc0),
        window_name: Some("w".to_owned()),
        window_state: OwnerWindowState::Open,
        workspace_id: None,
        workspace_name: Some("default".to_owned()),
        session_id: session,
        session_name: Some("build".to_owned()),
    }
}

fn walk(claims: Vec<PaneClaim>, complete: bool) -> ClaimWalk {
    ClaimWalk { claims, complete }
}

fn record(pane: Uuid, leader: Option<i32>, created: Option<u64>) -> LiveSessionRecord {
    LiveSessionRecord {
        name: daemon_name(pane),
        clients: Some(0),
        leader_pid: leader,
        created_at: created,
    }
}

fn target(pane: Uuid, session: Uuid, leader: i32, reason: Reason) -> Target {
    Target {
        pane_identity: pane,
        session_id: session,
        daemon: daemon_name(pane),
        leader_pid: leader,
        reason,
    }
}

fn unsupervised(pane: Uuid, leader: i32) -> Target {
    target(pane, SESSION_A, leader, Reason::Unsupervised)
}

fn marker(targets: Vec<Target>) -> Marker {
    Marker {
        version: Marker::CURRENT_VERSION,
        created_at: 0.0,
        targets,
    }
}

fn always(attribution: Attribution) -> impl Fn(&str, i32) -> Attribution {
    move |_, _| attribution
}

#[test]
fn select_keeps_orphaned_and_app_panes_only() {
    let (pane_c, pane_d) = (Uuid::from_u128(0xc1), Uuid::from_u128(0xd1));
    let claims = walk(
        vec![
            claim(PANE_A, PaneRole::Left, SESSION_A),
            claim(PANE_B, PaneRole::Left, SESSION_B),
            claim(pane_c, PaneRole::Left, SESSION_A),
            claim(pane_d, PaneRole::Left, SESSION_A),
        ],
        true,
    );
    let records = [
        record(PANE_A, Some(10), None),
        record(PANE_B, Some(11), None),
        record(pane_c, Some(12), None),
        record(pane_d, Some(13), None),
    ];
    let table = BTreeMap::from([
        (daemon_name(PANE_A), Attribution::Orphaned),
        (daemon_name(PANE_B), Attribution::App),
        (daemon_name(pane_c), Attribution::Supervisor),
        (daemon_name(pane_d), Attribution::Unknown),
    ]);
    let selection = select(&claims, &records, None, |name, _| table[name]);
    assert_eq!(
        selection.targets,
        [
            unsupervised(PANE_A, 10),
            target(PANE_B, SESSION_B, 11, Reason::Unsupervised)
        ]
    );
    assert!(selection.inventory_complete);
    assert_eq!(selection.session_count(), 2);
}

#[test]
fn select_takes_a_pane_created_before_the_cutoff_as_outdated_whatever_its_attribution() {
    for attribution in [
        Attribution::Supervisor,
        Attribution::Unknown,
        Attribution::Orphaned,
        Attribution::App,
    ] {
        let claims = walk(vec![claim(PANE_A, PaneRole::Left, SESSION_A)], true);
        let selection = select(
            &claims,
            &[record(PANE_A, Some(10), Some(999))],
            Some(CUTOFF),
            always(attribution),
        );
        assert_eq!(selection.targets, [target(PANE_A, SESSION_A, 10, Reason::Outdated)]);
    }
}

#[test]
fn select_leaves_a_current_or_undated_supervised_pane_alone() {
    for (created, cutoff) in [
        (Some(1000), Some(CUTOFF)),
        (Some(1500), Some(CUTOFF)),
        (None, Some(CUTOFF)),
        (Some(999), None),
    ] {
        let claims = walk(vec![claim(PANE_A, PaneRole::Left, SESSION_A)], true);
        let selection = select(
            &claims,
            &[record(PANE_A, Some(10), created)],
            cutoff,
            always(Attribution::Supervisor),
        );
        assert!(selection.targets.is_empty());
    }
    let claims = walk(vec![claim(PANE_A, PaneRole::Left, SESSION_A)], true);
    let current = select(
        &claims,
        &[record(PANE_A, Some(10), Some(1000))],
        Some(CUTOFF),
        always(Attribution::Orphaned),
    );
    assert_eq!(current.targets, [unsupervised(PANE_A, 10)]);
}

#[test]
fn select_excludes_panes_without_a_readable_leader() {
    let claims = walk(
        vec![
            claim(PANE_A, PaneRole::Left, SESSION_A),
            claim(PANE_B, PaneRole::Left, SESSION_A),
        ],
        false,
    );
    let selection = select(
        &claims,
        &[record(PANE_A, None, None)],
        None,
        always(Attribution::Orphaned),
    );
    assert!(selection.targets.is_empty());
    assert!(!selection.inventory_complete);
}

#[test]
fn select_counts_a_split_session_once_and_rejects_a_pane_claimed_twice() {
    let split = walk(
        vec![
            claim(PANE_A, PaneRole::Left, SESSION_A),
            claim(PANE_A_SPLIT, PaneRole::Right, SESSION_A),
        ],
        true,
    );
    let records = [record(PANE_A, Some(10), None), record(PANE_A_SPLIT, Some(20), None)];
    let selection = select(&split, &records, None, always(Attribution::Orphaned));
    assert_eq!((selection.targets.len(), selection.session_count()), (2, 1));
    let twice = walk(
        vec![
            claim(PANE_A, PaneRole::Left, SESSION_A),
            claim(PANE_A, PaneRole::Left, SESSION_B),
        ],
        true,
    );
    let selection = select(&twice, &records[..1], None, always(Attribution::Orphaned));
    assert_eq!(selection.targets, [unsupervised(PANE_A, 10)]);
    assert!(!selection.inventory_complete);
}

#[test]
fn the_outdated_session_count_counts_a_split_session_once() {
    let selection = Selection {
        targets: vec![
            target(PANE_A, SESSION_A, 10, Reason::Outdated),
            target(PANE_A_SPLIT, SESSION_A, 11, Reason::Outdated),
            target(PANE_B, SESSION_B, 12, Reason::Unsupervised),
        ],
        inventory_complete: true,
    };
    assert_eq!((selection.session_count(), selection.outdated_session_count()), (2, 1));
}

fn claimed(panes: &[Uuid]) -> BTreeSet<Uuid> {
    panes.iter().copied().collect()
}

#[test]
fn narrow_kills_only_the_same_leader_still_orphaned() {
    let target = unsupervised(PANE_A, 10);
    let narrowed = narrow(
        &marker(vec![target.clone()]),
        Some(&claimed(&[PANE_A])),
        Some(&[record(PANE_A, Some(10), None)]),
        None,
        always(Attribution::Orphaned),
    );
    assert_eq!(
        narrowed.dispositions,
        BTreeMap::from([(target.clone(), Disposition::Kill)])
    );
    assert_eq!(narrowed.kill(), [target]);
    assert!(!narrowed.inventory_failed);
}

#[test]
fn narrow_kills_an_outdated_target_whatever_its_attribution_and_skips_a_reattributed_unsupervised_one() {
    for attribution in [Attribution::Supervisor, Attribution::Unknown] {
        let records = [record(PANE_A, Some(10), Some(999))];
        let outdated = target(PANE_A, SESSION_A, 10, Reason::Outdated);
        let narrowed = narrow(
            &marker(vec![outdated.clone()]),
            Some(&claimed(&[PANE_A])),
            Some(&records),
            Some(CUTOFF),
            always(attribution),
        );
        assert_eq!(narrowed.kill(), [outdated]);
        let current = unsupervised(PANE_A, 10);
        let narrowed = narrow(
            &marker(vec![current.clone()]),
            Some(&claimed(&[PANE_A])),
            Some(&records),
            Some(CUTOFF),
            always(attribution),
        );
        assert_eq!(narrowed.dispositions[&current], Disposition::Skipped);
    }
}

#[test]
fn narrow_skips_an_outdated_target_that_no_longer_qualifies() {
    for (is_claimed, leader, created, cutoff) in [
        (false, Some(10), Some(999), Some(CUTOFF)),
        (true, Some(11), Some(999), Some(CUTOFF)),
        (true, Some(10), Some(1000), Some(CUTOFF)),
        (true, Some(10), None, Some(CUTOFF)),
        (true, Some(10), Some(999), None),
    ] {
        let target = target(PANE_A, SESSION_A, 10, Reason::Outdated);
        let panes = if is_claimed {
            claimed(&[PANE_A])
        } else {
            BTreeSet::new()
        };
        let narrowed = narrow(
            &marker(vec![target.clone()]),
            Some(&panes),
            Some(&[record(PANE_A, leader, created)]),
            cutoff,
            always(Attribution::Orphaned),
        );
        assert_eq!(narrowed.dispositions[&target], Disposition::Skipped);
    }
}

#[test]
fn narrow_skips_unclaimed_changed_unreadable_or_reattributed_targets() {
    for (is_claimed, leader, attribution) in [
        (false, Some(10), Attribution::Orphaned),
        (true, Some(11), Attribution::Orphaned),
        (true, None, Attribution::Orphaned),
        (true, Some(10), Attribution::Supervisor),
        (true, Some(10), Attribution::App),
        (true, Some(10), Attribution::Unknown),
    ] {
        let target = unsupervised(PANE_A, 10);
        let panes = if is_claimed {
            claimed(&[PANE_A])
        } else {
            BTreeSet::new()
        };
        let narrowed = narrow(
            &marker(vec![target.clone()]),
            Some(&panes),
            Some(&[record(PANE_A, leader, None)]),
            None,
            always(attribution),
        );
        assert_eq!(narrowed.dispositions[&target], Disposition::Skipped);
        assert!(narrowed.kill().is_empty());
    }
}

#[test]
fn narrow_marks_a_missing_daemon_gone_and_never_adds_one() {
    let target = unsupervised(PANE_A, 10);
    let gone = narrow(
        &marker(vec![target.clone()]),
        Some(&claimed(&[PANE_A])),
        Some(&[]),
        None,
        always(Attribution::Orphaned),
    );
    assert_eq!(gone.dispositions[&target], Disposition::Gone);
    assert!(gone.kill().is_empty());
    let narrowed = narrow(
        &marker(vec![target.clone()]),
        Some(&claimed(&[PANE_A, PANE_B])),
        Some(&[record(PANE_A, Some(10), None), record(PANE_B, Some(11), None)]),
        None,
        always(Attribution::Orphaned),
    );
    assert_eq!((narrowed.kill(), narrowed.dispositions.len()), (vec![target], 1));
}

#[test]
fn narrow_without_a_listing_or_claims_kills_nothing() {
    let target = unsupervised(PANE_A, 10);
    let unlisted = narrow(
        &marker(vec![target.clone(), target.clone()]),
        Some(&claimed(&[PANE_A])),
        None,
        None,
        always(Attribution::Orphaned),
    );
    assert!(unlisted.inventory_failed);
    assert_eq!(
        unlisted.dispositions,
        BTreeMap::from([(target.clone(), Disposition::Skipped)])
    );
    let unclaimed = narrow(
        &marker(vec![target.clone()]),
        None,
        Some(&[record(PANE_A, Some(10), None)]),
        None,
        always(Attribution::Orphaned),
    );
    assert!(!unclaimed.inventory_failed);
    assert_eq!(unclaimed.dispositions[&target], Disposition::Skipped);
}

fn narrowed(dispositions: &[(Target, Disposition)]) -> Narrowed {
    Narrowed {
        dispositions: dispositions.iter().cloned().collect(),
        inventory_failed: false,
    }
}

#[test]
fn the_outcome_counts_a_session_reset_when_every_pane_is_confirmed_or_gone() {
    let b = target(PANE_B, SESSION_B, 30, Reason::Unsupervised);
    let narrowed = narrowed(&[
        (unsupervised(PANE_A, 10), Disposition::Kill),
        (unsupervised(PANE_A_SPLIT, 20), Disposition::Gone),
        (b, Disposition::Kill),
    ]);
    let outcome = outcome(&narrowed, &BTreeSet::new(), false);
    assert_eq!(
        outcome.panes,
        PaneCounts {
            confirmed: 3,
            killed: 2,
            gone: 1,
            skipped: 0
        }
    );
    assert!(outcome.unconfirmed.is_empty());
    assert_eq!(
        outcome.sessions,
        SessionCounts {
            affected: 2,
            reset: 2,
            partial: 0,
            unconfirmed: 0
        }
    );
    assert_eq!(notification_text(&outcome), None);
}

#[test]
fn the_outcome_counts_a_split_session_with_one_survivor_as_one_partial_session() {
    let narrowed = narrowed(&[
        (unsupervised(PANE_A, 10), Disposition::Kill),
        (unsupervised(PANE_A_SPLIT, 20), Disposition::Kill),
    ]);
    let outcome = outcome(&narrowed, &BTreeSet::from([20]), false);
    assert_eq!(
        outcome.panes,
        PaneCounts {
            confirmed: 2,
            killed: 1,
            gone: 0,
            skipped: 0
        }
    );
    assert_eq!(outcome.unconfirmed, [PANE_A_SPLIT]);
    assert_eq!(
        outcome.sessions,
        SessionCounts {
            affected: 1,
            reset: 0,
            partial: 1,
            unconfirmed: 1
        }
    );
    assert_eq!(
        notification_text(&outcome).as_deref(),
        Some(
            "The reset covered 0 of 1 live sessions. Run Agterm ▸ Reset Live Sessions… again for the rest. Some \
             previous processes in 1 session may still be running; those commands were not restarted."
        )
    );
}

#[test]
fn the_outcome_counts_two_survivors_in_one_session_once() {
    let narrowed = narrowed(&[
        (unsupervised(PANE_A, 10), Disposition::Kill),
        (unsupervised(PANE_A_SPLIT, 20), Disposition::Kill),
        (
            target(PANE_B, SESSION_B, 30, Reason::Unsupervised),
            Disposition::Skipped,
        ),
    ]);
    let outcome = outcome(&narrowed, &BTreeSet::from([10, 20]), false);
    assert_eq!(
        outcome.unconfirmed.iter().copied().collect::<BTreeSet<_>>(),
        claimed(&[PANE_A, PANE_A_SPLIT])
    );
    assert_eq!(
        outcome.sessions,
        SessionCounts {
            affected: 2,
            reset: 0,
            partial: 2,
            unconfirmed: 1
        }
    );
    assert_eq!(outcome.panes.skipped, 1);
}

#[test]
fn the_notification_reports_a_partial_reset_and_an_unreadable_list() {
    let partial = Outcome {
        panes: PaneCounts {
            confirmed: 3,
            killed: 2,
            gone: 0,
            skipped: 1,
        },
        unconfirmed: Vec::new(),
        sessions: SessionCounts {
            affected: 3,
            reset: 2,
            partial: 1,
            unconfirmed: 0,
        },
        inventory_failed: false,
    };
    assert_eq!(
        notification_text(&partial).as_deref(),
        Some("The reset covered 2 of 3 live sessions. Run Agterm ▸ Reset Live Sessions… again for the rest.")
    );
    let failed = Outcome {
        inventory_failed: true,
        ..partial
    };
    assert_eq!(
        notification_text(&failed).as_deref(),
        Some("Live sessions were not reset: the session list could not be read.")
    );
    assert_eq!(
        serde_json::to_value(&failed).unwrap()["inventoryFailed"],
        serde_json::Value::Bool(true)
    );
}

#[test]
fn the_dialog_counts_sessions_and_names_those_that_predate_the_update() {
    for (count, opening) in [
        (1, "1 live session will be reset."),
        (2, "2 live sessions will be reset."),
    ] {
        let (title, body) = dialog_text(count, 0);
        assert_eq!(title, "Reset Live Sessions?");
        assert!(body.starts_with(opening));
        assert!(body.contains("quits and reopens itself"));
        assert!(!body.to_lowercase().contains("zmx"));
    }
    for (sessions, outdated, sentence) in [
        (
            1,
            1,
            "1 live session will be reset. It predates the last Live sessions update",
        ),
        (
            3,
            3,
            "3 live sessions will be reset. They all predate the last Live sessions update",
        ),
        (
            3,
            1,
            "3 live sessions will be reset. 1 of them predates the last Live sessions update",
        ),
        (
            3,
            2,
            "3 live sessions will be reset. 2 of them predate the last Live sessions update",
        ),
    ] {
        let (_, body) = dialog_text(sessions, outdated);
        assert!(body.starts_with(sentence), "{body}");
        assert!(body.contains("will be recreated on the current one. Agterm quits and reopens itself"));
    }
}

#[test]
fn the_menu_is_visible_only_when_both_modes_are_live() {
    let modes = [RestoreMode::None, RestoreMode::Rerun, RestoreMode::Live];
    for configured in modes {
        for active in modes {
            assert_eq!(
                menu_visible(configured, active),
                configured == RestoreMode::Live && active == RestoreMode::Live
            );
        }
    }
}

#[test]
fn the_marker_store_writes_then_consumes_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = MarkerStore::new(dir.path());
    let marker = marker(vec![
        unsupervised(PANE_A, 10),
        target(PANE_B, SESSION_B, 11, Reason::Outdated),
    ]);
    store.write(&marker).unwrap();
    assert!(dir.path().join(MARKER_FILE_NAME).exists());
    assert_eq!(store.consume().unwrap(), Some(marker));
    assert!(!dir.path().join(MARKER_FILE_NAME).exists());
    assert!(dir.path().join(CONSUMED_FILE_NAME).exists());
    assert_eq!(store.consume().unwrap(), None);
    store.remove_consumed();
    assert!(!dir.path().join(CONSUMED_FILE_NAME).exists());
}

#[test]
fn the_marker_store_removes_an_invalid_marker_and_reports_it() {
    for contents in ["not json", r#"{"version":3,"createdAt":0,"targets":[]}"#] {
        let dir = tempfile::tempdir().unwrap();
        let store = MarkerStore::new(dir.path());
        fs::write(dir.path().join(MARKER_FILE_NAME), contents).unwrap();
        assert!(matches!(store.consume(), Err(MarkerError::Invalid)));
        assert!(!dir.path().join(MARKER_FILE_NAME).exists());
        assert!(!dir.path().join(CONSUMED_FILE_NAME).exists());
    }
}

#[test]
fn the_marker_store_consumes_a_version_one_marker_as_unsupervised() {
    let dir = tempfile::tempdir().unwrap();
    let store = MarkerStore::new(dir.path());
    let v1 = format!(
        r#"{{"version":1,"createdAt":0,"targets":[{{"paneIdentity":"{}","sessionID":"{}","daemon":"{}","leaderPID":10}}]}}"#,
        crate::pane_env::uuid_string(PANE_A),
        crate::pane_env::uuid_string(SESSION_A),
        daemon_name(PANE_A)
    );
    fs::write(dir.path().join(MARKER_FILE_NAME), v1).unwrap();
    let consumed = store.consume().unwrap().unwrap();
    assert_eq!(consumed.version, 1);
    assert_eq!(consumed.targets, [unsupervised(PANE_A, 10)]);
}

#[test]
fn the_marker_is_written_as_upstream_encodes_it() {
    let marker = marker(vec![target(PANE_A, SESSION_A, 10, Reason::Outdated)]);
    assert_eq!(
        serde_json::to_value(&marker).unwrap(),
        serde_json::json!({"version": 2, "createdAt": 0.0, "targets": [{
            "paneIdentity": crate::pane_env::uuid_string(PANE_A),
            "sessionID": crate::pane_env::uuid_string(SESSION_A),
            "daemon": daemon_name(PANE_A), "leaderPID": 10, "reason": "outdated"}]})
    );
    let dir = tempfile::tempdir().unwrap();
    let store = MarkerStore::new(dir.path());
    store.write(&marker).unwrap();
    store.consume().unwrap();
    store.write(&marker).unwrap();
    store.remove();
    assert!(!dir.path().join(MARKER_FILE_NAME).exists());
    assert!(!dir.path().join(CONSUMED_FILE_NAME).exists());
}

#[test]
fn classify_attributes_a_leader_by_its_responsible_process() {
    use ResponsibleProcess::{Dead, Live};
    let classify = |leader, responsible| Attribution::classify(leader, responsible, Some(50), Some(60));
    assert_eq!(classify(None, Some(Dead)), Attribution::Unknown);
    assert_eq!(classify(Some(0), Some(Dead)), Attribution::Unknown);
    assert_eq!(classify(Some(10), Some(Dead)), Attribution::Orphaned);
    assert_eq!(classify(Some(10), Some(Live(10))), Attribution::Orphaned);
    assert_eq!(classify(Some(10), Some(Live(50))), Attribution::Supervisor);
    assert_eq!(classify(Some(10), Some(Live(60))), Attribution::App);
    assert_eq!(classify(Some(10), Some(Live(70))), Attribution::Unknown);
    assert_eq!(classify(Some(10), Some(Live(0))), Attribution::Unknown);
    assert_eq!(
        classify(Some(10), Some(ResponsibleProcess::Unknown)),
        Attribution::Unknown
    );
    assert_eq!(classify(Some(10), None), Attribution::Unknown);
}
