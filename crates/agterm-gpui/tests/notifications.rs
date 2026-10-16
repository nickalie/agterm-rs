//! Desktop notifications on a private session bus: a `dbus-daemon` of the test's own (no service directories, so
//! nothing on it can be activated) with a fake `org.freedesktop.Notifications` server, never the user's bus. The
//! banner task reaches the bus `notify-rust` connects to, `DBUS_SESSION_BUS_ADDRESS`, so the scenario runs in a child
//! of this test binary whose environment holds the private bus alone.
//!
//! The scenario: a repeat from one pane replaces its banner (`replaces_id`), another pane gets its own, a click on a
//! banner (`ActionInvoked` with the `default` key) reveals its identity, and a clear withdraws the session's banners
//! still on screen (`CloseNotification`).

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agterm_core::{NotificationEffect, PaneRole, notification_identity};
use agterm_gpui::notifications::{BannerSink as _, Desktop, deliver, run};
use uuid::Uuid;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

/// Names the private bus the child runs on; the child refuses any other.
const PRIVATE_BUS: &str = "AGTERM_GPUI_TEST_PRIVATE_BUS";
const PATH: &str = "/org/freedesktop/Notifications";
const DEADLINE: Duration = Duration::from_secs(20);
const APP_ID: &str = "io.github.melonamin.agterm.test";

/// A session bus that listens on `socket` only, activates nothing and lets every client talk to every other.
fn bus_config(socket: &Path) -> String {
    format!(
        r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
        socket.display()
    )
}

fn dbus_daemon() -> Option<std::path::PathBuf> {
    ["/usr/bin/dbus-daemon", "/bin/dbus-daemon"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
}

#[test]
fn banners_reach_a_private_bus_replace_click_and_withdraw() {
    let Some(daemon) = dbus_daemon() else {
        eprintln!("skipped: no dbus-daemon to run a private session bus on");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("bus.conf");
    std::fs::write(&config, bus_config(&dir.path().join("bus"))).unwrap();
    let mut bus = Command::new(daemon)
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--print-address=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(bus.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim().to_owned();
    assert!(address.starts_with("unix:"), "{address}");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "on_the_private_bus",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", dir.path())
        .env("DBUS_SESSION_BUS_ADDRESS", &address)
        .env(PRIVATE_BUS, &address)
        .status();
    let _ = bus.kill();
    let _ = bus.wait();
    assert!(status.unwrap().success(), "the scenario failed on the private bus");
}

/// What the fake server was asked, in order.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Notify {
        app: String,
        replaces: u32,
        id: u32,
        body: String,
        actions: Vec<String>,
        desktop_entry: Option<String>,
    },
    Close(u32),
}

/// A notification server that records its calls and numbers banners from 1.
struct Server {
    calls: Arc<Mutex<Vec<Call>>>,
    last: u32,
}

/// The `Notify` call as the server reads it.
struct Notify {
    app: String,
    replaces: u32,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
}

impl Server {
    fn notify(&mut self, request: Notify) -> u32 {
        let id = if request.replaces == 0 {
            self.last += 1;
            self.last
        } else {
            request.replaces
        };
        let desktop_entry = request
            .hints
            .get("desktop-entry")
            .and_then(|value| String::try_from(value.try_clone().ok()?).ok());
        record(
            &self.calls,
            Call::Notify {
                app: request.app,
                replaces: request.replaces,
                id,
                body: request.body,
                actions: request.actions,
                desktop_entry,
            },
        );
        id
    }
}

fn record(calls: &Mutex<Vec<Call>>, call: Call) {
    if let Ok(mut calls) = calls.lock() {
        calls.push(call);
    }
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    #[zbus(name = "Notify")]
    #[expect(
        clippy::too_many_arguments,
        reason = "the freedesktop `Notify` method's eight arguments"
    )]
    fn notify_method(
        &mut self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        drop((app_icon, summary, expire_timeout));
        self.notify(Notify {
            app: app_name,
            replaces: replaces_id,
            body,
            actions,
            hints,
        })
    }

    fn close_notification(&mut self, id: u32) {
        record(&self.calls, Call::Close(id));
    }

    #[zbus(signal)]
    async fn action_invoked(emitter: &SignalEmitter<'_>, id: u32, action_key: &str) -> zbus::Result<()>;
}

/// Waits until `done` holds of the recorded calls, or fails.
fn wait_for(calls: &Mutex<Vec<Call>>, what: &str, done: impl Fn(&[Call]) -> bool) -> Vec<Call> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let seen = calls.lock().map(|calls| calls.clone()).unwrap_or_default();
        if done(&seen) {
            return seen;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}: {seen:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn notifies(calls: &[Call]) -> Vec<(u32, u32, String)> {
    calls
        .iter()
        .filter_map(|call| match call {
            Call::Notify { replaces, id, body, .. } => Some((*replaces, *id, body.clone())),
            Call::Close(_) => None,
        })
        .collect()
}

#[test]
#[ignore = "run by banners_reach_a_private_bus_replace_click_and_withdraw, on its private bus"]
fn on_the_private_bus() {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    // only ever on the private bus the parent test started and named: never the user's
    if address.is_empty() || std::env::var(PRIVATE_BUS).ok().as_deref() != Some(address.as_str()) {
        eprintln!("skipped: not on the parent test's private bus");
        return;
    }
    let calls = Arc::new(Mutex::new(Vec::new()));
    let server = zbus::blocking::connection::Builder::address(address.as_str())
        .unwrap()
        .serve_at(
            PATH,
            Server {
                calls: Arc::clone(&calls),
                last: 0,
            },
        )
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .build()
        .unwrap();

    let (sink, queue) = Desktop::new();
    let (clicked, clicks) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        zbus::block_on(run(queue, APP_ID.to_owned(), move |identity| {
            let _ = clicked.send(identity);
        }));
    });
    let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
    let main = notification_identity(window, session, PaneRole::Main);
    let split = notification_identity(window, session, PaneRole::Split);

    sink.send(&main, "t", "one");
    let seen = wait_for(&calls, "the first banner", |calls| notifies(calls).len() == 1);
    assert_eq!(
        seen[0],
        Call::Notify {
            app: "agterm".to_owned(),
            replaces: 0,
            id: 1,
            body: "one".to_owned(),
            actions: vec!["default".to_owned(), String::new()],
            desktop_entry: Some(APP_ID.to_owned()),
        },
        "a banner the server can click, under the app's desktop entry"
    );
    sink.send(&main, "t", "two");
    sink.send(&split, "t", "three");
    let seen = wait_for(&calls, "the repeat and the split's banner", |calls| {
        notifies(calls).len() == 3
    });
    assert_eq!(
        notifies(&seen),
        [
            (0, 1, "one".to_owned()),
            (1, 1, "two".to_owned()),
            (0, 2, "three".to_owned())
        ],
        "a repeat from one pane replaces its banner; another pane gets its own"
    );

    let interface = server.object_server().interface::<_, Server>(PATH).unwrap();
    zbus::block_on(Server::action_invoked(interface.signal_emitter(), 1, "default")).unwrap();
    assert_eq!(
        clicks.recv_timeout(DEADLINE).unwrap(),
        main,
        "a click reveals the pane its banner names"
    );

    deliver(&sink, &[NotificationEffect::Clear { window, session }]);
    let seen = wait_for(&calls, "the withdrawal", |calls| calls.contains(&Call::Close(2)));
    assert!(
        !seen.contains(&Call::Close(1)),
        "the clicked banner is gone already: {seen:?}"
    );
    assert_eq!(notifies(&seen).len(), 3, "a clear posts nothing");
}
