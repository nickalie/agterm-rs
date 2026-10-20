//! The session's output on its way to the outer terminal: every query the session server answers itself taken out, so
//! the outer terminal answers none of the program's queries (the server's one answer is the only one), and the places
//! where the stream stands at ground found, where the attach client may write its own sequences (a role title, a
//! fence, a query of its own) without splitting one of the program's.
//!
//! The filter follows the states of Ghostty's VT parser (`src/terminal/parse_table.zig` at `LIBGHOSTTY_VT_REV`, its
//! "anywhere" transitions included), as [`agterm_ghostty_vt::Scanner`] mirrors them (`agterm-ghostty-vt`'s
//! `scan.rs`); a sequence that may be a query is held until it is complete, whatever the reads it comes in, then
//! dropped, passed on, or passed on rewritten. A sequence that cannot be a query is passed on as it comes. Held:
//!
//! - every CSI, up to [`CSI_LIMIT`] bytes (a longer one is passed on as it comes). Dropped: DA1, DA2 and DA3
//!   (`CSI [>=] Ps c`), DSR and CPR (`CSI [?] Ps n`, `CSI ? 996 n` included), DECRQM (`CSI [?] Ps $ p`), XTVERSION
//!   (`CSI > Ps q`), the Kitty keyboard flags query (`CSI ? u`) and the XTWINOPS reports Ghostty answers
//!   (`CSI 14/16/18/21 t`). Rewritten: DECSET and DECRST (`CSI ? Pm h|l`) lose modes 2031 and 2048, whose reports the
//!   server sends from its own terminal (and the attach client keeps its own 2031 subscription on the outer terminal:
//!   [`OutputFilter::after_reset`] puts it back after every RIS that passes, which turns it off),
//!   and mode 9001, win32-input-mode, which a Windows session's pseudoconsole asks of its terminal as it starts: an
//!   outer terminal that took it would encode its input, its answers to the client and the detach key included, as
//!   win32 key records the client does not read (`super::input` reads VT), and the session's pseudoconsole reads the
//!   plain VT the client forwards as well. A C0 control inside a CSI executes in passing, as in Ghostty's parser: the
//!   CSI is classified without it, and a dropped or rewritten one keeps its controls, ahead of what is written;
//! - every OSC, up to [`STRING_LIMIT`] bytes. Dropped: the colour queries (OSC 10 to 19 with a `?`, Kitty's OSC 21
//!   with a `key=?`), clipboard reads (OSC 52 `?`, Kitty's OSC 5522 `type=read`); OSC 4 loses its `index;?` pairs.
//!   Rewritten: a prompt mark `OSC 133;A` gets `redraw=0` (zmx's `rewritePromptRedraw`, `src/util.zig` at `8bab1f0`,
//!   MIT, NOTICE): the outer terminal must not clear the prompt's rows on a resize expecting the shell to redraw them,
//!   since the shell redraws through the session, laid out for the session's grid;
//! - a DCS's header: DECRQSS (`DCS $ q`) and XTGETTCAP (`DCS + q`) are dropped with their strings;
//! - ESC Z (DECID, Ghostty's DA1) is dropped, and so is ENQ (Ghostty's answerback).
//!
//! A string the filter dropped and ended with ESC takes the `\` of its ST with it. The Kitty graphics commands (APC G)
//! pass: the outer terminal shows the images, and the reply owner's terminal answers them (`super::owner`).

/// The longest CSI held for classification: a longer one is no query, and is passed on as it comes.
pub const CSI_LIMIT: usize = 256;

/// The longest OSC held for classification: a longer one (a clipboard write, a notification) is no query, and is
/// passed on as it comes.
pub const STRING_LIMIT: usize = 4096;

const ENQ: u8 = 0x05;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1A;
const ESC: u8 = 0x1B;

/// The parser states the filter tracks (Ghostty's, `parse_table.zig`; the CSI ones folded into one, as
/// `agterm-ghostty-vt`'s scanner folds them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    SosPmApc,
    Osc,
}

/// What becomes of the bytes of the sequence in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Held until the sequence can be classified.
    Hold,
    /// Passed on as they come.
    Pass,
    /// Dropped.
    Drop,
}

/// What a complete sequence becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Keep,
    Drop,
    /// Passed on as these bytes instead.
    Rewrite(Vec<u8>),
}

/// The output filter of one attach stream (module docs). Its state carries across reads and across snapshots, as the
/// session's stream does.
#[derive(Debug)]
pub struct OutputFilter {
    state: State,
    fate: Fate,
    /// The sequence held, from its introducer on.
    held: Vec<u8>,
    /// Where an OSC's body starts in `held` (after `ESC ]` or the C1 OSC).
    body: usize,
    /// UTF-8 continuation bytes still due in ground.
    utf8: u8,
    /// The last string ended with ESC, and was dropped: a `\` right after it is its ST.
    dropped_st: bool,
    /// Written after every RIS that passes.
    after_reset: &'static [u8],
}

impl Default for OutputFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputFilter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            fate: Fate::Pass,
            held: Vec::with_capacity(64),
            body: 0,
            utf8: 0,
            dropped_st: false,
            after_reset: &[],
        }
    }

    /// Writes `bytes` after every RIS (`ESC c`) that passes, where the stream stands at ground.
    pub fn after_reset(&mut self, bytes: &'static [u8]) {
        self.after_reset = bytes;
    }

    /// Whether the stream stands at ground: no sequence and no UTF-8 character in progress, so bytes written now
    /// split nothing of the program's.
    #[must_use]
    pub fn at_ground(&self) -> bool {
        self.state == State::Ground && self.utf8 == 0
    }

    /// Filters all of `bytes` into `out`.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut rest = bytes;
        while !rest.is_empty() {
            let taken = if self.state == State::Ground {
                self.ground_run(rest, out)
            } else {
                self.step(rest[0], out);
                1
            };
            rest = &rest[taken..];
        }
    }

    /// Filters `bytes` into `out` up to the first byte after which the stream stands at ground; returns how many bytes
    /// it took (all of them when it never reached ground).
    pub fn feed_until_ground(&mut self, bytes: &[u8], out: &mut Vec<u8>) -> usize {
        for (at, &byte) in bytes.iter().enumerate() {
            self.step(byte, out);
            if self.at_ground() {
                return at + 1;
            }
        }
        bytes.len()
    }

    /// Ends the sequence in progress as CAN ends it in every terminal: what was held is passed on, then CAN, and the
    /// filter stands at ground. Writing CAN within a UTF-8 character ends that character too.
    pub fn cancel(&mut self, out: &mut Vec<u8>) {
        self.release(out);
        out.push(CAN);
        self.state = State::Ground;
        self.utf8 = 0;
        self.dropped_st = false;
    }

    /// Ground bytes up to the next ESC (taken too, as the start of a sequence) or ENQ (dropped), in one copy.
    fn ground_run(&mut self, bytes: &[u8], out: &mut Vec<u8>) -> usize {
        for (at, &byte) in bytes.iter().enumerate() {
            match byte {
                ESC => {
                    out.extend_from_slice(&bytes[..at]);
                    self.utf8 = 0;
                    self.begin(State::Escape, byte);
                    return at + 1;
                }
                ENQ => {
                    out.extend_from_slice(&bytes[..at]);
                    self.utf8 = 0;
                    return at + 1;
                }
                _ => self.track_utf8(byte),
            }
        }
        out.extend_from_slice(bytes);
        bytes.len()
    }

    fn track_utf8(&mut self, byte: u8) {
        self.utf8 = match byte {
            0x80..=0xBF => self.utf8.saturating_sub(1),
            0xC2..=0xDF => 1,
            0xE0..=0xEF => 2,
            0xF0..=0xF4 => 3,
            _ => 0,
        };
    }

    /// Starts holding a sequence in `state` with its introducer `byte`.
    fn begin(&mut self, state: State, byte: u8) {
        self.held.clear();
        self.held.push(byte);
        self.fate = Fate::Hold;
        self.state = state;
        self.body = self.held.len();
    }

    /// Passes on what is held, unclassified (a sequence that ended without being dispatched, or one past its limit).
    fn release(&mut self, out: &mut Vec<u8>) {
        if self.fate == Fate::Hold {
            out.extend_from_slice(&self.held);
        }
        self.held.clear();
    }

    /// One byte in a sequence. Ground goes through [`OutputFilter::ground_run`].
    fn step(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {
                self.ground_run(&[byte], out);
            }
            State::Escape => self.escape(byte, out),
            State::EscapeIntermediate => match byte {
                _ if is_inert_c0(byte) || (0x20..=0x2F).contains(&byte) || byte == 0x7F => self.held.push(byte),
                0x30..=0x7E => self.finish(byte, out, &Verdict::Keep),
                _ => self.anywhere(byte, out),
            },
            State::Csi => self.csi(byte, out),
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => self.dcs_header(byte, out),
            State::DcsPassthrough | State::DcsIgnore => self.string(byte, out),
            State::SosPmApc => match byte {
                _ if is_inert_c0(byte) || (0x20..=0x7F).contains(&byte) => self.string(byte, out),
                _ if anywhere(byte) == Some(State::SosPmApc) => self.string(byte, out),
                _ => self.string_end(byte, out),
            },
            State::Osc => self.osc(byte, out),
        }
    }

    /// The sequence held ends with `byte`, which completes it, as `verdict` says.
    fn finish(&mut self, byte: u8, out: &mut Vec<u8>, verdict: &Verdict) {
        match (self.fate, verdict) {
            (Fate::Hold, Verdict::Keep) => {
                out.extend_from_slice(&self.held);
                out.push(byte);
            }
            (Fate::Hold, Verdict::Rewrite(bytes)) => out.extend_from_slice(bytes),
            (Fate::Pass, _) => out.push(byte),
            (Fate::Hold | Fate::Drop, Verdict::Drop) | (Fate::Drop, _) => {}
        }
        self.held.clear();
        self.state = State::Ground;
    }

    /// An "anywhere" transition out of a sequence that has not completed: what is held passes on unclassified (the
    /// terminal abandons the sequence too), and `byte` is taken as it would be in ground or starts its own sequence.
    fn anywhere(&mut self, byte: u8, out: &mut Vec<u8>) {
        self.release(out);
        match anywhere(byte) {
            Some(State::Escape) => self.begin(State::Escape, byte),
            Some(state @ (State::Csi | State::DcsEntry | State::Osc)) => self.begin(state, byte),
            Some(State::SosPmApc) => {
                out.push(byte);
                self.fate = Fate::Pass;
                self.state = State::SosPmApc;
            }
            _ => {
                out.push(byte);
                self.state = State::Ground;
            }
        }
    }

    fn escape(&mut self, byte: u8, out: &mut Vec<u8>) {
        let dropped_st = std::mem::take(&mut self.dropped_st);
        match byte {
            b'\\' if dropped_st => {
                self.held.clear();
                self.state = State::Ground;
            }
            b'[' => self.enter(State::Csi, byte),
            b']' => {
                self.enter(State::Osc, byte);
                self.body = self.held.len();
            }
            b'P' => self.enter(State::DcsEntry, byte),
            b'X' | b'^' | b'_' => {
                out.extend_from_slice(&self.held);
                out.push(byte);
                self.held.clear();
                self.fate = Fate::Pass;
                self.state = State::SosPmApc;
            }
            // DECID: Ghostty answers it as DA1
            b'Z' => self.finish(byte, out, &Verdict::Drop),
            0x20..=0x2F => self.enter(State::EscapeIntermediate, byte),
            b'c' => {
                self.finish(byte, out, &Verdict::Keep);
                out.extend_from_slice(self.after_reset);
            }
            0x30..=0x7E => self.finish(byte, out, &Verdict::Keep),
            _ if is_inert_c0(byte) || byte == 0x7F || byte >= 0xA0 => self.held.push(byte),
            _ => self.anywhere(byte, out),
        }
    }

    /// The held sequence goes on in `state` with `byte`.
    fn enter(&mut self, state: State, byte: u8) {
        self.held.push(byte);
        self.state = state;
    }

    fn csi(&mut self, byte: u8, out: &mut Vec<u8>) {
        match byte {
            0x40..=0x7E => {
                let verdict = if self.fate == Fate::Hold {
                    self.held.push(byte);
                    let verdict = classify_held_csi(&self.held);
                    self.held.pop();
                    verdict
                } else {
                    Verdict::Keep
                };
                self.finish(byte, out, &verdict);
            }
            _ if anywhere(byte).is_some() => self.anywhere(byte, out),
            _ => self.push_held(byte, CSI_LIMIT, out),
        }
    }

    /// Holds `byte`, or passes it on once the sequence outgrew `limit`.
    fn push_held(&mut self, byte: u8, limit: usize, out: &mut Vec<u8>) {
        match self.fate {
            Fate::Hold => {
                self.held.push(byte);
                if self.held.len() > limit {
                    out.extend_from_slice(&self.held);
                    self.held.clear();
                    self.fate = Fate::Pass;
                }
            }
            Fate::Pass => out.push(byte),
            Fate::Drop => {}
        }
    }

    fn dcs_header(&mut self, byte: u8, out: &mut Vec<u8>) {
        let next = dcs(self.state, byte);
        match next {
            State::DcsPassthrough => {
                self.held.push(byte);
                if is_dcs_query(&self.held) {
                    self.held.clear();
                    self.fate = Fate::Drop;
                } else {
                    out.extend_from_slice(&self.held);
                    self.held.clear();
                    self.fate = Fate::Pass;
                }
                self.state = next;
            }
            State::DcsIgnore => {
                out.extend_from_slice(&self.held);
                out.push(byte);
                self.held.clear();
                self.fate = Fate::Pass;
                self.state = next;
            }
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => {
                self.held.push(byte);
                self.state = next;
            }
            _ => self.anywhere(byte, out),
        }
    }

    /// A byte inside a DCS, SOS, PM or APC string: it stays in the string, or ends it.
    fn string(&mut self, byte: u8, out: &mut Vec<u8>) {
        let stays = match self.state {
            State::DcsPassthrough | State::DcsIgnore => !matches!(byte, CAN | SUB | ESC),
            _ => true,
        };
        if !stays {
            self.string_end(byte, out);
        } else if self.fate == Fate::Pass {
            out.push(byte);
        }
    }

    /// `byte` ends the string in progress: ESC starts the escape its ST is (taken with the string when it was
    /// dropped), anything else is an "anywhere" transition.
    fn string_end(&mut self, byte: u8, out: &mut Vec<u8>) {
        let dropped = self.fate == Fate::Drop;
        self.held.clear();
        self.fate = Fate::Pass;
        if byte == ESC {
            self.begin(State::Escape, byte);
            self.dropped_st = dropped;
        } else {
            self.anywhere(byte, out);
        }
    }

    fn osc(&mut self, byte: u8, out: &mut Vec<u8>) {
        match byte {
            BEL | ESC => {
                let verdict = if self.fate == Fate::Hold {
                    classify_osc(&self.held[self.body..])
                } else {
                    Verdict::Keep
                };
                let dropped = matches!((self.fate, &verdict), (Fate::Drop, _) | (_, Verdict::Drop));
                match (self.fate, &verdict) {
                    (Fate::Hold, Verdict::Keep) => out.extend_from_slice(&self.held),
                    (Fate::Hold, Verdict::Rewrite(body)) => {
                        out.extend_from_slice(&self.held[..self.body]);
                        out.extend_from_slice(body);
                    }
                    _ => {}
                }
                self.held.clear();
                self.fate = Fate::Pass;
                if byte == BEL {
                    if !dropped {
                        out.push(BEL);
                    }
                    self.state = State::Ground;
                } else {
                    self.begin(State::Escape, ESC);
                    self.dropped_st = dropped;
                }
            }
            // not dispatched: the terminal abandons the OSC
            CAN | SUB => {
                self.release(out);
                out.push(byte);
                self.state = State::Ground;
            }
            _ => self.push_held(byte, STRING_LIMIT, out),
        }
    }
}

/// C0 controls that execute (or are ignored) without leaving a state: everything below 0x20 but CAN, SUB and ESC.
fn is_inert_c0(byte: u8) -> bool {
    byte < 0x20 && !matches!(byte, CAN | SUB | ESC)
}

/// Ghostty's "anywhere" transitions (`parse_table.zig`), for states that do not override the byte.
fn anywhere(byte: u8) -> Option<State> {
    match byte {
        CAN | SUB | 0x80..=0x8F | 0x91..=0x97 | 0x99 | 0x9A | 0x9C => Some(State::Ground),
        ESC => Some(State::Escape),
        0x98 | 0x9E | 0x9F => Some(State::SosPmApc),
        0x9B => Some(State::Csi),
        0x90 => Some(State::DcsEntry),
        0x9D => Some(State::Osc),
        _ => None,
    }
}

/// The DCS header states (entry, param, intermediate) up to the passthrough or ignore state; `Ground` stands for an
/// "anywhere" transition.
fn dcs(state: State, byte: u8) -> State {
    match (state, byte) {
        (_, _) if is_inert_c0(byte) => state,
        (_, 0x7F) => state,
        (_, 0x40..=0x7E) => State::DcsPassthrough,
        (State::DcsEntry, 0x3A) | (State::DcsParam, 0x3A | 0x3C..=0x3F) | (State::DcsIntermediate, 0x30..=0x3F) => {
            State::DcsIgnore
        }
        (State::DcsEntry | State::DcsParam, 0x30..=0x3F) => State::DcsParam,
        (_, 0x20..=0x2F) => State::DcsIntermediate,
        _ => anywhere(byte).map_or(state, |_| State::Ground),
    }
}

/// The bytes of a held sequence after its introducer (`ESC` and one byte, or a C1 byte).
fn after_introducer(held: &[u8]) -> &[u8] {
    if held.first() == Some(&ESC) {
        held.get(2..).unwrap_or_default()
    } else {
        held.get(1..).unwrap_or_default()
    }
}

/// Whether a DCS header (introducer through its final byte) is DECRQSS (`$ q`) or XTGETTCAP (`+ q`).
fn is_dcs_query(header: &[u8]) -> bool {
    let body = after_introducer(header);
    let Some((&last, rest)) = body.split_last() else {
        return false;
    };
    let intermediates: Vec<u8> = rest
        .iter()
        .copied()
        .filter(|byte| (0x20..=0x2F).contains(byte))
        .collect();
    last == b'q' && matches!(intermediates.as_slice(), b"$" | b"+")
}

/// A complete CSI's parts.
struct Csi<'a> {
    /// `?`, `>`, `=` or `<`, when the parameters start with one.
    prefix: Option<u8>,
    params: &'a [u8],
    intermediates: &'a [u8],
    final_byte: u8,
}

fn parse_csi(sequence: &[u8]) -> Option<Csi<'_>> {
    let body = after_introducer(sequence);
    let (&final_byte, body) = body.split_last()?;
    let (prefix, body) = match body.split_first() {
        Some((&first @ (b'?' | b'>' | b'=' | b'<'), rest)) => (Some(first), rest),
        _ => (None, body),
    };
    let split = body
        .iter()
        .position(|byte| (0x20..=0x2F).contains(byte))
        .unwrap_or(body.len());
    let (params, intermediates) = body.split_at(split);
    Some(Csi {
        prefix,
        params,
        intermediates,
        final_byte,
    })
}

/// What becomes of a complete CSI as held, C0 controls and DEL inside it included (module docs): it is classified
/// without them, and the controls, which the terminal executes as it parses the CSI, stay ahead of what is written.
fn classify_held_csi(held: &[u8]) -> Verdict {
    let introducer = held.len() - after_introducer(held).len();
    let (start, body) = held.split_at(introducer);
    if !body.iter().any(|byte| *byte < 0x20 || *byte == 0x7F) {
        return classify_csi(held);
    }
    let controls: Vec<u8> = body.iter().copied().filter(|byte| *byte < 0x20).collect();
    let mut plain = start.to_vec();
    plain.extend(body.iter().filter(|byte| **byte >= 0x20 && **byte != 0x7F));
    match classify_csi(&plain) {
        Verdict::Keep => Verdict::Keep,
        Verdict::Drop if controls.is_empty() => Verdict::Drop,
        Verdict::Drop => Verdict::Rewrite(controls),
        Verdict::Rewrite(bytes) => Verdict::Rewrite([controls, bytes].concat()),
    }
}

/// What becomes of a complete CSI with no control inside it (module docs).
fn classify_csi(sequence: &[u8]) -> Verdict {
    let Some(csi) = parse_csi(sequence) else {
        return Verdict::Keep;
    };
    let first = csi.params.split(|byte| *byte == b';').next().unwrap_or_default();
    let shape = (csi.final_byte, csi.prefix, csi.intermediates);
    if matches!(shape, (b'h' | b'l', Some(b'?'), [])) {
        return without_withheld_modes(&csi);
    }
    let query = matches!(
        shape,
        (b'c', None | Some(b'>' | b'='), [])
            | (b'n', None | Some(b'?'), [])
            | (b'p', None | Some(b'?'), b"$")
            | (b'q', Some(b'>'), [])
            | (b'u', Some(b'?'), [])
    ) || (matches!(shape, (b't', None, [])) && matches!(first, b"14" | b"16" | b"18" | b"21"));
    if query { Verdict::Drop } else { Verdict::Keep }
}

/// DECSET or DECRST without modes 2031, 2048 and 9001 (module docs).
fn without_withheld_modes(csi: &Csi<'_>) -> Verdict {
    let modes: Vec<&[u8]> = csi.params.split(|byte| *byte == b';').collect();
    let kept: Vec<&[u8]> = modes
        .iter()
        .copied()
        .filter(|mode| !matches!(*mode, b"2031" | b"2048" | b"9001"))
        .collect();
    if kept.len() == modes.len() {
        Verdict::Keep
    } else if kept.is_empty() {
        Verdict::Drop
    } else {
        let mut bytes = b"\x1b[?".to_vec();
        bytes.extend_from_slice(&kept.join(&b';'));
        bytes.push(csi.final_byte);
        Verdict::Rewrite(bytes)
    }
}

/// What becomes of a complete OSC, by its body (module docs). A rewrite is the new body.
fn classify_osc(body: &[u8]) -> Verdict {
    let mut fields = body.split(|byte| *byte == b';');
    let number = fields.next().unwrap_or_default();
    let rest: Vec<&[u8]> = fields.collect();
    match number {
        b"4" => without_palette_queries(&rest),
        b"10" | b"11" | b"12" | b"13" | b"14" | b"15" | b"16" | b"17" | b"18" | b"19" => {
            drop_if(rest.contains(&b"?".as_slice()))
        }
        b"21" => drop_if(rest.iter().any(|field| field.ends_with(b"=?"))),
        b"52" => drop_if(rest.len() >= 2 && rest.last() == Some(&b"?".as_slice())),
        b"5522" => drop_if(
            rest.first()
                .is_some_and(|metadata| metadata.split(|byte| *byte == b':').any(|field| field == b"type=read")),
        ),
        b"133" => prompt_redraw(body),
        _ => Verdict::Keep,
    }
}

fn drop_if(query: bool) -> Verdict {
    if query { Verdict::Drop } else { Verdict::Keep }
}

/// OSC 4 without its `index;?` pairs: dropped when it held only those, kept when it held none.
fn without_palette_queries(pairs: &[&[u8]]) -> Verdict {
    let pairs: Vec<&[&[u8]]> = pairs.chunks(2).collect();
    let kept: Vec<&[&[u8]]> = pairs
        .iter()
        .copied()
        .filter(|pair| pair.get(1) != Some(&b"?".as_slice()))
        .collect();
    if kept.len() == pairs.len() {
        return Verdict::Keep;
    }
    if kept.is_empty() {
        return Verdict::Drop;
    }
    let mut body = b"4".to_vec();
    for field in kept.iter().flat_map(|pair| pair.iter()) {
        body.push(b';');
        body.extend_from_slice(field);
    }
    Verdict::Rewrite(body)
}

/// `133;A` with `redraw=0` (zmx's `rewritePromptRedraw`): added when missing, put in place of another value.
fn prompt_redraw(body: &[u8]) -> Verdict {
    let Some(options) = body.strip_prefix(b"133;A") else {
        return Verdict::Keep;
    };
    if !(options.is_empty() || options.starts_with(b";")) {
        return Verdict::Keep;
    }
    let mut fields: Vec<Vec<u8>> = options
        .split(|byte| *byte == b';')
        .skip(1)
        .map(<[u8]>::to_vec)
        .collect();
    match fields.iter_mut().find(|field| field.starts_with(b"redraw=")) {
        Some(field) if field.as_slice() == b"redraw=0" => return Verdict::Keep,
        Some(field) => *field = b"redraw=0".to_vec(),
        None => fields.push(b"redraw=0".to_vec()),
    }
    let mut rewritten = b"133;A".to_vec();
    for field in fields {
        rewritten.push(b';');
        rewritten.extend_from_slice(&field);
    }
    Verdict::Rewrite(rewritten)
}

#[cfg(test)]
mod tests;
