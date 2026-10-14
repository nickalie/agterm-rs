//! WPE `WebKit` host for the GPUI frontend's HTML overlays: pages rendered offscreen by WPE `WebKit` (the
//! `WPEPlatform` API) on an engine thread of their own, frames handed to the consumer as dmabufs on its GPU, or as SHM
//! pixels where dmabuf import fails.
//!
//! - [`Host::start`] spawns the engine thread with its own `GMainContext` (thread-default before any `WebKit` object
//!   exists) and a display that renders on the consumer's render node with the formats it imports ([`HostConfig`],
//!   [`FrameMode`]). One engine per process: `WebKit` binds its main run loop to the first thread that uses it, and
//!   that thread never ends (a shutdown closes everything and leaves it idle; the process then ends through
//!   [`exit_process`], since `WebKit`'s exit handlers must not run off its main thread).
//! - Commands go in through [`Host`]'s methods; everything else comes out as [`Event`]s on one channel: frames, load
//!   progress, what a page shows, and the questions `WebKit` asks (navigation and response policy, script messages,
//!   reads of the consumer's URI schemes), each held open until the consumer answers it by token.
//! - Frames follow `WebKit`'s buffer protocol (module `frames`): a frame counts as rendered once the consumer presented
//!   it, and a dmabuf goes back one frame later; acknowledgements run from a source of their own on the engine's
//!   context, never inside `WebKit`'s own hand-over. A page whose dmabufs the consumer cannot import falls back to
//!   SHM frames while it runs ([`Host::fall_back_to_shm`]).
//! - The `WebKit` surface the overlay uses: browser stores ([`SessionSpec`]: in memory, or on disk for saved logins;
//!   downloads refused), [`Settings`], user scripts and script message handlers, navigation and response policy, URI
//!   schemes, and what a page may not do (new windows, dialogs, file choosers, permissions, full screen) refused.
//! - [`launcher`]: running a program against the vendored WPE `WebKit`, whose helpers sit at a compiled-in path.
//!
//! Not here yet (Task 32 decides): IME, the clipboard (`WPEClipboard`), `<select>` popups (`show-option-menu`) and
//! context menus, accessibility.

mod config;
mod engine;
mod event;
mod frames;
mod host;
mod input;
pub mod launcher;
mod spec;

use agterm_wpe_javascriptcore_sys as jsc_sys;
use agterm_wpe_platform_sys as platform_sys;
use agterm_wpe_sys as webkit_sys;

pub use config::{
    BufferFormat, ConfigError, DmaBufTarget, FrameMode, HostConfig, MODIFIER_INVALID, MODIFIER_LINEAR, fourcc,
};
pub use event::{
    Event, LoadError, LoadErrorKind, LoadEvent, NavigationType, PageState, PolicyKind, PolicyRequest, PolicyToken,
    ReplyToken, SchemeError, SchemeRequest, SchemeResponse, SchemeToken, ScriptMessage, Termination,
};
pub use frames::{BufferId, DmaBuf, Frame, FrameContent, Plane, Rect, ShmRegion};
pub use host::{Evaluation, Host, Load, StartError, exit_process};
pub use input::{Button, Input, Key, Modifiers, Pointer, PointerKind, Scroll};
pub use spec::{
    Geometry, InjectionTime, MessageHandler, Rgba, SessionId, SessionSpec, Settings, UserScript, ViewId, ViewSpec,
};
