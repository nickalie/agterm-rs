//! `session.move`: reorder, to a workspace, next to an anchor, and their batch forms.

use ngterm_proto::Args;
use ngterm_proto::command::SessionCmd;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, answered_affected, answered_id, args, failure, wire};

/// `ids` with `id` moved one step `to` (`up|down|top|bottom`), as the suite models it.
fn reordered(ids: &[Uuid], id: Uuid, to: &str) -> Vec<Uuid> {
    let mut ids = ids.to_vec();
    let Some(index) = ids.iter().position(|candidate| *candidate == id) else {
        return ids;
    };
    ids.remove(index);
    let destination = match to {
        "up" => index.saturating_sub(1),
        "down" => (index + 1).min(ids.len()),
        "top" => 0,
        _ => ids.len(),
    };
    ids.insert(destination, id);
    ids
}

fn move_args(edit: impl FnOnce(&mut Args)) -> Args {
    args(edit)
}

#[test]
fn move_reorders_within_the_workspace_and_stops_at_the_ends() {
    let mut fixture = Fixture::new(2);
    let mut ids = fixture.sessions.clone();
    let id = ids[2];
    for to in ["up", "up", "up", "top", "down", "bottom", "down", "top"] {
        let response = fixture.session(SessionCmd::Move, Some(&wire(id)), move_args(|a| a.to = Some(to.into())));
        assert_eq!(answered_id(&response), Some(id), "{to}");
        ids = reordered(&ids, id, to);
        assert_eq!(fixture.order(fixture.workspace), ids, "{to}");
    }
}

#[test]
fn move_to_another_workspace_appends() {
    let mut fixture = Fixture::new(1);
    let (moving, staying) = (fixture.sessions[0], fixture.sessions[1]);
    let (destination, resident) = fixture.add_workspace("destination", 1);
    let response = fixture.session(
        SessionCmd::Move,
        Some(&wire(moving)),
        move_args(|a| a.workspace = Some(wire(destination))),
    );
    assert_eq!(answered_id(&response), Some(moving));
    assert_eq!(fixture.order(fixture.workspace), [staying]);
    assert_eq!(fixture.order(destination), [resident[0], moving]);
}

#[test]
fn move_next_to_an_anchor_across_workspaces() {
    let mut fixture = Fixture::new(1);
    let (moving, other) = (fixture.sessions[0], fixture.sessions[1]);
    let (destination, ids) = fixture.add_workspace("destination", 2);
    let (first, last) = (ids[0], ids[1]);
    let before_last = move_args(|a| a.before = Some(wire(last)));
    assert_eq!(
        answered_id(&fixture.session(SessionCmd::Move, Some(&wire(moving)), before_last)),
        Some(moving)
    );
    assert_eq!(fixture.order(destination), [first, moving, last]);
    let after_other = move_args(|a| a.after = Some(wire(other)));
    fixture.session(SessionCmd::Move, Some(&wire(moving)), after_other);
    assert_eq!(fixture.order(fixture.workspace), [other, moving]);
    assert_eq!(fixture.order(destination), [first, last]);
}

/// Placed like a sidebar drop: after an anchor below it lands right after the anchor, the drop's post-removal index.
#[test]
fn move_after_an_anchor_below_it_lands_right_after_it() {
    let mut fixture = Fixture::new(2);
    let [moving, anchor, last] = [0, 1, 2].map(|at| fixture.sessions[at]);
    let response = fixture.session(
        SessionCmd::Move,
        Some(&wire(moving)),
        move_args(|a| a.after = Some(wire(anchor))),
    );
    assert_eq!(answered_id(&response), Some(moving));
    assert_eq!(fixture.order(fixture.workspace), [anchor, moving, last]);
    // already in place: a no-op that still answers the id
    let response = fixture.session(
        SessionCmd::Move,
        Some(&wire(moving)),
        move_args(|a| a.after = Some(wire(anchor))),
    );
    assert_eq!(answered_id(&response), Some(moving));
    assert_eq!(fixture.order(fixture.workspace), [anchor, moving, last]);
}

#[test]
fn move_several_targets_keeps_tree_order_and_counts() {
    let mut fixture = Fixture::new(2);
    let [first, keep, last] = [0, 1, 2].map(|at| fixture.sessions[at]);
    let (destination, resident) = fixture.add_workspace("destination", 1);
    let to_destination = move_args(|a| {
        a.workspace = Some(wire(destination));
        a.targets = Some(vec![wire(last), wire(first)]);
    });
    let response = fixture.session(SessionCmd::Move, None, to_destination);
    assert_eq!(answered_affected(&response), Some(2));
    assert_eq!(fixture.order(fixture.workspace), [keep]);
    assert_eq!(fixture.order(destination), [resident[0], first, last]);
    let before_keep = move_args(|a| {
        a.before = Some(wire(keep));
        a.targets = Some(vec![wire(first), wire(last)]);
    });
    let response = fixture.session(SessionCmd::Move, None, before_keep);
    assert_eq!(answered_affected(&response), Some(2));
    assert_eq!(fixture.order(fixture.workspace), [first, last, keep]);
    // a block dropped inside itself moves nothing
    let in_place = move_args(|a| {
        a.before = Some(wire(keep));
        a.targets = Some(vec![wire(first), wire(last)]);
    });
    assert_eq!(
        answered_affected(&fixture.session(SessionCmd::Move, None, in_place)),
        Some(0)
    );
    assert_eq!(fixture.order(fixture.workspace), [first, last, keep]);
}

#[test]
fn a_batch_after_an_anchor_lands_after_it_as_one_block() {
    let mut fixture = Fixture::new(3);
    let [a, b, c, d] = [0, 1, 2, 3].map(|at| fixture.sessions[at]);
    let after_c = move_args(|args| {
        args.after = Some(wire(c));
        args.targets = Some(vec![wire(b), wire(a)]);
    });
    assert_eq!(
        answered_affected(&fixture.session(SessionCmd::Move, None, after_c)),
        Some(2)
    );
    assert_eq!(fixture.order(fixture.workspace), [c, a, b, d]);
}

#[test]
fn a_batch_of_one_target_moves_as_the_target_alone() {
    let mut fixture = Fixture::new(1);
    let moving = fixture.sessions[0];
    let (destination, _) = fixture.add_workspace("destination", 0);
    let one = move_args(|a| {
        a.workspace = Some(wire(destination));
        a.targets = Some(vec![wire(moving), wire(moving)[..8].to_lowercase()]);
    });
    // deduplicated to one, it still moves as a batch and counts
    assert_eq!(
        answered_affected(&fixture.session(SessionCmd::Move, None, one)),
        Some(1)
    );
    assert_eq!(fixture.order(destination), [moving]);
    let single = move_args(|a| {
        a.workspace = Some(wire(fixture.workspace));
        a.targets = Some(vec![wire(moving)]);
    });
    assert_eq!(
        answered_id(&fixture.session(SessionCmd::Move, None, single)),
        Some(moving)
    );
    assert!(fixture.order(destination).is_empty());
}

#[test]
fn a_batch_with_an_unknown_target_moves_nothing() {
    let mut fixture = Fixture::new(1);
    let (destination, _) = fixture.add_workspace("destination", 0);
    let batch = move_args(|a| {
        a.workspace = Some(wire(destination));
        a.targets = Some(vec![wire(fixture.sessions[0]), UNKNOWN.to_owned()]);
    });
    assert_eq!(
        fixture.session(SessionCmd::Move, None, batch),
        failure("no such session: zzz")
    );
    assert_eq!(fixture.order(fixture.workspace), fixture.sessions);
}

#[test]
fn move_rejects_every_invalid_form_without_moving() {
    let mut fixture = Fixture::new(1);
    let [sid, anchor] = [0, 1].map(|at| wire(fixture.sessions[at]));
    let workspace = wire(fixture.workspace);
    let cases: [(Args, &str); 10] = [
        (Args::default(), "session.move requires --to or a workspace"),
        (
            move_args(|a| a.to = Some("left".into())),
            "session.move --to must be up|down|top|bottom",
        ),
        (
            move_args(|a| {
                a.to = Some("up".into());
                a.workspace = Some(workspace.clone());
            }),
            "session.move takes either --to or a workspace, not both",
        ),
        (
            move_args(|a| {
                a.after = Some(anchor.clone());
                a.before = Some(anchor.clone());
            }),
            "use either --after or --before, not both",
        ),
        (
            move_args(|a| {
                a.after = Some(anchor.clone());
                a.to = Some("up".into());
            }),
            "session.move takes --after/--before or --to, not both",
        ),
        (
            move_args(|a| {
                a.before = Some(anchor.clone());
                a.workspace = Some(workspace.clone());
            }),
            "session.move takes --after/--before or a workspace, not both",
        ),
        (
            move_args(|a| {
                a.to = Some("up".into());
                a.targets = Some(vec![sid.clone(), anchor.clone()]);
            }),
            "session.move --target can be repeated only with a workspace or --after/--before",
        ),
        (
            move_args(|a| {
                a.workspace = Some(workspace.clone());
                a.targets = Some(Vec::new());
            }),
            "session.move requires at least one --target",
        ),
        (
            move_args(|a| a.workspace = Some(UNKNOWN.into())),
            "no such workspace: zzz",
        ),
        (move_args(|a| a.after = Some(UNKNOWN.into())), "no such session: zzz"),
    ];
    for (request, error) in cases {
        assert_eq!(
            fixture.session(SessionCmd::Move, Some(&sid), request),
            failure(error),
            "{error}"
        );
        assert_eq!(fixture.order(fixture.workspace), fixture.sessions, "{error}");
    }
    assert_eq!(
        fixture.session(SessionCmd::Move, Some(UNKNOWN), move_args(|a| a.to = Some("up".into()))),
        failure("no such session: zzz")
    );
}
