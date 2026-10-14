//! ghostty's keybinds against `Binding.zig`, `Config.Keybinds` and `Surface.maybeHandleBinding` at `GHOSTTY_REV`;
//! `agterm-ghostty`'s `config_parity` test checks the lines' diagnostics against the linked libghostty.

use super::binds::{
    Action, BindError, BindFlags, BindingLine, CloseTabMode, CopyFormat, Direction, Entry, KeyAction, KeyEffect,
    KeyEvent, KeyResult, KeyRoute, Keybinds, Keyboard, Mods, PHYSICAL_KEYS, Performer, PhysicalKey, Queued, Scope,
    SplitDirection, SplitFocus, Trigger, TriggerKey, WriteAction, WriteFormat, WriteScreen, action_names, check,
};
use super::{DEFAULTS, Loader};
use crate::keymap::{Chord, CustomCommandEngine, EngineOutcome, Modifiers, ShippedDefaults, parse_keymap};

/// Every action of `Binding.Action` at `GHOSTTY_REV`, in its order, but `cursor_key` (no config can set it).
const GHOSTTY_ACTIONS: &[&str] = &[
    "ignore",
    "unbind",
    "csi",
    "esc",
    "text",
    "reset",
    "copy_to_clipboard",
    "paste_from_clipboard",
    "paste_from_selection",
    "copy_url_to_clipboard",
    "copy_title_to_clipboard",
    "increase_font_size",
    "decrease_font_size",
    "reset_font_size",
    "set_font_size",
    "search",
    "search_selection",
    "navigate_search",
    "start_search",
    "end_search",
    "clear_screen",
    "select_all",
    "scroll_to_top",
    "scroll_to_bottom",
    "scroll_to_selection",
    "scroll_to_row",
    "scroll_page_up",
    "scroll_page_down",
    "scroll_page_fractional",
    "scroll_page_lines",
    "adjust_selection",
    "jump_to_prompt",
    "write_scrollback_file",
    "write_screen_file",
    "write_selection_file",
    "new_window",
    "new_tab",
    "previous_tab",
    "next_tab",
    "last_tab",
    "goto_tab",
    "move_tab",
    "move_tab_to_new_window",
    "toggle_tab_overview",
    "prompt_surface_title",
    "prompt_tab_title",
    "prompt_window_title",
    "set_surface_title",
    "set_tab_title",
    "set_window_title",
    "new_split",
    "goto_split",
    "goto_window",
    "toggle_split_zoom",
    "toggle_readonly",
    "resize_split",
    "equalize_splits",
    "reset_window_size",
    "inspector",
    "show_gtk_inspector",
    "show_on_screen_keyboard",
    "open_config",
    "reload_config",
    "close_surface",
    "close_tab",
    "close_window",
    "close_all_windows",
    "toggle_maximize",
    "toggle_fullscreen",
    "toggle_window_decorations",
    "toggle_window_float_on_top",
    "toggle_secure_input",
    "toggle_mouse_reporting",
    "toggle_command_palette",
    "toggle_quick_terminal",
    "toggle_visibility",
    "toggle_background_opacity",
    "check_for_updates",
    "undo",
    "redo",
    "end_key_sequence",
    "activate_key_table",
    "activate_key_table_once",
    "deactivate_key_table",
    "deactivate_all_key_tables",
    "quit",
    "crash",
];

const CTRL_SHIFT: Mods = Mods::CTRL.union(Mods::SHIFT);

fn action(text: &str) -> Action {
    text.parse().expect("a valid action")
}

fn trigger(text: &str) -> Trigger {
    Trigger::parse(text).expect("a valid trigger")
}

/// The physical key a trigger names, if it names one.
fn key_name(text: &str) -> Option<&'static str> {
    match trigger(text).key {
        TriggerKey::Physical(key) => Some(key.name()),
        TriggerKey::Unicode(_) | TriggerKey::CatchAll => None,
    }
}

fn physical(name: &str) -> PhysicalKey {
    PhysicalKey::new(name).expect("a physical key")
}

/// The binds of ghostty's defaults with `lines` over them.
fn with_lines(lines: &[&str]) -> Keybinds {
    let mut binds = Keybinds::defaults();
    for line in lines {
        binds.parse_cli(line).expect("a valid keybind line");
    }
    binds
}

/// The binds agterm's built-in config leaves: ghostty's, then `ghostty-defaults.conf`'s physical Edit chords.
fn agterm_binds() -> Keybinds {
    let mut loader = Loader::new(None);
    loader.load_text("ghostty-defaults.conf", DEFAULTS);
    loader.finish().finalize(false, &[]).config.keybind
}

/// The actions a trigger binds in the root set.
fn bound(binds: &Keybinds, text: &str) -> Option<(Vec<Action>, BindFlags)> {
    match binds.set.get(&trigger(text))? {
        Entry::Leaf(leaf) => Some((leaf.actions.clone(), leaf.flags)),
        Entry::Leader(_) => None,
    }
}

fn press<'a>(key: &str, mods: Mods, text: &'a str, unshifted: Option<char>) -> KeyEvent<'a> {
    KeyEvent {
        action: KeyAction::Press,
        key: physical(key),
        mods,
        text,
        unshifted,
    }
}

fn release<'a>(event: &KeyEvent<'a>) -> KeyEvent<'a> {
    KeyEvent {
        action: KeyAction::Release,
        ..*event
    }
}

/// A US-layout letter key, typed with `mods`.
fn latin(letter: char, mods: Mods) -> KeyEvent<'static> {
    let key = Box::leak(format!("key_{letter}").into_boxed_str());
    press(key, mods, "", Some(letter))
}

/// A frontend's terminal: records what it performed; performs only what `can` allows.
struct Terminal {
    performed: Vec<Action>,
    everywhere: Vec<Action>,
    can: fn(&Action) -> bool,
}

impl Terminal {
    fn new(can: fn(&Action) -> bool) -> Self {
        Self {
            performed: Vec::new(),
            everywhere: Vec::new(),
            can,
        }
    }

    fn capable() -> Self {
        Self::new(|_| true)
    }
}

impl Performer for Terminal {
    fn perform(&mut self, action: &Action) -> bool {
        self.performed.push(action.clone());
        (self.can)(action)
    }

    fn perform_all(&mut self, action: &Action) {
        self.everywhere.push(action.clone());
    }
}

const fn result(queued: Queued, effect: KeyEffect) -> KeyResult {
    KeyResult { queued, effect }
}

const ENCODE: KeyResult = result(Queued::Keep, KeyEffect::Encode);
const CONSUMED: KeyResult = result(Queued::Drop, KeyEffect::Consumed);

// -- the action table

#[test]
fn the_action_table_is_ghosttys() {
    assert_eq!(action_names().collect::<Vec<_>>(), GHOSTTY_ACTIONS);
    assert_eq!("cursor_key:x".parse::<Action>(), Err(BindError::InvalidAction));
}

#[test]
fn every_action_core_sends_through_binding_action_is_in_the_table() {
    // commands/surface.rs, commands/search.rs, commands/font.rs, app_actions.rs and the GTK dashboard's zoom
    let sent = [
        ("paste_from_clipboard", Action::PasteFromClipboard),
        ("select_all", Action::SelectAll),
        ("start_search", Action::StartSearch),
        ("end_search", Action::EndSearch),
        (
            "search:needle with: colon",
            Action::Search("needle with: colon".to_owned()),
        ),
        ("search:", Action::Search(String::new())),
        ("navigate_search:previous", Action::NavigateSearch(Direction::Previous)),
        ("navigate_search:next", Action::NavigateSearch(Direction::Next)),
        ("increase_font_size:1", Action::IncreaseFontSize(1.0)),
        ("decrease_font_size:1", Action::DecreaseFontSize(1.0)),
        ("reset_font_size", Action::ResetFontSize),
        ("set_font_size:15", Action::SetFontSize(15.0)),
        ("set_font_size:13.5", Action::SetFontSize(13.5)),
    ];
    for (text, expected) in sent {
        assert_eq!(text.parse::<Action>(), Ok(expected), "{text}");
    }
}

#[test]
fn actions_take_their_parameters_as_ghostty_parses_them() {
    assert_eq!(action("copy_to_clipboard"), Action::CopyToClipboard(CopyFormat::Mixed));
    assert_eq!(action("copy_to_clipboard:vt"), Action::CopyToClipboard(CopyFormat::Vt));
    assert_eq!(action("new_split"), Action::NewSplit(SplitDirection::Auto));
    assert_eq!(action("goto_split:top"), Action::GotoSplit(SplitFocus::Up));
    assert_eq!(action("goto_split:bottom"), Action::GotoSplit(SplitFocus::Down));
    assert_eq!(action("close_tab"), Action::CloseTab(CloseTabMode::This));
    assert_eq!(action("jump_to_prompt:-1"), Action::JumpToPrompt(-1));
    assert_eq!(action("goto_tab:1_0"), Action::GotoTab(10));
    assert_eq!(
        action("write_screen_file:open,html"),
        Action::WriteScreenFile(WriteScreen {
            action: WriteAction::Open,
            format: WriteFormat::Html
        })
    );
    assert_eq!(
        action("write_selection_file:copy"),
        Action::WriteSelectionFile(WriteScreen {
            action: WriteAction::Copy,
            format: WriteFormat::Plain
        })
    );
    assert_eq!(
        action("text:a=b:c"),
        Action::Text("a=b:c".to_owned()),
        "the parameter is the rest"
    );
    for (bad, error) in [
        ("", BindError::InvalidFormat),
        (":x", BindError::InvalidFormat),
        ("nope", BindError::InvalidAction),
        ("Select_all", BindError::InvalidAction),
        ("select_all:", BindError::InvalidFormat),
        ("text", BindError::InvalidFormat),
        ("inspector", BindError::InvalidFormat),
        ("goto_tab:-1", BindError::InvalidFormat),
        ("goto_tab:0x1", BindError::InvalidFormat),
        ("jump_to_prompt:32768", BindError::InvalidFormat),
        ("resize_split:up", BindError::InvalidFormat),
        ("resize_split:up,1,2", BindError::InvalidFormat),
        ("write_screen_file:copy,pdf", BindError::InvalidFormat),
        ("copy_to_clipboard:Plain", BindError::InvalidFormat),
    ] {
        assert_eq!(bad.parse::<Action>(), Err(error), "{bad:?}");
    }
}

#[test]
fn an_action_prints_as_a_config_line_writes_it() {
    for text in [
        "select_all",
        "copy_to_clipboard:mixed",
        "resize_split:left,10",
        "write_scrollback_file:paste,vt",
        "goto_split:up",
        "set_font_size:13.5",
        "text:\\x1b",
        "move_tab:-2",
    ] {
        assert_eq!(action(text).to_string(), text);
    }
}

#[test]
fn csi_esc_and_text_write_their_bytes() {
    assert_eq!(action("csi:A").pty_bytes(), Some(b"\x1b[A".to_vec()));
    assert_eq!(action("esc:b").pty_bytes(), Some(b"\x1bb".to_vec()));
    assert_eq!(
        action(r"text:\x05\n\u{444}x").pty_bytes(),
        Some(b"\x05\n\xd1\x84x".to_vec())
    );
    assert_eq!(
        action(r"text:\q").pty_bytes(),
        None,
        "a malformed escape writes nothing"
    );
    assert_eq!(
        action(&format!("csi:{}", "1".repeat(126))).pty_bytes().map(|b| b.len()),
        Some(128)
    );
    assert_eq!(action(&format!("csi:{}", "1".repeat(127))).pty_bytes(), None);
    assert_eq!(action("select_all").pty_bytes(), None);
}

#[test]
fn actions_know_their_scope_and_whether_they_close() {
    assert_eq!(action("quit").scope(), Scope::App);
    assert_eq!(action("new_window").scope(), Scope::App);
    assert_eq!(action("ignore").scope(), Scope::App);
    assert_eq!(action("copy_to_clipboard").scope(), Scope::Surface);
    assert_eq!(action("new_tab").scope(), Scope::Surface);
    assert!(action("close_surface").closes());
    assert!(action("close_tab:other").closes());
    assert!(!action("quit").closes());
}

// -- triggers

#[test]
fn triggers_parse_modifiers_and_one_key() {
    let ctrl_shift_c = Trigger::new(TriggerKey::Physical(physical("key_c")), CTRL_SHIFT);
    assert_eq!(trigger("ctrl+shift+key_c"), ctrl_shift_c);
    assert_eq!(trigger("shift+control+KeyC"), ctrl_shift_c, "an alias, a W3C code");
    assert_eq!(
        trigger("cmd+opt+a"),
        Trigger::new(TriggerKey::Unicode('a'), Mods::SUPER | Mods::ALT)
    );
    assert_eq!(key_name("ArrowUp"), Some("arrow_up"));
    assert_eq!(key_name("F12"), Some("f12"));
    assert_eq!(trigger("ctrl++"), Trigger::new(TriggerKey::Unicode('+'), Mods::CTRL));
    assert_eq!(trigger("+"), Trigger::new(TriggerKey::Unicode('+'), Mods::empty()));
    assert_eq!(trigger("catch_all"), Trigger::new(TriggerKey::CatchAll, Mods::empty()));
    assert!(trigger("ctrl").is_key_unset(), "modifiers alone");
}

#[test]
fn old_key_names_still_parse_and_malformed_triggers_do_not() {
    for (old, new) in [
        ("physical:one", "digit_1"),
        ("kp_enter", "numpad_enter"),
        ("kp_7", "numpad_7"),
        ("grave_accent", "backquote"),
        ("left_super", "meta_left"),
        ("physical:apostrophe", "quote"),
    ] {
        assert_eq!(key_name(old), Some(new), "{old}");
    }
    assert_eq!(trigger("one"), trigger("1"));
    assert_eq!(trigger("apostrophe"), trigger("'"));
    for bad in [
        "",
        "ctrl+ctrl+a",
        "control+ctrl+a",
        "a+b",
        "KeyA+KeyB",
        "foo",
        "physical:plus",
        "Key_A1",
    ] {
        assert_eq!(Trigger::parse(bad), Err(BindError::InvalidFormat), "{bad:?}");
    }
}

#[test]
fn character_triggers_compare_case_folded() {
    assert_eq!(trigger("ctrl+A"), trigger("ctrl+a"));
    assert_eq!(trigger("Ф"), trigger("ф"));
    assert_eq!(trigger("ẞ"), trigger("ß"));
    assert_eq!(trigger("ς"), trigger("σ"));
    assert_ne!(trigger("a"), trigger("key_a"), "a character is not a key position");
    assert_ne!(trigger("ctrl+a"), trigger("a"));
}

#[test]
fn the_physical_keys_are_ghosttys() {
    assert_eq!(PHYSICAL_KEYS.len(), 176);
    assert_eq!(PHYSICAL_KEYS.first(), Some(&"unidentified"));
    assert_eq!(
        PhysicalKey::from_w3c("NumpadMemoryAdd").map(PhysicalKey::name),
        Some("numpad_memory_add")
    );
    assert_eq!(PhysicalKey::from_w3c("IntlRo").map(PhysicalKey::name), Some("intl_ro"));
    assert_eq!(PhysicalKey::from_w3c("Key-A"), None);
    assert!(physical("shift_left").is_modifier());
    assert!(!physical("key_a").is_modifier());
}

// -- binding lines

#[test]
fn a_line_carries_flags_triggers_and_an_action() {
    let line = BindingLine::parse("unconsumed:performable:ctrl+a>ctrl+b=text:x").expect("valid");
    assert_eq!(line.flags, BindFlags::PERFORMABLE);
    assert_eq!(line.triggers, [trigger("ctrl+a"), trigger("ctrl+b")]);
    assert_eq!(line.action, action("text:x"));
    assert!(!line.chain);
    let equals = BindingLine::parse("ctrl+==increase_font_size:1").expect("valid");
    assert_eq!(equals.triggers, [trigger("ctrl+=")]);
    let chain = BindingLine::parse("chain=new_window").expect("valid");
    assert!(chain.chain && chain.triggers.is_empty());
    for (bad, error) in [
        ("garbage", BindError::InvalidFormat),
        ("a=nope", BindError::InvalidAction),
        ("foo=nope", BindError::InvalidAction),
        ("a>=ignore", BindError::InvalidFormat),
        ("all:all:a=ignore", BindError::InvalidFormat),
        ("global:a>b=ignore", BindError::InvalidFormat),
        ("unconsumed:chain=ignore", BindError::InvalidFormat),
    ] {
        assert_eq!(BindingLine::parse(bad), Err(error), "{bad}");
    }
}

#[test]
fn a_value_is_checked_alone_as_the_config_table_checks_it() {
    for good in [
        "",
        "clear",
        "vim/",
        "vim/j=ignore",
        "/=ignore",
        "ctrl+/=ignore",
        "chain=ignore",
        "a=text:/x",
    ] {
        assert_eq!(check(good), Ok(()), "{good:?}");
    }
    for bad in ["vim/chain=ignore", "chain=unbind", "vim/bogus", "a=nope"] {
        assert!(check(bad).is_err(), "{bad:?}");
    }
}

// -- the built-in binds

#[test]
fn ghosttys_linux_binds_are_the_defaults() {
    let binds = Keybinds::defaults();
    let performable = BindFlags::default() | BindFlags::PERFORMABLE;
    // a later default replaces an earlier one, as in `Keybinds.init`: shift+insert, ctrl+shift+w, shift+home
    for (trigger, text, flags) in [
        ("ctrl+shift+c", "copy_to_clipboard", performable),
        ("ctrl+shift+v", "paste_from_clipboard", performable),
        ("ctrl+insert", "copy_to_clipboard", BindFlags::default()),
        ("shift+insert", "paste_from_selection", BindFlags::default()),
        ("ctrl+shift+w", "close_tab", BindFlags::default()),
        ("shift+home", "scroll_to_top", BindFlags::default()),
        ("shift+arrow_left", "adjust_selection:left", performable),
        ("ctrl+shift+arrow_up", "jump_to_prompt:-1", BindFlags::default()),
        ("alt+digit_3", "goto_tab:3", performable),
        ("alt+3", "goto_tab:3", performable),
        ("ctrl+=", "increase_font_size:1", BindFlags::default()),
        ("escape", "end_search", performable),
        ("ctrl+shift+p", "toggle_command_palette", BindFlags::default()),
    ] {
        assert_eq!(bound(&binds, trigger), Some((vec![action(text)], flags)), "{trigger}");
    }
    assert_eq!(bound(&binds, "super+c"), None, "no macOS binds");
    assert_eq!(binds.set.iter().count(), 74);
    for (trigger, _) in binds.set.iter() {
        if let TriggerKey::Physical(key) = trigger.key {
            assert_eq!(PhysicalKey::new(key.name()), Some(key));
        }
    }
}

#[test]
fn agterms_config_adds_physical_edit_chords() {
    let binds = agterm_binds();
    assert_eq!(
        bound(&binds, "ctrl+shift+key_c"),
        Some((vec![action("copy_to_clipboard")], BindFlags::default()))
    );
    assert_eq!(
        bound(&binds, "ctrl+shift+key_v").map(|b| b.0),
        Some(vec![Action::PasteFromClipboard])
    );
    assert_eq!(
        bound(&binds, "ctrl+shift+key_a").map(|b| b.0),
        Some(vec![Action::SelectAll])
    );
    assert!(bound(&binds, "ctrl+shift+c").is_some(), "ghostty's own stay");
}

// -- lines over the binds

#[test]
fn a_later_line_overrides_and_unbind_removes() {
    let binds = with_lines(&["ctrl+shift+c=text:x", "ctrl+shift+v=unbind", "ctrl+shift+C=new_window"]);
    assert_eq!(
        bound(&binds, "ctrl+shift+c").map(|b| b.0),
        Some(vec![Action::NewWindow]),
        "case-folded"
    );
    assert_eq!(bound(&binds, "ctrl+shift+v"), None);
    let mut binds = binds;
    binds.parse_cli("").expect("valid");
    assert_eq!(binds, Keybinds::defaults(), "an empty value restores ghostty's binds");
    binds.parse_cli("clear").expect("valid");
    assert!(binds.set.is_empty());
}

#[test]
fn sequences_nest_and_unbind_back_to_what_was_there() {
    let mut binds = with_lines(&["ctrl+a=text:x", "ctrl+a>b=new_window", "ctrl+a>c=new_tab"]);
    assert!(matches!(binds.set.get(&trigger("ctrl+a")), Some(Entry::Leader(_))));
    binds.parse_cli("ctrl+a>b=unbind").expect("valid");
    assert!(
        matches!(binds.set.get(&trigger("ctrl+a")), Some(Entry::Leader(_))),
        "c is still there"
    );
    binds.parse_cli("ctrl+a>c=unbind").expect("valid");
    assert_eq!(binds.set.get(&trigger("ctrl+a")), None, "an emptied leader goes");
    // a sequence that only unbinds leaves the key's own binding alone
    let mut binds = binds_with_leaf();
    binds.parse_cli("ctrl+a>b=unbind").expect("valid");
    assert_eq!(bound(&binds, "ctrl+a").map(|b| b.0), Some(vec![action("text:x")]));
}

fn binds_with_leaf() -> Keybinds {
    with_lines(&["ctrl+a=text:x"])
}

#[test]
fn chains_extend_the_binding_before_them() {
    let binds = with_lines(&["ctrl+a=text:x", "chain=new_window", "chain=new_tab"]);
    assert_eq!(
        bound(&binds, "ctrl+a").map(|b| b.0),
        Some(vec![action("text:x"), Action::NewWindow, Action::NewTab])
    );
    let mut sequence = with_lines(&["ctrl+a>b=text:x", "chain=new_window"]);
    let Some(Entry::Leader(next)) = sequence.set.get(&trigger("ctrl+a")) else {
        panic!("a leader");
    };
    assert!(matches!(next.get(&trigger("b")), Some(Entry::Leaf(leaf)) if leaf.actions.len() == 2));
    // nothing to extend after a clear or an unbind; a refused line keeps the chain where it was
    assert_eq!(sequence.parse_cli("clear"), Ok(()));
    assert_eq!(sequence.parse_cli("chain=new_tab"), Err(BindError::InvalidFormat));
    let mut unbound = with_lines(&["ctrl+a=unbind"]);
    assert_eq!(unbound.parse_cli("chain=new_tab"), Err(BindError::InvalidFormat));
    let mut kept = with_lines(&["ctrl+a=text:x"]);
    assert_eq!(kept.parse_cli("ctrl+b=bogus"), Err(BindError::InvalidAction));
    assert_eq!(kept.parse_cli("chain=new_tab"), Ok(()));
    assert_eq!(bound(&kept, "ctrl+a").map(|b| b.0.len()), Some(2));
    assert_eq!(kept.parse_cli("chain=unbind"), Err(BindError::InvalidFormat));
    // the built-in binds leave the last of them to extend, as ghostty's do
    let mut fresh = Keybinds::defaults();
    assert_eq!(fresh.parse_cli("chain=new_tab"), Ok(()));
    assert_eq!(bound(&fresh, "ctrl+shift+p").map(|b| b.0.len()), Some(2));
}

#[test]
fn key_tables_hold_their_own_binds_and_chains() {
    let mut binds = with_lines(&["vim/j=scroll_page_lines:1", "chain=scroll_page_lines:1", "/=text:slash"]);
    let table = binds.table("vim").expect("a table");
    assert!(matches!(table.get(&trigger("j")), Some(Entry::Leaf(leaf)) if leaf.actions.len() == 2));
    assert_eq!(
        bound(&binds, "/").map(|b| b.0),
        Some(vec![action("text:slash")]),
        "the slash key"
    );
    assert_eq!(binds.parse_cli("vim/chain=ignore"), Err(BindError::InvalidFormat));
    assert_eq!(binds.parse_cli("other/bogus"), Err(BindError::InvalidFormat));
    assert_eq!(
        binds.table_names().collect::<Vec<_>>(),
        ["vim", "other"],
        "a refused line still defines it"
    );
    binds.parse_cli("vim/").expect("valid");
    assert!(binds.table("vim").is_some_and(super::binds::BindSet::is_empty));
}

#[test]
fn the_typed_config_counts_a_chain_with_nothing_to_extend() {
    let mut loader = Loader::new(None);
    loader.load_text(
        "test.conf",
        "keybind = clear\nkeybind = chain=new_tab\nkeybind = a=nope\n",
    );
    let built = loader.finish().finalize(false, &[]);
    let messages: Vec<_> = built.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(
        messages,
        ["unknown error error.InvalidFormat", "unknown error error.InvalidAction"]
    );
    assert!(built.config.keybind.set.is_empty());
}

// -- resolving keys

#[test]
fn a_copy_bound_by_character_needs_a_latin_layout_and_falls_through_unperformed() {
    let binds = Keybinds::defaults();
    let mut keyboard = Keyboard::default();
    let latin_c = latin('c', CTRL_SHIFT);
    let mut terminal = Terminal::capable();
    assert_eq!(keyboard.handle(&binds, &latin_c, &mut terminal), CONSUMED);
    assert_eq!(terminal.performed, [action("copy_to_clipboard")]);
    // performable: no selection, nothing copied, the key goes on to the encoder
    let mut empty = Terminal::new(|_| false);
    assert_eq!(
        keyboard.handle(&binds, &latin_c, &mut empty),
        result(Queued::Flush, KeyEffect::Encode)
    );
    // a Russian layout puts `с` on that key: ghostty's character bind never matches
    let cyrillic = press("key_c", CTRL_SHIFT, "", Some('с'));
    let mut terminal = Terminal::capable();
    assert_eq!(keyboard.handle(&binds, &cyrillic, &mut terminal), ENCODE);
    assert!(terminal.performed.is_empty());
}

#[test]
fn agterms_physical_edit_chords_work_on_any_layout_and_always_consume() {
    let binds = agterm_binds();
    let mut keyboard = Keyboard::default();
    for (key, unshifted, expected) in [
        ("key_c", 'с', action("copy_to_clipboard")),
        ("key_v", 'м', Action::PasteFromClipboard),
        ("key_a", 'ф', Action::SelectAll),
        ("key_c", 'ψ', action("copy_to_clipboard")),
    ] {
        let event = press(key, CTRL_SHIFT, "", Some(unshifted));
        // not performable: Copy without a selection must not reach the encoder (the kitty protocol would report it)
        let mut terminal = Terminal::new(|_| false);
        assert_eq!(
            keyboard.handle(&binds, &event, &mut terminal),
            CONSUMED,
            "{key} {unshifted}"
        );
        assert_eq!(terminal.performed, [expected]);
        assert_eq!(
            keyboard.handle(&binds, &release(&event), &mut terminal),
            result(Queued::Keep, KeyEffect::Consumed)
        );
    }
}

#[test]
fn keys_are_looked_up_by_position_then_character_then_unshifted_then_catch_all() {
    let binds = with_lines(&[
        "clear",
        "key_q=text:position",
        "Q=text:typed",
        "w=text:unshifted",
        "catch_all=text:any",
    ]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    for event in [
        press("key_q", Mods::empty(), "q", Some('q')),
        press("key_x", Mods::empty(), "q", Some('x')),
        press("key_z", Mods::empty(), "", Some('w')),
        press("key_z", Mods::empty(), "z", Some('z')),
        press("key_z", Mods::CTRL, "", Some('z')),
    ] {
        keyboard.handle(&binds, &event, &mut terminal);
    }
    let typed = ["position", "typed", "unshifted", "any", "any"].map(|text| Action::Text(text.to_owned()));
    assert_eq!(terminal.performed, typed);
}

#[test]
fn a_release_is_consumed_only_after_its_press_fired() {
    let binds = with_lines(&["ctrl+a=text:x"]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    let unbound = latin('b', Mods::CTRL);
    assert_eq!(keyboard.handle(&binds, &release(&unbound), &mut terminal), ENCODE);
    let bound = latin('a', Mods::CTRL);
    keyboard.handle(&binds, &bound, &mut terminal);
    let repeat = KeyEvent {
        action: KeyAction::Repeat,
        ..bound
    };
    assert_eq!(
        keyboard.handle(&binds, &repeat, &mut terminal),
        CONSUMED,
        "a repeat fires again"
    );
    assert_eq!(
        keyboard.handle(&binds, &release(&bound), &mut terminal).effect,
        KeyEffect::Consumed
    );
    assert_eq!(keyboard.handle(&binds, &release(&unbound), &mut terminal), ENCODE);
}

#[test]
fn unconsumed_performs_and_encodes_ignore_lets_the_window_have_it() {
    let binds = with_lines(&["unconsumed:ctrl+a=text:x", "ctrl+b=ignore", "ctrl+c=close_surface"]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    assert_eq!(
        keyboard.handle(&binds, &latin('a', Mods::CTRL), &mut terminal),
        result(Queued::Flush, KeyEffect::Encode)
    );
    assert_eq!(terminal.performed, [action("text:x")]);
    assert_eq!(
        keyboard.handle(&binds, &latin('b', Mods::CTRL), &mut terminal),
        result(Queued::Drop, KeyEffect::Ignored)
    );
    assert_eq!(
        keyboard.handle(&binds, &latin('c', Mods::CTRL), &mut terminal),
        result(Queued::Drop, KeyEffect::Closed)
    );
    // a binding that does nothing still takes the key unless it is performable
    let mut idle = Terminal::new(|_| false);
    assert_eq!(keyboard.handle(&binds, &latin('c', Mods::CTRL), &mut idle), CONSUMED);
}

#[test]
fn all_and_global_bindings_run_everywhere_and_count_as_performed() {
    let binds = with_lines(&["performable:all:ctrl+a=new_tab", "global:ctrl+b=quit"]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::new(|_| false);
    assert_eq!(
        keyboard.handle(&binds, &latin('a', Mods::CTRL), &mut terminal),
        CONSUMED
    );
    assert_eq!(
        keyboard.handle(&binds, &latin('b', Mods::CTRL), &mut terminal),
        CONSUMED
    );
    assert_eq!(terminal.everywhere, [Action::NewTab, Action::Quit]);
    assert!(terminal.performed.is_empty());
}

#[test]
fn a_sequence_queues_its_keys_until_it_completes_or_breaks() {
    let binds = with_lines(&["ctrl+a>ctrl+b>c=text:done", "catch_all=text:other"]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    let queue = result(Queued::Keep, KeyEffect::Queue);
    assert_eq!(keyboard.handle(&binds, &latin('a', Mods::CTRL), &mut terminal), queue);
    assert!(keyboard.in_sequence());
    // a modifier on its own waits, the sequence stays
    let shift = press("shift_left", Mods::SHIFT, "", None);
    assert_eq!(keyboard.handle(&binds, &shift, &mut terminal), ENCODE);
    assert_eq!(keyboard.handle(&binds, &latin('b', Mods::CTRL), &mut terminal), queue);
    assert_eq!(
        keyboard.handle(&binds, &latin('c', Mods::empty()), &mut terminal),
        CONSUMED
    );
    assert_eq!(terminal.performed, [action("text:done")]);
    assert!(!keyboard.in_sequence());
    // a key the sequence does not bind sends the queued keys, then itself
    keyboard.handle(&binds, &latin('a', Mods::CTRL), &mut terminal);
    assert_eq!(
        keyboard.handle(&binds, &latin('x', Mods::empty()), &mut terminal),
        result(Queued::Flush, KeyEffect::Encode)
    );
    assert!(!keyboard.in_sequence());
    assert_eq!(
        terminal.performed.len(),
        1,
        "the root catch-all is not consulted inside a sequence"
    );
}

#[test]
fn an_ignore_catch_all_drops_a_broken_sequence() {
    let binds = with_lines(&["ctrl+a>b=text:done", "catch_all=ignore"]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    keyboard.handle(&binds, &latin('a', Mods::CTRL), &mut terminal);
    assert_eq!(
        keyboard.handle(&binds, &latin('x', Mods::empty()), &mut terminal),
        result(Queued::Drop, KeyEffect::Ignored)
    );
}

#[test]
fn key_tables_take_keys_before_the_root_innermost_first() {
    let binds = with_lines(&[
        "ctrl+t=activate_key_table:vim",
        "vim/j=text:down",
        "vim/ctrl+o=activate_key_table_once:one",
        "vim/escape=deactivate_key_table",
        "one/j=text:once",
        "j=text:root",
    ]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    let j = latin('j', Mods::empty());
    keyboard.handle(&binds, &j, &mut terminal);
    assert_eq!(
        keyboard.handle(&binds, &latin('t', Mods::CTRL), &mut terminal),
        CONSUMED
    );
    assert_eq!(keyboard.active_tables().collect::<Vec<_>>(), ["vim"]);
    keyboard.handle(&binds, &j, &mut terminal);
    keyboard.handle(&binds, &latin('o', Mods::CTRL), &mut terminal);
    keyboard.handle(&binds, &j, &mut terminal);
    assert_eq!(
        keyboard.active_tables().collect::<Vec<_>>(),
        ["vim"],
        "a once table ends after one binding"
    );
    keyboard.handle(&binds, &press("escape", Mods::empty(), "", None), &mut terminal);
    keyboard.handle(&binds, &j, &mut terminal);
    let typed = ["root", "down", "once", "root"].map(|text| Action::Text(text.to_owned()));
    assert_eq!(
        terminal.performed, typed,
        "the keyboard performs the table actions itself"
    );
    // an undefined table is not performed, so a performable binding of it lets the key through
    let binds = with_lines(&["performable:ctrl+u=activate_key_table:nope"]);
    assert_eq!(
        keyboard.handle(&binds, &latin('u', Mods::CTRL), &mut terminal),
        result(Queued::Flush, KeyEffect::Encode)
    );
}

#[test]
fn agterms_keymap_sees_a_key_before_ghosttys_binds_and_the_encoder() {
    let (keymap, diagnostics) = parse_keymap("command \"Mine\" ctrl+shift+c echo mine", &ShippedDefaults::upstream());
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let mut engine = CustomCommandEngine::from_keymap(&keymap);
    let binds = agterm_binds();
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    let mut route = |chord: Chord, event: &KeyEvent<'_>, terminal: &mut Terminal| {
        keyboard.route(
            || Some(engine.advance(&chord)).filter(|outcome| *outcome != EngineOutcome::Unmatched),
            &binds,
            event,
            terminal,
        )
    };
    let ctrl_shift = Modifiers::CONTROL | Modifiers::SHIFT;
    // the keymap's chord: ghostty's copy bind never fires
    let taken = route(Chord::new(ctrl_shift, "c"), &latin('c', CTRL_SHIFT), &mut terminal);
    assert!(matches!(taken, KeyRoute::Keymap(EngineOutcome::Fired(_))), "{taken:?}");
    assert!(terminal.performed.is_empty());
    // not the keymap's: ghostty's binds take it
    let pasted = route(Chord::new(ctrl_shift, "v"), &latin('v', CTRL_SHIFT), &mut terminal);
    assert_eq!(pasted, KeyRoute::Terminal(CONSUMED));
    assert_eq!(terminal.performed, [Action::PasteFromClipboard]);
    // neither: the encoder
    let typed = route(
        Chord::new(Modifiers::empty(), "x"),
        &press("key_x", Mods::empty(), "x", Some('x')),
        &mut terminal,
    );
    assert_eq!(typed, KeyRoute::Terminal(ENCODE));
}

#[test]
fn end_key_sequence_sends_the_queued_keys() {
    let binds = with_lines(&["ctrl+a>escape=end_key_sequence"]);
    let mut keyboard = Keyboard::default();
    let mut terminal = Terminal::capable();
    keyboard.handle(&binds, &latin('a', Mods::CTRL), &mut terminal);
    let escape = press("escape", Mods::empty(), "", None);
    assert_eq!(
        keyboard.handle(&binds, &escape, &mut terminal),
        result(Queued::Flush, KeyEffect::Consumed)
    );
    assert!(!keyboard.in_sequence());
    assert!(terminal.performed.is_empty(), "the keyboard ends its own sequence");
}
