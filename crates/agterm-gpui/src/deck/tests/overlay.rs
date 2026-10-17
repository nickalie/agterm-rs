//! Program and pane overlays on a real page (the GTK build's `overlays_mount_in_place` walk): the program covering the
//! page or floating over its backdrop, a pane overlay over its pane, each taking the keyboard while the layout gives
//! it and handing it back to the pane once it goes; the program's exit status read back through
//! `session.overlay.result`; upstream's `show-image.sh` drawing its image through chunked Kitty APC in a PTY overlay.
//!
//! The terminals run what they are launched with (the overlay's wrapper and command), through `/bin/sh -c`, with no
//! inherited `AGTERM_*` or `ZMX_*` variable; a pane without a command runs `cat`.

use agterm_proto::Response;

use super::*;
use crate::overlay::ProgramPlacement;

/// Runs each launch's command through `/bin/sh -c` in its directory with its variables, `exec cat` without one.
fn running_launcher(launches: Arc<Mutex<Vec<Launch>>>) -> pane::Launcher {
    Rc::new(move |launch: &Launch| -> Spawn {
        launches.lock().unwrap().push(launch.clone());
        let line = launch.command.clone().unwrap_or_else(|| "exec cat".to_owned());
        let mut command = PaneCommand::new("/bin/sh").arg("-c").arg(line);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            let ours = launch.env.iter().any(|(set, _)| set.as_str() == name);
            if (name.starts_with("AGTERM_") || name.starts_with("ZMX_")) && !ours {
                command = command.env_remove(key);
            }
        }
        for (key, value) in &launch.env {
            command = command.env(key, value);
        }
        if let Some(directory) = launch.working_directory.as_ref().filter(|path| path.is_dir()) {
            command = command.cwd(directory);
        }
        Box::new(move |config: PaneConfig| Pane::spawn(&command, &config))
    })
}

/// A deck whose terminals run their launches, the first session's pane started; its session and pane token.
fn overlay_harness(cx: &mut TestAppContext) -> (Harness<'_>, Uuid, String) {
    let (library, window, ids) = three_sessions();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let spawner = running_launcher(Arc::clone(&recorded));
    let mut h = harness_with(cx, core_of(library), window, recorded, spawner);
    let session = ids[0];
    h.until("the pane to start", |h| h.realized(session, StatusPane::Left));
    let token = h.token(session, StatusPane::Left);
    assert!(h.focused(session, &token));
    (h, session, token)
}

/// The session-wide program's terminal: whether it shows and whether it holds the window's keyboard.
fn program(h: &mut Harness<'_>, session: Uuid) -> Option<(bool, bool)> {
    let deck = h.deck.clone();
    h.cx.update(|window, cx| {
        let program = deck.read(cx).page(session)?.overlays().program()?;
        let shown = program.view().read(cx).is_shown();
        Some((shown, program.focus_handle(cx).is_focused(window)))
    })
}

/// The pane overlays' terminals: each one's generation, whether it shows and whether it holds the keyboard.
fn pane_overlays(h: &mut Harness<'_>, session: Uuid) -> Vec<(u64, bool, bool)> {
    let deck = h.deck.clone();
    h.cx.update(|window, cx| {
        deck.read(cx).page(session).map_or_else(Vec::new, |page| {
            page.overlays()
                .panes()
                .iter()
                .map(|overlay| {
                    (
                        overlay.generation(),
                        overlay.view().read(cx).is_shown(),
                        overlay.focus_handle(cx).is_focused(window),
                    )
                })
                .collect()
        })
    })
}

fn program_realized(h: &Harness<'_>, session: Uuid) -> bool {
    h.session(session)
        .overlay
        .surface
        .get()
        .is_some_and(agterm_host::Surface::is_realized)
}

/// `session.overlay.result` of `session`'s program (`pane` `None`) or of its pane overlay.
fn result(h: &mut Harness<'_>, session: Uuid, pane: Option<&str>) -> Response {
    let args = Args {
        pane: pane.map(str::to_owned),
        ..Args::default()
    };
    h.state.core.dispatch(&Request {
        cmd: Command::Session(SessionCmd::OverlayResult),
        target: Some(uuid_string(session)),
        args: Some(args),
    })
}

fn exit_code(response: &Response) -> Option<i64> {
    assert!(response.ok, "{response:?}");
    let result = response.result.as_ref()?;
    result
        .get("exitCode")
        .or_else(|| result.get("exit_code"))
        .and_then(serde_json::Value::as_i64)
}

#[gpui_kit::test]
fn a_program_overlay_covers_the_page_takes_the_keyboard_and_reports_its_exit_status(cx: &mut TestAppContext) {
    let (mut h, session, token) = overlay_harness(cx);
    h.dispatch(Command::Session(SessionCmd::OverlayOpen), session, |args| {
        args.command = Some("read -r line; exit 7".to_owned());
    });
    h.turn();
    assert_eq!(
        program(&mut h, session),
        Some((true, true)),
        "it shows and takes the keyboard"
    );
    assert!(!h.shown(session, &token), "the pane hides under a covering program");
    assert!(h.session(session).overlay.surface.is_present(), "installed in the slot");
    let placement = h
        .deck
        .read_with(h.cx, |deck, _| deck.page(session).unwrap().overlays().placement());
    assert_eq!(placement, Some(ProgramPlacement { fraction: None }));
    h.until("the program to start", |h| program_realized(h, session));

    // the keys reach the program, whose end closes the overlay with its status
    h.cx.simulate_keystrokes("x enter");
    h.until("the program to end", |h| !h.session(session).overlay.active);
    assert_eq!(program(&mut h, session), None, "the terminal goes with the overlay");
    assert_eq!(exit_code(&result(&mut h, session, None)), Some(7));
    assert!(h.shown(session, &token));
    assert!(h.focused(session, &token), "the pane takes the keyboard back");
}

#[gpui_kit::test]
fn a_floating_program_shows_at_its_share_over_the_backdrop_and_hides_with_its_page(cx: &mut TestAppContext) {
    let (mut h, session, token) = overlay_harness(cx);
    h.dispatch(Command::Session(SessionCmd::OverlayOpen), session, |args| {
        args.command = Some("exec cat".to_owned());
        args.size_percent = Some(50);
    });
    h.turn();
    let (placement, backdrop) = h.deck.read_with(h.cx, |deck, _| {
        let page = deck.page(session).unwrap();
        (page.overlays().placement(), page.washes().backdrop.is_some())
    });
    assert_eq!(placement, Some(ProgramPlacement { fraction: Some(0.5) }));
    assert!(backdrop, "the backdrop washes the session behind it");
    assert!(
        h.shown(session, &token),
        "the pane stays drawn behind a floating program"
    );
    assert_eq!(program(&mut h, session), Some((true, true)));

    let other = h.store().sessions().map(Session::id).find(|id| *id != session).unwrap();
    h.store().select_session(Some(other));
    h.state.settle();
    h.turn();
    assert_eq!(program(&mut h, session), Some((false, false)), "a hidden page hides it");
    h.store().select_session(Some(session));
    h.state.settle();
    h.turn();
    assert_eq!(
        program(&mut h, session),
        Some((true, true)),
        "and shows it with the keyboard again"
    );

    h.dispatch(Command::Session(SessionCmd::OverlayClose), session, |_| {});
    h.turn();
    assert_eq!(program(&mut h, session), None);
    assert!(h.focused(session, &token), "a close gives the pane the keyboard back");
}

#[gpui_kit::test]
fn a_pane_overlay_covers_its_pane_and_gives_the_keyboard_back_when_closed(cx: &mut TestAppContext) {
    let (mut h, session, token) = overlay_harness(cx);
    h.dispatch(Command::Session(SessionCmd::OverlayOpen), session, |args| {
        args.command = Some("exec cat".to_owned());
        args.pane = Some("left".to_owned());
    });
    h.turn();
    let overlays = pane_overlays(&mut h, session);
    assert_eq!(overlays.len(), 1, "{overlays:?}");
    let (generation, shown, focused) = overlays[0];
    assert!(shown && focused, "it shows over its pane with the keyboard");
    assert!(!h.shown(session, &token), "the pane hides under its overlay");
    assert_eq!(program(&mut h, session), None, "no session-wide program");
    let owner = h.deck.read_with(h.cx, |deck, _| {
        let page = deck.page(session).unwrap();
        page.overlays().pane(generation).unwrap().shared().owner()
    });
    assert_eq!(
        owner,
        Some(SurfaceOwner::Overlay {
            session,
            generation,
            pane: true
        })
    );

    h.dispatch(Command::Session(SessionCmd::OverlayClose), session, |args| {
        args.pane = Some("left".to_owned());
    });
    h.turn();
    assert!(pane_overlays(&mut h, session).is_empty(), "a close drops it");
    assert!(h.shown(session, &token));
    assert!(h.focused(session, &token), "the pane takes the keyboard back");
}

#[gpui_kit::test]
fn a_held_program_waits_for_a_key_before_its_overlay_closes(cx: &mut TestAppContext) {
    let (mut h, session, _) = overlay_harness(cx);
    h.dispatch(Command::Session(SessionCmd::OverlayOpen), session, |args| {
        args.command = Some("exec cat".to_owned());
    });
    h.turn();
    let shared = h.deck.read_with(h.cx, |deck, _| {
        Rc::clone(deck.page(session).unwrap().overlays().program().unwrap().shared())
    });
    let held = agterm_term::ExitStatus {
        code: 0,
        signal: None,
        held: true,
    };
    h.deck.update(h.cx, |deck, cx| {
        let program = deck.page(session).unwrap().overlays().program().unwrap();
        program.terminal().update(cx, |_, cx| {
            cx.emit(TerminalEvent::Pane(agterm_term::PaneEvent::Exited(held)));
        });
    });
    h.turn();
    assert!(shared.holds_keys(), "a held end waits for a key");
    assert!(h.session(session).overlay.active, "the overlay stays up meanwhile");
    h.cx.simulate_keystrokes("q");
    h.until("the key to close it", |h| !h.session(session).overlay.active);
}

/// A 4x2 opaque PNG at `path`.
fn write_png(path: &Path) {
    let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 40, 40, 255]));
    image.save(path).unwrap();
}

/// POSIX single quotes around `path`.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

#[gpui_kit::test]
fn show_image_draws_its_image_in_a_pty_overlay_and_closes_on_enter(cx: &mut TestAppContext) {
    let (mut h, session, token) = overlay_harness(cx);
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("dot.png");
    write_png(&png);
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../resources/skills/agterm/scripts/show-image.sh");
    assert!(script.is_file(), "the skill's show-image.sh");
    // what `show-image.sh <image>` opens: itself in --emit mode inside a PTY overlay
    let command = format!("/bin/bash {} --emit {}", quoted(&script), quoted(&png));
    h.dispatch(Command::Session(SessionCmd::OverlayOpen), session, |args| {
        args.command = Some(command);
        args.size_percent = Some(60);
    });
    h.turn();
    let view = h.deck.read_with(h.cx, |deck, _| {
        deck.page(session).unwrap().overlays().program().unwrap().view().clone()
    });
    h.until("the image to be drawn", |h| {
        h.cx.update(|window, _| window.refresh());
        !view.read_with(h.cx, |view, _| view.images().drawn()).is_empty()
    });
    let image = view.read_with(h.cx, |view, _| view.images().drawn()).remove(0);
    assert_eq!(
        (image.size(0).width.0, image.size(0).height.0),
        (4, 2),
        "the PNG decoded"
    );
    assert!(h.cx.update(|window, _| window.has_image_atlas_entry(&image)));

    h.cx.simulate_keystrokes("enter");
    h.until("the overlay to close", |h| !h.session(session).overlay.active);
    assert_eq!(exit_code(&result(&mut h, session, None)), Some(0));
    assert!(h.focused(session, &token));
}

// MARK: - HTML pages (`crate::html_overlay`, on its recording engine)

/// Whether `session`'s session-wide page shows and holds the window's keyboard.
#[cfg(all(feature = "html", target_os = "linux"))]
fn program_page(h: &mut Harness<'_>, session: Uuid) -> Option<(bool, bool)> {
    let deck = h.deck.clone();
    h.cx.update(|window, cx| {
        let page = deck.read(cx).page(session)?.overlays().program_page()?;
        let handle = gpui_kit::Focusable::focus_handle(page.element().read(cx), cx);
        Some((page.is_shown(), handle.is_focused(window)))
    })
}

/// One reconcile of the deck with `modal` holding the window's keyboard (the window's picker, GUI ask, palette, an
/// alert, a popover or the Settings popup).
#[cfg(all(feature = "html", target_os = "linux"))]
fn reconcile(h: &mut Harness<'_>, modal: bool) {
    let (deck, state) = (h.deck.clone(), &mut h.state);
    let focused =
        h.cx.update(|window, cx| deck.update(cx, |deck, cx| deck.reconcile(&mut state.core, modal, window, cx)));
    if focused {
        h.state.settle();
    }
    h.cx.run_until_parked();
}

#[cfg(all(feature = "html", target_os = "linux"))]
#[gpui_kit::test]
fn a_page_takes_the_keyboard_from_its_pane_never_from_a_modal_and_hands_it_back(cx: &mut TestAppContext) {
    use crate::html_overlay::testing::{self, Call};

    let (mut h, session, token) = overlay_harness(cx);
    let web = testing::install();
    let poster = h.state.poster().clone();
    crate::html_overlay::install(&mut h.state.core, h.state_dir.path().to_path_buf(), poster);
    h.dispatch(Command::Session(SessionCmd::OverlayOpen), session, |args| {
        args.url = Some("https://example.invalid/".to_owned());
    });
    h.turn();
    assert_eq!(
        program_page(&mut h, session),
        Some((true, true)),
        "it covers the page and takes the keyboard"
    );
    assert!(!h.focused(session, &token));
    assert!(web.take().contains(&Call::Visible(true)));

    // a modal comes up and takes the keyboard: the page leaves it there, and takes it back once the modal goes
    let modal = h.cx.update(|window, cx| {
        let handle = cx.focus_handle();
        window.focus(&handle, cx);
        handle
    });
    reconcile(&mut h, true);
    assert_eq!(program_page(&mut h, session), Some((true, false)));
    assert!(
        h.cx.update(|window, _| modal.is_focused(window)),
        "the modal keeps the keyboard"
    );
    reconcile(&mut h, false);
    assert_eq!(
        program_page(&mut h, session),
        Some((true, true)),
        "handed back once the modal went"
    );

    // the page closes: the pane gets the keyboard back, and the engine's page goes at the release
    h.dispatch(Command::Session(SessionCmd::OverlayClose), session, |_| {});
    let state = &mut h.state;
    h.cx.update(|_, cx| crate::html_overlay::settle(&mut state.core, cx));
    h.turn();
    assert_eq!(program_page(&mut h, session), None);
    assert!(h.focused(session, &token), "the pane holds the keyboard again");
    assert!(
        web.take().iter().any(|call| matches!(call, Call::CloseView(_))),
        "the released page closed its engine page"
    );
}
