//! One terminal session: a sidebar row with a primary pane, an optional split pane, and the per-session state the
//! sidebar, title bar and control read-back project (upstream's `Session`, `AgentStatus.swift`).
//!
//! Fields mirror upstream's names (snake case) except where Rust groups them: the split fields live in
//! [`SplitLayout`], each pane's creation command in a [`PaneLaunch`], the session-wide overlay slot and its HUD in an
//! [`OverlaySlot`], the pane-scoped overlays in [`PaneOverlays`]. Remote-presentation state joins with the task that
//! implements it.

use std::time::SystemTime;

use uuid::Uuid;

use super::background::{BackgroundWatermark, PaneBackgrounds};
use super::naming::{self, last_path_component, sanitized, trimmed_or_none};
use super::overlay::OverlaySlot;
use super::pane::{PaneLaunch, PaneRestore, SplitLayout, StatusPane, SurfaceSlot};
use super::pane_overlay::PaneOverlays;
use super::search::SearchState;
use super::status::{StatusKeystroke, StatusReset, StatusShape};
use crate::hud::HudPaneFrames;

/// A session's agent status, set over the control channel (`session.status`); the sidebar row's glyph reacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum AgentStatus {
    /// No glyph.
    #[default]
    Idle,
    /// The agent is working.
    Active,
    /// A run finished.
    Completed,
    /// The agent waits on the user.
    Blocked,
}

impl AgentStatus {
    /// The wire spelling.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }

    /// Parses the wire spelling; `None` for anything else.
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        [Self::Idle, Self::Active, Self::Completed, Self::Blocked]
            .into_iter()
            .find(|status| status.wire_name() == name)
    }

    /// Whether a keystroke in the session's terminal clears this glyph back to idle (upstream's
    /// `clearedBy(keystroke:reset:)`). `blocked` and `completed` clear as `reset` says: on any key, on a submit only
    /// (a half-typed reply keeps the glyph), or never; under submit-only an interrupt also clears `blocked`, since
    /// cancelling a prompt fires no hook. `active` clears ONLY on an interrupt, whatever `reset` says, so typing while
    /// the agent works keeps the glyph.
    #[must_use]
    pub fn cleared_by(self, keystroke: StatusKeystroke, reset: StatusReset) -> bool {
        match self {
            Self::Blocked | Self::Completed => match reset {
                StatusReset::FirstKey => true,
                StatusReset::Enter => {
                    keystroke == StatusKeystroke::Submit
                        || (self == Self::Blocked && keystroke == StatusKeystroke::Interrupt)
                }
                StatusReset::Never => false,
            },
            Self::Active => keystroke == StatusKeystroke::Interrupt,
            Self::Idle => false,
        }
    }

    /// The sound a `session.status` write plays, `None` for none (upstream's `effectiveSound`): a per-call sound wins,
    /// else the configured blocked default plays for `blocked` only. Empty strings count as unset.
    #[must_use]
    pub fn effective_sound<'a>(self, per_call: Option<&'a str>, blocked_default: Option<&'a str>) -> Option<&'a str> {
        if let Some(sound) = per_call.filter(|sound| !sound.is_empty()) {
            return Some(sound);
        }
        blocked_default.filter(|sound| self == Self::Blocked && !sound.is_empty())
    }

    /// The states attention navigation (`session.go next-attention|prev-attention`) steps through: a `blocked`
    /// prompt or a `completed` run.
    #[must_use]
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::Blocked | Self::Completed)
    }

    /// Sort priority of the attention list, lower first. `idle` never makes the list but ranks last.
    #[must_use]
    pub fn attention_rank(self) -> u8 {
        match self {
            Self::Blocked => 0,
            Self::Active => 1,
            Self::Completed => 2,
            Self::Idle => 3,
        }
    }
}

/// A session's status glyph and how it behaves (upstream's `AgentIndicator`). Ephemeral.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentIndicator {
    pub status: AgentStatus,
    /// The visible glyph pulses for attention.
    pub blink: bool,
    /// The indicator resets to idle once the session is visited (selected), whatever its status.
    pub auto_reset: bool,
    /// A `#rrggbb` tint for this glyph only; `None` is the configured tint.
    pub color: Option<String>,
    /// A silhouette for this glyph only; `None` is the configured shape, else a circle.
    pub shape: Option<StatusShape>,
    /// Which pane set the status; `None` reads as the main pane.
    pub status_pane: Option<StatusPane>,
}

impl AgentIndicator {
    /// The pane tag as the store keeps it: a `right` tag on a session without a split folds to `left`, since a
    /// promoted survivor's shell keeps its baked `AGTERM_PANE=right` and the sole pane could never clear a `right`
    /// tag. `None` stays `None`, so the read-back omits the field (upstream's `normalizedPane`).
    #[must_use]
    pub fn normalized_pane(&self, has_split: bool) -> Option<StatusPane> {
        match self.status_pane {
            Some(StatusPane::Right) if !has_split => Some(StatusPane::Left),
            pane => pane,
        }
    }

    /// Whether a keystroke from `pane` clears this indicator: only when that pane owns the status (an untagged one
    /// is the main pane's) and [`AgentStatus::cleared_by`] allows it, so typing in one pane cannot wipe another
    /// pane's status (upstream's `clearedBy(pane:keystroke:reset:)`).
    #[must_use]
    pub fn cleared_by(&self, pane: StatusPane, keystroke: StatusKeystroke, reset: StatusReset) -> bool {
        self.status_pane.unwrap_or(StatusPane::Left) == pane && self.status.cleared_by(keystroke, reset)
    }
}

/// The most UTF-8 bytes a session context may hold (upstream's `Session.contextByteLimit`).
pub const CONTEXT_BYTE_LIMIT: usize = 256;

/// Validates a session context (upstream's `Session.validateContext`): trimmed, non-empty, at most
/// [`CONTEXT_BYTE_LIMIT`] bytes, and no control character or line or paragraph separator anywhere in the raw value,
/// so a trailing newline is refused rather than trimmed away.
///
/// # Errors
///
/// Upstream's message for the first rule broken.
pub fn validate_context(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("context must not be empty (use --clear to remove it)".to_owned());
    }
    if trimmed.len() > CONTEXT_BYTE_LIMIT {
        return Err(format!("context must be at most {CONTEXT_BYTE_LIMIT} UTF-8 bytes"));
    }
    if raw
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return Err("context must not contain control characters or line breaks".to_owned());
    }
    Ok(trimmed.to_owned())
}

/// Whether idle auto-follow already pulled the user to a session's current blocked episode (upstream's
/// `autoFollowConsumed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FollowMark {
    #[default]
    Fresh,
    Consumed,
}

/// One session. Its id is stable for its life and across a relaunch; its pane identities name the panes' processes
/// (and their live-session daemons) and follow a pane through a swap or a promotion.
#[derive(Debug)]
pub struct Session {
    id: Uuid,
    /// The primary pane's process identity. Follows a promoted split survivor.
    pub pane_identity: Uuid,
    /// The split pane's process identity while a split exists, hidden included.
    pub split_pane_identity: Option<Uuid>,
    /// A manual rename; wins the sidebar label while non-blank.
    pub custom_name: Option<String>,
    /// The primary pane's live cwd from the latest OSC 7 report. Persisted on structural saves only.
    pub current_cwd: Option<String>,
    initial_cwd: String,
    /// The primary pane's terminal title from the latest OSC 0/1/2 report. Ephemeral.
    pub osc_title: Option<String>,
    /// The split pane's live cwd, `None` without a split pane. Ephemeral.
    pub split_cwd: Option<String>,
    /// The split pane's terminal title, `None` without a split pane. Ephemeral.
    pub split_title: Option<String>,
    /// Where a restored split pane re-spawns (the persisted split cwd); `None` for a fresh split, which seeds from
    /// [`Session::effective_cwd`].
    pub initial_split_cwd: Option<String>,
    /// Notifications this session's panes fired while unfocused; the sidebar badge. Ephemeral.
    pub unseen_count: usize,
    /// The status glyph. Ephemeral.
    pub agent_indicator: AgentIndicator,
    /// When the status was last set, repeats included; wall clock, since `tree` ships it as epoch seconds a client
    /// compares against event timestamps. Never persisted.
    pub status_changed_at: Option<SystemTime>,
    auto_follow: FollowMark,
    remote_host: Option<String>,
    /// Membership of the flagged working set, the sidebar's cross-workspace flagged view. Persisted.
    pub flagged: bool,
    /// What the session is FOR, shown in the title bar. Persisted for a local session.
    pub context: Option<String>,
    /// The context mirrored from a remote origin; a local one wins over it. Ephemeral.
    pub mirrored_context: Option<String>,
    /// The split layout.
    pub split: SplitLayout,
    /// The primary pane's creation command.
    pub launch: PaneLaunch,
    /// The split pane's creation command, which a swap moves with the pane.
    pub split_launch: PaneLaunch,
    /// The terminal font size in points; `None` is the config default. Persisted.
    pub font_size: Option<f64>,
    /// Whether a restore rebuilt the session rather than a fresh create; gates re-running the creation command.
    /// Never persisted.
    pub was_restored: bool,
    /// Whether the scratch terminal is shown. Ephemeral.
    pub scratch_active: bool,
    /// What the next scratch terminal runs instead of a login shell (`session.scratch --command`); the frontend takes
    /// it when it creates the scratch, so it runs once. Ephemeral.
    pub scratch_command: Option<String>,
    /// The session's background; `None` is none. Persisted.
    pub background_watermark: Option<BackgroundWatermark>,
    /// Per-pane overrides of the background; the scratch's is never persisted.
    pub pane_backgrounds: PaneBackgrounds,
    /// The primary pane's restore state.
    pub restore: PaneRestore,
    /// The split pane's restore state, which a swap moves with the pane.
    pub split_restore: PaneRestore,
    /// The primary pane's terminal, created lazily by the frontend on first display.
    pub surface: SurfaceSlot,
    /// The split pane's terminal, created on the first split and kept while the split is hidden; freed when the split
    /// closes.
    pub split_surface: SurfaceSlot,
    /// The scratch terminal, created on its first show and kept while hidden; freed when its shell exits.
    pub scratch_surface: SurfaceSlot,
    /// The in-terminal search. Ephemeral.
    pub search: SearchState,
    /// The session-wide overlay slot and the HUD in it. Ephemeral.
    pub overlay: OverlaySlot,
    /// The pane-scoped overlays. Ephemeral.
    pub pane_overlays: PaneOverlays,
    /// The deck's last non-empty geometry of the session's detail area and panes, which a HUD is measured against
    /// (upstream's `hudPaneFrames`). Ephemeral.
    pub hud_frames: HudPaneFrames,
    /// The pending terminal ask ([`Session::ask`]), independent of the overlay slot. Ephemeral.
    pub(crate) ask: Option<super::ask::SessionAsk>,
    /// The overlay slots a viewer's presenter holds and the outcomes of remote jobs ([`Session::remote_overlays`]).
    /// Ephemeral.
    pub(crate) remote_overlays: super::remote_overlay::RemoteOverlays,
    /// What this machine keeps about the origin of a session attached from another machine
    /// ([`Session::remote_presentation`]); `None` for a local session. Ephemeral.
    pub(crate) remote_presentation: Option<super::remote_presentation::RemotePresentationState>,
}

/// The outcome of resolving a command's `--pane-id` and `--pane` pair (upstream's `Session.PaneAddress`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneAddress {
    /// The pane addressed; `None` when neither selector was given and the command's own default applies.
    Pane(Option<StatusPane>),
    /// A token no live pane of the session carries, and no role was named beside it.
    UnknownToken(String),
}

/// What a new session starts with (upstream's `Session.init` arguments).
#[derive(Debug, Clone, Default)]
pub struct SessionSeed {
    /// The session id; a fresh one when `None`.
    pub id: Option<Uuid>,
    pub initial_cwd: String,
    pub custom_name: Option<String>,
    /// The primary pane identity; a fresh one when `None`.
    pub pane_identity: Option<Uuid>,
    pub split_pane_identity: Option<Uuid>,
    /// The host a session attached from another machine lives on; `None` for a local one.
    pub remote_host: Option<String>,
}

impl Session {
    /// A session as seeded, with every other field at its default. Stores the values as given: callers sanitize.
    #[must_use]
    pub fn new(seed: SessionSeed) -> Self {
        Self {
            id: seed.id.unwrap_or_else(Uuid::new_v4),
            pane_identity: seed.pane_identity.unwrap_or_else(Uuid::new_v4),
            split_pane_identity: seed.split_pane_identity,
            custom_name: seed.custom_name,
            current_cwd: None,
            initial_cwd: seed.initial_cwd,
            osc_title: None,
            split_cwd: None,
            split_title: None,
            initial_split_cwd: None,
            unseen_count: 0,
            agent_indicator: AgentIndicator::default(),
            status_changed_at: None,
            auto_follow: FollowMark::Fresh,
            remote_host: seed.remote_host,
            flagged: false,
            context: None,
            mirrored_context: None,
            split: SplitLayout::default(),
            launch: PaneLaunch::default(),
            split_launch: PaneLaunch::default(),
            font_size: None,
            was_restored: false,
            scratch_active: false,
            scratch_command: None,
            background_watermark: None,
            pane_backgrounds: PaneBackgrounds::default(),
            restore: PaneRestore::default(),
            split_restore: PaneRestore::default(),
            surface: SurfaceSlot::default(),
            split_surface: SurfaceSlot::default(),
            scratch_surface: SurfaceSlot::default(),
            search: SearchState::default(),
            overlay: OverlaySlot::default(),
            pane_overlays: PaneOverlays::default(),
            hud_frames: HudPaneFrames::default(),
            ask: None,
            remote_overlays: super::remote_overlay::RemoteOverlays::default(),
            remote_presentation: None,
        }
    }

    /// Whether idle auto-follow already pulled the user to this blocked episode, so a later fire skips it; cleared when
    /// the status re-enters blocked, never by a blocked-over-blocked repeat. Ephemeral.
    #[must_use]
    pub fn auto_follow_consumed(&self) -> bool {
        self.auto_follow == FollowMark::Consumed
    }

    /// Marks or clears [`Session::auto_follow_consumed`].
    pub fn set_auto_follow_consumed(&mut self, consumed: bool) {
        self.auto_follow = if consumed {
            FollowMark::Consumed
        } else {
            FollowMark::Fresh
        };
    }

    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Where the session started; immutable.
    #[must_use]
    pub fn initial_cwd(&self) -> &str {
        &self.initial_cwd
    }

    /// The host this session is attached from, `None` for a local one. Set at construction and immutable, so no
    /// snapshot can ever carry a remote session's command.
    #[must_use]
    pub fn remote_host(&self) -> Option<&str> {
        self.remote_host.as_deref()
    }

    /// Whether the session may be written to disk at all: a remote one never is.
    #[must_use]
    pub fn is_persistable(&self) -> bool {
        self.remote_host.is_none()
    }

    /// The pane identities this instance's live-session daemons own: none for a remote session, whose panes run
    /// ssh.
    #[must_use]
    pub fn locally_managed_pane_identities(&self) -> Vec<Uuid> {
        if self.remote_host.is_some() {
            return Vec::new();
        }
        std::iter::once(self.pane_identity)
            .chain(self.split_pane_identity)
            .collect()
    }

    /// The local context, else the mirrored one.
    #[must_use]
    pub fn effective_context(&self) -> Option<&str> {
        self.context.as_deref().or(self.mirrored_context.as_deref())
    }

    /// Whether the split pane holds focus and exists. The existence term (upstream's `splitSurface != nil`) keeps a
    /// promoted survivor that momentarily re-raised the focus flag from masking the migrated primary fields.
    fn split_has_focus(&self) -> bool {
        self.split.focused && self.split_surface.is_present()
    }

    /// The sidebar label: a non-blank custom name, else the basename of [`Session::focused_cwd`] (`/` stays `/`, a
    /// trailing slash is ignored, an empty path reads `~`). The focused pane's non-blank terminal title comes before
    /// the basename only while [`naming::names_from_terminal_title`] is on.
    #[must_use]
    pub fn display_name(&self) -> String {
        if let Some(name) = self.custom_name.as_deref().and_then(trimmed_or_none) {
            return name;
        }
        if naming::names_from_terminal_title()
            && let Some(title) = self.focused_osc_title().and_then(trimmed_or_none)
        {
            return title;
        }
        last_path_component(self.focused_cwd())
    }

    /// The focused pane's cwd: the split pane's while it has focus and exists, else the primary's.
    #[must_use]
    pub fn focused_cwd(&self) -> &str {
        if self.split_has_focus()
            && let Some(cwd) = self.split_cwd.as_deref()
        {
            return cwd;
        }
        self.effective_cwd()
    }

    fn focused_osc_title(&self) -> Option<&str> {
        if self.split_has_focus() {
            self.split_title.as_deref()
        } else {
            self.osc_title.as_deref()
        }
    }

    /// The detail after the workspace name in the session palette, the switcher and the title bar: the focused
    /// pane's title unless it already is the display name, else the focused cwd.
    #[must_use]
    pub fn subtitle_detail(&self) -> String {
        if let Some(title) = self.focused_osc_title().and_then(trimmed_or_none)
            && title != self.display_name()
        {
            return title;
        }
        self.focused_cwd().to_owned()
    }

    /// [`Session::subtitle_detail`] led by the remote host for an attached session.
    #[must_use]
    pub fn switcher_detail(&self) -> String {
        match &self.remote_host {
            Some(host) => format!("{host} · {}", self.subtitle_detail()),
            None => self.subtitle_detail(),
        }
    }

    /// The primary pane's live cwd once reported, else where it started. Never focus-aware: it seeds new split,
    /// overlay, scratch and quick terminals.
    #[must_use]
    pub fn effective_cwd(&self) -> &str {
        self.current_cwd.as_deref().unwrap_or(&self.initial_cwd)
    }

    /// The working directory of a pane role: the split pane's for [`StatusPane::Right`], else the primary's.
    #[must_use]
    pub fn cwd_for(&self, pane: StatusPane) -> &str {
        match pane {
            StatusPane::Right => self
                .split_cwd
                .as_deref()
                .or(self.initial_split_cwd.as_deref())
                .unwrap_or_else(|| self.effective_cwd()),
            StatusPane::Left | StatusPane::Scratch => self.effective_cwd(),
        }
    }

    /// Records a reported cwd for the primary pane, sanitized as every stored path is.
    pub fn set_current_cwd(&mut self, cwd: &str) {
        self.current_cwd = Some(sanitized(cwd));
    }

    /// A pane's OSC 7 working-directory report, the split pane's when `split` (upstream's
    /// `GhosttySurfaceView.applyPwd`). Sanitized, since the value reaches `/bin/sh -c` lines unquoted. The caller
    /// does not save: the report fires on every prompt, and the live cwd is persisted with the next save.
    pub fn report_pwd(&mut self, split: bool, pwd: &str) {
        if split {
            self.split_cwd = Some(sanitized(pwd));
        } else {
            self.set_current_cwd(pwd);
        }
    }

    /// A pane's terminal title report, the split pane's when `split` (upstream's `GhosttySurfaceView.applyTitle`):
    /// sanitized, and dropped when it equals the pane's cwd, since libghostty answers OSC 7 with a synthetic title
    /// that is the bare path, which no shell titles with. Never saved, as [`Session::report_pwd`].
    pub fn report_title(&mut self, split: bool, title: &str) {
        let title = sanitized(title);
        let pane = if split { StatusPane::Right } else { StatusPane::Left };
        if title == self.cwd_for(pane) {
            return;
        }
        if split {
            self.split_title = Some(title);
        } else {
            self.osc_title = Some(title);
        }
    }

    /// The role of the live pane whose terminal carries `token` (upstream's `paneRole(forToken:)`); `None` for an
    /// empty token or one no pane carries.
    #[must_use]
    pub fn pane_role_for_token(&self, token: &str) -> Option<StatusPane> {
        if token.is_empty() {
            return None;
        }
        let carries = |slot: &SurfaceSlot| slot.get().is_some_and(|surface| surface.pane_token() == token);
        if carries(&self.surface) {
            Some(StatusPane::Left)
        } else if carries(&self.split_surface) {
            Some(StatusPane::Right)
        } else if carries(&self.scratch_surface) {
            Some(StatusPane::Scratch)
        } else {
            None
        }
    }

    /// Resolves a `--pane-id`/`--pane` pair (upstream's `paneAddress(token:pane:)`): a live token wins over the role
    /// named beside it, an unknown token falls back to that role, and an unknown token alone is refused.
    #[must_use]
    pub fn pane_address(&self, token: Option<&str>, pane: Option<StatusPane>) -> PaneAddress {
        match token.filter(|token| !token.is_empty()) {
            None => PaneAddress::Pane(pane),
            Some(token) => match self.pane_role_for_token(token).or(pane) {
                Some(resolved) => PaneAddress::Pane(Some(resolved)),
                None => PaneAddress::UnknownToken(token.to_owned()),
            },
        }
    }

    /// The terminal slot of a pane role.
    #[must_use]
    pub fn pane_slot(&self, pane: StatusPane) -> &SurfaceSlot {
        match pane {
            StatusPane::Left => &self.surface,
            StatusPane::Right => &self.split_surface,
            StatusPane::Scratch => &self.scratch_surface,
        }
    }

    /// The mutable terminal slot of a pane role.
    pub fn pane_slot_mut(&mut self, pane: StatusPane) -> &mut SurfaceSlot {
        match pane {
            StatusPane::Left => &mut self.surface,
            StatusPane::Right => &mut self.split_surface,
            StatusPane::Scratch => &mut self.scratch_surface,
        }
    }

    /// Whether the right pane is the focused one: it holds focus and is laid out or has a terminal (upstream's
    /// `focusedPane == .right`).
    #[must_use]
    pub fn focuses_split(&self) -> bool {
        self.split.focused && (self.split.shown || self.split_surface.is_present())
    }

    /// Whether the detail pane lays pane `right` (else the left one) out: both while the split is shown, else only
    /// the focused one, maximized (upstream's `rendersPane`).
    #[must_use]
    pub fn renders_pane(&self, right: bool) -> bool {
        self.split.shown || right == self.focuses_split()
    }

    /// The pane a command reads when none is named (upstream's `onScreenSurface`): the scratch while shown, else the
    /// focused split pane while it has a terminal (`activeSurface`), else the main pane.
    #[must_use]
    pub fn on_screen_pane(&self) -> StatusPane {
        if self.scratch_active {
            StatusPane::Scratch
        } else if self.split.focused && self.split_surface.is_present() {
            StatusPane::Right
        } else {
            StatusPane::Left
        }
    }

    /// The main pane, else a promoted split survivor's while the main slot is empty (upstream's
    /// `addressableSurface`).
    #[must_use]
    pub fn addressable_pane(&self) -> StatusPane {
        if !self.surface.is_present() && self.split_surface.is_present() {
            StatusPane::Right
        } else {
            StatusPane::Left
        }
    }

    /// Frees every terminal the session holds, the overlay slots' included (a HUD body file goes with it).
    pub fn teardown_surfaces(&mut self) {
        self.overlay.close();
        self.pane_overlays.teardown_all();
        self.surface.teardown();
        self.split_surface.teardown();
        self.scratch_surface.teardown();
        self.search.clear();
    }
}

#[cfg(test)]
mod tests {
    use agterm_host::fakes::FakeSurface;

    use super::*;

    fn session(cwd: &str) -> Session {
        Session::new(SessionSeed {
            initial_cwd: cwd.to_owned(),
            ..SessionSeed::default()
        })
    }

    #[test]
    fn display_name_prefers_name_then_basename_ignoring_the_title_by_default() {
        let mut s = session("/Users/me/project");
        s.osc_title = Some("vim".to_owned());
        assert_eq!(s.display_name(), "project");
        s.custom_name = Some("build".to_owned());
        assert_eq!(s.display_name(), "build");
        assert_eq!(s.subtitle_detail(), "vim", "the title still reaches line two");
    }

    #[test]
    fn display_name_prefers_name_then_title_then_basename() {
        naming::set_names_from_terminal_title(true);
        let mut s = session("/Users/me/project");
        assert_eq!(s.display_name(), "project");
        s.osc_title = Some("  vim  ".to_owned());
        assert_eq!(s.display_name(), "vim");
        s.custom_name = Some("build".to_owned());
        assert_eq!(s.display_name(), "build");
        s.custom_name = Some("   ".to_owned());
        assert_eq!(s.display_name(), "vim");
        s.osc_title = Some(" ".to_owned());
        assert_eq!(s.display_name(), "project");
    }

    #[test]
    fn pane_reports_land_on_their_pane_sanitized() {
        let mut s = session("/start");
        s.report_pwd(false, "/live\n/dir");
        assert_eq!(s.current_cwd.as_deref(), Some("/live/dir"));
        s.report_title(false, "vim\u{7}");
        assert_eq!(s.osc_title.as_deref(), Some("vim"));
        s.report_pwd(true, "/side");
        s.report_title(true, "htop");
        assert_eq!(
            (s.split_cwd.as_deref(), s.split_title.as_deref()),
            (Some("/side"), Some("htop"))
        );
        assert_eq!(
            (s.current_cwd.as_deref(), s.osc_title.as_deref()),
            (Some("/live/dir"), Some("vim"))
        );
    }

    #[test]
    fn a_title_that_echoes_the_panes_cwd_is_dropped() {
        let mut s = session("/start");
        s.report_title(false, "/start");
        assert_eq!(s.osc_title, None, "libghostty's synthetic OSC 7 title");
        s.report_pwd(false, "/next");
        s.report_title(false, "/next");
        assert_eq!(s.osc_title, None);
        s.report_title(false, "/start");
        assert_eq!(
            s.osc_title.as_deref(),
            Some("/start"),
            "only the pane's own cwd is an echo"
        );
        s.report_pwd(true, "/side");
        s.report_title(true, "/side");
        assert_eq!(s.split_title, None);
        s.report_title(true, "/next");
        assert_eq!(s.split_title.as_deref(), Some("/next"));
    }

    #[test]
    fn display_name_basename_pins() {
        assert_eq!(session("/").display_name(), "/");
        assert_eq!(session("/a/b/").display_name(), "b");
        assert_eq!(session("").display_name(), "~");
        let mut s = session("/start");
        s.current_cwd = Some("/live/dir".to_owned());
        assert_eq!(s.display_name(), "dir");
    }

    #[test]
    fn the_focused_split_drives_the_label_only_while_it_exists() {
        naming::set_names_from_terminal_title(true);
        let mut s = session("/main");
        s.split_cwd = Some("/side".to_owned());
        s.split_title = Some("split title".to_owned());
        s.split.focused = true;
        assert_eq!(
            s.display_name(),
            "main",
            "no split surface: the primary describes the row"
        );
        s.split_surface.replace(Box::new(FakeSurface::new("right")));
        assert_eq!(s.display_name(), "split title");
        s.split_title = None;
        assert_eq!(s.display_name(), "side");
        assert_eq!(s.focused_cwd(), "/side");
        assert_eq!(s.effective_cwd(), "/main", "effective cwd is never focus-aware");
    }

    #[test]
    fn subtitle_adds_the_title_only_when_it_is_not_the_label() {
        naming::set_names_from_terminal_title(true);
        let mut s = session("/work");
        assert_eq!(s.subtitle_detail(), "/work");
        s.osc_title = Some("me@host:~".to_owned());
        assert_eq!(s.display_name(), "me@host:~");
        assert_eq!(s.subtitle_detail(), "/work");
        s.custom_name = Some("named".to_owned());
        assert_eq!(s.subtitle_detail(), "me@host:~");
    }

    #[test]
    fn switcher_detail_leads_with_the_remote_host() {
        let remote = Session::new(SessionSeed {
            initial_cwd: "/srv".to_owned(),
            remote_host: Some("box".to_owned()),
            ..SessionSeed::default()
        });
        assert_eq!(remote.switcher_detail(), "box · /srv");
        assert!(!remote.is_persistable());
        assert!(remote.locally_managed_pane_identities().is_empty());
        assert_eq!(session("/x").switcher_detail(), "/x");
    }

    #[test]
    fn pane_cwds() {
        let mut s = session("/main");
        assert_eq!(s.cwd_for(StatusPane::Right), "/main");
        s.initial_split_cwd = Some("/restored".to_owned());
        assert_eq!(s.cwd_for(StatusPane::Right), "/restored");
        s.split_cwd = Some("/live".to_owned());
        assert_eq!(s.cwd_for(StatusPane::Right), "/live");
        assert_eq!(s.cwd_for(StatusPane::Scratch), "/main");
    }

    #[test]
    fn locally_managed_identities_list_the_split_when_present() {
        let mut s = session("/");
        assert_eq!(s.locally_managed_pane_identities(), vec![s.pane_identity]);
        let split = Uuid::new_v4();
        s.split_pane_identity = Some(split);
        assert_eq!(s.locally_managed_pane_identities(), vec![s.pane_identity, split]);
    }

    #[test]
    fn effective_context_prefers_the_local_one() {
        let mut s = session("/");
        s.mirrored_context = Some("origin".to_owned());
        assert_eq!(s.effective_context(), Some("origin"));
        s.context = Some("local".to_owned());
        assert_eq!(s.effective_context(), Some("local"));
    }

    #[test]
    fn context_validation_follows_upstreams_rules() {
        assert_eq!(validate_context("  PR #517  "), Ok("PR #517".to_owned()));
        for invalid in ["", "   ", "PR\n517", "PR\u{2028}517", "PR\t517", "PR517\n"] {
            assert!(validate_context(invalid).is_err(), "{invalid:?}");
        }
        assert_eq!(
            validate_context(&"a".repeat(CONTEXT_BYTE_LIMIT + 1)),
            Err("context must be at most 256 UTF-8 bytes".to_owned())
        );
        assert!(validate_context(&"a".repeat(CONTEXT_BYTE_LIMIT)).is_ok());
    }

    #[test]
    fn attention_states() {
        assert!(AgentStatus::Blocked.needs_attention());
        assert!(AgentStatus::Completed.needs_attention());
        assert!(!AgentStatus::Active.needs_attention());
        assert!(!AgentStatus::Idle.needs_attention());
        assert!(AgentStatus::Blocked.attention_rank() < AgentStatus::Active.attention_rank());
        assert!(AgentStatus::Active.attention_rank() < AgentStatus::Completed.attention_rank());
    }

    #[test]
    fn teardown_frees_every_pane() {
        let mut s = session("/");
        let main = FakeSurface::new("left");
        let split = FakeSurface::new("right");
        let scratch = FakeSurface::new("scratch");
        let probes = [main.probe(), split.probe(), scratch.probe()];
        s.surface.replace(Box::new(main));
        s.split_surface.replace(Box::new(split));
        s.scratch_surface.replace(Box::new(scratch));
        s.teardown_surfaces();
        assert!(probes.iter().all(|probe| probe.teardowns() == 1));
    }

    #[test]
    fn a_token_names_its_pane_the_scratch_included() {
        let mut s = session("/");
        s.surface.replace(Box::new(FakeSurface::new("L")));
        s.split_surface.replace(Box::new(FakeSurface::new("R")));
        s.scratch_surface.replace(Box::new(FakeSurface::new("S")));
        assert_eq!(s.pane_role_for_token("L"), Some(StatusPane::Left));
        assert_eq!(s.pane_role_for_token("R"), Some(StatusPane::Right));
        assert_eq!(s.pane_role_for_token("S"), Some(StatusPane::Scratch));
        assert_eq!(s.pane_role_for_token("X"), None);
        assert_eq!(s.pane_role_for_token(""), None);
    }

    #[test]
    fn the_focused_pane_is_the_only_one_a_hidden_split_lays_out() {
        let mut s = session("/");
        assert!(s.renders_pane(false) && !s.renders_pane(true));
        s.split = SplitLayout {
            shown: true,
            exists: true,
            focused: true,
            ..SplitLayout::default()
        };
        assert!(s.renders_pane(false) && s.renders_pane(true));
        s.split.shown = false;
        assert!(!s.focuses_split(), "no terminal: the left pane shows");
        s.split_surface.replace(Box::new(FakeSurface::new("R")));
        assert!(s.focuses_split());
        assert!(!s.renders_pane(false) && s.renders_pane(true));
    }
}
