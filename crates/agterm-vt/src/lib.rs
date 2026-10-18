//! Ghostty's terminal in Rust: the VT core behind agterm's panes (`agterm-term`) and its live-session server.
//!
//! # Provenance
//!
//! The code is Ghostty's terminal (`src/terminal`, `src/input`, `src/terminfo`, `src/unicode` and the support
//! modules they use) as rustty ported it from Zig to Rust, taken as code, not as a dependency: copied from rustty at
//! [`provenance::RUSTTY_REV`], trimmed to what agterm uses, reworked until it passes the workspace lints, and brought
//! to the Ghostty revision agterm follows, [`provenance::LIBGHOSTTY_VT_REV`] (`build/pins.env`). rustty followed
//! Ghostty at [`provenance::RUSTTY_GHOSTTY_REV`] (its Unicode tables at
//! [`provenance::RUSTTY_UNICODE_GHOSTTY_REV`]); every change Ghostty made between that revision and the pin, in a
//! directory a module here follows, is absorbed. Both projects are MIT; their notices are in the repository's
//! `NOTICE`. From the copy on, the code is agterm's: a Ghostty pin move is a change of its own, and a later rustty
//! commit is a hint, not an update.
//!
//! What is kept is what libghostty-vt has at the pin and agterm reaches. rustty's snapshot codec, tmux viewer, APC
//! glyph protocol, idle page compression, `StringMap` (Oniguruma), DECRQCRA checksums, C ABI, font, inspector and
//! event loop are not here, and the branches that reached them are gone from the kept modules. Identity is
//! Ghostty's, not rustty's: the terminfo entry `xterm-ghostty`, its XTGETTCAP answers, the device attributes and
//! the version behind XTVERSION are what libghostty-vt reports at the pin.
//!
//! # Module headers
//!
//! Every ported module opens its doc with what it is, then two provenance lines and the changes made on the way:
//!
//! ```text
//! //! Ghostty: `src/terminal/PageList.zig` at `LIBGHOSTTY_VT_REV`.
//! //! rustty: `crates/rustty-vt/src/pagelist/mod.rs` at `RUSTTY_REV` (MIT).
//! //! Changes: idle compression cut; drift absorbed: <Ghostty commits>.
//! ```
//!
//! A module of agterm's own (no Ghostty counterpart) says so instead. A Ghostty diff therefore maps onto the modules
//! that name its files, which mirror Ghostty's layout (`terminal/page_list`, `terminal/screen`, `input/key_encode`,
//! ...); rustty's leaf crates (util, datastruct, unicode, simd, os, lib, zigstd, render-types, identity, terminfo)
//! fold in as private support modules, cut to the items in use.
//!
//! # Unsafe
//!
//! Ghostty's page layout is kept, because it is what makes its scrollback, resize and reflow behave and perform as
//! they do: a page is one buffer, and its structures (the bitmap allocator, the ref-counted sets, the page-local hash
//! maps) live inside it, addressed by offsets. Here that buffer is a byte slice and every value stored in it is
//! `bytemuck::Pod`, read and written through bounds- and alignment-checked casts, so those structures, and pages,
//! the page list, screens and the terminal above them, are safe Rust: a wrong offset is a wrong value, never undefined
//! behavior. The `unsafe` left is the memory core's small audited part: the per-OS page allocator that maps and
//! unmaps page memory, one fallible box allocation, and the unit tests' allocator. Every `unsafe` block carries its
//! own `// SAFETY:` comment and does one operation, and the memory core's tests run under Miri.
//!
//! # Platforms
//!
//! Linux, macOS and Windows alike: the crate is pure Rust and holds its per-OS forms (page allocation, the Kitty
//! image media) side by side, built and tested on all three.

pub mod provenance;

// The support modules the terminal is built on. Until the terminal modules that use them are ported, only their own
// tests do, so outside tests they are dead code; each `expect` goes when its module gains a user.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the terminal modules, not ported yet")
)]
mod build_options;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the terminal modules, not ported yet")
)]
mod datastruct;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the terminal modules, not ported yet")
)]
mod mem;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the terminal modules, not ported yet")
)]
mod options;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "used by the terminal modules, not ported yet")
)]
mod os;
