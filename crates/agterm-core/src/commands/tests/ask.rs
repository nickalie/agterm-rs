//! `ask.*` (upstream's `ControlDispatcherAskTests`, `ControlServerAskTests`, `AskTests` and the ask case of
//! `AppStorePendingCloseTests`): full error strings, the session and window slots, results and cancels.

use agterm_proto::command::{AskCmd, PickCmd, SessionCmd, SurfaceCmd};
use agterm_proto::{Args, AskButton, Command, ControlAskOutcome, ControlAskResult, PickItem, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure, wire};
use crate::model::{AskNavigation, AskStyle, PendingAsk};

fn button(id: &str, label: &str, hotkey: Option<&str>) -> AskButton {
    AskButton {
        id: id.to_owned(),
        label: label.to_owned(),
        hotkey: hotkey.map(str::to_owned),
    }
}

fn yes_no() -> Vec<AskButton> {
    vec![button("yes", "Yes", Some("Y")), button("no", "No", None)]
}

fn ask_args(edit: impl FnOnce(&mut Args)) -> Args {
    args(|a| {
        a.title = Some("Proceed?".to_owned());
        a.buttons = Some(yes_no());
        edit(a);
    })
}

fn open(fixture: &mut Fixture, target: Option<Uuid>, edit: impl FnOnce(&mut Args)) -> Response {
    let target = target.map(wire);
    fixture.send(Command::Ask(AskCmd::Open), target.as_deref(), ask_args(edit))
}

fn opened_id(response: &Response) -> String {
    assert!(response.ok, "{response:?}");
    response.result.as_ref().unwrap()["id"].as_str().unwrap().to_owned()
}

fn result(fixture: &mut Fixture, id: &str) -> Response {
    fixture.send(Command::Ask(AskCmd::Result), Some(id), Args::default())
}

fn outcome(fixture: &mut Fixture, id: &str) -> Value {
    let response = result(fixture, id);
    assert!(response.ok, "{response:?}");
    response.result.unwrap()["ask"].clone()
}

fn cancel(fixture: &mut Fixture, id: &str) -> Response {
    fixture.send(Command::Ask(AskCmd::Cancel), Some(id), Args::default())
}

fn tree(fixture: &mut Fixture) -> Value {
    let response = fixture.send(Command::Tree, None, Args::default());
    response.result.expect("a tree")["tree"].clone()
}

fn node(fixture: &mut Fixture, id: Uuid) -> Value {
    tree(fixture)["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["sessions"].as_array().cloned().unwrap_or_default())
        .find(|node| node["id"] == json!(wire(id)))
        .expect("the session's node")
}

fn split_on(fixture: &mut Fixture, id: Uuid) {
    let response = fixture.session(SessionCmd::Split, Some(&wire(id)), args(|a| a.mode = Some("on".into())));
    assert!(response.ok, "{response:?}");
}

#[test]
fn open_refusals_carry_upstreams_strings_in_its_order() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    let seven: Vec<AskButton> = (0..7).map(|n| button(&n.to_string(), &n.to_string(), None)).collect();
    let cases: Vec<(Args, &str)> = vec![
        (args(|a| a.buttons = Some(yes_no())), "ask.open requires a title"),
        (ask_args(|a| a.title = Some("  ".into())), "ask.open requires a title"),
        (ask_args(|a| a.buttons = None), "ask.open requires buttons"),
        (
            ask_args(|a| a.buttons = Some(Vec::new())),
            "ask.open requires at least one button",
        ),
        (ask_args(|a| a.buttons = Some(seven)), "too many buttons (max 6)"),
        (
            ask_args(|a| a.buttons = Some(vec![button("a", "", None)])),
            "ask button label must not be empty",
        ),
        (
            ask_args(|a| a.buttons = Some(vec![button("a", "A", None), button("a", "B", None)])),
            "ask button ids must be unique",
        ),
        (
            ask_args(|a| a.title = Some("Q\n".into())),
            "ask text must not contain control characters",
        ),
        (
            ask_args(|a| a.message = Some("a\tb".into())),
            "ask text must not contain control characters",
        ),
        (
            ask_args(|a| a.message = Some("a\u{7f}".into())),
            "ask text must not contain control characters",
        ),
        (
            ask_args(|a| a.default_button = Some("maybe".into())),
            "unknown default button: maybe",
        ),
        (
            ask_args(|a| a.destructive_button = Some("maybe".into())),
            "unknown destructive button: maybe",
        ),
        (
            ask_args(|a| {
                a.default_button = Some("no".into());
                a.destructive_button = Some("no".into());
            }),
            "default button must not be destructive",
        ),
        (ask_args(|a| a.style = Some("GUI".into())), "unknown style"),
        (ask_args(|a| a.align = Some("top".into())), "unknown align"),
        (ask_args(|a| a.width = Some(9)), "width must be 10 to 100"),
        (ask_args(|a| a.width = Some(101)), "width must be 10 to 100"),
        (
            ask_args(|a| a.pane = Some("scratch".into())),
            "--pane must be left or right",
        ),
        (
            ask_args(|a| a.pane = Some("RIGHT".into())),
            "--pane must be left or right",
        ),
    ];
    for (request, error) in cases {
        let response = fixture.send(Command::Ask(AskCmd::Open), Some(&wire(id)), request);
        assert_eq!(response, failure(error));
    }
    assert!(node(&mut fixture, id).get("ask").is_none());
    assert!(tree(&mut fixture).get("askPending").is_none());
}

#[test]
fn hotkey_and_gui_pane_refusals() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    for hotkey in ["", "yes", "1", "!", " ", "\t", "é", "🦊"] {
        let response = open(&mut fixture, Some(id), |a| {
            a.buttons = Some(vec![button("a", "A", Some(hotkey))]);
        });
        assert_eq!(
            response,
            failure("ask button hotkey must be one ASCII letter"),
            "{hotkey:?}"
        );
    }
    let twice = vec![button("a", "A", Some("x")), button("b", "B", Some("X"))];
    assert_eq!(
        open(&mut fixture, Some(id), |a| a.buttons = Some(twice)),
        failure("ask button hotkeys must be unique")
    );
    for (pane, pane_id) in [(Some("right"), None), (None, Some("pane-id")), (None, Some(""))] {
        let response = open(&mut fixture, None, |a| {
            a.style = Some("gui".into());
            a.pane = pane.map(str::to_owned);
            a.pane_id = pane_id.map(str::to_owned);
        });
        assert_eq!(response, failure("--pane requires a session"));
    }
    assert!(node(&mut fixture, id).get("ask").is_none());
    assert!(tree(&mut fixture).get("askPending").is_none());
}

#[test]
fn lookups_need_an_id_and_a_known_one() {
    let mut fixture = Fixture::new(0);
    for (cmd, error) in [
        (AskCmd::Result, "ask.result requires an ask id"),
        (AskCmd::Cancel, "ask.cancel requires an ask id"),
    ] {
        assert_eq!(fixture.send(Command::Ask(cmd), None, Args::default()), failure(error));
        assert_eq!(
            fixture.send(Command::Ask(cmd), Some(UNKNOWN), Args::default()),
            failure(&format!("unknown ask: {UNKNOWN}"))
        );
    }
}

#[test]
fn a_terminal_ask_waits_on_its_session_and_cancel_keeps_its_result() {
    let mut fixture = Fixture::new(1);
    let (hidden, shown) = (fixture.sessions[0], fixture.sessions[1]);
    let ask_id = opened_id(&open(&mut fixture, Some(hidden), |a| {
        a.message = Some("Deploy.".into());
        a.default_button = Some("yes".into());
    }));
    assert_eq!(fixture.selected(), Some(shown), "a background ask selects nothing");
    assert_eq!(node(&mut fixture, hidden)["ask"], json!({"id": ask_id}));
    assert!(tree(&mut fixture).get("askPending").is_none());
    assert_eq!(outcome(&mut fixture, &ask_id), json!({"result": "pending"}));
    assert_eq!(open(&mut fixture, Some(hidden), |_| {}), failure("ask already pending"));
    let pending = fixture.store().session(hidden).unwrap().ask().unwrap().ask.clone();
    assert_eq!(
        pending.buttons[0].hotkey.as_deref(),
        Some("y"),
        "hotkeys are lower-cased"
    );
    assert_eq!(pending.style, AskStyle::Terminal);

    assert!(cancel(&mut fixture, &ask_id).ok);
    assert_eq!(outcome(&mut fixture, &ask_id), json!({"result": "cancelled"}));
    assert!(node(&mut fixture, hidden).get("ask").is_none());
    assert!(cancel(&mut fixture, &ask_id).ok, "a finished ask cancels as a no-op");
    assert_eq!(outcome(&mut fixture, &ask_id), json!({"result": "cancelled"}));
}

#[test]
fn a_terminal_ask_without_a_target_lands_on_the_selected_session() {
    let mut fixture = Fixture::new(1);
    let selected = fixture.selected().unwrap();
    let ask_id = opened_id(&open(&mut fixture, None, |_| {}));
    assert_eq!(node(&mut fixture, selected)["ask"], json!({"id": ask_id}));
}

#[test]
fn a_terminal_ask_anchors_to_a_laid_out_pane_and_a_live_token_wins() {
    let mut fixture = Fixture::new(0);
    let id = fixture.sessions[0];
    assert_eq!(
        open(&mut fixture, Some(id), |a| a.pane = Some("right".into())),
        failure("pane not visible")
    );
    assert_eq!(
        open(&mut fixture, Some(id), |a| a.pane_id = Some(UNKNOWN.into())),
        failure(&format!("unknown pane id: {UNKNOWN}"))
    );
    split_on(&mut fixture, id);
    let response = open(&mut fixture, Some(id), |a| a.pane = Some("split".into()));
    assert_eq!(response.result.as_ref().unwrap()["pane"], "right");
    let ask_id = opened_id(&response);
    assert_eq!(node(&mut fixture, id)["ask"], json!({"id": ask_id, "pane": "right"}));
    assert!(cancel(&mut fixture, &ask_id).ok);

    let left = agterm_host::fakes::FakeSurface::new("L");
    fixture
        .store_mut()
        .and_then(|store| store.session_mut(id))
        .unwrap()
        .surface
        .replace(Box::new(left));
    let left_token = "L".to_owned();
    let response = open(&mut fixture, Some(id), |a| {
        a.pane = Some("right".into());
        a.pane_id = Some(left_token);
    });
    assert_eq!(response.result.as_ref().unwrap()["pane"], "left");
}

#[test]
fn closing_the_covered_split_or_the_session_cancels_the_ask() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    split_on(&mut fixture, id);
    let on_split = opened_id(&open(&mut fixture, Some(id), |a| a.pane = Some("right".into())));
    fixture.store_mut().unwrap().close_split(id);
    assert_eq!(outcome(&mut fixture, &on_split), json!({"result": "cancelled"}));

    let soft = opened_id(&open(&mut fixture, Some(id), |_| {}));
    assert!(fixture.store_mut().unwrap().soft_close_session(id));
    assert_eq!(outcome(&mut fixture, &soft), json!({"result": "cancelled"}));
    assert!(fixture.store_mut().unwrap().undo_pending_close(None));
    assert!(
        fixture.store().session(id).unwrap().ask().is_none(),
        "an undo does not bring the ask back"
    );
    assert_eq!(outcome(&mut fixture, &soft), json!({"result": "cancelled"}));

    let hard = opened_id(&open(&mut fixture, Some(id), |_| {}));
    let response = fixture.session(SessionCmd::Close, Some(&wire(id)), Args::default());
    assert!(response.ok, "{response:?}");
    assert_eq!(outcome(&mut fixture, &hard), json!({"result": "cancelled"}));
}

#[test]
fn a_gui_ask_holds_the_window_slot_beside_a_terminal_ask() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let gui = opened_id(&open(&mut fixture, None, |a| {
        a.style = Some("gui".into());
        a.destructive_button = Some("no".into());
    }));
    assert_eq!(tree(&mut fixture)["askPending"], json!(gui));
    assert!(node(&mut fixture, sid).get("ask").is_none());
    assert_eq!(outcome(&mut fixture, &gui), json!({"result": "pending"}));
    assert_eq!(
        open(&mut fixture, None, |a| a.style = Some("gui".into())),
        failure("ask already pending")
    );
    let items = vec![PickItem {
        id: "a".into(),
        label: "Alpha".into(),
        subtitle: None,
    }];
    let pick = fixture.send(Command::Pick(PickCmd::Open), None, args(|a| a.items = Some(items)));
    assert_eq!(
        pick,
        failure("ask already pending"),
        "the picker names the ask holding the slot"
    );
    let terminal = opened_id(&open(&mut fixture, Some(sid), |_| {}));
    assert_eq!(node(&mut fixture, sid)["ask"], json!({"id": terminal}));

    assert!(cancel(&mut fixture, &gui).ok);
    assert!(tree(&mut fixture).get("askPending").is_none());
    assert_eq!(outcome(&mut fixture, &gui), json!({"result": "cancelled"}));
    assert_eq!(outcome(&mut fixture, &terminal), json!({"result": "pending"}));
}

#[test]
fn a_gui_ask_over_a_session_needs_it_on_screen() {
    let mut fixture = Fixture::new(1);
    let (hidden, shown) = (fixture.sessions[0], fixture.sessions[1]);
    assert_eq!(
        open(&mut fixture, Some(hidden), |a| a.style = Some("gui".into())),
        failure("session not visible")
    );
    let response = open(&mut fixture, Some(shown), |a| {
        a.style = Some("gui".into());
        a.pane = Some("left".into());
    });
    assert_eq!(response.result.as_ref().unwrap()["pane"], "left");
    let gui = opened_id(&response);
    assert_eq!(tree(&mut fixture)["askPending"], json!(gui));

    // selecting another session takes the anchor off screen: the dialog ends cancelled
    let selected = fixture.session(SessionCmd::Select, Some(&wire(hidden)), Args::default());
    assert!(selected.ok, "{selected:?}");
    fixture.core.commit();
    assert_eq!(outcome(&mut fixture, &gui), json!({"result": "cancelled"}));
    assert!(tree(&mut fixture).get("askPending").is_none());
}

#[test]
fn a_gui_ask_refuses_control_driven_covers_and_allows_dismissals() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let gui = opened_id(&open(&mut fixture, None, |a| a.style = Some("gui".into())));
    let blocked = failure("ask pending");
    let quick = fixture.send(
        Command::Quick(agterm_proto::command::QuickCmd::Visibility),
        None,
        args(|a| a.mode = Some("show".into())),
    );
    assert_eq!(quick, blocked);
    let zoom = |target: &str, mode: &str| {
        let target = target.to_owned();
        let mode = mode.to_owned();
        move |fixture: &mut Fixture| {
            fixture.send(
                Command::Surface(SurfaceCmd::Zoom),
                Some(&target),
                args(|a| a.mode = Some(mode)),
            )
        }
    };
    assert_eq!(zoom("active", "show")(&mut fixture), blocked);
    assert_eq!(
        zoom(&format!("surface:{}:left", wire(sid)), "show")(&mut fixture),
        blocked
    );
    let dashboard = fixture.send(Command::Dashboard, None, args(|a| a.targets = Some(vec![wire(sid)])));
    assert_eq!(dashboard, blocked);
    let search = fixture.session(
        SessionCmd::Search,
        Some(&wire(sid)),
        args(|a| a.text = Some("needle".into())),
    );
    assert_eq!(search, blocked);
    let next = fixture.session(
        SessionCmd::Search,
        Some(&wire(sid)),
        args(|a| a.to = Some("next".into())),
    );
    assert_eq!(next, blocked);

    let close = fixture.session(
        SessionCmd::Search,
        Some(&wire(sid)),
        args(|a| a.to = Some("close".into())),
    );
    assert!(close.ok, "{close:?}");
    let hide = fixture.send(
        Command::Quick(agterm_proto::command::QuickCmd::Visibility),
        None,
        args(|a| a.mode = Some("hide".into())),
    );
    assert!(hide.ok);
    assert!(zoom("active", "hide")(&mut fixture).ok);
    assert!(
        fixture
            .send(Command::Dashboard, None, args(|a| a.close = Some(true)))
            .ok
    );
    assert_eq!(tree(&mut fixture)["askPending"], json!(gui));
}

#[test]
fn a_user_answer_and_escape_end_the_ask_with_their_outcome() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let terminal = opened_id(&open(&mut fixture, Some(sid), |_| {}));
    let ask = fixture.store().session(sid).unwrap().ask().unwrap().ask.clone();
    fixture.core.answer_session_ask(sid, &terminal, ask.answer(1).unwrap());
    assert_eq!(
        outcome(&mut fixture, &terminal),
        json!({"result": "answered", "id": "no", "label": "No", "index": 1})
    );

    let window = fixture.window;
    let gui = opened_id(&open(&mut fixture, None, |a| a.style = Some("gui".into())));
    fixture
        .core
        .answer_window_ask(window, "stale", ControlAskResult::outcome(ControlAskOutcome::Escaped));
    assert_eq!(
        outcome(&mut fixture, &gui),
        json!({"result": "pending"}),
        "a stale id is ignored"
    );
    fixture
        .core
        .answer_window_ask(window, &gui, ControlAskResult::outcome(ControlAskOutcome::Escaped));
    assert_eq!(outcome(&mut fixture, &gui), json!({"result": "escaped"}));
    // the first outcome wins
    assert!(cancel(&mut fixture, &gui).ok);
    assert_eq!(outcome(&mut fixture, &gui), json!({"result": "escaped"}));
}

#[test]
fn results_are_scoped_to_the_named_window_and_outlive_it() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let window = fixture.window;
    let other = fixture.core.library_mut().new_window(None);
    let terminal = opened_id(&open(&mut fixture, Some(sid), |a| a.window = Some(wire(window))));
    let scoped = |fixture: &mut Fixture, id: &str, window: Uuid| {
        fixture.send(
            Command::Ask(AskCmd::Result),
            Some(id),
            args(|a| a.window = Some(wire(window))),
        )
    };
    assert!(scoped(&mut fixture, &terminal, window).ok);
    assert_eq!(
        scoped(&mut fixture, &terminal, other),
        failure(&format!("unknown ask: {terminal}"))
    );

    fixture.core.close_window(window);
    fixture.core.commit();
    assert_eq!(outcome(&mut fixture, &terminal), json!({"result": "cancelled"}));
}

#[test]
fn quitting_cancels_every_ask_and_pick() {
    let mut fixture = Fixture::new(0);
    let sid = fixture.sessions[0];
    let terminal = opened_id(&open(&mut fixture, Some(sid), |_| {}));
    let gui = opened_id(&open(&mut fixture, None, |a| a.style = Some("gui".into())));
    assert!(fixture.core.save_all());
    assert_eq!(outcome(&mut fixture, &terminal), json!({"result": "cancelled"}));
    assert_eq!(outcome(&mut fixture, &gui), json!({"result": "cancelled"}));
}

#[test]
fn follow_raises_the_owning_window() {
    let mut fixture = Fixture::new(0);
    let window = fixture.window;
    let _ = fixture.core.take_window_requests();
    opened_id(&open(&mut fixture, None, |a| a.follow = Some(true)));
    assert_eq!(
        fixture.core.take_window_requests(),
        vec![crate::windows::WindowRequest::Raise(window)]
    );
}

#[test]
fn navigation_starts_on_the_default_else_the_first_safe_button_and_wraps() {
    let ask = |default: Option<&str>, destructive: Option<&str>| PendingAsk {
        id: "x".into(),
        title: "T".into(),
        message: None,
        buttons: vec![
            button("a", "A", Some("q")),
            button("b", "B", None),
            button("c", "C", Some("z")),
        ],
        default_id: default.map(str::to_owned),
        destructive_id: destructive.map(str::to_owned),
        style: AskStyle::Terminal,
        align: crate::model::AskAlignment::Right,
        width: None,
        anchor: None,
    };
    assert_eq!(AskNavigation::new(&ask(Some("c"), None)).highlighted(), Some(2));
    assert_eq!(AskNavigation::new(&ask(None, Some("a"))).highlighted(), Some(1));
    let mut navigation = AskNavigation::new(&ask(None, None));
    assert_eq!(navigation.highlighted(), Some(0));
    navigation.move_backward();
    assert_eq!(navigation.activate(), Some(2));
    navigation.move_forward();
    assert_eq!(navigation.activate(), Some(0));
    assert_eq!(navigation.hotkey("Z"), Some(2));
    assert_eq!(navigation.hotkey("x"), None);
}
