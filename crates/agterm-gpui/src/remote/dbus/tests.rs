//! The pure parts of the D-Bus sources; the sources themselves run against fake services on private buses in
//! `tests/remote_links.rs`.

use super::*;

#[test]
fn network_managers_connectivity_decides_and_an_unknown_one_reads_the_state() {
    assert!(!reachable(CONNECTIVITY_NONE, 70), "connectivity none");
    for connectivity in [2, 3, 4] {
        assert!(reachable(connectivity, 20), "portal, limited, full: {connectivity}");
    }
    assert!(reachable(CONNECTIVITY_UNKNOWN, 70), "connected globally, the check off");
    assert!(reachable(CONNECTIVITY_UNKNOWN, STATE_CONNECTED_SITE), "a default route");
    assert!(!reachable(CONNECTIVITY_UNKNOWN, 50), "local only");
    assert!(!reachable(CONNECTIVITY_UNKNOWN, 20), "disconnected");
}

#[test]
fn the_connectivity_is_gios_reading_of_the_state_and_networkmanagers_check() {
    assert_eq!(connectivity(CONNECTIVITY_FULL, 50), Connectivity::Local, "local only");
    assert_eq!(connectivity(CONNECTIVITY_FULL, 20), Connectivity::Local, "disconnected");
    assert_eq!(
        connectivity(CONNECTIVITY_PORTAL, 60),
        Connectivity::Portal,
        "a site behind a portal"
    );
    assert_eq!(
        connectivity(CONNECTIVITY_FULL, 60),
        Connectivity::Limited,
        "a site only"
    );
    assert_eq!(connectivity(CONNECTIVITY_PORTAL, 70), Connectivity::Portal);
    assert_eq!(connectivity(CONNECTIVITY_LIMITED, 70), Connectivity::Limited);
    assert_eq!(connectivity(CONNECTIVITY_FULL, 70), Connectivity::Full);
    assert_eq!(
        connectivity(CONNECTIVITY_UNKNOWN, 70),
        Connectivity::Local,
        "the check off"
    );
    assert_ne!(
        connectivity(CONNECTIVITY_LIMITED, 70),
        connectivity(CONNECTIVITY_FULL, 70),
        "limited turning full is a change, both reachable"
    );
}

fn sleep_signal(start: bool) -> zbus::Message {
    zbus::Message::signal(LOGIND_PATH, LOGIND_MANAGER, "PrepareForSleep")
        .unwrap()
        .build(&(start,))
        .unwrap()
}

#[test]
fn only_a_wake_is_a_resume() {
    assert!(resumed(&sleep_signal(false)));
    assert!(!resumed(&sleep_signal(true)), "going to sleep");
    let other = zbus::Message::signal(LOGIND_PATH, LOGIND_MANAGER, "PrepareForSleep")
        .unwrap()
        .build(&("nope",))
        .unwrap();
    assert!(!resumed(&other));
}

#[test]
fn absent_buses_follow_nothing_and_end_at_once() {
    let endpoints = Endpoints {
        system: Endpoint::Absent,
        session: Endpoint::Absent,
        routes: Routes::Absent,
    };
    let report: Report = std::sync::Arc::new(|event| panic!("nothing to report: {event:?}"));
    let sources = start(&endpoints, &report);
    assert_eq!(sources.network.unwrap().join().unwrap(), NetworkSource::None);
    assert!(!sources.resume.unwrap().join().unwrap());
}
