//! The installers' GPUI side (the GTK build's `integrations/ui.rs`, upstream's `run()` of each installer and the
//! alerts they show), over `agterm-app`'s planners: the welcome's Install runs the checked ones. The filesystem work
//! runs off the main thread, the result shows as an alert over the window the install started from; each installer's
//! alert closes before the next one starts, as upstream's queue behind one another. The app menu's Help runs each on
//! its own, and the command line tool's installer ([`run_cli`]). (The integrations' settings come with Task 28.)

use std::cell::Cell;

use agterm_app::integrations::hooks::{self, OpenCodeStart};
use agterm_app::integrations::opencode::{self, Version};
use agterm_app::integrations::{Environment, InstallError, cli, skill};
use gpui_kit::{AnyWindowHandle, App, AppContext as _, AsyncApp};

use crate::modal::{AlertButton, AlertSpec, open_alert};

thread_local! {
    /// The hooks install in flight: a second start while it runs (its `OpenCode` probe, its chooser) does nothing.
    static HOOKS_RUNNING: Cell<bool> = const { Cell::new(false) };
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

async fn install_cli(window: AnyWindowHandle, cx: &mut AsyncApp) {
    let result = cx
        .background_spawn(async {
            let env = Environment::process().ok_or_else(no_home)?;
            let path = env.vars.get("PATH").cloned();
            Ok::<_, InstallError>(cli::apply(&cli::plan(&env), path.as_deref()))
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
    let report = cx
        .background_spawn(async {
            let env = Environment::process().ok_or_else(no_home)?;
            skill::plan(&env).map(|plan| skill::apply(&plan))
        })
        .await;
    match report {
        Ok(report) => {
            log_result(report.is_warning(), "agent skill", &report);
            present(window, report.title(), &report.text(), None, cx).await;
        }
        Err(error) => failed(window, &error, cx).await,
    }
}

async fn install_hooks(window: AnyWindowHandle, cx: &mut AsyncApp) {
    if HOOKS_RUNNING.replace(true) {
        return;
    }
    let outcome = hooks_outcome(window, cx).await;
    HOOKS_RUNNING.set(false);
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
    let env = Environment::process().ok_or_else(no_home)?;
    let probing = env.clone();
    let probe = cx
        .background_spawn(async move { opencode::detect(&probing.vars, &probing.home_str()) })
        .await;
    let start = match hooks::opencode_start(&env.home, probe.as_ref()) {
        OpenCodeStart::Ask => match choose_opencode_version(window, cx).await {
            Some(version) => OpenCodeStart::Install(version),
            None => OpenCodeStart::Decided(hooks::OpenCodeResult::Skipped),
        },
        start => start,
    };
    cx.background_spawn(async move { hooks::plan(&env, &start, probe.as_ref()).and_then(|plan| hooks::apply(&plan)) })
        .await
}

/// Upstream's `makeOpenCodeVersionAlert`: Skip `OpenCode`, then one button per supported version; `None` when the
/// user skips `OpenCode`.
async fn choose_opencode_version(window: AnyWindowHandle, cx: &mut AsyncApp) -> Option<Version> {
    let index = ask(window, opencode_chooser(), cx).await?;
    index.checked_sub(1).and_then(|index| Version::ALL.get(index).copied())
}

/// The `OpenCode` version chooser's alert.
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

/// A result alert: OK, plus Open Docs second when `docs` is set, so OK stays the default.
#[must_use]
pub fn result_alert(title: &str, text: &str, docs: bool) -> AlertSpec {
    let mut buttons = vec![AlertButton::cancel("OK").as_default().suggested()];
    if docs {
        buttons.push(AlertButton::plain("Open Docs"));
    }
    AlertSpec {
        heading: title.to_owned(),
        body: text.to_owned(),
        buttons,
        checks: Vec::new(),
        field: None,
    }
}

/// Opens `spec` over `window` and waits for its answer: the button's index, `None` when the window went first.
async fn ask(window: AnyWindowHandle, spec: AlertSpec, cx: &mut AsyncApp) -> Option<usize> {
    let (answered, answer) = async_channel::bounded(1);
    cx.update_window(window, |_, window, cx| {
        open_alert(
            window,
            spec,
            move |reply, _, _| {
                let _ = answered.try_send(reply.button);
            },
            cx,
        );
    })
    .ok()?;
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
    present(window, "Install Failed", &error.0, None, cx).await;
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

fn no_home() -> InstallError {
    InstallError("No home directory was found to install into.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_docs_button_is_second_so_ok_stays_the_default() {
        let alert = result_alert("Installed", "done", true);
        assert_eq!(alert.default_button(), Some(0));
        assert_eq!(alert.cancel_button(), Some(0));
        assert_eq!(alert.buttons[1].label.as_ref(), "Open Docs");
        assert_eq!(result_alert("Installed", "done", false).buttons.len(), 1);
    }

    #[test]
    fn the_opencode_chooser_skips_first_then_offers_each_version() {
        let alert = opencode_chooser();
        assert_eq!(alert.buttons[0].label.as_ref(), "Skip OpenCode");
        assert_eq!(alert.cancel_button(), Some(0), "Escape skips");
        assert_eq!(alert.buttons.len(), Version::ALL.len() + 1);
        assert_eq!(
            alert.buttons[1].label.as_ref(),
            format!("OpenCode {}", Version::ALL[0].raw())
        );
    }
}
