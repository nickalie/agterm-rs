//! The GPUI build as a process: single instance over a private session bus (both forwarding directions, with the GTK
//! build where it is built), the activation token reaching the primary and raising its window (Xvfb; a niri nested in
//! a headless weston), the SIGTERM quit and its exit status, and the app id fallback. Every process runs in a
//! [`Sandbox`]: nothing here can reach the user's session bus, desktop or live agterm.
//!
//! Tests that need a tool this machine lacks (weston, Xvfb, niri, lavapipe, the GTK build) print `SKIPPED:` with the
//! reason and pass.

#[macro_use]
mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use support::{DEADLINE, Display, Sandbox, TestResult, have, lavapipe_icd, wait_for, wait_for_socket};

/// The token and startup id a forwarding launch is handed in these tests.
const TOKEN: &str = "agterm-test-token";
const STARTUP_ID: &str = "agterm-test-launch_TIME4242";

/// The GTK build's binary beside this one (`cargo build -p agterm-gtk`), or `AGTERM_GTK_BIN`.
fn gtk_build() -> Option<PathBuf> {
    std::env::var_os("AGTERM_GTK_BIN")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(support::GPUI).with_file_name("agterm")))
        .filter(|path| path.is_file())
}

/// Starts the primary on the sandbox's display (none: headless) and waits until it owns its name and serves.
fn start_primary(sandbox: &mut Sandbox) -> TestResult<usize> {
    let primary = sandbox.start_gpui_primary("primary.log")?;
    sandbox.wait_owner(&sandbox.app_id.clone())?;
    Ok(primary)
}

#[test]
fn a_second_launch_forwards_its_token_to_the_primary_and_leaves_the_state_alone() -> TestResult {
    let mut sandbox = Sandbox::new()?;
    let primary = start_primary(&mut sandbox)?;
    let activations = sandbox.monitor_activations()?;
    let before = sandbox.settled_state(Duration::from_secs(1))?;
    let second = sandbox.spawn_gpui(
        &[("XDG_ACTIVATION_TOKEN", TOKEN), ("DESKTOP_STARTUP_ID", STARTUP_ID)],
        "second.log",
    )?;
    let (status, took) = sandbox.wait_exit(second, DEADLINE)?;
    assert!(status.success(), "{status:?}\n{}", sandbox.log("second.log"));
    assert!(took < Duration::from_secs(5), "forwarding took {took:?}");

    let (member, data) = activations.recv_timeout(DEADLINE)?;
    assert_eq!(member, "Activate");
    assert_eq!(data.get("activation-token").map(String::as_str), Some(TOKEN));
    assert_eq!(data.get("desktop-startup-id").map(String::as_str), Some(STARTUP_ID));
    assert_eq!(
        data.get("cwd").map(String::as_str),
        Some(&*sandbox.path("home").to_string_lossy())
    );
    let log = sandbox.wait_log("primary.log", "activation requested over the bus")?;
    assert!(log.contains(&format!("token=\"{TOKEN}\"")), "{log}");
    sandbox.wait_log("primary.log", "activation: no display server to raise a window on")?;

    let second_log = sandbox.log("second.log");
    assert!(second_log.contains("forwarded to the running instance"), "{second_log}");
    assert!(
        !second_log.contains("control socket at"),
        "the second launch served nothing"
    );
    assert_eq!(sandbox.state_snapshot(), before, "the forwarding launch wrote no state");
    assert!(sandbox.child(primary).try_wait()?.is_none(), "the primary runs on");
    Ok(())
}

#[test]
fn sigterm_quits_a_headless_primary_with_status_0_within_10_s() -> TestResult {
    let mut sandbox = Sandbox::new()?;
    let primary = start_primary(&mut sandbox)?;
    sandbox.terminate(primary)?;
    let (status, took) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0), "{status:?}\n{}", sandbox.log("primary.log"));
    assert!(took < Duration::from_secs(10));
    let log = sandbox.log("primary.log");
    assert!(
        log.contains("SIGTERM: quitting") && log.contains("quit: the state is saved"),
        "{log}"
    );
    assert!(!sandbox.path("state/agterm.sock").exists(), "the socket is removed");
    assert!(
        sandbox.path("state/windows.json").is_file(),
        "the quit save wrote the index"
    );
    Ok(())
}

#[test]
fn sigterm_quits_a_windowed_primary_without_the_exit_crash() -> TestResult {
    if !have("weston") {
        skip!("no weston");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (set AGTERM_LAVAPIPE_ICD)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_weston()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = start_primary(&mut sandbox)?;
    // wgpu's instance and device exist (on lavapipe, never a hardware driver) before the quit
    sandbox.wait_log("primary.log", "Selected GPU adapter: \"llvmpipe")?;
    std::thread::sleep(Duration::from_millis(300));
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0), "{status:?}\n{}", sandbox.log("primary.log"));
    Ok(())
}

#[test]
fn an_invalid_app_id_falls_back_to_the_default_one() -> TestResult {
    let mut sandbox = Sandbox::new()?;
    // the private bus only: the default name is the installed app's on the user's bus
    sandbox.set_env("AGTERM_APP_ID", "io.github.9invalid");
    let primary = sandbox.start_gpui_primary("primary.log")?;
    sandbox.wait_owner(agterm_gpui::instance::DEFAULT_APP_ID)?;
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0));
    Ok(())
}

#[test]
fn without_a_session_bus_the_launch_runs_alone() -> TestResult {
    let mut sandbox = Sandbox::new()?;
    let missing = sandbox.path("run/no-bus");
    sandbox.set_env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", missing.display()));
    let primary = sandbox.start_gpui_primary("primary.log")?;
    sandbox.wait_log("primary.log", "no session bus; running without single-instance")?;
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0));
    Ok(())
}

#[test]
fn a_gtk_build_launch_forwards_to_the_gpui_primary() -> TestResult {
    let Some(gtk) = gtk_build() else {
        skip!("no GTK build beside {} (cargo build -p agterm-gtk)", support::GPUI);
    };
    let mut sandbox = Sandbox::new()?;
    let primary = start_primary(&mut sandbox)?;
    let activations = sandbox.monitor_activations()?;
    let before = sandbox.settled_state(Duration::from_secs(1))?;
    let mut command = sandbox.command(&gtk);
    command.env("DESKTOP_STARTUP_ID", STARTUP_ID);
    let second = sandbox.spawn(command, "gtk-second.log")?;
    let (status, _) = sandbox.wait_exit(second, DEADLINE)?;
    assert!(status.success(), "{status:?}\n{}", sandbox.log("gtk-second.log"));
    let (member, data) = activations.recv_timeout(DEADLINE)?;
    assert_eq!(member, "Activate", "{data:?}");
    // a non-primary GApplication registers through DescribeAll first; it got past it, so the primary served it
    sandbox.wait_log("primary.log", "activation requested over the bus")?;
    assert_eq!(sandbox.state_snapshot(), before, "the GTK launch wrote no state");
    assert!(sandbox.child(primary).try_wait()?.is_none());
    Ok(())
}

#[test]
fn a_gpui_launch_forwards_to_a_gtk_primary() -> TestResult {
    let Some(gtk) = gtk_build() else {
        skip!("no GTK build beside {} (cargo build -p agterm-gtk)", support::GPUI);
    };
    if !have("weston") {
        skip!("no weston for the GTK primary");
    }
    let mut sandbox = Sandbox::new()?;
    sandbox.start_weston()?;
    let mut command = sandbox.command(&gtk);
    command.env("GDK_BACKEND", "wayland");
    if let Some(vendor) = support::mesa_egl_vendor() {
        command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
    }
    let primary = sandbox.spawn(command, "gtk-primary.log")?;
    wait_for_socket(&sandbox.path("state/agterm.sock"))?;
    sandbox.wait_owner(&sandbox.app_id.clone())?;
    let activations = sandbox.monitor_activations()?;
    let before = sandbox.settled_state(Duration::from_secs(1))?;
    let second = sandbox.spawn_gpui(
        &[("XDG_ACTIVATION_TOKEN", TOKEN), ("DESKTOP_STARTUP_ID", STARTUP_ID)],
        "second.log",
    )?;
    let (status, _) = sandbox.wait_exit(second, DEADLINE)?;
    assert!(status.success(), "{status:?}\n{}", sandbox.log("second.log"));
    let (member, data) = activations.recv_timeout(DEADLINE)?;
    assert_eq!(member, "Activate");
    assert_eq!(data.get("activation-token").map(String::as_str), Some(TOKEN));
    assert!(sandbox.log("second.log").contains("forwarded to the running instance"));
    assert_eq!(sandbox.state_snapshot(), before, "the forwarding launch wrote no state");
    assert!(sandbox.child(primary).try_wait()?.is_none(), "the GTK primary runs on");
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert!(status.success(), "{status:?}");
    Ok(())
}

/// X11's activation message, `_NET_WM_USER_TIME` and the startup sequence's end, as a window manager would see them.
mod x11 {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, EventMask};

    use super::{DEADLINE, Display, Sandbox, TestResult};

    /// What the root window saw.
    #[derive(Debug, Default)]
    pub struct Seen {
        /// `_NET_ACTIVE_WINDOW`: the window, the source indication and the timestamp.
        pub active: Option<(u32, u32, u32)>,
        /// The startup-notification message, reassembled.
        pub startup: String,
    }

    pub struct Root {
        conn: x11rb::rust_connection::RustConnection,
        root: u32,
    }

    impl Root {
        /// Watches the sandbox's Xvfb root for client messages, as a window manager selects them.
        pub fn watch(sandbox: &Sandbox) -> TestResult<Self> {
            let Display::X11(number) = sandbox.display() else {
                return Err("not an X11 sandbox".into());
            };
            let (conn, screen) = x11rb::connect(Some(&format!(":{number}")))?;
            let root = conn.setup().roots[screen].root;
            let mask = EventMask::SUBSTRUCTURE_NOTIFY | EventMask::PROPERTY_CHANGE;
            conn.change_window_attributes(root, &ChangeWindowAttributesAux::new().event_mask(mask))?
                .check()?;
            Ok(Self { conn, root })
        }

        fn atom(&self, name: &str) -> TestResult<u32> {
            Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        }

        /// Collects until the activation and a NUL-terminated startup message arrived, or the deadline.
        pub fn collect(&self) -> TestResult<Seen> {
            let (active, begin, more) = (
                self.atom("_NET_ACTIVE_WINDOW")?,
                self.atom("_NET_STARTUP_INFO_BEGIN")?,
                self.atom("_NET_STARTUP_INFO")?,
            );
            let mut seen = Seen::default();
            let mut bytes = Vec::new();
            let started = std::time::Instant::now();
            while started.elapsed() < DEADLINE && (seen.active.is_none() || !bytes.contains(&0)) {
                let Some(Event::ClientMessage(event)) = self.conn.poll_for_event()? else {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                };
                if event.type_ == active {
                    let data = event.data.as_data32();
                    seen.active = Some((event.window, data[0], data[1]));
                } else if event.type_ == begin {
                    bytes = event.data.as_data8().to_vec();
                } else if event.type_ == more {
                    bytes.extend_from_slice(&event.data.as_data8());
                }
            }
            let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
            seen.startup = String::from_utf8_lossy(&bytes[..end]).into_owned();
            Ok(seen)
        }

        /// A CARDINAL property of `window`.
        pub fn cardinal(&self, window: u32, name: &str) -> TestResult<Option<u32>> {
            let reply = self
                .conn
                .get_property(false, window, self.atom(name)?, AtomEnum::CARDINAL, 0, 1)?
                .reply()?;
            Ok(reply.value32().and_then(|mut values| values.next()))
        }

        /// `WM_CLASS` of `window`, as text.
        pub fn class(&self, window: u32) -> TestResult<String> {
            let reply = self
                .conn
                .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024)?
                .reply()?;
            Ok(String::from_utf8_lossy(&reply.value).replace('\0', " "))
        }

        pub fn root(&self) -> u32 {
            self.root
        }
    }
}

/// Asserts what a forwarded launch with [`STARTUP_ID`] did on X11.
fn assert_x11_activation(root: &x11::Root, app_id: &str) -> TestResult {
    let seen = root.collect()?;
    let (window, source, time) = seen.active.ok_or("no _NET_ACTIVE_WINDOW reached the root")?;
    assert_ne!(window, root.root());
    assert!(root.class(window)?.contains(app_id), "the primary's window");
    assert_eq!(
        (source, time),
        (1, 4242),
        "an application's request with the launch's time"
    );
    assert_eq!(root.cardinal(window, "_NET_WM_USER_TIME")?, Some(4242));
    assert_eq!(seen.startup, format!("remove: ID={STARTUP_ID}"));
    Ok(())
}

#[test]
fn x11_activation_carries_the_startup_time_and_ends_the_sequence() -> TestResult {
    if !have("Xvfb") {
        skip!("no Xvfb");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (set AGTERM_LAVAPIPE_ICD)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.start_xvfb()?;
    sandbox.use_software_rendering(&lavapipe);
    let app_id = sandbox.app_id.clone();
    let primary = start_primary(&mut sandbox)?;
    sandbox.wait_log("primary.log", "instance: primary")?;
    std::thread::sleep(Duration::from_millis(300));

    let root = x11::Root::watch(&sandbox)?;
    let second = sandbox.spawn_gpui(&[("DESKTOP_STARTUP_ID", STARTUP_ID)], "second.log")?;
    assert!(sandbox.wait_exit(second, DEADLINE)?.0.success());
    assert_x11_activation(&root, &app_id)?;

    if let Some(gtk) = gtk_build() {
        let root = x11::Root::watch(&sandbox)?;
        let mut command = sandbox.command(&gtk);
        command.env("GDK_BACKEND", "x11").env("DESKTOP_STARTUP_ID", STARTUP_ID);
        let second = sandbox.spawn(command, "gtk-second.log")?;
        assert!(
            sandbox.wait_exit(second, DEADLINE)?.0.success(),
            "{}",
            sandbox.log("gtk-second.log")
        );
        assert_x11_activation(&root, &app_id)?;
    } else {
        eprintln!("SKIPPED (GTK launch part): no GTK build beside {}", support::GPUI);
    }
    sandbox.terminate(primary)?;
    assert_eq!(sandbox.wait_exit(primary, Duration::from_secs(10))?.0.code(), Some(0));
    Ok(())
}

/// The nested niri's focused window's app id.
fn niri_focused(sandbox: &Sandbox) -> Option<String> {
    let output = sandbox
        .command("niri")
        .args(["msg", "--json", "focused-window"])
        .output()
        .ok()?;
    let window: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    window.get("app_id")?.as_str().map(str::to_owned)
}

/// The nested niri's window id of the window with `app_id`.
fn niri_window(sandbox: &Sandbox, app_id: &str) -> Option<u64> {
    let output = sandbox
        .command("niri")
        .args(["msg", "--json", "windows"])
        .output()
        .ok()?;
    let windows: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).ok()?;
    windows
        .iter()
        .find(|window| window.get("app_id").and_then(serde_json::Value::as_str) == Some(app_id))?
        .get("id")?
        .as_u64()
}

/// Waits until niri focuses the window of `app_id`.
fn wait_focused(sandbox: &Sandbox, app_id: &str) -> TestResult {
    wait_for(|| (niri_focused(sandbox).as_deref() == Some(app_id)).then_some(()))
        .map_err(|_| format!("niri never focused {app_id} (focused: {:?})", niri_focused(sandbox)).into())
}

/// Has niri spawn `argv`, so it hands the child an activation token of its own making, as a launcher does.
fn niri_spawn(sandbox: &Sandbox, argv: &[&str]) -> TestResult {
    let status = sandbox
        .command("niri")
        .args(["msg", "action", "spawn", "--"])
        .args(argv)
        .status()?;
    assert!(status.success());
    Ok(())
}

#[test]
fn a_launchers_token_raises_the_primary_under_a_nested_niri() -> TestResult {
    if !have("niri") || !have("weston") {
        skip!("no niri or weston: the nested-niri activation check needs both");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (set AGTERM_LAVAPIPE_ICD)");
    };
    let mut sandbox = Sandbox::new()?;
    sandbox.use_software_rendering(&lavapipe);
    sandbox.start_nested_niri()?;
    let app_id = sandbox.app_id.clone();
    let _primary = start_primary(&mut sandbox)?;
    wait_focused(&sandbox, &app_id)?;

    // a decoy of another id, with a state of its own, takes the focus
    let decoy_id = format!("{app_id}.decoy");
    let decoy_state = sandbox.path("decoy-state");
    let decoy_socket = decoy_state.join("agterm.sock");
    let decoy_env: BTreeMap<&str, String> = BTreeMap::from([
        ("AGTERM_APP_ID", decoy_id.clone()),
        ("AGTERM_STATE_DIR", decoy_state.to_string_lossy().into_owned()),
        ("AGTERM_CONTROL_SOCKET", decoy_socket.to_string_lossy().into_owned()),
    ]);
    let decoy_pairs: Vec<(&str, &str)> = decoy_env.iter().map(|(key, value)| (*key, value.as_str())).collect();
    sandbox.spawn_gpui(&decoy_pairs, "decoy.log")?;
    wait_focused(&sandbox, &decoy_id)?;

    niri_spawn(&sandbox, &[support::GPUI])?;
    wait_focused(&sandbox, &app_id)?;
    let log = sandbox.wait_log("primary.log", "activation: window raised")?;
    assert!(log.contains("token=true"), "the token arrived: {log}");

    if let Some(gtk) = gtk_build() {
        let decoy = niri_window(&sandbox, &decoy_id).ok_or("no decoy window")?;
        let status = sandbox
            .command("niri")
            .args(["msg", "action", "focus-window", "--id", &decoy.to_string()])
            .status()?;
        assert!(status.success());
        wait_focused(&sandbox, &decoy_id)?;
        let gtk = gtk.to_string_lossy().into_owned();
        niri_spawn(&sandbox, &["/usr/bin/env", "GDK_BACKEND=wayland", &gtk])?;
        wait_focused(&sandbox, &app_id)?;
    } else {
        eprintln!("SKIPPED (GTK launch part): no GTK build beside {}", support::GPUI);
    }
    Ok(())
}
