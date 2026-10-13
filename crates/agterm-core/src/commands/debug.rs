//! `debug.appearance` (upstream's app-side `ControlServer.setDebugAppearance`): a UI-test-only seam that forces the
//! app's light/dark appearance so a UI test can drive the real flip path, refused outside a UI-test launch. A set
//! echoes the side it forced; the bare form reads the side the last config feed applied. Test scaffolding: no
//! `agtermctl` subcommand.

use agterm_proto::{Args, ControlResult, Request};

use super::{Outcome, args_of};
use crate::Core;
use crate::error::ControlError;
use crate::paths::Env;

/// The launch variables that mark a UI-test launch (upstream's `ContentView.isUITestLaunch`): an isolated state
/// directory and the UI-test sentinel.
const UI_TEST_VARIABLES: [&str; 2] = ["AGTERM_STATE_DIR", "AGTERM_UITEST_FORCE_SIDEBAR_VISIBLE"];

/// Whether `env` is a UI-test launch: both of [`UI_TEST_VARIABLES`] set.
#[must_use]
pub fn is_ui_test_launch(env: &dyn Env) -> bool {
    UI_TEST_VARIABLES.iter().all(|name| env.var(name).is_some())
}

/// The seam's state: whether it is open, the side last applied, and a forced side the frontend has not taken.
#[derive(Debug, Default)]
pub(crate) struct DebugAppearance {
    ui_test: bool,
    applied_dark: bool,
    requested: Option<bool>,
}

impl Core {
    /// Opens `debug.appearance` for a UI-test launch ([`is_ui_test_launch`]).
    pub fn set_ui_test_launch(&mut self, ui_test: bool) {
        self.debug_appearance.ui_test = ui_test;
    }

    /// The frontend fed the config on the dark side, or the light one (upstream's `lastAppliedIsDark`).
    pub fn report_applied_appearance(&mut self, dark: bool) {
        self.debug_appearance.applied_dark = dark;
    }

    /// Takes the side `debug.appearance` forced since the last take: the frontend forces the app's appearance to it
    /// and runs its appearance flip.
    pub fn take_debug_appearance(&mut self) -> Option<bool> {
        self.debug_appearance.requested.take()
    }
}

/// `debug.appearance [light|dark]` (the side rides `args.name`).
pub(crate) fn appearance(core: &mut Core, request: &Request) -> Outcome {
    if !core.debug_appearance.ui_test {
        return Err(ControlError::rejected("debug.appearance is a UI-test-only seam"));
    }
    let args: &Args = args_of(request);
    let Some(name) = args.name.as_deref() else {
        return Ok(ControlResult::text(side(core.debug_appearance.applied_dark)));
    };
    let dark = match name.trim() {
        "dark" => true,
        "light" => false,
        _ => return Err(ControlError::rejected("debug.appearance requires light|dark")),
    };
    core.debug_appearance.requested = Some(dark);
    Ok(ControlResult::text(side(dark)))
}

fn side(dark: bool) -> &'static str {
    if dark { "dark" } else { "light" }
}
