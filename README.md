# agterm-rs

agterm in Rust, Linux first: a GTK4/libadwaita terminal over libghostty, driven by `agtermctl` over a control socket.

- The parity target is upstream macOS agterm ([umputun/agterm](https://github.com/umputun/agterm) v0.36.0): its wire
  protocol, command semantics, error strings, `agtermctl`, on-disk formats, model and UI, the UI in GTK4/libadwaita
  idioms where AppKit has no direct equivalent.
- The Linux build keeps the Swift/GTK fork's (`agterm-linux`) application id `io.github.melonamin.agterm`, state dir
  (`$XDG_DATA_HOME/agterm`, else `~/.local/share/agterm`) and control socket (`<state dir>/agterm.sock`), so it can
  replace the installed fork with the existing state.
- One extension beyond upstream: Live sessions in bash and fish login shells too (upstream: zsh only), under the
  Rust-only capability `live-any-shell`.
- Parity is measured by the black-box suite in [conformance/](conformance/README.md), held by a ratchet.

## Prerequisites

- Rust 1.98.1 with clippy and rustfmt, pinned by `rust-toolchain.toml` (`rustup toolchain install` reads it).
- Zig 0.16.0 for libghostty, libghostty-vt and zmx (`*_ZIG_VERSION` in `build/pins.env`). The build scripts take
  `AGTERM_GHOSTTY_ZIG` / `AGTERM_GHOSTTY_VT_ZIG` / `AGTERM_ZMX_ZIG`, a zig 0.16.0 on `PATH` or from mise, else they
  download the pinned ziglang.org tarball and check its sha256.
- GTK 4.14+, libadwaita 1.5+ and WebKitGTK 6.0 with their development files, and pkg-config.
- gtk4-layer-shell: the distribution's package, or `scripts/build-layer-shell.sh` (below), which needs meson, ninja, a
  C compiler and the Wayland development files.
- WPE WebKit 2.54+ with its WPEPlatform API for the WPE -sys crates of the GPUI frontend
  (see [WPE WebKit](#wpe-webkit)); the GTK build does not need it.
- For the native builds: curl, tar, xz, patch, sha256sum and ncurses' `tic`/`infocmp`.
- For the GTK unit tests: Xvfb's `xvfb-run`, so they get a display of their own instead of the desktop.
- For the conformance suite: Python 3.11+ with venv, weston and dbus-daemon (each instance gets its own headless
  weston and bus); zsh or fish as the login shell for the Live tests of that shell.

On Ubuntu 24.04, the CI image's packages plus `xvfb` and `python3-venv` for local runs:

```sh
sudo apt-get install pkg-config libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev \
  meson ninja-build libwayland-dev wayland-protocols curl xz-utils patch ncurses-bin \
  weston dbus-daemon libgl1-mesa-dri libegl-mesa0 xvfb python3-venv
```

Run-time dependencies of every `agterm` binary:

- GTK 4, libadwaita;
- WebKitGTK 6.0, linked under `agterm-gtk`'s default `html` feature (HTML overlays). Without the feature
  (`--no-default-features`) the app installs no page host and refuses every page. On Ubuntu 23.10+ see
  [WebKit sandbox on Ubuntu (AppArmor)](#webkit-sandbox-on-ubuntu-apparmor);
- `libgtk4-layer-shell.so.0`, the quick terminal's layer-shell panel. It is linked unconditionally, since it must load
  before libwayland-client; on a compositor without layer-shell the quick terminal falls back to a window;
- `libghostty.so` and, for Live sessions, `zmx`: both come from the vendored prefixes and are staged next to the
  binaries.

### WebKit sandbox on Ubuntu (AppArmor)

WebKit runs the web process of every HTML overlay inside a bubblewrap sandbox, which needs an unprivileged user
namespace. Ubuntu 23.10 and later (`kernel.apparmor_restrict_unprivileged_userns=1`) grant one only to a program whose
AppArmor profile allows `userns`, as the profiles Ubuntu ships for its own WebKitGTK apps (epiphany, devhelp) do.
Without one, `bwrap` fails (`setting up uid map: Permission denied`) and WebKit would abort the whole app, so agterm
refuses every HTML page instead and names this fix in the error. The sandbox stays on either way.

The fix is agterm's profile, [packaging/linux/apparmor/agterm](packaging/linux/apparmor/agterm): Ubuntu's shape,
unconfined plus `userns`, attached to one agterm binary by its real path (the `bwrap` it starts inherits it):

```sh
scripts/install-apparmor-profile.sh /path/to/bin/agterm   # writes /etc/apparmor.d/agterm (sudo) and loads it
scripts/install-apparmor-profile.sh --uninstall           # unloads and removes it
scripts/install-apparmor-profile.sh --print [BINARY]      # shows the profile, changes nothing
```

Without a path it covers the conformance stage's `target/conformance-stage/bin/agterm`. A moved binary needs the
profile installed again for its new path. On a host without AppArmor, or one that does not restrict user namespaces,
the script installs nothing.

## Native dependencies

libghostty, libghostty-vt, zmx and gtk4-layer-shell are built outside cargo, each into its own vendored prefix
`${AGTERM_VENDOR:-~/.cache/agterm-rs/vendor}/<name>-<key>/`. The key hashes the build's pins in `build/pins.env` and
its patches in `build/patches/`, so a complete prefix is reused as is and a pin of one build never rebuilds another.

```sh
scripts/build-ghostty.sh       # the pinned, patched libghostty, its themes, shell integration and terminfo
scripts/build-ghostty-vt.sh    # the pinned static libghostty-vt (Ghostty's vt C API) for the GPUI frontend
scripts/build-layer-shell.sh   # gtk4-layer-shell 1.3.0; skip it when the distribution's package is installed
scripts/build-zmx.sh           # the pinned, patched zmx behind Live sessions
scripts/fetch-wpe.sh           # the pinned WPE WebKit binary package, only where the system has none (below)
```

Each script also takes `--print-prefix` and `--verify`. Cargo never runs zig: `agterm-ghostty-sys` fails with "run
scripts/build-ghostty.sh" when the prefix is missing, `agterm-ghostty-vt-sys` with "run scripts/build-ghostty-vt.sh".
libghostty-vt has its own Ghostty pin (`LIBGHOSTTY_VT_*`, the source archive's sha256 included) while both frontends
coexist, and is linked statically, so staging gains nothing from it. `.cargo/config.toml` makes
`scripts/pkg-config.sh` the pkg-config of every cargo build, which puts the vendored gtk4-layer-shell ahead of a system
package. After bumping the `LAYER_SHELL_*` pins, run `cargo clean -p gtk4-layer-shell-sys` so the link follows the new
prefix.
`scripts/gen-ghostty-bindings.sh` regenerates the checked-in bindings after a `GHOSTTY_REV` or ghostty patch change,
`scripts/gen-ghostty-vt-bindings.sh` those of `agterm-ghostty-vt-sys` after a `LIBGHOSTTY_VT_REV` bump,
`scripts/gen-wpe-bindings.sh` the gir crates `agterm-wpe-sys`, `agterm-wpe-platform-sys` and
`agterm-wpe-javascriptcore-sys` after a `WPE_*` or `GIR_*` bump (`--check` compares instead of writing).

### WPE WebKit

The GPUI frontend's HTML overlays run WPE WebKit offscreen (WPEPlatform API). Its bindings are generated by gtk-rs
`gir` from the `.gir` files of the pinned package (`WPE_*` and `GIR_*` in `build/pins.env`, WPE WebKit 2.54.1) and
need WPE WebKit 2.54 or newer (`WPE_API_VERSION`) at build time, found through pkg-config (`wpe-webkit-2.0`):

- Arch, and every packaged agterm: the distribution's `wpewebkit` (`sudo pacman -S wpewebkit`). A distribution build
  has its helper processes' path compiled in (`/usr/lib/wpe-webkit-2.0`), so the app depends on that package; a system
  `wpe-webkit-2.0` of the bound API wins over anything vendored.
- No root, Arch-compatible userland (glibc 2.44+ and the package's libraries): `scripts/fetch-wpe.sh` verifies the
  pinned Arch package (`WPE_ARCH_PACKAGE_*`) and unpacks it into the vendored prefix, which `scripts/pkg-config.sh`
  offers when the system has no WPE WebKit of the bound API. Building, linking and the crates' tests need nothing more;
  running pages from that prefix needs the helper path overlaid (`bwrap --ro-overlay /usr/lib` with the prefix's
  `lib/` on top), since nothing relocates it. Pick the distribution package whenever you can.
- Ubuntu 24.04 packages no `wpewebkit` at all (none since 22.04's 2.36) and cannot load the Arch package (glibc 2.39):
  it builds the GTK app without the WPE crates (`WORKSPACE_WITHOUT_WPE` in `scripts/lib/native.sh`, which
  `scripts/run-dev.sh`, `run-rust-linux.sh` and the Ubuntu CI jobs use). Before the GPUI frontend makes WPE a
  dependency of what Ubuntu builds, it needs a provider there (a source build into the vendored prefix) or the GPUI
  checks move to the Arch CI job.
- CI: the `wpe` job installs `wpewebkit` from a date-pinned Arch Linux Archive snapshot in a digest-pinned `archlinux`
  container, checks the WPE crates there and that the checked-in bindings equal a regeneration.

Cargo does not notice the provider changing: after installing or removing either, run
`cargo clean -p agterm-wpe-sys -p agterm-wpe-platform-sys -p agterm-wpe-javascriptcore-sys`.

## Build and run

```sh
cargo build --workspace --release      # without WPE WebKit: "${WORKSPACE_WITHOUT_WPE[@]}" instead of --workspace
scripts/stage.sh /tmp/agterm-stage     # bin/{agterm,agtermctl,zmx}, lib/, share/{ghostty,terminfo,agterm}
scripts/run-dev.sh [--release] [--state DIR] [-- AGTERM_ARGS...]
```

`stage.sh` (into a new or empty directory; `--profile debug` for a debug build) assembles the layout the suite,
`run-dev.sh` and an install run from; the staged binaries find `lib/` through their `$ORIGIN/../lib` rpath.
`run-dev.sh` builds and stages into a fresh `/tmp/agterm-dev-*` root and launches an isolated instance: a unique
`AGTERM_APP_ID`, its own `AGTERM_STATE_DIR` and `AGTERM_CONTROL_SOCKET`, and `AGTERM_*`/`ZMX_*` inherited from a live
agterm session dropped (all but `AGTERM_LOG`). It prints the `agtermctl <command> --socket ...` line that drives the
instance.

The build uses the fork's app id, state dir and socket: while an installed agterm runs, launch a checkout's build only
through `run-dev.sh`. A plain launch is forwarded to the running instance, or shares its `windows.json`.

`AGTERM_LOG` sets the log level (`tracing` filter syntax); the log is `agterm.log` in the state dir.

## Test

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
scripts/lint-report.sh                      # complexity #[expect]s with their reasons; refuses inner #![allow]
GDK_BACKEND=x11 xvfb-run -a cargo test --workspace
conformance/tools/run-rust-linux.sh [--stage DIR] [-- PYTEST_ARGS...]
```

- The GTK unit tests need a display and skip without one (as in CI, which runs plain `cargo test`); under `xvfb-run`
  they run without touching the desktop.
- `--workspace` includes the WPE -sys crates, which need WPE WebKit ([WPE WebKit](#wpe-webkit)); without it, pass
  `"${WORKSPACE_WITHOUT_WPE[@]}"` (from `scripts/lib/native.sh`) instead, as the Ubuntu CI jobs do.
- `run-rust-linux.sh` builds the release workspace, stages it into `target/conformance-stage`, runs the protocol, cli
  and behaviors layers with `--impl rust` and exits with the ratchet's status. It runs the suite with `$PYTHON`, else
  `.venv/bin/python`, else `python3`, with the suite installed:
  `python3 -m venv .venv && .venv/bin/pip install -e conformance`. See
  [conformance/README.md](conformance/README.md) and [conformance/docs/rust-build.md](conformance/docs/rust-build.md).
- Lint rules: `[workspace.lints]` in `Cargo.toml` (clippy `all` + `pedantic`, the complexity and robustness lints
  denied) with the thresholds in `clippy.toml`; the only exception is a reasoned outer `#[expect]` on one item.

## CI

- `rust-linux.yml`: `rust-linux` (format, clippy, the lint report into the job summary, `cargo test`, all on the
  workspace without the WPE crates), `wpe` (clippy, tests and the bindings' regeneration check of the WPE crates on
  Arch's `wpewebkit`, see [WPE WebKit](#wpe-webkit)), then the staged build under the suite, once per login shell:
  `conformance` (bash), `conformance-fish` and `conformance-zsh`, each with `--must-pass live-any-shell`, the zsh job
  with `--must-pass live-sessions` too. Each installs agterm's AppArmor profile for the staged binary and keeps the
  runner's user-namespace restriction on, so the HTML tests prove it.
- `conformance-gate.yml`: the suite's self-tests and catalog coverage gate.
- `reference-macos.yml`, `spike-macos.yml`: the suite against upstream's macOS release.

## Layout

- `crates/`: the Cargo workspace (crate map in [CLAUDE.md](CLAUDE.md)).
- `conformance/`: the suite, its catalog of upstream's commands, the ratchet list `rust-passing.txt`.
- `build/`: `pins.env`, the ghostty, zmx and gir patches, agterm's own themes.
- `scripts/`: native builds, staging, `run-dev.sh`, the lint report, the AppArmor profile installer.
- `packaging/linux/apparmor/`: agterm's AppArmor profile (WebKit's sandbox on Ubuntu).
- `resources/`: the agent-status hooks and agent skill the app installs.
- `docs/linux-ui-checklist.md`: every upstream UI feature with its GTK form and manual check (no Linux UI driver
  exists). `docs/plans/`: implementation plans.
