//! The agent integrations' GPUI side (the GTK build's `integrations/ui.rs`, upstream's `run()` of `SkillInstaller`,
//! `AgentHooksInstaller` and `CLIInstaller` and the alerts they show), over `agterm-app`'s planners: Help's three
//! installers ([`run_skill`], [`run_hooks`], [`run_cli`]) and the welcome's Install ([`run_picked`]). Each plans from
//! what it finds on disk and applies off the main thread, then shows upstream's result alert over the window it started
//! from; installers run one after another, each after the previous one's alert closed, as upstream's modal alerts
//! queue. The hooks install probes `OpenCode` in the login shell and asks for its version (upstream's chooser) when the
//! probe could not tell; a second hooks start while one runs does nothing. A missing home, or a panic in the work,
//! shows "Install Failed" rather than leaving the alert never to come.
//!
//! The installers read the process's own environment ([`Environment::process`]: `$HOME`, the staged resources, the
//! bundled `agtermctl`, the variables the `OpenCode` probe runs with) unless a [`Source`] was installed (the tests' temp
//! home).

use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;

use agterm_app::integrations::hooks::{self, OpenCodeStart};
use agterm_app::integrations::opencode::{self, Version};
use agterm_app::integrations::{Environment, InstallError, cli, skill};
use gpui_kit::{AnyWindowHandle, App, AppContext as _, AsyncApp, Global};

use crate::modal::{AlertButton, AlertSpec, open_alert};

thread_local! {
    /// The hooks install in flight: a second start while it runs (its `OpenCode` probe, its chooser) does nothing.
    static HOOKS_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// The failure text of an install whose work panicked (the GTK build's).
pub const STOPPED: &str = "The install stopped unexpectedly.";
/// The failure text without a home directory to install into (the GTK build's).
pub const NO_HOME: &str = "No home directory was found to install into.";
/// The heading of every failure (upstream's).
pub const FAILED_TITLE: &str = "Install Failed";
/// The result alert's second button while the Codex hooks need a hand merge (upstream's).
pub const OPEN_DOCS: &str = "Open Docs";

/// Where the installers find the home, the bundle and the variables: called off the main thread at each install, so
/// a change of `$HOME` or the bundle between installs is seen. `None`: no home to install into.
#[derive(Clone)]
pub struct Source(Arc<dyn Fn() -> Option<Environment> + Send + Sync>);

impl Global for Source {}

impl Source {
    /// The process's own environment, the app's.
    #[must_use]
    pub fn process() -> Self {
        Self(Arc::new(Environment::process))
    }

    /// An environment of the caller's (a test's temp home).
    pub fn new(environment: impl Fn() -> Option<Environment> + Send + Sync + 'static) -> Self {
        Self(Arc::new(environment))
    }

    /// Makes `self` what every later install reads.
    pub fn install(self, cx: &mut App) {
        cx.set_global(self);
    }

    fn of(cx: &AsyncApp) -> Self {
        cx.try_read_global(|source: &Self, _| source.clone())
            .unwrap_or_else(Self::process)
    }

    fn environment(&self) -> Result<Environment, InstallError> {
        (self.0)().ok_or_else(|| InstallError(NO_HOME.to_owned()))
    }
}

/// Help's Install Agent Skill…: the skill into each coding agent's skills directory, its result over `window`.
pub fn run_skill(window: AnyWindowHandle, cx: &mut App) {
    run_picked(window, true, false, cx);
}

/// Help's Install Agent Status Hooks…: the hooks package and every agent's wiring, its result over `window`.
pub fn run_hooks(window: AnyWindowHandle, cx: &mut App) {
    run_picked(window, false, true, cx);
}

/// The welcome's Install: the checked installers over `window`, the skill first, each after the previous one's alert.
pub fn run_picked(window: AnyWindowHandle, skill: bool, hooks: bool, cx: &mut App) {
    cx.spawn(async move |cx| {
        if skill {
            install_skill(window, cx).await;
        }
        if hooks {
            install_hooks(window, cx).await;
        }
    })
    .detach();
}

/// Help's Install Command Line Tool…: links `agtermctl` onto the `PATH`, its result over `window`.
pub fn run_cli(window: AnyWindowHandle, cx: &mut App) {
    cx.spawn(async move |cx| install_cli(window, cx).await).detach();
}

/// Runs `job` off the main thread; `None` when it panicked.
async fn off_main<T: Send + 'static>(cx: &AsyncApp, job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    cx.background_spawn(async move { panic::catch_unwind(AssertUnwindSafe(job)).ok() })
        .await
}

/// Runs an install's `job` over the source's environment off the main thread; a missing home or a panic is the
/// install's failure.
async fn install_off_main<T: Send + 'static>(
    cx: &AsyncApp,
    job: impl FnOnce(Environment) -> Result<T, InstallError> + Send + 'static,
) -> Result<T, InstallError> {
    let source = Source::of(cx);
    off_main(cx, move || source.environment().and_then(job))
        .await
        .unwrap_or_else(|| Err(InstallError(STOPPED.to_owned())))
}

async fn install_cli(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let result = install_off_main(cx, |env| {
        let path = env.vars.get("PATH").cloned();
        Ok(cli::apply(&cli::plan(&env), path.as_deref()))
    })
    .await;
    match result {
        Ok(result) => {
            log_result(result.is_warning(), "command line tool", &result);
            present(window, result.title(), &result.text(), None, cx).await;
        }
        Err(error) => failed(window, &error, cx).await,
    }
}

async fn install_skill(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let report = install_off_main(cx, |env| skill::plan(&env).map(|plan| skill::apply(&plan))).await;
    match report {
        Ok(report) => {
            log_result(report.is_warning(), "agent skill", &report);
            present(window, report.title(), &report.text(), None, cx).await;
        }
        Err(error) => failed(window, &error, cx).await,
    }
}

/// Clears [`HOOKS_RUNNING`] however the install ends, a dropped task included.
struct HooksRunning;

impl HooksRunning {
    /// `None` while another hooks install runs.
    fn start() -> Option<Self> {
        // never built while another runs: dropping one would clear the other's flag
        if HOOKS_RUNNING.replace(true) { None } else { Some(Self) }
    }
}

impl Drop for HooksRunning {
    fn drop(&mut self) {
        HOOKS_RUNNING.set(false);
    }
}

async fn install_hooks(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let Some(running) = HooksRunning::start() else {
        tracing::debug!("agent status hooks install already running");
        return;
    };
    let outcome = hooks_outcome(window, cx).await;
    drop(running);
    match outcome {
        Ok(outcome) => {
            log_result(outcome.is_warning(), "agent status hooks", &outcome);
            let docs = outcome
                .codex
                .needs_manual_merge()
                .then_some(hooks::CODEX_MANUAL_DOCS_URL);
            present(window, outcome.title(), &outcome.text(), docs, cx).await;
        }
        Err(error) => failed(window, &error, cx).await,
    }
}

/// Probes `OpenCode` in the login shell, asks for its version when the probe could not tell, then plans and applies.
async fn hooks_outcome(window: AnyWindowHandle, cx: &mut AsyncApp) -> Result<hooks::HooksOutcome, InstallError> {
    let env = install_off_main(cx, Ok).await?;
    let probing = env.clone();
    // a probe that panicked found nothing, as one whose shell did not run
    let probe = off_main(cx, move || opencode::detect(&probing.vars, &probing.home_str()))
        .await
        .flatten();
    let start = match hooks::opencode_start(&env.home, probe.as_ref()) {
        OpenCodeStart::Ask => match choose_opencode_version(window, cx).await {
            Some(version) => OpenCodeStart::Install(version),
            None => OpenCodeStart::Decided(hooks::OpenCodeResult::Skipped),
        },
        start => start,
    };
    off_main(cx, move || {
        hooks::plan(&env, &start, probe.as_ref()).and_then(|plan| hooks::apply(&plan))
    })
    .await
    .unwrap_or_else(|| Err(InstallError(STOPPED.to_owned())))
}

/// Upstream's `makeOpenCodeVersionAlert`: Skip `OpenCode`, then one button per supported version; `None` when the
/// user skips `OpenCode` (or the window went first).
async fn choose_opencode_version(window: AnyWindowHandle, cx: &mut AsyncApp) -> Option<Version> {
    let index = ask(window, opencode_chooser(), cx).await?;
    chosen_version(index)
}

/// The version the chooser's button `index` picks: none for Skip `OpenCode`.
#[must_use]
pub fn chosen_version(index: usize) -> Option<Version> {
    index.checked_sub(1).and_then(|index| Version::ALL.get(index).copied())
}

/// The `OpenCode` version chooser's alert: Skip `OpenCode` (also Escape), then each version.
#[must_use]
pub fn opencode_chooser() -> AlertSpec {
    let mut buttons = vec![AlertButton::cancel("Skip OpenCode")];
    buttons.extend(
        Version::ALL
            .into_iter()
            .map(|version| AlertButton::plain(format!("OpenCode {}", version.raw()))),
    );
    AlertSpec {
        heading: hooks::OPENCODE_CHOOSER_TITLE.to_owned(),
        body: hooks::OPENCODE_CHOOSER_TEXT.to_owned(),
        buttons,
        checks: Vec::new(),
        field: None,
    }
}

/// A result alert: OK (Return and Escape), plus Open Docs second when `docs` is set, so OK stays the default.
#[must_use]
pub fn result_alert(title: &str, text: &str, docs: bool) -> AlertSpec {
    let mut buttons = vec![AlertButton::cancel("OK").as_default().suggested()];
    if docs {
        buttons.push(AlertButton::plain(OPEN_DOCS));
    }
    AlertSpec {
        heading: title.to_owned(),
        body: text.to_owned(),
        buttons,
        checks: Vec::new(),
        field: None,
    }
}

/// Opens `spec` over `window` and waits for its answer: the button's index, `None` when the window went first (its
/// alerts are answered with their cancel button as it closes, so this is the window already gone).
async fn ask(window: AnyWindowHandle, spec: AlertSpec, cx: &mut AsyncApp) -> Option<usize> {
    let (answered, answer) = async_channel::bounded(1);
    let opened = cx.update_window(window, |_, window, cx| {
        open_alert(
            window,
            spec,
            move |reply, _, _| {
                let _ = answered.try_send(reply.button);
            },
            cx,
        );
    });
    if opened.is_err() {
        tracing::warn!("the window an install started from is gone: its alert is not shown");
        return None;
    }
    answer.recv().await.ok()
}

async fn present(window: AnyWindowHandle, title: &str, text: &str, docs: Option<&str>, cx: &mut AsyncApp) {
    let button = ask(window, result_alert(title, text, docs.is_some()), cx).await;
    if let Some(docs) = docs.filter(|_| button == Some(1)) {
        let docs = docs.to_owned();
        cx.update(|cx| cx.open_url(&docs));
    }
}

async fn failed(window: AnyWindowHandle, error: &InstallError, cx: &mut AsyncApp) {
    tracing::warn!(error = %error.0, "install failed");
    present(window, FAILED_TITLE, &error.0, None, cx).await;
}

/// The install's result in the log, at warning level when the user has something to act on (upstream's warning-style
/// alert).
fn log_result(warning: bool, what: &str, result: &dyn std::fmt::Debug) {
    if warning {
        tracing::warn!(?result, "{what} install finished with a warning");
    } else {
        tracing::info!(?result, "{what} install finished");
    }
}

#[cfg(test)]
mod tests;
