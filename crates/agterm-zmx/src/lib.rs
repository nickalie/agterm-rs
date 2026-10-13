//! agterm's live-session host over the patched zmx (`scripts/build-zmx.sh`): the [`agterm_host::SessionHost`] the
//! frontend hands the core. The decisions (daemon names, the socket namespace, the wrap configuration, the list
//! parser, reap, prune and reset policies, the lead protocol) are host-free and live in [`agterm_core::zmx`]; this
//! crate reads the host and runs the binary.
//!
//! - [`launch`]: what a live launch reads of this host, and the wrap configuration of a pane over it;
//! - [`client`]: the bounded zmx process runner and [`ZmxClient`], the `SessionHost`;
//! - [`inventory`]: the client side of the lifecycle: the forced batch kill behind the finalizer and the launch reap;
//! - [`sweep`]: the process sweeper that hangs up a killed shell's foreground job.

pub mod client;
pub mod inventory;
pub mod launch;
pub mod sweep;

pub use client::ZmxClient;
pub use launch::Runtime;
