//! `agtermctl`: drive agterm over its control socket. The command tree mirrors `agtermctlKit/Commands.swift`: every
//! socket subcommand takes upstream's `BasicOptions` (`--socket`, `--json`) after its own name, one request per run,
//! and reports through [`output`]. Each command group lives in its own module.

mod client;
mod output;
mod socket;
mod usage;
mod version;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use agterm_core::paths::{Env, ProcessEnv};
use agterm_proto::Request;
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
    Version(version::Version),
}

impl Command {
    fn run(&self, env: &dyn Env) -> Result<(), Failure> {
        match self {
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

/// Why a subcommand did not succeed.
#[derive(Debug)]
enum Failure {
    /// Already reported (a server refusal, printed with its reply): exit with this status.
    Exit(ExitCode),
    /// A local failure, reported as `Error: <text>` with exit 1.
    Local(String),
}

impl From<ClientError> for Failure {
    fn from(error: ClientError) -> Self {
        Self::Local(error.0)
    }
}

/// A subcommand that sends one request and prints the reply (upstream's `RequestCommand`).
trait RequestCommand {
    /// The connection options; a group with more options (`--window`) still carries these.
    fn options(&self) -> &BasicOptions;

    /// The request this invocation sends.
    fn make_request(&self) -> Request;

    /// Whether the human output echoes `result.id`: only the create commands do, since the caller does not know the
    /// new id yet. `--json` always carries it.
    fn echoes_result_id(&self) -> bool {
        false
    }

    /// Sends the request once and prints the reply; a refusal exits 1 after its `error:` line.
    fn default_run(&self, env: &dyn Env) -> Result<(), Failure> {
        let options = self.options();
        let reply = client::send(&options.socket_path(env), &self.make_request())?;
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

/// The report of a parse failure of `args` (the program name first), help wrapped to `width`.
fn parse_report(args: &[OsString], error: &clap::Error, width: usize) -> usage::ParseExit {
    let args: Vec<String> = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    usage::parse_exit(&Agtermctl::command(), &args, error, width)
}

fn main() -> ExitCode {
    restore_default_sigpipe();
    let args: Vec<OsString> = std::env::args_os().collect();
    let cli = match Agtermctl::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => return report_parse_error(&args, &error),
    };
    match cli.command.run(&ProcessEnv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Exit(code)) => code,
        Err(Failure::Local(message)) => output::fail(&message),
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

    #[test]
    fn connection_options_follow_the_subcommand() {
        let parsed = Agtermctl::try_parse_from(["agtermctl", "version", "--socket", "/s", "--json"]).unwrap();
        let Command::Version(version) = parsed.command;
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
        let Command::Version(version) = parsed.command;
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
}
