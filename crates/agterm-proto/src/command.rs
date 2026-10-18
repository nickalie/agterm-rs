//! Control command names: the `cmd` field of a request.
//!
//! A [`Command`] is two-level: a [`Group`] (the first dotted component of the wire name) and, unless the group's
//! only command is the bare group name (`tree`, `dashboard`, `notify`, `version`), the group's own enum. Wire names
//! come from the `strum` derives on the group enums, so parsing is a 22-arm match on the group followed by the group
//! enum's derived parser.

use std::fmt;
use std::str::FromStr;

use serde_with::{DeserializeFromStr, SerializeDisplay};
use strum::IntoEnumIterator;

/// The first dotted component of a command's wire name. Each group routes to one handler.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumString, strum::IntoStaticStr, strum::Display, strum::EnumIter,
)]
#[strum(serialize_all = "lowercase")]
pub enum Group {
    Ask,
    Browser,
    Config,
    Dashboard,
    Debug,
    Events,
    Font,
    Hooks,
    Keymap,
    Notify,
    Pick,
    Quick,
    Restore,
    Session,
    Sidebar,
    Surface,
    Theme,
    Tree,
    Version,
    Window,
    Workspace,
    Live,
}

/// Declares one group's command enum, each variant with its full dotted wire name.
macro_rules! group_commands {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumString, strum::IntoStaticStr, strum::EnumIter)]
        pub enum $name {
            $(
                #[strum(serialize = $wire)]
                $variant,
            )+
        }
    };
}

group_commands!(
    /// `ask.*`: the modal question dialog.
    AskCmd { Open = "ask.open", Result = "ask.result", Cancel = "ask.cancel" }
);

group_commands!(
    /// `browser.*`: the overlay browser profile.
    BrowserCmd { Clear = "browser.clear", Links = "browser.links" }
);

group_commands!(
    /// `config.*`: the agterm-scoped ghostty config.
    ConfigCmd { Reload = "config.reload" }
);

group_commands!(
    /// `debug.*`: UI-test-only seams, exempt from the suite.
    DebugCmd { Appearance = "debug.appearance" }
);

group_commands!(
    /// `events.*`: the app-run event ring.
    EventsCmd { Read = "events.read" }
);

group_commands!(
    /// `font.*`: the terminal font size.
    FontCmd { Inc = "font.inc", Dec = "font.dec", Reset = "font.reset" }
);

group_commands!(
    /// `hooks.*`: event hooks from `hooks.conf`.
    HooksCmd { Reload = "hooks.reload", List = "hooks.list" }
);

group_commands!(
    /// `keymap.*`: the key bindings from `keymap.conf`.
    KeymapCmd { Reload = "keymap.reload", List = "keymap.list", Run = "keymap.run" }
);

group_commands!(
    /// `pick.*`: the fuzzy picker.
    PickCmd { Open = "pick.open", Result = "pick.result", Cancel = "pick.cancel" }
);

group_commands!(
    /// `quick` and `quick.*`: the quick (scratch) terminal. `quick` itself shows, hides or toggles it.
    QuickCmd { Visibility = "quick", Type = "quick.type", Text = "quick.text" }
);

group_commands!(
    /// `restore.*`: the restore-mode policy and command capture.
    RestoreCmd { Clear = "restore.clear", Capture = "restore.capture", Mode = "restore.mode" }
);

group_commands!(
    /// `session.*`: sessions, their panes, overlays and HUDs.
    SessionCmd {
        New = "session.new",
        Duplicate = "session.duplicate",
        Close = "session.close",
        Select = "session.select",
        Go = "session.go",
        Rename = "session.rename",
        Reveal = "session.reveal",
        Move = "session.move",
        Type = "session.type",
        Status = "session.status",
        Flag = "session.flag",
        Context = "session.context",
        Seen = "session.seen",
        Restore = "session.restore",
        Restart = "session.restart",
        Background = "session.background",
        Split = "session.split",
        SplitClose = "session.split.close",
        Swap = "session.swap",
        Lead = "session.lead",
        Scratch = "session.scratch",
        Focus = "session.focus",
        Resize = "session.resize",
        Copy = "session.copy",
        Paste = "session.paste",
        SelectAll = "session.selectall",
        Text = "session.text",
        Search = "session.search",
        OverlayOpen = "session.overlay.open",
        OverlayClose = "session.overlay.close",
        OverlayResize = "session.overlay.resize",
        OverlayReload = "session.overlay.reload",
        OverlayNavigate = "session.overlay.navigate",
        OverlayResult = "session.overlay.result",
        OverlaySubmit = "session.overlay.submit",
        OverlayCopy = "session.overlay.copy",
        OverlayText = "session.overlay.text",
        OverlayJobRun = "session.overlay.job.run",
        HudOpen = "session.hud.open",
        HudUpdate = "session.hud.update",
        HudClose = "session.hud.close",
    }
);

group_commands!(
    /// `sidebar` and `sidebar.*`. `sidebar` itself shows, hides or toggles the sidebar.
    SidebarCmd {
        Visibility = "sidebar",
        Mode = "sidebar.mode",
        FlaggedLayout = "sidebar.flagged-layout",
        Expand = "sidebar.expand",
        Collapse = "sidebar.collapse",
        Width = "sidebar.width",
    }
);

group_commands!(
    /// `surface.*`: the addressed terminal surface.
    SurfaceCmd { Zoom = "surface.zoom", Cursor = "surface.cursor" }
);

group_commands!(
    /// `theme.*`: the terminal theme.
    ThemeCmd { Set = "theme.set", List = "theme.list" }
);

group_commands!(
    /// `window.*`: app windows.
    WindowCmd {
        New = "window.new",
        List = "window.list",
        Select = "window.select",
        Go = "window.go",
        Close = "window.close",
        Rename = "window.rename",
        Delete = "window.delete",
        Resize = "window.resize",
        Move = "window.move",
        Zoom = "window.zoom",
        Fullscreen = "window.fullscreen",
        Minimize = "window.minimize",
    }
);

group_commands!(
    /// `workspace.*`: workspaces in the sidebar tree.
    WorkspaceCmd {
        New = "workspace.new",
        Rename = "workspace.rename",
        Delete = "workspace.delete",
        Select = "workspace.select",
        Go = "workspace.go",
        Move = "workspace.move",
        Focus = "workspace.focus",
        Filter = "workspace.filter",
        Collapse = "workspace.collapse",
        Expand = "workspace.expand",
    }
);

group_commands!(
    /// `live.*`: live-session daemons (upstream's `zmx.*`, renamed: conformance/docs/rust-build.md).
    LiveCmd {
        List = "live.list",
        Prune = "live.prune",
        Kill = "live.kill",
        Reset = "live.reset",
        Tree = "live.tree",
        Attach = "live.attach",
        Present = "live.present",
        Screen = "live.screen",
    }
);

/// A control command. Groups with a single command named after the group (`tree`, `dashboard`, `notify`,
/// `version`) are unit variants; every other group carries its own enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, SerializeDisplay, DeserializeFromStr)]
pub enum Command {
    Ask(AskCmd),
    Browser(BrowserCmd),
    Config(ConfigCmd),
    Dashboard,
    Debug(DebugCmd),
    Events(EventsCmd),
    Font(FontCmd),
    Hooks(HooksCmd),
    Keymap(KeymapCmd),
    Notify,
    Pick(PickCmd),
    Quick(QuickCmd),
    Restore(RestoreCmd),
    Session(SessionCmd),
    Sidebar(SidebarCmd),
    Surface(SurfaceCmd),
    Theme(ThemeCmd),
    Tree,
    Version,
    Window(WindowCmd),
    Workspace(WorkspaceCmd),
    Live(LiveCmd),
}

impl Command {
    /// The dotted wire name, e.g. `session.overlay.open`.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Ask(c) => c.into(),
            Self::Browser(c) => c.into(),
            Self::Config(c) => c.into(),
            Self::Dashboard => "dashboard",
            Self::Debug(c) => c.into(),
            Self::Events(c) => c.into(),
            Self::Font(c) => c.into(),
            Self::Hooks(c) => c.into(),
            Self::Keymap(c) => c.into(),
            Self::Notify => "notify",
            Self::Pick(c) => c.into(),
            Self::Quick(c) => c.into(),
            Self::Restore(c) => c.into(),
            Self::Session(c) => c.into(),
            Self::Sidebar(c) => c.into(),
            Self::Surface(c) => c.into(),
            Self::Theme(c) => c.into(),
            Self::Tree => "tree",
            Self::Version => "version",
            Self::Window(c) => c.into(),
            Self::Workspace(c) => c.into(),
            Self::Live(c) => c.into(),
        }
    }

    /// The group this command routes through.
    #[must_use]
    pub fn group(self) -> Group {
        match self {
            Self::Ask(_) => Group::Ask,
            Self::Browser(_) => Group::Browser,
            Self::Config(_) => Group::Config,
            Self::Dashboard => Group::Dashboard,
            Self::Debug(_) => Group::Debug,
            Self::Events(_) => Group::Events,
            Self::Font(_) => Group::Font,
            Self::Hooks(_) => Group::Hooks,
            Self::Keymap(_) => Group::Keymap,
            Self::Notify => Group::Notify,
            Self::Pick(_) => Group::Pick,
            Self::Quick(_) => Group::Quick,
            Self::Restore(_) => Group::Restore,
            Self::Session(_) => Group::Session,
            Self::Sidebar(_) => Group::Sidebar,
            Self::Surface(_) => Group::Surface,
            Self::Theme(_) => Group::Theme,
            Self::Tree => Group::Tree,
            Self::Version => Group::Version,
            Self::Window(_) => Group::Window,
            Self::Workspace(_) => Group::Workspace,
            Self::Live(_) => Group::Live,
        }
    }

    /// Every command, group by group.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut all = vec![Self::Dashboard, Self::Notify, Self::Tree, Self::Version];
        all.extend(AskCmd::iter().map(Self::Ask));
        all.extend(BrowserCmd::iter().map(Self::Browser));
        all.extend(ConfigCmd::iter().map(Self::Config));
        all.extend(DebugCmd::iter().map(Self::Debug));
        all.extend(EventsCmd::iter().map(Self::Events));
        all.extend(FontCmd::iter().map(Self::Font));
        all.extend(HooksCmd::iter().map(Self::Hooks));
        all.extend(KeymapCmd::iter().map(Self::Keymap));
        all.extend(PickCmd::iter().map(Self::Pick));
        all.extend(QuickCmd::iter().map(Self::Quick));
        all.extend(RestoreCmd::iter().map(Self::Restore));
        all.extend(SessionCmd::iter().map(Self::Session));
        all.extend(SidebarCmd::iter().map(Self::Sidebar));
        all.extend(SurfaceCmd::iter().map(Self::Surface));
        all.extend(ThemeCmd::iter().map(Self::Theme));
        all.extend(WindowCmd::iter().map(Self::Window));
        all.extend(WorkspaceCmd::iter().map(Self::Workspace));
        all.extend(LiveCmd::iter().map(Self::Live));
        all
    }
}

/// A unit command matches only its bare group name, never a dotted name under it.
fn unit(name: &str, group: Group, command: Command) -> Option<Command> {
    (name == <&'static str>::from(group)).then_some(command)
}

impl FromStr for Command {
    type Err = UnknownCommand;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        let prefix = name.split_once('.').map_or(name, |(group, _)| group);
        let group = Group::from_str(prefix).map_err(|_| UnknownCommand(name.to_owned()))?;
        let command = match group {
            Group::Ask => AskCmd::from_str(name).ok().map(Self::Ask),
            Group::Browser => BrowserCmd::from_str(name).ok().map(Self::Browser),
            Group::Config => ConfigCmd::from_str(name).ok().map(Self::Config),
            Group::Dashboard => unit(name, group, Self::Dashboard),
            Group::Debug => DebugCmd::from_str(name).ok().map(Self::Debug),
            Group::Events => EventsCmd::from_str(name).ok().map(Self::Events),
            Group::Font => FontCmd::from_str(name).ok().map(Self::Font),
            Group::Hooks => HooksCmd::from_str(name).ok().map(Self::Hooks),
            Group::Keymap => KeymapCmd::from_str(name).ok().map(Self::Keymap),
            Group::Notify => unit(name, group, Self::Notify),
            Group::Pick => PickCmd::from_str(name).ok().map(Self::Pick),
            Group::Quick => QuickCmd::from_str(name).ok().map(Self::Quick),
            Group::Restore => RestoreCmd::from_str(name).ok().map(Self::Restore),
            Group::Session => SessionCmd::from_str(name).ok().map(Self::Session),
            Group::Sidebar => SidebarCmd::from_str(name).ok().map(Self::Sidebar),
            Group::Surface => SurfaceCmd::from_str(name).ok().map(Self::Surface),
            Group::Theme => ThemeCmd::from_str(name).ok().map(Self::Theme),
            Group::Tree => unit(name, group, Self::Tree),
            Group::Version => unit(name, group, Self::Version),
            Group::Window => WindowCmd::from_str(name).ok().map(Self::Window),
            Group::Workspace => WorkspaceCmd::from_str(name).ok().map(Self::Workspace),
            Group::Live => LiveCmd::from_str(name).ok().map(Self::Live),
        };
        command.ok_or_else(|| UnknownCommand(name.to_owned()))
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire_name())
    }
}

/// A `cmd` that names no command. A request carrying one fails to decode as a whole.
///
/// The text is the context Foundation's `JSONDecoder` gives a `String`-backed enum it cannot initialize (upstream's
/// `Command` in `agtermCore/Sources/agtermCore/ControlProtocol.swift`), so a refused request names its `cmd` as
/// upstream's does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCommand(pub String);

impl fmt::Display for UnknownCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cannot initialize Command from invalid String value {}", self.0)
    }
}

impl std::error::Error for UnknownCommand {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_shape_of_wire_name() {
        assert_eq!("tree".parse(), Ok(Command::Tree));
        assert_eq!("quick".parse(), Ok(Command::Quick(QuickCmd::Visibility)));
        assert_eq!("quick.type".parse(), Ok(Command::Quick(QuickCmd::Type)));
        assert_eq!(
            "session.split.close".parse(),
            Ok(Command::Session(SessionCmd::SplitClose))
        );
        assert_eq!(
            "session.overlay.job.run".parse(),
            Ok(Command::Session(SessionCmd::OverlayJobRun))
        );
        assert_eq!(
            "sidebar.flagged-layout".parse(),
            Ok(Command::Sidebar(SidebarCmd::FlaggedLayout))
        );
    }

    #[test]
    fn refuses_names_outside_the_protocol() {
        for name in [
            "",
            "session",
            "tree.changed",
            "version.",
            "Session.new",
            "session.new ",
            "nope.list",
            ".",
        ] {
            assert_eq!(
                name.parse::<Command>(),
                Err(UnknownCommand(name.to_owned())),
                "{name:?}"
            );
        }
        assert_eq!(
            UnknownCommand("nope.list".to_owned()).to_string(),
            "Cannot initialize Command from invalid String value nope.list"
        );
    }

    #[test]
    fn every_command_round_trips_through_its_wire_name() {
        let all = Command::all();
        assert_eq!(all.len(), 110);
        for command in all {
            assert_eq!(command.wire_name().parse(), Ok(command));
            assert_eq!(
                command.wire_name().split('.').next(),
                Some(<&str>::from(command.group()))
            );
        }
    }

    #[test]
    fn has_twenty_two_groups() {
        assert_eq!(Group::iter().count(), 22);
    }

    #[test]
    fn serializes_as_the_wire_string() {
        let json = serde_json::to_string(&Command::Session(SessionCmd::SelectAll)).unwrap();
        assert_eq!(json, r#""session.selectall""#);
        let back: Command = serde_json::from_str(r#""window.fullscreen""#).unwrap();
        assert_eq!(back, Command::Window(WindowCmd::Fullscreen));
        assert!(serde_json::from_str::<Command>(r#""window.nope""#).is_err());
    }
}
