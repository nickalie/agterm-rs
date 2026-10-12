//! Smoke tests against the real, vendored libghostty: init, load a config file, read values back, diagnostics.

use std::error::Error;
use std::path::{Path, PathBuf};

use agterm_ghostty::{Color, Config, ConfigKey, init, keys};

type TestResult = Result<(), Box<dyn Error>>;

fn write_config(dir: &Path, contents: &str) -> Result<PathBuf, std::io::Error> {
    let path = dir.join("ghostty.conf");
    std::fs::write(&path, contents)?;
    Ok(path)
}

#[test]
fn init_is_idempotent() -> TestResult {
    init()?;
    init()?;
    Ok(())
}

#[test]
fn loads_a_config_file_and_reads_values_back() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = write_config(
        dir.path(),
        "font-size = 17.5\nbackground = #102030\nforeground = #a0b0c0\ncursor-opacity = 0.25\n\
         cursor-style-blink = true\nmouse-hide-while-typing = true\n",
    )?;
    let mut config = Config::new()?;
    config.load_file(&path);
    config.finalize();

    assert_eq!(config.diagnostics(), []);
    assert_eq!(config.get(keys::FONT_SIZE), Some(17.5));
    assert_eq!(
        config.get(keys::BACKGROUND),
        Some(Color {
            r: 0x10,
            g: 0x20,
            b: 0x30
        })
    );
    assert_eq!(
        config.get(keys::FOREGROUND),
        Some(Color {
            r: 0xa0,
            g: 0xb0,
            b: 0xc0
        })
    );
    assert_eq!(config.get(keys::CURSOR_OPACITY), Some(0.25));
    assert_eq!(config.get(keys::CURSOR_STYLE_BLINK), Some(true));
    assert_eq!(config.get(keys::MOUSE_HIDE_WHILE_TYPING), Some(true));
    Ok(())
}

#[test]
fn defaults_apply_without_files() -> TestResult {
    let mut config = Config::new()?;
    config.finalize();
    assert_eq!(
        config.get(keys::BACKGROUND),
        Some(Color {
            r: 0x28,
            g: 0x2c,
            b: 0x34
        })
    );
    assert_eq!(config.get(keys::MOUSE_HIDE_WHILE_TYPING), Some(false));
    // an unset optional reads as absent
    assert_eq!(config.get(keys::CURSOR_STYLE_BLINK), None);
    Ok(())
}

#[test]
fn unknown_keys_read_as_absent() -> TestResult {
    let config = Config::new()?;
    assert_eq!(config.get(ConfigKey::<bool>::new("no-such-key")), None);
    Ok(())
}

#[test]
fn a_missing_config_file_reports_a_diagnostic() -> TestResult {
    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("absent.conf");
    let mut config = Config::new()?;
    config.load_file(&missing);
    config.finalize();

    let diagnostics = config.diagnostics();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert!(
        diagnostics[0].message.starts_with(&format!("{}: ", missing.display())),
        "{diagnostics:?}"
    );
    // the config stays usable with its defaults
    assert_eq!(
        config.get(keys::BACKGROUND),
        Some(Color {
            r: 0x28,
            g: 0x2c,
            b: 0x34
        })
    );
    Ok(())
}

#[test]
fn a_directory_in_place_of_a_file_reports_a_diagnostic() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut config = Config::new()?;
    config.load_file(dir.path());
    assert_eq!(config.diagnostics().len(), 1, "{:?}", config.diagnostics());
    Ok(())
}

#[test]
fn a_relative_path_is_loaded() -> TestResult {
    // integration tests run with the package root as the working directory. libghostty asserts on a relative path,
    // but its ReleaseFast build does not trap, so the make-absolute step itself is pinned by a unit test in config.rs
    let relative = Path::new("tests/relative.conf");
    assert!(relative.is_relative() && relative.is_file());
    let mut config = Config::new()?;
    config.load_file(relative);
    config.finalize();
    assert_eq!(config.diagnostics(), []);
    assert_eq!(config.get(keys::FONT_SIZE), Some(19.0));
    Ok(())
}

#[test]
fn a_fifo_in_place_of_a_file_reports_a_diagnostic_without_blocking() -> TestResult {
    let dir = tempfile::tempdir()?;
    let fifo = dir.path().join("config.fifo");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status()?.success());
    // opening a FIFO without a writer blocks, so the load runs on its own thread under a deadline
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let diagnostics = Config::new().map(|mut config| {
            config.load_file(&fifo);
            config.diagnostics().len()
        });
        sender.send(diagnostics.map_err(|err| err.to_string())).ok();
    });
    let diagnostics = receiver.recv_timeout(std::time::Duration::from_secs(10))?;
    assert_eq!(diagnostics?, 1);
    Ok(())
}

#[test]
fn an_invalid_line_reports_libghostty_diagnostics() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = write_config(dir.path(), "no-such-key = 1\nfont-size = 13\n")?;
    let mut config = Config::new()?;
    config.load_file(&path);
    config.finalize();

    let diagnostics = config.diagnostics();
    assert!(
        diagnostics.iter().any(|d| d.message.contains("no-such-key")),
        "{diagnostics:?}"
    );
    // the valid line still applies
    assert_eq!(config.get(keys::FONT_SIZE), Some(13.0));
    Ok(())
}

#[test]
fn a_clone_is_independent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = write_config(dir.path(), "font-size = 21\n")?;
    let mut config = Config::new()?;
    config.load_file(&path);
    config.load_file(dir.path().join("absent.conf"));
    let clone = config.try_clone()?;
    drop(config);
    assert_eq!(clone.get(keys::FONT_SIZE), Some(21.0));
    assert_eq!(clone.diagnostics().len(), 1);
    Ok(())
}
