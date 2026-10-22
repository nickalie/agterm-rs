//! Typing on a Cyrillic layout end to end: a private Xvfb keyboard set to XKB's `ru` layout, keys pressed through XTEST
//! as any keyboard of the X server, read by GPUI's X11 backend through XKB and by ngterm's terminal, and the bytes the
//! program in the pane received. The layout's letters arrive as the text it types; Ctrl with a letter key is that
//! key's Latin control code (Ghostty's legacy encoder takes the Latin key at the position, as every terminal does);
//! the keymap's chords and ghostty's physical-key binds match by key position, as upstream's `chordKey` does; under
//! the Kitty keyboard protocol a chord reports the layout's key with its base layout (US) key as the alternate.
//!
//! What a real input method adds (a preedit composed by `IBus` or `Fcitx`, committed text) is the platform's input
//! handler path, held headless by `terminal::input_tests`; with no XIM server here, the X server's own XKB keymap is
//! the "input method" GPUI reads.
//!
//! A test whose tool is missing prints `SKIPPED:` and passes, unless `NGTERM_REQUIRE_DISPLAY_TESTS` is set.

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::time::Duration;

use serde_json::{Value, json};
use support::{DEADLINE, Display, Sandbox, TestResult, have, lavapipe_icd, wait_for};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, InputFocus, MapState, Window};
use x11rb::protocol::xtest::ConnectionExt as _;

/// X keycodes (evdev + 8) on every evdev keymap: the keys' positions, whatever the layout puts on them.
const CONTROL: u8 = 37;
const SHIFT: u8 = 50;
/// The US layout's C, E, J, A and V keys.
const KEY_C: u8 = 54;
const KEY_E: u8 = 26;
const KEY_J: u8 = 44;
const KEY_A: u8 = 38;
const KEY_V: u8 = 55;

/// One control request with `args` (and `target`) to the sandbox's socket; its result, or the error it answered.
fn request(sandbox: &Sandbox, cmd: &str, target: Option<&str>, args: &Value) -> TestResult<Value> {
    let mut socket = std::os::unix::net::UnixStream::connect(sandbox.path("state/ngterm.sock"))?;
    socket.set_read_timeout(Some(DEADLINE))?;
    let mut line = json!({ "cmd": cmd, "args": args });
    if let Some(target) = target {
        line["target"] = json!(target);
    }
    writeln!(socket, "{line}")?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply)?;
    if reply["ok"] == json!(true) {
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(format!("{cmd} {args}: {reply}").into())
    }
}

/// The pane's program: raw mode, the Kitty flags (disambiguate and report alternate keys) when `$HOME/kitty` exists,
/// `READY<n>` on screen, then everything typed into `$HOME/in<n>`, `n` counting the panes started.
const RECORDER: &str = r#"#!/bin/sh
stty raw -echo
n=$(($(cat "$HOME/count" 2>/dev/null || echo 0) + 1))
echo "$n" > "$HOME/count"
if [ -f "$HOME/kitty" ]; then printf '\033[>5u'; fi
printf 'READY%s' "$n"
exec cat > "$HOME/in$n"
"#;

/// A sandbox on a private Xvfb whose keyboard has XKB's `ru` layout, the ngterm-scoped ghostty config running the
/// recorder in place of the shell, and a keymap chord that leaves a file behind; with the test's connection to the X
/// server, which must stay open: the server resets its keymap to the default when its last client disconnects.
fn cyrillic_sandbox() -> TestResult<(Sandbox, X11)> {
    let mut sandbox = Sandbox::new()?;
    let number = sandbox.start_xvfb()?;
    let x11 = X11::connect(&sandbox)?;
    let status = sandbox
        .command("setxkbmap")
        .args(["-display", &format!(":{number}"), "-layout", "ru"])
        .status()?;
    assert!(status.success(), "setxkbmap -layout ru: {status:?}");
    let recorder = sandbox.path("home/recorder.sh");
    std::fs::write(&recorder, RECORDER)?;
    std::fs::set_permissions(&recorder, std::fs::Permissions::from_mode(0o755))?;
    let config = sandbox.path("state/config");
    std::fs::create_dir_all(&config)?;
    // a state a launch left before (`FirstRunWelcome::has_prior_state`): no welcome dialog holds the keys
    std::fs::create_dir_all(sandbox.path("state/windows"))?;
    std::fs::write(
        config.join("ghostty.conf"),
        format!("command = {}\n", recorder.display()),
    )?;
    std::fs::write(
        config.join("keymap.conf"),
        format!(
            "command \"probe\" ctrl+e touch {}\n",
            sandbox.path("home/fired").display()
        ),
    )?;
    Ok((sandbox, x11))
}

/// The X server as the test's keyboard and window manager.
struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    root: Window,
}

impl X11 {
    fn connect(sandbox: &Sandbox) -> TestResult<Self> {
        let Display::X11(number) = sandbox.display() else {
            return Err("not an X11 sandbox".into());
        };
        let (conn, screen) = x11rb::connect(Some(&format!(":{number}")))?;
        let root = conn.setup().roots[screen].root;
        Ok(Self { conn, root })
    }

    /// Whether `window` is mapped and its `WM_CLASS` names `app_id`.
    fn is_app_window(&self, window: Window, app_id: &str) -> Option<bool> {
        let attributes = self.conn.get_window_attributes(window).ok()?.reply().ok()?;
        let class = self
            .conn
            .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        Some(attributes.map_state == MapState::VIEWABLE && String::from_utf8_lossy(&class.value).contains(app_id))
    }

    /// Waits for the app's window and gives it the keyboard focus, as a window manager would.
    fn focus_app_window(&self, app_id: &str) -> TestResult {
        let window = wait_for(|| {
            let tree = self.conn.query_tree(self.root).ok()?.reply().ok()?;
            tree.children
                .into_iter()
                .find(|window| self.is_app_window(*window, app_id) == Some(true))
        })
        .map_err(|_| "no ngterm window was mapped")?;
        self.conn
            .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)?
            .check()?;
        Ok(())
    }

    /// Presses `keycodes` down in order and lets them up in reverse, as a keyboard of the X server.
    fn press(&self, keycodes: &[u8]) -> TestResult {
        const KEY_PRESS: u8 = 2;
        const KEY_RELEASE: u8 = 3;
        for keycode in keycodes {
            self.conn
                .xtest_fake_input(KEY_PRESS, *keycode, 0, x11rb::NONE, 0, 0, 0)?;
        }
        for keycode in keycodes.iter().rev() {
            self.conn
                .xtest_fake_input(KEY_RELEASE, *keycode, 0, x11rb::NONE, 0, 0, 0)?;
        }
        self.conn.flush()?;
        // GPUI's X11 backend reads a release followed within 20 ms by a press of the same key as an autorepeat
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    }
}

/// The selected session's id once its recorder shows `READY<n>`.
fn wait_ready(sandbox: &Sandbox, n: u32) -> TestResult<String> {
    let marker = format!("READY{n}");
    wait_for(|| {
        let tree = request(sandbox, "tree", None, &json!({})).ok()?;
        let sid = selected_session(&tree)?;
        let text = request(sandbox, "session.text", Some(&sid), &json!({})).ok()?;
        text["text"].as_str()?.contains(&marker).then_some(sid)
    })
    .map_err(|_| format!("{marker} never showed:\n{}", sandbox.log("primary.log")).into())
}

/// The id of the active session of the active workspace.
fn selected_session(tree: &Value) -> Option<String> {
    let workspace = tree["tree"]["workspaces"]
        .as_array()?
        .iter()
        .find(|workspace| workspace["active"] == json!(true))?;
    let session = workspace["sessions"]
        .as_array()?
        .iter()
        .find(|session| session["active"] == json!(true))?;
    Some(session["id"].as_str()?.to_owned())
}

/// What pane `n`'s program received, once it holds `expected.len()` bytes (and a moment more for any excess).
fn received(sandbox: &Sandbox, n: u32, expected: &[u8]) -> TestResult<Vec<u8>> {
    let path = sandbox.path(&format!("home/in{n}"));
    let got = wait_for(|| std::fs::read(&path).ok().filter(|bytes| bytes.len() >= expected.len())).map_err(|_| {
        format!(
            "pane {n} got {:?}, not {:?}",
            String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()),
            String::from_utf8_lossy(expected)
        )
    })?;
    std::thread::sleep(Duration::from_millis(200));
    Ok(std::fs::read(&path).unwrap_or(got))
}

#[test]
fn a_cyrillic_layout_types_its_letters_and_binds_by_key_position() -> TestResult {
    if !have("Xvfb") || !have("setxkbmap") {
        skip!("no Xvfb or setxkbmap");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let (mut sandbox, x11) = cyrillic_sandbox()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = sandbox.start_gpui_primary("primary.log")?;
    x11.focus_app_window(&sandbox.app_id)?;
    wait_ready(&sandbox, 1)?;

    // с and С on the C key; Ctrl there is Ctrl-C; Ctrl on the E key (у) fires the keymap's ctrl+e and sends nothing;
    // о on the J key
    x11.press(&[KEY_C])?;
    x11.press(&[SHIFT, KEY_C])?;
    x11.press(&[CONTROL, KEY_C])?;
    x11.press(&[CONTROL, KEY_E])?;
    wait_for(|| sandbox.path("home/fired").exists().then_some(())).map_err(|_| {
        format!(
            "the keymap's ctrl+e never fired; the pane got {:?}:\n{}",
            String::from_utf8_lossy(&std::fs::read(sandbox.path("home/in1")).unwrap_or_default()),
            sandbox.log("primary.log")
        )
    })?;
    x11.press(&[KEY_J])?;
    let expected = "сС\u{3}о".as_bytes();
    assert_eq!(
        String::from_utf8_lossy(&received(&sandbox, 1, expected)?),
        String::from_utf8_lossy(expected)
    );

    // ghostty's physical-key binds on the same layout: Ctrl+Shift+A (Ф) selects all, Ctrl+Shift+C (С) copies it and
    // Ctrl+Shift+V (М) pastes it back into the program
    x11.press(&[CONTROL, SHIFT, KEY_A])?;
    x11.press(&[CONTROL, SHIFT, KEY_C])?;
    x11.press(&[CONTROL, SHIFT, KEY_V])?;
    let pasted = [expected, b"READY1"].concat();
    assert_eq!(
        String::from_utf8_lossy(&received(&sandbox, 1, &pasted)?),
        String::from_utf8_lossy(&pasted)
    );

    // a program under the Kitty keyboard protocol (disambiguate, report alternate keys): the plain key is its text,
    // Ctrl-С is reported as U+0441 with the US layout's c (99) as its base layout key, as Ghostty's encoder does
    std::fs::write(sandbox.path("home/kitty"), "")?;
    request(&sandbox, "session.new", None, &json!({}))?;
    wait_ready(&sandbox, 2)?;
    x11.press(&[KEY_C])?;
    x11.press(&[CONTROL, KEY_C])?;
    let expected = "с\u{1b}[1089::99;5u".as_bytes();
    assert_eq!(
        String::from_utf8_lossy(&received(&sandbox, 2, expected)?),
        String::from_utf8_lossy(expected)
    );

    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}
