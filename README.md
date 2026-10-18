# agterm-rs

agterm in Rust: a GPUI terminal over Ghostty's libghostty-vt, driven by `agtermctl` over a control socket. macOS,
Linux and Windows are equal targets; today it is built and tested on Linux, and everything below is the Linux build.

- The parity target is upstream macOS agterm ([umputun/agterm](https://github.com/umputun/agterm) v0.36.0): its wire
  protocol, command semantics, error strings, `agtermctl`, on-disk formats, model and UI, the UI in GPUI idioms where
  AppKit has no direct equivalent.
- The Linux build keeps the Swift/GTK fork's (`agterm-linux`) application id `io.github.melonamin.agterm`, state dir
  (`$XDG_DATA_HOME/agterm`, else `~/.local/share/agterm`) and control socket (`<state dir>/agterm.sock`), so it can
  replace the installed fork with the existing state. One instance runs per application id: the first owns the id's
  name on the session bus and serves `GApplication`'s D-Bus protocol there (through `zbus`), and a later launch,
  of this build or of the fork's GTK one, forwards its activation to it and exits before touching the state.
- One extension beyond upstream: Live sessions in bash and fish login shells too (upstream: zsh only), under the
  Rust-only capability `live-any-shell`.
- Parity is measured by the black-box suite in [conformance/](conformance/README.md), held by a ratchet.

## Prerequisites

- Rust 1.98.1 with clippy and rustfmt, pinned by `rust-toolchain.toml` (`rustup toolchain install` reads it).
- Zig 0.16.0 for libghostty-vt and zmx (`*_ZIG_VERSION` in `build/pins.env`). The build scripts take
  `AGTERM_GHOSTTY_VT_ZIG` / `AGTERM_ZMX_ZIG`, a zig 0.16.0 on `PATH` or from mise, else they download the pinned
  ziglang.org tarball and check its sha256.
- pkg-config, and the development files GPUI links: Wayland, xkbcommon, xcb, fontconfig and freetype.
- WPE WebKit 2.54+ with its WPEPlatform API for the HTML overlays (feature `html`, the default; see
  [WPE WebKit](#wpe-webkit)).
- For the native builds: curl, tar, xz, zstd, patch, sha256sum and ncurses' `tic`/`infocmp`; for
  `scripts/build-weston.sh` also meson, ninja, python3, a C++ compiler and weston's own development files (the Ubuntu
  list below); for regenerating the libghostty-vt bindings `bindgen` (`cargo install bindgen-cli`).
- A Vulkan loader and driver: GPUI renders through wgpu on Vulkan (Mesa's drivers, lavapipe for every headless app
  instance).
- For the app's tests: Xvfb's `xvfb-run` and dbus' `dbus-run-session`, so they get a display and a session bus of
  their own instead of the desktop's.
- For the app's process tests (`crates/agterm-gpui/tests`): dbus-daemon, a private bus per test; the ones with a
  display also need weston, Xvfb and Mesa's lavapipe (`mesa-vulkan-drivers`, an unpacked one under the vendor dir, or
  `AGTERM_LAVAPIPE_ICD`), the keyboard layout test `setxkbmap` (`x11-xkb-utils`). A missing one skips the test
  (`SKIPPED:`, shown with `--nocapture`) unless `AGTERM_REQUIRE_DISPLAY_TESTS` is set, as CI sets it, which fails it
  instead. The single-instance checks against a GTK build of agterm (the Linux fork) take its binary from
  `AGTERM_GTK_BIN` and skip without one, required or not. The nested-niri activation check (niri inside a headless
  weston) is `#[ignore]`d, since CI has no niri: `cargo test -p agterm-gpui --test instance -- --ignored`. The external texture
  test (`--test external_texture`, dmabufs drawn by GPUI's headless renderer) runs on lavapipe and on the first render
  node a Mesa Vulkan driver runs; its synthetic dmabufs need read-write access to `/dev/udmabuf` (a kernel built with
  `CONFIG_UDMABUF=y`, never a module; systemd's udev rules give the device to the `kvm` group; skipped without, failed
  under `AGTERM_REQUIRE_DISPLAY_TESTS`), its hardware case GBM and EGL.
- For the conformance suite: Python 3.11+ with venv, weston and dbus-daemon (each instance gets its own headless
  weston and bus); zsh or fish as the login shell for the Live tests of that shell.

On Ubuntu 26.04, the `rust-linux` CI job's packages (the last two lines, with meson and ninja, build weston 15, which
the GPUI runs need and 26.04 lacks; fish runs the fish replay script's test) plus `zstd` and `python3-venv`. Ubuntu
has no WPE WebKit, so the app builds there without its HTML overlays ([WPE WebKit](#wpe-webkit)):

```sh
sudo apt-get install pkg-config libfontconfig-dev libfreetype-dev \
  meson ninja-build libwayland-dev wayland-protocols libxkbcommon-dev libxkbcommon-x11-dev libxcb1-dev \
  curl xz-utils zstd patch ncurses-bin fish fontconfig fonts-dejavu-core \
  dbus-daemon weston xvfb x11-xkb-utils libgl1-mesa-dri libegl-mesa0 mesa-vulkan-drivers python3-venv \
  g++ libpixman-1-dev libinput-dev libevdev-dev libudev-dev libdrm-dev libegl-dev libgles-dev libgbm-dev \
  libcairo2-dev libpango1.0-dev libpng-dev hwdata
```

On Arch, the `conformance` CI container's packages but git and rustup (WPE WebKit and lavapipe from the distribution;
zsh and fish for the Live tests of those login shells):

```sh
sudo pacman -S --needed wpewebkit bubblewrap meson ninja wayland wayland-protocols libxkbcommon libxcb \
  curl xz zstd patch ncurses fontconfig ttf-dejavu procps-ng weston dbus xorg-server-xvfb mesa vulkan-swrast \
  pixman libinput libevdev libdrm cairo pango libpng hwdata seatd lcms2 python zsh fish
```

Run-time dependencies of every `agterm` binary:

- a Vulkan driver (GPUI renders through wgpu), Wayland or X11;
- WPE WebKit, linked under `agterm-gpui`'s default `html` feature (HTML overlays). Without the feature
  (`--no-default-features`) the app refuses every page. On Ubuntu 23.10+ see
  [WebKit sandbox on Ubuntu (AppArmor)](#webkit-sandbox-on-ubuntu-apparmor);
- for Live sessions, `zmx`, built into its vendored prefix and staged next to the binaries. libghostty-vt is linked
  statically; the quick terminal's layer-shell panel is GPUI's own Wayland surface, a window where the compositor
  offers no layer-shell.

### WebKit sandbox on Ubuntu (AppArmor)

WebKit runs the web process of every HTML overlay inside a bubblewrap sandbox, which needs an unprivileged user
namespace. Ubuntu 23.10 and later (`kernel.apparmor_restrict_unprivileged_userns=1`) grant one only to a program whose
AppArmor profile allows `userns`, as the profiles Ubuntu ships for its own WebKit apps (epiphany, devhelp) do.
Without one, `bwrap` fails (`setting up uid map: Permission denied`) and WebKit would abort the whole app, so agterm
refuses every HTML page instead and names this fix in the error. The sandbox stays on either way.

The fix is agterm's profile, [packaging/linux/apparmor/agterm](packaging/linux/apparmor/agterm): Ubuntu's shape,
unconfined plus `userns`, attached to a binary by its real path (the `bwrap` it starts inherits it):

```sh
scripts/install-apparmor-profile.sh /path/to/bin/agterm   # writes /etc/apparmor.d/agterm (sudo) and loads it
scripts/install-apparmor-profile.sh --uninstall           # unloads and removes it
scripts/install-apparmor-profile.sh --print [BINARY]      # shows the profile, changes nothing
```

Without a path it covers the conformance stage's `target/conformance-stage/bin/agterm`. A moved binary needs the profile installed again for its new path. On
a host without AppArmor, or one that does not restrict user namespaces, the script installs nothing.

## Native dependencies

libghostty-vt (with Ghostty's resources) and zmx are built outside cargo, each into its own vendored prefix
`${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/<name>-<key>/`. The key hashes the build's pins in `build/pins.env` and
its patches in `build/patches/`, so a complete prefix is reused as is and a pin of one build never rebuilds another.

```sh
scripts/build-ghostty-vt.sh    # the pinned, patched static libghostty-vt (Ghostty's vt C API), and Ghostty's themes,
                               # shell integration and terminfo at the same pin
scripts/build-zmx.sh           # the pinned, patched zmx behind Live sessions
scripts/fetch-wpe.sh           # the pinned WPE WebKit binary package, only where the system has none (below)
scripts/fetch-fonts.sh         # the fonts Ghostty embeds, which the GPUI frontend embeds too (below)
scripts/fetch-lavapipe.sh      # Mesa's lavapipe for headless GPUI runs: the system's, else the pinned Arch package
scripts/build-weston.sh        # weston 15 for headless GPUI runs where the system's is older (Ubuntu 26.04: 14)
```

The app's headless instances (the conformance suite's, `agterm-gpui`'s window tests') render on Mesa's software
Vulkan driver lavapipe only, never on the machine's GPU. The render-path harnesses also render offscreen on a hardware
render node, each GPU in a child process of its own: `agterm-gpui`'s `external_texture` and `html_overlay` tests (on
lavapipe and on the first render node a Mesa Vulkan driver runs) and `agterm-wpe`'s engine tests (on the first DRM
render node and in shared memory, [WPE WebKit](#wpe-webkit)).
`scripts/fetch-lavapipe.sh` prints its ICD
manifest: `AGTERM_LAVAPIPE_ICD` when set, else the distribution's (Ubuntu/Debian/Fedora `mesa-vulkan-drivers`, Arch
`vulkan-swrast`), else it verifies the pinned Arch `vulkan-swrast` package (`LAVAPIPE_*` in `build/pins.env`) and
unpacks it into the vendored prefix, refusing a host whose libLLVM or glibc it cannot load (`--print-icd` finds without
fetching); a manifest whose driver is missing is never used. Both the suite and the process tests ask it
(`tests/support/lavapipe.rs`), so they share one rule.
They also need a headless weston with a seat, which only `--fake-seat` gives (weston 15.0 and later): where the system's
is older, `scripts/build-weston.sh` builds the pinned weston (`WESTON_*`, with the wayland-protocols and
libdisplay-info it needs as subprojects) into the vendored prefix; `run-rust-linux.sh` takes it when
the weston on PATH has no `--fake-seat`, and the process tests take the first weston on PATH that has it (put the
prefix's `bin/` first, as CI does).

Each script also takes `--print-prefix` and `--verify`. Cargo never runs zig: `agterm-ghostty-vt-sys` fails with "run
scripts/build-ghostty-vt.sh" when the prefix is missing, `agterm-gpui` with "run scripts/fetch-fonts.sh".
`LIBGHOSTTY_VT_*` is the one Ghostty pin (the source archive's sha256 and the theme package's included): it follows
ghostty main, not upstream agterm's `GHOSTTY_REV`, whose vt API lacks what the app needs. libghostty-vt is linked
statically; staging takes the prefix's `share/` (the resources) only. `.cargo/config.toml` makes `scripts/pkg-config.sh`
the pkg-config of every cargo build (it picks the WPE WebKit, below).
`scripts/gen-ghostty-vt-bindings.sh` regenerates the bindings of `agterm-ghostty-vt-sys` after a `LIBGHOSTTY_VT_REV`
or ghostty-vt patch change, `scripts/gen-ghostty-config-keys.sh` agterm-core's ghostty config key table after a
`LIBGHOSTTY_VT_REV` bump, `scripts/gen-wpe-bindings.sh` the gir crates `agterm-wpe-sys`, `agterm-wpe-platform-sys` and
`agterm-wpe-javascriptcore-sys` after a `WPE_*` or `GIR_*` bump (`--check` compares instead of writing).

The GPUI frontend's terminal embeds the fonts Ghostty embeds, from the package deps of the Ghostty at
`LIBGHOSTTY_VT_REV` (`FONTS_*` in `build/pins.env`, each archive's sha256 pinned): JetBrains Mono 2.304 (its static
faces; SIL Open Font License 1.1), the face when `font-family` is unset, and Symbols Nerd Font Mono 3.4.0 (MIT), every
family's symbol fallback. Their licenses are in the prefix's `share/doc/fonts/`; a package of the binary ships them. The terminal's procedural sprites (box drawing, blocks, powerline, braille, legacy computing) are a port of
Ghostty's sprite font through qwertty-term (both MIT); `crates/agterm-gpui/tests/golden/` holds Ghostty's sprite test
images with Ghostty's license.

Some GPUI dependencies are patched in the tree rather than built: `vendor/calloop` and
`vendor/gpui-pre{,-wgpu,-linux}` (GPUI's dmabuf external surfaces for the HTML overlays, the font metrics accessors and
the coverage masks the terminal's sprites paint with, the native key behind each key event, the HTML clipboard offer,
xdg-output geometry and X11 window state) are verified crates.io sources plus `build/patches/<crate>`, recreated by
`scripts/vendor-calloop.sh` and `scripts/vendor-gpui.sh` (`--check` compares, as CI does; `vendor-gpui.sh --diff`
turns edits under `vendor/` back into the patch files).

### WPE WebKit

The GPUI frontend's HTML overlays run WPE WebKit offscreen (WPEPlatform API). Its bindings are generated by gtk-rs
`gir` from the `.gir` files of the pinned package (`WPE_*` and `GIR_*` in `build/pins.env`, WPE WebKit 2.54.1) and
need WPE WebKit 2.54 or newer (`WPE_API_VERSION`) at build time, found through pkg-config (`wpe-webkit-2.0`):

- Arch, and every packaged agterm: the distribution's `wpewebkit` (`sudo pacman -S wpewebkit`). A distribution build
  has its helper processes' path compiled in (`/usr/lib/wpe-webkit-2.0`), so the app depends on that package; a system
  `wpe-webkit-2.0` of the bound API wins over anything vendored.
- No root, Arch-compatible userland (glibc 2.44+ and the package's libraries): `scripts/fetch-wpe.sh` verifies the
  pinned Arch package (`WPE_ARCH_PACKAGE_*`) and unpacks it into the vendored prefix, which `scripts/pkg-config.sh`
  offers when the system has no WPE WebKit of the bound API. Building and linking need nothing more; running pages
  from that prefix needs the helper path overlaid (`bwrap --ro-overlay /usr/lib` with the prefix's `lib/` on top),
  since nothing relocates it. That overlay is a user namespace with `PR_SET_NO_NEW_PRIVS`, inherited by everything
  started in it (no `sudo` or setuid program, root's files shown as `nobody`'s), so the app never enters it: the
  engine runs in a worker process under it (`agterm_wpe::worker`: the app's own executable re-run, driven over a
  socket; every `main` that may start an engine calls `agterm_wpe::worker::serve_if_asked` first), and the app, its
  terminals, their zmx sessions and everything they start keep the user's privileges (`cargo test -p agterm-gpui --test
  privileges`). `agterm-wpe`'s engine tests (`cargo test -p agterm-wpe`) run every case through a worker and again in
  process inside the overlay (`agterm_wpe::launcher::command`). Pick the distribution package whenever you can: with
  it the engine runs in the app's own process.
- Ubuntu packages no `wpewebkit` at all (none since 22.04's 2.36, 26.04 included), and 24.04 cannot load the Arch
  package (glibc 2.39): Ubuntu builds the workspace without what links WPE (`WORKSPACE_WITHOUT_WPE` in
  `scripts/lib/native.sh`: the WPE crates and the GPUI frontend) and the GPUI frontend without its HTML overlays
  (`GPUI_WITHOUT_HTML`: `--no-default-features`, the core then refuses every page). The app with its pages (feature
  `html`, the default) needs a WPE WebKit: `run-rust-linux.sh` and `scripts/run-gpui-dev.sh` build it so, and fail
  naming this section where there is none.
- CI: the `wpe` job installs `wpewebkit` from a date-pinned Arch Linux Archive snapshot in a digest-pinned `archlinux`
  container, checks the WPE crates there and that the checked-in bindings equal a regeneration, then the GPUI
  frontend with its HTML overlays (clippy, unit tests, the headless page harness on lavapipe); the `conformance` jobs
  run in the same container.

`agterm-wpe` is the host over those bindings: WPE WebKit on an engine thread with its own `GMainContext`, a
`WPEDisplay`/`WPEView` subclass rendering on the render node the GPUI side names with the formats it imports, frames
out as dmabufs (SHM as a last-resort fallback), and the WebKit surface the HTML overlay uses: stores, scripts and the
bridge, policies, schemes, the clipboard, `<select>` popups, the context menu's hit test and input methods; in this
process or in a worker (`Host::start_worker` asks for one with the system's package too, as the engine tests do). Its
engine tests render headless on this machine's first DRM render node (Mesa; `WPE_TEST_RENDER_NODE` picks another) and
in shared memory, each frame mode on each backend in a child process of its own. The GPUI side (`agterm-gpui`'s `html_overlay`) has a
headless harness of its own (`cargo test -p agterm-gpui --test html_overlay`): the real engine on the GPU GPUI's
headless renderer opened, its frames read back through the external texture element, on lavapipe (SHM) and on a Mesa
render node (dmabufs).

Cargo does not notice the provider changing: after installing or removing either, run
`cargo clean -p agterm-wpe -p agterm-wpe-sys -p agterm-wpe-platform-sys -p agterm-wpe-javascriptcore-sys`.

## Build and run

```sh
cargo build --workspace --release      # without WPE WebKit: "${WORKSPACE_WITHOUT_WPE[@]}" and "${GPUI_WITHOUT_HTML[@]}"
scripts/stage.sh /tmp/agterm-stage     # bin/{agterm,agtermctl,zmx}, share/{ghostty,terminfo,agterm}
scripts/run-gpui-dev.sh [--release] [--state DIR] [-- AGTERM_ARGS...]   # an isolated dev instance
```

`stage.sh` (into a new or empty directory; `--profile debug` for a debug build) assembles the layout the suite, the
dev script and an install run from.
`run-gpui-dev.sh` (`scripts/lib/dev-instance.sh`) builds and stages the app into a fresh `/tmp/agterm-dev-*` root and
launches an isolated instance: a unique
`AGTERM_APP_ID`, its own `AGTERM_STATE_DIR` and `AGTERM_CONTROL_SOCKET`, and `AGTERM_*`/`ZMX_*` inherited from a live
agterm session dropped (all but `AGTERM_LOG`). It prints the `agtermctl <command> --socket ...` line that drives the
instance.

The build uses the fork's app id, state dir and socket: while an installed agterm runs, launch a checkout's build only
through `run-gpui-dev.sh`. A plain launch is forwarded to the running instance over the session bus (the D-Bus name
above), or, with no bus between them, shares its `windows.json`.

`AGTERM_LOG` sets the log level (`tracing` filter syntax); the log is `agterm.log` in the state dir.

## Test

The gates every change passes (CI runs the same checks):

```sh
cargo fmt --all --check
cargo metadata --locked --format-version 1 >/dev/null   # Cargo.lock is current
scripts/check-tracing-features.sh           # no dependency turns on tracing's `log` fallback
scripts/vendor-calloop.sh --check           # vendor/ equals its crates.io sources plus build/patches/
scripts/vendor-gpui.sh --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy -p agterm-gpui --no-default-features --all-targets -- -D warnings   # the app without HTML overlays
scripts/lint-report.sh                      # complexity #[expect]s with their reasons; refuses inner #![allow]
GDK_BACKEND=x11 xvfb-run -a dbus-run-session cargo test --workspace
.venv/bin/python -m pytest conformance/selftests   # the suite's own tests, host-free
conformance/tools/run-rust-linux.sh [--ratchet-only] [--stage DIR] [-- PYTEST_ARGS...]
```

A change to the WPE or gir pins also runs `scripts/gen-wpe-bindings.sh --check`, as the `wpe` CI job does.

- The app's window tests need a display and a session bus; under `xvfb-run` and `dbus-run-session` they get their
  own and never touch the desktop's. The process tests (`crates/agterm-gpui/tests`) start their own private weston,
  Xvfb and bus besides, from an environment built from scratch, and render on lavapipe
  (`tests/support/lavapipe.rs`).
- The selftests, and the pytest runs some of them start, import the harness the venv's editable install points at: a
  venv installed from another checkout (a worktree sharing the main checkout's) needs this checkout's `conformance/`
  first on `PYTHONPATH` (`run-rust-linux.sh` puts it there itself).
- `--workspace` includes the WPE crates (`agterm-wpe` and its -sys crates) and the GPUI frontend's HTML overlays,
  which need WPE WebKit ([WPE WebKit](#wpe-webkit)); without it, pass `"${WORKSPACE_WITHOUT_WPE[@]}"` and then
  `"${GPUI_WITHOUT_HTML[@]}"` (from `scripts/lib/native.sh`) instead, as the Ubuntu CI jobs do.
- `run-rust-linux.sh` builds the release workspace, stages it into `target/conformance-stage`, runs the protocol, cli
  and behaviors layers with `--impl rust` and exits with the ratchet's status. It runs the suite with `$PYTHON`, else
  `.venv/bin/python`, else `python3`, with the suite installed:
  `python3 -m venv .venv && .venv/bin/pip install -e conformance`. See
  [conformance/README.md](conformance/README.md) and [conformance/docs/rust-build.md](conformance/docs/rust-build.md).
  It renders on lavapipe from `scripts/fetch-lavapipe.sh`.
- Lint rules: `[workspace.lints]` in `Cargo.toml` (clippy `all` + `pedantic`, the complexity and robustness lints
  denied) with the thresholds in `clippy.toml`; the only exception is a reasoned outer `#[expect]` on one item.

## CI

- `rust-linux.yml`: `rust-linux` (format, clippy, the lint report into the job summary, `cargo test`, all on the
  workspace without the WPE crates and the app without its HTML overlays), `wpe` (clippy, tests and the bindings'
  regeneration check of the WPE crates on Arch's `wpewebkit`, then the app with its overlays, see
  [WPE WebKit](#wpe-webkit)), then the staged build (HTML overlays on) under the suite in the `wpe` job's Arch
  container, on its lavapipe, held by `rust-passing.txt`, once per login shell (`conformance`, a matrix: bash, fish
  and zsh), each with `--must-pass live-any-shell`, the zsh one with `--must-pass live-sessions` too; the bash one runs
  the app's process tests with the overlays on first.
- `cross-platform.yml`: the crates that build on all three OSes (`agterm-vt` and `agterm-session` so far), on
  Ubuntu, macOS and Windows runners: format, clippy and `cargo test` (`crates`, a matrix), then the VT memory core's
  unit tests under Miri on a nightly pinned by date (`miri`). It also runs on pushes to `ci/**` branches, to check a
  macOS or Windows form without a PR.
- `conformance-gate.yml`: the suite's self-tests and catalog coverage gate.
- `reference-macos.yml`, `spike-macos.yml`: the suite against upstream's macOS release.

## Layout

- `crates/`: the Cargo workspace (crate map in [CLAUDE.md](CLAUDE.md)).
- `conformance/`: the suite, its catalog of upstream's commands, the ratchet list `rust-passing.txt`.
- `build/`: `pins.env`, the ghostty-vt, zmx, gir, calloop and GPUI patches, agterm's own themes.
- `vendor/`: the patched GPUI and calloop crates, recreated by the vendor scripts (an edit there goes back into its
  patch).
- `scripts/`: native builds, staging, `run-gpui-dev.sh`, the lint report, the AppArmor profile installer.
- `packaging/linux/apparmor/`: agterm's AppArmor profile (WebKit's sandbox on Ubuntu).
- `resources/`: the agent-status hooks and agent skill the app installs.
- `docs/linux-ui-checklist.md`: every upstream UI feature with its Linux form and manual check (no Linux UI driver
  exists). `docs/plans/`: implementation plans.
