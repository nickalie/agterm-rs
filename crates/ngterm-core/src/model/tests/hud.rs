//! The store's HUD sizing, ported from upstream's `AppStoreHudTests` (the measured-panel cases of v0.36.0): the height
//! in points, the sticky width bound, a width forced by `overlay.resize` and held through a remeasure, a size set back,
//! and the tree's `sticky` and `frame`.

use ngterm_proto::tree::ControlHudNode;
use uuid::Uuid;

use super::support::add;
use crate::hud::{self, HudPosition, HudSpec, MAX_SIZE_PERCENT, PaneMetrics};
use crate::model::{AppStore, HudOpening, Session};
use crate::projection;

/// Upstream's `widePane`: an 8x18 cell in a 1600x1000 pane, padded 8 across and 6 down.
const WIDE_PANE: PaneMetrics = PaneMetrics {
    cell_width: 8.0,
    cell_height: 18.0,
    pane_width: 1600.0,
    pane_height: 1000.0,
    padding_width: 8.0,
    padding_height: 6.0,
};

/// A store with one session whose HUD `spec` was opened measured against `pane` (upstream's `measuredHud`).
fn measured_hud(spec: HudSpec, pane: &PaneMetrics) -> (AppStore, Uuid, tempfile::TempDir) {
    let mut store = AppStore::new();
    let ws = store.add_workspace("work");
    let id = add(&mut store, ws, "/repo");
    let dir = tempfile::tempdir().unwrap();
    let opening = HudOpening {
        command: "hud.sh".to_owned(),
        file: dir.path().join("hud"),
        size: hud::panel_size(&spec, pane),
        pane_identity: None,
        font_size: None,
    };
    assert!(store.open_hud(id, spec, opening));
    (store, id, dir)
}

fn session(store: &AppStore, id: Uuid) -> &Session {
    store.session(id).expect("the session exists")
}

fn height_points(store: &AppStore, id: Uuid) -> Option<f64> {
    session(store, id)
        .overlay
        .hud
        .as_ref()
        .and_then(|hud| hud.height_points)
}

fn forced_width(store: &AppStore, id: Uuid) -> Option<i64> {
    session(store, id)
        .overlay
        .hud
        .as_ref()
        .and_then(|hud| hud.resized_width_percent)
}

fn hud_node(store: &AppStore, id: Uuid) -> ControlHudNode {
    projection::session_node(session(store, id), true)
        .hud
        .expect("a hud node")
}

fn caption(position: HudPosition, size_percent: Option<i64>, sticky: bool) -> HudSpec {
    HudSpec {
        size_percent,
        position,
        sticky,
        ..HudSpec::new("caption")
    }
}

#[test]
fn open_stores_the_height_in_points_and_close_clears_it() {
    let (mut store, id, _dir) = measured_hud(HudSpec::new("working"), &WIDE_PANE);

    // three rows (the message and its padding row above and below) of 18 plus 6 of padding on each side
    assert_eq!(height_points(&store, id), Some(66.0));
    assert_eq!(
        session(&store, id)
            .overlay
            .hud_panel_size()
            .and_then(|size| size.height_points),
        Some(66.0)
    );

    assert!(store.close_hud(id));

    assert_eq!(height_points(&store, id), None);
    assert_eq!(session(&store, id).overlay.hud_panel_size(), None);
}

#[test]
fn a_sticky_hud_may_take_the_full_width_and_a_loose_one_is_still_capped() {
    let sticky = caption(HudPosition::TopCenter, Some(100), true);
    let (mut store, id, _dir) = measured_hud(sticky, &WIDE_PANE);

    assert_eq!(session(&store, id).overlay.size_percent, Some(100));
    assert_eq!(hud_node(&store, id).size_percent, Some(100));

    let loose = caption(HudPosition::TopCenter, Some(100), false);
    let size = hud::panel_size(&loose, &WIDE_PANE);
    assert!(store.update_hud(id, &loose, size, None));

    assert_eq!(session(&store, id).overlay.size_percent, Some(MAX_SIZE_PERCENT));
}

#[test]
fn a_resize_forces_the_width_until_the_next_update() {
    let (mut store, id, _dir) = measured_hud(HudSpec::new("working"), &WIDE_PANE);

    assert_eq!(forced_width(&store, id), None);
    assert!(store.resize_overlay(id, Some(100)));

    let slot = &session(&store, id).overlay;
    assert_eq!(slot.size_percent, Some(MAX_SIZE_PERCENT));
    assert_eq!(forced_width(&store, id), Some(MAX_SIZE_PERCENT));
    assert_eq!(
        slot.effective_hud_spec().and_then(|spec| spec.size_percent),
        Some(MAX_SIZE_PERCENT)
    );
    assert_eq!(slot.hud.as_ref().unwrap().spec.size_percent, None);

    let next = HudSpec::new("done");
    let size = hud::panel_size(&next, &WIDE_PANE);
    assert!(store.update_hud(id, &next, size, None));

    assert_eq!(forced_width(&store, id), None);
}

#[test]
fn a_remeasure_keeps_a_forced_width_and_follows_the_pane() {
    let line = ["word"; 20].join(" ");
    let spec = HudSpec {
        markdown: true,
        ..HudSpec::new(line)
    };
    let (mut store, id, _dir) = measured_hud(spec, &WIDE_PANE);
    let wrapped = height_points(&store, id).unwrap();
    assert!(store.resize_overlay(id, Some(80)));

    assert!(store.remeasure_hud(id, &WIDE_PANE));

    assert_eq!(session(&store, id).overlay.size_percent, Some(80));
    assert_eq!(forced_width(&store, id), Some(80));
    assert_eq!(height_points(&store, id), Some(wrapped - 18.0));

    let short = PaneMetrics {
        pane_height: 500.0,
        ..WIDE_PANE
    };
    assert!(store.remeasure_hud(id, &short));

    assert_eq!(height_points(&store, id), Some(wrapped - 18.0));
    assert_eq!(session(&store, id).overlay.hud.as_ref().unwrap().height_percent, 14);
}

#[test]
fn setting_the_size_back_restores_a_measured_width_as_measured() {
    let (mut store, id, _dir) = measured_hud(HudSpec::new("working"), &WIDE_PANE);
    let measured = session(&store, id).overlay.hud_panel_size().unwrap();
    assert!(store.resize_overlay(id, Some(60)));

    assert!(store.set_hud_size(id, measured, None));

    assert_eq!(session(&store, id).overlay.hud_panel_size(), Some(measured));
    assert_eq!(forced_width(&store, id), None);
    assert!(!store.set_hud_size(Uuid::new_v4(), measured, None));
}

#[test]
fn the_tree_reports_sticky_and_frame_and_an_older_servers_node_decodes_to_the_defaults() {
    let spec = HudSpec {
        frame: false,
        ..caption(HudPosition::TopCenter, None, true)
    };
    let (store, id, _dir) = measured_hud(spec, &WIDE_PANE);

    let node = hud_node(&store, id);
    let older: ControlHudNode =
        serde_json::from_str(r#"{"message":"m","spinner":"none","position":"center","hideAfter":0}"#).unwrap();

    assert!(node.sticky);
    assert!(!node.frame);
    let round_trip: ControlHudNode = serde_json::from_str(&serde_json::to_string(&node).unwrap()).unwrap();
    assert_eq!(round_trip, node);
    assert!(!older.sticky);
    assert!(older.frame);
}

#[test]
fn a_plain_hud_reads_back_not_sticky_and_framed() {
    let (store, id, _dir) = measured_hud(HudSpec::new("working"), &WIDE_PANE);

    let node = hud_node(&store, id);

    assert!(!node.sticky);
    assert!(node.frame);
}
