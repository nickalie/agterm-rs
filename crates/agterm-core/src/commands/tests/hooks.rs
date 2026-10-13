//! `hooks.*` and the hooks the events start, over a fake process launcher.

use agterm_host::ProcessExit;
use agterm_proto::command::{HooksCmd, SessionCmd};
use agterm_proto::{Args, Command};
use serde_json::{Value, json};

use super::keymap::Host;
use super::{UNKNOWN, args, failure, wire};
use crate::NotificationEffect;

fn hooks(host: &mut Host, cmd: HooksCmd, target: Option<&str>, args: Args) -> agterm_proto::Response {
    host.fixture.send(Command::Hooks(cmd), target, args)
}

fn listing(host: &mut Host) -> Value {
    let response = hooks(host, HooksCmd::List, None, Args::default());
    assert!(response.ok, "{response:?}");
    response.result.unwrap()["hooks"].clone()
}

fn reload(host: &mut Host) -> Value {
    let response = hooks(host, HooksCmd::Reload, None, Args::default());
    assert!(response.ok, "{response:?}");
    response.result.unwrap()
}

#[test]
fn hooks_take_no_target_or_window() {
    let mut host = Host::new(0);
    for (cmd, name) in [(HooksCmd::Reload, "hooks.reload"), (HooksCmd::List, "hooks.list")] {
        let error = format!("{name} takes no target or --window");
        assert_eq!(hooks(&mut host, cmd, Some("active"), Args::default()), failure(&error));
        assert_eq!(hooks(&mut host, cmd, Some(UNKNOWN), Args::default()), failure(&error));
        let window = args(|a| a.window = Some(UNKNOWN.to_owned()));
        assert_eq!(hooks(&mut host, cmd, None, window), failure(&error));
    }
}

#[test]
fn reload_reports_each_bad_line_and_keeps_the_good_ones() {
    let mut host = Host::new(0);
    assert_eq!(reload(&mut host), json!({"count": 0}));
    assert_eq!(
        listing(&mut host),
        json!({"path": host.config("hooks.conf").to_string_lossy(), "diagnostics": [], "hooks": []})
    );
    let good = "echo created > /dev/null";
    host.write(
        "hooks.conf",
        &format!(
            "# conformance hooks\non session.created {good}\non session.renamed true\noff status true\non notify\n\
             on session.created   {good}  \n"
        ),
    );
    assert_eq!(reload(&mut host), json!({"count": 4}));
    let banners: Vec<String> = host
        .fixture
        .core
        .take_notifications()
        .into_iter()
        .filter_map(|effect| match effect {
            NotificationEffect::App { body, .. } => Some(body),
            _ => None,
        })
        .collect();
    assert_eq!(banners, ["4 issues — see Settings ▸ Key Mapping"]);
    let rows = listing(&mut host);
    assert_eq!(rows["diagnostics"].as_array().unwrap().len(), 4);
    assert_eq!(
        rows["hooks"],
        json!([{"kind": "session.created", "command": good, "line": 2, "pending": 0, "dropped": 0}])
    );
}

/// Creates a session and commits, as a control request and the main loop's settle would; its id.
fn new_session(host: &mut Host) -> String {
    let response = host.fixture.session(SessionCmd::New, None, Args::default());
    let _ = host.fixture.core.commit();
    response.result.unwrap()["id"].as_str().unwrap().to_owned()
}

#[test]
fn an_event_starts_its_hook_with_the_event_on_stdin_and_its_environment() {
    let mut host = Host::new(0);
    host.write("hooks.conf", "on session.created cat > /dev/null\n");
    assert_eq!(reload(&mut host), json!({"count": 0}));
    let session = new_session(&mut host);
    let launches = host.launcher.launches();
    let [launch] = launches.as_slice() else {
        panic!("{launches:?}")
    };
    assert_eq!(launch.args, ["-c", "cat > /dev/null"]);
    let event: Value = serde_json::from_slice(launch.stdin.as_deref().unwrap()).unwrap();
    assert_eq!(
        (event["kind"].as_str(), event["session"].as_str()),
        (Some("session.created"), Some(session.as_str()))
    );
    let env = |key: &str| {
        launch
            .env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(env("AGT_EVENT_KIND").as_deref(), Some("session.created"));
    assert_eq!(env("AGT_SESSION_ID"), Some(session));
    assert_eq!(env("AGT_WINDOW_ID"), Some(wire(host.fixture.window)));
    assert_eq!(env("AGT_SOCKET").as_deref(), Some("/tmp/agterm-test.sock"));
}

#[test]
fn a_second_event_queues_behind_the_running_child() {
    let mut host = Host::new(0);
    host.write("hooks.conf", "on session.created cat > /dev/null\n");
    reload(&mut host);
    new_session(&mut host);
    new_session(&mut host);
    let row = &listing(&mut host)["hooks"][0];
    assert_eq!(row["pending"], 1);
    assert_eq!(row["runningPid"], json!(host.launcher.running()[0]));
    assert_eq!(host.launcher.launches().len(), 1);
    let pid = host.launcher.running()[0];
    host.launcher.finish(pid, ProcessExit::default());
    host.drain();
    assert_eq!(host.launcher.launches().len(), 2);
    assert_eq!(listing(&mut host)["hooks"][0]["pending"], 0);
}

#[test]
fn a_failing_hook_banners_once_until_it_succeeds() {
    let mut host = Host::new(0);
    host.write("hooks.conf", "on session.created false\n");
    reload(&mut host);
    let _ = host.fixture.core.take_notifications();
    for status in [1, 1] {
        assert!(host.fixture.session(SessionCmd::New, None, Args::default()).ok);
        let _ = host.fixture.core.commit();
        let pid = host.launcher.running()[0];
        host.launcher.finish(
            pid,
            ProcessExit {
                status,
                ..ProcessExit::default()
            },
        );
        host.drain();
    }
    let bodies: Vec<String> = host
        .fixture
        .core
        .take_notifications()
        .into_iter()
        .filter_map(|effect| match effect {
            NotificationEffect::App { identity, body, .. } => Some(format!("{identity}|{body}")),
            _ => None,
        })
        .collect();
    assert_eq!(
        bodies,
        ["hook-failure:session.created:false|on session.created false (exit 1)"]
    );
    assert_eq!(listing(&mut host)["hooks"][0]["lastFailure"], "exit 1");
}

#[test]
fn a_hook_removed_while_running_is_retired_until_its_child_exits() {
    let mut host = Host::new(0);
    host.write("hooks.conf", "on session.created sleep 1\n");
    reload(&mut host);
    assert!(host.fixture.session(SessionCmd::New, None, Args::default()).ok);
    let _ = host.fixture.core.commit();
    host.write("hooks.conf", "# no hooks\n");
    reload(&mut host);
    assert_eq!(listing(&mut host)["hooks"][0]["retired"], true);
    let pid = host.launcher.running()[0];
    host.launcher.finish(pid, ProcessExit::default());
    host.drain();
    assert_eq!(listing(&mut host)["hooks"], json!([]));
}

#[test]
fn a_broken_file_reports_the_error() {
    let mut host = Host::new(0);
    host.write("hooks.conf", "on session.created true\n");
    reload(&mut host);
    std::fs::write(host.config("hooks.conf"), b"on session.created \xff\n").unwrap();
    assert_eq!(reload(&mut host), json!({"count": 1}));
    let rows = listing(&mut host);
    assert_eq!(rows["hooks"], json!([]));
    assert!(
        rows["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .starts_with("could not read hooks.conf: ")
    );
}
