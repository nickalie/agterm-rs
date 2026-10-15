//! The Kitty graphics a frame shows ([`KittyView`]), in this crate's own types.
//!
//! The view is captured with the frame, under the VT lock (the library's image handles die on the terminal's next
//! mutation): the placements and where they land on the viewport every time, the pixels of an image only when its
//! `(id, generation)` is new — an unchanged image is shared with the previous view.

use std::sync::Arc;

use agterm_ghostty_vt as vt;

/// An image's pixel layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KittyFormat {
    Rgb,
    Rgba,
    Gray,
    GrayAlpha,
}

/// What identifies an image's pixels: a new transmission under the same id gets a new generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyImageKey {
    pub id: u32,
    pub generation: u64,
}

/// An image a placement shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImage {
    pub key: KittyImageKey,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub format: KittyFormat,
    pub pixels: Arc<[u8]>,
}

/// Where a placement is drawn relative to the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KittyLayer {
    BelowBackground,
    BelowText,
    AboveText,
}

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SourceRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Where a placement lands on the viewport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PlacementRect {
    /// The top-left cell; negative when the placement scrolled partly off the top (or left).
    pub column: i32,
    pub row: i32,
    /// The cells it covers.
    pub columns: u32,
    pub rows: u32,
    /// The drawn size in pixels.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// The part of the image drawn.
    pub source: SourceRect,
}

/// One placement of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyPlacement {
    pub image: KittyImageKey,
    pub placement_id: u32,
    pub layer: KittyLayer,
    pub z: i32,
    /// Shown through Unicode placeholder cells rather than at a position of its own.
    pub is_virtual: bool,
    /// The pixel offset inside the top-left cell.
    pub x_offset: u32,
    pub y_offset: u32,
    /// The cells asked for (`c=`, `r=`; 0: from the image's size): a virtual placement's grid, which its placeholder
    /// cells ([`crate::virtual_placements`]) show parts of.
    pub columns: u32,
    pub rows: u32,
    /// `None` when off the viewport (or virtual).
    pub rect: Option<PlacementRect>,
}

/// The Kitty graphics of a frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KittyView {
    /// The storage's generation (0: never written).
    pub generation: u64,
    /// Every image a placement shows, once.
    pub images: Vec<Arc<KittyImage>>,
    /// In storage order; draw each layer's in ascending `z`.
    pub placements: Vec<KittyPlacement>,
    /// The images the previous frame's view had and this one has not: their textures can go. A view that skipped
    /// frames compares against what it drew instead.
    pub removed: Vec<KittyImageKey>,
}

impl KittyView {
    #[must_use]
    pub fn image(&self, key: KittyImageKey) -> Option<&Arc<KittyImage>> {
        self.images.iter().find(|image| image.key == key)
    }

    fn is_empty(&self) -> bool {
        self.images.is_empty() && self.placements.is_empty()
    }
}

/// Capture the Kitty graphics of `graphics` (a terminal's storage, or the one a synchronized-output hold is captured
/// from) after `previous`; `None` when nothing changed (draw `previous`).
pub(crate) fn capture(graphics: Option<vt::KittyGraphics<'_>>, previous: &KittyView) -> Option<KittyView> {
    let Some(graphics) = graphics else {
        return (!previous.is_empty()).then(|| removed_all(previous));
    };
    let generation = graphics.generation();
    if generation == 0 && previous.is_empty() {
        return None;
    }
    let placements = match graphics.placements() {
        Ok(placements) => placements,
        Err(err) => {
            tracing::warn!(%err, "reading the Kitty placements failed");
            return None;
        }
    };
    let mut images: Vec<Arc<KittyImage>> = Vec::new();
    let mut converted = Vec::with_capacity(placements.len());
    for placement in placements {
        let Some(image) = graphics.image(placement.image_id) else {
            continue;
        };
        let key = KittyImageKey {
            id: image.id,
            generation: image.generation,
        };
        if images.iter().all(|known| known.key != key) {
            match previous.image(key) {
                Some(kept) => images.push(Arc::clone(kept)),
                // a pending payload has no pixels yet: its placements wait for them
                None => match copy(&image, key) {
                    Some(copied) => images.push(Arc::new(copied)),
                    None => continue,
                },
            }
        }
        converted.push(convert(&placement, key));
    }
    if generation == previous.generation && converted == previous.placements {
        return None;
    }
    let removed = previous
        .images
        .iter()
        .map(|image| image.key)
        .filter(|key| images.iter().all(|image| image.key != *key))
        .collect();
    Some(KittyView {
        generation,
        images,
        placements: converted,
        removed,
    })
}

fn removed_all(previous: &KittyView) -> KittyView {
    KittyView {
        removed: previous.images.iter().map(|image| image.key).collect(),
        ..KittyView::default()
    }
}

/// The image's pixels copied out of the terminal; `None` while they are pending.
fn copy(image: &vt::KittyImage<'_>, key: KittyImageKey) -> Option<KittyImage> {
    Some(KittyImage {
        key,
        number: image.number,
        width: image.width,
        height: image.height,
        format: match image.format {
            vt::KittyFormat::Rgb => KittyFormat::Rgb,
            vt::KittyFormat::Rgba => KittyFormat::Rgba,
            vt::KittyFormat::Gray => KittyFormat::Gray,
            vt::KittyFormat::GrayAlpha => KittyFormat::GrayAlpha,
        },
        pixels: Arc::from(image.pixels?),
    })
}

fn convert(placement: &vt::KittyPlacement, image: KittyImageKey) -> KittyPlacement {
    KittyPlacement {
        image,
        placement_id: placement.placement_id,
        layer: match vt::KittyLayer::of(placement.z) {
            vt::KittyLayer::BelowBackground => KittyLayer::BelowBackground,
            vt::KittyLayer::BelowText => KittyLayer::BelowText,
            vt::KittyLayer::AboveText => KittyLayer::AboveText,
        },
        z: placement.z,
        is_virtual: placement.is_virtual,
        x_offset: placement.x_offset,
        y_offset: placement.y_offset,
        columns: placement.columns,
        rows: placement.rows,
        rect: placement.render.map(|render| PlacementRect {
            column: render.viewport_col,
            row: render.viewport_row,
            columns: render.grid_cols,
            rows: render.grid_rows,
            pixel_width: render.pixel_width,
            pixel_height: render.pixel_height,
            source: SourceRect {
                x: render.source.x,
                y: render.source.y,
                width: render.source.width,
                height: render.source.height,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x1 RGBA image, sent in two chunks and placed (`a=T`).
    const CHUNKED: &[u8] = b"\x1b_Ga=T,f=32,s=2,v=1,i=7,m=1;AQIDBAUG\x1b\\\x1b_Gm=0;Bwg=\x1b\\";

    fn terminal() -> vt::Terminal {
        crate::init();
        let mut terminal = vt::Terminal::new(20, 5).unwrap();
        terminal.resize(20, 5, 10, 20).unwrap();
        terminal.set_kitty_image_storage_limit(10_000_000).unwrap();
        terminal
    }

    #[test]
    fn nothing_stored_makes_no_view() {
        let mut terminal = terminal();
        assert!(capture(terminal.kitty_graphics(), &KittyView::default()).is_none());
    }

    #[test]
    fn an_image_is_copied_once_and_shared_while_it_stays() {
        let mut terminal = terminal();
        terminal.vt_write(CHUNKED);
        let first = capture(terminal.kitty_graphics(), &KittyView::default()).unwrap();
        assert_eq!(first.images.len(), 1);
        let image = &first.images[0];
        assert_eq!((image.key.id, image.width, image.height), (7, 2, 1));
        assert_eq!(&*image.pixels, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let placement = first.placements[0];
        assert_eq!(placement.image, image.key);
        assert_eq!(placement.layer, KittyLayer::AboveText);
        assert_eq!(placement.rect.map(|rect| (rect.column, rect.row)), Some((0, 0)));
        assert!(capture(terminal.kitty_graphics(), &first).is_none(), "unchanged");
        // a scroll moves the placement but keeps the pixels
        terminal.vt_write(b"\r\n\r\n\r\n\r\n\r\n");
        let moved = capture(terminal.kitty_graphics(), &first).unwrap();
        assert!(Arc::ptr_eq(&moved.images[0], &first.images[0]));
        assert_ne!(moved.placements[0].rect, first.placements[0].rect);
        assert!(moved.removed.is_empty());
    }

    #[test]
    fn a_deleted_image_is_reported_removed() {
        let mut terminal = terminal();
        terminal.vt_write(CHUNKED);
        let first = capture(terminal.kitty_graphics(), &KittyView::default()).unwrap();
        terminal.vt_write(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        let after = capture(terminal.kitty_graphics(), &first).unwrap();
        assert!(after.images.is_empty() && after.placements.is_empty());
        assert_eq!(after.removed, [first.images[0].key]);
    }
}
