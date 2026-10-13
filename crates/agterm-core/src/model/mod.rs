//! The app model, ported from upstream's `agtermCore`: a [`WindowLibrary`] of windows, each open one an
//! [`AppStore`] holding [`Workspace`]s of [`Session`]s, each with a primary pane and an optional split pane.
//!
//! The model is plain data on the main thread. A mutation records the events and snapshot writes it implies in its
//! store's [`Outbox`]; the library drains them for the event ring and persistence.

mod background;
mod focus;
pub mod naming;
mod navigation;
mod outbox;
pub mod pane;
mod recency;
mod session;
mod snapshot;
mod store;
mod window_library;
mod workspace;

#[cfg(test)]
mod tests;

pub use background::{BackgroundWatermark, PaneBackgrounds, WatermarkFit, WatermarkKind, WatermarkPosition};
pub use focus::FocusMode;
pub use navigation::{NewSessionPlacement, SessionNavigation, WorkspaceNavigation};
pub use outbox::{EventPayload, ModelEvent, Outbox, SaveRequest};
pub use pane::{PaneLaunch, PaneRestore, SplitAxis, SplitLayout, StatusPane, SurfaceSlot};
pub use recency::{RECENCY_LIMIT, RecencyStack};
pub use session::{AgentIndicator, AgentStatus, CONTEXT_BYTE_LIMIT, Session, SessionSeed, validate_context};
pub use store::{
    AppStore, FocusFilter, NewSession, SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SessionLocation,
    SidebarState, WorkspaceOptions, WorkspaceStep, attention_order, clamp_sidebar_width,
};
pub use window_library::{LibraryEffects, WindowEvent, WindowInfo, WindowLibrary};
pub use workspace::{ReorderDirection, SidebarMode, Workspace};
