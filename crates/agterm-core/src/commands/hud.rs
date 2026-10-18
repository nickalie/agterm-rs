//! `session.hud.open`, `.update` and `.close`: validation and its messages as upstream's `ControlDispatcher+Hud.swift`,
//! the effects as `agterm/Control/ControlServer+Hud.swift` (measure the panel against the session's geometry, take the
//! overlay slot, write the body file the painter re-reads, arm the auto-hide).
//!
//! What the core cannot know itself the frontend supplies: the bundled painter's path ([`Core::set_hud_helper`]), the
//! deck's geometry ([`Core::report_hud_frames`]), a cell estimate for a font no terminal draws yet
//! ([`Core::set_cell_estimator`]), and the auto-hide delays it arms ([`Core::take_hud_timers`]).

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use agterm_proto::command::SessionCmd;
use agterm_proto::{Args, Request};
use uuid::Uuid;

use super::{Outcome, args_of, with_id};
use crate::Core;
use crate::dashboard::GHOSTTY_DEFAULT_FONT_SIZE;
use crate::error::ControlError;
use crate::hud::markdown::renders_visible_text;
use crate::hud::{self, HudPaneFrames, HudPosition, HudSpec, HudSpinner, PaneMetrics};
use crate::model::{AppStore, HudOpening, OverlayPane, Session, is_valid_color_hex};
use crate::pane_env::uuid_string;
use crate::resolve::Resolver;

/// `no hud` (upstream's `OverlayHudError.noHud`).
pub(crate) const NO_HUD: &str = "no hud";
/// The failed body write (upstream's `OverlayHudError.writeFailed`).
const WRITE_FAILED: &str = "could not write the hud message";
/// A placement over a pane the deck does not lay out (upstream's `PaneOverlayError.paneNotVisible`).
pub(crate) const PANE_NOT_VISIBLE: &str = "pane not visible";

/// The terminal's padding inside the panel, per side (upstream's `ControlServer.windowPadding`); the panel's surface
/// is given exactly this padding, so the grid the painter centers in owes it.
pub const WINDOW_PADDING: (f64, f64) = (8.0, 6.0);

/// A cell of a font family (`None` for the default monospace) at a point size, width and height in points, for a
/// session no terminal draws yet (upstream's `ControlServer.cellSize(family:size:)`).
pub type CellEstimator = Box<dyn Fn(Option<&str>, f64) -> (f64, f64)>;

/// One armed auto-hide: the HUD of `session` comes down after `delay` unless an open, update or close superseded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HudHideTimer {
    pub session: Uuid,
    pub delay: Duration,
    revision: u64,
}

/// The HUD commands' state beside the model (upstream's `hudAutoHide` and the app-side inputs).
#[derive(Default)]
pub(crate) struct HudRuntime {
    /// The bundled painter's script.
    helper: Option<PathBuf>,
    /// The private directory the body files go to.
    body_dir: Option<PathBuf>,
    cell_estimator: Option<CellEstimator>,
    /// The revision of each session's armed auto-hide; a timer of another revision is inert.
    armed: HashMap<Uuid, u64>,
    next_revision: u64,
    /// Auto-hides armed since the frontend last took them.
    timers: Vec<HudHideTimer>,
}

impl fmt::Debug for HudRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HudRuntime")
            .field("helper", &self.helper)
            .field("body_dir", &self.body_dir)
            .field("armed", &self.armed)
            .field("timers", &self.timers)
            .finish_non_exhaustive()
    }
}

impl HudRuntime {
    /// The eval'd command line for the painter, through `/bin/sh` so a copy without the executable bit still runs.
    fn helper_command(&self) -> Option<String> {
        let helper = self.helper.as_deref().filter(|path| path.is_file())?;
        Some(format!("/bin/sh {}", shell_quoted(helper)))
    }

    /// The per-session body file (upstream's `bodyFile(for:)`), so an update rewrites the path the running painter
    /// opened and a replacement reuses it. `None` until the frontend installed the painter.
    fn body_file(&self, session: Uuid) -> Option<PathBuf> {
        let dir = self.body_dir.as_deref()?;
        Some(dir.join(format!("agterm-hud-{}.txt", uuid_string(session))))
    }

    /// Arms `spec`'s auto-hide for `session`, superseding whatever was armed; a spec that stays only cancels.
    fn arm(&mut self, session: Uuid, spec: &HudSpec) {
        self.arm_after(session, spec.effective_hide_after());
    }

    /// Arms an auto-hide of `hide_after` seconds (0: none) for `session`, superseding whatever was armed.
    fn arm_after(&mut self, session: Uuid, hide_after: f64) {
        self.next_revision += 1;
        let revision = self.next_revision;
        self.armed.insert(session, revision);
        let seconds = hide_after.min(HudSpec::MAX_HIDE_AFTER);
        if seconds > 0.0 {
            self.timers.push(HudHideTimer {
                session,
                delay: Duration::from_secs_f64(seconds),
                revision,
            });
        }
    }
}

/// `path` single-quoted for `/bin/sh`.
fn shell_quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

// MARK: - validation

/// Where a HUD goes (upstream's `ControlHudPlacement`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Placement {
    pub(crate) pane: Option<OverlayPane>,
    pub(crate) pane_id: Option<String>,
}

/// Answers a `session.hud.*` command.
pub(crate) fn dispatch(core: &mut Core, cmd: SessionCmd, request: &Request) -> Outcome {
    let args = args_of(request);
    let target = request.target.as_deref();
    let window = args.window.as_deref();
    if cmd == SessionCmd::HudClose {
        return close(core, target, window);
    }
    let pane = args
        .pane
        .as_deref()
        .map(|raw| {
            OverlayPane::from_control_name(raw).ok_or_else(|| ControlError::rejected("--pane must be left or right"))
        })
        .transpose()?;
    let placement = Placement {
        pane,
        pane_id: args.pane_id.clone(),
    };
    let spec = parse_spec(cmd, args)?;
    if cmd == SessionCmd::HudOpen {
        open(core, target, window, spec, &placement)
    } else {
        update(core, target, window, &spec, &placement)
    }
}

/// Any control character, DEL included.
fn has_control_characters(text: &str) -> bool {
    text.chars().any(char::is_control)
}

/// [`has_control_characters`] less LF and TAB, which markdown structure needs.
fn has_markdown_control_characters(text: &str) -> bool {
    text.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}

/// Validates open and update alike except `--font-size`, which only open takes (upstream's `parseHudSpec`).
fn parse_spec(cmd: SessionCmd, args: &Args) -> Result<HudSpec, ControlError> {
    let name = agterm_proto::Command::Session(cmd).wire_name();
    let markdown = args.markdown.unwrap_or(false);
    let requires_message = || ControlError::rejected(format!("{name} requires a message"));
    let message = args
        .message
        .as_deref()
        .filter(|message| !message.trim().is_empty())
        .ok_or_else(requires_message)?;
    let message_safe = if markdown {
        !has_markdown_control_characters(message)
    } else {
        !has_control_characters(message)
    };
    if !message_safe || has_control_characters(args.detail.as_deref().unwrap_or("")) {
        return Err(ControlError::rejected("hud text must not contain control characters"));
    }
    let cap = if markdown {
        HudSpec::MAX_MARKDOWN_LENGTH
    } else {
        HudSpec::MAX_TEXT_LENGTH
    };
    if hud::text_length(message) > cap {
        return Err(ControlError::rejected(format!(
            "hud message too long (max {cap} characters)"
        )));
    }
    if markdown && !renders_visible_text(message) {
        return Err(requires_message());
    }
    if hud::text_length(args.detail.as_deref().unwrap_or("")) > HudSpec::MAX_TEXT_LENGTH {
        return Err(ControlError::rejected(format!(
            "hud detail too long (max {} characters)",
            HudSpec::MAX_TEXT_LENGTH
        )));
    }
    check_colors(args)?;
    check_numbers(cmd, name, args)?;
    Ok(HudSpec {
        message: message.to_owned(),
        detail: args.detail.clone(),
        spinner: parse_spinner(args.spinner.as_deref())?,
        background_color: args.color.clone(),
        text_color: args.text_color.clone(),
        size_percent: args.size_percent,
        position: parse_position(args.position.as_deref())?,
        hide_after: args.hide_after,
        markdown,
        font_size: args.font_size,
        sticky: args.sticky.unwrap_or(false),
        frame: args.frame.unwrap_or(true),
    })
}

fn check_colors(args: &Args) -> Result<(), ControlError> {
    if let Some(color) = args.color.as_deref().filter(|color| !is_valid_color_hex(color)) {
        return Err(ControlError::rejected(format!("invalid color: {color} (#rrggbb)")));
    }
    if let Some(color) = args.text_color.as_deref().filter(|color| !is_valid_color_hex(color)) {
        return Err(ControlError::rejected(format!("invalid text color: {color} (#rrggbb)")));
    }
    Ok(())
}

/// `--font-size`, `--size-percent` and `--hide-after`, rejected rather than clamped.
fn check_numbers(cmd: SessionCmd, name: &str, args: &Args) -> Result<(), ControlError> {
    if let Some(points) = args.font_size {
        if cmd != SessionCmd::HudOpen {
            return Err(ControlError::rejected(
                "session.hud.update: --font-size is fixed at open; reopen the hud to change it",
            ));
        }
        if !HudSpec::is_valid_font_size(points) {
            let range = HudSpec::FONT_SIZE_RANGE;
            return Err(ControlError::rejected(format!(
                "session.hud.open: --font-size must be {}...{} points",
                range.start(),
                range.end()
            )));
        }
    }
    if args.size_percent.is_some_and(|percent| !(1..=100).contains(&percent)) {
        return Err(ControlError::rejected(format!(
            "{name}: --size-percent must be 1...100"
        )));
    }
    if args
        .hide_after
        .is_some_and(|seconds| !HudSpec::is_valid_hide_after(seconds))
    {
        return Err(ControlError::rejected(format!(
            "{name}: --hide-after must be 0...{} seconds",
            HudSpec::MAX_HIDE_AFTER
        )));
    }
    Ok(())
}

/// `--position`, aliases included; the default when absent.
fn parse_position(raw: Option<&str>) -> Result<HudPosition, ControlError> {
    raw.map_or(Ok(HudPosition::DEFAULT), |raw| {
        HudPosition::parse(raw).ok_or_else(|| {
            ControlError::rejected(format!(
                "invalid position: {raw} ({})",
                HudPosition::accepted_names_list()
            ))
        })
    })
}

/// `--spinner`: `none` (the read-back's spelling) and absence are a static panel.
fn parse_spinner(raw: Option<&str>) -> Result<Option<HudSpinner>, ControlError> {
    match raw {
        None => Ok(None),
        Some(raw) if raw == HudSpinner::NONE_NAME => Ok(None),
        Some(raw) => HudSpinner::from_wire(raw).map(Some).ok_or_else(|| {
            ControlError::rejected(format!(
                "invalid spinner: {raw} ({})",
                HudSpinner::accepted_names_list()
            ))
        }),
    }
}

// MARK: - placement

/// A `--pane-id`/`--pane` pair resolved to the pane's stable identity and role (upstream's `resolvePanePlacement`): a
/// live token wins over the role beside it (the scratch's is refused with `invalid_pane`), an unknown token falls back
/// to that role and is refused alone. With `require_visible` the pane must be laid out.
pub(crate) fn resolve_pane_placement(
    session: &Session,
    fallback: Option<OverlayPane>,
    pane_id: Option<&str>,
    require_visible: bool,
    invalid_pane: &str,
) -> Result<Option<(Uuid, OverlayPane)>, ControlError> {
    let mut pane = fallback;
    if let Some(token) = pane_id.filter(|token| !token.is_empty()) {
        match session.pane_role_for_token(token) {
            Some(crate::model::StatusPane::Scratch) => return Err(ControlError::rejected(invalid_pane)),
            Some(crate::model::StatusPane::Left) => pane = Some(OverlayPane::Left),
            Some(crate::model::StatusPane::Right) => pane = Some(OverlayPane::Right),
            None if pane.is_none() => return Err(super::unknown_pane_id(token)),
            None => {}
        }
    }
    let Some(pane) = pane else {
        return Ok(None);
    };
    if require_visible && !session.renders_overlay_pane(pane) {
        return Err(ControlError::rejected(PANE_NOT_VISIBLE));
    }
    match pane {
        OverlayPane::Left => Ok(Some((session.pane_identity, pane))),
        OverlayPane::Right => match session.split_pane_identity {
            Some(identity) => Ok(Some((identity, pane))),
            None if require_visible => Err(ControlError::rejected(PANE_NOT_VISIBLE)),
            None => Err(ControlError::rejected("session has no split")),
        },
    }
}

// MARK: - measuring

/// The point size the live HUD's surface was created at (upstream's `liveHudFontSize`).
fn live_hud_font_size(session: &Session, base: f64) -> f64 {
    session
        .overlay
        .hud
        .as_ref()
        .and_then(|hud| hud.font_size)
        .or(session.font_size)
        .unwrap_or(base)
}

/// The cell libghostty draws for `session` at `font_size` (upstream's `liveCellSize`): the live panel's own at its
/// size, else a pane's scaled from the size that pane is drawn at.
fn live_cell_size(session: &Session, font_size: f64, base: f64) -> Option<(f64, f64)> {
    if session.overlay.hud_active()
        && (live_hud_font_size(session, base) - font_size).abs() < 0.01
        && let Some(cell) = session.overlay.surface.get().and_then(agterm_host::Surface::cell_size)
    {
        return Some(cell);
    }
    [&session.surface, &session.split_surface]
        .into_iter()
        .filter_map(|slot| slot.get())
        .find_map(|surface| {
            let (width, height) = surface.cell_size()?;
            let drawn = surface.font_size().filter(|drawn| *drawn > 0.0)?;
            let scale = font_size / drawn;
            Some((width * scale, height * scale))
        })
}

/// The bounds a panel scoped to `pane` (else session-wide) is laid out in, from the deck's last report.
fn layout_size(frames: &HudPaneFrames, pane: Option<OverlayPane>) -> (f64, f64) {
    let frame = match pane {
        Some(pane) => frames.pane(pane == OverlayPane::Right),
        None => frames.detail.or_else(|| union(frames.left, frames.right)),
    };
    frame.map_or((0.0, 0.0), |frame| (frame.width, frame.height))
}

fn union(a: Option<hud::HudPaneFrame>, b: Option<hud::HudPaneFrame>) -> Option<hud::HudPaneFrame> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
            Some(hud::HudPaneFrame {
                x,
                y,
                width: (a.x + a.width).max(b.x + b.width) - x,
                height: (a.y + a.height).max(b.y + b.height) - y,
            })
        }
        (one, other) => one.or(other),
    }
}

impl Core {
    /// The base point size a session without its own font takes: the settings' font size, else ghostty's default.
    fn hud_base_font_size(&self) -> f64 {
        self.settings().font_size.unwrap_or(GHOSTTY_DEFAULT_FONT_SIZE)
    }

    /// The metrics a panel over `session` (scoped to `pane`) is measured with at `font_size` (upstream's
    /// `paneMetrics`); an unmeasured session takes the layout's fallbacks.
    fn hud_metrics(&self, session: &Session, pane: Option<OverlayPane>, font_size: f64) -> PaneMetrics {
        let base = self.hud_base_font_size();
        let (cell_width, cell_height) = live_cell_size(session, font_size, base)
            .or_else(|| {
                let family = self.settings().font_family.as_deref();
                self.hud
                    .cell_estimator
                    .as_ref()
                    .map(|estimate| estimate(family, font_size))
            })
            .unwrap_or((0.0, 0.0));
        let (pane_width, pane_height) = layout_size(&session.hud_frames, pane);
        PaneMetrics {
            cell_width,
            cell_height,
            pane_width,
            pane_height,
            padding_width: WINDOW_PADDING.0,
            padding_height: WINDOW_PADDING.1,
        }
    }

    /// Installs the bundled painter's script and the private directory the body files go to
    /// ([`crate::paths::hud_body_dir`], made 0700 on each write); without them `session.hud.open` is refused.
    pub fn set_hud_helper(&mut self, script: PathBuf, body_dir: PathBuf) {
        self.hud.helper = Some(script);
        self.hud.body_dir = Some(body_dir);
    }

    /// Installs the cell estimate a HUD over a session no terminal draws yet is sized with.
    pub fn set_cell_estimator(&mut self, estimator: CellEstimator) {
        self.hud.cell_estimator = Some(estimator);
    }

    /// Takes the auto-hides armed since the last take; each fires [`Core::fire_hud_hide`] after its delay.
    pub fn take_hud_timers(&mut self) -> Vec<HudHideTimer> {
        std::mem::take(&mut self.hud.timers)
    }

    /// An auto-hide elapsed: the HUD comes down unless a later open, update or close superseded the timer. Whether it
    /// closed one; commit afterwards.
    pub fn fire_hud_hide(&mut self, timer: HudHideTimer) -> bool {
        if self.hud.armed.get(&timer.session) != Some(&timer.revision) {
            return false;
        }
        self.hud.armed.remove(&timer.session);
        self.library_mut()
            .store_for_session_mut(timer.session)
            .is_some_and(|store| store.close_hud(timer.session))
    }

    /// The deck laid session `session` out anew: its detail area or pane frames, merged over the last non-empty ones
    /// (upstream's `cachePaneFrames`). A live HUD is measured again against them and its body rewritten. Whether a HUD
    /// was re-measured, which then needs a commit.
    pub fn report_hud_frames(&mut self, session: Uuid, frames: &HudPaneFrames) -> bool {
        let Some(found) = self
            .library_mut()
            .store_for_session_mut(session)
            .and_then(|store| store.session_mut(session))
        else {
            return false;
        };
        let before = found.hud_frames;
        found.hud_frames.merge(frames);
        if found.hud_frames == before || !found.overlay.hud_active() {
            return false;
        }
        self.refresh_hud(session);
        true
    }

    /// Measures the live HUD against the bounds it is laid out in now and rewrites its body (upstream's
    /// `refreshHud`), as after a geometry change, a resize or the panel's surface coming up. The spec, scope, surface
    /// and auto-hide stay; a refused write puts the size back. Whether it landed.
    pub fn refresh_hud(&mut self, session: Uuid) -> bool {
        let Some((metrics, previous, forced)) = self.session_of(session).and_then(|found| {
            let slot = &found.overlay;
            let previous = slot.hud_panel_size()?;
            let forced = slot.hud.as_ref()?.resized_width_percent;
            let font = live_hud_font_size(found, self.hud_base_font_size());
            Some((self.hud_metrics(found, found.hud_target_pane(), font), previous, forced))
        }) else {
            return false;
        };
        let Some(store) = self.library_mut().store_for_session_mut(session) else {
            return false;
        };
        store.remeasure_hud(session, &metrics);
        if write_body(store, session, &metrics) {
            return true;
        }
        store.set_hud_size(session, previous, forced);
        false
    }

    /// The terminal of slot generation `generation` of session `session` ended (upstream's overlay `onExit`): the slot
    /// closes, unless a replacement took it since, wherever its session is, a pending close included (its terminal
    /// runs on, and an undo brings the same object back). Whether it closed; commit afterwards.
    pub fn overlay_exited(&mut self, session: Uuid, generation: u64) -> bool {
        let closed = self.library_mut().held_session_mut(session).is_some_and(|found| {
            found.overlay.active && found.overlay.generation == generation && found.overlay.close()
        });
        if closed {
            self.config_edit_ended(session, generation);
        }
        closed
    }

    /// Session `id`, wherever its window is.
    fn session_of(&self, id: Uuid) -> Option<&Session> {
        let window = self.library().window_holding_session(id)?;
        self.library().store(window)?.session(id)
    }
}

/// Writes the live HUD's body atomically (a temp file renamed over it: the painter re-reads it with no locking), its
/// grid the one the panel took (upstream's `writeHudBody`). False with no HUD up or a refused write.
fn write_body(store: &AppStore, session: Uuid, pane: &PaneMetrics) -> bool {
    let Some(slot) = store.session(session).map(|found| &found.overlay) else {
        return false;
    };
    let (Some(hud_state), Some(spec), Some(size)) =
        (slot.hud.as_ref(), slot.effective_hud_spec(), slot.hud_panel_size())
    else {
        return false;
    };
    let grid = hud::paint_grid(&spec, &size, pane);
    let body = hud::rendered_body(&spec, grid, std::process::id());
    let staging = hud_state.file.with_extension("txt.tmp");
    let written = write_private(&staging, body.as_bytes()).and_then(|()| std::fs::rename(&staging, &hud_state.file));
    if let Err(error) = written {
        tracing::warn!(file = %hud_state.file.display(), %error, "could not write the hud body");
        let _ = std::fs::remove_file(&staging);
        return false;
    }
    true
}

/// Writes `contents` to a fresh `path` only its owner can read, in a directory only its owner can enter: the body
/// carries whatever an agent posts, and in a shared directory another user could read it or squat its name. A
/// staging file a crash left behind is replaced. Unix closes the modes (0700, 0600); Windows gives the directory and
/// the file an owner-only DACL ([`crate::owner_only`]), wherever the directory is.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
        let metadata = std::fs::symlink_metadata(dir)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::other("the hud body directory is not a directory"));
        }
        close_private_dir(dir, &metadata)?;
    }
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    // created under the directory's owner-only grants already; the file's own DACL keeps them across the rename
    #[cfg(windows)]
    crate::owner_only::restrict(path, false)?;
    file.write_all(contents)
}

/// Creates `dir` and its parents, the last one owner-only (0700).
#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// Creates `dir` and its parents; [`close_private_dir`] then gives the last one to its owner.
#[cfg(windows)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().recursive(true).create(dir)
}

/// Gives `dir`, new or not, an owner-only DACL: what it inherited from a shared parent (a state directory override,
/// `C:\tmp`) could grant other users, and Windows has no mode bits to close.
#[cfg(windows)]
fn close_private_dir(dir: &Path, _metadata: &std::fs::Metadata) -> std::io::Result<()> {
    crate::owner_only::restrict(dir, true)
}

/// Closes an existing `dir` that its group or others can reach back to 0700.
#[cfg(unix)]
fn close_private_dir(dir: &Path, metadata: &std::fs::Metadata) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    if metadata.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// MARK: - the commands

/// `session.hud.open` (upstream's `openHud`).
pub(crate) fn open(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    spec: HudSpec,
    placement: &Placement,
) -> Outcome {
    let resolved = Resolver::new(core.library()).session(target, window)?;
    let id = resolved.session;
    let session = core
        .library()
        .store(resolved.window)
        .and_then(|store| store.session(id))
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    let (Some(command), Some(file)) = (core.hud.helper_command(), core.hud.body_file(id)) else {
        return Err(ControlError::rejected("hud helper is not bundled in this build"));
    };
    let resolved_pane = resolve_pane_placement(
        session,
        placement.pane,
        placement.pane_id.as_deref(),
        true,
        "hud pane must be left or right",
    )?;
    let font_size = spec
        .font_size
        .or(session.font_size)
        .unwrap_or_else(|| core.hud_base_font_size());
    let metrics = core.hud_metrics(session, resolved_pane.map(|(_, pane)| pane), font_size);
    let opening = HudOpening {
        command,
        file,
        size: hud::panel_size(&spec, &metrics),
        pane_identity: resolved_pane.map(|(identity, _)| identity),
        font_size: Some(font_size),
    };
    let store = core
        .library_mut()
        .store_mut(resolved.window)
        .ok_or_else(|| ControlError::rejected("no such session"))?;
    // open first, write second: replacing a live HUD discards its body file at this same path
    let hide_after = spec.effective_hide_after();
    if !store.open_hud(id, spec, opening) {
        return Err(ControlError::rejected("overlay already open"));
    }
    if !write_body(store, id, &metrics) {
        store.close_hud(id);
        return Err(ControlError::rejected(WRITE_FAILED));
    }
    core.hud.arm_after(id, hide_after);
    // published once its body is on disk, never before
    core.publish_hud(id, hide_after);
    Ok(with_id(id))
}

/// `session.hud.update` (upstream's `updateHud`): the body rewritten and the panel re-sized in place; a failed write
/// rolls the store back, keeping the deadline the painted message came with.
pub(crate) fn update(
    core: &mut Core,
    target: Option<&str>,
    window: Option<&str>,
    spec: &HudSpec,
    placement: &Placement,
) -> Outcome {
    let resolved = Resolver::new(core.library()).session(target, window)?;
    let id = resolved.session;
    let session = core
        .library()
        .store(resolved.window)
        .and_then(|store| store.session(id))
        .filter(|session| session.overlay.hud_active())
        .ok_or_else(|| ControlError::rejected(NO_HUD))?;
    let (Some(previous), Some(previous_size)) = (session.overlay.hud.clone(), session.overlay.hud_panel_size()) else {
        return Err(ControlError::rejected(NO_HUD));
    };
    let resolved_pane = resolve_pane_placement(
        session,
        placement.pane,
        placement.pane_id.as_deref(),
        false,
        "hud pane must be left or right",
    )?;
    let font_size = live_hud_font_size(session, core.hud_base_font_size());
    let metrics = core.hud_metrics(session, resolved_pane.map(|(_, pane)| pane), font_size);
    let size = hud::panel_size(spec, &metrics);
    let store = core
        .library_mut()
        .store_mut(resolved.window)
        .ok_or_else(|| ControlError::rejected(NO_HUD))?;
    store.update_hud(id, spec, size, resolved_pane.map(|(identity, _)| identity));
    if !write_body(store, id, &metrics) {
        store.update_hud(id, &previous.spec, previous_size, previous.pane_identity);
        store.set_hud_size(id, previous_size, previous.resized_width_percent);
        return Err(ControlError::rejected(WRITE_FAILED));
    }
    core.hud.arm(id, spec);
    core.publish_hud(id, spec.effective_hide_after());
    Ok(with_id(id))
}

/// `session.hud.close` (upstream's `closeHud`): refused for a caller's program.
fn close(core: &mut Core, target: Option<&str>, window: Option<&str>) -> Outcome {
    let (store, id) = super::session_store(core, target, window)?;
    if !store.close_hud(id) {
        return Err(ControlError::rejected(NO_HUD));
    }
    Ok(with_id(id))
}
