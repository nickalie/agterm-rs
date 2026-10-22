# Ghostty config keys the GPUI build ignores

**Deferred with the user's sign-off on 2026-10-17** («Давай отложим эти ключи», the Task 35 review of
`docs/plans/20261014-gpui-frontend.md`). Work item: [docs/backlog/implement-deferred-ghostty-config-keys.md](backlog/implement-deferred-ghostty-config-keys.md).

This is a living list: when a key gets implemented, take its row out (and the backlog item out with the last one).

## What "ignored" means

The GPUI build parses Ghostty's config itself (`crates/ngterm-core/src/ghostty_config/`). Every key Ghostty knows at
`LIBGHOSTTY_VT_REV` is accepted, and its value is checked as libghostty checks it, so `config.reload` counts the same
diagnostics. The typed model (`GhosttyConfig`, `model.rs`) carries only a subset of the keys. The keys below reach the
"ghostty config key not supported; ignored" arms of `GhosttyConfig::apply` (logged at debug level) and change nothing.
A user who sets one gets Ghostty's default behavior, or ngterm's own behavior where ngterm owns the feature.

148 keys are in that state. The list comes from the key table itself: each key in `known_keys()` was applied to a
default `GhosttyConfig`, and the keys the model refused were collected. Three more keys miss the model but are read
elsewhere, so they are not listed: `keybind` (`binds.rs`), `config-file` (the loader's includes) and `command`
(`ngterm-gpui` `session_host.rs`, a pane's default program).

## How the GTK build was judged

The removed GTK build (last at `f3c0f93`, `crates/agterm-gtk`, `crates/ngterm-ghostty`) embedded the full libghostty
at Ghostty `683d8db` through its embedded apprt (`apprt/embedded.zig`). It did not use Ghostty's own GTK app runtime.
A key's consumer in Ghostty's source at `683d8db` therefore decides whether the GTK build honored it:

- **core** (`Surface.zig`, `termio/Termio.zig`, `renderer/generic.zig`, `renderer/OpenGL.zig`, `renderer/Thread.zig`,
  `font/SharedGridSet.zig`): libghostty applied it to every surface, so the GTK build honored it on Linux, unless
  the code is gated to macOS.
- **GTK apprt** (`apprt/gtk/...`) or Ghostty's macOS app: Ghostty's own app runtimes, which the GTK build did not run.
  The key had no effect there, unless `agterm-gtk` read it itself. It read only `font-size`, `background`,
  `foreground`, `palette`, `cursor-opacity`, `cursor-style-blink`, `mouse-hide-while-typing`,
  `shell-integration-features` and `title` (`ngterm-ghostty` `config::keys`), and none of them is listed here.
- **an apprt action from core** (`command_finished`, `mouse_over_link`, `initial_size`, `toggle_fullscreen`,
  `needs_confirm_quit`): it counted only where the GTK build handled that action. `ngterm-ghostty`'s `Action` enum
  handled `Render`, `SetTitle`, `Pwd`, `CellSize`, `DesktopNotification`, `ShowChildExited`, the search actions,
  `ConfigChange`, `MouseShape`, `MouseVisibility`, `OpenUrl`, `ColorChange` and `RingBell`. Every other action
  arrived as `Other` and was dropped.

Upstream macOS agterm also embeds libghostty, so its status matches the GTK build's, except for keys gated to macOS.
Those keys are marked "macOS" below: libghostty honors them on upstream's platform, and they matter to a macOS port of
this build, not to Linux.

| category | keys | status |
|---|---:|---|
| honored by the GTK build | 49 | deferred work (the backlog item) |
| macOS only | 31 | no Linux meaning; revisit with a macOS port |
| Ghostty app-runtime keys the GTK build did not honor | 64 | not a parity gap; ngterm owns the UI they shape |
| not in the GTK build's Ghostty (new at the pin) | 3 | not a parity gap |
| CLI only | 1 | no effect in a config file, in Ghostty too |

## 1. Honored by the GTK build: the deferred work

### Fonts and cell metrics (25)

| key | Ghostty meaning | GTK build |
|---|---|---|
| `adjust-cell-width` | grow or shrink the cell width, in points or a percentage | core: `font/SharedGridSet.zig` metric modifiers |
| `adjust-cell-height` | grow or shrink the cell height; the text stays centered | core: `SharedGridSet.zig` |
| `adjust-font-baseline` | move the text baseline | core: `SharedGridSet.zig` |
| `adjust-underline-position` | move the underline | core: `SharedGridSet.zig` |
| `adjust-underline-thickness` | change the underline thickness | core: `SharedGridSet.zig` |
| `adjust-strikethrough-position` | move the strikethrough | core: `SharedGridSet.zig` |
| `adjust-strikethrough-thickness` | change the strikethrough thickness | core: `SharedGridSet.zig` |
| `adjust-overline-position` | move the overline | core: `SharedGridSet.zig` |
| `adjust-overline-thickness` | change the overline thickness | core: `SharedGridSet.zig` |
| `adjust-cursor-thickness` | change the bar, underline and hollow cursor thickness | core: `SharedGridSet.zig` |
| `adjust-cursor-height` | change the cursor height | core: `SharedGridSet.zig` |
| `adjust-box-thickness` | change the thickness of drawn box-drawing lines | core: `SharedGridSet.zig` (sprite metrics) |
| `adjust-icon-height` | change the maximum height of Nerd Font icons | core: `SharedGridSet.zig` |
| `font-style` | named style (or `false`) of the regular face of `font-family` | core: `SharedGridSet.zig` |
| `font-style-bold` | named style, or `false` to disable bold | core: `SharedGridSet.zig`, `renderer/generic.zig` |
| `font-style-italic` | named style, or `false` to disable italic | core: `SharedGridSet.zig`, `renderer/generic.zig` |
| `font-style-bold-italic` | named style, or `false` to disable bold italic | core: `SharedGridSet.zig`, `renderer/generic.zig` |
| `font-synthetic-style` | synthesize bold/italic when the family lacks the face | core: `SharedGridSet.zig` |
| `font-variation` | OpenType variation axes (`wght=500`) of the regular face | core: `SharedGridSet.zig` |
| `font-variation-bold` | variation axes of the bold face | core: `SharedGridSet.zig` |
| `font-variation-italic` | variation axes of the italic face | core: `SharedGridSet.zig` |
| `font-variation-bold-italic` | variation axes of the bold-italic face | core: `SharedGridSet.zig` |
| `font-codepoint-map` | a font family for a codepoint range | core: `SharedGridSet.zig` |
| `font-shaping-break` | where shaping runs break (`cursor`) | core: `renderer/generic.zig` |
| `freetype-load-flags` | FreeType hinting, autohint, monochrome and light flags | core: `SharedGridSet.zig` (FreeType builds, that is Linux) |

### Colors and rendering (13)

| key | Ghostty meaning | GTK build |
|---|---|---|
| `bold-color` | the color of bold text: a color, or `bright` for the bright palette entry | core: `renderer/generic.zig` |
| `faint-opacity` | the opacity of faint (SGR 2) text | core: `renderer/generic.zig` |
| `minimum-contrast` | the lowest contrast ratio between text and its background; the text color moves to reach it | core: `renderer/generic.zig` |
| `alpha-blending` | color space of alpha blending (`native`, `linear`, `linear-corrected`) | core: `renderer/generic.zig`, `renderer/OpenGL.zig` (sRGB framebuffer) |
| `background-opacity-cells` | apply `background-opacity` to cells with an explicit background too | core: `renderer/generic.zig` |
| `palette-generate` | derive palette entries 16-255 from the base 16 colors | core: `termio/Termio.zig` |
| `palette-harmonious` | generate the palette so it follows the theme's light/dark direction | core: `termio/Termio.zig` |
| `search-foreground` | the text color of search matches | core: `renderer/generic.zig`; the GTK build ran libghostty's search (`StartSearch`, `SearchTotal`, `SearchSelected`) |
| `search-background` | the background of search matches | core: `renderer/generic.zig` |
| `search-selected-foreground` | the text color of the focused match | core: `renderer/generic.zig` |
| `search-selected-background` | the background of the focused match | core: `renderer/generic.zig` |
| `custom-shader` | GLSL post-processing shaders (Shadertoy-style) over the terminal | core: `renderer/generic.zig` on OpenGL. The plan's Solution Overview says "`custom-shader`: not supported" for the GPUI build |
| `custom-shader-animation` | keep redrawing while a custom shader runs | core: `renderer/generic.zig` |

### Terminal protocol responses (4)

| key | Ghostty meaning | GTK build |
|---|---|---|
| `enquiry-response` | the reply to ENQ (`0x05`) | core: `termio/Termio.zig` |
| `osc-color-report-format` | the format of OSC 4/10/11 color reports (`none`, `8-bit`, `16-bit`) | core: `termio/Termio.zig` |
| `title-report` | whether CSI 21 t may report the title | core: `Surface.zig` |
| `vt-kam-allowed` | whether a program may lock the keyboard (KAM, mode 2) | core: `Surface.zig` |

### Behavior (7)

| key | Ghostty meaning | GTK build |
|---|---|---|
| `scroll-to-bottom` | jump to the bottom on a keystroke and/or on output | core: `Surface.zig`, `renderer/generic.zig` |
| `scrollback-compression` | compress idle scrollback pages in memory | core: `renderer/Thread.zig` |
| `link` | user-defined link patterns and what clicking them does | core: `Surface.zig`, `renderer/generic.zig` |
| `link-osc8` | whether OSC 8 hyperlinks are clickable | core: `Surface.zig` |
| `key-remap` | remap modifiers (`ctrl=super`) before keys reach the terminal | core: `Surface.zig` |
| `desktop-notifications` | whether programs may post desktop notifications (OSC 9, OSC 777) | core: `Surface.zig` gates the `desktop_notification` action, which the GTK build handled (`DesktopNotification`) |
| `initial-command` | the command of the first surface the app creates, over `command` | core: `Surface.zig` (`app.first`); the embedder's command did not win over it |

## 2. macOS only (31)

These keys are gated to macOS in libghostty or read only by Ghostty's macOS app. The GTK build could not honor them;
upstream macOS agterm gets the libghostty-gated ones (`macos-option-as-alt`, `font-thicken*`, `window-colorspace`,
`window-vsync`, `background-blur`) from libghostty.

| key | Ghostty meaning | evidence |
|---|---|---|
| `macos-option-as-alt` | treat Option as Alt | `Surface.zig` passes it to the key encoder, which applies it on macOS |
| `macos-non-native-fullscreen` | fullscreen without a new Space | `Surface.zig` → `toggle_fullscreen`, macOS app |
| `macos-titlebar-style` | native, transparent, tabs or hidden titlebar | macOS app |
| `macos-titlebar-proxy-icon` | show the proxy icon in the titlebar | macOS app |
| `macos-window-buttons` | show the traffic-light buttons | macOS app |
| `macos-window-shadow` | the window shadow | macOS app |
| `macos-auto-secure-input` | enable Secure Input at a password prompt | macOS app |
| `macos-secure-input-indication` | show that Secure Input is on | macOS app |
| `macos-applescript` | expose the AppleScript dictionary | macOS app |
| `macos-shortcuts` | let macOS Shortcuts control the app | macOS app |
| `macos-dock-drop-behavior` | what a drop on the dock icon opens | macOS app |
| `macos-hidden` | hide the app from the dock and the switcher | macOS app |
| `macos-icon` | the app icon | macOS app |
| `macos-custom-icon` | a custom app icon file | macOS app |
| `macos-icon-frame` | the material of the icon's frame | macOS app |
| `macos-icon-ghost-color` | the color of the ghost in the icon | macOS app |
| `macos-icon-screen-color` | the color of the screen in the icon | macOS app |
| `font-thicken` | draw glyphs thicker | `renderer/generic.zig` passes it to CoreText; `font/face/freetype.zig` has no thicken ("currently only supported on macOS") |
| `font-thicken-strength` | how much thicker | as `font-thicken` |
| `window-colorspace` | sRGB or Display P3 | `renderer/generic.zig` (`use_display_p3`, Metal); "only supported on macOS" |
| `window-vsync` | draw on the display link | `renderer/generic.zig` `syncDisplayLink` (CoreVideo); "only supported currently on macOS" |
| `background-blur` | blur behind a translucent background | `renderer/generic.zig` (`comptime builtin.os.tag == .macos`), `apprt/embedded.zig` (`ghostty_set_window_background_blur`). On Linux both builds blur from ngterm's own Settings (`background_blur`) |
| `auto-update` | check for updates | macOS app (Sparkle) |
| `auto-update-channel` | stable or tip updates | macOS app |
| `undo-timeout` | how long a closed window can be undone | macOS app ("Linux doesn't support undo operations") |
| `window-position-x` | the first window's x | macOS app ("only supported on macOS") |
| `window-position-y` | the first window's y | macOS app |
| `window-save-state` | restore windows at launch | macOS app; ngterm restores its own state (`windows.json`) |
| `window-step-resize` | resize in cell steps | macOS app |
| `quick-terminal-animation-duration` | the quick terminal's slide animation | macOS app ("only implemented on macOS") |
| `quick-terminal-space-behavior` | the quick terminal across Spaces | macOS app |

## 3. Ghostty app-runtime keys the GTK build did not honor (64)

Ghostty's own GTK app runtime (or both of its runtimes) reads these keys. The GTK build ran the embedded runtime, and
`agterm-gtk` did not read them, so setting them changed nothing there either. Several of them shape UI that ngterm
draws itself, after upstream: windows, titlebar, sidebar, splits, quick terminal, palette, notifications.

| key | Ghostty meaning | evidence (Ghostty `683d8db`) |
|---|---|---|
| `gtk-custom-css` | extra GTK CSS | `apprt/gtk/class/application.zig` |
| `gtk-horizontal-tab-scroll` | scroll the tab bar horizontally | `apprt/gtk/class/surface.zig` |
| `gtk-opengl-debug` | GTK's OpenGL debug log | `apprt/gtk/class/application.zig` |
| `gtk-quick-terminal-layer` | layer-shell layer of the quick terminal | Ghostty GTK runtime (Wayland) |
| `gtk-quick-terminal-namespace` | layer-shell namespace of the quick terminal | Ghostty GTK runtime (Wayland) |
| `gtk-single-instance` | Ghostty's single-instance mode | `apprt/gtk/class/application.zig`; ngterm has its own single instance |
| `gtk-tabs-location` | the tab bar's side | `apprt/gtk/class/window.zig` |
| `gtk-titlebar` | draw the GTK titlebar | `apprt/gtk/class/window.zig` |
| `gtk-titlebar-hide-when-maximized` | hide it when maximized | `apprt/gtk/class/window.zig` |
| `gtk-titlebar-style` | native or tabs titlebar | `apprt/gtk/class/window.zig` |
| `gtk-toolbar-style` | flat, raised or raised-border toolbar | `apprt/gtk/class/window.zig` |
| `gtk-wide-tabs` | wide tabs | `apprt/gtk/class/window.zig` |
| `class` | the GTK application id | `apprt/gtk/class/application.zig`; ngterm keeps the app id `com.nickalie.ngterm` |
| `x11-instance-name` | the X11 WM_CLASS instance | Ghostty GTK runtime |
| `language` | the UI language | `apprt/gtk/class/application.zig` |
| `async-backend` | the event loop backend (epoll, io_uring) | `apprt/gtk/class/application.zig` |
| `linux-cgroup` | run each surface in its own cgroup | `os/cgroup.zig`, called by the GTK runtime only |
| `linux-cgroup-memory-limit` | the surface cgroup's memory limit | GTK runtime (cgroup) |
| `linux-cgroup-processes-limit` | the surface cgroup's process limit | GTK runtime (cgroup) |
| `linux-cgroup-hard-fail` | fail when the cgroup cannot be set up | GTK runtime (cgroup) |
| `app-notifications` | in-app toasts (copied to clipboard, reloaded config) | `apprt/gtk/class/window.zig` |
| `bell-features` | what the bell does (system sound, audio file, attention, title, border) | `apprt/gtk/class/{application,surface,window}.zig`; the GTK build handled `RingBell` its own way |
| `bell-audio-path` | the bell's audio file | `apprt/gtk/class/application.zig` |
| `bell-audio-volume` | the bell's volume | `apprt/gtk/class/application.zig` |
| `progress-style` | show OSC 9;4 progress as a bar | `apprt/gtk/class/surface.zig`; ngterm draws its own progress |
| `resize-overlay` | show the grid size while resizing | `apprt/gtk/class/surface.zig` |
| `resize-overlay-position` | where it shows | `apprt/gtk/class/surface.zig` |
| `resize-overlay-duration` | how long it shows | `apprt/gtk/class/surface.zig` |
| `scrollbar` | show a scrollbar | Ghostty app runtimes |
| `drag-handle` | the split drag handle | `apprt/gtk/class/surface.zig` |
| `split-divider-color` | the split divider's color | `apprt/gtk/class/application.zig` |
| `unfocused-split-opacity` | dim unfocused splits | `apprt/gtk/class/application.zig`, macOS app (`config/CApi.zig`) |
| `unfocused-split-fill` | the dimming color | as above |
| `split-preserve-zoom` | keep a zoomed split zoomed on navigation | Ghostty app runtimes |
| `split-inherit-working-directory` | a new split starts in its parent's directory | `apprt/surface.zig` `newConfig`; the GTK build passed every pane's directory itself |
| `tab-inherit-working-directory` | a new tab starts in its parent's directory | as above |
| `window-inherit-working-directory` | a new window starts in its parent's directory | as above |
| `window-inherit-font-size` | a new surface takes its parent's font size | `apprt/embedded.zig` with a parent surface; the GTK build created none (session zoom is ngterm's) |
| `window-decoration` | client, server or no decorations | `apprt/gtk/class/window.zig`; ngterm draws its own chrome |
| `window-theme` | the window's light/dark theme | `apprt/gtk/class/{application,window}.zig` |
| `window-title-font-family` | the titlebar font | `apprt/gtk/class/application.zig` |
| `window-titlebar-background` | the titlebar background (`window-theme = ghostty`) | `apprt/gtk/class/application.zig` |
| `window-titlebar-foreground` | the titlebar foreground | `apprt/gtk/class/application.zig` |
| `window-subtitle` | a subtitle under the title | `apprt/gtk/class/window.zig` |
| `window-show-tab-bar` | when the tab bar shows | `apprt/gtk/class/window.zig` |
| `window-new-tab-position` | where a new tab goes | `apprt/gtk/class/window.zig` |
| `window-width` | the first window's width in cells | `Surface.zig` → `initial_size` action, which the GTK build dropped (`Other`) |
| `window-height` | the first window's height in cells | as `window-width` |
| `maximize` | start maximized | `apprt/gtk/class/window.zig` |
| `fullscreen` | start fullscreen | `Surface.zig` → `toggle_fullscreen` action (dropped), `apprt/gtk/class/window.zig` |
| `initial-window` | open a window at launch | `apprt/gtk/class/application.zig`; ngterm restores its windows |
| `quit-after-last-window-closed` | quit when the last window closes | `apprt/gtk/class/application.zig`; ngterm owns its lifecycle |
| `quit-after-last-window-closed-delay` | wait before quitting | as above |
| `quick-terminal-position` | the quick terminal's edge | Ghostty app runtimes; ngterm's quick terminal has its own placement |
| `quick-terminal-size` | its size | Ghostty app runtimes |
| `quick-terminal-screen` | its monitor | Ghostty app runtimes |
| `quick-terminal-autohide` | hide it on focus loss | `apprt/gtk/class/window.zig` |
| `quick-terminal-keyboard-interactivity` | its layer-shell keyboard mode | Ghostty GTK runtime (Wayland) |
| `command-palette-entry` | entries of Ghostty's command palette | Ghostty app runtimes; ngterm has its own palette |
| `confirm-close-surface` | confirm closing a surface whose process runs | `Surface.zig` → `needs_confirm_quit` and the close callback's flag; the GTK build used neither (ngterm's own quit rule, `QuitReason::needs_confirmation`) |
| `notify-on-command-finish` | notify when a long command finishes | `Surface.zig` → `command_finished` action, which the GTK build dropped |
| `notify-on-command-finish-action` | bell and/or notification | as above |
| `notify-on-command-finish-after` | the minimum command duration | as above |
| `link-previews` | show a hovered link's target | `Surface.zig` → `mouse_over_link` action, which the GTK build dropped |

## 4. Not in the GTK build's Ghostty (3)

Ghostty added these keys after `683d8db`. At `LIBGHOSTTY_VT_REV` they are core features (`Surface.zig`,
`termio/Termio.zig`), but neither the GTK build nor upstream v0.36.0 (also `683d8db`) had them.

| key | Ghostty meaning |
|---|---|
| `vt-window-resize-allowed` | whether a program may resize the window (CSI 8 t) |
| `vt-xt-checksum-report` | whether DECRQCRA (rectangle checksum) is answered |
| `vt-xt-checksum-extension` | XTerm's checksum extension flags |

## 5. CLI only (1)

| key | Ghostty meaning |
|---|---|
| `config-default-files` | load the default config files; "can only be set via CLI arguments", so a config line has no effect in Ghostty either |
