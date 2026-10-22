//! `config.reload`, `browser.links`, `browser.clear` and `debug.appearance` (upstream's `ControlDispatcher` app and
//! browser arms, `ControlServer+AppCommands` `reloadGhosttyConfig`/`linkOpenMode`/`clearBrowser`, and the app's
//! `setDebugAppearance`): the reload reaching every session of every window through the frontend and answering its
//! count, the link mode setting, and the saved store's clear through a fake web-view host.

use std::cell::RefCell;
use std::rc::Rc;

use ngterm_host::fakes::FakeSurface;
use ngterm_proto::command::{BrowserCmd, ConfigCmd, DebugCmd, ThemeCmd};
use ngterm_proto::{Command, Request, Response};
use serde_json::json;
use uuid::Uuid;

use super::{Fixture, UNKNOWN, args, failure};
use crate::NotificationEffect;
use crate::dispatch::Dispatched;
use crate::html::{ClearProgress, HtmlHost, HtmlNavigation, StoreClear};
use crate::model::{AppStore, NewSession};

// MARK: - config.reload

/// Gives session `id` of `store` a fake main terminal and a stepped font size.
fn zoomed(store: &mut AppStore, id: Uuid, size: f64) {
    let session = store.session_mut(id).unwrap();
    session.surface.replace(Box::new(FakeSurface::new("L")));
    session.font_size = Some(size);
}

fn reload() -> Request {
    Request::new(Command::Config(ConfigCmd::Reload))
}

fn banners(fixture: &mut Fixture) -> Vec<String> {
    fixture
        .core
        .take_notifications()
        .into_iter()
        .filter_map(|effect| match effect {
            NotificationEffect::App { identity, title, body } => Some(format!("{identity}|{title}|{body}")),
            _ => None,
        })
        .collect()
}

#[test]
fn a_reload_clears_every_sessions_font_size_in_every_window_and_asks_the_frontend() {
    let mut fixture = Fixture::new(1);
    let (first, second) = (fixture.sessions[0], fixture.sessions[1]);
    zoomed(fixture.store_mut().unwrap(), first, 17.0);
    zoomed(fixture.store_mut().unwrap(), second, 9.0);
    let other = fixture.core.library_mut().new_window(None);
    let other_session = {
        let store = fixture.core.library_mut().store_mut(other).unwrap();
        let workspace = store.workspaces()[0].id();
        let id = store.add_session(workspace, NewSession::at_cwd("/tmp")).unwrap();
        zoomed(store, id, 21.0);
        id
    };

    let Dispatched::Pending { resume, .. } = fixture.core.begin(&reload()) else {
        panic!("config.reload waits for the frontend's reload");
    };

    let size = |fixture: &Fixture, window: Uuid, id: Uuid| {
        fixture
            .core
            .library()
            .store(window)
            .unwrap()
            .session(id)
            .unwrap()
            .font_size
    };
    for (window, id) in [
        (fixture.window, first),
        (fixture.window, second),
        (other, other_session),
    ] {
        assert_eq!(size(&fixture, window, id), None, "cleared before the frontend reloads");
    }
    assert!(
        fixture.core.take_config_reload(),
        "the frontend is asked to reload every terminal"
    );
    fixture.core.report_config_reload(Some(0));
    let response = resume(&mut fixture.core).finish_now(&mut fixture.core);
    assert_eq!(response, Response::with_result(json!({"count": 0})));
    assert_eq!(
        banners(&mut fixture),
        Vec::<String>::new(),
        "a clean reload posts nothing"
    );
}

#[test]
fn a_reload_answers_the_frontends_diagnostic_count_and_posts_the_banner() {
    let mut fixture = Fixture::new(0);
    let Dispatched::Pending { resume, .. } = fixture.core.begin(&reload()) else {
        panic!("config.reload waits for the frontend's reload");
    };
    assert!(fixture.core.take_config_reload());
    fixture.core.report_config_reload(Some(2));
    let response = resume(&mut fixture.core).finish_now(&mut fixture.core);
    assert_eq!(response, Response::with_result(json!({"count": 2})));
    assert_eq!(
        banners(&mut fixture),
        ["config-diagnostics|Config|2 issues in ghostty config — see ngterm.log, then Reload Config"]
    );

    let Dispatched::Pending { resume, .. } = fixture.core.begin(&reload()) else {
        panic!("config.reload waits for the frontend's reload");
    };
    fixture.core.report_config_reload(Some(1));
    assert_eq!(
        resume(&mut fixture.core).finish_now(&mut fixture.core),
        Response::with_result(json!({"count": 1}))
    );
    assert_eq!(
        banners(&mut fixture),
        ["config-diagnostics|Config|1 issue in ghostty config — see ngterm.log, then Reload Config"]
    );
}

#[test]
fn a_reload_no_frontend_ran_counts_nothing_and_a_config_that_cannot_be_built_counts_zero() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        fixture.core.dispatch(&reload()),
        Response::with_result(json!({"count": 0}))
    );
    let Dispatched::Pending { resume, .. } = fixture.core.begin(&reload()) else {
        panic!("config.reload waits for the frontend's reload");
    };
    fixture.core.report_config_reload(None);
    assert_eq!(
        resume(&mut fixture.core).finish_now(&mut fixture.core),
        Response::with_result(json!({"count": 0}))
    );
}

#[test]
fn a_settings_reload_with_diagnostics_posts_no_banner() {
    let mut fixture = Fixture::new(0);
    let response = fixture.send(Command::Theme(ThemeCmd::Set), None, args(|a| a.name = None));
    assert!(response.ok, "{response:?}");
    assert!(fixture.core.take_config_reload(), "a changed settings config reloads");
    fixture.core.report_config_reload(Some(3));
    assert_eq!(banners(&mut fixture), Vec::<String>::new());
}

// MARK: - browser.links

fn links(fixture: &mut Fixture, mode: Option<&str>) -> Response {
    fixture.send(
        Command::Browser(BrowserCmd::Links),
        None,
        args(|a| a.mode = mode.map(str::to_owned)),
    )
}

#[test]
fn links_sets_and_reads_the_link_open_mode() {
    let mut fixture = Fixture::new(0);
    let text = |mode: &str| Response::with_result(json!({"text": mode}));
    assert_eq!(links(&mut fixture, None), text("browser"));
    assert_eq!(links(&mut fixture, Some("overlay")), text("overlay"));
    assert_eq!(fixture.core.settings().link_open_mode.as_deref(), Some("overlay"));
    assert_eq!(links(&mut fixture, Some("overlay")), text("overlay"));
    assert_eq!(links(&mut fixture, None), text("overlay"));
    assert_eq!(links(&mut fixture, Some("browser")), text("browser"));
    assert_eq!(
        fixture.core.settings().link_open_mode,
        None,
        "the default is stored as no value"
    );
}

#[test]
fn links_refuses_an_unknown_mode_and_leaves_the_setting() {
    let mut fixture = Fixture::new(0);
    links(&mut fixture, Some("overlay"));
    for mode in ["bogus", "Overlay", ""] {
        assert_eq!(
            links(&mut fixture, Some(mode)),
            failure(&format!("invalid link mode: {mode}"))
        );
    }
    assert_eq!(fixture.core.settings().link_open_mode.as_deref(), Some("overlay"));
}

#[test]
fn the_browser_commands_take_no_target_or_window() {
    let mut fixture = Fixture::new(0);
    for cmd in [BrowserCmd::Links, BrowserCmd::Clear] {
        let name = Command::Browser(cmd).wire_name();
        let refusal = failure(&format!("{name} takes no target or --window"));
        for target in ["active", UNKNOWN] {
            let response = fixture.send(
                Command::Browser(cmd),
                Some(target),
                args(|a| a.mode = Some("overlay".into())),
            );
            assert_eq!(response, refusal);
        }
        let response = fixture.send(
            Command::Browser(cmd),
            None,
            args(|a| {
                a.window = Some(UNKNOWN.to_owned());
                a.mode = Some("overlay".into());
            }),
        );
        assert_eq!(response, refusal);
    }
    assert_eq!(fixture.core.settings().link_open_mode, None, "nothing was set");
}

// MARK: - browser.clear

/// The fake web views' saved store.
#[derive(Default)]
struct StoreLog {
    refusal: Option<String>,
    exists: bool,
    clearing: bool,
    failure: Option<String>,
    clears: usize,
}

struct ClearHost(Rc<RefCell<StoreLog>>);

impl HtmlHost for ClearHost {
    fn persistent_store_failure(&self) -> Option<String> {
        None
    }

    fn navigate(&self, _page: Uuid, _navigation: HtmlNavigation) -> Result<(), String> {
        Ok(())
    }

    fn clear_saved_store(&self) -> Result<StoreClear, String> {
        let mut log = self.0.borrow_mut();
        if let Some(refusal) = &log.refusal {
            return Err(refusal.clone());
        }
        if !log.exists {
            return Ok(StoreClear::Nothing);
        }
        log.clears += 1;
        log.clearing = true;
        Ok(StoreClear::Started)
    }

    fn saved_store_clear(&self) -> ClearProgress {
        let mut log = self.0.borrow_mut();
        if log.clearing {
            ClearProgress::Running
        } else {
            log.failure.take().map_or(ClearProgress::Done, ClearProgress::Failed)
        }
    }
}

fn clear_fixture() -> (Fixture, Rc<RefCell<StoreLog>>) {
    let mut fixture = Fixture::new(0);
    let log = Rc::new(RefCell::new(StoreLog::default()));
    fixture.core.set_html_host(Box::new(ClearHost(Rc::clone(&log))));
    (fixture, log)
}

fn clear() -> Request {
    Request::new(Command::Browser(BrowserCmd::Clear))
}

#[test]
fn a_clear_without_web_views_is_refused() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        fixture.core.dispatch(&clear()),
        failure("browser.clear: persistent browser storage is not available")
    );
}

#[test]
fn a_clear_without_a_saved_store_answers_at_once() {
    let (mut fixture, log) = clear_fixture();
    assert!(matches!(fixture.core.begin(&clear()), Dispatched::Done(response) if response == Response::success()));
    assert_eq!(log.borrow().clears, 0);
}

#[test]
fn a_clear_answers_once_the_store_is_empty() {
    let (mut fixture, log) = clear_fixture();
    log.borrow_mut().exists = true;
    let Dispatched::Pending { resume, .. } = fixture.core.begin(&clear()) else {
        panic!("a clear waits for the web engine");
    };
    let Dispatched::Pending { resume, .. } = resume(&mut fixture.core) else {
        panic!("still clearing");
    };
    log.borrow_mut().clearing = false;
    assert!(matches!(resume(&mut fixture.core), Dispatched::Done(response) if response == Response::success()));
    assert_eq!(log.borrow().clears, 1);
}

#[test]
fn a_clear_the_web_engine_fails_answers_its_error() {
    let (mut fixture, log) = clear_fixture();
    log.borrow_mut().exists = true;
    let Dispatched::Pending { resume, .. } = fixture.core.begin(&clear()) else {
        panic!("a clear waits for the web engine");
    };
    {
        let mut log = log.borrow_mut();
        log.clearing = false;
        log.failure = Some("Error removing website data".to_owned());
    }
    assert!(matches!(
        resume(&mut fixture.core),
        Dispatched::Done(response) if response == failure("browser.clear: Error removing website data")
    ));
    let Dispatched::Pending { resume, .. } = fixture.core.begin(&clear()) else {
        panic!("a second clear runs");
    };
    log.borrow_mut().clearing = false;
    assert!(
        matches!(resume(&mut fixture.core), Dispatched::Done(response) if response == Response::success()),
        "the failure was answered once"
    );
}

#[test]
fn a_refused_clear_names_the_command() {
    let (mut fixture, log) = clear_fixture();
    for refusal in [
        crate::html::errors::CLEARING.to_owned(),
        crate::html::errors::pages_open(1),
        "browser profile is not a UUID: /s/browser-profile".to_owned(),
    ] {
        log.borrow_mut().refusal = Some(refusal.clone());
        assert_eq!(
            fixture.core.dispatch(&clear()),
            failure(&format!("browser.clear: {refusal}"))
        );
    }
    assert_eq!(log.borrow().clears, 0, "a refused clear removes nothing");
}

// MARK: - debug.appearance

fn appearance(fixture: &mut Fixture, side: Option<&str>) -> Response {
    fixture.send(
        Command::Debug(DebugCmd::Appearance),
        None,
        args(|a| a.name = side.map(str::to_owned)),
    )
}

#[test]
fn debug_appearance_is_refused_outside_a_ui_test_launch() {
    let mut fixture = Fixture::new(0);
    for side in [None, Some("dark")] {
        assert_eq!(
            appearance(&mut fixture, side),
            failure("debug.appearance is a UI-test-only seam")
        );
    }
    assert_eq!(fixture.core.take_debug_appearance(), None);
}

#[test]
fn debug_appearance_forces_a_side_and_reads_the_applied_one() {
    let mut fixture = Fixture::new(0);
    fixture.core.set_ui_test_launch(true);
    let text = |side: &str| Response::with_result(json!({"text": side}));
    assert_eq!(appearance(&mut fixture, None), text("light"));
    fixture.core.report_applied_appearance(true);
    assert_eq!(appearance(&mut fixture, None), text("dark"));
    assert_eq!(appearance(&mut fixture, Some(" light ")), text("light"));
    assert_eq!(fixture.core.take_debug_appearance(), Some(false));
    assert_eq!(fixture.core.take_debug_appearance(), None, "taken once");
    for side in ["", "Dark", "auto"] {
        assert_eq!(
            appearance(&mut fixture, Some(side)),
            failure("debug.appearance requires light|dark")
        );
    }
    assert_eq!(fixture.core.take_debug_appearance(), None);
}

#[test]
fn a_ui_test_launch_needs_the_state_dir_and_the_sentinel() {
    use std::ffi::OsString;
    use std::path::PathBuf;

    struct Vars(&'static [&'static str]);
    impl crate::paths::Env for Vars {
        fn var(&self, key: &str) -> Option<OsString> {
            self.0.contains(&key).then(OsString::new)
        }
        fn passwd_home(&self) -> Option<PathBuf> {
            None
        }
    }
    assert!(crate::is_ui_test_launch(&Vars(&[
        "NGTERM_STATE_DIR",
        "NGTERM_UITEST_FORCE_SIDEBAR_VISIBLE"
    ])));
    assert!(!crate::is_ui_test_launch(&Vars(&["NGTERM_STATE_DIR"])));
    assert!(!crate::is_ui_test_launch(&Vars(&[
        "NGTERM_UITEST_FORCE_SIDEBAR_VISIBLE"
    ])));
}
