//! A session's panes: the split layout (`isSplit`, `hasSplit`, `splitFocused`, `splitAxis`, `splitRatio` on
//! upstream's `Session`), each pane's creation command, the live terminal slots and the pane roles.
//!
//! The split commands that mutate these (`session.split`, swap, close, promotion) come with the split group; this
//! module holds the state they act on and the bounds they share.

use std::fmt;

use agterm_host::Surface;

pub use crate::pane_env::StatusPane;

/// The physical arrangement of a session's two panes, by its wire name: the user-facing divider direction, so a
/// vertical divider makes left/right panes and a horizontal one top/bottom panes (upstream's `SplitAxis`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SplitAxis {
    /// Left and right panes (`vertical`). A fresh or legacy session starts here.
    #[default]
    LeftRight,
    /// Top and bottom panes (`horizontal`).
    TopBottom,
}

impl SplitAxis {
    /// The wire and snapshot spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::LeftRight => "vertical",
            Self::TopBottom => "horizontal",
        }
    }

    /// Parses the wire spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        match name {
            "vertical" => Some(Self::LeftRight),
            "horizontal" => Some(Self::TopBottom),
            _ => None,
        }
    }
}

/// The persisted split-divider primary-pane fraction bounds: a live capture skips values outside them and a restore
/// clamps into them, so the stored ratio is always within (upstream's `AppStore.splitRatioMin`/`splitRatioMax`).
pub const SPLIT_RATIO_MIN: f64 = 0.05;
/// See [`SPLIT_RATIO_MIN`].
pub const SPLIT_RATIO_MAX: f64 = 0.95;
/// The even split a never-moved divider renders at, and the base of a relative `session.resize` while the ratio is
/// unset.
pub const SPLIT_RATIO_DEFAULT: f64 = 0.5;

/// Clamps a primary-pane fraction into [`SPLIT_RATIO_MIN`]`..=`[`SPLIT_RATIO_MAX`] (upstream's `clampSplitRatio`).
#[must_use]
pub fn clamp_split_ratio(ratio: f64) -> f64 {
    ratio.clamp(SPLIT_RATIO_MIN, SPLIT_RATIO_MAX)
}

/// A session's one-level split, upstream's split fields on `Session` grouped.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SplitLayout {
    /// Whether the split is SHOWN (upstream's `isSplit`); the detail pane shows or hides the second pane.
    pub shown: bool,
    /// Whether the session HAS a split pane at all, shown or hidden (upstream's `hasSplit`). Stays set across a hide,
    /// so the sidebar and title-bar indicators persist while hidden; only closing the split clears it.
    pub exists: bool,
    /// While split, whether the second pane holds focus rather than the primary (upstream's `splitFocused`).
    /// Meaningless without a split.
    pub focused: bool,
    /// The panes' arrangement; changing it while both panes exist transposes them without replacing either terminal.
    pub axis: SplitAxis,
    /// The divider's primary-pane fraction, within the split ratio bounds; `None` is even.
    pub ratio: Option<f64>,
}

/// What a pane runs instead of the login shell, set by `session.new --command` (upstream's `initialCommand` and
/// `commandWait`, and their `split` twins).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaneLaunch {
    /// The command line the pane's process runs; the pane closes when it exits. Persisted, so a restored command
    /// session re-runs it in `rerun` launch mode.
    pub command: Option<String>,
    /// Whether the pane HOLDS after the command exits (libghostty's "press any key to close" prompt) instead of
    /// closing. Meaningful only with `command`.
    pub wait: bool,
}

impl PaneLaunch {
    /// Whether the pane holds after its command exits: a hold set without a command means nothing, which is what
    /// the `commandWait` read-back reports.
    #[must_use]
    pub fn holds(&self) -> bool {
        self.command.is_some() && self.wait
    }
}

/// A live terminal slot of a session: empty until the frontend creates the pane's surface, freed exactly once.
#[derive(Default)]
pub struct SurfaceSlot(Option<Box<dyn Surface>>);

impl SurfaceSlot {
    /// Whether the slot holds a surface.
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.0.is_some()
    }

    /// The surface, when present.
    #[must_use]
    pub fn get(&self) -> Option<&dyn Surface> {
        self.0.as_deref()
    }

    /// The surface for mutation, when present.
    pub fn get_mut(&mut self) -> Option<&mut (dyn Surface + 'static)> {
        self.0.as_deref_mut()
    }

    /// Puts `surface` in the slot, returning the one it held so the caller decides its fate.
    pub fn replace(&mut self, surface: Box<dyn Surface>) -> Option<Box<dyn Surface>> {
        self.0.replace(surface)
    }

    /// Empties the slot without freeing the surface, for a move into another slot.
    pub fn take(&mut self) -> Option<Box<dyn Surface>> {
        self.0.take()
    }

    /// Frees the surface the slot holds, if any, and empties the slot.
    pub fn teardown(&mut self) {
        if let Some(surface) = self.0.take() {
            surface.teardown();
        }
    }
}

impl fmt::Debug for SurfaceSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Some(surface) => f
                .debug_struct("SurfaceSlot")
                .field("pane_token", &surface.pane_token())
                .field("realized", &surface.is_realized())
                .finish(),
            None => f.write_str("SurfaceSlot(empty)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use agterm_host::fakes::FakeSurface;

    use super::*;

    #[test]
    fn split_axis_round_trips_its_wire_names() {
        for axis in [SplitAxis::LeftRight, SplitAxis::TopBottom] {
            assert_eq!(SplitAxis::from_wire(axis.wire_name()), Some(axis));
        }
        assert_eq!(SplitAxis::from_wire("leftRight"), None);
        assert_eq!(SplitAxis::default(), SplitAxis::LeftRight);
    }

    #[test]
    fn split_ratio_clamps_to_the_persisted_bounds() {
        assert!((clamp_split_ratio(0.0) - SPLIT_RATIO_MIN).abs() < f64::EPSILON);
        assert!((clamp_split_ratio(1.0) - SPLIT_RATIO_MAX).abs() < f64::EPSILON);
        assert!((clamp_split_ratio(0.3) - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn a_hold_needs_a_command() {
        let mut launch = PaneLaunch {
            command: None,
            wait: true,
        };
        assert!(!launch.holds());
        launch.command = Some("htop".to_owned());
        assert!(launch.holds());
    }

    #[test]
    fn a_slot_tears_its_surface_down_once() {
        let surface = FakeSurface::new("pane");
        let probe = surface.probe();
        let mut slot = SurfaceSlot::default();
        assert!(!slot.is_present());
        assert!(slot.replace(Box::new(surface)).is_none());
        assert!(slot.is_present());
        assert_eq!(slot.get().map(Surface::pane_token), Some("pane"));
        slot.teardown();
        slot.teardown();
        assert!(!slot.is_present());
        assert_eq!(probe.teardowns(), 1);
    }
}
