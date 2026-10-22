//! `config.reload` (upstream's `ControlServer+AppCommands.reloadGhosttyConfig` → `AppActions.reloadGhosttyConfig` →
//! `SettingsModel.reloadGhosttyConfig`): every session's font size cleared first, in every window, closed ones' files
//! included, then the ghostty config rebuilt from its files and fed to every terminal by the frontend, which reports
//! the rebuilt config's diagnostic count back. The answer is that count, across all config sources (libghostty's
//! diagnostics carry no file), so the command waits for the frontend's next render. App-global: no target or window.
//!
//! File ▸ Reload Config, the palette row and the ghostty.conf editor's exit take the same path
//! ([`Core::request_config_reload`]); an explicit reload with diagnostics posts the warning banner.

use std::time::Duration;

use ngterm_proto::ControlResult;

use crate::Core;
use crate::dispatch::Dispatched;

/// What the explicit reloads and the frontend's reports leave between them.
#[derive(Debug, Default)]
pub(crate) struct ConfigReloads {
    /// An explicit reload is waiting for the frontend: its diagnostics post the banner.
    explicit: bool,
    /// The diagnostic count of the frontend's last reload; `None` until one ran since the last request.
    count: Option<usize>,
}

impl Core {
    /// Reload Config (upstream's `reloadConfigClearingSessionZoom`): every session's font size cleared, then the config
    /// reloaded into every terminal by the frontend at its next render. Commit afterwards.
    pub(crate) fn request_config_reload(&mut self) {
        self.reset_session_font_sizes();
        self.themes.reload_requested = true;
        self.config_reloads.explicit = true;
        self.config_reloads.count = None;
    }

    /// The frontend reloaded the config [`Core::take_config_reload`] asked for: `count` is the rebuilt config's
    /// diagnostic count, `None` when it could not be built. An explicit reload with diagnostics posts the warning
    /// banner (upstream's `notifyConfigDiagnostics`); a settings change's reload posts none.
    pub fn report_config_reload(&mut self, count: Option<usize>) {
        let count = count.unwrap_or(0);
        self.config_reloads.count = Some(count);
        if std::mem::take(&mut self.config_reloads.explicit) && count > 0 {
            let body = format!(
                "{count} issue{} in ghostty config — see ngterm.log, then Reload Config",
                if count == 1 { "" } else { "s" }
            );
            self.post_app_banner("config-diagnostics", "Config", body);
        }
    }
}

/// `config.reload`: asks for the reload, then answers with the count the frontend reports after rendering it; 0 when
/// no frontend reloaded anything (upstream's count before the settings model is wired).
pub(crate) fn begin_reload(core: &mut Core) -> Dispatched {
    core.request_config_reload();
    Dispatched::Pending {
        after: Duration::ZERO,
        resume: Box::new(|core: &mut Core| {
            let count = core.config_reloads.count.unwrap_or(0);
            Dispatched::done(Ok(ControlResult::count(i64::try_from(count).unwrap_or(i64::MAX))))
        }),
    }
}
