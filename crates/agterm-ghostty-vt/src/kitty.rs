//! Kitty graphics (`kitty_graphics.h`): the images and placements stored on the terminal's active screen.
//!
//! Every handle the library lends here dies on the terminal's next mutating call, so [`KittyGraphics`] borrows the
//! terminal mutably for its whole life: nothing can write the terminal while placements are read or an image's pixels
//! are borrowed.
//!
//! A placement's id namespace and a relative placement's parent link come from this build's libghostty-vt patch
//! (`build/patches/ghostty-vt/0001-kitty-placement-namespace-and-parent.patch`): the C API at the pin reports neither.
//! They are read out as storage holds them; resolving a chain is the caller's (`agterm-term`).

use std::marker::PhantomData;

use crate::{Error, Handle, HeldTerminal, Terminal, ffi};

/// A stored image's pixel layout (always decoded and decompressed: never PNG).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KittyFormat {
    Rgb,
    Rgba,
    Gray,
    GrayAlpha,
}

impl KittyFormat {
    fn from_raw(raw: ffi::GhosttyKittyImageFormat) -> Option<Self> {
        match raw {
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_RGB => Some(Self::Rgb),
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => Some(Self::Rgba),
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => Some(Self::Gray),
            ffi::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => Some(Self::GrayAlpha),
            _ => None,
        }
    }
}

/// Where a placement is drawn relative to the text, from its z-index (the protocol's conventions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KittyLayer {
    /// `z < i32::MIN / 2`: under the cell backgrounds.
    BelowBackground,
    /// `i32::MIN / 2 <= z < 0`: over the backgrounds, under the text.
    BelowText,
    /// `z >= 0`.
    AboveText,
}

impl KittyLayer {
    #[must_use]
    pub fn of(z: i32) -> Self {
        if z < i32::MIN / 2 {
            Self::BelowBackground
        } else if z < 0 {
            Self::BelowText
        } else {
            Self::AboveText
        }
    }
}

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// The namespace of a placement id: a terminal-assigned and a client's placement of one image can share a number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum KittyIdNamespace {
    /// Assigned by the terminal to a placement created without `p=`.
    #[default]
    Internal,
    /// The client's `p=`: the ids Unicode placeholders and `Q=` name.
    External,
}

impl KittyIdNamespace {
    fn from_raw(raw: ffi::GhosttyKittyPlacementIdNamespace) -> Self {
        if raw == ffi::GHOSTTY_KITTY_PLACEMENT_ID_NAMESPACE_EXTERNAL {
            Self::External
        } else {
            Self::Internal
        }
    }
}

/// The key storage holds a placement under.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KittyPlacementKey {
    pub image_id: u32,
    pub placement_id: u32,
    pub namespace: KittyIdNamespace,
}

/// A relative placement's (`P=`/`Q=`) link to its parent, as storage holds it (Ghostty's `Placement.Relative`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyParent {
    /// The parent placement: `Q=`, or the one storage picked when the client named none.
    pub key: KittyPlacementKey,
    /// Cells from the parent's origin (`H=`, `V=`).
    pub horizontal_offset: i32,
    pub vertical_offset: i32,
}

/// What the library resolves of a placement's drawing: its size and source always, its viewport cell when visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct KittyRenderInfo {
    /// The drawn size in pixels.
    pub pixel_width: u32,
    pub pixel_height: u32,
    /// The cells it covers.
    pub grid_cols: u32,
    pub grid_rows: u32,
    /// The top-left cell on the viewport; negative when it scrolled partly off the top or left. Meaningful only when
    /// `visible`.
    pub viewport_col: i32,
    pub viewport_row: i32,
    /// Whether it lands on the viewport: false off it, for a virtual placement and for a relative placement whose
    /// chain roots at a virtual one (which has no position of its own) or does not resolve.
    pub visible: bool,
    /// The part of the image drawn, clamped to its bounds.
    pub source: PixelRect,
}

/// One placement of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KittyPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    /// The namespace of `placement_id`.
    pub namespace: KittyIdNamespace,
    /// A virtual placement (Unicode placeholders) has no position of its own.
    pub is_virtual: bool,
    pub z: i32,
    /// The pixel offset inside the top-left cell.
    pub x_offset: u32,
    pub y_offset: u32,
    /// The cells asked for (`c=`, `r=`; 0: from the image's size); a virtual placement's grid.
    pub columns: u32,
    pub rows: u32,
    /// `None` when the library resolves nothing (its image is missing).
    pub render: Option<KittyRenderInfo>,
    /// A relative placement's parent link; `None` for the others.
    pub parent: Option<KittyParent>,
}

impl KittyPlacement {
    #[must_use]
    pub fn key(&self) -> KittyPlacementKey {
        KittyPlacementKey {
            image_id: self.image_id,
            placement_id: self.placement_id,
            namespace: self.namespace,
        }
    }
}

/// An image, borrowed from the terminal.
#[derive(Debug, Clone, Copy)]
pub struct KittyImage<'a> {
    pub id: u32,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    pub format: KittyFormat,
    /// Changes whenever the pixels may have (a retransmission, an animation frame); unique process-wide, never zero.
    pub generation: u64,
    /// The pixels in `format`; `None` while the payload is still pending.
    pub pixels: Option<&'a [u8]>,
}

/// The image storage of the terminal's active screen, borrowed with the terminal.
#[derive(Debug)]
pub struct KittyGraphics<'t> {
    raw: ffi::GhosttyKittyGraphics,
    terminal: ffi::GhosttyTerminal,
    borrow: PhantomData<&'t mut Terminal>,
}

impl Terminal {
    /// The Kitty image storage; `None` when the library was built without Kitty graphics.
    pub fn kitty_graphics(&mut self) -> Option<KittyGraphics<'_>> {
        // SAFETY: the terminal is live and the result borrows `self` mutably, so no mutating call runs while it lives.
        unsafe { KittyGraphics::of(self.as_raw()) }
    }
}

impl HeldTerminal<'_> {
    /// The Kitty image storage of the terminal running the synchronized-output callback, read as the frame it holds is
    /// captured; `None` when the library was built without Kitty graphics.
    pub fn kitty_graphics(&mut self) -> Option<KittyGraphics<'_>> {
        // SAFETY: the callback's terminal is live for the call, its caller holds what serializes it, and the result
        // borrows `self` (so the callback) mutably: the terminal is not mutated while it lives.
        unsafe { KittyGraphics::of(self.as_raw()) }
    }
}

impl KittyGraphics<'_> {
    /// The storage of `terminal`.
    ///
    /// # Safety
    ///
    /// `terminal` must be live and not mutated while the result lives.
    unsafe fn of(terminal: ffi::GhosttyTerminal) -> Option<Self> {
        let mut raw: ffi::GhosttyKittyGraphics = std::ptr::null_mut();
        // SAFETY: the terminal is live (the caller's contract); `DATA_KITTY_GRAPHICS` writes a handle borrowed until
        // the next mutating call, which the caller rules out while the result lives.
        let result = unsafe {
            ffi::ghostty_terminal_get(
                terminal,
                ffi::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS,
                (&raw mut raw).cast(),
            )
        };
        (result == ffi::GHOSTTY_SUCCESS && !raw.is_null()).then_some(Self {
            raw,
            terminal,
            borrow: PhantomData,
        })
    }
}

impl KittyGraphics<'_> {
    /// The storage's generation: unchanged means the same placements and image data (geometry may still have moved
    /// with scrolling); zero for a storage never written.
    #[must_use]
    pub fn generation(&self) -> u64 {
        let mut generation: u64 = 0;
        // SAFETY: the storage is live (the terminal is borrowed); `DATA_GENERATION` writes a `uint64_t`.
        unsafe {
            ffi::ghostty_kitty_graphics_get(
                self.raw,
                ffi::GHOSTTY_KITTY_GRAPHICS_DATA_GENERATION,
                (&raw mut generation).cast(),
            )
        };
        generation
    }

    /// Every placement, with where it lands on the viewport.
    ///
    /// # Errors
    ///
    /// The library's result when the placements cannot be read.
    pub fn placements(&self) -> Result<Vec<KittyPlacement>, Error> {
        let mut iterator: ffi::GhosttyKittyGraphicsPlacementIterator = std::ptr::null_mut();
        // SAFETY: a null allocator selects the default one; `iterator` is a valid out-pointer.
        Error::check(unsafe {
            ffi::ghostty_kitty_graphics_placement_iterator_new(std::ptr::null(), &raw mut iterator)
        })?;
        let iterator = Handle::new(iterator, ffi::ghostty_kitty_graphics_placement_iterator_free)?;
        let mut filled = iterator.ptr();
        // SAFETY: the storage is live; `DATA_PLACEMENT_ITERATOR` fills the pre-allocated iterator through a pointer to
        // its handle.
        Error::check(unsafe {
            ffi::ghostty_kitty_graphics_get(
                self.raw,
                ffi::GHOSTTY_KITTY_GRAPHICS_DATA_PLACEMENT_ITERATOR,
                (&raw mut filled).cast(),
            )
        })?;
        let mut placements = Vec::new();
        // SAFETY: the iterator is live and filled from storage that cannot change while `self` borrows the terminal.
        while unsafe { ffi::ghostty_kitty_graphics_placement_next(iterator.ptr()) } {
            placements.push(self.placement(iterator.ptr()));
        }
        Ok(placements)
    }

    /// The image `id`; `None` when there is none.
    #[must_use]
    pub fn image(&self, id: u32) -> Option<KittyImage<'_>> {
        // SAFETY: the storage is live; the returned handle is borrowed until the next mutating terminal call, which
        // `self`'s borrow rules out for the image's lifetime.
        let image = unsafe { ffi::ghostty_kitty_graphics_image(self.raw, id) };
        if image.is_null() {
            return None;
        }
        let u32_of = |data| image_value::<u32>(image, data).unwrap_or(0);
        let format = image_value::<ffi::GhosttyKittyImageFormat>(image, ffi::GHOSTTY_KITTY_IMAGE_DATA_FORMAT)
            .and_then(KittyFormat::from_raw)?;
        let mut pointer: *const u8 = std::ptr::null();
        // SAFETY: the image handle is live; `DATA_DATA_PTR` writes a `const uint8_t *`, or reports no value while the
        // payload is pending (leaving null).
        unsafe {
            ffi::ghostty_kitty_graphics_image_get(
                image,
                ffi::GHOSTTY_KITTY_IMAGE_DATA_DATA_PTR,
                (&raw mut pointer).cast(),
            )
        };
        let len = image_value::<usize>(image, ffi::GHOSTTY_KITTY_IMAGE_DATA_DATA_LEN).unwrap_or(0);
        Some(KittyImage {
            id: u32_of(ffi::GHOSTTY_KITTY_IMAGE_DATA_ID),
            number: u32_of(ffi::GHOSTTY_KITTY_IMAGE_DATA_NUMBER),
            width: u32_of(ffi::GHOSTTY_KITTY_IMAGE_DATA_WIDTH),
            height: u32_of(ffi::GHOSTTY_KITTY_IMAGE_DATA_HEIGHT),
            format,
            generation: image_value::<u64>(image, ffi::GHOSTTY_KITTY_IMAGE_DATA_GENERATION).unwrap_or(0),
            // SAFETY: `DATA_PTR` points at `DATA_LEN` bytes owned by the storage, valid while the terminal is not
            // mutated, which the borrow of `self` (and so of the terminal) guarantees for `'_`.
            pixels: (!pointer.is_null()).then(|| unsafe { crate::raw_slice(pointer, len) }),
        })
    }

    /// The placement the iterator is on.
    fn placement(&self, iterator: ffi::GhosttyKittyGraphicsPlacementIterator) -> KittyPlacement {
        let u32_of = |data| placement_value::<u32>(iterator, data).unwrap_or(0);
        let image_id = u32_of(ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IMAGE_ID);
        KittyPlacement {
            image_id,
            placement_id: u32_of(ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PLACEMENT_ID),
            namespace: namespace_value(
                iterator,
                ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PLACEMENT_ID_NAMESPACE,
            )
            .unwrap_or_default(),
            is_virtual: placement_value::<bool>(iterator, ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_IS_VIRTUAL)
                .unwrap_or(false),
            z: placement_value::<i32>(iterator, ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Z).unwrap_or(0),
            x_offset: u32_of(ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_X_OFFSET),
            y_offset: u32_of(ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_Y_OFFSET),
            columns: u32_of(ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_COLUMNS),
            rows: u32_of(ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_ROWS),
            render: self.render_info(iterator, image_id),
            parent: parent(iterator),
        }
    }

    fn render_info(
        &self,
        iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
        image_id: u32,
    ) -> Option<KittyRenderInfo> {
        // SAFETY: as in `image`.
        let image = unsafe { ffi::ghostty_kitty_graphics_image(self.raw, image_id) };
        if image.is_null() {
            return None;
        }
        let mut info = ffi::GhosttyKittyGraphicsPlacementRenderInfo {
            size: std::mem::size_of::<ffi::GhosttyKittyGraphicsPlacementRenderInfo>(),
            pixel_width: 0,
            pixel_height: 0,
            grid_cols: 0,
            grid_rows: 0,
            viewport_col: 0,
            viewport_row: 0,
            viewport_visible: false,
            source_x: 0,
            source_y: 0,
            source_width: 0,
            source_height: 0,
        };
        // SAFETY: the iterator is positioned on a placement of `image`; the terminal is live and borrowed; `info` is a
        // sized out-struct.
        let result =
            unsafe { ffi::ghostty_kitty_graphics_placement_render_info(iterator, image, self.terminal, &raw mut info) };
        (result == ffi::GHOSTTY_SUCCESS).then_some(KittyRenderInfo {
            pixel_width: info.pixel_width,
            pixel_height: info.pixel_height,
            grid_cols: info.grid_cols,
            grid_rows: info.grid_rows,
            viewport_col: info.viewport_col,
            viewport_row: info.viewport_row,
            visible: info.viewport_visible,
            source: PixelRect {
                x: info.source_x,
                y: info.source_y,
                width: info.source_width,
                height: info.source_height,
            },
        })
    }
}

/// One `ghostty_kitty_graphics_image_get` datum of type `T`; `None` when the library has no value.
fn image_value<T: Copy + Default>(
    image: ffi::GhosttyKittyGraphicsImage,
    data: ffi::GhosttyKittyGraphicsImageData,
) -> Option<T> {
    let mut value = T::default();
    // SAFETY: the image handle is live (callers hold the terminal borrowed); callers pass the output type `data`
    // documents as `T`.
    let result = unsafe { ffi::ghostty_kitty_graphics_image_get(image, data, (&raw mut value).cast()) };
    (result == ffi::GHOSTTY_SUCCESS).then_some(value)
}

/// One `ghostty_kitty_graphics_placement_get` datum of type `T`; `None` when the library has no value.
fn placement_value<T: Copy + Default>(
    iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
    data: ffi::GhosttyKittyGraphicsPlacementData,
) -> Option<T> {
    let mut value = T::default();
    // SAFETY: the iterator is live and positioned; callers pass the output type `data` documents as `T`.
    let result = unsafe { ffi::ghostty_kitty_graphics_placement_get(iterator, data, (&raw mut value).cast()) };
    (result == ffi::GHOSTTY_SUCCESS).then_some(value)
}

/// A namespace datum of the placement the iterator is on.
fn namespace_value(
    iterator: ffi::GhosttyKittyGraphicsPlacementIterator,
    data: ffi::GhosttyKittyGraphicsPlacementData,
) -> Option<KittyIdNamespace> {
    placement_value::<ffi::GhosttyKittyPlacementIdNamespace>(iterator, data).map(KittyIdNamespace::from_raw)
}

/// The parent link of the relative placement the iterator is on; `None` for any other placement (every parent datum
/// reports no value).
fn parent(iterator: ffi::GhosttyKittyGraphicsPlacementIterator) -> Option<KittyParent> {
    let image_id = placement_value::<u32>(iterator, ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PARENT_IMAGE_ID)?;
    Some(KittyParent {
        key: KittyPlacementKey {
            image_id,
            placement_id: placement_value::<u32>(
                iterator,
                ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PARENT_PLACEMENT_ID,
            )?,
            namespace: namespace_value(
                iterator,
                ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_PARENT_PLACEMENT_ID_NAMESPACE,
            )?,
        },
        horizontal_offset: placement_value::<i32>(
            iterator,
            ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_RELATIVE_HORIZONTAL_OFFSET,
        )?,
        vertical_offset: placement_value::<i32>(
            iterator,
            ffi::GHOSTTY_KITTY_GRAPHICS_PLACEMENT_DATA_RELATIVE_VERTICAL_OFFSET,
        )?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x1 RGBA image sent directly (`f=32`), in two chunks, then placed.
    const CHUNKED: &[u8] = b"\x1b_Ga=T,f=32,s=2,v=1,i=7,m=1;AQIDBAUG\x1b\\\x1b_Gm=0;Bwg=\x1b\\";

    fn terminal() -> Terminal {
        let mut terminal = Terminal::new(20, 5).unwrap();
        terminal.resize(20, 5, 10, 20).unwrap();
        terminal.set_kitty_image_storage_limit(10_000_000).unwrap();
        terminal
    }

    #[test]
    fn a_chunked_transmission_stores_an_image_and_places_it() {
        let mut terminal = terminal();
        assert_eq!(terminal.kitty_graphics().unwrap().generation(), 0);
        terminal.vt_write(CHUNKED);
        let graphics = terminal.kitty_graphics().unwrap();
        assert_ne!(graphics.generation(), 0);
        let image = graphics.image(7).unwrap();
        assert_eq!((image.id, image.width, image.height), (7, 2, 1));
        assert_eq!(image.format, KittyFormat::Rgba);
        assert_eq!(image.pixels, Some(&[1, 2, 3, 4, 5, 6, 7, 8][..]));
        assert_ne!(image.generation, 0);
        let placements = graphics.placements().unwrap();
        assert_eq!(placements.len(), 1);
        let placement = placements[0];
        assert_eq!(placement.image_id, 7);
        assert!(!placement.is_virtual);
        let render = placement.render.unwrap();
        assert!(render.visible);
        assert_eq!((render.viewport_col, render.viewport_row), (0, 0));
        assert_eq!((render.grid_cols, render.grid_rows), (1, 1));
        assert_eq!(
            render.source,
            PixelRect {
                x: 0,
                y: 0,
                width: 2,
                height: 1
            }
        );
        assert!(graphics.image(8).is_none());
    }

    #[test]
    fn a_deletion_removes_the_placement() {
        let mut terminal = terminal();
        terminal.vt_write(CHUNKED);
        let before = terminal.kitty_graphics().unwrap().generation();
        terminal.vt_write(b"\x1b_Ga=d,d=I,i=7\x1b\\");
        let graphics = terminal.kitty_graphics().unwrap();
        assert!(graphics.generation() > before);
        assert!(graphics.placements().unwrap().is_empty());
        assert!(graphics.image(7).is_none());
    }

    #[test]
    fn a_virtual_placement_has_its_grid_but_no_position() {
        let mut terminal = terminal();
        terminal.vt_write(b"\x1b_Ga=T,U=1,q=2,f=32,s=2,v=1,i=9,p=4,c=3,r=2;AQIDBAUGBwg=\x1b\\");
        let graphics = terminal.kitty_graphics().unwrap();
        let placements = graphics.placements().unwrap();
        assert_eq!(placements.len(), 1);
        let placement = placements[0];
        assert!(placement.is_virtual);
        assert_eq!((placement.image_id, placement.placement_id), (9, 4));
        assert_eq!((placement.columns, placement.rows), (3, 2));
        let render = placement.render.unwrap();
        assert!(!render.visible);
        assert_eq!((render.grid_cols, render.grid_rows), (3, 2));
        assert_eq!(placement.namespace, KittyIdNamespace::External);
        assert!(placement.parent.is_none());
    }

    /// The placements of image `image_id`, internal ids first, each namespace by number.
    fn of_image(placements: &[KittyPlacement], image_id: u32) -> Vec<KittyPlacement> {
        let mut of_image: Vec<KittyPlacement> = placements
            .iter()
            .filter(|placement| placement.image_id == image_id)
            .copied()
            .collect();
        of_image.sort_by_key(|placement| {
            (
                placement.namespace == KittyIdNamespace::External,
                placement.placement_id,
            )
        });
        of_image
    }

    #[test]
    fn a_placement_reports_its_id_namespace() {
        let mut terminal = terminal();
        // two terminal-assigned placements (internal 0 and 1) and the client's p=1 (external 1) of one image
        terminal.vt_write(b"\x1b_Ga=T,q=2,f=24,s=1,v=1,i=1,C=1;AAAA\x1b\\");
        terminal.vt_write(b"\x1b_Ga=p,q=2,i=1,C=1\x1b\\\x1b_Ga=p,q=2,i=1,p=1,C=1\x1b\\");
        let graphics = terminal.kitty_graphics().unwrap();
        let keys: Vec<(u32, KittyIdNamespace)> = of_image(&graphics.placements().unwrap(), 1)
            .iter()
            .map(|placement| (placement.placement_id, placement.namespace))
            .collect();
        assert_eq!(
            keys,
            [
                (0, KittyIdNamespace::Internal),
                (1, KittyIdNamespace::Internal),
                (1, KittyIdNamespace::External),
            ]
        );
    }

    /// Image 1 placed virtually (external 1); image 2 relative to it at (2, -1), two cells wide; image 3 relative to
    /// image 2 at (1, 3), with no `p=` of its own; image 4 on a pin (external 2) with image 5 relative to it at (3, 0),
    /// naming no parent placement (`Q=`).
    const CHAINS: &[u8] = concat!(
        "\x1b_Ga=T,U=1,q=2,f=24,s=1,v=1,i=1,p=1;AAAA\x1b\\",
        "\x1b_Ga=t,q=2,f=24,s=1,v=1,i=2;AAAA\x1b\\",
        "\x1b_Ga=p,q=2,i=2,p=1,P=1,Q=1,H=2,V=-1,c=2,r=1\x1b\\",
        "\x1b_Ga=t,q=2,f=24,s=1,v=1,i=3;AAAA\x1b\\",
        "\x1b_Ga=p,q=2,i=3,P=2,Q=1,H=1,V=3,C=1\x1b\\",
        "\x1b_Ga=T,q=2,f=24,s=1,v=1,i=4,p=2,C=1;AAAA\x1b\\",
        "\x1b_Ga=t,q=2,f=24,s=1,v=1,i=5;AAAA\x1b\\",
        "\x1b_Ga=p,q=2,i=5,P=4,H=3,C=1\x1b\\",
    )
    .as_bytes();

    fn client(image_id: u32, placement_id: u32) -> KittyPlacementKey {
        KittyPlacementKey {
            image_id,
            placement_id,
            namespace: KittyIdNamespace::External,
        }
    }

    #[test]
    fn a_relative_placement_reports_its_parent_link() {
        let mut terminal = terminal();
        terminal.vt_write(CHAINS);
        let graphics = terminal.kitty_graphics().unwrap();
        let placements = graphics.placements().unwrap();
        let only = |image_id| {
            let found = of_image(&placements, image_id);
            assert_eq!(found.len(), 1, "image {image_id}: {found:?}");
            found[0]
        };
        assert_eq!(only(1).parent, None);
        assert_eq!(only(4).parent, None);
        let child = only(2);
        assert_eq!(
            child.parent,
            Some(KittyParent {
                key: client(1, 1),
                horizontal_offset: 2,
                vertical_offset: -1,
            })
        );
        // rooted at a virtual placement: sized and sourced, but no position of its own
        let render = child.render.unwrap();
        assert!(!render.visible);
        assert_eq!((render.grid_cols, render.grid_rows), (2, 1));
        assert_eq!((render.pixel_width, render.pixel_height), (20, 20));
        // its own link only, not the chain's sum
        let grandchild = only(3);
        assert_eq!(grandchild.namespace, KittyIdNamespace::Internal);
        assert_eq!(
            grandchild.parent,
            Some(KittyParent {
                key: client(2, 1),
                horizontal_offset: 1,
                vertical_offset: 3,
            })
        );
        // no `Q=`: the parent storage picked, the pinned image's only placement
        let pinned = only(5);
        assert_eq!(
            pinned.parent,
            Some(KittyParent {
                key: client(4, 2),
                horizontal_offset: 3,
                vertical_offset: 0,
            })
        );
        // rooted at a pin: the library positions it itself
        let render = pinned.render.unwrap();
        assert!(render.visible);
        assert_eq!((render.viewport_col, render.viewport_row), (3, 0));
    }

    #[test]
    fn layers_follow_the_z_index() {
        assert_eq!(KittyLayer::of(i32::MIN), KittyLayer::BelowBackground);
        assert_eq!(KittyLayer::of(-1), KittyLayer::BelowText);
        assert_eq!(KittyLayer::of(0), KittyLayer::AboveText);
    }

    #[test]
    fn storage_off_stores_nothing() {
        let mut terminal = Terminal::new(20, 5).unwrap();
        terminal.set_kitty_image_storage_limit(0).unwrap();
        terminal.vt_write(CHUNKED);
        if let Some(graphics) = terminal.kitty_graphics() {
            assert!(graphics.image(7).is_none());
            assert!(graphics.placements().unwrap().is_empty());
        }
    }
}
