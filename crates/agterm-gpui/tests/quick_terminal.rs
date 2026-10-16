//! The quick terminal panel on real displays, each the sandbox's own (CLAUDE.md > Never touch the user's running
//! agterm): the window host on a headless weston (`--fake-seat`, no layer-shell: the compositor refuses the layer
//! surface and the panel falls back) and on a private Xvfb (GPUI's X11 backend, no window manager), and the layer
//! surface under a niri nested in that weston. Each drives the GPUI build through its control socket (`quick`,
//! `quick.type`, `quick.text`, `surface.zoom --target quick`) and reads the panel back from the build's log, the X
//! server or niri's IPC: open, size, resize (the zoom and back), hide and show with the shell kept, placement. Focus
//! and maximize are the window manager's on X11 and are not asserted there.
//!
//! A test whose tool is missing prints `SKIPPED:` and passes, unless `AGTERM_REQUIRE_DISPLAY_TESTS` is set. The
//! nested-niri check is `#[ignore]`d (CI has no niri): run it with `--ignored`.

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::io::{BufRead as _, BufReader, Write as _};

use serde_json::{Value, json};
use support::{DEADLINE, Display, NO_SEATED_WESTON, Sandbox, TestResult, have, lavapipe_icd, seated_weston, wait_for};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MapState};

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

fn quick(sandbox: &Sandbox, mode: &str) -> TestResult {
    request(sandbox, "quick", None, &json!({ "mode": mode }))?;
    Ok(())
}

fn zoom(sandbox: &Sandbox, mode: &str) -> TestResult {
    request(sandbox, "surface.zoom", Some("quick"), &json!({ "mode": mode }))?;
    Ok(())
}

/// The quick terminal's visibility and zoom, read back from the tree.
fn quick_state(sandbox: &Sandbox) -> TestResult<(bool, bool)> {
    let tree = request(sandbox, "tree", None, &json!({}))?;
    let tree = &tree["tree"];
    Ok((
        tree["quickVisible"] == json!(true),
        tree["zoomedSurface"] == json!("quick"),
    ))
}

/// How many lines of the primary's log hold `needle`.
fn count(sandbox: &Sandbox, needle: &str) -> usize {
    sandbox.log("primary.log").matches(needle).count()
}

/// Waits until the primary's log holds `needle` `times` times.
fn wait_count(sandbox: &Sandbox, needle: &str, times: usize) -> TestResult {
    wait_for(|| (count(sandbox, needle) >= times).then_some(())).map_err(|_| {
        format!(
            "{needle:?} never appeared {times} times in the log:\n{}",
            sandbox.log("primary.log")
        )
        .into()
    })
}

/// Types a marker into the quick terminal and waits until its output reads back: the shell runs, shown or hidden.
fn round_trip(sandbox: &Sandbox, marker: &str) -> TestResult {
    let (head, tail) = marker.split_at(4);
    request(
        sandbox,
        "quick.type",
        None,
        &json!({ "text": format!("printf '%s%s\\n' {head} {tail}\n") }),
    )?;
    wait_for(|| {
        let text = request(sandbox, "quick.text", None, &json!({})).ok()?;
        let text = text["text"].as_str()?.to_owned();
        text.lines().any(|line| line == marker).then_some(())
    })
    .map_err(|_| format!("{marker} never read back from the quick terminal").into())
}

/// The sandbox's GPUI primary on its display, rendering on lavapipe, serving its socket.
fn primary(sandbox: &mut Sandbox, lavapipe: &std::path::Path) -> TestResult<usize> {
    sandbox.use_software_rendering(lavapipe);
    sandbox.start_gpui_primary("primary.log")
}

#[test]
fn the_panel_falls_back_to_a_window_on_weston() -> TestResult {
    if seated_weston().is_none() {
        skip!("{NO_SEATED_WESTON}");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_weston()?;
    let primary = primary(&mut sandbox, &lavapipe)?;

    quick(&sandbox, "show")?;
    wait_count(&sandbox, "the compositor has no layer-shell; the panel is a window", 1)?;
    wait_count(&sandbox, "quick terminal: shown", 1)?;
    // weston's output is 1280x800: 90% capped at 1100x700
    wait_count(&sandbox, "quick terminal: sized size=1100x700", 1)?;
    wait_count(&sandbox, "quick terminal: holds the keyboard", 1)?;
    round_trip(&sandbox, "QWIN1")?;

    // the zoom maximizes the window in place, and the unzoom restores it
    zoom(&sandbox, "show")?;
    wait_count(&sandbox, "quick terminal: sized size=1280x800", 1)?;
    assert_eq!(quick_state(&sandbox)?, (true, true));
    zoom(&sandbox, "hide")?;
    wait_count(&sandbox, "quick terminal: sized size=1100x700", 2)?;
    assert_eq!(count(&sandbox, "quick terminal: shown"), 1, "the zoom kept the window");

    // hidden, the window goes and the shell stays typable; shown, a new window over the same shell
    quick(&sandbox, "hide")?;
    wait_count(&sandbox, "quick terminal: hidden", 1)?;
    round_trip(&sandbox, "QWIN2")?;
    quick(&sandbox, "show")?;
    wait_count(&sandbox, "quick terminal: shown", 2)?;
    wait_count(&sandbox, "quick terminal: sized size=1100x700", 3)?;
    assert_eq!(
        count(&sandbox, "the compositor has no layer-shell"),
        1,
        "the window host is kept for the run"
    );
    let text = request(&sandbox, "quick.text", None, &json!({ "all": true }))?;
    assert!(text["text"].as_str().is_some_and(|text| text.contains("QWIN2")));

    sandbox.terminate(primary)?;
    assert_eq!(
        sandbox.wait_exit(primary, std::time::Duration::from_secs(10))?.0.code(),
        Some(0)
    );
    Ok(())
}

/// The X server's view of the panel window.
struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
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

    /// The mapped top-level window titled `title`: its position on the root and its size.
    fn mapped(&self, title: &str) -> TestResult<Option<(i32, i32, u32, u32)>> {
        let tree = self.conn.query_tree(self.root)?.reply()?;
        for window in tree.children {
            let name = self
                .conn
                .get_property(false, window, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 1024)?
                .reply()?;
            if String::from_utf8_lossy(&name.value) != title {
                continue;
            }
            if self.conn.get_window_attributes(window)?.reply()?.map_state != MapState::VIEWABLE {
                continue;
            }
            let geometry = self.conn.get_geometry(window)?.reply()?;
            let at = self.conn.translate_coordinates(window, self.root, 0, 0)?.reply()?;
            return Ok(Some((
                i32::from(at.dst_x),
                i32::from(at.dst_y),
                u32::from(geometry.width),
                u32::from(geometry.height),
            )));
        }
        Ok(None)
    }

    fn wait_mapped(&self, title: &str) -> TestResult<(i32, i32, u32, u32)> {
        wait_for(|| self.mapped(title).ok().flatten()).map_err(|_| format!("no {title} window was mapped").into())
    }

    fn wait_gone(&self, title: &str) -> TestResult {
        wait_for(|| matches!(self.mapped(title), Ok(None)).then_some(()))
            .map_err(|_| format!("the {title} window stayed").into())
    }
}

/// The panel at 1100x700 centered on the 1280x800 screen, at (90, 50): GPUI's X11 backend creates every window 2 px
/// right of the origin it is given (`bounds.origin.x + 2` in its `CreateWindow`).
fn assert_centered(geometry: (i32, i32, u32, u32)) {
    let (x, y, width, height) = geometry;
    assert!((90..=92).contains(&x) && y == 50, "placed at ({x}, {y})");
    assert_eq!((width, height), (1100, 700));
}

#[test]
fn the_panel_is_a_centered_window_on_x11() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    let primary = primary(&mut sandbox, &lavapipe)?;
    let x11 = X11::connect(&sandbox)?;

    quick(&sandbox, "show")?;
    // the screen is 1280x800: 1100x700 centered on it, no layer surface tried
    assert_centered(x11.wait_mapped("Quick Terminal")?);
    wait_count(&sandbox, "quick terminal: shown", 1)?;
    assert_eq!(
        count(&sandbox, "no layer-shell"),
        0,
        "X11 takes the window host directly"
    );
    wait_count(&sandbox, "quick terminal: sized size=1100x700", 1)?;
    round_trip(&sandbox, "QX111")?;

    quick(&sandbox, "hide")?;
    x11.wait_gone("Quick Terminal")?;
    round_trip(&sandbox, "QX112")?;
    quick(&sandbox, "show")?;
    assert_centered(x11.wait_mapped("Quick Terminal")?);
    quick(&sandbox, "toggle")?;
    x11.wait_gone("Quick Terminal")?;

    sandbox.terminate(primary)?;
    assert_eq!(
        sandbox.wait_exit(primary, std::time::Duration::from_secs(10))?.0.code(),
        Some(0)
    );
    Ok(())
}

/// The nested niri's layer surface in `namespace`, if mapped.
fn niri_layer(sandbox: &Sandbox, namespace: &str) -> Option<Value> {
    let output = sandbox
        .command("niri")
        .args(["msg", "--json", "layers"])
        .output()
        .ok()?;
    let layers: Vec<Value> = serde_json::from_slice(&output.stdout).ok()?;
    layers
        .into_iter()
        .find(|layer| layer.get("namespace").and_then(Value::as_str) == Some(namespace))
}

#[test]
#[ignore = "needs niri (none in CI): niri nested in a private headless weston; run with --ignored"]
fn the_panel_is_an_overlay_layer_surface_under_a_nested_niri() -> TestResult {
    if !have("niri") || seated_weston().is_none() {
        skip!("no niri, or {NO_SEATED_WESTON}: the nested-niri check needs both");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (scripts/fetch-lavapipe.sh)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.use_software_rendering(&lavapipe);
    sandbox.start_nested_niri()?;
    let primary = sandbox.start_gpui_primary("primary.log")?;
    let namespace = "agterm-quick-terminal";

    quick(&sandbox, "show")?;
    // niri lists a surface before its first commit with the protocol's defaults (background layer, no keyboard)
    let committed = || niri_layer(&sandbox, namespace).filter(|layer| layer["keyboard_interactivity"] != json!("None"));
    let layer = wait_for(committed).map_err(|_| {
        format!(
            "{:?}
{}",
            niri_layer(&sandbox, namespace),
            sandbox.log("primary.log")
        )
    })?;
    assert_eq!(layer["layer"], json!("Overlay"), "{layer}");
    assert_eq!(layer["keyboard_interactivity"], json!("OnDemand"), "{layer}");
    wait_count(&sandbox, "quick terminal: shown", 1)?;
    assert_eq!(count(&sandbox, "no layer-shell"), 0, "niri has layer-shell");
    wait_count(&sandbox, "quick terminal: sized size=1100x700", 1)?;
    wait_count(&sandbox, "quick terminal: holds the keyboard", 1)?;
    round_trip(&sandbox, "QNIR1")?;

    // the zoom reopens the surface anchored to every edge, the unzoom back at the panel's size; the new surface takes
    // the keyboard and neither surface reports losing it (the control show pinned the panel, so a focus loss would
    // not hide it: the log is what shows one)
    zoom(&sandbox, "show")?;
    wait_count(&sandbox, "quick terminal: shown", 2)?;
    wait_count(&sandbox, "quick terminal: sized size=1280x800", 1)?;
    wait_count(&sandbox, "quick terminal: holds the keyboard", 2)?;
    assert_eq!(quick_state(&sandbox)?, (true, true));
    zoom(&sandbox, "hide")?;
    wait_count(&sandbox, "quick terminal: shown", 3)?;
    wait_count(&sandbox, "quick terminal: sized size=1100x700", 2)?;
    wait_count(&sandbox, "quick terminal: holds the keyboard", 3)?;
    assert_eq!(
        count(&sandbox, "quick terminal: lost the keyboard"),
        0,
        "the reopens are no focus loss:\n{}",
        sandbox.log("primary.log")
    );

    quick(&sandbox, "hide")?;
    wait_for(|| niri_layer(&sandbox, namespace).is_none().then_some(()))
        .map_err(|_| "the layer surface stayed".to_owned())?;
    round_trip(&sandbox, "QNIR2")?;
    quick(&sandbox, "show")?;
    wait_for(|| niri_layer(&sandbox, namespace)).map_err(|_| sandbox.log("primary.log"))?;

    sandbox.terminate(primary)?;
    assert_eq!(
        sandbox.wait_exit(primary, std::time::Duration::from_secs(10))?.0.code(),
        Some(0)
    );
    Ok(())
}
