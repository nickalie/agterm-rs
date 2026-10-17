//! The agterm window on a private Xvfb (GPUI's X11 backend, no window manager; CLAUDE.md > Never touch the user's
//! running agterm): `window.list` reads its minimized state back from `_NET_WM_STATE_HIDDEN` as a window manager sets
//! it (GPUI patch `gpui-pre-linux` 0003, the GTK build's GDK read-back) and its frame is saved maximized while it is
//! minimized maximized, the frame it had at the quit comes back at the next launch (upstream's `TitleProbeView`;
//! `window_frames.rs`), on a `RandR` monitor attached now (`monitors.rs`), also after one is docked or undocked while
//! the app runs (the monitors read at GPUI's own X11 scale, patch 0003), and a translucent blurred window asks `KWin`'s
//! blur behind it (patch 0003).
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

    /// Whether `window` carries property `name`.
    fn has_property(&self, window: Window, name: &str) -> TestResult<bool> {
        let reply = self
            .conn
            .get_property(false, window, self.atom(name)?, AtomEnum::ANY, 0, 0)?
            .reply()?;
        Ok(reply.type_ != u32::from(AtomEnum::NONE))
    }

    /// `window`'s position and size on the screen (no window manager: the root is its parent).
    fn geometry(&self, window: Window) -> TestResult<(i16, i16, u16, u16)> {
        let geometry = self.conn.get_geometry(window)?.reply()?;
        Ok((geometry.x, geometry.y, geometry.width, geometry.height))
    }

    /// Lays the screen out as `RandR` monitors `(x, y, width, height, primary)`, the first one on the screen's output,
    /// for as long as this connection stays open (`RandR` drops a client's monitors with it).
    fn set_monitors(&self, monitors: &[(i16, i16, u16, u16, bool)]) -> TestResult {
        use x11rb::protocol::randr::{ConnectionExt as _, MonitorInfo};
        let outputs = self.conn.randr_get_screen_resources(self.root)?.reply()?.outputs;
        for (index, (x, y, width, height, primary)) in monitors.iter().copied().enumerate() {
            let info = MonitorInfo {
                name: self.atom(&format!("monitor-{index}"))?,
                primary,
                automatic: false,
                x,
                y,
                width,
                height,
                width_in_millimeters: 300,
                height_in_millimeters: 200,
                outputs: if index == 0 { outputs.clone() } else { Vec::new() },
            };
            self.conn.randr_set_monitor(self.root, info)?.check()?;
        }
        Ok(())
    }

    /// Resizes the screen to `width`×`height` with `RandR` monitors `(x, y, width, height, primary)` on it, as docking or
    /// undocking a monitor does, for as long as this connection stays open. Xvfb's output has only the mode it started
    /// with, so it goes dark and the monitors stand without one.
    fn lay_out(&self, (width, height): (u16, u16), monitors: &[(i16, i16, u16, u16, bool)]) -> TestResult {
        use x11rb::protocol::randr::{ConnectionExt as _, MonitorInfo, Rotation, SetConfig};
        for monitor in self.conn.randr_get_monitors(self.root, false)?.reply()?.monitors {
            if !monitor.automatic {
                self.conn.randr_delete_monitor(self.root, monitor.name)?.check()?;
            }
        }
        let resources = self.conn.randr_get_screen_resources(self.root)?.reply()?;
        for crtc in resources.crtcs {
            let reply = self
                .conn
                .randr_set_crtc_config(
                    crtc,
                    x11rb::CURRENT_TIME,
                    resources.config_timestamp,
                    0,
                    0,
                    0,
                    Rotation::ROTATE0,
                    &[],
                )?
                .reply()?;
            if reply.status != SetConfig::SUCCESS {
                return Err(format!("the output did not go dark: {:?}", reply.status).into());
            }
        }
        let millimeters = |pixels: u16| u32::from(pixels) / 4;
        self.conn
            .randr_set_screen_size(self.root, width, height, millimeters(width), millimeters(height))?
            .check()?;
        for (index, (x, y, width, height, primary)) in monitors.iter().copied().enumerate() {
            let info = MonitorInfo {
                name: self.atom(&format!("monitor-{index}"))?,
                primary,
                automatic: false,
                x,
                y,
                width,
                height,
                width_in_millimeters: millimeters(width),
                height_in_millimeters: millimeters(height),
                outputs: Vec::new(),
            };
            self.conn.randr_set_monitor(self.root, info)?.check()?;
        }
        Ok(())
    }

    /// The mapped top-level windows.
    fn all_mapped(&self) -> TestResult<Vec<Window>> {
        let mut mapped = Vec::new();
        for window in self.conn.query_tree(self.root)?.reply()?.children {
            if self.conn.get_window_attributes(window)?.reply()?.map_state == MapState::VIEWABLE {
                mapped.push(window);
            }
        }
        Ok(mapped)
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
    assert!(
        !x11.has_property(window, BLUR)?,
        "an opaque window asks no blur behind it"
    );
    let id = listed(&sandbox)?["id"].as_str().ok_or("no window id")?.to_lowercase();

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
    // and its frame is saved maximized, as the window manager keeps it, for the next launch
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(sandbox.path("state/window-frames.json"))?)?;
    assert_eq!(saved[&id]["maximized"], json!(true), "the saved frame: {saved}");
    Ok(())
}

/// `KWin`'s blur-behind property (GPUI patch `gpui-pre-linux` 0003).
const BLUR: &str = "_KDE_NET_WM_BLUR_BEHIND_REGION";

#[test]
fn a_translucent_blurred_window_asks_the_compositor_for_blur_behind_it() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    std::fs::write(
        sandbox.path("state/settings.json"),
        r#"{"backgroundOpacity": 0.8, "backgroundBlur": 20}"#,
    )?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = sandbox.start_gpui_primary("primary.log")?;
    let x11 = X11::connect(&sandbox)?;
    let (window, _, _) = x11.wait_mapped()?;
    wait_for(|| x11.has_property(window, BLUR).ok().filter(|has| *has))
        .map_err(|_| format!("no {BLUR} on the window"))?;
    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}

#[test]
fn a_frame_saved_on_a_monitor_gone_since_comes_back_on_one_attached_now() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(&lavapipe);
    // a 1280×800 screen as a tall monitor and a short primary one beside it, the screen's lower right corner on none
    let x11 = X11::connect(&sandbox)?;
    let (tall, short) = ((0, 0, 640, 800, false), (640, 0, 640, 400, true));
    x11.set_monitors(&[tall, short])?;
    let first = sandbox.start_gpui_primary("first.log")?;
    x11.wait_mapped()?;
    let id = listed(&sandbox)?["id"].as_str().ok_or("no window id")?.to_lowercase();
    sandbox.terminate(first)?;
    assert_eq!(sandbox.wait_exit(first, Duration::from_secs(10))?.0.code(), Some(0));
    // the frame of a window last seen in that corner, on a monitor detached since
    std::fs::write(
        sandbox.path("state/window-frames.json"),
        json!({ id: { "x": 700.0, "y": 450.0, "width": 640.0, "height": 400.0 } }).to_string(),
    )?;

    let second = sandbox.start_gpui_primary("second.log")?;
    let (window, _, _) = x11.wait_mapped()?;
    let (x, y, width, height) = x11.geometry(window)?;
    // nothing of it in the corner no monitor shows (GPUI may nudge a new window by a pixel or two, so the test asks
    // for no overlap with the dead corner rather than an exact place)
    let (right, bottom) = (i32::from(x) + i32::from(width), i32::from(y) + i32::from(height));
    let dead_width = (right.min(1280) - i32::from(short.0).max(i32::from(x))).max(0);
    let dead_height = (bottom.min(800) - i32::from(short.3).max(i32::from(y))).max(0);
    assert_eq!(
        dead_width * dead_height,
        0,
        "the window at {x},{y} {width}×{height} lies on the monitors, not in the corner below the short one"
    );
    assert_eq!((width, height), (640, 400), "fitted to the primary monitor");
    sandbox.terminate(second)?;
    assert_eq!(sandbox.wait_exit(second, Duration::from_secs(10))?.0.code(), Some(0));
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

/// A screen's size and its `RandR` monitors `(x, y, width, height, primary)`, in the screen's pixels.
type Layout = ((u16, u16), &'static [(i16, i16, u16, u16, bool)]);

/// One 2560×1600 monitor, 1280×800 logical at GPUI's scale of 2.
const UNDOCKED: Layout = ((2560, 1600), &[(0, 0, 2560, 1600, true)]);
/// [`UNDOCKED`] with a second such monitor docked to its right.
const DOCKED: Layout = ((5120, 1600), &[(0, 0, 2560, 1600, true), (2560, 0, 2560, 1600, false)]);

/// Launches the app on `launch`, closed window `id`'s saved frame a 900×600 one at logical `origin`, lays the screen
/// out as `later` while it runs, reopens `id` and reads its place on the screen; then closes it again and quits.
fn reopened_after_a_layout_change(
    sandbox: &mut Sandbox,
    x11: &X11,
    id: &str,
    (launch, later): (Layout, Layout),
    (x, y): (f64, f64),
) -> TestResult<(i16, i16, u16, u16)> {
    x11.lay_out(launch.0, launch.1)?;
    std::fs::write(
        sandbox.path("state/window-frames.json"),
        json!({ id.to_lowercase(): { "x": x, "y": y, "width": 900.0, "height": 600.0 } }).to_string(),
    )?;
    let app = sandbox.start_gpui_primary(&format!("{id}-{x}.log"))?;
    x11.wait_mapped()?;
    let before = x11.all_mapped()?;
    x11.lay_out(later.0, later.1)?;
    request(sandbox, "window.select", Some(id), &json!({}))?;
    let window = wait_for(|| {
        x11.all_mapped()
            .ok()?
            .into_iter()
            .find(|window| !before.contains(window))
    })
    .map_err(|_| "the closed window never reopened")?;
    let geometry = x11.geometry(window)?;
    request(sandbox, "window.close", Some(id), &json!({}))?;
    wait_for(|| (!x11.all_mapped().ok()?.contains(&window)).then_some(()))
        .map_err(|_| "the reopened window never closed")?;
    sandbox.terminate(app)?;
    assert_eq!(sandbox.wait_exit(app, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(geometry)
}

#[test]
fn a_window_reopened_after_a_monitor_is_docked_or_undocked_lands_on_the_monitors_attached_now() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb_sized(DOCKED.0.0, DOCKED.0.1)?;
    sandbox.use_software_rendering(&lavapipe);
    // GPUI settles its X11 scale at its start and keeps it through a resize of the screen
    sandbox.set_env("GPUI_X11_SCALE_FACTOR", "2");
    let x11 = X11::connect(&sandbox)?;

    // a second window, closed, for the launches below to reopen
    let first = sandbox.start_gpui_primary("first.log")?;
    x11.wait_mapped()?;
    let created = request(&sandbox, "window.new", None, &json!({}))?;
    let id = created["id"].as_str().ok_or("no new window id")?.to_owned();
    request(&sandbox, "window.close", Some(&id), &json!({}))?;
    sandbox.terminate(first)?;
    assert_eq!(sandbox.wait_exit(first, Duration::from_secs(10))?.0.code(), Some(0));

    // a monitor docked while the app runs: the frame fits the primary monitor as it is, 1280×800 logical (GPUI may
    // nudge a new window by a pixel or two, so its place is asked for within a few)
    let (x, y, width, height) =
        reopened_after_a_layout_change(&mut sandbox, &x11, &id, (UNDOCKED, DOCKED), (100.0, 100.0))?;
    assert_eq!(
        (width, height),
        (1800, 1200),
        "the saved 900×600 at a scale of 2, not fitted to monitors read at a fraction of their size"
    );
    assert!(
        (i32::from(x) - 200).abs() <= 4 && (i32::from(y) - 200).abs() <= 4,
        "left at its saved place: {x},{y}"
    );

    // the monitor it was saved on undocked while the app runs: it comes back on the one left
    let (x, _, width, height) =
        reopened_after_a_layout_change(&mut sandbox, &x11, &id, (DOCKED, UNDOCKED), (1500.0, 100.0))?;
    assert_eq!(
        (width, height),
        (1800, 1200),
        "the saved size, which fits the monitor left"
    );
    // against its right edge, logical 380 (within GPUI's nudge), not left at 1500, off the screen
    assert!(
        (i32::from(x) - 760).abs() <= 4,
        "moved onto the monitor left, against its right edge: x {x}, width {width}"
    );
    Ok(())
}
