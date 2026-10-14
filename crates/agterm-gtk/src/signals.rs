//! SIGTERM quits without a prompt (`conformance/docs/rust-build.md`: within 10 s): the control channel stops, the
//! terminals are torn down, which ends their shells, and the application quits.

use adw::prelude::*;

/// Handles the next SIGTERM on the main loop; a second one gets the default action.
pub fn quit_on_sigterm(app: &adw::Application) {
    let app = app.downgrade();
    glib_unix::unix_signal_add_local_once(libc::SIGTERM, move || {
        tracing::info!("SIGTERM: quitting");
        crate::app::shutdown();
        if let Some(app) = app.upgrade() {
            tracing::info!("SIGTERM: quitting the application");
            app.quit();
            tracing::info!("SIGTERM: the application quit returned");
        }
    });
}
