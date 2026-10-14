//! `agtermctl`: drive agterm over its control socket. The command tree mirrors `agtermctlKit/Commands.swift`: every
//! socket subcommand takes upstream's `BasicOptions` (`--socket`, `--json`) after its own name, one request per run,
//! and reports through [`output`]. Each command group lives in its own module.

mod ask;
mod browser;
mod client;
mod clipboard;
mod config;
mod dashboard;
mod events;
mod font;
mod hooks;
mod keymap;
mod local;
mod modal;
mod notify;
mod output;
mod overlay_job;
mod overlay_page;
mod pick;
mod quick;
mod restore;
mod session;
mod session_background;
mod session_hud;
mod session_io;
mod session_meta;
mod session_overlay;
mod session_panes;
mod session_restore;
mod sidebar;
mod socket;
mod stream_bridge;
mod surface;
mod terminfo;
mod theme;
mod tree;
mod usage;
mod version;
mod window;
mod workspace;
mod zmx;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use agterm_core::paths::{Env, ProcessEnv};
use agterm_proto::{Args, Request};
use clap::{CommandFactory, Parser, Subcommand};

use crate::client::ClientError;

/// The root command (upstream's `Agtermctl`). Usage errors and help are swift-argument-parser's ([`usage`]): `help`
/// is the library's help command, not clap's subcommand, and a repeated option takes its last value.
#[derive(Debug, Parser)]
#[command(
    name = "agtermctl",
    about = "Drive agterm over its control socket, and install its terminfo entry on other hosts.",
    subcommand_required = true,
    arg_required_else_help = true,
    disable_help_subcommand = true,
    args_override_self = true
)]
struct Agtermctl {
    #[command(subcommand)]
    command: Command,
}

/// The subcommands, in upstream's `Agtermctl.configuration.subcommands` order; later tasks add the groups. A command's
/// abstract is an `about` attribute (clap derive drops a doc comment's final period), as upstream's `abstract:`.
#[derive(Debug, Subcommand)]
enum Command {
    Tree(tree::Tree),
    Events(events::Events),
    Workspace(workspace::Workspace),
    Session(session::Session),
    Surface(surface::Surface),
    Dashboard(dashboard::Dashboard),
    Window(window::Window),
    Quick(quick::Quick),
    Sidebar(sidebar::Sidebar),
    Notify(notify::Notify),
    Font(font::Font),
    Keymap(keymap::Keymap),
    Hooks(hooks::Hooks),
    Browser(browser::Browser),
    Config(config::Config),
    Theme(theme::Theme),
    Pick(pick::Pick),
    Ask(ask::Ask),
    Restore(restore::Restore),
    Zmx(zmx::Zmx),
    Terminfo(terminfo::Terminfo),
    Clipboard(clipboard::Clipboard),
    Version(version::Version),
}

impl Command {
    /// Checks what the parser cannot (upstream's `validate()`): a refusal is a usage error, before anything is sent.
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Tree(command) => command.validate(),
            Self::Events(command) => command.validate(),
            Self::Workspace(group) => group.command().validate(),
            Self::Session(group) => group.command().validate(),
            Self::Surface(group) => group.command().validate(),
            Self::Dashboard(command) => command.check(),
            Self::Window(group) => group.command().validate(),
            Self::Quick(command) => command.check(),
            Self::Sidebar(command) => command.check(),
            Self::Notify(command) => command.validate(),
            Self::Font(command) => command.check(),
            Self::Keymap(command) => command.check(),
            Self::Hooks(command) => command.check(),
            Self::Browser(command) => command.check(),
            Self::Config(command) => command.check(),
            Self::Theme(command) => command.check(),
            Self::Ask(command) => command.check(),
            Self::Restore(command) => command.check(),
            Self::Zmx(command) => command.check(),
            Self::Terminfo(command) => command.check(),
            Self::Pick(_) | Self::Clipboard(_) | Self::Version(_) => Ok(()),
        }
    }

    fn run(&self, env: &dyn Env) -> Result<(), Failure> {
        match self {
            Self::Tree(command) => command.default_run(env),
            Self::Events(command) => command.run(env),
            Self::Workspace(group) => group.command().default_run(env),
            Self::Session(group) => group.command().invoke(env),
            Self::Surface(group) => group.command().default_run(env),
            Self::Dashboard(command) => command.execute(env),
            Self::Window(group) => group.command().default_run(env),
            Self::Quick(command) => command.execute(env),
            Self::Sidebar(command) => command.execute(env),
            Self::Notify(command) => command.default_run(env),
            Self::Font(command) => command.execute(env),
            Self::Keymap(command) => command.execute(env),
            Self::Hooks(command) => command.execute(env),
            Self::Browser(command) => command.execute(env),
            Self::Config(command) => command.execute(env),
            Self::Theme(command) => command.execute(env),
            Self::Pick(command) => command.execute(env),
            Self::Ask(command) => command.execute(env),
            Self::Restore(command) => command.execute(env),
            Self::Zmx(command) => command.execute(env),
            Self::Terminfo(command) => command.execute(),
            Self::Clipboard(command) => command.execute(),
            Self::Version(command) => command.run(env),
        }
    }
}

/// Where to connect and how to print: the options every socket subcommand accepts (upstream's `BasicOptions`).
/// They follow the subcommand's name, as upstream's: `agtermctl --socket S version` is a usage error.
#[derive(Debug, clap::Args)]
struct BasicOptions {
    #[arg(long, help = "Override the control socket path.")]
    socket: Option<PathBuf>,

    #[arg(long, help = "Print the raw JSON response.")]
    json: bool,
}

impl BasicOptions {
    /// `--socket`, else `<AGTERM_STATE_DIR>/agterm.sock`, else the platform default.
    fn socket_path(&self, env: &dyn Env) -> PathBuf {
        socket::resolve(self.socket.as_deref(), env)
    }
}

/// [`BasicOptions`] plus the `--window` selector of the commands that act on a window's tree (upstream's
/// `ClientOptions`).
#[derive(Debug, clap::Args)]
struct ClientOptions {
    #[command(flatten)]
    basic: BasicOptions,

    #[arg(
        long,
        help = "Target window id, unique prefix, or 'active' (defaults to the frontmost)."
    )]
    window: Option<String>,
}

impl ClientOptions {
    /// Folds `--window` into `base`, or makes args carrying only it; `None` without a window and a base, so the
    /// request carries no empty `args` object.
    fn with_window(&self, base: Option<Args>) -> Option<Args> {
        let Some(window) = &self.window else {
            return base;
        };
        let mut args = base.unwrap_or_default();
        args.window = Some(window.clone());
        Some(args)
    }
}

/// The `--target` of a command addressing one session or workspace (upstream's `TargetOptions`).
#[derive(Debug, clap::Args)]
struct TargetOptions {
    #[arg(
        long,
        default_value = "active",
        help = "Target session/workspace id, unique prefix, or 'active'."
    )]
    target: String,
}

/// The repeatable `--target` of a batch-capable session command (upstream's `BatchTargetOptions`): the targets go
/// on the wire in command-line order, the first also as the request's own target, so a server without batches acts
/// on it rather than on the active session.
#[derive(Debug, clap::Args)]
struct BatchTargetOptions {
    #[arg(
        long = "target",
        action = clap::ArgAction::Append,
        help = "Target session id, prefix, or 'active'. Repeat for a batch."
    )]
    targets: Vec<String>,
}

impl BatchTargetOptions {
    /// The request's own target: the first `--target`, else `active`.
    fn first(&self) -> String {
        self.targets.first().cloned().unwrap_or_else(|| "active".to_owned())
    }

    /// `args.targets`: the whole batch when more than one target was given.
    fn batch(&self) -> Option<Vec<String>> {
        (self.targets.len() > 1).then(|| self.targets.clone())
    }
}

/// Why a subcommand did not succeed.
#[derive(Debug)]
enum Failure {
    /// Already reported (a server refusal, printed with its reply): exit with this status.
    Exit(ExitCode),
    /// A local failure, reported as `Error: <text>` with exit 1.
    Local(String),
    /// A refusal while building the request, after validation passed (upstream's `ValidationError` thrown from
    /// `run()`): a usage error naming the root command, since the library no longer knows which subcommand ran.
    Usage(String),
}

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        Self::Local(error.message)
    }
}

/// A subcommand that sends one request and prints the reply (upstream's `RequestCommand`).
trait RequestCommand {
    /// The connection options; a group with more options (`--window`) still carries these.
    fn options(&self) -> &BasicOptions;

    /// The request this invocation sends.
    fn make_request(&self) -> Request;

    /// The request a run sends: [`Self::make_request`], unless the command reads its payload when it runs
    /// (`--stdin`) or refuses then, as upstream's throwing `makeRequest()`.
    fn run_request(&self) -> Result<Request, Failure> {
        Ok(self.make_request())
    }

    /// Whether the human output echoes `result.id`: only the create commands do, since the caller does not know the
    /// new id yet. `--json` always carries it.
    fn echoes_result_id(&self) -> bool {
        false
    }

    /// What the parser cannot check (upstream's `validate()`), as the message of a usage error.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }

    /// Runs the command: [`Self::default_run`], unless it waits or supervises beyond one round trip (an overlay's
    /// `--block`, `result --page`, `run-job`), as upstream's overridden `run()`.
    fn invoke(&self, env: &dyn Env) -> Result<(), Failure> {
        self.default_run(env)
    }

    /// Sends the request once and prints the reply; a refusal exits 1 after its `error:` line.
    fn default_run(&self, env: &dyn Env) -> Result<(), Failure> {
        let options = self.options();
        let request = self.run_request()?;
        let reply = client::send(&options.socket_path(env), &request)?;
        output::print_response(&reply, options.json, self.echoes_result_id());
        if reply.response.ok {
            Ok(())
        } else {
            Err(Failure::Exit(ExitCode::FAILURE))
        }
    }
}

/// Puts SIGPIPE back to its default action; std sets it to ignored before `main`. A reader that closes stdout early
/// (`agtermctl tree | head -1`) then ends the process quietly by the signal, as upstream's CLI, where `println!`
/// would panic on `EPIPE` and exit 101. The socket write still gets `EPIPE` as an error: it sends with
/// `MSG_NOSIGNAL`.
fn restore_default_sigpipe() {
    // SAFETY: `signal` with a valid signal number and `SIG_DFL` installs no handler code, and this runs first thing
    // in `main`, before any other thread exists.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
}

/// Prints a parse failure as swift-argument-parser does (help on stdout, a usage error on stderr) and returns its exit
/// status.
fn report_parse_error(args: &[OsString], error: &clap::Error) -> ExitCode {
    let report = parse_report(args, error, usage::screen_width(&ProcessEnv));
    if report.to_stdout {
        print!("{}", report.text);
    } else {
        eprint!("{}", report.text);
    }
    ExitCode::from(report.code)
}

/// Prints a validation refusal as swift-argument-parser does (`Error: <message>`, the usage line, on stderr) and
/// returns its exit status.
fn report_validation_error(args: &[OsString], message: &str) -> ExitCode {
    let report = validation_report(args, message);
    eprint!("{}", report.text);
    ExitCode::from(report.code)
}

/// The report of a validation refusal of `args` (the program name first).
fn validation_report(args: &[OsString], message: &str) -> usage::ParseExit {
    usage::validation_exit(&Agtermctl::command(), &plain_args(args), message)
}

/// `args` without the program name, as text.
fn plain_args(args: &[OsString]) -> Vec<String> {
    args.iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

/// The report of a parse failure of `args` (the program name first), help wrapped to `width`.
fn parse_report(args: &[OsString], error: &clap::Error, width: usize) -> usage::ParseExit {
    usage::parse_exit(&Agtermctl::command(), &plain_args(args), error, width)
}

fn main() -> ExitCode {
    restore_default_sigpipe();
    let args: Vec<OsString> = std::env::args_os().collect();
    let args = usage::name_default_subcommands(&Agtermctl::command(), &args);
    let cli = match Agtermctl::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => return report_parse_error(&args, &error),
    };
    if let Err(message) = cli.command.validate() {
        return report_validation_error(&args, &message);
    }
    match cli.command.run(&ProcessEnv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Exit(code)) => code,
        Err(Failure::Local(message)) => output::fail(&message),
        Err(Failure::Usage(message)) => report_validation_error(&[], &message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report of `agtermctl args`, which must fail to parse.
    fn report(args: &[&str]) -> usage::ParseExit {
        let args: Vec<OsString> = std::iter::once("agtermctl")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect();
        let error = Agtermctl::try_parse_from(&args).unwrap_err();
        parse_report(&args, &error, 80)
    }

    #[test]
    fn the_command_tree_is_consistent() {
        Agtermctl::command().debug_assert();
    }

    /// The catalog names of `command`'s subcommands below `path`: a leaf's path, except that a group's default
    /// subcommand is named by the group's own path (`pick open` is `pick`).
    fn catalog_names(command: &clap::Command, path: &[&str], names: &mut std::collections::BTreeSet<String>) {
        for sub in command.get_subcommands() {
            let mut sub_path = path.to_vec();
            sub_path.push(sub.get_name());
            if sub.has_subcommands() {
                catalog_names(sub, &sub_path, names);
            } else if usage::default_subcommand(path) == Some(sub.get_name()) {
                names.insert(path.join(" "));
            } else {
                names.insert(sub_path.join(" "));
            }
        }
    }

    #[test]
    fn the_command_tree_is_the_catalogs_cli() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/catalog.yaml");
        let catalog: serde_norway::Value = serde_norway::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let cli = catalog["cli"].as_mapping().unwrap();
        // `zmx tree`/`zmx present` come with remote sessions (plan Task 29) and `zmx attach` with Task 30, which drop
        // this exception
        let not_yet = ["zmx tree", "zmx present", "zmx attach"];
        let expected: std::collections::BTreeSet<String> = cli
            .keys()
            .map(|name| name.as_str().unwrap().to_owned())
            .filter(|name| !not_yet.contains(&name.as_str()))
            .collect();
        let mut ours = std::collections::BTreeSet::new();
        catalog_names(&Agtermctl::command(), &[], &mut ours);
        assert_eq!(ours, expected);
    }

    #[test]
    fn the_local_commands_are_no_request_commands() {
        for argv in [
            &["agtermctl", "clipboard", "set", "hello"][..],
            &["agtermctl", "terminfo", "install", "buildbox"],
        ] {
            let parsed = Agtermctl::try_parse_from(argv).unwrap();
            assert!(
                matches!(parsed.command, Command::Clipboard(_) | Command::Terminfo(_)),
                "{argv:?}"
            );
            assert_eq!(parsed.command.validate(), Ok(()));
        }
    }

    #[test]
    fn connection_options_follow_the_subcommand() {
        let parsed = Agtermctl::try_parse_from(["agtermctl", "version", "--socket", "/s", "--json"]).unwrap();
        let Command::Version(version) = parsed.command else {
            panic!("not version")
        };
        assert_eq!(version.options().socket.as_deref(), Some(std::path::Path::new("/s")));
        assert!(version.options().json);
        for misplaced in [&["--socket", "/s", "version"][..], &["--json", "version"]] {
            assert_eq!(report(misplaced).code, usage::USAGE_EXIT, "{misplaced:?}");
        }
    }

    #[test]
    fn a_repeated_option_takes_its_last_value() {
        let parsed = Agtermctl::try_parse_from([
            "agtermctl",
            "version",
            "--socket",
            "/a",
            "--socket",
            "/b",
            "--json",
            "--json",
        ])
        .unwrap();
        let Command::Version(version) = parsed.command else {
            panic!("not version")
        };
        assert_eq!(version.options().socket.as_deref(), Some(std::path::Path::new("/b")));
        assert!(version.options().json);
    }

    #[test]
    fn a_bare_invocation_prints_help_on_stdout() {
        let report = report(&[]);
        assert!(report.to_stdout && report.code == 0, "{report:?}");
        assert!(report.text.starts_with("OVERVIEW: "), "{}", report.text);
    }

    #[test]
    fn an_unknown_option_is_a_usage_error() {
        let report = report(&["version", "--bogus"]);
        assert!(!report.to_stdout && report.code == usage::USAGE_EXIT, "{report:?}");
        assert_eq!(
            report.text,
            "Error: Unknown option '--bogus'\nUsage: agtermctl version [--socket <socket>] [--json]\n  See 'agtermctl \
             version --help' for more information.\n"
        );
    }

    fn tree_request(options: &[&str]) -> Request {
        let command_line = ["agtermctl", "tree"].iter().chain(options).copied();
        let Command::Tree(tree) = Agtermctl::try_parse_from(command_line).unwrap().command else {
            panic!("not tree")
        };
        tree.make_request()
    }

    #[test]
    fn tree_sends_the_window_only_when_given() {
        assert_eq!(
            serde_json::to_value(tree_request(&[])).unwrap(),
            serde_json::json!({"cmd": "tree"})
        );
        assert_eq!(
            serde_json::to_value(tree_request(&["--window", "ab12", "--json"])).unwrap(),
            serde_json::json!({"cmd": "tree", "args": {"window": "ab12"}})
        );
    }

    #[test]
    fn tree_usage_lists_the_window_after_the_connection_options() {
        let report = report(&["tree", "--bogus"]);
        assert_eq!(report.code, usage::USAGE_EXIT);
        assert!(
            report
                .text
                .contains("\nUsage: agtermctl tree [--socket <socket>] [--json] [--window <window>]\n"),
            "{}",
            report.text
        );
    }

    #[derive(Debug, Parser)]
    #[command(args_override_self = true)]
    struct Targets {
        #[command(flatten)]
        target: TargetOptions,
    }

    #[derive(Debug, Parser)]
    #[command(args_override_self = true)]
    struct Batch {
        #[command(flatten)]
        batch: BatchTargetOptions,
    }

    #[test]
    fn a_single_target_defaults_to_active_and_takes_its_last_value() {
        assert_eq!(Targets::try_parse_from(["t"]).unwrap().target.target, "active");
        let parsed = Targets::try_parse_from(["t", "--target", "a", "--target", "b"]).unwrap();
        assert_eq!(parsed.target.target, "b");
    }

    #[test]
    fn batch_targets_keep_command_line_order() {
        let none = Batch::try_parse_from(["t"]).unwrap().batch;
        assert_eq!((none.first(), none.batch()), ("active".to_owned(), None));
        let one = Batch::try_parse_from(["t", "--target", "a"]).unwrap().batch;
        assert_eq!((one.first(), one.batch()), ("a".to_owned(), None));
        let many = Batch::try_parse_from(["t", "--target", "b", "--target", "a", "--target", "b"])
            .unwrap()
            .batch;
        assert_eq!(many.first(), "b");
        assert_eq!(many.batch(), Some(vec!["b".to_owned(), "a".to_owned(), "b".to_owned()]));
    }
}
