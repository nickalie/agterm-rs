//! How an overlay program starts and how its exit status comes back (upstream's `OverlayCapture.swift`).
//!
//! The real command and the status file's path ride environment variables, so the wrapper never interpolates the
//! caller's command text. The wrapper redirects neither stdout nor stderr: a terminal UI program renders normally and
//! only its exit status is captured, into a file the frontend's surface leaves behind and the core reads when the
//! overlay's terminal is torn down.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::pane_env::PaneEnv;

/// The variable carrying the caller's command (`AGTERM_OVL_CMD`).
pub const CMD_ENV_KEY: &str = "AGTERM_OVL_CMD";
/// The variable naming the exit-status file (`AGTERM_OVL_CODE`).
pub const CODE_ENV_KEY: &str = "AGTERM_OVL_CODE";

/// The wrapper's shell line: runs the command in a subshell and writes its status to the file.
pub const SHELL_LINE: &str = r#"( eval "$AGTERM_OVL_CMD" ); echo $? > "$AGTERM_OVL_CODE""#;

/// The command an overlay terminal runs (upstream's `surfaceCommand`): `env` takes the dash libghostty's `exec -l`
/// puts on argv[0], so `/bin/sh` starts non-login.
#[must_use]
pub fn surface_command() -> String {
    format!("/usr/bin/env /bin/sh -c '{SHELL_LINE}'")
}

/// The status a wrapper wrote, `None` for anything that is not a whole number once trimmed.
#[must_use]
pub fn parse_exit_code(text: &str) -> Option<i32> {
    text.trim().parse().ok()
}

/// A fresh status-file path in the temporary directory, one per overlay terminal, so two overlays never share one.
#[must_use]
pub fn mint_code_file() -> PathBuf {
    std::env::temp_dir().join(format!(
        "agterm-ovl-{}.code",
        crate::pane_env::uuid_string(Uuid::new_v4())
    ))
}

/// Reads and deletes a status file: the status it holds, `None` when it is missing, unreadable or malformed (a program
/// killed by its overlay's close never writes one).
#[must_use]
pub fn take_exit_code(file: &Path) -> Option<i32> {
    let code = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| parse_exit_code(&text));
    if code.is_none() {
        tracing::debug!(file = %file.display(), "overlay exit-code file unreadable or empty");
    }
    if let Err(error) = std::fs::remove_file(file)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(file = %file.display(), %error, "could not delete the overlay exit-code file");
    }
    code
}

/// What an overlay program starts with (upstream's `OverlayLaunchContext`), built once on the instance that owns the
/// session whether the program runs under a local terminal or under a viewer's supervising helper. Only the local
/// launch adds its exit-status file; a helper reports the status itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayLaunchContext {
    pub command: String,
    pub cwd: String,
    pub environment: BTreeMap<String, String>,
}

impl OverlayLaunchContext {
    /// The context of `command` in `cwd`; `session_environment` is the owning session's surface environment. The
    /// command rides `AGTERM_OVL_CMD`, so no launcher interpolates it.
    #[must_use]
    pub fn new(command: &str, cwd: &str, session_environment: PaneEnv) -> Self {
        let mut environment = session_environment;
        environment.insert(CMD_ENV_KEY.to_owned(), command.to_owned());
        Self {
            command: command.to_owned(),
            cwd: cwd.to_owned(),
            environment,
        }
    }

    /// The environment of a local overlay terminal: this context plus the wrapper's status file, and the body file
    /// when the slot holds a HUD (upstream's `localEnvironment`).
    #[must_use]
    pub fn local_environment(&self, code_file: &Path, hud_file: Option<&Path>) -> PaneEnv {
        let mut environment = self.environment.clone();
        environment.insert(CODE_ENV_KEY.to_owned(), code_file.to_string_lossy().into_owned());
        if let Some(hud_file) = hud_file {
            environment.insert(
                crate::hud::FILE_ENV_KEY.to_owned(),
                hud_file.to_string_lossy().into_owned(),
            );
        }
        environment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_reads_the_command_and_file_from_the_environment() {
        assert_eq!(
            surface_command(),
            r#"/usr/bin/env /bin/sh -c '( eval "$AGTERM_OVL_CMD" ); echo $? > "$AGTERM_OVL_CODE"'"#
        );
    }

    #[test]
    fn exit_codes_parse_trimmed_whole_numbers_only() {
        assert_eq!(parse_exit_code("7\n"), Some(7));
        assert_eq!(parse_exit_code("  0 "), Some(0));
        assert_eq!(parse_exit_code(""), None);
        assert_eq!(parse_exit_code("x"), None);
    }

    #[test]
    fn a_status_file_is_read_once_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.code");
        std::fs::write(&file, "5\n").unwrap();
        assert_eq!(take_exit_code(&file), Some(5));
        assert!(!file.exists());
        assert_eq!(take_exit_code(&file), None, "gone: nothing left to read");
    }

    #[test]
    fn the_context_carries_the_command_and_the_local_launch_its_files() {
        let session = PaneEnv::from([("AGTERM_SESSION_ID".to_owned(), "S".to_owned())]);
        let context = OverlayLaunchContext::new("exit 3", "/tmp", session);
        assert_eq!(context.environment[CMD_ENV_KEY], "exit 3");
        assert_eq!(context.environment["AGTERM_SESSION_ID"], "S");
        let local = context.local_environment(Path::new("/t/c.code"), Some(Path::new("/t/h.body")));
        assert_eq!(local[CODE_ENV_KEY], "/t/c.code");
        assert_eq!(local[crate::hud::FILE_ENV_KEY], "/t/h.body");
        assert!(
            !context.environment.contains_key(CODE_ENV_KEY),
            "only the local launch adds the file"
        );
    }

    #[test]
    fn the_context_round_trips_as_upstream_names_it() {
        let context = OverlayLaunchContext::new("ls", "/", PaneEnv::new());
        let json = serde_json::to_value(&context).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"command": "ls", "cwd": "/", "environment": {"AGTERM_OVL_CMD": "ls"}})
        );
        assert_eq!(serde_json::from_value::<OverlayLaunchContext>(json).unwrap(), context);
    }
}
