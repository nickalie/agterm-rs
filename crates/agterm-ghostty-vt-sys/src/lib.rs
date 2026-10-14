//! Raw bindings to the parts of libghostty-vt's official C API (`ghostty/vt.h`) that `agterm-ghostty-vt` binds.
//!
//! The library is the prebuilt static `libghostty-vt.a` in the vendored prefix that `scripts/build-ghostty-vt.sh`
//! fills at `LIBGHOSTTY_VT_REV`; cargo never runs zig. It is linked statically, so a binary needs nothing from the
//! prefix at run time. The build script exports the prefix to dependents as `DEP_GHOSTTY_VT_ROOT`. The bindings are
//! checked in: regenerate them with `scripts/gen-ghostty-vt-bindings.sh` after a pin bump.

include!("bindings.rs");
