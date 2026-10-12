//! `agterm`, the GTK4/libadwaita frontend: one window hosting one libghostty terminal for now.
//!
//! Startup order matters: the environment is settled while the process has one thread (GDK's GL switch before GTK
//! starts, ghostty's resources dir), GTK reads its switch at display init, the switch is put back on activate, and
//! only then is libghostty initialised, which snapshots the environment its shells inherit.

mod clipboard;
mod ghostty_app;
mod input;
mod surface;

use std::cell::RefCell;
use std::ffi::OsString;

use adw::prelude::*;
use gtk::glib;
use tracing_subscriber::EnvFilter;

use crate::surface::{SurfaceSpec, TerminalSurface};

/// The application id the Linux fork registered, kept so this build can take over its state.
const DEFAULT_APP_ID: &str = "io.github.melonamin.agterm";
const APP_ID_ENV: &str = "AGTERM_APP_ID";
const LOG_ENV: &str = "AGTERM_LOG";

/// GDK's switch that keeps GL areas on desktop OpenGL, and the flag to add, for a GTK minor version: libghostty
/// renders through desktop GL only, and GDK creates every area's context to share the display's, so a display that
/// picked GLES (NVIDIA's EGL default) leaves no terminal a usable context. `None` when the flag is already there or
/// GTK has no switch (before 4.14).
fn gdk_gl_override(gtk_minor: u32, current: impl Fn(&str) -> Option<String>) -> Option<(&'static str, String)> {
    let (name, flag) = match gtk_minor {
        16.. => ("GDK_DISABLE", "gles-api"),
        14 | 15 => ("GDK_DEBUG", "gl-disable-gles"),
        _ => return None,
    };
    let existing = current(name).unwrap_or_default();
    if existing.split([',', ':', ' ']).any(|item| item == flag) {
        return None;
    }
    let value = if existing.is_empty() {
        flag.to_owned()
    } else {
        format!("{existing},{flag}")
    };
    Some((name, value))
}

/// The user's value of the GDK switch this process overrode, put back before any shell can inherit the override.
struct GlEnvRestore {
    name: &'static str,
    previous: Option<OsString>,
}

impl GlEnvRestore {
    /// # Safety
    ///
    /// Mutates the process environment: call it while the process has one thread.
    unsafe fn prefer_desktop_gl() -> Option<Self> {
        let current = |name: &str| std::env::var(name).ok();
        let (name, value) = gdk_gl_override(gtk::minor_version(), current)?;
        let previous = std::env::var_os(name);
        // SAFETY: single-threaded per the caller's contract.
        unsafe { std::env::set_var(name, value) };
        Some(Self { name, previous })
    }

    fn restore(self) {
        match self.previous {
            // SAFETY: GTK's own threads exist by now. The variable already exists, so glibc overwrites its slot or
            // shifts entries down under its environment lock without reallocating the environ array, and it never
            // frees the strings: a concurrent getenv sees the old or the new value. libghostty takes its snapshot
            // after this.
            Some(value) => unsafe { std::env::set_var(self.name, value) },
            // SAFETY: as above.
            None => unsafe { std::env::remove_var(self.name) },
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env(LOG_ENV).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn app_id() -> String {
    std::env::var(APP_ID_ENV)
        .ok()
        .filter(|id| gtk::gio::Application::id_is_valid(id))
        .unwrap_or_else(|| DEFAULT_APP_ID.to_owned())
}

fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let dark = adw::StyleManager::default().is_dark();
    if let Err(err) = ghostty_app::start(dark) {
        tracing::error!(%err, "libghostty did not start");
        app.quit();
        return;
    }
    let terminal = TerminalSurface::new(SurfaceSpec::default());
    let header = adw::HeaderBar::new();
    let content = adw::ToolbarView::new();
    content.add_top_bar(&header);
    content.set_content(Some(terminal.widget()));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("agterm")
        .default_width(960)
        .default_height(640)
        .content(&content)
        .build();
    let titled = window.downgrade();
    terminal.connect_title(move |title| {
        if let Some(window) = titled.upgrade() {
            window.set_title(Some(title));
        }
    });
    let closing = window.downgrade();
    terminal.connect_exit(move || {
        if let Some(window) = closing.upgrade() {
            window.close();
        }
    });
    window.present();
    terminal.grab_focus();
}

fn main() -> glib::ExitCode {
    // SAFETY: first thing in main, while the process has one thread.
    let gl_env = unsafe { GlEnvRestore::prefer_desktop_gl() };
    // SAFETY: as above.
    unsafe { ghostty_app::export_resources_dir() };
    init_tracing();

    let app = adw::Application::builder().application_id(app_id()).build();
    let gl_env = RefCell::new(gl_env);
    app.connect_activate(move |app| {
        if let Some(restore) = gl_env.borrow_mut().take() {
            restore.restore();
        }
        activate(app);
    });
    // agterm's arguments are its own, not GApplication's
    let argv0: Vec<String> = std::env::args().take(1).collect();
    let status = app.run_with_args(&argv0);
    ghostty_app::shutdown();
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gtk_4_16_and_later_disable_the_gles_api() {
        assert_eq!(
            gdk_gl_override(16, |_| None),
            Some(("GDK_DISABLE", "gles-api".to_owned()))
        );
        assert_eq!(
            gdk_gl_override(24, |_| Some("vulkan".to_owned())),
            Some(("GDK_DISABLE", "vulkan,gles-api".to_owned()))
        );
    }

    #[test]
    fn gtk_4_14_uses_the_debug_flag() {
        assert_eq!(
            gdk_gl_override(14, |_| None),
            Some(("GDK_DEBUG", "gl-disable-gles".to_owned()))
        );
    }

    #[test]
    fn an_existing_flag_or_an_old_gtk_needs_nothing() {
        assert_eq!(gdk_gl_override(18, |_| Some("color-mgmt,gles-api".to_owned())), None);
        assert_eq!(gdk_gl_override(14, |_| Some("opengl:gl-disable-gles".to_owned())), None);
        assert_eq!(gdk_gl_override(12, |_| None), None);
    }
}
