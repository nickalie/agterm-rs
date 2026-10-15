//! A sandbox per test, isolated from the user's desktop and live agterm (CLAUDE.md > Never touch the user's running
//! agterm): a fresh root under `/tmp`, a private session bus, its own state dir, socket and application id, and
//! every child's environment built from scratch, so no `AGTERM_*`, `ZMX_*`, `WAYLAND_DISPLAY`, `DISPLAY` or
//! `NIRI_SOCKET` of the session the tests run in reaches it. Displays are private too: a headless weston, a private
//! Xvfb, or a niri nested in that weston; GPUI on them renders through Mesa's lavapipe only.
//!
//! [`Sandbox::command`] refuses an environment whose bus, runtime dir or display is not the sandbox's own.
//!
//! A test whose tool is missing skips (`skip!`), unless [`REQUIRE_ENV`] is set, as CI sets it: then it fails.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

mod lavapipe;

pub use lavapipe::lavapipe_icd;

/// How long any wait in these tests may take before it fails.
pub const DEADLINE: Duration = Duration::from_secs(30);

/// The GPUI build under test.
pub const GPUI: &str = env!("CARGO_BIN_EXE_agterm-gpui");

pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The private session bus's configuration, as the conformance harness writes it (no service directories, so
/// nothing is activated; monitoring allowed for the same user).
const DBUS_CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={path}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

const WESTON_CONFIG: &str =
    "[core]\nidle-time=0\n\n[shell]\npanel-position=none\nlocking=false\n\n[input-method]\npath=\n";

/// A private niri: no startup programs, no Xwayland, no hotkey overlay, no animations; stock activation policy (no
/// `honor-xdg-activation-with-invalid-serial`).
const NIRI_CONFIG: &str = "input {\n    keyboard {\n        xkb {\n            layout \"us\"\n        }\n    }\n}\n\
hotkey-overlay {\n    skip-at-startup\n}\nxwayland-satellite {\n    off\n}\nanimations {\n    off\n}\nprefer-no-csd\n";

/// A display the sandbox runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Display {
    /// None: GPUI runs headless.
    None,
    /// A Wayland socket inside the sandbox.
    Wayland(PathBuf),
    /// A private Xvfb's display number.
    X11(u32),
}

pub struct Sandbox {
    pub root: tempfile::TempDir,
    pub app_id: String,
    env: BTreeMap<String, String>,
    display: Display,
    helpers: Vec<(String, Child)>,
    children: Vec<Child>,
}

impl Sandbox {
    /// A root with home, state and runtime dirs, and a private session bus running.
    pub fn new() -> TestResult<Self> {
        let root = tempfile::Builder::new().prefix("agt-gpui").tempdir_in("/tmp")?;
        let path = root.path().to_owned();
        for dir in ["home", "state", "run", "config", "data", "cache"] {
            std::fs::create_dir_all(path.join(dir))?;
        }
        std::fs::set_permissions(path.join("run"), std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let app_id = format!("io.github.melonamin.agterm.test.i{}", uuid::Uuid::new_v4().simple());
        let mut env = BTreeMap::new();
        let mut set = |key: &str, value: &Path| env.insert(key.to_owned(), value.to_string_lossy().into_owned());
        set("HOME", &path.join("home"));
        set("XDG_RUNTIME_DIR", &path.join("run"));
        set("XDG_CONFIG_HOME", &path.join("config"));
        set("XDG_DATA_HOME", &path.join("data"));
        set("XDG_CACHE_HOME", &path.join("cache"));
        set("AGTERM_STATE_DIR", &path.join("state"));
        set("AGTERM_CONTROL_SOCKET", &path.join("state/agterm.sock"));
        env.insert("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned());
        env.insert("SHELL".to_owned(), "/bin/sh".to_owned());
        env.insert("AGTERM_APP_ID".to_owned(), app_id.clone());
        env.insert("AGTERM_LOG".to_owned(), "info".to_owned());
        // plain logs to match against
        env.insert("NO_COLOR".to_owned(), "1".to_owned());
        env.insert("GIO_USE_PORTALS".to_owned(), "0".to_owned());
        let bus = path.join("run/bus");
        env.insert(
            "DBUS_SESSION_BUS_ADDRESS".to_owned(),
            format!("unix:path={}", bus.display()),
        );
        let mut sandbox = Self {
            root,
            app_id,
            env,
            display: Display::None,
            helpers: Vec::new(),
            children: Vec::new(),
        };
        sandbox.start_bus(&bus)?;
        Ok(sandbox)
    }

    pub fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    pub fn set_env(&mut self, key: &str, value: impl Into<String>) {
        self.env.insert(key.to_owned(), value.into());
    }

    pub fn bus_address(&self) -> &str {
        &self.env["DBUS_SESSION_BUS_ADDRESS"]
    }

    pub fn display(&self) -> &Display {
        &self.display
    }

    /// `program` with only the sandbox's environment, after checking that it names nothing outside the sandbox.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        self.check_isolated(&self.env);
        let mut command = Command::new(program);
        command.env_clear().envs(&self.env).current_dir(self.path("home"));
        command
    }

    /// Panics unless `env`'s bus, runtime dir and display are this sandbox's.
    pub fn check_isolated(&self, env: &BTreeMap<String, String>) {
        let root = self.root.path().to_string_lossy().into_owned();
        let bus = &env["DBUS_SESSION_BUS_ADDRESS"];
        assert!(
            bus.starts_with(&format!("unix:path={root}/")),
            "the bus {bus} is not the sandbox's"
        );
        assert!(
            env["XDG_RUNTIME_DIR"].starts_with(&root),
            "a runtime dir outside the sandbox"
        );
        if let Some(wayland) = env.get("WAYLAND_DISPLAY") {
            assert!(
                wayland.starts_with(&root),
                "a Wayland display outside the sandbox: {wayland}"
            );
        }
        if let Some(niri) = env.get("NIRI_SOCKET") {
            assert!(niri.starts_with(&root), "a niri outside the sandbox: {niri}");
        }
        if let Some(x11) = env.get("DISPLAY") {
            assert_eq!(
                Display::X11(x11.trim_start_matches(':').parse().unwrap_or(u32::MAX)),
                self.display
            );
        }
        assert!(env.keys().all(|key| !key.starts_with("ZMX_")));
        assert_ne!(env["AGTERM_STATE_DIR"], "", "no state dir");
    }

    fn start_bus(&mut self, socket: &Path) -> TestResult {
        let config = self.path("dbus.conf");
        std::fs::write(&config, DBUS_CONFIG.replace("{path}", &socket.to_string_lossy()))?;
        let child = self
            .command("dbus-daemon")
            .args(["--nofork", "--nopidfile", "--nosyslog"])
            .arg(format!("--config-file={}", config.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        self.helpers.push(("dbus-daemon".to_owned(), child));
        wait_for_socket(socket)
    }

    /// A private headless weston (with a seat GPUI needs, [`seated_weston`]); clients get its socket as
    /// `WAYLAND_DISPLAY`.
    pub fn start_weston(&mut self) -> TestResult<PathBuf> {
        let weston = seated_weston().ok_or(NO_SEATED_WESTON)?;
        let ini = self.path("weston.ini");
        std::fs::write(&ini, WESTON_CONFIG)?;
        let mut command = self.command(weston);
        command
            .args([
                "--backend=headless",
                "--renderer=gl",
                "--socket=wayland-0",
                "--fake-seat",
            ])
            .args(["--width=1280", "--height=800"])
            .arg(format!("--config={}", ini.display()))
            .arg(format!("--log={}", self.path("weston.log").display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(vendor) = mesa_egl_vendor() {
            command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
        }
        let child = command.spawn()?;
        self.helpers.push(("weston".to_owned(), child));
        let socket = self.path("run/wayland-0");
        wait_for_socket(&socket)?;
        self.use_wayland(socket.clone());
        Ok(socket)
    }

    /// A niri nested in the sandbox's weston; clients get its socket and IPC.
    pub fn start_nested_niri(&mut self) -> TestResult {
        let parent = self.start_weston()?;
        let run = self.path("niri-run");
        std::fs::create_dir_all(&run)?;
        std::fs::set_permissions(&run, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        let config = self.path("niri.kdl");
        std::fs::write(&config, NIRI_CONFIG)?;
        let mut command = self.command("niri");
        command
            .arg("-c")
            .arg(&config)
            .env("WAYLAND_DISPLAY", &parent)
            .env("XDG_RUNTIME_DIR", &run)
            .env("NIRI_DISABLE_SYSTEM_MANAGER_NOTIFY", "1")
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(self.path("niri.log"))?);
        if let Some(vendor) = mesa_egl_vendor() {
            command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
        }
        let child = command.spawn()?;
        self.helpers.push(("niri".to_owned(), child));
        let display = wait_for(|| {
            first_match(&run, |name| {
                name.starts_with("wayland-") && !has_extension(name, "lock")
            })
        })?;
        let ipc = wait_for(|| first_match(&run, |name| name.starts_with("niri.") && has_extension(name, "sock")))?;
        wait_for_socket(&display)?;
        self.env
            .insert("XDG_RUNTIME_DIR".to_owned(), run.to_string_lossy().into_owned());
        self.env
            .insert("NIRI_SOCKET".to_owned(), ipc.to_string_lossy().into_owned());
        self.use_wayland(display);
        Ok(())
    }

    fn use_wayland(&mut self, socket: PathBuf) {
        self.env.remove("DISPLAY");
        self.env
            .insert("WAYLAND_DISPLAY".to_owned(), socket.to_string_lossy().into_owned());
        self.display = Display::Wayland(socket);
    }

    /// A private Xvfb (no TCP, a free display number of its choosing, no window manager).
    pub fn start_xvfb(&mut self) -> TestResult<u32> {
        let mut child = self
            .command("Xvfb")
            .args(["-displayfd", "1", "-nolisten", "tcp", "-screen", "0", "1280x800x24"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("no Xvfb stdout")?;
        self.helpers.push(("Xvfb".to_owned(), child));
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line)?;
        let number: u32 = line.trim().parse()?;
        self.env.remove("WAYLAND_DISPLAY");
        self.env.insert("DISPLAY".to_owned(), format!(":{number}"));
        self.display = Display::X11(number);
        Ok(number)
    }

    /// GPUI on the sandbox's display renders through lavapipe and Mesa's EGL only, never a hardware driver.
    pub fn use_software_rendering(&mut self, lavapipe: &Path) {
        let icd = lavapipe.to_string_lossy().into_owned();
        self.env.insert("VK_DRIVER_FILES".to_owned(), icd.clone());
        self.env.insert("VK_ICD_FILENAMES".to_owned(), icd);
        // wgpu's GL backend (GPUI enables it beside Vulkan) goes through Mesa's llvmpipe too
        self.env.insert("LIBGL_ALWAYS_SOFTWARE".to_owned(), "1".to_owned());
        if let Some(vendor) = mesa_egl_vendor() {
            self.env.insert("__EGL_VENDOR_LIBRARY_FILENAMES".to_owned(), vendor);
        }
    }

    /// Starts `command`, its output in `log` under the root; reaped when the sandbox drops.
    pub fn spawn(&mut self, mut command: Command, log: &str) -> TestResult<usize> {
        let out = std::fs::File::create(self.path(log))?;
        let child = command
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()?;
        self.children.push(child);
        Ok(self.children.len() - 1)
    }

    /// The GPUI build with the sandbox's environment plus `extra`, output in `log`.
    pub fn spawn_gpui(&mut self, extra: &[(&str, &str)], log: &str) -> TestResult<usize> {
        let mut command = self.command(GPUI);
        command.envs(extra.iter().copied());
        self.spawn(command, log)
    }

    pub fn child(&mut self, index: usize) -> &mut Child {
        &mut self.children[index]
    }

    /// Waits for child `index` to exit; its status and how long the wait took.
    pub fn wait_exit(&mut self, index: usize, limit: Duration) -> TestResult<(ExitStatus, Duration)> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.children[index].try_wait()? {
                return Ok((status, started.elapsed()));
            }
            if started.elapsed() > limit {
                return Err(format!("child {index} still running after {limit:?}").into());
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Sends SIGTERM to child `index`.
    pub fn terminate(&mut self, index: usize) -> TestResult {
        let pid = libc::pid_t::try_from(self.children[index].id())?;
        // SAFETY: `kill` only sends a signal to a child this sandbox spawned and has not reaped yet.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }

    pub fn log(&self, name: &str) -> String {
        std::fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    /// Waits until `log` contains `needle`.
    pub fn wait_log(&self, log: &str, needle: &str) -> TestResult<String> {
        let started = Instant::now();
        loop {
            let text = self.log(log);
            if text.contains(needle) {
                return Ok(text);
            }
            if started.elapsed() > DEADLINE {
                return Err(format!("{needle:?} never appeared in {log}:\n{text}").into());
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Starts the GPUI primary and waits until it serves its control socket.
    pub fn start_gpui_primary(&mut self, log: &str) -> TestResult<usize> {
        let index = self.spawn_gpui(&[], log)?;
        wait_for_socket(&self.path("state/agterm.sock")).map_err(|error| format!("{error}\n{}", self.log(log)))?;
        Ok(index)
    }

    /// Sends one control request (`{"cmd": cmd}`) to the sandbox's socket; the reply line.
    pub fn request(&self, cmd: &str) -> TestResult<String> {
        use std::io::Write as _;
        let mut socket = std::os::unix::net::UnixStream::connect(self.path("state/agterm.sock"))?;
        socket.set_read_timeout(Some(DEADLINE))?;
        writeln!(socket, "{}", serde_json::json!({ "cmd": cmd }))?;
        let mut reply = String::new();
        BufReader::new(socket).read_line(&mut reply)?;
        Ok(reply)
    }

    /// A connection to the private bus.
    pub fn bus(&self) -> TestResult<zbus::blocking::Connection> {
        Ok(zbus::blocking::connection::Builder::address(self.bus_address())?.build()?)
    }

    /// Waits until `name` has an owner on the private bus.
    pub fn wait_owner(&self, name: &str) -> TestResult {
        let bus = self.bus()?;
        let proxy = zbus::blocking::fdo::DBusProxy::new(&bus)?;
        let started = Instant::now();
        while !proxy.name_has_owner(name.try_into()?)? {
            if started.elapsed() > DEADLINE {
                return Err(format!("{name} never got an owner").into());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// Watches the private bus for `org.gtk.Application` calls; each arrives as its member and platform data.
    pub fn monitor_activations(&self) -> TestResult<mpsc::Receiver<(String, BTreeMap<String, String>)>> {
        let bus = self.bus()?;
        let rules = vec!["type='method_call',interface='org.gtk.Application'"];
        bus.call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus.Monitoring"),
            "BecomeMonitor",
            &(rules, 0_u32),
        )?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for message in zbus::blocking::MessageIterator::from(bus).flatten() {
                let header = message.header();
                let interface = header.interface().map(|name| name.as_str().to_owned());
                if message.message_type() != zbus::message::Type::MethodCall
                    || interface.as_deref() != Some("org.gtk.Application")
                {
                    continue;
                }
                let member = header.member().map(ToString::to_string).unwrap_or_default();
                let data = platform_data(&message);
                if sender.send((member, data)).is_err() {
                    break;
                }
            }
        });
        Ok(receiver)
    }

    /// The `windows.json` index and every window file in the state dir, as they are now.
    pub fn state_snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut snapshot = BTreeMap::new();
        collect(&self.path("state"), &mut snapshot);
        snapshot
    }

    /// The state dir once the running primary stopped writing it (unchanged for `quiet`): what a forwarding launch
    /// must leave as it is.
    pub fn settled_state(&self, quiet: Duration) -> TestResult<BTreeMap<PathBuf, Vec<u8>>> {
        let started = Instant::now();
        let mut last = self.state_snapshot();
        let mut since = Instant::now();
        while since.elapsed() < quiet {
            if started.elapsed() > DEADLINE {
                return Err("the primary kept writing its state".into());
            }
            thread::sleep(Duration::from_millis(50));
            let now = self.state_snapshot();
            if now != last {
                last = now;
                since = Instant::now();
            }
        }
        Ok(last)
    }
}

/// The platform data of an `Activate` (`a{sv}` last argument), its string and bytestring values as text.
fn platform_data(message: &zbus::Message) -> BTreeMap<String, String> {
    use std::collections::HashMap;
    use zbus::zvariant::OwnedValue;
    let body = message.body();
    let data: Option<HashMap<String, OwnedValue>> = match message.header().member().map(|m| m.as_str().to_owned()) {
        Some(member) if member == "Activate" => body.deserialize::<(HashMap<String, OwnedValue>,)>().ok().map(|(d,)| d),
        Some(member) if member == "Open" => body
            .deserialize::<(Vec<String>, String, HashMap<String, OwnedValue>)>()
            .ok()
            .map(|(_, _, d)| d),
        _ => None,
    };
    data.unwrap_or_default()
        .into_iter()
        .map(|(key, value)| (key, value_text(&value)))
        .collect()
}

/// A string value as is, a bytestring (`ay`, NUL-terminated) as text, anything else empty.
fn value_text(value: &zbus::zvariant::Value<'_>) -> String {
    if let Ok(text) = <&str>::try_from(value) {
        return text.to_owned();
    }
    let Ok(owned) = value.try_clone() else {
        return String::new();
    };
    Vec::<u8>::try_from(owned)
        .map(|bytes| String::from_utf8_lossy(&bytes).trim_end_matches('\0').to_owned())
        .unwrap_or_default()
}

/// Whether file `name` ends in `.extension`.
fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

fn collect(dir: &Path, into: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            collect(&path, into);
        } else if has_extension(&name, "json") {
            into.insert(path.clone(), std::fs::read(&path).unwrap_or_default());
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for child in self
            .children
            .iter_mut()
            .chain(self.helpers.iter_mut().rev().map(|(_, child)| child))
        {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}

/// Mesa's EGL vendor file, so nothing loads a hardware vendor's EGL.
pub fn mesa_egl_vendor() -> Option<String> {
    ["/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d"]
        .iter()
        .find_map(|dir| {
            first_match(Path::new(dir), |name| {
                name.contains("mesa") && has_extension(name, "json")
            })
        })
        .map(|path| path.to_string_lossy().into_owned())
}

/// What a test lacking a seat-capable weston says when it skips.
pub const NO_SEATED_WESTON: &str = "no weston with --fake-seat on PATH (weston 15.0 or later: scripts/build-weston.sh)";

/// The first weston on the test process's PATH that takes `--fake-seat` (weston 15.0 and later): headless weston has
/// a `wl_seat`, which GPUI panics without, only with it, and an older one exits on the option.
pub fn seated_weston() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("weston"))
        .filter(|weston| weston.is_file())
        .find(|weston| {
            Command::new(weston)
                .arg("--help")
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("DISPLAY")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .is_ok_and(|help| String::from_utf8_lossy(&help.stdout).contains("--fake-seat"))
        })
}

/// Whether `program` is on the test process's PATH.
pub fn have(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

fn first_match(dir: &Path, wanted: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|entry| wanted(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found.into_iter().next()
}

pub fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> TestResult<T> {
    let started = Instant::now();
    loop {
        if let Some(found) = probe() {
            return Ok(found);
        }
        if started.elapsed() > DEADLINE {
            return Err("timed out".into());
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Waits until `path` is a Unix socket accepting connections.
pub fn wait_for_socket(path: &Path) -> TestResult {
    let started = Instant::now();
    while std::os::unix::net::UnixStream::connect(path).is_err() {
        if started.elapsed() > DEADLINE {
            return Err(format!("{} never accepted a connection", path.display()).into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Set (to anything) where the tools must be there, as in CI: a test missing one fails instead of skipping.
pub const REQUIRE_ENV: &str = "AGTERM_REQUIRE_DISPLAY_TESTS";

/// Whether a missing tool fails the test ([`REQUIRE_ENV`]).
pub fn tools_required() -> bool {
    std::env::var_os(REQUIRE_ENV).is_some()
}

/// The part of a test that needs a missing tool is left out: as `skip!`, failing under [`REQUIRE_ENV`].
pub fn skip_part(reason: &str) {
    assert!(!tools_required(), "required by {REQUIRE_ENV}: {reason}");
    eprintln!("SKIPPED (part): {reason}");
}

/// A test that cannot run without a missing tool: it fails under [`REQUIRE_ENV`], otherwise it says why on stderr
/// (shown with `--nocapture`) and returns, passing.
macro_rules! skip {
    ($($reason:tt)*) => {{
        let reason = format!($($reason)*);
        assert!(!support::tools_required(), "required by {}: {reason}", support::REQUIRE_ENV);
        eprintln!("SKIPPED: {reason}");
        return Ok(());
    }};
}
