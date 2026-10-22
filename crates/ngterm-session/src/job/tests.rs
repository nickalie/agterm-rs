use std::sync::{Arc, Mutex};

use super::*;

fn record(pid: i32, started: u64, group: i32, foreground: i32) -> ProcessRecord {
    ProcessRecord {
        pid,
        started,
        group,
        foreground,
    }
}

/// A Unix table entry: `record` with a parent nobody walks.
fn grouped(pid: i32, started: u64, group: i32, foreground: i32) -> ProcessEntry {
    ProcessEntry {
        record: record(pid, started, group, foreground),
        parent: 1,
    }
}

/// A Windows table entry: its own group, no terminal foreground, and its parent.
fn child(pid: i32, started: u64, parent: i32) -> ProcessEntry {
    ProcessEntry {
        record: record(pid, started, pid, 0),
        parent,
    }
}

#[test]
fn the_group_rule_takes_the_foreground_group_without_the_shell() {
    let table = [
        grouped(10, 1, 10, 20),
        grouped(20, 2, 20, 20),
        grouped(21, 3, 20, 20),
        grouped(30, 4, 30, 30),
    ];
    assert_eq!(group_job(10, &table), [record(20, 2, 20, 20), record(21, 3, 20, 20)]);
    // a shell holding its own foreground, and one the table does not hold
    assert!(group_job(30, &table).is_empty());
    assert!(group_job(99, &table).is_empty());
}

#[test]
fn the_descendant_rule_takes_the_shells_live_tree() {
    let table = [
        child(4, 0, 0),
        child(100, 10, 4),
        child(200, 11, 100),
        child(300, 12, 200),
        child(201, 13, 100),
        child(400, 14, 4),
    ];
    assert_eq!(
        descendant_job(100, &table),
        [
            record(200, 11, 200, 0),
            record(201, 13, 201, 0),
            record(300, 12, 300, 0)
        ]
    );
    assert!(descendant_job(400, &table).is_empty(), "a shell running nothing");
    assert!(
        descendant_job(999, &table).is_empty(),
        "a shell the table does not hold"
    );
}

#[test]
fn a_process_older_than_its_parent_belongs_to_the_parents_recycled_pid() {
    // 500 is the shell; 600 claims it as parent but started first, so its parent was another process with pid 500
    let table = [
        child(500, 50, 1),
        child(600, 40, 500),
        child(601, 41, 600),
        child(700, 51, 500),
    ];
    assert_eq!(descendant_job(500, &table), [record(700, 51, 700, 0)]);
    // a process that names itself as parent (pid 0 on Windows, the idle process) is not its own child
    assert!(descendant_job(0, &[child(0, 0, 0)]).is_empty());
}

#[test]
fn this_oses_rule_is_the_one_foreground_job_takes() {
    let table = [child(100, 10, 1), child(200, 11, 100)];
    let expected = if cfg!(windows) {
        descendant_job(100, &table)
    } else {
        group_job(100, &table)
    };
    assert_eq!(foreground_job(100, &table), expected);
}

#[test]
fn a_sweep_signals_only_the_surviving_job() {
    let table = Arc::new(Mutex::new(vec![
        grouped(10, 1, 10, 20),
        grouped(20, 2, 20, 20),
        grouped(21, 3, 20, 20),
    ]));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (reader, log) = (Arc::clone(&table), Arc::clone(&sent));
    let sweeper = Sweeper::new(
        Box::new(move || Some(reader.lock().unwrap().clone())),
        Box::new(move |signal, record| log.lock().unwrap().push((signal, record.pid))),
    );
    let shells = [("ngterm-a".to_owned(), 10), ("ngterm-b".to_owned(), 99)];
    let jobs = if cfg!(windows) {
        // the descendant rule, over the same pids
        *table.lock().unwrap() = vec![child(10, 1, 1), child(20, 2, 10), child(21, 3, 20)];
        let jobs = sweeper.capture(&shells);
        *table.lock().unwrap() = vec![child(20, 2, 10)];
        jobs
    } else {
        let jobs = sweeper.capture(&shells);
        // the shell is gone and 21 exited: only 20 is hung up
        *table.lock().unwrap() = vec![grouped(20, 2, 20, 20)];
        jobs
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].0, "ngterm-a");
    assert_eq!(
        jobs[0]
            .1
            .iter()
            .map(|record| (record.pid, record.started))
            .collect::<Vec<_>>(),
        [(20, 2), (21, 3)]
    );
    sweeper.send(Signal::HangUp, &jobs[0].1);
    assert_eq!(*sent.lock().unwrap(), [(Signal::HangUp, 20)]);
    assert!(sweeper.is_running(&jobs[0].1));
    table.lock().unwrap().clear();
    assert!(!sweeper.is_running(&jobs[0].1));
}

#[test]
fn an_unreadable_table_counts_as_running_and_signals_nothing() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&sent);
    let sweeper = Sweeper::new(
        Box::new(|| None),
        Box::new(move |signal, record| log.lock().unwrap().push((signal, record.pid))),
    );
    let job = [record(20, 2, 20, 20)];
    assert!(sweeper.is_running(&job));
    assert!(!sweeper.is_running(&[]));
    assert_eq!(sweeper.foreground_job(10), None, "unknown, never \"no job\"");
    assert!(sweeper.capture(&[("ngterm-a".to_owned(), 10)]).is_empty());
    sweeper.send(Signal::Kill, &job);
    assert!(sent.lock().unwrap().is_empty());
}

#[test]
fn the_live_table_holds_this_test_process_with_its_parent() {
    let table = read_table().unwrap();
    let pid = i32::try_from(std::process::id()).unwrap();
    let own = table.iter().find(|entry| entry.record.pid == pid).unwrap();
    assert!(own.parent > 0, "{own:?}");
    assert!(own.record.started > 0, "{own:?}");
    if cfg!(windows) {
        assert_eq!((own.record.group, own.record.foreground), (pid, 0));
    }
    // the parent outlives the test and started before it
    let parent = table.iter().find(|entry| entry.record.pid == own.parent).unwrap();
    assert!(parent.record.started <= own.record.started, "{parent:?} {own:?}");
}
