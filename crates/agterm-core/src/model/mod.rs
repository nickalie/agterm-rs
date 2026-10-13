//! The app model, ported from upstream's `agtermCore`: a [`WindowLibrary`] of windows, each open one an
//! [`AppStore`] holding [`Workspace`]s of [`Session`]s, each with a primary pane and an optional split pane.
//!
//! The model is plain data on the main thread. A mutation records the events and snapshot writes it implies in its
//! store's [`Outbox`]; the library drains them for the event ring and persistence.

mod ask;
mod background;
mod duplicate;
mod focus;
pub mod naming;
mod navigation;
mod outbox;
mod overlay;
pub mod pane;
mod pane_overlay;
mod panes;
mod pending_close;
mod recency;
mod recent_closed;
mod restore;
mod search;
mod session;
mod snapshot;
mod status;
mod store;
mod window_library;
mod workspace;

#[cfg(test)]
mod tests;

pub use ask::{AskAlignment, AskAnchor, AskNavigation, AskStyle, EndedAsk, MAX_ASK_BUTTONS, PendingAsk, SessionAsk};
pub use background::{BackgroundWatermark, PaneBackgrounds, WatermarkFit, WatermarkKind, WatermarkPosition};
pub use focus::FocusMode;
pub use navigation::{NewSessionPlacement, SessionNavigation, WorkspaceNavigation};
pub use outbox::{EventPayload, ModelEvent, Outbox, RecentClosedChange, SaveRequest, WatermarkRemoval};
pub use overlay::{HudOpening, HudState, OverlayPane, OverlayProgram, OverlaySlot};
pub use pane::{PaneLaunch, PaneRestore, SplitAxis, SplitLayout, StatusPane, SurfaceSlot};
pub use pane_overlay::{PaneOverlay, PaneOverlayOpenFailure, PaneOverlaySlot, PaneOverlays};
pub use panes::SwapRefusal;
pub use pending_close::{
    PENDING_CLOSE_GRACE, PendingCloseKind, PendingCloseMember, PendingCloseSummary, PendingCloseTimer,
};
pub use recency::{RECENCY_LIMIT, RecencyStack};
pub use recent_closed::RecentClosedRestore;
pub use search::{SearchFollowUp, SearchReport, SearchState};
pub use session::{
    AgentIndicator, AgentStatus, CONTEXT_BYTE_LIMIT, PaneAddress, Session, SessionSeed, validate_context,
};
pub use status::{
    STANDARD_SOUND_NAMES, StatusKeystroke, StatusReset, StatusShape, StatusWriteResult, is_known_sound,
    is_valid_color_hex,
};
pub use store::{
    AppStore, FocusFilter, NewSession, SIDEBAR_WIDTH_DEFAULT, SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SessionLocation,
    SidebarState, WorkspaceOptions, WorkspaceStep, attention_order, clamp_sidebar_width,
};
pub use window_library::{LibraryEffects, WindowEvent, WindowInfo, WindowLibrary};
pub use workspace::{ReorderDirection, SidebarMode, Workspace};
