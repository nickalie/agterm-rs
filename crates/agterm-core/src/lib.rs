//! agterm's platform-neutral core: the state owner and command dispatcher ([`Core`]), control errors with the Swift
//! fork's wire strings ([`ControlError`]), and state-dir and socket path resolution ([`paths`]).
//!
//! Work that needs a live terminal, widget, daemon or process goes through the `agterm-host` traits.

pub mod dispatch;
pub mod error;
pub mod paths;

pub use dispatch::{Core, CoreConfig, decode_request};
pub use error::{ControlError, Noun};
