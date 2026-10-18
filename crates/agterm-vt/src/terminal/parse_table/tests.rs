use super::TABLE;
use crate::terminal::parser::{State, TransitionAction as Action};

/// Zig's test forces the comptime evaluation of the table; here it is a `static`, and its shape is checked.
#[test]
fn force_table_creation() {
    assert_eq!(256, TABLE.len());
    assert_eq!(State::COUNT, TABLE[0].len());
}

/// Bytes 0x80-0xFF within a DCS string are payload data, not C1 controls. This includes 0x9C (8-bit ST): a raw 0x9C is
/// indistinguishable from a UTF-8 continuation byte (e.g. "Ü" is 0xC3 0x9C) and Ghostty doesn't support 8-bit C1
/// controls anywhere else.
#[test]
fn dcs_passthrough_high_bytes_are_payload_data() {
    for entry in &TABLE[0x80..] {
        let entry = entry[State::DcsPassthrough as usize];
        assert_eq!(State::DcsPassthrough, entry.state);
        assert_eq!(Action::Put, entry.action);
    }
}

/// Same as `dcs_passthrough`: a UTF-8 payload inside an ignored DCS must not trigger "anywhere" C1 transitions (e.g.
/// 0x9B beginning a CSI mid-string).
#[test]
fn dcs_ignore_high_bytes_are_ignored_payload_data() {
    for entry in &TABLE[0x80..] {
        let entry = entry[State::DcsIgnore as usize];
        assert_eq!(State::DcsIgnore, entry.state);
        assert_eq!(Action::Ignore, entry.action);
    }
}

/// 7-bit ST (ESC \) is the DCS terminator and CAN/SUB abort, so these must continue to leave `dcs_passthrough`.
/// `dcs_unhook` is emitted by the parser on any transition out of `dcs_passthrough`.
#[test]
fn dcs_passthrough_esc_can_and_sub_still_exit() {
    assert_eq!(State::Escape, TABLE[0x1B][State::DcsPassthrough as usize].state);
    assert_eq!(State::Ground, TABLE[0x18][State::DcsPassthrough as usize].state);
    assert_eq!(State::Ground, TABLE[0x1A][State::DcsPassthrough as usize].state);
}

/// The "anywhere" transitions hold in states that do not override them, and an unset transition stays put.
#[test]
fn anywhere_transitions_and_defaults() {
    for state in State::ALL {
        assert_eq!(State::Escape, TABLE[0x1B][state as usize].state, "{state:?}");
        assert_eq!(State::Ground, TABLE[0x18][state as usize].state, "{state:?}");
    }
    assert_eq!(State::CsiEntry, TABLE[0x9B][State::Ground as usize].state);
    assert_eq!(Action::OscPut, TABLE[0x9B][State::OscString as usize].action);
    // csi_entry has no transition for 0x80 other than anywhere's execute
    assert_eq!(Action::Execute, TABLE[0x80][State::CsiEntry as usize].action);
    // sos_pm_apc_string sets nothing for 0xA0: it stays in its state and does nothing
    let unset = TABLE[0xA0][State::SosPmApcString as usize];
    assert_eq!((State::SosPmApcString, Action::None), (unset.state, unset.action));
    // BEL ends an OSC
    assert_eq!(State::Ground, TABLE[0x07][State::OscString as usize].state);
    assert_eq!(Action::Print, TABLE[usize::from(b'a')][State::Ground as usize].action);
}
