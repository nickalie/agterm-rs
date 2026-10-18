//! This file contains all the terminal modes that we support and various support types for them: an enum of supported
//! modes, a packed struct to store mode values, a more generalized state struct to store values plus handle
//! save/restore, and much more.
//!
//! Ghostty: `src/terminal/modes.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/modes.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig generates `Mode` and the packed `ModePacked` from the entry table at comptime; here [`Mode`] is an
//! enum whose discriminants are the packed tags (the mode number, with the ANSI bit on top), [`ModePacked`] is a `u64`
//! with one bit per entry in table order, and the table ([`ENTRIES`]) is written out in that order, with a test that
//! the two agree. Entries have no `disabled` flag: its only user, `kitty_paste_events`, is disabled only outside
//! libghostty-vt and macOS, and this crate is libghostty-vt (`options::ARTIFACT`). The encoders write to a
//! `core::fmt::Write`. Drift absorbed: `d432b01a3` and `b21f82c0e` (DECRQM reports the unimplemented ANSI modes as
//! permanently reset).

use core::fmt::Write;

use crate::datastruct::comptime_int_set::ComptimeIntSet;

/// The ANSI bit of a mode's packed tag (Zig's `ModeTag.ansi`, above the 15-bit value).
const ANSI: u16 = 0x8000;

/// The packed tag of ANSI mode `n`.
const fn ansi(n: u16) -> u16 {
    n | ANSI
}

/// A struct that maintains the state of all the settable modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModeState {
    /// The values of the current modes.
    pub(crate) values: ModePacked,

    /// The saved values. We only allow saving each mode once. This is in line with other terminals that implement
    /// XTSAVE and XTRESTORE. We can improve this in the future if it becomes a real-world issue but we need to be
    /// aware of a `DoS` vector.
    pub(crate) saved: ModePacked,

    /// The default values for the modes. This is used to reset the modes to their default values during reset.
    pub(crate) default: ModePacked,
}

impl ModeState {
    /// Reset the modes to their default values. This also clears the saved state.
    pub(crate) fn reset(&mut self) {
        self.values = self.default;
        self.saved = ModePacked::default();
    }

    /// Set a mode to a value.
    pub(crate) fn set(&mut self, mode: Mode, value: bool) {
        self.values.set(mode, value);
    }

    /// Set the reset default and current value for a mode.
    pub(crate) fn set_default(&mut self, mode: Mode, value: bool) {
        self.values.set(mode, value);
        self.default.set(mode, value);
    }

    /// Get the value of a mode.
    pub(crate) fn get(&self, mode: Mode) -> bool {
        self.values.get(mode)
    }

    /// Get the value a mode is reset to.
    pub(crate) fn get_default(&self, mode: Mode) -> bool {
        self.default.get(mode)
    }

    /// Save the state of the given mode. This can then be restored with restore. This will only be accurate if the
    /// previous mode was saved exactly once and not restored. Otherwise this will just keep restoring the last stored
    /// value in memory.
    pub(crate) fn save(&mut self, mode: Mode) {
        self.saved.set(mode, self.values.get(mode));
    }

    /// See save. This will return the restored value.
    pub(crate) fn restore(&mut self, mode: Mode) -> bool {
        self.values.set(mode, self.saved.get(mode));
        self.values.get(mode)
    }

    /// Return a DECRPM report for the given mode tag. If the tag does not correspond to a known mode, the report state
    /// is [`State::NotRecognized`], except for the permanently-reset modes below.
    pub(crate) fn get_report(&self, tag: ReportTag) -> Report {
        // DECECM (Erase Color Mode, DEC private mode 117) controls whether erasing and scrolling use the default
        // background or the active background color. Ghostty's behavior is fixed equivalent to DECECM reset, and
        // DECRQM has a "permanently reset" response for recognized modes that cannot be changed. Report that instead
        // of "not recognized" so applications can query and adapt to Ghostty's erase-color behavior.
        //
        // See VT520/VT525 Programmer Information, "Erase Color" and DECRQM/DECRPM:
        // https://web.mit.edu/dosathena/doc/www/ek-vt520-rm.pdf
        if !tag.ansi && tag.value == 117 {
            return Report {
                tag,
                state: State::PermanentlyReset,
            };
        }

        // These unimplemented ANSI modes report fixed reset, matching xterm: GATM, SRTM, VEM, HEM, PUM, FEAM, FETM,
        // MATM, TTM, SATM, TSM, EBM. Report permanently_reset instead of not_recognized so DECRQM callers hear
        // "unsupported" rather than "unknown". (A test keeps them out of the mode table, as Zig's comptime check
        // does.)
        if tag.ansi && PERMANENTLY_RESET_ANSI.contains(&tag.value) {
            return Report {
                tag,
                state: State::PermanentlyReset,
            };
        }

        let state = match mode_from_int(tag.value, tag.ansi) {
            None => State::NotRecognized,
            Some(mode) if self.get(mode) => State::Set,
            Some(_) => State::Reset,
        };
        Report { tag, state }
    }
}

/// The unimplemented ANSI modes DECRQM reports as permanently reset: GATM, SRTM, VEM, HEM, PUM, FEAM, FETM, MATM,
/// TTM, SATM, TSM, EBM.
const PERMANENTLY_RESET_ANSI: [u16; 12] = [1, 5, 7, 10, 11, 13, 14, 15, 16, 17, 18, 19];

/// A packed struct of all the settable modes: one bit per entry, in [`ENTRIES`] order. This shouldn't be used directly
/// but rather through the [`ModeState`] struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModePacked(u64);

impl Default for ModePacked {
    /// Every mode at its default value (Zig's field defaults).
    fn default() -> Self {
        let mut bits: u64 = 0;
        let mut i = 0;
        while i < ENTRIES.len() {
            if ENTRIES[i].default {
                bits |= 1_u64 << i;
            }
            i += 1;
        }
        Self(bits)
    }
}

impl ModePacked {
    /// The value of `mode` (Zig's `@field(packed, name)`).
    pub(crate) fn get(self, mode: Mode) -> bool {
        (self.0 >> mode.index()) & 1 == 1
    }

    /// Set the value of `mode`.
    pub(crate) fn set(&mut self, mode: Mode, value: bool) {
        let bit = 1_u64 << mode.index();
        if value {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// An enum(u16) of the available modes. See entries for available values. Each value is the mode's packed tag: its
/// number, with the ANSI bit for an ANSI mode.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    // ANSI
    /// KAM
    DisableKeyboard = ansi(2),
    Insert = ansi(4),
    /// SRM
    SendReceive = ansi(12),
    Linefeed = ansi(20),

    // DEC
    /// DECCKM
    CursorKeys = 1,
    Column132 = 3,
    SlowScroll = 4,
    ReverseColors = 5,
    Origin = 6,
    Wraparound = 7,
    Autorepeat = 8,
    MouseEventX10 = 9,
    CursorBlinking = 12,
    CursorVisible = 25,
    EnableMode3 = 40,
    ReverseWrap = 45,
    AltScreenLegacy = 47,
    KeypadKeys = 66,
    /// DEC Backarrow Key Mode (DECBKM). See <https://vt100.net/dec/ek-vt3xx-tp-002.pdf> page 170. If `false` (the
    /// default), `backspace` emits 0x7f. If `true`, `backspace` emits 0x08.
    BackarrowKey = 67,
    EnableLeftAndRightMargin = 69,
    MouseEventNormal = 1000,
    MouseEventButton = 1002,
    MouseEventAny = 1003,
    FocusEvent = 1004,
    MouseFormatUtf8 = 1005,
    MouseFormatSgr = 1006,
    MouseAlternateScroll = 1007,
    MouseFormatUrxvt = 1015,
    MouseFormatSgrPixels = 1016,
    IgnoreKeypadWithNumlock = 1035,
    AltEscPrefix = 1036,
    AltSendsEscape = 1039,
    ReverseWrapExtended = 1045,
    AltScreen = 1047,
    SaveCursor = 1048,
    AltScreenSaveCursorClearEnter = 1049,
    BracketedPaste = 2004,
    SynchronizedOutput = 2026,
    GraphemeCluster = 2027,
    ReportColorScheme = 2031,
    ReportVisibility = 2033,
    InBandSizeReports = 2048,
    /// Kitty clipboard protocol paste events. When set, a user-initiated paste sends an unsolicited OSC 5522 targets
    /// listing with a one-time password instead of pasting the text. See <https://sw.kovidgoyal.net/kitty/clipboard/>.
    KittyPasteEvents = 5522,
}

impl Mode {
    /// The mode's packed tag (`@intFromEnum`).
    pub(crate) const fn tag(self) -> ModeTag {
        ModeTag::from_backing(self as u16)
    }

    /// The index of the mode's entry in [`ENTRIES`]. For a constant mode the search folds away.
    #[inline]
    fn index(self) -> usize {
        // Every mode has an entry (a test holds the table and the enum together), so the lookup does not fail.
        ENTRY_SET.index_of(self as u16).unwrap_or_default()
    }

    /// Every mode in table order (Zig's `@typeInfo(Mode).@"enum".fields`).
    pub(crate) fn all() -> impl Iterator<Item = Self> {
        ENTRIES.iter().map(|entry| entry.mode)
    }
}

/// The tag type for our enum is a u16 but we use a packed struct in order to pack the ansi bit into the tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ModeTag {
    /// The mode number (Zig `u15`).
    pub(crate) value: u16,
    /// Whether this is an ANSI mode, not a DEC private one.
    pub(crate) ansi: bool,
}

impl ModeTag {
    /// The tag packed into its `u16` (`@bitCast`): the value in the low 15 bits, the ANSI bit on top.
    pub(crate) const fn to_backing(self) -> u16 {
        (self.value & !ANSI) | if self.ansi { ANSI } else { 0 }
    }

    /// The tag of a packed `u16` (`@bitCast`).
    pub(crate) const fn from_backing(int: u16) -> Self {
        Self {
            value: int & !ANSI,
            ansi: int & ANSI != 0,
        }
    }
}

/// Returns the mode for a mode number as it appears in a control sequence, or null if the mode is unknown.
///
/// ANSI and DEC private modes are numbered separately, so the same number can mean two different modes. `ansi`
/// selects which one. For example, `CSI ? 4 h` is `mode_from_int(4, false)` (slow scroll) and `CSI 4 h` is
/// `mode_from_int(4, true)` (insert).
#[inline]
pub(crate) fn mode_from_int(v: u16, ansi: bool) -> Option<Mode> {
    // ModeTag stores the number in 15 bits. A larger number can't be a known mode, and packing it below would spill
    // into the ansi bit.
    if v & ANSI != 0 {
        return None;
    }

    // This runs for every mode that is set, reset, or queried, so we use a sorted lookup instead of comparing against
    // each entry in turn.
    let int = ModeTag { value: v, ansi }.to_backing();
    let idx = ENTRY_SET.index_of(int)?;
    Some(ENTRIES[idx].mode)
}

/// The number of mode entries.
const ENTRIES_LEN: usize = ENTRIES.len();

/// The tag of every entry, in the same order as `entries`. Looking up a tag here returns the index of its entry.
const ENTRY_SET: ComptimeIntSet<u16, ENTRIES_LEN> = ComptimeIntSet::<u16, ENTRIES_LEN>::new({
    let mut keys = [0_u16; ENTRIES_LEN];
    let mut i = 0;
    while i < ENTRIES_LEN {
        keys[i] = ENTRIES[i].mode as u16;
        i += 1;
    }
    keys
});

/// A DECRPM mode report response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Report {
    pub(crate) tag: ReportTag,
    pub(crate) state: State,
}

/// A query identifier can use the full parser parameter range. Keep it separate from [`ModeTag`], which packs
/// supported modes and the ANSI flag into a u16 and is also used by the public C API.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReportTag {
    pub(crate) value: u16,
    pub(crate) ansi: bool,
}

impl ReportTag {
    /// The report tag of `mode`.
    pub(crate) const fn from_mode(mode: Mode) -> Self {
        let tag = mode.tag();
        Self {
            value: tag.value,
            ansi: tag.ansi,
        }
    }
}

/// The state of a mode as reported in a DECRPM response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    NotRecognized = 0,
    Set = 1,
    Reset = 2,
    PermanentlySet = 3,
    PermanentlyReset = 4,
}

impl Report {
    /// The largest possible report in terms of values: `CSI ? 65535 ; 4 $ y`.
    pub(crate) const MAX_SIZE: usize = "\x1B[?65535;4$y".len();

    /// Encode the DECRPM report sequence.
    pub(crate) fn encode<W: Write>(self, writer: &mut W) -> core::fmt::Result {
        write!(
            writer,
            "\x1B[{}{};{}$y",
            if self.tag.ansi { "" } else { "?" },
            self.tag.value,
            self.state as u8,
        )
    }
}

/// A single entry of a possible mode we support. This is used to define the enum's table.
struct ModeEntry {
    /// The mode (its tag carries the value and the ANSI bit).
    mode: Mode,
    /// The mode's name, as Zig spells the `Mode` field.
    name: &'static str,
    default: bool,
    /// Whether an embedder may safely configure this bit as reset policy. Modes that perform a transition or mirror
    /// additional terminal state must use their semantic configuration API instead.
    default_configurable: bool,
}

/// Return whether a mode can safely be configured as a reset default by an embedder. This excludes modes whose
/// set/reset operations have side effects beyond changing the mode bit.
pub(crate) fn default_configurable(mode: Mode) -> bool {
    ENTRIES[mode.index()].default_configurable
}

/// An entry with the defaults of Zig's `ModeEntry`: off, configurable.
const fn e(mode: Mode, name: &'static str) -> ModeEntry {
    ModeEntry {
        mode,
        name,
        default: false,
        default_configurable: true,
    }
}

/// `.default = true`.
const fn on(mut entry: ModeEntry) -> ModeEntry {
    entry.default = true;
    entry
}

/// `.default_configurable = false`.
const fn fixed(mut entry: ModeEntry) -> ModeEntry {
    entry.default_configurable = false;
    entry
}

/// The full list of available entries. For documentation see how they're used within Ghostty or google their values.
/// It is not valuable to redocument them all here.
const ENTRIES: &[ModeEntry] = &[
    // ANSI
    e(Mode::DisableKeyboard, "disable_keyboard"),
    e(Mode::Insert, "insert"),
    on(e(Mode::SendReceive, "send_receive_mode")),
    e(Mode::Linefeed, "linefeed"),
    // DEC
    e(Mode::CursorKeys, "cursor_keys"),
    fixed(e(Mode::Column132, "132_column")),
    e(Mode::SlowScroll, "slow_scroll"),
    e(Mode::ReverseColors, "reverse_colors"),
    fixed(e(Mode::Origin, "origin")),
    on(e(Mode::Wraparound, "wraparound")),
    e(Mode::Autorepeat, "autorepeat"),
    fixed(e(Mode::MouseEventX10, "mouse_event_x10")),
    fixed(e(Mode::CursorBlinking, "cursor_blinking")),
    on(e(Mode::CursorVisible, "cursor_visible")),
    e(Mode::EnableMode3, "enable_mode_3"),
    e(Mode::ReverseWrap, "reverse_wrap"),
    fixed(e(Mode::AltScreenLegacy, "alt_screen_legacy")),
    e(Mode::KeypadKeys, "keypad_keys"),
    e(Mode::BackarrowKey, "backarrow_key_mode"),
    fixed(e(Mode::EnableLeftAndRightMargin, "enable_left_and_right_margin")),
    fixed(e(Mode::MouseEventNormal, "mouse_event_normal")),
    fixed(e(Mode::MouseEventButton, "mouse_event_button")),
    fixed(e(Mode::MouseEventAny, "mouse_event_any")),
    e(Mode::FocusEvent, "focus_event"),
    fixed(e(Mode::MouseFormatUtf8, "mouse_format_utf8")),
    fixed(e(Mode::MouseFormatSgr, "mouse_format_sgr")),
    on(e(Mode::MouseAlternateScroll, "mouse_alternate_scroll")),
    fixed(e(Mode::MouseFormatUrxvt, "mouse_format_urxvt")),
    fixed(e(Mode::MouseFormatSgrPixels, "mouse_format_sgr_pixels")),
    on(e(Mode::IgnoreKeypadWithNumlock, "ignore_keypad_with_numlock")),
    on(e(Mode::AltEscPrefix, "alt_esc_prefix")),
    e(Mode::AltSendsEscape, "alt_sends_escape"),
    e(Mode::ReverseWrapExtended, "reverse_wrap_extended"),
    fixed(e(Mode::AltScreen, "alt_screen")),
    fixed(e(Mode::SaveCursor, "save_cursor")),
    fixed(e(
        Mode::AltScreenSaveCursorClearEnter,
        "alt_screen_save_cursor_clear_enter",
    )),
    e(Mode::BracketedPaste, "bracketed_paste"),
    fixed(e(Mode::SynchronizedOutput, "synchronized_output")),
    e(Mode::GraphemeCluster, "grapheme_cluster"),
    e(Mode::ReportColorScheme, "report_color_scheme"),
    fixed(e(Mode::ReportVisibility, "report_visibility")),
    e(Mode::InBandSizeReports, "in_band_size_reports"),
    // The macOS app and libghostty-vt can both serve the follow-up Kitty clipboard read that a paste event grants.
    e(Mode::KittyPasteEvents, "kitty_paste_events"),
];

impl Mode {
    /// The mode's name, as Zig spells the `Mode` field (`@tagName`).
    pub(crate) fn name(self) -> &'static str {
        ENTRIES[self.index()].name
    }
}

#[cfg(test)]
mod tests;
