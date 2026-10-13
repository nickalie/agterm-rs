# Linux UI checklist

Every user-visible feature of upstream macOS agterm (v0.36.0), with the form it takes in the GTK4/libadwaita build and
a manual check against upstream's behavior. The suite has no Linux UI driver (`tests/ui` skips on Linux), so this list
is how the UI is verified: Task 36 walks it and fixes or records every gap with ⚠️.

Sources, all under `.upstream/agterm`: `site/docs.html` (the user guide), `.claude/rules/*.md` (per-subsystem
contracts), `agterm/Views/*`, `agterm/agtermApp+Menus.swift`, and `conformance/ui-ids.yaml` (the accessibility ids
upstream exposes). The fork's UI (`../agterm-linux`) is only a hint for the GTK form.

How to read it:

- **GTK form**: the widget or idiom standing in for the AppKit/SwiftUI one. Where a control carries one of upstream's
  accessibility ids, the GTK widget's name is that id (`gtk_widget_get_name`), so a future UI driver can find it.
- **Check**: what to do and what upstream does, which the GTK build must match.
- **Built in**: the task that builds it; a later task's number means not built yet at the time of writing.
- Launch for a walk with `scripts/run-dev.sh` (never by hand while the installed fork runs).

## App shell, menus, first run and quit

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| App menu: File, View, Navigate with live enablement and the keymap's chords as accelerators (`agtermApp+Menus.swift`, `menu-actions.md`) | the header bar's primary menu (`gio::Menu`), one submenu per upstream menu, items as `menu.*` actions | every item enables/disables as upstream's for the frontmost window; a chord rebound in `keymap.conf` shows on its item after Reload Keymap | 24 |
| Settings… (⌘,) and Quit (⌘Q) in the app menu | Preferences and Quit closing the primary menu; no accelerator (no keymap action, and ⌘ chords belong to the desktop on Linux) | Preferences opens the Settings dialog over the frontmost window, a second open raises it; Quit asks as below | 33 |
| About, Help-menu installers, Reset Live Sessions… | primary-menu items | About shows the version and commit; the installers as in Integrations below | 36, 28 |
| First-run welcome (`FirstRunWelcome.swift`, `WelcomeAlert.swift`; ids `welcome-install`, `welcome-later`, `welcome-skill-checkbox`, `welcome-hooks-checkbox`) | `adw::AlertDialog` "Welcome to agterm" over the first window, upstream's text, two checked `GtkCheckButton`s (named with upstream's ids), responses Later / Install (suggested) | on an empty state dir (`AGTERM_STATE_DIR=$(mktemp -d)`) it shows once after the window; `settings.json` has `"welcomeShown" : true` before any installer runs; never shown with `settings.json`, `workspaces.json` or `windows/` already there; never on a UI-test launch unless `AGTERM_UITEST_SHOW_WELCOME` is set; Install runs the checked installers | 33 (installers: 36) |
| Quit confirmation (`QuitPrompt.swift`, `AppDelegate.applicationShouldTerminate`) | `adw::AlertDialog` "Quit Agterm?", Cancel / Quit (default, destructive) | menu Quit with windows open asks, naming "N windows and M sessions" and "ending all running shells" unless the launch runs live sessions; SIGTERM (logout, shutdown) and closing the last window quit without asking; a UI-test launch never asks | 33 |
| Launch forwarding: a second launch raises the running instance | GApplication single instance | launching again raises the frontmost window | 7 |

## Settings (`SettingsView.swift`, `SettingsModel.swift`, `settings.md`)

One `adw::PreferencesDialog`, searchable, upstream's six tabs as pages in upstream's order, opening on General. Every
edit writes `settings.json` at once (a control's default value is stored as nil, except the font size, stored
explicitly so it beats `ghostty.conf`) and applies live; a change made
elsewhere (`theme.set`, `sidebar.flagged-layout`, `browser.links`) shows in the open dialog.

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| General ▸ Mouse: scroll speed 1–10 (`settings-scroll-speed`), right-click pastes, open links in Browser / Session overlay, click a workspace row to expand or collapse | slider row with an `Nx` readout, switch rows, combo row | the scroll multiplier changes in the terminals at once; right-click off makes a right click do nothing; links follow the mode; the row click toggles only while on | 33 |
| General ▸ Sessions: new sessions open in Home / Current session's directory / Custom directory, with Choose… for the custom one ("Not set" until picked); new sessions are added at the end / after the current; restore mode with "Changes apply after restarting agterm." and the live refusal reason; confirm before closing a session; allow undo after closing; flagged view layout; use my global Ghostty config | combo rows, an action row with a `GtkFileDialog` folder chooser (shown only in Custom), switch rows | the next New Session follows the directory and placement; the restore mode reads back on `agtermctl restore mode` as configured, active unchanged; the flagged layout re-lays every window's flagged view; the global toggle reloads the config with `~/.config/ghostty/config` (or `$XDG_CONFIG_HOME/ghostty/config`) underneath the scoped one | 33 |
| Appearance ▸ Terminal: font (Default + the system's monospaced families), size 8–32, cursor shape (Default/Block/Bar/Underline), cursor blink (Default/Blink/Steady), theme (ghostty's built-in only while not following), follow system appearance, the other appearance's theme while following | combo rows (searchable past ten entries), spin row, switch row; the alternate picker titled "Dark theme" or "Light theme" with "Used when the desktop is in … mode." | each change re-renders every terminal at once and clears per-session zoom only when the generated `ghostty-settings.conf` changed; following picks the side with the desktop's style; turning following off keeps the theme on screen | 33 |
| Appearance ▸ Window: toolbar Normal/Compact/Hidden, background opacity and blur (blur disabled at 100%), sidebar tint ("None", "Lighter n", "Darker n"), sidebar font size 9–20, palette and switcher font size 9–20, inactive pane and backdrop mute 0–10 | combo row, slider rows with readouts, spin rows | the sidebar text size changes at once; ⚠️ the GTK window draws no translucency, the titlebar modes and the tint/mute washes are rendered by Tasks 35 and later: check that each persists and is picked up once its renderer exists | 33 (renderers: 35) |
| Interface: a toggle per title-bar element and per sidebar element (`settings-interface-<raw>`), workspace name and custom commands off by default; show sidebar only in the active window; quick terminal size Default / 40–90% of screen | one switch row per element (a scrolling page needs no two-per-row layout), switch row, combo row | hiding a sidebar footer button removes it at once; auto-hide on hides every inactive window's sidebar at once; the quick terminal opens at the picked share | 33 |
| Notifications: banners, badges, bounce Dock icon (None/Once/Until focused), notification sound (None + upstream's names, previewed), attention indicator | switch rows, combo rows | banners off stops desktop notifications while badges keep counting; badges off hides the sidebar pills; ⚠️ Linux has no Dock bounce or named system sounds: a sound previews and plays as the display's alert | 33 |
| Agent Status: Active / Blocked / Completed rows with color and shape, blocked sound, status reset (On first key / On Enter / Disabled), auto-follow (Disabled, 5s … 5m) and "auto-follow away from a running session" (disabled while off), Reset to defaults | per status one combo row (the shape) titled with the status and a `GtkColorDialogButton` (`settings-status-<status>`), combo rows, switch row, a button | a picked color or shape repaints the sidebar glyphs at once; Reset clears colors, shapes, the blocked sound and the status reset, not auto-follow | 33 |
| Key Mapping: the config directory (selectable path, Choose…, Use Default while set; "The directory holding keymap.conf and hooks.conf. Changing it reloads both."), keymap diagnostics with Reload, hooks diagnostics with Reload | an action row with two buttons, a monospace selectable label per file ("No issues." when clean) with a Reload in the group header | choosing a directory reloads the keymap, the hooks and the ghostty config from it; a broken line in `keymap.conf` shows as `line N: …` after Reload | 33 |

## Windows (`windows.md`)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Several windows, each its own tree; New / Rename / Delete Window | `adw::ApplicationWindow` per open window; rename and delete as `adw::AlertDialog`s | the windows open at quit reopen; Delete Window confirms while it holds sessions, naming the undo grace | 11, 14, 24 |
| Window title follows the active session | the header bar title | renaming a session retitles its window | 11 |
| Full screen | View ▸ Toggle Full Screen (F11) | toggles the window's fullscreen | 24 |
| Window frames restored | ⚠️ Wayland gives a client no position: size only | the size comes back after a restart | 14 |

## Sidebar (`sidebar.md`, `WorkspaceSidebar*.swift`)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Workspace and session rows with kind icons, status glyphs, unseen badges (`workspace-row`, `session-row`, `workspace-icon`, `session-icon`, `agent-status`, `notify-badge`) | list rows built once and updated in place | glyph colors and shapes follow the settings and `session.status` overrides; a badge rolls up onto a collapsed workspace | 22 |
| Inline rename (`edit-field`) by double-click | an entry replacing the row's label | Enter commits, Esc reverts, an empty name reverts a session to its directory name | 22 |
| Context menus (rename, copy name, duplicate, flag, move, close, clear status, reveal in the file manager) | `GtkPopoverMenu` per row | right-clicking inside a multi-selection acts on the whole selection, outside it narrows to the row; Reveal is disabled for a missing directory | 22, 35 |
| Drag-and-drop reorder and moves; folders dropped from the file manager open sessions | GTK drag sources and drop targets | a dragged session keeps running; up to 20 folders open sessions in the row's workspace; collapsed workspaces spring open while hovering | 22 |
| Flagged view (flat / plain / tree) and the workspace focus filter | footer toggles (`flagged-view-toggle`, `focus-filter-toggle`) | as `docs.html` > Workspaces & sessions | 22 |
| Footer: New Workspace, add-session menu (`add-session`), per-workspace "+" on hover (`workspace-add-session`) | footer buttons, a row suffix button | each hides with its Interface toggle | 22 |
| Sidebar width and visibility | a draggable divider; View ▸ Hide Sidebar | width persists per window and clamps at the floor | 22 |

## Title bar (`WindowContentView+Titlebar.swift`, `TitlebarComposition.swift`)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Identity: workspace — session — window names, remote host, session context; toolbar modes | header bar widgets | each element hides with its Interface toggle; Normal adds the working directory; Hidden leaves a drag strip | 35 |
| Buttons: sidebar toggle, recent sessions, scratch, split, dashboard, quick terminal, custom commands, attention (`sidebar-toggle-button`, `recent-sessions-button`, `scratch-toggle`, `split-toggle`, `dashboard-toggle-button`, `quick-terminal-toggle`, `custom-commands-button`, `attention-button`) | header bar buttons and popovers (`recent-session-row`, `attention-session-row`, `custom-command-row`) | each runs its action; the attention bell is dim / plain / amber as upstream's | 35 |

## Terminals (`docs.html` > Terminals, `libghostty.md`)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Split ⌘D / ⌘⇧D (Ctrl+Shift+D / H here), transpose, hide, divider drag and double-click reset, swap panes | a pane stack with a divider | the other shortcut transposes, the same one hides; the ratio persists | 16 |
| Scratch per session | a pane over the session's page | hides without killing; opens in the session's directory | 16 |
| Quick terminal (Ctrl+`) | a floating layer-shell panel | opens at the size setting on the focused monitor in the active session's directory; click-away dismisses unless opened by `quick show` | 17, 26.1 |
| In-terminal search (`search-field`, `search-counter`) | a search bar over the terminal | "N of M", Enter / Shift-Enter step, Esc closes | 34 |
| Terminal zoom (`terminal-zoom`, `terminal-zoom-exit`) | the window filled by one surface with a slim exit strip | exit restores split ratios, focus and visibility | 17 |
| Font zoom ⌘+ ⌘− ⌘0 (Ctrl+= / − / 0) | libghostty bindings | per-session size persists; Reload Config resets it | 23 |
| Live theme picker (View ▸ Select Theme…) | the palette with live preview | Enter commits and syncs Settings, Esc reverts | 34 |
| Links: ⌘-click opens in the browser or a session overlay | Ctrl-click | follows Settings ▸ General ▸ Open links in | 21, 25 |
| Clipboard prompts for OSC 52 | `adw::AlertDialog` | as upstream's clipboard prompt | 15 |

## Overlays, dialogs and the dashboard

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Program overlay and pane overlay | a framed layer over the page or the pane | the session is unchanged when the program exits | 20 |
| HUD message panel | a passive terminal layer | the session stays typable underneath | 18 |
| HTML overlay with the identity strip and navigation (`htmlOverlay.*`) | WebKitGTK view under a strip | back/forward/reload/open in browser/show in file manager or copy link/close; zoom steps persist | 21 |
| Question dialog (`ask-dialog`, `ask-title`, `ask-message`, `ask-button-<choice>`) | a panel over the page or window | keyboard navigation and answers as upstream's | 19 |
| Native picker (`pick-palette`, `pick-scrim`) | a palette panel over a scrim | typing filters, Esc cancels | 19 |
| Dashboard (`dashboard`, `dashboard-cell`, `dashboard-highlighted`, `dashboard-exit`, `dashboard-overlay-cover*`) | the deck laid out as a grid under a chrome layer | arrows move the highlight, Enter or a click jumps to that pane, Esc closes; covered panes show their label | 17 |

## Navigation (`docs.html` > Navigation)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Session switcher (Ctrl+P), action palette (Ctrl+Shift+P), custom-commands palette (`command-palette`, `palette-item-<item>`, `palette-subtitle`, `palette-badge`) | palette panels | disabled actions stay listed greyed out; interface font size applies | 34 |
| Ctrl-Tab MRU switcher (`session-switcher`) | a switcher panel while Ctrl is held | a single tap flips back; the order survives a relaunch | 34 |
| Previous/next session and attention session, first/last, workspaces, windows, panes | menu items with the Linux chords | as upstream's | 24 |
| Dock menu (recent sessions, attention, New Session/Window) | ⚠️ no Dock on Linux; a desktop-file action list is the nearest form | record the exception | — |

## Notifications and agent status (`notifications.md`, `docs.html` > Agent status)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| OSC 9/777 and `notify` banners; a click focuses the exact pane | `GNotification` with a default action | the banner's click raises the window and focuses the pane; focusing the session clears its badge | 18 |
| Status glyphs, blink, auto-reset, per-pane block | sidebar row glyphs (cairo) | as `docs.html` > Agent status | 22 |
| Blocked sound and `--sound` | the display's alert beep | ⚠️ no named sounds on Linux | 18 |
| Auto-follow blocked sessions | core timers | follows after the idle timeout; never leaves a running session when the toggle says so | 17 |

## Keymap, hooks and custom commands (`keymap.md`)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| `keymap.conf` rebinding built-ins and custom commands, with a commented starter | the capture-phase key monitor and menu accelerators | a rebound chord works and shows in the menu after Reload Keymap; diagnostics post a banner and list in Settings ▸ Key Mapping | 24 |
| Edit Keymap / Hooks / ghostty.conf | an editor overlay reloading on exit when changed | as upstream's | 24 |
| `global-hotkey` | ⚠️ not registered: Wayland has no client-side global hotkey | record the exception | 24 |

## Integrations (`control-api.md` > Installation)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| Install the agent skill, the agent status hooks, the command line tool | Help-menu items and the welcome's Install, Linux install paths | each reports its result; a second run is idempotent | 36 |

## Accessibility (`docs.html` > Accessibility)

| Feature | GTK form | Check | Built in |
|---|---|---|---|
| The terminal advertises an editable text area for dictation tools | GTK accessible role on the terminal widget | an input method or dictation tool commits text at the prompt | 6 |
| Reduce Motion / Reduce Transparency | ⚠️ GTK's `gtk-enable-animations` stands in for Reduce Motion; no Reduce Transparency setting exists | blinking glyphs stop with animations off | 35 |
