//! Where this crate's code comes from: the Ghostty revision it follows and the rustty commit it was taken from.
//!
//! [`LIBGHOSTTY_VT_REPO`], [`LIBGHOSTTY_VT_REV`], [`RUSTTY_REPO`] and [`RUSTTY_REV`] equal their lines in
//! `build/pins.env` (a test holds them together). [`RUSTTY_GHOSTTY_REV`] and [`RUSTTY_UNICODE_GHOSTTY_REV`] are
//! facts about rustty at [`RUSTTY_REV`]: the Ghostty revisions its port and its dumped Unicode tables follow, the
//! start of the drift this crate absorbs up to [`LIBGHOSTTY_VT_REV`].
//!
//! ```
//! use agterm_vt::provenance::{LIBGHOSTTY_VT_REV, RUSTTY_GHOSTTY_REV, RUSTTY_REPO, RUSTTY_REV};
//!
//! for rev in [LIBGHOSTTY_VT_REV, RUSTTY_REV, RUSTTY_GHOSTTY_REV] {
//!     assert_eq!(rev.len(), 40);
//!     assert!(rev.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
//! }
//! assert_eq!(RUSTTY_REPO, "https://github.com/AirelyDev/rustty");
//! ```

/// Ghostty's repository, whose terminal this crate is.
pub const LIBGHOSTTY_VT_REPO: &str = "https://github.com/ghostty-org/ghostty";

/// The Ghostty revision this crate follows: every module cites its Ghostty file at this revision. The one Ghostty
/// pin of the build (`build/pins.env`), shared with the resources and the config keys.
pub const LIBGHOSTTY_VT_REV: &str = "81598385d608dda4946093d6d6a234d204a2f225";

/// rustty's repository, the Zig-to-Rust port of Ghostty the code was taken from (MIT).
pub const RUSTTY_REPO: &str = "https://github.com/AirelyDev/rustty";

/// The rustty commit the code was taken from (2026-10-12): every module cites its rustty path at this commit.
pub const RUSTTY_REV: &str = "b57bcee7953450d79ed5b9dab321c2afd226e29c";

/// The Ghostty revision rustty's port follows at [`RUSTTY_REV`] (2026-10-09), its "oracle".
pub const RUSTTY_GHOSTTY_REV: &str = "246f702876b924a1cb7cade1e99274d1470302fc";

/// The Ghostty revision rustty's Unicode tables were dumped at (2026-09-28), with uucode 0.2.0.
pub const RUSTTY_UNICODE_GHOSTTY_REV: &str = "12752b2ac1bb05ce53402ed8c853ed1f96eef0b1";
