//! `theme.set` and `theme.list` over a themes directory (upstream's `ControlServer+AppCommands.setTheme`/`listThemes`
//! and `SettingsModel`'s slot setters): the light/dark slots by sync, the refusals that leave the slots alone, and the
//! config rewrite that asks for a zoom-clearing reload.

use agterm_proto::command::ThemeCmd;
use agterm_proto::{Args, Command, Response};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{Fixture, args, failure};
use crate::persist::AppSettings;

const LIGHT: &str = "Dracula";
/// A refusal case: the args it sends and the error it gets.
type Refusal = (Box<dyn FnOnce(&mut Args)>, String);
const DARK: &str = "Nord";

/// A fixture whose core lists the themes of a temporary directory and writes its settings config beside it.
struct Themes {
    fixture: Fixture,
    dir: TempDir,
}

impl Themes {
    fn new(settings: AppSettings) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let themes = dir.path().join("themes");
        std::fs::create_dir_all(&themes).unwrap();
        for name in [DARK, LIGHT, "Builtin Light", "agterm", ".agterm-resource-manifest"] {
            std::fs::write(themes.join(name), "").unwrap();
        }
        let mut fixture = Fixture::with_settings(0, settings);
        fixture
            .core
            .set_theme_paths(Some(themes), Some(dir.path().join("ghostty-settings.conf")));
        Self { fixture, dir }
    }

    fn set(&mut self, edit: impl FnOnce(&mut Args)) -> Response {
        self.fixture.send(Command::Theme(ThemeCmd::Set), None, args(edit))
    }

    fn list(&mut self) -> Value {
        let response = self.fixture.send(Command::Theme(ThemeCmd::List), None, Args::default());
        assert!(response.ok, "{response:?}");
        response.result.unwrap_or_default()
    }

    /// `theme.set` with `edit`, checked against `expected` and against its `theme.list` read-back.
    fn set_ok(&mut self, edit: impl FnOnce(&mut Args), expected: &Value) {
        let response = self.set(edit);
        assert!(response.ok, "{response:?}");
        assert_eq!(response.result.as_ref(), Some(expected));
        let mut listed = self.list();
        listed.as_object_mut().unwrap().remove("themes");
        assert_eq!(&listed, expected);
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("ghostty-settings.conf")).unwrap_or_default()
    }
}

fn name(value: &str) -> impl FnOnce(&mut Args) + use<> {
    let value = value.to_owned();
    move |a: &mut Args| a.name = Some(value)
}

#[test]
fn the_list_names_the_bundled_themes_sorted_without_hidden_files() {
    let mut themes = Themes::new(AppSettings::default());
    assert_eq!(
        themes.list(),
        json!({"themes": ["agterm", "Builtin Light", "Dracula", "Nord"], "sync": false})
    );
    themes.set_ok(name(LIGHT), &json!({"theme": LIGHT, "sync": false}));
    assert_eq!(themes.list()["theme"], json!(LIGHT));
}

#[test]
fn a_single_theme_is_set_by_name_trimmed_or_by_light() {
    let mut themes = Themes::new(AppSettings::default());
    themes.set_ok(name(LIGHT), &json!({"theme": LIGHT, "sync": false}));
    themes.set_ok(name(&format!("  {DARK}  ")), &json!({"theme": DARK, "sync": false}));
    themes.set_ok(
        |a| a.light = Some(LIGHT.to_owned()),
        &json!({"theme": LIGHT, "sync": false}),
    );
    assert_eq!(themes.fixture.core.settings().theme.as_deref(), Some(LIGHT));
}

#[test]
fn the_dark_slot_turns_sync_on_and_none_turns_it_off() {
    let mut themes = Themes::new(AppSettings::default());
    themes.set_ok(name(LIGHT), &json!({"theme": LIGHT, "sync": false}));
    themes.set_ok(
        |a| a.dark = Some(DARK.to_owned()),
        &json!({"sync": true, "light": LIGHT, "dark": DARK}),
    );
    themes.set_ok(
        |a| {
            a.light = Some(DARK.to_owned());
            a.dark = Some(LIGHT.to_owned());
        },
        &json!({"sync": true, "light": DARK, "dark": LIGHT}),
    );
    themes.set_ok(
        |a| a.dark = Some("NONE".to_owned()),
        &json!({"theme": DARK, "sync": false}),
    );
    themes.set_ok(
        |a| {
            a.name = Some(LIGHT.to_owned());
            a.dark = Some(DARK.to_owned());
        },
        &json!({"sync": true, "light": LIGHT, "dark": DARK}),
    );
    themes.set_ok(
        |a| {
            a.name = Some(DARK.to_owned());
            a.dark = Some("none".to_owned());
        },
        &json!({"theme": DARK, "sync": false}),
    );
}

#[test]
fn a_dark_theme_without_a_theme_seeds_the_builtin_light_side() {
    let mut themes = Themes::new(AppSettings::default());
    themes.set_ok(
        |a| a.dark = Some(DARK.to_owned()),
        &json!({"sync": true, "light": "Builtin Light", "dark": DARK}),
    );
}

#[test]
fn a_bare_set_returns_to_ghosttys_builtin_colors() {
    let mut themes = Themes::new(AppSettings::default());
    themes.set_ok(
        |a| {
            a.light = Some(LIGHT.to_owned());
            a.dark = Some(DARK.to_owned());
        },
        &json!({"sync": true, "light": LIGHT, "dark": DARK}),
    );
    themes.set_ok(|_| {}, &json!({"sync": false}));
    themes.set_ok(name("   "), &json!({"sync": false}));
    let settings = themes.fixture.core.settings();
    assert_eq!((settings.theme.as_ref(), settings.dark_theme.as_ref()), (None, None));
    assert_eq!(settings.follow_system_appearance, None);
}

#[test]
fn refusals_leave_the_slots_alone() {
    let mut themes = Themes::new(AppSettings::default());
    themes.set_ok(name(DARK), &json!({"theme": DARK, "sync": false}));
    let cases: [Refusal; 4] = [
        (
            Box::new(|a| {
                a.name = Some(LIGHT.to_owned());
                a.light = Some(DARK.to_owned());
            }),
            "theme.set takes either a name or --light, not both".to_owned(),
        ),
        (
            Box::new(name("No Such Theme")),
            "unknown theme: No Such Theme".to_owned(),
        ),
        (
            Box::new(|a| {
                a.light = Some(LIGHT.to_owned());
                a.dark = Some("No Such Theme".to_owned());
            }),
            "unknown theme: No Such Theme".to_owned(),
        ),
        (
            Box::new(name(&LIGHT.to_lowercase())),
            format!("unknown theme: {}", LIGHT.to_lowercase()),
        ),
    ];
    for (edit, error) in cases {
        assert_eq!(themes.set(edit), failure(&error));
        let mut listed = themes.list();
        listed.as_object_mut().unwrap().remove("themes");
        assert_eq!(listed, json!({"theme": DARK, "sync": false}));
    }
}

#[test]
fn without_a_themes_directory_every_name_is_unknown() {
    let mut fixture = Fixture::new(0);
    assert_eq!(
        fixture.send(Command::Theme(ThemeCmd::Set), None, args(name(LIGHT))),
        failure("unknown theme: Dracula")
    );
    let listed = fixture.send(Command::Theme(ThemeCmd::List), None, Args::default());
    assert_eq!(listed.result, Some(json!({"themes": [], "sync": false})));
}

#[test]
fn a_changed_config_is_rewritten_clears_the_font_zoom_and_asks_for_a_reload() {
    let mut themes = Themes::new(AppSettings::default());
    let session = themes.fixture.sessions[0];
    themes.fixture.store_mut().unwrap().set_font_size(session, 18.0);
    themes.set_ok(
        |a| {
            a.light = Some(LIGHT.to_owned());
            a.dark = Some(DARK.to_owned());
        },
        &json!({"sync": true, "light": LIGHT, "dark": DARK}),
    );
    assert!(
        themes.config().contains("theme = light:Dracula,dark:Nord\n"),
        "{}",
        themes.config()
    );
    assert!(themes.fixture.core.take_config_reload());
    assert!(!themes.fixture.core.take_config_reload(), "taken once");
    assert_eq!(themes.fixture.store().session(session).unwrap().font_size, None);
    // the same slots again: nothing to rewrite, nothing to reload, the zoom stays
    themes.fixture.store_mut().unwrap().set_font_size(session, 18.0);
    themes.set_ok(
        |a| {
            a.light = Some(LIGHT.to_owned());
            a.dark = Some(DARK.to_owned());
        },
        &json!({"sync": true, "light": LIGHT, "dark": DARK}),
    );
    assert!(!themes.fixture.core.take_config_reload());
    assert_eq!(themes.fixture.store().session(session).unwrap().font_size, Some(18.0));
}

#[test]
fn the_launch_write_creates_the_config_once() {
    let mut themes = Themes::new(AppSettings {
        theme: Some("agterm".to_owned()),
        ..AppSettings::default()
    });
    assert!(themes.fixture.core.write_settings_config());
    assert_eq!(
        themes.config(),
        "theme = agterm\nmouse-scroll-multiplier = 3\nright-click-action = paste\n"
    );
    assert!(!themes.fixture.core.write_settings_config());
    assert!(
        !themes.fixture.core.take_config_reload(),
        "the launch write asks for no reload"
    );
}

/// Upstream's `ghosttyConfigLines` on a host that draws window translucency itself: a translucent opacity pins the
/// renderer transparent; a host without translucency (the default) never writes the pins.
#[test]
fn a_translucent_host_pins_the_renderer_transparent_under_a_translucent_opacity() {
    let settings = AppSettings {
        background_opacity: Some(0.8),
        ..AppSettings::default()
    };
    let mut plain = Themes::new(settings.clone());
    assert!(plain.fixture.core.write_settings_config());
    assert!(!plain.config().contains("background-opacity"), "{}", plain.config());
    let mut host = Themes::new(settings);
    host.fixture.core.set_translucent_host(true);
    assert!(host.fixture.core.write_settings_config());
    assert_eq!(
        host.config(),
        "background-opacity = 0\nbackground-blur = 0\nmouse-scroll-multiplier = 3\nright-click-action = paste\n"
    );
    let mut opaque = Themes::new(AppSettings::default());
    opaque.fixture.core.set_translucent_host(true);
    assert!(opaque.fixture.core.write_settings_config());
    assert!(!opaque.config().contains("background-opacity"), "opaque: no pins");
}
