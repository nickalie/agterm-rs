//! `agtermctl live list|screen|prune|kill|reset|tree|attach|present` (`agtermctlKit/ZmxCommands.swift`, upstream's
//! `agtermctl zmx`, renamed: conformance/docs/rust-build.md): the daemons behind live sessions, and attaching them
//! from another machine.
//! Every one needs a running agterm, the only party that can join its windows, pending closes and saved snapshots
//! against what the session host reports. `list` prints through [`format_live`]; the rest print their `result.text`
//! (or `ok`).

use agterm_core::live::inventory::PaneRole;
use agterm_core::paths::Env;
use agterm_proto::command::LiveCmd;
use agterm_proto::{Args, Command, ControlLiveEntry, ControlLiveInventory, Request};

use crate::output::format_restore_status;
use crate::{BasicOptions, Failure, RequestCommand};

/// `agtermctl live <subcommand>`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Inspect and manage the daemons behind live sessions.",
    long_about = "Live sessions keep each primary and split pane's process alive in a live-session daemon, so closing \
                  agterm ends the attach client while the process keeps running. These commands show which daemons \
                  exist, who owns them, and which are left over.\n\
                  \n\
                  Every one needs a running agterm: only the app can join its live windows, its pending closes and \
                  its persisted snapshots against what the session host reports. With agterm stopped there is nothing \
                  to ask.",
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Live {
    #[command(subcommand)]
    command: LiveCommand,
}

/// The subcommands, in upstream's `Zmx.configuration.subcommands` order.
#[derive(Debug, clap::Subcommand)]
enum LiveCommand {
    List(List),
    Screen(Screen),
    Prune(Prune),
    Kill(Kill),
    Reset(Reset),
    Tree(Tree),
    Attach(Attach),
    Present(Present),
}

impl Live {
    fn command(&self) -> &dyn RequestCommand {
        match &self.command {
            LiveCommand::List(command) => command,
            LiveCommand::Screen(command) => command,
            LiveCommand::Prune(command) => command,
            LiveCommand::Kill(command) => command,
            LiveCommand::Reset(command) => command,
            LiveCommand::Tree(command) => command,
            LiveCommand::Attach(command) => command,
            LiveCommand::Present(command) => command,
        }
    }

    /// What the parser cannot check, as a usage error's message.
    pub fn check(&self) -> Result<(), String> {
        self.command().validate()
    }

    /// Runs the command.
    pub fn execute(&self, env: &dyn Env) -> Result<(), Failure> {
        self.command().invoke(env)
    }
}

fn live_request(cmd: LiveCmd, target: Option<String>, args: Option<Args>) -> Request {
    Request {
        cmd: Command::Live(cmd),
        target,
        args,
    }
}

/// `agtermctl live list`.
#[derive(Debug, clap::Args)]
#[command(
    about = "List every daemon and the pane that claims it.",
    long_about = "One row per daemon and per pane expecting one, so a leaked daemon and a pane whose daemon has \
                  vanished are both visible. The header reports the restore mode this launch is running.\n\
                  \n\
                  State reads: claimed (a pane owns it), orphan (nothing does), pendingClose (its session is inside \
                  its undo window), foreign (a session in this namespace that is not agterm's), unknown (the pane \
                  inventory was incomplete, so no row can be called an orphan), conflicted (two panes claim it).\n\
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
        live_request(LiveCmd::List, None, None)
    }
}

/// `agtermctl live screen NAME [--all | --lines N]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Print a daemon's screen as plain text, whether or not a window shows its pane.",
    long_about = "NAME is a daemon name as `live list` prints it, not a session id. The read attaches nothing, opens \
                  no window and changes no pane's size, so it reaches a pane in a closed window.\n\
                  \n\
                  The default is the daemon's current screen at the size its last leader gave it, which has no scroll \
                  position of its own. --all adds the scrollback the daemon retains; --lines keeps the last N lines of \
                  that."
)]
pub struct Screen {
    #[arg(help = "Daemon name, from `live list`.")]
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
        live_request(LiveCmd::Screen, None, Some(args))
    }
}

/// `agtermctl live prune`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Kill the daemons no pane claims and nothing is attached to.",
    long_about = "Acts only on rows `live list` shows as orphan with no clients, so you can see what is eligible \
                  before running it. It refuses outright when the pane inventory is incomplete or two panes claim one \
                  daemon, since neither answer can be acted on safely.\n\
                  \n\
                  The check is not atomic. The session host has no kill-if-detached, so this re-lists immediately \
                  before it kills and drops anything that gained a client in between; a client attaching from outside \
                  agterm inside that last gap can still be terminated.\n\
                  \n\
                  It reports each daemon separately. A \"cleaned up a stale socket\" line is not a kill: the session \
                  host unlinked a socket it could not reach, and that daemon may still be running."
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
        live_request(LiveCmd::Prune, None, None)
    }
}

/// `agtermctl live kill --pane P --force --target S [--window W]`.
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
                  It refuses a daemon that is already gone, one the session host could not read (forcing that can \
                  unlink a live daemon's socket and leave it running unreachable), and a session inside its undo \
                  window."
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
        live_request(LiveCmd::Kill, Some(self.target.clone()), Some(args))
    }
}

/// `agtermctl live reset --force`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Reset live sessions that are unsupervised or predate the recorded live-session update, then reopen.",
    long_about = "The same operation as Reset Live Sessions in the app menu, without the dialog. It covers every live \
                  session created before the recorded first launch with this session host build: live sessions keep \
                  the session host they started with through updates. The reset ends those sessions' processes at the \
                  next launch and recreates them, starting their captured commands again where possible. Other \
                  sessions are left alone.\n\
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
        live_request(LiveCmd::Reset, None, Some(args))
    }
}

/// `live.list`: the daemon inventory under the restore header (upstream's `formatZmx`). Owner window state is its own
/// column rather than left to the client count: a closed window's panes sit at zero clients normally.
#[must_use]
pub fn format_live(inventory: &ControlLiveInventory) -> String {
    let mut lines = vec![format_restore_status(&inventory.restore)];
    if !inventory.inventory_complete {
        lines.push("inventory incomplete: some pane is unaccounted for, so nothing can be pruned".to_owned());
    }
    if inventory.entries.is_empty() {
        lines.push("no daemons".to_owned());
        return lines.join("\n");
    }
    lines.push(String::new());
    lines.extend(inventory.entries.iter().map(live_row));
    lines.join("\n")
}

/// One row, carrying the ids `live kill` needs, not just names: a closed or unindexed row may not appear in `tree`.
fn live_row(entry: &ControlLiveEntry) -> String {
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

/// `agtermctl live tree [HOST]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "List the sessions that can be attached to, here or on another Mac.",
    long_about = "With a HOST, runs over ssh against a machine that is also running agterm and reports what it offers, \
                  across every one of its open windows. Feed the result to a picker and hand what the user chooses to \
                  `live attach`.\n\
                  \n\
                  With no HOST, reports this app's own attachable sessions in the same shape — which is exactly the \
                  form the remote call runs on the far side, so it is also how to see what another machine would \
                  answer without sshing anywhere.\n\
                  \n\
                  Only a session whose every pane has a live daemon is listed. One whose daemon has gone is omitted \
                  rather than offered, because attaching to a name that no longer exists would create a fresh shell \
                  wearing it. An empty list is a successful answer: it does not distinguish a store that is not \
                  running in live mode, which `live list` reports.\n\
                  \n\
                  The connection is non-interactive: ssh runs with BatchMode, so key-based auth must already work for \
                  this host and a password or host-key prompt is a failure rather than a question."
)]
pub struct Tree {
    #[arg(help = "The host, as ssh would take it. Omit for this app's own sessions.")]
    host: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Tree {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        let args = self.host.as_ref().map(|host| Args {
            host: Some(host.clone()),
            ..Args::default()
        });
        live_request(LiveCmd::Tree, None, args)
    }
}

/// `agtermctl live attach HOST SESSION [--window]`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Attach to a session on another Mac, as a session here.",
    long_about = "Takes a host and the id of one of the sessions `live tree` listed, and opens it in the chosen window \
                  marked as remote. Defaults to the frontmost window after discovery. A remote session with a split \
                  arrives with the same split.\n\
                  \n\
                  The remote is resolved again before anything is created, so a session that has gone since the list \
                  was taken fails rather than handing back a fresh shell wearing its name.\n\
                  \n\
                  Closing it here ends only this side's connection: the far-side processes keep running, and nothing \
                  this command does can kill them. The session is not restored after a relaunch."
)]
pub struct Attach {
    #[arg(help = "The host, as ssh would take it.")]
    host: String,

    #[arg(help = "The remote session's id, as `live tree` prints it.")]
    session: String,

    #[arg(
        long,
        help = "Local open window id, unique prefix, or active (default: frontmost after discovery)."
    )]
    window: Option<String>,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Attach {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    /// It creates a local session, so it echoes that session's id like every other create command.
    fn echoes_result_id(&self) -> bool {
        true
    }

    fn make_request(&self) -> Request {
        let args = Args {
            host: Some(self.host.clone()),
            window: self.window.clone(),
            ..Args::default()
        };
        live_request(LiveCmd::Attach, Some(self.session.clone()), Some(args))
    }
}

/// `agtermctl live present SESSION`.
#[derive(Debug, clap::Args)]
#[command(
    about = "Carry a presentation stream for one session between stdin/stdout and this app.",
    long_about = "Run by the agterm on another Mac, over ssh, after it attached one of this app's sessions: it is \
                  how that Mac shows this session's status, context, notifications and HUD. Stdout carries \
                  newline-delimited JSON frames and nothing else, and stdin takes the other side's frames. It is not \
                  meant to be run by hand. A refused session exits nonzero with the reason on stderr."
)]
pub struct Present {
    #[arg(help = "The session id the other Mac attached.")]
    session: String,

    #[command(flatten)]
    options: BasicOptions,
}

impl RequestCommand for Present {
    fn options(&self) -> &BasicOptions {
        &self.options
    }

    fn make_request(&self) -> Request {
        live_request(LiveCmd::Present, Some(self.session.clone()), None)
    }

    /// Opens the stream and bridges it to stdin/stdout until either side ends (upstream's `Present.run`).
    fn invoke(&self, env: &dyn Env) -> Result<(), Failure> {
        let socket = crate::client::connect(&self.options.socket_path(env))?;
        let bridge = crate::stream_bridge::StreamBridge::new(socket, libc::STDIN_FILENO, libc::STDOUT_FILENO);
        bridge.open(&self.make_request()).map_err(Failure::Local)?;
        bridge.pump();
        Ok(())
    }
}

/// `live.tree`: one attachable session per line, carrying the id `live attach` takes (upstream's `formatRemoteTree`).
/// An empty answer says so rather than printing nothing, which a caller cannot tell from a failed read.
#[must_use]
pub fn format_remote_tree(tree: &agterm_proto::ControlRemoteTree) -> String {
    let place = tree
        .host
        .as_deref()
        .map(|host| format!(" on {host}"))
        .unwrap_or_default();
    if tree.sessions.is_empty() {
        return format!("no attachable sessions{place}");
    }
    tree.sessions
        .iter()
        .map(|session| {
            let split = if session.panes.len() > 1 {
                let axis = session
                    .split_axis
                    .as_deref()
                    .map(|axis| format!(" {axis}"))
                    .unwrap_or_default();
                format!(" (split{axis})")
            } else {
                String::new()
            };
            // what the row is for, then where it is: the running command answers "attach to which one" more often
            // than the path does, and the context line is the far side's own answer to it
            let running: Vec<&str> = session
                .panes
                .iter()
                .filter_map(|pane| pane.foreground.as_ref()?.first())
                .map(|program| agterm_core::command_restore::basename(program))
                .collect();
            let detail = [
                session.context.clone(),
                (!running.is_empty()).then(|| running.join(" | ")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("  ");
            let tail = if detail.is_empty() {
                String::new()
            } else {
                format!("  {detail}")
            };
            let at = format!("{}/{}/{}", session.window_name, session.workspace_name, session.name);
            format!("  {at}{split}  [{}]  {}{tail}", session.id, session.cwd)
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    fn entry(daemon: &str) -> ControlLiveEntry {
        ControlLiveEntry {
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
        let inventory = ControlLiveInventory {
            restore: restore(),
            inventory_complete: false,
            endpoint: None,
            live_reset: None,
            entries: Vec::new(),
        };
        assert_eq!(
            format_live(&inventory),
            "configured: live (next launch)\nthis launch: requested live, active live\n\
             inventory incomplete: some pane is unaccounted for, so nothing can be pruned\nno daemons"
        );
    }

    #[test]
    fn a_row_carries_the_owner_path_and_short_ids() {
        let claimed = ControlLiveEntry {
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
        let orphan = ControlLiveEntry {
            clients: Some(0),
            ..entry("agterm-b")
        };
        let inventory = ControlLiveInventory {
            restore: restore(),
            inventory_complete: true,
            endpoint: None,
            live_reset: None,
            entries: vec![claimed, orphan, entry("notes")],
        };
        assert_eq!(
            format_live(&inventory),
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
