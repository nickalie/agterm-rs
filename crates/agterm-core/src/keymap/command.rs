//! Custom commands and their fire-time context (upstream's `CustomCommand.swift`): the parsed `command` line and the
//! `{AGT_X}` token table both the template expansion and the exported environment read.

use uuid::Uuid;

use crate::hud::HudPosition;
use crate::model::OverlayPane;

/// A user-defined command: a shell line run via `/bin/sh -c`, optionally bound to a shortcut, always listed in the
/// action palette. The body may carry `{AGT_X}` tokens, expanded at fire time and exported as `$AGT_X`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    /// Fresh per parse; identifies the command to the matcher.
    pub id: Uuid,
    /// The display name, unique within a file: the palette row and the failure banner's subject.
    pub name: String,
    /// The shell line, `{AGT_X}` tokens unexpanded.
    pub command: String,
    /// The surviving `|`-joined alternatives in the user's own spelling; empty means palette-only.
    pub shortcut: String,
    /// `--error-hud`: a failure also opens a HUD panel.
    pub error_hud: bool,
    /// `--error-position`, the panel's placement.
    pub error_position: HudPosition,
    /// `--error-pane`, the pane the panel is scoped to; `None` for the whole session.
    pub error_pane: Option<OverlayPane>,
    /// `--repeat`: a leader shortcut stays live for another press of its last chord after firing.
    pub repeats: bool,
}

impl CustomCommand {
    /// A command with a fresh id and every option at its default.
    #[must_use]
    pub fn new(name: impl Into<String>, command: impl Into<String>, shortcut: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            command: command.into(),
            shortcut: shortcut.into(),
            error_hud: false,
            error_position: HudPosition::DEFAULT,
            error_pane: None,
            repeats: false,
        }
    }
}

/// Which pane a command fired from. The wire names are exactly the `--pane` argument strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CommandPane {
    /// The main pane, and the only pane of a single-pane session.
    #[default]
    Left,
    /// The split pane.
    Right,
    /// The session's scratch terminal.
    Scratch,
}

impl CommandPane {
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Scratch => "scratch",
        }
    }
}

/// The token base names whose value comes from an active session, workspace or selection. Each expands empty in
/// a session-free context, which is dangerous (`rm -rf {AGT_SESSION_PWD}/*`), so a command naming one stays inert
/// there. The socket, window and pane tokens resolve without a session and are excluded.
pub const SESSION_SCOPED_TOKEN_BASES: [&str; 3] = ["AGT_SESSION", "AGT_WORKSPACE", "AGT_SELECTION"];

/// The resolved session context of one command fire: one field per `{AGT_X}` token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandContext {
    pub session_id: String,
    pub session_name: String,
    pub session_pwd: String,
    /// The SSH destination of an attached remote session, empty for a local one.
    pub session_host: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub window_id: String,
    pub window_name: String,
    /// The pane that had focus at fire time.
    pub pane: CommandPane,
    /// The stable token of the surface in `pane`'s slot, empty for an empty slot or a session-free context.
    pub pane_id: String,
    pub selection: String,
    pub socket: String,
}

/// The `AGT_X` token names in declaration order, the order [`CommandContext::token_names`] lists.
const TOKEN_NAMES: [&str; 12] = [
    "AGT_SESSION_ID",
    "AGT_SESSION_NAME",
    "AGT_SESSION_PWD",
    "AGT_SESSION_HOST",
    "AGT_WORKSPACE_ID",
    "AGT_WORKSPACE_NAME",
    "AGT_WINDOW_ID",
    "AGT_WINDOW_NAME",
    "AGT_PANE",
    "AGT_PANE_ID",
    "AGT_SELECTION",
    "AGT_SOCKET",
];

impl CommandContext {
    /// The `AGT_X` token names a command body may use, in declaration order.
    #[must_use]
    pub fn token_names() -> [&'static str; 12] {
        TOKEN_NAMES
    }

    /// The single table mapping each token name to its value here; [`Self::expand`] and [`Self::environment`]
    /// both read it, so the two sets cannot drift.
    fn tokens(&self) -> impl Iterator<Item = (&'static str, &str)> {
        let values = [
            self.session_id.as_str(),
            &self.session_name,
            &self.session_pwd,
            &self.session_host,
            &self.workspace_id,
            &self.workspace_name,
            &self.window_id,
            &self.window_name,
            self.pane.wire_name(),
            &self.pane_id,
            &self.selection,
            &self.socket,
        ];
        TOKEN_NAMES.into_iter().zip(values)
    }

    /// Whether a command body references a session-scoped token in any form (`{AGT_X}`, `$AGT_X`, `${AGT_X}`): a
    /// plain substring match, the base names being specific enough not to occur by accident.
    #[must_use]
    pub fn references_session_scoped_context(command_body: &str) -> bool {
        SESSION_SCOPED_TOKEN_BASES
            .iter()
            .any(|base| command_body.contains(base))
    }

    /// `template` with each `{AGT_X}` replaced by its value. Single pass, so a value containing a token literal is
    /// not substituted again; an unknown `{...}` stays as written and an unclosed `{` copies the rest verbatim.
    #[must_use]
    pub fn expand(&self, template: &str) -> String {
        let tokens: Vec<(&str, &str)> = self.tokens().collect();
        let mut result = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            result.push_str(&rest[..open]);
            let after_open = &rest[open + 1..];
            let Some(close) = after_open.find('}') else {
                result.push_str(&rest[open..]);
                return result;
            };
            let name = &after_open[..close];
            match tokens.iter().find(|(token, _)| *token == name) {
                Some((_, value)) => result.push_str(value),
                None => result.push_str(&rest[open..=open + 1 + close]),
            }
            rest = &after_open[close + 1..];
        }
        result.push_str(rest);
        result
    }

    /// The `AGT_X` environment exported to the spawned process, in token order.
    #[must_use]
    pub fn environment(&self) -> Vec<(String, String)> {
        self.tokens()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect()
    }
}
