//! `browser.links` and `browser.clear` (upstream's `ControlDispatcher+Browser.swift` and the app's `linkOpenMode` /
//! `clearBrowser`): one saved store and one link-open setting serve every window, so a target or `--window` is
//! refused before anything runs. `browser.clear` empties the saved store through the frontend's web views and answers
//! once the web engine is done, polling as upstream awaits `removeData`.

use agterm_proto::command::BrowserCmd;
use agterm_proto::{Command, ControlResult, Request, Response};

use super::surface::POLL_INTERVAL;
use super::{Outcome, args_of};
use crate::Core;
use crate::dispatch::Dispatched;
use crate::error::ControlError;
use crate::html::{ClearProgress, HtmlHost, StoreClear, errors};
use crate::persist::settings::LinkOpenMode;

/// Refuses a target or `--window`: the browser commands are app-global.
fn refuse_scope(cmd: BrowserCmd, request: &Request) -> Result<(), ControlError> {
    if request.target.is_some() || args_of(request).window.is_some() {
        return Err(ControlError::rejected(format!(
            "{} takes no target or --window",
            Command::Browser(cmd).wire_name()
        )));
    }
    Ok(())
}

/// `browser.links [browser|overlay]`: sets where a clicked web link in a terminal opens, through the settings setter
/// (the default `browser` is stored as no value), and answers the effective mode; without a mode it only reads it.
pub(crate) fn links(core: &mut Core, request: &Request) -> Outcome {
    refuse_scope(BrowserCmd::Links, request)?;
    if let Some(raw) = args_of(request).mode.as_deref() {
        let mode =
            LinkOpenMode::from_raw(raw).ok_or_else(|| ControlError::rejected(format!("invalid link mode: {raw}")))?;
        core.update_settings(|settings| {
            if settings.effective_link_open_mode() != mode {
                settings.link_open_mode = (mode != LinkOpenMode::Browser).then(|| mode.raw().to_owned());
            }
        });
    }
    Ok(ControlResult::text(core.settings().effective_link_open_mode().raw()))
}

/// `browser.clear`: removes every cookie and all site data the saved store holds. Refused while a clear runs, while
/// persistent pages are open, and when the profile cannot be read; a store never created is left uncreated.
pub(crate) fn begin_clear(core: &mut Core, request: &Request) -> Dispatched {
    if let Err(error) = refuse_scope(BrowserCmd::Clear, request) {
        return Dispatched::done(Err(error));
    }
    let started = core.html_host().map_or_else(
        || Err(errors::PERSISTENT_UNAVAILABLE.to_owned()),
        HtmlHost::clear_saved_store,
    );
    match started {
        Err(failure) => Dispatched::done(Err(ControlError::rejected(format!("browser.clear: {failure}")))),
        Ok(StoreClear::Nothing) => Dispatched::Done(Response::success()),
        Ok(StoreClear::Started) => wait_cleared(),
    }
}

/// Answers once the web engine finished emptying the store, with its error when it could not.
fn wait_cleared() -> Dispatched {
    Dispatched::Pending {
        after: POLL_INTERVAL,
        resume: Box::new(|core: &mut Core| {
            match core
                .html_host()
                .map_or(ClearProgress::Done, HtmlHost::saved_store_clear)
            {
                ClearProgress::Running => wait_cleared(),
                ClearProgress::Done => Dispatched::Done(Response::success()),
                ClearProgress::Failed(failure) => {
                    Dispatched::done(Err(ControlError::rejected(format!("browser.clear: {failure}"))))
                }
            }
        }),
    }
}
