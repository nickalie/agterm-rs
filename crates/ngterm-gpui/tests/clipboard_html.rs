//! The `text/html` clipboard offer of our `gpui-pre-linux` patch (`with_clipboard_html`), on a real X11 server: a GPUI
//! app on a private Xvfb copies with HTML (beside the text on the clipboard, alone on the primary selection, as
//! Ghostty's `copy_to_clipboard:mixed` and `:html` offer them), and another X11 client reads the selections' targets
//! and data the way a paste does. The writer is this test binary again, run in the sandbox ([`WRITER_ENV`]).
//!
//! Wayland is not covered here: a Wayland client may own a selection only with the serial of an input event, which a
//! headless compositor never delivers.

#[macro_use]
#[expect(dead_code, reason = "the sandbox serves the process tests too")]
mod support;

use std::time::{Duration, Instant};

use support::{Sandbox, TestResult, have, lavapipe_icd};
use x11rb::connection::Connection as _;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, WindowClass};

/// Set in the writer's environment: the test body becomes the writer.
const WRITER_ENV: &str = "NGTERM_TEST_CLIPBOARD_WRITER";
const PLAIN: &str = "plain words";
const HTML: &str = "<pre><span style=\"color:red\">red words</span></pre>";
const HTML_ONLY: &str = "<b>bold</b>";

/// The writer: copies with HTML and keeps owning the selections until it is terminated.
#[test]
#[ignore = "run by html_copies_offer_text_html_on_x11 in its sandbox"]
fn clipboard_writer() {
    if std::env::var_os(WRITER_ENV).is_none() {
        return;
    }
    gpui_kit::application().run(|cx| {
        let mixed = gpui_linux::ClipboardHtml {
            html: HTML.to_owned(),
            with_text: true,
        };
        gpui_linux::with_clipboard_html(mixed, || {
            cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(PLAIN.to_owned()));
        });
        let alone = gpui_linux::ClipboardHtml {
            html: HTML_ONLY.to_owned(),
            with_text: false,
        };
        gpui_linux::with_clipboard_html(alone, || {
            cx.write_to_primary(gpui_kit::ClipboardItem::new_string(HTML_ONLY.to_owned()));
        });
        println!("written");
    });
}

/// An X11 client reading selections.
struct Reader {
    conn: x11rb::rust_connection::RustConnection,
    window: u32,
    property: u32,
}

impl Reader {
    fn new(display: u32) -> TestResult<Self> {
        let (conn, screen) = x11rb::connect(Some(&format!(":{display}")))?;
        let root = conn.setup().roots[screen].root;
        let window = conn.generate_id()?;
        conn.create_window(
            0,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        let property = conn.intern_atom(false, b"NGTERM_TEST_PROPERTY")?.reply()?.atom;
        conn.flush()?;
        Ok(Self { conn, window, property })
    }

    fn atom(&self, name: &str) -> TestResult<u32> {
        Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    }

    /// The selection converted to `target`: its type and bytes; `None` when the owner refused it.
    fn convert(&self, selection: &str, target: &str) -> TestResult<Option<(u32, Vec<u8>)>> {
        let (selection, target) = (self.atom(selection)?, self.atom(target)?);
        self.conn
            .convert_selection(self.window, selection, target, self.property, x11rb::CURRENT_TIME)?;
        self.conn.flush()?;
        let deadline = Instant::now() + support::DEADLINE;
        loop {
            if let Some(Event::SelectionNotify(notify)) = self.conn.poll_for_event()? {
                if notify.property == u32::from(AtomEnum::NONE) {
                    return Ok(None);
                }
                let reply = self
                    .conn
                    .get_property(true, self.window, self.property, AtomEnum::ANY, 0, u32::MAX)?
                    .reply()?;
                return Ok(Some((reply.type_, reply.value)));
            }
            if Instant::now() > deadline {
                return Err("no SelectionNotify".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The names of the targets the selection's owner offers.
    fn targets(&self, selection: &str) -> TestResult<Vec<String>> {
        let Some((_, bytes)) = self.convert(selection, "TARGETS")? else {
            return Err("no TARGETS".into());
        };
        let mut names = Vec::new();
        for atom in bytes.as_chunks::<4>().0 {
            let atom = u32::from_ne_bytes(*atom);
            names.push(String::from_utf8(self.conn.get_atom_name(atom)?.reply()?.name)?);
        }
        Ok(names)
    }
}

#[test]
fn html_copies_offer_text_html_on_x11() -> TestResult {
    if !have("Xvfb") {
        skip!("Xvfb is not installed");
    }
    let mut sandbox = Sandbox::new()?;
    let display = sandbox.start_xvfb()?;
    if let Some(lavapipe) = lavapipe_icd() {
        sandbox.use_software_rendering(&lavapipe);
    }
    let mut writer = sandbox.command(std::env::current_exe()?);
    writer
        .args([
            "clipboard_writer",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(WRITER_ENV, "1");
    let child = sandbox.spawn(writer, "writer.log")?;
    sandbox.wait_log("writer.log", "written")?;

    let reader = Reader::new(display)?;
    let targets = reader.targets("CLIPBOARD")?;
    assert!(
        targets.iter().any(|t| t == "text/html") && targets.iter().any(|t| t == "UTF8_STRING"),
        "{targets:?}"
    );
    let (_, html) = reader.convert("CLIPBOARD", "text/html")?.ok_or("no text/html")?;
    assert_eq!(String::from_utf8(html)?, HTML);
    let (_, plain) = reader.convert("CLIPBOARD", "UTF8_STRING")?.ok_or("no text")?;
    assert_eq!(String::from_utf8(plain)?, PLAIN);

    let primary = reader.targets("PRIMARY")?;
    assert!(primary.iter().any(|t| t == "text/html"), "{primary:?}");
    assert!(!primary.iter().any(|t| t == "UTF8_STRING"), "HTML alone: {primary:?}");
    let (_, html) = reader.convert("PRIMARY", "text/html")?.ok_or("no text/html")?;
    assert_eq!(String::from_utf8(html)?, HTML_ONLY);
    sandbox.terminate(child)?;
    Ok(())
}
