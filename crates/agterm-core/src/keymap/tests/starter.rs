//! Ported from upstream's `ConfigPathsTests` (the starter keymap and the editor command).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use super::*;

use BuiltinAction as A;

#[test]
fn the_starter_keymap_is_commented_and_documents_the_verbs() {
    let starter = starter_keymap_conf(&ShippedDefaults::upstream());
    for expected in [
        "agterm keymap — a kitty-flavored config",
        "map <chord> [--repeat] <action>",
        "command \"<name>\" [chord] [--repeat] <shell...>",
        "single chord OR a leader like `ctrl+a>g`",
        "`cmd+shift+l`, `ctrl+\\``",
        "command \"Open folder\"  cmd+shift+e  xdg-open \"$AGT_SESSION_PWD\"",
        "command \"Lazygit\"      ctrl+a>g     agtermctl session overlay open 'sh -lc lazygit' --target \
         \"$AGT_SESSION_ID\" --socket \"$AGT_SOCKET\"",
        "command \"Deploy\"                    ./deploy.sh",
        "ctrl+shift+p",
        "#   new_session",
        "cmd+n",
        "#   increase_font_size",
        "(not expressible)",
        "#   rename_session",
        "(no default)",
        "a {AGT_X} token is substituted RAW into the /bin/sh line",
        "a remote host can also set",
        "the session title (OSC) and the working directory (OSC 7)",
        "{AGT_SESSION_NAME} and",
        "{AGT_SESSION_PWD} are equally unsafe raw",
        "environment variable, QUOTED, e.g. \"$AGT_SELECTION\"",
    ] {
        assert!(starter.contains(expected), "missing {expected:?}");
    }
    for absent in [
        "super",
        "{AGT_SESSION}",
        "{AGT_WINDOW}",
        "{AGT_CWD}",
        "launchd",
        "homebrew",
        "zsh",
        "Mac",
    ] {
        assert!(!starter.contains(absent), "unexpected {absent:?}");
    }
    for (action, chord) in [
        (A::FocusLeftPane, "cmd+opt+left"),
        (A::FocusRightPane, "cmd+opt+right"),
        (A::PreviousSession, "cmd+opt+up"),
        (A::NextSession, "cmd+opt+down"),
        (A::PreviousAttentionSession, "ctrl+opt+up"),
        (A::NextAttentionSession, "ctrl+opt+down"),
    ] {
        let line = starter
            .lines()
            .find(|line| line.contains(&format!("#   {} ", action.name())));
        assert!(
            line.is_some_and(|line| line.ends_with(chord)),
            "{} should print {chord}",
            action.name()
        );
    }
    assert!(starter.ends_with("# map cmd+shift+l toggle_split\n"));
    // Linux registers no global hotkey: the file says so and offers no global-hotkey line to uncomment.
    assert!(starter.contains("does NOT register it"));
    assert!(
        starter
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
    );
    let (keymap, diagnostics) = parse(&starter);
    assert!(keymap.builtin_overrides().is_empty());
    assert!(keymap.commands().is_empty());
    assert!(diagnostics.is_empty());
}

#[test]
fn the_starter_keymap_lists_every_action_and_token() {
    let starter = starter_keymap_conf(&ShippedDefaults::upstream());
    for action in BuiltinAction::all() {
        assert!(starter.contains(&format!("#   {}", action.name())), "{}", action.name());
    }
    for token in CommandContext::token_names() {
        assert!(starter.contains(&format!("#   {{{token}}}")), "{token}");
    }
}

#[test]
fn the_action_column_is_padded_past_the_longest_name() {
    let starter = starter_keymap_conf(&ShippedDefaults::upstream());
    assert!(starter.contains("#   new_session                 cmd+n\n"));
    assert!(starter.contains("#   previous_attention_session  ctrl+opt+up\n"));
}

#[test]
fn the_action_column_follows_the_given_table() {
    let defaults = ShippedDefaults::from_table([(A::NewSession, chord(CTRL | SHIFT, "t"))], Vec::new());
    let starter = starter_keymap_conf(&defaults);
    assert!(starter.contains("#   new_session                 ctrl+shift+t\n"));
    assert!(starter.contains("#   close_session               (no default)\n"));
}

/// Uncommenting every `map` and `command` example (and any `global-hotkey` one) must parse clean and bind `expected_bound`
/// chords: an example rots when a default claims its chord (issue #405), on either verb.
fn assert_starter_examples_apply(defaults: &ShippedDefaults, expected_bound: usize) {
    let starter = starter_keymap_conf(defaults);
    let examples: Vec<&str> = starter
        .lines()
        .filter_map(|line| {
            let text = line.trim_start_matches(|c: char| c == '#' || c.is_whitespace());
            let verb = text.split_whitespace().next().unwrap_or_default();
            // `<` skips the two verb-syntax lines, which state a grammar rather than an example.
            (matches!(verb, "map" | "command" | "global-hotkey") && !text.contains('<')).then_some(text)
        })
        .collect();
    // three `map`, three `command`, no `global-hotkey` (not registered on Linux): an example dropped from the filter
    // must fail here.
    assert_eq!(examples.len(), 6, "{examples:?}");
    let mut bound = 0;
    for example in examples {
        let (keymap, diagnostics) = parse_keymap(example, defaults);
        assert!(
            diagnostics.is_empty(),
            "starter example '{example}' is skipped: {:?}",
            messages(&diagnostics)
        );
        bound += keymap.builtin_overrides().len()
            + keymap
                .commands()
                .iter()
                .filter(|command| !command.shortcut.is_empty())
                .count()
            + keymap.builtin_sequences().values().map(Vec::len).sum::<usize>()
            + usize::from(keymap.global_hotkey().is_some());
    }
    // every `map` example (the alternatives one twice: menu chord and monitor half), the two chorded commands
    // (`Deploy` is palette-only by design).
    assert_eq!(bound, expected_bound);
}

#[test]
fn the_starter_examples_apply_when_uncommented() {
    assert_starter_examples_apply(&ShippedDefaults::upstream(), 6);
    assert_starter_examples_apply(&linux_defaults(), 6);
}

#[test]
fn the_editor_command_runs_through_an_interactive_login_shell_then_posix_sh() {
    assert_eq!(
        editor_command("/home/test/.config/agterm/keymap.conf"),
        "${SHELL:-/bin/sh} -ilc 'exec /bin/sh -c '\\''${VISUAL:-${EDITOR:-vi}} \"$1\"'\\'' agterm-config-edit \
         '\\''/home/test/.config/agterm/keymap.conf'\\'''"
    );
}

/// An executable at `<dir>/<name>` recording its first argument in `<dir>/<name>.got`.
fn recorder(dir: &Path, name: &str) -> (String, std::path::PathBuf) {
    let got = dir.join(format!("{name}.got"));
    let script = dir.join(name);
    fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", got.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script.display().to_string(), got)
}

/// Runs the editor command as the overlay does (`/bin/sh -c`), the login shell's rc isolated to `home` and
/// EDITOR/VISUAL set only by `vars`.
fn run_editor_command(path: &str, home: &Path, vars: &[(&str, &str)]) -> bool {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(editor_command(path))
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_DATA_HOME", home)
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .env_remove("ENV")
        .env_remove("BASH_ENV");
    for (key, value) in vars {
        command.env(key, value);
    }
    command.status().is_ok_and(|status| status.success())
}

#[test]
fn the_editor_command_resolves_an_exported_editor_and_preserves_the_path() {
    let home = tempfile::tempdir().unwrap();
    let (editor, got) = recorder(home.path(), "editor");
    // the path carries a space and a single quote, the characters the nested quoting can break.
    let path = home.path().join("a b/o'd.conf").display().to_string();
    assert!(run_editor_command(
        &path,
        home.path(),
        &[("SHELL", "/bin/sh"), ("EDITOR", &editor)]
    ));
    assert_eq!(fs::read_to_string(got).unwrap(), path);
}

#[test]
fn the_editor_command_prefers_visual_over_editor() {
    let home = tempfile::tempdir().unwrap();
    let (visual, visual_got) = recorder(home.path(), "visual");
    let (editor, editor_got) = recorder(home.path(), "editor");
    let path = home.path().join("k.conf").display().to_string();
    assert!(run_editor_command(
        &path,
        home.path(),
        &[("SHELL", "/bin/sh"), ("VISUAL", &visual), ("EDITOR", &editor)]
    ));
    assert_eq!(fs::read_to_string(visual_got).unwrap(), path);
    assert!(!editor_got.exists(), "EDITOR must not run when VISUAL is set");
}

#[test]
fn the_editor_command_falls_back_to_sh_without_a_shell() {
    let home = tempfile::tempdir().unwrap();
    let (editor, got) = recorder(home.path(), "editor");
    let path = home.path().join("k.conf").display().to_string();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(editor_command(&path))
        .env("HOME", home.path())
        .env_remove("SHELL");
    command
        .env_remove("VISUAL")
        .env_remove("ENV")
        .env_remove("BASH_ENV")
        .env("EDITOR", &editor);
    assert!(command.status().is_ok_and(|status| status.success()));
    assert_eq!(fs::read_to_string(got).unwrap(), path);
}

#[test]
fn the_editor_command_sources_the_login_shells_profile_for_an_exported_editor() {
    let Some(bash) = ["/usr/bin/bash", "/bin/bash"]
        .into_iter()
        .find(|path| Path::new(path).exists())
    else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let (editor, got) = recorder(home.path(), "rc-editor");
    // `bash -il` reads ~/.bash_profile; EDITOR is exported only there, never in the process environment.
    fs::write(home.path().join(".bash_profile"), format!("export EDITOR='{editor}'\n")).unwrap();
    let path = home.path().join("k.conf").display().to_string();
    assert!(run_editor_command(&path, home.path(), &[("SHELL", bash)]));
    assert_eq!(fs::read_to_string(got).unwrap(), path);
}

#[test]
fn the_editor_command_works_under_fish() {
    let Some(fish) = ["/usr/bin/fish", "/bin/fish", "/usr/local/bin/fish"]
        .into_iter()
        .find(|path| Path::new(path).exists())
    else {
        return;
    };
    let home = tempfile::tempdir().unwrap();
    let (editor, got) = recorder(home.path(), "editor");
    let path = home.path().join("a b/o'd.conf").display().to_string();
    assert!(run_editor_command(
        &path,
        home.path(),
        &[("SHELL", fish), ("EDITOR", &editor)]
    ));
    assert_eq!(fs::read_to_string(got).unwrap(), path);
}
