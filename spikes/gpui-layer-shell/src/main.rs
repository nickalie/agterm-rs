//! Spike: the quick terminal on GPUI's `WindowKind::LayerShell` and window activation through an xdg-activation token
//! handed over by a second launch (docs/plans/20261014-gpui-frontend.md, Task 3). Throwaway code; the deliverable is
//! the report in docs/spikes/20261014-gpui-layer-shell.md.
//!
//! Modes (one per process, driven by `drive.py`):
//! - `quick`: the quick-terminal panel's life cycle, scripted: show, resize, hide, show again, zoom, one show per
//!   output, focus loss; layer-shell where the compositor has it, else an undecorated normal window.
//! - `primary APP_ID`: one normal window plus GApplication's D-Bus side; `Activate` with an `activation-token` in its
//!   platform data raises and focuses the window with that token.
//! - `window TITLE`: one plain window (the decoy that takes focus away from the primary).
//!
//! Every result is one `RESULT {json}` line on stdout.

mod activation;
mod gapplication;
mod niri;
mod quick;
mod report;

use gpui_kit::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or_default().to_owned();
    let rest: Vec<String> = args.into_iter().skip(1).collect();
    match mode.as_str() {
        "quick" => quick::run(),
        "primary" => activation::run_primary(rest.first().cloned().unwrap_or_else(|| "io.github.melonamin.agterm.SpikeT3".into())),
        "window" => run_window(rest.first().cloned().unwrap_or_else(|| "spike-window".into())),
        _ => {
            eprintln!("usage: gpui-layer-shell-spike quick | primary APP_ID | window TITLE");
            std::process::exit(2);
        }
    }
}

/// A plain titled window that stays until it is closed.
fn run_window(title: String) {
    application().with_quit_mode(QuitMode::Explicit).run(move |cx| {
        let options = activation::toplevel_options(&title, "io.github.melonamin.agterm.SpikeT3.Decoy");
        let opened = cx.open_window(options, |_, cx| cx.new(|_| report::Label(title.clone().into())));
        report::result("window_opened", opened.is_ok(), serde_json::json!({ "title": title, "error": opened.err().map(|e| e.to_string()) }));
        cx.on_window_closed(|cx, _| cx.quit()).detach();
    });
}
