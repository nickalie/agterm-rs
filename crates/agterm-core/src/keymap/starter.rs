//! The commented starter `keymap.conf` and the editor command the Edit Keymap overlay runs (upstream's
//! `ConfigPaths.starterKeymapConf` and `ConfigPaths.editorCommand`), with the macOS-only wording made neutral and
//! `global-hotkey` documented as the Linux build registers it (an X11 key grab, else the desktop's global shortcuts
//! portal), and what to do where neither exists.

use std::fmt::Write as _;

use super::BuiltinAction;
use super::command::CommandContext;
use super::defaults::ShippedDefaults;
use super::keybind::{Chord, parse_keybind};

/// The file's head: the three verbs, their rules and examples.
const STARTER_HEAD: &str = r#"# agterm keymap — a kitty-flavored config for rebinding built-in shortcuts and defining
# custom shell commands. Edit this file and run File ▸ Reload Keymap (or `ngtermctl keymap
# reload`) to apply. Blank lines and lines starting with `#` are ignored. A line that is
# rejected or skipped is reported in Settings ▸ Key Mapping and by `ngtermctl keymap list`.
#
# Three verbs:
#
#   map <chord> [--repeat] <action>
#       Rebind a built-in action. Chords use kitty syntax: mods joined by `+`, e.g.
#       `cmd+shift+l`, `ctrl+\``. Mods: ctrl, cmd, opt, shift. A Shift-typed symbol is
#       shift+<base key> (shift+/ for ?, shift+= for +, shift+5 for %). Several
#       alternatives may be joined by `|` with no spaces around it; the first single-chord
#       alternative becomes the menu shortcut and the rest fire through a key monitor, so
#       their first chord needs a modifier or a function key (f1 through f20). A line offering
#       no single chord leaves the action with no menu shortcut at all. `--repeat` keeps a
#       leader's prefix live until half a second after its key is released, so its last key
#       repeats on its own (tmux `bind -r`): `ctrl+a>ctrl+l` then `ctrl+l` fires twice. Examples:
#
#           map cmd+shift+l     toggle_split
#           map cmd+t|ctrl+a>t  toggle_scratch
#
#   command "<name>" [chord] [--repeat] <shell...>
#       Define a custom command, shown in the action palette marked `custom`. The quoted
#       name may contain spaces. An optional chord (single chord OR a leader like `ctrl+a>g`,
#       or several of either joined by `|`) binds it to a key; every alternative must start with
#       a modifier or a function key (f1 through f20). Others are dropped, and a line left with
#       none becomes palette-only. Omit the chord for a palette-only command. The rest of the
#       line is run via `/bin/sh -c`, detached with no terminal — so it suits fire-and-forget
#       launches (GUI apps, scripts), NOT a bare interactive or full-screen TUI program, which
#       has no TTY and exits at once. Launch a TUI over a session through an overlay terminal,
#       as the Lazygit example does. The line resolves binaries against the app's own `PATH`
#       plus the directory of the bundled ngtermctl, so a bare ngtermctl works; anything else
#       your shell profile adds may not, so give it an absolute path or wrap the line in
#       `$SHELL -lc '...'` — `$SHELL -ilc '...'` when that PATH comes from an interactive rc
#       file, which -lc does not read. The program an overlay or scratch terminal runs gets the
#       app's own unwidened PATH and always needs one of those.
#       Examples:
#
#           command "Open folder"  cmd+shift+e  xdg-open "$AGT_SESSION_PWD"
#           command "Lazygit"      ctrl+a>g     ngtermctl session overlay open 'sh -lc lazygit' --target "$AGT_SESSION_ID" --socket "$AGT_SOCKET"
#           command "Deploy"                    ./deploy.sh
#
#   global-hotkey <chord>
#       Bind ONE system-wide chord that summons the quick terminal while any application has
#       the keyboard — the only binding here that fires when agterm does not. Exactly one
#       chord: no `|` alternatives, no leader sequence. A modifier is required except for
#       function keys (f1 through f20). A second global-hotkey line replaces the first.
#       agterm registers it: on X11 as a key grab by physical key position, so it works on a
#       non-Latin layout; on Wayland through the desktop's global shortcuts portal (KDE
#       Plasma, GNOME 48 and later, Hyprland), which gets the chord by key name, so whether
#       it survives a layout switch is up to the desktop, and which may ask you to confirm or
#       change it. A Wayland compositor without that portal gives an app no system-wide key
#       at all: there, bind a custom shortcut in the desktop's own settings that runs
#       `ngtermctl quick toggle` instead. The desktop owns the chord, so it takes no
#       part in the collision rules above, and it WINS, agterm included, so a chord shared
#       with a menu action fires this and the menu binding never sees it. There is no
#       default: registering a chord takes it from every other application. Spending it on
#       the chord the in-app binding already uses is how to get ONE shortcut that works
#       everywhere.
#       Examples:
#
#           global-hotkey ctrl+opt+space
#           global-hotkey f5  # takes F5 desktop-wide, including from local map/command bindings
#           global-hotkey ctrl+`            # same chord as the in-app quick_terminal binding
#
# Built-in actions (raw name → shipped default chord):
#
"#;

/// Between the action table and the token list.
const STARTER_TOKENS_HEAD: &str = "#
# Custom-command tokens (expanded in the shell line and exported as $AGT_X env vars):
#
";

/// The file's tail: the raw-substitution warning and the commented first lines.
const STARTER_TAIL: &str = r#"#
# NOTE: a {AGT_X} token is substituted RAW into the /bin/sh line — convenient, but unsafe for
# content you don't control. {AGT_SELECTION} is the obvious case, but a remote host can also set
# the session title (OSC) and the working directory (OSC 7), so {AGT_SESSION_NAME} and
# {AGT_SESSION_PWD} are equally unsafe raw. For any such content prefer the matching $AGT_X
# environment variable, QUOTED, e.g. "$AGT_SELECTION".
#
# Uncomment and edit a line below to start.
# map cmd+shift+l toggle_split
"#;

/// The commented starter `keymap.conf`: the three-verb syntax, every built-in's name with its default chord from
/// `defaults` (`(no default)` for a keyless one, `(not expressible)` for one the grammar cannot spell back), and
/// the `{AGT_X}` tokens. Every line is a comment, so a fresh file rebinds nothing.
#[must_use]
pub fn starter_keymap_conf(defaults: &ShippedDefaults) -> String {
    // pad to the longest name plus a two-space gutter, so a longer future action never truncates.
    let width = BuiltinAction::all()
        .map(|action| action.name().len())
        .max()
        .unwrap_or(0)
        + 2;
    let mut text = STARTER_HEAD.to_owned();
    for action in BuiltinAction::all() {
        let chord = defaults
            .chord(action)
            .map_or_else(|| "(no default)".to_owned(), starter_chord_syntax);
        let _ = writeln!(text, "#   {:<width$}{chord}", action.name());
    }
    text.push_str(STARTER_TOKENS_HEAD);
    for token in CommandContext::token_names() {
        let _ = writeln!(text, "#   {{{token}}}");
    }
    text.push_str(STARTER_TAIL);
    text
}

/// A default chord as the file spells it, or `(not expressible)` when that spelling does not parse back to it
/// (`cmd++`: `+` is the chord joiner).
fn starter_chord_syntax(chord: &Chord) -> String {
    let rendered = chord.display_string();
    if parse_keybind(&rendered).is_some_and(|keybind| keybind == [chord.clone()]) {
        rendered
    } else {
        "(not expressible)".to_owned()
    }
}

/// The shell command that opens `path` in the user's editor (`$VISUAL`, else `$EDITOR`, else `vi`), run under
/// `/bin/sh -c` by the overlay.
///
/// The user's interactive login shell (`$SHELL -ilc`) runs first so it sources its rc and exports
/// `$EDITOR`/`$VISUAL` (the overlay's own `/bin/sh` reads no user config); it then `exec`s a POSIX `/bin/sh` that
/// resolves `${VISUAL:-${EDITOR:-vi}}`, which fish could not parse itself. The POSIX text and the path ride in
/// single quotes both POSIX shells and fish pass verbatim, the path as the inner shell's `$1`, so spaces and quotes
/// survive. `$SHELL` must accept `-ilc` (sh, bash, zsh and fish do; csh and tcsh do not), and a non-exported editor
/// variable falls back to `vi`.
#[must_use]
pub fn editor_command(path: &str) -> String {
    let inner = r#"${VISUAL:-${EDITOR:-vi}} "$1""#;
    let via_posix = format!(
        "exec /bin/sh -c {} agterm-config-edit {}",
        single_quoted(inner),
        single_quoted(path)
    );
    format!("${{SHELL:-/bin/sh}} -ilc {}", single_quoted(&via_posix))
}

/// POSIX single quoting: wrapped in `'…'`, each embedded `'` written `'\''` (fish reads it the same way).
fn single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
