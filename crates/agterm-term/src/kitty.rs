//! The Kitty graphics a frame shows ([`KittyView`]), in this crate's own types.
//!
//! The view is captured with the frame, under the VT lock (the library's image handles die on the terminal's next
//! mutation): the placements and where they land on the viewport every time, the pixels of an image only when its
//! `(id, generation)` is new — an unchanged image is shared with the previous view. A new image's pixels are copied
//! on the pane's IO thread after the batch that completed it ([`Prefetch`]), so the paint that captures it (on the
//! frontend's main thread) only shares them; only an image a paint catches in the middle of a batch is copied there.
//!
//! A relative placement (`P=`) whose chain roots at a virtual placement has no position the library can resolve; its
//! chain is resolved here ([`VirtualRooted`]) from the parent links libghostty-vt reports, as Ghostty's renderer
//! resolves it: `ImageStorage.resolveChain` and `parent_chain_limit` (`src/terminal/kitty/graphics_storage.zig` at
//! `LIBGHOSTTY_VT_REV`, <https://github.com/ghostty-org/ghostty>, MIT, Copyright (c) 2024 Mitchell Hashimoto, Ghostty
//! contributors), checked against rustty's port of it (`resolve_chain`, `crates/rustty-vt/src/kitty/
//! graphics_storage.rs`, <https://github.com/AirelyDev/rustty>, MIT, Copyright (c) 2026 Airely, Gael Hatchue, and
//! rustty contributors).

use std::collections::HashMap;
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

/// The namespace of a placement id: a terminal-assigned and a client's placement of one image can share a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum PlacementNamespace {
    /// Assigned by the terminal to a placement created without `p=`.
    #[default]
    Internal,
    /// The client's `p=`: the ids Unicode placeholders and `Q=` name.
    External,
}

/// What identifies a placement in the terminal's storage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PlacementKey {
    pub image_id: u32,
    pub placement_id: u32,
    pub namespace: PlacementNamespace,
}

/// A relative placement (`P=`/`Q=`) whose parent chain roots at a virtual placement: it has no position of its own
/// and is drawn at the minimum column and the minimum row of the placeholder cells showing `root`, plus the chain's
/// offsets (Ghostty's `pending_relative`); with none of those cells on the viewport it is not drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VirtualRooted {
    pub root: PlacementKey,
    /// Cells from the root's origin, summed over the chain (`H=`, `V=`).
    pub horizontal_offset: i32,
    pub vertical_offset: i32,
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
    /// The namespace of `placement_id`.
    pub namespace: PlacementNamespace,
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
    /// `None` when off the viewport, virtual or rooted at a virtual placement.
    pub rect: Option<PlacementRect>,
    /// Set for a relative placement rooted at a virtual placement.
    pub virtual_root: Option<VirtualRooted>,
}

impl KittyPlacement {
    #[must_use]
    pub fn key(&self) -> PlacementKey {
        PlacementKey {
            image_id: self.image.id,
            placement_id: self.placement_id,
            namespace: self.namespace,
        }
    }
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

/// The pixels of the placed images, copied on the pane's IO thread after each batch that changed the storage (kept
/// with the terminal, under the VT lock): what a capture shares instead of copying on the thread that paints.
#[derive(Debug, Default)]
pub(crate) struct Prefetch {
    generation: u64,
    images: HashMap<KittyImageKey, Arc<KittyImage>>,
}

impl Prefetch {
    /// Copies the placed images it does not hold yet and forgets the ones no longer placed, when the storage changed.
    pub(crate) fn update(&mut self, graphics: Option<vt::KittyGraphics<'_>>) {
        let Some(graphics) = graphics else {
            *self = Self::default();
            return;
        };
        let generation = graphics.generation();
        if generation == self.generation {
            return;
        }
        let Ok(placements) = graphics.placements() else {
            return;
        };
        self.generation = generation;
        let mut kept = HashMap::new();
        for placement in placements {
            let Some(image) = graphics.image(placement.image_id) else {
                continue;
            };
            let key = KittyImageKey {
                id: image.id,
                generation: image.generation,
            };
            if kept.contains_key(&key) {
                continue;
            }
            // a pending payload has no pixels yet: the batch that completes it changes the storage again
            if let Some(copied) = self.images.remove(&key).or_else(|| copy(&image, key).map(Arc::new)) {
                kept.insert(key, copied);
            }
        }
        self.images = kept;
    }

    fn get(&self, key: KittyImageKey) -> Option<&Arc<KittyImage>> {
        self.images.get(&key)
    }
}

/// Capture the Kitty graphics of `graphics` (a terminal's storage, or the one a synchronized-output hold is captured
/// from) after `previous`, sharing the pixels `prefetched` holds; `None` when nothing changed (draw `previous`).
pub(crate) fn capture(
    graphics: Option<vt::KittyGraphics<'_>>,
    previous: &KittyView,
    prefetched: &Prefetch,
) -> Option<KittyView> {
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
    let by_key: HashMap<vt::KittyPlacementKey, &vt::KittyPlacement> = placements
        .iter()
        .map(|placement| (placement.key(), placement))
        .collect();
    let mut images: Vec<Arc<KittyImage>> = Vec::new();
    let mut converted = Vec::with_capacity(placements.len());
    for placement in &placements {
        let Some(image) = graphics.image(placement.image_id) else {
            continue;
        };
        let key = KittyImageKey {
            id: image.id,
            generation: image.generation,
        };
        if images.iter().all(|known| known.key != key) {
            match previous.image(key).or_else(|| prefetched.get(key)) {
                Some(kept) => images.push(Arc::clone(kept)),
                // a pending payload has no pixels yet: its placements wait for them
                None => match copy(&image, key) {
                    Some(copied) => images.push(Arc::new(copied)),
                    None => continue,
                },
            }
        }
        converted.push(convert(placement, key, &by_key));
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

fn namespace(namespace: vt::KittyIdNamespace) -> PlacementNamespace {
    match namespace {
        vt::KittyIdNamespace::Internal => PlacementNamespace::Internal,
        vt::KittyIdNamespace::External => PlacementNamespace::External,
    }
}

fn source(source: vt::PixelRect) -> SourceRect {
    SourceRect {
        x: source.x,
        y: source.y,
        width: source.width,
        height: source.height,
    }
}

/// Ghostty's `ImageStorage.parent_chain_limit`: the most parent links a chain may have.
const PARENT_CHAIN_LIMIT: usize = 8;

/// A relative placement's parent chain resolved to its root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolvedChain<'a> {
    /// The chain's root: a placement with a position of its own or a virtual one, never a relative one.
    root: &'a vt::KittyPlacement,
    /// Cells from the root's origin, summed over every link (saturating).
    horizontal_offset: i32,
    vertical_offset: i32,
}

/// Follows `parent` up to the chain's root, summing each link's offsets (Ghostty's `ImageStorage.resolveChain`);
/// `None` when a link names a placement storage does not hold or the chain has more than [`PARENT_CHAIN_LIMIT`]
/// links (which also ends a cycle).
fn resolve_chain<'a>(
    parent: vt::KittyParent,
    placements: &HashMap<vt::KittyPlacementKey, &'a vt::KittyPlacement>,
) -> Option<ResolvedChain<'a>> {
    let mut horizontal = parent.horizontal_offset;
    let mut vertical = parent.vertical_offset;
    let mut key = parent.key;
    let mut depth = 1;
    loop {
        let placement = *placements.get(&key)?;
        let Some(link) = placement.parent else {
            return Some(ResolvedChain {
                root: placement,
                horizontal_offset: horizontal,
                vertical_offset: vertical,
            });
        };
        if depth >= PARENT_CHAIN_LIMIT {
            return None;
        }
        depth += 1;
        horizontal = horizontal.saturating_add(link.horizontal_offset);
        vertical = vertical.saturating_add(link.vertical_offset);
        key = link.key;
    }
}

/// The placement's chain when it roots at a virtual placement (Ghostty's `pending_relative`), with the size and
/// source the library resolved; a chain rooted at a positioned placement is positioned by the library itself.
fn virtual_root(
    placement: &vt::KittyPlacement,
    placements: &HashMap<vt::KittyPlacementKey, &vt::KittyPlacement>,
) -> Option<VirtualRooted> {
    let chain = resolve_chain(placement.parent?, placements)?;
    if !chain.root.is_virtual {
        return None;
    }
    let render = placement.render?;
    Some(VirtualRooted {
        root: key(chain.root.key()),
        horizontal_offset: chain.horizontal_offset,
        vertical_offset: chain.vertical_offset,
        columns: render.grid_cols,
        rows: render.grid_rows,
        pixel_width: render.pixel_width,
        pixel_height: render.pixel_height,
        source: source(render.source),
    })
}

fn key(key: vt::KittyPlacementKey) -> PlacementKey {
    PlacementKey {
        image_id: key.image_id,
        placement_id: key.placement_id,
        namespace: namespace(key.namespace),
    }
}

fn convert(
    placement: &vt::KittyPlacement,
    image: KittyImageKey,
    placements: &HashMap<vt::KittyPlacementKey, &vt::KittyPlacement>,
) -> KittyPlacement {
    KittyPlacement {
        image,
        placement_id: placement.placement_id,
        namespace: namespace(placement.namespace),
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
        rect: placement
            .render
            .filter(|render| render.visible)
            .map(|render| PlacementRect {
                column: render.viewport_col,
                row: render.viewport_row,
                columns: render.grid_cols,
                rows: render.grid_rows,
                pixel_width: render.pixel_width,
                pixel_height: render.pixel_height,
                source: source(render.source),
            }),
        virtual_root: virtual_root(placement, placements),
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
        assert!(capture(terminal.kitty_graphics(), &KittyView::default(), &Prefetch::default()).is_none());
    }

    #[test]
    fn an_image_is_copied_once_and_shared_while_it_stays() {
        let mut terminal = terminal();
        terminal.vt_write(CHUNKED);
        let first = capture(terminal.kitty_graphics(), &KittyView::default(), &Prefetch::default()).unwrap();
        assert_eq!(first.images.len(), 1);
        let image = &first.images[0];
        assert_eq!((image.key.id, image.width, image.height), (7, 2, 1));
        assert_eq!(&*image.pixels, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let placement = first.placements[0];
        assert_eq!(placement.image, image.key);
        assert_eq!(placement.layer, KittyLayer::AboveText);
        assert_eq!(placement.rect.map(|rect| (rect.column, rect.row)), Some((0, 0)));
        assert!(
            capture(terminal.kitty_graphics(), &first, &Prefetch::default()).is_none(),
            "unchanged"
        );
        // a scroll moves the placement but keeps the pixels
        terminal.vt_write(b"\r\n\r\n\r\n\r\n\r\n");
        let moved = capture(terminal.kitty_graphics(), &first, &Prefetch::default()).unwrap();
        assert!(Arc::ptr_eq(&moved.images[0], &first.images[0]));
        assert_ne!(moved.placements[0].rect, first.placements[0].rect);
        assert!(moved.removed.is_empty());
    }

    #[test]
    fn a_capture_shares_the_pixels_the_io_thread_copied() {
        let mut terminal = terminal();
        terminal.vt_write(CHUNKED);
        let mut prefetched = Prefetch::default();
        prefetched.update(terminal.kitty_graphics());
        assert_eq!(prefetched.images.len(), 1);
        let view = capture(terminal.kitty_graphics(), &KittyView::default(), &prefetched).unwrap();
        let copied = Arc::clone(prefetched.get(view.images[0].key).unwrap());
        assert!(Arc::ptr_eq(&view.images[0], &copied), "shared, not copied again");
        // an unchanged storage copies nothing; a deleted image is let go
        prefetched.update(terminal.kitty_graphics());
        assert!(Arc::ptr_eq(prefetched.get(view.images[0].key).unwrap(), &copied));
        terminal.vt_write(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        prefetched.update(terminal.kitty_graphics());
        assert!(prefetched.images.is_empty());
    }

    #[test]
    fn a_view_keeps_namespaces_and_the_chains_rooted_at_virtual_placements() {
        let mut terminal = terminal();
        // image 1 placed virtually under the client's id 1 and positioned under a terminal-assigned id; image 2
        // relative to the virtual one at (2, -1), two cells wide; image 3 relative to the positioned one
        terminal.vt_write(b"\x1b_Ga=T,U=1,q=2,f=24,s=1,v=1,i=1,p=1;AAAA\x1b\\\x1b_Ga=p,q=2,i=1,C=1\x1b\\");
        terminal.vt_write(b"\x1b_Ga=t,q=2,f=24,s=1,v=1,i=2;AAAA\x1b\\\x1b_Ga=p,q=2,i=2,P=1,Q=1,H=2,V=-1,c=2,r=1\x1b\\");
        terminal.vt_write(b"\x1b_Ga=t,q=2,f=24,s=1,v=1,i=3;AAAA\x1b\\\x1b_Ga=p,q=2,i=3,P=1,H=1,C=1\x1b\\");
        let view = capture(terminal.kitty_graphics(), &KittyView::default(), &Prefetch::default()).unwrap();
        let of_image = |id: u32| -> Vec<KittyPlacement> {
            let mut found: Vec<KittyPlacement> = view
                .placements
                .iter()
                .filter(|placement| placement.image.id == id)
                .copied()
                .collect();
            found.sort_by_key(|placement| placement.namespace == PlacementNamespace::External);
            found
        };
        let first = of_image(1);
        assert_eq!(
            first
                .iter()
                .map(|placement| (placement.namespace, placement.is_virtual))
                .collect::<Vec<_>>(),
            [
                (PlacementNamespace::Internal, false),
                (PlacementNamespace::External, true)
            ]
        );
        let child = of_image(2)[0];
        assert_eq!(child.rect, None, "no position of its own");
        assert_eq!(
            child.virtual_root,
            Some(VirtualRooted {
                root: first[1].key(),
                horizontal_offset: 2,
                vertical_offset: -1,
                columns: 2,
                rows: 1,
                pixel_width: 20,
                pixel_height: 20,
                source: SourceRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
            })
        );
        // P= without Q= picks the client's placement over the terminal's: rooted at the virtual one too
        let other = of_image(3)[0];
        assert_eq!(other.virtual_root.map(|rooted| rooted.root), Some(first[1].key()));
        assert_eq!(other.rect, None);
    }

    #[test]
    fn a_chain_sums_its_links_to_its_root_and_a_pinned_root_is_the_librarys() {
        let mut terminal = terminal();
        // image 1 virtual (client's 1); image 2 relative to it at (2, -1) under the client's 1; image 3 relative to
        // image 2 at (1, 3); image 4 on a pin (client's 2) and image 5 relative to it at (3, 1)
        terminal.vt_write(b"\x1b_Ga=T,U=1,q=2,f=24,s=1,v=1,i=1,p=1;AAAA\x1b\\");
        terminal.vt_write(b"\x1b_Ga=t,q=2,f=24,s=1,v=1,i=2;AAAA\x1b\\\x1b_Ga=p,q=2,i=2,p=1,P=1,Q=1,H=2,V=-1,C=1\x1b\\");
        terminal.vt_write(b"\x1b_Ga=t,q=2,f=24,s=1,v=1,i=3;AAAA\x1b\\\x1b_Ga=p,q=2,i=3,P=2,Q=1,H=1,V=3,C=1\x1b\\");
        terminal.vt_write(b"\x1b_Ga=T,q=2,f=24,s=1,v=1,i=4,p=2,C=1;AAAA\x1b\\");
        terminal.vt_write(b"\x1b_Ga=t,q=2,f=24,s=1,v=1,i=5;AAAA\x1b\\\x1b_Ga=p,q=2,i=5,P=4,H=3,V=1,C=1\x1b\\");
        let view = capture(terminal.kitty_graphics(), &KittyView::default(), &Prefetch::default()).unwrap();
        let only = |id: u32| {
            let found: Vec<&KittyPlacement> = view.placements.iter().filter(|p| p.image.id == id).collect();
            assert_eq!(found.len(), 1, "image {id}");
            *found[0]
        };
        let root = only(1).key();
        let grandchild = only(3).virtual_root.unwrap();
        assert_eq!(
            (
                grandchild.root,
                grandchild.horizontal_offset,
                grandchild.vertical_offset
            ),
            (root, 3, 2)
        );
        let pinned = only(5);
        assert_eq!(pinned.virtual_root, None);
        assert_eq!(pinned.rect.map(|rect| (rect.column, rect.row)), Some((3, 1)));
    }

    /// A one-cell placement of image `image_id` under the client's `placement_id`, relative to `parent` at `offset`
    /// (`None`: on a pin).
    fn link(image_id: u32, placement_id: u32, parent: Option<(u32, (i32, i32))>) -> vt::KittyPlacement {
        vt::KittyPlacement {
            image_id,
            placement_id,
            namespace: vt::KittyIdNamespace::External,
            is_virtual: false,
            z: 0,
            x_offset: 0,
            y_offset: 0,
            columns: 1,
            rows: 1,
            render: None,
            parent: parent.map(|(parent_image, (horizontal_offset, vertical_offset))| vt::KittyParent {
                key: vt::KittyPlacementKey {
                    image_id: parent_image,
                    placement_id: 1,
                    namespace: vt::KittyIdNamespace::External,
                },
                horizontal_offset,
                vertical_offset,
            }),
        }
    }

    /// Images `1..=links` each relative to the one before at (1, -1), image 0 the root.
    fn chain_of(links: u32) -> Vec<vt::KittyPlacement> {
        std::iter::once(link(0, 1, None))
            .chain((1..=links).map(|image| link(image, 1, Some((image - 1, (1, -1))))))
            .collect()
    }

    fn resolved(placements: &[vt::KittyPlacement], image_id: u32) -> Option<(u32, i32, i32)> {
        let by_key: HashMap<vt::KittyPlacementKey, &vt::KittyPlacement> = placements
            .iter()
            .map(|placement| (placement.key(), placement))
            .collect();
        let placement = placements.iter().find(|placement| placement.image_id == image_id)?;
        resolve_chain(placement.parent?, &by_key)
            .map(|chain| (chain.root.image_id, chain.horizontal_offset, chain.vertical_offset))
    }

    #[test]
    fn a_chain_resolves_up_to_ghosttys_link_limit() {
        // Ghostty's parent_chain_limit: eight links resolve, a ninth does not (as for a chain an ancestor's
        // replacement deepened after the fact)
        assert_eq!(resolved(&chain_of(8), 8), Some((0, 8, -8)));
        assert_eq!(resolved(&chain_of(9), 9), None);
        assert_eq!(resolved(&chain_of(9), 8), Some((0, 8, -8)));
    }

    #[test]
    fn a_broken_or_cyclic_chain_does_not_resolve_and_offsets_saturate() {
        // a link to a placement storage does not hold
        assert_eq!(resolved(&[link(1, 1, Some((7, (0, 0))))], 1), None);
        // a cycle ends at the link limit
        let cycle = [link(1, 1, Some((2, (1, 0)))), link(2, 1, Some((1, (1, 0))))];
        assert_eq!(resolved(&cycle, 1), None);
        // a namespace is part of the key: a terminal-numbered placement 1 is not the client's 1
        let internal = vt::KittyPlacement {
            namespace: vt::KittyIdNamespace::Internal,
            ..link(0, 1, None)
        };
        assert_eq!(resolved(&[internal, link(1, 1, Some((0, (0, 0))))], 1), None);
        let far = [
            link(0, 1, None),
            link(1, 1, Some((0, (i32::MAX, i32::MIN)))),
            link(2, 1, Some((1, (5, -5)))),
        ];
        assert_eq!(resolved(&far, 2), Some((0, i32::MAX, i32::MIN)));
    }

    #[test]
    fn a_deleted_image_is_reported_removed() {
        let mut terminal = terminal();
        terminal.vt_write(CHUNKED);
        let first = capture(terminal.kitty_graphics(), &KittyView::default(), &Prefetch::default()).unwrap();
        terminal.vt_write(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        let after = capture(terminal.kitty_graphics(), &first, &Prefetch::default()).unwrap();
        assert!(after.images.is_empty() && after.placements.is_empty());
        assert_eq!(after.removed, [first.images[0].key]);
    }
}
