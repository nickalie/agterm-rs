//! The state transition table for VT emulation, built at compile time.
//!
//! This is based on the vt100.net state machine: <https://vt100.net/emu/dec_ansi_parser>. But has some modifications:
//!
//!   * `csi_param` accepts the colon character (`:`) since the SGR command accepts colon as a valid parameter value.
//!
//! Ghostty: `src/terminal/parse_table.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/parse_table.rs` at `RUSTTY_REV` (MIT).
//! Changes: the table is built by a `const fn` into a `static`, one helper per source state (Zig's single comptime
//! function); a later transition for the same byte and state replaces an earlier one, as in Zig.

use crate::terminal::parser::{State, TransitionAction as Action};

/// The state transition table, indexed by byte and then by state.
pub(crate) static TABLE: Table = gen_table();

/// Table is the type of the state table: `[u8][State]Transition`.
pub(crate) type Table = [[Transition; State::COUNT]; 256];

/// The table under construction: `None` where no transition was set yet.
type OptionalTable = [[Option<Transition>; State::COUNT]; 256];

/// Transition is the transition to take within the table
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Transition {
    pub(crate) state: State,
    pub(crate) action: Action,
}

/// The transition from `s0` on byte `c` to `s1`, taking `a`.
const fn single(t: &mut OptionalTable, c: u8, s0: State, s1: State, a: Action) {
    t[c as usize][s0 as usize] = Some(Transition { state: s1, action: a });
}

/// The transitions from `s0` on every byte in `from..=to` to `s1`, taking `a`.
const fn range(t: &mut OptionalTable, from: u8, to: u8, s0: State, s1: State, a: Action) {
    let mut i = from;
    loop {
        single(t, i, s0, s1, a);
        // If 'to' is 0xFF, our next pass would overflow. Return early to prevent the loop from executing its continue
        // expression.
        if i == to {
            break;
        }
        i += 1;
    }
}

/// The C0 controls that `s` executes (or `a`s) without leaving it: 0x00-0x17, 0x19, 0x1C-0x1F.
const fn events(t: &mut OptionalTable, s: State, a: Action) {
    single(t, 0x19, s, s, a);
    range(t, 0, 0x17, s, s, a);
    range(t, 0x1C, 0x1F, s, s, a);
}

/// The transitions from any state.
const fn anywhere(t: &mut OptionalTable) {
    let mut fi = 0;
    while fi < State::ALL.len() {
        let source = State::ALL[fi];

        // anywhere => ground
        single(t, 0x18, source, State::Ground, Action::Execute);
        single(t, 0x1A, source, State::Ground, Action::Execute);
        range(t, 0x80, 0x8F, source, State::Ground, Action::Execute);
        range(t, 0x91, 0x97, source, State::Ground, Action::Execute);
        single(t, 0x99, source, State::Ground, Action::Execute);
        single(t, 0x9A, source, State::Ground, Action::Execute);
        single(t, 0x9C, source, State::Ground, Action::None);

        // anywhere => escape
        single(t, 0x1B, source, State::Escape, Action::None);

        // anywhere => sos_pm_apc_string
        single(t, 0x98, source, State::SosPmApcString, Action::None);
        single(t, 0x9E, source, State::SosPmApcString, Action::None);
        single(t, 0x9F, source, State::SosPmApcString, Action::None);

        // anywhere => csi_entry
        single(t, 0x9B, source, State::CsiEntry, Action::None);

        // anywhere => dcs_entry
        single(t, 0x90, source, State::DcsEntry, Action::None);

        // anywhere => osc_string
        single(t, 0x9D, source, State::OscString, Action::None);

        fi += 1;
    }
}

/// ground, `escape_intermediate` and `sos_pm_apc_string`.
const fn ground_and_strings(t: &mut OptionalTable) {
    // ground
    events(t, State::Ground, Action::Execute);
    range(t, 0x20, 0x7F, State::Ground, State::Ground, Action::Print);

    // escape_intermediate
    let source = State::EscapeIntermediate;
    events(t, source, Action::Execute);
    range(t, 0x20, 0x2F, source, source, Action::Collect);
    single(t, 0x7F, source, source, Action::Ignore);
    // => ground
    range(t, 0x30, 0x7E, source, State::Ground, Action::EscDispatch);

    // sos_pm_apc_string
    let source = State::SosPmApcString;
    events(t, source, Action::ApcPut);
    range(t, 0x20, 0x7F, source, source, Action::ApcPut);
}

/// escape
const fn escape(t: &mut OptionalTable) {
    let source = State::Escape;

    // events
    events(t, source, Action::Execute);
    single(t, 0x7F, source, source, Action::Ignore);

    // => ground
    range(t, 0x30, 0x4F, source, State::Ground, Action::EscDispatch);
    range(t, 0x51, 0x57, source, State::Ground, Action::EscDispatch);
    range(t, 0x60, 0x7E, source, State::Ground, Action::EscDispatch);
    single(t, 0x59, source, State::Ground, Action::EscDispatch);
    single(t, 0x5A, source, State::Ground, Action::EscDispatch);
    single(t, 0x5C, source, State::Ground, Action::EscDispatch);

    // => escape_intermediate
    range(t, 0x20, 0x2F, source, State::EscapeIntermediate, Action::Collect);

    // => sos_pm_apc_string
    single(t, 0x58, source, State::SosPmApcString, Action::None);
    single(t, 0x5E, source, State::SosPmApcString, Action::None);
    single(t, 0x5F, source, State::SosPmApcString, Action::None);

    // => dcs_entry
    single(t, 0x50, source, State::DcsEntry, Action::None);

    // => csi_entry
    single(t, 0x5B, source, State::CsiEntry, Action::None);

    // => osc_string
    single(t, 0x5D, source, State::OscString, Action::None);
}

/// `dcs_entry` and `dcs_intermediate`
const fn dcs_entry(t: &mut OptionalTable) {
    let source = State::DcsEntry;
    events(t, source, Action::Ignore);
    single(t, 0x7F, source, source, Action::Ignore);
    // => dcs_intermediate
    range(t, 0x20, 0x2F, source, State::DcsIntermediate, Action::Collect);
    // => dcs_ignore
    single(t, 0x3A, source, State::DcsIgnore, Action::None);
    // => dcs_param
    range(t, 0x30, 0x39, source, State::DcsParam, Action::Param);
    single(t, 0x3B, source, State::DcsParam, Action::Param);
    range(t, 0x3C, 0x3F, source, State::DcsParam, Action::Collect);
    // => dcs_passthrough
    range(t, 0x40, 0x7E, source, State::DcsPassthrough, Action::None);

    let source = State::DcsIntermediate;
    events(t, source, Action::Ignore);
    range(t, 0x20, 0x2F, source, source, Action::Collect);
    single(t, 0x7F, source, source, Action::Ignore);
    // => dcs_ignore
    range(t, 0x30, 0x3F, source, State::DcsIgnore, Action::None);
    // => dcs_passthrough
    range(t, 0x40, 0x7E, source, State::DcsPassthrough, Action::None);
}

/// `dcs_ignore`, `dcs_param` and `dcs_passthrough`
const fn dcs_rest(t: &mut OptionalTable) {
    let source = State::DcsIgnore;
    events(t, source, Action::Ignore);
    // High bytes are ignored payload data, overriding the "anywhere" C1 transitions. See dcs_passthrough below for
    // more. In dcs_ignore the additional concern is that a UTF-8 payload in an ignored DCS could otherwise begin a live
    // sequence mid-string (e.g. 0x9B => csi_entry).
    range(t, 0x80, 0xFF, source, source, Action::Ignore);

    let source = State::DcsParam;
    events(t, source, Action::Ignore);
    range(t, 0x30, 0x39, source, source, Action::Param);
    single(t, 0x3B, source, source, Action::Param);
    single(t, 0x7F, source, source, Action::Ignore);
    // => dcs_ignore
    single(t, 0x3A, source, State::DcsIgnore, Action::None);
    range(t, 0x3C, 0x3F, source, State::DcsIgnore, Action::None);
    // => dcs_intermediate
    range(t, 0x20, 0x2F, source, State::DcsIntermediate, Action::Collect);
    // => dcs_passthrough
    range(t, 0x40, 0x7E, source, State::DcsPassthrough, Action::None);

    let source = State::DcsPassthrough;
    events(t, source, Action::Put);
    range(t, 0x20, 0x7E, source, source, Action::Put);
    single(t, 0x7F, source, source, Action::Ignore);
    // High bytes are payload data, overriding the "anywhere" C1 transitions, matching how osc_string handles them
    // below. Ghostty is UTF-8 only, and DCS payloads carry UTF-8 text (e.g. tmux control mode pane content): without
    // this, a UTF-8 continuation byte in the C1 range terminates or corrupts the string and 0xA0-0xFF are silently
    // dropped.
    //
    // This includes 0x9C (8-bit ST) on purpose: a raw 0x9C is indistinguishable from a UTF-8 continuation byte ("Ü" is
    // 0xC3 0x9C), and Ghostty doesn't honor 8-bit C1 controls in the ground state either (they go through UTF-8
    // decoding). DCS strings terminate via 7-bit ST (ESC \) and abort via CAN/SUB, which are unaffected here.
    range(t, 0x80, 0xFF, source, source, Action::Put);
}

/// `csi_param`, `csi_ignore` and `csi_intermediate`
const fn csi_param(t: &mut OptionalTable) {
    let source = State::CsiParam;
    events(t, source, Action::Execute);
    range(t, 0x30, 0x39, source, source, Action::Param);
    single(t, 0x3A, source, source, Action::Param);
    single(t, 0x3B, source, source, Action::Param);
    single(t, 0x7F, source, source, Action::Ignore);
    // => ground
    range(t, 0x40, 0x7E, source, State::Ground, Action::CsiDispatch);
    // => csi_ignore
    range(t, 0x3C, 0x3F, source, State::CsiIgnore, Action::None);
    // => csi_intermediate
    range(t, 0x20, 0x2F, source, State::CsiIntermediate, Action::Collect);

    let source = State::CsiIgnore;
    events(t, source, Action::Execute);
    range(t, 0x20, 0x3F, source, source, Action::Ignore);
    single(t, 0x7F, source, source, Action::Ignore);
    // => ground
    range(t, 0x40, 0x7E, source, State::Ground, Action::None);

    let source = State::CsiIntermediate;
    events(t, source, Action::Execute);
    range(t, 0x20, 0x2F, source, source, Action::Collect);
    single(t, 0x7F, source, source, Action::Ignore);
    // => ground
    range(t, 0x40, 0x7E, source, State::Ground, Action::CsiDispatch);
    // => csi_ignore
    range(t, 0x30, 0x3F, source, State::CsiIgnore, Action::None);
}

/// `csi_entry` and `osc_string`
const fn csi_entry_and_osc(t: &mut OptionalTable) {
    let source = State::CsiEntry;
    events(t, source, Action::Execute);
    single(t, 0x7F, source, source, Action::Ignore);
    // => ground
    range(t, 0x40, 0x7E, source, State::Ground, Action::CsiDispatch);
    // => csi_ignore
    single(t, 0x3A, source, State::CsiIgnore, Action::None);
    // => csi_intermediate
    range(t, 0x20, 0x2F, source, State::CsiIntermediate, Action::Collect);
    // => csi_param
    range(t, 0x30, 0x39, source, State::CsiParam, Action::Param);
    single(t, 0x3B, source, State::CsiParam, Action::Param);
    range(t, 0x3C, 0x3F, source, State::CsiParam, Action::Collect);

    let source = State::OscString;
    single(t, 0x19, source, source, Action::Ignore);
    range(t, 0, 0x06, source, source, Action::Ignore);
    range(t, 0x08, 0x17, source, source, Action::Ignore);
    range(t, 0x1C, 0x1F, source, source, Action::Ignore);
    range(t, 0x20, 0xFF, source, source, Action::OscPut);
    // XTerm accepts either BEL or ST for terminating OSC sequences, and when returning information, uses the same
    // terminator used in a query.
    single(t, 0x07, source, State::Ground, Action::None);
}

/// Function to generate the full state transition table for VT emulation.
const fn gen_table() -> Table {
    // We accumulate using an "optional" table so we can detect unset transitions.
    let mut result: OptionalTable = [[None; State::COUNT]; 256];
    anywhere(&mut result);
    ground_and_strings(&mut result);
    escape(&mut result);
    dcs_entry(&mut result);
    dcs_rest(&mut result);
    csi_param(&mut result);
    csi_entry_and_osc(&mut result);

    // Create our immutable version: an unset transition stays in its state and does nothing.
    let mut table: Table = [[Transition {
        state: State::Ground,
        action: Action::None,
    }; State::COUNT]; 256];
    let mut i = 0;
    while i < 256 {
        let mut j = 0;
        while j < State::COUNT {
            table[i][j] = match result[i][j] {
                Some(t) => t,
                None => Transition {
                    state: State::ALL[j],
                    action: Action::None,
                },
            };
            j += 1;
        }
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests;
