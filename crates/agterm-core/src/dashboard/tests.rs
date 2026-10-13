//! Ported from upstream's `DashboardLayoutTests`, `DashboardTargetTests`, `DashboardControllerTests` and
//! `AppStore+DashboardTests`.

use agterm_host::fakes::FakeSurface;

use super::*;
use crate::model::NewSession;

fn primary(id: Uuid) -> DashboardMember {
    DashboardMember::primary(id)
}

fn split(id: Uuid) -> DashboardMember {
    DashboardMember::split(id)
}

fn ids(count: usize) -> Vec<Uuid> {
    (0..count).map(|_| Uuid::new_v4()).collect()
}

// MARK: - layout

#[test]
fn grid_dimensions_follow_ceil_sqrt() {
    let table = [
        (1, 1, 1),
        (2, 2, 1),
        (3, 2, 2),
        (4, 2, 2),
        (5, 3, 2),
        (6, 3, 2),
        (7, 3, 3),
        (8, 3, 3),
        (9, 3, 3),
    ];
    for (count, cols, rows) in table {
        assert_eq!(grid(count), (cols, rows), "n={count}");
    }
}

#[test]
fn grid_clamps_degenerate_and_oversized_counts() {
    assert_eq!(grid(0), (1, 1));
    assert_eq!(grid(12), (3, 3));
}

#[test]
fn cell_placement_is_row_major() {
    assert_eq!(cell(0, 3), (0, 0));
    assert_eq!(cell(2, 3), (2, 0));
    assert_eq!(cell(3, 3), (0, 1));
    assert_eq!(cell(5, 3), (2, 1));
    assert_eq!(cell(6, 3), (0, 2));
    assert_eq!(cell(1, 2), (1, 0));
    assert_eq!(cell(2, 2), (0, 1));
}

/// Each `(from, direction, expected)` move in a `cols`-wide grid of `count` cells.
fn assert_moves(cols: usize, count: usize, moves: &[(usize, Direction, usize)]) {
    for (from, direction, expected) in moves {
        assert_eq!(
            move_index(*from, *direction, cols, count),
            *expected,
            "{from} {direction:?} in {cols}x{count}"
        );
    }
}

#[test]
fn moves_in_full_grids() {
    use Direction::{Down, Left, Right, Up};
    // 0 1
    assert_moves(
        2,
        2,
        &[
            (0, Right, 1),
            (1, Left, 0),
            (0, Left, 0),
            (1, Right, 1),
            (0, Up, 0),
            (0, Down, 0),
        ],
    );
    // 0 1 / 2 3
    assert_moves(2, 4, &[(0, Down, 2), (3, Up, 1), (3, Left, 2), (2, Down, 2)]);
    // 0 1 2 / 3 4 5 / 6 7 8
    let nine = [
        (4, Up, 1),
        (4, Down, 7),
        (4, Left, 3),
        (4, Right, 5),
        (8, Down, 8),
        (8, Right, 8),
    ];
    assert_moves(3, 9, &nine);
    // 0 1 2 / 3 4 5
    assert_moves(3, 6, &[(1, Down, 4), (2, Right, 2), (5, Down, 5)]);
}

#[test]
fn moves_clamp_at_a_ragged_last_row() {
    use Direction::{Down, Right, Up};
    // 0 1 / 2
    assert_moves(2, 3, &[(0, Down, 2), (1, Down, 1), (2, Right, 2), (2, Up, 0)]);
    // 0 1 2 / 3 4
    assert_moves(3, 5, &[(4, Right, 4), (2, Down, 2), (1, Down, 4), (3, Up, 0)]);
    // 0 1 2 / 3 4 5 / 6
    assert_moves(3, 7, &[(3, Down, 6), (4, Down, 4)]);
    // 0 1 2 / 3 4 5 / 6 7
    assert_moves(3, 8, &[(4, Down, 7), (7, Up, 4)]);
}

#[test]
fn moves_defend_against_zero_cols_and_an_out_of_range_start() {
    use Direction::{Down, Left, Right, Up};
    assert_eq!(cell(2, 0), (0, 2));
    assert_eq!(move_index(1, Right, 0, 3), 1);
    assert_eq!(move_index(1, Up, 0, 3), 0);
    assert_eq!(move_index(1, Down, 0, 3), 2);
    assert_eq!(move_index(2, Down, 0, 3), 2);
    assert_eq!(move_index(9, Left, 3, 5), 4);
}

/// Font sizes are whole points here: compared as integers.
fn points(size: f64) -> i64 {
    #[expect(clippy::cast_possible_truncation, reason = "test sizes are small whole numbers")]
    let whole = size.round() as i64;
    whole
}

#[test]
fn the_auto_font_size_scales_by_grid_shape_and_floors() {
    assert_eq!(points(GHOSTTY_DEFAULT_FONT_SIZE), 13);
    let at = |cols, rows, base| points(dashboard_font_size(cols, rows, base));
    let thirteen = [
        at(1, 1, 13.0),
        at(2, 1, 13.0),
        at(2, 2, 13.0),
        at(3, 2, 13.0),
        at(3, 3, 13.0),
    ];
    assert_eq!(thirteen, [13, 11, 10, 8, 7]);
    let sixteen = [
        at(1, 1, 16.0),
        at(2, 1, 16.0),
        at(2, 2, 16.0),
        at(3, 2, 16.0),
        at(3, 3, 16.0),
    ];
    assert_eq!(sixteen, [16, 14, 12, 10, 9]);
    assert_eq!(at(3, 3, 8.0), points(MIN_FONT_SIZE), "8 × 0.55 floors at the minimum");
    assert_eq!(at(2, 2, 6.0), points(MIN_FONT_SIZE));
    assert_eq!(at(1, 1, 6.0), 6);
}

#[test]
fn the_applied_size_resolves_per_mode() {
    assert_eq!(DashboardFontMode::Untouched.applied_font_size(4, 13.0), None);
    assert_eq!(DashboardFontMode::Fixed(20.0).applied_font_size(9, 13.0), Some(20.0));
    assert_eq!(DashboardFontMode::Auto.applied_font_size(4, 16.0), Some(12.0));
    assert_eq!(DashboardFontMode::Auto.applied_font_size(9, 13.0), Some(7.0));
    let names = [
        DashboardFontMode::Untouched,
        DashboardFontMode::Fixed(1.0),
        DashboardFontMode::Auto,
    ]
    .map(DashboardFontMode::wire_name);
    assert_eq!(names, ["untouched", "fixed", "auto"]);
}

// MARK: - targets

const UUID: &str = "9F3CAAAA-0000-0000-0000-000000000001";

fn target(head: &str, pane: Option<ZoomSurface>) -> DashboardTarget {
    DashboardTarget {
        head: head.to_owned(),
        pane,
    }
}

#[test]
fn bare_targets_take_every_pane() {
    for head in [UUID, "active", "9F3C"] {
        assert_eq!(DashboardTarget::parse(head), Some(target(head, None)));
    }
}

#[test]
fn pane_suffixes_and_their_aliases_select_one_cell_in_any_case() {
    for suffix in ["left", "top", "primary", "LEFT", "Left", "lEfT"] {
        let raw = format!("{UUID}:{suffix}");
        assert_eq!(
            DashboardTarget::parse(&raw),
            Some(target(UUID, Some(ZoomSurface::Primary))),
            "{raw}"
        );
    }
    for suffix in ["right", "bottom", "split"] {
        let raw = format!("{UUID}:{suffix}");
        assert_eq!(
            DashboardTarget::parse(&raw),
            Some(target(UUID, Some(ZoomSurface::Split))),
            "{raw}"
        );
    }
    assert_eq!(
        DashboardTarget::parse("active:left"),
        Some(target("active", Some(ZoomSurface::Primary)))
    );
    assert_eq!(
        DashboardTarget::parse("9F3C:right"),
        Some(target("9F3C", Some(ZoomSurface::Split)))
    );
}

#[test]
fn malformed_targets_are_refused() {
    let surface_form = format!("surface:{UUID}:left");
    let refused = [
        String::new(),
        ":left".to_owned(),
        format!("{UUID}:"),
        format!("{UUID}:lft"),
        format!("{UUID}:scratch"),
        format!("{UUID}:overlay"),
        format!("{UUID}:overlay-left"),
        format!("{UUID}:overlay-right"),
        surface_form,
    ];
    for raw in refused {
        assert_eq!(DashboardTarget::parse(&raw), None, "{raw}");
    }
}

// MARK: - controller

#[test]
fn open_sets_members_highlight_and_mode_then_close_resets() {
    let mut controller = DashboardController::default();
    assert!(!controller.is_open());
    let [a, b, c] = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let members = vec![primary(a), primary(b), primary(c)];
    controller.open(members.clone(), Some(primary(b)), DashboardFontMode::Auto);
    assert!(controller.is_open());
    assert_eq!(controller.members(), members);
    assert_eq!(controller.highlighted(), Some(primary(b)));
    assert_eq!(controller.font_mode(), DashboardFontMode::Auto);
    controller.set_applied_font_size(Some(9.0));
    controller.close();
    assert_eq!(controller, DashboardController::default());
}

#[test]
fn the_highlight_starts_on_the_supplied_member_else_the_first() {
    let [a, b, c] = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let members = vec![primary(a), primary(b), primary(c)];
    let mut controller = DashboardController::default();
    controller.open(members.clone(), None, DashboardFontMode::Untouched);
    assert_eq!(controller.highlighted(), Some(primary(a)));
    controller.open(members.clone(), Some(primary(c)), DashboardFontMode::Untouched);
    assert_eq!(controller.highlighted(), Some(primary(c)));
    controller.open(members, Some(primary(Uuid::new_v4())), DashboardFontMode::Untouched);
    assert_eq!(controller.highlighted(), Some(primary(a)));
}

#[test]
fn a_reopen_over_the_same_members_takes_the_latest_mode() {
    let members = vec![primary(Uuid::new_v4()), primary(Uuid::new_v4())];
    let mut controller = DashboardController::default();
    controller.open(members.clone(), Some(members[1]), DashboardFontMode::Fixed(20.0));
    controller.open(members.clone(), Some(members[1]), DashboardFontMode::Untouched);
    assert_eq!(controller.highlighted(), Some(members[1]));
    assert_eq!(controller.font_mode(), DashboardFontMode::Untouched);
}

#[test]
fn the_highlight_walks_the_grid_and_clamps() {
    let members: Vec<DashboardMember> = ids(4).into_iter().map(primary).collect();
    let mut controller = DashboardController::default();
    controller.open(members.clone(), None, DashboardFontMode::Untouched);
    for (direction, expected) in [
        (Direction::Right, 1),
        (Direction::Down, 3),
        (Direction::Left, 2),
        (Direction::Up, 0),
        (Direction::Up, 0),
        (Direction::Left, 0),
    ] {
        controller.move_highlight(direction);
        assert_eq!(controller.highlighted(), Some(members[expected]), "{direction:?}");
    }

    let ragged: Vec<DashboardMember> = ids(5).into_iter().map(primary).collect();
    controller.open(ragged.clone(), Some(ragged[4]), DashboardFontMode::Untouched);
    controller.move_highlight(Direction::Right);
    controller.move_highlight(Direction::Down);
    assert_eq!(controller.highlighted(), Some(ragged[4]));
    controller.move_highlight(Direction::Up);
    assert_eq!(controller.highlighted(), Some(ragged[1]));

    let session = Uuid::new_v4();
    controller.open(
        vec![primary(session), split(session)],
        None,
        DashboardFontMode::Untouched,
    );
    controller.move_highlight(Direction::Right);
    assert_eq!(
        controller.highlighted(),
        Some(split(session)),
        "a split session is two cells"
    );
}

#[test]
fn highlight_and_move_change_nothing_off_the_grid_or_closed() {
    let mut controller = DashboardController::default();
    controller.highlight(primary(Uuid::new_v4()));
    controller.move_highlight(Direction::Right);
    assert_eq!(controller.highlighted(), None);

    let [a, c] = [Uuid::new_v4(), Uuid::new_v4()];
    controller.open(vec![primary(a), primary(c)], None, DashboardFontMode::Untouched);
    controller.highlight(primary(c));
    assert_eq!(controller.highlighted(), Some(primary(c)));
    controller.highlight(primary(Uuid::new_v4()));
    assert_eq!(controller.highlighted(), Some(primary(c)));
}

#[test]
fn reconcile_drops_gone_cells_moves_the_highlight_and_closes_when_empty() {
    let [a, b, c] = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
    let mut controller = DashboardController::default();
    controller.open(
        vec![primary(a), split(a), primary(b), primary(c)],
        Some(split(a)),
        DashboardFontMode::Fixed(14.0),
    );
    controller.set_applied_font_size(Some(14.0));
    controller.reconcile(&[primary(a), primary(b), primary(c)].into_iter().collect());
    assert_eq!(controller.members(), [primary(a), primary(b), primary(c)]);
    assert_eq!(controller.highlighted(), Some(primary(a)));
    controller.highlight(primary(b));
    controller.reconcile(&[primary(a), primary(c)].into_iter().collect());
    assert_eq!(controller.highlighted(), Some(primary(a)), "the first survivor");
    controller.reconcile(&[primary(a), primary(c)].into_iter().collect());
    assert_eq!(controller.members(), [primary(a), primary(c)], "a no-op reconcile");
    controller.reconcile(&HashSet::new());
    assert_eq!(controller, DashboardController::default());
}

#[test]
fn a_promotion_rewrites_or_collapses_the_split_cell() {
    let [a, b] = [Uuid::new_v4(), Uuid::new_v4()];
    let mut controller = DashboardController::default();
    controller.open(vec![split(a)], None, DashboardFontMode::Untouched);
    controller.promote_split_member(a);
    assert_eq!(controller.members(), [primary(a)]);
    assert_eq!(controller.highlighted(), Some(primary(a)));

    controller.open(
        vec![primary(a), split(a), primary(b)],
        Some(split(a)),
        DashboardFontMode::Untouched,
    );
    controller.promote_split_member(a);
    assert_eq!(controller.members(), [primary(a), primary(b)]);
    assert_eq!(controller.highlighted(), Some(primary(a)));

    controller.open(
        vec![split(a), primary(b)],
        Some(primary(b)),
        DashboardFontMode::Untouched,
    );
    controller.promote_split_member(a);
    assert_eq!(controller.members(), [primary(a), primary(b)]);
    assert_eq!(controller.highlighted(), Some(primary(b)));

    controller.open(vec![primary(a)], None, DashboardFontMode::Untouched);
    controller.promote_split_member(a);
    assert_eq!(controller.members(), [primary(a)]);
}

#[test]
fn control_refs_name_the_session_and_pane() {
    let id = Uuid::parse_str(UUID).unwrap();
    assert_eq!(primary(id).control_ref(), format!("{UUID}:left"));
    assert_eq!(split(id).control_ref(), format!("{UUID}:right"));
}

// MARK: - the store's cells

/// A store of one workspace with `count` sessions, the last added selected.
fn store_with(count: usize) -> (AppStore, Vec<Uuid>) {
    let mut store = AppStore::new();
    let workspace = store.add_workspace("work");
    let ids = (0..count)
        .map(|at| {
            store
                .add_session(workspace, NewSession::at_cwd(format!("/{at}")))
                .unwrap()
        })
        .collect();
    (store, ids)
}

fn resolved(session: Uuid, pane: Option<ZoomSurface>) -> ResolvedDashboardTarget {
    ResolvedDashboardTarget { session, pane }
}

#[test]
fn bare_sessions_expand_to_their_panes_in_order() {
    let (mut store, ids) = store_with(2);
    store.set_split_visibility(ids[1], true, None);
    let (members, dropped) = store.dashboard_members_for_ids(&ids, MAX_CELLS);
    assert_eq!(dropped, 0);
    assert_eq!(members, [primary(ids[0]), primary(ids[1]), split(ids[1])]);
    store.set_split_visibility(ids[1], false, None);
    assert_eq!(
        store.dashboard_members_for_ids(&ids, MAX_CELLS).0.len(),
        3,
        "a hidden split still counts"
    );
}

#[test]
fn the_cap_counts_panes_and_reports_the_dropped() {
    let (store, ids) = store_with(5);
    let (members, dropped) = store.dashboard_members_for_ids(&ids, 3);
    assert_eq!(members, ids[..3].iter().copied().map(primary).collect::<Vec<_>>());
    assert_eq!(dropped, 2);
    let targets: Vec<_> = ids.iter().map(|id| resolved(*id, Some(ZoomSurface::Primary))).collect();
    assert_eq!(store.dashboard_members(&targets, 3).1, 2);
}

#[test]
fn unknown_sessions_are_skipped() {
    let (store, ids) = store_with(1);
    let targets = [
        resolved(Uuid::new_v4(), None),
        resolved(Uuid::new_v4(), Some(ZoomSurface::Primary)),
        resolved(ids[0], None),
    ];
    assert_eq!(store.dashboard_members(&targets, MAX_CELLS), (vec![primary(ids[0])], 0));
}

#[test]
fn pane_targets_take_one_cell_keep_order_and_collapse_repeats() {
    let (mut store, ids) = store_with(2);
    store.set_split_visibility(ids[0], true, None);
    store.set_split_visibility(ids[1], true, None);
    let mixed = [
        resolved(ids[0], Some(ZoomSurface::Primary)),
        resolved(ids[1], Some(ZoomSurface::Split)),
    ];
    assert_eq!(
        store.dashboard_members(&mixed, MAX_CELLS).0,
        [primary(ids[0]), split(ids[1])]
    );
    let overlapping = [resolved(ids[0], None), resolved(ids[0], Some(ZoomSurface::Primary))];
    assert_eq!(
        store.dashboard_members(&overlapping, MAX_CELLS).0,
        [primary(ids[0]), split(ids[0])]
    );
    let repeated = [
        resolved(ids[0], Some(ZoomSurface::Split)),
        resolved(ids[0], Some(ZoomSurface::Split)),
    ];
    assert_eq!(store.dashboard_members(&repeated, MAX_CELLS).0, [split(ids[0])]);
}

#[test]
fn mru_members_follow_recency_and_expand() {
    let (mut store, ids) = store_with(2);
    store.set_split_visibility(ids[1], true, None);
    store.select_session(Some(ids[0]));
    store.select_session(Some(ids[1]));
    assert_eq!(
        store.dashboard_mru_members(MAX_CELLS),
        [primary(ids[1]), split(ids[1]), primary(ids[0])]
    );
    assert!(AppStore::new().dashboard_mru_members(MAX_CELLS).is_empty());
}

#[test]
fn a_store_sync_follows_a_promotion_and_prunes_a_closed_split() {
    let (mut store, ids) = store_with(1);
    let id = ids[0];
    store.set_split_visibility(id, true, None);
    let session = store.session_mut(id).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("L")));
    session.split_surface.replace(Box::new(FakeSurface::new("R")));
    let mut controller = DashboardController::default();
    controller.open(vec![split(id)], None, DashboardFontMode::Untouched);
    controller.track_splits(&store);
    store.swap_panes(id).unwrap();
    controller.sync_with_store(&store);
    assert_eq!(controller.members(), [split(id)], "a swap keeps the cell on its slot");

    store.close_primary_pane(id);
    controller.sync_with_store(&store);
    assert_eq!(
        controller.members(),
        [primary(id)],
        "the cell followed the promoted pane"
    );

    store.set_split_visibility(id, true, None);
    controller.open(vec![primary(id), split(id)], None, DashboardFontMode::Untouched);
    controller.track_splits(&store);
    store.close_split(id);
    controller.sync_with_store(&store);
    assert_eq!(
        controller.members(),
        [primary(id)],
        "the split's own exit drops its cell"
    );
}
