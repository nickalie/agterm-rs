//! `agterm`, the GTK4/libadwaita frontend: a window of a session sidebar beside a deck of libghostty terminals, one
//! per session, and the control socket.
//!
//! Startup order matters: the environment is settled while the process has one thread (GDK's GL switch before GTK
//! starts, ghostty's resources dir), GTK reads its switch at display init, the switch is put back on activate, and
//! only then is libghostty initialised, which snapshots the environment its shells inherit.

mod app;
mod app_menu;
mod ask_dialog;
mod background;
mod clipboard;
mod dashboard;
mod deck;
mod ghostty_app;
#[cfg(test)]
mod gtk_test;
mod hook_runner;
mod html_overlay;
mod hud;
mod input;
mod keymap_actions;
mod keymap_dispatch;
mod logging;
mod main_loop;
mod modal;
mod notifications;
mod overlay;
mod pane;
mod picker;
mod quick;
mod row_icon;
mod sidebar;
mod signals;
mod split;
mod state;
mod status_glyph;
mod surface;
mod theme;
mod window;
mod windows;

use std::cell::RefCell;
use std::ffi::OsString;

use adw::prelude::*;
use agterm_core::paths::ProcessEnv;
use gtk::glib;

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

fn main() -> glib::ExitCode {
    // SAFETY: first thing in main, while the process has one thread.
    let gl_env = unsafe { GlEnvRestore::prefer_desktop_gl() };
    // SAFETY: as above.
    unsafe { ghostty_app::export_resources_dir() };
    logging::init(&ProcessEnv);

    let app = adw::Application::builder()
        .application_id(app::app_id(&ProcessEnv))
        .build();
    // the primary instance only: a launch forwarded to a running agterm exits on its own
    app.connect_startup(signals::quit_on_sigterm);
    let gl_env = RefCell::new(gl_env);
    app.connect_activate(move |app| {
        if let Some(restore) = gl_env.borrow_mut().take() {
            restore.restore();
        }
        app::activate(app);
    });
    // agterm's arguments are its own, not GApplication's
    let argv0: Vec<String> = std::env::args().take(1).collect();
    let status = app.run_with_args(&argv0);
    app::shutdown();
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
