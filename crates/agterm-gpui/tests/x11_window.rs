//! The agterm window on a private Xvfb (GPUI's X11 backend, no window manager; CLAUDE.md > Never touch the user's
//! running agterm): `window.list` reads its minimized state back from `_NET_WM_STATE_HIDDEN` as a window manager sets
//! it (GPUI patch `gpui-pre-linux` 0003, the GTK build's GDK read-back), and the frame it had at the quit comes back
//! at the next launch (upstream's `TitleProbeView`; `window_frames.rs`).
//!
//! A test whose tool is missing prints `SKIPPED:` and passes, unless `AGTERM_REQUIRE_DISPLAY_TESTS` is set.

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::io::{BufRead as _, BufReader, Write as _};
use std::time::Duration;

use serde_json::{Value, json};
use support::{DEADLINE, Display, Sandbox, TestResult, have, lavapipe_icd, wait_for};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState, PropMode, Window};
use x11rb::wrapper::ConnectionExt as _;

/// One control request with `args` (and `target`) to the sandbox's socket; its result, or the error it answered.
fn request(sandbox: &Sandbox, cmd: &str, target: Option<&str>, args: &Value) -> TestResult<Value> {
    let mut socket = std::os::unix::net::UnixStream::connect(sandbox.path("state/agterm.sock"))?;
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

/// The first window `window.list` reports.
fn listed(sandbox: &Sandbox) -> TestResult<Value> {
    let result = request(sandbox, "window.list", None, &json!({}))?;
    Ok(result["windows"][0].clone())
}

/// The X server, as a window manager sees it.
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

    /// The mapped top-level window, and its size.
    fn mapped(&self) -> TestResult<Option<(Window, u32, u32)>> {
        for window in self.conn.query_tree(self.root)?.reply()?.children {
            if self.conn.get_window_attributes(window)?.reply()?.map_state != MapState::VIEWABLE {
                continue;
            }
            let geometry = self.conn.get_geometry(window)?.reply()?;
            return Ok(Some((window, u32::from(geometry.width), u32::from(geometry.height))));
        }
        Ok(None)
    }

    fn wait_mapped(&self) -> TestResult<(Window, u32, u32)> {
        wait_for(|| self.mapped().ok().flatten()).map_err(|_| "no agterm window was mapped".into())
    }

    fn atom(&self, name: &str) -> TestResult<u32> {
        Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    }

    /// Sets `window`'s `_NET_WM_STATE` to `states`, as a window manager does when it iconifies or restores it.
    fn set_states(&self, window: Window, states: &[&str]) -> TestResult {
        let property = self.atom("_NET_WM_STATE")?;
        let atoms = states
            .iter()
            .map(|name| self.atom(name))
            .collect::<TestResult<Vec<u32>>>()?;
        self.conn
            .change_property32(PropMode::REPLACE, window, property, AtomEnum::ATOM, &atoms)?;
        self.conn.flush()?;
        Ok(())
    }
}

fn wait_minimized(sandbox: &Sandbox, minimized: bool) -> TestResult {
    wait_for(|| {
        let window = listed(sandbox).ok()?;
        (window["minimized"] == json!(minimized)).then_some(())
    })
    .map_err(|_| format!("window.list never read minimized = {minimized}: {:?}", listed(sandbox)).into())
}

#[test]
fn window_list_reads_the_window_managers_minimized_state() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = sandbox.start_gpui_primary("primary.log")?;
    let x11 = X11::connect(&sandbox)?;
    let (window, _, _) = x11.wait_mapped()?;
    wait_minimized(&sandbox, false)?;

    // a window manager iconifies it, then restores it
    x11.set_states(window, &["_NET_WM_STATE_HIDDEN"])?;
    wait_minimized(&sandbox, true)?;
    x11.set_states(window, &[])?;
    wait_minimized(&sandbox, false)?;
    // a maximized window that is iconified reads minimized, not zoomed
    x11.set_states(
        window,
        &[
            "_NET_WM_STATE_MAXIMIZED_VERT",
            "_NET_WM_STATE_MAXIMIZED_HORZ",
            "_NET_WM_STATE_HIDDEN",
        ],
    )?;
    wait_minimized(&sandbox, true)?;

    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}

#[test]
fn the_window_reopens_at_the_size_it_quit_with() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(&lavapipe);
    let first = sandbox.start_gpui_primary("first.log")?;
    let x11 = X11::connect(&sandbox)?;
    let (_, width, height) = x11.wait_mapped()?;
    assert_eq!((width, height), (900, 600), "upstream's default size");
    let id = listed(&sandbox)?["id"].as_str().ok_or("no window id")?.to_owned();
    request(
        &sandbox,
        "window.resize",
        Some(&id),
        &json!({ "width": 1000, "height": 700 }),
    )?;
    wait_for(|| {
        let (_, width, height) = x11.mapped().ok().flatten()?;
        (width == 1000 && height == 700).then_some(())
    })
    .map_err(|_| "the resize never reached the X server")?;
    sandbox.terminate(first)?;
    assert_eq!(sandbox.wait_exit(first, Duration::from_secs(10))?.0.code(), Some(0));
    let saved = std::fs::read_to_string(sandbox.path("state/window-frames.json"))?;
    assert!(
        saved.contains(&id.to_lowercase()) && saved.contains("1000.0") && saved.contains("700.0"),
        "the quit saved the window's frame: {saved}"
    );

    let second = sandbox.start_gpui_primary("second.log")?;
    let (_, width, height) = x11.wait_mapped()?;
    assert_eq!((width, height), (1000, 700), "the frame it quit with");
    sandbox.terminate(second)?;
    assert_eq!(sandbox.wait_exit(second, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}
