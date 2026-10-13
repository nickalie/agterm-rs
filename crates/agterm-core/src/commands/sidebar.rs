//! `sidebar` and `sidebar.*` (upstream's `ControlDispatcher.swift` sidebar arms for the argument checks, and
//! `ControlServer+AppCommands.swift` for the effects): visibility and view mode act on the frontmost window, the
//! flagged layout is an app-wide setting, and expand, collapse and width take an optional open `--window`.
//!
//! Expand and collapse persist the model first and then ask that window's outline to follow, so they hold with the
//! sidebar hidden too (where upstream's coordinator, mounted only while the sidebar shows, would drop them).

use agterm_proto::command::SidebarCmd;
use agterm_proto::{ControlResult, Request};

use super::{ToggleMode, args_of};
use crate::Core;
use crate::error::ControlError;
use crate::model::{AppStore, SidebarMode};
use crate::persist::settings::FlaggedViewLayout;
use crate::resolve::Resolver;

/// `sidebar.mode`'s argument (upstream's `ControlSidebarViewMode`): `toggle` when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Tree,
    Flagged,
    Toggle,
}

impl ViewMode {
    fn parse(mode: Option<&str>) -> Option<Self> {
        match mode.unwrap_or("toggle") {
            "tree" => Some(Self::Tree),
            "flagged" => Some(Self::Flagged),
            "toggle" => Some(Self::Toggle),
            _ => None,
        }
    }
}

/// `sidebar.flagged-layout`'s argument (upstream's `ControlFlaggedLayoutMode`): `toggle` when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Set(FlaggedViewLayout),
    Toggle,
}

impl LayoutMode {
    fn parse(mode: Option<&str>) -> Option<Self> {
        match mode.unwrap_or("toggle") {
            "toggle" => Some(Self::Toggle),
            raw => FlaggedViewLayout::from_raw(raw).map(Self::Set),
        }
    }

    /// The layout asked for given the `current` one: a toggle goes to `tree` from either flat form and back to
    /// `flat`.
    fn resolve(self, current: FlaggedViewLayout) -> FlaggedViewLayout {
        match self {
            Self::Set(layout) => layout,
            Self::Toggle if current == FlaggedViewLayout::Tree => FlaggedViewLayout::Flat,
            Self::Toggle => FlaggedViewLayout::Tree,
        }
    }
}

/// Answers the `sidebar` group; `None` is a bare `ok`.
pub(crate) fn dispatch(
    core: &mut Core,
    cmd: SidebarCmd,
    request: &Request,
) -> Result<Option<ControlResult>, ControlError> {
    let args = args_of(request);
    let mode = args.mode.as_deref();
    let window = args.window.as_deref();
    match cmd {
        SidebarCmd::Visibility => {
            let mode = ToggleMode::parse_with(mode, "show", "hide").ok_or_else(|| invalid_sidebar_mode(mode))?;
            let store = frontmost(core)?;
            let visible = mode.desired_value(store.sidebar().visible);
            store.set_sidebar_visible(visible);
            Ok(None)
        }
        SidebarCmd::Mode => {
            let mode = ViewMode::parse(mode).ok_or_else(|| invalid_sidebar_mode(mode))?;
            let store = frontmost(core)?;
            let flagged = match mode {
                ViewMode::Tree => false,
                ViewMode::Flagged => true,
                ViewMode::Toggle => store.sidebar().mode == SidebarMode::Tree,
            };
            let want = if flagged {
                SidebarMode::Flagged
            } else {
                SidebarMode::Tree
            };
            store.set_sidebar_mode(want);
            Ok(None)
        }
        SidebarCmd::FlaggedLayout => {
            let mode = LayoutMode::parse(mode).ok_or_else(|| {
                ControlError::rejected(format!("invalid flagged layout: {}", mode.unwrap_or("toggle")))
            })?;
            let current = core.settings().effective_flagged_view_layout();
            let want = mode.resolve(current);
            // upstream's `SettingsModel.setFlaggedViewLayout`: skips an unchanged effective layout, stores the
            // default `flat` as unset
            if want != current {
                let stored = (want != FlaggedViewLayout::Flat).then(|| want.raw().to_owned());
                core.update_settings(|settings| settings.flagged_view_layout = stored);
            }
            Ok(Some(ControlResult::text(want.raw())))
        }
        SidebarCmd::Expand => {
            let layout = core.settings().effective_flagged_view_layout();
            open_store(core, window)?.expand_all_workspaces(layout);
            Ok(None)
        }
        SidebarCmd::Collapse => {
            let layout = core.settings().effective_flagged_view_layout();
            open_store(core, window)?.collapse_other_workspaces(layout);
            Ok(None)
        }
        SidebarCmd::Width => {
            let points = args
                .sidebar_width
                .filter(|points| points.is_finite())
                .ok_or_else(|| ControlError::rejected("sidebar.width requires a width in points"))?;
            let store = open_store(core, window)?;
            store.set_sidebar_width(points);
            Ok(Some(ControlResult::sidebar_width(store.sidebar().width)))
        }
    }
}

/// `invalid sidebar mode: <mode>`, shared by `sidebar` and `sidebar.mode` (upstream spells both alike).
fn invalid_sidebar_mode(mode: Option<&str>) -> ControlError {
    ControlError::rejected(format!("invalid sidebar mode: {}", mode.unwrap_or("toggle")))
}

/// The frontmost window's store; `no open window` with every window closed (upstream's `library.activeStore`).
fn frontmost(core: &mut Core) -> Result<&mut AppStore, ControlError> {
    core.library_mut().active_store_mut().ok_or(ControlError::NoOpenWindow)
}

/// The `--window` store, else the frontmost, which must be open (upstream's `resolveOpenPlacementStore`).
fn open_store<'a>(core: &'a mut Core, window: Option<&str>) -> Result<&'a mut AppStore, ControlError> {
    let id = Resolver::new(core.library()).open_placement_window(window)?;
    core.library_mut().store_mut(id).ok_or(ControlError::NoOpenWindow)
}
