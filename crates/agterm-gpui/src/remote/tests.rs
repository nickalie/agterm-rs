//! The retries the link events give.

use std::sync::{Arc, Mutex};

use super::*;

#[test]
fn the_first_network_report_is_the_state_at_start_and_a_later_available_one_retries() {
    let mut retries = LinkRetries::default();
    assert!(
        !retries.retries(LinkEvent::Network { available: true }),
        "the state at start"
    );
    assert!(
        retries.retries(LinkEvent::Network { available: true }),
        "a change while up"
    );
    assert!(
        !retries.retries(LinkEvent::Network { available: false }),
        "nothing to reach while down"
    );
    assert!(retries.retries(LinkEvent::Network { available: true }), "back up");
    let mut down_first = LinkRetries::default();
    assert!(!down_first.retries(LinkEvent::Network { available: false }));
    assert!(down_first.retries(LinkEvent::Network { available: true }));
}

#[test]
fn every_resume_retries_even_before_any_network_report() {
    let mut retries = LinkRetries::default();
    assert!(retries.retries(LinkEvent::Resumed));
    assert!(retries.retries(LinkEvent::Resumed));
    assert!(
        !retries.retries(LinkEvent::Network { available: true }),
        "a resume is no network report: the first one is still the state at start"
    );
}

#[test]
fn the_report_runs_the_retry_for_the_retrying_events_only_from_any_thread() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let report = retrying(move |event| sink.lock().unwrap().push(event));
    let network = Arc::clone(&report);
    std::thread::spawn(move || {
        network(LinkEvent::Network { available: false });
        network(LinkEvent::Network { available: true });
    })
    .join()
    .unwrap();
    report(LinkEvent::Resumed);
    assert_eq!(
        *seen.lock().unwrap(),
        [LinkEvent::Network { available: true }, LinkEvent::Resumed]
    );
}
