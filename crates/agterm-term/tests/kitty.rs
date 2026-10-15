//! Kitty graphics through a real PTY: a chunked APC transmission (raw pixels, and PNG as `show-image.sh` sends it)
//! becomes an image and a placement in the frame's view; deleting it reports it removed.

#[expect(dead_code, reason = "the file and event helpers serve the other test files")]
mod support;

use std::fmt::Write as _;

use agterm_term::{KittyFormat, KittyLayer, PaneConfig};
use base64::Engine as _;
use support::{SIZE, TestResult, spawn, spawn_with, wait_frame};

/// The `printf` commands that send `payload` (base64) as chunked kitty APC with the `control` keys on the first chunk.
fn chunked(control: &str, payload: &str, chunk: usize) -> String {
    let chunks: Vec<_> = payload.as_bytes().chunks(chunk).map(String::from_utf8_lossy).collect();
    let mut script = String::new();
    for (index, data) in chunks.iter().enumerate() {
        let more = u8::from(index + 1 < chunks.len());
        let keys = if index == 0 {
            format!("{control},m={more}")
        } else {
            format!("m={more}")
        };
        let _ = write!(script, "printf '\\033_G{keys};{data}\\033\\\\'; ");
    }
    script
}

fn png(width: u32, height: u32, rgba: &[u8]) -> TestResult<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(bytes)
}

#[test]
fn a_chunked_png_becomes_an_image_and_a_placement() -> TestResult {
    let rgba: Vec<u8> = (0..3 * 2 * 4).map(|byte| u8::try_from(byte).unwrap()).collect();
    let payload = base64::engine::general_purpose::STANDARD.encode(png(3, 2, &rgba)?);
    let script = format!(
        "printf 'before\\r\\n'; {}printf 'after'; exec sleep 30",
        chunked("a=T,f=100,i=42", &payload, 16)
    );
    let fixture = spawn(&script)?;
    let frame = wait_frame(&fixture.pane, |frame| !frame.kitty.placements.is_empty());
    let view = &frame.kitty;
    assert_eq!(view.images.len(), 1);
    let image = &view.images[0];
    assert_eq!((image.key.id, image.width, image.height), (42, 3, 2));
    assert_eq!(image.format, KittyFormat::Rgba);
    assert_eq!(&*image.pixels, rgba.as_slice());
    let placement = view.placements[0];
    assert_eq!(placement.image, image.key);
    assert_eq!(placement.layer, KittyLayer::AboveText);
    let rect = placement.rect.unwrap();
    // placed at the cursor: column 0 of the row after `before`; 3x2 px fits one 8x16 cell
    assert_eq!((rect.column, rect.row, rect.columns, rect.rows), (0, 1, 1, 1));
    assert_eq!((rect.source.width, rect.source.height), (3, 2));
    assert!(view.removed.is_empty());
    Ok(())
}

#[test]
fn raw_pixels_scroll_with_the_text_and_a_deletion_is_reported() -> TestResult {
    // 2x1 RGBA sent in two chunks and placed, quietly (`q=2`: no `OK` reply lands in the `read` below); then on
    // input scrolled, and on more input deleted
    let script = format!(
        "{}read _; for i in $(seq 1 30); do printf 'line %s\\r\\n' $i; done; printf scrolled; read _; \
         printf '\\033_Ga=d,d=I,i=7,q=2\\033\\\\deleted'; exec sleep 30",
        chunked("a=T,f=32,s=2,v=1,i=7,q=2", "AQIDBAUGBwg=", 8)
    );
    let fixture = spawn(&script)?;
    let pane = &fixture.pane;
    let shown = wait_frame(pane, |frame| !frame.kitty.placements.is_empty());
    let image = &shown.kitty.images[0];
    assert_eq!(&*image.pixels, &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(shown.kitty.placements[0].rect.map(|rect| rect.row), Some(0));

    pane.write(b"\n")?;
    let scrolled = wait_frame(pane, |frame| frame.lines().iter().any(|line| line.contains("scrolled")));
    // off the top now: still stored, no longer on the viewport; the pixels were not copied again
    assert_eq!(scrolled.kitty.placements.len(), 1);
    assert_eq!(scrolled.kitty.placements[0].rect, None);
    assert!(std::sync::Arc::ptr_eq(&scrolled.kitty.images[0], image));

    pane.write(b"\n")?;
    let deleted = wait_frame(pane, |frame| frame.lines().iter().any(|line| line.contains("deleted")));
    assert!(deleted.kitty.placements.is_empty() && deleted.kitty.images.is_empty());
    Ok(())
}

#[test]
fn a_zero_storage_limit_turns_kitty_graphics_off() -> TestResult {
    let config = PaneConfig {
        kitty_image_limit: 0,
        ..PaneConfig::new(SIZE)
    };
    let script = format!(
        "{}printf done; exec sleep 30",
        chunked("a=T,f=32,s=2,v=1,i=7", "AQIDBAUGBwg=", 8)
    );
    let fixture = spawn_with(&script, &config)?;
    let frame = wait_frame(&fixture.pane, |frame| {
        frame.lines().iter().any(|line| line.contains("done"))
    });
    assert!(frame.kitty.images.is_empty() && frame.kitty.placements.is_empty());
    Ok(())
}
