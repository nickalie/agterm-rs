//! The quick-terminal panel's life cycle on GPUI, scripted (see `agterm-gtk/src/quick.rs` for what the GTK build
//! does): a layer surface on the overlay layer, anchored to the top edge and pushed down by a top margin so it sits
//! centered, keyboard on demand, no exclusive zone, no output named (the compositor puts it on its current output);
//! the zoom anchors it to all four edges. Where the compositor has no layer-shell, an undecorated normal window.
//!
//! GPUI cannot hide a window or re-anchor a layer surface, so hiding closes the window and showing or zooming opens a
//! new one; in agterm-gpui the terminal lives in the model (agterm-term), not in the window, so nothing is lost.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::layer_shell::{Anchor, KeyboardInteractivity, Layer, LayerShellNotSupportedError, LayerShellOptions};
use gpui_kit::*;
use serde_json::json;

use crate::{niri, report};

/// The layer surface's namespace, for compositor rules; the GTK build uses `agterm-quick-terminal`.
const NAMESPACE: &str = "agterm-quick-terminal-spike";
const APP_ID: &str = "io.github.melonamin.agterm.SpikeT3";
/// How long a step waits for the compositor before it reads the outcome.
const SETTLE: Duration = Duration::from_millis(400);
/// The longest a show may take to become focused.
const SHOW_DEADLINE: Duration = Duration::from_secs(3);

thread_local! {
    /// The layer surface's keyboard interactivity: on demand, as the GTK build; one probe tries exclusive.
    static INTERACTIVITY: Cell<KeyboardInteractivity> = const { Cell::new(KeyboardInteractivity::OnDemand) };
}

/// Where the panel sits.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Placement {
    /// Centered at the quick-terminal size of the output it is sized against.
    Centered,
    /// Filling the output's usable area (the quick zoom).
    Filled,
}

/// What hosts the panel: chosen by the first open, as the GTK build chooses once per panel.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Host {
    LayerShell,
    Window,
}

/// The quick-terminal size on an output of `size`: 90% capped at 1100x700 (`agterm_core::quick_metrics::panel_size`
/// with no user setting).
fn panel_size(output: Size<Pixels>) -> Size<Pixels> {
    size(
        px((f32::from(output.width) * 0.9).min(1100.)),
        px((f32::from(output.height) * 0.9).min(700.)),
    )
}

/// The layer surface's options for `placement` on an output of `output` size.
fn layer_options(placement: Placement, output: Size<Pixels>) -> (LayerShellOptions, Size<Pixels>) {
    let (anchor, margin, size) = match placement {
        Placement::Centered => {
            let panel = panel_size(output);
            let top = ((output.height - panel.height) / 2.).max(px(0.));
            (Anchor::TOP, Some((top, px(0.), px(0.), px(0.))), panel)
        }
        // a zero size along an axis anchored at both edges takes the whole axis
        Placement::Filled => (Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT, None, size(px(0.), px(0.))),
    };
    let options = LayerShellOptions {
        namespace: NAMESPACE.into(),
        layer: Layer::Overlay,
        anchor,
        exclusive_zone: Some(px(0.)),
        exclusive_edge: None,
        margin,
        keyboard_interactivity: INTERACTIVITY.with(Cell::get),
    };
    (options, size)
}

/// The window options of the panel on `host`.
/// `display` sizes the panel; the layer surface names it as its output only when `named` (else the compositor
/// chooses, as on a show).
fn options(host: Host, placement: Placement, display: Option<&Rc<dyn PlatformDisplay>>, named: bool) -> WindowOptions {
    let output = display.map_or(size(px(1920.), px(1080.)), |display| display.bounds().size);
    let base = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        is_movable: false,
        is_resizable: false,
        display_id: None,
        window_background: WindowBackgroundAppearance::Transparent,
        app_id: Some(APP_ID.into()),
        window_decorations: Some(WindowDecorations::Client),
        ..WindowOptions::default()
    };
    match host {
        Host::LayerShell => {
            let (layer, size) = layer_options(placement, output);
            WindowOptions {
                kind: WindowKind::LayerShell(layer),
                window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size })),
                display_id: display.filter(|_| named).map(|display| display.id()),
                ..base
            }
        }
        Host::Window => {
            let bounds = display.map_or_else(
                || Bounds { origin: point(px(0.), px(0.)), size: panel_size(output) },
                |display| Bounds::centered_at(display.bounds().center(), panel_size(output)),
            );
            WindowOptions {
                kind: WindowKind::Normal,
                window_bounds: Some(match placement {
                    Placement::Centered => WindowBounds::Windowed(bounds),
                    Placement::Filled => WindowBounds::Maximized(bounds),
                }),
                ..base
            }
        }
    }
}

/// The open panel window and what it has seen.
struct Shown {
    handle: AnyWindowHandle,
    opened_at: Instant,
    /// When the window first became active (held the keyboard).
    activated_at: Rc<Cell<Option<Instant>>>,
    /// Whether it lost the keyboard after holding it.
    deactivated: Rc<Cell<bool>>,
}

/// Opens the panel on `host`; falls back from layer-shell to a window when the compositor has none, returning the host
/// that took it.
fn open(cx: &mut App, host: Host, placement: Placement, display: Option<&Rc<dyn PlatformDisplay>>, named: bool) -> anyhow::Result<(Host, Shown)> {
    let activated_at = Rc::new(Cell::new(None));
    let deactivated = Rc::new(Cell::new(false));
    let opened_at = Instant::now();
    let build = {
        let activated_at = Rc::clone(&activated_at);
        let deactivated = Rc::clone(&deactivated);
        move |window: &mut Window, cx: &mut App| {
            cx.new(|cx: &mut Context<report::Label>| {
                cx.observe_window_activation(window, move |_, window, _| {
                    if window.is_window_active() {
                        if activated_at.get().is_none() {
                            activated_at.set(Some(Instant::now()));
                        }
                    } else if activated_at.get().is_some() {
                        deactivated.set(true);
                    }
                })
                .detach();
                report::Label("agterm quick terminal (GPUI spike)".into())
            })
        }
    };
    match cx.open_window(options(host, placement, display, named), build.clone()) {
        Ok(handle) => Ok((host, Shown { handle: handle.into(), opened_at, activated_at, deactivated })),
        Err(error) if host == Host::LayerShell && error.downcast_ref::<LayerShellNotSupportedError>().is_some() => {
            report::info("layer_shell_unsupported", json!({ "error": error.to_string() }));
            let handle = cx.open_window(options(Host::Window, placement, display, named), build)?;
            Ok((Host::Window, Shown { handle: handle.into(), opened_at, activated_at, deactivated }))
        }
        Err(error) => Err(error),
    }
}

/// The window's viewport, activity and display, read on the main thread.
fn inspect(cx: &mut AsyncApp, shown: &Shown) -> serde_json::Value {
    shown
        .handle
        .update(cx, |_, window, cx| {
            json!({
                "viewport": report::size_json(window.viewport_size()),
                "active": window.is_window_active(),
                "display": window.display(cx).map(|display| report::display_json(display.as_ref())),
            })
        })
        .unwrap_or_else(|error| json!({ "error": error.to_string() }))
}

/// Waits until the panel holds the keyboard or `SHOW_DEADLINE` passes; the time it took, if it did.
async fn wait_active(cx: &mut AsyncApp, shown: &Shown) -> Option<Duration> {
    let deadline = Instant::now() + SHOW_DEADLINE;
    while Instant::now() < deadline {
        if let Some(at) = shown.activated_at.get() {
            return Some(at - shown.opened_at);
        }
        cx.background_executor().timer(Duration::from_millis(10)).await;
    }
    None
}

async fn settle(cx: &mut AsyncApp) {
    cx.background_executor().timer(SETTLE).await;
}

fn close(cx: &mut AsyncApp, shown: &Shown) {
    let _ = shown.handle.update(cx, |_, window, _| window.remove_window());
}

/// The panel's size matches `expected` within a pixel.
fn fits(state: &serde_json::Value, expected: Size<Pixels>) -> bool {
    let viewport = &state["viewport"];
    let close = |index: usize, want: Pixels| viewport[index].as_f64().is_some_and(|got| (got - f64::from(f32::from(want))).abs() <= 1.0);
    close(0, expected.width) && close(1, expected.height)
}

/// One show: opens, waits for focus, reads the window and the compositor's view of it.
async fn show(cx: &mut AsyncApp, host: Host, placement: Placement, display: Option<Rc<dyn PlatformDisplay>>, named: bool, step: &str) -> anyhow::Result<(Host, Shown)> {
    let (host, shown) = cx.update(|cx| open(cx, host, placement, display.as_ref(), named))?;
    let took = wait_active(cx, &shown).await;
    settle(cx).await;
    let state = inspect(cx, &shown);
    let layer = niri::layer(NAMESPACE);
    report::result(
        &format!("{step}_focused"),
        took.is_some(),
        json!({ "host": format!("{host:?}"), "ms_to_focus": took.map(|took| took.as_secs_f64() * 1000.), "window": state, "niri_layer": layer, "niri_focused_window": niri::focused_window() }),
    );
    Ok((host, shown))
}

/// The whole scripted life cycle.
async fn script(cx: &mut AsyncApp) -> anyhow::Result<()> {
    let displays = cx.update(|cx| cx.displays());
    report::info("displays", json!(displays.iter().map(|display| report::display_json(display.as_ref())).collect::<Vec<_>>()));
    report::info("niri_focused_output_before", json!(niri::focused_output()));
    let estimate = displays.first().cloned();
    // GPUI builds `WindowKind::LayerShell` on X11 too, as an ordinary window: only Wayland tries the layer surface (and
    // falls back on `LayerShellNotSupportedError`)
    let compositor = cx.update(|cx| cx.compositor_name());
    report::info("compositor", json!(compositor));
    let first_host = if compositor == "Wayland" { Host::LayerShell } else { Host::Window };

    // show: the compositor chooses the output; sized against the first display until it says where it landed
    let (host, shown) = show(cx, first_host, Placement::Centered, estimate.clone(), false, "show").await?;
    let state = inspect(cx, &shown);
    if host == Host::LayerShell {
        let layer = niri::layer(NAMESPACE);
        let attrs_ok = !niri::available()
            || layer.as_ref().is_some_and(|layer| layer["layer"] == "Overlay" && layer["keyboard_interactivity"] == "OnDemand");
        report::result("layer_attributes", attrs_ok, json!({ "niri_layer": layer }));
        // the landing output, as GPUI reports it (wl_surface.enter), against the one niri reports
        let landed = state["display"]["debug"].as_str().unwrap_or_default().to_owned();
        let niri_output = layer.as_ref().and_then(|layer| layer["output"].as_str().map(str::to_owned));
        report::result(
            "landing_output_reported",
            niri_output.as_ref().is_none_or(|name| landed.contains(name.as_str())),
            json!({ "gpui_display": state["display"], "niri_output": niri_output }),
        );
    }
    if let Some(display) = &estimate {
        let expected = panel_size(display.bounds().size);
        report::result("show_size", fits(&state, expected), json!({ "expected": report::size_json(expected), "window": state }));
    }

    // the compositor chose another output than the estimate: GPUI cannot re-anchor or re-margin a layer surface, so
    // the refit is a new surface named on the output it landed on
    let landed_id = state["display"]["id"].as_u64();
    let estimate_id = estimate.as_ref().map(|display| u64::from(display.id()));
    let shown = if host == Host::LayerShell && landed_id.is_some() && landed_id != estimate_id {
        let landed = displays.iter().find(|display| Some(u64::from(display.id())) == landed_id).cloned();
        close(cx, &shown);
        let refit_started = Instant::now();
        let (_, refitted) = show(cx, host, Placement::Centered, landed.clone(), true, "refit_on_landing").await?;
        let state = inspect(cx, &refitted);
        let expected = landed.as_ref().map(|display| panel_size(display.bounds().size)).unwrap_or_default();
        report::result("refit_size", fits(&state, expected), json!({ "expected": report::size_json(expected), "window": state, "ms_including_settle": refit_started.elapsed().as_secs_f64() * 1000. }));
        refitted
    } else {
        report::info("refit_on_landing", json!({ "needed": false, "landed": landed_id, "estimate": estimate_id }));
        shown
    };

    // resize while shown: 60% of the first display
    if let Some(display) = &estimate {
        let target = size(display.bounds().size.width * 0.6, display.bounds().size.height * 0.6);
        let _ = shown.handle.update(cx, |_, window, _| window.resize(target));
        settle(cx).await;
        let state = inspect(cx, &shown);
        report::result("resize", fits(&state, target), json!({ "expected": report::size_json(target), "window": state }));
    }

    // hide: the window goes; the compositor drops the surface
    let hide_started = Instant::now();
    close(cx, &shown);
    settle(cx).await;
    let gone = niri::layer(NAMESPACE).is_none();
    report::result("hide", !niri::available() || gone, json!({ "niri_layer_gone": gone, "ms": hide_started.elapsed().as_secs_f64() * 1000. }));

    // show again (a new window on the same host)
    let (_, shown) = show(cx, host, Placement::Centered, estimate.clone(), false, "show_again").await?;
    close(cx, &shown);
    settle(cx).await;

    // zoom: fills the output's usable area
    let (_, shown) = show(cx, host, Placement::Filled, estimate.clone(), false, "zoom").await?;
    let state = inspect(cx, &shown);
    if let Some(display) = &estimate {
        let visible = display.visible_bounds().size;
        report::info("zoom_size", json!({ "window": state, "display_bounds": report::display_json(display.as_ref()), "usable": report::size_json(visible) }));
    }
    close(cx, &shown);
    settle(cx).await;

    // one show per output, the output named
    for (index, display) in displays.iter().enumerate() {
        let step = format!("output_{index}");
        let (_, shown) = show(cx, host, Placement::Centered, Some(display.clone()), true, &step).await?;
        let state = inspect(cx, &shown);
        let expected = panel_size(display.bounds().size);
        let on_it = state["display"]["id"].as_u64() == Some(u64::from(display.id()));
        report::result(&format!("{step}_placed"), on_it && fits(&state, expected), json!({ "wanted": report::display_json(display.as_ref()), "expected": report::size_json(expected), "window": state, "niri_layer": niri::layer(NAMESPACE) }));
        close(cx, &shown);
        settle(cx).await;
        // shown on an output niri does not focus, an on-demand surface may not get the keyboard: try exclusive
        if host == Host::LayerShell && shown.activated_at.get().is_none() {
            INTERACTIVITY.with(|value| value.set(KeyboardInteractivity::Exclusive));
            let (_, exclusive) = show(cx, host, Placement::Centered, Some(display.clone()), true, &format!("{step}_exclusive")).await?;
            INTERACTIVITY.with(|value| value.set(KeyboardInteractivity::OnDemand));
            close(cx, &exclusive);
            settle(cx).await;
        }
    }

    // focus loss: a toplevel of ours opens over the shown panel; the panel must see it lose the keyboard
    let (_, shown) = show(cx, host, Placement::Centered, estimate.clone(), false, "focus_loss_setup").await?;
    let decoy = cx.update(|cx| cx.open_window(crate::activation::toplevel_options("spike-focus-thief", APP_ID), |_, cx| cx.new(|_| report::Label("focus thief".into()))))?;
    settle(cx).await;
    report::result("focus_loss_observed", shown.deactivated.get(), json!({ "panel": inspect(cx, &shown), "niri_focused_window": niri::focused_window() }));
    let _ = decoy.update(cx, |_, window, _| window.remove_window());
    close(cx, &shown);
    settle(cx).await;
    Ok(())
}

pub fn run() {
    application().with_quit_mode(QuitMode::Explicit).run(|cx| {
        cx.spawn(async move |cx| {
            if let Err(error) = script(cx).await {
                report::result("script", false, json!({ "error": format!("{error:#}") }));
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
