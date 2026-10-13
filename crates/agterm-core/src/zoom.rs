//! Terminal zoom and the surface vocabulary `surface.*` addresses (upstream's `TerminalZoom.swift`): which of a
//! session's surfaces a `surface:<session-id>:<kind>` id names, whether it is addressable, and the per-window zoom
//! target a window fills with one surface.
//!
//! A program overlay and the pane overlays are addressable while open; a HUD never is (it takes no input), and a page
//! never is (it has no terminal).

use uuid::Uuid;

use crate::commands::ToggleMode;
use crate::model::{AppStore, OverlayPane, Session, SurfaceSlot};
use crate::pane_env::uuid_string;

/// One of a session's terminal slots (upstream's `TerminalZoomSurface`), in its declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ZoomSurface {
    /// The main pane, `left`.
    Primary,
    /// The split pane, `right`.
    Split,
    Scratch,
    /// The session-wide program overlay.
    Overlay,
    /// The left pane's own overlay.
    OverlayLeft,
    /// The right pane's own overlay.
    OverlayRight,
}

impl ZoomSurface {
    /// Every kind, in upstream's order: the first active one is a session's active surface.
    pub const ALL: [Self; 6] = [
        Self::Primary,
        Self::Split,
        Self::Scratch,
        Self::Overlay,
        Self::OverlayLeft,
        Self::OverlayRight,
    ];

    /// Parses a kind with the pane aliases (`top`/`primary`, `bottom`/`split`).
    #[must_use]
    pub fn from_control_name(name: &str) -> Option<Self> {
        match name {
            "left" | "top" | "primary" => Some(Self::Primary),
            "right" | "bottom" | "split" => Some(Self::Split),
            "scratch" => Some(Self::Scratch),
            "overlay" => Some(Self::Overlay),
            "overlay-left" => Some(Self::OverlayLeft),
            "overlay-right" => Some(Self::OverlayRight),
            _ => None,
        }
    }

    /// The canonical spelling in a surface id.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Primary => "left",
            Self::Split => "right",
            Self::Scratch => "scratch",
            Self::Overlay => "overlay",
            Self::OverlayLeft => "overlay-left",
            Self::OverlayRight => "overlay-right",
        }
    }

    /// Whether the slot is addressable: a session can be while its terminal has yet to come up. A HUD is not: it
    /// takes no input, and `tree` reports the slot as no overlay while one is up.
    #[must_use]
    pub fn is_available(self, session: &Session) -> bool {
        match self {
            // a focused split survivor while the main slot is empty leaves no main pane
            Self::Primary => {
                !(!session.surface.is_present() && session.split_surface.is_present() && session.split.focused)
            }
            Self::Split => session.split.exists || session.split_surface.is_present(),
            Self::Scratch => session.scratch_active || session.scratch_surface.is_present(),
            Self::Overlay => session.overlay.program_active(),
            Self::OverlayLeft => program_covers(session, OverlayPane::Left),
            Self::OverlayRight => program_covers(session, OverlayPane::Right),
        }
    }

    /// The pane role of a terminal slot; `None` for the overlay kinds.
    #[must_use]
    pub fn pane(self) -> Option<crate::model::StatusPane> {
        use crate::model::StatusPane;
        match self {
            Self::Primary => Some(StatusPane::Left),
            Self::Split => Some(StatusPane::Right),
            Self::Scratch => Some(StatusPane::Scratch),
            Self::Overlay | Self::OverlayLeft | Self::OverlayRight => None,
        }
    }

    /// The pane a pane overlay kind covers; `None` for the other kinds.
    #[must_use]
    pub fn overlay_pane(self) -> Option<OverlayPane> {
        match self {
            Self::OverlayLeft => Some(OverlayPane::Left),
            Self::OverlayRight => Some(OverlayPane::Right),
            _ => None,
        }
    }

    /// The pane overlay kind covering `pane`.
    #[must_use]
    pub fn of_overlay_pane(pane: OverlayPane) -> Self {
        match pane {
            OverlayPane::Left => Self::OverlayLeft,
            OverlayPane::Right => Self::OverlayRight,
        }
    }

    /// The terminal slot this kind names in `session`.
    #[must_use]
    pub fn slot(self, session: &Session) -> &SurfaceSlot {
        match (self.pane(), self.overlay_pane()) {
            (Some(pane), _) => session.pane_slot(pane),
            (None, Some(pane)) => &session.pane_overlays.slot(pane).surface,
            (None, None) => &session.overlay.surface,
        }
    }

    /// Whether this is the session's active surface; at most one kind is (upstream's `isActive`).
    #[must_use]
    pub fn is_active(self, session: &Session) -> bool {
        let uncovered = !session.cover_overlay_active() && !session.scratch_active;
        let focused = session.focused_overlay_pane_role();
        let covered = |pane: OverlayPane| session.pane_overlay(pane).is_some();
        let program = |pane: OverlayPane| program_covers(session, pane);
        match self {
            Self::Primary => uncovered && focused == OverlayPane::Left && !covered(OverlayPane::Left),
            Self::Split => uncovered && focused == OverlayPane::Right && !covered(OverlayPane::Right),
            Self::Scratch => !session.cover_overlay_active() && session.scratch_active,
            Self::Overlay => session.overlay.program_active(),
            Self::OverlayLeft => uncovered && focused == OverlayPane::Left && program(OverlayPane::Left),
            Self::OverlayRight => uncovered && focused == OverlayPane::Right && program(OverlayPane::Right),
        }
    }

    /// Whether the detail pane shows this surface (upstream's `isVisible`): a pane while laid out, uncovered by the
    /// session-wide cover and the scratch and by its own overlay, which then takes the visibility.
    #[must_use]
    pub fn is_visible(self, session: &Session) -> bool {
        let pane_visible = |pane: OverlayPane| {
            !session.cover_overlay_active() && !session.scratch_active && session.renders_overlay_pane(pane)
        };
        let covered = |pane: OverlayPane| session.pane_overlay(pane).is_some();
        let program = |pane: OverlayPane| program_covers(session, pane);
        match self {
            Self::Primary => pane_visible(OverlayPane::Left) && !covered(OverlayPane::Left),
            Self::Split => pane_visible(OverlayPane::Right) && !covered(OverlayPane::Right),
            Self::Scratch => !session.cover_overlay_active() && session.scratch_active,
            Self::Overlay => session.overlay.program_active(),
            Self::OverlayLeft => pane_visible(OverlayPane::Left) && program(OverlayPane::Left),
            Self::OverlayRight => pane_visible(OverlayPane::Right) && program(OverlayPane::Right),
        }
    }
}

/// Whether `pane`'s own overlay runs a program (upstream: a pane overlay that is not a page).
fn program_covers(session: &Session, pane: OverlayPane) -> bool {
    session
        .pane_overlay(pane)
        .is_some_and(crate::model::PaneOverlay::is_program)
}

/// A surface id, `surface:<SESSION-ID>:<kind>` (upstream's `TerminalSurfaceID`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceId {
    pub session: Uuid,
    pub surface: ZoomSurface,
}

impl SurfaceId {
    /// Parses `surface:<uuid>:<kind>`; the kind takes its aliases and the uuid either case.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split(':');
        let (Some("surface"), Some(session), Some(kind), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        // Foundation's `UUID(uuidString:)` takes only the hyphenated form
        if session.len() != 36 {
            return None;
        }
        Some(Self {
            session: Uuid::parse_str(session).ok()?,
            surface: ZoomSurface::from_control_name(kind)?,
        })
    }

    /// The canonical id, the session id upper case.
    #[must_use]
    pub fn control_id(self) -> String {
        format!("surface:{}:{}", uuid_string(self.session), self.surface.wire_name())
    }

    /// Whether the session is in `store` and this slot of it is addressable (upstream's `isTargetValid`).
    #[must_use]
    pub fn is_valid_in(self, store: &AppStore) -> bool {
        store
            .session(self.session)
            .is_some_and(|session| self.surface.is_available(session))
    }

    /// The active surface of the store's selected session (upstream's `resolveTarget`); `None` without a selection or
    /// an active surface.
    #[must_use]
    pub fn active_in(store: &AppStore) -> Option<Self> {
        let session = store.selected_session_id().and_then(|id| store.session(id))?;
        let surface = ZoomSurface::ALL.into_iter().find(|kind| kind.is_active(session))?;
        Some(Self {
            session: session.id(),
            surface,
        })
    }
}

/// Applies a show/hide/toggle `mode` for `requested` to a window's zoom `current` (upstream's
/// `TerminalZoomController.set`): show zooms it, hide unzooms it only when it is the one zoomed (or unzooms whatever
/// is when `requested` is `None`), toggle flips it.
#[must_use]
pub(crate) fn apply_mode(
    current: Option<SurfaceId>,
    mode: ToggleMode,
    requested: Option<SurfaceId>,
) -> Option<SurfaceId> {
    match (mode, requested) {
        (ToggleMode::On, Some(requested)) => Some(requested),
        (ToggleMode::On, None) => current,
        (ToggleMode::Off, Some(requested)) => current.filter(|current| *current != requested),
        (ToggleMode::Off | ToggleMode::Toggle, None) => None,
        (ToggleMode::Toggle, Some(requested)) => (current != Some(requested)).then_some(requested),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(surface: ZoomSurface) -> SurfaceId {
        SurfaceId {
            session: Uuid::from_u128(7),
            surface,
        }
    }

    #[test]
    fn surface_ids_round_trip_with_aliases() {
        let left = id(ZoomSurface::Primary);
        assert_eq!(left.control_id(), "surface:00000000-0000-0000-0000-000000000007:left");
        assert_eq!(SurfaceId::parse(&left.control_id()), Some(left));
        assert_eq!(
            SurfaceId::parse("surface:00000000-0000-0000-0000-000000000007:bottom"),
            Some(id(ZoomSurface::Split))
        );
        for bad in [
            "zzz",
            "surface:00000000-0000-0000-0000-000000000007:bogus",
            "surface:00000000000000000000000000000007:left",
            "surface:00000000-0000-0000-0000-000000000007:left:x",
            "pane:00000000-0000-0000-0000-000000000007:left",
        ] {
            assert_eq!(SurfaceId::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn modes_set_clear_and_toggle_like_upstreams_controller() {
        let (left, right) = (id(ZoomSurface::Primary), id(ZoomSurface::Split));
        assert_eq!(apply_mode(None, ToggleMode::On, Some(left)), Some(left));
        assert_eq!(apply_mode(Some(left), ToggleMode::On, None), Some(left));
        assert_eq!(apply_mode(Some(left), ToggleMode::Off, Some(right)), Some(left));
        assert_eq!(apply_mode(Some(left), ToggleMode::Off, Some(left)), None);
        assert_eq!(apply_mode(Some(left), ToggleMode::Off, None), None);
        assert_eq!(apply_mode(Some(left), ToggleMode::Toggle, Some(left)), None);
        assert_eq!(apply_mode(Some(left), ToggleMode::Toggle, Some(right)), Some(right));
        assert_eq!(apply_mode(Some(left), ToggleMode::Toggle, None), None);
    }
}
