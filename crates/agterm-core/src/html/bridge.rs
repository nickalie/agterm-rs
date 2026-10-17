//! A page's message turned into the socket's request (upstream's `HtmlBridge.swift`), and agterm's scripts a page gets
//! with the answer it reads back (`Views/HtmlOverlayBridge.swift`). A page speaks the wire protocol and nothing else:
//! what it left out is filled from where it sits, and only there, so an explicit target, `active`, window or batch
//! resolves exactly as it would over the socket.

use agterm_proto::command::{AskCmd, BrowserCmd, FontCmd, HooksCmd, KeymapCmd, SessionCmd, WindowCmd, ZmxCmd};
use agterm_proto::{Args, Command, Request, Response};
use serde_json::Value;
use uuid::Uuid;

use crate::model::OverlayPane;
use crate::pane_env::uuid_string;

/// The script message handler's name a page posts to (upstream's `handlerName`).
pub const HANDLER_NAME: &str = "agterm";
/// The script world agterm's adapter runs in, apart from the page's own (upstream's `world`).
pub const WORLD: &str = "agterm-bridge";

/// The `data-agterm` adapter (upstream's `adapterScript`), run in [`WORLD`] so it works with the page's JavaScript off
/// and the page cannot replace it, while the DOM it listens on is shared with the page. Its submit and click listeners
/// run in the capture phase and cancel the default before anything awaits, so a tagged form never navigates. A button
/// inside a tagged form belongs to the form's submit; a tagged button elsewhere must be `type="button"`, or it would
/// submit an untagged form it sits in as well. A key is refused when it would take two values, because keeping either
/// one silently drops the other. `postMessage` answers with a promise: the handler is registered with a reply.
pub const ADAPTER_SCRIPT: &str = r"(() => {
  const handler = window.webkit.messageHandlers.agterm;
  const show = (el, text) => {
    const selector = el.getAttribute('data-agterm-into');
    const into = selector ? document.querySelector(selector) : null;
    if (into) into.textContent = text;
  };
  const shown = (result) => {
    if (result && typeof result.text === 'string') return result.text;
    return JSON.stringify(result ?? {});
  };
  const formArgs = (form, args, submitter) => {
    const seen = new Set();
    for (const control of form.elements) {
      const name = control.name;
      if (!name || control.matches(':disabled')) continue;
      const type = (control.type || '').toLowerCase();
      if (['submit', 'button', 'reset', 'image'].includes(type) && control !== submitter) continue;
      if (type === 'file') throw new Error(`file inputs are not supported: ${name}`);
      if (type === 'radio' && !control.checked) continue;
      let value;
      if (type === 'checkbox') {
        value = control.checked;
      } else if (type === 'number') {
        if (control.value === '') continue;
        value = control.valueAsNumber;
        if (!Number.isFinite(value)) throw new Error(`not a number: ${name}`);
      } else if (control instanceof HTMLSelectElement && control.multiple) {
        const picked = Array.from(control.selectedOptions);
        if (picked.length > 1) throw new Error(`more than one value for ${name}`);
        if (picked.length === 0) continue;
        value = picked[0].value;
      } else {
        value = control.value;
      }
      if (seen.has(name)) throw new Error(`more than one value for ${name}`);
      seen.add(name);
      args[name] = value;
    }
    return args;
  };
  const send = (el, form, submitter) => {
    const body = {cmd: el.getAttribute('data-agterm')};
    const target = el.getAttribute('data-agterm-target');
    if (target !== null) body.target = target;
    try {
      const base = el.getAttribute('data-agterm-args');
      let args = base === null ? undefined : JSON.parse(base);
      if (form) args = formArgs(form, args ?? {}, submitter);
      if (args !== undefined) body.args = args;
    } catch (error) {
      show(el, error.message);
      return;
    }
    handler.postMessage(body).then((result) => show(el, shown(result)), (error) => show(el, error.message));
  };
  document.addEventListener('submit', (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || !form.hasAttribute('data-agterm')) return;
    event.preventDefault();
    send(form, form, event.submitter);
  }, true);
  document.addEventListener('click', (event) => {
    const el = event.target instanceof Element ? event.target.closest('[data-agterm]') : null;
    if (!el || el instanceof HTMLFormElement || el.closest('form[data-agterm]')) return;
    if (el instanceof HTMLButtonElement && el.type !== 'button') return;
    event.preventDefault();
    send(el, null, null);
  }, true);
})();";

/// A `--js` page's relay in the bridge's world. WPE (as `WebKitGTK`) names no frame on a script message, so the
/// handler is registered in agterm's world alone, which neither page scripts nor frames reach, and the page's own
/// requests come through `window.postMessage`: this relay forwards only those whose source is the top window itself,
/// as upstream refuses requests from frames. (The Linux fork's answer to the same gap.)
pub const RELAY_SCRIPT: &str = r"(() => {
  const handler = window.webkit.messageHandlers.agterm;
  window.addEventListener('message', (event) => {
    const data = event.data;
    if (event.source !== window || !data || data.agterm !== 'request') return;
    const answer = (fields) => window.postMessage(Object.assign({agterm: 'reply', id: data.id}, fields), '*');
    handler.postMessage(data.body).then((result) => answer({result}),
      (error) => answer({error: String((error && error.message) || error)}));
  });
})();";

/// A `--js` page's `agterm.request` in its own world, over the relay (upstream's `helperScript`, the same entry point
/// and envelope, where upstream posts to a second registration of the handler); the second argument is the request envelope, not the arguments themselves.
pub const HELPER_SCRIPT: &str = r"(() => {
  let next = 0;
  const waiting = new Map();
  window.addEventListener('message', (event) => {
    const data = event.data;
    if (event.source !== window || !data || data.agterm !== 'reply' || !waiting.has(data.id)) return;
    const {resolve, reject} = waiting.get(data.id);
    waiting.delete(data.id);
    if (typeof data.error === 'string') reject(new Error(data.error)); else resolve(data.result);
  });
  const request = (cmd, {target, args} = {}) => new Promise((resolve, reject) => {
    const body = {cmd};
    if (target !== undefined) body.target = target;
    if (args !== undefined) body.args = args;
    const id = ++next;
    waiting.set(id, {resolve, reject});
    try {
      window.postMessage({agterm: 'request', id, body}, '*');
    } catch (error) {
      waiting.delete(id);
      reject(error);
    }
  });
  Object.defineProperty(window, 'agterm', {value: Object.freeze({request}), enumerable: false});
})();";

/// Each needs more than one request and reply: a stream handed over, or work done after the reply.
const REFUSED: [Command; 3] = [
    Command::Zmx(ZmxCmd::Present),
    Command::Session(SessionCmd::OverlayJobRun),
    Command::Zmx(ZmxCmd::Reset),
];
/// Commands that address no session, or address it some other way than by `target`.
const NOT_SESSION_TARGETED: [Command; 3] = [
    Command::Session(SessionCmd::New),
    Command::Session(SessionCmd::Go),
    Command::Session(SessionCmd::OverlayJobRun),
];
/// Session commands outside the `session.` names; `ask.open` joins them unless it asks in the window's GUI.
const SESSION_TARGETED: [Command; 5] = [
    Command::Notify,
    Command::Font(FontCmd::Inc),
    Command::Font(FontCmd::Dec),
    Command::Font(FontCmd::Reset),
    Command::Keymap(KeymapCmd::Run),
];
/// The page's own overlay commands, which also get its pane.
const OWN_OVERLAY: [Command; 4] = [
    Command::Session(SessionCmd::OverlayClose),
    Command::Session(SessionCmd::OverlayReload),
    Command::Session(SessionCmd::OverlayNavigate),
    Command::Session(SessionCmd::OverlaySubmit),
];
/// These take their window as `target`, not as `args.window`.
const WINDOW_TARGETED: [Command; 9] = [
    Command::Window(WindowCmd::Close),
    Command::Window(WindowCmd::Select),
    Command::Window(WindowCmd::Rename),
    Command::Window(WindowCmd::Delete),
    Command::Window(WindowCmd::Resize),
    Command::Window(WindowCmd::Move),
    Command::Window(WindowCmd::Zoom),
    Command::Window(WindowCmd::Fullscreen),
    Command::Window(WindowCmd::Minimize),
];
/// App-global commands that refuse any window.
const WINDOWLESS: [Command; 4] = [
    Command::Hooks(HooksCmd::Reload),
    Command::Hooks(HooksCmd::List),
    Command::Browser(BrowserCmd::Clear),
    Command::Browser(BrowserCmd::Links),
];
/// Their ids name something else (a remote session, dashboard cells), and they still land in a local window.
const PLACED_LOCALLY: [Command; 2] = [Command::Zmx(ZmxCmd::Attach), Command::Dashboard];

/// Where a requesting page sits when its request arrives (upstream's `HtmlBridgePage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlBridgePage {
    /// The id of the window holding it, as the wire spells window ids.
    pub window: Option<String>,
    pub session: Uuid,
    /// The pane slot holding it; `None` for the session-wide one.
    pub pane: Option<OverlayPane>,
}

/// Why a page's message was not turned into a request (upstream's `HtmlBridgeError`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlBridgeError {
    pub message: String,
}

impl HtmlBridgeError {
    /// A command a page cannot send (upstream's `HtmlBridgeError.unsupported`).
    fn unsupported(cmd: Command) -> Self {
        Self {
            message: format!("{cmd} cannot be sent from a page"),
        }
    }
}

/// The request a page's message `data` (JSON) makes, what it left out filled from where it sits (upstream's
/// `HtmlBridge.request(from:page:)`). It decodes through the socket's decoder, so a page and the socket get the same
/// `invalid request: ...` text.
///
/// # Errors
///
/// The message is not a request, or names a command a page cannot send.
pub fn request(data: &[u8], page: &HtmlBridgePage) -> Result<Request, HtmlBridgeError> {
    let decoded = crate::decode_request(data).map_err(|error| HtmlBridgeError {
        message: error.to_string(),
    })?;
    if REFUSED.contains(&decoded.cmd) {
        return Err(HtmlBridgeError::unsupported(decoded.cmd));
    }
    Ok(with_defaults(decoded, page))
}

/// Fills what an unaddressed request left out from where the page sits (upstream's `HtmlBridge.withDefaults`).
fn with_defaults(mut request: Request, page: &HtmlBridgePage) -> Request {
    let mut args = request.args.take().unwrap_or_default();
    let addressed = request.target.is_some() || args.targets.is_some() || args.window.is_some();
    if request.cmd == Command::Session(SessionCmd::OverlayReload) && args.current.is_none() {
        args.current = Some(true);
    }
    if PLACED_LOCALLY.contains(&request.cmd) {
        if args.window.is_none() {
            args.window.clone_from(&page.window);
        }
    } else if !addressed && !WINDOWLESS.contains(&request.cmd) {
        if is_session_targeted(request.cmd, &args) {
            request.target = Some(uuid_string(page.session));
            if OWN_OVERLAY.contains(&request.cmd) && args.pane.is_none() {
                args.pane = page.pane.map(|pane| pane.wire_name().to_owned());
            }
        } else if WINDOW_TARGETED.contains(&request.cmd) {
            request.target.clone_from(&page.window);
        } else {
            args.window.clone_from(&page.window);
        }
    }
    request.args = (args != Args::default()).then_some(args);
    request
}

/// Whether an unaddressed `cmd` acts on the page's own session (upstream's `HtmlBridge.isSessionTargeted`): a
/// `session.` command that addresses its session by `target`, a session command outside those names, or an `ask.open`
/// that asks in the terminal rather than the window's GUI.
fn is_session_targeted(cmd: Command, args: &Args) -> bool {
    if cmd == Command::Ask(AskCmd::Open) {
        return args.style.as_deref() != Some("gui");
    }
    (cmd.wire_name().starts_with("session.") && !NOT_SESSION_TARGETED.contains(&cmd)) || SESSION_TARGETED.contains(&cmd)
}

/// The answer a page gets (upstream's `HtmlOverlayBridge.reply`): the result as a JSON object, `{}` without one (or
/// with one that is not a JSON container, which Foundation's `JSONSerialization` does not read back).
///
/// # Errors
///
/// The response's error, `request failed` when it has none.
pub fn reply(response: &Response) -> Result<Value, String> {
    if !response.ok {
        return Err(response.error.clone().unwrap_or_else(|| "request failed".to_owned()));
    }
    Ok(match &response.result {
        Some(result @ (Value::Object(_) | Value::Array(_))) => result.clone(),
        _ => Value::Object(serde_json::Map::new()),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Upstream's `HtmlBridgeTests` fixture: a page in the right pane of a session in window `W1`.
    struct Fixture {
        session: Uuid,
        page: HtmlBridgePage,
    }

    impl Fixture {
        fn new() -> Self {
            let session = Uuid::new_v4();
            Self {
                session,
                page: HtmlBridgePage {
                    window: Some("W1".to_owned()),
                    session,
                    pane: Some(OverlayPane::Right),
                },
            }
        }

        fn build(&self, json: &str) -> Result<Request, HtmlBridgeError> {
            request(json.as_bytes(), &self.page)
        }

        fn request(&self, json: &str) -> Request {
            self.build(json).unwrap()
        }

        fn session_target(&self) -> String {
            uuid_string(self.session)
        }
    }

    fn window(request: &Request) -> Option<&str> {
        request.args.as_ref().and_then(|args| args.window.as_deref())
    }

    fn pane(request: &Request) -> Option<&str> {
        request.args.as_ref().and_then(|args| args.pane.as_deref())
    }

    fn targets(request: &Request) -> Option<Vec<String>> {
        request.args.as_ref().and_then(|args| args.targets.clone())
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn a_message_decodes_into_the_socket_request() {
        let built = Fixture::new().request(r#"{"cmd":"theme.set","args":{"name":"Dracula"}}"#);
        assert_eq!(built.cmd, Command::Theme(agterm_proto::command::ThemeCmd::Set));
        assert_eq!(built.args.unwrap().name.as_deref(), Some("Dracula"));
    }

    #[test]
    fn an_unknown_command_or_wrong_type_gets_the_sockets_error_text() {
        let fixture = Fixture::new();
        let unknown = fixture.build(r#"{"cmd":"session.nope"}"#).unwrap_err();
        let wrong_type = fixture
            .build(r#"{"cmd":"session.overlay.resize","args":{"sizePercent":"big"}}"#)
            .unwrap_err();
        for error in [&unknown, &wrong_type] {
            assert!(error.message.starts_with("invalid request: "), "{}", error.message);
        }
        let socket = crate::decode_request(br#"{"cmd":"session.nope"}"#).unwrap_err();
        assert_eq!(unknown.message, socket.to_string());
        assert_eq!(
            unknown.message,
            "invalid request: Cannot initialize Command from invalid String value session.nope"
        );
    }

    #[test]
    fn an_untargeted_session_command_acts_on_the_pages_own_session() {
        let fixture = Fixture::new();
        for json in [
            r#"{"cmd":"session.rename","args":{"name":"build"}}"#,
            r#"{"cmd":"session.status","args":{"status":"completed"}}"#,
        ] {
            let built = fixture.request(json);
            assert_eq!(built.target, Some(fixture.session_target()), "{json}");
            assert_eq!(window(&built), None, "{json}");
        }
    }

    #[test]
    fn explicit_targets_windows_and_batches_are_kept_as_given() {
        let fixture = Fixture::new();
        let active = fixture.request(r#"{"cmd":"session.rename","target":"active","args":{"name":"x"}}"#);
        assert_eq!(active.target.as_deref(), Some("active"));
        let other = fixture.request(r#"{"cmd":"session.select","target":"9C41"}"#);
        assert_eq!(other.target.as_deref(), Some("9C41"));
        assert_eq!(window(&other), None);
        let batch = fixture.request(r#"{"cmd":"session.close","args":{"targets":["A","B"]}}"#);
        assert_eq!(batch.target, None);
        assert_eq!(targets(&batch), Some(strings(&["A", "B"])));
    }

    #[test]
    fn an_explicit_window_gets_no_page_session_or_pane() {
        let fixture = Fixture::new();
        let rename = fixture.request(r#"{"cmd":"session.rename","args":{"name":"x","window":"W2"}}"#);
        assert_eq!(rename.target, None);
        assert_eq!(window(&rename), Some("W2"));
        let close = fixture.request(r#"{"cmd":"session.overlay.close","args":{"window":"W2"}}"#);
        assert_eq!(close.target, None);
        assert_eq!(pane(&close), None);
    }

    #[test]
    fn the_window_is_filled_only_when_nothing_else_addresses_the_request() {
        let fixture = Fixture::new();
        let tree = fixture.request(r#"{"cmd":"tree"}"#);
        assert_eq!(window(&tree), Some("W1"));
        let go = fixture.request(r#"{"cmd":"session.go","args":{"to":"next"}}"#);
        assert_eq!(window(&go), Some("W1"));
        assert_eq!(go.target, None);
        let workspace = fixture.request(r#"{"cmd":"workspace.select","target":"WS"}"#);
        assert_eq!(window(&workspace), None);
    }

    #[test]
    fn a_command_refusing_any_window_gets_none() {
        let fixture = Fixture::new();
        for cmd in ["hooks.reload", "hooks.list", "browser.clear", "browser.links"] {
            let built = fixture.request(&format!(r#"{{"cmd":"{cmd}"}}"#));
            assert_eq!(built.target, None, "{cmd}");
            assert_eq!(built.args, None, "{cmd}");
        }
    }

    #[test]
    fn an_untargeted_window_command_acts_on_the_pages_window() {
        let fixture = Fixture::new();
        for cmd in [
            "window.close",
            "window.select",
            "window.rename",
            "window.delete",
            "window.resize",
            "window.move",
            "window.zoom",
            "window.fullscreen",
            "window.minimize",
        ] {
            let built = fixture.request(&format!(r#"{{"cmd":"{cmd}"}}"#));
            assert_eq!(built.target.as_deref(), Some("W1"), "{cmd}");
            assert_eq!(built.args, None, "{cmd}");
            let explicit = fixture.request(&format!(r#"{{"cmd":"{cmd}","target":"W2"}}"#));
            assert_eq!(explicit.target.as_deref(), Some("W2"), "{cmd}");
        }
    }

    #[test]
    fn an_untargeted_font_command_acts_on_the_pages_session() {
        let fixture = Fixture::new();
        for cmd in ["font.inc", "font.dec", "font.reset"] {
            let built = fixture.request(&format!(r#"{{"cmd":"{cmd}"}}"#));
            assert_eq!(built.target, Some(fixture.session_target()), "{cmd}");
            assert_eq!(window(&built), None, "{cmd}");
        }
    }

    /// A name-only `keymap.run` from a background page ran against the window's selected session.
    #[test]
    fn an_untargeted_keymap_run_acts_on_the_pages_session() {
        let fixture = Fixture::new();
        let built = fixture.request(r#"{"cmd":"keymap.run","args":{"name":"Deploy"}}"#);
        assert_eq!(built.target, Some(fixture.session_target()));
        assert_eq!(window(&built), None);
        let explicit = fixture.request(r#"{"cmd":"keymap.run","target":"other","args":{"name":"Deploy"}}"#);
        assert_eq!(explicit.target.as_deref(), Some("other"));
    }

    #[test]
    fn a_notification_acts_on_the_pages_session() {
        let fixture = Fixture::new();
        let built = fixture.request(r#"{"cmd":"notify","args":{"title":"t"}}"#);
        assert_eq!(built.target, Some(fixture.session_target()));
        assert_eq!(window(&built), None);
    }

    #[test]
    fn a_terminal_ask_goes_to_the_pages_session_and_a_gui_ask_to_its_window() {
        let fixture = Fixture::new();
        let terminal = fixture.request(r#"{"cmd":"ask.open","args":{"title":"t"}}"#);
        assert_eq!(terminal.target, Some(fixture.session_target()));
        let gui = fixture.request(r#"{"cmd":"ask.open","args":{"title":"t","style":"gui"}}"#);
        assert_eq!(gui.target, None);
        assert_eq!(window(&gui), Some("W1"));
    }

    #[test]
    fn attach_and_dashboard_keep_their_ids_and_land_in_the_pages_window() {
        let fixture = Fixture::new();
        let attach = fixture.request(r#"{"cmd":"zmx.attach","target":"REMOTE","args":{"host":"mac"}}"#);
        assert_eq!(attach.target.as_deref(), Some("REMOTE"));
        assert_eq!(window(&attach), Some("W1"));
        let dashboard = fixture.request(r#"{"cmd":"dashboard","args":{"targets":["A","B"]}}"#);
        assert_eq!(targets(&dashboard), Some(strings(&["A", "B"])));
        assert_eq!(window(&dashboard), Some("W1"));
        let elsewhere = fixture.request(r#"{"cmd":"dashboard","args":{"targets":["A"],"window":"W2"}}"#);
        assert_eq!(window(&elsewhere), Some("W2"));
    }

    #[test]
    fn the_pane_is_filled_only_for_the_pages_own_overlay_commands() {
        let fixture = Fixture::new();
        for cmd in [
            "session.overlay.close",
            "session.overlay.reload",
            "session.overlay.navigate",
            "session.overlay.submit",
        ] {
            let built = fixture.request(&format!(r#"{{"cmd":"{cmd}"}}"#));
            assert_eq!(built.target, Some(fixture.session_target()), "{cmd}");
            assert_eq!(pane(&built), Some("right"), "{cmd}");
        }
        let rename = fixture.request(r#"{"cmd":"session.rename","args":{"name":"x"}}"#);
        assert_eq!(pane(&rename), None);
        let targeted = fixture.request(r#"{"cmd":"session.overlay.close","target":"9C41"}"#);
        assert_eq!(pane(&targeted), None);
        let session_wide = HtmlBridgePage {
            window: Some("W1".to_owned()),
            session: fixture.session,
            pane: None,
        };
        let built = request(br#"{"cmd":"session.overlay.close"}"#, &session_wide).unwrap();
        assert_eq!(built.target, Some(fixture.session_target()));
        assert_eq!(built.args, None, "nothing left to carry");
    }

    #[test]
    fn reload_is_current_only_when_current_was_omitted() {
        let fixture = Fixture::new();
        let current = |json: &str| fixture.request(json).args.and_then(|args| args.current);
        assert_eq!(current(r#"{"cmd":"session.overlay.reload"}"#), Some(true));
        assert_eq!(
            current(r#"{"cmd":"session.overlay.reload","args":{"current":false}}"#),
            Some(false)
        );
    }

    #[test]
    fn stream_and_after_reply_commands_are_refused() {
        let fixture = Fixture::new();
        for cmd in ["zmx.present", "session.overlay.job.run", "zmx.reset"] {
            let error = fixture.build(&format!(r#"{{"cmd":"{cmd}"}}"#)).unwrap_err();
            assert_eq!(error.message, format!("{cmd} cannot be sent from a page"));
        }
    }

    #[test]
    fn a_windowless_page_leaves_the_window_out() {
        let page = HtmlBridgePage {
            window: None,
            session: Uuid::new_v4(),
            pane: None,
        };
        let tree = request(br#"{"cmd":"tree"}"#, &page).unwrap();
        assert_eq!(tree.target, None);
        assert_eq!(tree.args, None);
    }

    #[test]
    fn the_scripts_post_to_the_agterm_handler() {
        for script in [ADAPTER_SCRIPT, RELAY_SCRIPT] {
            assert!(script.starts_with("(() => {\n  const handler = window.webkit.messageHandlers.agterm;\n"));
        }
        for script in [ADAPTER_SCRIPT, RELAY_SCRIPT, HELPER_SCRIPT] {
            assert!(script.ends_with("\n})();"));
        }
        assert!(
            !HELPER_SCRIPT.contains("messageHandlers"),
            "the page world reaches the handler only through the relay"
        );
        assert!(HELPER_SCRIPT.contains(
            "Object.defineProperty(window, 'agterm', {value: Object.freeze({request}), enumerable: false});"
        ));
        assert!(ADAPTER_SCRIPT.contains("if (el instanceof HTMLButtonElement && el.type !== 'button') return;"));
    }

    #[test]
    fn a_reply_is_the_result_object_or_the_error() {
        let result = json!({"id": "ABC", "text": "done"});
        assert_eq!(reply(&Response::with_result(result.clone())), Ok(result));
        assert_eq!(reply(&Response::success()), Ok(json!({})));
        assert_eq!(reply(&Response::with_result(json!("bare"))), Ok(json!({})));
        assert_eq!(
            reply(&Response::failure("no such session: x")),
            Err("no such session: x".to_owned())
        );
        let unexplained = Response {
            ok: false,
            result: Some(json!({"id": "x"})),
            error: None,
        };
        assert_eq!(reply(&unexplained), Err("request failed".to_owned()));
    }
}
