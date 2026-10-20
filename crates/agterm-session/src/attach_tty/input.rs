//! What the outer terminal sends the attach client: the user's input, which goes to the session, and the outer
//! terminal's answers, which never do. The answers to the client's own queries (`super::owner`) are picked out of the
//! stream; every other answer or report is dropped, since the session server answers the program's queries itself
//! (`super::filter` keeps them away from the outer terminal, so an answer here is the client's, or a report the server
//! sends on its own: in-band resize, colour scheme).
//!
//! Recognized, as Ghostty and xterm send them (`ctlseqs`): DA1's answer `CSI ? … c` (the client's fence), OSC 10, 11
//! and 12 and OSC 4 colours (`rgb:r/g/b` with 1 to 4 hex digits per channel, or `#` hex), an OSC 52 clipboard's
//! base64, `CSI ? 997 ; 1|2 n` (dark or light), the cell size `CSI 6 ; h ; w t`, and Kitty graphics answers
//! (`ESC _ G … ESC \`). Dropped: other OSC, DCS, SOS, PM and APC strings, DA2 and DA3
//! answers (`CSI > … c`, `CSI = … c`), DSR answers and reports (`CSI … n`), DECRPM (`CSI … $ y`), the Kitty keyboard
//! flags (`CSI ? … u`) and every other `t` report (`CSI 48 ; … t`). Keys, mouse and focus reports pass (a CPR-shaped
//! `CSI … R` is a function key: the outer terminal is never asked for the cursor), and so does a bracketed paste,
//! whole and unparsed, up to its end.
//!
//! A sequence cut at the end of a read, a lone ESC included, is held for the next one; [`InputParser::flush`] gives it
//! up once the input stays quiet, for as long as [`InputParser::held`] says it may be waiting (the attach loop's
//! escape time for what may be a key, longer for what may be an answer while one is due): what may be a key is then
//! input, and so is what may be an answer when none is due; a cut answer while one is due is dropped. A cut end of a
//! bracketed paste is never given up: the paste goes on until its end. An answer can come cut anywhere: a Windows
//! console hands the characters of one over as they are parsed, a few per read.
//!
//! No answer holds a control inside it: a C0 byte (but the BEL ending an OSC and the ESC of an ST), DEL, or for a
//! CSI any byte past its parameters but a final, ends what is held then and there as typed (Escape or an Alt chord
//! followed by more keys: readline's `M-_` and Enter, vim's Escape, `P` and an arrow), and the control is read on its
//! own, so the detach key still detaches.
//!
//! # A clipboard answer keeps its framing
//!
//! While a clipboard read is asked of the outer terminal and its answer has not come
//! ([`InputParser::set_clipboard_due`], the relay's `clipboard_asked`, whether or not the read's request is still
//! live), an OSC 52 answer is framed once its header (`ESC ] 5 2`, then `;` or the read's end) is recognized: it is
//! read up to its BEL or ST (an ESC at a read's end waits for the next byte to say whether it starts the ST), however
//! long the pauses inside it, and no quiet period gives it up, types it or forgets it. Its end gives the relay the
//! answer (`Answer::Clipboard`, `None` for a payload that is no base64 or past [`STRING_LIMIT`], whose bytes are
//! discarded up to the end), which completes the read, or is discarded when the read's request is gone, so the next
//! read is asked. A control inside the framing (a C0 byte but the BEL, DEL, an ESC that does not start the ST: the
//! user's key, as base64 holds none) drops the framed bytes untyped, leaves the read asked, and is read on its own:
//! Ctrl+\ still detaches, Enter still reaches the program. Collected clipboard bytes are never replayed as keys.
//!
//! What is left: a header cut before its `52` (`ESC ]`, a key, or `ESC ] 5`, answer-shaped) is held as any other cut
//! sequence, so a pause there past its wait types it, and the rest of the answer after it; and a control that is
//! garbage inside a real answer ends its framing, so the printable bytes after it are typed. The other way round,
//! keys typed while a read is asked that start as the header does (Alt+], `5`, `2`) are taken for the answer, and
//! dropped up to the next control.
//!
//! The detach key (zmx's `isCtrlBackslash`, `src/util.zig` at `8bab1f0`, MIT, NOTICE): Ctrl+\ typed (anywhere in a read
//! outside a bracketed paste, where zmx looked at a read's first byte only: a console's reads do not follow the
//! keys), or encoded by the Kitty keyboard protocol (`CSI 92 ; 5 u`, a press) or xterm's modifyOtherKeys
//! (`CSI 27 ; 5 ; 92 ~`), lock modifiers tolerated. What came before it in the read goes to the program first.

use base64::Engine as _;

use crate::protocol::{ColorScheme, Rgb};

const BEL: u8 = 0x07;
const ESC: u8 = 0x1B;
/// Ctrl+\ as a plain control byte.
const CTRL_BACKSLASH: u8 = 0x1C;

/// The longest CSI looked for an end: past it the ESC is input.
const CSI_LIMIT: usize = 256;
/// The longest string (OSC, DCS, APC) looked for an end: an OSC 52 answer carries a whole clipboard.
pub(crate) const STRING_LIMIT: usize = 64 * 1024 * 1024;

/// The header an OSC 52 answer is framed from (module docs).
const CLIPBOARD_HEADER: &[u8] = b"\x1b]52";

/// The end of a bracketed paste.
const PASTE_END: &[u8] = b"\x1b[201~";

/// A dynamic colour the outer terminal answers for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dynamic {
    Foreground,
    Background,
    Cursor,
}

/// An answer of the outer terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// DA1's answer: everything the outer terminal answered before it is in.
    PrimaryAttributes,
    /// OSC 10, 11 or 12; `None` for a colour it did not read.
    Dynamic { which: Dynamic, color: Option<Rgb> },
    /// OSC 4.
    Palette { index: u8, color: Option<Rgb> },
    /// OSC 52: the clipboard's bytes; `None` for a payload that is not base64.
    Clipboard(Option<Vec<u8>>),
    /// `CSI ? 997 ; Ps n`, answering `CSI ? 996 n` or reporting a change under mode 2031.
    ColorScheme(ColorScheme),
    /// `CSI 6 ; h ; w t`: one cell, in pixels.
    CellSize { width: u32, height: u32 },
    /// A Kitty graphics answer, whole (`ESC _ G … ESC \`).
    Kitty(Vec<u8>),
}

/// What a read of the outer terminal holds, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// Bytes for the session's program.
    Input(Vec<u8>),
    Answer(Answer),
    /// The detach key: leave the session running.
    Detach,
}

/// What a cut sequence held may be, which says how long it waits for its end ([`InputParser::held`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// A lone ESC, or ESC and one byte: a key (Escape, an Alt chord) as likely as the start of a sequence.
    Key,
    /// A CSI without a private prefix: a key's (an arrow with modifiers, a function key) or an answer's.
    Csi,
    /// What the outer terminal's answers start with: an OSC with a number, a Kitty graphics APC (`ESC _ G`), a DCS as
    /// a report starts (a digit, `>` or `!`), or a CSI with a `?`, `>` or `=` prefix. Typed, it is Escape or an Alt
    /// chord followed by more keys, so it is an answer only while one is due.
    Answer,
}

/// One sequence at the front of the bytes.
enum Parsed {
    /// Its length, and what it is.
    Complete(usize, Kind),
    /// It may go on in the next read.
    Incomplete,
}

enum Kind {
    /// Input for the program: a key, a mouse or focus report.
    Input,
    /// The start of a bracketed paste.
    PasteStart,
    Answer(Answer),
    /// An answer or report nobody reads.
    Dropped,
    Detach,
}

/// The OSC 52 answer being read in its framing (module docs).
#[derive(Debug)]
struct ClipboardFrame {
    /// What follows its `ESC ]` so far; `None` past [`STRING_LIMIT`], its bytes discarded up to its end.
    content: Option<Vec<u8>>,
}

impl ClipboardFrame {
    fn extend(&mut self, bytes: &[u8]) {
        if let Some(content) = &mut self.content {
            if content.len() + bytes.len() > STRING_LIMIT {
                self.content = None;
            } else {
                content.extend_from_slice(bytes);
            }
        }
    }
}

/// The input parser of one attach client (module docs).
#[derive(Debug, Default)]
pub struct InputParser {
    /// The cut sequence a read ended in.
    held: Vec<u8>,
    /// Inside a bracketed paste.
    paste: bool,
    /// Whether the detach key detaches.
    detach_key: bool,
    /// An OSC 52 answer is due ([`InputParser::set_clipboard_due`]).
    clipboard_due: bool,
    /// The OSC 52 answer being read.
    clipboard: Option<ClipboardFrame>,
}

impl InputParser {
    #[must_use]
    pub fn new(detach_key: bool) -> Self {
        Self {
            held: Vec::new(),
            paste: false,
            detach_key,
            clipboard_due: false,
            clipboard: None,
        }
    }

    /// Whether the reads from here on may hold an OSC 52 answer: a clipboard read was asked of the outer terminal and
    /// its answer has not come. Its answer is framed then (module docs).
    pub fn set_clipboard_due(&mut self, due: bool) {
        self.clipboard_due = due;
    }

    /// Whether a cut sequence waits for more input.
    #[must_use]
    pub fn holding(&self) -> bool {
        !self.held.is_empty() || self.clipboard.is_some()
    }

    /// What the cut sequence held may be; `None` for none, or for a cut end of a bracketed paste or of a framed OSC 52
    /// answer, which waits for the rest however long it takes.
    #[must_use]
    pub fn held(&self) -> Option<Held> {
        if self.paste || self.clipboard.is_some() || self.held.is_empty() {
            return None;
        }
        // what follows the ESC a cut sequence starts with
        Some(match &self.held[1..] {
            [] | [_] => Held::Key,
            [b'[', b'?' | b'>' | b'=', ..]
            | [b']', b'0'..=b'9', ..]
            | [b'_', b'G', ..]
            | [b'P', b'0'..=b'9' | b'>' | b'!', ..] => Held::Answer,
            [b'[', ..] => Held::Csi,
            _ => Held::Key,
        })
    }

    /// The tokens of one read.
    pub fn feed(&mut self, read: &[u8]) -> Vec<Token> {
        let mut bytes = std::mem::take(&mut self.held);
        bytes.extend_from_slice(read);
        let mut tokens = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            if self.paste {
                at += self.paste_run(&bytes[at..], &mut tokens);
                if self.paste {
                    break;
                }
                continue;
            }
            if self.clipboard.is_some() {
                at += self.clipboard_run(&bytes[at..], &mut tokens);
                continue;
            }
            let rest = &bytes[at..];
            if rest[0] != ESC {
                let detach_key = self.detach_key;
                let run = rest
                    .iter()
                    .position(|byte| *byte == ESC || (detach_key && *byte == CTRL_BACKSLASH))
                    .unwrap_or(rest.len());
                push_input(&mut tokens, &rest[..run]);
                if rest.get(run) == Some(&CTRL_BACKSLASH) {
                    tokens.push(Token::Detach);
                    break;
                }
                at += run;
                continue;
            }
            if self.clipboard_due && clipboard_header(rest) {
                self.clipboard = Some(ClipboardFrame {
                    content: Some(b"52".to_vec()),
                });
                at += CLIPBOARD_HEADER.len();
                continue;
            }
            match self.sequence(rest) {
                Parsed::Complete(length, kind) => {
                    self.take(kind, &rest[..length], &mut tokens);
                    at += length;
                }
                Parsed::Incomplete => {
                    self.held = rest.to_vec();
                    break;
                }
            }
            if tokens.last() == Some(&Token::Detach) {
                break;
            }
        }
        tokens
    }

    /// The cut sequence held, given up once the input stayed quiet: dropped when it may be an answer and one is due
    /// (`answer_due`), else input (module docs). A cut end of a bracketed paste or of a framed OSC 52 answer stays
    /// held.
    pub fn flush(&mut self, answer_due: bool) -> Vec<Token> {
        let mut tokens = Vec::new();
        match self.held() {
            None => {}
            Some(Held::Answer) if answer_due => self.held.clear(),
            Some(_) => push_input(&mut tokens, &std::mem::take(&mut self.held)),
        }
        tokens
    }

    /// A paste's bytes up to its end, passed whole; how many it took. Bytes that may start the end marker are held.
    fn paste_run(&mut self, bytes: &[u8], tokens: &mut Vec<Token>) -> usize {
        if let Some(end) = bytes.windows(PASTE_END.len()).position(|window| window == PASTE_END) {
            push_input(tokens, &bytes[..end + PASTE_END.len()]);
            self.paste = false;
            return end + PASTE_END.len();
        }
        let keep = (1..PASTE_END.len())
            .rev()
            .find(|&length| bytes.len() >= length && PASTE_END.starts_with(&bytes[bytes.len() - length..]))
            .unwrap_or(0);
        push_input(tokens, &bytes[..bytes.len() - keep]);
        self.held = bytes[bytes.len() - keep..].to_vec();
        bytes.len()
    }

    /// A framed OSC 52 answer's bytes up to its end (its answer taken), or to a control that ends its framing (its
    /// bytes dropped, the control left to be read on its own); how many it took. An ESC at the end is held: the next
    /// byte says whether it starts the ST.
    fn clipboard_run(&mut self, bytes: &[u8], tokens: &mut Vec<Token>) -> usize {
        let Some(frame) = &mut self.clipboard else {
            return 0;
        };
        let Some(at) = bytes.iter().position(|byte| matches!(byte, 0x00..=0x1F | 0x7F)) else {
            frame.extend(bytes);
            return bytes.len();
        };
        frame.extend(&bytes[..at]);
        let terminator = match (bytes[at], bytes.get(at + 1)) {
            (BEL, _) => 1,
            (ESC, Some(b'\\')) => 2,
            (ESC, None) => {
                self.held = vec![ESC];
                return bytes.len();
            }
            // a key: the framing ends, nothing of it typed
            _ => {
                self.clipboard = None;
                return at;
            }
        };
        let data = self
            .clipboard
            .take()
            .and_then(|frame| frame.content)
            .and_then(|content| clipboard_data(&content));
        tokens.push(Token::Answer(Answer::Clipboard(data)));
        at + terminator
    }

    fn take(&mut self, kind: Kind, bytes: &[u8], tokens: &mut Vec<Token>) {
        match kind {
            Kind::Input => push_input(tokens, bytes),
            Kind::PasteStart => {
                push_input(tokens, bytes);
                self.paste = true;
            }
            Kind::Answer(answer) => tokens.push(Token::Answer(answer)),
            Kind::Dropped => {}
            Kind::Detach => tokens.push(Token::Detach),
        }
    }

    /// The sequence `bytes` starts with (an ESC).
    fn sequence(&self, bytes: &[u8]) -> Parsed {
        let Some(&kind) = bytes.get(1) else {
            return Parsed::Incomplete;
        };
        match kind {
            b'[' => self.csi(bytes),
            b']' | b'P' | b'_' | b'^' | b'X' => string(bytes),
            b'O' => match bytes.get(2) {
                Some(_) => Parsed::Complete(3, Kind::Input),
                None => Parsed::Incomplete,
            },
            // ESC ESC: the first is a key of its own (Alt+Escape, or Escape before a sequence)
            ESC => Parsed::Complete(1, Kind::Input),
            _ => Parsed::Complete(2, Kind::Input),
        }
    }

    fn csi(&self, bytes: &[u8]) -> Parsed {
        // past the parameters and intermediates
        let Some(end) = bytes.iter().skip(2).position(|byte| !(0x20..0x40).contains(byte)) else {
            return if bytes.len() > CSI_LIMIT {
                // no CSI: the ESC is a key
                Parsed::Complete(1, Kind::Input)
            } else {
                Parsed::Incomplete
            };
        };
        let at = end + 2;
        if !(0x40..=0x7E).contains(&bytes[at]) {
            // a control or a character: no key or answer goes on so, what came before it was typed (module docs)
            return Parsed::Complete(at, Kind::Input);
        }
        let length = at + 1;
        Parsed::Complete(length, self.classify_csi(&bytes[..length]))
    }

    fn classify_csi(&self, sequence: &[u8]) -> Kind {
        let body = &sequence[2..sequence.len() - 1];
        let final_byte = sequence[sequence.len() - 1];
        let (prefix, params) = match body.split_first() {
            Some((&first @ (b'?' | b'>' | b'=' | b'<'), rest)) => (Some(first), rest),
            _ => (None, body),
        };
        let numbers = numbers(params);
        match (final_byte, prefix) {
            (b'c', Some(b'?')) => Kind::Answer(Answer::PrimaryAttributes),
            (b'c', Some(b'>' | b'=')) | (b'u', Some(b'?')) | (b'n', None) => Kind::Dropped,
            (b'n', Some(b'?')) => match numbers.as_slice() {
                [Some(997), Some(1)] => Kind::Answer(Answer::ColorScheme(ColorScheme::Dark)),
                [Some(997), Some(2)] => Kind::Answer(Answer::ColorScheme(ColorScheme::Light)),
                _ => Kind::Dropped,
            },
            (b't', None) => match numbers.as_slice() {
                [Some(6), Some(height), Some(width)] => Kind::Answer(Answer::CellSize {
                    width: *width,
                    height: *height,
                }),
                _ => Kind::Dropped,
            },
            (b'y', _) if params.ends_with(b"$") => Kind::Dropped,
            (b'~', None) if params == b"200" => Kind::PasteStart,
            _ if self.detach_key && is_detach(final_byte, prefix, params) => Kind::Detach,
            _ => Kind::Input,
        }
    }
}

/// Appends `bytes` to the input at the end of `tokens`.
fn push_input(tokens: &mut Vec<Token>, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    if let Some(Token::Input(input)) = tokens.last_mut() {
        input.extend_from_slice(bytes);
    } else {
        tokens.push(Token::Input(bytes.to_vec()));
    }
}

/// The `;`-separated numbers of CSI parameters; `None` for one that is not a plain number.
fn numbers(params: &[u8]) -> Vec<Option<u32>> {
    if params.is_empty() {
        return Vec::new();
    }
    params
        .split(|byte| *byte == b';')
        .map(|field| std::str::from_utf8(field).ok()?.parse().ok())
        .collect()
}

/// Whether a CSI is Ctrl+\ pressed: Kitty's `CSI 92 ; 5[:1] u` or modifyOtherKeys' `CSI 27 ; 5 ; 92 ~`, with Caps
/// Lock and Num Lock ignored (zmx's `keypressWithMod`, `modifyOtherMatches`).
fn is_detach(final_byte: u8, prefix: Option<u8>, params: &[u8]) -> bool {
    if prefix.is_some() {
        return false;
    }
    let fields: Vec<&[u8]> = params.split(|byte| *byte == b';').collect();
    let ctrl = |modifiers: &[u8]| {
        let mut parts = modifiers.split(|byte| *byte == b':');
        let encoded = parts
            .next()
            .and_then(|value| std::str::from_utf8(value).ok()?.parse::<u32>().ok());
        let event = parts.next();
        let pressed = event.is_none_or(|event| event == b"1" || event == b"2");
        encoded.is_some_and(|encoded| encoded >= 1 && (encoded - 1) & 0b0011_1111 == 0b100) && pressed
    };
    match (final_byte, fields.as_slice()) {
        (b'u', [key, modifiers, ..]) => key.split(|byte| *byte == b':').next() == Some(b"92") && ctrl(modifiers),
        (b'~', [sentinel, modifiers, key]) => *sentinel == b"27" && *key == b"92" && ctrl(modifiers),
        _ => false,
    }
}

/// A string sequence (OSC, DCS, APC, SOS, PM) at the front of `bytes`, up to its ST (or BEL, for an OSC). A control
/// before it (an ESC starting anything but the ST included) ends it as typed: the ESC and the introducer are a key, and
/// what follows is read again (module docs).
fn string(bytes: &[u8]) -> Parsed {
    let osc = bytes[1] == b']';
    let body = &bytes[2..];
    for (at, &byte) in body.iter().enumerate() {
        let end = match byte {
            BEL if osc => Some((at, 1)),
            ESC => match body.get(at + 1) {
                Some(b'\\') => Some((at, 2)),
                None => return Parsed::Incomplete,
                Some(_) => return Parsed::Complete(2, Kind::Input),
            },
            0x00..=0x1F | 0x7F => return Parsed::Complete(2, Kind::Input),
            _ => None,
        };
        if let Some((content, terminator)) = end {
            let length = 2 + content + terminator;
            let kind = classify_string(bytes[1], &body[..content], &bytes[..length]);
            return Parsed::Complete(length, kind);
        }
    }
    if bytes.len() > STRING_LIMIT {
        // no string: the ESC is a key
        Parsed::Complete(1, Kind::Input)
    } else {
        Parsed::Incomplete
    }
}

/// The bytes after an ESC start an OSC 52 answer's header: `ESC ] 5 2`, then `;` or the read's end.
fn clipboard_header(bytes: &[u8]) -> bool {
    bytes.starts_with(CLIPBOARD_HEADER) && bytes.get(CLIPBOARD_HEADER.len()).is_none_or(|byte| *byte == b';')
}

/// The clipboard's bytes in a framed OSC 52 answer's content (`52;<selection>;<base64>`); `None` for one that is not
/// so.
fn clipboard_data(content: &[u8]) -> Option<Vec<u8>> {
    match osc(content) {
        Some(Answer::Clipboard(data)) => data,
        _ => None,
    }
}

/// What a whole string is: an answer the client reads, or one dropped.
fn classify_string(kind: u8, content: &[u8], whole: &[u8]) -> Kind {
    match kind {
        b']' => osc(content).map_or(Kind::Dropped, Kind::Answer),
        b'_' if content.first() == Some(&b'G') => Kind::Answer(Answer::Kitty(whole.to_vec())),
        _ => Kind::Dropped,
    }
}

fn osc(content: &[u8]) -> Option<Answer> {
    let mut fields = content.split(|byte| *byte == b';');
    let number = fields.next()?;
    let rest: Vec<&[u8]> = fields.collect();
    let which = match number {
        b"10" => Dynamic::Foreground,
        b"11" => Dynamic::Background,
        b"12" => Dynamic::Cursor,
        b"4" => {
            let [index, spec] = rest.as_slice() else {
                return None;
            };
            return Some(Answer::Palette {
                index: std::str::from_utf8(index).ok()?.parse().ok()?,
                color: parse_color(spec),
            });
        }
        b"52" => {
            let [_, data] = rest.as_slice() else {
                return None;
            };
            return Some(Answer::Clipboard(
                base64::engine::general_purpose::STANDARD.decode(data).ok(),
            ));
        }
        _ => return None,
    };
    Some(Answer::Dynamic {
        which,
        color: rest.first().and_then(|spec| parse_color(spec)),
    })
}

/// An X11 colour spec as terminals answer with: `rgb:r/g/b` (1 to 4 hex digits per channel, scaled to 8 bits) or
/// `#` with 1 to 4 hex digits per channel.
#[must_use]
pub fn parse_color(spec: &[u8]) -> Option<Rgb> {
    let spec = std::str::from_utf8(spec).ok()?;
    if let Some(channels) = spec.strip_prefix("rgb:") {
        let parts: Vec<&str> = channels.split('/').collect();
        let [r, g, b] = parts.as_slice() else {
            return None;
        };
        return Some(Rgb {
            r: channel(r)?,
            g: channel(g)?,
            b: channel(b)?,
        });
    }
    let hex = spec.strip_prefix('#')?;
    if hex.is_empty() || hex.len() % 3 != 0 || hex.len() > 12 {
        return None;
    }
    let width = hex.len() / 3;
    Some(Rgb {
        r: channel(hex.get(..width)?)?,
        g: channel(hex.get(width..2 * width)?)?,
        b: channel(hex.get(2 * width..)?)?,
    })
}

/// One channel of 1 to 4 hex digits, scaled to 8 bits.
fn channel(digits: &str) -> Option<u8> {
    if digits.is_empty() || digits.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    let max = (1_u32 << (4 * digits.len())) - 1;
    u8::try_from((value * 255 + max / 2) / max).ok()
}

#[cfg(test)]
mod tests;
