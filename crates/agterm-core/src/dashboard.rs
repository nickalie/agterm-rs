//! The dashboard's host-free model: a window's view-only grid of up to [`MAX_CELLS`] pane cells, its keyboard
//! highlight and font mode (upstream's `DashboardLayout.swift`, `DashboardTarget.swift`, `DashboardController.swift`
//! and `AppStore+Dashboard.swift`).
//!
//! A cell is a session plus one of its two panes ([`DashboardMember`]), so a split session takes two cells. The grid
//! is `ceil(sqrt(n))` columns wide, filled row-major; the highlight walks it without wrapping. The `dashboard` command
//! and the frontend drive one [`DashboardController`] per window; the frontend lays the members' own terminals out at
//! their cells (a terminal is never re-parented) and sizes their font by [`DashboardFontMode`].

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::model::AppStore;
use crate::pane_env::uuid_string;
use crate::zoom::ZoomSurface;

/// The most cells a grid holds; a bigger request is capped to it.
pub const MAX_CELLS: usize = 9;

/// The smallest font size (points) the auto size applies, so a dense grid stays legible.
pub const MIN_FONT_SIZE: f64 = 6.0;

/// libghostty's built-in terminal font size (points): the auto size's base when Settings sets none.
pub const GHOSTTY_DEFAULT_FONT_SIZE: f64 = 13.0;

/// A one-step highlight move in the grid; no wraparound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// The grid of `count` cells as `(cols, rows)`: `cols = ceil(sqrt(n))`, `rows = ceil(n / cols)`, with `n` clamped into
/// `1..=MAX_CELLS` so no request makes an empty or an oversized grid.
#[must_use]
pub fn grid(count: usize) -> (usize, usize) {
    let n = count.clamp(1, MAX_CELLS);
    let cols = (1..=n).find(|cols| cols * cols >= n).unwrap_or(n);
    (cols, n.div_ceil(cols))
}

/// The row-major `(col, row)` of the cell at `index` in a `cols`-wide grid (a zero width counts as one column).
#[must_use]
pub fn cell(index: usize, cols: usize) -> (usize, usize) {
    let width = cols.max(1);
    (index % width, index / width)
}

/// The highlighted index after a one-step `direction` move from `from` in a `cols`-wide grid of `count` cells. A move
/// off an edge, or into an empty slot of a ragged last row, stays put; an index out of range is clamped into it first.
#[must_use]
pub fn move_index(from: usize, direction: Direction, cols: usize, count: usize) -> usize {
    let width = cols.max(1);
    let total = count.max(1);
    if from >= total {
        return total - 1;
    }
    let (col, row) = cell(from, width);
    match direction {
        Direction::Left if col > 0 => from - 1,
        Direction::Right if col + 1 < width && from + 1 < total => from + 1,
        Direction::Up if row > 0 => from - width,
        Direction::Down if (row + 1) * width + col < total => (row + 1) * width + col,
        _ => from,
    }
}

/// The auto font size (points) of a `cols`×`rows` grid over `base`: the base shrunk by the grid's factor, rounded,
/// floored at [`MIN_FONT_SIZE`].
#[must_use]
pub fn dashboard_font_size(cols: usize, rows: usize, base: f64) -> f64 {
    (base * font_factor(cols, rows)).round().max(MIN_FONT_SIZE)
}

/// Upstream's per-shape shrink factor; [`grid`] never makes a shape denser than 3×3.
fn font_factor(cols: usize, rows: usize) -> f64 {
    match (cols, rows) {
        (1, 1) => 1.00,
        (2, 1) => 0.85,
        (2, 2) => 0.75,
        (3, 2) => 0.62,
        _ => 0.55,
    }
}

/// How the grid sizes its member terminals' font: `Untouched` leaves each at its own size, `Fixed` applies one size to
/// every cell, `Auto` derives one from the grid shape so a denser grid shrinks.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum DashboardFontMode {
    #[default]
    Untouched,
    Fixed(f64),
    Auto,
}

impl DashboardFontMode {
    /// The size the frontend applies to every member over `member_count` cells: `None` for `Untouched`, the fixed
    /// value, or the grid's auto size over `base`. The command and the frontend share it, so the read-back is the
    /// applied size from the moment the command returns.
    #[must_use]
    pub fn applied_font_size(self, member_count: usize, base: f64) -> Option<f64> {
        match self {
            Self::Untouched => None,
            Self::Fixed(size) => Some(size),
            Self::Auto => {
                let (cols, rows) = grid(member_count);
                Some(dashboard_font_size(cols, rows, base))
            }
        }
    }

    /// The `dashboardFontMode` read-back.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Untouched => "untouched",
            Self::Fixed(_) => "fixed",
            Self::Auto => "auto",
        }
    }
}

/// One cell: a session and which of its panes it hosts, [`ZoomSurface::Primary`] or [`ZoomSurface::Split`] (never a
/// scratch or an overlay).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DashboardMember {
    pub session: Uuid,
    pub surface: ZoomSurface,
}

impl DashboardMember {
    #[must_use]
    pub fn primary(session: Uuid) -> Self {
        Self {
            session,
            surface: ZoomSurface::Primary,
        }
    }

    #[must_use]
    pub fn split(session: Uuid) -> Self {
        Self {
            session,
            surface: ZoomSurface::Split,
        }
    }

    /// The `tree` reference, `<UUID>:left` or `<UUID>:right`.
    #[must_use]
    pub fn control_ref(self) -> String {
        format!("{}:{}", uuid_string(self.session), self.surface.wire_name())
    }
}

/// One `dashboard` target: a head the resolver matches (`active`, an id, a prefix) and an optional pane; `None` takes
/// every pane of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardTarget {
    pub head: String,
    pub pane: Option<ZoomSurface>,
}

impl DashboardTarget {
    /// Parses a raw target; `None` for an empty one or a malformed pane suffix. It splits on the first colon, as a head
    /// never holds one, so the `surface.zoom` form `surface:<id>:left` fails here. The pane takes its position and role
    /// aliases in any case.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let Some((head, suffix)) = raw.split_once(':') else {
            return (!raw.is_empty()).then(|| Self {
                head: raw.to_owned(),
                pane: None,
            });
        };
        if head.is_empty() {
            return None;
        }
        let pane = match suffix.to_lowercase().as_str() {
            "left" | "top" | "primary" => ZoomSurface::Primary,
            "right" | "bottom" | "split" => ZoomSurface::Split,
            _ => return None,
        };
        Some(Self {
            head: head.to_owned(),
            pane: Some(pane),
        })
    }
}

/// A [`DashboardTarget`] whose head resolved to a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedDashboardTarget {
    pub session: Uuid,
    pub pane: Option<ZoomSurface>,
}

/// One window's dashboard: the cells in grid order, the highlighted one, the font mode and the size applied. Open
/// while it has members.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DashboardController {
    members: Vec<DashboardMember>,
    highlighted: Option<DashboardMember>,
    font_mode: DashboardFontMode,
    applied_font_size: Option<f64>,
    /// The split pane identity each split cell watched at the last sync, so a promotion (the primary's shell exiting
    /// moves the split's into the primary slot, identity and all) carries the cell along.
    split_identities: HashMap<Uuid, Uuid>,
}

impl DashboardController {
    #[must_use]
    pub fn is_open(&self) -> bool {
        !self.members.is_empty()
    }

    /// The cells, row-major; empty when closed.
    #[must_use]
    pub fn members(&self) -> &[DashboardMember] {
        &self.members
    }

    /// The highlighted cell, always a member while open.
    #[must_use]
    pub fn highlighted(&self) -> Option<DashboardMember> {
        self.highlighted
    }

    #[must_use]
    pub fn font_mode(&self) -> DashboardFontMode {
        self.font_mode
    }

    /// The size the members' font is set to (`dashboardFontSize`); `None` when untouched or closed.
    #[must_use]
    pub fn applied_font_size(&self) -> Option<f64> {
        self.applied_font_size
    }

    /// Opens over `members`, the highlight on `highlighted` when it is one of them, else on the first.
    pub fn open(
        &mut self,
        members: Vec<DashboardMember>,
        highlighted: Option<DashboardMember>,
        font_mode: DashboardFontMode,
    ) {
        self.highlighted = highlighted
            .filter(|member| members.contains(member))
            .or_else(|| members.first().copied());
        self.members = members;
        self.font_mode = font_mode;
    }

    /// Closes and resets everything.
    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// Moves the highlight to `member` when it is one of the cells (a click flashes the cell it enters).
    pub fn highlight(&mut self, member: DashboardMember) {
        if self.members.contains(&member) {
            self.highlighted = Some(member);
        }
    }

    /// Walks the highlight one step, clamped by the grid; nothing while closed.
    pub fn move_highlight(&mut self, direction: Direction) {
        let Some(from) = self
            .highlighted
            .and_then(|highlighted| self.members.iter().position(|member| *member == highlighted))
        else {
            return;
        };
        let (cols, _) = grid(self.members.len());
        let to = move_index(from, direction, cols, self.members.len());
        self.highlighted = self.members.get(to).copied();
    }

    /// Records the size the members' font is set to.
    pub fn set_applied_font_size(&mut self, size: Option<f64>) {
        self.applied_font_size = size;
    }

    /// Follows a promoted split survivor: a split cell of `session` now names the primary slot, collapsing into an
    /// existing primary cell, the highlight carried along. Nothing without a split cell.
    pub fn promote_split_member(&mut self, session: Uuid) {
        let promoted = DashboardMember::split(session);
        let destination = DashboardMember::primary(session);
        let Some(index) = self.members.iter().position(|member| *member == promoted) else {
            return;
        };
        if self.members.contains(&destination) {
            self.members.remove(index);
        } else {
            self.members[index] = destination;
        }
        if self.highlighted == Some(promoted) {
            self.highlighted = Some(destination);
        }
    }

    /// Drops the cells not in `existing` (a session or split closed while open), keeping the order; the highlight moves
    /// to the first survivor when its cell went, and the dashboard closes when none survives. A no-op when nothing went.
    pub fn reconcile(&mut self, existing: &HashSet<DashboardMember>) {
        let survivors: Vec<DashboardMember> = self
            .members
            .iter()
            .copied()
            .filter(|member| existing.contains(member))
            .collect();
        if survivors.len() == self.members.len() {
            return;
        }
        if survivors.is_empty() {
            self.close();
            return;
        }
        if self
            .highlighted
            .is_some_and(|highlighted| !survivors.contains(&highlighted))
        {
            self.highlighted = survivors.first().copied();
        }
        self.members = survivors;
    }

    /// Brings the cells in line with `store` after any model change: a split cell whose watched pane was promoted into
    /// the primary slot follows it ([`Self::promote_split_member`]), then the cells whose pane is gone drop out
    /// ([`Self::reconcile`]), the applied font size follows the surviving cell count over the settings' `base` size
    /// (upstream re-applies the mode whenever the members change), and the split identities are recorded for the
    /// next sync.
    pub fn sync_with_store(&mut self, store: &AppStore, base: f64) {
        let promoted: Vec<Uuid> = self
            .members
            .iter()
            .filter(|member| member.surface == ZoomSurface::Split)
            .filter_map(|member| {
                let session = store.session(member.session)?;
                let watched = self.split_identities.get(&member.session)?;
                (!session.split.exists && session.pane_identity == *watched).then_some(member.session)
            })
            .collect();
        for session in promoted {
            self.promote_split_member(session);
        }
        self.reconcile(&store.dashboard_valid_members());
        if self.is_open() {
            self.applied_font_size = self.font_mode.applied_font_size(self.members.len(), base);
        }
        self.track_splits(store);
    }

    /// Records the split pane identity of each split cell.
    pub fn track_splits(&mut self, store: &AppStore) {
        self.split_identities = self
            .members
            .iter()
            .filter(|member| member.surface == ZoomSurface::Split)
            .filter_map(|member| {
                let identity = store.session(member.session)?.split_pane_identity?;
                Some((member.session, identity))
            })
            .collect();
    }
}

impl AppStore {
    /// The cells of `targets` in order: a bare target takes the session's primary pane plus its split when it has one
    /// (shown or hidden), an explicit pane that cell alone. Unknown sessions are skipped and repeated cells collapse,
    /// so no terminal is hosted twice. Uncapped (upstream's `dashboardPaneCells`).
    #[must_use]
    pub fn dashboard_pane_cells(&self, targets: &[ResolvedDashboardTarget]) -> Vec<DashboardMember> {
        let mut seen = HashSet::new();
        let mut members = Vec::new();
        for target in targets {
            let Some(session) = self.session(target.session) else {
                continue;
            };
            let mut cells = vec![DashboardMember {
                session: target.session,
                surface: target.pane.unwrap_or(ZoomSurface::Primary),
            }];
            if target.pane.is_none() && session.split.exists {
                cells.push(DashboardMember::split(target.session));
            }
            members.extend(cells.into_iter().filter(|cell| seen.insert(*cell)));
        }
        members
    }

    /// The cells of `targets` capped to `limit`, and how many panes the cap dropped.
    #[must_use]
    pub fn dashboard_members(
        &self,
        targets: &[ResolvedDashboardTarget],
        limit: usize,
    ) -> (Vec<DashboardMember>, usize) {
        let mut members = self.dashboard_pane_cells(targets);
        let dropped = members.len().saturating_sub(limit);
        members.truncate(limit);
        (members, dropped)
    }

    /// The cells of the bare sessions `ids`, capped to `limit`, and how many panes the cap dropped.
    #[must_use]
    pub fn dashboard_members_for_ids(&self, ids: &[Uuid], limit: usize) -> (Vec<DashboardMember>, usize) {
        let targets: Vec<ResolvedDashboardTarget> = ids
            .iter()
            .map(|id| ResolvedDashboardTarget {
                session: *id,
                pane: None,
            })
            .collect();
        self.dashboard_members(&targets, limit)
    }

    /// The cells of the most recently used sessions, `limit` bounding both the sessions taken and the cells.
    #[must_use]
    pub fn dashboard_mru_members(&self, limit: usize) -> Vec<DashboardMember> {
        self.dashboard_members_for_ids(&self.recent_sessions(limit), limit).0
    }

    /// Every cell the store can host now: each session's primary pane, plus its split while it has one.
    #[must_use]
    pub fn dashboard_valid_members(&self) -> HashSet<DashboardMember> {
        self.sessions()
            .flat_map(|session| {
                let split = session.split.exists.then(|| DashboardMember::split(session.id()));
                std::iter::once(DashboardMember::primary(session.id())).chain(split)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
