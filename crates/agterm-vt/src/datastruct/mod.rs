//! General data structures the terminal uses beside page memory.
//!
//! rustty's `rustty-datastruct` and `rustty-zigstd` crates (its ports of Zig's standard library), cut to what the
//! kept code reaches and re-based on `std` where its semantics allow; each module names the files it follows.

pub(crate) mod array_hash_map;
pub(crate) mod wyhash;
