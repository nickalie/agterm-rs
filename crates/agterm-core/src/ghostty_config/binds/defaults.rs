//! ghostty's built-in binds on Linux (`Keybinds.init` at `LIBGHOSTTY_VT_REV`, the non-macOS branches), in its order: a
//! later bind of the same trigger replaces an earlier one, as there.

use super::action::{
    Action, CloseTabMode, CopyFormat, InspectorMode, OpenConfig, ResizeDirection, SelectionAdjust, SplitDirection,
    SplitFocus, WriteAction, WriteFormat, WriteScreen,
};
use super::set::{BindFlags, BindSet};
use super::trigger::{Mods, PhysicalKey, Trigger, TriggerKey};

const CTRL: Mods = Mods::CTRL;
const SHIFT: Mods = Mods::SHIFT;
const ALT: Mods = Mods::ALT;
const SUPER: Mods = Mods::SUPER;
const CTRL_SHIFT: Mods = CTRL.union(SHIFT);

/// One built-in bind: a physical key or a character, its modifiers, its action, and whether it is `performable:`.
struct Bind(Key, Mods, Action, bool);

enum Key {
    Physical(&'static str),
    Unicode(char),
}

/// The built-in binds, in `Keybinds.init`'s order.
pub fn put_defaults(set: &mut BindSet) {
    let groups = [
        clipboard_and_font(),
        selection_and_tabs(),
        splits_and_scroll(),
        search_and_tabs(),
    ];
    for Bind(key, mods, action, performable) in groups.into_iter().flatten() {
        let key = match key {
            Key::Physical(name) => TriggerKey::Physical(PhysicalKey::named(name)),
            Key::Unicode(character) => TriggerKey::Unicode(character),
        };
        let flags = if performable {
            BindFlags::default() | BindFlags::PERFORMABLE
        } else {
            BindFlags::default()
        };
        set.put(Trigger::new(key, mods), action, flags);
    }
}

fn physical(name: &'static str, mods: Mods, action: Action) -> Bind {
    Bind(Key::Physical(name), mods, action, false)
}

fn unicode(character: char, mods: Mods, action: Action) -> Bind {
    Bind(Key::Unicode(character), mods, action, false)
}

fn performable(bind: Bind) -> Bind {
    Bind(bind.0, bind.1, bind.2, true)
}

fn write_screen(action: WriteAction) -> Action {
    Action::WriteScreenFile(WriteScreen {
        action,
        format: WriteFormat::Plain,
    })
}

fn clipboard_and_font() -> Vec<Bind> {
    let copy = || Action::CopyToClipboard(CopyFormat::Mixed);
    vec![
        unicode(',', CTRL_SHIFT, Action::ReloadConfig),
        unicode(',', CTRL, Action::OpenConfig(OpenConfig::OsOpen)),
        physical("copy", Mods::empty(), copy()),
        physical("paste", Mods::empty(), Action::PasteFromClipboard),
        physical("insert", CTRL, copy()),
        physical("insert", SHIFT, Action::PasteFromClipboard),
        performable(unicode('c', CTRL_SHIFT, copy())),
        performable(unicode('v', CTRL_SHIFT, Action::PasteFromClipboard)),
        unicode('=', CTRL, Action::IncreaseFontSize(1.0)),
        unicode('+', CTRL, Action::IncreaseFontSize(1.0)),
        unicode('-', CTRL, Action::DecreaseFontSize(1.0)),
        unicode('0', CTRL, Action::ResetFontSize),
        unicode('j', CTRL_SHIFT | SUPER, write_screen(WriteAction::Copy)),
        unicode('j', CTRL_SHIFT, write_screen(WriteAction::Paste)),
        unicode('j', CTRL_SHIFT | ALT, write_screen(WriteAction::Open)),
    ]
}

fn selection_and_tabs() -> Vec<Bind> {
    let adjust = |key, direction| performable(physical(key, SHIFT, Action::AdjustSelection(direction)));
    vec![
        adjust("arrow_left", SelectionAdjust::Left),
        adjust("arrow_right", SelectionAdjust::Right),
        adjust("arrow_up", SelectionAdjust::Up),
        adjust("arrow_down", SelectionAdjust::Down),
        adjust("page_up", SelectionAdjust::PageUp),
        adjust("page_down", SelectionAdjust::PageDown),
        adjust("home", SelectionAdjust::Home),
        adjust("end", SelectionAdjust::End),
        physical("tab", CTRL_SHIFT, Action::PreviousTab),
        physical("tab", CTRL, Action::NextTab),
        unicode('n', CTRL_SHIFT, Action::NewWindow),
        unicode('w', CTRL_SHIFT, Action::CloseSurface),
        unicode('q', CTRL_SHIFT, Action::Quit),
        physical("f4", ALT, Action::CloseWindow),
        unicode('t', CTRL_SHIFT, Action::NewTab),
        unicode('w', CTRL_SHIFT, Action::CloseTab(CloseTabMode::This)),
        performable(physical("arrow_left", CTRL_SHIFT, Action::PreviousTab)),
        performable(physical("arrow_right", CTRL_SHIFT, Action::NextTab)),
        performable(physical("page_up", CTRL, Action::PreviousTab)),
        performable(physical("page_down", CTRL, Action::NextTab)),
    ]
}

fn splits_and_scroll() -> Vec<Bind> {
    let focus = |key, focus| performable(physical(key, CTRL | ALT, Action::GotoSplit(focus)));
    let resize = |key, direction| performable(physical(key, CTRL_SHIFT | SUPER, Action::ResizeSplit(direction, 10)));
    vec![
        unicode('o', CTRL_SHIFT, Action::NewSplit(SplitDirection::Right)),
        unicode('e', CTRL_SHIFT, Action::NewSplit(SplitDirection::Down)),
        performable(unicode('[', CTRL | SUPER, Action::GotoSplit(SplitFocus::Previous))),
        performable(unicode(']', CTRL | SUPER, Action::GotoSplit(SplitFocus::Next))),
        focus("arrow_up", SplitFocus::Up),
        focus("arrow_down", SplitFocus::Down),
        focus("arrow_left", SplitFocus::Left),
        focus("arrow_right", SplitFocus::Right),
        resize("arrow_up", ResizeDirection::Up),
        resize("arrow_down", ResizeDirection::Down),
        resize("arrow_left", ResizeDirection::Left),
        resize("arrow_right", ResizeDirection::Right),
        physical("home", SHIFT, Action::ScrollToTop),
        physical("end", SHIFT, Action::ScrollToBottom),
        physical("page_up", SHIFT, Action::ScrollPageUp),
        physical("page_down", SHIFT, Action::ScrollPageDown),
        physical("arrow_up", CTRL_SHIFT, Action::JumpToPrompt(-1)),
        physical("arrow_down", CTRL_SHIFT, Action::JumpToPrompt(1)),
        performable(physical("page_up", CTRL_SHIFT, Action::MoveTab(-1))),
        performable(physical("page_down", CTRL_SHIFT, Action::MoveTab(1))),
    ]
}

fn search_and_tabs() -> Vec<Bind> {
    let mut binds = vec![
        performable(unicode('f', CTRL_SHIFT, Action::StartSearch)),
        performable(physical("escape", Mods::empty(), Action::EndSearch)),
        unicode('i', CTRL_SHIFT, Action::Inspector(InspectorMode::Toggle)),
        unicode('a', CTRL_SHIFT, Action::SelectAll),
        physical("insert", SHIFT, Action::PasteFromSelection),
    ];
    let digits = [
        "digit_1", "digit_2", "digit_3", "digit_4", "digit_5", "digit_6", "digit_7", "digit_8",
    ];
    for (tab, (digit, key)) in (1..).zip(('1'..='8').zip(digits)) {
        binds.push(performable(physical(key, ALT, Action::GotoTab(tab))));
        binds.push(performable(unicode(digit, ALT, Action::GotoTab(tab))));
    }
    binds.extend([
        performable(unicode('9', ALT, Action::LastTab)),
        physical("enter", CTRL, Action::ToggleFullscreen),
        physical("enter", CTRL_SHIFT, Action::ToggleSplitZoom),
        unicode('p', CTRL_SHIFT, Action::ToggleCommandPalette),
    ]);
    binds
}
