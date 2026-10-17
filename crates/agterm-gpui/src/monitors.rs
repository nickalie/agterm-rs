//! The displays a restored window is kept on ([`crate::window_frames::Frame::constrained`]) and whether the platform
//! places a window at its frame's origin at all. GPUI's displays serve, except on X11: there GPUI has one display per
//! X screen, the root window that spans every monitor, with dead areas where the monitors differ in size, so a frame
//! saved on a monitor gone since could come back inside the root yet on no monitor. There the monitors are `RandR`'s
//! (`RRGetMonitors`), read on a connection of the app's own, in GPUI's logical pixels.

use agterm_core::window_geometry::Rect;
use gpui_kit::App;

use crate::window_frames::Placement;

/// Where windows go on this platform: at their origin, except on Wayland, whose compositor places every window.
#[must_use]
pub fn placement(cx: &App) -> Placement {
    if cx.compositor_name() == "Wayland" {
        Placement::ByCompositor
    } else {
        Placement::AtOrigin
    }
}

/// The displays attached now, the primary one first: the X11 monitors, else (or when `RandR` reads none) GPUI's
/// displays.
#[must_use]
pub fn display_rects(cx: &App) -> Vec<Rect> {
    if cx.compositor_name() == "X11"
        && let Some(monitors) = x11_monitors(cx)
    {
        return monitors;
    }
    let primary = cx.primary_display().map(|display| display.id());
    let mut displays = cx.displays();
    displays.sort_by_key(|display| Some(display.id()) != primary);
    displays.iter().map(|display| rect(display.bounds())).collect()
}

fn rect(bounds: gpui_kit::Bounds<gpui_kit::Pixels>) -> Rect {
    Rect::new(
        f64::from(f32::from(bounds.origin.x)),
        f64::from(f32::from(bounds.origin.y)),
        f64::from(f32::from(bounds.size.width)),
        f64::from(f32::from(bounds.size.height)),
    )
}

/// A `RandR` monitor, in the X screen's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Monitor {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub primary: bool,
}

/// `monitors` in logical pixels, the X screen's divided by `scale` (GPUI's X11 scale factor), the primary one first;
/// a monitor with no area is left out.
#[must_use]
pub fn logical(monitors: &[Monitor], scale: f64) -> Vec<Rect> {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let mut ordered: Vec<&Monitor> = monitors
        .iter()
        .filter(|monitor| monitor.width > 0 && monitor.height > 0)
        .collect();
    ordered.sort_by_key(|monitor| !monitor.primary);
    ordered
        .into_iter()
        .map(|monitor| {
            Rect::new(
                f64::from(monitor.x) / scale,
                f64::from(monitor.y) / scale,
                f64::from(monitor.width) / scale,
                f64::from(monitor.height) / scale,
            )
        })
        .collect()
}

/// The X screen's monitors in GPUI's logical pixels; `None` (logged) when `RandR` reads none. The scale is GPUI's: its
/// primary display (the X screen) in logical pixels against the screen's own width.
fn x11_monitors(cx: &App) -> Option<Vec<Rect>> {
    let logical_width = f64::from(f32::from(cx.primary_display()?.bounds().size.width));
    match read_x11_monitors() {
        Ok((monitors, screen_width)) if !monitors.is_empty() && logical_width > 0.0 => {
            Some(logical(&monitors, f64::from(screen_width) / logical_width))
        }
        Ok(_) => None,
        Err(err) => {
            tracing::debug!(%err, "the X11 monitors do not read; the X screen stands for them");
            None
        }
    }
}

/// `RandR`'s active monitors of the default X screen and the screen's width in pixels.
fn read_x11_monitors() -> Result<(Vec<Monitor>, u16), Box<dyn std::error::Error>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::randr::ConnectionExt as _;

    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots.get(screen).ok_or("the display has no such screen")?;
    let (window, width) = (root.root, root.width_in_pixels);
    let reply = conn.randr_get_monitors(window, true)?.reply()?;
    let monitors = reply
        .monitors
        .iter()
        .map(|monitor| Monitor {
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
            primary: monitor.primary,
        })
        .collect();
    Ok((monitors, width))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(x: i16, y: i16, width: u16, height: u16, primary: bool) -> Monitor {
        Monitor {
            x,
            y,
            width,
            height,
            primary,
        }
    }

    #[test]
    fn monitors_read_in_logical_pixels_the_primary_first() {
        let monitors = [
            monitor(0, 0, 1920, 2160, false),
            monitor(1920, 0, 1920, 1080, true),
            monitor(0, 0, 0, 0, false),
        ];
        assert_eq!(
            logical(&monitors, 2.0),
            vec![Rect::new(960.0, 0.0, 960.0, 540.0), Rect::new(0.0, 0.0, 960.0, 1080.0)],
            "halved at a scale of 2, the primary first, the empty one dropped"
        );
        assert_eq!(logical(&monitors[..1], 0.0), vec![Rect::new(0.0, 0.0, 1920.0, 2160.0)]);
        assert_eq!(logical(&[], 1.0), Vec::<Rect>::new());
    }
}
