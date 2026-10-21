//! `agterm-session`'s commands besides `server`: the attach contract's client and the api's commands, each reaching the
//! server of the namespace `AGTERM_LIVE_DIR` names (zmx's CLI, `src/main.zig` at `8bab1f0`, MIT, NOTICE, in what the
//! attach contract and agterm keep of it).
//!
//! ```text
//! agterm-session attach <name> [<program> <argument>...]   attach, creating the session to run the program
//! agterm-session list [--short]                            the sessions
//! agterm-session kill [--force] <name>...                  end sessions
//! agterm-session screen [--all] <name>                     the session's screen (with its scrollback)
//! agterm-session type <name>                               type standard input into the session
//! agterm-session print <name> [<text>...]                  write the text (else standard input) into its output
//! agterm-session version                                   this build, its protocol and the server's
//! ```
//!
//! # The attach contract
//!
//! The variables, each unset when empty (an attach line clears an inherited one by setting it empty):
//!
//! - `AGTERM_LIVE_DIR`: the namespace directory, whose server every command reaches (`attach` starts it, the others
//!   never do: a namespace without a server holds no session). Required by every command but `version`.
//! - `AGTERM_LIVE_SESSION`: set by the server in its sessions' programs, as `AGTERM_LIVE_DIR` is. An attach run where
//!   it is set (inside a session) is refused: it would nest the session's terminal in itself (zmx switched sessions
//!   there instead, which this server does not do).
//! - `AGTERM_LIVE_SESSION_PREFIX`: put before every session name a command is given.
//! - `AGTERM_LIVE_NO_DETACH_KEY`: Ctrl+\ goes to the program instead of detaching (`attach_tty::input`).
//! - `AGTERM_LIVE_MANAGED`: explicit leadership under that nonce (1 to 64 ASCII letters, digits and dashes; another
//!   value leaves the attach unmanaged): the client leads only by claiming and writes its role as the title
//!   `OSC 2;ngterm-live-role;<nonce>:<role>:<generation>` (`server::lead`). `AGTERM_LIVE_MANAGED_CLAIM` with it
//!   claims the lead at the attach. A Windows pseudoconsole between the client and its terminal passes on only the
//!   latest title it holds when it paints, so there a role title the program's own title follows at once is lost.
//!
//! `attach <name> [argv]` is create-only: a session that exists is attached to and the argv ignored; one that does not
//! is created running the argv (the login shell without one), in the client's working directory and environment. An
//! attach whose argv is a guard (`/bin/sh -c "printf '…gone'; exit 1"`, `agterm-core::remote::ssh`) so never hands
//! back a fresh program under a session's name that went away.
//!
//! # Exit status
//!
//! 0 for success; `attach` exits with the session program's exit status once it ends (0 when the server cannot know
//! it, after an update handoff, and 1 for one past 255), 0 when the client detached; 1 when a command failed (a
//! session it names does not exist, no server could be reached, a kill that killed not every session named); 2 for a
//! command line it does not take, or `AGTERM_LIVE_DIR` unset.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::client::{Client, ClientError};
use crate::protocol::{ApiErrorKind, KillOutcome, Managed, PROTOCOL_VERSION, Screen, SessionRecord};
use crate::server::handoff;
use crate::server::lead::is_nonce;
use crate::server::pane::{DIRECTORY_VARIABLE, MANAGED_VARIABLES, SESSION_VARIABLE};
use crate::server::{EXIT_FAILED, EXIT_USAGE};

pub mod attach;

/// The prefix put before every session name a command is given.
pub const PREFIX_VARIABLE: &str = "AGTERM_LIVE_SESSION_PREFIX";
/// Set, the detach key goes to the program.
pub const NO_DETACH_KEY_VARIABLE: &str = "AGTERM_LIVE_NO_DETACH_KEY";
/// The managed attach's nonce.
pub const MANAGED_VARIABLE: &str = MANAGED_VARIABLES[0];
/// Set with [`MANAGED_VARIABLE`], the attach claims the lead.
pub const CLAIM_VARIABLE: &str = MANAGED_VARIABLES[1];

/// A command's success.
pub const EXIT_OK: u8 = 0;

const USAGE: &str = "usage: agterm-session <command>

  attach <name> [<program> <argument>...]   attach to a live session, creating it to run the program
  list [--short]                            list the sessions
  kill [--force] <name>...                  end sessions
  screen [--all] <name>                     print a session's screen, with its scrollback under --all
  type <name>                               type standard input into a session
  print <name> [<text>...]                  write text (else standard input) into a session's output
  version                                   print this build, its protocol and the running server's
  server <namespace directory>              run the server (started by an attach)
    [--handoff-import <channel> <token>]    taking over an updated server's sessions (started by its handoff)

The namespace is the directory AGTERM_LIVE_DIR names.";

/// The attach contract's settings as the environment gives them (module docs); an empty variable is unset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contract {
    /// `AGTERM_LIVE_DIR`.
    pub directory: Option<PathBuf>,
    /// `AGTERM_LIVE_SESSION`.
    pub session: Option<String>,
    /// `AGTERM_LIVE_SESSION_PREFIX`.
    pub prefix: String,
    /// Ctrl+\ detaches (no `AGTERM_LIVE_NO_DETACH_KEY`).
    pub detach_key: bool,
    /// `AGTERM_LIVE_MANAGED` (a valid nonce) and `AGTERM_LIVE_MANAGED_CLAIM`.
    pub managed: Option<Managed>,
}

impl Contract {
    /// The settings `get` reads, a variable's value or `None`.
    pub fn from_env(get: impl Fn(&str) -> Option<OsString>) -> Self {
        let value = |name: &str| get(name).filter(|value| !value.is_empty());
        let text = |name: &str| value(name).map(|value| value.to_string_lossy().into_owned());
        let managed = text(MANAGED_VARIABLE)
            .filter(|nonce| is_nonce(nonce))
            .map(|nonce| Managed {
                nonce,
                claim: value(CLAIM_VARIABLE).is_some(),
            });
        Self {
            directory: value(DIRECTORY_VARIABLE).map(PathBuf::from),
            session: text(SESSION_VARIABLE),
            prefix: text(PREFIX_VARIABLE).unwrap_or_default(),
            detach_key: value(NO_DETACH_KEY_VARIABLE).is_none(),
            managed,
        }
    }

    /// The settings of this process's environment.
    #[must_use]
    pub fn current() -> Self {
        Self::from_env(|name| std::env::var_os(name))
    }

    /// The session `name` names, the prefix before it (zmx's `getSeshName`).
    ///
    /// # Errors
    ///
    /// The name is empty, holds a `/` or a NUL, or is `.` or `..`.
    pub fn session_name(&self, name: &str) -> Result<String, String> {
        let full = format!("{}{name}", self.prefix);
        if full.is_empty() {
            return Err("a session name is required".to_owned());
        }
        if full.contains(['/', '\0']) || full == "." || full == ".." {
            return Err(format!("{full:?} is not a session name"));
        }
        Ok(full)
    }
}

/// A command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Attach { name: String, argv: Vec<String> },
    List { short: bool },
    Kill { names: Vec<String>, force: bool },
    Screen { name: String, all: bool },
    Type { name: String },
    Print { name: String, text: Option<String> },
    Version,
    Help,
}

impl Command {
    /// The command's name, for its messages.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Attach { .. } => "attach",
            Self::List { .. } => "list",
            Self::Kill { .. } => "kill",
            Self::Screen { .. } => "screen",
            Self::Type { .. } => "type",
            Self::Print { .. } => "print",
            Self::Version => "version",
            Self::Help => "help",
        }
    }
}

/// The command `args` (without the program's name) is.
///
/// # Errors
///
/// The usage error's message.
pub fn parse(args: &[OsString]) -> Result<Command, String> {
    let args: Vec<String> = args
        .iter()
        .map(|arg| {
            arg.to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{} is not UTF-8", arg.display()))
        })
        .collect::<Result<_, _>>()?;
    let Some((command, rest)) = args.split_first() else {
        return Err("a command is required".to_owned());
    };
    let rest = rest.to_vec();
    match command.as_str() {
        "attach" | "a" => {
            let (name, program) = rest.split_first().ok_or("attach needs a session name")?;
            Ok(Command::Attach {
                name: name.clone(),
                argv: program.to_vec(),
            })
        }
        "list" | "ls" | "l" => {
            let short = flags(&rest, &["--short"])?.contains(&"--short");
            positionals(&rest, 0, "list")?;
            Ok(Command::List { short })
        }
        "kill" | "k" => {
            let force = flags(&rest, &["--force"])?.contains(&"--force");
            let names = positionals(&rest, usize::MAX, "kill")?;
            if names.is_empty() {
                return Err("kill needs a session name".to_owned());
            }
            Ok(Command::Kill { names, force })
        }
        "screen" => {
            let all = flags(&rest, &["--all"])?.contains(&"--all");
            let name = one_name(&rest, "screen")?;
            Ok(Command::Screen { name, all })
        }
        "type" => Ok(Command::Type {
            name: one_name(&rest, "type")?,
        }),
        "print" | "p" => {
            let (name, text) = rest.split_first().ok_or("print needs a session name")?;
            Ok(Command::Print {
                name: name.clone(),
                text: (!text.is_empty()).then(|| text.join(" ")),
            })
        }
        "version" | "-v" | "--version" => Ok(Command::Version),
        "help" | "-h" | "--help" => Ok(Command::Help),
        // the binary runs `server <namespace directory>` and a handoff's `server <namespace directory>
        // --handoff-import <channel> <token>` itself, before these commands
        "server" => Err(match rest.as_slice() {
            [] => "server needs the namespace directory".to_owned(),
            [_] => "server runs only as agterm-session's own command line".to_owned(),
            [_, flag, _, _] if flag == handoff::IMPORT_FLAG => {
                "server runs only as agterm-session's own command line".to_owned()
            }
            [_, flag, ..] if flag == handoff::IMPORT_FLAG => {
                format!("server's {} takes a channel and a token", handoff::IMPORT_FLAG)
            }
            [_, extra, ..] => format!("server takes one namespace directory, not {extra:?} too"),
        }),
        other => Err(format!("unknown command {other:?}")),
    }
}

/// The flags among `args`, each one of `known`.
fn flags<'a>(args: &'a [String], known: &[&str]) -> Result<Vec<&'a str>, String> {
    args.iter()
        .filter(|arg| arg.starts_with("--"))
        .map(|arg| {
            known
                .contains(&arg.as_str())
                .then_some(arg.as_str())
                .ok_or_else(|| format!("unknown option {arg:?}"))
        })
        .collect()
}

/// The arguments that are no flags, at most `most`.
fn positionals(args: &[String], most: usize, command: &str) -> Result<Vec<String>, String> {
    let names: Vec<String> = args.iter().filter(|arg| !arg.starts_with("--")).cloned().collect();
    if names.len() > most {
        return Err(format!("{command} takes no argument {:?}", names[most]));
    }
    Ok(names)
}

fn one_name(args: &[String], command: &str) -> Result<String, String> {
    let mut names = positionals(args, 1, command)?;
    names.pop().ok_or_else(|| format!("{command} needs a session name"))
}

/// Runs `agterm-session` with `args` (without the program's name), every command but `server`.
#[must_use]
pub fn run(args: &[OsString]) -> ExitCode {
    let command = match parse(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("agterm-session: {message}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    let contract = Contract::current();
    let code = match command {
        Command::Attach { name, argv } => attach::run(&contract, &name, argv),
        command => execute(
            &command,
            &contract,
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        ),
    };
    ExitCode::from(code)
}

/// Runs an api command (every one but `attach`), its output to `out` and its complaints to `err`; its exit status.
pub fn execute(
    command: &Command,
    contract: &Contract,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let name = command.name();
    let result = match command {
        Command::Help => writeln!(out, "{USAGE}").map(|()| EXIT_OK).map_err(Failure::from),
        Command::Version => version(contract, out),
        command => match &contract.directory {
            Some(directory) => api_command(command, contract, directory, stdin, out, err),
            None => Err(Failure::Usage(format!(
                "{DIRECTORY_VARIABLE} is not set: it names the live sessions' namespace directory"
            ))),
        },
    };
    let _ = out.flush();
    match result {
        Ok(code) => code,
        Err(Failure::Usage(message)) => {
            let _ = writeln!(err, "agterm-session {name}: {message}");
            EXIT_USAGE
        }
        Err(Failure::Failed(message)) => {
            let _ = writeln!(err, "agterm-session {name}: {message}");
            EXIT_FAILED
        }
    }
}

/// Why a command failed.
#[derive(Debug)]
enum Failure {
    /// Exit status 2.
    Usage(String),
    /// Exit status 1.
    Failed(String),
}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        Self::Failed(error.to_string())
    }
}

fn api_command(
    command: &Command,
    contract: &Contract,
    directory: &Path,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<u8, Failure> {
    let client = Client::new(directory);
    let session = |name: &str| contract.session_name(name).map_err(Failure::Usage);
    match command {
        Command::List { short } => list(&client, *short, out, err),
        Command::Kill { names, force } => {
            let names = names.iter().map(|name| session(name)).collect::<Result<Vec<_>, _>>()?;
            kill(&client, &names, *force, out, err)
        }
        Command::Screen { name, all } => screen(&client, &session(name)?, *all, out),
        Command::Type { name } => {
            let name = session(name)?;
            let mut data = Vec::new();
            stdin.read_to_end(&mut data)?;
            if data.is_empty() {
                return Err(Failure::Failed(
                    "type reads the bytes to queue from standard input".to_owned(),
                ));
            }
            with_session(&client, &name, |api| api.type_input(&name, &data)).map(|()| EXIT_OK)
        }
        Command::Print { name, text } => {
            let name = session(name)?;
            let data = if let Some(text) = text {
                text.clone().into_bytes()
            } else {
                let mut data = Vec::new();
                stdin.read_to_end(&mut data)?;
                data
            };
            with_session(&client, &name, |api| api.print(&name, &data)).map(|()| EXIT_OK)
        }
        Command::Attach { .. } | Command::Version | Command::Help => Ok(EXIT_OK),
    }
}

/// `call` on an api connection; a session that does not exist (no server included) is a failure naming it.
fn with_session<T>(
    client: &Client,
    name: &str,
    call: impl FnOnce(&mut crate::client::Api) -> Result<T, ClientError>,
) -> Result<T, Failure> {
    let gone = || Failure::Failed(format!("no live session named {name}"));
    let mut api = client.api()?.ok_or_else(gone)?;
    match call(&mut api) {
        Err(ClientError::Api(error)) if error.kind == ApiErrorKind::NoSuchSession => Err(gone()),
        result => Ok(result?),
    }
}

/// One session's `list` line (zmx's `writeSessionLine`): `name=…␉pid=…␉clients=…␉created=…`, the pid `-` once its
/// program ended.
#[must_use]
pub fn session_line(record: &SessionRecord) -> String {
    let pid = record.leader_pid.map_or_else(|| "-".to_owned(), |pid| pid.to_string());
    format!(
        "name={}\tpid={pid}\tclients={}\tcreated={}",
        record.name, record.clients, record.created_at
    )
}

fn list(client: &Client, short: bool, out: &mut dyn Write, err: &mut dyn Write) -> Result<u8, Failure> {
    let mut sessions = match client.api()? {
        Some(mut api) => api.list()?,
        None => Vec::new(),
    };
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    if sessions.is_empty() && !short {
        writeln!(err, "no sessions found in {}", client.endpoint().directory().display())?;
    }
    for record in &sessions {
        if short {
            writeln!(out, "{}", record.name)?;
        } else {
            writeln!(out, "{}", session_line(record))?;
        }
    }
    Ok(EXIT_OK)
}

fn kill(
    client: &Client,
    names: &[String],
    force: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<u8, Failure> {
    let results = match client.api()? {
        Some(mut api) => api.kill(names, force)?,
        None => Vec::new(),
    };
    let mut code = EXIT_OK;
    for name in names {
        match results
            .iter()
            .find(|result| &result.name == name)
            .map(|result| &result.outcome)
        {
            Some(KillOutcome::Killed) => writeln!(out, "killed session {name}")?,
            Some(KillOutcome::Failed(reason)) => {
                writeln!(err, "agterm-session kill: {name}: {reason}")?;
                code = EXIT_FAILED;
            }
            Some(KillOutcome::Gone) | None => {
                writeln!(err, "agterm-session kill: no live session named {name}")?;
                code = EXIT_FAILED;
            }
        }
    }
    Ok(code)
}

/// The `screen` header (zmx's): `revision cols rows cursor_x cursor_y alternate`, the cursor zero-based.
#[must_use]
pub fn screen_header(screen: &Screen) -> String {
    format!(
        "{} {} {} {} {} {}",
        screen.revision,
        screen.columns,
        screen.rows,
        screen.cursor_column,
        screen.cursor_row,
        u8::from(screen.alternate)
    )
}

fn screen(client: &Client, name: &str, all: bool, out: &mut dyn Write) -> Result<u8, Failure> {
    let screen = with_session(client, name, |api| api.screen(name, all))?;
    writeln!(out, "{}", screen_header(&screen))?;
    out.write_all(screen.text.as_bytes())?;
    Ok(EXIT_OK)
}

fn version(contract: &Contract, out: &mut dyn Write) -> Result<u8, Failure> {
    writeln!(out, "agterm-session\t{}", env!("CARGO_PKG_VERSION"))?;
    writeln!(out, "build\t{}", crate::build_id::of_this_process())?;
    writeln!(out, "protocol\t{PROTOCOL_VERSION}")?;
    if let Some(directory) = &contract.directory {
        writeln!(out, "live_dir\t{}", directory.display())?;
        // a server that runs answers its build; a failure to reach one is no failure of `version`
        if let Ok(Some(api)) = Client::new(directory).api() {
            writeln!(out, "server\t{} (pid {})", api.welcome.build, api.welcome.pid)?;
        }
    }
    Ok(EXIT_OK)
}

/// The program a session created without an argv runs: `$SHELL`, else the login shell (Unix: the password database's;
/// Windows: PowerShell), else `/bin/sh` or `cmd.exe`, as a login shell where the shell takes `-l`.
#[must_use]
pub fn default_argv(shell_variable: Option<&OsStr>) -> Vec<String> {
    let shell = shell_variable
        .filter(|shell| !shell.is_empty())
        .map(|shell| shell.to_string_lossy().into_owned())
        .or_else(crate::launch::login_shell)
        .unwrap_or_else(|| if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.to_owned());
    let name = Path::new(&shell)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let login = !cfg!(windows) && matches!(name.as_str(), "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "mksh");
    let mut argv = vec![shell];
    if login {
        argv.push("-l".to_owned());
    }
    argv
}

#[cfg(test)]
mod tests;
