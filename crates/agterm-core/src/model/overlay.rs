//! The session-wide overlay slot and the HUD that occupies it (upstream's `overlay*`/`hud*` fields on `Session`,
//! `Session+Hud.swift`, and the slot mutations in `AppStore+Panes.swift`: `openOverlay`, `resizeOverlay`,
//! `setHudSize`, `remeasureHud`, `closeOverlay`, `openHud`, `updateHud`, `closeHud`).
//!
//! The slot holds one occupant at a time. A HUD is a passive message panel the app paints with its bundled helper
//! (the slot's command), re-reading a body file the HUD commands write; a caller's program or page replaces it, and
//! it never replaces them. Everything here is ephemeral: never persisted.

use std::path::PathBuf;

use uuid::Uuid;

use super::pane::SurfaceSlot;
use super::session::Session;
use super::store::AppStore;
use crate::html::HtmlOverlay;
use crate::hud::{self, HudPanelSize, HudSpec, PaneMetrics};
use crate::overlay_capture::{mint_code_file, take_exit_code};

/// Which pane a pane-scoped placement covers (upstream's `OverlayPane`): two cases, so a scratch placement is not
/// representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OverlayPane {
    Left,
    Right,
}

impl OverlayPane {
    /// The pane spellings upstream accepts (`left|top|primary`, `right|bottom|split`); `None` for anything else, the
    /// scratch included.
    #[must_use]
    pub fn from_control_name(raw: &str) -> Option<Self> {
        match raw {
            "left" | "top" | "primary" => Some(Self::Left),
            "right" | "bottom" | "split" => Some(Self::Right),
            _ => None,
        }
    }

    /// The canonical spelling the read-back carries.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// The live HUD's state beside the slot (upstream's `hudSpec`, `hudPaneIdentity`, `hudFile`, `hudHeightPercent`,
/// `hudHeightPoints`, `hudFontSize`, `hudResizedWidthPercent`).
#[derive(Debug, Clone, PartialEq)]
pub struct HudState {
    /// The spec the panel paints, the live background and font size held through updates.
    pub spec: HudSpec,
    /// The pane whose bounds scope the panel, by its stable identity; `None` for the session's whole detail area.
    pub pane_identity: Option<Uuid>,
    /// The body file the helper re-reads every tick (`AGTERM_HUD_FILE`).
    pub file: PathBuf,
    /// The panel's measured share of the pane's height.
    pub height_percent: i64,
    /// The height the panel is laid out at, in points; `None` over an unmeasured pane.
    pub height_points: Option<f64>,
    /// The point size the panel's surface was created at.
    pub font_size: Option<f64>,
    /// The width an `overlay.resize` forced, until the next open or update.
    pub resized_width_percent: Option<i64>,
}

/// The session-wide overlay slot (upstream's `overlayActive` and its companions).
#[derive(Debug, Default)]
pub struct OverlaySlot {
    /// The slot is occupied, by a program or page that covers, or by a passive HUD.
    pub active: bool,
    /// What the occupant's terminal runs; for a HUD, the bundled helper.
    pub command: Option<String>,
    /// The occupant's working directory; `None` inherits the session's.
    pub cwd: Option<String>,
    /// Hold the surface after the command exits.
    pub wait: bool,
    /// A floating occupant's share of the pane (a HUD's WIDTH); `None` is a full-pane cover.
    pub size_percent: Option<i64>,
    /// The occupant's own `#rrggbb` background, read at creation.
    pub background_color: Option<String>,
    /// The last program's exit status, kept past its close.
    pub exit_code: Option<i32>,
    /// Where the open program's wrapper writes its exit status, read when its terminal is torn down; `None` for a
    /// HUD, which runs the app's painter rather than a program a caller could ask about.
    pub code_file: Option<PathBuf>,
    /// Bumped on every open, so the frontend builds a fresh surface for a replacement.
    pub generation: u64,
    /// The occupant's terminal, created by the frontend once the slot is open.
    pub surface: SurfaceSlot,
    /// The HUD occupying the slot.
    pub hud: Option<HudState>,
    /// The page the slot shows instead of running a program (upstream's `htmlOverlay`); `command` is then unset.
    pub html: Option<HtmlOverlay>,
}

impl OverlaySlot {
    /// Whether the slot holds a passive HUD (upstream's `hudActive`).
    #[must_use]
    pub fn hud_active(&self) -> bool {
        self.active && self.hud.is_some()
    }

    /// Whether the slot runs a caller's program (upstream's `programOverlayActive`), the terminal-surface question:
    /// neither a HUD nor a page counts.
    #[must_use]
    pub fn program_active(&self) -> bool {
        self.active && self.hud.is_none() && self.html.is_none()
    }

    /// Whether a page covers the session (upstream's `htmlOverlayActive`): it owns input like a program but has no
    /// terminal, zoom target or exit status.
    #[must_use]
    pub fn html_active(&self) -> bool {
        self.active && self.html.is_some()
    }

    /// The page the slot shows while it is open.
    #[must_use]
    pub fn page(&self) -> Option<&HtmlOverlay> {
        self.html.as_ref().filter(|_| self.active)
    }

    /// The live HUD's spec, at the width an `overlay.resize` forced when one did (upstream's `effectiveHudSpec`).
    #[must_use]
    pub fn effective_hud_spec(&self) -> Option<HudSpec> {
        let hud = self.hud.as_ref().filter(|_| self.active)?;
        Some(
            hud.spec
                .with_size_percent(hud.resized_width_percent.or(hud.spec.size_percent)),
        )
    }

    /// The live HUD's size as one value (upstream's `hudPanelSize`).
    #[must_use]
    pub fn hud_panel_size(&self) -> Option<HudPanelSize> {
        let hud = self.hud.as_ref().filter(|_| self.active)?;
        Some(HudPanelSize {
            width_percent: self.size_percent?,
            height_percent: hud.height_percent,
            height_points: hud.height_points,
        })
    }

    /// Drops the HUD: deletes its body file and clears the state describing it (upstream's `discardHudBody`), the
    /// single owner of that deletion, so a HUD closed before its surface existed leaves no file behind.
    pub fn discard_hud(&mut self) {
        if let Some(hud) = self.hud.take()
            && let Err(error) = std::fs::remove_file(&hud.file)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(file = %hud.file.display(), %error, "could not delete the hud body file");
        }
    }

    /// Empties the slot: its surface torn down, a page released, its fields cleared, a HUD discarded (upstream's
    /// `closeOverlay`). False with nothing open. The exit status survives, and a program's is recorded here from its
    /// wrapper's file (upstream reads it at the surface's teardown). A released page's outcome is settled by the core,
    /// which notices the page gone from every slot.
    pub fn close(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.active = false;
        self.surface.teardown();
        if let Some(code) = self.code_file.take().and_then(|file| take_exit_code(&file)) {
            self.exit_code = Some(code);
        }
        self.html = None;
        self.command = None;
        self.cwd = None;
        self.wait = false;
        self.size_percent = None;
        self.background_color = None;
        self.discard_hud();
        true
    }
}

impl Session {
    /// The role of the pane carrying the stable identity `identity`, following a promotion (upstream's
    /// `paneRole(forIdentity:)`).
    #[must_use]
    pub fn pane_role_for_identity(&self, identity: Uuid) -> Option<OverlayPane> {
        if self.pane_identity == identity {
            Some(OverlayPane::Left)
        } else if self.split_pane_identity == Some(identity) {
            Some(OverlayPane::Right)
        } else {
            None
        }
    }

    /// The stable identity of the pane in role `role`; `None` for the right one without a split pane.
    #[must_use]
    pub fn overlay_pane_identity(&self, role: OverlayPane) -> Option<Uuid> {
        match role {
            OverlayPane::Left => Some(self.pane_identity),
            OverlayPane::Right => self.split_pane_identity,
        }
    }

    /// The terminal slot of the pane in role `role`.
    #[must_use]
    pub fn overlay_pane_slot(&self, role: OverlayPane) -> &super::pane::SurfaceSlot {
        match role {
            OverlayPane::Left => &self.surface,
            OverlayPane::Right => &self.split_surface,
        }
    }

    /// The pane the live HUD is scoped to now (upstream's `hudTargetPane`); `None` for session-wide or no HUD.
    #[must_use]
    pub fn hud_target_pane(&self) -> Option<OverlayPane> {
        self.overlay
            .hud
            .as_ref()
            .and_then(|hud| hud.pane_identity)
            .and_then(|identity| self.pane_role_for_identity(identity))
    }

    /// Whether the detail pane lays `pane` out (upstream's `rendersPane(_:)`).
    #[must_use]
    pub fn renders_overlay_pane(&self, pane: OverlayPane) -> bool {
        self.renders_pane(pane == OverlayPane::Right)
    }
}

/// What a caller's program in an overlay runs with (upstream's `openOverlay`/`openPaneOverlay` arguments).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayProgram {
    pub command: String,
    /// The working directory; `None` inherits the session's.
    pub cwd: Option<String>,
    /// Hold the terminal after the program exits.
    pub wait: bool,
    /// A floating overlay's share of the pane, `None` for a full-pane cover; pane overlays never take one.
    pub size_percent: Option<i64>,
    /// The overlay's own `#rrggbb` background.
    pub background_color: Option<String>,
}

/// What opening a HUD takes besides its spec (upstream's `openHud` arguments).
#[derive(Debug, Clone, PartialEq)]
pub struct HudOpening {
    /// The helper's command line.
    pub command: String,
    pub file: PathBuf,
    pub size: HudPanelSize,
    pub pane_identity: Option<Uuid>,
    /// The effective point size the caller measured with.
    pub font_size: Option<f64>,
}

impl AppStore {
    /// Opens the session-wide slot for a caller's program (upstream's `openOverlay`): a live HUD yields the slot, a
    /// caller's program refuses. The size is clamped into 1...100; the wrapper's exit-status file is minted here. False
    /// for an unknown session or an occupied slot.
    pub fn open_overlay(&mut self, id: Uuid, program: OverlayProgram) -> bool {
        self.occupy_overlay(id, program, Some(mint_code_file()))
    }

    /// Takes the session-wide slot for `program`, replacing a HUD, with `code_file` for its exit status.
    fn occupy_overlay(&mut self, id: Uuid, program: OverlayProgram, code_file: Option<PathBuf>) -> bool {
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        let slot = &mut session.overlay;
        if slot.hud_active() {
            slot.close();
        }
        if slot.active {
            return false;
        }
        slot.generation += 1;
        slot.html = None;
        slot.command = Some(program.command);
        slot.cwd = program.cwd;
        slot.wait = program.wait;
        slot.exit_code = None;
        slot.code_file = code_file;
        slot.size_percent = program.size_percent.map(|percent| percent.clamp(1, 100));
        slot.background_color = program.background_color;
        slot.active = true;
        // a local open starts a new result, a remote job's failure included
        session.remote_overlays.clear_failure(None);
        true
    }

    /// Resizes the open occupant in place (upstream's `resizeOverlay`): a program takes `size_percent` clamped into
    /// 1...100 (`None` is full-pane); a HUD takes its own width bound and records the width as forced. False with
    /// nothing open. The caller owes a HUD a remeasure.
    pub fn resize_overlay(&mut self, id: Uuid, size_percent: Option<i64>) -> bool {
        let Some(slot) = self.session_mut(id).map(|session| &mut session.overlay) else {
            return false;
        };
        if !slot.active {
            return false;
        }
        match slot.hud.as_mut() {
            Some(hud) => {
                slot.size_percent = size_percent.map(|percent| hud::clamp_size_percent_for(percent, &hud.spec));
                hud.resized_width_percent = slot.size_percent;
            }
            None => slot.size_percent = size_percent.map(|percent| percent.clamp(1, 100)),
        }
        true
    }

    /// Sets a live HUD's size as one step, the width bounded for its own spec (upstream's `setHudSize`);
    /// `forced_width_percent` is the `overlay.resize` width left in force. False with no HUD.
    pub fn set_hud_size(&mut self, id: Uuid, size: HudPanelSize, forced_width_percent: Option<i64>) -> bool {
        let Some(slot) = self.session_mut(id).map(|session| &mut session.overlay) else {
            return false;
        };
        if !slot.active {
            return false;
        }
        let Some(hud) = slot.hud.as_mut() else {
            return false;
        };
        slot.size_percent = Some(hud::clamp_size_percent_for(size.width_percent, &hud.spec));
        hud.height_percent = size.height_percent;
        hud.height_points = size.height_points;
        hud.resized_width_percent = forced_width_percent;
        true
    }

    /// Measures the live HUD again against `pane` (upstream's `remeasureHud`): wrap, rows, width and height together,
    /// at the forced width when one is in force; the spec, scope, generation and auto-hide stay.
    pub fn remeasure_hud(&mut self, id: Uuid, pane: &PaneMetrics) -> bool {
        let Some(slot) = self.session(id).map(|session| &session.overlay) else {
            return false;
        };
        let (Some(spec), Some(forced)) = (slot.effective_hud_spec(), slot.hud.as_ref()) else {
            return false;
        };
        let forced = forced.resized_width_percent;
        self.set_hud_size(id, hud::panel_size(&spec, pane), forced)
    }

    /// Closes the session-wide slot (upstream's `closeOverlay`): its surface torn down and a HUD discarded. False with
    /// nothing open.
    pub fn close_overlay(&mut self, id: Uuid) -> bool {
        self.session_mut(id).is_some_and(|session| session.overlay.close())
    }

    /// Opens a HUD in the slot (upstream's `openHud`): a live HUD is replaced (a fresh surface picks the new file up),
    /// a caller's program refuses. Always floating, its width bounded by [`hud::clamp_size_percent_for`]. False for an
    /// unknown session or an occupied slot.
    pub fn open_hud(&mut self, id: Uuid, spec: HudSpec, opening: HudOpening) -> bool {
        let width = hud::clamp_size_percent_for(opening.size.width_percent, &spec);
        let program = OverlayProgram {
            command: opening.command.clone(),
            size_percent: Some(width),
            background_color: spec.background_color.clone(),
            ..OverlayProgram::default()
        };
        if !self.occupy_overlay(id, program, None) {
            return false;
        }
        let Some(session) = self.session_mut(id) else {
            return false;
        };
        session.overlay.hud = Some(HudState {
            spec,
            pane_identity: opening.pane_identity,
            file: opening.file,
            height_percent: opening.size.height_percent,
            height_points: opening.size.height_points,
            font_size: opening.font_size,
            resized_width_percent: None,
        });
        true
    }

    /// Rewrites a live HUD's spec and size in place (upstream's `updateHud`): the surface stays, the background and
    /// font size the surface was created with are held. False with no HUD.
    pub fn update_hud(&mut self, id: Uuid, spec: &HudSpec, size: HudPanelSize, pane_identity: Option<Uuid>) -> bool {
        let Some(slot) = self.session_mut(id).map(|session| &mut session.overlay) else {
            return false;
        };
        if !slot.active {
            return false;
        }
        let Some(hud) = slot.hud.as_mut() else {
            return false;
        };
        hud.spec = spec.holding_creation_fields(&hud.spec);
        hud.pane_identity = pane_identity;
        self.set_hud_size(id, size, None)
    }

    /// Closes a HUD through the ordinary slot teardown (upstream's `closeHud`); refused for a caller's program, so it
    /// can never kill one. False with no HUD.
    pub fn close_hud(&mut self, id: Uuid) -> bool {
        if !self.session(id).is_some_and(|session| session.overlay.hud_active()) {
            return false;
        }
        self.close_overlay(id)
    }

    /// Closes session `id`'s HUD when it has an auto-hide (upstream's `closeTimedHud`), before a soft close takes the
    /// session out of the tree: the expiry cannot find a pending session, and an undo would bring back a panel whose
    /// time is up and that nothing closes any more.
    pub(crate) fn close_timed_hud(&mut self, id: Uuid) {
        let timed = self.session(id).is_some_and(|session| {
            session.overlay.hud_active()
                && session
                    .overlay
                    .hud
                    .as_ref()
                    .is_some_and(|hud| hud.spec.effective_hide_after() > 0.0)
        });
        if timed {
            self.close_hud(id);
        }
    }
}
