//! The default routes' rules and the datagrams' reading; the socket itself runs in a network namespace of its own in
//! `tests/remote_routes.rs`.

use std::net::{Ipv4Addr, Ipv6Addr};

use netlink_packet_core::{DoneMessage, ErrorMessage};
use netlink_packet_route::route::{RouteAddress, RouteHeader};

use super::*;

const MAIN: u8 = RouteHeader::RT_TABLE_MAIN;

fn route(family: AddressFamily, prefix: u8, attributes: Vec<RouteAttribute>) -> RouteMessage {
    let mut route = RouteMessage::default();
    route.header.address_family = family;
    route.header.destination_prefix_length = prefix;
    route.header.table = MAIN;
    route.header.kind = RouteType::Unicast;
    route.attributes = attributes;
    route
}

fn gateway_v4() -> RouteAttribute {
    RouteAttribute::Gateway(RouteAddress::Inet(Ipv4Addr::new(192, 0, 2, 1)))
}

fn key(table: u32, metric: u32, oif: u32) -> RouteKey {
    RouteKey { table, metric, oif }
}

#[test]
fn a_default_route_reaching_a_nexthop_is_keyed_by_table_metric_and_interface() {
    let via_gateway = route(
        AddressFamily::Inet,
        0,
        vec![gateway_v4(), RouteAttribute::Oif(2), RouteAttribute::Priority(600)],
    );
    assert_eq!(default_route(&via_gateway), Some((Family::V4, key(254, 600, 2))));
    let on_link = route(AddressFamily::Inet6, 0, vec![RouteAttribute::Oif(3)]);
    assert_eq!(default_route(&on_link), Some((Family::V6, key(254, 0, 3))));
    let multipath = route(
        AddressFamily::Inet,
        0,
        vec![RouteAttribute::MultiPath(Vec::new()), RouteAttribute::Table(1000)],
    );
    assert_eq!(
        default_route(&multipath),
        Some((Family::V4, key(1000, 0, 0))),
        "a table attribute names tables past 255"
    );
}

#[test]
fn other_routes_name_no_default_route() {
    let subnet = route(AddressFamily::Inet, 24, vec![RouteAttribute::Oif(2)]);
    assert_eq!(default_route(&subnet), None, "a subnet");
    let nowhere = route(AddressFamily::Inet, 0, vec![RouteAttribute::Priority(5)]);
    assert_eq!(default_route(&nowhere), None, "no nexthop");
    for kind in [
        RouteType::Unreachable,
        RouteType::BlackHole,
        RouteType::Prohibit,
        RouteType::Throw,
    ] {
        let mut dead_end = route(AddressFamily::Inet, 0, vec![RouteAttribute::Oif(2)]);
        dead_end.header.kind = kind;
        assert_eq!(default_route(&dead_end), None, "{kind:?}");
    }
    let other_family = route(AddressFamily::Mpls, 0, vec![RouteAttribute::Oif(2)]);
    assert_eq!(default_route(&other_family), None);
}

#[test]
fn a_familys_first_default_route_and_its_last_one_flip_and_nothing_else() {
    let mut routes = DefaultRoutes::default();
    routes.begin_dump();
    assert!(
        !routes.apply(RouteChange::Added(Family::V4, key(254, 0, 2))),
        "the dump collects apart"
    );
    assert!(!routes.available(), "until it ends");
    assert!(routes.finish_dump(), "the dump at start found a route");
    assert!(routes.available());
    assert!(
        !routes.apply(RouteChange::Added(Family::V4, key(254, 0, 2))),
        "replaced"
    );
    assert!(
        !routes.apply(RouteChange::Added(Family::V4, key(254, 100, 2))),
        "a second one"
    );
    assert!(
        routes.apply(RouteChange::Added(Family::V6, key(254, 0, 2))),
        "IPv6's first"
    );
    assert!(
        !routes.apply(RouteChange::Removed(Family::V4, key(254, 0, 2))),
        "one of two"
    );
    assert!(
        !routes.apply(RouteChange::Removed(Family::V4, key(254, 7, 2))),
        "one never there"
    );
    assert!(
        routes.apply(RouteChange::Removed(Family::V4, key(254, 100, 2))),
        "IPv4's last"
    );
    assert!(routes.available(), "IPv6's is still there");
    assert!(routes.apply(RouteChange::Removed(Family::V6, key(254, 0, 2))));
    assert!(!routes.available());
}

#[test]
fn a_dump_replaces_the_routes_and_a_deletion_in_its_middle_counts() {
    let mut routes = DefaultRoutes::default();
    assert!(routes.apply(RouteChange::Added(Family::V4, key(254, 0, 2))));
    routes.begin_dump();
    assert!(!routes.apply(RouteChange::Added(Family::V4, key(254, 0, 2))));
    assert!(
        routes.apply(RouteChange::Removed(Family::V4, key(254, 0, 2))),
        "the live route went"
    );
    assert!(!routes.finish_dump(), "the dump does not bring it back");
    assert!(!routes.available());
    routes.begin_dump();
    assert!(!routes.apply(RouteChange::Added(Family::V6, key(254, 0, 3))));
    assert!(routes.finish_dump(), "a route the socket missed");
    assert!(routes.available());
    assert!(!routes.finish_dump(), "no dump being read");
}

fn datagram(payloads: Vec<NetlinkPayload<RouteNetlinkMessage>>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for payload in payloads {
        let mut message = NetlinkMessage::new(NetlinkHeader::default(), payload);
        message.finalize();
        let mut buffer = vec![0; message.buffer_len()];
        message.serialize(&mut buffer);
        bytes.extend(buffer);
    }
    bytes
}

fn new_route(route: RouteMessage) -> NetlinkPayload<RouteNetlinkMessage> {
    NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewRoute(route))
}

fn del_route(route: RouteMessage) -> NetlinkPayload<RouteNetlinkMessage> {
    NetlinkPayload::InnerMessage(RouteNetlinkMessage::DelRoute(route))
}

#[test]
fn a_datagram_carries_several_messages_and_the_dumps_end() {
    let default_v4 = route(AddressFamily::Inet, 0, vec![gateway_v4(), RouteAttribute::Oif(2)]);
    let subnet = route(AddressFamily::Inet, 24, vec![RouteAttribute::Oif(2)]);
    let mut routes = DefaultRoutes::default();
    routes.begin_dump();
    let dump = datagram(vec![
        new_route(subnet.clone()),
        new_route(default_v4.clone()),
        NetlinkPayload::Done(DoneMessage::default()),
    ]);
    assert_eq!(
        read_datagram(&dump, &mut routes),
        Read {
            flipped: true,
            overflowed: false
        }
    );
    assert!(!routes.dumping() && routes.available());
    let gone = datagram(vec![del_route(subnet), del_route(default_v4)]);
    assert!(read_datagram(&gone, &mut routes).flipped);
    assert!(!routes.available());
    let mut cut = datagram(vec![new_route(route(
        AddressFamily::Inet6,
        0,
        vec![RouteAttribute::Oif(4)],
    ))]);
    cut.extend([0xff; 7]);
    assert!(read_datagram(&cut, &mut routes).flipped, "a stray tail is skipped");
}

#[test]
fn an_overflow_asks_for_another_dump_and_another_error_ends_the_dump() {
    let mut overflow = ErrorMessage::default();
    overflow.code = std::num::NonZeroI32::new(-libc::ENOBUFS);
    let mut routes = DefaultRoutes::default();
    assert_eq!(
        read_datagram(&datagram(vec![NetlinkPayload::Error(overflow)]), &mut routes),
        Read {
            flipped: false,
            overflowed: true
        }
    );
    routes.begin_dump();
    assert!(!routes.apply(RouteChange::Added(Family::V6, key(254, 0, 2))));
    let mut refused = ErrorMessage::default();
    refused.code = std::num::NonZeroI32::new(-libc::EBUSY);
    let read = read_datagram(&datagram(vec![NetlinkPayload::Error(refused)]), &mut routes);
    assert!(read.flipped && !read.overflowed);
    assert!(!routes.dumping() && routes.available(), "what the dump listed");
}

#[test]
fn an_ipv6_gateway_counts_too() {
    let via = route(
        AddressFamily::Inet6,
        0,
        vec![RouteAttribute::Gateway(RouteAddress::Inet6(Ipv6Addr::LOCALHOST))],
    );
    assert_eq!(default_route(&via), Some((Family::V6, key(254, 0, 0))));
}
