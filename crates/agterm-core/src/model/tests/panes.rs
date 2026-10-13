//! Split and scratch panes, ported from upstream's `AppStorePaneTests` and `AppStorePaneSwapTests`.

use agterm_host::fakes::{FakeSurface, FakeSurfaceProbe};
use agterm_proto::EventKind;
use uuid::Uuid;

use super::support::add;
use crate::model::{
    AgentIndicator, AgentStatus, AppStore, BackgroundWatermark, ModelEvent, PaneLaunch, PaneRestore, SaveRequest,
    Session, SplitAxis, SplitLayout, StatusPane, SwapRefusal, WatermarkKind, WatermarkRemoval,
};
use crate::pane_env::uuid_string;

/// A store with one workspace holding one selected session at `/a`, its outbox drained.
fn seeded() -> (AppStore, Uuid) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let session = add(&mut store, ws, "/a");
    store.take_outbox();
    (store, session)
}

fn get(store: &AppStore, id: Uuid) -> &Session {
    store.session(id).expect("the session is in the tree")
}

fn get_mut(store: &mut AppStore, id: Uuid) -> &mut Session {
    store.session_mut(id).expect("the session is in the tree")
}

/// Installs a fake terminal with `token` in pane `pane`.
fn install(store: &mut AppStore, id: Uuid, pane: StatusPane, token: &str) -> FakeSurfaceProbe {
    let surface = FakeSurface::new(token);
    let probe = surface.probe();
    get_mut(store, id).pane_slot_mut(pane).replace(Box::new(surface));
    probe
}

fn token(store: &AppStore, id: Uuid, pane: StatusPane) -> Option<String> {
    get(store, id)
        .pane_slot(pane)
        .get()
        .map(|surface| surface.pane_token().to_owned())
}

/// The `status` payloads of the pending `kind` events (`shown`/`hidden`).
fn visibility(store: &AppStore, kind: EventKind) -> Vec<String> {
    store
        .outbox()
        .events
        .iter()
        .filter(|event| event.kind == kind)
        .filter_map(|event: &ModelEvent| event.payload.status.clone())
        .collect()
}

/// A background of `kind` with `value` as its text or color.
fn watermark(kind: WatermarkKind, value: &str) -> BackgroundWatermark {
    let (text, color_hex) = match kind {
        WatermarkKind::Color => (None, Some(value.to_owned())),
        _ => (Some(value.to_owned()), None),
    };
    BackgroundWatermark {
        kind,
        image_path: None,
        text,
        color_hex,
        opacity: None,
        fit: None,
        position: None,
        repeats: None,
    }
}

fn blocked(pane: Option<StatusPane>) -> AgentIndicator {
    AgentIndicator {
        status: AgentStatus::Blocked,
        status_pane: pane,
        ..AgentIndicator::default()
    }
}

/// A shown split with both panes' terminals (`L`, `R`) up.
fn split_with_terminals() -> (AppStore, Uuid, FakeSurfaceProbe, FakeSurfaceProbe) {
    let (mut store, id) = seeded();
    store.toggle_split(id, None);
    let primary = install(&mut store, id, StatusPane::Left, "L");
    let split = install(&mut store, id, StatusPane::Right, "R");
    store.take_outbox();
    (store, id, primary, split)
}

// MARK: - show and hide

#[test]
fn toggle_split_flips_the_flag_and_a_new_split_focuses_its_pane() {
    let (mut store, id) = seeded();
    assert_eq!(get(&store, id).split, SplitLayout::default());
    assert_eq!(get(&store, id).split_pane_identity, None);
    store.toggle_split(id, None);
    let split = get(&store, id).split;
    assert!(split.shown && split.exists && split.focused);
    let identity = get(&store, id).split_pane_identity;
    assert!(identity.is_some());
    assert_eq!(store.outbox().save, SaveRequest::Now);
    store.toggle_split(id, None);
    let split = get(&store, id).split;
    assert!(!split.shown && split.exists && split.focused);
    assert_eq!(get(&store, id).split_pane_identity, identity);
    assert_eq!(visibility(&store, EventKind::PaneSplit), ["shown", "hidden"]);
}

#[test]
fn reshowing_a_hidden_split_keeps_the_focused_pane() {
    let (mut store, id) = seeded();
    store.toggle_split(id, None);
    get_mut(&mut store, id).split.focused = false;
    store.toggle_split(id, None);
    assert!(!get(&store, id).split.shown);
    assert!(!get(&store, id).split.focused);
    store.toggle_split(id, None);
    assert!(get(&store, id).split.shown);
    assert!(!get(&store, id).split.focused, "no jerk back to the right pane");
}

#[test]
fn an_axis_creates_hides_transposes_and_reshows() {
    let (mut store, id) = seeded();
    store.toggle_split(id, Some(SplitAxis::TopBottom));
    let split = get(&store, id).split;
    assert!(split.shown && split.exists && split.focused);
    assert_eq!(split.axis, SplitAxis::TopBottom);

    get_mut(&mut store, id).split.focused = false;
    store.toggle_split(id, Some(SplitAxis::LeftRight));
    let split = get(&store, id).split;
    assert!(split.shown && !split.focused, "a transposition keeps the focused pane");
    assert_eq!(split.axis, SplitAxis::LeftRight);

    store.toggle_split(id, Some(SplitAxis::LeftRight));
    let split = get(&store, id).split;
    assert!(!split.shown && split.exists);
    assert_eq!(split.axis, SplitAxis::LeftRight);

    store.toggle_split(id, Some(SplitAxis::TopBottom));
    let split = get(&store, id).split;
    assert!(split.shown && !split.focused);
    assert_eq!(split.axis, SplitAxis::TopBottom);
}

#[test]
fn the_generic_toggle_keeps_the_axis() {
    let (mut store, id) = seeded();
    get_mut(&mut store, id).split.axis = SplitAxis::TopBottom;
    store.toggle_split(id, None);
    assert!(get(&store, id).split.shown);
    store.toggle_split(id, None);
    assert_eq!(get(&store, id).split.axis, SplitAxis::TopBottom);
}

#[test]
fn visibility_is_idempotent_and_emits_only_real_changes() {
    let (mut store, id) = seeded();
    store.set_split_visibility(id, false, None);
    assert!(!get(&store, id).split.exists, "hiding no split creates none");
    store.set_split_visibility(id, true, Some(SplitAxis::TopBottom));
    store.set_split_visibility(id, true, None);
    assert_eq!(get(&store, id).split.axis, SplitAxis::TopBottom);
    store.set_split_visibility(id, false, Some(SplitAxis::LeftRight));
    assert_eq!(
        get(&store, id).split.axis,
        SplitAxis::TopBottom,
        "hiding never changes the axis"
    );
    store.set_split_visibility(id, false, None);
    assert_eq!(visibility(&store, EventKind::PaneSplit), ["shown", "hidden"]);
    let event = &store.outbox().events[0];
    assert_eq!(event.session, Some(id));
    assert_eq!(event.payload.name.as_deref(), Some("a"));
}

#[test]
fn an_unknown_session_is_a_no_op() {
    let (mut store, id) = seeded();
    let gone = Uuid::new_v4();
    store.toggle_split(gone, None);
    store.set_split_visibility(gone, true, None);
    store.toggle_scratch(gone, true);
    assert_eq!(store.apply_split_ratio(0.5, gone), None);
    assert!(!store.close_scratch(gone, true));
    assert!(!store.set_split_focus(gone, true));
    assert_eq!(store.swap_panes(gone), Err(SwapRefusal::NoSession));
    store.close_split(gone);
    assert!(store.outbox().is_empty());
    assert!(!get(&store, id).scratch_active);
}

// MARK: - ratio

#[test]
fn the_ratio_clamps_sets_saves_and_returns() {
    let (mut store, id) = seeded();
    assert_eq!(store.apply_split_ratio(0.7, id), Some(0.7));
    assert_eq!(get(&store, id).split.ratio, Some(0.7));
    assert_eq!(store.outbox().save, SaveRequest::Now);
    assert_eq!(store.apply_split_ratio(2.0, id), Some(0.95));
    assert_eq!(store.apply_split_ratio(-1.0, id), Some(0.05));
    assert_eq!(get(&store, id).split.ratio, Some(0.05));
}

// MARK: - focus on a hidden split (hidden-split-holds-focus)

#[test]
fn focus_moves_on_a_hidden_split_and_shows_that_pane_maximized() {
    let (mut store, id, _, _) = split_with_terminals();
    assert!(store.set_split_focus(id, false));
    store.set_split_visibility(id, false, None);
    assert!(get(&store, id).renders_pane(false) && !get(&store, id).renders_pane(true));
    assert!(store.set_split_focus(id, true));
    let session = get(&store, id);
    assert!(!session.split.shown && session.split.focused);
    assert!(session.renders_pane(true) && !session.renders_pane(false));
    assert_eq!(
        session.on_screen_pane(),
        StatusPane::Right,
        "the maximized pane takes input"
    );
}

#[test]
fn stale_focus_reports_never_undo_the_chosen_pane_of_a_hidden_split() {
    let (mut store, id, _, _) = split_with_terminals();
    store.set_split_visibility(id, false, None);
    store.set_split_focus(id, true);
    assert!(
        !store.note_pane_focus(id, StatusPane::Left),
        "the left pane is not laid out"
    );
    assert!(get(&store, id).split.focused);
    store.set_split_visibility(id, true, None);
    assert!(get(&store, id).split.focused, "showing both keeps the chosen pane");
    assert!(
        store.note_pane_focus(id, StatusPane::Left),
        "a click on a shown pane moves focus"
    );
    assert!(!get(&store, id).split.focused);
    assert!(!store.note_pane_focus(id, StatusPane::Left), "no change");
    assert!(!store.note_pane_focus(id, StatusPane::Scratch));
    get_mut(&mut store, id).scratch_active = true;
    assert!(
        !store.note_pane_focus(id, StatusPane::Right),
        "the scratch covers both panes"
    );
}

#[test]
fn focus_needs_a_split() {
    let (mut store, id) = seeded();
    assert!(!store.set_split_focus(id, true));
    assert!(!get(&store, id).split.focused);
    assert!(!store.note_pane_focus(id, StatusPane::Right));
}

// MARK: - close

#[test]
fn closing_the_split_hides_it_tears_it_down_and_drops_what_it_owned() {
    let (mut store, id, primary, split) = split_with_terminals();
    let session = get_mut(&mut store, id);
    session.split.ratio = Some(0.7);
    session.split.axis = SplitAxis::TopBottom;
    session.split_cwd = Some("/var/log".to_owned());
    session.split_title = Some("logs".to_owned());
    session.initial_split_cwd = Some("/seed".to_owned());
    session.split_restore.foreground = Some(vec!["htop".to_owned()]);
    session.split_restore.pending_foreground = Some(vec!["htop".to_owned()]);
    session.split_launch = PaneLaunch {
        command: Some("ssh split-host".to_owned()),
        wait: true,
    };
    session.pane_backgrounds.right = Some(watermark(WatermarkKind::Text, "R"));
    session.search.active = true;
    store.close_split(id);
    let session = get(&store, id);
    assert_eq!(session.split, SplitLayout::default(), "a fresh split opens even");
    assert!(!session.split_surface.is_present());
    assert_eq!((split.teardowns(), primary.teardowns()), (1, 0));
    assert_eq!(
        (&session.split_cwd, &session.split_title, &session.initial_split_cwd),
        (&None, &None, &None)
    );
    assert_eq!(session.split_pane_identity, None);
    assert_eq!(session.split_restore, PaneRestore::default());
    assert_eq!(session.split_launch, PaneLaunch::default());
    assert_eq!(session.pane_backgrounds.right, None);
    assert!(!session.search.active);
    assert_eq!(visibility(&store, EventKind::PaneSplit), ["hidden"]);
}

#[test]
fn closing_a_hidden_split_emits_nothing() {
    let (mut store, id, _, _) = split_with_terminals();
    store.set_split_visibility(id, false, None);
    store.take_outbox();
    store.close_split(id);
    assert!(visibility(&store, EventKind::PaneSplit).is_empty());
    assert!(!get(&store, id).split.exists);
}

#[test]
fn closing_the_split_clears_only_a_status_its_pane_owned() {
    let (mut store, id, _, _) = split_with_terminals();
    store.set_agent_indicator(blocked(Some(StatusPane::Right)), id);
    store.close_split(id);
    assert_eq!(get(&store, id).agent_indicator.status, AgentStatus::Idle);

    let (mut store, id, _, _) = split_with_terminals();
    store.set_agent_indicator(blocked(None), id);
    store.close_split(id);
    assert_eq!(get(&store, id).agent_indicator.status, AgentStatus::Blocked);
}

#[test]
fn the_primary_exiting_promotes_the_split_survivor() {
    let (mut store, id, primary, split) = split_with_terminals();
    let primary_identity = get(&store, id).pane_identity;
    let split_identity = get(&store, id).split_pane_identity;
    get_mut(&mut store, id).split.ratio = Some(0.3);
    get_mut(&mut store, id).launch.command = Some("ssh host".to_owned());
    store.close_primary_pane(id);
    let session = get(&store, id);
    assert_eq!((primary.teardowns(), split.teardowns(), split.promotions()), (1, 0, 1));
    assert_eq!(token(&store, id, StatusPane::Left).as_deref(), Some("R"));
    assert!(!session.split_surface.is_present());
    assert_eq!(session.split, SplitLayout::default());
    assert_eq!(
        session.launch,
        PaneLaunch::default(),
        "the exited command pane is not resurrected"
    );
    assert_eq!(Some(session.pane_identity), split_identity);
    assert_ne!(session.pane_identity, primary_identity);
    assert_eq!(session.split_pane_identity, None);
    assert_eq!(session.addressable_pane(), StatusPane::Left);
    assert_eq!(visibility(&store, EventKind::PaneSplit), ["hidden"]);
}

#[test]
fn a_promotion_moves_the_survivors_cwd_title_and_capture_up() {
    let (mut store, id, _, _) = split_with_terminals();
    let session = get_mut(&mut store, id);
    session.split_cwd = Some("/var/log".to_owned());
    session.split_title = Some("remote-host".to_owned());
    session.split_restore.foreground = Some(vec!["ssh".to_owned(), "host".to_owned()]);
    store.close_primary_pane(id);
    let session = get(&store, id);
    assert_eq!(
        (session.current_cwd.as_deref(), session.osc_title.as_deref()),
        (Some("/var/log"), Some("remote-host"))
    );
    assert_eq!(
        session.restore.foreground,
        Some(vec!["ssh".to_owned(), "host".to_owned()])
    );
    assert_eq!((&session.split_cwd, &session.split_title), (&None, &None));
    assert_eq!(session.split_restore.foreground, None);
}

#[test]
fn a_promotion_carries_the_split_creation_command_and_restore_halves() {
    let (mut store, id, _, _) = split_with_terminals();
    let session = get_mut(&mut store, id);
    session.launch.command = Some("ssh primary-host".to_owned());
    session.split_launch = PaneLaunch {
        command: Some("ssh split-host".to_owned()),
        wait: true,
    };
    session.restore.command = Some("primary pin".to_owned());
    session.split_restore.command = None;
    session.split_restore.pending_command = Some("split armed".to_owned());
    store.close_primary_pane(id);
    let session = get(&store, id);
    assert_eq!(session.launch.command.as_deref(), Some("ssh split-host"));
    assert!(session.launch.wait);
    assert_eq!(session.split_launch, PaneLaunch::default());
    assert_eq!(session.restore.command, None, "an absent split pin migrates as a clear");
    assert_eq!(session.restore.pending_command.as_deref(), Some("split armed"));
}

#[test]
fn a_promotion_moves_the_survivors_armed_capture_left_and_clears_the_split_slot() {
    let (mut store, id, _, _) = split_with_terminals();
    let session = get_mut(&mut store, id);
    // the primary consumed its own capture at its spawn; the split still waits for its paced grant
    session.restore.pending_foreground = None;
    session.split_restore.pending_foreground = Some(vec!["htop".to_owned()]);
    store.close_primary_pane(id);
    let session = get(&store, id);
    assert_eq!(
        session.restore.pending_foreground.as_deref(),
        Some(["htop".to_owned()].as_slice()),
        "the survivor replays its capture from the left slot"
    );
    assert_eq!(
        session.split_restore.pending_foreground, None,
        "the next split does not inherit the gone pane's capture"
    );
}

#[test]
fn a_promotion_uses_the_restored_split_cwd_and_drops_the_primary_title() {
    let (mut store, id, _, _) = split_with_terminals();
    let session = get_mut(&mut store, id);
    session.current_cwd = Some("/primary".to_owned());
    session.osc_title = Some("primary-title".to_owned());
    session.initial_split_cwd = Some("/restored-split".to_owned());
    store.close_primary_pane(id);
    let session = get(&store, id);
    assert_eq!(session.current_cwd.as_deref(), Some("/restored-split"));
    assert_eq!(session.osc_title, None);
    assert_eq!(session.initial_split_cwd, None);
}

#[test]
fn a_promotion_keeps_the_survivors_search_and_ends_the_primarys() {
    let (mut store, id, _, _) = split_with_terminals();
    get_mut(&mut store, id).search.active = true;
    get_mut(&mut store, id).search.owner = Some(StatusPane::Right);
    store.close_primary_pane(id);
    let search = &get(&store, id).search;
    assert!(search.active);
    assert_eq!(
        search.owner,
        Some(StatusPane::Left),
        "it follows the survivor into the left role"
    );

    let (mut store, id, _, _) = split_with_terminals();
    get_mut(&mut store, id).search.active = true;
    get_mut(&mut store, id).search.owner = Some(StatusPane::Left);
    store.close_primary_pane(id);
    assert!(!get(&store, id).search.active);
}

#[test]
fn a_promotion_ends_the_primarys_status_and_retags_the_survivors() {
    let (mut store, id, _, _) = split_with_terminals();
    store.set_agent_indicator(blocked(None), id);
    store.close_primary_pane(id);
    assert_eq!(get(&store, id).agent_indicator.status, AgentStatus::Idle);

    let (mut store, id, _, _) = split_with_terminals();
    store.set_agent_indicator(blocked(Some(StatusPane::Right)), id);
    store.close_primary_pane(id);
    let indicator = &get(&store, id).agent_indicator;
    assert_eq!(indicator.status, AgentStatus::Blocked);
    assert_eq!(indicator.status_pane, Some(StatusPane::Left));

    let (mut store, id, _, _) = split_with_terminals();
    store.set_agent_indicator(blocked(Some(StatusPane::Scratch)), id);
    store.close_primary_pane(id);
    assert_eq!(get(&store, id).agent_indicator.status_pane, Some(StatusPane::Scratch));
}

#[test]
fn the_last_pane_exiting_closes_the_session() {
    let (mut store, id) = seeded();
    let primary = install(&mut store, id, StatusPane::Left, "L");
    store.close_primary_pane(id);
    assert!(store.session(id).is_none());
    assert_eq!(primary.teardowns(), 1);

    let (mut store, id) = seeded();
    let split = install(&mut store, id, StatusPane::Right, "R");
    store.close_split_pane(id);
    assert!(store.session(id).is_none());
    assert_eq!(split.teardowns(), 1);
}

#[test]
fn the_split_pane_exiting_collapses_to_the_primary() {
    let (mut store, id, primary, split) = split_with_terminals();
    store.apply_split_ratio(0.4, id);
    store.close_split_pane(id);
    let session = get(&store, id);
    assert_eq!((split.teardowns(), primary.teardowns()), (1, 0));
    assert!(!session.split_surface.is_present() && !session.split.shown);
    assert_eq!(session.split.ratio, None);
}

#[test]
fn the_promoted_survivor_exiting_closes_the_session_and_tears_it_down_once() {
    let (mut store, id, _, split) = split_with_terminals();
    store.close_primary_pane(id);
    store.close_split_pane(id);
    assert!(store.session(id).is_none(), "no zombie session");
    assert_eq!(split.teardowns(), 1);
}

#[test]
fn a_promoted_main_pane_exiting_after_a_resplit_collapses_to_the_new_split() {
    let (mut store, id, _, first) = split_with_terminals();
    store.close_primary_pane(id);
    store.toggle_split(id, None);
    let second = install(&mut store, id, StatusPane::Right, "R2");
    store.close_primary_pane(id);
    assert!(store.session(id).is_some());
    assert_eq!(token(&store, id, StatusPane::Left).as_deref(), Some("R2"));
    assert!(!get(&store, id).split_surface.is_present());
    assert_eq!((second.promotions(), second.teardowns()), (1, 0));
    assert_eq!(first.teardowns(), 1);
}

// MARK: - swap

/// A shown-then-hidden top/bottom split with every pane-owned field seeded per side.
fn seeded_swap() -> (AppStore, Uuid, FakeSurfaceProbe, FakeSurfaceProbe) {
    let (mut store, id, primary, split) = split_with_terminals();
    store.set_split_visibility(id, false, Some(SplitAxis::TopBottom));
    let session = get_mut(&mut store, id);
    session.split.axis = SplitAxis::TopBottom;
    session.split.ratio = Some(0.3);
    session.split.focused = false;
    session.pane_identity = Uuid::from_u128(1);
    session.split_pane_identity = Some(Uuid::from_u128(2));
    session.current_cwd = Some("/live-left".to_owned());
    session.split_cwd = Some("/live-right".to_owned());
    session.osc_title = Some("left-title".to_owned());
    session.split_title = Some("right-title".to_owned());
    session.restore = PaneRestore {
        command: None,
        foreground: Some(vec!["left".to_owned(), "foreground".to_owned()]),
        pending_command: Some("left pending restore".to_owned()),
        pending_foreground: Some(vec!["left".to_owned(), "pending".to_owned()]),
    };
    session.split_restore = PaneRestore {
        command: Some("right restore".to_owned()),
        foreground: Some(vec!["right".to_owned(), "foreground".to_owned()]),
        pending_command: None,
        pending_foreground: Some(vec!["right".to_owned(), "pending".to_owned()]),
    };
    session.launch = PaneLaunch {
        command: Some("left command".to_owned()),
        wait: false,
    };
    session.split_launch = PaneLaunch {
        command: Some("right command".to_owned()),
        wait: true,
    };
    session.pane_backgrounds.left = Some(watermark(WatermarkKind::Text, "DRIVER"));
    session.pane_backgrounds.right = Some(watermark(WatermarkKind::Color, "#201414"));
    store.take_outbox();
    (store, id, primary, split)
}

#[test]
fn a_swap_exchanges_the_terminals_identities_cwds_and_titles() {
    let (mut store, id, _, _) = seeded_swap();
    assert_eq!(store.swap_panes(id), Ok(()));
    let session = get(&store, id);
    assert_eq!(
        (
            token(&store, id, StatusPane::Left),
            token(&store, id, StatusPane::Right)
        ),
        (Some("R".to_owned()), Some("L".to_owned()))
    );
    assert_eq!(
        (session.pane_identity, session.split_pane_identity),
        (Uuid::from_u128(2), Some(Uuid::from_u128(1)))
    );
    assert_eq!(
        (
            session.current_cwd.as_deref(),
            session.split_cwd.as_deref(),
            session.initial_split_cwd.as_deref()
        ),
        (Some("/live-right"), Some("/live-left"), Some("/live-left"))
    );
    assert_eq!(
        (session.osc_title.as_deref(), session.split_title.as_deref()),
        (Some("right-title"), Some("left-title"))
    );
    assert_eq!(store.outbox().save, SaveRequest::Now);
}

#[test]
fn a_swap_exchanges_the_restore_state_creation_commands_and_backgrounds() {
    let (mut store, id, _, _) = seeded_swap();
    let (left, right) = {
        let session = get(&store, id);
        (session.restore.clone(), session.split_restore.clone())
    };
    assert_eq!(store.swap_panes(id), Ok(()));
    let session = get(&store, id);
    assert_eq!((&session.restore, &session.split_restore), (&right, &left));
    assert_eq!(session.launch.command.as_deref(), Some("right command"));
    assert!(session.launch.wait && !session.split_launch.wait);
    assert_eq!(
        session
            .pane_backgrounds
            .left
            .as_ref()
            .and_then(|b| b.color_hex.as_deref()),
        Some("#201414")
    );
    assert_eq!(
        session.pane_backgrounds.right.as_ref().and_then(|b| b.text.as_deref()),
        Some("DRIVER")
    );
}

#[test]
fn a_swap_keeps_the_layout_and_focus_follows_the_terminal() {
    let (mut store, id, _, _) = seeded_swap();
    store.set_agent_indicator(blocked(Some(StatusPane::Left)), id);
    let changed_at = get(&store, id).status_changed_at;
    assert_eq!(store.swap_panes(id), Ok(()));
    let session = get(&store, id);
    assert_eq!(session.agent_indicator.status_pane, Some(StatusPane::Right));
    assert_eq!(session.status_changed_at, changed_at);
    let split = session.split;
    assert!(!split.shown && split.exists);
    assert_eq!((split.axis, split.ratio), (SplitAxis::TopBottom, Some(0.3)));
    assert!(split.focused, "focus follows the terminal");
}

#[test]
fn a_swap_falls_back_on_the_seed_cwds() {
    let (mut store, id, _, _) = seeded_swap();
    let session = get_mut(&mut store, id);
    session.current_cwd = None;
    session.split_cwd = None;
    session.initial_split_cwd = Some("/seed-right".to_owned());
    assert_eq!(store.swap_panes(id), Ok(()));
    let session = get(&store, id);
    assert_eq!(session.current_cwd.as_deref(), Some("/seed-right"));
    assert_eq!(session.split_cwd.as_deref(), Some("/a"));
    assert_eq!(session.initial_split_cwd.as_deref(), Some("/a"));
}

#[test]
fn swapping_twice_restores_the_panes_and_the_snapshot() {
    let (mut store, id, _, _) = seeded_swap();
    let before = store.snapshot();
    assert_eq!(store.swap_panes(id), Ok(()));
    assert_eq!(store.swap_panes(id), Ok(()));
    assert_eq!(store.snapshot(), before);
    assert_eq!(token(&store, id, StatusPane::Left).as_deref(), Some("L"));
    assert!(!get(&store, id).split.focused);
}

#[test]
fn a_swap_retags_every_status_owner_and_the_search_owner() {
    let cases = [
        (AgentStatus::Blocked, None, Some(StatusPane::Right)),
        (AgentStatus::Completed, Some(StatusPane::Left), Some(StatusPane::Right)),
        (AgentStatus::Active, Some(StatusPane::Right), Some(StatusPane::Left)),
        (
            AgentStatus::Blocked,
            Some(StatusPane::Scratch),
            Some(StatusPane::Scratch),
        ),
        (AgentStatus::Idle, None, None),
    ];
    for (status, before, expected) in cases {
        let (mut store, id, _, _) = seeded_swap();
        let session = get_mut(&mut store, id);
        session.agent_indicator = AgentIndicator {
            status,
            status_pane: before,
            ..AgentIndicator::default()
        };
        session.search.owner = Some(StatusPane::Left);
        assert_eq!(store.swap_panes(id), Ok(()));
        let session = get(&store, id);
        assert_eq!(session.agent_indicator.status, status);
        assert_eq!(session.agent_indicator.status_pane, expected, "{status:?} {before:?}");
        assert_eq!(session.search.owner, Some(StatusPane::Right));
    }
}

#[test]
fn a_swap_refuses_without_changing_anything() {
    let (mut store, id) = seeded();
    install(&mut store, id, StatusPane::Left, "L");
    install(&mut store, id, StatusPane::Right, "R");
    assert_eq!(store.swap_panes(id), Err(SwapRefusal::NoSplit));
    assert_eq!(token(&store, id, StatusPane::Left).as_deref(), Some("L"));

    for missing in [StatusPane::Left, StatusPane::Right] {
        let (mut store, id, _, _) = seeded_swap();
        get_mut(&mut store, id).pane_slot_mut(missing).teardown();
        let before = store.snapshot();
        assert_eq!(store.swap_panes(id), Err(SwapRefusal::SlotNotRealized));
        assert_eq!(store.snapshot(), before);
        assert!(store.outbox().is_empty());
    }
}

#[test]
fn a_split_closed_after_a_swap_drops_only_the_departed_creation_command() {
    let (mut store, id, _, _) = seeded_swap();
    assert_eq!(store.swap_panes(id), Ok(()));
    store.close_split(id);
    let session = get(&store, id);
    assert_eq!(session.launch.command.as_deref(), Some("right command"));
    assert!(session.launch.wait);
    assert_eq!(session.split_launch, PaneLaunch::default());
}

#[test]
fn a_primary_exit_after_a_swap_promotes_the_surviving_terminal() {
    let (mut store, id, primary, _) = seeded_swap();
    assert_eq!(store.swap_panes(id), Ok(()));
    store.close_primary_pane(id);
    assert_eq!(token(&store, id, StatusPane::Left).as_deref(), Some("L"));
    assert_eq!(primary.promotions(), 1);
    let session = get(&store, id);
    assert_eq!(session.launch.command.as_deref(), Some("left command"));
    assert!(!session.launch.wait && !session.split.exists);
}

// MARK: - scratch

#[test]
fn toggling_the_scratch_flips_the_flag_and_keeps_its_terminal() {
    let (mut store, id) = seeded();
    store.toggle_scratch(id, true);
    assert!(get(&store, id).scratch_active);
    let scratch = install(&mut store, id, StatusPane::Scratch, "S");
    store.toggle_scratch(id, true);
    assert!(!get(&store, id).scratch_active);
    assert_eq!(token(&store, id, StatusPane::Scratch).as_deref(), Some("S"));
    assert_eq!(scratch.teardowns(), 0);
    store.toggle_scratch(id, false);
    assert!(get(&store, id).scratch_active);
    assert_eq!(visibility(&store, EventKind::PaneScratch), ["shown", "hidden"]);
    assert!(
        store.outbox().save == SaveRequest::None,
        "the scratch is never persisted"
    );
}

#[test]
fn closing_the_scratch_tears_it_down_once() {
    let (mut store, id) = seeded();
    store.toggle_scratch(id, true);
    let scratch = install(&mut store, id, StatusPane::Scratch, "S");
    store.take_outbox();
    assert!(store.close_scratch(id, true));
    assert!(!get(&store, id).scratch_active);
    assert!(!get(&store, id).scratch_surface.is_present());
    assert_eq!(scratch.teardowns(), 1);
    assert!(!store.close_scratch(id, true));
    assert_eq!(visibility(&store, EventKind::PaneScratch), ["hidden"]);
}

#[test]
fn closing_the_scratch_ends_only_its_own_search_and_status() {
    let (mut store, id) = seeded();
    store.toggle_scratch(id, true);
    install(&mut store, id, StatusPane::Scratch, "S");
    let session = get_mut(&mut store, id);
    session.search.active = true;
    session.search.owner = Some(StatusPane::Scratch);
    session.search.needle = "needle".to_owned();
    store.set_agent_indicator(blocked(Some(StatusPane::Scratch)), id);
    assert!(store.close_scratch(id, true));
    assert!(!get(&store, id).search.active);
    assert_eq!(get(&store, id).search.needle, "");
    assert_eq!(get(&store, id).agent_indicator.status, AgentStatus::Idle);

    let (mut store, id) = seeded();
    store.toggle_scratch(id, true);
    install(&mut store, id, StatusPane::Scratch, "S");
    let session = get_mut(&mut store, id);
    session.search.active = true;
    session.search.owner = Some(StatusPane::Left);
    store.set_agent_indicator(blocked(None), id);
    assert!(store.close_scratch(id, true));
    assert!(get(&store, id).search.active);
    assert_eq!(get(&store, id).agent_indicator.status, AgentStatus::Blocked);
}

#[test]
fn closing_the_session_or_its_workspace_tears_the_scratch_down() {
    let (mut store, id) = seeded();
    let scratch = install(&mut store, id, StatusPane::Scratch, "S");
    store.close_session(id);
    assert_eq!(scratch.teardowns(), 1);

    let mut store = AppStore::new();
    let keep = store.add_workspace("keep");
    add(&mut store, keep, "/k");
    let ws = store.add_workspace("work");
    let id = add(&mut store, ws, "/a");
    let scratch = install(&mut store, id, StatusPane::Scratch, "S");
    store.remove_workspace(ws);
    assert_eq!(scratch.teardowns(), 1);
}

/// Upstream's `dropPaneBackground` and `removeAllRenderedText`: a departing pane's text override and a session removed
/// for good leave their rendered files for the persistence to delete.
#[test]
fn a_departing_panes_text_and_a_closed_session_leave_their_files_for_removal() {
    let removed = |store: &mut AppStore| store.take_outbox().removed_watermarks;
    let pane = |session: Uuid, key: String| WatermarkRemoval::Pane { session, key };

    // the exiting primary's text goes; the survivor's color override has no file
    let (mut store, id, _, _) = seeded_swap();
    store.close_primary_pane(id);
    assert_eq!(removed(&mut store), [pane(id, uuid_string(Uuid::from_u128(1)))]);

    // the split's own text goes with it, named by the identity it had
    let (mut store, id, _, _) = seeded_swap();
    assert_eq!(store.swap_panes(id), Ok(()));
    store.take_outbox();
    store.close_split(id);
    assert_eq!(removed(&mut store), [pane(id, uuid_string(Uuid::from_u128(1)))]);

    install(&mut store, id, StatusPane::Scratch, "S");
    get_mut(&mut store, id).pane_backgrounds.scratch = Some(watermark(WatermarkKind::Text, "S"));
    assert!(store.close_scratch(id, true));
    assert_eq!(removed(&mut store), [pane(id, "scratch".to_owned())]);
    install(&mut store, id, StatusPane::Scratch, "S2");
    get_mut(&mut store, id).pane_backgrounds.scratch = Some(watermark(WatermarkKind::Color, "#101010"));
    assert!(store.close_scratch(id, true));
    assert!(removed(&mut store).is_empty());

    store.close_session(id);
    assert_eq!(removed(&mut store), [WatermarkRemoval::Session(id)]);
}
