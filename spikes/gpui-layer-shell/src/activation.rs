//! Window activation with an xdg-activation token a second launch hands over through GApplication's platform data.
//!
//! GPUI consumes `XDG_ACTIVATION_TOKEN` from its own environment for its first toplevel only, and its
//! `Window::activate_window` asks the compositor for a fresh token with the last pointer serial (which niri refuses
//! to focus with while another client holds the keyboard). It has no call that activates a window with a token from
//! elsewhere, so this binds `xdg_activation_v1` on GPUI's own `wl_display` (a second event queue over the same
//! connection, through libwayland) and calls `activate(token, surface)` on the window's `wl_surface`, both taken from
//! the window's raw handles. That is the whole of what a GPUI patch would add (`Window::activate_with_token`).

use std::ffi::c_void;
use std::time::{Duration, Instant};

use futures::StreamExt;
use gpui_kit::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use serde_json::json;
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;

use crate::gapplication::{self, Request};
use crate::{niri, report};

/// A plain toplevel's options.
pub fn toplevel_options(title: &str, app_id: &str) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions { title: Some(title.to_owned().into()), ..TitlebarOptions::default() }),
        window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(0.), px(0.)), size: size(px(640.), px(400.)) })),
        app_id: Some(app_id.to_owned()),
        ..WindowOptions::default()
    }
}

struct Queue;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Queue {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<XdgActivationV1, ()> for Queue {
    fn event(_: &mut Self, _: &XdgActivationV1, _: <XdgActivationV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// Activates `window` with `token` (see the module docs).
pub fn activate_with_token(window: &Window, token: &str) -> anyhow::Result<()> {
    let RawDisplayHandle::Wayland(display) = HasDisplayHandle::display_handle(window)?.as_raw() else {
        anyhow::bail!("not a Wayland window");
    };
    let RawWindowHandle::Wayland(surface) = HasWindowHandle::window_handle(window)?.as_raw() else {
        anyhow::bail!("not a Wayland window");
    };
    // SAFETY: GPUI's live `wl_display`; the backend made from it does not take ownership (a foreign display).
    let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
    let connection = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<Queue>(&connection)?;
    let activation: XdgActivationV1 = globals.bind(&queue.handle(), 1..=1, ())?;
    // SAFETY: the window's live `wl_surface`, a `wl_surface` proxy of the same connection.
    let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.surface.as_ptr().cast::<c_void>().cast()) }?;
    let surface = WlSurface::from_id(&connection, id)?;
    activation.activate(token.to_owned(), &surface);
    activation.destroy();
    queue.roundtrip(&mut Queue)?;
    Ok(())
}

/// The primary: one window titled after the app, GApplication's D-Bus side, and the activation checks.
pub fn run_primary(app_id: String) {
    let (sender, mut requests) = futures::channel::mpsc::unbounded();
    let served = gapplication::serve(&app_id, sender);
    report::result("gapplication_name_owned", served.is_ok(), json!({ "app_id": app_id, "error": served.as_ref().err().map(ToString::to_string) }));
    let Ok(connection) = served else {
        return;
    };
    let title = format!("{app_id} primary");
    application().with_quit_mode(QuitMode::Explicit).run(move |cx| {
        // the bus name lives as long as the process
        let _ = Box::leak(Box::new(connection.clone()));
        let window = cx.open_window(toplevel_options(&title, &app_id), |_, cx| cx.new(|_| report::Label(title.clone().into())));
        let Ok(window) = window else {
            report::result("primary_window", false, json!({ "error": window.err().map(|e| e.to_string()) }));
            cx.quit();
            return;
        };
        cx.spawn(async move |cx| {
            while let Some(request) = requests.next().await {
                handle(cx, window.into(), &title, request).await;
            }
        })
        .detach();
    });
}

async fn handle(cx: &mut AsyncApp, window: AnyWindowHandle, title: &str, request: Request) {
    match request {
        Request::Activate { token, keys } => {
            report::info("activate_received", json!({ "platform_data_keys": keys, "token": token.is_some() }));
            let started = Instant::now();
            let outcome = match &token {
                // X11 has no token: GPUI's `_NET_ACTIVE_WINDOW` request (with CURRENT_TIME) is all there is
                Some(_) if cx.update(|cx| cx.compositor_name()) == "X11" => window.update(cx, |_, window, _| window.activate_window()),
                Some(token) => window.update(cx, |_, window, _| activate_with_token(window, token)).and_then(|result| result),
                None => window.update(cx, |_, window, _| window.activate_window()),
            };
            let focused = wait_focused(cx, title).await;
            report::result(
                if token.is_some() { "activate_with_token" } else { "activate_without_token" },
                outcome.is_ok() && focused.is_some(),
                json!({ "error": outcome.err().map(|e| format!("{e:#}")), "ms_to_focus": focused.map(|_| started.elapsed().as_secs_f64() * 1000.), "niri_focused_window": niri::focused_window() }),
            );
        }
        Request::Action(name) if name == "raise-without-token" => {
            // GPUI's own call: a fresh token from the last pointer serial
            let started = Instant::now();
            let _ = window.update(cx, |_, window, _| window.activate_window());
            let focused = wait_focused(cx, title).await;
            report::result("gpui_activate_window", focused.is_some(), json!({ "ms_to_focus": focused.map(|_| started.elapsed().as_secs_f64() * 1000.), "niri_focused_window": niri::focused_window() }));
        }
        Request::Action(name) if name == "quit" => cx.update(|cx| cx.quit()),
        Request::Action(name) => report::info("unknown_action", json!({ "name": name })),
    }
}

/// Waits up to a second for the compositor (niri) or the X server to report `title` focused.
async fn wait_focused(cx: &mut AsyncApp, title: &str) -> Option<()> {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if niri::focused_title().as_deref() == Some(title) {
            return Some(());
        }
        cx.background_executor().timer(Duration::from_millis(20)).await;
    }
    None
}
