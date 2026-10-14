//! What the compositor says, through `niri msg --json` on the niri named by `NIRI_SOCKET` (the drive script points it
//! at the compositor under test). Read-only queries; nothing here acts on any window.

use serde_json::Value;

/// Whether a niri is there to ask.
pub fn available() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some()
}

fn query(what: &str) -> Option<Value> {
    if !available() {
        return None;
    }
    let output = std::process::Command::new("niri").args(["msg", "--json", what]).output().ok()?;
    serde_json::from_slice(&output.stdout).ok()
}

/// The layer surface with `namespace`, as niri lists it (layer, output, keyboard interactivity).
pub fn layer(namespace: &str) -> Option<Value> {
    query("layers")?
        .as_array()?
        .iter()
        .find(|layer| layer["namespace"] == namespace)
        .cloned()
}

/// The focused window (title, app id), `Null` when a layer surface or nothing holds the keyboard.
pub fn focused_window() -> Option<Value> {
    query("focused-window")
}

/// The focused window's title: niri's where a niri is under test, else X11's input focus (through `xdotool`).
pub fn focused_title() -> Option<String> {
    if available() {
        return focused_window()?["title"].as_str().map(str::to_owned);
    }
    std::env::var_os("DISPLAY")?;
    let output = std::process::Command::new("xdotool").args(["getwindowfocus", "getwindowname"]).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The output niri considers focused.
pub fn focused_output() -> Option<Value> {
    query("focused-output").map(|output| output["name"].clone())
}
