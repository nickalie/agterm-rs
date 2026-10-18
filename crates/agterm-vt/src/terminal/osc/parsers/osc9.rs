//! OSC 9: an iTerm2 notification, or one of `ConEmu`'s extensions (<https://conemu.github.io/en/AnsiEscapeCodes.html>).
//!
//! Ghostty: `src/terminal/osc/parsers/osc9.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/osc/parsers/osc9.rs` at `RUSTTY_REV` (MIT).
//! Changes: the body is classified first ([`classify`]), then the commands that carry text write Zig's NUL sentinel
//! (refused by a full capture, as in Zig) and borrow their text without it.

use crate::terminal::osc::parsers::semantic_prompt;
use crate::terminal::osc::{Command, ConemuChangeTabTitle, Parser, ProgressReport, ProgressReportState, State};

/// A command built from the text after a `ConEmu` prefix.
type TextCommand = for<'a> fn(&'a [u8]) -> Command<'a>;

/// What an OSC 9 body is, before any text is borrowed.
enum Kind {
    /// A `ConEmu` command that carries no text.
    Done(Command<'static>),
    /// A `ConEmu` command carrying the text from `start` on.
    Text { start: usize, make: TextCommand },
    /// Not `ConEmu`: an iTerm2 notification whose body is the whole text.
    Notification,
}

/// Parse OSC 9, which could be an iTerm2 notification or a `ConEmu` extension.
pub(crate) fn parse(parser: &mut Parser, _: Option<u8>) -> Option<Command<'_>> {
    let Some(cap) = parser.capture.as_ref() else {
        parser.state = State::Invalid;
        return None;
    };
    // Check first to see if this is a ConEmu OSC
    let (start, make) = match classify(cap.trailing(&parser.buffer)) {
        Kind::Done(command) => return Some(command),
        Kind::Text { start, make } => (start, make),
        // If it's not a ConEmu OSC, it's an iTerm2 notification
        Kind::Notification => (0, notification as TextCommand),
    };
    parser.terminated_trailing().map(|text| make(&text[start..]))
}

/// The iTerm2 notification with body `body` and no title.
fn notification(body: &[u8]) -> Command<'_> {
    Command::ShowDesktopNotification { title: b"", body }
}

/// OSC 9;9: the working directory.
fn report_pwd(value: &[u8]) -> Command<'_> {
    Command::ReportPwd { value }
}

/// OSC 9;3 with a title.
fn change_tab_title(value: &[u8]) -> Command<'_> {
    Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Value(value))
}

/// OSC 9;2: show a message box.
fn message_box(text: &[u8]) -> Command<'_> {
    Command::ConemuShowMessageBox(text)
}

/// OSC 9;6: a GUI macro.
fn guimacro(text: &[u8]) -> Command<'_> {
    Command::ConemuGuimacro(text)
}

/// OSC 9;7: run a process.
fn run_process(text: &[u8]) -> Command<'_> {
    Command::ConemuRunProcess(text)
}

/// OSC 9;8: output an environment variable.
fn output_environment_variable(text: &[u8]) -> Command<'_> {
    Command::ConemuOutputEnvironmentVariable(text)
}

/// OSC 9;11: a comment.
fn comment(text: &[u8]) -> Command<'_> {
    Command::ConemuComment(text)
}

/// What the body `data` is (data before the sentinel).
fn classify(data: &[u8]) -> Kind {
    let text = |start, make| Kind::Text { start, make };
    match data {
        // Check for OSC 9;1 9;10 9;11 9;12
        [b'1', rest @ ..] => classify_1(rest),
        // OSC 9;2 show message box
        [b'2', b';', ..] => text(2, message_box),
        // OSC 9;3 change tab title
        [b'3', b';'] => Kind::Done(Command::ConemuChangeTabTitle(ConemuChangeTabTitle::Reset)),
        [b'3', b';', ..] => text(2, change_tab_title),
        // OSC 9;4 progress report
        [b'4', b';', state, rest @ ..] => progress_report(*state, rest).map_or(Kind::Notification, |report| {
            Kind::Done(Command::ConemuProgressReport(report))
        }),
        // OSC 9;5 wait for input
        [b'5', ..] => Kind::Done(Command::ConemuWaitInput),
        // OSC 9;6 guimacro
        [b'6', b';', ..] => text(2, guimacro),
        // OSC 9;7 run process
        [b'7', b';', ..] => text(2, run_process),
        // OSC 9;8 output environment variable
        [b'8', b';', ..] => text(2, output_environment_variable),
        // OSC 9;9 current working directory
        [b'9', b';', ..] => text(2, report_pwd),
        _ => Kind::Notification,
    }
}

/// The OSC 9;1x forms, `rest` being the body after the `1`.
fn classify_1(rest: &[u8]) -> Kind {
    match rest {
        // OSC 9;1 sleep
        [b';', duration @ ..] => Kind::Done(Command::ConemuSleep {
            duration_ms: crate::parse_int::parse_int::<u16, 10>(duration).map_or(100, |num| num.min(10_000)),
        }),
        // OSC 9;10 xterm keyboard and output emulation
        [b'0'] => xterm_emulation(Some(true), Some(true)),
        [b'0', b';', mode, ..] => match mode {
            b'0' => xterm_emulation(Some(false), Some(false)),
            b'1' => xterm_emulation(Some(true), Some(true)),
            b'2' => xterm_emulation(None, Some(false)),
            b'3' => xterm_emulation(None, Some(true)),
            _ => Kind::Notification,
        },
        // OSC 9;11 comment
        [b'1', b';', ..] => Kind::Text {
            start: 3,
            make: comment,
        },
        // OSC 9;12 mark prompt start
        [b'2', ..] => Kind::Done(Command::SemanticPrompt(semantic_prompt::Command::init(
            semantic_prompt::Action::FreshLineNewPrompt,
        ))),
        _ => Kind::Notification,
    }
}

/// OSC 9;10 with the keyboard and output emulation it sets (`None`: unchanged).
fn xterm_emulation(keyboard: Option<bool>, output: Option<bool>) -> Kind {
    Kind::Done(Command::ConemuXtermEmulation { keyboard, output })
}

/// OSC 9;4: the report of state digit `state`, then `rest` (`;progress` for the states that take one). `None` for an
/// unknown state.
fn progress_report(state: u8, rest: &[u8]) -> Option<ProgressReport> {
    let (state, progress) = match state {
        b'0' => (ProgressReportState::Remove, None),
        b'1' => (ProgressReportState::Set, Some(0)),
        b'2' => (ProgressReportState::Error, None),
        b'3' => (ProgressReportState::Indeterminate, None),
        b'4' => (ProgressReportState::Pause, None),
        _ => return None,
    };
    let progress = match (state, rest) {
        (ProgressReportState::Remove | ProgressReportState::Indeterminate, _) => progress,
        // parse the progress value
        (_, [b';', value @ ..]) => crate::parse_int::parse_int::<usize, 10>(value)
            .ok()
            .map(|v| u8::try_from(v.min(100)).unwrap_or(100)),
        (_, _) => progress,
    };
    Some(ProgressReport { state, progress })
}

#[cfg(test)]
mod tests;
