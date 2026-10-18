//! The stream's C0, OSC and ESC dispatch: the parser's `execute`, `osc_dispatch` and `esc_dispatch` actions turned
//! into handler calls.
//!
//! Ghostty: `src/terminal/stream.zig` (`execute`, `oscDispatch`, `configureCharset`, `escDispatch`) at
//! `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/stream/dispatch.rs` at `RUSTTY_REV` (MIT).
//! Changes: free functions over the handler; the ESC commands that take no intermediates are a table ([`Plain`]). The
//! branches and their log lines are Ghostty's.

use super::{
    ClipboardContents, ColorOperation, ConfigureCharset, Handler, InvokeCharset, LOG_SCOPE, Mode, ReportPwd,
    SEEN_INVALID_C0, SEEN_OSC1_CHANGE_ICON, SEEN_UNIMPLEMENTED_ESC_ACTION, ShowDesktopNotification, StartHyperlink,
    WindowTitle, log_unsupported_once,
};
use crate::terminal::mouse::Shape as MouseShape;
use crate::terminal::parser::{Esc, InlineVec};
use crate::terminal::{ansi, charsets, device_attributes, modes, osc};

/// Execute a C0 control, or a C1 one as its ESC equivalent.
pub(super) fn execute<H: Handler>(handler: &mut H, c: u8) {
    // If the character is > 0x7F, it's a C1 (8-bit) control, which is strictly equivalent to `ESC` plus `c - 0x40`.
    if c > 0x7F {
        tracing::info!(target: LOG_SCOPE, "executing C1 0x{c:x} as ESC {}", char::from(c - 0x40));
        esc_dispatch(
            handler,
            &Esc {
                intermediates: InlineVec::default(),
                final_: c - 0x40,
            },
        );
        return;
    }

    match ansi::C0::from_int(c) {
        // We ignore SOH/STX: https://github.com/microsoft/terminal/issues/10786
        ansi::C0::Nul | ansi::C0::Soh | ansi::C0::Stx => {}
        ansi::C0::Enq => handler.enquiry(),
        ansi::C0::Bel => handler.bell(),
        ansi::C0::Bs => handler.backspace(),
        ansi::C0::Ht => handler.horizontal_tab(1),
        ansi::C0::Lf | ansi::C0::Vt | ansi::C0::Ff => handler.linefeed(),
        ansi::C0::Cr => handler.carriage_return(),
        ansi::C0::So => handler.invoke_charset(invoke(charsets::ActiveSlot::GL, charsets::Slots::G1, false)),
        ansi::C0::Si => handler.invoke_charset(invoke(charsets::ActiveSlot::GL, charsets::Slots::G0, false)),
        ansi::C0::Other(_) => {
            if log_unsupported_once(&SEEN_INVALID_C0, u16::from(c)) {
                tracing::warn!(target: LOG_SCOPE, "invalid C0 character, ignoring: 0x{c:x}");
            }
        }
    }
}

/// An `invoke_charset` action.
const fn invoke(bank: charsets::ActiveSlot, charset: charsets::Slots, locking: bool) -> InvokeCharset {
    InvokeCharset { bank, charset, locking }
}

/// Dispatch a parsed OSC command.
pub(super) fn osc_dispatch<H: Handler>(handler: &mut H, cmd: osc::Command<'_>) {
    // The most common OSC commands in real world data are, in order, hyperlink_end, change_window_title,
    // change_window_icon, hyperlink_start, report_pwd, color_operation and semantic_prompt: together about 96% of all
    // OSC commands (ref: https://github.com/qwerasd205/asciinema-stats).
    match cmd {
        osc::Command::SemanticPrompt(sp) => handler.semantic_prompt(sp),
        osc::Command::ChangeWindowTitle(title) => {
            if core::str::from_utf8(title).is_err() {
                tracing::warn!(target: LOG_SCOPE, "change title request: invalid utf-8, ignoring request");
                return;
            }
            handler.window_title(WindowTitle { title });
        }
        osc::Command::ChangeWindowIcon(icon) => {
            if log_unsupported_once(&SEEN_OSC1_CHANGE_ICON, 0) {
                tracing::warn!(
                    target: LOG_SCOPE,
                    "OSC 1 (change icon) received and ignored icon={}",
                    String::from_utf8_lossy(icon)
                );
            }
        }
        osc::Command::ClipboardContents { kind, data, terminator } => {
            handler.clipboard_contents(ClipboardContents { kind, data, terminator });
        }
        osc::Command::ReportPwd { value } => handler.report_pwd(ReportPwd { url: value }),
        osc::Command::MouseShape { value } => mouse_shape(handler, value),
        osc::Command::ColorOperation {
            op,
            requests,
            terminator,
        } => handler.color_operation(ColorOperation {
            op,
            requests,
            terminator,
        }),
        osc::Command::KittyColorProtocol(v) => handler.kitty_color_report(v),
        osc::Command::ShowDesktopNotification { title, body } => {
            handler.show_desktop_notification(ShowDesktopNotification { title, body });
        }
        osc::Command::HyperlinkStart { id, uri } => handler.start_hyperlink(StartHyperlink { uri, id }),
        osc::Command::HyperlinkEnd => handler.end_hyperlink(),
        osc::Command::ConemuProgressReport(v) => handler.progress_report(v),
        osc::Command::KittyClipboardProtocol(v) => handler.kitty_clipboard(v),
        osc::Command::KittyDndProtocol(v) => handler.kitty_dnd(v),
        osc::Command::ProgramStatus(v) => handler.program_status(v),
        osc::Command::Unknown(v) => handler.osc_unknown(v),
        osc::Command::ConemuSleep { .. }
        | osc::Command::ConemuShowMessageBox(_)
        | osc::Command::ConemuChangeTabTitle(_)
        | osc::Command::ConemuWaitInput
        | osc::Command::ConemuGuimacro(_)
        | osc::Command::ConemuComment(_)
        | osc::Command::ConemuXtermEmulation { .. }
        | osc::Command::ConemuOutputEnvironmentVariable(_)
        | osc::Command::ConemuRunProcess(_)
        | osc::Command::KittyTextSizing(_)
        | osc::Command::KittyDesktopNotification(_)
        | osc::Command::ContextSignal(_) => {
            tracing::debug!(target: LOG_SCOPE, "unimplemented OSC callback: {cmd:?}");
        }
        // This is an invalid internal state, not an invalid OSC string being parsed. We shouldn't see this.
        osc::Command::Invalid => tracing::warn!(target: LOG_SCOPE, "invalid OSC, should never happen"),
    }
}

/// OSC 22: set the mouse shape, or reset it when empty.
fn mouse_shape<H: Handler>(handler: &mut H, value: &[u8]) {
    if value.is_empty() {
        handler.mouse_shape_reset();
        return;
    }
    if let Some(shape) = MouseShape::from_string(value) {
        handler.mouse_shape(shape);
    } else {
        tracing::warn!(target: LOG_SCOPE, "unknown cursor shape: {}", String::from_utf8_lossy(value));
    }
}

/// SCS - Select Character Set: designate `set` to the slot the intermediate names.
fn configure_charset<H: Handler>(handler: &mut H, intermediates: &[u8], set: charsets::Charset) {
    let slot = match *intermediates {
        // TODO: support slots '-', '.', '/'
        [b'('] => charsets::Slots::G0,
        [b')'] => charsets::Slots::G1,
        [b'*'] => charsets::Slots::G2,
        [b'+'] => charsets::Slots::G3,
        _ => {
            tracing::warn!(target: LOG_SCOPE, "invalid charset intermediate: {intermediates:?}");
            return;
        }
    };
    handler.configure_charset(ConfigureCharset { slot, charset: set });
}

/// Dispatch an ESC sequence.
pub(super) fn esc_dispatch<H: Handler>(handler: &mut H, action: &Esc) {
    // The most common ESC finals in real world data are, in order, B, \, 0, M, 8, 7, > and =: together nearly 99% of
    // all ESC sequences (ref: https://github.com/qwerasd205/asciinema-stats).
    match action.final_ {
        // Charsets
        b'B' => configure_charset(handler, &action.intermediates, charsets::Charset::Ascii),
        b'A' => configure_charset(handler, &action.intermediates, charsets::Charset::British),
        b'0' => configure_charset(handler, &action.intermediates, charsets::Charset::DecSpecial),
        b'8' => match *action.intermediates {
            // DECRC - Restore Cursor
            [] => handler.restore_cursor(),
            // DECALN - Fill Screen with E
            [b'#'] => handler.decaln(),
            _ => unimplemented_esc(action),
        },
        // Sets ST (string terminator). We don't have to do anything because our parser always accepts ST.
        b'\\' => {}
        final_ => match Plain::from_final(final_) {
            Some(plain) if action.intermediates.is_empty() => plain.apply(handler),
            Some(plain) => tracing::warn!(target: LOG_SCOPE, "{}: {action}", plain.refusal()),
            None => unimplemented_esc(action),
        },
    }
}

/// `unimplemented ESC action`, logged once per final.
fn unimplemented_esc(action: &Esc) {
    if log_unsupported_once(&SEEN_UNIMPLEMENTED_ESC_ACTION, u16::from(action.final_)) {
        tracing::warn!(target: LOG_SCOPE, "unimplemented ESC action: {action}");
    }
}

/// The ESC commands that take no intermediates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plain {
    /// DECSC - Save Cursor (`7`).
    SaveCursor,
    /// IND - Index (`D`).
    Index,
    /// NEL - Next Line (`E`).
    NextLine,
    /// HTS - Horizontal Tab Set (`H`).
    TabSet,
    /// RI - Reverse Index (`M`).
    ReverseIndex,
    /// SS2 - Single Shift 2 (`N`).
    SingleShift2,
    /// SS3 - Single Shift 3 (`O`).
    SingleShift3,
    /// SPA - Start of Guarded Area (`V`).
    StartGuardedArea,
    /// EPA - End of Guarded Area (`W`).
    EndGuardedArea,
    /// DECID (`Z`).
    Identify,
    /// RIS - Full Reset (`c`).
    FullReset,
    /// LS2 - Locking Shift 2 (`n`).
    LockingShift2,
    /// LS3 - Locking Shift 3 (`o`).
    LockingShift3,
    /// LS1R - Locking Shift 1 Right (`~`).
    LockingShift1Right,
    /// LS2R - Locking Shift 2 Right (`}`).
    LockingShift2Right,
    /// LS3R - Locking Shift 3 Right (`|`).
    LockingShift3Right,
    /// DECKPAM - Set application keypad mode (`=`).
    KeypadApplication,
    /// DECKPNM - Reset application keypad mode (`>`).
    KeypadNumeric,
}

impl Plain {
    /// The command of a final byte.
    const fn from_final(final_: u8) -> Option<Self> {
        Some(match final_ {
            b'7' => Self::SaveCursor,
            b'D' => Self::Index,
            b'E' => Self::NextLine,
            b'H' => Self::TabSet,
            b'M' => Self::ReverseIndex,
            b'N' => Self::SingleShift2,
            b'O' => Self::SingleShift3,
            b'V' => Self::StartGuardedArea,
            b'W' => Self::EndGuardedArea,
            b'Z' => Self::Identify,
            b'c' => Self::FullReset,
            b'n' => Self::LockingShift2,
            b'o' => Self::LockingShift3,
            b'~' => Self::LockingShift1Right,
            b'}' => Self::LockingShift2Right,
            b'|' => Self::LockingShift3Right,
            b'=' => Self::KeypadApplication,
            b'>' => Self::KeypadNumeric,
            _ => return None,
        })
    }

    /// What the log says when the command comes with intermediates (LS2 and LS3 reuse SS2's and SS3's, as Ghostty's
    /// do).
    const fn refusal(self) -> &'static str {
        match self {
            Self::SaveCursor => "invalid command",
            Self::Index => "invalid index command",
            Self::NextLine => "invalid next line command",
            Self::TabSet => "invalid tab set command",
            Self::ReverseIndex => "invalid reverse index command",
            Self::SingleShift2 | Self::LockingShift2 => "invalid single shift 2 command",
            Self::SingleShift3 | Self::LockingShift3 => "invalid single shift 3 command",
            Self::StartGuardedArea | Self::EndGuardedArea | Self::Identify => "unimplemented ESC callback",
            Self::FullReset => "invalid full reset command",
            Self::LockingShift1Right => "invalid locking shift 1 right command",
            Self::LockingShift2Right => "invalid locking shift 2 right command",
            Self::LockingShift3Right => "invalid locking shift 3 right command",
            Self::KeypadApplication | Self::KeypadNumeric => "unimplemented setMode",
        }
    }

    /// Call the handler.
    fn apply<H: Handler>(self, handler: &mut H) {
        use charsets::ActiveSlot::{GL, GR};
        use charsets::Slots::{G1, G2, G3};
        let keypad = Mode {
            mode: modes::Mode::KeypadKeys,
        };
        match self {
            Self::SaveCursor => handler.save_cursor(),
            Self::Index => handler.index(),
            Self::NextLine => handler.next_line(),
            Self::TabSet => handler.tab_set(),
            Self::ReverseIndex => handler.reverse_index(),
            Self::SingleShift2 => handler.invoke_charset(invoke(GL, G2, true)),
            Self::SingleShift3 => handler.invoke_charset(invoke(GL, G3, true)),
            Self::StartGuardedArea => handler.protected_mode_iso(),
            Self::EndGuardedArea => handler.protected_mode_off(),
            Self::Identify => handler.device_attributes(device_attributes::Req::Primary),
            Self::FullReset => handler.full_reset(),
            Self::LockingShift2 => handler.invoke_charset(invoke(GL, G2, false)),
            Self::LockingShift3 => handler.invoke_charset(invoke(GL, G3, false)),
            Self::LockingShift1Right => handler.invoke_charset(invoke(GR, G1, false)),
            Self::LockingShift2Right => handler.invoke_charset(invoke(GR, G2, false)),
            Self::LockingShift3Right => handler.invoke_charset(invoke(GR, G3, false)),
            Self::KeypadApplication => handler.set_mode(keypad),
            Self::KeypadNumeric => handler.reset_mode(keypad),
        }
    }
}
