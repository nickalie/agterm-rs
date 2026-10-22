//! The saved frames: written per window, read back, pruned to the library, and constrained to the displays attached.

use super::*;

const DISPLAY: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);

fn frame(x: f64, y: f64, width: f64, height: f64) -> Frame {
    Frame {
        x,
        y,
        width,
        height,
        maximized: false,
    }
}

#[test]
fn a_saved_frame_reads_back_and_a_deleted_window_s_goes() {
    let dir = tempfile::tempdir().unwrap();
    let (kept, deleted) = (Uuid::new_v4(), Uuid::new_v4());
    let mut frames = WindowFrames::load(dir.path());
    assert_eq!(frames.get(kept), None, "nothing saved yet");
    frames.save(deleted, frame(0.0, 0.0, 700.0, 500.0), &[kept, deleted]);
    let maximized = Frame {
        maximized: true,
        ..frame(40.0, 30.0, 1200.0, 800.0)
    };
    frames.save(kept, maximized, &[kept, deleted]);
    let again = WindowFrames::load(dir.path());
    assert_eq!(again.get(kept), Some(maximized), "read back from the file");
    assert_eq!(again.get(deleted), Some(frame(0.0, 0.0, 700.0, 500.0)));
    // a later save drops the frame of a window the library no longer has
    let mut again = again;
    again.save(kept, frame(1.0, 2.0, 800.0, 600.0), &[kept]);
    let last = WindowFrames::load(dir.path());
    assert_eq!(last.get(kept), Some(frame(1.0, 2.0, 800.0, 600.0)));
    assert_eq!(last.get(deleted), None);
}

#[test]
fn an_unreadable_file_or_an_unusable_frame_restores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(FILE_NAME), b"{ not json").unwrap();
    let id = Uuid::new_v4();
    let mut frames = WindowFrames::load(dir.path());
    assert_eq!(frames.get(id), None);
    frames.save(id, frame(0.0, 0.0, 0.0, 600.0), &[id]);
    assert_eq!(frames.get(id), None, "a zero width is no frame");
    let text = format!(r#"{{"{id}": {{"x": 0, "y": 0, "width": -5, "height": 600}}}}"#);
    std::fs::write(dir.path().join(FILE_NAME), text).unwrap();
    assert_eq!(
        WindowFrames::load(dir.path()).get(id),
        None,
        "a negative size is dropped on read"
    );
}

#[test]
fn a_frame_is_restored_onto_a_display_attached_now() {
    let second = Rect::new(1920.0, 0.0, 1280.0, 1024.0);
    // on its display, unchanged
    assert_eq!(
        frame(100.0, 100.0, 900.0, 600.0).constrained(&[DISPLAY, second], Placement::AtOrigin),
        frame(100.0, 100.0, 900.0, 600.0)
    );
    // mostly on the second display: kept there, shrunk to it
    assert_eq!(
        frame(2000.0, 10.0, 1500.0, 900.0).constrained(&[DISPLAY, second], Placement::AtOrigin),
        frame(1920.0, 10.0, 1280.0, 900.0)
    );
    // on a display gone since: the first (primary) one takes it, wholly on screen
    let gone = Frame {
        maximized: true,
        ..frame(5000.0, 3000.0, 800.0, 500.0)
    };
    assert_eq!(
        gone.constrained(&[DISPLAY], Placement::AtOrigin),
        Frame {
            maximized: true,
            ..frame(1120.0, 580.0, 800.0, 500.0)
        },
        "the maximized state rides along"
    );
    // below the least size it grows; with no display known it is left alone
    assert_eq!(
        frame(0.0, 0.0, 200.0, 100.0).constrained(&[DISPLAY], Placement::AtOrigin),
        frame(0.0, 0.0, 640.0, 400.0)
    );
    assert_eq!(
        frame(0.0, 0.0, 200.0, 100.0).constrained(&[], Placement::AtOrigin),
        frame(0.0, 0.0, 200.0, 100.0)
    );
}

#[test]
fn a_frame_the_compositor_places_keeps_its_size_up_to_the_largest_display() {
    // Wayland: a laptop panel and a larger external monitor; the saved origin is no position, so the external
    // monitor's window is not fitted to the panel its origin happens to fall on
    let (panel, external) = (
        Rect::new(0.0, 0.0, 1280.0, 800.0),
        Rect::new(1280.0, 0.0, 2560.0, 1440.0),
    );
    let displays = [panel, external];
    assert_eq!(
        frame(0.0, 0.0, 2400.0, 1300.0).constrained(&displays, Placement::ByCompositor),
        frame(0.0, 0.0, 2400.0, 1300.0),
        "kept whole"
    );
    // larger than any display: the largest one's size; below the least size: grown
    assert_eq!(
        frame(0.0, 0.0, 3000.0, 2000.0).constrained(&displays, Placement::ByCompositor),
        frame(0.0, 0.0, 2560.0, 1440.0)
    );
    let maximized = Frame {
        maximized: true,
        ..frame(10.0, 20.0, 200.0, 100.0)
    };
    assert_eq!(
        maximized.constrained(&displays, Placement::ByCompositor),
        Frame {
            maximized: true,
            ..frame(10.0, 20.0, 640.0, 400.0)
        }
    );
    assert_eq!(
        frame(0.0, 0.0, 2400.0, 1300.0).constrained(&[], Placement::ByCompositor),
        frame(0.0, 0.0, 2400.0, 1300.0)
    );
}
