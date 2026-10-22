//! ngterm's frontend-neutral app services: the host pieces every frontend shares that need no window or toolkit, so
//! a frontend keeps only its widget glue over them.
//!
//! - [`hook_runner`]: the [`ngterm_host::ProcessLauncher`] for hooks and custom commands.
//! - [`process_table`]: this OS's [`ngterm_host::ProcessTable`], `ngterm-session`'s.
//! - [`restore_launch`]: the restore decision a launch takes before its first terminal.
//! - [`logging`]: `tracing` to stderr and to the state directory's log file.
//! - [`quick_placement`]: the quick terminal's host and placement decisions.
//! - [`integrations`]: the agent integrations' planners and installers.
//! - [`remote`]: the presentation bridge to an attached row's origin as a child process.

pub mod hook_runner;
pub mod integrations;
pub mod logging;
pub mod process_table;
pub mod quick_placement;
pub mod remote;
pub mod restore_launch;
