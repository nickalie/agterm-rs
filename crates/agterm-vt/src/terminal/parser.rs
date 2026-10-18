//! VT-series parser for escape and control sequences.
//!
//! This is implemented directly as the state machine described on vt100.net: <https://vt100.net/emu/dec_ansi_parser>
//!
//! Ghostty: `src/terminal/Parser.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/parser.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's CSI, ESC and DCS payloads are slices into the parser's buffers, "only valid until the next call to
//! next"; here they own their bytes in fixed-capacity inline arrays ([`InlineVec`], the parser's own capacities), held
//! by value in the [`Action`] as Zig's are, so building an action never allocates. rustty boxes them to keep an
//! [`Action`] small; that is not taken: the stream's fast paths keep the ground, CSI parameter and OSC/APC payload
//! bytes off [`Parser::next`], so the wider return is paid only on the bytes that still reach it (escape sequence
//! starts and finals off the fast paths, DCS payloads). The OSC payload borrows the OSC parser's storage, as Zig's
//! does, so [`Action`] carries that borrow's lifetime. The exit action is computed after the transition and entry
//! actions: an `osc_string` exit borrows the OSC parser for as long as the returned actions live, and nothing else in
//! a step that leaves `osc_string` touches it. Zig's `format` logging helpers are `Display` impls, printing what Zig's
//! do. `StaticBitSet` is [`SepList`].

use crate::terminal::osc;
use crate::terminal::parse_table::TABLE;

/// The log target of the parser (Ghostty's `.parser` scope).
const LOG_SCOPE: &str = "parser";

/// Log a warning for a CSI dispatch with colon/mixed separators on a non-'m' command.
///
/// This is `#[inline(never)]` on purpose so that this unlikely (cold) behavior doesn't bloat the hot dispatch path,
/// which has been measured to actually affect both binary size and performance due to icache busts.
#[cold]
#[inline(never)]
fn warn_csi_sep_mismatch(csi: &Csi) {
    tracing::warn!(
        target: LOG_SCOPE,
        "CSI colon or mixed separators only allowed for 'm' command, got: {csi}"
    );
}

/// States for the state machine
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Ground = 0,
    Escape = 1,
    EscapeIntermediate = 2,
    CsiEntry = 3,
    CsiIntermediate = 4,
    CsiParam = 5,
    CsiIgnore = 6,
    DcsEntry = 7,
    DcsParam = 8,
    DcsIntermediate = 9,
    DcsPassthrough = 10,
    DcsIgnore = 11,
    OscString = 12,
    SosPmApcString = 13,
}

impl State {
    /// Every state, in declaration order (`@typeInfo(State).@"enum".fields`); each one's index is its value.
    pub(crate) const ALL: [Self; 14] = [
        Self::Ground,
        Self::Escape,
        Self::EscapeIntermediate,
        Self::CsiEntry,
        Self::CsiIntermediate,
        Self::CsiParam,
        Self::CsiIgnore,
        Self::DcsEntry,
        Self::DcsParam,
        Self::DcsIntermediate,
        Self::DcsPassthrough,
        Self::DcsIgnore,
        Self::OscString,
        Self::SosPmApcString,
    ];

    /// The number of states.
    pub(crate) const COUNT: usize = Self::ALL.len();
}

/// Transition action is an action that can be taken during a state transition. This is more of an internal action,
/// not one used by end users, typically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransitionAction {
    None,
    Ignore,
    Print,
    Execute,
    Collect,
    Param,
    EscDispatch,
    CsiDispatch,
    Put,
    OscPut,
    ApcPut,
}

/// Action is the action that a caller of the parser is expected to take as a result of some input character.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Action<'p> {
    /// Draw character to the screen. This is a unicode codepoint.
    Print(u32),

    /// Execute the C0 or C1 function.
    Execute(u8),

    /// Execute the CSI command.
    CsiDispatch(Csi),

    /// Execute the ESC command.
    EscDispatch(Esc),

    /// Execute the OSC command. It borrows the OSC parser: it is only valid until the next call to the parser.
    OscDispatch(osc::Command<'p>),

    /// DCS-related events.
    DcsHook(Dcs),
    DcsPut(u8),
    DcsUnhook,

    /// APC data
    ApcStart,
    ApcPut(u8),
    ApcEnd,
}

/// A CSI command: `CSI intermediates params final`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Csi {
    pub(crate) intermediates: InlineVec<u8, MAX_INTERMEDIATE>,
    pub(crate) params: InlineVec<u16, MAX_PARAMS>,
    /// The separator after each parameter (`;` or `:`).
    pub(crate) params_sep: SepList,
    pub(crate) final_: u8,
}

/// Zig's `{s}` of a byte slice: the bytes as they are, which a log line shows lossily.
fn write_bytes(f: &mut core::fmt::Formatter<'_>, bytes: &[u8]) -> core::fmt::Result {
    f.write_str(&String::from_utf8_lossy(bytes))
}

/// Zig's `{any}` of a slice: `{ 1, 2 }`, and `{  }` when it is empty.
fn write_list(f: &mut core::fmt::Formatter<'_>, values: &[u16]) -> core::fmt::Result {
    f.write_str("{ ")?;
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{v}")?;
    }
    f.write_str(" }")
}

impl core::fmt::Display for Csi {
    /// `ESC [ {intermediates} {params} {final}`, as the logs print it.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ESC [ ")?;
        write_bytes(f, &self.intermediates)?;
        f.write_str(" ")?;
        write_list(f, &self.params)?;
        write!(f, " {}", char::from(self.final_))
    }
}

/// An ESC command: `ESC intermediates final`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Esc {
    pub(crate) intermediates: InlineVec<u8, MAX_INTERMEDIATE>,
    pub(crate) final_: u8,
}

impl core::fmt::Display for Esc {
    /// `ESC {intermediates} {final}`, as the logs print it.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("ESC ")?;
        write_bytes(f, &self.intermediates)?;
        write!(f, " {}", char::from(self.final_))
    }
}

/// The header of a DCS string: `DCS intermediates params final`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dcs {
    pub(crate) intermediates: InlineVec<u8, MAX_INTERMEDIATE>,
    pub(crate) params: InlineVec<u16, MAX_PARAMS>,
    pub(crate) final_: u8,
}

/// The list of separators used for CSI params: a set bit is a colon, a clear one a semicolon (Zig's `Sep`). The index
/// of this bit set specifies the separator AFTER that param. For example: 0;4:3 would have index 1 set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SepList {
    bits: u32,
}

impl SepList {
    /// No separator is a colon (`initEmpty`).
    pub(crate) const fn init_empty() -> Self {
        Self { bits: 0 }
    }

    /// Mark the separator after parameter `idx` a colon.
    pub(crate) fn set(&mut self, idx: usize) {
        debug_assert!(idx < MAX_PARAMS);
        self.bits |= 1_u32 << idx;
    }

    /// Whether the separator after parameter `idx` is a colon.
    pub(crate) fn is_set(self, idx: usize) -> bool {
        idx < MAX_PARAMS && (self.bits >> idx) & 1 == 1
    }

    /// The number of colon separators.
    pub(crate) fn count(self) -> u32 {
        self.bits.count_ones()
    }
}

/// A fixed-capacity inline payload: the parser's buffer and the length of its used prefix. It stands in for Zig's
/// `self.intermediates[0..self.intermediates_idx]` prefix slices over the parser's fixed arrays, owned by value, with
/// no heap allocation and no drop glue. The tail beyond `len` is unspecified; every observation goes through the
/// prefix.
#[derive(Clone, Copy)]
pub(crate) struct InlineVec<T: Copy, const N: usize> {
    buf: [T; N],
    len: u8,
}

impl<T: Copy, const N: usize> InlineVec<T, N> {
    /// The first `len` elements of the parser's buffer `buf`.
    #[inline]
    pub(crate) fn from_buf(buf: [T; N], len: u8) -> Self {
        debug_assert!(usize::from(len) <= N);
        Self { buf, len }
    }

    /// The used prefix: the only observable content.
    #[inline]
    pub(crate) fn as_slice(&self) -> &[T] {
        &self.buf[..usize::from(self.len)]
    }
}

impl<T: Copy + Default, const N: usize> Default for InlineVec<T, N> {
    /// An empty payload.
    fn default() -> Self {
        Self {
            buf: [T::default(); N],
            len: 0,
        }
    }
}

impl<T: Copy, const N: usize> core::ops::Deref for InlineVec<T, N> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy + PartialEq, const N: usize> PartialEq for InlineVec<T, N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Copy + Eq, const N: usize> Eq for InlineVec<T, N> {}

impl<T: Copy + PartialEq, const N: usize> PartialEq<&[T]> for InlineVec<T, N> {
    fn eq(&self, other: &&[T]) -> bool {
        self.as_slice() == *other
    }
}

impl<T: Copy + PartialEq, const N: usize> PartialEq<InlineVec<T, N>> for &[T] {
    fn eq(&self, other: &InlineVec<T, N>) -> bool {
        *self == other.as_slice()
    }
}

impl<T: Copy + PartialEq, const N: usize, const M: usize> PartialEq<[T; M]> for InlineVec<T, N> {
    fn eq(&self, other: &[T; M]) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: Copy + core::fmt::Debug, const N: usize> core::fmt::Debug for InlineVec<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.as_slice().fmt(f)
    }
}

impl<'s, T: Copy, const N: usize> IntoIterator for &'s InlineVec<T, N> {
    type Item = &'s T;
    type IntoIter = core::slice::Iter<'s, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// Maximum number of intermediate characters during parsing. This is 4 because we also use the intermediates array
/// for UTF8 decoding which can be at most 4 bytes.
pub(crate) const MAX_INTERMEDIATE: usize = 4;

/// Maximum number of CSI parameters. This is arbitrary. Practically, the only CSI command that uses more than 3
/// parameters is the SGR command which can be infinitely long. 24 is a reasonable limit based on empirical data. This
/// used to be 16 but Kakoune has a SGR command that uses 17 parameters.
///
/// We could in the future make this the static limit and then allocate after but that's a lot more work and
/// practically its so rare to exceed this number. I implore TUI authors to not use more than this number of CSI
/// params, but I suspect we'll introduce a slow path with heap allocation one day.
pub(crate) const MAX_PARAMS: usize = 24;

/// VT-series parser for escape and control sequences.
pub(crate) struct Parser {
    /// Current state of the state machine
    pub(crate) state: State,

    /// Intermediate tracking.
    pub(crate) intermediates: [u8; MAX_INTERMEDIATE],
    pub(crate) intermediates_idx: u8,

    /// Param tracking, building
    pub(crate) params: [u16; MAX_PARAMS],
    pub(crate) params_sep: SepList,
    pub(crate) params_idx: u8,
    pub(crate) param_acc: u16,
    pub(crate) param_acc_idx: u8,

    /// Parser for OSC sequences
    pub(crate) osc: osc::Parser,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// A parser in the ground state; its OSC parser has no allocator until the stream gives it one.
    pub(crate) const fn new() -> Self {
        Self {
            state: State::Ground,
            intermediates: [0; MAX_INTERMEDIATE],
            intermediates_idx: 0,
            params: [0; MAX_PARAMS],
            params_sep: SepList::init_empty(),
            params_idx: 0,
            param_acc: 0,
            param_acc_idx: 0,
            osc: osc::Parser::new(false),
        }
    }

    /// Next consumes the next character c and returns the actions to execute. Up to 3 actions may need to be executed
    /// -- in order -- representing the state exit, transition, and entry actions.
    #[inline]
    pub(crate) fn next(&mut self, c: u8) -> [Option<Action<'_>>; 3] {
        let state = self.state;
        let effect = TABLE[usize::from(c)][state as usize];
        let next_state = effect.state;
        let action = effect.action;

        // When going from one state to another, the actions take place in this order:
        //
        // 1. exit action from old state
        // 2. transition action
        // 3. entry action to new state
        //
        // Staying in the same state there is neither an exit nor an entry action, so the result is just the
        // transition's; the print and execute arms are spelled out so the hot ground path builds no payload.
        if state == next_state {
            return match action {
                TransitionAction::Print => [None, Some(Action::Print(u32::from(c))), None],
                TransitionAction::Execute => [None, Some(Action::Execute(c)), None],
                _ => [None, self.do_action(action, c), None],
            };
        }

        let transition = self.do_action(action, c);
        let entry = self.entry(next_state, c);

        // The state is set once all actions are built (Zig's `defer`).
        self.state = next_state;

        // Exit depends on current state. It is computed last: the `osc_string` exit's command borrows the OSC parser
        // for as long as the returned actions live. The transition and entry of an `osc_string` exit never touch the
        // OSC parser (the table leaves `osc_string` only on BEL, ESC, CAN and SUB, into `ground` or `escape`).
        let exit = match state {
            State::OscString => {
                debug_assert!(matches!(action, TransitionAction::None | TransitionAction::Execute));
                self.osc.end(Some(c)).map(Action::OscDispatch)
            }
            State::DcsPassthrough => Some(Action::DcsUnhook),
            State::SosPmApcString => Some(Action::ApcEnd),
            _ => None,
        };

        [exit, transition, entry]
    }

    /// The entry action of `next_state`, entered on byte `c`.
    #[inline]
    fn entry(&mut self, next_state: State, c: u8) -> Option<Action<'static>> {
        match next_state {
            State::Escape | State::DcsEntry | State::CsiEntry => {
                self.clear();
                None
            }
            State::OscString => {
                self.osc.reset();
                None
            }
            // Ignore too many parameters
            State::DcsPassthrough if usize::from(self.params_idx) >= MAX_PARAMS => None,
            State::DcsPassthrough => {
                // Finalize parameters
                if self.param_acc_idx > 0 {
                    self.params[usize::from(self.params_idx)] = self.param_acc;
                    self.params_idx += 1;
                }
                Some(Action::DcsHook(Dcs {
                    intermediates: InlineVec::from_buf(self.intermediates, self.intermediates_idx),
                    params: InlineVec::from_buf(self.params, self.params_idx),
                    final_: c,
                }))
            }
            State::SosPmApcString => Some(Action::ApcStart),
            _ => None,
        }
    }

    /// Collect an intermediate byte.
    #[inline]
    pub(crate) fn collect(&mut self, c: u8) {
        if usize::from(self.intermediates_idx) >= MAX_INTERMEDIATE {
            tracing::warn!(target: LOG_SCOPE, "invalid intermediates count");
            return;
        }

        self.intermediates[usize::from(self.intermediates_idx)] = c;
        self.intermediates_idx += 1;
    }

    /// The transition action `action` on byte `c`.
    #[inline]
    fn do_action(&mut self, action: TransitionAction, c: u8) -> Option<Action<'static>> {
        match action {
            TransitionAction::None | TransitionAction::Ignore => None,
            TransitionAction::Print => Some(Action::Print(u32::from(c))),
            TransitionAction::Execute => Some(Action::Execute(c)),
            TransitionAction::Collect => {
                self.collect(c);
                None
            }
            TransitionAction::Param => {
                self.param(c);
                None
            }
            TransitionAction::OscPut => {
                self.osc.next(c);
                None
            }
            TransitionAction::CsiDispatch => self.csi_dispatch(c),
            TransitionAction::EscDispatch => Some(Action::EscDispatch(Esc {
                intermediates: InlineVec::from_buf(self.intermediates, self.intermediates_idx),
                final_: c,
            })),
            TransitionAction::Put => Some(Action::DcsPut(c)),
            TransitionAction::ApcPut => Some(Action::ApcPut(c)),
        }
    }

    /// A parameter byte: a digit, or a `;` or `:` separator.
    #[inline]
    fn param(&mut self, c: u8) {
        // Semicolon separates parameters. If we encounter a semicolon we need to store and move on to the next
        // parameter.
        if c == b';' || c == b':' {
            // Ignore too many parameters
            if usize::from(self.params_idx) >= MAX_PARAMS {
                return;
            }

            // Set param final value
            self.params[usize::from(self.params_idx)] = self.param_acc;
            if c == b':' {
                self.params_sep.set(usize::from(self.params_idx));
            }
            self.params_idx += 1;

            // Reset current param value to 0
            self.param_acc = 0;
            self.param_acc_idx = 0;
            return;
        }

        // A numeric value. Add it to our accumulator.
        self.param_acc = self.param_acc.saturating_mul(10).saturating_add(u16::from(c - b'0'));

        // Increment our accumulator index. If we overflow then we're out of bounds and we exit immediately.
        self.param_acc_idx = self.param_acc_idx.wrapping_add(1);
    }

    /// The CSI dispatch on final byte `c`.
    #[inline]
    fn csi_dispatch(&mut self, c: u8) -> Option<Action<'static>> {
        // Ignore too many parameters
        if usize::from(self.params_idx) >= MAX_PARAMS {
            return None;
        }

        // Finalize parameters if we have one
        if self.param_acc_idx > 0 {
            self.params[usize::from(self.params_idx)] = self.param_acc;
            self.params_idx += 1;
        }

        let csi = Csi {
            intermediates: InlineVec::from_buf(self.intermediates, self.intermediates_idx),
            params: InlineVec::from_buf(self.params, self.params_idx),
            params_sep: self.params_sep,
            final_: c,
        };

        // We only allow colon or mixed separators for the 'm' command.
        if c != b'm' && self.params_sep.count() > 0 {
            warn_csi_sep_mismatch(&csi);
            return None;
        }

        Some(Action::CsiDispatch(csi))
    }

    /// Forget the sequence collected so far.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.intermediates_idx = 0;
        self.params_idx = 0;
        self.params_sep = SepList::init_empty();
        self.param_acc = 0;
        self.param_acc_idx = 0;
    }
}

#[cfg(test)]
mod tests;
