//! The libghostty app on the GTK main thread: its resources, its config, the runtime callbacks and the tick, after
//! upstream's `GhosttyApp.swift` and `GhosttyCallbacks.swift`.
//!
//! Wakeups arrive from any thread and coalesce into one idle-priority tick on the main loop: the flag clears before
//! the tick, so a wakeup during it schedules another instead of being lost. Idle priority keeps a stream of ticks from
//! starving GTK's input and redraw sources.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use agterm_core::link_policy::{self, ClickOrigin, Route};
use agterm_core::model::SearchReport;
use agterm_core::paths::{self, Env, ProcessEnv};
use agterm_core::persist::settings::LinkOpenMode;
use agterm_ghostty::{
    Action, App, ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, ColorScheme,
    Config, PendingRequest, Runtime, Surface, SurfaceKey, SurfaceOptions, Target, keys,
};
use gtk::prelude::*;
use gtk::{gio, glib};

use crate::{clipboard, surface};

/// The variable libghostty reads its resources (shell integration, themes) from; it derives `TERMINFO` from it.
const RESOURCES_ENV: &str = "GHOSTTY_RESOURCES_DIR";
const DEFAULTS: &str = include_str!("ghostty-defaults.conf");
/// Shell integration features whose ssh wrappers call a `ghostty` CLI agterm does not ship (upstream #463).
const UNSUPPORTED_SHELL_FEATURES: [&str; 2] = ["ssh-env", "ssh-terminfo"];
/// `shell-integration-features` flags in libghostty's field order: the index is the bit.
const SHELL_FEATURES: [&str; 6] = ["cursor", "sudo", "title", "ssh-env", "ssh-terminfo", "path"];

static RESOURCES_RESOLVED: OnceLock<bool> = OnceLock::new();
static TICK_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// The running app and what building its config learned.
struct GhosttyApp {
    app: App,
    /// The app's config, read for the colors a rendered watermark defaults to; kept for the reloads to come.
    config: Config,
    /// The user's static `title`, applied by agterm in place of program titles (libghostty would drop them all).
    static_title: Option<String>,
    scheme: ColorScheme,
}

thread_local! {
    static GHOSTTY: RefCell<Option<Rc<GhosttyApp>>> = const { RefCell::new(None) };
}

/// The app, cloned out so no borrow is held while libghostty calls back.
fn current() -> Option<Rc<GhosttyApp>> {
    GHOSTTY.with(|slot| slot.borrow().clone())
}

/// Candidate resource directories, highest priority first: the staged layout beside the binary (`bin/` and
/// `share/ghostty`), then the vendored prefix this build linked (a `cargo run`), then a system ghostty package.
fn resource_candidates(exe: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(bin) = exe.and_then(Path::parent) {
        candidates.push(bin.join("../share/ghostty"));
    }
    candidates.push(Path::new(env!("AGTERM_GHOSTTY_PREFIX")).join("share/ghostty"));
    candidates.push(PathBuf::from("/usr/share/ghostty"));
    candidates
}

/// The first absolute candidate with `shell-integration` in it and the compiled terminfo beside it
/// (`../terminfo/x/xterm-ghostty`): libghostty sets `TERMINFO` to that sibling, so a half-present set breaks `TERM`.
fn resolve_resources(candidates: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates
        .iter()
        .filter(|dir| dir.is_absolute())
        .find(|dir| exists(&dir.join("shell-integration")) && exists(&dir.join("../terminfo/x/xterm-ghostty")))
        .cloned()
}

/// Exports `GHOSTTY_RESOURCES_DIR` from this build's own candidates, ignoring an inherited value (a stale one would
/// shadow a complete set), or removes it when none resolves.
///
/// # Safety
///
/// Mutates the process environment: call it while the process has one thread, before libghostty is initialised.
pub unsafe fn export_resources_dir() {
    let exe = std::env::current_exe().ok();
    let resolved = resolve_resources(&resource_candidates(exe.as_deref()), Path::exists);
    if let Some(dir) = &resolved {
        // SAFETY: single-threaded per the caller's contract.
        unsafe { std::env::set_var(RESOURCES_ENV, dir) };
    } else {
        // SAFETY: single-threaded per the caller's contract.
        unsafe { std::env::remove_var(RESOURCES_ENV) };
    }
    RESOURCES_RESOLVED.get_or_init(|| resolved.is_some());
}

/// The built-in defaults; without ghostty's terminfo the terminal claims `xterm-256color` instead of its own.
fn defaults_text(resources_resolved: bool) -> String {
    if resources_resolved {
        DEFAULTS.to_owned()
    } else {
        format!("{DEFAULTS}\nterm = xterm-256color\n")
    }
}

/// A `shell-integration-features` value restating every flag from the resolved `bits` with the unsupported ones off.
/// All six are named: libghostty re-parses the key from its defaults, so an omitted flag would be reset.
fn shell_features_override(bits: u32) -> String {
    SHELL_FEATURES
        .iter()
        .enumerate()
        .map(|(bit, name)| {
            let on = bits & (1 << bit) != 0 && !UNSUPPORTED_SHELL_FEATURES.contains(name);
            if on { (*name).to_owned() } else { format!("no-{name}") }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// The app's config, upstream's stack in order: built-in defaults, the agterm-scoped `ghostty.conf`, the Settings
/// window's `ghostty-settings.conf`, their `config-file` includes; then the ssh features forced off and the static
/// title taken out. The user's global ghostty config joins with the settings model (`inheritGlobalGhosttyConfig`,
/// off by default).
fn build_config(env: &dyn Env, resources_resolved: bool) -> Result<(Config, Option<String>), agterm_ghostty::Error> {
    build_config_with(env, resources_resolved, None)
}

/// [`build_config`] with a per-surface `overlay` loaded after the includes, so it wins over all of them (upstream's
/// `loadConfig(_:extraOverlayPath:)`).
fn build_config_with(
    env: &dyn Env,
    resources_resolved: bool,
    overlay: Option<&str>,
) -> Result<(Config, Option<String>), agterm_ghostty::Error> {
    let mut config = Config::new()?;
    config.load_text(&defaults_text(resources_resolved));
    for file in [
        paths::ghostty_config_path(&paths::config_dir(env, None)),
        paths::ghostty_settings_config_path(env),
    ] {
        if file.is_file() {
            config.load_file(&file);
        }
    }
    config.load_recursive_files();
    if let Some(overlay) = overlay.filter(|overlay| !overlay.is_empty()) {
        config.load_text(overlay);
    }
    if let Some(bits) = config.get(keys::SHELL_INTEGRATION_FEATURES) {
        config.load_text(&format!(
            "shell-integration-features = {}\n",
            shell_features_override(bits)
        ));
    } else {
        tracing::warn!("could not read shell-integration-features; ssh features left as configured");
    }
    let static_title = config.get_string(keys::TITLE).filter(|title| !title.is_empty());
    if static_title.is_some() {
        config.load_text("title =\n");
    }
    config.finalize();
    for diagnostic in config.diagnostics() {
        tracing::warn!(%diagnostic, "ghostty config");
    }
    Ok((config, static_title))
}

/// Create the app once, on the main thread. A dark launch re-sides the app config before any surface exists
/// (upstream `syncLaunchColorScheme`): a surface born under a scheme the app config does not match is rebuilt from the
/// config files alone and loses its environment, initial input and command (#260).
///
/// # Errors
///
/// libghostty could not build the config or the app.
pub fn start(dark: bool) -> Result<(), agterm_ghostty::Error> {
    if current().is_some() {
        return Ok(());
    }
    let resolved = RESOURCES_RESOLVED.get().copied().unwrap_or(false);
    let (config, static_title) = build_config(&ProcessEnv, resolved)?;
    let app = App::new(&config, Rc::new(GtkRuntime), Box::new(schedule_tick))?;
    let scheme = if dark { ColorScheme::Dark } else { ColorScheme::Light };
    if dark {
        app.set_color_scheme(scheme);
        app.update_config(&config);
    }
    let ghostty = GhosttyApp {
        app,
        config,
        static_title,
        scheme,
    };
    GHOSTTY.with(|slot| *slot.borrow_mut() = Some(Rc::new(ghostty)));
    Ok(())
}

/// Release the app; libghostty frees it once its last surface is gone.
pub fn shutdown() {
    GHOSTTY.with(|slot| slot.borrow_mut().take());
}

/// Create a surface in the app, rendering into the GL context current now.
///
/// # Errors
///
/// No app is running, or libghostty refused the surface.
pub fn create_surface(key: SurfaceKey, options: &SurfaceOptions<'_>) -> Result<Surface, agterm_ghostty::Error> {
    let ghostty = current().ok_or(agterm_ghostty::Error::Null("ghostty app"))?;
    let surface = Surface::new(&ghostty.app, key, options)?;
    surface.set_color_scheme(ghostty.scheme);
    Ok(surface)
}

/// The user's static title, when the config sets one.
pub fn static_title() -> Option<String> {
    current().and_then(|ghostty| ghostty.static_title.clone())
}

/// A config for one surface: the app's stack with `overlay` loaded last (upstream's `configWithOverlay`), for
/// [`agterm_ghostty::Surface::update_config`]. `None`, logged, when libghostty cannot build it.
pub fn config_with_overlay(overlay: &str) -> Option<Config> {
    let resolved = RESOURCES_RESOLVED.get().copied().unwrap_or(false);
    build_config_with(&ProcessEnv, resolved, Some(overlay))
        .map(|(config, _)| config)
        .inspect_err(|err| tracing::warn!(%err, "could not build a terminal's config overlay"))
        .ok()
}

/// The terminals' default foreground color, from the app config.
pub fn foreground_color() -> Option<agterm_ghostty::Color> {
    current().and_then(|ghostty| ghostty.config.get(keys::FOREGROUND))
}

fn schedule_tick() {
    if TICK_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    glib::idle_add_once(|| {
        TICK_SCHEDULED.store(false, Ordering::Release);
        if let Some(ghostty) = current() {
            ghostty.app.tick();
        }
    });
}

/// The libghostty callbacks, all on the main thread.
struct GtkRuntime;

impl Runtime for GtkRuntime {
    fn action(&self, target: Target, action: Action) -> bool {
        handle_action(target, action)
    }

    fn clipboard_read_result(&self, read: &ClipboardRead) -> ClipboardReadResult {
        clipboard::read_result(read)
    }

    fn start_clipboard_read(&self, read: ClipboardRead, pending: PendingRequest) {
        clipboard::start_read(read, pending);
    }

    fn confirm_clipboard_request(&self, confirm: ClipboardConfirm, pending: PendingRequest) {
        clipboard::confirm(confirm, pending);
    }

    fn write_clipboard(
        &self,
        surface: Option<SurfaceKey>,
        clipboard: ClipboardKind,
        contents: Vec<ClipboardContent>,
        confirm: bool,
    ) {
        clipboard::write(surface, clipboard, contents, confirm);
    }

    fn close_surface(&self, key: SurfaceKey, _process_alive: bool) {
        // never inside the callback: handling the exit closes the window, which frees the surface
        glib::idle_add_local_once(move || {
            if let Some(surface) = surface::lookup(key) {
                surface.handle_process_exit();
            }
        });
    }
}

fn handle_action(target: Target, action: Action) -> bool {
    let surface = match target {
        Target::Surface(key) => surface::lookup(key),
        Target::App => None,
    };
    match action {
        Action::Render => {
            surface.inspect(surface::TerminalSurface::queue_render);
            true
        }
        Action::SetTitle(title) => {
            if let Some(surface) = surface {
                surface.apply_title(&static_title().unwrap_or(title));
            }
            true
        }
        Action::Pwd(pwd) => {
            if let Some(surface) = surface {
                surface.apply_pwd(pwd);
            }
            true
        }
        Action::MouseShape(shape) => {
            if let Some(surface) = surface {
                surface.set_mouse_shape(shape);
            }
            true
        }
        Action::MouseVisibility { hidden } => {
            if let Some(surface) = surface {
                surface.set_mouse_hidden(hidden);
            }
            true
        }
        Action::ShowChildExited { .. } => surface.is_some_and(|surface| surface.child_exited()),
        // upstream keeps the applied app config for the chrome colors; nothing reads them yet
        Action::ConfigChange(_) => true,
        Action::OpenUrl { url, .. } => {
            // always consumed: unhandled, libghostty would open any scheme itself. Opened off the callback, as
            // upstream's main-queue hop, through the link policy.
            if let Some(surface) = surface {
                glib::idle_add_local_once(move || surface.open_url(&url));
            }
            true
        }
        Action::DesktopNotification { title, body } => {
            // the strings are owned copies already; the handlers only post a task
            if let Some(surface) = surface {
                surface.desktop_notification(&title, &body);
            }
            true
        }
        Action::StartSearch { needle } => report_search(surface, &SearchReport::Started(needle)),
        Action::EndSearch => report_search(surface, &SearchReport::Ended),
        Action::SearchTotal(total) => report_search(surface, &SearchReport::Total(total)),
        Action::SearchSelected(selected) => report_search(surface, &SearchReport::Selected(selected)),
        _ => false,
    }
}

/// Hands a search report to the surface's handlers; consumed whether or not the surface still exists, as upstream.
fn report_search(surface: Option<surface::TerminalSurface>, report: &SearchReport) -> bool {
    if let Some(surface) = surface {
        surface.report_search(report);
    }
    true
}

/// Carries out what the link policy decides for a clicked terminal link (upstream's `LinkOpener.follow`): a web or
/// mail link opens with the desktop's handler, a local file is revealed in the file manager (never opened, which
/// could run it), anything else is ignored. A session overlay route opens in the browser for now: the overlay is not
/// there to take it yet, which is upstream's fallback when it cannot.
pub fn follow_link(raw: &str, mode: LinkOpenMode, origin: ClickOrigin) {
    match link_policy::route(raw, mode, origin) {
        Route::Browser(url) | Route::Overlay { url, .. } => open_uri(&url),
        Route::Reveal(path) => crate::app::reveal_in_file_manager(&path),
        Route::Ignore => tracing::debug!(raw, "link click ignored by the link policy"),
    }
}

/// Opens `uri` with the desktop's handler for its scheme, through the portal when sandboxed, without blocking; a
/// failure is only logged.
fn open_uri(uri: &str) {
    let parent = gio::Application::default()
        .and_downcast::<gtk::Application>()
        .and_then(|app| app.active_window());
    gtk::UriLauncher::new(uri).launch(parent.as_ref(), None::<&gio::Cancellable>, |result| {
        if let Err(err) = result {
            tracing::warn!(%err, "could not open the link");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_need_shell_integration_and_terminfo_beside_them() {
        let candidates = [
            PathBuf::from("relative/ghostty"),
            PathBuf::from("/stage/share/ghostty"),
            PathBuf::from("/vendor/share/ghostty"),
        ];
        let present = |set: &'static [&'static str]| move |path: &Path| set.iter().any(|p| Path::new(p) == path);
        // the stage has shell integration but no terminfo: skipped for the complete vendored set
        let found = resolve_resources(
            &candidates,
            present(&[
                "/stage/share/ghostty/shell-integration",
                "/vendor/share/ghostty/shell-integration",
                "/vendor/share/ghostty/../terminfo/x/xterm-ghostty",
            ]),
        );
        assert_eq!(found, Some(PathBuf::from("/vendor/share/ghostty")));
        // a relative candidate never counts
        let relative = resolve_resources(
            &candidates[..1],
            present(&[
                "relative/ghostty/shell-integration",
                "relative/ghostty/../terminfo/x/xterm-ghostty",
            ]),
        );
        assert_eq!(relative, None);
        assert_eq!(resolve_resources(&candidates, |_| false), None);
    }

    #[test]
    fn the_staged_layout_comes_first() {
        let candidates = resource_candidates(Some(Path::new("/opt/agterm/bin/agterm")));
        assert_eq!(
            candidates.first(),
            Some(&PathBuf::from("/opt/agterm/bin/../share/ghostty"))
        );
        assert_eq!(candidates.last(), Some(&PathBuf::from("/usr/share/ghostty")));
        assert_eq!(resource_candidates(None).len(), 2);
    }

    #[test]
    fn shell_features_restate_every_flag_with_ssh_off() {
        // cursor, sudo, title, ssh-env, ssh-terminfo, path
        assert_eq!(
            shell_features_override(0b11_1111),
            "cursor,sudo,title,no-ssh-env,no-ssh-terminfo,path"
        );
        assert_eq!(
            shell_features_override(0b10_0000),
            "no-cursor,no-sudo,no-title,no-ssh-env,no-ssh-terminfo,path"
        );
    }

    #[test]
    fn unresolved_resources_fall_back_to_xterm_256color() {
        assert!(!defaults_text(true).contains("term ="));
        assert!(defaults_text(false).ends_with("term = xterm-256color\n"));
    }

    #[test]
    fn the_config_stack_applies_defaults_then_the_scoped_file() -> Result<(), Box<dyn std::error::Error>> {
        let state = tempfile::tempdir()?;
        std::fs::create_dir_all(state.path().join("config"))?;
        std::fs::write(
            state.path().join("config/ghostty.conf"),
            "window-padding-x = 3\ntitle = pinned\nshell-integration-features = ssh-env,ssh-terminfo\n",
        )?;
        let env = paths::MapEnv::new()
            .with("HOME", "/nonexistent")
            .with(paths::STATE_DIR_ENV, state.path());
        let (config, static_title) = build_config(&env, true)?;
        assert_eq!(config.diagnostics(), []);
        assert_eq!(static_title.as_deref(), Some("pinned"));
        assert_eq!(config.get_string(keys::TITLE), None);
        // the scoped file re-enabled cursor and title by restating the key; ssh-env and ssh-terminfo are forced off
        assert_eq!(config.get(keys::SHELL_INTEGRATION_FEATURES), Some(0b10_0101));
        assert_eq!(
            config.get(agterm_ghostty::ConfigKey::<bool>::new("cursor-click-to-move")),
            Some(false)
        );
        Ok(())
    }
}
