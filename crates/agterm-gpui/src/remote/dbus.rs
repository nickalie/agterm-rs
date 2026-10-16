//! Linux's link sources over `zbus`, each on a thread of its own that connects, picks its source and follows it for
//! the app's life (the thread ends with its bus):
//!
//! - **Network**: `NetworkManager` on the system bus when it owns its name (what gio's `NetworkMonitor` reads on a
//!   desktop): every `PropertiesChanged` of its manager re-reads `Connectivity`, `State` and `PrimaryConnection`, and a
//!   change of the reachability or of the primary connection (a path change while up, as upstream's path monitor
//!   reports one) is reported. Without it, the desktop portal's `org.freedesktop.portal.NetworkMonitor` on the session
//!   bus (`GetAvailable` at start and on each `changed`), which works on any desktop running xdg-desktop-portal.
//!   Without either the network reads as always reachable: nothing is reported (logged).
//! - **Resume**: logind's `PrepareForSleep(false)` on the system bus; without logind (or elogind) owning
//!   `org.freedesktop.login1` no resume is reported (logged).
//!
//! The sources subscribe before they read, so no change between the first read and the subscription is lost. They
//! never activate a service: a name nobody owns counts as absent.

use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;

use super::{LinkEvent, Report};

/// `NetworkManager`'s bus name and manager interface, and its object.
pub const NETWORK_MANAGER: &str = "org.freedesktop.NetworkManager";
pub const NETWORK_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
/// The desktop portal and its network monitor.
pub const PORTAL: &str = "org.freedesktop.portal.Desktop";
pub const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
pub const NETWORK_MONITOR: &str = "org.freedesktop.portal.NetworkMonitor";
/// logind's bus name, manager object and interface.
pub const LOGIND: &str = "org.freedesktop.login1";
pub const LOGIND_PATH: &str = "/org/freedesktop/login1";
pub const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
/// How long a call may take: these run off the main thread, a slow bus delays only the first report.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// `NMConnectivityState`.
const CONNECTIVITY_UNKNOWN: u32 = 0;
const CONNECTIVITY_NONE: u32 = 1;
/// `NMState`'s `NM_STATE_CONNECTED_SITE`: a default route, whatever lies past it.
const STATE_CONNECTED_SITE: u32 = 60;

/// Where a bus is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    /// The desktop's: `DBUS_SYSTEM_BUS_ADDRESS` / `DBUS_SESSION_BUS_ADDRESS`, else the well-known places.
    Desktop,
    /// A bus at this address (a test's own daemon).
    Address(String),
    /// No such bus.
    Absent,
}

/// The system bus and the session bus the sources connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buses {
    pub system: Endpoint,
    pub session: Endpoint,
}

impl Buses {
    /// The desktop's buses.
    #[must_use]
    pub fn desktop() -> Self {
        Self {
            system: Endpoint::Desktop,
            session: Endpoint::Desktop,
        }
    }
}

/// Which source reports the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSource {
    NetworkManager,
    Portal,
    /// Neither: the network reads as always reachable.
    None,
}

/// The sources' threads: each ends with its bus and says what it followed.
pub struct Sources {
    /// The network's source, once its bus closed (or at once, without one).
    pub network: Option<std::thread::JoinHandle<NetworkSource>>,
    /// Whether logind was followed, once the system bus closed (or at once, without logind).
    pub resume: Option<std::thread::JoinHandle<bool>>,
}

/// Starts the network and the resume sources on `buses`, each reporting to `report`.
pub fn start(buses: &Buses, report: &Report) -> Sources {
    let (system, session, network_report) = (buses.system.clone(), buses.session.clone(), report.clone());
    let network = spawn("agterm-network", move || {
        follow_network(&system, &session, &*network_report)
    });
    let (system, resume_report) = (buses.system.clone(), report.clone());
    let resume = spawn("agterm-resume", move || follow_resume(&system, &*resume_report));
    Sources { network, resume }
}

fn spawn<T: Send + 'static>(
    name: &str,
    run: impl FnOnce() -> T + Send + 'static,
) -> Option<std::thread::JoinHandle<T>> {
    match std::thread::Builder::new().name(name.to_owned()).spawn(run) {
        Ok(handle) => Some(handle),
        Err(error) => {
            tracing::error!(%error, name, "remote links: the source's thread did not start");
            None
        }
    }
}

/// A connection to the bus at `endpoint`; `None` (logged) without one.
fn connect(endpoint: &Endpoint, which: &str) -> Option<Connection> {
    let builder = match endpoint {
        Endpoint::Desktop if which == "system" => zbus::blocking::connection::Builder::system(),
        Endpoint::Desktop => zbus::blocking::connection::Builder::session(),
        Endpoint::Address(address) => zbus::blocking::connection::Builder::address(address.as_str()),
        Endpoint::Absent => return None,
    };
    match builder
        .map(|builder| builder.method_timeout(CALL_TIMEOUT))
        .and_then(zbus::blocking::connection::Builder::build)
    {
        Ok(connection) => Some(connection),
        Err(error) => {
            tracing::info!(%error, bus = which, "remote links: no bus");
            None
        }
    }
}

/// Whether `name` has an owner on `bus` (a name nobody owns is absent: nothing is activated).
fn has_owner(bus: &Connection, name: &str) -> bool {
    let owned = zbus::names::BusName::try_from(name)
        .map_err(zbus::Error::from)
        .and_then(|name| Ok(zbus::blocking::fdo::DBusProxy::new(bus)?.name_has_owner(name)?));
    owned.unwrap_or_else(|error| {
        tracing::debug!(%error, name, "remote links: cannot ask for the name's owner");
        false
    })
}

fn proxy(
    bus: &Connection,
    destination: &'static str,
    path: &'static str,
    interface: &'static str,
) -> zbus::Result<Proxy<'static>> {
    zbus::blocking::proxy::Builder::new(bus)
        .destination(destination)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
}

/// Picks the network's source and follows it until its bus closes.
fn follow_network(system: &Endpoint, session: &Endpoint, report: &dyn Fn(LinkEvent)) -> NetworkSource {
    if let Some(bus) = connect(system, "system").filter(|bus| has_owner(bus, NETWORK_MANAGER)) {
        if let Err(error) = follow_network_manager(&bus, report) {
            tracing::warn!(%error, "remote links: cannot follow NetworkManager");
        }
        return NetworkSource::NetworkManager;
    }
    if let Some(bus) = connect(session, "session").filter(|bus| has_owner(bus, PORTAL)) {
        match follow_portal(&bus, report) {
            Ok(()) => return NetworkSource::Portal,
            Err(error) => tracing::info!(%error, "remote links: the portal has no network monitor"),
        }
    }
    tracing::info!(
        "remote links: neither NetworkManager nor the portal's network monitor; the network reads as always \
         reachable and the links retry on their backoff alone"
    );
    NetworkSource::None
}

/// `NetworkManager`'s state as far as the links care.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagerState {
    available: bool,
    primary: OwnedObjectPath,
}

/// Whether `NetworkManager`'s `connectivity` and `state` let anything past the machine be reached: connectivity none is
/// down, a portal, limited or full one up; unknown (the connectivity check off or not run yet) is up with a default
/// route.
#[must_use]
pub fn reachable(connectivity: u32, state: u32) -> bool {
    match connectivity {
        CONNECTIVITY_NONE => false,
        CONNECTIVITY_UNKNOWN => state >= STATE_CONNECTED_SITE,
        _ => true,
    }
}

fn manager_state(manager: &Proxy<'_>) -> zbus::Result<ManagerState> {
    let connectivity: u32 = manager.get_property("Connectivity")?;
    let state: u32 = manager.get_property("State")?;
    let primary: OwnedObjectPath = manager.get_property("PrimaryConnection")?;
    Ok(ManagerState {
        available: reachable(connectivity, state),
        primary,
    })
}

fn follow_network_manager(bus: &Connection, report: &dyn Fn(LinkEvent)) -> zbus::Result<()> {
    let changes = proxy(bus, NETWORK_MANAGER, NETWORK_MANAGER_PATH, PROPERTIES)?
        .receive_signal_with_args("PropertiesChanged", &[(0, NETWORK_MANAGER)])?;
    let manager = proxy(bus, NETWORK_MANAGER, NETWORK_MANAGER_PATH, NETWORK_MANAGER)?;
    let mut last = manager_state(&manager)?;
    tracing::info!(available = last.available, "remote links: following NetworkManager");
    report(LinkEvent::Network {
        available: last.available,
    });
    for _ in changes {
        match manager_state(&manager) {
            Ok(now) if now != last => {
                report(LinkEvent::Network {
                    available: now.available,
                });
                last = now;
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(%error, "remote links: an unreadable NetworkManager state"),
        }
    }
    tracing::debug!("remote links: NetworkManager's signals ended");
    Ok(())
}

fn follow_portal(bus: &Connection, report: &dyn Fn(LinkEvent)) -> zbus::Result<()> {
    let monitor = proxy(bus, PORTAL, PORTAL_PATH, NETWORK_MONITOR)?;
    let changes = monitor.receive_signal("changed")?;
    let available: bool = monitor.call("GetAvailable", &())?;
    tracing::info!("remote links: following the portal's network monitor");
    report(LinkEvent::Network { available });
    for _ in changes {
        match monitor.call("GetAvailable", &()) {
            Ok(available) => report(LinkEvent::Network { available }),
            Err(error) => tracing::debug!(%error, "remote links: the portal's network monitor did not answer"),
        }
    }
    tracing::debug!("remote links: the portal's signals ended");
    Ok(())
}

/// Follows logind's sleep signal until the system bus closes; whether logind was there.
fn follow_resume(system: &Endpoint, report: &dyn Fn(LinkEvent)) -> bool {
    let Some(bus) = connect(system, "system").filter(|bus| has_owner(bus, LOGIND)) else {
        tracing::info!("remote links: no logind; the links are not retried on resume");
        return false;
    };
    let signals =
        proxy(&bus, LOGIND, LOGIND_PATH, LOGIND_MANAGER).and_then(|manager| manager.receive_signal("PrepareForSleep"));
    let signals = match signals {
        Ok(signals) => signals,
        Err(error) => {
            tracing::warn!(%error, "remote links: cannot follow logind; the links are not retried on resume");
            return true;
        }
    };
    tracing::info!("remote links: following logind's sleep signal");
    for signal in signals {
        if resumed(&signal) {
            report(LinkEvent::Resumed);
        }
    }
    tracing::debug!("remote links: logind's signals ended");
    true
}

/// Whether a `PrepareForSleep` signal says the machine woke (`false`) rather than goes to sleep.
#[must_use]
pub fn resumed(signal: &zbus::Message) -> bool {
    matches!(signal.body().deserialize::<(bool,)>(), Ok((false,)))
}

#[cfg(test)]
mod tests;
