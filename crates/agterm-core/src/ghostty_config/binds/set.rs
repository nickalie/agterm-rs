//! A binding line and the set it goes into, ported from `Binding.Parser` and `Binding.Set` (`parseAndPut`,
//! `putFlags`, `appendChain`, `remove`, `getEvent`) at `GHOSTTY_REV`.

use std::collections::HashMap;

use bitflags::bitflags;

use super::BindError;
use super::action::Action;
use super::keyboard::KeyEvent;
use super::trigger::{Mods, Trigger, TriggerKey};

bitflags! {
    /// A binding's flags (`Binding.Flags`), set by its `all:`, `global:`, `unconsumed:` and `performable:` prefixes.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct BindFlags: u8 {
        /// The key goes no further once the binding fires (off with `unconsumed:`).
        const CONSUMED = 1;
        /// The action runs on every terminal (`all:`).
        const ALL = 1 << 1;
        /// A system-wide binding (`global:`); in the app it acts as `all:`.
        const GLOBAL = 1 << 2;
        /// The binding exists only when its action can be performed (`performable:`).
        const PERFORMABLE = 1 << 3;
    }
}

impl Default for BindFlags {
    fn default() -> Self {
        Self::CONSUMED
    }
}

/// One `keybind` binding, parsed and checked whole before anything changes (`Parser.init` and a full iteration).
#[derive(Debug, Clone, PartialEq)]
pub struct BindingLine {
    pub flags: BindFlags,
    /// A `chain=ACTION` line, which adds its action to the binding set before it.
    pub chain: bool,
    /// The trigger sequence (`a>b>c`); empty for a chain.
    pub triggers: Vec<Trigger>,
    pub action: Action,
}

impl BindingLine {
    /// `Parser.init`: flags, then the trigger(s) up to the real `=`, then the action; every trigger is parsed too.
    ///
    /// # Errors
    ///
    /// [`BindError::InvalidAction`] for an unknown action, else [`BindError::InvalidFormat`].
    pub fn parse(raw: &str) -> Result<Self, BindError> {
        let (flags, input) = parse_flags(raw)?;
        let equals = trigger_end(input).ok_or(BindError::InvalidFormat)?;
        let trigger_text = &input[..equals];
        let chain = trigger_text == "chain";
        if chain && flags != BindFlags::default() {
            return Err(BindError::InvalidFormat);
        }
        let action = input[equals + 1..].parse()?;
        let triggers = if chain {
            Vec::new()
        } else {
            let triggers = sequence(trigger_text)?;
            if triggers.len() > 1 && flags.intersects(BindFlags::ALL | BindFlags::GLOBAL) {
                return Err(BindError::InvalidFormat);
            }
            triggers
        };
        Ok(Self {
            flags,
            chain,
            triggers,
            action,
        })
    }
}

/// `Parser.parseFlags`: the known `prefix:` flags in front, each at most once; the first other prefix ends them.
fn parse_flags(raw: &str) -> Result<(BindFlags, &str), BindError> {
    let mut flags = BindFlags::default();
    let mut input = raw;
    while let Some((prefix, rest)) = input.split_once(':') {
        let (flag, set) = match prefix {
            "all" => (BindFlags::ALL, true),
            "global" => (BindFlags::GLOBAL, true),
            "unconsumed" => (BindFlags::CONSUMED, false),
            "performable" => (BindFlags::PERFORMABLE, true),
            _ => break,
        };
        if flags.contains(flag) == set {
            return Err(BindError::InvalidFormat);
        }
        flags.set(flag, set);
        input = rest;
    }
    Ok((flags, input))
}

/// The `=` between trigger and action: the first one not followed by `+` or `=` (those belong to a trigger such as
/// `ctrl+==action` or `=+ctrl=action`).
fn trigger_end(input: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut offset = 0;
    while let Some(found) = input[offset..].find('=') {
        let index = offset + found;
        if matches!(bytes.get(index + 1), Some(b'+' | b'=')) {
            offset = index + 1;
            continue;
        }
        return Some(index);
    }
    None
}

/// `SequenceIterator`: the `>`-separated triggers; an empty one (`a>`, `>a`) is malformed.
fn sequence(input: &str) -> Result<Vec<Trigger>, BindError> {
    input.split('>').map(Trigger::parse).collect()
}

/// What a trigger leads to.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// The first key of a sequence: the set its next key is looked up in.
    Leader(BindSet),
    /// A binding.
    Leaf(Leaf),
}

/// A binding's actions (more than one once `chain=` lines extend it) and flags.
#[derive(Debug, Clone, PartialEq)]
pub struct Leaf {
    pub actions: Vec<Action>,
    pub flags: BindFlags,
}

/// A set of bindings (`Binding.Set`): the root set, a key table, or a sequence's next keys.
#[derive(Debug, Clone, Default)]
pub struct BindSet {
    bindings: HashMap<Trigger, Entry>,
    /// The binding a `chain=` line extends: the triggers leading to it from this set.
    chain_parent: Option<Vec<Trigger>>,
}

/// As ghostty's `Keybinds.equal`: the bindings, not the chain state.
impl PartialEq for BindSet {
    fn eq(&self, other: &Self) -> bool {
        self.bindings == other.bindings
    }
}

/// What a put below a set did.
enum Put {
    /// A binding was set, at these triggers from the set.
    Added(Vec<Trigger>),
    /// A binding was removed and its set left empty: the parent removes its leader too (`error.SequenceUnbind`).
    Unbound,
    /// Nothing was added.
    Nothing,
}

impl BindSet {
    /// Whether the set binds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// The entry of exactly `trigger` (unicode keys case-folded).
    #[must_use]
    pub fn get(&self, trigger: &Trigger) -> Option<&Entry> {
        self.bindings.get(trigger)
    }

    /// Every trigger and its entry, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = (&Trigger, &Entry)> {
        self.bindings.iter()
    }

    /// `Set.putFlags`: binds `trigger` to `action`, over whatever it bound; a `chain=` line extends it next.
    pub fn put(&mut self, trigger: Trigger, action: Action, flags: BindFlags) {
        self.bindings.insert(
            trigger,
            Entry::Leaf(Leaf {
                actions: vec![action],
                flags,
            }),
        );
        self.chain_parent = Some(vec![trigger]);
    }

    /// `Set.remove`: unbinds `trigger`; nothing is left to chain to.
    pub fn remove(&mut self, trigger: &Trigger) -> Option<Entry> {
        self.chain_parent = None;
        self.bindings.remove(trigger)
    }

    /// `Set.parseAndPut`: one binding line into the set (a key table's or the root's).
    ///
    /// # Errors
    ///
    /// What [`BindingLine::parse`] refuses, a `chain=unbind`, and a `chain=` with no binding before it to extend
    /// ([`BindError::InvalidFormat`]); nothing changes then.
    pub fn parse_and_put(&mut self, input: &str) -> Result<(), BindError> {
        let line = BindingLine::parse(input)?;
        if line.chain {
            return self.append_chain(line.action);
        }
        self.chain_parent = match put_below(self, &line.triggers, &line.action, line.flags) {
            Put::Added(path) => Some(path),
            Put::Unbound | Put::Nothing => None,
        };
        Ok(())
    }

    /// `Set.appendChain`: one more action for the binding the last put set.
    fn append_chain(&mut self, action: Action) -> Result<(), BindError> {
        if action == Action::Unbind {
            return Err(BindError::InvalidFormat);
        }
        let path = self.chain_parent.clone().ok_or(BindError::InvalidFormat)?;
        let (last, leaders) = path.split_last().ok_or(BindError::InvalidFormat)?;
        let mut set = self;
        for leader in leaders {
            set = match set.bindings.get_mut(leader) {
                Some(Entry::Leader(next)) => next,
                _ => return Err(BindError::InvalidFormat),
            };
        }
        match set.bindings.get_mut(last) {
            Some(Entry::Leaf(leaf)) => {
                leaf.actions.push(action);
                Ok(())
            }
            _ => Err(BindError::InvalidFormat),
        }
    }

    /// `Set.getEvent`: the entry for a key event, tried as its physical key, the one character it typed, its
    /// unshifted character, then `catch_all` with its modifiers and without any.
    #[must_use]
    pub fn lookup(&self, event: &KeyEvent<'_>) -> Option<(&Trigger, &Entry)> {
        let mods = event.mods.binding();
        let mut keys = vec![TriggerKey::Physical(event.key)];
        let mut typed = event.text.chars();
        if let (Some(character), None) = (typed.next(), typed.next()) {
            keys.push(TriggerKey::Unicode(character));
        }
        keys.extend(event.unshifted.map(TriggerKey::Unicode));
        keys.into_iter()
            .chain([TriggerKey::CatchAll])
            .map(|key| Trigger::new(key, mods))
            .chain((!mods.is_empty()).then(|| Trigger::new(TriggerKey::CatchAll, Mods::empty())))
            .find_map(|trigger| self.bindings.get_key_value(&trigger))
    }
}

/// `parseAndPutRecurse`: the rest of a trigger sequence below `set`.
fn put_below(set: &mut BindSet, triggers: &[Trigger], action: &Action, flags: BindFlags) -> Put {
    let Some((first, rest)) = triggers.split_first() else {
        return Put::Nothing;
    };
    if rest.is_empty() {
        if *action == Action::Unbind {
            set.remove(first);
            return Put::Unbound;
        }
        set.put(*first, action.clone(), flags);
        return Put::Added(vec![*first]);
    }
    if let Some(Entry::Leader(next)) = set.bindings.get_mut(first) {
        return match put_below(next, rest, action, flags) {
            // the child unbound: an emptied leader goes too
            Put::Unbound if next.is_empty() => {
                set.remove(first);
                Put::Unbound
            }
            Put::Unbound | Put::Nothing => Put::Nothing,
            Put::Added(path) => Put::Added(prefixed(*first, path)),
        };
    }
    // a leaf there gives way to the new leader, and comes back if the sequence only unbinds
    let old = set.remove(first);
    let mut next = BindSet::default();
    match put_below(&mut next, rest, action, flags) {
        Put::Unbound => {
            if let Some(old) = old {
                set.bindings.insert(*first, old);
            }
            Put::Nothing
        }
        put => {
            set.bindings.insert(*first, Entry::Leader(next));
            match put {
                Put::Added(path) => Put::Added(prefixed(*first, path)),
                _ => Put::Nothing,
            }
        }
    }
}

fn prefixed(first: Trigger, path: Vec<Trigger>) -> Vec<Trigger> {
    std::iter::once(first).chain(path).collect()
}
