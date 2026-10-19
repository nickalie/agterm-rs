use agterm_host::ProcessRecord;

use super::*;

/// A synthetic `KERN_PROCARGS2` blob (upstream's test helper): host-order argc, the NUL-terminated executable path,
/// `padding` extra NULs, then each argument NUL-terminated.
fn blob(count: i32, path: &str, padding: usize, arguments: &[&[u8]]) -> Vec<u8> {
    let mut bytes = count.to_ne_bytes().to_vec();
    bytes.extend_from_slice(path.as_bytes());
    bytes.push(0);
    bytes.extend(std::iter::repeat_n(0, padding));
    for arg in arguments {
        bytes.extend_from_slice(arg);
        bytes.push(0);
    }
    bytes
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn procargs_read_the_argv_past_the_executable_paths_padding() {
    assert_eq!(
        parse_procargs(&blob(2, "/usr/bin/ssh", 3, &[b"ssh", b"gate"])),
        Some(strings(&["ssh", "gate"]))
    );
    assert_eq!(
        parse_procargs(&blob(3, "/usr/bin/ssh", 1, &[b"ssh", b"gate", b"-t ssh inner"])),
        Some(strings(&["ssh", "gate", "-t ssh inner"]))
    );
}

#[test]
fn procargs_refuse_a_truncated_empty_or_implausible_blob() {
    assert_eq!(parse_procargs(&[]), None);
    assert_eq!(
        parse_procargs(&blob(2, "/bin/sh", 0, &[b"sh"])),
        None,
        "fewer arguments than argc"
    );
    assert_eq!(parse_procargs(&[1, 2]), None, "shorter than the argc header");
    assert_eq!(parse_procargs(&blob(0, "/bin/sh", 0, &[])), None);
    assert_eq!(parse_procargs(&blob(-1, "/bin/sh", 1, &[b"sh"])), None);
    assert_eq!(
        parse_procargs(&blob(5000, "/bin/sh", 1, &[b"sh", b"x"])),
        None,
        "beyond the cap"
    );
    let mut unterminated = 1_i32.to_ne_bytes().to_vec();
    unterminated.extend_from_slice(b"/bin/shhhhhh");
    assert_eq!(parse_procargs(&unterminated), None, "no NUL after the path");
}

#[test]
fn procargs_take_an_empty_path_and_ignore_the_environment() {
    assert_eq!(parse_procargs(&blob(1, "", 0, &[b"sh"])), Some(strings(&["sh"])));
    let mut with_env = blob(1, "/bin/sh", 1, &[b"sh"]);
    with_env.extend_from_slice(b"PATH=/bin\0");
    assert_eq!(parse_procargs(&with_env), Some(strings(&["sh"])));
}

#[test]
fn procargs_decode_lossily_and_keep_spaces_and_quotes() {
    assert_eq!(
        parse_procargs(&blob(2, "/usr/bin/grep", 0, &[b"grep", b"caf\xe9"])),
        Some(strings(&["grep", "caf\u{fffd}"]))
    );
    assert_eq!(
        parse_procargs(&blob(3, "/bin/sh", 2, &[b"sh", b"a b", b"it's \"quoted\""])),
        Some(strings(&["sh", "a b", "it's \"quoted\""]))
    );
}

fn entry(pid: i32, started: u64, parent: i32) -> ProcessEntry {
    ProcessEntry {
        record: ProcessRecord {
            pid,
            started,
            group: pid,
            foreground: 0,
        },
        parent,
    }
}

#[test]
fn a_consoles_foreground_is_the_shells_newest_child_else_the_shell() {
    let table = [
        entry(100, 10, 1),
        entry(200, 11, 100),
        entry(201, 15, 100),
        entry(300, 16, 201),
        entry(400, 20, 1),
    ];
    assert_eq!(console_foreground(100, &table), Some(201));
    assert_eq!(console_foreground(201, &table), Some(300));
    assert_eq!(console_foreground(400, &table), Some(400), "a shell at its prompt");
    assert_eq!(console_foreground(999, &table), None);
    // a child older than the shell came from a process that had the shell's pid before it
    assert_eq!(
        console_foreground(500, &[entry(500, 50, 1), entry(600, 40, 500)]),
        Some(500)
    );
}
