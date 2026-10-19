//! Desktop notifications on a private session bus: a `dbus-daemon` of the test's own (no service directories, so
//! nothing on it can be activated) with a fake `org.freedesktop.Notifications` server, never the user's bus. The
//! banner task connects to the session bus, `DBUS_SESSION_BUS_ADDRESS`, so the scenario runs in a child of this test
//! binary whose environment holds the private bus alone.
//!
//! The scenario: a repeat from one pane replaces its banner (`replaces_id`), another pane gets its own, a click on a
//! banner (`ActionInvoked` with the `default` key) reveals its identity with the activation token the server sent
//! before it (`ActivationToken`), a banner the server closed (`NotificationClosed`) is forgotten, a failed repeat keeps
//! the banner still on screen, signals from anyone but the server count for nothing, a clear withdraws the
//! session's banners still on screen (`CloseNotification`), and a server that took over the name (the old one
//! restarted) never gets the old one's ids: no `replaces_id` and no `CloseNotification` of them.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agterm_core::{NotificationEffect, PaneRole, notification_identity};
use agterm_gpui::notifications::{BannerSink as _, Click, Desktop, deliver, run};
use uuid::Uuid;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

/// Names the private bus the child runs on; the child refuses any other.
const PRIVATE_BUS: &str = "AGTERM_GPUI_TEST_PRIVATE_BUS";
const PATH: &str = "/org/freedesktop/Notifications";
const DEADLINE: Duration = Duration::from_secs(20);
const APP_ID: &str = "com.nickalie.ngterm.test";
/// The body the fake server refuses to post.
const FAILING: &str = "fail";

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

/// The notification server's bus name.
const SERVER_NAME: &str = "org.freedesktop.Notifications";

/// A fake server recording into `calls`, serving on the bus at `address` under the server's name.
fn serve(address: &str, calls: &Arc<Mutex<Vec<Call>>>) -> TestResult<zbus::blocking::Connection> {
    Ok(zbus::blocking::connection::Builder::address(address)?
        .serve_at(
            PATH,
            Server {
                calls: Arc::clone(calls),
                last: 0,
            },
        )?
        .name(SERVER_NAME)?
        .build()?)
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
    fn notify(&mut self, request: Notify) -> zbus::fdo::Result<u32> {
        if request.body == FAILING {
            return Err(zbus::fdo::Error::Failed("refused".to_owned()));
        }
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
        Ok(id)
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
    ) -> zbus::fdo::Result<u32> {
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

    #[zbus(signal)]
    async fn activation_token(emitter: &SignalEmitter<'_>, id: u32, activation_token: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn notification_closed(emitter: &SignalEmitter<'_>, id: u32, reason: u32) -> zbus::Result<()>;
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

/// What a scenario step returns: a failed bus call fails the scenario.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The scenario's parts: the fake server on the private bus, what it was asked, the banner task's sink and clicks, and
/// the three panes' identities of one session.
struct Scenario {
    address: String,
    calls: Arc<Mutex<Vec<Call>>>,
    server: zbus::blocking::Connection,
    sink: Desktop,
    clicks: std::sync::mpsc::Receiver<Click>,
    window: Uuid,
    session: Uuid,
    main: String,
    split: String,
    overlay: String,
}

impl Scenario {
    /// The fake server serving on the bus at `address`, and the banner task running beside it.
    fn start(address: String) -> TestResult<Self> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let server = serve(&address, &calls)?;
        let (sink, queue) = Desktop::new();
        let (clicked, clicks) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            zbus::block_on(run(queue, APP_ID.to_owned(), move |click| {
                let _ = clicked.send(click);
            }));
        });
        let (window, session) = (Uuid::new_v4(), Uuid::new_v4());
        Ok(Self {
            address,
            calls,
            server,
            sink,
            clicks,
            window,
            session,
            main: notification_identity(window, session, PaneRole::Main),
            split: notification_identity(window, session, PaneRole::Split),
            overlay: notification_identity(window, session, PaneRole::Overlay),
        })
    }

    /// The fake server's interface, whose emitter sends its signals.
    fn interface(&self) -> TestResult<zbus::blocking::object_server::InterfaceRef<Server>> {
        Ok(self.server.object_server().interface::<_, Server>(PATH)?)
    }

    /// The next click, or a failure.
    fn click(&self) -> TestResult<Click> {
        Ok(self.clicks.recv_timeout(DEADLINE)?)
    }
}

/// Sends a signal of the fake server's.
fn emit(signal: impl std::future::Future<Output = zbus::Result<()>>) -> TestResult {
    Ok(zbus::block_on(signal)?)
}

#[test]
#[ignore = "run by banners_reach_a_private_bus_replace_click_and_withdraw, on its private bus"]
fn on_the_private_bus() -> TestResult {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    // only ever on the private bus the parent test started and named: never the user's
    if address.is_empty() || std::env::var(PRIVATE_BUS).ok().as_deref() != Some(address.as_str()) {
        eprintln!("skipped: not on the parent test's private bus");
        return Ok(());
    }
    let scenario = Scenario::start(address)?;
    repeats_replace_their_banner(&scenario);
    a_click_carries_the_servers_token(&scenario)?;
    refused_and_closed_banners(&scenario)?;

    let clear = NotificationEffect::Clear {
        window: scenario.window,
        session: scenario.session,
    };
    deliver(&scenario.sink, &[clear]);
    let seen = wait_for(&scenario.calls, "the withdrawal", |calls| {
        calls.contains(&Call::Close(2))
    });
    let closes: Vec<_> = seen.iter().filter(|call| matches!(call, Call::Close(_))).collect();
    assert_eq!(
        closes,
        [&Call::Close(2)],
        "only the split's banner is still up, kept through its refused repeat"
    );
    assert_eq!(notifies(&seen).len(), 5, "a clear posts nothing");
    a_restarted_server_gets_none_of_the_old_ids(&scenario)
}

/// The server's name passes to another server that numbers its banners from 1 again (the old one restarted without
/// closing its banners): the new one gets none of the old one's ids, neither as a repeat's `replaces_id` nor in a
/// withdrawal, and its signals are the ones that count.
fn a_restarted_server_gets_none_of_the_old_ids(scenario: &Scenario) -> TestResult {
    let Scenario {
        calls,
        sink,
        main,
        split,
        overlay,
        ..
    } = scenario;
    sink.send(main, "t", "six");
    sink.send(split, "t", "seven");
    let before = wait_for(calls, "the banners before the restart", |calls| {
        notifies(calls).len() == 7
    });
    assert_eq!(
        notifies(&before)[5..],
        [(0, 5, "six".to_owned()), (0, 6, "seven".to_owned())]
    );

    scenario.server.release_name(SERVER_NAME)?;
    let restarted = Arc::new(Mutex::new(Vec::new()));
    let successor = serve(&scenario.address, &restarted)?;
    sink.send(main, "t", "eight");
    deliver(
        sink,
        &[NotificationEffect::Clear {
            window: scenario.window,
            session: scenario.session,
        }],
    );
    sink.send(overlay, "t", "nine");
    sink.send(split, "t", "ten");
    let seen = wait_for(&restarted, "the banners after the restart", |calls| {
        notifies(calls).len() == 3
    });
    let summary: Vec<_> = seen
        .iter()
        .map(|call| match call {
            Call::Notify { replaces, id, body, .. } => format!("notify {replaces} {id} {body}"),
            Call::Close(id) => format!("close {id}"),
        })
        .collect();
    assert_eq!(
        summary,
        ["notify 0 1 eight", "close 1", "notify 0 2 nine", "notify 0 3 ten"],
        "the new server replaces and withdraws none of the old one's banners, only its own"
    );
    assert_eq!(
        calls.lock().map(|calls| calls.len()).unwrap_or_default(),
        before.len(),
        "the old server gets nothing more"
    );

    // the old server's click counts for nothing now; the new one's does
    let old = scenario.interface()?;
    emit(Server::action_invoked(old.signal_emitter(), 3, "default"))?;
    let new = successor.object_server().interface::<_, Server>(PATH)?;
    emit(Server::action_invoked(new.signal_emitter(), 3, "default"))?;
    assert_eq!(
        scenario.click()?,
        Click {
            identity: split.clone(),
            token: None
        },
        "the new server's click reveals its banner's pane; the old server's counts for nothing"
    );
    Ok(())
}

/// A banner the server can click, under the app's desktop entry; a repeat from one pane replaces its banner, another
/// pane gets its own.
fn repeats_replace_their_banner(scenario: &Scenario) {
    let Scenario {
        calls,
        sink,
        main,
        split,
        ..
    } = scenario;
    sink.send(main, "t", "one");
    let seen = wait_for(calls, "the first banner", |calls| notifies(calls).len() == 1);
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
    sink.send(main, "t", "two");
    sink.send(split, "t", "three");
    let seen = wait_for(calls, "the repeat and the split's banner", |calls| {
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
}

/// A click reveals the pane its banner names, with the activation token the server sent before it.
fn a_click_carries_the_servers_token(scenario: &Scenario) -> TestResult {
    let interface = scenario.interface()?;
    let emitter = interface.signal_emitter();
    emit(Server::activation_token(emitter, 1, "token-1"))?;
    emit(Server::action_invoked(emitter, 1, "default"))?;
    assert_eq!(
        scenario.click()?,
        Click {
            identity: scenario.main.clone(),
            token: Some("token-1".to_owned())
        },
        "a click reveals the pane its banner names, with the server's activation token"
    );
    Ok(())
}

/// A repeat the server refuses leaves the split's banner known; a clicked banner and one the server closed are
/// forgotten; a signal from a peer that is not the server counts for nothing.
fn refused_and_closed_banners(scenario: &Scenario) -> TestResult {
    let Scenario {
        calls,
        sink,
        main,
        split,
        overlay,
        ..
    } = scenario;
    sink.send(split, "t", FAILING);
    sink.send(main, "t", "four");
    sink.send(overlay, "t", "five");
    let seen = wait_for(calls, "the banners after the refused one", |calls| {
        notifies(calls).len() == 5
    });
    assert_eq!(
        notifies(&seen)[3..],
        [(0, 3, "four".to_owned()), (0, 4, "five".to_owned())],
        "a clicked banner is forgotten: its pane's next banner replaces nothing"
    );

    // the server closed the main pane's banner: a click on it after reveals nothing; nor does a peer's signal; the
    // overlay's click, sent last, is the next one seen
    let interface = scenario.interface()?;
    let emitter = interface.signal_emitter();
    emit(Server::notification_closed(emitter, 3, 2))?;
    emit(Server::action_invoked(emitter, 3, "default"))?;
    let peer = zbus::blocking::connection::Builder::address(scenario.address.as_str())?.build()?;
    peer.emit_signal(
        None::<&str>,
        PATH,
        "org.freedesktop.Notifications",
        "ActionInvoked",
        &(2_u32, "default"),
    )?;
    emit(Server::action_invoked(emitter, 4, "default"))?;
    assert_eq!(
        scenario.click()?,
        Click {
            identity: overlay.clone(),
            token: None
        },
        "a closed banner and a peer's signal reveal nothing; a click without a token carries none"
    );
    Ok(())
}
