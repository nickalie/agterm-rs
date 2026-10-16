//! Linux's last network source, without `NetworkManager` and without the portal's network monitor: the kernel's
//! default routes over rtnetlink, as gio's netlink `NetworkMonitor` reads them (what the GTK build's gio fell back to
//! without `NetworkManager`, a bare compositor on systemd-networkd or iwd for one). It runs on the network source's
//! thread for the app's life and needs no privilege: the route groups are open to any process.
//!
//! - **Available** while an IPv4 or an IPv6 default route reaches a nexthop (a gateway, an output interface or
//!   multipath nexthops); an unreachable, blackhole, prohibit or throw route reaches nothing. Each default route is
//!   keyed as gio keys it (table, metric, output interface), what survives an `ip route replace`.
//! - **Reported**: the state at start once the dump at start ends, then each time a family's first default route comes
//!   or its last one goes (gio's `network-changed`, which the GTK build retried on), the other family's routes
//!   notwithstanding.
//! - **Overflow**: a socket that overflowed may have missed a change, so the routes are dumped again a second later and
//!   the dump's routes replace the followed ones (a flip between the two reports).
//! - **Replace**: the kernel replaces an IPv4 route in place (`ip route replace` on another interface) with one new
//!   route message flagged replace and no deletion, so the replaced key would linger; the routes are dumped again a
//!   second later too, once no dump is being read. gio keeps the stale key instead.

use std::collections::HashSet;
use std::io;
use std::time::Duration;

use netlink_packet_core::{NLM_F_DUMP, NLM_F_REPLACE, NLM_F_REQUEST, NetlinkHeader, NetlinkMessage, NetlinkPayload};
use netlink_packet_route::route::{RouteAttribute, RouteMessage, RouteType};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};
use netlink_sys::protocols::NETLINK_ROUTE;
use netlink_sys::{Socket, SocketAddr};

use super::LinkEvent;

/// Where the default routes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routes {
    /// The kernel's, over rtnetlink.
    Kernel,
    /// None: a test's sources read their private buses only.
    Absent,
}

/// The socket's receive buffer, gio's: a burst of route changes then overflows less often.
const RECEIVE_BUFFER: usize = 1024 * 1024;
/// The largest datagram read: a dump's are capped by the kernel near 32 KiB (gio's bound).
const DATAGRAM: usize = 64 * 1024;
/// How long after an overflow the routes are dumped again (gio's).
const REDUMP_AFTER: Duration = Duration::from_secs(1);
/// A netlink message header's length: anything shorter ends a datagram.
const HEADER: usize = 16;

/// A route's address family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    V4,
    V6,
}

impl Family {
    fn index(self) -> usize {
        match self {
            Self::V4 => 0,
            Self::V6 => 1,
        }
    }
}

/// A default route as gio keys it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RouteKey {
    pub table: u32,
    pub metric: u32,
    pub oif: u32,
}

/// A default route the kernel added or deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteChange {
    Added(Family, RouteKey),
    /// Added in place of a route the kernel names nowhere, which may be another key.
    Replaced(Family, RouteKey),
    Removed(Family, RouteKey),
}

/// The default routes followed, per family, and those of the dump being read.
#[derive(Debug, Default)]
pub struct DefaultRoutes {
    live: [HashSet<RouteKey>; 2],
    dump: Option<[HashSet<RouteKey>; 2]>,
    /// A default route was replaced since the last dump started: the key it replaced may still be followed.
    stale: bool,
}

impl DefaultRoutes {
    /// Whether a default route of either family is there.
    #[must_use]
    pub fn available(&self) -> bool {
        self.live.iter().any(|routes| !routes.is_empty())
    }

    /// Whether a dump is being read.
    #[must_use]
    pub fn dumping(&self) -> bool {
        self.dump.is_some()
    }

    /// Whether a dump is due because a default route was replaced, none being read now.
    #[must_use]
    pub fn redump_due(&self) -> bool {
        self.stale && !self.dumping()
    }

    /// A dump starts: the routes it lists collect apart until it ends.
    pub fn begin_dump(&mut self) {
        self.dump = Some(Default::default());
        self.stale = false;
    }

    /// Applies a route message; whether a family's default route came or went.
    pub fn apply(&mut self, change: RouteChange) -> bool {
        match change {
            RouteChange::Replaced(family, key) => {
                self.stale = true;
                self.apply(RouteChange::Added(family, key))
            }
            RouteChange::Added(family, key) => {
                if let Some(dump) = &mut self.dump {
                    dump[family.index()].insert(key);
                    return false;
                }
                let routes = &mut self.live[family.index()];
                let came = routes.is_empty();
                routes.insert(key);
                came
            }
            RouteChange::Removed(family, key) => {
                // a deletion is the kernel's own event and may come in the middle of a dump, which it then also leaves
                if let Some(dump) = &mut self.dump {
                    dump[family.index()].remove(&key);
                }
                let routes = &mut self.live[family.index()];
                routes.remove(&key) && routes.is_empty()
            }
        }
    }

    /// The dump ended: its routes are the followed ones now; whether a family's presence flipped.
    pub fn finish_dump(&mut self) -> bool {
        let Some(dump) = self.dump.take() else {
            return false;
        };
        let flipped = self
            .live
            .iter()
            .zip(&dump)
            .any(|(live, dumped)| live.is_empty() != dumped.is_empty());
        self.live = dump;
        flipped
    }
}

/// The default route a route message names, if it names one that reaches a nexthop.
#[must_use]
pub fn default_route(route: &RouteMessage) -> Option<(Family, RouteKey)> {
    let family = match route.header.address_family {
        AddressFamily::Inet => Family::V4,
        AddressFamily::Inet6 => Family::V6,
        _ => return None,
    };
    let reaches = !matches!(
        route.header.kind,
        RouteType::Unreachable | RouteType::BlackHole | RouteType::Prohibit | RouteType::Throw
    );
    if route.header.destination_prefix_length != 0 || !reaches {
        return None;
    }
    let mut key = RouteKey {
        table: route.header.table.into(),
        metric: 0,
        oif: 0,
    };
    let mut nexthop = false;
    for attribute in &route.attributes {
        match attribute {
            RouteAttribute::Gateway(_) | RouteAttribute::MultiPath(_) => nexthop = true,
            RouteAttribute::Oif(oif) => {
                key.oif = *oif;
                nexthop = true;
            }
            RouteAttribute::Priority(metric) => key.metric = *metric,
            RouteAttribute::Table(table) => key.table = *table,
            _ => {}
        }
    }
    nexthop.then_some((family, key))
}

/// What one datagram did to the routes.
#[derive(Debug, Default, PartialEq, Eq)]
struct Read {
    /// A family's presence flipped.
    flipped: bool,
    /// The socket overflowed: dump again.
    overflowed: bool,
}

/// Applies one datagram's messages to `routes`.
fn read_datagram(datagram: &[u8], routes: &mut DefaultRoutes) -> Read {
    let mut read = Read::default();
    let mut rest = datagram;
    while rest.len() >= HEADER {
        let length = rest
            .get(..4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .and_then(|bytes| usize::try_from(u32::from_ne_bytes(bytes)).ok())
            .unwrap_or(0);
        if length < HEADER || length > rest.len() {
            tracing::debug!(length, "remote links: a truncated netlink message");
            break;
        }
        match NetlinkMessage::<RouteNetlinkMessage>::deserialize(&rest[..length]) {
            Ok(message) => apply_message(message.header.flags, message.payload, routes, &mut read),
            Err(error) => tracing::debug!(%error, "remote links: an unreadable netlink message"),
        }
        rest = rest.get(length.next_multiple_of(4)..).unwrap_or_default();
    }
    read
}

fn apply_message(
    flags: u16,
    payload: NetlinkPayload<RouteNetlinkMessage>,
    routes: &mut DefaultRoutes,
    read: &mut Read,
) {
    let change = match payload {
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(route)) => {
            default_route(&route).map(|(family, key)| {
                if flags & NLM_F_REPLACE == 0 {
                    RouteChange::Added(family, key)
                } else {
                    RouteChange::Replaced(family, key)
                }
            })
        }
        NetlinkPayload::InnerMessage(RouteNetlinkMessage::DelRoute(route)) => {
            default_route(&route).map(|(family, key)| RouteChange::Removed(family, key))
        }
        NetlinkPayload::Done(_) => {
            read.flipped |= routes.finish_dump();
            None
        }
        NetlinkPayload::Error(error) => {
            let code = error.code.map_or(0, std::num::NonZeroI32::get);
            if code == -libc::ENOBUFS {
                read.overflowed = true;
            } else {
                tracing::debug!(code, "remote links: a netlink error");
                read.flipped |= routes.finish_dump();
            }
            None
        }
        _ => None,
    };
    if let Some(change) = change {
        read.flipped |= routes.apply(change);
    }
}

/// A route socket in the IPv4 and IPv6 route groups.
fn open() -> io::Result<Socket> {
    let mut socket = Socket::new(NETLINK_ROUTE)?;
    if let Err(error) = socket.set_rx_buf_sz(RECEIVE_BUFFER) {
        tracing::debug!(%error, "remote links: the route socket's buffer stays small");
    }
    let groups = u32::try_from(libc::RTMGRP_IPV4_ROUTE | libc::RTMGRP_IPV6_ROUTE).unwrap_or_default();
    socket.bind(&SocketAddr::new(0, groups))?;
    Ok(socket)
}

/// Asks the kernel for every route; the dump's messages follow on the socket.
fn request_dump(socket: &Socket, routes: &mut DefaultRoutes) -> io::Result<()> {
    let mut header = NetlinkHeader::default();
    header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    let mut request = NetlinkMessage::new(
        header,
        NetlinkPayload::from(RouteNetlinkMessage::GetRoute(RouteMessage::default())),
    );
    request.finalize();
    let mut bytes = vec![0; request.buffer_len()];
    request.serialize(&mut bytes);
    socket.send_to(&bytes, &SocketAddr::new(0, 0), 0)?;
    routes.begin_dump();
    Ok(())
}

/// The route socket, its dump at start asked for.
pub struct Watch {
    socket: Socket,
    routes: DefaultRoutes,
}

impl Watch {
    /// Opens the route socket and asks for the dump at start.
    ///
    /// # Errors
    ///
    /// Without rtnetlink: the socket cannot be opened, bound or asked for the dump.
    pub fn start() -> io::Result<Self> {
        let socket = open()?;
        let mut routes = DefaultRoutes::default();
        request_dump(&socket, &mut routes)?;
        Ok(Self { socket, routes })
    }

    /// Follows the default routes, reporting to `report`, until the socket fails: its error.
    pub fn follow(mut self, report: &dyn Fn(LinkEvent)) -> io::Error {
        tracing::info!("remote links: following the kernel's default routes");
        let mut started = false;
        let mut datagram = vec![0_u8; DATAGRAM];
        loop {
            let read = match self.socket.recv_from(&mut &mut datagram[..], 0) {
                Ok((length, sender)) if sender.port_number() == 0 && length <= datagram.len() => {
                    read_datagram(&datagram[..length], &mut self.routes)
                }
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.raw_os_error() == Some(libc::ENOBUFS) => Read {
                    flipped: false,
                    overflowed: true,
                },
                Err(error) => return error,
            };
            if let Err(error) = self.redump_if_due(&read) {
                return error;
            }
            let at_start = !started && !self.routes.dumping();
            if at_start || (started && read.flipped) {
                started = true;
                report(LinkEvent::Network {
                    available: self.routes.available(),
                });
            }
        }
    }

    /// Dumps the routes again a while after an overflow or a replaced default route, unless a dump is being read: that
    /// one goes on (the kernel writes it as it is read) and lists the routes as they are now; a route replaced while it
    /// is read is dumped again after it.
    fn redump_if_due(&mut self, read: &Read) -> io::Result<()> {
        if (read.overflowed && !self.routes.dumping()) || self.routes.redump_due() {
            tracing::debug!(
                overflowed = read.overflowed,
                "remote links: the routes may be stale; dumping them again"
            );
            std::thread::sleep(REDUMP_AFTER);
            request_dump(&self.socket, &mut self.routes)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
