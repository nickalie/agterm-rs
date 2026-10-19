//! keymap.conf's `global-hotkey` end to end, on the sandbox's own displays and bus (CLAUDE.md > Never touch the
//! user's running agterm): on a private Xvfb the chord's key is grabbed on the root window and a press from the X
//! server (XTEST, as any other client's keyboard) toggles the quick terminal, a keymap reload moving the grab; on a
//! headless weston the chord is bound through a fake global shortcuts portal on the private bus, as its preferred
//! trigger, and the portal's `Activated` toggles the panel.
//!
//! A test whose tool is missing prints `SKIPPED:` and passes, unless `AGTERM_REQUIRE_DISPLAY_TESTS` is set.

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};
use support::{DEADLINE, Display, NO_SEATED_WESTON, Sandbox, TestResult, have, lavapipe_icd, seated_weston, wait_for};
use x11rb::connection::Connection as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value as ZValue};

/// One control request with `args` to the sandbox's socket; its result, or the error it answered.
fn request(sandbox: &Sandbox, cmd: &str, args: &Value) -> TestResult<Value> {
    let mut socket = std::os::unix::net::UnixStream::connect(sandbox.path("state/ngterm.sock"))?;
    socket.set_read_timeout(Some(DEADLINE))?;
    writeln!(socket, "{}", json!({ "cmd": cmd, "args": args }))?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    let reply: Value = serde_json::from_str(&reply)?;
    if reply["ok"] == json!(true) {
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(format!("{cmd} {args}: {reply}").into())
    }
}

fn quick_visible(sandbox: &Sandbox) -> bool {
    request(sandbox, "tree", &json!({})).is_ok_and(|tree| tree["tree"]["quickVisible"] == json!(true))
}

fn wait_quick(sandbox: &Sandbox, visible: bool) -> TestResult {
    wait_for(|| (quick_visible(sandbox) == visible).then_some(())).map_err(|_| {
        format!(
            "the quick terminal never read visible = {visible}:\n{}",
            sandbox.log("primary.log")
        )
        .into()
    })
}

/// The keymap in the config directory a state dir override implies (`<state dir>/config`).
fn write_keymap(sandbox: &Sandbox, text: &str) -> TestResult {
    let dir = sandbox.path("state/config");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("keymap.conf"), text)?;
    Ok(())
}

/// Presses `keycodes` down in order and lets them up in reverse, as a keyboard of the X server.
fn press(conn: &x11rb::rust_connection::RustConnection, keycodes: &[u8]) -> TestResult {
    const KEY_PRESS: u8 = 2;
    const KEY_RELEASE: u8 = 3;
    for keycode in keycodes {
        conn.xtest_fake_input(KEY_PRESS, *keycode, 0, x11rb::NONE, 0, 0, 0)?;
    }
    for keycode in keycodes.iter().rev() {
        conn.xtest_fake_input(KEY_RELEASE, *keycode, 0, x11rb::NONE, 0, 0, 0)?;
    }
    conn.flush()?;
    Ok(())
}

/// Left Control, the key left of 1 (the backquote), T and left Alt: X keycodes on every evdev keymap.
const CONTROL: u8 = 37;
const BACKQUOTE: u8 = 49;
const T: u8 = 28;
const ALT: u8 = 64;

#[test]
fn the_grabbed_chord_toggles_the_quick_terminal_from_the_x_server() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    write_keymap(&sandbox, "global-hotkey ctrl+`\n")?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = sandbox.start_gpui_primary("primary.log")?;
    sandbox.wait_log("primary.log", "global hotkey: grabbed")?;
    let Display::X11(number) = sandbox.display() else {
        return Err("not an X11 sandbox".into());
    };
    let (conn, _) = x11rb::connect(Some(&format!(":{number}")))?;
    assert!(!quick_visible(&sandbox));

    press(&conn, &[CONTROL, BACKQUOTE])?;
    wait_quick(&sandbox, true)?;
    press(&conn, &[CONTROL, BACKQUOTE])?;
    wait_quick(&sandbox, false)?;

    // a reload moves the grab: the old chord does nothing, the new one toggles
    write_keymap(&sandbox, "global-hotkey ctrl+alt+t\n")?;
    request(&sandbox, "keymap.reload", &json!({}))?;
    wait_for(|| (sandbox.log("primary.log").matches("global hotkey: grabbed").count() >= 2).then_some(()))
        .map_err(|_| "the reload grabbed nothing new")?;
    press(&conn, &[CONTROL, BACKQUOTE])?;
    std::thread::sleep(Duration::from_millis(300));
    assert!(!quick_visible(&sandbox), "the old chord is let go");
    press(&conn, &[CONTROL, ALT, T])?;
    wait_quick(&sandbox, true)?;

    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}

/// What the fake portal was asked.
#[derive(Default)]
struct Asked {
    /// The session last created.
    session: Option<OwnedObjectPath>,
    /// Each bound shortcut's id and preferred trigger.
    bound: Vec<(String, Option<String>)>,
}

/// A global shortcuts portal answering through `org.freedesktop.portal.Request` as the real one does.
struct Portal {
    asked: Arc<Mutex<Asked>>,
    version: u32,
    connection: Arc<std::sync::OnceLock<zbus::Connection>>,
}

/// The `Request` or `Session` object path the portal derives from the caller and its token.
fn handle(kind: &str, sender: &str, token: &str) -> String {
    let caller = sender.trim_start_matches(':').replace('.', "_");
    format!("/org/freedesktop/portal/desktop/{kind}/{caller}/{token}")
}

fn token(options: &HashMap<String, OwnedValue>, key: &str) -> String {
    options
        .get(key)
        .and_then(|value| String::try_from(value.clone()).ok())
        .unwrap_or_default()
}

/// Answers request `path` of `sender` with `results`, a little later, as a portal answers once its dialog is done.
fn respond(connection: zbus::Connection, sender: String, path: String, results: HashMap<String, OwnedValue>) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let sent = zbus::block_on(connection.emit_signal(
            Some(sender.as_str()),
            path.as_str(),
            "org.freedesktop.portal.Request",
            "Response",
            &(0_u32, results),
        ));
        if let Err(error) = sent {
            eprintln!("the fake portal's response failed: {error}");
        }
    });
}

fn failed(error: impl std::fmt::Display) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(error.to_string())
}

impl Portal {
    /// The portal's own connection, which answers the requests.
    fn connection(&self) -> zbus::fdo::Result<zbus::Connection> {
        self.connection
            .get()
            .cloned()
            .ok_or_else(|| failed("the portal is not up yet"))
    }
}

#[zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts")]
impl Portal {
    #[zbus(property)]
    fn version(&self) -> u32 {
        self.version
    }

    #[expect(clippy::needless_pass_by_value, reason = "zbus hands a method its arguments owned")]
    fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = header.sender().map(ToString::to_string).unwrap_or_default();
        let request = handle("request", &sender, &token(&options, "handle_token"));
        let session = handle("session", &sender, &token(&options, "session_handle_token"));
        let session_path = OwnedObjectPath::try_from(session.clone()).map_err(failed)?;
        self.asked.lock().unwrap_or_else(PoisonError::into_inner).session = Some(session_path);
        let results = HashMap::from([(
            "session_handle".to_owned(),
            OwnedValue::try_from(ZValue::from(session)).map_err(failed)?,
        )]);
        respond(self.connection()?, sender, request.clone(), results);
        OwnedObjectPath::try_from(request).map_err(failed)
    }

    #[expect(clippy::needless_pass_by_value, reason = "zbus hands a method its arguments owned")]
    fn bind_shortcuts(
        &self,
        session: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let sender = header.sender().map(ToString::to_string).unwrap_or_default();
        let request = handle("request", &sender, &token(&options, "handle_token"));
        let mut asked = self.asked.lock().unwrap_or_else(PoisonError::into_inner);
        if asked.session.as_ref() != Some(&session) {
            return Err(failed(format!("no such session: {session} (parent {parent_window:?})")));
        }
        for (id, info) in shortcuts {
            let trigger = info
                .get("preferred_trigger")
                .and_then(|value| String::try_from(value.clone()).ok());
            asked.bound.push((id, trigger));
        }
        respond(self.connection()?, sender, request.clone(), HashMap::new());
        OwnedObjectPath::try_from(request).map_err(failed)
    }
}

#[test]
fn the_portal_binds_the_chord_and_its_activation_toggles_the_quick_terminal() -> TestResult {
    if seated_weston().is_none() {
        skip!("{NO_SEATED_WESTON}");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    let asked = Arc::new(Mutex::new(Asked::default()));
    let connection = Arc::new(std::sync::OnceLock::new());
    let portal = zbus::blocking::connection::Builder::address(sandbox.bus_address())?
        .name("org.freedesktop.portal.Desktop")?
        .serve_at(
            "/org/freedesktop/portal/desktop",
            Portal {
                asked: Arc::clone(&asked),
                version: 1,
                connection: Arc::clone(&connection),
            },
        )?
        .build()?;
    let _ = connection.set(portal.inner().clone());
    write_keymap(&sandbox, "global-hotkey ctrl+`\n")?;
    sandbox.start_weston()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = sandbox.start_gpui_primary("primary.log")?;
    sandbox.wait_log("primary.log", "global hotkey: bound through the portal")?;
    let session = {
        let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(
            asked.bound,
            [("quick-terminal".to_owned(), Some("CTRL+grave".to_owned()))],
            "the chord offered as the preferred trigger"
        );
        asked.session.clone().ok_or("no session was created")?
    };
    let activate = |id: &str| -> TestResult {
        portal.emit_signal(
            None::<()>,
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.GlobalShortcuts",
            "Activated",
            &(session.clone(), id, 0_u64, HashMap::<String, OwnedValue>::new()),
        )?;
        Ok(())
    };
    activate("something-else")?;
    std::thread::sleep(Duration::from_millis(300));
    assert!(!quick_visible(&sandbox), "another shortcut's activation does nothing");
    activate("quick-terminal")?;
    wait_quick(&sandbox, true)?;
    activate("quick-terminal")?;
    wait_quick(&sandbox, false)?;

    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}
