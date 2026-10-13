//! Ported from upstream's `TerminfoInstallTests` and `TerminfoCommandsTests`: fake `ssh` and `infocmp` scripts in a temp
//! root, the real `infocmp`/`tic` only against a temp database, and the remote side as a temp `HOME`.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command as Process;

use clap::Parser;

use super::*;
use crate::{Agtermctl, Command as Cli};

fn env(pairs: &[(&str, &Path)]) -> HashMap<OsString, OsString> {
    pairs
        .iter()
        .map(|(name, value)| (OsString::from(name), value.as_os_str().to_owned()))
        .collect()
}

/// A temp root holding fake `ssh`/`infocmp` scripts, a fake stage, and what the fakes recorded.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn script(path: &Path, body: &str) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        path.to_path_buf()
    }

    /// `<root>/<name>/<layout>/xterm-ghostty`, an empty file where the entry would sit.
    fn database(&self, name: &str, layout: &str) -> PathBuf {
        let dir = self.path(name);
        fs::create_dir_all(dir.join(layout)).unwrap();
        fs::write(dir.join(layout).join(ENTRY), b"").unwrap();
        dir
    }

    /// The staging layout the installed CLI resolves into: `bin/agtermctl` beside `share/terminfo`.
    fn stage(&self, layout: &str) -> (PathBuf, PathBuf) {
        let bin = self.path("stage/bin");
        fs::create_dir_all(&bin).unwrap();
        let client = bin.join("agtermctl");
        fs::write(&client, b"").unwrap();
        (client, self.database("stage/share/terminfo", layout))
    }

    /// A database the real `tic` compiled from `source`, so the real `infocmp` has something to dump.
    fn compiled_database(&self, source: &str) -> PathBuf {
        let dir = self.path("compiled");
        fs::create_dir_all(&dir).unwrap();
        let mut tic = Process::new("/usr/bin/tic")
            .args(["-x", "-o"])
            .arg(&dir)
            .arg("-")
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        tic.stdin.take().unwrap().write_all(source.as_bytes()).unwrap();
        assert!(tic.wait().unwrap().success(), "tic could not compile the fixture entry");
        dir
    }

    fn fake_infocmp(&self, output: &str) -> PathBuf {
        let file = self.path("infocmp.out");
        fs::write(&file, output).unwrap();
        Self::script(&self.path("infocmp"), &format!("cat '{}'", file.display()))
    }

    fn failing_infocmp(&self, message: &str) -> PathBuf {
        Self::script(
            &self.path("infocmp"),
            &format!("printf '%s\\n' '{message}' >&2; exit 1"),
        )
    }

    /// Records its arguments one per line and, unless told not to, copies stdin to a file.
    fn fake_ssh(&self, exit_code: i32, reads_stdin: bool, extra: &str) -> String {
        let read = if reads_stdin {
            format!("cat > '{}'", self.path("ssh.stdin").display())
        } else {
            String::new()
        };
        let script = Self::script(
            &self.path("ssh"),
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\n{extra}\n{read}\nexit {exit_code}",
                self.path("ssh.args").display()
            ),
        );
        script.display().to_string()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap()
    }

    fn ssh_arguments(&self) -> Vec<String> {
        self.read("ssh.args").lines().map(str::to_owned).collect()
    }

    /// A stand-in for the remote account: an empty `HOME` and a `PATH` of a bin dir plus `/bin` (mkdir, but on a
    /// merged-usr system tic too, so the no-tic case uses the bin dir alone) and a fake `tic` that records its
    /// arguments and stdin, or none.
    fn remote_home(&self, with_tic: bool) -> (PathBuf, Vec<(&'static str, String)>) {
        let home = self.path("remote-home");
        let bin = self.path("remote-bin");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&bin).unwrap();
        let path = if with_tic {
            Self::script(
                &bin.join("tic"),
                &format!(
                    "printf '%s\\n' \"$@\" > '{}'\ncat > '{}'",
                    self.path("tic.args").display(),
                    self.path("tic.stdin").display()
                ),
            );
            format!("{}:/bin", bin.display())
        } else {
            for tool in ["mkdir", "printf"] {
                std::os::unix::fs::symlink(Path::new("/usr/bin").join(tool), bin.join(tool)).unwrap();
            }
            bin.display().to_string()
        };
        (home.clone(), vec![("HOME", home.display().to_string()), ("PATH", path)])
    }
}

fn connection(destination: &str) -> Connection {
    Connection {
        destination: destination.to_owned(),
        ..Connection::default()
    }
}

// -- the command line

fn parse(argv: &[&str]) -> Result<Install, clap::Error> {
    let line = ["agtermctl", "terminfo", "install"].iter().chain(argv).copied();
    let Cli::Terminfo(group) = Agtermctl::try_parse_from(line)?.command else {
        panic!("not a terminfo command")
    };
    let TerminfoCommand::Install(install) = group.command;
    Ok(install)
}

#[test]
fn install_maps_its_typed_options_onto_the_connection() {
    let install = parse(&[
        "me@buildbox",
        "-p",
        "2222",
        "-i",
        "/k1",
        "-i",
        "/k2",
        "-J",
        "bastion",
        "-F",
        "/cfg",
    ])
    .unwrap();
    assert_eq!(
        install.connection(),
        Connection {
            destination: "me@buildbox".to_owned(),
            port: Some(2222),
            identities: vec!["/k1".to_owned(), "/k2".to_owned()],
            jump: Some("bastion".to_owned()),
            config: Some("/cfg".to_owned()),
        }
    );
    assert_eq!(parse(&["buildbox"]).unwrap().connection(), connection("buildbox"));
}

#[test]
fn install_refuses_anything_beyond_the_destination_and_its_options() {
    for argv in [
        &[][..],
        &["buildbox", "uptime"],
        &["--", "-G"],
        &["-N", "buildbox"],
        &["buildbox", "--socket", "/tmp/s"],
    ] {
        let refused = parse(argv)
            .map_err(drop)
            .and_then(|install| install.validate().map_err(drop));
        assert!(refused.is_err(), "{argv:?}");
    }
}

#[test]
fn install_validation_names_the_destination() {
    for destination in ["-oProxyCommand=x", "two words", ""] {
        assert_eq!(
            parse(&["--", destination]).unwrap().validate(),
            Err(format!(
                "destination must be a host, user@host or an ssh alias, not '{destination}'"
            ))
        );
    }
}

#[test]
fn every_failure_has_its_own_message() {
    let cases = [
        (
            InstallFailure::TerminfoNotFound {
                searched: vec![PathBuf::from("/a/share/terminfo"), PathBuf::from("/t")],
            },
            "no xterm-ghostty terminfo entry next to this agtermctl; looked in /a/share/terminfo, /t",
        ),
        (
            InstallFailure::DumpFailed {
                status: 1,
                stderr: "infocmp: no terminfo file\n".to_owned(),
            },
            "infocmp exited 1: infocmp: no terminfo file",
        ),
        (
            InstallFailure::SpawnFailed {
                operation: "posix_spawnp".to_owned(),
                reason: "No such file or directory".to_owned(),
            },
            "could not start ssh: posix_spawnp failed with No such file or directory",
        ),
    ];
    for (failure, message) in cases {
        assert_eq!(failure.message(), message);
    }
}

// -- where the entry comes from

#[test]
fn the_stage_beside_the_client_wins_over_the_environment() {
    let fixture = Fixture::new();
    let (client, terminfo) = fixture.stage("78");
    let from_env = fixture.database("env", "x");
    assert_eq!(
        terminfo_directory(Some(&client), &env(&[("TERMINFO", &from_env)])),
        Some(terminfo)
    );
}

#[test]
fn the_environment_is_the_fallback_when_the_client_has_no_stage() {
    let fixture = Fixture::new();
    let from_env = fixture.database("env", "x");
    let loose = fixture.path("target/debug/agtermctl");
    let environment = env(&[("TERMINFO", &from_env)]);
    assert_eq!(terminfo_directory(Some(&loose), &environment), Some(from_env.clone()));
    assert_eq!(terminfo_directory(None, &environment), Some(from_env));
}

#[test]
fn a_directory_without_the_entry_is_skipped() {
    let fixture = Fixture::new();
    let empty = fixture.path("empty");
    fs::create_dir_all(&empty).unwrap();
    assert_eq!(terminfo_directory(None, &env(&[("TERMINFO", &empty)])), None);
    assert_eq!(terminfo_directory(None, &HashMap::new()), None);
    assert!(candidate_directories(None, &env(&[("TERMINFO", Path::new(""))])).is_empty());
}

// -- the ssh invocation

#[test]
fn install_maps_every_connection_option_after_the_execution_settings() {
    let argv = install_command(
        &Connection {
            destination: "me@buildbox".to_owned(),
            port: Some(2222),
            identities: vec!["/k1".to_owned(), "/k2".to_owned()],
            jump: Some("bastion".to_owned()),
            config: Some("/cfg".to_owned()),
        },
        "ssh",
    )
    .unwrap();
    assert_eq!(argv[0], "ssh");
    assert_eq!(argv[1..=EXECUTION_OPTIONS.len()], EXECUTION_OPTIONS);
    assert_eq!(
        argv[1 + EXECUTION_OPTIONS.len()..argv.len() - 1],
        [
            "-p",
            "2222",
            "-i",
            "/k1",
            "-i",
            "/k2",
            "-J",
            "bastion",
            "-F",
            "/cfg",
            "me@buildbox"
        ]
    );
    let bare = install_command(&connection("buildbox"), "ssh").unwrap();
    assert_eq!(bare.len(), EXECUTION_OPTIONS.len() + 3);
    assert_eq!(bare[EXECUTION_OPTIONS.len() + 1], "buildbox");
}

#[test]
fn install_overrides_the_execution_settings_a_config_file_sets() {
    let ssh = Path::new("/usr/bin/ssh");
    if !ssh.exists() {
        return;
    }
    let fixture = Fixture::new();
    let hostile = fixture.path("hostile.sshconfig");
    fs::write(
        &hostile,
        "Host *\n  RequestTTY yes\n  StdinNull yes\n  SessionType none\n  ForkAfterAuthentication yes\n  RemoteCommand \
         tmux attach\n",
    )
    .unwrap();
    let mut with_config = connection("example.invalid");
    with_config.config = Some(hostile.display().to_string());
    let argv = install_command(&with_config, "/usr/bin/ssh").unwrap();
    let resolved = Process::new(ssh).arg("-G").args(&argv[1..]).output().unwrap();
    assert!(
        resolved.status.success(),
        "{}",
        String::from_utf8_lossy(&resolved.stderr)
    );
    let settings: HashMap<String, String> = String::from_utf8_lossy(&resolved.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    assert_eq!(settings.get("requesttty").map(String::as_str), Some("false"));
    assert_eq!(settings.get("stdinnull").map(String::as_str), Some("no"));
    assert_eq!(settings.get("sessiontype").map(String::as_str), Some("default"));
    assert_eq!(settings.get("forkafterauthentication").map(String::as_str), Some("no"));
    assert_eq!(settings.get("remotecommand"), None);
}

#[test]
fn the_remote_command_is_one_quoted_sh_invocation_on_one_line() {
    let argv = install_command(&connection("buildbox"), "ssh").unwrap();
    let remote = argv.last().unwrap();
    assert!(remote.starts_with("'/bin/sh' '-c' '"), "{remote}");
    assert!(!remote.contains('\n'));
}

/// The installed login shells among those upstream runs the remote command under.
fn login_shells() -> Vec<&'static str> {
    ["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash", "/bin/tcsh", "/bin/csh"]
        .into_iter()
        .filter(|shell| Path::new(shell).exists())
        .collect()
}

#[test]
fn the_remote_command_compiles_the_source_under_every_login_shell() {
    let remote = install_command(&connection("buildbox"), "ssh").unwrap().pop().unwrap();
    for shell in login_shells() {
        let fixture = Fixture::new();
        let (home, environment) = fixture.remote_home(true);
        let mut child = Process::new(shell)
            .args(["-c", &remote])
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"xterm-ghostty|test,\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let terminfo = home.join(".terminfo");
        assert_eq!(
            fixture.read("tic.args").lines().collect::<Vec<_>>(),
            ["-x", "-o", &terminfo.display().to_string(), "-"],
            "{shell}"
        );
        assert_eq!(fixture.read("tic.stdin"), "xterm-ghostty|test,\n");
        assert!(terminfo.is_dir());
    }
}

#[test]
fn the_remote_command_says_so_and_exits_3_when_the_host_has_no_tic() {
    let remote = install_command(&connection("buildbox"), "ssh").unwrap().pop().unwrap();
    for shell in login_shells() {
        let fixture = Fixture::new();
        let (home, environment) = fixture.remote_home(false);
        let output = Process::new(shell)
            .args(["-c", &remote])
            .env_clear()
            .envs(environment)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(3), "{shell}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("tic is not installed on this host"));
        assert!(!home.join(".terminfo").exists());
    }
}

#[test]
fn an_option_looking_or_unprintable_destination_is_refused() {
    for destination in ["", "-G", "-N", "host name", "host\tname", "\u{1b}host", "host\u{7f}"] {
        assert_eq!(
            install_command(&connection(destination), "ssh"),
            Err(InstallFailure::InvalidDestination),
            "{destination:?}"
        );
    }
}

// -- the pipeline, with fake executables

#[test]
fn the_dump_reaches_ssh_stdin_byte_for_byte_and_ends_in_eof() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.fake_infocmp("xterm-ghostty|test,\n\tcols#80,\n");
    let ssh = fixture.fake_ssh(0, true, "");
    let outcome = run(
        &connection("buildbox"),
        None,
        &env(&[("TERMINFO", &database)]),
        &ssh,
        &infocmp,
    );
    assert_eq!(outcome, Ok(Outcome::Exited(0)));
    assert_eq!(fixture.read("ssh.stdin"), "xterm-ghostty|test,\n\tcols#80,\n");
    let arguments = fixture.ssh_arguments();
    assert_eq!(arguments[0], "-T");
    assert!(arguments.contains(&"buildbox".to_owned()));
}

#[test]
fn a_failed_dump_never_starts_ssh() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.failing_infocmp("infocmp: no terminfo file for xterm-ghostty");
    let ssh = fixture.fake_ssh(0, true, "");
    let outcome = run(
        &connection("buildbox"),
        None,
        &env(&[("TERMINFO", &database)]),
        &ssh,
        &infocmp,
    );
    assert_eq!(
        outcome,
        Err(InstallFailure::DumpFailed {
            status: 1,
            stderr: "infocmp: no terminfo file for xterm-ghostty\n".to_owned()
        })
    );
    assert!(!fixture.path("ssh.args").exists());
}

#[test]
fn a_missing_entry_never_starts_anything() {
    let fixture = Fixture::new();
    let infocmp = fixture.fake_infocmp("unused");
    let ssh = fixture.fake_ssh(0, true, "");
    let outcome = run(&connection("buildbox"), None, &HashMap::new(), &ssh, &infocmp);
    assert_eq!(outcome, Err(InstallFailure::TerminfoNotFound { searched: vec![] }));
    assert!(!fixture.path("ssh.args").exists());
}

#[test]
fn a_non_zero_remote_status_is_reported_as_itself() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.fake_infocmp("x");
    let ssh = fixture.fake_ssh(3, true, "");
    let outcome = run(
        &connection("buildbox"),
        None,
        &env(&[("TERMINFO", &database)]),
        &ssh,
        &infocmp,
    );
    assert_eq!(outcome, Ok(Outcome::Exited(3)));
}

#[test]
fn a_missing_ssh_is_a_spawn_failure() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.fake_infocmp("x");
    let missing = fixture.path("no-ssh").display().to_string();
    let outcome = run(
        &connection("buildbox"),
        None,
        &env(&[("TERMINFO", &database)]),
        &missing,
        &infocmp,
    );
    assert_eq!(
        outcome,
        Err(InstallFailure::SpawnFailed {
            operation: "posix_spawnp".to_owned(),
            reason: "No such file or directory".to_owned()
        })
    );
}

#[test]
fn ssh_stays_in_the_callers_process_group() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.fake_infocmp("x");
    let record = fixture.path("ssh.pgid");
    let ssh = fixture.fake_ssh(
        0,
        true,
        &format!("ps -o pgid= -p $$ | tr -d ' ' > '{}'", record.display()),
    );
    let environment = env(&[("TERMINFO", &database), ("PATH", Path::new("/bin:/usr/bin"))]);
    let outcome = run(&connection("buildbox"), None, &environment, &ssh, &infocmp);
    assert_eq!(outcome, Ok(Outcome::Exited(0)));
    // SAFETY: `getpgrp` only reads the calling process's group.
    let group = unsafe { libc::getpgrp() };
    assert_eq!(fixture.read("ssh.pgid").trim(), group.to_string());
}

#[test]
fn ssh_inherits_no_descriptor_beyond_its_standard_three() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.fake_infocmp("x");
    // a descriptor without close-on-exec, as one a caller leaked would be
    let mut stray = [-1; 2];
    // SAFETY: `stray` is a valid two-element array for `pipe` to fill.
    assert_eq!(unsafe { libc::pipe(stray.as_mut_ptr()) }, 0);
    let record = fixture.path("ssh.fd");
    let probe = format!(
        "if [ -e /dev/fd/{fd} ]; then echo inherited; else echo absent; fi > '{}'",
        record.display(),
        fd = stray[1]
    );
    let ssh = fixture.fake_ssh(0, true, &probe);
    let environment = env(&[("TERMINFO", &database), ("PATH", Path::new("/bin:/usr/bin"))]);
    let outcome = run(&connection("buildbox"), None, &environment, &ssh, &infocmp);
    for fd in stray {
        // SAFETY: closing the two descriptors `pipe` opened above, once each.
        unsafe { libc::close(fd) };
    }
    assert_eq!(outcome, Ok(Outcome::Exited(0)));
    assert_eq!(fixture.read("ssh.fd").trim(), "absent");
}

/// Runs `body` with `signal` blocked on this thread only, the previous mask restored after.
fn with_blocked<T>(signal: i32, body: impl FnOnce() -> T) -> T {
    // SAFETY: a zeroed set is initialized by `sigemptyset` before use.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: a zeroed set is written by `pthread_sigmask` before use.
    let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `blocked` is a valid set.
    unsafe { libc::sigemptyset(&raw mut blocked) };
    // SAFETY: `blocked` is a valid set and `signal` a valid signal.
    unsafe { libc::sigaddset(&raw mut blocked, signal) };
    // SAFETY: both sets are valid; the mask is this thread's.
    let set = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const blocked, &raw mut previous) };
    assert_eq!(set, 0);
    let result = body();
    // SAFETY: `previous` was written by the call above.
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const previous, std::ptr::null_mut()) };
    result
}

#[test]
fn ssh_starts_with_signals_unblocked_whatever_the_caller_blocks() {
    let fixture = Fixture::new();
    let database = fixture.database("env", "78");
    let infocmp = fixture.fake_infocmp("x");
    let ssh = fixture.fake_ssh(0, true, "kill -TERM $$");
    let environment = env(&[("TERMINFO", &database), ("PATH", Path::new("/bin:/usr/bin"))]);
    let outcome = with_blocked(libc::SIGTERM, || {
        run(&connection("buildbox"), None, &environment, &ssh, &infocmp)
    });
    assert_eq!(outcome, Ok(Outcome::Signaled(libc::SIGTERM)));
}

#[test]
fn an_ssh_that_exits_before_reading_stdin_still_reports_its_status() {
    // with SIGPIPE at its default, as the real binary runs (a real infocmp dump stays under a pipe buffer, so only a
    // fake one reaches this end to end)
    crate::local::default_sigpipe::run(
        "terminfo::tests::an_ssh_that_exits_before_reading_stdin_still_reports_its_status",
        || {
            let fixture = Fixture::new();
            let database = fixture.database("env", "78");
            // larger than a pipe buffer, so the write would block or EPIPE against a reader that has gone
            let infocmp = fixture.fake_infocmp(&"x".repeat(1 << 17));
            let ssh = fixture.fake_ssh(255, false, "");
            let outcome = run(
                &connection("buildbox"),
                None,
                &env(&[("TERMINFO", &database)]),
                &ssh,
                &infocmp,
            );
            assert_eq!(outcome, Ok(Outcome::Exited(255)));
        },
    );
}

// -- the real infocmp against a real compiled entry

#[test]
fn the_real_infocmp_dumps_an_entry_tic_compiled() {
    if !Path::new("/usr/bin/tic").exists() || !Path::new(INFOCMP).exists() {
        return;
    }
    let fixture = Fixture::new();
    let database = fixture.compiled_database("xterm-ghostty|agterm test entry,\n\tcols#80, lines#24,\n");
    let ssh = fixture.fake_ssh(0, true, "");
    let outcome = run(
        &connection("buildbox"),
        None,
        &env(&[("TERMINFO", &database)]),
        &ssh,
        Path::new(INFOCMP),
    );
    assert_eq!(outcome, Ok(Outcome::Exited(0)));
    let dumped = fixture.read("ssh.stdin");
    assert!(dumped.contains("xterm-ghostty|agterm test entry"), "{dumped}");
    assert!(dumped.contains("cols#80"), "{dumped}");
}
