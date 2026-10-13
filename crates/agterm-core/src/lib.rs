//! agterm's platform-neutral core: the state owner and command dispatcher ([`Core`]), the window, workspace and
//! session model ([`model`]), its `tree` read-back ([`projection`]), the app-run event ring ([`events`]), control
//! target resolution ([`resolve`]), control errors with upstream agterm's wire strings ([`ControlError`]), the sidebar's drag-and-drop index math
//! ([`sidebar_drop`]), state-dir and socket path resolution ([`paths`]), the environment of spawned shells
//! ([`pane_env`]), and what the core and the frontend exchange about on-screen windows ([`windows`]). The command
//! groups' handlers live in the private `commands` module.
//!
//! Work that needs a live terminal, widget, daemon or process goes through the `agterm-host` traits.

pub mod auto_follow;
pub mod clipboard_policy;
pub mod command_restore;
mod commands;
pub mod dashboard;
pub mod dispatch;
pub mod error;
pub mod events;
pub mod interrupt;
pub mod keystrokes;
pub mod link_policy;
pub mod model;
pub mod osc_background;
pub mod pane_env;
pub mod paths;
pub mod persist;
pub mod projection;
pub mod quick_metrics;
pub mod resolve;
pub mod restart_replay;
pub mod sidebar_drop;
pub mod watermark;
pub mod windows;
pub mod zoom;

pub use commands::PaneReport;
pub use commands::surface::POLL_INTERVAL;
pub use dispatch::{CommitTimers, Core, CoreConfig, CoreState, Dispatched, Resume, decode_request};
pub use error::{ControlError, Noun};
pub use windows::{WindowRequest, WindowSize, WindowState};
