//! A surface's I/O against the real, vendored libghostty on a surfaceless EGL context: typing through key events
//! (a keyless text press, Return as Linux's XKB keycode), reading the viewport and the screen, the selection, the
//! cursor column solved under a nonzero padding, and the foreground pid.

mod common;

use std::error::Error as StdError;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agterm_ghostty::input::{KeyAction, KeyEvent, Mods};
use agterm_ghostty::{App, Config, PointTag, Selection, Surface, SurfaceKey, SurfaceOptions};
use common::{EglContext, Quiet};

type TestResult = Result<(), Box<dyn StdError>>;

/// Linux's XKB keycode for Return, what the GTK frontend sends for a synthetic Return.
const RETURN_KEYCODE: u32 = 36;

fn app() -> Result<App, Box<dyn StdError>> {
    let mut config = Config::new()?;
    // an odd padding, so the cursor column must be calibrated rather than assumed
    config.load_text("font-size = 14\nwindow-padding-x = 11\nwindow-padding-balance = false\n");
    config.finalize();
    Ok(App::new(&config, Rc::new(Quiet), Box::new(|| {}))?)
}

fn surface(app: &App, command: &str) -> Result<Surface, Box<dyn StdError>> {
    let surface = Surface::new(
        app,
        SurfaceKey::new(NonZeroUsize::MIN),
        &SurfaceOptions {
            scale_factor: 1.0,
            command: Some(command),
            ..SurfaceOptions::default()
        },
    )?;
    surface.set_size(640, 360);
    Ok(surface)
}

fn viewport(surface: &Surface) -> Option<String> {
    surface
        .read_text(Selection::whole(PointTag::Viewport))
        .map(|read| read.text)
}

/// Ticks until the viewport's text satisfies `done`; returns it.
fn wait_for(app: &App, surface: &Surface, done: impl Fn(&str) -> bool) -> Result<String, Box<dyn StdError>> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = String::new();
    while Instant::now() < deadline {
        app.tick();
        if let Some(text) = viewport(surface) {
            if done(&text) {
                return Ok(text);
            }
            last = text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("the viewport never got there; it reads {last:?}").into())
}

fn key(action: KeyAction, keycode: u32, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action,
        mods: Mods::empty(),
        consumed_mods: Mods::empty(),
        keycode,
        text: text.map(str::to_owned),
        unshifted_codepoint: 0,
        composing: false,
    }
}

#[test]
fn a_surface_reads_back_its_text_cursor_selection_and_process() -> TestResult {
    let egl = EglContext::new()?;
    let app = app()?;
    let surface = surface(&app, "printf 'first-line\\nabc'; exec sleep 60")?;

    let text = wait_for(&app, &surface, |text| text.contains("first-line\nabc"))?;
    assert!(text.starts_with("first-line\nabc"), "{text:?}");
    let screen = surface
        .read_text(Selection::whole(PointTag::Screen))
        .map(|read| read.text);
    assert!(screen.is_some_and(|screen| screen.contains("first-line\nabc")));

    assert_eq!(
        surface.cursor_column(),
        Some(3),
        "after `abc`, under an 11-point padding"
    );
    // the calibration cell sits past the configured padding (libghostty may add to it), not at 0
    let probe = surface.read_text(Selection::cell(PointTag::Viewport, 0, 0));
    assert!(
        probe.is_some_and(|read| read.top_left_x >= 11.0),
        "column 0 sits at the padding"
    );

    assert!(surface.foreground_pid().is_some());

    assert_eq!(surface.read_selection(), None, "nothing is selected yet");
    assert!(surface.binding_action("select_all"));
    let selected = surface.read_selection().map(|read| read.text);
    assert!(
        selected.as_deref().is_some_and(|text| text.contains("first-line")),
        "{selected:?}"
    );

    drop(surface);
    drop(app);
    drop(egl);
    Ok(())
}

#[test]
fn typed_keys_reach_the_program_and_a_return_submits() -> TestResult {
    let egl = EglContext::new()?;
    let app = app()?;
    // cat in a cooked tty: the line echoes as typed, and cat prints it again once Return submits it
    let surface = surface(&app, "printf 'ready\\n'; exec cat")?;
    wait_for(&app, &surface, |text| text.contains("ready"))?;

    assert!(surface.send_key(&key(KeyAction::Press, 0, Some("typed-run"))));
    wait_for(&app, &surface, |text| text.matches("typed-run").count() == 1)?;
    let _ = surface.send_key(&key(KeyAction::Press, RETURN_KEYCODE, None));
    let _ = surface.send_key(&key(KeyAction::Release, RETURN_KEYCODE, None));
    let text = wait_for(&app, &surface, |text| text.matches("typed-run").count() == 2)?;
    assert!(text.contains("typed-run\ntyped-run"), "{text:?}");

    drop(surface);
    drop(app);
    drop(egl);
    Ok(())
}
