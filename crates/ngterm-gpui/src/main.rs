//! `ngterm`, the GPUI frontend: one instance per application id (`instance.rs`), the core and its control
//! socket on GPUI's main thread (`app.rs`).
//!
//! Startup order matters: a process started as the HTML overlays' engine worker (`ngterm_wpe::worker`, the vendored WPE
//! `WebKit`'s engine in a process of its own) serves the engine and ends before anything of the app starts; then the
//! activation token and the startup id are read while the process has one thread and before GPUI's platform exists
//! (GPUI removes the token from the environment; the startup id is removed here), and a launch that forwards to a
//! running ngterm exits before GPUI starts and before it reads or writes the model state (it only appends to the shared
//! log).

use std::io::Write as _;
use std::process::ExitCode;

use ngterm_core::paths::ProcessEnv;
use ngterm_gpui::app::{self, Launch};
use ngterm_gpui::instance::{self, Activation, Registration};

fn main() -> ExitCode {
    // the pages' engine worker: it serves the engine (inside the vendored WPE WebKit's overlay, where the app never
    // goes) and ends here, appending to the app's log, and touches nothing else of the app
    #[cfg(all(feature = "html", target_os = "linux"))]
    // SAFETY: first thing in main, while the process has one thread.
    unsafe {
        ngterm_wpe::worker::serve_if_asked(|| ngterm_app::logging::init(&ProcessEnv));
    }
    let activation = Activation::from_env(&ProcessEnv);
    // GDK unsets it once read, so no child (a hook, a custom command, a shell) claims this launch's sequence
    // SAFETY: first thing in main, while the process has one thread.
    unsafe { std::env::remove_var(instance::STARTUP_ID_ENV) };
    ngterm_app::logging::init(&ProcessEnv);
    let app_id = instance::app_id(&ProcessEnv);
    let (requests, received) = async_channel::unbounded();
    // kept for the whole run: dropping it gives the name up
    let _bus = match instance::register(&app_id, requests) {
        Ok(Registration::Primary(connection)) => {
            tracing::info!(app_id, "instance: primary");
            Some(connection)
        }
        Ok(Registration::Remote(remote)) => return forward(&remote, &activation),
        Ok(Registration::Unique(error)) => {
            tracing::warn!(%error, "instance: no session bus; running without single-instance");
            None
        }
        Err(error) => {
            tracing::error!(%error, "instance: registration failed");
            return ExitCode::FAILURE;
        }
    };
    let status = app::run(Launch {
        app_id,
        requests: received,
        activation,
    });
    exit_now(status)
}

/// Hands this launch to the running primary.
fn forward(remote: &instance::Remote, activation: &Activation) -> ExitCode {
    let cwd = std::env::current_dir().ok();
    match remote.activate(activation, cwd.as_deref()) {
        Ok(()) => {
            tracing::info!(
                token = activation.token.is_some(),
                startup_id = activation.startup_id.is_some(),
                "instance: forwarded to the running instance"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "instance: could not forward to the running instance");
            ExitCode::FAILURE
        }
    }
}

/// Ends the process after the orderly shutdown, skipping the exit handlers and thread-local destructors: wgpu's GLES
/// instance (GPUI creates its wgpu instance with the GL backend beside Vulkan) terminates EGL in a TLS destructor
/// after the Wayland connection is gone and crashes every exit with SIGSEGV (`docs/spikes/20261014-wpe-dmabuf.md`,
/// quirk 12; `docs/upstream-issues/gpui-wgpu-gles-exit-segfault.md`). Everything that must outlive the process is
/// written by then: the quit save, the socket removed, the log written unbuffered.
fn exit_now(status: i32) -> ! {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` only ends the process; nothing of this process runs after it, so no invariant can break.
    unsafe { libc::_exit(status) }
}
