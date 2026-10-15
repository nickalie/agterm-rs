//! The appearance rules (the GTK build's `theme.rs` tests, upstream's `appearanceChanged`), the portal's values, the
//! debounce on the main loop, and the reloads the windows run.

use std::sync::Mutex;

use agterm_core::model::WindowLibrary;
use agterm_core::{Core, CoreConfig, CoreState};
use agterm_host::{MainLoop, MainLoopClosed, MainTask};
use agterm_proto::command::{ConfigCmd, DebugCmd};
use agterm_proto::{AppIdentity, Args, Command, Request};

use super::*;

fn following(theme: Option<&str>, dark: Option<&str>, follow: Option<bool>) -> AppSettings {
    AppSettings {
        theme: theme.map(str::to_owned),
        dark_theme: dark.map(str::to_owned),
        follow_system_appearance: follow,
        ..AppSettings::default()
    }
}

fn both() -> AppSettings {
    following(Some("Paper"), Some("Ink"), Some(true))
}

#[test]
fn the_portals_values_and_their_wrapping() {
    assert_eq!(ColorScheme::from_value(0), ColorScheme::NoPreference);
    assert_eq!(ColorScheme::from_value(1), ColorScheme::PreferDark);
    assert_eq!(ColorScheme::from_value(2), ColorScheme::PreferLight);
    assert_eq!(
        ColorScheme::from_value(7),
        ColorScheme::NoPreference,
        "an undefined value"
    );
    assert!(ColorScheme::PreferDark.is_dark());
    assert!(!ColorScheme::PreferLight.is_dark());
    assert!(
        !ColorScheme::NoPreference.is_dark(),
        "no preference is light, as libadwaita reads it"
    );
    assert_eq!(ColorScheme::from_variant(&Value::U32(1)), Some(ColorScheme::PreferDark));
    // the deprecated `Read` wraps the value in one more variant
    let wrapped = Value::Value(Box::new(Value::Value(Box::new(Value::U32(2)))));
    assert_eq!(ColorScheme::from_variant(&wrapped), Some(ColorScheme::PreferLight));
    assert_eq!(ColorScheme::from_variant(&Value::from("dark")), None);
}

#[test]
fn a_flip_reloads_only_while_following_both_slots_and_on_a_new_side() {
    let light = Appearance::new(false);
    assert!(light.flip_reloads(&both(), true));
    assert!(!light.flip_reloads(&both(), false), "the side did not move");
    assert!(Appearance::new(true).flip_reloads(&both(), false));
    for settings in [
        following(Some("Paper"), Some("Ink"), None),
        following(Some("Paper"), None, Some(true)),
        following(None, Some("Ink"), Some(true)),
    ] {
        assert!(!light.flip_reloads(&settings, true), "{settings:?}");
    }
}

#[test]
fn the_desktop_moves_the_side_and_reloads_once_per_flip() {
    let mut appearance = Appearance::new(false);
    assert!(!appearance.side());
    assert!(appearance.desktop_changed(&both(), true));
    assert!(appearance.side());
    assert!(appearance.take_reload());
    assert!(!appearance.take_reload(), "taken once");
    appearance.applied(true);
    assert!(appearance.last_applied_dark());
    assert!(!appearance.desktop_changed(&both(), true), "the side applied already");
    // not following: the side still moves for the next reload, without one of its own
    assert!(!appearance.desktop_changed(&AppSettings::default(), false));
    assert!(!appearance.side());
    assert!(!appearance.take_reload());
}

#[test]
fn a_forced_side_reloads_once_and_holds_against_the_desktop() {
    let mut appearance = Appearance::new(false);
    appearance.applied(true);
    // forcing the side on screen asks for nothing; forcing the other one reloads
    assert!(!appearance.force(&both(), true), "dark applied already");
    assert!(appearance.force(&both(), false));
    assert!(appearance.take_reload());
    // a forced side holds against the desktop
    assert!(!appearance.desktop_changed(&both(), true));
    assert!(!appearance.side(), "forced light");
    assert!(!appearance.take_reload());
}

fn core(settings: AppSettings) -> Core {
    Core::new(
        CoreConfig {
            app: AppIdentity::from_recorded("1.2.3", None),
            capabilities: Vec::new(),
        },
        CoreState {
            library: WindowLibrary::seeded("/home/u"),
            settings,
            persistence: None,
        },
    )
}

fn debug_appearance(state: &mut AppState, side: Option<&str>) -> String {
    let request = Request {
        cmd: Command::Debug(DebugCmd::Appearance),
        target: None,
        args: Some(Args {
            name: side.map(str::to_owned),
            ..Args::default()
        }),
    };
    let reply = state.handle_request(&request).settled();
    assert!(reply.response.ok, "{:?}", reply.response);
    serde_json::to_value(&reply.response.result)
        .ok()
        .and_then(|value| value.get("text").and_then(|text| text.as_str().map(str::to_owned)))
        .unwrap_or_default()
}

#[test]
fn a_flip_asks_the_windows_for_a_reload_on_the_new_side() {
    let mut state = AppState::new(core(both()));
    state.launched_on(false);
    assert_eq!(state.take_config_reload(), None);
    state.appearance_changed(true);
    assert_eq!(
        state.take_config_reload(),
        Some(ConfigReload {
            dark: true,
            asked: false
        })
    );
    state.config_applied(true);
    assert!(state.applied_dark());
    state.appearance_changed(true);
    assert_eq!(state.take_config_reload(), None, "the side applied already");
}

#[test]
fn a_command_reload_loads_the_desktops_side_even_without_following() {
    let mut state = AppState::new(core(AppSettings::default()));
    state.launched_on(true);
    state.appearance_changed(false);
    assert_eq!(state.take_config_reload(), None, "not following: no flip");
    // `config.reload`: its answer waits for the windows' report
    let _pending = state.handle_request(&Request::new(Command::Config(ConfigCmd::Reload)));
    assert_eq!(
        state.take_config_reload(),
        Some(ConfigReload {
            dark: false,
            asked: true
        }),
        "the desktop's side now"
    );
}

#[test]
fn debug_appearance_forces_the_side_and_reads_the_applied_one() {
    let mut state = AppState::new(core(both()));
    state.core.set_ui_test_launch(true);
    state.launched_on(false);
    assert_eq!(debug_appearance(&mut state, None), "light");
    assert_eq!(debug_appearance(&mut state, Some("dark")), "dark");
    let reload = state.take_config_reload().expect("the forced flip reloads at once");
    assert!(reload.dark && !reload.asked);
    state.config_applied(reload.dark);
    assert_eq!(debug_appearance(&mut state, None), "dark");
    // the desktop no longer moves a forced app
    state.appearance_changed(false);
    assert_eq!(state.take_config_reload(), None);
}

/// A loop that keeps what is posted, for the test to run.
struct Recording {
    posted: Mutex<Vec<(Option<Duration>, MainTask<AppState>)>>,
}

impl MainLoop<AppState> for Recording {
    fn post(&self, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.posted.lock().unwrap().push((None, task));
        Ok(())
    }

    fn post_after(&self, delay: Duration, task: MainTask<AppState>) -> Result<(), MainLoopClosed> {
        self.posted.lock().unwrap().push((Some(delay), task));
        Ok(())
    }
}

#[test]
fn a_burst_of_reports_lands_once_with_the_last_side() {
    let recording = Arc::new(Recording {
        posted: Mutex::new(Vec::new()),
    });
    let held: Arc<dyn MainLoop<AppState>> = Arc::clone(&recording) as _;
    let poster: Poster = Arc::downgrade(&held);
    let latest = Arc::new(AtomicU64::new(0));
    debounce(&poster, &latest, true);
    debounce(&poster, &latest, false);
    debounce(&poster, &latest, true);
    let tasks = std::mem::take(&mut *recording.posted.lock().unwrap());
    assert_eq!(tasks.len(), 3);
    assert!(tasks.iter().all(|(delay, _)| *delay == Some(DEBOUNCE)));
    let mut state = AppState::new(core(both()));
    state.launched_on(false);
    let mut reloads = 0;
    for (_, task) in tasks {
        task(&mut state);
        reloads += usize::from(state.take_config_reload().is_some());
    }
    assert_eq!(reloads, 1, "only the last report lands");
    assert!(state.appearance.side());
}
