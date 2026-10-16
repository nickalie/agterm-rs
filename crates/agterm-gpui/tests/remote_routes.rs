//! The kernel's default routes as the network's last source reads them, over a real rtnetlink socket in a network
//! namespace of the test's own: the test runs itself again under `unshare --user --map-root-user --net`, and the copy
//! inside adds and deletes routes on its own loopback with `ip`, never touching the machine's network. Skipped (with a
//! note) where unprivileged user namespaces are refused.

#![cfg(target_os = "linux")]

use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use agterm_gpui::remote::LinkEvent;
use agterm_gpui::remote::routes::Watch;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Set for the copy inside the namespace: the outer test's network namespace, which the copy's must differ from.
const OUTER_NAMESPACE: &str = "ROUTES_TEST_OUTER_NETNS";
const TEST: &str = "the_kernels_default_routes_are_followed_in_a_network_namespace";
/// How long a report may take, and how long one that must not come is waited for.
const DEADLINE: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

fn namespace() -> TestResult<String> {
    Ok(std::fs::read_link("/proc/self/ns/net")?.to_string_lossy().into_owned())
}

fn have_namespaces() -> bool {
    Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "true"])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
fn the_kernels_default_routes_are_followed_in_a_network_namespace() -> TestResult {
    if let Some(outer) = std::env::var_os(OUTER_NAMESPACE) {
        return inside(&outer.to_string_lossy());
    }
    if !have_namespaces() {
        eprintln!("skipped: no unprivileged user and network namespaces here (`unshare -rn` refused)");
        return Ok(());
    }
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net"])
        .arg(std::env::current_exe()?)
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(OUTER_NAMESPACE, namespace()?)
        .output()?;
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "the copy in the namespace failed:\n{printed}");
    assert!(printed.contains("1 passed"), "the copy ran no test:\n{printed}");
    Ok(())
}

fn ip(args: &str) -> TestResult {
    let output = Command::new("ip").args(args.split(' ')).output()?;
    if !output.status.success() {
        return Err(format!("ip {args}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(())
}

fn next(reports: &mpsc::Receiver<LinkEvent>, what: &str) -> TestResult<LinkEvent> {
    reports
        .recv_timeout(DEADLINE)
        .map_err(|error| format!("{what}: no report ({error})").into())
}

fn quiet(reports: &mpsc::Receiver<LinkEvent>, what: &str) {
    let received = reports.recv_timeout(QUIET);
    assert!(received.is_err(), "{what}: {received:?}");
}

const UP: LinkEvent = LinkEvent::Network { available: true };
const DOWN: LinkEvent = LinkEvent::Network { available: false };

/// The copy inside the namespace: a fresh one, without any route.
fn inside(outer: &str) -> TestResult {
    assert_ne!(
        namespace()?,
        outer,
        "refusing to change routes outside a namespace of the test's own"
    );
    let watch = Watch::start()?;
    let (sender, reports) = mpsc::channel();
    std::thread::spawn(move || {
        watch.follow(&move |event| {
            let _ = sender.send(event);
        })
    });
    assert_eq!(
        next(&reports, "the state at start")?,
        DOWN,
        "no route in a new namespace"
    );
    ip("link set lo up")?;
    quiet(&reports, "the loopback's own routes are no default route");
    ip("route add default dev lo")?;
    assert_eq!(next(&reports, "IPv4's first default route")?, UP);
    ip("route add default dev lo metric 100")?;
    quiet(&reports, "a second IPv4 default route");
    ip("route replace default dev lo metric 100")?;
    quiet(&reports, "a replaced one");
    ip("-6 route add default dev lo")?;
    assert_eq!(next(&reports, "IPv6's first default route")?, UP);
    ip("route del default dev lo metric 0")?;
    quiet(&reports, "one of IPv4's two");
    ip("route del default dev lo metric 100")?;
    assert_eq!(next(&reports, "IPv4's last, IPv6's still there")?, UP);
    ip("-6 route del default dev lo")?;
    assert_eq!(next(&reports, "no default route left")?, DOWN);
    ip("route add blackhole default")?;
    quiet(&reports, "a blackhole reaches nothing");
    ip("route add 198.51.100.0/24 dev lo")?;
    quiet(&reports, "a subnet");
    Ok(())
}
