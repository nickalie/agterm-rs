//! The engine cases, each run against one engine in dmabuf or SHM mode.

use std::time::Duration;

use agterm_wpe::{
    Button, Event, FrameContent, Geometry, Host, HostConfig, InjectionTime, Input, Key, Load, LoadErrorKind, LoadEvent,
    MessageHandler, Modifiers, NavigationType, Pointer, PointerKind, PolicyKind, SchemeError, SchemeResponse, Scroll,
    SessionSpec, Settings, StartError, UserScript, fourcc,
};

use crate::support::{Harness, Outcome, SCHEME, Server, data_url, ensure};

/// What the cases share besides the engine.
pub struct Context {
    pub server: Server,
    pub scratch: tempfile::TempDir,
}

pub type Case = fn(&Harness, &Context) -> Outcome;

pub const CASES: &[(&str, Case)] = &[
    ("frames_arrive_in_the_configured_mode_and_follow_the_geometry", frames),
    ("a_running_page_falls_back_to_shm_and_goes_on_drawing", fallback),
    ("scheme_pages_are_served_by_the_consumer", schemes),
    (
        "the_script_bridge_round_trips_in_the_page_world_and_a_script_world",
        bridge,
    ),
    ("navigation_and_response_policy_wait_for_the_consumer", policy),
    ("pointer_keyboard_and_wheel_input_reach_the_page", input),
    ("a_saved_profile_outlives_its_pages_and_its_session", profile),
    ("downloads_are_refused", downloads),
    ("closing_a_page_answers_what_it_left_open", closing),
    ("a_page_that_cannot_be_built_is_reported", unbuilt),
    ("the_engine_starts_once_per_process", once),
];

const RED: (u8, u8, u8) = (0xc0, 0x30, 0x30);
const BLUE: (u8, u8, u8) = (0x30, 0x30, 0xc0);

fn page(body: &str) -> Load {
    Load::Uri(data_url(&format!("<html><body style='margin:0'>{body}</body></html>")))
}

// MARK: - frames

fn frames(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(view, page("<style>body{background:#c03030}</style>"))?;
    let frame = h.pixel_frame(view, 160, 120, RED)?;
    ensure((frame.width, frame.height) == (320, 240), || {
        format!("a 320x240 page at scale 1 drew {}x{}", frame.width, frame.height)
    })?;
    match (&frame.content, h.shm) {
        (FrameContent::Shm(_), true) => {}
        (FrameContent::DmaBuf(buffer), false) => {
            ensure([fourcc(b"XR24"), fourcc(b"AR24")].contains(&buffer.fourcc), || {
                format!("a format the consumer does not import: {:#x}", buffer.fourcc)
            })?;
            ensure(buffer.modifier == agterm_wpe::MODIFIER_LINEAR, || {
                format!("a modifier the consumer does not import: {:#x}", buffer.modifier)
            })?;
            ensure(!buffer.planes.is_empty(), || "a dmabuf without planes".to_owned())?;
        }
        (content, shm) => return Err(format!("{content:?} in a host with shm={shm}")),
    }
    // a repaint reaches the consumer as a new frame
    h.evaluate(view, "document.body.style.background = '#3030c0'", None)?;
    h.pixel_frame(view, 160, 120, BLUE)?;
    // a new size and scale: the buffer set is recreated at the new pixel size
    h.host.set_geometry(
        view,
        Geometry {
            width: 200,
            height: 100,
            scale: 2.0,
        },
    );
    let mut removed = false;
    let resized = h.wait_for("a frame at 400x200", |event| match event {
        Event::BuffersRemoved { view: id, buffers } if *id == view && !buffers.is_empty() => {
            removed = true;
            None
        }
        Event::Frame(frame) if frame.view == view && (frame.width, frame.height) == (400, 200) => Some(frame.clone()),
        _ => None,
    })?;
    h.host.frame_presented(view, resized.buffer);
    ensure(removed, || "the old buffers were never reported gone".to_owned())?;
    h.host.close_view(view);
    Ok(())
}

/// The consumer's fall-back when it cannot import a page's dmabufs: the running page switches to SHM frames (its web
/// process replaced, the page reloaded) and goes on drawing, while another page keeps its own web process; in an SHM
/// engine the page is left as it is.
fn fallback(h: &Harness, _: &Context) -> Outcome {
    let session = h.ephemeral();
    let bystander = h.view(session, Settings::overlay(true));
    h.load(bystander, page("<script>window.mark = 'kept';</script>"))?;
    let view = h.view(session, Settings::overlay(true));
    h.load(
        view,
        page("<style>body{background:#c03030}</style><script>window.mark = 'first';</script>"),
    )?;
    let before = h.pixel_frame(view, 160, 120, RED)?;
    let dmabuf_first = matches!(before.content, FrameContent::DmaBuf(_));
    ensure(dmabuf_first != h.shm, || {
        format!("the first frame reads {:?}", before.content)
    })?;
    h.evaluate(view, "window.mark = 'changed'", None)?;
    h.host.fall_back_to_shm(view);
    if !h.shm {
        let mut terminated = false;
        h.wait_for("the page to reload", |event| match event {
            Event::WebProcessTerminated { view: id, .. } if *id == view => {
                terminated = true;
                None
            }
            Event::Load {
                view: id,
                event: LoadEvent::Finished,
            } if *id == view => Some(()),
            _ => None,
        })?;
        ensure(!terminated, || {
            "the fall-back reported its own web process ending".to_owned()
        })?;
    }
    // an SHM engine's page drew its last frame before the fall-back and has no new one to show
    if !h.shm {
        let after = h.wait_for("an SHM frame of the reloaded page", |event| match event {
            Event::Frame(frame) if frame.view == view => {
                let pixel = Harness::pixel(frame, 160, 120);
                h.host.frame_presented(frame.view, frame.buffer);
                (matches!(frame.content, FrameContent::Shm(_)) && pixel == Ok(RED)).then(|| frame.clone())
            }
            _ => None,
        })?;
        ensure((after.width, after.height) == (320, 240), || {
            format!("the SHM frame is {}x{}", after.width, after.height)
        })?;
    }
    let mark = h.evaluate(view, "window.mark", None)?;
    let expected = if h.shm { "\"changed\"" } else { "\"first\"" };
    ensure(mark.as_deref() == Some(expected), || {
        format!("the page's script state reads {mark:?}, not {expected}")
    })?;
    // it goes on drawing, in SHM
    h.evaluate(view, "document.body.style.background = '#3030c0'", None)?;
    let blue = h.pixel_frame(view, 160, 120, BLUE)?;
    ensure(matches!(blue.content, FrameContent::Shm(_)), || {
        format!("a frame after the fall-back reads {:?}", blue.content)
    })?;
    let kept = h.evaluate(bystander, "window.mark", None)?;
    ensure(kept.as_deref() == Some("\"kept\""), || {
        format!("the other page's state reads {kept:?}")
    })?;
    h.host.close_view(view);
    h.host.close_view(bystander);
    Ok(())
}

// MARK: - schemes

fn schemes(h: &Harness, context: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    let uri = format!("{SCHEME}://page/index.html");
    h.host.load(view, Load::Uri(uri.clone()));
    let request = h.scheme_request(|request| request.uri == uri)?;
    ensure(request.view == Some(view), || {
        format!("the read names {:?}", request.view)
    })?;
    h.host.finish_scheme_request(
        request.token,
        SchemeResponse::Bytes {
            data: b"<title>scheme page</title><img src='missing.png'>".to_vec(),
            mime: "text/html".to_owned(),
        },
    );
    let image = h.scheme_request(|request| request.uri.ends_with("/missing.png"))?;
    h.host.finish_scheme_request(
        image.token,
        SchemeResponse::Error {
            kind: SchemeError::NotFound,
            message: "no image".to_owned(),
        },
    );
    h.finished(view)?;
    h.title(view, "scheme page")?;

    let file = context.scratch.path().join("served.html");
    std::fs::write(&file, "<title>file page</title>").map_err(|err| err.to_string())?;
    let uri = format!("{SCHEME}://page/served.html");
    h.host.load(view, Load::Uri(uri.clone()));
    let request = h.scheme_request(|request| request.uri == uri)?;
    h.host.finish_scheme_request(request.token, SchemeResponse::File(file));
    h.finished(view)?;
    h.title(view, "file page")?;

    let uri = format!("{SCHEME}://page/denied.html");
    h.host.load(view, Load::Uri(uri.clone()));
    let request = h.scheme_request(|request| request.uri == uri)?;
    h.host.finish_scheme_request(
        request.token,
        SchemeResponse::Error {
            kind: SchemeError::PermissionDenied,
            message: "outside the grant".to_owned(),
        },
    );
    let error = h.wait_for("the refused load", |event| match event {
        Event::LoadFailed { view: id, error, .. } if *id == view => Some(error.clone()),
        _ => None,
    })?;
    ensure(
        error.kind == LoadErrorKind::Failed && error.message.contains("outside the grant"),
        || format!("the refusal reads {error:?}"),
    )?;
    h.host.close_view(view);
    Ok(())
}

// MARK: - the script bridge

const ASK: &str = "function ask(body) { window.webkit.messageHandlers.agterm.postMessage(body).then(\
                   reply => { document.title = 'reply:' + JSON.stringify(reply); }, \
                   error => { document.title = 'error:' + error.message; }); }";

fn bridge(h: &Harness, _: &Context) -> Outcome {
    let session = h.ephemeral();
    let handler = |world: Option<&str>| MessageHandler {
        name: "agterm".to_owned(),
        world: world.map(str::to_owned),
    };
    // the page's own world, its scripts on
    let view = h.bridged_view(session, Settings::overlay(true), handler(None), Vec::new());
    h.load(view, page(&format!("<script>{ASK}</script>")))?;
    h.evaluate(view, "ask({ask: 2})", None)?;
    let message = h.script_message(view)?;
    ensure(message.handler == "agterm", || {
        format!("the handler is {}", message.handler)
    })?;
    ensure(message.body.as_deref() == Some(r#"{"ask":2}"#), || {
        format!("the body is {:?}", message.body)
    })?;
    h.host
        .reply_script_message(message.token, Ok(r#"{"answer":4}"#.to_owned()));
    h.title(view, r#"reply:{"answer":4}"#)?;
    h.evaluate(view, "ask('text')", None)?;
    let message = h.script_message(view)?;
    ensure(message.body.is_none(), || {
        format!("a string body reads {:?}", message.body)
    })?;
    h.host
        .reply_script_message(message.token, Err("invalid request".to_owned()));
    h.title(view, "error:invalid request")?;

    ensure(h.evaluate(view, "1 + 1", None)? == Some("2".to_owned()), || {
        "1 + 1".to_owned()
    })?;
    ensure(h.evaluate(view, "undefined", None)?.is_none(), || {
        "undefined".to_owned()
    })?;
    let thrown = h.evaluate(view, "throw new Error('boom')", None);
    ensure(matches!(&thrown, Err(message) if message.contains("boom")), || {
        format!("a throw gave {thrown:?}")
    })?;
    h.host.close_view(view);

    // a script world of agterm's own on a page whose scripts are off, as the overlay's relay runs
    let world = "agterm-world";
    let relay = UserScript {
        source: format!("{ASK} ask({{from: 'world'}});"),
        time: InjectionTime::End,
        world: Some(world.to_owned()),
    };
    let view = h.bridged_view(session, Settings::overlay(false), handler(Some(world)), vec![relay]);
    h.host
        .load(view, page("<script>document.title = 'page script ran';</script>"));
    let message = h.script_message(view)?;
    ensure(message.body.as_deref() == Some(r#"{"from":"world"}"#), || {
        format!("the body is {:?}", message.body)
    })?;
    h.host.reply_script_message(message.token, Ok("true".to_owned()));
    h.title(view, "reply:true")?;
    let page_world = h.evaluate(
        view,
        "typeof (window.webkit && window.webkit.messageHandlers.agterm)",
        None,
    )?;
    let own_world = h.evaluate(
        view,
        "typeof (window.webkit && window.webkit.messageHandlers.agterm)",
        Some(world),
    )?;
    ensure(
        page_world.as_deref() == Some("\"undefined\"") && own_world.as_deref() == Some("\"object\""),
        || format!("the handler is {page_world:?} in the page's world and {own_world:?} in agterm's"),
    )?;
    h.host.close_view(view);
    Ok(())
}

// MARK: - policy

fn policy(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    let next = format!("{SCHEME}://page/next.html");
    let popup = format!("{SCHEME}://page/popup.html");
    h.load(
        view,
        page(&format!(
            "<a id=b target=_blank href='{popup}' style='display:block;width:320px;height:120px'>popup</a>\
             <a id=a href='{next}'>next</a>"
        )),
    )?;
    h.evaluate(view, "document.getElementById('a').click()", None)?;
    let (token, kind) = h.policy(
        view,
        |kind| matches!(kind, PolicyKind::Navigation { uri, .. } if *uri == next),
    )?;
    ensure(
        matches!(
            kind,
            PolicyKind::Navigation {
                navigation: NavigationType::LinkClicked,
                new_window: false,
                ..
            }
        ),
        || format!("the click reads {kind:?}"),
    )?;
    h.host.decide_policy(token, false);
    h.idle(Duration::from_secs(1), |event| match event {
        Event::SchemeRequest(request) if request.uri == next => Some("an ignored navigation was read".to_owned()),
        Event::Load { view: id, .. } if *id == view => Some("an ignored navigation loaded".to_owned()),
        _ => None,
    })?;

    // a real click on a link to a new window
    h.host.set_focused(view, true);
    h.click(view, 40.0, 40.0);
    let (token, kind) = h.policy(
        view,
        |kind| matches!(kind, PolicyKind::Navigation { uri, .. } if *uri == popup),
    )?;
    ensure(matches!(kind, PolicyKind::Navigation { new_window: true, .. }), || {
        format!("the new-window click reads {kind:?}")
    })?;
    h.host.decide_policy(token, false);

    // a refused response fails the load as interrupted
    let blocked = format!("{SCHEME}://page/blocked.html");
    h.host.load(view, Load::Uri(blocked.clone()));
    let request = h.scheme_request(|request| request.uri == blocked)?;
    h.host.finish_scheme_request(
        request.token,
        SchemeResponse::Bytes {
            data: b"<title>blocked</title>".to_vec(),
            mime: "text/html".to_owned(),
        },
    );
    let (token, kind) = h.policy(
        view,
        |kind| matches!(kind, PolicyKind::Response { uri, .. } if *uri == blocked),
    )?;
    ensure(
        matches!(
            kind,
            PolicyKind::Response {
                main_frame: true,
                mime_supported: true,
                ..
            }
        ),
        || format!("the response reads {kind:?}"),
    )?;
    h.host.decide_policy(token, false);
    let error = h.wait_for("the interrupted load", |event| match event {
        Event::LoadFailed { view: id, error, .. } if *id == view => Some(error.clone()),
        _ => None,
    })?;
    ensure(error.kind == LoadErrorKind::Interrupted, || {
        format!("the refused response failed as {error:?}")
    })?;
    h.host.close_view(view);
    Ok(())
}

// MARK: - input

fn input(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(
        view,
        page(
            "<div style='height:2000px'></div><script>\
             addEventListener('mousedown', e => { document.title = 'down:' + e.button + ':' + e.detail; });\
             addEventListener('keydown', e => { document.title = 'key:' + e.key + (e.shiftKey ? ':shift' : ''); });\
             addEventListener('wheel', () => { document.title = 'wheel'; });</script>",
        ),
    )?;
    h.host.set_focused(view, true);
    h.click(view, 10.0, 10.0);
    h.title(view, "down:0:1")?;
    h.click(view, 10.0, 10.0);
    h.title(view, "down:0:2")?;
    let key = |pressed, modifiers| Key {
        pressed,
        keyval: 0x61,
        keycode: None,
        modifiers,
    };
    h.host.input(view, Input::Key(key(true, Modifiers::empty())));
    h.host.input(view, Input::Key(key(false, Modifiers::empty())));
    h.title(view, "key:a")?;
    h.host.input(view, Input::Key(key(true, Modifiers::SHIFT)));
    h.host.input(view, Input::Key(key(false, Modifiers::SHIFT)));
    h.title(view, "key:a:shift")?;
    h.host.input(
        view,
        Input::Scroll(Scroll {
            x: 100.0,
            y: 100.0,
            delta_x: 0.0,
            delta_y: -3.0,
            precise: false,
            stop: false,
            modifiers: Modifiers::empty(),
        }),
    );
    h.title(view, "wheel")?;
    h.host.close_view(view);
    Ok(())
}

// MARK: - stores

fn profile(h: &Harness, context: &Context) -> Outcome {
    let store = SessionSpec::Persistent {
        data_dir: context.scratch.path().join("browser/data"),
        cache_dir: context.scratch.path().join("browser/cache"),
    };
    let saved = h.host.create_session(store.clone());
    let set = Load::Uri(format!("{}/set", context.server.origin));
    let get = Load::Uri(format!("{}/get", context.server.origin));
    let first = h.view(saved, Settings::overlay(true));
    h.load(first, set)?;
    h.title(first, "set")?;
    h.host.close_view(first);
    let second = h.view(saved, Settings::overlay(true));
    h.load(second, get.clone())?;
    h.title(second, "cookie=saved=1;storage=v")?;
    h.host.close_view(second);

    let memory = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(memory, get.clone())?;
    h.title(memory, "cookie=;storage=null")?;
    h.host.close_view(memory);

    // a new session on the same directories reads the login from disk
    h.host.drop_session(saved);
    let reopened = h.host.create_session(store);
    let third = h.view(reopened, Settings::overlay(true));
    h.load(third, get.clone())?;
    h.title(third, "cookie=saved=1;storage=v")?;
    h.host.close_view(third);
    let data = context.scratch.path().join("browser/data");
    let kept = std::fs::read_dir(&data).map_or(0, Iterator::count);
    ensure(kept > 0, || format!("nothing on disk under {}", data.display()))?;

    h.host.clear_session(reopened);
    let cleared = h.wait_for("the store to clear", |event| match event {
        Event::SessionCleared { session, result } if *session == reopened => Some(result.clone()),
        _ => None,
    })?;
    ensure(cleared.is_ok(), || format!("the clear failed: {cleared:?}"))?;
    let fourth = h.view(reopened, Settings::overlay(true));
    h.load(fourth, get)?;
    h.title(fourth, "cookie=;storage=null")?;
    h.host.close_view(fourth);
    Ok(())
}

fn downloads(h: &Harness, context: &Context) -> Outcome {
    let session = h.ephemeral();
    let view = h.view(session, Settings::overlay(true));
    h.host
        .load(view, Load::Uri(format!("{}/download", context.server.origin)));
    let (from, page, uri) = h.wait_for("the refused download", |event| match event {
        Event::DownloadRefused { session, view, uri } => Some((*session, *view, uri.clone())),
        _ => None,
    })?;
    ensure(
        from == session && page == Some(view) && uri.ends_with("/file.bin"),
        || format!("the download reads {from:?} {page:?} {uri}"),
    )?;
    h.host.close_view(view);
    Ok(())
}

// MARK: - lifecycle

fn closing(h: &Harness, _: &Context) -> Outcome {
    let session = h.ephemeral();
    let view = h.view(session, Settings::overlay(true));
    let slow = format!("{SCHEME}://page/slow.html");
    h.host.load(view, Load::Uri(slow.clone()));
    let request = h.scheme_request(|request| request.uri == slow)?;
    h.host.close_view(view);
    // answering after the close finds nothing open, and the page is gone
    h.host.finish_scheme_request(
        request.token,
        SchemeResponse::Bytes {
            data: b"late".to_vec(),
            mime: "text/plain".to_owned(),
        },
    );
    let gone = h.evaluate(view, "1", None);
    ensure(gone == Err("no such page".to_owned()), || {
        format!("a closed page evaluated {gone:?}")
    })?;
    h.idle(Duration::from_millis(500), |event| match event {
        Event::Frame(frame) if frame.view == view => Some("a closed page sent a frame".to_owned()),
        Event::Page { view: id, .. } | Event::Load { view: id, .. } if *id == view => {
            Some("a closed page reported".to_owned())
        }
        _ => None,
    })?;
    // the engine goes on
    let next = h.view(session, Settings::overlay(false));
    h.load(next, page("<title>after</title>"))?;
    h.title(next, "after")?;
    h.host.close_view(next);
    Ok(())
}

fn unbuilt(h: &Harness, _: &Context) -> Outcome {
    let failed = |view| {
        h.wait_for("the page's failure", |event| match event {
            Event::ViewFailed { view: id, message } if *id == view => Some(message.clone()),
            _ => None,
        })
    };
    let gone = h.ephemeral();
    h.host.drop_session(gone);
    let orphan = h.view(gone, Settings::overlay(false));
    let message = failed(orphan)?;
    ensure(message == "no such browser store", || {
        format!("a page on a dropped store failed with {message:?}")
    })?;
    let handler = || MessageHandler {
        name: "agterm".to_owned(),
        world: None,
    };
    let twice = h.view_with(agterm_wpe::ViewSpec {
        session: h.ephemeral(),
        settings: Settings::overlay(true),
        scripts: Vec::new(),
        message_handlers: vec![handler(), handler()],
        zoom: 1.0,
        background: None,
        geometry: Geometry {
            width: 320,
            height: 240,
            scale: 1.0,
        },
    });
    let message = failed(twice)?;
    ensure(message.contains("registered twice"), || {
        format!("a doubled handler failed with {message:?}")
    })?;
    let gone = h.evaluate(twice, "1", None);
    ensure(gone == Err("no such page".to_owned()), || {
        format!("an unbuilt page evaluated {gone:?}")
    })
}

fn once(_: &Harness, _: &Context) -> Outcome {
    let again = Host::start(HostConfig {
        frames: agterm_wpe::FrameMode::Shm,
        schemes: Vec::new(),
    });
    ensure(matches!(again, Err(StartError::AlreadyStarted)), || {
        format!("a second start gave {again:?}")
    })
}

/// A press and release of the primary button at (`x`, `y`).
impl Harness {
    pub fn click(&self, view: agterm_wpe::ViewId, x: f64, y: f64) {
        let pointer = |kind| {
            Input::Pointer(Pointer {
                kind,
                x,
                y,
                modifiers: Modifiers::empty(),
            })
        };
        self.host.input(view, pointer(PointerKind::Move));
        self.host.input(view, pointer(PointerKind::Down(Button::Primary)));
        self.host.input(view, pointer(PointerKind::Up(Button::Primary)));
    }
}
