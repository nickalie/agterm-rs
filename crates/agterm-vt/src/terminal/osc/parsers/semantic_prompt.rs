//! OSC 133: semantic prompts, the shell marking its prompts, input and command output:
//! <https://gitlab.freedesktop.org/Per_Bothner/specifications/blob/master/proposals/semantic-prompts.md>
//!
//! Ghostty: `src/terminal/osc/parsers/semantic_prompt.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/semantic_prompt.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's comptime `Option` and its `Type()` are two descriptors, [`ValueOption`] for the parsed options and
//! [`ByteOption`] for the text ones, read through [`ReadableOption`]. The command line decodes into a
//! `std::io::Write` (`os::string_encoding`).

use crate::os::string_encoding::{self, DecodeError};
use crate::terminal::osc::{Command as OscCommand, Parser, State};

/// A single semantic prompt command.
///
/// Technically according to the spec, not all commands have options
/// but it is easier to be "liberal in what we accept" here since
/// all except one do and the spec does also say to ignore unknown
/// options. So, I think this is a fair interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Command<'p> {
    pub(crate) action: Action,
    pub(crate) options_unvalidated: &'p [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    FreshLine,                       // 'L'
    FreshLineNewPrompt,              // 'A'
    NewCommand,                      // 'N'
    PromptStart,                     // 'P'
    EndPromptStartInput,             // 'B'
    EndPromptStartInputTerminateEol, // 'I'
    EndInputStartOutput,             // 'C'
    EndCommand,                      // 'D'
}

impl Command<'_> {
    /// The command `action` with no options.
    pub(crate) const fn init(action: Action) -> Self {
        Self {
            action,
            options_unvalidated: &[],
        }
    }

    /// Read an option for this command. Returns null if unset or invalid.
    pub(crate) fn read_option<'a, O: ReadableOption<'a>>(&'a self, option: O) -> Option<O::Output> {
        option.read(self.options_unvalidated)
    }

    /// Write the decoded command line (if any) to the writer. If an error
    /// occurs garbage may have been written to the writer.
    pub(crate) fn write_command_line(&self, writer: &mut impl std::io::Write) -> Result<(), DecodeError> {
        if let Some(command_line) = self.read_option(option::CMDLINE) {
            return string_encoding::printf_q_decode(writer, command_line);
        }
        if let Some(command_line) = self.read_option(option::CMDLINE_URL) {
            return string_encoding::url_percent_decode(writer, command_line);
        }
        Ok(())
    }
}

// ClickEvents can either be a click_events=1 or click_events=2.
// The click_events=1 sends a click event with the absolute coordinates
// of the click.
// The click_events=2 sends a click event with the coordinates of the click
// relative to the prompt area.
// See https://github.com/ghostty-org/ghostty/issues/10865 and
// https://github.com/kovidgoyal/kitty/issues/9500
// for further details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickEvents {
    Absolute,
    Relative,
}

/// Descriptor for an option whose value type `T` is a `Copy`, lifetime-free
/// value (everything except the `[]const u8`-typed options).
#[derive(Clone, Copy)]
pub(crate) struct ValueOption<T> {
    /// The option key, e.g. `b"cl"`. Empty for `exit_code` which has no key
    /// (the special case handled before ever calling key).
    key: &'static [u8],
    /// `true` only for `exit_code`, which is parsed from the first value rather
    /// than a `key=value` pair.
    is_exit_code: bool,
    /// Maps the matched raw value bytes to the option's parsed value.
    decode: fn(&[u8]) -> Option<T>,
}

/// Descriptor for a `[]const u8`-typed option (`aid`, `err`, `cmdline`,
/// `cmdline_url`) whose `read` returns a borrow into the caller's `raw`.
#[derive(Clone, Copy)]
pub(crate) struct ByteOption {
    /// The option key, e.g. `b"aid"`.
    key: &'static [u8],
}

fn scan_value<'a>(raw: &'a [u8], key: &[u8], is_exit_code: bool) -> Option<&'a [u8]> {
    let mut remaining = raw;
    'scan: while !remaining.is_empty() {
        // Length of the next value is up to the `;` or the
        // end of the string.
        let len = remaining.iter().position(|&b| b == b';').unwrap_or(remaining.len());

        // Grab our full value and move our cursor past the `;`
        let full = &remaining[0..len];

        // If we're looking for exit_code we special case it.
        // as the first value.
        if is_exit_code {
            return Some(full);
        }

        // Parse our key=value and verify our key matches our
        // expectation.
        let value = 'value: {
            if let Some(eql_idx) = full.iter().position(|&b| b == b'=')
                && &full[0..eql_idx] == key
            {
                break 'value &full[eql_idx + 1..];
            }

            // No match!
            if len < remaining.len() {
                remaining = &remaining[len + 1..];
                continue 'scan;
            }

            break 'scan;
        };

        return Some(value);
    }

    // Not found
    None
}

impl<T> ValueOption<T> {
    /// Read the option value from the raw options string.
    ///
    /// The raw options string is the raw unparsed data after the
    /// OSC 133 command. e.g. for `133;A;aid=14;cl=line`, the
    /// raw options string would be `aid=14;cl=line`.
    ///
    /// Any errors in the raw string will return null since the OSC133
    /// specification says to ignore unknown or malformed options.
    pub(crate) fn read(&self, raw: &[u8]) -> Option<T> {
        let value = scan_value(raw, self.key, self.is_exit_code)?;
        (self.decode)(value)
    }
}

impl ByteOption {
    /// Read the option value from the raw options string. See `ValueOption::read`.
    pub(crate) fn read<'a>(&self, raw: &'a [u8]) -> Option<&'a [u8]> {
        // The `[]const u8` options' `switch (self)` arm is the identity
        // `=> value`, so the matched sub-slice is the result directly.
        scan_value(raw, self.key, false)
    }
}

pub(crate) trait ReadableOption<'a> {
    type Output;
    fn read(self, raw: &'a [u8]) -> Option<Self::Output>;
}

impl<'a, T: 'a> ReadableOption<'a> for ValueOption<T> {
    type Output = T;
    fn read(self, raw: &'a [u8]) -> Option<T> {
        ValueOption::read(&self, raw)
    }
}

impl<'a> ReadableOption<'a> for ByteOption {
    type Output = &'a [u8];
    fn read(self, raw: &'a [u8]) -> Option<&'a [u8]> {
        ByteOption::read(&self, raw)
    }
}

pub(crate) mod option {
    use super::{ByteOption, Click, ClickEvents, PromptKind, Redraw, ValueOption};

    pub(crate) const AID: ByteOption = ByteOption { key: b"aid" };
    pub(crate) const CL: ValueOption<Click> = ValueOption {
        key: b"cl",
        is_exit_code: false,
        decode: Click::init,
    };
    pub(crate) const PROMPT_KIND: ValueOption<PromptKind> = ValueOption {
        key: b"k",
        is_exit_code: false,
        decode: |value| {
            if value.len() == 1 {
                PromptKind::init(value[0])
            } else {
                None
            }
        },
    };
    pub(crate) const ERR: ByteOption = ByteOption { key: b"err" };
    pub(crate) const CMDLINE: ByteOption = ByteOption { key: b"cmdline" };
    pub(crate) const CMDLINE_URL: ByteOption = ByteOption { key: b"cmdline_url" };

    // https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
    // Kitty supports a "redraw" option for prompt_start. This is extended
    // by Ghostty with the "last" option. See Redraw the type for more details.
    pub(crate) const REDRAW: ValueOption<Redraw> = ValueOption {
        key: b"redraw",
        is_exit_code: false,
        decode: |value| {
            if value == b"0" {
                Some(Redraw::False)
            } else if value == b"1" {
                Some(Redraw::True)
            } else if value == b"last" {
                Some(Redraw::Last)
            } else {
                None
            }
        },
    };

    // Use a special key instead of arrow keys to move the cursor on
    // mouse click. Useful if arrow keys have side-effets like triggering
    // auto-complete. The shell integration script should bind the special
    // key as needed.
    // See: https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
    pub(crate) const SPECIAL_KEY: ValueOption<bool> = ValueOption {
        key: b"special_key",
        is_exit_code: false,
        decode: |value| {
            if value.len() == 1 {
                match value[0] {
                    b'0' => Some(false),
                    b'1' => Some(true),
                    _ => None,
                }
            } else {
                None
            }
        },
    };

    // If true, the shell is capable of handling mouse click events.
    // Ghostty will then send a click event to the shell when the user
    // clicks somewhere in the prompt. The shell can then move the cursor
    // to that position or perform some other appropriate action. If false,
    // Ghostty may generate a number of fake key events to move the cursor
    // which is not very robust.
    // See: https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
    pub(crate) const CLICK_EVENTS: ValueOption<ClickEvents> = ValueOption {
        key: b"click_events",
        is_exit_code: false,
        decode: |value| {
            if value.len() == 1 {
                match value[0] {
                    b'1' => Some(ClickEvents::Absolute),
                    b'2' => Some(ClickEvents::Relative),
                    _ => None,
                }
            } else {
                None
            }
        },
    };

    // Not technically an option that can be set with k=v and only
    // present currently with command 'D' but its easier to just
    // parse it into our options.
    pub(crate) const EXIT_CODE: ValueOption<i32> = ValueOption {
        // special case, handled before ever calling key
        key: b"",
        is_exit_code: true,
        decode: |full| crate::parse_int::parse_int::<i32, 10>(full).ok(),
    };
}

/// The `cl` option specifies what kind of cursor key sequences are handled
/// by the application for click-to-move-cursor functionality.
///
/// `line` allows movement within one input line. `multiple` allows movement
/// across lines with left/right sequences. The two vertical modes additionally
/// allow up/down sequences, with `smart_vertical` permitting editor-aware
/// column clamping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Click {
    /// Value: "line". Allows motion within a single input line using standard
    /// left/right arrow escape sequences. Only a single left/right sequence
    /// should be emitted for double-width characters.
    Line,

    /// Value: "m". Allows movement between different lines in the same group,
    /// but only using left/right arrow escape sequences.
    Multiple,

    /// Value: "v". Like `multiple` but cursor up/down should be used. The
    /// terminal should be conservative when moving between lines: move the
    /// cursor left to the start of line, emit the needed up/down sequences,
    /// then move the cursor right to the clicked destination.
    ConservativeVertical,

    /// Value: "w". Like `conservative_vertical` but specifies that there are
    /// no spurious spaces at the end of the line, and the application editor
    /// handles "smart vertical movement" (moving 2 lines up from position 20,
    /// where the intermediate line is 15 chars wide and the destination is
    /// 18 chars wide, ends at position 18).
    SmartVertical,
}

impl Click {
    pub(crate) fn init(value: &[u8]) -> Option<Click> {
        if value.len() == 1 {
            match value[0] {
                b'm' => Some(Click::Multiple),
                b'v' => Some(Click::ConservativeVertical),
                b'w' => Some(Click::SmartVertical),
                _ => None,
            }
        } else if value == b"line" {
            Some(Click::Line)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Initial,
    Right,
    Continuation,
    Secondary,
}

impl PromptKind {
    pub(crate) fn init(c: u8) -> Option<PromptKind> {
        match c {
            b'i' => Some(PromptKind::Initial),
            b'r' => Some(PromptKind::Right),
            b'c' => Some(PromptKind::Continuation),
            b's' => Some(PromptKind::Secondary),
            _ => None,
        }
    }
}

/// The values for the `redraw` extension to OSC133. This was
/// started by Kitty[1] and extended by Ghostty (the "last" option).
///
/// [1]: https://sw.kovidgoyal.net/kitty/shell-integration/#notes-for-shell-developers
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Redraw {
    /// The shell supports redrawing the full prompt and all continuations.
    /// This is the default value, it does not need to be explicitly set
    /// unless it is to reset a prior other value.
    True,

    /// The shell does NOT support redrawing. In this case, Rustty will NOT
    /// clear any prompt lines on resize.
    False,

    /// The shell supports redrawing only the LAST line of the prompt.
    /// Rustty will only clear the last line of the prompt on resize.
    ///
    /// This is specifically introduced because Bash only redraws the last
    /// line. It is literally the only shell that does this and it does this
    /// because its bad and they should feel bad. Don't be like Bash.
    Last,
}

/// Parse OSC 133, semantic prompts
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<OscCommand<'_>> {
    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    let data = cap.trailing(&parser.buffer);
    let command = data.split_first().and_then(|(&action, rest)| {
        let action = match action {
            b'A' => Action::FreshLineNewPrompt,
            b'B' => Action::EndPromptStartInput,
            b'I' => Action::EndPromptStartInputTerminateEol,
            b'C' => Action::EndInputStartOutput,
            b'D' => Action::EndCommand,
            // `L` takes no options.
            b'L' if rest.is_empty() => Action::FreshLine,
            b'N' => Action::NewCommand,
            b'P' => Action::PromptStart,
            _ => return None,
        };
        let options_unvalidated = match rest {
            [] => &[][..],
            [b';', options @ ..] => options,
            _ => return None,
        };
        Some(Command {
            action,
            options_unvalidated,
        })
    });
    if command.is_none() {
        // Any fallthroughs are invalid
        parser.state = State::Invalid;
    }
    command.map(OscCommand::SemanticPrompt)
}

#[cfg(test)]
mod tests;
