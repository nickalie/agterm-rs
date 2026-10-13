//! Terminal zoom and the surface vocabulary `surface.*` addresses (upstream's `TerminalZoom.swift`): which of a
//! session's surfaces a `surface:<session-id>:<kind>` id names, whether it is addressable, and the per-window zoom
//! target a window fills with one surface.
//!
//! The overlay kinds parse but are never available yet: program and pane overlays join with their task.

use uuid::Uuid;

use crate::commands::ToggleMode;
use crate::model::{AppStore, Session};
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

    /// Whether the slot is addressable: a session can be while its terminal has yet to come up.
    #[must_use]
    pub fn is_available(self, session: &Session) -> bool {
        match self {
            // a focused split survivor while the main slot is empty leaves no main pane
            Self::Primary => {
                !(!session.surface.is_present() && session.split_surface.is_present() && session.split.focused)
            }
            Self::Split => session.split.exists || session.split_surface.is_present(),
            Self::Scratch => session.scratch_active,
            Self::Overlay | Self::OverlayLeft | Self::OverlayRight => false,
        }
    }

    /// Whether this is the session's active surface; at most one kind is.
    #[must_use]
    pub fn is_active(self, session: &Session) -> bool {
        let uncovered = !session.scratch_active;
        match self {
            Self::Primary => uncovered && !focuses_split(session),
            Self::Split => uncovered && focuses_split(session),
            Self::Scratch => session.scratch_active,
            Self::Overlay | Self::OverlayLeft | Self::OverlayRight => false,
        }
    }
}

/// Upstream's `focusedPane == .right`: the split holds focus and is laid out or has a terminal.
fn focuses_split(session: &Session) -> bool {
    session.split.focused && (session.split.shown || session.split_surface.is_present())
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
