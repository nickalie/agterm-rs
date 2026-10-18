use super::{
    ENTRIES, Mode, ModePacked, ModeState, ModeTag, PERMANENTLY_RESET_ANSI, Report, ReportTag, State,
    default_configurable, mode_from_int,
};

/// We have this here so that we explicitly fail when we change the size of modes. The size of modes is NOT
/// particularly important, we just want to be mentally aware when it happens.
#[test]
fn mode_packed_size() {
    assert_eq!(8, core::mem::size_of::<ModePacked>());
    assert!(ENTRIES.len() <= 64, "one bit per entry");
}

#[test]
fn order() {
    let t = ModeTag { value: 1, ansi: false };
    assert_eq!(1_u16, t.to_backing());
    assert_eq!(t, ModeTag::from_backing(1));
    assert_eq!(ModeTag { value: 4, ansi: true }, Mode::Insert.tag());
}

/// The table is in the enum's order and every mode has one entry: Zig generates both from one list.
#[test]
fn entries_match_mode() {
    for (i, entry) in ENTRIES.iter().enumerate() {
        assert_eq!(i, entry.mode.index(), "{}", entry.name);
        assert_eq!(Some(entry.mode), Mode::all().nth(i));
    }
    assert_eq!(ENTRIES.len(), Mode::all().count());
    assert_eq!("132_column", Mode::Column132.name());
    assert_eq!("kitty_paste_events", Mode::KittyPasteEvents.name());
}

#[test]
fn mode_from_int_test() {
    assert_eq!(Some(Mode::Insert), mode_from_int(4, true));
    assert_eq!(None, mode_from_int(9, true));
    assert_eq!(Some(Mode::MouseEventX10), mode_from_int(9, false));
    assert_eq!(None, mode_from_int(14, true));

    // Numbers too large for the tag must not be mistaken for an ANSI mode.
    assert_eq!(None, mode_from_int(4 | 0x8000, false));
    assert_eq!(None, mode_from_int(u16::MAX, true));
}

/// Every possible input against a plain scan of the entries. Under Miri a sample: a stride through the range plus
/// every entry's neighbours.
#[test]
fn mode_from_int_matches_entries_exhaustively() {
    let stride = if cfg!(miri) { 257 } else { 1 };
    let mut inputs: Vec<u16> = (0..=u16::MAX).step_by(stride).collect();
    for entry in ENTRIES {
        let v = entry.mode.tag().value;
        inputs.extend([v.saturating_sub(1), v, v.saturating_add(1), v | 0x8000]);
    }
    for ansi in [false, true] {
        for &v in &inputs {
            let expected = ENTRIES
                .iter()
                .find(|entry| entry.mode.tag() == ModeTag { value: v, ansi })
                .map(|entry| entry.mode);
            assert_eq!(expected, mode_from_int(v, ansi), "{v} ansi={ansi}");
        }
    }
}

/// `kitty_paste_events` (5522) is a mode in libghostty-vt on every OS: Ghostty disables it only in its app outside
/// macOS.
#[test]
fn mode_5522_is_enabled_in_the_library() {
    assert_eq!(Some(Mode::KittyPasteEvents), mode_from_int(5522, false));
    let mut state = ModeState::default();
    state.set(Mode::KittyPasteEvents, true);
    let report = state.get_report(ReportTag {
        value: 5522,
        ansi: false,
    });
    assert_eq!(State::Set, report.state);
}

#[test]
fn mode_state() {
    let mut state = ModeState::default();

    // Normal set/get
    assert!(!state.get(Mode::CursorKeys));
    state.set(Mode::CursorKeys, true);
    assert!(state.get(Mode::CursorKeys));

    // Save/restore
    state.save(Mode::CursorKeys);
    state.set(Mode::CursorKeys, false);
    assert!(!state.get(Mode::CursorKeys));
    assert!(state.restore(Mode::CursorKeys));
    assert!(state.get(Mode::CursorKeys));
}

#[test]
fn mode_state_set_default_updates_current_and_reset_value() {
    let mut state = ModeState::default();

    state.set_default(Mode::GraphemeCluster, true);
    assert!(state.get(Mode::GraphemeCluster));
    assert!(state.default.get(Mode::GraphemeCluster));
    assert!(state.get_default(Mode::GraphemeCluster));

    state.set(Mode::GraphemeCluster, false);
    assert!(!state.get(Mode::GraphemeCluster));
    assert!(state.default.get(Mode::GraphemeCluster));

    state.set_default(Mode::GraphemeCluster, true);
    assert!(state.get(Mode::GraphemeCluster));

    state.set(Mode::GraphemeCluster, false);
    state.reset();
    assert!(state.get(Mode::GraphemeCluster));
}

#[test]
fn defaults_follow_the_table() {
    let state = ModeState::default();
    for mode in [
        Mode::SendReceive,
        Mode::Wraparound,
        Mode::CursorVisible,
        Mode::MouseAlternateScroll,
        Mode::IgnoreKeypadWithNumlock,
        Mode::AltEscPrefix,
    ] {
        assert!(state.get(mode), "{}", mode.name());
    }
    assert_eq!(6, Mode::all().filter(|&mode| state.get(mode)).count());
}

#[test]
fn default_configurable_modes() {
    assert!(default_configurable(Mode::GraphemeCluster));
    assert!(default_configurable(Mode::Wraparound));
    assert!(!default_configurable(Mode::AltScreen));
    assert!(!default_configurable(Mode::CursorBlinking));
}

#[test]
fn get_report_known_dec_mode() {
    let mut state = ModeState::default();
    let report = state.get_report(ReportTag { value: 1, ansi: false });
    assert_eq!(State::Reset, report.state);
    assert!(!report.tag.ansi);
    assert_eq!(1, report.tag.value);

    state.set(Mode::CursorKeys, true);
    assert_eq!(State::Set, state.get_report(ReportTag { value: 1, ansi: false }).state);
}

#[test]
fn get_report_known_ansi_mode() {
    let mut state = ModeState::default();
    state.set(Mode::Insert, true);
    let report = state.get_report(ReportTag { value: 4, ansi: true });
    assert_eq!(State::Set, report.state);
    assert!(report.tag.ansi);
}

#[test]
fn get_report_dececm_permanently_reset() {
    let state = ModeState::default();
    let report = state.get_report(ReportTag {
        value: 117,
        ansi: false,
    });
    assert_eq!(State::PermanentlyReset, report.state);
    assert!(!report.tag.ansi);
}

/// Drift `d432b01a3`/`b21f82c0e`: the unimplemented ANSI modes report permanently reset, and none of them has a mode
/// table entry (Zig checks that at compile time).
#[test]
fn get_report_unsupported_ansi_modes_permanently_reset() {
    let state = ModeState::default();
    for value in PERMANENTLY_RESET_ANSI {
        // None of these may gain a settable mode table entry without leaving this list first.
        assert_eq!(None, mode_from_int(value, true));
        let report = state.get_report(ReportTag { value, ansi: true });
        assert_eq!(State::PermanentlyReset, report.state);
        assert!(report.tag.ansi);
        assert_eq!(value, report.tag.value);
    }

    // An unlisted ANSI mode is still unknown, not permanently reset.
    let unknown = state.get_report(ReportTag { value: 9, ansi: true });
    assert_eq!(State::NotRecognized, unknown.state);
    // The DEC modes of the same numbers are not affected.
    assert_eq!(
        State::Reset,
        state.get_report(ReportTag { value: 1, ansi: false }).state
    );
}

#[test]
fn get_report_unknown_mode() {
    let state = ModeState::default();
    let report = state.get_report(ReportTag {
        value: 9999,
        ansi: false,
    });
    assert_eq!(State::NotRecognized, report.state);
}

fn encoded(report: Report) -> String {
    let mut buf = String::new();
    report.encode(&mut buf).unwrap();
    assert!(buf.len() <= Report::MAX_SIZE);
    buf
}

#[test]
fn report_encode() {
    let cases = [
        (1, false, State::Set, "\x1B[?1;1$y"),
        (1, false, State::Reset, "\x1B[?1;2$y"),
        (4, true, State::Set, "\x1B[4;1$y"),
        (9999, false, State::NotRecognized, "\x1B[?9999;0$y"),
        (3, false, State::PermanentlySet, "\x1B[?3;3$y"),
    ];
    for (value, ansi, state, want) in cases {
        assert_eq!(
            want,
            encoded(Report {
                tag: ReportTag { value, ansi },
                state
            })
        );
    }
}

/// These would alias ANSI insert mode and fixed-status DEC mode 117.
#[test]
fn get_report_large_unknown_modes() {
    let state = ModeState::default();
    for tag in [
        ReportTag {
            value: 32772,
            ansi: true,
        },
        ReportTag {
            value: 32885,
            ansi: false,
        },
    ] {
        let report = state.get_report(tag);
        assert_eq!(State::NotRecognized, report.state);
        assert_eq!(tag, report.tag);
    }
}

#[test]
fn report_encode_maximum_size() {
    let report = Report {
        tag: ReportTag {
            value: 65535,
            ansi: false,
        },
        state: State::PermanentlyReset,
    };
    let buf = encoded(report);
    assert_eq!("\x1b[?65535;4$y", buf);
    assert_eq!(Report::MAX_SIZE, buf.len());
}

#[test]
fn report_tag_from_mode() {
    assert_eq!(ReportTag { value: 4, ansi: true }, ReportTag::from_mode(Mode::Insert));
    assert_eq!(
        ReportTag {
            value: 2026,
            ansi: false
        },
        ReportTag::from_mode(Mode::SynchronizedOutput)
    );
}
