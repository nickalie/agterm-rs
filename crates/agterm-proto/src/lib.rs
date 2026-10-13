//! Wire types of the agterm control protocol: one newline-delimited JSON request per connection
//! (`{cmd, target?, args?}`) answered by one response (`{ok, result?, error?}`).

pub mod command;
pub mod events;
pub mod request;
pub mod response;
pub mod tree;
pub mod version;

pub use command::{Command, Group, UnknownCommand};
pub use events::EventKind;
pub use request::{Args, AskButton, MAX_REQUEST_LINE_BYTES, PickItem, Request};
pub use response::Response;
pub use tree::{ControlSessionNode, ControlSurfaceNode, ControlTree, ControlWorkspaceNode};
pub use version::{AppIdentity, ConformanceProfile, PARITY_LEVEL, VersionResult};
