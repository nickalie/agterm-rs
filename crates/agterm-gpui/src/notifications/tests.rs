//! What reaches the notification server, in order (ported from the GTK build's tests). The server itself, on a private
//! session bus, is tested in `tests/notifications.rs`.

use std::cell::RefCell;

use agterm_core::{NotificationPost, PaneRole, notification_identity as identity};
use uuid::Uuid;

use super::*;

#[test]
fn the_notification_sound_follows_a_posted_banner_only() {
    let banner = NotificationEffect::App {
        identity: "w:s:main".to_owned(),
        title: "t".to_owned(),
        body: "b".to_owned(),
    };
    let mut settings = agterm_core::persist::AppSettings::default();
    assert_eq!(
        banner_sound(std::slice::from_ref(&banner), &settings),
        None,
        "no sound picked"
    );
    settings.notification_sound_name = Some("Glass".to_owned());
    assert_eq!(banner_sound(std::slice::from_ref(&banner), &settings), Some("Glass"));
    assert_eq!(banner_sound(&[], &settings), None, "no banner, no sound");
}

/// Records what reaches the notification server, in order.
#[derive(Default)]
struct Recorder(RefCell<Vec<String>>);

impl BannerSink for Recorder {
    fn send(&self, identity: &str, _title: &str, body: &str) {
        self.0.borrow_mut().push(format!("send {identity} {body}"));
    }

    fn withdraw(&self, identity: &str) {
        self.0.borrow_mut().push(format!("withdraw {identity}"));
    }
}

fn post(window: Uuid, session: Uuid, pane: PaneRole, body: &str) -> NotificationEffect {
    NotificationEffect::Post(NotificationPost {
        window,
        session,
        pane,
        title: "t".to_owned(),
        body: body.to_owned(),
    })
}

#[test]
fn posts_and_withdrawals_reach_the_server_in_order_and_only_for_their_session() {
    let (window, other_window) = (Uuid::new_v4(), Uuid::new_v4());
    let (session, other) = (Uuid::new_v4(), Uuid::new_v4());
    let effects = [
        post(window, session, PaneRole::Split, "one"),
        post(window, other, PaneRole::Main, "two"),
        post(other_window, session, PaneRole::Main, "elsewhere"),
        NotificationEffect::Clear { window, session },
        post(window, session, PaneRole::Main, "three"),
    ];
    let recorder = Recorder::default();
    deliver(&recorder, &effects);
    let expected = [
        format!("send {} one", identity(window, session, PaneRole::Split)),
        format!("send {} two", identity(window, other, PaneRole::Main)),
        format!("send {} elsewhere", identity(other_window, session, PaneRole::Main)),
        format!("withdraw {}", identity(window, session, PaneRole::Main)),
        format!("withdraw {}", identity(window, session, PaneRole::Split)),
        format!("withdraw {}", identity(window, session, PaneRole::Overlay)),
        format!("send {} three", identity(window, session, PaneRole::Main)),
    ];
    assert_eq!(*recorder.0.borrow(), expected);
}

#[test]
fn the_desktop_sink_queues_banners_in_order_for_the_task() {
    let (sink, queue) = Desktop::new();
    let window = Uuid::new_v4();
    let session = Uuid::new_v4();
    deliver(
        &sink,
        &[
            post(window, session, PaneRole::Main, "b"),
            NotificationEffect::Clear { window, session },
        ],
    );
    let main = identity(window, session, PaneRole::Main);
    assert_eq!(
        queue.try_recv().ok(),
        Some(Banner::Send {
            identity: main.clone(),
            title: "t".to_owned(),
            body: "b".to_owned()
        })
    );
    assert_eq!(queue.try_recv().ok(), Some(Banner::Withdraw { identity: main }));
    assert_eq!(queue.len(), 2, "the split's and the overlay's identities follow");
}
