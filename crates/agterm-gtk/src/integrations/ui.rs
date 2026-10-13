//! The installers' GTK side (upstream's `run()` of each installer and the alerts they show): the Help menu items and
//! the welcome's Install run them; the filesystem work runs off the main thread, the result shows as an alert over
//! the frontmost window. Each installer's alert closes before the next one starts, as upstream's queue behind one
//! another.

use std::cell::Cell;

use adw::prelude::*;
use gtk::{gio, glib};

use super::hooks::{self, OpenCodeStart};
use super::opencode::{self, Version};
use super::{Environment, InstallError, cli, skill};

/// Developer Documentation… in the Help menu (upstream's).
pub const DEVELOPER_DOCS_URL: &str = "https://agterm.com/docs#agtermctl";

thread_local! {
    /// The hooks install in flight: a second click while it runs (its `OpenCode` probe, its chooser) does nothing.
    static HOOKS_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// Help ▸ Install Command Line Tool…
pub fn run_cli(parent: Option<adw::ApplicationWindow>) {
    glib::spawn_future_local(async move { install_cli(parent.as_ref()).await });
}

/// Help ▸ Install Agent Skill…
pub fn run_skill(parent: Option<adw::ApplicationWindow>) {
    glib::spawn_future_local(async move { install_skill(parent.as_ref()).await });
}

/// Help ▸ Install Agent Status Hooks…
pub fn run_hooks(parent: Option<adw::ApplicationWindow>) {
    glib::spawn_future_local(async move { install_hooks(parent.as_ref()).await });
}

/// The welcome's Install: the checked installers, the skill first, each after the previous one's alert.
pub fn run_picked(parent: Option<adw::ApplicationWindow>, skill: bool, hooks: bool) {
    glib::spawn_future_local(async move {
        if skill {
            install_skill(parent.as_ref()).await;
        }
        if hooks {
            install_hooks(parent.as_ref()).await;
        }
    });
}

/// Opens the developer documentation in the browser.
pub fn open_developer_docs(parent: Option<&adw::ApplicationWindow>) {
    open_uri(parent, DEVELOPER_DOCS_URL);
}

async fn install_cli(parent: Option<&adw::ApplicationWindow>) {
    let result = gio::spawn_blocking(|| {
        let env = Environment::process().ok_or_else(no_home)?;
        let path = env.vars.get("PATH").cloned();
        Ok::<_, InstallError>(cli::apply(&cli::plan(&env), path.as_deref()))
    })
    .await;
    match flatten(result) {
        Ok(result) => {
            log_result(result.is_warning(), "command line tool", &result);
            present(parent, result.title(), &result.text(), None).await;
        }
        Err(err) => failed(parent, &err).await,
    }
}

async fn install_skill(parent: Option<&adw::ApplicationWindow>) {
    let report = gio::spawn_blocking(|| {
        let env = Environment::process().ok_or_else(no_home)?;
        skill::plan(&env).map(|plan| skill::apply(&plan))
    })
    .await;
    match flatten(report) {
        Ok(report) => {
            log_result(report.is_warning(), "agent skill", &report);
            present(parent, report.title(), &report.text(), None).await;
        }
        Err(err) => failed(parent, &err).await,
    }
}

async fn install_hooks(parent: Option<&adw::ApplicationWindow>) {
    if HOOKS_RUNNING.replace(true) {
        return;
    }
    let outcome = hooks_outcome(parent).await;
    HOOKS_RUNNING.set(false);
    match outcome {
        Ok(outcome) => {
            log_result(outcome.is_warning(), "agent status hooks", &outcome);
            let docs = outcome
                .codex
                .needs_manual_merge()
                .then_some(hooks::CODEX_MANUAL_DOCS_URL);
            present(parent, outcome.title(), &outcome.text(), docs).await;
        }
        Err(err) => failed(parent, &err).await,
    }
}

/// Probes `OpenCode` in the login shell, asks for its version when the probe could not tell, then plans and applies.
async fn hooks_outcome(parent: Option<&adw::ApplicationWindow>) -> Result<hooks::HooksOutcome, InstallError> {
    let env = Environment::process().ok_or_else(no_home)?;
    let probing = env.clone();
    let probe = gio::spawn_blocking(move || opencode::detect(&probing.vars, &probing.home_str()))
        .await
        .ok()
        .flatten();
    let start = match hooks::opencode_start(&env.home, probe.as_ref()) {
        OpenCodeStart::Ask => match choose_opencode_version(parent).await {
            Some(version) => OpenCodeStart::Install(version),
            None => OpenCodeStart::Decided(hooks::OpenCodeResult::Skipped),
        },
        start => start,
    };
    let applied =
        gio::spawn_blocking(move || hooks::plan(&env, &start, probe.as_ref()).and_then(|plan| hooks::apply(&plan)))
            .await;
    flatten(applied)
}

/// The `OpenCode` version chooser; `None` when the user skips `OpenCode`.
async fn choose_opencode_version(parent: Option<&adw::ApplicationWindow>) -> Option<Version> {
    let response = opencode_chooser().choose_future(parent).await;
    Version::ALL.into_iter().find(|version| response == version.raw())
}

/// Upstream's `makeOpenCodeVersionAlert`: Skip `OpenCode`, then one response per supported version.
fn opencode_chooser() -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(hooks::OPENCODE_CHOOSER_TITLE), Some(hooks::OPENCODE_CHOOSER_TEXT));
    dialog.add_response("skip", "Skip OpenCode");
    for version in Version::ALL {
        dialog.add_response(version.raw(), &format!("OpenCode {}", version.raw()));
    }
    dialog.set_close_response("skip");
    dialog
}

/// A result alert: OK, plus Open Docs second when `docs` is set, so OK stays the default.
fn result_dialog(title: &str, text: &str, docs: Option<&str>) -> adw::AlertDialog {
    let dialog = adw::AlertDialog::new(Some(title), Some(text));
    dialog.add_response("ok", "OK");
    if docs.is_some() {
        dialog.add_response("docs", "Open Docs");
    }
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog
}

async fn present(parent: Option<&adw::ApplicationWindow>, title: &str, text: &str, docs: Option<&str>) {
    let response = result_dialog(title, text, docs).choose_future(parent).await;
    if let Some(docs) = docs.filter(|_| response == "docs") {
        open_uri(parent, docs);
    }
}

async fn failed(parent: Option<&adw::ApplicationWindow>, err: &InstallError) {
    tracing::warn!(error = %err.0, "install failed");
    present(parent, "Install Failed", &err.0, None).await;
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

/// A blocking job's result, a panic in it reported as a failure.
fn flatten<T>(result: Result<Result<T, InstallError>, Box<dyn std::any::Any + Send>>) -> Result<T, InstallError> {
    result.unwrap_or_else(|_| Err(InstallError("The install stopped unexpectedly.".to_owned())))
}

fn open_uri(parent: Option<&adw::ApplicationWindow>, uri: &str) {
    gtk::UriLauncher::new(uri).launch(parent, None::<&gio::Cancellable>, |result| {
        if let Err(err) = result {
            tracing::warn!(%err, "could not open the link");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gtk_test::on_gtk_thread;

    #[test]
    fn the_docs_button_is_second_so_ok_stays_the_default() {
        on_gtk_thread(|| {
            let dialog = result_dialog("t", "x", Some(hooks::CODEX_MANUAL_DOCS_URL));
            assert_eq!(dialog.response_label("ok"), "OK");
            assert_eq!(dialog.response_label("docs"), "Open Docs");
            assert_eq!(dialog.default_response().as_deref(), Some("ok"));
            assert_eq!(dialog.close_response(), "ok");
            let plain = result_dialog("t", "x", None);
            assert!(!plain.has_response("docs"));
        });
    }

    #[test]
    fn the_opencode_chooser_offers_skip_and_each_version() {
        on_gtk_thread(|| {
            let dialog = opencode_chooser();
            assert_eq!(dialog.response_label("skip"), "Skip OpenCode");
            assert_eq!(dialog.response_label("v1"), "OpenCode v1");
            assert_eq!(dialog.response_label("v2"), "OpenCode v2");
            assert_eq!(dialog.close_response(), "skip");
            assert!(dialog.body().contains("opencode --version"));
        });
    }
}
