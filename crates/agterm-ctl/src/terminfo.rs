//! `agtermctl terminfo install` (`agtermctlKit/TerminfoCommands.swift`, `agtermCore/TerminfoInstall.swift`): installs
//! the bundled `xterm-ghostty` terminfo entry on a remote host over one ssh connection. A local `infocmp` dumps the
//! entry as source and the remote `tic` compiles it into that account's `~/.terminfo`. Local-only: it never opens the
//! control socket, so it has no `--socket` or `--json` and needs no running agterm.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use agterm_core::command_restore::shell_quoted_line;

use crate::Failure;
use crate::local::{SigpipeIgnored, close_inherited_descriptors, exit_status, reset_signals};
use crate::version::client_path;

/// The entry this installs.
pub const ENTRY: &str = "xterm-ghostty";

/// The local `infocmp` (upstream's default).
const INFOCMP: &str = "/usr/bin/infocmp";

/// What the remote account runs, with the source on stdin. One line, because the account's login shell parses the
/// quoted command before `/bin/sh` sees it and tcsh rejects a quoted newline.
pub const REMOTE_SCRIPT: &str = "command -v tic >/dev/null 2>&1 || { printf '%s\\n' 'agterm: tic is not installed on \
                                 this host, install ncurses first' >&2; exit 3; }; mkdir -p \"$HOME/.terminfo\" && \
                                 exec tic -x -o \"$HOME/.terminfo\" -";

/// Installer-owned execution settings; a command-line `-o` wins over any config file, and each of these set the other
/// way would discard the source, skip or detach the command, or refuse it.
pub const EXECUTION_OPTIONS: [&str; 9] = [
    "-T",
    "-o",
    "StdinNull=no",
    "-o",
    "SessionType=default",
    "-o",
    "ForkAfterAuthentication=no",
    "-o",
    "RemoteCommand=none",
];

/// `agtermctl terminfo <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "The terminal's terminfo entry on other hosts.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Terminfo {
    #[command(subcommand)]
    command: TerminfoCommand,
}

/// The subcommands, in upstream's `Terminfo.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum TerminfoCommand {
    Install(Install),
}

impl Terminfo {
    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        match &self.command {
            TerminfoCommand::Install(command) => command.validate(),
        }
    }

    /// Runs the command.
    pub fn execute(&self) -> Result<(), Failure> {
        match &self.command {
            TerminfoCommand::Install(command) => command.run(),
        }
    }
}

/// `agtermctl terminfo install [-p <port>] [-i <file> ...] [-J <host>] [-F <file>] <destination>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Install the xterm-ghostty terminfo entry on a remote host over ssh.",
    long_about = "agterm's shells run with TERM=xterm-ghostty. A host without that entry makes less, vim and other \
                  full-screen programs warn that the terminal is not fully functional. This dumps the bundled entry \
                  with infocmp and compiles it into the remote account's ~/.terminfo with tic, over one ssh \
                  connection. Run it once per host and account; nothing is cached and ssh itself is untouched.\n\nThe \
                  connection is interactive: a password or host-key prompt is answered on this terminal. Other \
                  connection settings belong in ~/.ssh/config under a host alias, which ssh reads as usual; the \
                  settings that decide how the command itself runs (no pty, stdin kept, a plain session, no fork, no \
                  RemoteCommand) are set by the installer and win over the config. The remote needs tic, which comes \
                  with ncurses, and says so when it is missing. Exits with ssh's status."
)]
pub struct Install {
    #[arg(help = "The host, as ssh would take it: host, user@host, or an alias from ~/.ssh/config.")]
    destination: String,

    #[arg(short = 'p', value_name = "port", help = "Port to connect to on the remote host.")]
    port: Option<i64>,

    #[arg(
        short = 'i',
        value_name = "file",
        action = clap::ArgAction::Append,
        help = "Identity file for public key authentication. Repeat to offer several."
    )]
    identity: Vec<String>,

    #[arg(short = 'J', value_name = "host", help = "Jump host, as ssh -J takes it.")]
    jump: Option<String>,

    #[arg(short = 'F', value_name = "file", help = "Per-user ssh configuration file.")]
    config: Option<String>,
}

impl Install {
    fn connection(&self) -> Connection {
        Connection {
            destination: self.destination.clone(),
            port: self.port,
            identities: self.identity.clone(),
            jump: self.jump.clone(),
            config: self.config.clone(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        install_command(&self.connection(), "ssh").map(drop).map_err(|_| {
            format!(
                "destination must be a host, user@host or an ssh alias, not '{}'",
                self.destination
            )
        })
    }

    fn run(&self) -> Result<(), Failure> {
        let environment: HashMap<OsString, OsString> = std::env::vars_os().collect();
        let outcome = run(
            &self.connection(),
            client_path().as_deref(),
            &environment,
            "ssh",
            Path::new(INFOCMP),
        )
        .map_err(|failure| Failure::Local(failure.message()))?;
        match outcome {
            Outcome::Exited(0) => {
                println!("installed {ENTRY} on {}", self.destination);
                Ok(())
            }
            Outcome::Exited(status) => {
                eprintln!("ssh exited {status}; {ENTRY} was not installed");
                Err(Failure::Exit(exit_code(status)))
            }
            Outcome::Signaled(signal) => {
                eprintln!("ssh was killed by signal {signal}; {ENTRY} was not installed");
                Err(Failure::Exit(exit_code(128 + signal)))
            }
        }
    }
}

/// `status` as a process exit status (its low byte, as `exit` keeps it).
fn exit_code(status: i32) -> ExitCode {
    ExitCode::from(u8::try_from(status & 0xff).unwrap_or(u8::MAX))
}

/// The connection, as a narrower surface than ssh's own (upstream's `TerminfoInstall.Connection`). A verbatim argument
/// bag would let `-G` print the config and claim success, `-N` never run `tic`, and `-n` discard the source; anything
/// beyond these belongs in the user's ssh config under a host alias.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connection {
    pub destination: String,
    pub port: Option<i64>,
    pub identities: Vec<String>,
    pub jump: Option<String>,
    pub config: Option<String>,
}

/// Why an install did not reach ssh (upstream's `TerminfoInstall.Failure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallFailure {
    /// The destination is empty, contains whitespace or a control character, or starts with `-`, which ssh would read
    /// as an option.
    InvalidDestination,
    /// No terminfo database holding the entry was found at the searched paths.
    TerminfoNotFound { searched: Vec<PathBuf> },
    /// `infocmp` exited non-zero; its stderr is carried for the user.
    DumpFailed { status: i32, stderr: String },
    /// `infocmp` could not be started (upstream lets Foundation's own error through).
    DumpUnavailable { infocmp: PathBuf, reason: String },
    /// A system call on the way to running ssh failed with this OS error text.
    SpawnFailed { operation: String, reason: String },
}

impl InstallFailure {
    /// The text the CLI prints after `Error: ` (upstream's `Terminfo.Install.run()` catch arms): a runtime failure,
    /// with no usage block under it.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::InvalidDestination => "invalid destination".to_owned(),
            Self::TerminfoNotFound { searched } => format!(
                "no {ENTRY} terminfo entry next to this agtermctl; looked in {}",
                searched
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::DumpFailed { status, stderr } => format!("infocmp exited {status}: {}", stderr.trim()),
            Self::DumpUnavailable { infocmp, reason } => format!("could not start {}: {reason}", infocmp.display()),
            Self::SpawnFailed { operation, reason } => {
                format!("could not start ssh: {operation} failed with {reason}")
            }
        }
    }
}

/// How the remote side ended: ssh's exit status, or the signal that killed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Exited(i32),
    Signaled(i32),
}

/// A host or remote-session token: not empty, no whitespace, no control characters (upstream's
/// `RemoteSession.isPlain`).
#[must_use]
pub fn is_plain(value: &str) -> bool {
    !value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_whitespace() || u32::from(c) < 0x20 || u32::from(c) == 0x7f)
}

/// Where the entry is looked up, in order. The database beside the running CLI comes first, because the stage is the
/// only source whose entry is known to match this build: `<prefix>/share/terminfo` for `<prefix>/bin/agtermctl` (the
/// staging layout, upstream's `Contents/Resources/terminfo` beside `Contents/MacOS`). `TERMINFO` from the environment
/// is the fallback for a CLI running outside a stage, such as a cargo build inside an agterm shell, where libghostty
/// has exported the app's own database. `client` is the CLI's resolved real path.
#[must_use]
pub fn candidate_directories(client: Option<&Path>, environment: &HashMap<OsString, OsString>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(client) = client {
        let bin = client.parent().unwrap_or_else(|| Path::new("/"));
        let prefix = bin.parent().unwrap_or_else(|| Path::new("/"));
        candidates.push(prefix.join("share/terminfo"));
    }
    if let Some(terminfo) = environment
        .get(&OsString::from("TERMINFO"))
        .filter(|value| !value.is_empty())
    {
        candidates.push(PathBuf::from(terminfo));
    }
    candidates
}

/// The first candidate directory that holds the entry under either hashed layout: ncurses files it under the first
/// letter (`x/`), Darwin's under its hex code (`78/`).
#[must_use]
pub fn terminfo_directory(client: Option<&Path>, environment: &HashMap<OsString, OsString>) -> Option<PathBuf> {
    candidate_directories(client, environment)
        .into_iter()
        .find(|directory| {
            ["78", "x"]
                .iter()
                .any(|layout| directory.join(layout).join(ENTRY).exists())
        })
}

/// The ssh invocation. `-T` because the source travels on stdin and the remote must see EOF, which a pty would never
/// deliver. No `BatchMode`: this is run by hand, so a password or host-key prompt is expected and answered on the
/// terminal. The script runs through a quoted `/bin/sh -c` because sshd hands the command to the account's shell,
/// which need not parse POSIX syntax.
pub fn install_command(connection: &Connection, ssh: &str) -> Result<Vec<String>, InstallFailure> {
    let destination = &connection.destination;
    if !is_plain(destination) || destination.starts_with('-') {
        return Err(InstallFailure::InvalidDestination);
    }
    let mut argv = vec![ssh.to_owned()];
    argv.extend(EXECUTION_OPTIONS.iter().map(|option| (*option).to_owned()));
    if let Some(port) = connection.port {
        argv.extend(["-p".to_owned(), port.to_string()]);
    }
    for identity in &connection.identities {
        argv.extend(["-i".to_owned(), identity.clone()]);
    }
    if let Some(jump) = &connection.jump {
        argv.extend(["-J".to_owned(), jump.clone()]);
    }
    if let Some(config) = &connection.config {
        argv.extend(["-F".to_owned(), config.clone()]);
    }
    argv.push(destination.clone());
    argv.push(shell_quoted_line(&["/bin/sh", "-c", REMOTE_SCRIPT]));
    Ok(argv)
}

/// Dumps locally, then installs remotely. The dump completes before ssh starts, so a missing or broken local entry
/// never opens a connection. ssh inherits stdout and stderr, which is where its prompts and the remote diagnostics
/// go. `ssh` and `infocmp` are injectable so tests can stand in fakes.
pub fn run(
    connection: &Connection,
    client: Option<&Path>,
    environment: &HashMap<OsString, OsString>,
    ssh: &str,
    infocmp: &Path,
) -> Result<Outcome, InstallFailure> {
    let argv = install_command(connection, ssh)?;
    let Some(directory) = terminfo_directory(client, environment) else {
        return Err(InstallFailure::TerminfoNotFound {
            searched: candidate_directories(client, environment),
        });
    };
    let source = dump(&directory, infocmp)?;
    install(&argv, &source, environment)
}

/// `infocmp -x` so the entry's extended capabilities (the ones worth installing it for) come along, with `TERMINFO`
/// the child's whole environment so it reads the chosen database and nothing else. Both pipes are drained before the
/// wait, or a dump larger than the pipe buffer would deadlock.
fn dump(directory: &Path, infocmp: &Path) -> Result<Vec<u8>, InstallFailure> {
    let output = Command::new(infocmp)
        .args(["-x", ENTRY])
        .env_clear()
        .env("TERMINFO", directory)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| InstallFailure::DumpUnavailable {
            infocmp: infocmp.to_path_buf(),
            reason: crate::client::os_message(&error),
        })?;
    if !output.status.success() {
        return Err(InstallFailure::DumpFailed {
            status: exit_status(output.status),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(output.stdout)
}

/// Runs ssh with the source on its stdin. The child stays in the caller's process group (ssh reads its password and
/// host-key prompts from the controlling tty, and a background group doing that is stopped by SIGTTIN), inherits only
/// its standard three descriptors (upstream's `POSIX_SPAWN_CLOEXEC_DEFAULT`), and starts with every signal unblocked and
/// at its default action, whatever the caller blocks or ignores.
fn install(
    argv: &[String],
    source: &[u8],
    environment: &HashMap<OsString, OsString>,
) -> Result<Outcome, InstallFailure> {
    let (program, arguments) = argv.split_first().ok_or_else(|| InstallFailure::SpawnFailed {
        operation: "posix_spawnp".to_owned(),
        reason: "no program".to_owned(),
    })?;
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // SAFETY: the hook only makes async-signal-safe calls (`sigemptyset` on its own stack, `sigprocmask`, `signal`,
    // `close_range`) and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            reset_signals();
            close_inherited_descriptors();
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| InstallFailure::SpawnFailed {
        operation: "posix_spawnp".to_owned(),
        reason: crate::client::os_message(&error),
    })?;
    if let Some(mut stdin) = child.stdin.take() {
        // an ssh that fails before reading would otherwise SIGPIPE the CLI before it can report; the EPIPE the write
        // gets instead is dropped because ssh's own stderr and status say what happened
        let _sigpipe = SigpipeIgnored::new();
        write_dropping_errors(&mut stdin, source);
    }
    let status = child.wait().map_err(|error| InstallFailure::SpawnFailed {
        operation: "waitpid".to_owned(),
        reason: crate::client::os_message(&error),
    })?;
    Ok(match status.signal() {
        Some(signal) => Outcome::Signaled(signal),
        None => Outcome::Exited(status.code().unwrap_or(-1)),
    })
}

/// Writes `bytes`, giving up quietly at the first error ([`io::Write::write_all`] retries `EINTR`).
fn write_dropping_errors(writer: &mut impl Write, bytes: &[u8]) {
    let _: io::Result<()> = writer.write_all(bytes);
}

#[cfg(test)]
mod tests;
