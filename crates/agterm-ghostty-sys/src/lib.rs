//! Raw bindings to the subset of libghostty's embedding API (`ghostty.h`) agterm uses.
//!
//! The library is the prebuilt, patched `libghostty.so` in the vendored prefix that `scripts/build-ghostty.sh`
//! fills; cargo never runs zig. The build script links it and exports the prefix to dependents as
//! `DEP_GHOSTTY_ROOT`, so their build scripts can emit rpaths (an rpath emitted here would reach only this crate's
//! own targets). The bindings are checked in: regenerate them with `scripts/gen-ghostty-bindings.sh`.

include!("bindings.rs");
