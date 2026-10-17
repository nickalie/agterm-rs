//! The web engine as the overlay drives it: [`Web`], the commands of `agterm_wpe::Host` the pages send, so the
//! registry, the pages and the element run against a recording stand-in in the unit tests (`testing::Recorder`), where
//! no `WebKit` can start (one engine per process, and it needs the overlay of a vendored package).

use std::sync::Arc;

use agterm_wpe::{
    BufferId, EditingCommand, Geometry, Host, Input, Load, MenuToken, PolicyToken, ReplyToken, SchemeResponse,
    SchemeToken, SessionId, SessionSpec, UserScript, ViewId, ViewSpec,
};

/// Tells the engine a page's frame is on screen; called from the renderer's threads.
pub type Presenter = Arc<dyn Fn(ViewId, BufferId) + Send + Sync>;

/// The engine's commands (`agterm_wpe::Host`'s, one for one).
pub trait Web {
    fn create_session(&self, spec: SessionSpec) -> SessionId;
    fn drop_session(&self, session: SessionId);
    fn clear_session(&self, session: SessionId);
    fn create_view(&self, spec: ViewSpec) -> ViewId;
    fn close_view(&self, view: ViewId);
    fn load(&self, view: ViewId, load: Load);
    fn reload(&self, view: ViewId);
    fn go_back(&self, view: ViewId);
    fn go_forward(&self, view: ViewId);
    fn set_zoom(&self, view: ViewId, zoom: f64);
    fn set_scripts(&self, view: ViewId, scripts: Vec<UserScript>);
    fn set_geometry(&self, view: ViewId, geometry: Geometry);
    fn set_visible(&self, view: ViewId, visible: bool);
    fn set_focused(&self, view: ViewId, focused: bool);
    fn input(&self, view: ViewId, input: Input);
    fn fall_back_to_shm(&self, view: ViewId);
    fn decide_policy(&self, token: PolicyToken, allow: bool);
    fn reply_script_message(&self, token: ReplyToken, reply: Result<String, String>);
    fn finish_scheme_request(&self, token: SchemeToken, response: SchemeResponse);
    fn set_clipboard(&self, text: String);
    fn choose_option(&self, token: MenuToken, index: Option<u32>);
    fn editing_command(&self, view: ViewId, command: EditingCommand);
    fn ime_preedit(&self, view: ViewId, text: String, cursor: u32);
    fn ime_commit(&self, view: ViewId, text: String);
    /// What answers a frame's outcome, which may run on any thread.
    fn presenter(&self) -> Presenter;
    /// Closes every page and stops the engine (the quit).
    fn shut_down(&self);
}

/// The real engine.
pub struct Engine(pub Arc<Host>);

impl Web for Engine {
    fn create_session(&self, spec: SessionSpec) -> SessionId {
        self.0.create_session(spec)
    }

    fn drop_session(&self, session: SessionId) {
        self.0.drop_session(session);
    }

    fn clear_session(&self, session: SessionId) {
        self.0.clear_session(session);
    }

    fn create_view(&self, spec: ViewSpec) -> ViewId {
        self.0.create_view(spec)
    }

    fn close_view(&self, view: ViewId) {
        self.0.close_view(view);
    }

    fn load(&self, view: ViewId, load: Load) {
        self.0.load(view, load);
    }

    fn reload(&self, view: ViewId) {
        self.0.reload(view);
    }

    fn go_back(&self, view: ViewId) {
        self.0.go_back(view);
    }

    fn go_forward(&self, view: ViewId) {
        self.0.go_forward(view);
    }

    fn set_zoom(&self, view: ViewId, zoom: f64) {
        self.0.set_zoom(view, zoom);
    }

    fn set_scripts(&self, view: ViewId, scripts: Vec<UserScript>) {
        self.0.set_scripts(view, scripts);
    }

    fn set_geometry(&self, view: ViewId, geometry: Geometry) {
        self.0.set_geometry(view, geometry);
    }

    fn set_visible(&self, view: ViewId, visible: bool) {
        self.0.set_visible(view, visible);
    }

    fn set_focused(&self, view: ViewId, focused: bool) {
        self.0.set_focused(view, focused);
    }

    fn input(&self, view: ViewId, input: Input) {
        self.0.input(view, input);
    }

    fn fall_back_to_shm(&self, view: ViewId) {
        self.0.fall_back_to_shm(view);
    }

    fn decide_policy(&self, token: PolicyToken, allow: bool) {
        self.0.decide_policy(token, allow);
    }

    fn reply_script_message(&self, token: ReplyToken, reply: Result<String, String>) {
        self.0.reply_script_message(token, reply);
    }

    fn finish_scheme_request(&self, token: SchemeToken, response: SchemeResponse) {
        self.0.finish_scheme_request(token, response);
    }

    fn set_clipboard(&self, text: String) {
        self.0.set_clipboard(text);
    }

    fn choose_option(&self, token: MenuToken, index: Option<u32>) {
        self.0.choose_option(token, index);
    }

    fn editing_command(&self, view: ViewId, command: EditingCommand) {
        self.0.editing_command(view, command);
    }

    fn ime_preedit(&self, view: ViewId, text: String, cursor: u32) {
        self.0.ime_preedit(view, text, cursor);
    }

    fn ime_commit(&self, view: ViewId, text: String) {
        self.0.ime_commit(view, text);
    }

    fn presenter(&self) -> Presenter {
        let host = Arc::clone(&self.0);
        Arc::new(move |view, buffer| host.frame_presented(view, buffer))
    }

    fn shut_down(&self) {
        self.0.shutdown();
    }
}
