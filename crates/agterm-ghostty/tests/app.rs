//! The app and its runtime callbacks against the real, vendored libghostty (no surface: those need a GL context).

use std::cell::RefCell;
use std::error::Error as StdError;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use agterm_ghostty::{
    Action, App, ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, Config, Error,
    PendingRequest, Runtime, SurfaceKey, Target, keys,
};

type TestResult = Result<(), Box<dyn StdError>>;

#[derive(Default)]
struct Recorder {
    actions: RefCell<Vec<(Target, String)>>,
    applied_font_size: RefCell<Option<f32>>,
}

impl Runtime for Recorder {
    fn action(&self, target: Target, action: Action) -> bool {
        if let Action::ConfigChange(Some(config)) = &action {
            *self.applied_font_size.borrow_mut() = config.get(keys::FONT_SIZE);
        }
        self.actions.borrow_mut().push((target, format!("{action:?}")));
        true
    }

    fn clipboard_read_result(&self, _read: &ClipboardRead) -> ClipboardReadResult {
        ClipboardReadResult::Unsupported
    }

    fn start_clipboard_read(&self, _read: ClipboardRead, _pending: PendingRequest) {}

    fn confirm_clipboard_request(&self, _confirm: ClipboardConfirm, _pending: PendingRequest) {}

    fn write_clipboard(&self, _: Option<SurfaceKey>, _: ClipboardKind, _: Vec<ClipboardContent>, _: bool) {}

    fn close_surface(&self, _surface: SurfaceKey, _process_alive: bool) {}
}

fn finalized(text: &str) -> Result<Config, Error> {
    let mut config = Config::new()?;
    config.load_text(text);
    config.finalize();
    Ok(config)
}

#[test]
fn an_app_reports_the_config_it_applied() -> TestResult {
    let recorder = Rc::new(Recorder::default());
    let wakeups = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakeups);
    let app = App::new(
        &finalized("font-size = 11\n")?,
        recorder.clone(),
        Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    )?;
    app.tick();
    app.update_config(&finalized("font-size = 23\n")?);
    app.tick();
    assert_eq!(
        *recorder.applied_font_size.borrow(),
        Some(23.0),
        "{:?}",
        recorder.actions.borrow()
    );
    let actions = recorder.actions.borrow();
    assert!(
        actions
            .iter()
            .any(|(target, action)| *target == Target::App && action.starts_with("ConfigChange")),
        "{actions:?}"
    );
    drop(actions);
    drop(app);
    Ok(())
}

#[test]
fn one_app_per_thread() -> TestResult {
    let config = finalized("")?;
    let first = App::new(&config, Rc::new(Recorder::default()), Box::new(|| {}))?;
    let second = App::new(&config, Rc::new(Recorder::default()), Box::new(|| {}));
    assert_eq!(
        second.err().map(|err| err.to_string()),
        Some(Error::AppExists.to_string())
    );
    drop(first);
    // the slot is free again once the first app is gone
    let third = App::new(&config, Rc::new(Recorder::default()), Box::new(|| {}))?;
    drop(third);
    Ok(())
}

#[test]
fn config_text_loads_like_a_file() -> TestResult {
    let config = finalized("font-size = 15\ntitle = fixed\n")?;
    assert_eq!(config.diagnostics(), []);
    assert_eq!(config.get(keys::FONT_SIZE), Some(15.0));
    assert_eq!(config.get_string(keys::TITLE).as_deref(), Some("fixed"));
    Ok(())
}

#[test]
fn an_unset_string_key_reads_as_absent() -> TestResult {
    assert_eq!(finalized("")?.get_string(keys::TITLE), None);
    Ok(())
}

#[test]
fn shell_integration_features_read_as_bits() -> TestResult {
    let config = finalized("shell-integration-features = no-cursor,sudo,no-title,ssh-env\n")?;
    // cursor, sudo, title, ssh-env, ssh-terminfo, path from bit 0; path defaults on
    assert_eq!(config.get(keys::SHELL_INTEGRATION_FEATURES), Some(0b10_1010));
    Ok(())
}
