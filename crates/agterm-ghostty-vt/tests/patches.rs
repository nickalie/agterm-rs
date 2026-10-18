//! The behavior each logic patch of `build/patches/ghostty-vt/` (its README lists them) changes, through the wrapper,
//! so a pin bump that drops or breaks one fails here. 0002 is the exception: what it changes (a `--libc` file reaching
//! Wuffs' translator, the allocator stubs of a hosted build with SIMD off) never occurs in the SIMD build without a
//! `--libc` file this crate links, so no test here can see it; its README entry's verification builds what does.

use agterm_ghostty_vt::{Format, FormatOptions, KittyFormat, Point, PointTag, Region, Terminal};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The Wuffs build 0002 patches still works in the build this crate links: Kitty frame composition converts an RGB
/// image to RGBA through Wuffs' swizzler. It passes without 0002 as well (see the module doc).
#[test]
fn wuffs_swizzles_rgb_to_rgba() -> TestResult {
    let mut terminal = Terminal::new(10, 3)?;
    terminal.resize(10, 3, 10, 20)?;
    terminal.set_kitty_image_storage_limit(10_000_000)?;
    // a red 1x1 RGB image (f=24), then a frame over it (a=f), which composes in RGBA
    terminal.vt_write(b"\x1b_Ga=t,f=24,s=1,v=1,i=1,q=2;/wAA\x1b\\");
    terminal.vt_write(b"\x1b_Ga=f,i=1,f=32,s=1,v=1;AAD//w==\x1b\\");
    let graphics = terminal.kitty_graphics().ok_or("built without Kitty graphics")?;
    let image = graphics.image(1).ok_or("the image was not stored")?;
    assert_eq!(image.format, KittyFormat::Rgba);
    assert_eq!(image.pixels, Some(&[255, 0, 0, 255][..]));
    Ok(())
}

/// 0003 (cursor hyperlink across page replacement): an OSC 8 link open while the cursor moves onto a page whose style
/// set is full, so the move grows (replaces) that page, stays the cursor's link; unpatched, the move left a stale link
/// id and the link's end dereferenced null, killing the process (herdr issue 5296).
///
/// The first page of a 215-column terminal (Ghostty's standard page) holds its first 215 rows, so a 500-row screen
/// spans two pages. The first page's style set is filled with `fill` distinct styles; every count up to the second
/// capacity step is tried, so one of them leaves the set exactly full for the move.
#[test]
fn a_hyperlink_survives_a_move_onto_a_full_page() -> TestResult {
    const URI: &str = "https://example.com/5296";
    for fill in 1..=260u32 {
        let mut terminal = Terminal::new(215, 500)?;
        let mut bytes = Vec::new();
        for n in 1..=fill {
            bytes.extend_from_slice(format!("\x1b[38;2;0;{};{}m#", n / 256, n % 256).as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[0m");
        // on the second page: a style of its own (bold, a color no filler has) and the link
        bytes.extend_from_slice(b"\x1b[500;1H\x1b[1;38;2;250;250;250m");
        bytes.extend_from_slice(format!("\x1b]8;id=keep;{URI}\x1b\\").as_bytes());
        // back onto the first page, then the link's text and its end
        bytes.extend_from_slice(b"\x1b[3;1Hx\x1b]8;;\x1b\\y");
        terminal.vt_write(&bytes);

        let at = |col| Point::new(PointTag::Active, col, 2);
        assert_eq!(terminal.hyperlink_uri(at(0))?.as_deref(), Some(URI), "fill {fill}");
        assert_eq!(terminal.hyperlink_uri(at(1))?, None, "fill {fill}");
    }
    Ok(())
}

fn vt(terminal: &mut Terminal) -> Result<String, Box<dyn std::error::Error>> {
    let options = FormatOptions {
        format: Format::Vt,
        unwrap: false,
        trim: true,
    };
    let bytes = terminal.format(Region::Viewport, options)?.ok_or("no viewport")?;
    Ok(String::from_utf8(bytes)?)
}

/// 0004 (formatter unstyled blanks, upstream's zmx patch): cells a program skipped with a cursor move are written as
/// unstyled spaces, not under the previous run's style, so a reattach's snapshot does not paint the gap; spaces the
/// program wrote keep their style.
#[test]
fn skipped_cells_are_written_unstyled() -> TestResult {
    let mut terminal = Terminal::new(20, 2)?;
    terminal.vt_write(b"\x1b[41mAB\x1b[3CX\x1b[0m");
    let skipped = vt(&mut terminal)?;
    assert!(skipped.contains("AB\x1b[0m   \x1b["), "{skipped:?}");
    assert!(!skipped.contains("AB   "), "{skipped:?}");

    let mut terminal = Terminal::new(20, 2)?;
    terminal.vt_write(b"\x1b[41mAB   X\x1b[0m");
    let written = vt(&mut terminal)?;
    assert!(written.contains("AB   X"), "{written:?}");
    Ok(())
}

/// 0004 in HTML: the skipped cells sit outside the styled run's element.
#[test]
fn skipped_cells_close_the_html_style() -> TestResult {
    let mut terminal = Terminal::new(20, 2)?;
    terminal.vt_write(b"\x1b[41mAB\x1b[3CX\x1b[0m");
    let options = FormatOptions {
        format: Format::Html,
        unwrap: false,
        trim: true,
    };
    let html = String::from_utf8(terminal.format(Region::Viewport, options)?.ok_or("no viewport")?)?;
    assert!(html.contains("AB</div>   <div"), "{html:?}");
    Ok(())
}
