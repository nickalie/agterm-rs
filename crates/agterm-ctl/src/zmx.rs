//! `agtermctl zmx list|screen|prune|kill|reset` (`agtermctlKit/ZmxCommands.swift`): the daemons behind live sessions.
//! Every one needs a running agterm, the only party that can join its windows, pending closes and saved snapshots
//! against what zmx reports. `list` prints through [`format_zmx`]; the rest print their `result.text` (or `ok`).

use agterm_core::paths::Env;
use agterm_core::zmx::inventory::PaneRole;
use agterm_proto::command::ZmxCmd;
use agterm_proto::{Args, Command, ControlZmxEntry, ControlZmxInventory, Request};

use crate::output::format_restore_status;
use crate::{BasicOptions, Failure, RequestCommand};

/// `agtermctl zmx <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Inspect and manage the daemons behind live sessions.",
    long_about = "Live sessions keep each primary and split pane's process alive in a zmx daemon, so closing agterm \
                  ends the attach client while the process keeps running. These commands show which daemons exist, \
                  who owns them, and which are left over.\n\
                  \n\
                  Every one needs a running agterm: only the app can join its live windows, its pending closes and \
                  its persisted snapshots against what zmx reports. With agterm stopped there is nothing to ask.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Zmx {
    #[command(subcommand)]
    command: ZmxCommand,
}

/// The subcommands, in upstream's `Zmx.configuration.subcommands` order; `tree`, `attach` and `present` come with
/// remote sessions.
#[derive(Debug, clap::Subcommand)]
enum ZmxCommand {
    List(List),
    Screen(Screen),
    Prune(Prune),
    Kill(Kill),
    Reset(Reset),
}

impl Zmx {
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            ZmxCommand::List(command) => command,
            ZmxCommand::Screen(command) => command,
            ZmxCommand::Prune(command) => command,
            ZmxCommand::Kill(command) => command,
            ZmxCommand::Reset(command) => command,
        }
    }

    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        self.command().validate()
    }

    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        self.command().default_run(env)
    }
}

fn zmx_request(cmd: ZmxCmd, target: Option<String>, args: Option<Args>) -> Request {
    Request {
        cmd: Command::Zmx(cmd),
        target,
        args,
    }
}

/// `agtermctl zmx list`.
#[derive(Debug, clap::Args)]
#[command(
    about = "List every daemon and the pane that claims it.",
    long_about = "One row per daemon and per pane expecting one, so a leaked daemon and a pane whose daemon has \
                  vanished are both visible. The header reports the restore mode this launch is running.\n\
                  \n\
                  State reads: claimed (a pane owns it), orphan (nothing does), pendingClose (its session is inside \
                  its undo window), foreign (a zmx session that is not agterm's), unknown (the pane inventory was \
                  incomplete, so no row can be called an orphan), conflicted (two panes claim it).\n\
                  \n\
                  A closed window's panes are claimed with zero clients. That is the normal resting state after you \
                  close a window, not a leak."
)]
pub struct List {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for List {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        zmx_request(ZmxCmd::List, None, None)
    }
}

/// `agtermctl zmx screen NAME [--all | --lines N]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Print a daemon's screen as plain text, whether or not a window shows its pane.",
    long_about = "NAME is a daemon name as `zmx list` prints it, not a session id. The read attaches nothing, opens \
                  no window and changes no pane's size, so it reaches a pane in a closed window.\n\
                  \n\
                  The default is the daemon's current screen at the size its last leader gave it, which has no scroll \
                  position of its own. --all adds the scrollback the daemon retains; --lines keeps the last N lines of \
                  that."
)]
pub struct Screen {
    #[arg(help = "Daemon name, from `zmx list`.")]
    name: String,

    #[arg(long, help = "Read the screen plus the retained scrollback.")]
    all: bool,

    #[arg(long, help = "Keep only the last N lines of the full buffer.")]
    lines: Option<i64>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Screen {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if self.all && self.lines.is_some() {
            return Err("use either --all or --lines, not both".to_owned());
        }
        if self.lines.is_some_and(|lines| lines <= 0) {
            return Err("--lines must be greater than 0".to_owned());
        }
        Ok(())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            name: Some(self.name.clone()),
            all: self.all.then_some(true),
            lines: self.lines,
            ..Args::default()
        };
        zmx_request(ZmxCmd::Screen, None, Some(args))
    }
}

/// `agtermctl zmx prune`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Kill the daemons no pane claims and nothing is attached to.",
    long_about = "Acts only on rows `zmx list` shows as orphan with no clients, so you can see what is eligible before \
                  running it. It refuses outright when the pane inventory is incomplete or two panes claim one daemon, \
                  since neither answer can be acted on safely.\n\
                  \n\
                  The check is not atomic. zmx has no kill-if-detached, so this re-lists immediately before it kills \
                  and drops anything that gained a client in between; a client attaching from outside agterm inside \
                  that last gap can still be terminated.\n\
                  \n\
                  It reports each daemon separately. A \"cleaned up a stale socket\" line is not a kill: zmx unlinked \
                  a socket it could not reach, and that daemon may still be running."
)]
pub struct Prune {
    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Prune {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        zmx_request(ZmxCmd::Prune, None, None)
    }
}

/// `agtermctl zmx kill --pane P --force --target S [--window W]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Destroy one pane's daemon and the process running in it.",
    long_about = "This kills a backend process, not a model object. It reaches a pane no window is currently showing, \
                  and it takes down every client attached to that daemon, so there is no sensible default for who is \
                  affected: the target, the pane and --force are all required.\n\
                  \n\
                  What happens next depends on the pane. Killing a shown split closes that split; killing a primary \
                  promotes its split survivor, or closes the session when there is none. A pane whose window is \
                  closed simply comes back as a fresh shell on its next attach. None of these gets the three-second \
                  undo, and Reopen Closed Item brings the session back with a fresh shell rather than the process that \
                  was running.\n\
                  \n\
                  Killing the daemon of the pane you are typing in can kill this agtermctl before it reads the reply, \
                  the same way \"session close\" on your own session can.\n\
                  \n\
                  It refuses a daemon that is already gone, one zmx could not read (forcing that can unlink a live \
                  daemon's socket and leave it running unreachable), and a session inside its undo window."
)]
pub struct Kill {
    #[arg(long, help = "Which pane's daemon: left/primary/top or right/split/bottom.")]
    pane: String,

    #[arg(long, help = "Required. Confirms destroying the process running in that pane.")]
    force: bool,

    #[arg(long, help = "Session id or unique prefix. Required; 'active' is not accepted.")]
    target: String,

    #[arg(
        long,
        help = "Window id or unique prefix to disambiguate. Omit to search every window; 'active' is not accepted."
    )]
    window: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Kill {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if PaneRole::from_control_name(&self.pane).is_none() {
            return Err("--pane must be left/primary/top or right/split/bottom".to_owned());
        }
        if !self.force {
            return Err("--force is required to destroy a running process".to_owned());
        }
        if self.target == "active" {
            return Err("--target must name a session; 'active' is not accepted here".to_owned());
        }
        if self.window.as_deref() == Some("active") {
            return Err("--window must name a window; 'active' is not accepted here".to_owned());
        }
        Ok(())
    }

    fn make_request(&self) -> Request {
        let args = Args {
            force: Some(true),
            window: self.window.clone(),
            pane: Some(self.pane.clone()),
            ..Args::default()
        };
        zmx_request(ZmxCmd::Kill, Some(self.target.clone()), Some(args))
    }
}

/// `agtermctl zmx reset --force`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Reset live sessions that are unsupervised or predate the recorded zmx update, then reopen.",
    long_about = "The same operation as Reset Live Sessions in the app menu, without the dialog. It covers every live \
                  session created before the recorded first launch with this zmx build: live sessions keep the zmx \
                  they started with through updates. The reset ends those sessions' processes at the next launch and \
                  recreates them, starting their captured commands again where possible. Other sessions are left \
                  alone.\n\
                  \n\
                  agterm quits and reopens itself right after answering. Running work in the affected sessions stops, \
                  and agent conversations may need to be resumed by hand. Run from inside one of those sessions, this \
                  kills the shell this agtermctl runs in.\n\
                  \n\
                  It refuses outside Live sessions mode, when a mode change is waiting for a restart, when the pane \
                  inventory is incomplete, and when nothing needs resetting. The next launch re-checks every session \
                  and only ever resets fewer than confirmed; the tree's `liveReset` reports the result."
)]
pub struct Reset {
    #[arg(
        long,
        help = "Required. Confirms ending the processes in every affected live session."
    )]
    force: bool,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Reset {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn validate(&self) -> Result<(), String> {
        if self.force {
            Ok(())
        } else {
            Err("--force is required to end the processes in the affected live sessions".to_owned())
        }
    }

    fn make_request(&self) -> Request {
        let args = Args {
            force: Some(true),
            ..Args::default()
        };
        zmx_request(ZmxCmd::Reset, None, Some(args))
    }
}

/// `zmx.list`: the daemon inventory under the restore header (upstream's `formatZmx`). Owner window state is its own
/// column rather than left to the client count: a closed window's panes sit at zero clients normally.
#[must_use]
pub fn format_zmx(inventory: &ControlZmxInventory) -> String {
    let mut lines = vec![format_restore_status(&inventory.restore)];
    if !inventory.inventory_complete {
        lines.push("inventory incomplete: some pane is unaccounted for, so nothing can be pruned".to_owned());
    }
    if inventory.entries.is_empty() {
        lines.push("no daemons".to_owned());
        return lines.join("\n");
    }
    lines.push(String::new());
    lines.extend(inventory.entries.iter().map(zmx_row));
    lines.join("\n")
}

/// One row, carrying the ids `zmx kill` needs, not just names: a closed or unindexed row may not appear in `tree`.
fn zmx_row(entry: &ControlZmxEntry) -> String {
    let clients = entry.clients.map_or_else(
        || "-".to_owned(),
        |clients| format!("{clients} client{}", if clients == 1 { "" } else { "s" }),
    );
    let owner = entry.session_id.as_deref().map_or_else(
        || "-".to_owned(),
        |session| {
            let window = entry
                .window_name
                .as_deref()
                .or(entry.window_state.as_deref())
                .unwrap_or("?");
            let path = [
                window,
                entry.workspace_name.as_deref().unwrap_or("?"),
                entry.session_name.as_deref().unwrap_or("?"),
            ]
            .join(" / ");
            let pane = entry
                .pane
                .as_deref()
                .map(|pane| format!(" ({pane})"))
                .unwrap_or_default();
            let window_id = entry
                .window_id
                .as_deref()
                .map(|id| format!(" win {}", short_id(id)))
                .unwrap_or_default();
            format!("{} {path}{pane}{window_id}", short_id(session))
        },
    );
    let state = match &entry.window_state {
        Some(window) => format!("{} [{window} window]", entry.state),
        None => entry.state.clone(),
    };
    let observation = if entry.outdated == Some(true) {
        format!("{} outdated", entry.observation)
    } else {
        entry.observation.clone()
    };
    format!("{}  {state}  {observation}  {clients}  {owner}", entry.daemon)
}

/// The prefix a caller pastes into `--target`/`--window`.
fn short_id(id: &str) -> &str {
    id.char_indices().nth(8).map_or(id, |(end, _)| &id[..end])
}

#[cfg(test)]
mod tests {
    use agterm_proto::ControlRestoreStatus;

    use super::*;

    fn restore() -> ControlRestoreStatus {
        ControlRestoreStatus {
            configured: "live".to_owned(),
            requested_at_launch: "live".to_owned(),
            active: "live".to_owned(),
            restart_required: false,
            unavailable_reason: None,
        }
    }

    fn entry(daemon: &str) -> ControlZmxEntry {
        ControlZmxEntry {
            daemon: daemon.to_owned(),
            state: "orphan".to_owned(),
            observation: "running".to_owned(),
            clients: None,
            leader_pid: None,
            window_id: None,
            window_name: None,
            window_state: None,
            workspace_id: None,
            workspace_name: None,
            session_id: None,
            session_name: None,
            pane: None,
            outdated: None,
        }
    }

    #[test]
    fn an_empty_inventory_says_so_under_the_header() {
        let inventory = ControlZmxInventory {
            restore: restore(),
            inventory_complete: false,
            endpoint: None,
            live_reset: None,
            entries: Vec::new(),
        };
        assert_eq!(
            format_zmx(&inventory),
            "configured: live (next launch)\nthis launch: requested live, active live\n\
             inventory incomplete: some pane is unaccounted for, so nothing can be pruned\nno daemons"
        );
    }

    #[test]
    fn a_row_carries_the_owner_path_and_short_ids() {
        let claimed = ControlZmxEntry {
            state: "claimed".to_owned(),
            clients: Some(1),
            window_id: Some("0A0B0C0D-1111-2222-3333-444444444444".to_owned()),
            window_name: Some("main".to_owned()),
            window_state: Some("open".to_owned()),
            workspace_name: Some("dev".to_owned()),
            session_id: Some("99887766-1111-2222-3333-444444444444".to_owned()),
            session_name: Some("build".to_owned()),
            pane: Some("left".to_owned()),
            outdated: Some(true),
            ..entry("agterm-a")
        };
        let orphan = ControlZmxEntry {
            clients: Some(0),
            ..entry("agterm-b")
        };
        let inventory = ControlZmxInventory {
            restore: restore(),
            inventory_complete: true,
            endpoint: None,
            live_reset: None,
            entries: vec![claimed, orphan, entry("notes")],
        };
        assert_eq!(
            format_zmx(&inventory),
            "configured: live (next launch)\nthis launch: requested live, active live\n\n\
             agterm-a  claimed [open window]  running outdated  1 client  99887766 main / dev / build (left) win 0A0B0C0D\n\
             agterm-b  orphan  running  0 clients  -\n\
             notes  orphan  running  -  -"
        );
    }

    #[test]
    fn kill_refuses_before_sending() {
        let kill = |pane: &str, force: bool, target: &str, window: Option<&str>| Kill {
            pane: pane.to_owned(),
            force,
            target: target.to_owned(),
            window: window.map(str::to_owned),
            options: BasicOptions {
                socket: None,
                json: false,
            },
        };
        assert_eq!(
            kill("middle", true, "S", None).validate(),
            Err("--pane must be left/primary/top or right/split/bottom".to_owned())
        );
        assert_eq!(
            kill("left", false, "S", None).validate(),
            Err("--force is required to destroy a running process".to_owned())
        );
        assert_eq!(
            kill("left", true, "active", None).validate(),
            Err("--target must name a session; 'active' is not accepted here".to_owned())
        );
        assert_eq!(
            kill("split", true, "S", Some("active")).validate(),
            Err("--window must name a window; 'active' is not accepted here".to_owned())
        );
        assert_eq!(kill("bottom", true, "S", Some("W")).validate(), Ok(()));
    }
}
