# Spike: the quick terminal on GPUI layer-shell, and window activation (Task 3)

Plan: `docs/plans/20261014-gpui-frontend.md`, Task 3. Code: `spikes/gpui-layer-shell/` (throwaway, its own cargo
workspace outside the repository's). Raw results: `spikes/gpui-layer-shell/results/*.jsonl` (one JSON line per check).

Spike code and raw results: branch `spike/gpui-layer-shell` at commit `caa56d7` (`spikes/gpui-layer-shell/`,
removed from `gpui-frontend` in Task 4).

**Verdict: pass.** The quick terminal can go on GPUI as planned: `WindowKind::LayerShell` is in the pinned GPUI
(gpui-kit's pin, `gpui-pre` 0.3.9), it works on niri with the GTK build's surface attributes, and the
no-layer-shell fallback works where a compositor has none. A second launch's xdg-activation token raises and focuses
the primary's window on niri. Nothing here needs a GPUI patch to ship; four gaps want one (or a workaround in
agterm-gpui), listed at the end.

## What was run

Versions, looked up on 2026-10-14: `gpui-kit` 0.7.3 (`default-features = false`: no component or asset layer, no
`gpui-fast`), which pins `gpui-pre` / `gpui-pre-platform` `=0.3.9` (the platform with `font-kit`, `x11`, `wayland`,
`runtime_shaders`); `zbus` 5.19.0, `wayland-client` 0.31.15, `wayland-backend` 0.3.17 (`client_system`),
`wayland-protocols` 0.32.13, toolchain 1.98.1. niri 26.04, weston 15.0.1, GLib 2.90.1, GTK 4.24.1.

`spikes/gpui-layer-shell/drive.py RUN…` runs each check, isolated from the live agterm: `AGTERM_*`/`ZMX_*` stripped,
a private session bus, its own app id (`io.github.melonamin.agterm.SpikeT3`); nothing talks to agterm's socket or
state, and no zmx is involved. Every build and run went through `agterm-agents.slice`.

| run | where | what |
|---|---|---|
| `quick-real` | the user's niri session (two outputs: HDMI-A-1 3440x1440 @1, eDP-1 2880x1800 @1.75), NVIDIA box | the panel's life cycle on layer-shell |
| `activation-real` | the same session | activation by a second launch |
| `quick-nested`, `activation-nested` | a private niri (stock config, no `honor-xdg-activation-with-invalid-serial`) nested in a private headless weston | the same checks, reproducible and off the desktop |
| `quick-weston` | a private headless weston (`--fake-seat`), which has no layer-shell | the fallback path |
| `quick-x11`, `activation-x11` | GPUI's X11 backend on a private Xvfb, no window manager | the X11 fallback path |

The quick run is a script inside the spike binary (`src/quick.rs`): show (layer surface on the overlay layer,
namespace `agterm-quick-terminal-spike`, anchored to the top edge with a top margin that centers it, keyboard on
demand, exclusive zone 0, no output named, sized 90% capped at 1100x700 of the estimate display: the GTK build's
`layer.rs`/`placement.rs` choices), refit to the output it landed on, resize while shown, hide, show again, zoom
(anchored to all four edges), one show per output with the output named, then focus loss (a toplevel opens over the
shown panel). The activation run (`src/activation.rs`, `src/gapplication.rs`, `second_launch.py`): a primary owns the
app id on the bus and serves `org.gtk.Application` / `org.gtk.Actions` (`DescribeAll`, `Activate`) at GApplication's
object path, a decoy window takes focus away, then GPUI's own `Window::activate_window`, then second launches — a
plain `Gio.Application` and a `Gtk.Application` (the GTK build's kind), started by `niri msg action spawn` so niri
hands them an `XDG_ACTIVATION_TOKEN`, or with a hand-made `DESKTOP_STARTUP_ID` on X11.

## Results per plan point

### 1. `WindowKind::LayerShell` in the pinned GPUI: anchored top layer, keyboard, show/hide, resize — **pass**

- Present in `gpui-pre` 0.3.9 (`platform/layer_shell.rs`: `LayerShellOptions { namespace, layer, anchor,
  exclusive_zone, exclusive_edge, margin, keyboard_interactivity }`), behind GPUI's `wayland` feature, which gpui-kit
  enables. Wayland only; a compositor without `zwlr_layer_shell_v1` makes `open_window` fail with
  `LayerShellNotSupportedError` (downcastable from the `anyhow::Error`).
- niri lists the surface exactly as the GTK build's: `layer: Overlay`, `keyboard_interactivity: OnDemand`, on the
  output it chose (real and nested).
- Focus on show: the panel holds the keyboard 17–22 ms after `open_window` on the real session and nested niri, 122 ms
  for the process's first window (GPU device setup).
- Resize while shown: `Window::resize` maps to `zwlr_layer_surface_v1.set_size`; the new size is applied (real:
  864x540, nested: 768x480, as asked).
- Hide/show: GPUI on Linux cannot hide a window, so hiding is `remove_window` (niri drops the surface) and showing
  opens a new layer surface (17–21 ms to focus). This is fine for agterm-gpui because the terminal lives in the model
  (agterm-term), not in the window, unlike the GTK build's `GtkGLArea` that had to stay realized.
- `QuitMode`: GPUI's default on Linux quits when the last window closes, so hiding the panel quit the first spike
  run. agterm-gpui must use `Application::with_quit_mode(QuitMode::Explicit)`.

### 2. What the quick terminal needs (`crates/agterm-gtk/src/quick*`) — **pass, with workarounds**

| need (GTK build) | GPUI | result |
|---|---|---|
| overlay layer, namespace, keyboard on demand, exclusive zone 0 | creation options | pass (niri: `Overlay`, `OnDemand`) |
| centered: top anchor + top margin + size | creation options | pass (1100x700 on both outputs) |
| zoom: anchor all four edges | creation options; no runtime `set_anchor`/`set_margin`/`set_layer` | pass by reopening: real 3440x1440, nested 1280x800 (the output's usable area) |
| no output named on show, so the compositor picks its focused one | `display_id: None` | pass: landed on niri's focused output (HDMI-A-1) |
| refit to the landing output when the estimate was another one | `window.display()` follows `wl_surface.enter`; `observe_window_display` exists | pass by reopening named on the landed output: the estimate was eDP-1 (GPUI's first display), it landed on HDMI-A-1, the refit reopened there and held focus (20 ms) |
| multi-output placement | `WindowOptions::display_id` → layer surface output | pass: one show per output, each on the output asked |
| focus loss dismisses a summoned panel | `observe_window_activation` | pass: the panel saw the keyboard leave when a toplevel opened over it (niri focused the new toplevel) |
| monitor change under a shown panel | `zwlr_layer_surface_v1.closed` closes the GPUI window | not run (unplugging an output of the user's desktop was out of bounds); from source the window closes, which agterm-gpui maps to the dismissal the GTK build does |

Two placement observations from the real session:

- An on-demand layer surface shown on an output niri does not focus (eDP-1 named while HDMI-A-1 was focused) did
  **not** get the keyboard; `KeyboardInteractivity::Exclusive` did not either. That is niri's policy, not GPUI's, and
  it is why the GTK build names no output on show. agterm-gpui keeps that: name no output on show, refit (reopen
  named) only on the output it landed on.
- GPUI's display bounds ignore fractional scale: eDP-1 at scale 1.75 is 1645x1028 logical in niri but GPUI reports
  1440x900 at y 205.5 (physical size over the integer `wl_output.scale` 2; it does not use `xdg-output`). The panel's
  own rendering is right (GPUI uses `wp_fractional_scale_v1` for windows), but the centering margin and a
  percentage size computed from `display.bounds()` are off on such outputs (64 px too high here). Gap 3 below.

### 3. Fallback for compositors without layer-shell — **pass (weston, run); X11 pass for placement, focus WM-bound**

- weston (no `zwlr_layer_shell_v1`): `open_window` with `LayerShell` fails with `LayerShellNotSupportedError`, the
  panel falls back to an undecorated normal window (`WindowDecorations::Client`, no titlebar, transparent
  background): shown and focused (62 ms first, 16–18 ms after), 1100x700, resize, hide/show, zoom (maximized to
  1920x1080), named output, focus loss — all pass.
- GPUI on weston's headless backend panics without a seat (`Wayland compositor does not provide wl_seat`); the
  harness's weston (`impls/base.py`) must add `--fake-seat` for any GPUI run (Task 17).
- X11 (Xvfb, GPUI's X11 backend): GPUI accepts `WindowKind::LayerShell` on X11 and opens an ordinary window, so the
  host must be chosen by backend (`cx.compositor_name() == "Wayland"`), not by the open error. The window host:
  shown at 1100x700, resize, hide/show, placement on the screen — pass. Zoom (`WindowBounds::Maximized`) stays
  1100x700 and nothing gets keyboard focus, because Xvfb has no window manager: on X11 both are the window manager's
  job, and GPUI's X11 backend never sets input focus itself (no `XSetInputFocus`, no `_NET_WM_USER_TIME`). With a
  real WM (GNOME on X11, KDE on X11) a newly mapped normal window normally gets focus: inferred, not run (no X11 WM is
  installed on this machine).

### 4. Window activation by a second launch — **pass on Wayland (niri); X11 needs a patch**

| case | nested niri (stock) | real niri (user config) | Xvfb |
|---|---|---|---|
| primary focused on open | pass | pass | no WM: nothing focused |
| decoy takes focus | pass | pass | no WM |
| GPUI `Window::activate_window` (fresh token from the last pointer serial) | refused, as expected | **focused**: the user's niri config has `debug { honor-xdg-activation-with-invalid-serial }` | no WM |
| `Gio.Application` second launch, no token | platform data `cwd` only; not focused | same | same |
| `Gio.Application` second launch with `XDG_ACTIVATION_TOKEN` in its environment | **plain GApplication does not forward the token** (platform data `cwd` only); not focused | same | same |
| `Gtk.Application` second launch (the GTK build's kind) with niri's token | platform data `activation-token`, `desktop-startup-id`, `cwd`; the primary activates with the token: **focused in 12 ms** | **focused in 10 ms** | GTK sends the startup id as `activation-token` and `desktop-startup-id`; GPUI's `_NET_ACTIVE_WINDOW` (time 0) has no WM to honor it |

How the primary uses the token: GPUI consumes `XDG_ACTIVATION_TOKEN` from its own environment only for its first
toplevel (and removes the variable from the process environment at platform creation), and has no API to activate an
existing window with a token from elsewhere. The spike binds `xdg_activation_v1` on GPUI's own `wl_display` (a second
event queue over the same libwayland connection, from the window's `raw-window-handle` display and surface) and calls
`activate(token, surface)` (`src/activation.rs`, about 20 lines, `unsafe` for the two foreign pointers). It works on
the real session and the nested niri. The proper form is a one-method GPUI patch (gap 1).

For agterm-gpui's single-instance design (Task 12):

- A GTK launch forwarding to a running GPUI primary works as is: GtkApplication puts the token in `activation-token`.
- A GPUI launch forwarding to a running primary must read `XDG_ACTIVATION_TOKEN` (and `DESKTOP_STARTUP_ID`) itself,
  before GPUI's platform is created (GPUI removes it), and put it in the `Activate` platform data as
  `activation-token` — plain GApplication does not, so copying GApplication's behavior is not enough.
- The primary must serve `org.gtk.Actions.DescribeAll` (a non-primary calls it while registering); the spike's zbus
  server with an empty `DescribeAll` is accepted by GLib 2.90's GApplication and GtkApplication.
- On X11 the token's counterpart is the startup-notification id (`DESKTOP_STARTUP_ID`, platform data
  `desktop-startup-id`; GTK also sends it as `activation-token`): its `_TIME<n>` suffix is the launching event's X
  server timestamp, which the primary should pass in its `_NET_ACTIVE_WINDOW` request (source 1) and/or as the
  window's `_NET_WM_USER_TIME`, and then end the startup sequence. GPUI's X11 `activate_window` sends
  `_NET_ACTIVE_WINDOW` with `CURRENT_TIME` (0), which window managers with focus-stealing prevention may treat as
  "no user action" (gap 2).

## Per-environment table

| environment | host | run? | result | gaps / patches |
|---|---|---|---|---|
| niri (wlroots-style layer-shell) | layer surface | run: real session (2 outputs, fractional eDP-1) and nested | pass: attributes, focus on show, resize, hide/show, zoom, multi-output, refit, focus loss, activation with a second launch's token | gap 1 (token activation API), gap 3 (fractional-scale display bounds), gap 4 (re-anchor needs a reopen) |
| other wlroots compositors (sway, Hyprland, labwc) | layer surface | not run (not installed) | inferred pass: the same protocol GPUI's `LayerShell` targets (Zed PR #35610 lists wlroots compositors) | as niri; focus policy for on-demand surfaces and activation tokens is per compositor |
| KDE Plasma (KWin, Wayland) | layer surface | not run (not installed) | inferred pass: KWin implements `zwlr_layer_shell_v1`, GPUI documents it as supported | as niri |
| GNOME (Mutter, Wayland) | normal window (fallback) | not run (not installed) | inferred pass: same path as weston — Mutter has no layer-shell, `open_window` fails with `LayerShellNotSupportedError`, the window host takes over; Mutter honors xdg-activation tokens | gap 1 for activation; no panel above other windows (as the GTK build on GNOME) |
| weston | normal window (fallback) | run (headless, `--fake-seat`) | pass: show/focus, size, resize, hide/show, zoom (maximize), focus loss | harness needs `--fake-seat` for GPUI |
| X11 (any WM) | normal window (fallback, chosen by `compositor_name`) | run on Xvfb without a WM | pass for open, size, resize, hide/show; focus, maximize and activation are the WM's and could not run | gap 2 (startup-id timestamp on `_NET_ACTIVE_WINDOW`); GPUI never sets input focus itself |

## Gaps that need a GPUI patch (or a workaround in agterm-gpui)

1. **Activate a window with an external xdg-activation token** (`Window::activate_with_token(&str)` → Wayland
   `xdg_activation_v1.activate(token, surface)`). Workaround proven here: bind `xdg_activation_v1` on GPUI's
   `wl_display` through `raw-window-handle` (no GPUI change, ~20 lines). A small upstreamable patch.
2. **X11 activation with a timestamp** (`_NET_ACTIVE_WINDOW` with the startup id's `_TIME`, `_NET_WM_USER_TIME`,
   startup-notification `remove`). Workaround: send the client message ourselves through the window's raw X11 handle
   (x11rb), the same shape as gap 1. Not verifiable without an X11 window manager.
3. **Display bounds on fractional-scale Wayland outputs** use the integer `wl_output.scale` instead of
   `xdg-output`'s logical size. Affects centering and percentage sizes of the panel on such outputs. Workaround: a
   zoom-style probe is not worth it; patch GPUI's Wayland display to read `zxdg_output_v1` logical geometry.
4. **No runtime `set_anchor` / `set_margin` / `set_layer` / `set_keyboard_interactivity`** on a layer surface (only
   size and exclusive zone/edge). The zoom toggle and the landing refit reopen the surface (one frame of absence; the
   terminal state is unaffected). A patch exposing the `zwlr_layer_surface_v1` setters would make them in-place.

Not gaps but rules for the implementation: `QuitMode::Explicit`; choose the host by `cx.compositor_name()` (GPUI
builds `LayerShell` on X11 as a plain window); name no output on show; GPUI removes `XDG_ACTIVATION_TOKEN` from the
environment at startup, so read it first.

## Surprises and caveats

- The user's niri honors activation with an invalid serial (`honor-xdg-activation-with-invalid-serial`, set for
  Noctalia), so on this desktop even GPUI's serial-less `activate_window` raises windows; stock niri refuses it. The
  nested run is the one that shows the token path is needed.
- During the real-session runs another spike (Task 2's WPE window) was on the same desktop; one activation case's
  decoy step saw that window focused instead of the spike's decoy, which does not change the outcome (the primary was
  raised from an unfocused state each time).
- The rounded, transparent panel frame was not inspected visually (no screenshot of the user's desktop was taken).
- The real session's zoom filled 3440x1440 on HDMI-A-1: the desktop's top bar reserves no exclusive zone there, so
  that is the output's usable area.
