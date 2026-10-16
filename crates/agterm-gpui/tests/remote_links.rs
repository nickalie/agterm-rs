//! The remote links' sources and `remotes.conf`'s watch on private buses: a fake `NetworkManager` and logind on the
//! sandbox's "system" bus and a fake portal network monitor on its session bus, never the machine's
//! (`support::Sandbox`). First the sources as the library starts them (what each reports, which network source is
//! picked, none without a service), then the GPUI build launched on those buses retrying its links when the network
//! comes back and when the machine wakes, and posting the remotes problems banner when `remotes.conf` turns bad on disk.

#[macro_use]
#[expect(
    dead_code,
    reason = "the sandbox serves every process test; this crate uses its bus and weston parts"
)]
mod support;

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use agterm_gpui::remote::dbus::{
    Buses, Endpoint, LOGIND, LOGIND_MANAGER, LOGIND_PATH, NETWORK_MANAGER, NETWORK_MANAGER_PATH, NETWORK_MONITOR,
    NetworkSource, PORTAL, PORTAL_PATH, Sources, start,
};
use agterm_gpui::remote::{LinkEvent, Report};
use support::{DEADLINE, Sandbox, TestResult, have, lavapipe_icd};
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, Value};

/// How long a report that must not come is waited for.
const QUIET: Duration = Duration::from_millis(300);

/// `NetworkManager`'s state the fake serves: connectivity, state, primary connection.
type ManagerState = Arc<Mutex<(u32, u32, OwnedObjectPath)>>;

/// A `NetworkManager` manager object serving `state`.
struct Manager {
    state: ManagerState,
}

impl Manager {
    fn read<T>(&self, pick: impl FnOnce(&(u32, u32, OwnedObjectPath)) -> T) -> T {
        pick(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    #[zbus(property)]
    fn connectivity(&self) -> u32 {
        self.read(|state| state.0)
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        self.read(|state| state.1)
    }

    #[zbus(property)]
    fn primary_connection(&self) -> OwnedObjectPath {
        self.read(|state| state.2.clone())
    }
}

/// The portal's network monitor serving `available`.
struct Monitor {
    available: Arc<Mutex<bool>>,
}

#[zbus::interface(name = "org.freedesktop.portal.NetworkMonitor")]
impl Monitor {
    fn get_available(&self) -> bool {
        *self.available.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn path(raw: &str) -> OwnedObjectPath {
    zbus::zvariant::ObjectPath::from_str_unchecked(raw).into()
}

/// A fake `NetworkManager` on the sandbox's system bus, down and disconnected at first.
fn network_manager(sandbox: &Sandbox) -> TestResult<(Connection, ManagerState)> {
    let state: ManagerState = Arc::new(Mutex::new((1, 20, path("/"))));
    let connection = zbus::blocking::connection::Builder::address(sandbox.system_bus_address())?
        .name(NETWORK_MANAGER)?
        .serve_at(
            NETWORK_MANAGER_PATH,
            Manager {
                state: Arc::clone(&state),
            },
        )?
        .build()?;
    Ok((connection, state))
}

/// Sets the fake `NetworkManager`'s state and announces a change, as `NetworkManager` does on any property change.
fn set_manager(connection: &Connection, state: &ManagerState, now: (u32, u32, &str)) -> TestResult {
    *state.lock().unwrap_or_else(PoisonError::into_inner) = (now.0, now.1, path(now.2));
    connection.emit_signal(
        None::<zbus::names::BusName<'_>>,
        NETWORK_MANAGER_PATH,
        "org.freedesktop.DBus.Properties",
        "PropertiesChanged",
        &(NETWORK_MANAGER, HashMap::<&str, Value<'_>>::new(), Vec::<&str>::new()),
    )?;
    Ok(())
}

/// A fake logind on the sandbox's system bus (its manager's signal is all the source reads).
fn logind(sandbox: &Sandbox) -> TestResult<Connection> {
    Ok(
        zbus::blocking::connection::Builder::address(sandbox.system_bus_address())?
            .name(LOGIND)?
            .build()?,
    )
}

fn prepare_for_sleep(logind: &Connection, start: bool) -> TestResult {
    logind.emit_signal(
        None::<zbus::names::BusName<'_>>,
        LOGIND_PATH,
        LOGIND_MANAGER,
        "PrepareForSleep",
        &(start,),
    )?;
    Ok(())
}

/// The sandbox's two buses.
fn buses(sandbox: &Sandbox) -> Buses {
    Buses {
        system: Endpoint::Address(sandbox.system_bus_address().to_owned()),
        session: Endpoint::Address(sandbox.bus_address().to_owned()),
    }
}

/// The sources started on `buses`, their reports arriving on the receiver.
fn started(buses: &Buses) -> (Sources, mpsc::Receiver<LinkEvent>) {
    let (sender, reports) = mpsc::channel();
    let report: Report = Arc::new(move |event| {
        let _ = sender.send(event);
    });
    (start(buses, &report), reports)
}

fn next(reports: &mpsc::Receiver<LinkEvent>) -> TestResult<LinkEvent> {
    Ok(reports.recv_timeout(DEADLINE)?)
}

fn quiet(reports: &mpsc::Receiver<LinkEvent>, what: &str) {
    // a timeout, or every source gone without a word
    let received = reports.recv_timeout(QUIET);
    assert!(received.is_err(), "{what}: {received:?}");
}

/// Waits for a source's thread to end (its bus gone) and gives what it says.
fn ended<T>(handle: Option<std::thread::JoinHandle<T>>) -> TestResult<T> {
    let handle = handle.ok_or("the source's thread did not start")?;
    let started = Instant::now();
    while !handle.is_finished() {
        if started.elapsed() > DEADLINE {
            return Err("the source's thread outlived its bus".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.join().map_err(|_| "the source's thread panicked".into())
}

/// Wakes the machine until the resume source answers (it subscribes on a thread of its own, with no other sign of
/// being ready); then the source is listening.
fn wait_resume_ready(logind: &Connection, reports: &mpsc::Receiver<LinkEvent>) -> TestResult {
    let started = Instant::now();
    loop {
        prepare_for_sleep(logind, false)?;
        match reports.recv_timeout(Duration::from_millis(100)) {
            Ok(LinkEvent::Resumed) => return Ok(()),
            Ok(other) => return Err(format!("not a resume: {other:?}").into()),
            Err(_) if started.elapsed() < DEADLINE => {}
            Err(error) => return Err(format!("logind's signal never reached the source: {error}").into()),
        }
    }
}

#[test]
fn network_manager_reports_each_change_and_logind_each_wake() -> TestResult {
    let sandbox = Sandbox::new()?;
    let (manager, state) = network_manager(&sandbox)?;
    let logind = logind(&sandbox)?;
    let (sources, reports) = started(&buses(&sandbox));
    assert_eq!(
        next(&reports)?,
        LinkEvent::Network { available: false },
        "the state at start: connectivity none"
    );
    set_manager(
        &manager,
        &state,
        (4, 70, "/org/freedesktop/NetworkManager/ActiveConnection/1"),
    )?;
    assert_eq!(next(&reports)?, LinkEvent::Network { available: true }, "back up");
    set_manager(
        &manager,
        &state,
        (4, 70, "/org/freedesktop/NetworkManager/ActiveConnection/1"),
    )?;
    quiet(&reports, "a change of nothing the links care about");
    set_manager(
        &manager,
        &state,
        (4, 70, "/org/freedesktop/NetworkManager/ActiveConnection/2"),
    )?;
    assert_eq!(
        next(&reports)?,
        LinkEvent::Network { available: true },
        "another primary connection: the path changed while up"
    );
    set_manager(
        &manager,
        &state,
        (0, 70, "/org/freedesktop/NetworkManager/ActiveConnection/2"),
    )?;
    quiet(&reports, "connectivity unknown with a global route is still up");
    set_manager(&manager, &state, (0, 20, "/"))?;
    assert_eq!(next(&reports)?, LinkEvent::Network { available: false }, "disconnected");
    wait_resume_ready(&logind, &reports)?;
    prepare_for_sleep(&logind, true)?;
    quiet(&reports, "going to sleep is no resume");
    prepare_for_sleep(&logind, false)?;
    assert_eq!(next(&reports)?, LinkEvent::Resumed);
    quiet(&reports, "one signal, one resume");
    drop(sandbox);
    assert_eq!(ended(sources.network)?, NetworkSource::NetworkManager);
    assert!(ended(sources.resume)?, "logind was followed");
    drop((manager, logind));
    Ok(())
}

#[test]
fn without_network_manager_the_portals_monitor_reports() -> TestResult {
    let sandbox = Sandbox::new()?;
    let available = Arc::new(Mutex::new(true));
    let portal = zbus::blocking::connection::Builder::address(sandbox.bus_address())?
        .name(PORTAL)?
        .serve_at(
            PORTAL_PATH,
            Monitor {
                available: Arc::clone(&available),
            },
        )?
        .build()?;
    let changed = |now: bool| -> TestResult {
        *available.lock().unwrap_or_else(PoisonError::into_inner) = now;
        portal.emit_signal(
            None::<zbus::names::BusName<'_>>,
            PORTAL_PATH,
            NETWORK_MONITOR,
            "changed",
            &(),
        )?;
        Ok(())
    };
    let (sources, reports) = started(&buses(&sandbox));
    assert_eq!(
        next(&reports)?,
        LinkEvent::Network { available: true },
        "the state at start"
    );
    changed(false)?;
    assert_eq!(next(&reports)?, LinkEvent::Network { available: false });
    changed(true)?;
    assert_eq!(next(&reports)?, LinkEvent::Network { available: true });
    assert!(!ended(sources.resume)?, "no logind on the system bus: no resume source");
    drop(sandbox);
    assert_eq!(ended(sources.network)?, NetworkSource::Portal);
    drop(portal);
    Ok(())
}

#[test]
fn without_a_service_the_network_reads_as_always_reachable() -> TestResult {
    let sandbox = Sandbox::new()?;
    // a portal without the network monitor counts as none
    let portal = zbus::blocking::connection::Builder::address(sandbox.bus_address())?
        .name(PORTAL)?
        .build()?;
    let (sources, reports) = started(&buses(&sandbox));
    assert_eq!(ended(sources.network)?, NetworkSource::None);
    assert!(!ended(sources.resume)?);
    quiet(&reports, "nothing to report");
    drop(portal);
    Ok(())
}

/// The `Notify` call's arguments (the freedesktop notification specification's eight).
type NotifyCall = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    HashMap<String, zbus::zvariant::OwnedValue>,
    i32,
);

/// A notification server on the sandbox's session bus that answers every `Notify` and sends its summary and body.
fn notification_server(sandbox: &Sandbox) -> TestResult<(Connection, mpsc::Receiver<(String, String)>)> {
    let connection = zbus::blocking::connection::Builder::address(sandbox.bus_address())?
        .name("org.freedesktop.Notifications")?
        .build()?;
    let (banners, received) = mpsc::channel();
    let serving = connection.clone();
    std::thread::spawn(move || {
        for message in zbus::blocking::MessageIterator::from(&serving).flatten() {
            let header = message.header();
            if header.message_type() != zbus::message::Type::MethodCall
                || header.member().map(zbus::names::MemberName::as_str) != Some("Notify")
            {
                continue;
            }
            if let Ok(call) = message.body().deserialize::<NotifyCall>() {
                let _ = banners.send((call.3, call.4));
            }
            let _ = serving.reply(&header, &(1_u32,));
        }
    });
    Ok((connection, received))
}

#[test]
fn the_app_retries_its_links_on_the_network_and_the_wake_and_follows_remotes_conf() -> TestResult {
    if !have("weston") {
        skip!("no weston");
    }
    let Some(lavapipe) = lavapipe_icd() else {
        skip!("no lavapipe ICD (set AGTERM_LAVAPIPE_ICD)");
    };
    let mut sandbox = Sandbox::new()?;
    let (manager, state) = network_manager(&sandbox)?;
    let logind = logind(&sandbox)?;
    let (notifications, banner) = notification_server(&sandbox)?;
    sandbox.set_env(
        "AGTERM_LOG",
        "info,agterm_gpui::remotes_watcher=debug,agterm_gpui::notifications=debug",
    );
    sandbox.start_weston()?;
    sandbox.use_software_rendering(&lavapipe);
    let primary = sandbox.start_gpui_primary("primary.log")?;
    sandbox.wait_log("primary.log", "remote links: following NetworkManager")?;
    sandbox.wait_log("primary.log", "remotes.conf: following")?;
    sandbox.wait_log("primary.log", "remote links: following logind's sleep signal")?;
    let retries = |sandbox: &Sandbox| {
        sandbox
            .log("primary.log")
            .matches("retrying the remote links now")
            .count()
    };
    assert_eq!(retries(&sandbox), 0, "the state at start retries nothing");
    set_manager(
        &manager,
        &state,
        (4, 70, "/org/freedesktop/NetworkManager/ActiveConnection/1"),
    )?;
    support::wait_for(|| (retries(&sandbox) == 1).then_some(()))
        .map_err(|error| format!("{error}\n{}", sandbox.log("primary.log")))?;
    prepare_for_sleep(&logind, false)?;
    support::wait_for(|| (retries(&sandbox) == 2).then_some(()))
        .map_err(|error| format!("{error}\n{}", sandbox.log("primary.log")))?;
    // `remotes.conf` turning unreadable on disk (a directory now) posts the problems banner, with no reload
    std::fs::create_dir_all(sandbox.path("state/config/remotes.conf"))?;
    let (summary, body) = banner
        .recv_timeout(DEADLINE)
        .map_err(|error| format!("no banner: {error}\n{}", sandbox.log("primary.log")))?;
    assert_eq!(
        (summary.as_str(), body.as_str()),
        ("Remotes", "1 issue in remotes.conf")
    );
    sandbox.terminate(primary)?;
    let (status, _) = sandbox.wait_exit(primary, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(0), "{status:?}");
    drop((manager, logind, notifications));
    Ok(())
}
