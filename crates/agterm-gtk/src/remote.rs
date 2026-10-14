//! The remote links' GTK side: the network and resume observer that retries them. The presentation bridge itself is
//! frontend-neutral ([`agterm_app::remote`]); the core decides everything else ([`agterm_core::remote::client`]).

use agterm_core::remote::reconnect::NetworkReports;
use gtk::gio;
use gtk::gio::prelude::*;

use crate::state::{self, AppState, Poster};

thread_local! {
    /// Observing started (once per app).
    static OBSERVING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The logind subscription the wake retry rides on, kept for the app's life.
    static WAKE: std::cell::RefCell<Option<gio::SignalSubscription>> = const { std::cell::RefCell::new(None) };
}

/// Retries the remote links when the network comes back or the machine wakes (upstream's `RemoteLinkObserver`, its
/// path monitor and its screens-did-wake bridge): GIO's network monitor reports every change, the first one being
/// the state at start ([`NetworkReports`]), and logind's `PrepareForSleep(false)` says the machine resumed. A
/// backoff measured on the monotonic clock does not advance across a suspend, so without this a pane could keep
/// waiting for up to five minutes after a resume. Idempotent.
pub fn observe_links(poster: &Poster) {
    if OBSERVING.with(|observing| observing.replace(true)) {
        return;
    }
    let reports = std::rc::Rc::new(std::cell::RefCell::new(NetworkReports::default()));
    let posting = poster.clone();
    gio::NetworkMonitor::default().connect_network_changed(move |_, available| {
        if reports.borrow_mut().changed(available) {
            retry_links(&posting);
        }
    });
    let posting = poster.clone();
    gio::bus_get(gio::BusType::System, None::<&gio::Cancellable>, move |bus| {
        let bus = match bus {
            Ok(bus) => bus,
            Err(error) => {
                tracing::debug!(%error, "no system bus: remote links are not retried on wake");
                return;
            }
        };
        let subscription = bus.subscribe_to_signal(
            Some("org.freedesktop.login1"),
            Some("org.freedesktop.login1.Manager"),
            Some("PrepareForSleep"),
            Some("/org/freedesktop/login1"),
            None,
            gio::DBusSignalFlags::NONE,
            move |signal| {
                if resumed(signal.parameters) {
                    retry_links(&posting);
                }
            },
        );
        WAKE.with(|wake| *wake.borrow_mut() = Some(subscription));
    });
}

/// Whether a `PrepareForSleep` signal's parameters say the machine woke (`false`) rather than goes to sleep.
fn resumed(parameters: &gtk::glib::Variant) -> bool {
    parameters.get::<(bool,)>() == Some((false,))
}

fn retry_links(poster: &Poster) {
    tracing::info!("network back or machine awake: retrying the remote links now");
    state::post(poster, AppState::retry_remote_links);
}

#[cfg(test)]
mod tests {
    use gtk::glib::variant::ToVariant;

    use super::*;

    #[test]
    fn only_a_wake_retries_the_links() {
        assert!(resumed(&(false,).to_variant()));
        assert!(!resumed(&(true,).to_variant()), "going to sleep");
        assert!(!resumed(&("nope",).to_variant()));
    }
}
