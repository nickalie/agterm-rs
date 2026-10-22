//! The ngterm identity and `NGTERM_*` values a spawned shell gets, as upstream's
//! `agtermCore/Sources/agtermCore/SurfaceEnvironment.swift`. The frontend owns shell creation; this keeps the variable
//! set testable.
//!
//! `NGTERM_SOCKET` is always set, never omitted: a consumer reads an absent one as "resolve the default", which is
//! another instance's. A refused instance passes its `<socket>.unavailable` path.

use std::collections::BTreeMap;

use uuid::Uuid;

/// Variable name to value, sorted by name.
pub type PaneEnv = BTreeMap<String, String>;

/// Which pane of a session a surface fills (upstream's `StatusPane`), baked in as `NGTERM_PANE` so the status hook
/// forwards `--pane` and a background pane's status records which surface blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPane {
    /// The main pane.
    Left,
    /// The split pane.
    Right,
    Scratch,
}

impl StatusPane {
    /// The stable spelling of read-back and `NGTERM_PANE`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Scratch => "scratch",
        }
    }

    /// Parses a pane selector with its role and position aliases (upstream's `StatusPane(controlName:)`):
    /// `left|top|primary`, `right|bottom|split`, `scratch`.
    #[must_use]
    pub fn from_control_name(name: &str) -> Option<Self> {
        match name {
            "left" | "top" | "primary" => Some(Self::Left),
            "right" | "bottom" | "split" => Some(Self::Right),
            "scratch" => Some(Self::Scratch),
            _ => None,
        }
    }
}

/// A session-owned surface: main pane, split pane, overlay or scratch.
#[derive(Debug, Clone, Copy)]
pub struct SessionPane<'a> {
    pub session: Uuid,
    /// The owning window; omitted when unknown.
    pub window: Option<Uuid>,
    /// The owning workspace; omitted when unknown.
    pub workspace: Option<Uuid>,
    /// What the instance advertises: its socket, or `<socket>.unavailable` when refused.
    pub socket_path: &'a str,
    /// `AppIdentity.version`.
    pub program_version: &'a str,
    /// `None` for an overlay surface.
    pub pane: Option<StatusPane>,
    /// The surface's stable spawn identity, `NGTERM_PANE_ID`, which the hook forwards as `--pane-id` so the status
    /// handler resolves the live role after a promote and re-split; omitted when `None` or empty.
    pub pane_token: Option<&'a str>,
}

/// Foundation's `UUID.uuidString`: hyphenated, upper case.
#[must_use]
pub fn uuid_string(id: Uuid) -> String {
    id.hyphenated().to_string().to_uppercase()
}

/// `TERM_PROGRAM` and its version: embedded libghostty defaults them to Ghostty, so Ghostty-aware tools would look
/// for a standalone Ghostty.
fn terminal_identity(program_version: &str) -> PaneEnv {
    PaneEnv::from([
        ("TERM_PROGRAM".to_owned(), "ngterm".to_owned()),
        ("TERM_PROGRAM_VERSION".to_owned(), program_version.to_owned()),
    ])
}

/// The environment of a session-owned surface.
#[must_use]
pub fn session(pane: &SessionPane<'_>) -> PaneEnv {
    let mut env = terminal_identity(pane.program_version);
    env.insert("NGTERM_ENABLED".to_owned(), "1".to_owned());
    env.insert("NGTERM_SESSION_ID".to_owned(), uuid_string(pane.session));
    env.insert("NGTERM_SOCKET".to_owned(), pane.socket_path.to_owned());
    if let Some(window) = pane.window {
        env.insert("NGTERM_WINDOW_ID".to_owned(), uuid_string(window));
    }
    if let Some(workspace) = pane.workspace {
        env.insert("NGTERM_WORKSPACE_ID".to_owned(), uuid_string(workspace));
    }
    if let Some(role) = pane.pane {
        env.insert("NGTERM_PANE".to_owned(), role.as_str().to_owned());
    }
    if let Some(token) = pane.pane_token.filter(|token| !token.is_empty()) {
        env.insert("NGTERM_PANE_ID".to_owned(), token.to_owned());
    }
    env
}

/// The environment of the quick terminal: one panel per app, outside the session tree and owned by no window, so an
/// untargeted `ngtermctl` run from it resolves the active window like any other caller.
#[must_use]
pub fn quick_terminal(socket_path: &str, program_version: &str) -> PaneEnv {
    let mut env = terminal_identity(program_version);
    env.insert("NGTERM_ENABLED".to_owned(), "1".to_owned());
    env.insert("NGTERM_SOCKET".to_owned(), socket_path.to_owned());
    env
}

/// Ported from upstream's `SurfaceEnvironmentTests.swift`.
#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: Uuid = Uuid::from_u128(0x1111_1111_1111_1111_1111_1111_1111_1111);
    const WINDOW: Uuid = Uuid::from_u128(0x2222_2222_2222_2222_2222_2222_2222_2222);
    const WORKSPACE: Uuid = Uuid::from_u128(0x3333_3333_3333_3333_3333_3333_3333_3333);
    const SOCKET: &str = "/tmp/ngterm.sock";

    fn bare() -> SessionPane<'static> {
        SessionPane {
            session: SESSION,
            window: None,
            workspace: None,
            socket_path: SOCKET,
            program_version: "0.12.0",
            pane: None,
            pane_token: None,
        }
    }

    fn env_of(pairs: &[(&str, &str)]) -> PaneEnv {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn terminal_identity_belongs_to_ngterm() {
        let session_env = session(&SessionPane {
            window: Some(WINDOW),
            ..bare()
        });
        for env in [session_env, quick_terminal(SOCKET, "0.12.0")] {
            assert_eq!(env["TERM_PROGRAM"], "ngterm");
            assert_eq!(env["TERM_PROGRAM_VERSION"], "0.12.0");
        }
    }

    #[test]
    fn session_environment_carries_all_known_identifiers() {
        let env = session(&SessionPane {
            window: Some(WINDOW),
            workspace: Some(WORKSPACE),
            ..bare()
        });
        assert_eq!(
            env,
            env_of(&[
                ("NGTERM_ENABLED", "1"),
                ("NGTERM_SESSION_ID", "11111111-1111-1111-1111-111111111111"),
                ("NGTERM_WINDOW_ID", "22222222-2222-2222-2222-222222222222"),
                ("NGTERM_WORKSPACE_ID", "33333333-3333-3333-3333-333333333333"),
                ("NGTERM_SOCKET", SOCKET),
                ("TERM_PROGRAM", "ngterm"),
                ("TERM_PROGRAM_VERSION", "0.12.0"),
            ])
        );
    }

    #[test]
    fn ids_are_upper_case_like_foundation() {
        let id = Uuid::from_u128(0xabcd_ef01_2345_6789_abcd_ef01_2345_6789);
        let env = session(&SessionPane { session: id, ..bare() });
        assert_eq!(env["NGTERM_SESSION_ID"], "ABCDEF01-2345-6789-ABCD-EF0123456789");
    }

    #[test]
    fn session_environment_omits_unknown_window_and_workspace() {
        assert_eq!(
            session(&bare()),
            env_of(&[
                ("NGTERM_ENABLED", "1"),
                ("NGTERM_SESSION_ID", "11111111-1111-1111-1111-111111111111"),
                ("NGTERM_SOCKET", SOCKET),
                ("TERM_PROGRAM", "ngterm"),
                ("TERM_PROGRAM_VERSION", "0.12.0"),
            ])
        );
    }

    #[test]
    fn session_environment_injects_the_pane_when_given() {
        for (role, expected) in [
            (StatusPane::Left, "left"),
            (StatusPane::Right, "right"),
            (StatusPane::Scratch, "scratch"),
        ] {
            let env = session(&SessionPane {
                pane: Some(role),
                ..bare()
            });
            assert_eq!(env["NGTERM_PANE"], expected);
            assert_eq!(env["NGTERM_ENABLED"], "1");
            assert_eq!(env["NGTERM_SESSION_ID"], "11111111-1111-1111-1111-111111111111");
            assert_eq!(env["NGTERM_SOCKET"], SOCKET);
        }
    }

    #[test]
    fn session_environment_omits_the_pane_when_none() {
        let env = session(&SessionPane {
            window: Some(WINDOW),
            workspace: Some(WORKSPACE),
            ..bare()
        });
        assert!(!env.contains_key("NGTERM_PANE"));
    }

    #[test]
    fn session_environment_injects_the_pane_token_when_given() {
        let env = session(&SessionPane {
            pane: Some(StatusPane::Right),
            pane_token: Some("surface-token-abc"),
            ..bare()
        });
        assert_eq!(env["NGTERM_PANE_ID"], "surface-token-abc");
        assert_eq!(env["NGTERM_PANE"], "right");
    }

    #[test]
    fn session_environment_omits_a_missing_or_empty_pane_token() {
        for token in [None, Some("")] {
            let env = session(&SessionPane {
                pane: Some(StatusPane::Left),
                pane_token: token,
                ..bare()
            });
            assert!(!env.contains_key("NGTERM_PANE_ID"), "{token:?}");
        }
    }

    #[test]
    fn quick_terminal_environment_omits_session_workspace_and_window_identifiers() {
        assert_eq!(
            quick_terminal(SOCKET, "0.12.0"),
            env_of(&[
                ("NGTERM_ENABLED", "1"),
                ("NGTERM_SOCKET", SOCKET),
                ("TERM_PROGRAM", "ngterm"),
                ("TERM_PROGRAM_VERSION", "0.12.0"),
            ])
        );
    }

    #[test]
    fn an_empty_socket_path_is_still_emitted() {
        let env = session(&SessionPane {
            socket_path: "",
            ..bare()
        });
        assert_eq!(env["NGTERM_SOCKET"], "");
    }

    #[test]
    fn both_variants_always_set_the_socket_variable() {
        let unavailable = "/tmp/ngterm.sock.unavailable";
        let session_env = session(&SessionPane {
            socket_path: unavailable,
            ..bare()
        });
        assert_eq!(session_env["NGTERM_SOCKET"], unavailable);
        assert_eq!(quick_terminal(unavailable, "0.12.0")["NGTERM_SOCKET"], unavailable);
    }
}
