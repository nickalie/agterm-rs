//! The stream's CSI dispatch: a CSI's final byte, intermediates and parameters turned into handler calls.
//!
//! Ghostty: `src/terminal/stream.zig` (`csiDispatch`) at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/stream/csi_dispatch.rs` at `RUSTTY_REV` (MIT).
//! Changes: Ghostty's `csiDispatch` is one switch over the final byte; here each final (or group of finals) is a
//! function, and the commands that take one count are a table ([`Counted`]). The branches and their log lines are
//! Ghostty's. DECRQCRA and XTCHECKSUM (`CSI * y`, `CSI # y`) are not dispatched (see the stream's module doc). Drift
//! absorbed: `6104c78f1` (CSI 19 t), `d0c0d4008` (TBC with no parameter), `fbda61b87` (DECXCPR, through
//! [`device_status::req_from_int`]).

use super::{
    CursorMovement, CursorPos, DeviceStatus, Handler, KittyKeyboardFlags, LOG_SCOPE, Margin, Mode, RawMode,
    ResizeWindow, SEEN_CSI_22_23_T_EXTRA_PARAMS, SEEN_CSI_T_UNIMPLEMENTED_PARAM, SEEN_INVALID_DEVICE_ATTRIBUTES,
    SEEN_UNIMPLEMENTED_MODE, SEEN_UNKNOWN_CSI_M_INTERMEDIATE, log_unsupported_once,
};
use crate::terminal::parser::Csi;
use crate::terminal::{ansi, csi, device_attributes, device_status, kitty, modes, sgr};

/// Dispatch one CSI to the handler.
pub(super) fn csi_dispatch<H: Handler>(handler: &mut H, input: &Csi) {
    // The most common CSI finals in real world data are, in order, m, H, K, A, C, X, l, h and r: together about 96% of
    // all CSI sequences (ref: https://github.com/qwerasd205/asciinema-stats).
    if let Some(counted) = Counted::from_final(input.final_) {
        counted.dispatch(handler, input);
        return;
    }
    match input.final_ {
        b'H' | b'f' => cursor_pos(handler, input),
        b'J' => erase_display(handler, input),
        b'K' => erase_line(handler, input),
        b'W' => tab_control(handler, input),
        b'c' => device_attributes(handler, input),
        b'g' => tab_clear(handler, input),
        b'h' => set_or_reset_mode(handler, input, true),
        b'l' => set_or_reset_mode(handler, input, false),
        b'm' => select_graphic_rendition(handler, input),
        b'n' => device_status_report(handler, input),
        b'p' => request_mode_or_soft_reset(handler, input),
        b'q' => cursor_style_or_protection(handler, input),
        b'r' => top_and_bottom_margin(handler, input),
        b's' => left_and_right_margin(handler, input),
        b't' => xtwinops(handler, input),
        b'u' => restore_cursor_or_kitty_keyboard(handler, input),
        b'}' => active_status_display(handler, input),
        _ => tracing::warn!(target: LOG_SCOPE, "unimplemented CSI action: {input}"),
    }
}

/// `ignoring unimplemented CSI {final} with intermediates: ...`, for a command that takes none.
fn warn_intermediates(letter: char, input: &Csi) {
    tracing::warn!(
        target: LOG_SCOPE,
        "ignoring unimplemented CSI {letter} with intermediates: {}",
        String::from_utf8_lossy(&input.intermediates)
    );
}

/// The commands that take no intermediates and at most one parameter, a count or position that defaults to 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Counted {
    /// CUU - Cursor Up (`A`, `k`).
    CursorUp,
    /// CUD - Cursor Down.
    CursorDown,
    /// CUF - Cursor Right.
    CursorRight,
    /// CUB - Cursor Left (`D`, `j`).
    CursorLeft,
    /// CNL - Cursor Next Line.
    NextLine,
    /// CPL - Cursor Previous Line.
    PreviousLine,
    /// HPA - Cursor Horizontal Position Absolute (`G`, `` ` ``).
    ColumnAbsolute,
    /// CHT - Cursor Horizontal Tabulation.
    Tab,
    /// IL - Insert Lines.
    InsertLines,
    /// DL - Delete Lines.
    DeleteLines,
    /// DCH - Delete Character.
    DeleteChars,
    /// SU - Scroll Up.
    ScrollUp,
    /// SD - Scroll Down.
    ScrollDown,
    /// ECH - Erase Characters.
    EraseChars,
    /// CBT - Cursor Horizontal Tabulation Back.
    TabBack,
    /// HPR - Cursor Horizontal Position Relative.
    ColumnRelative,
    /// REP - Repeat Previous Char.
    Repeat,
    /// VPA - Cursor Vertical Position Absolute.
    RowAbsolute,
    /// VPR - Cursor Vertical Position Relative.
    RowRelative,
    /// ICH - Insert Blanks.
    InsertBlanks,
}

impl Counted {
    /// The command of a final byte.
    const fn from_final(final_: u8) -> Option<Self> {
        Some(match final_ {
            b'A' | b'k' => Self::CursorUp,
            b'B' => Self::CursorDown,
            b'C' => Self::CursorRight,
            b'D' | b'j' => Self::CursorLeft,
            b'E' => Self::NextLine,
            b'F' => Self::PreviousLine,
            b'G' | b'`' => Self::ColumnAbsolute,
            b'I' => Self::Tab,
            b'L' => Self::InsertLines,
            b'M' => Self::DeleteLines,
            b'P' => Self::DeleteChars,
            b'S' => Self::ScrollUp,
            b'T' => Self::ScrollDown,
            b'X' => Self::EraseChars,
            b'Z' => Self::TabBack,
            b'a' => Self::ColumnRelative,
            b'b' => Self::Repeat,
            b'd' => Self::RowAbsolute,
            b'e' => Self::RowRelative,
            b'@' => Self::InsertBlanks,
            _ => return None,
        })
    }

    /// The final the intermediates warning names (the first of a pair).
    const fn letter(self) -> char {
        match self {
            Self::CursorUp => 'A',
            Self::CursorDown => 'B',
            Self::CursorRight => 'C',
            Self::CursorLeft => 'D',
            Self::NextLine => 'E',
            Self::PreviousLine => 'F',
            Self::ColumnAbsolute => 'G',
            Self::Tab => 'I',
            Self::InsertLines => 'L',
            Self::DeleteLines => 'M',
            Self::DeleteChars => 'P',
            Self::ScrollUp => 'S',
            Self::ScrollDown => 'T',
            Self::EraseChars => 'X',
            Self::TabBack => 'Z',
            Self::ColumnRelative => 'a',
            Self::Repeat => 'b',
            Self::RowAbsolute => 'd',
            Self::RowRelative => 'e',
            Self::InsertBlanks => '@',
        }
    }

    /// The name in `invalid {name} command` (CNL and CPL name the movement they reuse, as Ghostty's do).
    const fn name(self) -> &'static str {
        match self {
            Self::CursorUp | Self::NextLine => "cursor up",
            Self::CursorDown | Self::PreviousLine => "cursor down",
            Self::CursorRight => "cursor right",
            Self::CursorLeft => "cursor left",
            Self::ColumnAbsolute => "HPA",
            Self::Tab => "horizontal tab",
            Self::InsertLines => "IL",
            Self::DeleteLines => "DL",
            Self::DeleteChars => "delete characters",
            Self::ScrollUp => "scroll up",
            Self::ScrollDown => "scroll down",
            Self::EraseChars => "erase characters",
            Self::TabBack => "horizontal tab back",
            Self::ColumnRelative => "HPR",
            Self::Repeat => "print repeat",
            Self::RowAbsolute => "VPA",
            Self::RowRelative => "VPR",
            Self::InsertBlanks => "ICH",
        }
    }

    /// Check the intermediates and the parameter count, then call the handler.
    fn dispatch<H: Handler>(self, handler: &mut H, input: &Csi) {
        if !input.intermediates.is_empty() {
            if self == Self::InsertBlanks {
                tracing::warn!(target: LOG_SCOPE, "ignoring unimplemented CSI @: {input}");
            } else {
                warn_intermediates(self.letter(), input);
            }
            return;
        }
        let value = match *input.params {
            [] => 1,
            [v] => v,
            _ => {
                tracing::warn!(target: LOG_SCOPE, "invalid {} command: {input}", self.name());
                return;
            }
        };
        self.apply(handler, value);
    }

    /// Call the handler with the count.
    fn apply<H: Handler>(self, handler: &mut H, value: u16) {
        let movement = CursorMovement { value };
        let count = usize::from(value);
        match self {
            Self::CursorUp => handler.cursor_up(movement),
            Self::CursorDown => handler.cursor_down(movement),
            Self::CursorRight => handler.cursor_right(movement),
            Self::CursorLeft => handler.cursor_left(movement),
            Self::NextLine => {
                handler.cursor_down(movement);
                handler.carriage_return();
            }
            Self::PreviousLine => {
                handler.cursor_up(movement);
                handler.carriage_return();
            }
            Self::ColumnAbsolute => handler.cursor_col(movement),
            Self::Tab => handler.horizontal_tab(value),
            Self::InsertLines => handler.insert_lines(count),
            Self::DeleteLines => handler.delete_lines(count),
            Self::DeleteChars => handler.delete_chars(count),
            Self::ScrollUp => handler.scroll_up(count),
            Self::ScrollDown => handler.scroll_down(count),
            Self::EraseChars => handler.erase_chars(count),
            Self::TabBack => handler.horizontal_tab_back(value),
            Self::ColumnRelative => handler.cursor_col_relative(movement),
            Self::Repeat => handler.print_repeat(count),
            Self::RowAbsolute => handler.cursor_row(movement),
            Self::RowRelative => handler.cursor_row_relative(movement),
            Self::InsertBlanks => handler.insert_blanks(count.max(1)),
        }
    }
}

/// CUP - Set Cursor Position (`H`, `f`).
fn cursor_pos<H: Handler>(handler: &mut H, input: &Csi) {
    if !input.intermediates.is_empty() {
        warn_intermediates('H', input);
        return;
    }
    let pos = match *input.params {
        [] => CursorPos { row: 1, col: 1 },
        [row] => CursorPos { row, col: 1 },
        [row, col] => CursorPos { row, col },
        _ => {
            tracing::warn!(target: LOG_SCOPE, "invalid CUP command: {input}");
            return;
        }
    };
    handler.cursor_pos(pos);
}

/// Whether an erase is selective (`CSI ?`): `None` for any other intermediates.
fn erase_protected(input: &Csi) -> Option<bool> {
    match *input.intermediates {
        [] => Some(false),
        [b'?'] => Some(true),
        _ => None,
    }
}

/// ED - Erase Display.
fn erase_display<H: Handler>(handler: &mut H, input: &Csi) {
    let mode = match *input.params {
        [] => Some(csi::EraseDisplay::Below),
        [v] => csi::EraseDisplay::from_int(v),
        _ => None,
    };
    let (Some(protected), Some(mode)) = (erase_protected(input), mode) else {
        tracing::warn!(target: LOG_SCOPE, "invalid erase display command: {input}");
        return;
    };
    match mode {
        csi::EraseDisplay::Below => handler.erase_display_below(protected),
        csi::EraseDisplay::Above => handler.erase_display_above(protected),
        csi::EraseDisplay::Complete => handler.erase_display_complete(protected),
        csi::EraseDisplay::Scrollback => handler.erase_display_scrollback(protected),
        csi::EraseDisplay::ScrollComplete => handler.erase_display_scroll_complete(protected),
    }
}

/// EL - Erase Line.
fn erase_line<H: Handler>(handler: &mut H, input: &Csi) {
    let mode = match *input.params {
        [] => Some(csi::EraseLine::Right),
        // Only the first three modes are accepted from the wire.
        [v @ 0..=2] => Some(csi::EraseLine::from_int(low_byte(v))),
        _ => None,
    };
    let (Some(protected), Some(mode)) = (erase_protected(input), mode) else {
        tracing::warn!(target: LOG_SCOPE, "invalid erase line command: {input}");
        return;
    };
    match mode {
        csi::EraseLine::Right => handler.erase_line_right(protected),
        csi::EraseLine::Left => handler.erase_line_left(protected),
        csi::EraseLine::Complete => handler.erase_line_complete(protected),
        csi::EraseLine::RightUnlessPendingWrap => handler.erase_line_right_unless_pending_wrap(protected),
        csi::EraseLine::Other(_) => tracing::warn!(target: LOG_SCOPE, "invalid erase line mode: {mode:?}"),
    }
}

/// A parameter known to fit a byte, as a byte.
fn low_byte(v: u16) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

/// CTC - Cursor Tabulation Control (`W`), and DECST8C (`CSI ? 5 W`).
fn tab_control<H: Handler>(handler: &mut H, input: &Csi) {
    match (&*input.intermediates, &*input.params) {
        ([], [] | [0]) => handler.tab_set(),
        ([], [2]) => handler.tab_clear_current(),
        ([], [5]) => handler.tab_clear_all(),
        ([b'?'], [5]) => handler.tab_reset(),
        ([] | [_], _) => tracing::warn!(target: LOG_SCOPE, "invalid cursor tabulation control: {input}"),
        _ => warn_intermediates('W', input),
    }
}

/// DA - Device Attributes (`c`, `>c`, `=c`).
fn device_attributes<H: Handler>(handler: &mut H, input: &Csi) {
    let req = match *input.intermediates {
        [] => device_attributes::Req::Primary,
        [b'>'] => device_attributes::Req::Secondary,
        [b'='] => device_attributes::Req::Tertiary,
        _ => {
            let key = input.params.first().copied().unwrap_or(0);
            if log_unsupported_once(&SEEN_INVALID_DEVICE_ATTRIBUTES, key) {
                tracing::warn!(target: LOG_SCOPE, "invalid device attributes command: {input}");
            }
            return;
        }
    };
    handler.device_attributes(req);
}

/// TBC - Tab Clear.
fn tab_clear<H: Handler>(handler: &mut H, input: &Csi) {
    if !input.intermediates.is_empty() {
        warn_intermediates('g', input);
        return;
    }
    let mode = match *input.params {
        // Drift: no parameter clears the current stop, as `0` does (Ghostty `d0c0d4008`).
        [] => csi::TabClear::Current,
        [v] => {
            let Ok(v) = u8::try_from(v) else {
                tracing::warn!(target: LOG_SCOPE, "invalid tab clear mode: {v}");
                return;
            };
            csi::TabClear::from_int(v)
        }
        _ => {
            tracing::warn!(target: LOG_SCOPE, "invalid tab clear command: {input}");
            return;
        }
    };
    match mode {
        csi::TabClear::Current => handler.tab_clear_current(),
        csi::TabClear::All => handler.tab_clear_all(),
        csi::TabClear::Other(_) => tracing::warn!(target: LOG_SCOPE, "unknown tab clear mode: {mode:?}"),
    }
}

/// Whether a mode command's modes are ANSI (no intermediates) or DEC private (`?`); `None` for anything else.
fn mode_kind(input: &Csi) -> Option<bool> {
    match *input.intermediates {
        [] => Some(true),
        [b'?'] => Some(false),
        _ => None,
    }
}

/// SM - Set Mode (`h`) and RM - Reset Mode (`l`).
fn set_or_reset_mode<H: Handler>(handler: &mut H, input: &Csi, set: bool) {
    let Some(ansi_mode) = mode_kind(input) else {
        tracing::warn!(target: LOG_SCOPE, "invalid set mode command: {input}");
        return;
    };
    for &mode_int in &input.params {
        match modes::mode_from_int(mode_int, ansi_mode) {
            Some(mode) if set => handler.set_mode(Mode { mode }),
            Some(mode) => handler.reset_mode(Mode { mode }),
            None => {
                if log_unsupported_once(&SEEN_UNIMPLEMENTED_MODE, mode_int) {
                    tracing::warn!(target: LOG_SCOPE, "unimplemented mode: {mode_int}");
                }
            }
        }
    }
}

/// SGR - Select Graphic Rendition (`m`), and XTMODKEYS (`>m`).
fn select_graphic_rendition<H: Handler>(handler: &mut H, input: &Csi) {
    match *input.intermediates {
        // This is the most common case.
        [] => {
            let mut p = sgr::Parser {
                params: &input.params,
                params_sep: input.params_sep,
                idx: 0,
            };
            while let Some(attr) = p.next() {
                handler.set_attribute(attr);
            }
        }
        [b'>'] => modify_key_format(handler, input),
        [c] => {
            if log_unsupported_once(&SEEN_UNKNOWN_CSI_M_INTERMEDIATE, u16::from(c)) {
                tracing::warn!(target: LOG_SCOPE, "unknown CSI m with intermediate: {c}");
            }
        }
        // There are other forms of CSI m that have intermediates. `vim --clean` uses `CSI ? 4 m` and I don't know
        // what that means.
        _ => warn_intermediates('m', input),
    }
}

/// XTMODKEYS - Set/reset key modifier options (`CSI > Pp ; Pv m`).
fn modify_key_format<H: Handler>(handler: &mut H, input: &Csi) {
    let Some(&first) = input.params.first() else {
        // Reset
        handler.modify_key_format(ansi::ModifyKeyFormat::Legacy);
        return;
    };
    let format = match first {
        0 => ansi::ModifyKeyFormat::Legacy,
        1 => ansi::ModifyKeyFormat::CursorKeys,
        2 => ansi::ModifyKeyFormat::FunctionKeys,
        4 => ansi::ModifyKeyFormat::OtherKeysNone,
        _ => {
            tracing::warn!(target: LOG_SCOPE, "invalid setModifyKeyFormat: {input}");
            return;
        }
    };
    if input.params.len() > 2 {
        tracing::warn!(target: LOG_SCOPE, "invalid setModifyKeyFormat: {input}");
        return;
    }
    // We don't support any of the subparams yet but other keys' numeric form.
    let format = match (format, input.params.get(1)) {
        (ansi::ModifyKeyFormat::OtherKeysNone, Some(2)) => ansi::ModifyKeyFormat::OtherKeysNumeric,
        _ => format,
    };
    handler.modify_key_format(format);
}

/// DSR - Device Status Report (`n`, `?n`), and the XTMODKEYS reset (`>n`).
fn device_status_report<H: Handler>(handler: &mut H, input: &Csi) {
    let question = match *input.intermediates {
        [] => false,
        [b'?'] => true,
        // This isn't strictly correct. CSI > n has parameters that control what exactly is being disabled. However,
        // we only support reverting back to modify other keys in numeric except format.
        [b'>'] => {
            handler.modify_key_format(ansi::ModifyKeyFormat::OtherKeysNumericExcept);
            return;
        }
        [c] => {
            tracing::warn!(target: LOG_SCOPE, "unknown CSI n with intermediate: {c}");
            return;
        }
        // A longer `?` prefix is still a status report, refused here as Ghostty does.
        [b'?', ..] if input.params.len() == 1 => {
            tracing::warn!(target: LOG_SCOPE, "invalid set mode command: {input}");
            return;
        }
        [b'?', ..] => {
            tracing::warn!(target: LOG_SCOPE, "invalid device status report command: {input}");
            return;
        }
        _ => {
            warn_intermediates('n', input);
            return;
        }
    };
    // Drift: `CSI ? 6 n` is DECXCPR, the extended cursor position report (Ghostty `fbda61b87`).
    let req = match *input.params {
        [v] => device_status::req_from_int(v, question),
        _ => None,
    };
    let Some(request) = req else {
        tracing::warn!(target: LOG_SCOPE, "invalid device status report command: {input}");
        return;
    };
    handler.device_status(DeviceStatus { request });
}

/// DECRQM - Request Mode (`$p`, `?$p`) and DECSTR - Soft Terminal Reset (`!p`).
fn request_mode_or_soft_reset<H: Handler>(handler: &mut H, input: &Csi) {
    let ansi_mode = match *input.intermediates {
        [b'!'] => {
            if input.params.is_empty() {
                handler.soft_reset();
            } else {
                tracing::warn!(target: LOG_SCOPE, "invalid DECSTR command: {input}");
            }
            return;
        }
        [b'$'] => true,
        [b'?', b'$'] => false,
        _ => {
            warn_intermediates('p', input);
            return;
        }
    };
    let [mode_raw] = *input.params else {
        tracing::warn!(target: LOG_SCOPE, "invalid DECRQM command: {input}");
        return;
    };
    match modes::mode_from_int(mode_raw, ansi_mode) {
        Some(mode) => handler.request_mode(Mode { mode }),
        None => handler.request_mode_unknown(RawMode {
            mode: mode_raw,
            ansi: ansi_mode,
        }),
    }
}

/// DECSCUSR - Select Cursor Style (` q`), DECSCA - Select Character Protection Attribute (`"q`) and XTVERSION (`>q`).
fn cursor_style_or_protection<H: Handler>(handler: &mut H, input: &Csi) {
    match *input.intermediates {
        [b' '] => cursor_style(handler, input),
        [b'"'] => protected_mode(handler, input),
        [b'>'] => handler.xtversion(),
        [_] => warn_intermediates('q', input),
        _ => warn_intermediates('p', input),
    }
}

/// DECSCUSR - Select Cursor Style.
fn cursor_style<H: Handler>(handler: &mut H, input: &Csi) {
    let style = match *input.params {
        [] | [0] => ansi::CursorStyle::Default,
        [1] => ansi::CursorStyle::BlinkingBlock,
        [2] => ansi::CursorStyle::SteadyBlock,
        [3] => ansi::CursorStyle::BlinkingUnderline,
        [4] => ansi::CursorStyle::SteadyUnderline,
        [5] => ansi::CursorStyle::BlinkingBar,
        [6] => ansi::CursorStyle::SteadyBar,
        [v] => {
            tracing::warn!(target: LOG_SCOPE, "invalid cursor style value: {v}");
            return;
        }
        _ => {
            tracing::warn!(target: LOG_SCOPE, "invalid set cursor style command: {input}");
            return;
        }
    };
    handler.cursor_style(style);
}

/// DECSCA - Select Character Protection Attribute.
fn protected_mode<H: Handler>(handler: &mut H, input: &Csi) {
    match *input.params {
        [] | [0 | 2] => handler.protected_mode_off(),
        [1] => handler.protected_mode_dec(),
        _ => tracing::warn!(target: LOG_SCOPE, "invalid set protected mode command: {input}"),
    }
}

/// The modes of a save (`?s`) or restore (`?r`) of DEC private modes.
fn save_or_restore_modes<H: Handler>(handler: &mut H, input: &Csi, save: bool) {
    for &mode_int in &input.params {
        match modes::mode_from_int(mode_int, false) {
            Some(mode) if save => handler.save_mode(Mode { mode }),
            Some(mode) => handler.restore_mode(Mode { mode }),
            None if save => tracing::warn!(target: LOG_SCOPE, "unimplemented save mode: {mode_int}"),
            None => tracing::warn!(target: LOG_SCOPE, "unimplemented restore mode: {mode_int}"),
        }
    }
}

/// The margins of DECSTBM or DECSLRM: zero for an omitted one.
fn margin(input: &Csi) -> Option<Margin> {
    match *input.params {
        [] => Some(Margin {
            top_left: 0,
            bottom_right: 0,
        }),
        [top_left] => Some(Margin {
            top_left,
            bottom_right: 0,
        }),
        [top_left, bottom_right] => Some(Margin { top_left, bottom_right }),
        _ => None,
    }
}

/// DECSTBM - Set Top and Bottom Margins (`r`), and the restore of DEC private modes (`?r`).
fn top_and_bottom_margin<H: Handler>(handler: &mut H, input: &Csi) {
    match *input.intermediates {
        [] => {
            if let Some(m) = margin(input) {
                handler.top_and_bottom_margin(m);
            } else {
                tracing::warn!(target: LOG_SCOPE, "invalid DECSTBM command: {input}");
            }
        }
        [b'?'] => save_or_restore_modes(handler, input, false),
        [_] => tracing::warn!(target: LOG_SCOPE, "unknown CSI s with intermediate: {input}"),
        _ => tracing::warn!(target: LOG_SCOPE, "ignoring unimplemented CSI s with intermediates: {input}"),
    }
}

/// DECSLRM - Set Left and Right Margins (`s`), the save of DEC private modes (`?s`) and XTSHIFTESCAPE (`>s`).
fn left_and_right_margin<H: Handler>(handler: &mut H, input: &Csi) {
    match *input.intermediates {
        // CSI s is ambiguous with zero params so we defer to our handler to do the proper logic. If mode 69 is set,
        // then we should invoke DECSLRM, otherwise we should invoke SC.
        [] if input.params.is_empty() => handler.left_and_right_margin_ambiguous(),
        [] => {
            if let Some(m) = margin(input) {
                handler.left_and_right_margin(m);
            } else {
                tracing::warn!(target: LOG_SCOPE, "invalid DECSLRM command: {input}");
            }
        }
        [b'?'] => save_or_restore_modes(handler, input, true),
        [b'>'] => match *input.params {
            [] | [0] => handler.mouse_shift_capture(false),
            [1] => handler.mouse_shift_capture(true),
            _ => tracing::warn!(target: LOG_SCOPE, "invalid XTSHIFTESCAPE command: {input}"),
        },
        [_] => tracing::warn!(target: LOG_SCOPE, "unknown CSI s with intermediate: {input}"),
        _ => tracing::warn!(target: LOG_SCOPE, "ignoring unimplemented CSI s with intermediates: {input}"),
    }
}

/// XTWINOPS - Window manipulation (`t`).
fn xtwinops<H: Handler>(handler: &mut H, input: &Csi) {
    if !input.intermediates.is_empty() {
        tracing::warn!(target: LOG_SCOPE, "ignoring unimplemented CSI t with intermediates: {input}");
        return;
    }
    let Some(&op) = input.params.first() else {
        tracing::error!(target: LOG_SCOPE, "ignoring CSI t with no parameters: {input}");
        return;
    };
    let style = match op {
        // resize the text area in characters
        8 => {
            if input.params.len() <= 3 {
                handler.resize_window(ResizeWindow {
                    rows: input.params.get(1).copied().unwrap_or(0),
                    columns: input.params.get(2).copied().unwrap_or(0),
                });
            } else {
                tracing::warn!(target: LOG_SCOPE, "ignoring CSI 8 t with extra parameters: {input}");
            }
            return;
        }
        22 | 23 => {
            title_push_pop(handler, input, op == 22);
            return;
        }
        // report the text area size in pixels
        14 => csi::SizeReportStyle::Csi14T,
        // report cell size in pixels
        16 => csi::SizeReportStyle::Csi16T,
        // report text area size in characters
        18 => csi::SizeReportStyle::Csi18T,
        // Drift: report screen size in characters (Ghostty `6104c78f1`).
        19 => csi::SizeReportStyle::Csi19T,
        // report window title
        21 => csi::SizeReportStyle::Csi21T,
        _ => {
            if log_unsupported_once(&SEEN_CSI_T_UNIMPLEMENTED_PARAM, op) {
                tracing::warn!(target: LOG_SCOPE, "ignoring CSI t with unimplemented parameter: {input}");
            }
            return;
        }
    };
    if input.params.len() == 1 {
        handler.size_report(style);
    } else {
        tracing::warn!(target: LOG_SCOPE, "ignoring CSI {op} t with extra parameters: {input}");
    }
}

/// XTWINOPS 22 and 23: push or pop the window title (the only one supported).
fn title_push_pop<H: Handler>(handler: &mut H, input: &Csi, push: bool) {
    match *input.params {
        [_, 0 | 2] | [_, 0 | 2, _] => {
            let index = input.params.get(2).copied().unwrap_or(0);
            if push {
                handler.title_push(index);
            } else {
                handler.title_pop(index);
            }
        }
        _ => {
            if log_unsupported_once(&SEEN_CSI_22_23_T_EXTRA_PARAMS, input.params[0]) {
                tracing::warn!(target: LOG_SCOPE, "ignoring CSI 22/23 t with extra parameters: {input}");
            }
        }
    }
}

/// SCORC - Restore Cursor (`u`), and the Kitty keyboard protocol (`?u`, `>u`, `<u`, `=u`).
fn restore_cursor_or_kitty_keyboard<H: Handler>(handler: &mut H, input: &Csi) {
    match *input.intermediates {
        [] => handler.restore_cursor(),
        [b'?'] => handler.kitty_keyboard_query(),
        [b'>'] => {
            let flags = match *input.params {
                [v] => kitty_flags(v),
                _ => Some(KittyKeyboardFlags {
                    flags: kitty::key::Flags::from_int(0),
                }),
            };
            if let Some(flags) = flags {
                handler.kitty_keyboard_push(flags);
            } else {
                tracing::warn!(target: LOG_SCOPE, "invalid pushKittyKeyboard command: {input}");
            }
        }
        [b'<'] => handler.kitty_keyboard_pop(match *input.params {
            [v] => v,
            _ => 1,
        }),
        [b'='] => kitty_keyboard_set(handler, input),
        [_] => tracing::warn!(target: LOG_SCOPE, "unknown CSI s with intermediate: {input}"),
        _ => tracing::warn!(target: LOG_SCOPE, "ignoring unimplemented CSI u: {input}"),
    }
}

/// Kitty keyboard flags from a parameter: `None` unless it fits the protocol's five bits.
fn kitty_flags(v: u16) -> Option<KittyKeyboardFlags> {
    let v = u8::try_from(v).ok().filter(|v| *v < 32)?;
    Some(KittyKeyboardFlags {
        flags: kitty::key::Flags::from_int(v),
    })
}

/// `CSI = flags ; mode u`: set, or (2) add, or (3) remove the Kitty keyboard flags.
fn kitty_keyboard_set<H: Handler>(handler: &mut H, input: &Csi) {
    let flags = match input.params.first() {
        Some(&v) => kitty_flags(v),
        None => kitty_flags(0),
    };
    let Some(flags) = flags else {
        tracing::warn!(target: LOG_SCOPE, "invalid setKittyKeyboard command: {input}");
        return;
    };
    match input.params.get(1).copied().unwrap_or(1) {
        1 => handler.kitty_keyboard_set(flags),
        2 => handler.kitty_keyboard_set_or(flags),
        3 => handler.kitty_keyboard_set_not(flags),
        _ => tracing::warn!(target: LOG_SCOPE, "invalid setKittyKeyboard command: {input}"),
    }
}

/// DECSASD - Select Active Status Display (`$}`).
fn active_status_display<H: Handler>(handler: &mut H, input: &Csi) {
    let display = match (&*input.intermediates, &*input.params) {
        ([b'$'], [0]) => ansi::StatusDisplay::Main,
        ([b'$'], [1]) => ansi::StatusDisplay::StatusLine,
        _ => {
            tracing::warn!(target: LOG_SCOPE, "unimplemented CSI callback: {input}");
            return;
        }
    };
    handler.active_status_display(display);
}
