//! Wire types of the ngterm control protocol: one newline-delimited JSON request per connection
//! (`{cmd, target?, args?}`) answered by one response (`{ok, result?, error?}`).

pub mod command;
pub mod events;
pub mod keymap;
pub mod request;
pub mod response;
pub mod result;
pub mod tree;
pub mod version;

pub use command::{Command, Group, UnknownCommand};
pub use events::{ControlEvent, EventBatch, EventKind, EventPayload};
pub use keymap::{
    ControlHookEntry, ControlHooks, ControlKeymap, ControlKeymapAction, ControlKeymapCommand, ControlKeymapDiagnostic,
    ControlKeymapMenuItem,
};
pub use request::{Args, AskButton, MAX_REQUEST_LINE_BYTES, PickItem, Request};
pub use response::Response;
pub use result::{
    ControlAskOutcome, ControlAskResult, ControlCursor, ControlHtmlPageOutcome, ControlHtmlPageOutcomeState,
    ControlLiveEndpoint, ControlLiveEntry, ControlLiveInventory, ControlLiveResetStatus, ControlPickOutcome,
    ControlPickResult, ControlRemotePane, ControlRemoteSession, ControlRemoteTree, ControlRestartReceipt,
    ControlRestoreStatus, ControlResult, ControlWindowFrame, ControlWindowNode,
};
pub use tree::{ControlSessionNode, ControlSurfaceNode, ControlTree, ControlWorkspaceNode};
pub use version::{AppIdentity, ConformanceProfile, PARITY_LEVEL, VersionResult};
