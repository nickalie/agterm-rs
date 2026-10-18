//! Kitty keyboard protocol support.
//!
//! Ghostty: `src/terminal/kitty/key.zig` at `LIBGHOSTTY_VT_REV`.
//! rustty: `crates/rustty-vt/src/kitty/key.rs` at `RUSTTY_REV` (MIT).
//! Changes: Zig's `u3` stack index is a `u8` kept in `0..8` by masking, which is Zig's wrapping `+%`/`-%` on a `u3`;
//! the packed `Flags` converts to and from its `u5` with [`Flags::int`] and [`Flags::from_int`].

/// The number of entries in a [`FlagStack`].
const STACK_LEN: usize = 8;

/// Stack for the key flags. This implements the push/pop behavior of the CSI > u and CSI < u sequences. We implement
/// the stack as fixed size to avoid heap allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlagStack {
    flags: [Flags; STACK_LEN],
    /// The current entry (Zig `u3`).
    idx: u8,
}

impl Default for FlagStack {
    fn default() -> Self {
        Self {
            flags: [Flags::DISABLED; STACK_LEN],
            idx: 0,
        }
    }
}

/// Zig's `u3` wrapping arithmetic: the index modulo the stack length.
const fn wrap(idx: u8) -> u8 {
    idx & 0b111
}

impl FlagStack {
    /// Return the current stack value
    pub(crate) fn current(self) -> Flags {
        self.flags[usize::from(self.idx)]
    }

    /// Perform the "set" operation as described in the spec for the CSI = u sequence.
    pub(crate) fn set(&mut self, mode: SetMode, v: Flags) {
        let current = &mut self.flags[usize::from(self.idx)];
        *current = match mode {
            SetMode::Set => v,
            SetMode::Or => Flags::from_int(current.int() | v.int()),
            SetMode::Not => Flags::from_int(current.int() & !v.int()),
        };
    }

    /// Push a new set of flags onto the stack. If the stack is full then the oldest entry is evicted.
    pub(crate) fn push(&mut self, flags: Flags) {
        // Overflow and wrap around if we're full, which evicts the oldest entry.
        self.idx = wrap(self.idx.wrapping_add(1));
        self.flags[usize::from(self.idx)] = flags;
    }

    /// Pop `n` entries from the stack. This will just wrap around if `n` is greater than the amount in the stack.
    pub(crate) fn pop(&mut self, n: usize) {
        // If n is more than our length then we just reset the stack. This also avoids a DoS vector where a malicious
        // client could send a huge number of pop commands to waste cpu.
        if n >= self.flags.len() {
            *self = Self::default();
            return;
        }

        for _ in 0..n {
            self.flags[usize::from(self.idx)] = Flags::DISABLED;
            self.idx = wrap(self.idx.wrapping_sub(1));
        }
    }
}

/// The possible flags for the Kitty keyboard protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the protocol's five flag bits, Ghostty's packed struct(u5)"
)]
pub(crate) struct Flags {
    pub(crate) disambiguate: bool,
    pub(crate) report_events: bool,
    pub(crate) report_alternates: bool,
    pub(crate) report_all: bool,
    pub(crate) report_associated: bool,
}

impl Flags {
    /// Kitty keyboard protocol disabled (all flags off).
    pub(crate) const DISABLED: Self = Self::from_int(0);

    /// Sets all modes on.
    pub(crate) const TRUE: Self = Self::from_int(0b1_1111);

    /// The flags as the protocol's integer (Zig's packed `u5`, `disambiguate` the lowest bit).
    pub(crate) const fn int(self) -> u8 {
        (self.disambiguate as u8)
            | ((self.report_events as u8) << 1)
            | ((self.report_alternates as u8) << 2)
            | ((self.report_all as u8) << 3)
            | ((self.report_associated as u8) << 4)
    }

    /// The flags of the protocol's integer `v`; bits above the fifth are ignored.
    pub(crate) const fn from_int(v: u8) -> Self {
        Self {
            disambiguate: v & 0b0_0001 != 0,
            report_events: v & 0b0_0010 != 0,
            report_alternates: v & 0b0_0100 != 0,
            report_all: v & 0b0_1000 != 0,
            report_associated: v & 0b1_0000 != 0,
        }
    }
}

/// The possible modes for setting the key flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SetMode {
    Set,
    Or,
    Not,
}

#[cfg(test)]
mod tests;
