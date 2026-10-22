//! The windows' frames, saved per window id when a window closes (the user's close, a command's, the quit) and given
//! back once when it opens again (upstream's `TitleProbeView`, which keeps them in `UserDefaults` under the window's
//! UUID). Here they live in `window-frames.json` in the state directory: the size, the origin and whether the window
//! was maximized.
//!
//! A frame comes back constrained to the displays attached now (upstream's `constrainedRestoredFrame`, with
//! [`ngterm_core::window_geometry`]). Where the platform places a window at its origin (X11, whose displays are its
//! monitors here): on the display it overlaps most, else the primary one, shrunk to it, grown to the window's least
//! size, and wholly on it. Wayland gives a client neither its window's position nor a say in it, so a saved origin
//! there means nothing, and no display is matched by it: the size alone comes back, shrunk to the largest display
//! attached and grown to the least size, and the compositor places the window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ngterm_core::window_geometry::{self, Rect, Size};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The file in the state directory.
pub const FILE_NAME: &str = "window-frames.json";
/// A window's least size (upstream's `.frame(minWidth: 640, minHeight: 400)`).
pub const MIN_SIZE: Size = Size::new(640.0, 400.0);
/// A window's size the first time it opens (upstream's `.defaultSize(width: 900, height: 600)`).
pub const DEFAULT_SIZE: Size = Size::new(900.0, 600.0);

/// Whether the platform puts a window where its frame says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// At the frame's origin (X11 and the other desktops).
    AtOrigin,
    /// Wherever the compositor chooses (Wayland): the origin is no position.
    ByCompositor,
}

/// One window's frame, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Maximized when it closed; the frame is then the one it is restored to.
    #[serde(default)]
    pub maximized: bool,
}

impl Frame {
    #[must_use]
    pub const fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.width, self.height)
    }

    /// The frame placed at `rect`, its maximized state kept.
    #[must_use]
    pub const fn at(self, rect: Rect) -> Self {
        Self {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
            maximized: self.maximized,
        }
    }

    /// Whether it describes a window at all (a positive, finite size).
    #[must_use]
    pub fn is_usable(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width > 0.0
            && self.height > 0.0
    }

    /// The frame to restore on `displays` (the first is the fallback) under `placement`: placed at its origin, on the
    /// display it overlaps most, shrunk, grown to [`MIN_SIZE`] and contained; placed by the compositor, only its size,
    /// shrunk to the largest display and grown to [`MIN_SIZE`], the origin left as it is. Unchanged when no display is
    /// known.
    #[must_use]
    pub fn constrained(self, displays: &[Rect], placement: Placement) -> Self {
        let Some(fallback) = displays.first() else {
            return self;
        };
        match placement {
            Placement::AtOrigin => {
                let display = window_geometry::best_display_index(self.rect(), displays)
                    .and_then(|index| displays.get(index))
                    .unwrap_or(fallback);
                self.at(window_geometry::constrain(self.rect(), MIN_SIZE, *display))
            }
            Placement::ByCompositor => {
                let largest = displays
                    .iter()
                    .map(|display| display.size)
                    .fold(fallback.size, |largest, size| {
                        if size.width * size.height > largest.width * largest.height {
                            size
                        } else {
                            largest
                        }
                    });
                let size = window_geometry::clamp_size(self.rect().size, MIN_SIZE, largest);
                self.at(Rect::new(self.x, self.y, size.width, size.height))
            }
        }
    }
}

/// The saved frames and their file.
#[derive(Debug, Default)]
pub struct WindowFrames {
    path: Option<PathBuf>,
    frames: BTreeMap<Uuid, Frame>,
}

impl WindowFrames {
    /// The frames saved under `state_dir`; none when the file is missing or unreadable (logged).
    #[must_use]
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(FILE_NAME);
        let frames = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<BTreeMap<Uuid, Frame>>(&bytes).unwrap_or_else(|err| {
                tracing::warn!(%err, path = %path.display(), "the saved window frames do not read");
                BTreeMap::new()
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "the saved window frames do not read");
                BTreeMap::new()
            }
        };
        Self {
            path: Some(path),
            frames: frames.into_iter().filter(|(_, frame)| frame.is_usable()).collect(),
        }
    }

    /// Window `id`'s saved frame.
    #[must_use]
    pub fn get(&self, id: Uuid) -> Option<Frame> {
        self.frames.get(&id).copied()
    }

    /// Records window `id`'s `frame` and writes the file (a failure is logged); an unusable frame is not kept. Frames
    /// of windows not in `known` (deleted from the library) are dropped on the way.
    pub fn save(&mut self, id: Uuid, frame: Frame, known: &[Uuid]) {
        if frame.is_usable() {
            self.frames.insert(id, frame);
        }
        self.frames.retain(|id, _| known.contains(id));
        self.write();
    }

    fn write(&self) {
        let Some(path) = &self.path else { return };
        let bytes = match serde_json::to_vec_pretty(&self.frames) {
            Ok(bytes) => bytes,
            Err(err) => {
                tracing::warn!(%err, "the window frames do not serialize");
                return;
            }
        };
        if let Err(err) = ngterm_core::persist::json::write_atomic(path, &bytes) {
            tracing::warn!(%err, path = %path.display(), "the window frames were not saved");
        }
    }
}

#[cfg(test)]
mod tests;
