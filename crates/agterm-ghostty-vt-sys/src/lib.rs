//! Raw bindings to the parts of libghostty-vt's official C API (`ghostty/vt.h`) that `agterm-ghostty-vt` binds.
//!
//! The build script builds the static library with Zig for cargo's target (`zig build -Demit-lib-vt` into `OUT_DIR`)
//! from the patched source tree `scripts/build-ghostty-vt.sh` prepares at `LIBGHOSTTY_VT_REV` in the vendored prefix,
//! and never reaches the network. It is linked statically, so a binary needs nothing from the prefix at run time. The
//! bindings are checked in, with bindgen's compile-time layout assertions (a target whose C layout differs fails to
//! build): regenerate them with `scripts/gen-ghostty-vt-bindings.sh` after a pin bump or a patch change.

include!("bindings.rs");
