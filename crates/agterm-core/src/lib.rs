//! agterm's platform-neutral core: the state owner and command dispatcher ([`Core`]), the window, workspace and
//! session model ([`model`]), its `tree` read-back ([`projection`]), control target resolution ([`resolve`]), control
//! errors with upstream agterm's wire strings ([`ControlError`]), state-dir and socket path resolution ([`paths`]),
//! and the environment of spawned shells ([`pane_env`]).
//!
//! Work that needs a live terminal, widget, daemon or process goes through the `agterm-host` traits.

pub mod dispatch;
pub mod error;
pub mod model;
pub mod pane_env;
pub mod paths;
pub mod persist;
pub mod projection;
pub mod resolve;

pub use dispatch::{Core, CoreConfig, CoreState, decode_request};
pub use error::{ControlError, Noun};
