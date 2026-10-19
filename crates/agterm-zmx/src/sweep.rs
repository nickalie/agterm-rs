//! The process sweeper (upstream's `ProcessSweeper` in `agterm/Ghostty/ProcessTable.swift`), injected into
//! [`crate::ZmxClient`], which has none by default: a test's fake listing names pids that are real processes on the
//! machine running it. It is [`agterm_session::job`]'s, which reads the process table and signals what is left of a
//! killed shell's foreground job on every OS; re-exported here until zmx goes.

pub use agterm_session::job::{ProcessEntry, Signal, Signaller, Sweeper, TableReader, read_table};
pub use agterm_session::procfs::parse_record;
