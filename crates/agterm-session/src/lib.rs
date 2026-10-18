//! agterm's live-session server, on herdr's architecture: one background server per agterm instance owns every live
//! pane's PTY and terminal state, and the app, `agtermctl` and other viewers are its clients.
//!
//! This crate holds, so far, the parts every other one stands on:
//!
//! - [`protocol`]: the wire, frames with a stable header and a stable JSON control layer every version reads, and the
//!   postcard messages of the api and attach roles;
//! - [`endpoint`]: where a namespace's server listens, a Unix domain socket or a Windows named pipe, owner-only;
//! - [`server`]: the server's lifecycle: started on demand by a client that finds none, detached from it per OS, one
//!   per namespace, stopped by itself once it holds no session and no client. The `agterm-session` binary runs it as
//!   `agterm-session server <namespace directory>`.
//!
//! # Provenance
//!
//! The code is taken from herdr (<https://github.com/herdrdev/herdr>, Apache-2.0) at the commit `build/pins.env` pins
//! as `HERDR_REV`, trimmed to what agterm uses and reworked. herdr's files carry no license header, so every derived
//! file names in its header the herdr file it comes from, that it is licensed under the Apache License 2.0, and that
//! it was modified; herdr's license is in the repository's `NOTICE`. Hence the crate's `MIT AND Apache-2.0`.

pub mod endpoint;
pub mod protocol;
pub mod server;
