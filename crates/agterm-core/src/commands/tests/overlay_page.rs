//! `session.overlay.*` against an HTML page (upstream's `HtmlPageOutcomesTests`, `HtmlOverlayTests` store cases,
//! `ControlDispatcherOverlayTests` page arms and the control server's page arms): the slot rules a page shares with a
//! program and a HUD, the outcome bookkeeping by page id, the frontend's reports, and the bridge's entry.

use std::cell::RefCell;
use std::rc::Rc;

use agterm_host::fakes::FakeSurface;
use agterm_proto::command::{SessionCmd, ZmxCmd};
use agterm_proto::{Args, Command, Response};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Fixture, answered_id, args, failure, wire};
use crate::dashboard::DashboardCover;
use crate::html::{HtmlHost, HtmlLoadState, HtmlNavigation, HtmlPageInfo, HtmlReloadTarget, errors};
use crate::model::OverlayPane;

/// What a fake host was asked, and what it answers.
#[derive(Default)]
struct HostLog {
    navigations: Vec<(Uuid, HtmlNavigation)>,
    navigate_error: Option<String>,
    store_failure: Option<String>,
    unavailable: Option<String>,
}

/// The frontend's web views, faked: it records navigations and answers what the test set.
struct FakeHost(Rc<RefCell<HostLog>>);

impl HtmlHost for FakeHost {
    fn web_views_unavailable(&self) -> Option<String> {
        self.0.borrow().unavailable.clone()
    }

    fn persistent_store_failure(&self) -> Option<String> {
        self.0.borrow().store_failure.clone()
    }

    fn navigate(&self, page: Uuid, navigation: HtmlNavigation) -> Result<(), String> {
        let mut log = self.0.borrow_mut();
        log.navigations.push((page, navigation));
        log.navigate_error.clone().map_or(Ok(()), Err)
    }
}

/// A fixture whose core has web views, and their log.
fn fixture_with_host(extra: usize) -> (Fixture, Rc<RefCell<HostLog>>) {
    let mut fixture = Fixture::new(extra);
    let log = Rc::new(RefCell::new(HostLog::default()));
    fixture.core.set_html_host(Box::new(FakeHost(Rc::clone(&log))));
    (fixture, log)
}

fn overlay(fixture: &mut Fixture, cmd: SessionCmd, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    fixture.session(cmd, Some(&wire(id)), args(edit))
}

/// Opens a file page in the session-wide slot, or `pane`'s; its page id when it opened.
fn open_page(fixture: &mut Fixture, id: Uuid, edit: impl FnOnce(&mut Args)) -> Response {
    overlay(fixture, SessionCmd::OverlayOpen, id, |a| {
        a.html = Some("/tmp/site/page.html".to_owned());
        edit(a);
    })
}

fn page_id(response: &Response) -> Uuid {
    let id = response.result.as_ref().unwrap()["pageID"].as_str().unwrap();
    Uuid::parse_str(id).unwrap()
}

fn outcome(fixture: &mut Fixture, page: Uuid) -> Response {
    fixture.session(SessionCmd::OverlayResult, None, args(|a| a.page = Some(wire(page))))
}

fn outcome_state(fixture: &mut Fixture, page: Uuid) -> Option<String> {
    let response = outcome(fixture, page);
    Some(response.result?["pageOutcome"]["outcome"].as_str()?.to_owned())
}

fn node(fixture: &mut Fixture, id: Uuid) -> Value {
    let response = fixture.send(Command::Tree, None, Args::default());
    let tree = response.result.expect("a tree")["tree"].clone();
    tree["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| workspace["sessions"].as_array().cloned().unwrap_or_default())
        .find(|node| node["id"] == json!(wire(id)))
        .expect("the session's node")
}

fn kinds(node: &Value) -> Vec<String> {
    node["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|surface| surface["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Shows the split of session `id`.
fn split(fixture: &mut Fixture, id: Uuid) {
    let store = fixture.store_mut().unwrap();
    store.set_split_visibility(id, true, None);
    let session = store.session_mut(id).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("L")));
    session.split_surface.replace(Box::new(FakeSurface::new("R")));
}

#[test]
fn a_page_occupies_the_slot_reads_back_and_is_dismissed_by_its_close() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    let opened = open_page(&mut fixture, id, |_| {});
    assert_eq!(answered_id(&opened), Some(id));
    let page = page_id(&opened);
    let node = node(&mut fixture, id);
    assert_eq!(node["overlay"], json!(true));
    assert!(node.get("overlaySizePercent").is_none() && node.get("hud").is_none());
    assert!(!kinds(&node).contains(&"overlay".to_owned()), "a page has no terminal");
    assert_eq!(
        node["htmlOverlays"],
        json!([{
            "file": "/tmp/site/page.html",
            "state": "loading",
            "javascript": false,
            "chromeless": false,
            "persistent": false,
            "browse": false,
            "zoom": 1.0,
            "id": wire(page),
        }])
    );
    assert_eq!(
        outcome(&mut fixture, page).result,
        Some(json!({"pageOutcome": {"pageID": wire(page), "outcome": "pending"}}))
    );
    for (cmd, error) in [
        (
            SessionCmd::OverlayResult,
            "no overlay result: the slot holds an html page",
        ),
        (
            SessionCmd::OverlayText,
            "no overlay to read: the slot holds an html page",
        ),
        (
            SessionCmd::OverlayCopy,
            "no overlay to read: the slot holds an html page",
        ),
    ] {
        assert_eq!(overlay(&mut fixture, cmd, id, |_| {}), failure(error));
    }
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    let node = self::node(&mut fixture, id);
    assert_eq!(node["overlay"], json!(false));
    assert!(node.get("htmlOverlays").is_none());
    assert_eq!(
        outcome(&mut fixture, page).result,
        Some(json!({"pageOutcome": {"pageID": wire(page), "outcome": "dismissed"}}))
    );
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.take_released_pages(), vec![page], "the web view goes");
    assert!(fixture.core.take_released_pages().is_empty());
}

/// Upstream's `submitRecordsTheValueAndClosesThePage`, `anEmptyOrMultilineValueIsARealAnswer`.
#[test]
fn submit_answers_the_page_once_and_closes_it() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    for value in ["feature-x", "", "a\nb"] {
        let page = page_id(&open_page(&mut fixture, id, |_| {}));
        assert_eq!(
            answered_id(&overlay(&mut fixture, SessionCmd::OverlaySubmit, id, |a| a.value =
                Some(value.to_owned()))),
            Some(id)
        );
        assert_eq!(node(&mut fixture, id)["overlay"], json!(false));
        let _ = fixture.core.commit();
        assert_eq!(
            outcome(&mut fixture, page).result,
            Some(json!({"pageOutcome": {"pageID": wire(page), "outcome": "submitted", "value": value}})),
            "the close never turns an answer into a dismissal"
        );
        assert_eq!(fixture.core.take_released_pages(), vec![page]);
    }
}

#[test]
fn submit_and_the_page_commands_need_a_page() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    let value = |a: &mut Args| a.value = Some("x".to_owned());
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlaySubmit, id, value),
        failure("no overlay")
    );
    assert!(
        overlay(&mut fixture, SessionCmd::OverlayOpen, id, |a| a.command =
            Some("exec cat".into()))
        .ok
    );
    for (cmd, edit) in [
        (SessionCmd::OverlaySubmit, Box::new(value) as Box<dyn FnOnce(&mut Args)>),
        (SessionCmd::OverlayReload, Box::new(|_: &mut Args| {})),
        (
            SessionCmd::OverlayNavigate,
            Box::new(|a: &mut Args| a.to = Some("back".into())),
        ),
    ] {
        assert_eq!(
            overlay(&mut fixture, cmd, id, edit),
            failure("the overlay is not an html page")
        );
    }
    assert!(overlay(&mut fixture, SessionCmd::OverlayClose, id, |_| {}).ok);
    let page = page_id(&open_page(&mut fixture, id, |_| {}));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlaySubmit, id, |_| {}),
        failure("session.overlay.submit requires a value")
    );
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("pending"));
    assert_eq!(
        fixture.session(SessionCmd::OverlaySubmit, Some("zzz"), args(value)),
        failure("no such session: zzz")
    );
}

#[test]
fn reload_moves_the_revision_and_navigate_reaches_the_web_view() {
    let (mut fixture, log) = fixture_with_host(1);
    let id = fixture.sessions[0];
    let page = page_id(&open_page(&mut fixture, id, |_| {}));
    let info = HtmlPageInfo {
        page: "/tmp/site/other.html".to_owned(),
        title: Some("Other".to_owned()),
        can_go_back: true,
        can_go_forward: false,
    };
    assert!(fixture.core.set_html_load_state(page, HtmlLoadState::Loaded, None));
    assert!(fixture.core.set_html_page(page, Some(info.clone())));
    let read = |fixture: &Fixture| fixture.store().session(id).unwrap().overlay.html.clone().unwrap();
    assert!(overlay(&mut fixture, SessionCmd::OverlayReload, id, |a| a.current = Some(true)).ok);
    let reloaded = read(&fixture);
    assert_eq!(
        (reloaded.reload_revision, reloaded.reload_target, reloaded.load_state),
        (1, HtmlReloadTarget::Current, HtmlLoadState::Loading)
    );
    assert_eq!(reloaded.current, Some(info), "a reload of the current page keeps it");
    assert!(overlay(&mut fixture, SessionCmd::OverlayReload, id, |_| {}).ok);
    let reloaded = read(&fixture);
    assert_eq!(
        (reloaded.reload_revision, reloaded.reload_target),
        (2, HtmlReloadTarget::Original)
    );
    assert_eq!(reloaded.current, None, "the original forgets where the user went");

    assert!(
        overlay(&mut fixture, SessionCmd::OverlayNavigate, id, |a| a.to =
            Some("forward".into()))
        .ok
    );
    assert_eq!(log.borrow().navigations, vec![(page, HtmlNavigation::Forward)]);
    log.borrow_mut().navigate_error = Some(errors::no_history(HtmlNavigation::Back));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayNavigate, id, |a| a.to =
            Some("back".into())),
        failure("no page to go back to")
    );
}

#[test]
fn a_page_shares_the_slot_with_a_program_and_a_hud() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    assert_eq!(
        open_page(&mut fixture, id, |a| a.size_percent = Some(150)),
        failure("session.overlay.open: --size-percent must be 1...100")
    );
    let _helper = with_hud_helper(&mut fixture);
    let page = page_id(&open_page(&mut fixture, id, |a| a.size_percent = Some(40)));
    assert_eq!(node(&mut fixture, id)["overlaySizePercent"], json!(40));
    assert_eq!(open_page(&mut fixture, id, |_| {}), failure("overlay already open"));
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayOpen, id, |a| a.command =
            Some("top".into())),
        failure("overlay already open")
    );
    assert_eq!(
        overlay(&mut fixture, SessionCmd::HudOpen, id, |a| a.message = Some("hi".into())),
        failure("overlay already open")
    );
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("pending"));
    assert!(overlay(&mut fixture, SessionCmd::OverlayResize, id, |a| a.full = Some(true)).ok);
    assert!(node(&mut fixture, id).get("overlaySizePercent").is_none());
}

#[test]
fn a_refused_open_registers_nothing() {
    let (mut fixture, log) = fixture_with_host(1);
    let id = fixture.sessions[0];
    let _ = page_id(&open_page(&mut fixture, id, |_| {}));
    assert_eq!(open_page(&mut fixture, id, |_| {}), failure("overlay already open"));
    let _ = fixture.core.commit();
    assert!(fixture.core.take_released_pages().is_empty());
    log.borrow_mut().store_failure = Some(errors::PERSISTENT_UNAVAILABLE.to_owned());
    let other = fixture.sessions[1];
    let refused = overlay(&mut fixture, SessionCmd::OverlayOpen, other, |a| {
        a.url = Some("https://example.com/".into());
        a.persistent = Some(true);
    });
    assert_eq!(
        refused,
        failure("session.overlay.open: persistent browser storage is not available")
    );
    assert_eq!(node(&mut fixture, other)["overlay"], json!(false));
}

#[test]
fn a_host_whose_web_views_cannot_run_refuses_every_page_and_registers_nothing() {
    let (mut fixture, log) = fixture_with_host(1);
    let id = fixture.sessions[0];
    log.borrow_mut().unavailable = Some("the web engine's sandbox cannot start (bwrap: no permission)".to_owned());
    assert_eq!(
        open_page(&mut fixture, id, |_| {}),
        failure(
            "session.overlay.open: html overlays are not available here: the web engine's sandbox cannot start \
             (bwrap: no permission)"
        )
    );
    assert_eq!(
        open_page(&mut fixture, id, |a| a.html = Some("rel.html".into())),
        failure("session.overlay.open: html file must be an absolute path"),
        "validation still answers first"
    );
    assert_eq!(node(&mut fixture, id)["overlay"], json!(false));
    let _ = fixture.core.commit();
    assert!(fixture.core.take_released_pages().is_empty());
    log.borrow_mut().unavailable = None;
    let _ = page_id(&open_page(&mut fixture, id, |_| {}));
}

#[test]
fn a_build_without_web_views_refuses_a_valid_page() {
    let mut fixture = Fixture::new(1);
    let id = fixture.sessions[0];
    assert_eq!(
        open_page(&mut fixture, id, |_| {}),
        failure("session.overlay.open: html overlays are not available in this build")
    );
    assert_eq!(
        open_page(&mut fixture, id, |a| a.html = Some("rel.html".into())),
        failure("session.overlay.open: html file must be an absolute path"),
        "validation still answers first"
    );
}

/// Upstream's `aPaneSlotPageSubmitsAndDismissesLikeTheSessionSlot`.
#[test]
fn a_pane_page_covers_its_pane_alone() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    assert_eq!(
        open_page(&mut fixture, id, |a| a.pane = Some("right".into())),
        failure("pane not visible")
    );
    split(&mut fixture, id);
    let page = page_id(&open_page(&mut fixture, id, |a| a.pane = Some("right".into())));
    let node = node(&mut fixture, id);
    assert_eq!(node["paneOverlays"], json!(["right"]));
    assert_eq!(node["overlay"], json!(false));
    assert_eq!(node["htmlOverlays"][0]["pane"], json!("right"));
    assert!(
        !kinds(&node).contains(&"overlay-right".to_owned()),
        "a page is no zoom surface"
    );
    assert_eq!(
        open_page(&mut fixture, id, |a| a.pane = Some("right".into())),
        failure("pane overlay already open")
    );
    assert_eq!(
        overlay(&mut fixture, SessionCmd::OverlayResult, id, |a| a.pane =
            Some("right".into())),
        failure("no overlay result: the slot holds an html page")
    );
    assert!(
        overlay(&mut fixture, SessionCmd::OverlaySubmit, id, |a| {
            a.pane = Some("right".into());
            a.value = Some("r".into());
        })
        .ok
    );
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("submitted"));
    let dismissed = page_id(&open_page(&mut fixture, id, |a| a.pane = Some("right".into())));
    assert!(
        overlay(&mut fixture, SessionCmd::OverlayClose, id, |a| a.pane =
            Some("right".into()))
        .ok
    );
    assert_eq!(outcome_state(&mut fixture, dismissed).as_deref(), Some("dismissed"));
}

#[test]
fn a_pane_page_follows_a_swap_and_its_reports_find_it() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    let page = page_id(&open_page(&mut fixture, id, |a| a.pane = Some("left".into())));
    let slot = fixture.core.html_overlay_slot(page).unwrap();
    assert_eq!((slot.session, slot.pane), (id, Some(OverlayPane::Left)));
    assert!(
        fixture
            .send(Command::Session(SessionCmd::Swap), Some(&wire(id)), Args::default())
            .ok
    );
    assert_eq!(
        fixture.core.html_overlay_slot(page).unwrap().pane,
        Some(OverlayPane::Right)
    );
    fixture.store_mut().unwrap().set_split_focus(id, false);
    assert!(fixture.core.html_page_focused(page));
    assert!(
        fixture.store().session(id).unwrap().split.focused,
        "focus follows the page's pane"
    );
    assert!(
        fixture
            .core
            .set_html_load_state(page, HtmlLoadState::Failed, Some("boom".into()))
    );
    let node = node(&mut fixture, id);
    assert_eq!(node["htmlOverlays"][0]["state"], json!("failed"));
    assert_eq!(node["htmlOverlays"][0]["error"], json!("boom"));
    assert!(
        fixture.core.close_html_overlay(page),
        "the strip's close finds it after the swap"
    );
    assert!(
        !fixture.core.set_html_load_state(page, HtmlLoadState::Loaded, None),
        "gone"
    );
}

#[test]
fn a_page_replaces_a_hud() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    let _helper = with_hud_helper(&mut fixture);
    assert!(overlay(&mut fixture, SessionCmd::HudOpen, id, |a| a.message = Some("hi".into())).ok);
    let page = page_id(&open_page(&mut fixture, id, |_| {}));
    let node = node(&mut fixture, id);
    assert!(node.get("hud").is_none());
    assert_eq!(node["htmlOverlays"][0]["id"], json!(wire(page)));
}

#[test]
fn a_page_in_an_undoable_close_stays_pending_until_the_close_is_final() {
    let (mut fixture, _) = fixture_with_host(2);
    let id = fixture.sessions[0];
    let page = page_id(&open_page(&mut fixture, id, |_| {}));
    assert!(fixture.store_mut().unwrap().soft_close_session(id));
    let _ = fixture.core.commit();
    assert!(fixture.core.take_released_pages().is_empty(), "the web view goes on");
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("pending"));
    assert!(
        fixture.core.set_html_load_state(page, HtmlLoadState::Loaded, None),
        "reports still land"
    );
    assert!(
        fixture.core.html_overlay_slot(page).is_none(),
        "but no visible slot holds it"
    );
    assert!(!fixture.core.close_html_overlay(page));
    assert!(fixture.store_mut().unwrap().undo_pending_close(None));
    assert_eq!(node(&mut fixture, id)["htmlOverlays"][0]["state"], json!("loaded"));
    assert!(fixture.store_mut().unwrap().soft_close_session(id));
    assert!(fixture.store_mut().unwrap().finalize_pending_close_of_session(id));
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.take_released_pages(), vec![page]);
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("dismissed"));
}

#[test]
fn a_closed_window_releases_its_pages() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    let page = page_id(&open_page(&mut fixture, id, |_| {}));
    let window = fixture.window;
    fixture.core.library_mut().close_window(window);
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("dismissed"));
    let _ = fixture.core.commit();
    assert_eq!(fixture.core.take_released_pages(), vec![page]);
}

#[test]
fn a_page_bridge_request_acts_from_where_the_page_sits() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    let page = page_id(&open_page(&mut fixture, id, |a| a.pane = Some("right".into())));
    let request = fixture
        .core
        .html_bridge_request(page, br#"{"cmd":"session.overlay.submit","args":{"value":"v"}}"#)
        .unwrap();
    assert_eq!(request.target, Some(wire(id)));
    assert_eq!(
        request.args.as_ref().and_then(|a| a.pane.clone()).as_deref(),
        Some("right")
    );
    let response = fixture.core.dispatch(&request);
    assert!(response.ok, "{response:?}");
    assert_eq!(outcome_state(&mut fixture, page).as_deref(), Some("submitted"));
    assert_eq!(
        fixture.core.html_bridge_request(page, br#"{"cmd":"tree"}"#).map(|_| ()),
        Err("page closed".to_owned())
    );
    let other = page_id(&open_page(&mut fixture, id, |_| {}));
    let streaming = format!(r#"{{"cmd":"{}"}}"#, Command::Zmx(ZmxCmd::Present));
    assert_eq!(
        fixture
            .core
            .html_bridge_request(other, streaming.as_bytes())
            .map(|_| ()),
        Err("zmx.present cannot be sent from a page".to_owned())
    );
}

#[test]
fn a_clicked_link_opens_a_browsing_page_unless_a_hud_or_a_zoom_is_up() {
    let (mut fixture, _) = fixture_with_host(2);
    let id = fixture.sessions[0];
    let url = url::Url::parse("https://example.com/docs").unwrap();
    let zoomed = fixture.sessions[2];
    let window = fixture.window;
    fixture.core.set_zoom_target(
        window,
        Some(crate::zoom::SurfaceId {
            session: zoomed,
            surface: crate::zoom::ZoomSurface::Primary,
        }),
    );
    assert!(
        !fixture.core.open_link_overlay(url.clone(), zoomed),
        "a zoom refuses it"
    );
    assert!(node(&mut fixture, zoomed).get("htmlOverlays").is_none());
    fixture.core.set_zoom_target(window, None);
    assert!(fixture.core.open_link_overlay(url.clone(), id));
    let page = node(&mut fixture, id)["htmlOverlays"][0].clone();
    assert_eq!(page["url"], json!("https://example.com/docs"));
    assert_eq!(
        (
            &page["navigation"],
            &page["javascript"],
            &page["persistent"],
            &page["browse"]
        ),
        (&json!(true), &json!(true), &json!(true), &json!(true))
    );
    assert!(!fixture.core.open_link_overlay(url.clone(), id), "the slot is taken");
    let other = fixture.sessions[1];
    let _helper = with_hud_helper(&mut fixture);
    assert!(
        overlay(&mut fixture, SessionCmd::HudOpen, other, |a| a.message =
            Some("hi".into()))
        .ok
    );
    assert!(!fixture.core.open_link_overlay(url, other), "a hud refuses it");
    assert!(node(&mut fixture, other).get("hud").is_some());
}

#[test]
fn the_dashboard_names_a_covering_page() {
    let (mut fixture, _) = fixture_with_host(1);
    let id = fixture.sessions[0];
    split(&mut fixture, id);
    let page = page_id(&open_page(&mut fixture, id, |a| a.pane = Some("right".into())));
    assert!(fixture.core.set_html_page(
        page,
        Some(HtmlPageInfo {
            page: "/tmp/site/page.html".into(),
            title: Some("Report".into()),
            can_go_back: false,
            can_go_forward: false,
        })
    ));
    let cover = fixture.store().session(id).unwrap().dashboard_cover(OverlayPane::Right);
    assert_eq!(
        cover,
        Some(DashboardCover::Page {
            identity: "page.html".into(),
            title: Some("Report".into())
        })
    );
    assert_eq!(
        fixture.store().session(id).unwrap().dashboard_cover(OverlayPane::Left),
        None
    );
}

/// A HUD helper the HUD commands can launch.
fn with_hud_helper(fixture: &mut Fixture) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("hud.sh");
    std::fs::write(&helper, "#!/bin/sh\n").unwrap();
    fixture.core.set_hud_helper(helper, dir.path().join("bodies"));
    dir
}

#[test]
fn a_font_command_over_a_page_steps_the_page_zoom_and_leaves_the_terminal() {
    use agterm_proto::command::FontCmd;
    let (mut fixture, _) = fixture_with_host(0);
    let id = fixture.sessions[0];
    let surface = FakeSurface::new("L");
    let probe = surface.probe();
    fixture
        .store_mut()
        .unwrap()
        .session_mut(id)
        .unwrap()
        .surface
        .replace(Box::new(surface));
    assert!(open_page(&mut fixture, id, |_| {}).ok);
    let target = wire(id);
    let font = |fixture: &mut Fixture, cmd: FontCmd| fixture.send(Command::Font(cmd), Some(&target), Args::default());
    assert_eq!(answered_id(&font(&mut fixture, FontCmd::Inc)), Some(id));
    assert!(fixture.core.html_zoom() > 1.0);
    assert!(fixture.core.settings().html_overlay_zoom.is_some());
    assert_eq!(answered_id(&font(&mut fixture, FontCmd::Reset)), Some(id));
    assert_eq!(fixture.core.settings().html_overlay_zoom, None);
    assert!(probe.actions().is_empty(), "the terminal under the page keeps its size");
    // a missing pane keeps its error even under a page
    assert_eq!(
        fixture.send(
            Command::Font(FontCmd::Inc),
            Some(&target),
            args(|a| a.pane = Some("right".to_owned()))
        ),
        failure("session has no split pane")
    );
}
