//! `hooks.reload` / `hooks.list` (upstream's `ControlDispatcher+Hooks.swift` and the app's `reloadHooks`/`listHooks`),
//! and the app side of `hooks.conf`: the scheduler fed every event the ring sequences (upstream's
//! `HookController` over `WindowLibrary.onControlEvent`), its children started through the frontend's launcher
//! (`HookProcessRunner`), and its failures posted as banners.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use ngterm_host::{LaunchSpec, ProcessExit, ProcessLauncher};
use ngterm_proto::command::HooksCmd;
use ngterm_proto::{Command, ControlEvent, ControlHooks, ControlKeymapDiagnostic, ControlResult, Request};

use super::process::CorePoster;
use crate::Core;
use crate::error::ControlError;
use crate::hooks::{
    HookEntry, HookLauncher, HookRun, HookRunToken, HookScheduler, ensure_starter_hooks, hook_environment, hook_stdin,
    hooks_path, load_hooks,
};
use crate::keymap::KeymapDiagnostic;

/// The hooks the app runs and the last load's diagnostics.
#[derive(Default)]
pub(crate) struct HookRuntime {
    scheduler: HookScheduler,
    diagnostics: Vec<KeymapDiagnostic>,
}

impl fmt::Debug for HookRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRuntime")
            .field("diagnostics", &self.diagnostics)
            .finish_non_exhaustive()
    }
}

/// Starts hook children through the frontend's launcher; each exit comes back to [`Core::hook_exited`] on the main
/// loop.
struct Launcher<'a> {
    host: Option<(&'a Arc<dyn ProcessLauncher>, &'a CorePoster)>,
    socket: &'a str,
    path: &'a str,
}

impl HookLauncher for Launcher<'_> {
    fn launch(&mut self, run: HookRun<'_>) -> Result<i32, String> {
        let Some((processes, poster)) = self.host else {
            return Err("no process launcher".to_owned());
        };
        let (stdin, encode_failure) = match hook_stdin(run.event) {
            Ok(bytes) => (bytes, None),
            Err(err) => (Vec::new(), Some(format!("encode: {err}"))),
        };
        let mut env = hook_environment(run.event, self.socket);
        env.push(("PATH".to_owned(), self.path.to_owned()));
        let spec = LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".to_owned(), run.entry.command().to_owned()],
            env,
            cwd: None,
            stdin: Some(stdin),
            capture_stderr: false,
        };
        let token = run.token;
        let poster = Arc::clone(poster);
        let on_exit = Box::new(move |mut exit: ProcessExit| {
            if exit.stdin_failure.is_none() {
                exit.stdin_failure = encode_failure;
            }
            poster(Box::new(move |core: &mut Core| core.hook_exited(&token, &exit)));
        });
        let started = processes.launch(&spec, on_exit).map_err(|err| err.to_string())?;
        i32::try_from(started.pid).map_err(|_| format!("pid {} out of range", started.pid))
    }
}

impl Core {
    /// The problems the last `hooks.conf` read found (Settings ▸ Key Mapping's Hooks section).
    #[must_use]
    pub fn hooks_diagnostics(&self) -> &[KeymapDiagnostic] {
        &self.hooks.diagnostics
    }

    /// The `hooks.conf` the app reads.
    #[must_use]
    pub fn hooks_path(&self) -> PathBuf {
        hooks_path(&self.config_dir())
    }

    /// The launch step: writes the commented starter `hooks.conf` when none exists and loads it, quietly (upstream's
    /// `SettingsModel` init and `HookController.start`).
    pub fn load_hooks_at_launch(&mut self) {
        ensure_starter_hooks(&self.hooks_path());
        self.load_hooks_file();
    }

    /// Re-reads `hooks.conf` and applies it (File > Reload Hooks, `hooks.reload`); diagnostics post a banner. The
    /// diagnostic count.
    pub fn reload_hooks(&mut self) -> usize {
        self.load_hooks_file();
        let count = self.hooks.diagnostics.len();
        if count > 0 {
            let body = format!(
                "{count} issue{} — see Settings ▸ Key Mapping",
                if count == 1 { "" } else { "s" }
            );
            self.post_app_banner("hooks-diagnostics", "Hooks", body);
        }
        self.post_hook_failures();
        count
    }

    fn load_hooks_file(&mut self) {
        let (hooks, diagnostics) = load_hooks(&self.hooks_path());
        self.hooks.scheduler.apply(&hooks);
        self.hooks.diagnostics = diagnostics;
    }

    /// Feeds the hooks every event sequenced since the last feed, in order.
    pub(crate) fn run_hooks(&mut self) {
        let events: Vec<ControlEvent> = self.events.take_appended();
        if events.is_empty() {
            return;
        }
        let (socket, path) = (self.child_socket(), self.child_path());
        let mut launcher = Launcher {
            host: self.processes.host.as_ref().map(|host| (&host.launcher, &host.poster)),
            socket: &socket,
            path: &path,
        };
        for event in &events {
            self.hooks.scheduler.dispatch(event, &mut launcher);
        }
        self.post_hook_failures();
    }

    /// A hook child exited: its slot frees and its next queued event starts. Settle afterwards.
    pub fn hook_exited(&mut self, token: &HookRunToken, exit: &ProcessExit) {
        if let Some(message) = &exit.stdin_failure {
            self.hooks.scheduler.delivery_failed(token, message);
        }
        let (socket, path) = (self.child_socket(), self.child_path());
        let mut launcher = Launcher {
            host: self.processes.host.as_ref().map(|host| (&host.launcher, &host.poster)),
            socket: &socket,
            path: &path,
        };
        self.hooks.scheduler.exited(token, exit.status, &mut launcher);
        self.post_hook_failures();
    }

    /// One banner per failed hook until it succeeds or the file reloads (upstream's `notifyHookFailure`).
    fn post_hook_failures(&mut self) {
        for (entry, detail) in self.hooks.scheduler.take_failures() {
            self.post_hook_failure(&entry, &detail);
        }
    }

    fn post_hook_failure(&mut self, entry: &HookEntry, detail: &str) {
        let kind = entry.kind().wire_name();
        let identity = format!("hook-failure:{kind}:{}", entry.command());
        let body = format!("on {kind} {} ({detail})", entry.command());
        self.post_app_banner(identity, "Hook failed", body);
    }
}

/// Answers `hooks.*`: app-global, so a target or `--window` is refused before anything runs.
pub(crate) fn dispatch(core: &mut Core, cmd: HooksCmd, request: &Request) -> Result<ControlResult, ControlError> {
    let window = request.args.as_ref().and_then(|args| args.window.as_ref());
    if request.target.is_some() || window.is_some() {
        return Err(ControlError::rejected(format!(
            "{} takes no target or --window",
            Command::Hooks(cmd).wire_name()
        )));
    }
    match cmd {
        HooksCmd::Reload => Ok(ControlResult::count(
            i64::try_from(core.reload_hooks()).unwrap_or(i64::MAX),
        )),
        HooksCmd::List => Ok(ControlResult::hooks(ControlHooks {
            path: core.hooks_path().to_string_lossy().into_owned(),
            diagnostics: wire_diagnostics(&core.hooks.diagnostics),
            hooks: core.hooks.scheduler.status(),
        })),
    }
}

/// Diagnostics as the read-backs carry them.
pub(crate) fn wire_diagnostics(diagnostics: &[KeymapDiagnostic]) -> Vec<ControlKeymapDiagnostic> {
    diagnostics
        .iter()
        .map(|diagnostic| ControlKeymapDiagnostic {
            line: i64::try_from(diagnostic.line).unwrap_or(i64::MAX),
            message: diagnostic.message.clone(),
        })
        .collect()
}
