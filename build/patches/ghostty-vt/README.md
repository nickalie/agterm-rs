# libghostty-vt patches

`scripts/build-ghostty-vt.sh` applies every `*.patch` here to the pinned Ghostty source (`LIBGHOSTTY_VT_REV` in
`build/pins.env`), in name order, with `patch -p1 --fuzz=0`; the patches and their sha256s key the prefix and land in
its stamp, so a changed patch prepares a new tree and rebuilds the library. Each is a unified diff against that
pristine tree (`a/` and `b/` labels), its header saying what it changes and where it comes from. A patch that changes a
header is followed by `scripts/gen-ghostty-vt-bindings.sh` (its `--check` holds).

Two kinds are carried. One exposes data the library already stores, as C API the app needs (0001). The other changes
the library's logic, for a fix or a parity behavior Rust cannot reach from outside (0002 to 0004): each of those is
recorded here, in the form of herdr's `vendor/libghostty-vt.patches.md`, with its reason, its upstream status and a
remove-when condition with its verification. A gap Rust can fill is filled in Rust, not here.

Remove a patch only when the pinned source has the behavior and the patch's verification still passes without it. A
pin bump rebases every patch onto the new revision (with no fuzz) and re-runs every verification below.

The verifications use the prepared tree: `tree="$(scripts/build-ghostty-vt.sh)/src"`, and the pinned zig
(`zig=` in `$tree/../share/agterm/ghostty-vt-STAMP`). Ghostty's own tests run there with a scratch cache, so the tree
stays as the build script reads it:
`(cd "$tree" && "$zig" build test-lib-vt --cache-dir "$(mktemp -d)" -Dtest-filter='<name>')`. On Linux add the
build's glibc target, `-Dtarget=x86_64-linux-gnu.2.39` (or `aarch64-…`): the native target links the distro's
`crt1.o`, whose `.sframe` relocations (`R_X86_64_PC64`, gcc 16) Zig 0.16's linker refuses. The test filters below
were run that way on the pin (`cursorAbsolute`: 38 of 38 pass, herdr's two among them).

## 0001 Kitty placement-id namespaces and relative placements' parent links

status: active

patch: `build/patches/ghostty-vt/0001-kitty-placement-namespace-and-parent.patch`

agterm issue: none; Task 34 of the GPUI frontend plan (Unicode placeholders and relative placements drawn as
Ghostty's renderer draws them)

upstream discussion: not opened

upstream pr: not opened

base: `LIBGHOSTTY_VT_REV` `81598385d608dda4946093d6d6a234d204a2f225`

files:

- `include/ghostty/vt/kitty_graphics.h`
- `src/terminal/c/kitty_graphics.zig`
- `src/terminal/c/types.zig`

reason: The C API reports a placement's id without its namespace and a relative placement (`P=`/`Q=`) without its
parent, so an embedder cannot resolve a placeholder's target or a chain rooted at a virtual placement the way
Ghostty's renderer does (`placeholderTarget`, `pending_relative`). The patch reads out what storage holds, additively,
with no logic of its own; `agterm-term` and `agterm-gpui` resolve chains, targets and positions in Rust.

remove when: the pinned C API reports the placement key's namespace and a relative placement's parent key and offsets,
and the wrapper reads them from there.

verification:

```sh
cargo test -p agterm-ghostty-vt kitty
(cd "$tree" && "$zig" build test-lib-vt --cache-dir "$(mktemp -d)" -Dtest-filter='placement_get')
scripts/gen-ghostty-vt-bindings.sh --check
```

## 0002 hosted Wuffs builds

status: active

patch: `build/patches/ghostty-vt/0002-hosted-wuffs-builds.patch`

origin: herdr's `vendor/patches/libghostty-vt/0004-fix-hosted-wuffs-builds.patch` (`HERDR_REV` 143bf5f,
Apache-2.0), modified: rebased onto the pin, whose `pkg/translate-c` wrapper takes `libc_file` as a union
(`.direct` or `.detect_darwin`), so the option forwards the `--libc` file as `.direct` and stays `.detect_darwin`
without one

herdr issue: none; it keeps Windows cross-compilation and non-SIMD hosted builds working

upstream discussion: not opened

upstream pr: not opened

base: `LIBGHOSTTY_VT_REV` `81598385d608dda4946093d6d6a234d204a2f225` (herdr's: `44f2a44d`); ghostty main at
`e3ebb1d1` (2026-10-16) is still unpatched

files:

- `pkg/wuffs/build.zig`
- `pkg/wuffs/src/main.zig`

reason: Wuffs needs MSVC libc headers when it targets Windows. Zig's `--libc` file reaches the C compiles, but the
translate-c step needs its own explicit option, so a build for `windows-msvc` with an SDK named by `--libc` fails
without it (Task 10's Windows build passes one when the host has no detectable SDK). Separately, the no-libc Wuffs
module exports hidden weak `calloc`/`free` stubs; on a hosted target built with SIMD off (libc then unlinked), the
stubs in the static archive override the embedding executable's libc allocator, and every zeroed allocation fails.
The stubs are now exported for freestanding targets only. Native builds without a `--libc` file and the SIMD builds
agterm makes are unchanged.

The build `agterm-ghostty-vt-sys` makes on Linux cannot observe this patch: it has SIMD on, which links libc
(`src/build/GhosttyZig.zig`), so the unpatched source exports no stubs either, and it passes no `--libc` file. Neither
the wrapper's tests nor an `nm` of that archive fail without the patch. Its checks build what it changes: a
`windows-msvc` build that names its SDK with a `--libc` file (the translator half), and a hosted build of the prepared
tree with SIMD off (the stubs half). The second was run on the pin: the patched tree's archive defines neither
symbol, and with the `main.zig` hunk reverted it defines both as weak (`W calloc`, `W free`).

remove when: the pinned source forwards the build's libc file to the Wuffs translator and exports no allocator stubs on
hosted targets, and the checks below pass without this patch.

verification:

```sh
# the translator half: a windows-msvc build with --libc naming the MSVC SDK (Task 10's Windows build, where it
# passes one) succeeds
# the stubs half: a hosted build with SIMD off defines no calloc or free (only undefined references)
out="$(mktemp -d)" version="$(sed -n 's/^version-string=//p' "$tree/../share/agterm/ghostty-vt-STAMP")"
(cd "$tree" && "$zig" build -Demit-lib-vt=true -Doptimize=ReleaseFast -Dsimd=false -Dcpu=baseline \
  -Dtarget=x86_64-linux-gnu.2.39 -Dversion-string="$version" --prefix "$out/prefix" --cache-dir "$out/cache")
nm "$out/prefix/lib/libghostty-vt.a" | grep -E ' [TtWwVv] (calloc|free)$'   # prints nothing
```

The wrapper's `cargo test -p agterm-ghostty-vt --test patches wuffs` checks only that the Wuffs build the patch
touches still swizzles in agterm's build; it passes without the patch.

## 0003 keep the cursor hyperlink across page replacement

status: active

patch: `build/patches/ghostty-vt/0003-cursor-hyperlink-page-replace.patch`

origin: herdr's `vendor/patches/libghostty-vt/0008-cursor-hyperlink-page-replace.patch` (`HERDR_REV` 143bf5f,
Apache-2.0), modified: rebased onto the pin (paths and line numbers; the code and its two Screen tests are herdr's)

herdr issue: https://github.com/herdrdev/herdr/issues/5296

upstream discussion: not opened

upstream pr: not opened

base: `LIBGHOSTTY_VT_REV` `81598385d608dda4946093d6d6a234d204a2f225` (herdr's: `44f2a44d`); ghostty main at
`e3ebb1d1` (2026-10-16) is still unpatched (`cursorChangePin`)

files:

- `src/terminal/Screen.zig`

reason: A crash. Moving the cursor onto another page while an OSC 8 link and a non-default style are open can grow
that page during style migration. The growth replaces the page, re-adds the cursor's heap link and frees it; the move
then clears the pointer without clearing the id the replacement installed, so the link's end dereferences null and the
process dies. In the session server one libghostty-vt crash ends every live session, so the fix is carried with the
server's VT and the app's alike. The same function also crashes when style migration hits the maximum style capacity:
the page split moves the cursor again, and the nested pin change releases link id 0 from a page that no longer holds
it; the patch skips that release.

remove when: the pinned source keeps the cursor hyperlink across a page replacement or page split during a pin
change, and the checks below pass without this patch.

verification:

```sh
cargo test -p agterm-ghostty-vt --test patches hyperlink   # SIGSEGV without the patch
(cd "$tree" && "$zig" build test-lib-vt --cache-dir "$(mktemp -d)" \
  -Dtest-filter='cursorAbsolute across a full page keeps the hyperlink')
(cd "$tree" && "$zig" build test-lib-vt --cache-dir "$(mktemp -d)" \
  -Dtest-filter='cursorAbsolute keeps the hyperlink when style migration splits the page')
```

## 0004 the formatter writes skipped cells unstyled

status: active

patch: `build/patches/ghostty-vt/0004-formatter-unstyled-blanks.patch`

origin: upstream agterm v0.36.0's `scripts/zmx-patches/ghostty/0001-formatter-unstyled-blanks.patch` (MIT), the patch
upstream applies to the Ghostty its zmx links; rebased onto the pin (line numbers; the code is upstream's). zmx's copy
(`build/patches/zmx/ghostty/`) stays until zmx goes.

agterm issue: none; parity with upstream's re-attach snapshot

upstream discussion: not opened

upstream pr: not opened

base: `LIBGHOSTTY_VT_REV` `81598385d608dda4946093d6d6a234d204a2f225`; ghostty main at `e3ebb1d1` (2026-10-16) is still
unpatched

files:

- `src/terminal/formatter.zig`

reason: The formatter wrote cells a program skipped with a cursor move as spaces under the previous cell's style, so in
a re-attach's snapshot a gap after a colored run came back painted. A cell with no text now closes the open style
before it is buffered as a blank; cells the program wrote, spaces included, keep their style, and plain output is
unchanged. No formatter option controls this at the pin, and the session server's snapshot is the formatter's VT
output, so Rust cannot fix it from outside. Every styled formatter output takes it, agterm's own `copy_to_clipboard`
and `write_*_file` in VT or HTML too: their gap now reads as the screen shows it.

remove when: the pinned formatter writes skipped cells unstyled (or an option asks for it and the snapshot sets it),
and the checks below pass without this patch.

verification:

```sh
cargo test -p agterm-ghostty-vt --test patches skipped_cells
```

## herdr patches not carried

Reviewed against the pin and against what agterm uses (herdr's `vendor/libghostty-vt.patches.md` at `HERDR_REV`):

- **0002 expose modifyOtherKeys mode through terminal data**: herdr runs inside another terminal and must ask that
  outer terminal for printable key releases while an inner program has modifyOtherKeys 2 on. agterm is the outermost
  terminal: its key encoder reads the mode from the terminal itself (`ghostty_key_encoder_setopt_from_terminal`), so
  nothing needs the scalar.
- **0005 bounded word selection for wrapped link activation**: herdr's Ctrl+click resolves a wrapped token beyond the
  viewport through a new selection API. agterm finds links in Rust with Ghostty's own matcher over the frame's rows
  (`agterm-gpui` `terminal/links.rs`), as Ghostty does, and needs no library API for it.
- **0006 clear screen while preserving the cursor line**: a new C function for herdr's own "clear pane" feature.
  agterm's `clear_screen` follows Ghostty's `Termio.clearScreen` and is written in Rust as VT sequences at a ground
  point (`agterm-term` `actions.rs`).
- **0007 experimental encoded PNG and immutable source retention**: a default-off experiment for forwarding Kitty PNG
  uploads through a multiplexer without decoding. agterm decodes and draws images itself (the frontend's PNG decoder
  through `sys.h`), and the session server's VT runs with image media off (plan Task 16), so nothing would retain or
  forward the encoded bytes.
