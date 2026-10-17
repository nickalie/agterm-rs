//! Host-free window geometry (upstream's `WindowGeometry`): the clamps a frontend applies with the live displays and
//! the window's minimum size it alone knows — a restored frame kept on a current display, a requested size or origin
//! kept usable. Every rectangle shares one coordinate space (the frontend's logical pixels).

/// A width and height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

impl Size {
    #[must_use]
    pub const fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }
}

/// An x/y point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// An origin and a size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            origin: Point::new(x, y),
            size: Size::new(width, height),
        }
    }
}

/// The least strip kept on the display when an origin is clamped, so a window pushed off screen still shows enough
/// of itself to grab and drag back (upstream's `visibleMargin`).
pub const VISIBLE_MARGIN: f64 = 80.0;

/// Each dimension of `requested` clamped into `[min, max]`; with `min` over `max` the upper bound wins.
#[must_use]
pub fn clamp_size(requested: Size, min: Size, max: Size) -> Size {
    Size::new(
        clamp(requested.width, min.width, max.width),
        clamp(requested.height, min.height, max.height),
    )
}

/// `requested` clamped so the window keeps at least [`VISIBLE_MARGIN`] overlapping `display` in each axis; an origin
/// already on screen is unchanged.
#[must_use]
pub fn clamp_origin(requested: Point, window: Size, display: Rect) -> Point {
    let min_x = display.origin.x + VISIBLE_MARGIN - window.width;
    let max_x = display.origin.x + display.size.width - VISIBLE_MARGIN;
    let min_y = display.origin.y + VISIBLE_MARGIN - window.height;
    let max_y = display.origin.y + display.size.height - VISIBLE_MARGIN;
    Point::new(clamp(requested.x, min_x, max_x), clamp(requested.y, min_y, max_y))
}

/// The index of the display `frame` overlaps most, `None` when it overlaps none (a display no longer attached).
#[must_use]
pub fn best_display_index(frame: Rect, displays: &[Rect]) -> Option<usize> {
    let mut best = None;
    let mut best_area = 0.0;
    for (index, display) in displays.iter().enumerate() {
        let area = intersection_area(frame, *display);
        if area > best_area {
            best_area = area;
            best = Some(index);
        }
    }
    best
}

/// A saved frame made to fit `display` for a restore: an oversized one shrunk to the display, an undersized one grown
/// to `min`, then the whole frame kept on the display, so a stale coordinate never restores a window whose title bar
/// is out of reach.
#[must_use]
pub fn constrain(frame: Rect, min: Size, display: Rect) -> Rect {
    let size = clamp_size(frame.size, min, display.size);
    let origin = Point::new(
        clamp(
            frame.origin.x,
            display.origin.x,
            display.origin.x + display.size.width - size.width,
        ),
        clamp(
            frame.origin.y,
            display.origin.y,
            display.origin.y + display.size.height - size.height,
        ),
    );
    Rect { origin, size }
}

/// `value` clamped into `[lo, hi]`; with `lo` over `hi` the upper bound wins.
fn clamp(value: f64, lo: f64, hi: f64) -> f64 {
    value.max(lo).min(hi)
}

fn intersection_area(lhs: Rect, rhs: Rect) -> f64 {
    let min_x = lhs.origin.x.max(rhs.origin.x);
    let max_x = (lhs.origin.x + lhs.size.width).min(rhs.origin.x + rhs.size.width);
    let min_y = lhs.origin.y.max(rhs.origin.y);
    let max_y = (lhs.origin.y + lhs.size.height).min(rhs.origin.y + rhs.size.height);
    (max_x - min_x).max(0.0) * (max_y - min_y).max(0.0)
}

/// Upstream's `WindowGeometryTests`.
#[cfg(test)]
mod tests {
    use super::*;

    const DISPLAY: Rect = Rect::new(0.0, 0.0, 1920.0, 1080.0);
    const MIN: Size = Size::new(640.0, 400.0);

    #[test]
    fn clamp_size_bounds_an_oversized_request_to_max() {
        let max = Size::new(1000.0, 800.0);
        let min = Size::new(400.0, 300.0);
        assert_eq!(clamp_size(Size::new(5000.0, 4000.0), min, max), max);
        assert_eq!(clamp_size(Size::new(100.0, 50.0), min, max), min);
        assert_eq!(clamp_size(Size::new(700.0, 500.0), min, max), Size::new(700.0, 500.0));
    }

    #[test]
    fn clamp_size_with_min_over_max_returns_max() {
        let result = clamp_size(
            Size::new(2000.0, 1500.0),
            Size::new(1200.0, 900.0),
            Size::new(800.0, 600.0),
        );
        assert_eq!(result, Size::new(800.0, 600.0));
    }

    #[test]
    fn clamp_origin_keeps_a_strip_on_screen() {
        let window = Size::new(800.0, 600.0);
        assert_eq!(
            clamp_origin(Point::new(100.0, 100.0), window, DISPLAY),
            Point::new(100.0, 100.0)
        );
        assert_eq!(
            clamp_origin(Point::new(5000.0, 100.0), window, DISPLAY),
            Point::new(1920.0 - VISIBLE_MARGIN, 100.0)
        );
        assert_eq!(
            clamp_origin(Point::new(-5000.0, -5000.0), window, DISPLAY),
            Point::new(VISIBLE_MARGIN - 800.0, VISIBLE_MARGIN - 600.0)
        );
        assert_eq!(
            clamp_origin(Point::new(100.0, 5000.0), window, DISPLAY),
            Point::new(100.0, 1080.0 - VISIBLE_MARGIN)
        );
    }

    #[test]
    fn best_display_index_chooses_the_largest_overlap_or_none() {
        let displays = [DISPLAY, Rect::new(1920.0, 0.0, 1920.0, 1080.0)];
        assert_eq!(
            best_display_index(Rect::new(1800.0, 100.0, 400.0, 500.0), &displays),
            Some(1)
        );
        assert_eq!(
            best_display_index(Rect::new(5000.0, 100.0, 800.0, 600.0), &[DISPLAY]),
            None
        );
    }

    #[test]
    fn constrain_shrinks_and_contains_a_saved_frame() {
        assert_eq!(
            constrain(Rect::new(100.0, 100.0, 5000.0, 4000.0), MIN, DISPLAY),
            DISPLAY
        );
        assert_eq!(
            constrain(Rect::new(5000.0, 100.0, 800.0, 600.0), MIN, DISPLAY),
            Rect::new(1920.0 - 800.0, 100.0, 800.0, 600.0)
        );
        assert_eq!(
            constrain(Rect::new(100.0, 5000.0, 800.0, 600.0), MIN, DISPLAY),
            Rect::new(100.0, 1080.0 - 600.0, 800.0, 600.0)
        );
        assert_eq!(
            constrain(Rect::new(5000.0, 100.0, 5000.0, 4000.0), MIN, DISPLAY),
            DISPLAY
        );
        assert_eq!(
            constrain(Rect::new(10.0, 10.0, 100.0, 50.0), MIN, DISPLAY),
            Rect::new(10.0, 10.0, 640.0, 400.0),
            "an undersized frame grows to the minimum"
        );
    }
}
