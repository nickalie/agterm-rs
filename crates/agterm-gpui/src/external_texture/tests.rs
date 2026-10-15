//! Format and modifier negotiation, the per-buffer check, fence gating, the fall-back to SHM and the SHM canvas,
//! without a GPU (the renderer's side runs in `tests/external_texture`).

use std::io::Write as _;
use std::sync::mpsc;

use gpui_kit::{DmaBufFormat, DmaBufModifier, DmaBufPlane};

use super::*;

const AR24: u32 = fourcc(b"AR24");
const XR24: u32 = fourcc(b"XR24");
const XB24: u32 = fourcc(b"XB24");
const NV12: u32 = fourcc(b"NV12");
/// Intel's X tiling, a one-plane modifier.
const X_TILED: u64 = 0x0100_0000_0000_0001;
/// Intel's 4-tiled CCS with clear color: three memory planes in one object.
const TILE4_CCS_CC: u64 = 0x0100_0000_0000_000d;
/// An NVIDIA block-linear modifier, what `WebKit` on NVIDIA's driver picks when it cannot allocate LINEAR.
const NV_BLOCK_LINEAR: u64 = 0x0300_0000_00e0_8014;

fn modifier(modifier: u64, plane_count: u32) -> DmaBufModifier {
    DmaBufModifier { modifier, plane_count }
}

fn caps(formats: Vec<DmaBufFormat>) -> DmaBufImportCaps {
    DmaBufImportCaps {
        adapter: "Test GPU".to_owned(),
        render_node: Some(PathBuf::from("/dev/dri/renderD128")),
        formats,
        unsupported: None,
    }
}

fn intel_caps() -> DmaBufImportCaps {
    caps(vec![
        DmaBufFormat {
            fourcc: XR24,
            modifiers: vec![
                modifier(MODIFIER_LINEAR, 1),
                modifier(X_TILED, 1),
                modifier(MODIFIER_INVALID, 1),
                modifier(TILE4_CCS_CC, 3),
            ],
        },
        DmaBufFormat {
            fourcc: NV12,
            modifiers: vec![modifier(MODIFIER_LINEAR, 2)],
        },
        DmaBufFormat {
            fourcc: AR24,
            modifiers: vec![modifier(MODIFIER_LINEAR, 1)],
        },
        DmaBufFormat {
            fourcc: XB24,
            modifiers: vec![modifier(MODIFIER_INVALID, 1)],
        },
    ])
}

/// A plane over a fresh file: a regular file polls readable, like a dmabuf with no write in flight.
fn plane() -> DmaBufPlane {
    let file = tempfile::tempfile().expect("a scratch file");
    DmaBufPlane {
        fd: OwnedFd::from(file),
        offset: 0,
        stride: 256,
    }
}

fn buffer(fourcc: u32, modifier: u64, planes: usize) -> Arc<DmaBuffer> {
    Arc::new(DmaBuffer {
        width: 64,
        height: 32,
        fourcc,
        modifier,
        planes: (0..planes).map(|_| plane()).collect(),
    })
}

/// An outcome callback and where its outcome lands (nowhere once the test dropped the receiver: a texture dropped at
/// the end of a test reports its frames discarded).
fn listener() -> (impl FnOnce(FrameOutcome) + Send + 'static, mpsc::Receiver<FrameOutcome>) {
    let (sender, receiver) = mpsc::channel();
    (
        move |outcome| {
            let _ = sender.send(outcome);
        },
        receiver,
    )
}

// MARK: - negotiation

#[test]
fn no_device_yet_means_shm() {
    assert!(matches!(negotiate(None), FrameSource::Shm { reason } if reason.contains("not opened")));
}

#[test]
fn a_device_without_dmabuf_import_means_shm_with_its_reason() {
    let caps = DmaBufImportCaps {
        unsupported: Some("missing VK_EXT_image_drm_format_modifier".to_owned()),
        formats: Vec::new(),
        ..intel_caps()
    };
    let FrameSource::Shm { reason } = negotiate(Some(&caps)) else {
        panic!("dmabufs chosen on a device that imports none");
    };
    assert!(reason.contains("Test GPU"), "{reason}");
    assert!(reason.contains("VK_EXT_image_drm_format_modifier"), "{reason}");
}

#[test]
fn a_device_without_a_render_node_means_shm() {
    let caps = DmaBufImportCaps {
        render_node: None,
        ..intel_caps()
    };
    assert!(matches!(negotiate(Some(&caps)), FrameSource::Shm { reason } if reason.contains("render node")));
}

#[test]
fn the_offer_is_the_drawn_formats_preferred_first_with_explicit_modifiers_linear_last() {
    let FrameSource::DmaBuf { render_node, formats } = negotiate(Some(&intel_caps())) else {
        panic!("SHM chosen while dmabufs import");
    };
    assert_eq!(render_node, PathBuf::from("/dev/dri/renderD128"));
    assert_eq!(
        formats,
        vec![
            OfferedFormat {
                fourcc: AR24,
                modifiers: vec![MODIFIER_LINEAR],
            },
            OfferedFormat {
                fourcc: XR24,
                modifiers: vec![X_TILED, TILE4_CCS_CC, MODIFIER_LINEAR],
            },
        ],
        "NV12 is not drawn, XB24 has only the implicit modifier"
    );
}

#[test]
fn a_device_importing_no_drawn_format_means_shm() {
    let caps = caps(vec![DmaBufFormat {
        fourcc: NV12,
        modifiers: vec![modifier(MODIFIER_LINEAR, 2)],
    }]);
    assert!(matches!(negotiate(Some(&caps)), FrameSource::Shm { reason } if reason.contains("drawn formats")));
}

// MARK: - the per-buffer check

#[test]
fn a_buffer_the_device_imports_passes() {
    let caps = intel_caps();
    assert_eq!(import_refusal(&caps, &buffer(XR24, X_TILED, 1)), None);
    assert_eq!(import_refusal(&caps, &buffer(XR24, TILE4_CCS_CC, 3)), None);
}

#[test]
fn a_modifier_outside_the_devices_set_is_refused() {
    // WebKit on NVIDIA's own driver ignores an offered LINEAR and renders block-linear.
    let refusal = import_refusal(&intel_caps(), &buffer(XR24, NV_BLOCK_LINEAR, 1)).expect("refused");
    assert!(refusal.contains("0x0300000000e08014"), "{refusal}");
}

#[test]
fn a_format_the_device_does_not_import_is_refused() {
    let refusal = import_refusal(&intel_caps(), &buffer(fourcc(b"RG16"), MODIFIER_LINEAR, 1)).expect("refused");
    assert!(refusal.contains("RG16"), "{refusal}");
}

#[test]
fn a_plane_count_other_than_the_modifiers_is_refused() {
    let refusal = import_refusal(&intel_caps(), &buffer(XR24, TILE4_CCS_CC, 1)).expect("refused");
    assert!(refusal.contains("3 planes"), "{refusal}");
}

#[test]
fn an_empty_buffer_is_refused() {
    let empty = Arc::new(DmaBuffer {
        width: 0,
        ..Arc::into_inner(buffer(XR24, MODIFIER_LINEAR, 1)).expect("one owner")
    });
    assert!(import_refusal(&intel_caps(), &empty).is_some());
}

#[test]
fn every_buffer_is_refused_by_a_device_that_imports_none() {
    let caps = DmaBufImportCaps {
        unsupported: Some("not Vulkan".to_owned()),
        ..intel_caps()
    };
    assert!(
        import_refusal(&caps, &buffer(XR24, MODIFIER_LINEAR, 1)).is_some_and(|reason| reason.contains("not Vulkan"))
    );
}

// MARK: - frames, fences and the fall-back

fn texture() -> ExternalTexture {
    ExternalTexture::with_caps(Arc::new(intel_caps()))
}

#[test]
fn a_frame_shows_only_once_its_sync_file_signals() {
    let mut texture = texture();
    let (reader, mut writer) = std::io::pipe().expect("a pipe");
    let (done, _outcome) = listener();
    texture.push_dmabuf(
        buffer(XR24, MODIFIER_LINEAR, 1),
        Acquire::SyncFile(OwnedFd::from(reader)),
        done,
    );
    assert!(texture.promote_ready(), "still waiting");
    assert!(texture.shown().is_none());
    writer.write_all(b"x").expect("signal");
    assert!(!texture.promote_ready());
    assert!(matches!(texture.shown(), Some(ExternalSurface::DmaBuf(_))));
}

#[test]
fn a_frame_with_an_idle_buffer_shows_at_once() {
    let mut texture = texture();
    let (done, _outcome) = listener();
    texture.push_dmabuf(buffer(XR24, MODIFIER_LINEAR, 1), Acquire::Implicit, done);
    assert!(!texture.promote_ready());
    assert!(texture.shown().is_some());
}

#[test]
fn a_waiting_frame_replaced_by_a_newer_one_is_discarded() {
    let mut texture = texture();
    let (reader, _writer) = std::io::pipe().expect("a pipe");
    let (first, first_outcome) = listener();
    texture.push_dmabuf(
        buffer(XR24, MODIFIER_LINEAR, 1),
        Acquire::SyncFile(OwnedFd::from(reader)),
        first,
    );
    let (second, second_outcome) = listener();
    texture.push_dmabuf(buffer(XR24, MODIFIER_LINEAR, 1), Acquire::Implicit, second);
    assert_eq!(first_outcome.try_recv(), Ok(FrameOutcome::Discarded));
    assert!(!texture.promote_ready());
    assert!(
        second_outcome.try_recv().is_err(),
        "the shown frame waits for the renderer"
    );
    texture.clear();
    assert_eq!(second_outcome.try_recv(), Ok(FrameOutcome::Discarded));
}

#[test]
fn the_renderers_outcome_reaches_the_producer() {
    let mut texture = texture();
    let (done, outcome) = listener();
    texture.push_dmabuf(buffer(XR24, MODIFIER_LINEAR, 1), Acquire::Implicit, done);
    texture.promote_ready();
    let shown = texture.shown().expect("shown").clone();
    shown.completion().complete(FrameOutcome::Presented);
    assert_eq!(outcome.try_recv(), Ok(FrameOutcome::Presented));
    assert_eq!(texture.take_fallback(), None, "a presented frame asks for no fall-back");
    assert_eq!(texture.fallback(), &Fallback::None);
}

#[test]
fn a_refused_buffer_fails_its_frame_and_requests_the_fallback_once() {
    let mut texture = texture();
    let (done, outcome) = listener();
    texture.push_dmabuf(buffer(XR24, NV_BLOCK_LINEAR, 1), Acquire::Implicit, done);
    assert!(matches!(outcome.try_recv(), Ok(FrameOutcome::Failed(_))));
    assert!(texture.shown().is_none());
    assert!(
        texture
            .take_fallback()
            .is_some_and(|reason| reason.contains("modifier"))
    );
    assert_eq!(texture.take_fallback(), None);
    assert_eq!(texture.fallback(), &Fallback::Taken);
    // The producer is switching to SHM: dmabuf frames still in flight are discarded, SHM frames shown.
    let (late, late_outcome) = listener();
    texture.push_dmabuf(buffer(XR24, MODIFIER_LINEAR, 1), Acquire::Implicit, late);
    assert_eq!(late_outcome.try_recv(), Ok(FrameOutcome::Discarded));
    let (shm, _shm_outcome) = listener();
    texture.push_shm(2, 2, [], shm);
    assert!(matches!(texture.shown(), Some(ExternalSurface::Pixels(_))));
}

#[test]
fn a_frame_the_renderer_fails_requests_the_fallback() {
    let mut texture = texture();
    let (done, outcome) = listener();
    texture.push_dmabuf(buffer(XR24, MODIFIER_LINEAR, 1), Acquire::Implicit, done);
    texture.promote_ready();
    assert_eq!(texture.take_fallback(), None);
    let shown = texture.shown().expect("shown").clone();
    shown.completion().complete(FrameOutcome::Failed(
        "vkCreateImage: ERROR_INVALID_DRM_FORMAT_MODIFIER".to_owned(),
    ));
    assert!(matches!(outcome.try_recv(), Ok(FrameOutcome::Failed(_))));
    assert!(
        texture
            .take_fallback()
            .is_some_and(|reason| reason.contains("vkCreateImage"))
    );
}

#[test]
fn without_a_device_a_dmabuf_frame_fails() {
    let mut texture = ExternalTexture::new();
    let (done, outcome) = listener();
    texture.push_dmabuf(buffer(XR24, MODIFIER_LINEAR, 1), Acquire::Implicit, done);
    assert!(matches!(outcome.try_recv(), Ok(FrameOutcome::Failed(reason)) if reason.contains("not opened")));
}

// MARK: - SHM

fn pixels(frame: &ExternalSurface) -> (Arc<PixelFrame>, Vec<u8>) {
    let ExternalSurface::Pixels(frame) = frame else {
        panic!("not an SHM frame");
    };
    (Arc::clone(frame), frame.pixels.to_vec())
}

fn region(rect: PixelRect, value: u8) -> (PixelRect, Vec<u8>) {
    (rect, vec![value; rect.width as usize * rect.height as usize * 4])
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect { x, y, width, height }
}

#[test]
fn shm_regions_compose_into_the_frame_with_their_damage() {
    let mut texture = texture();
    let (first_rect, first_pixels) = region(rect(0, 0, 4, 2), 1);
    let (done, _outcome) = listener();
    texture.push_shm(
        4,
        2,
        [ShmRegion {
            rect: first_rect,
            pixels: &first_pixels,
        }],
        done,
    );
    let (first, _) = pixels(texture.shown().expect("shown"));
    assert_eq!(first.serial, 1);
    assert_eq!(first.damage, vec![first_rect]);

    let (second_rect, second_pixels) = region(rect(1, 1, 2, 1), 9);
    let (done, _outcome) = listener();
    texture.push_shm(
        4,
        2,
        [ShmRegion {
            rect: second_rect,
            pixels: &second_pixels,
        }],
        done,
    );
    let (second, bytes) = pixels(texture.shown().expect("shown"));
    assert_eq!(second.serial, 2);
    assert_eq!(second.damage, vec![second_rect]);
    assert!(Arc::ptr_eq(&first.surface, &second.surface), "one texture per surface");
    let row1: Vec<u8> = bytes[16..32].to_vec();
    assert_eq!(row1, [[1u8; 4], [9; 4], [9; 4], [1; 4]].concat());
    assert!(
        first.pixels.iter().all(|&byte| byte == 1),
        "the frame the renderer may still read is untouched"
    );
}

#[test]
fn shm_regions_are_clipped_to_the_frame_and_short_ones_ignored() {
    let mut texture = texture();
    let (overhang, overhang_pixels) = region(rect(3, 1, 4, 4), 7);
    let short = vec![0u8; 3];
    let (done, _outcome) = listener();
    texture.push_shm(
        4,
        2,
        [
            ShmRegion {
                rect: overhang,
                pixels: &overhang_pixels,
            },
            ShmRegion {
                rect: rect(0, 0, 2, 2),
                pixels: &short,
            },
            ShmRegion {
                rect: rect(9, 9, 1, 1),
                pixels: &[0; 4],
            },
        ],
        done,
    );
    let (frame, bytes) = pixels(texture.shown().expect("shown"));
    assert_eq!(frame.damage, vec![rect(3, 1, 1, 1)]);
    assert_eq!(&bytes[28..32], &[7; 4]);
    assert_eq!(bytes.iter().filter(|&&byte| byte != 0).count(), 4);
}

#[test]
fn a_new_shm_size_starts_a_new_frame() {
    let mut texture = texture();
    let (done, _outcome) = listener();
    texture.push_shm(4, 2, [], done);
    let (done, _outcome) = listener();
    let (whole, whole_pixels) = region(rect(0, 0, 2, 2), 5);
    texture.push_shm(
        2,
        2,
        [ShmRegion {
            rect: whole,
            pixels: &whole_pixels,
        }],
        done,
    );
    let (frame, bytes) = pixels(texture.shown().expect("shown"));
    assert_eq!((frame.width, frame.height, frame.serial), (2, 2, 1));
    assert_eq!(bytes, vec![5; 16]);
}
