//! Result lines and the trivial views the spike's windows show.

use gpui_kit::*;
use serde_json::Value;

/// Prints one `RESULT` line: a named check, whether it passed, and what was measured.
pub fn result(check: &str, pass: bool, detail: Value) {
    let line = serde_json::json!({ "check": check, "pass": pass, "detail": detail });
    println!("RESULT {line}");
}

/// Prints one `INFO` line: a measurement that is not a pass/fail check by itself.
pub fn info(name: &str, detail: Value) {
    let line = serde_json::json!({ "info": name, "detail": detail });
    println!("INFO {line}");
}

/// A window body: one line of text over an opaque, rounded frame (the quick panel's look: 12 px radius, white at
/// 18% border) on a transparent window, so the corners show what is behind.
pub struct Label(pub SharedString);

impl Render for Label {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .rounded(px(12.))
            .border_1()
            .border_color(rgba(0xffffff2e))
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .p_4()
            .child(self.0.clone())
    }
}

/// A display's identity in results: GPUI's id, its bounds, and its `Debug` form (the Wayland output name is in it).
pub fn display_json(display: &dyn PlatformDisplay) -> Value {
    let bounds = display.bounds();
    let visible = display.visible_bounds();
    serde_json::json!({
        "id": u64::from(display.id()),
        "debug": format!("{display:?}"),
        "bounds": [f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height)],
        "visible": [f32::from(visible.origin.x), f32::from(visible.origin.y), f32::from(visible.size.width), f32::from(visible.size.height)],
    })
}

/// A size in results.
pub fn size_json(size: Size<Pixels>) -> Value {
    serde_json::json!([f32::from(size.width), f32::from(size.height)])
}
