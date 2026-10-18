//! `agtermctl clipboard set` (`agtermctlKit/ClipboardCommands.swift`, `agtermCore/TerminalClipboard.swift`): prints an
//! OSC 52 clipboard write into the caller's pane through its live-session daemon (`print` of the bundled zmx), so the
//! write reaches every terminal attached to the pane. Local-only: it never opens the control socket, so it has no
//! `--socket` or `--json`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::Failure;
use crate::local::{SigpipeIgnored, exit_status};
use crate::version::client_path;

/// The encoded size limit: under the 8 MiB above which libghostty drops an OSC whole; the margin covers the `52;c;`
/// prefix and the terminator.
pub const MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024 - 64;

/// `agtermctl clipboard <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "The clipboard of the terminals showing this pane.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Clipboard {
    #[command(subcommand)]
    command: ClipboardCommand,
}

/// The subcommands, in upstream's `Clipboard.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum ClipboardCommand {
    Set(Set),
}

impl Clipboard {
    /// Runs the command.
    pub fn execute(&self) -> Result<(), Failure> {
        match &self.command {
            ClipboardCommand::Set(command) => command.run(),
        }
    }
}

/// `agtermctl clipboard set [<text>]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Copy text to the clipboard of every terminal showing this pane.",
    long_about = "Run it inside a main or split pane started under Live sessions. The text goes out through the pane \
                  itself, so it reaches the Mac you are looking at: for a session attached from another Mac that is \
                  the Mac showing it, where pbcopy would fill the clipboard of the Mac it runs on. Every terminal \
                  attached to the pane receives it, the origin included.\n\nThe text is the argument, or standard \
                  input when there is none, copied byte for byte. Text that starts with a dash would be read as an \
                  option: pipe it on standard input, or put -- before it. It needs no terminal, so an agent's shell \
                  tool can run it. Each receiving terminal applies its own clipboard-write setting, so one set to ask \
                  prompts and one set to deny drops the copy; exit status 0 does not confirm that the session or any \
                  terminal received it. A pane that is not backed by a live session is refused, and so is text over \
                  about 6 MB."
)]
pub struct Set {
    #[arg(
        help = "The text to copy. Read from standard input when omitted. Put -- before text that starts with a dash."
    )]
    text: Option<String>,
}

impl Set {
    fn run(&self) -> Result<(), Failure> {
        let text = self.text()?;
        let environment: HashMap<OsString, OsString> = std::env::vars_os().collect();
        copy(&text, client_path().as_deref(), &environment, None).map_err(|failure| Failure::Local(failure.message()))
    }

    /// The argument, else standard input read to its end, byte for byte; a terminal on stdin is refused, since
    /// nothing would end the read.
    fn text(&self) -> Result<Vec<u8>, Failure> {
        if let Some(text) = &self.text {
            return Ok(text.as_bytes().to_vec());
        }
        let mut stdin = io::stdin();
        if stdin.is_terminal() {
            return Err(Failure::Usage(
                "give the text as an argument or pipe it on standard input".to_owned(),
            ));
        }
        let mut data = Vec::new();
        stdin.read_to_end(&mut data).map_err(|error| {
            Failure::Local(format!(
                "could not read standard input: {}",
                crate::client::os_message(&error)
            ))
        })?;
        Ok(data)
    }
}

/// Why a copy did not go out (upstream's `TerminalClipboard.Failure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyFailure {
    EmptyText,
    TooLarge {
        encoded_bytes: usize,
    },
    /// The environment names no agterm zmx daemon: a scratch, quick or overlay terminal has none.
    NotLivePane,
    /// Where zmx was expected, `None` when the CLI's own path is unknown.
    ZmxNotFound {
        path: Option<PathBuf>,
    },
    PrintFailed {
        status: i32,
        stderr: String,
    },
    /// zmx could not be started at all (upstream lets Foundation's own error through).
    SpawnFailed {
        path: PathBuf,
        reason: String,
    },
}

impl CopyFailure {
    /// The text the CLI prints after `Error: ` (upstream's `Clipboard.Set.message(for:)`).
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::EmptyText => "nothing to copy".to_owned(),
            Self::TooLarge { encoded_bytes } => format!(
                "text is too large to copy through the terminal: {encoded_bytes} bytes encoded, the limit is \
                 {MAX_ENCODED_BYTES}"
            ),
            Self::NotLivePane => {
                "this pane has no live session; run it in a main or split pane started under Live sessions".to_owned()
            }
            Self::ZmxNotFound { path: None } => "no live-session executable next to this agtermctl".to_owned(),
            Self::ZmxNotFound { path: Some(path) } => {
                format!(
                    "no live-session executable next to this agtermctl; looked for {}",
                    path.display()
                )
            }
            Self::PrintFailed { status, stderr } => {
                format!("the live-session print exited {status}: {}", stderr.trim())
            }
            Self::SpawnFailed { path, reason } => format!("could not start {}: {reason}", path.display()),
        }
    }
}

/// The OSC 52 clipboard write of `text`, ending in BEL.
pub fn sequence(text: &[u8]) -> Result<Vec<u8>, CopyFailure> {
    if text.is_empty() {
        return Err(CopyFailure::EmptyText);
    }
    let encoded = STANDARD.encode(text);
    if encoded.len() > MAX_ENCODED_BYTES {
        return Err(CopyFailure::TooLarge {
            encoded_bytes: encoded.len(),
        });
    }
    let mut sequence = b"\x1b]52;c;".to_vec();
    sequence.extend_from_slice(encoded.as_bytes());
    sequence.push(0x07);
    Ok(sequence)
}

/// The pane's zmx daemon name. Only a name agterm would have created is accepted, so a `ZMX_SESSION` inherited from an
/// unrelated zmx session is never printed into; `ZMX_DIR` must be set because zmx reads it from the environment.
pub fn daemon(environment: &HashMap<OsString, OsString>) -> Result<String, CopyFailure> {
    let session = environment
        .get(&OsString::from("ZMX_SESSION"))
        .and_then(|value| value.to_str())
        .filter(|name| agterm_core::live::is_daemon_name(name));
    let directory = environment
        .get(&OsString::from("ZMX_DIR"))
        .filter(|value| !value.is_empty());
    match (session, directory) {
        (Some(session), Some(_)) => Ok(session.to_owned()),
        _ => Err(CopyFailure::NotLivePane),
    }
}

/// The zmx beside the CLI (`bin/zmx` in the staging layout, upstream's `Contents/MacOS`); `client` is the CLI's
/// resolved real path. `PATH` is not searched: only the bundled zmx is known to match this build.
pub fn zmx_path(client: Option<&Path>) -> Result<PathBuf, CopyFailure> {
    let Some(client) = client else {
        return Err(CopyFailure::ZmxNotFound { path: None });
    };
    let path = client.parent().unwrap_or_else(|| Path::new("/")).join("zmx");
    if is_executable_file(&path) {
        Ok(path)
    } else {
        Err(CopyFailure::ZmxNotFound { path: Some(path) })
    }
}

/// Whether `path` is a regular file this process may execute (Foundation's `isExecutableFile`).
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call; `access` only reads it.
    metadata.is_file() && (unsafe { libc::access(c_path.as_ptr(), libc::X_OK) }) == 0
}

/// Prints `text` as an OSC 52 write into the pane's daemon with `zmx print`, returning once zmx exits zero. That
/// confirms neither that the daemon received the write nor that a terminal applied it: zmx reads no reply, and each
/// terminal applies its own clipboard-write policy. `zmx` overrides the executable beside the CLI (tests).
pub fn copy(
    text: &[u8],
    client: Option<&Path>,
    environment: &HashMap<OsString, OsString>,
    zmx: Option<&Path>,
) -> Result<(), CopyFailure> {
    let sequence = sequence(text)?;
    let daemon = daemon(environment)?;
    let executable = match zmx {
        Some(zmx) => zmx.to_path_buf(),
        None => zmx_path(client)?,
    };
    let spawn_failed = |error: io::Error| CopyFailure::SpawnFailed {
        path: executable.clone(),
        reason: crate::client::os_message(&error),
    };
    let mut child = Command::new(&executable)
        .args(["print", &daemon])
        .env_clear()
        .envs(environment)
        // empty is unset to zmx; an inherited prefix would resolve a name agterm never created
        .env("ZMX_SESSION", "")
        .env("ZMX_SESSION_PREFIX", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_failed)?;
    let stdin = child.stdin.take();
    // a zmx that exits before reading everything must surface as its status, not kill this process
    let _sigpipe = SigpipeIgnored::new();
    let diagnostics = thread::scope(|scope| {
        if let Some(mut stdin) = stdin {
            let sequence = &sequence;
            scope.spawn(move || {
                // EPIPE is dropped: zmx's own status and stderr say what happened
                let _ = stdin.write_all(sequence);
            });
        }
        let mut diagnostics = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_end(&mut diagnostics);
        }
        diagnostics
    });
    let status = child.wait().map_err(spawn_failed)?;
    if status.success() {
        return Ok(());
    }
    Err(CopyFailure::PrintFailed {
        status: exit_status(status),
        stderr: String::from_utf8_lossy(&diagnostics).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use clap::Parser;

    use super::*;
    use crate::{Agtermctl, Command as Cli};

    const DAEMON: &str = "agterm-357e181888f34450b5c225bd00239b9b";

    fn live() -> HashMap<OsString, OsString> {
        env(&[("ZMX_SESSION", DAEMON), ("ZMX_DIR", "/tmp/agterm-zmx-1")])
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<OsString, OsString> {
        pairs
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect()
    }

    fn parse(argv: &[&str]) -> Result<Option<String>, clap::Error> {
        let line = ["agtermctl", "clipboard", "set"].iter().chain(argv).copied();
        let Cli::Clipboard(group) = Agtermctl::try_parse_from(line)?.command else {
            panic!("not a clipboard command")
        };
        let ClipboardCommand::Set(set) = group.command;
        Ok(set.text)
    }

    #[test]
    fn set_takes_its_text_as_the_argument_or_leaves_it_to_stdin() {
        assert_eq!(parse(&["hello world"]).unwrap().as_deref(), Some("hello world"));
        assert_eq!(parse(&[]).unwrap(), None);
    }

    #[test]
    fn set_takes_dash_leading_text_only_after_a_terminator() {
        for text in ["--force", "- item one", "-1"] {
            assert!(parse(&[text]).is_err(), "{text}");
            assert_eq!(parse(&["--", text]).unwrap().as_deref(), Some(text));
        }
    }

    #[test]
    fn set_refuses_a_second_argument_and_every_control_option() {
        for argv in [
            &["one", "two"][..],
            &["--socket", "/tmp/s"],
            &["--target", "active"],
            &["--json"],
        ] {
            assert!(parse(argv).is_err(), "{argv:?}");
        }
    }

    #[test]
    fn every_failure_has_its_own_message() {
        let cases = [
            (CopyFailure::EmptyText, "nothing to copy"),
            (
                CopyFailure::TooLarge {
                    encoded_bytes: 9_000_000,
                },
                "text is too large to copy through the terminal: 9000000 bytes encoded, the limit is 8388544",
            ),
            (
                CopyFailure::NotLivePane,
                "this pane has no live session; run it in a main or split pane started under Live sessions",
            ),
            (
                CopyFailure::ZmxNotFound { path: None },
                "no live-session executable next to this agtermctl",
            ),
            (
                CopyFailure::ZmxNotFound {
                    path: Some(PathBuf::from("/x/zmx")),
                },
                "no live-session executable next to this agtermctl; looked for /x/zmx",
            ),
            (
                CopyFailure::PrintFailed {
                    status: 1,
                    stderr: "session not found\n".to_owned(),
                },
                "the live-session print exited 1: session not found",
            ),
        ];
        for (failure, message) in cases {
            assert_eq!(failure.message(), message);
        }
    }

    #[test]
    fn the_sequence_is_an_osc52_clipboard_write_ending_in_bel() {
        assert_eq!(sequence("héllo\n".as_bytes()).unwrap(), b"\x1b]52;c;aMOpbGxvCg==\x07");
        assert_eq!(sequence(b""), Err(CopyFailure::EmptyText));
    }

    #[test]
    fn text_encoding_to_exactly_the_limit_is_accepted_and_past_it_refused() {
        let at_limit = vec![0; MAX_ENCODED_BYTES / 4 * 3];
        assert_eq!(sequence(&at_limit).unwrap().len(), MAX_ENCODED_BYTES + 8);
        let past = vec![0; MAX_ENCODED_BYTES / 4 * 3 + 1];
        assert_eq!(
            sequence(&past),
            Err(CopyFailure::TooLarge {
                encoded_bytes: MAX_ENCODED_BYTES + 4
            })
        );
    }

    #[test]
    fn the_daemon_is_the_one_the_pane_was_given() {
        assert_eq!(daemon(&live()).unwrap(), DAEMON);
    }

    #[test]
    fn an_environment_that_names_no_agterm_daemon_is_not_a_live_pane() {
        let cases: [&[(&str, &str)]; 7] = [
            &[],
            &[("ZMX_SESSION", DAEMON)],
            &[("ZMX_DIR", "/tmp/agterm-zmx-1")],
            &[("ZMX_SESSION", ""), ("ZMX_DIR", "/tmp/agterm-zmx-1")],
            &[("ZMX_SESSION", DAEMON), ("ZMX_DIR", "")],
            &[("ZMX_SESSION", "notes"), ("ZMX_DIR", "/tmp/agterm-zmx-1")],
            &[("ZMX_SESSION", "agterm-notes"), ("ZMX_DIR", "/tmp/agterm-zmx-1")],
        ];
        for pairs in cases {
            assert_eq!(daemon(&env(pairs)), Err(CopyFailure::NotLivePane), "{pairs:?}");
        }
    }

    /// A temp root holding a fake zmx and what it recorded.
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

        fn script(path: &Path, body: &str) {
            agterm_host::fakes::write_executable(path, &format!("#!/bin/sh\n{body}\n")).unwrap();
        }

        /// `bin/agtermctl` (an empty file) with or without an executable `bin/zmx` beside it.
        fn bundle(&self, with_zmx: bool) -> (PathBuf, PathBuf) {
            let bin = self.path("stage/bin");
            fs::create_dir_all(&bin).unwrap();
            let client = bin.join("agtermctl");
            fs::write(&client, b"").unwrap();
            let zmx = bin.join("zmx");
            if with_zmx {
                Self::script(&zmx, "exit 0");
            }
            (client, zmx)
        }

        fn fake_zmx(&self, exit_code: i32, stderr: Option<&str>, reads_stdin: bool) -> PathBuf {
            let read = if reads_stdin {
                format!("cat > '{}'", self.path("stdin").display())
            } else {
                String::new()
            };
            let complain = stderr
                .map(|text| format!("printf '%s\\n' '{text}' >&2"))
                .unwrap_or_default();
            let zmx = self.path("zmx");
            Self::script(
                &zmx,
                &format!(
                    "printf '%s\\n' \"$@\" > '{args}'\nenv | grep '^ZMX_' | sort > '{env}'\n{read}\n{complain}\nexit \
                     {exit_code}",
                    args = self.path("args").display(),
                    env = self.path("env").display(),
                ),
            );
            zmx
        }

        fn lines(&self, name: &str) -> Vec<String> {
            fs::read_to_string(self.path(name))
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[test]
    fn zmx_is_the_executable_beside_the_client() {
        let fixture = Fixture::new();
        let (client, zmx) = fixture.bundle(true);
        assert_eq!(zmx_path(Some(&client)).unwrap(), zmx);
    }

    #[test]
    fn a_stage_without_zmx_reports_where_it_looked() {
        let fixture = Fixture::new();
        let (client, zmx) = fixture.bundle(false);
        assert_eq!(
            zmx_path(Some(&client)),
            Err(CopyFailure::ZmxNotFound { path: Some(zmx) })
        );
        assert_eq!(zmx_path(None), Err(CopyFailure::ZmxNotFound { path: None }));
    }

    #[test]
    fn copy_prints_the_sequence_into_the_panes_daemon_on_stdin() {
        let fixture = Fixture::new();
        let zmx = fixture.fake_zmx(0, None, true);
        copy(b"hello", None, &live(), Some(&zmx)).unwrap();
        assert_eq!(fixture.lines("args"), ["print", DAEMON]);
        assert_eq!(fs::read(fixture.path("stdin")).unwrap(), b"\x1b]52;c;aGVsbG8=\x07");
    }

    #[test]
    fn copy_clears_the_session_variables_and_keeps_the_socket_directory() {
        let fixture = Fixture::new();
        let zmx = fixture.fake_zmx(0, None, true);
        let mut environment = live();
        environment.insert("ZMX_SESSION_PREFIX".into(), "work-".into());
        copy(b"hello", None, &environment, Some(&zmx)).unwrap();
        assert_eq!(
            fixture.lines("env"),
            ["ZMX_DIR=/tmp/agterm-zmx-1", "ZMX_SESSION=", "ZMX_SESSION_PREFIX="]
        );
    }

    #[test]
    fn a_failed_print_carries_its_status_and_stderr() {
        let fixture = Fixture::new();
        let zmx = fixture.fake_zmx(3, Some("session not found"), true);
        assert_eq!(
            copy(b"hello", None, &live(), Some(&zmx)),
            Err(CopyFailure::PrintFailed {
                status: 3,
                stderr: "session not found\n".to_owned()
            })
        );
    }

    #[test]
    fn a_zmx_that_exits_without_reading_stdin_still_reports_its_status() {
        // with SIGPIPE at its default, as the real binary runs: the write past the pipe buffer must get EPIPE
        crate::local::default_sigpipe::run(
            "clipboard::tests::a_zmx_that_exits_without_reading_stdin_still_reports_its_status",
            || {
                let fixture = Fixture::new();
                let zmx = fixture.fake_zmx(4, None, false);
                let text = vec![0; 1024 * 1024];
                assert_eq!(
                    copy(&text, None, &live(), Some(&zmx)),
                    Err(CopyFailure::PrintFailed {
                        status: 4,
                        stderr: String::new()
                    })
                );
            },
        );
    }

    #[test]
    fn a_pane_that_is_not_live_never_starts_zmx() {
        let fixture = Fixture::new();
        let zmx = fixture.fake_zmx(0, None, true);
        assert_eq!(
            copy(b"hello", None, &HashMap::new(), Some(&zmx)),
            Err(CopyFailure::NotLivePane)
        );
        assert!(!fixture.path("args").exists());
    }
}
