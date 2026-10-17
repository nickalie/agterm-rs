//! Kitty graphics in the terminal element (plan > Task 16; Technical Details > Frame): the images of a frame's
//! [`KittyView`] as GPUI textures, and their placements — positioned ones and the runs of Unicode placeholder cells
//! that show virtual ones — drawn in Ghostty's three layers: below the cell backgrounds, between the backgrounds and
//! the text, above the text.
//!
//! Ported from Ghostty's renderer (<https://github.com/ghostty-org/ghostty>, MIT, Copyright Mitchell Hashimoto and
//! Ghostty contributors: `src/renderer/image.zig` `kittyUpdate`/`prepKittyVirtualPlacement`/`draw`,
//! `shaders/glsl/image.v.glsl`) by way of qwertty-term (<https://github.com/joshka/qwertty-term>, MIT, Copyright (c)
//! 2026 Josh `McKinney`: the z sort and layer split of `qwertty-term-renderer`, and the eviction of images the
//! terminal no longer holds in `prepare_image_frame`). The relative placements rooted at virtual ones
//! ([`rooted_at_virtual`], `kittyUpdate`'s `pending_relative` and `virtual_origins`) are checked against rustty's port
//! of `kittyUpdate` (<https://github.com/AirelyDev/rustty>, MIT, Copyright (c) 2026 Airely, Gael Hatchue, and rustty
//! contributors: `crates/rustty-renderer/src/image/linux_impl.rs`).
//!
//! - **Geometry**: a placement lands at its viewport cell times the cell size plus its offset inside that cell
//!   (clamped to the cell, Ghostty's `cellOffset`), at the pixel size and from the source rectangle libghostty-vt
//!   resolved; a placement scrolled partly off the top has a negative row and is clipped by the element. A run of
//!   placeholder cells ([`agterm_term::virtual_placements`]) draws its slice of the image fitted into its virtual
//!   placement's grid ([`placeholder::render`]) at `z = -1`, as Ghostty does. A relative placement (`P=`) rooted at
//!   a virtual placement lands at the minimum column and the minimum row of the runs showing its root plus its chain's
//!   offsets ([`agterm_term::VirtualRooted`], Ghostty's `pending_relative`), and is not drawn while none of those runs
//!   is on the viewport. The pixels are the terminal's (the owner's cells): a viewer drawing at its own font size
//!   scales them by its cell over the terminal's.
//! - **Order**: by `z`, then image id (Ghostty's `zLessThan`), each layer drawn where Ghostty draws it.
//! - **Images**: converted off the main thread, bounded and shared by the terminal's views ([`textures`]); a
//!   placement whose image is still converting draws nothing yet. The source rectangle is drawn by painting the whole
//!   image scaled so that rectangle covers the placement, clipped to it ([`image_bounds`]): GPUI then samples only
//!   that part of the tile.
//! - **Atlas**: a view keeps what it painted into its window's sprite atlas ([`ViewImages`]) and drops an image from
//!   the atlas once it stops drawing it (deleted, replaced by a new generation, scrolled out) and all of them when the
//!   view goes.

pub mod placeholder;
pub mod textures;

use std::collections::HashMap;
use std::sync::Arc;

use agterm_term::{
    Frame, KittyImageKey, KittyLayer, KittyPlacement, KittyView, PlacementKey, Row, SourceRect, VirtualPlacement,
    virtual_placements,
};
use gpui_kit::{Bounds, Context, Corners, ImageId, Pixels, RenderImage, Subscription, Window, point, px, size};

pub use self::textures::{KittyStore, MAX_TEXTURE_PIXELS, MAX_TEXTURE_SIDE, Texture, texture_size};
use super::background::{BackgroundPaint, Spec};
use super::{CellMetrics, TerminalView};

/// The z-index Ghostty draws a virtual placement's cells at (below the text).
pub const VIRTUAL_Z: i32 = -1;

/// A rectangle in device pixels, from the grid's top-left corner.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DeviceRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where one placement is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacementGeometry {
    pub image: KittyImageKey,
    pub layer: KittyLayer,
    pub z: i32,
    /// The rectangle drawn (device pixels from the grid's top-left corner; negative when scrolled partly off).
    pub dest: DeviceRect,
    /// The part of the image drawn, in the image's pixels.
    pub source: SourceRect,
}

/// The cell a frame's pixel geometry was computed with (the terminal's, the owner view's) and the cell a view draws
/// with, both in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cells {
    pub engine: (u32, u32),
    pub view: (u32, u32),
}

impl Cells {
    /// View pixels per terminal pixel, across and down.
    fn scale(self) -> (f64, f64) {
        let ratio = |view: u32, engine: u32| {
            if engine == 0 {
                1.0
            } else {
                f64::from(view) / f64::from(engine)
            }
        };
        (ratio(self.view.0, self.engine.0), ratio(self.view.1, self.engine.1))
    }

    /// The rectangle at viewport cell (`column`, `row`) plus `offset` terminal pixels, `size` terminal pixels large.
    fn place(self, (column, row): (i64, i64), offset: (u32, u32), size: (u32, u32)) -> DeviceRect {
        let (sx, sy) = self.scale();
        #[expect(clippy::cast_precision_loss, reason = "viewport cells are far below 2^52")]
        let (column, row) = (column as f64, row as f64);
        DeviceRect {
            x: column * f64::from(self.view.0) + f64::from(offset.0) * sx,
            y: row * f64::from(self.view.1) + f64::from(offset.1) * sy,
            width: f64::from(size.0) * sx,
            height: f64::from(size.1) * sy,
        }
    }
}

/// The placements a view draws for `view` over the viewport `rows`, in drawing order (by `z`, then image id): the
/// positioned ones on the viewport, the runs of placeholder cells in `rows` and the relative placements rooted at the
/// virtual placements those runs show; placements without their image and empty ones are left out.
#[must_use]
pub fn placements(view: &KittyView, rows: &[Arc<Row>], cells: Cells) -> Vec<PlacementGeometry> {
    let mut drawn = positioned(view, cells);
    // a relative placement rooted at a virtual placement implies that virtual placement
    let runs = if view.placements.iter().any(|placement| placement.is_virtual) {
        virtual_placements(rows)
    } else {
        Vec::new()
    };
    drawn.extend(placeholders(view, &runs, cells));
    let columns = rows.first().map_or(0, |row| row.cells().len());
    drawn.extend(rooted_at_virtual(view, &runs, (columns, rows.len()), cells));
    // stable: placements of one image at one z keep their order
    drawn.sort_by(|a, b| a.z.cmp(&b.z).then(a.image.id.cmp(&b.image.id)));
    drawn
}

/// The placements with a position of their own.
fn positioned(view: &KittyView, cells: Cells) -> Vec<PlacementGeometry> {
    view.placements
        .iter()
        .filter(|placement| !placement.is_virtual && view.image(placement.image).is_some())
        .filter_map(|placement| {
            let rect = placement.rect?;
            let empty = rect.pixel_width == 0 || rect.pixel_height == 0;
            if empty || rect.source.width == 0 || rect.source.height == 0 {
                return None;
            }
            Some(PlacementGeometry {
                image: placement.image,
                layer: placement.layer,
                z: placement.z,
                dest: cells.place(
                    (i64::from(rect.column), i64::from(rect.row)),
                    cell_offset(placement, cells),
                    (rect.pixel_width, rect.pixel_height),
                ),
                source: rect.source,
            })
        })
        .collect()
}

/// Ghostty's `cellOffset`: the placement's offset inside its first cell, kept inside that cell.
fn cell_offset(placement: &KittyPlacement, cells: Cells) -> (u32, u32) {
    let (engine_width, engine_height) = cells.engine;
    (
        placement.x_offset.min(engine_width.saturating_sub(1)),
        placement.y_offset.min(engine_height.saturating_sub(1)),
    )
}

/// The runs of placeholder cells, each drawing its slice of its virtual placement's image (Ghostty's
/// `prepKittyVirtualPlacement`).
fn placeholders(view: &KittyView, runs: &[VirtualPlacement], cells: Cells) -> Vec<PlacementGeometry> {
    runs.iter()
        .filter_map(|run| {
            let target = placeholder::target(view, run)?;
            let image = view.image(target.image)?;
            let render = placeholder::render(run, target, (image.width, image.height), cells.engine)?;
            if render.width == 0 || render.height == 0 {
                return None;
            }
            Some(PlacementGeometry {
                image: target.image,
                layer: KittyLayer::BelowText,
                z: VIRTUAL_Z,
                dest: cells.place(
                    (i64::from(run.viewport_col), i64::from(run.viewport_row)),
                    (render.offset_x, render.offset_y),
                    (render.width, render.height),
                ),
                source: render.source,
            })
        })
        .collect()
}

/// The relative placements rooted at a virtual placement, each at the minimum column and the minimum row of the runs
/// showing its root plus its chain's offsets (Ghostty's `pending_relative` in `kittyUpdate`); one whose root shows no
/// run on the `viewport` (columns, rows), or that falls wholly outside it, is not drawn.
pub(super) fn rooted_at_virtual(
    view: &KittyView,
    runs: &[VirtualPlacement],
    viewport: (usize, usize),
    cells: Cells,
) -> Vec<PlacementGeometry> {
    if view.placements.iter().all(|placement| placement.virtual_root.is_none()) {
        return Vec::new();
    }
    let mut origins: HashMap<PlacementKey, (u16, u16)> = HashMap::new();
    for run in runs {
        let Some(target) = placeholder::target(view, run) else {
            continue;
        };
        origins
            .entry(target.key())
            .and_modify(|(column, row)| {
                *column = (*column).min(run.viewport_col);
                *row = (*row).min(run.viewport_row);
            })
            .or_insert((run.viewport_col, run.viewport_row));
    }
    let fits = |count: usize| i64::try_from(count).unwrap_or(i64::MAX);
    let (viewport_columns, viewport_rows) = (fits(viewport.0), fits(viewport.1));
    view.placements
        .iter()
        .filter_map(|placement| {
            let rooted = placement.virtual_root?;
            let &(column, row) = origins.get(&rooted.root)?;
            view.image(placement.image)?;
            let empty = rooted.columns == 0 || rooted.rows == 0 || rooted.pixel_width == 0 || rooted.pixel_height == 0;
            if empty || rooted.source.width == 0 || rooted.source.height == 0 {
                return None;
            }
            let column = i64::from(column) + i64::from(rooted.horizontal_offset);
            let row = i64::from(row) + i64::from(rooted.vertical_offset);
            let off_rows = row >= viewport_rows || row + i64::from(rooted.rows) <= 0;
            if off_rows || column >= viewport_columns || column + i64::from(rooted.columns) <= 0 {
                return None;
            }
            Some(PlacementGeometry {
                image: placement.image,
                layer: placement.layer,
                z: placement.z,
                dest: cells.place(
                    (column, row),
                    cell_offset(placement, cells),
                    (rooted.pixel_width, rooted.pixel_height),
                ),
                source: rooted.source,
            })
        })
        .collect()
}

/// The bounds to paint a whole image of `image` pixels at so that its `source` rectangle covers `dest` (GPUI's
/// `paint_image` then draws only the part of the tile inside `dest`).
#[must_use]
pub fn image_bounds(dest: Bounds<Pixels>, source: SourceRect, image: (u32, u32)) -> Bounds<Pixels> {
    let ratio = |dest: Pixels, source: u32| f32::from(dest) / narrow(f64::from(source.max(1)));
    let (rx, ry) = (
        ratio(dest.size.width, source.width),
        ratio(dest.size.height, source.height),
    );
    let origin = point(
        dest.origin.x - px(narrow(f64::from(source.x)) * rx),
        dest.origin.y - px(narrow(f64::from(source.y)) * ry),
    );
    Bounds::new(
        origin,
        size(px(narrow(f64::from(image.0)) * rx), px(narrow(f64::from(image.1)) * ry)),
    )
}

/// A device-pixel quantity as GPUI's `f32`.
#[expect(clippy::cast_possible_truncation, reason = "pixel lengths fit an f32")]
pub(super) fn narrow(value: f64) -> f32 {
    value as f32
}

/// The images a view painted into its window's sprite atlas, so it can drop the ones it stops drawing (module docs).
#[derive(Default)]
pub struct ViewImages {
    drawn: HashMap<ImageId, Arc<RenderImage>>,
    release: Option<Subscription>,
}

impl ViewImages {
    /// Records what this frame draws and drops from `window`'s atlas what the last frame drew and this one does not.
    pub fn retain(&mut self, window: &mut Window, used: impl IntoIterator<Item = Arc<RenderImage>>) {
        let used: HashMap<ImageId, Arc<RenderImage>> = used.into_iter().map(|image| (image.id, image)).collect();
        for (id, image) in std::mem::take(&mut self.drawn) {
            if !used.contains_key(&id) {
                drop_from_atlas(window, image);
            }
        }
        self.drawn = used;
    }

    /// Drops everything from `window`'s atlas: the view is gone, or stopped drawing.
    pub fn release(&mut self, window: &mut Window) {
        for (_, image) in std::mem::take(&mut self.drawn) {
            drop_from_atlas(window, image);
        }
    }

    /// Whether the view registered its release hook (once, at its first paint in a window).
    #[must_use]
    pub fn has_release_hook(&self) -> bool {
        self.release.is_some()
    }

    pub fn set_release_hook(&mut self, subscription: Subscription) {
        self.release = Some(subscription);
    }

    /// The images the last frame drew (tests).
    #[must_use]
    pub fn drawn(&self) -> Vec<Arc<RenderImage>> {
        self.drawn.values().cloned().collect()
    }
}

fn drop_from_atlas(window: &mut Window, image: Arc<RenderImage>) {
    if let Err(err) = window.drop_image(image) {
        tracing::debug!(%err, "an image could not be dropped from the atlas");
    }
}

/// What paint needs to draw the placements of one frame.
#[derive(Default)]
pub struct KittyPaint {
    pub placements: Vec<(PlacementGeometry, Texture)>,
}

impl KittyPaint {
    /// The frame's placements whose textures are ready.
    #[must_use]
    pub fn new(frame: &Frame, cells: Cells, store: &KittyStore) -> Self {
        let placements = placements(&frame.kitty, &frame.rows, cells)
            .into_iter()
            .filter_map(|geometry| Some((geometry, store.get(geometry.image)?.clone())))
            .collect();
        Self { placements }
    }

    /// Every texture drawn.
    pub fn images(&self) -> impl Iterator<Item = Arc<RenderImage>> + '_ {
        self.placements.iter().map(|(_, texture)| Arc::clone(&texture.image))
    }

    /// Paints the placements of `layer`, the grid's top-left corner at `origin`, at `scale` device pixels per
    /// logical pixel (clipped by the element's content mask); how many were painted.
    pub fn paint(&self, window: &mut Window, layer: KittyLayer, origin: gpui_kit::Point<Pixels>, scale: f32) -> usize {
        let logical = |device: f64| px(narrow(device / f64::from(scale)));
        let mut painted = 0;
        for (geometry, texture) in self.placements.iter().filter(|(geometry, _)| geometry.layer == layer) {
            let dest = Bounds::new(
                point(origin.x + logical(geometry.dest.x), origin.y + logical(geometry.dest.y)),
                size(logical(geometry.dest.width), logical(geometry.dest.height)),
            );
            let whole = image_bounds(dest, geometry.source, texture.size);
            let image = Arc::clone(&texture.image);
            match window.paint_image(dest, whole, Corners::default(), image, 0, false) {
                Ok(()) => painted += 1,
                Err(err) => tracing::debug!(%err, id = geometry.image.id, "a Kitty placement was not painted"),
            }
        }
        painted
    }
}

/// The image layers of one paint: the Kitty placements and the background image.
#[derive(Default)]
pub struct Images {
    pub kitty: KittyPaint,
    pub background: Option<BackgroundPaint>,
}

impl TerminalView {
    /// Prepares this paint's image layers from `frame` at the view's `metrics`: has the terminal convert new Kitty
    /// images, asks it for its background image, and drops from the window's atlas what the view no longer draws.
    pub(super) fn prepare_images(
        &mut self,
        frame: Option<&Frame>,
        metrics: &CellMetrics,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Images {
        if !self.images.has_release_hook() {
            let hook = cx.on_release_in(window, |view: &mut Self, window, _| view.images.release(window));
            self.images.set_release_hook(hook);
        }
        let kitty = frame.map_or_else(KittyPaint::default, |frame| {
            self.shared.follow_kitty(frame, cx);
            let view = (metrics.cell_width, metrics.cell_height);
            // the frame's pixels are the terminal's: the cell the owner laid the pane out with
            let engine = self
                .shared
                .laid
                .get()
                .map_or(view, |laid| (laid.grid.cell_width, laid.grid.cell_height));
            KittyPaint::new(frame, Cells { engine, view }, &self.shared.kitty.borrow())
        });
        let config = self.shared.config();
        let color = frame.map_or(config.theme.background, |frame| frame.colors.background);
        let spec = Spec::from_config(&config.background_image, config.palette.background_alpha, color);
        let background = self
            .shared
            .background_image(spec.clone(), cx)
            .zip(spec)
            .map(|(decoded, spec)| BackgroundPaint { decoded, spec });
        let used: Vec<Arc<RenderImage>> = kitty
            .images()
            .chain(background.iter().map(|paint| Arc::clone(&paint.decoded.texture)))
            .collect();
        self.images.retain(window, used);
        Images { kitty, background }
    }

    /// The images this view draws and keeps in its window's atlas (tests).
    #[must_use]
    pub fn images(&self) -> &ViewImages {
        &self.images
    }

    /// Drops everything this view put into `window`'s atlas (teardown).
    pub(super) fn release_images(&mut self, window: &mut Window) {
        self.images.release(window);
    }
}
