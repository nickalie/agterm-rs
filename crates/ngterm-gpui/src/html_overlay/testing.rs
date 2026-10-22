//! The engine's recording stand-in for the unit tests (the element's, the deck's): [`Recorder`] implements
//! [`Web`], keeping every command it gets and every frame answer, and [`install`] makes it the registry's engine.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use ngterm_wpe::{
    BufferId, EditingCommand, Geometry, Input, Load, MenuToken, PolicyToken, ReplyToken, SchemeResponse, SchemeToken,
    SessionId, SessionSpec, UserScript, ViewId, ViewSpec,
};

use super::engine::{Presenter, Web};

/// A command the pages or their elements sent.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Call {
    CreateSession(SessionSpec),
    CreateView { handlers: usize, transparent: bool },
    CloseView(ViewId),
    Load(Load),
    Reload,
    GoBack,
    GoForward,
    Geometry(Geometry),
    Visible(bool),
    Focused(bool),
    Input(Input),
    FallBack,
    Clipboard(String),
    Choose(MenuToken, Option<u32>),
    Editing(EditingCommand),
    Preedit(String),
    Commit(String),
    Other,
}

/// The engine's stand-in: every command recorded, every frame answer counted.
#[derive(Default)]
pub(crate) struct Recorder {
    calls: RefCell<Vec<Call>>,
    presented: Arc<Mutex<Vec<BufferId>>>,
    next: Cell<u64>,
}

impl Recorder {
    fn record(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }

    fn id(&self) -> u64 {
        self.next.set(self.next.get() + 1);
        self.next.get()
    }

    /// The commands since the last take.
    pub(crate) fn take(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }

    /// The buffers answered so far.
    pub(crate) fn presented(&self) -> Vec<BufferId> {
        self.presented.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// Forgets every page and runs the registry on a fresh recorder, which it returns.
pub(crate) fn install() -> Rc<Recorder> {
    super::registry::reset();
    let web = Rc::new(Recorder::default());
    super::registry::use_web(Rc::clone(&web) as Rc<dyn Web>);
    web
}

impl Web for Recorder {
    fn create_session(&self, spec: SessionSpec) -> SessionId {
        self.record(Call::CreateSession(spec));
        SessionId::from_raw(self.id())
    }
    fn drop_session(&self, _: SessionId) {}
    fn clear_session(&self, _: SessionId) {}
    fn create_view(&self, spec: ViewSpec) -> ViewId {
        self.record(Call::CreateView {
            handlers: spec.message_handlers.len(),
            transparent: spec.background == Some(ngterm_wpe::Rgba::TRANSPARENT),
        });
        ViewId::from_raw(self.id())
    }
    fn close_view(&self, view: ViewId) {
        self.record(Call::CloseView(view));
    }
    fn load(&self, _: ViewId, load: Load) {
        self.record(Call::Load(load));
    }
    fn reload(&self, _: ViewId) {
        self.record(Call::Reload);
    }
    fn go_back(&self, _: ViewId) {
        self.record(Call::GoBack);
    }
    fn go_forward(&self, _: ViewId) {
        self.record(Call::GoForward);
    }
    fn set_zoom(&self, _: ViewId, _: f64) {
        self.record(Call::Other);
    }
    fn set_scripts(&self, _: ViewId, _: Vec<UserScript>) {
        self.record(Call::Other);
    }
    fn set_geometry(&self, _: ViewId, geometry: Geometry) {
        self.record(Call::Geometry(geometry));
    }
    fn set_visible(&self, _: ViewId, visible: bool) {
        self.record(Call::Visible(visible));
    }
    fn set_focused(&self, _: ViewId, focused: bool) {
        self.record(Call::Focused(focused));
    }
    fn input(&self, _: ViewId, input: Input) {
        self.record(Call::Input(input));
    }
    fn fall_back_to_shm(&self, _: ViewId) {
        self.record(Call::FallBack);
    }
    fn decide_policy(&self, _: PolicyToken, _: bool) {
        self.record(Call::Other);
    }
    fn reply_script_message(&self, _: ReplyToken, _: Result<String, String>) {
        self.record(Call::Other);
    }
    fn finish_scheme_request(&self, _: SchemeToken, _: SchemeResponse) {
        self.record(Call::Other);
    }
    fn set_clipboard(&self, text: String) {
        self.record(Call::Clipboard(text));
    }
    fn choose_option(&self, token: MenuToken, index: Option<u32>) {
        self.record(Call::Choose(token, index));
    }
    fn editing_command(&self, _: ViewId, command: EditingCommand) {
        self.record(Call::Editing(command));
    }
    fn ime_preedit(&self, _: ViewId, text: String, _: u32) {
        self.record(Call::Preedit(text));
    }
    fn ime_commit(&self, _: ViewId, text: String) {
        self.record(Call::Commit(text));
    }
    fn presenter(&self) -> Presenter {
        let presented = Arc::clone(&self.presented);
        Arc::new(move |_, buffer| presented.lock().unwrap_or_else(PoisonError::into_inner).push(buffer))
    }
    fn shut_down(&self) {}
}
