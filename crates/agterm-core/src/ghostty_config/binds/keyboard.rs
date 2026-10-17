//! A terminal's keyboard state over the binds, ported from `Surface.maybeHandleBinding`, `endKeySequence` and the
//! key-table actions of `performBindingAction` at `LIBGHOSTTY_VT_REV`: which binding a key event fires, what the
//! half-typed sequence and the active key tables become, and whether the key still goes to the encoder.

use super::Keybinds;
use super::action::Action;
use super::set::{BindFlags, BindSet, Entry, Leaf};
use super::trigger::{Mods, PhysicalKey, Trigger, TriggerKey};

/// How many key tables can be active at once (`max_active_key_tables`).
const MAX_ACTIVE_TABLES: usize = 8;

/// A key event's kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Repeat,
    Release,
}

/// A key event as the binds see it (`input.KeyEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent<'a> {
    pub action: KeyAction,
    /// The key's position; [`PhysicalKey::UNIDENTIFIED`] when the frontend cannot place it.
    pub key: PhysicalKey,
    pub mods: Mods,
    /// What the key typed under the layout, with its modifiers; empty for none.
    pub text: &'a str,
    /// The character the key types with no modifier, under the layout.
    pub unshifted: Option<char>,
}

impl KeyEvent<'_> {
    /// `KeyEvent.bindingHash`'s fields: what ties a release to the press a binding consumed.
    fn identity(&self) -> (PhysicalKey, Option<char>, Mods) {
        (self.key, self.unshifted, self.mods.binding())
    }
}

/// Carries out binding actions on the terminal the key reached (the frontend's side of `performBindingAction`).
pub trait Performer {
    /// Performs `action` on this terminal; whether it was performed. An action the frontend does not support is not
    /// performed: a `performable:` binding of it then lets the key through.
    fn perform(&mut self, action: &Action) -> bool;

    /// Performs `action` everywhere, for an `all:` or `global:` binding: an app action once, a terminal action on
    /// every terminal. Always counts as performed. A key-table action or `end_key_sequence` has already been applied
    /// to this terminal's [`Keyboard`]; the other terminals' keyboards take it through [`Keyboard::apply`] (and
    /// [`Performer::flush_sequence`] for their queued keys).
    fn perform_all(&mut self, action: &Action) {
        self.perform(action);
    }

    /// Sends the keys this terminal's half-typed sequence queued to the terminal now, in order (`end_key_sequence`,
    /// before any action chained after it runs).
    fn flush_sequence(&mut self);
}

/// Where the key goes after the binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEffect {
    /// No binding kept it: the encoder gets it (`maybeHandleBinding` returned null).
    Encode,
    /// The first keys of a sequence: encode it into the sequence's queue, not to the terminal yet.
    Queue,
    /// A binding took it; nothing is encoded (`.consumed`).
    Consumed,
    /// An `ignore` binding: neither encoded nor consumed, the window may still act on it (`.ignored`).
    Ignored,
    /// A binding closed the terminal; stop handling the key (`.closed`).
    Closed,
}

/// What happens to the keys a half-typed sequence queued, before [`KeyEffect`] applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queued {
    /// They stay queued.
    Keep,
    /// They go to the terminal now, in order (the sequence broke off or let the key through).
    Flush,
    /// They are dropped (the sequence completed, or an `ignore` ended it).
    Drop,
}

/// What a key event resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyResult {
    pub queued: Queued,
    pub effect: KeyEffect,
}

impl KeyResult {
    const fn new(queued: Queued, effect: KeyEffect) -> Self {
        Self { queued, effect }
    }
}

/// Where a key press goes, in agterm's order: the window's keymap first (custom commands, the built-in chords;
/// upstream's local key monitor and menu, which see a key before the focused terminal), then the terminal, where
/// ghostty's binds take it before the key encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRoute<T> {
    /// agterm's keymap took the key; the terminal never sees it.
    Keymap(T),
    /// The terminal: the binds' verdict, then the encoder for [`KeyEffect::Encode`].
    Terminal(KeyResult),
}

/// One terminal's keyboard state (`Surface.keyboard`): the half-typed sequence and the active key tables.
#[derive(Debug, Clone, Default)]
pub struct Keyboard {
    sequence: Option<Sequence>,
    /// The active key tables, innermost last, each with whether it ends after one binding (`_once`).
    tables: Vec<(String, bool)>,
    /// The press a consumed binding took: its release is consumed too.
    last_trigger: Option<(PhysicalKey, Option<char>, Mods)>,
}

/// A half-typed sequence: the key table it started in (`None` for the root set) and the triggers typed so far.
#[derive(Debug, Clone, Default)]
struct Sequence {
    table: Option<String>,
    path: Vec<Trigger>,
}

/// The binding a key event found, and the sequence it continues (an empty one at the start).
struct Found {
    trigger: Trigger,
    entry: Entry,
    sequence: Sequence,
}

impl Keyboard {
    /// Whether a sequence is half typed.
    #[must_use]
    pub fn in_sequence(&self) -> bool {
        self.sequence.is_some()
    }

    /// The active key tables, innermost last.
    pub fn active_tables(&self) -> impl Iterator<Item = &str> {
        self.tables.iter().map(|(name, _)| name.as_str())
    }

    /// Routes a key: `keymap` first (agterm's keymap; `Some` when it takes the key), then [`Keyboard::handle`].
    pub fn route<T>(
        &mut self,
        keymap: impl FnOnce() -> Option<T>,
        binds: &Keybinds,
        event: &KeyEvent<'_>,
        performer: &mut dyn Performer,
    ) -> KeyRoute<T> {
        match keymap() {
            Some(taken) => KeyRoute::Keymap(taken),
            None => KeyRoute::Terminal(self.handle(binds, event, performer)),
        }
    }

    /// `maybeHandleBinding`: what the binds do with `event`, performing the actions of the binding it fires.
    pub fn handle(&mut self, binds: &Keybinds, event: &KeyEvent<'_>, performer: &mut dyn Performer) -> KeyResult {
        const ENCODE: KeyResult = KeyResult::new(Queued::Keep, KeyEffect::Encode);
        if event.action == KeyAction::Release {
            let consumed = self.last_trigger == Some(event.identity());
            return if consumed {
                KeyResult::new(Queued::Keep, KeyEffect::Consumed)
            } else {
                ENCODE
            };
        }
        let found = match self.sequence.take() {
            Some(sequence) => match sequence_entry(binds, sequence, event) {
                Ok(found) => Some(found),
                Err(sequence) => return self.break_sequence(binds, sequence, event),
            },
            None => self.table_or_root_entry(binds, event, performer),
        };
        let Some(Found {
            trigger,
            entry,
            mut sequence,
        }) = found
        else {
            return ENCODE;
        };
        match entry {
            Entry::Leader(_) => {
                sequence.path.push(trigger);
                self.sequence = Some(sequence);
                KeyResult::new(Queued::Keep, KeyEffect::Queue)
            }
            Entry::Leaf(leaf) => self.fire(binds, &leaf, event, performer),
        }
    }

    /// Applies an action that changes the keyboard itself (`ghostty`'s key-table actions and `end_key_sequence`,
    /// whose queued keys the caller then sends); `None` for any other action. Whether it was performed: a table
    /// activates when it is defined, not already the innermost and below the depth limit; a deactivation needs an
    /// active table.
    pub fn apply(&mut self, binds: &Keybinds, action: &Action) -> Option<bool> {
        Some(match action {
            Action::ActivateKeyTable(name) => self.activate(binds, name, false),
            Action::ActivateKeyTableOnce(name) => self.activate(binds, name, true),
            Action::DeactivateKeyTable => self.tables.pop().is_some(),
            Action::DeactivateAllKeyTables => {
                let active = !self.tables.is_empty();
                self.tables.clear();
                active
            }
            Action::EndKeySequence => {
                self.sequence = None;
                true
            }
            _ => return None,
        })
    }

    /// A key the half-typed sequence does not bind: a modifier waits, an `ignore` catch-all drops the queue and the
    /// key, anything else flushes the queue and goes to the encoder.
    fn break_sequence(&mut self, binds: &Keybinds, sequence: Sequence, event: &KeyEvent<'_>) -> KeyResult {
        if event.key.is_modifier() {
            self.sequence = Some(sequence);
            return KeyResult::new(Queued::Keep, KeyEffect::Encode);
        }
        if self.catch_all_is_ignore(binds) {
            return KeyResult::new(Queued::Drop, KeyEffect::Ignored);
        }
        KeyResult::new(Queued::Flush, KeyEffect::Encode)
    }

    /// The entry of `event` in the active tables, innermost first, then the root set; a `_once` table that fires
    /// at the top of the stack is deactivated first.
    fn table_or_root_entry(
        &mut self,
        binds: &Keybinds,
        event: &KeyEvent<'_>,
        performer: &mut dyn Performer,
    ) -> Option<Found> {
        for (depth, (name, once)) in self.tables.iter().rev().enumerate() {
            let Some((trigger, entry)) = binds.table(name).and_then(|set| set.lookup(event)) else {
                continue;
            };
            let found = Found {
                trigger: *trigger,
                entry: entry.clone(),
                sequence: Sequence {
                    table: Some(name.clone()),
                    path: Vec::new(),
                },
            };
            if *once && depth == 0 {
                self.perform(binds, &Action::DeactivateKeyTable, performer);
            }
            return Some(found);
        }
        let (trigger, entry) = binds.set.lookup(event)?;
        Some(Found {
            trigger: *trigger,
            entry: entry.clone(),
            sequence: Sequence::default(),
        })
    }

    /// Fires a binding's actions and decides where the key goes.
    fn fire(
        &mut self,
        binds: &Keybinds,
        leaf: &Leaf,
        event: &KeyEvent<'_>,
        performer: &mut dyn Performer,
    ) -> KeyResult {
        let everywhere = leaf.flags.intersects(BindFlags::ALL | BindFlags::GLOBAL);
        let consumed = everywhere || leaf.flags.contains(BindFlags::CONSUMED);
        self.last_trigger = None;
        self.sequence = None;
        // every action runs; any one performing counts, and an `all:` binding always does (on this keyboard too)
        let mut done = everywhere;
        for action in &leaf.actions {
            if everywhere {
                self.own(binds, action, performer);
                performer.perform_all(action);
            } else {
                done |= self.perform(binds, action, performer);
            }
        }
        if done && leaf.actions.iter().any(Action::closes) {
            return KeyResult::new(Queued::Drop, KeyEffect::Closed);
        }
        if done && leaf.actions.contains(&Action::Ignore) {
            return KeyResult::new(Queued::Drop, KeyEffect::Ignored);
        }
        if leaf.flags.contains(BindFlags::PERFORMABLE) && !done {
            return KeyResult::new(Queued::Flush, KeyEffect::Encode);
        }
        if consumed {
            self.last_trigger = Some(event.identity());
            return KeyResult::new(Queued::Drop, KeyEffect::Consumed);
        }
        KeyResult::new(Queued::Flush, KeyEffect::Encode)
    }

    /// Performs one action: the keyboard's own (`ignore`, key tables, `end_key_sequence`) here, the rest through the
    /// performer.
    fn perform(&mut self, binds: &Keybinds, action: &Action, performer: &mut dyn Performer) -> bool {
        if *action == Action::Ignore {
            return true;
        }
        self.own(binds, action, performer)
            .unwrap_or_else(|| performer.perform(action))
    }

    /// [`Keyboard::apply`] on this terminal: an `end_key_sequence` sends the queued keys at once, as ghostty's
    /// `endKeySequence(.flush)` does, before any action chained after it.
    fn own(&mut self, binds: &Keybinds, action: &Action, performer: &mut dyn Performer) -> Option<bool> {
        let applied = self.apply(binds, action)?;
        if *action == Action::EndKeySequence {
            performer.flush_sequence();
        }
        Some(applied)
    }

    /// `activate_key_table`: a defined table, not already the innermost, below the depth limit.
    fn activate(&mut self, binds: &Keybinds, name: &str, once: bool) -> bool {
        let defined = binds.table(name).is_some();
        let innermost = self.tables.last().is_some_and(|(active, _)| active == name);
        if !defined || innermost || self.tables.len() >= MAX_ACTIVE_TABLES {
            return false;
        }
        self.tables.push((name.to_owned(), once));
        true
    }

    /// `catchAllIsIgnore`: the innermost `catch_all` binding (tables, then the root) includes `ignore`.
    fn catch_all_is_ignore(&self, binds: &Keybinds) -> bool {
        let catch_all = Trigger::new(TriggerKey::CatchAll, Mods::empty());
        let tables = self.tables.iter().rev().filter_map(|(name, _)| binds.table(name));
        let entry = tables.chain([&binds.set]).find_map(|set: &BindSet| set.get(&catch_all));
        matches!(entry, Some(Entry::Leaf(leaf)) if leaf.actions.contains(&Action::Ignore))
    }
}

/// The entry of `event` in the set a sequence reached; the sequence back when it binds nothing there (or its set is
/// gone with a config reload).
fn sequence_entry(binds: &Keybinds, sequence: Sequence, event: &KeyEvent<'_>) -> Result<Found, Sequence> {
    let mut set = match &sequence.table {
        Some(name) => binds.table(name),
        None => Some(&binds.set),
    };
    for trigger in &sequence.path {
        set = match set.and_then(|set| set.get(trigger)) {
            Some(Entry::Leader(next)) => Some(next),
            _ => None,
        };
    }
    let Some((trigger, entry)) = set.and_then(|set| set.lookup(event)) else {
        return Err(sequence);
    };
    Ok(Found {
        trigger: *trigger,
        entry: entry.clone(),
        sequence,
    })
}
