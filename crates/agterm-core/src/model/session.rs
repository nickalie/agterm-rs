//! One terminal session: a sidebar row with a primary pane, an optional split pane, and the per-session state the
//! sidebar, title bar and control read-back project (upstream's `Session`, `AgentStatus.swift`).
//!
//! Fields mirror upstream's names (snake case) except where Rust groups them: the split fields live in
//! [`SplitLayout`], each pane's creation command in a [`PaneLaunch`]. Overlay, HUD, ask, scratch, restore and
//! remote-presentation state joins with the tasks that implement them.

use std::time::SystemTime;

use uuid::Uuid;

use super::background::{BackgroundWatermark, PaneBackgrounds};
use super::naming::{last_path_component, sanitized, trimmed_or_none};
use super::pane::{PaneLaunch, PaneRestore, SplitLayout, StatusPane, SurfaceSlot};

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
            background_watermark: None,
            pane_backgrounds: PaneBackgrounds::default(),
            restore: PaneRestore::default(),
            split_restore: PaneRestore::default(),
            surface: SurfaceSlot::default(),
            split_surface: SurfaceSlot::default(),
        }
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

    /// The sidebar label: a non-blank custom name, else the focused pane's non-blank terminal title, else the
    /// basename of [`Session::focused_cwd`] (`/` stays `/`, a trailing slash is ignored, an empty path reads `~`).
    #[must_use]
    pub fn display_name(&self) -> String {
        if let Some(name) = self.custom_name.as_deref().and_then(trimmed_or_none) {
            return name;
        }
        if let Some(title) = self.focused_osc_title().and_then(trimmed_or_none) {
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

    /// Frees every terminal the session holds.
    pub fn teardown_surfaces(&mut self) {
        self.surface.teardown();
        self.split_surface.teardown();
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
    fn display_name_prefers_name_then_title_then_basename() {
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
    fn teardown_frees_both_panes() {
        let mut s = session("/");
        let main = FakeSurface::new("left");
        let split = FakeSurface::new("right");
        let (main_probe, split_probe) = (main.probe(), split.probe());
        s.surface.replace(Box::new(main));
        s.split_surface.replace(Box::new(split));
        s.teardown_surfaces();
        assert_eq!((main_probe.teardowns(), split_probe.teardowns()), (1, 1));
    }
}
