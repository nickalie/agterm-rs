//! The cases for what an offscreen page cannot draw or connect itself (`engine::interaction`, `engine::ime`): the
//! clipboard in and out, `<select>` popups, the context menu's hit test, and an input method's composition.

use agterm_wpe::{
    Button, EditingCommand, Event, Input, Key, Modifiers, OptionKind, Pointer, PointerKind, SchemeResponse, Settings,
    ViewId,
};

use crate::cases::Context;
use crate::support::{Harness, Outcome, SCHEME, data_url, ensure};

fn page(body: &str) -> agterm_wpe::Load {
    agterm_wpe::Load::Uri(data_url(&format!("<html><body style='margin:0'>{body}</body></html>")))
}

/// A page's copy reaches the consumer; the consumer's text reaches the page's paste, by editing command and by key.
pub fn clipboard(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(view, page("<textarea id=t>copied from the page</textarea>"))?;
    h.host.set_focused(view, true);
    h.evaluate(view, "t.focus(); t.select(); true", None)?;
    h.host.editing_command(view, EditingCommand::Copy);
    let copied = h.wait_for("the page's copy", |event| match event {
        Event::Clipboard { text, .. } => Some(text.clone()),
        _ => None,
    })?;
    ensure(copied == "copied from the page", || {
        format!("the copy reported {copied:?}")
    })?;

    h.host.set_clipboard("pasted by command".to_owned());
    h.evaluate(view, "t.value = ''; t.focus(); true", None)?;
    h.host.editing_command(view, EditingCommand::Paste);
    value_is(h, view, "t.value", "pasted by command")?;

    h.host.set_clipboard("pasted by key".to_owned());
    h.evaluate(view, "t.value = ''; t.focus(); true", None)?;
    let v = |pressed| Key {
        pressed,
        keyval: 0x76,
        keycode: None,
        modifiers: Modifiers::CONTROL,
    };
    h.host.input(view, Input::Key(v(true)));
    h.host.input(view, Input::Key(v(false)));
    value_is(h, view, "t.value", "pasted by key")?;
    h.host.close_view(view);
    Ok(())
}

/// Scheme answers and pastes larger than the worker's queue holds in its working set (64 MiB), each followed at once
/// by more commands, leave the transport up: the page loads, takes the paste that follows them by key, and evaluates.
pub fn large_payloads(h: &Harness, _: &Context) -> Outcome {
    const LARGE: usize = (64 << 20) + (1 << 20);
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    let uri = format!("{SCHEME}://large/index.html");
    h.host.load(view, agterm_wpe::Load::Uri(uri.clone()));
    let document = h.scheme_request(|request| request.uri == uri)?;
    h.host.finish_scheme_request(
        document.token,
        SchemeResponse::Bytes {
            data: b"<body style='margin:0'><textarea id=t></textarea><img src='a.bin'><img src='b.bin'></body>"
                .to_vec(),
            mime: "text/html".to_owned(),
        },
    );
    let mut reads = Vec::new();
    let reads = h.wait_for("both large reads", |event| {
        if let Event::SchemeRequest(request) = event
            && matches!(request.uri.rsplit('/').next(), Some("a.bin" | "b.bin"))
        {
            reads.push(request.token);
        }
        (reads.len() == 2).then(|| reads.clone())
    })?;
    // both answers at once (the second waits while the writer still copies the first), input right behind them
    for token in reads {
        h.host.finish_scheme_request(
            token,
            SchemeResponse::Bytes {
                data: vec![0; LARGE],
                mime: "application/octet-stream".to_owned(),
            },
        );
    }
    h.host.set_focused(view, true);
    h.finished(view)?;
    h.evaluate(view, "t.focus(); true", None)?;
    let large = "l".repeat(LARGE);
    for _ in 0..2 {
        h.host.set_clipboard(large.clone());
        h.host.set_clipboard("pasted after the large ones".to_owned());
    }
    drop(large);
    let v = |pressed| Key {
        pressed,
        keyval: 0x76,
        keycode: None,
        modifiers: Modifiers::CONTROL,
    };
    h.host.input(view, Input::Key(v(true)));
    h.host.input(view, Input::Key(v(false)));
    value_is(h, view, "t.value", "pasted after the large ones")?;
    h.host.close_view(view);
    Ok(())
}

/// Waits until `expression` in `view` reads the string `expected`.
fn value_is(h: &Harness, view: ViewId, expression: &str, expected: &str) -> Outcome {
    let wanted = serde_json_string(expected);
    let mut last = None;
    for _ in 0..200 {
        let value = h.evaluate(view, expression, None)?;
        if value.as_deref() == Some(wanted.as_str()) {
            return Ok(());
        }
        last = value;
        h.idle(std::time::Duration::from_millis(20), |_| None)?;
    }
    Err(format!("{expression} reads {last:?}, not {wanted}"))
}

/// `text` as a JSON string, as the engine's evaluation spells a string value.
fn serde_json_string(text: &str) -> String {
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// A click on a `<select>` reports its popup with every row; the consumer's choice changes the select, and a popup
/// closed unanswered leaves it.
pub fn option_menu(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(
        view,
        page(
            "<select id=s style='position:absolute;left:20px;top:30px;width:120px;height:30px'>\
             <option>one<optgroup label=more><option>two<option disabled>three</optgroup></select>",
        ),
    )?;
    h.host.set_focused(view, true);
    h.click(view, 40.0, 40.0);
    let menu = h.wait_for("the select's popup", |event| match event {
        Event::OptionMenu(menu) if menu.view == view => Some(menu.clone()),
        _ => None,
    })?;
    let rows: Vec<(&str, OptionKind, bool, bool)> = menu
        .items
        .iter()
        .map(|item| (item.label.as_str(), item.kind, item.enabled, item.selected))
        .collect();
    ensure(
        rows == [
            ("one", OptionKind::Option, true, true),
            ("more", OptionKind::GroupLabel, false, false),
            ("two", OptionKind::GroupChild, true, false),
            ("three", OptionKind::GroupChild, false, false),
        ],
        || format!("the popup's rows are {rows:?}"),
    )?;
    ensure(menu.area.x == 20 && menu.area.y == 30 && menu.area.width == 120, || {
        format!("the popup is placed by {:?}", menu.area)
    })?;
    h.host.choose_option(menu.token, Some(2));
    value_is(h, view, "s.options[s.selectedIndex].text", "two")?;

    h.click(view, 40.0, 40.0);
    let again = h.wait_for("the popup again", |event| match event {
        Event::OptionMenu(menu) if menu.view == view => Some(menu.clone()),
        _ => None,
    })?;
    h.host.choose_option(again.token, None);
    h.idle(std::time::Duration::from_millis(200), |_| None)?;
    value_is(h, view, "s.options[s.selectedIndex].text", "two")?;
    h.host.close_view(view);
    Ok(())
}

/// A right-click reports what lies under it, and `WebKit` shows no menu of its own.
pub fn context_menu(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(
        view,
        page(
            "<a href='https://link.invalid/x' style='position:absolute;left:0;top:0;width:100px;height:40px;\
             display:block'>link</a><input id=i style='position:absolute;left:0;top:80px;width:100px'>",
        ),
    )?;
    h.host.set_focused(view, true);
    let link = right_click(h, view, 20.0, 20.0)?;
    ensure(
        link.link.as_deref() == Some("https://link.invalid/x") && !link.editable,
        || format!("over the link: {link:?}"),
    )?;
    let field = right_click(h, view, 20.0, 90.0)?;
    ensure(field.editable && field.link.is_none(), || {
        format!("over the field: {field:?}")
    })?;
    ensure((field.x, field.y) == (20, 90), || {
        format!("the menu was asked for at ({}, {})", field.x, field.y)
    })?;
    h.host.close_view(view);
    Ok(())
}

fn right_click(h: &Harness, view: ViewId, x: f64, y: f64) -> Result<agterm_wpe::ContextMenu, String> {
    let pointer = |kind| {
        Input::Pointer(Pointer {
            kind,
            x,
            y,
            modifiers: Modifiers::empty(),
        })
    };
    h.host.input(view, pointer(PointerKind::Move));
    h.host.input(view, pointer(PointerKind::Down(Button::Secondary)));
    h.host.input(view, pointer(PointerKind::Up(Button::Secondary)));
    h.wait_for("a context menu", |event| match event {
        Event::ContextMenu(menu) if menu.view == view => Some(menu.clone()),
        _ => None,
    })
}

/// `requestAnimationFrame` runs at a display's rate while the consumer presents every frame: a page animates.
pub fn animation_frames(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(
        view,
        page(
            "<canvas id=c width=64 height=64></canvas><script>window.n = 0; const g = c.getContext('2d');\
             function draw() { window.n++; g.fillStyle = window.n % 2 ? '#c03030' : '#3030c0';\
             g.fillRect(0, 0, 64, 64); requestAnimationFrame(draw); } requestAnimationFrame(draw);</script>",
        ),
    )?;
    h.idle(std::time::Duration::from_secs(1), |_| None)?;
    let frames = h
        .evaluate(view, "window.n", None)?
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or_default();
    h.host.close_view(view);
    ensure(frames >= 30, || format!("{frames} animation frames in a second"))
}

/// An input method's composition shows in the focused field as a composition, and its commit lands as text.
pub fn input_method(h: &Harness, _: &Context) -> Outcome {
    let view = h.view(h.ephemeral(), Settings::overlay(true));
    h.load(
        view,
        page(
            "<input id=i><script>i.addEventListener('compositionstart', () => { document.title = 'composing'; });\
             i.addEventListener('compositionend', e => { document.title = 'composed:' + e.data; });</script>",
        ),
    )?;
    h.host.set_focused(view, true);
    h.evaluate(view, "i.focus(); true", None)?;
    h.click(view, 10.0, 10.0);
    h.host.ime_preedit(view, "にほ".to_owned(), 2);
    h.title(view, "composing")?;
    h.host.ime_commit(view, "日本".to_owned());
    value_is(h, view, "i.value", "日本")?;
    h.title(view, "composed:日本")?;
    h.host.close_view(view);
    Ok(())
}
