//! `agtermctl`: drive agterm over its control socket. The command tree mirrors `agtermctlKit/Commands.swift`: every
//! socket subcommand takes upstream's `BasicOptions` (`--socket`, `--json`) after its own name, one request per run,
//! and reports through [`output`]. Each command group lives in its own module.

mod client;
mod output;
mod socket;
mod version;

use std::path::PathBuf;
use std::process::ExitCode;

use agterm_core::paths::{Env, ProcessEnv};
use agterm_proto::Request;
use clap::{Parser, Subcommand};

use crate::client::ClientError;

/// The root command.
#[derive(Debug, Parser)]
#[command(
    name = "agtermctl",
    about = "Drive agterm over its control socket, and install its terminfo entry on other hosts.",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Agtermctl {
    #[command(subcommand)]
    command: Command,
}

/// The subcommands, in upstream's catalog order; later tasks add the groups.
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
    /// Override the control socket path.
    #[arg(long, value_name = "socket")]
    socket: Option<PathBuf>,

    /// Print the raw JSON response.
    #[arg(long)]
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

fn main() -> ExitCode {
    restore_default_sigpipe();
    let cli = match Agtermctl::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let report = output::parse_exit(&error);
            if report.to_stdout {
                print!("{}", report.text);
            } else {
                eprint!("{}", report.text);
            }
            return ExitCode::from(report.code);
        }
    };
    match cli.command.run(&ProcessEnv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Exit(code)) => code,
        Err(Failure::Local(message)) => output::fail(&message),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

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
        for misplaced in [
            &["agtermctl", "--socket", "/s", "version"][..],
            &["agtermctl", "--json", "version"],
        ] {
            let error = Agtermctl::try_parse_from(misplaced).unwrap_err();
            assert_eq!(output::parse_exit(&error).code, output::USAGE_EXIT, "{misplaced:?}");
        }
    }

    #[test]
    fn a_bare_invocation_prints_help_on_stdout() {
        let error = Agtermctl::try_parse_from(["agtermctl"]).unwrap_err();
        let report = output::parse_exit(&error);
        assert!(report.to_stdout && report.code == 0, "{report:?}");
        assert!(report.text.contains("version"), "{}", report.text);
    }

    #[test]
    fn an_unknown_option_is_a_usage_error() {
        let error = Agtermctl::try_parse_from(["agtermctl", "version", "--bogus"]).unwrap_err();
        let report = output::parse_exit(&error);
        assert!(!report.to_stdout && report.code == output::USAGE_EXIT, "{report:?}");
        assert!(report.text.starts_with("Error: "), "{}", report.text);
        assert!(report.text.contains("\nUsage: agtermctl version "), "{}", report.text);
        assert!(
            report
                .text
                .ends_with("  See 'agtermctl version --help' for more information.\n"),
            "{}",
            report.text
        );
    }
}
