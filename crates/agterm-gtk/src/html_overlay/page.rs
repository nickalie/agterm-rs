//! One page's web view and the handlers that keep the model in step with it: load state, the page and title shown,
//! history, the navigation policy, the hand-off prompt and the bridge (upstream's `HtmlOverlayPage`).
//!
//! Every report to the core is a posted main-loop task, never a call in place: `WebKit` calls these handlers from its
//! own signals, some of them while the app state is busy.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use agterm_core::html::bridge::{ADAPTER_SCRIPT, HANDLER_NAME, HELPER_SCRIPT, RELAY_SCRIPT, WORLD};
use agterm_core::html::{
    HtmlLoadState, HtmlNavigation, HtmlNavigationDecision, HtmlOverlay, HtmlOverlayTheme, HtmlReloadTarget, HtmlSource,
    decide, errors,
};
use gtk::{gdk, glib};
use url::Url;
use uuid::Uuid;
use webkit6::prelude::*;
use webkit6::{
    ContextMenuAction, NavigationPolicyDecision, NavigationType, NetworkError, NetworkSession, PolicyDecision,
    PolicyDecisionType, PolicyError, ResponsePolicyDecision, ScriptDialogType, ScriptMessageReply, Settings,
    UserContentInjectedFrames, UserContentManager, UserScript, UserScriptInjectionTime, WebView, javascriptcore,
};

use super::files;
use super::panel::{Panel, PanelColors};
use super::policy::{self, DecisionTarget, LoadEvent, LoadOutcome, LoadTracker};
use super::registry;
use crate::state::{self, AppState, Poster};

/// The script world the theme runs in, apart from the page's and the bridge's (upstream's `themeWorld`).
const THEME_WORLD: &str = "agterm-theme";

/// Context-menu items that would open a window or save a file.
const REFUSED_MENU_ITEMS: [ContextMenuAction; 11] = [
    ContextMenuAction::OpenLinkInNewWindow,
    ContextMenuAction::DownloadLinkToDisk,
    ContextMenuAction::OpenImageInNewWindow,
    ContextMenuAction::DownloadImageToDisk,
    ContextMenuAction::OpenFrameInNewWindow,
    ContextMenuAction::OpenVideoInNewWindow,
    ContextMenuAction::OpenAudioInNewWindow,
    ContextMenuAction::DownloadVideoToDisk,
    ContextMenuAction::DownloadAudioToDisk,
    ContextMenuAction::InspectElement,
    ContextMenuAction::OpenLink,
];

/// The browser store a page is built on, or why it has none and loads nothing (upstream's `Storage`).
pub(super) enum Storage {
    Ready(NetworkSession),
    Unavailable(String),
}

/// What the page's handlers share.
struct PageState {
    /// The model's value as last applied.
    overlay: HtmlOverlay,
    applied_revision: u64,
    load: LoadTracker,
    /// The address of the document a browsing page shows; `None` until its first commit.
    shown_url: Option<Url>,
    /// A granted page's grant as a real path, which the scheme's handler serves from.
    grant_root: Option<PathBuf>,
    storage_failure: Option<String>,
    prompt: Option<adw::AlertDialog>,
    /// A declined prompt silences the page until real input reaches its view: script can click links in a loop, but
    /// it cannot make native pointer or key events.
    prompts_silenced: bool,
    on_screen: bool,
    life: Life,
}

impl PageState {
    /// Not released yet.
    fn active(&self) -> bool {
        self.life != Life::Released
    }
}

/// Where a page is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Life {
    Active,
    /// The page's `window.close()` asked for its close. It stays asked while the session sits in an undoable close,
    /// where no slot is found to close, so a restored page closes at its next apply.
    CloseRequested,
    /// Released: the handlers still connected do nothing.
    Released,
}

/// One page (upstream's `HtmlOverlayPage`).
pub(super) struct Page {
    id: Uuid,
    web_view: WebView,
    panel: Panel,
    content: UserContentManager,
    theme: HtmlOverlayTheme,
    poster: Poster,
    state: RefCell<PageState>,
}

impl Page {
    /// The page for `overlay`, its web view built on `storage` at `zoom` and loading its source.
    pub(super) fn new(
        overlay: &HtmlOverlay,
        background: Option<&str>,
        poster: &Poster,
        storage: Storage,
        zoom: f64,
    ) -> Rc<Self> {
        let colors = PanelColors::new(background);
        let (session, storage_failure) = match storage {
            Storage::Ready(session) => (session, None),
            Storage::Unavailable(failure) => (NetworkSession::new_ephemeral(), Some(failure)),
        };
        let content = UserContentManager::new();
        let web_view = WebView::builder()
            .network_session(&session)
            .settings(&settings(overlay.flags.javascript))
            .user_content_manager(&content)
            .zoom_level(zoom)
            .hexpand(true)
            .vexpand(true)
            .build();
        // a file page's unstyled canvas shows the themed panel behind it while authored backgrounds still paint; a
        // URL page keeps the browser's opaque canvas, which a web app styled against it relies on
        if overlay.themed() {
            web_view.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        }
        let panel = Panel::new(overlay, &colors, &web_view, poster);
        let page = Rc::new(Self {
            id: overlay.id,
            web_view,
            panel,
            content,
            theme: colors.theme(),
            poster: poster.clone(),
            state: RefCell::new(PageState {
                overlay: overlay.clone(),
                applied_revision: overlay.reload_revision,
                load: LoadTracker::default(),
                shown_url: None,
                grant_root: None,
                storage_failure,
                prompt: None,
                prompts_silenced: false,
                on_screen: false,
                life: Life::Active,
            }),
        });
        page.install_scripts(overlay);
        if overlay.themed() {
            page.connect_bridge();
        }
        page.connect_loads();
        page.connect_reports();
        page.connect_policy();
        page.connect_refusals();
        page.connect_input();
        files::refuse_file_pastes(&page.web_view);
        page.load_original();
        page
    }

    /// The panel the page is mounted by.
    pub(super) fn widget(&self) -> gtk::Widget {
        self.panel.widget()
    }

    pub(super) fn web_view(&self) -> &WebView {
        &self.web_view
    }

    pub(super) fn grant_root(&self) -> Option<PathBuf> {
        self.state.borrow().grant_root.clone()
    }

    #[cfg(test)]
    pub(super) fn on_screen(&self) -> bool {
        self.state.borrow().on_screen
    }

    fn weak(self: &Rc<Self>) -> Weak<Self> {
        Rc::downgrade(self)
    }

    fn active(&self) -> bool {
        self.state.borrow().active()
    }

    // one set: the theme, and on a file page the bridge's scripts (upstream's `installScripts`)
    fn install_scripts(&self, overlay: &HtmlOverlay) {
        let themed = overlay.themed();
        self.content.remove_all_scripts();
        let add = |source: &str, time: UserScriptInjectionTime, world: Option<&str>| {
            let frames = UserContentInjectedFrames::TopFrame;
            let script = match world {
                Some(world) => UserScript::for_world(source, frames, time, world, &[], &[]),
                None => UserScript::new(source, frames, time, &[], &[]),
            };
            self.content.add_script(&script);
        };
        add(
            &self.theme.script(themed),
            UserScriptInjectionTime::Start,
            Some(THEME_WORLD),
        );
        if !themed {
            return;
        }
        if overlay.flags.javascript {
            add(RELAY_SCRIPT, UserScriptInjectionTime::Start, Some(WORLD));
            add(HELPER_SCRIPT, UserScriptInjectionTime::Start, None);
        }
        add(ADAPTER_SCRIPT, UserScriptInjectionTime::End, Some(WORLD));
    }

    // MARK: - the bridge

    fn connect_bridge(self: &Rc<Self>) {
        self.content
            .register_script_message_handler_with_reply(HANDLER_NAME, Some(WORLD));
        let weak = self.weak();
        self.content
            .connect_script_message_with_reply_received(Some(HANDLER_NAME), move |_, value, reply| {
                match weak.upgrade() {
                    Some(page) => page.bridge_message(value, reply),
                    None => reply.return_error_message("page closed"),
                }
                true
            });
    }

    /// Runs one request the page sent and answers it once (upstream's `handleBridgeRequest`): the core resolves the
    /// page where it sits when the task runs, so a page that left its slot is refused before anything runs.
    fn bridge_message(&self, value: &javascriptcore::Value, reply: &ScriptMessageReply) {
        if !self.active() {
            reply.return_error_message("page closed");
            return;
        }
        let body = if value.is_object() { value.to_json(0) } else { None };
        let (Some(body), Some(context)) = (body, value.context()) else {
            reply.return_error_message("invalid request");
            return;
        };
        let token = registry::park(reply.clone(), context);
        let (id, body) = (self.id, body.to_string());
        state::post(&self.poster, move |state: &mut AppState| {
            match state.core.html_bridge_request(id, body.as_bytes()) {
                Ok(request) => state.handle_page_request(&request, token),
                Err(message) => registry::reject(token, &message),
            }
        });
    }

    // MARK: - load state and the page shown

    fn connect_loads(self: &Rc<Self>) {
        let weak = self.weak();
        self.web_view.connect_load_changed(move |_, event| {
            let Some(page) = weak.upgrade() else { return };
            match event {
                webkit6::LoadEvent::Started => page.load_event(&LoadEvent::Started),
                webkit6::LoadEvent::Committed => page.committed(),
                webkit6::LoadEvent::Finished => page.load_event(&LoadEvent::Finished),
                _ => {}
            }
        });
        let weak = self.weak();
        // handled, so `WebKit` shows no error page of its own: the panel shows the failure
        self.web_view.connect_load_failed(move |_, _, _, error| {
            if let Some(page) = weak.upgrade() {
                page.load_event(&failure_event(error));
            }
            true
        });
        let weak = self.weak();
        self.web_view.connect_web_process_terminated(move |_, _| {
            if let Some(page) = weak.upgrade() {
                page.state.borrow_mut().shown_url = None;
                page.load_event(&LoadEvent::Terminated);
            }
        });
    }

    fn load_event(&self, event: &LoadEvent) {
        let outcome = {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.load.handle(event)
        };
        self.report_outcome(outcome);
    }

    fn committed(&self) {
        let web = self.web_url();
        {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.load.handle(&LoadEvent::Committed);
            state.shown_url = web;
        }
        self.report_page();
    }

    fn report_outcome(&self, outcome: LoadOutcome) {
        match outcome {
            LoadOutcome::None => {}
            LoadOutcome::Loading => self.report_load(HtmlLoadState::Loading, None),
            LoadOutcome::Loaded => {
                self.report_page();
                self.report_load(HtmlLoadState::Loaded, None);
            }
            LoadOutcome::Failed(message) => self.report_load(HtmlLoadState::Failed, Some(message)),
        }
    }

    fn report_load(&self, load: HtmlLoadState, error: Option<String>) {
        {
            let state = self.state.borrow();
            if state.overlay.load_state == load && state.overlay.load_error == error {
                return;
            }
        }
        let id = self.id;
        state::post(&self.poster, move |state: &mut AppState| {
            if state.core.set_html_load_state(id, load, error) {
                state.settle();
            }
        });
    }

    fn fail(&self, message: String) {
        let outcome = self.state.borrow_mut().load.fail(message);
        self.report_outcome(outcome);
    }

    /// The web view's address as the app spells it.
    fn web_url(&self) -> Option<Url> {
        self.web_view
            .uri()
            .and_then(|uri| Url::parse(&policy::app_uri(&uri)).ok())
    }

    fn connect_reports(self: &Rc<Self>) {
        let weak = self.weak();
        self.web_view.connect_title_notify(move |_| {
            if let Some(page) = weak.upgrade() {
                page.report_page();
            }
        });
        let weak = self.weak();
        self.web_view.connect_uri_notify(move |_| {
            if let Some(page) = weak.upgrade() {
                page.url_changed();
            }
        });
        if let Some(history) = self.web_view.back_forward_list() {
            let weak = self.weak();
            history.connect_local("changed", false, move |_| {
                if let Some(page) = weak.upgrade() {
                    page.report_page();
                }
                None
            });
        }
    }

    fn url_changed(&self) {
        let web = self.web_url();
        {
            let mut state = self.state.borrow_mut();
            state.shown_url = policy::shown_after_change(state.shown_url.as_ref(), web.as_ref());
        }
        self.report_page();
    }

    /// Reports what the page shows and where its history can go (upstream's `reportPage`).
    fn report_page(&self) {
        if self.web_view.uri().is_none() {
            return;
        }
        let web = self.web_url();
        let info = {
            let state = self.state.borrow();
            if !state.active() {
                return;
            }
            let Some(url) = policy::page_url(&state.overlay, state.shown_url.as_ref(), web.as_ref()) else {
                return;
            };
            let title = self.web_view.title();
            let info = policy::page_info(
                &url,
                title.as_deref(),
                self.web_view.can_go_back(),
                self.web_view.can_go_forward(),
            );
            if state.overlay.current.as_ref() == Some(&info) {
                return;
            }
            info
        };
        let id = self.id;
        state::post(&self.poster, move |state: &mut AppState| {
            if state.core.set_html_page(id, Some(info)) {
                state.settle();
            }
        });
    }

    // MARK: - the model's value

    /// Takes the model's latest value and whether the page is on screen; reloads when its revision moved (upstream's
    /// `apply` and `setOnScreen`). A page out of sight asks nothing, and its pending prompt goes with it.
    pub(super) fn apply(&self, overlay: &HtmlOverlay, on_screen: bool) {
        let reload = {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.overlay = overlay.clone();
            state.on_screen = on_screen;
            let moved = overlay.reload_revision != state.applied_revision;
            state.applied_revision = overlay.reload_revision;
            moved.then_some(overlay.reload_target)
        };
        self.panel.update(overlay);
        if !on_screen {
            self.end_prompt();
        }
        self.close_if_requested();
        match reload {
            Some(HtmlReloadTarget::Current) => self.reload_shown(),
            Some(HtmlReloadTarget::Original) => self.load_original(),
            None => {}
        }
    }

    /// Reloads what the page shows; before a first commit `WebKit` has nothing to reload, so the source loads again.
    fn reload_shown(&self) {
        let reload = {
            let mut state = self.state.borrow_mut();
            let reload = !state.overlay.text_loaded() && state.load.committed();
            if reload {
                state.load.begin();
            }
            reload
        };
        if reload {
            self.web_view.reload();
        } else {
            self.load_original();
        }
    }

    /// Loads the page's source (upstream's `loadOriginal`): a URL as is, a granted file through the grant's scheme, a
    /// file without a grant from its text at `about:blank`; a page without its store loads nothing and fails.
    fn load_original(&self) {
        let source = {
            let mut state = self.state.borrow_mut();
            if let Some(failure) = state.storage_failure.clone() {
                drop(state);
                self.fail(failure);
                return;
            }
            state.load.begin();
            state.overlay.source.clone()
        };
        match source {
            HtmlSource::Url(url) => self.web_view.load_uri(url.as_str()),
            HtmlSource::File {
                path,
                grant_root: Some(root),
            } => {
                self.state.borrow_mut().grant_root = std::fs::canonicalize(&root).ok();
                match policy::granted_uri(&path) {
                    Some(uri) => self.web_view.load_uri(&uri),
                    None => self.fail(format!("not an absolute path: {path}")),
                }
            }
            HtmlSource::File { path, grant_root: None } => match std::fs::read_to_string(&path) {
                Ok(text) => self.web_view.load_html(&text, None),
                Err(error) => self.fail(error.to_string()),
            },
        }
    }

    pub(super) fn set_zoom(&self, zoom: f64) {
        self.web_view.set_zoom_level(zoom);
    }

    // MARK: - navigation and hand-offs

    /// A history step or hand-off (upstream's `navigate`).
    pub(super) fn navigate(&self, navigation: HtmlNavigation) -> Result<(), String> {
        match navigation {
            HtmlNavigation::Back => {
                if !self.web_view.can_go_back() {
                    return Err(errors::no_history(navigation));
                }
                self.web_view.go_back();
            }
            HtmlNavigation::Forward => {
                if !self.web_view.can_go_forward() {
                    return Err(errors::no_history(navigation));
                }
                self.web_view.go_forward();
            }
            HtmlNavigation::Browser => {
                let opened = self
                    .browser_url()
                    .is_some_and(|url| registry::open_in_browser(url.as_str()));
                if !opened {
                    return Err(errors::NO_BROWSER.to_owned());
                }
            }
            HtmlNavigation::Finder => {
                if !self.state.borrow().overlay.themed() {
                    return Err(errors::FINDER_REQUIRES_FILE.to_owned());
                }
                if let Some(path) = self.page_path() {
                    crate::app::reveal_in_file_manager(&path);
                }
            }
        }
        Ok(())
    }

    /// What a hand-off to the browser opens (upstream's `browserURL`).
    pub(super) fn browser_url(&self) -> Option<Url> {
        let web = self.web_url();
        let state = self.state.borrow();
        policy::browser_url(&state.overlay, state.shown_url.as_ref(), web.as_ref())
    }

    /// The file the page shows, which the file manager reveals.
    fn page_path(&self) -> Option<PathBuf> {
        let web = self.web_url();
        let state = self.state.borrow();
        policy::page_url(&state.overlay, state.shown_url.as_ref(), web.as_ref())?
            .to_file_path()
            .ok()
    }

    // MARK: - the navigation policy

    fn connect_policy(self: &Rc<Self>) {
        let weak = self.weak();
        self.web_view
            .connect_decide_policy(move |_, decision, kind| match weak.upgrade() {
                Some(page) => page.decide_policy(decision, kind),
                None => false,
            });
    }

    fn decide_policy(self: &Rc<Self>, decision: &PolicyDecision, kind: PolicyDecisionType) -> bool {
        match kind {
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction => {
                let Some(navigation) = decision.downcast_ref::<NavigationPolicyDecision>() else {
                    return false;
                };
                let Some(action) = navigation.navigation_action() else {
                    decision.ignore();
                    return true;
                };
                let uri = action.request().and_then(|request| request.uri());
                let clicked = action.navigation_type() == NavigationType::LinkClicked;
                let window = kind == PolicyDecisionType::NewWindowAction;
                let target = if window {
                    DecisionTarget::NewWindow
                } else {
                    DecisionTarget::Frame
                };
                let uri = uri.map(|uri| uri.to_string()).unwrap_or_default();
                if policy::navigation_proceeds(&uri, window, || self.allows(&uri, target, clicked)) {
                    decision.use_();
                } else {
                    decision.ignore();
                }
                true
            }
            PolicyDecisionType::Response => {
                let Some(response) = decision.downcast_ref::<ResponsePolicyDecision>() else {
                    return false;
                };
                self.decide_response(decision, response);
                true
            }
            _ => false,
        }
    }

    /// A response: nothing is saved to disk (`WebKit` reports an unshowable main resource as interrupted), and the main
    /// frame's address is checked against the policy, which a refused one fails while a load is pending.
    fn decide_response(self: &Rc<Self>, decision: &PolicyDecision, response: &ResponsePolicyDecision) {
        if !response.is_mime_type_supported() {
            decision.ignore();
            return;
        }
        let main = response.is_main_frame_main_resource();
        let uri = response
            .response()
            .and_then(|response| response.uri())
            .map(|uri| uri.to_string())
            .unwrap_or_default();
        let target = if main {
            DecisionTarget::MainResponse
        } else {
            DecisionTarget::SubframeResponse
        };
        if self.allows(&uri, target, false) {
            decision.use_();
            return;
        }
        decision.ignore();
        if main && self.active() {
            let outcome = self.state.borrow_mut().load.blocked(&policy::app_uri(&uri));
            self.report_outcome(outcome);
        }
    }

    /// Whether the shared policy allows `uri`; a hand-off it offers is asked about.
    fn allows(self: &Rc<Self>, uri: &str, target: DecisionTarget, clicked: bool) -> bool {
        let Ok(url) = Url::parse(&policy::app_uri(uri)) else {
            return false;
        };
        let decision = {
            let state = self.state.borrow();
            if !state.active() {
                return false;
            }
            decide(&policy::navigation_action(url.clone(), target, clicked), &state.overlay)
        };
        if decision == HtmlNavigationDecision::OpenExternal {
            let weak = self.weak();
            glib::idle_add_local_once(move || {
                if let Some(page) = weak.upgrade() {
                    page.ask_to_open(url);
                }
            });
        }
        decision == HtmlNavigationDecision::Allow
    }

    /// A page's own hand-off asks first, one prompt at a time, only while the page is on screen; a request while one
    /// is up is dropped (upstream's `askToOpen`). Cancel is the default, so typing meant for the page cannot approve
    /// a surprise prompt.
    fn ask_to_open(self: &Rc<Self>, url: Url) {
        {
            let state = self.state.borrow();
            if !state.active() || !state.on_screen || state.prompt.is_some() || state.prompts_silenced {
                return;
            }
        }
        if self.web_view.root().is_none() {
            return;
        }
        let shown = HtmlSource::origin_of(&url).unwrap_or_else(|| url.as_str().to_owned());
        let dialog = adw::AlertDialog::new(Some(&format!("Open {shown} in your browser?")), Some(url.as_str()));
        dialog.add_responses(&[("cancel", "Cancel"), ("open", "Open")]);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let weak = self.weak();
        dialog.connect_response(None, move |dialog, response| {
            if let Some(page) = weak.upgrade() {
                page.answer_prompt(dialog, &url, response == "open");
            }
        });
        self.state.borrow_mut().prompt = Some(dialog.clone());
        dialog.present(Some(&self.web_view));
    }

    fn answer_prompt(&self, dialog: &adw::AlertDialog, url: &Url, approved: bool) {
        {
            let mut state = self.state.borrow_mut();
            if state.prompt.as_ref() != Some(dialog) {
                return;
            }
            state.prompt = None;
            if !approved {
                state.prompts_silenced = true;
            }
        }
        if approved && !registry::open_in_browser(url.as_str()) {
            tracing::warn!("no default web browser to open the link in");
        }
    }

    fn end_prompt(&self) {
        let prompt = self.state.borrow_mut().prompt.take();
        if let Some(dialog) = prompt {
            dialog.force_close();
        }
    }

    // MARK: - what a page may not do

    fn connect_refusals(self: &Rc<Self>) {
        self.web_view.connect_create(|_, _| None);
        let weak = self.weak();
        // the page's `window.close()` closes the overlay, as the strip's close button does; deferred, since the close
        // releases this page
        self.web_view.connect_close(move |_| {
            if let Some(page) = weak.upgrade() {
                let mut state = page.state.borrow_mut();
                if state.life == Life::Active {
                    state.life = Life::CloseRequested;
                }
                drop(state);
                page.close_if_requested();
            }
        });
        // script dialogs are answered silently: dismissed, a page leaving still leaves
        self.web_view.connect_script_dialog(|_, dialog| {
            let dialog = dialog.clone();
            match dialog.dialog_type() {
                ScriptDialogType::Confirm => dialog.confirm_set_confirmed(false),
                ScriptDialogType::BeforeUnloadConfirm => dialog.confirm_set_confirmed(true),
                _ => {}
            }
            true
        });
        self.web_view.connect_run_file_chooser(|_, request| {
            request.cancel();
            true
        });
        self.web_view.connect_permission_request(|_, request| {
            request.deny();
            true
        });
        self.web_view.connect_enter_fullscreen(|_| true);
        self.web_view.connect_show_notification(|_, _| true);
        // a paste the clipboard's files could ride in goes too, as the paste keys refuse it
        self.web_view.connect_context_menu(|web_view, menu, _| {
            let pastes_refused = files::clipboard_may_carry_files(web_view);
            for item in menu.items() {
                let action = item.stock_action();
                let paste = matches!(action, ContextMenuAction::Paste | ContextMenuAction::PasteAsPlainText);
                if REFUSED_MENU_ITEMS.contains(&action) || (paste && pastes_refused) {
                    menu.remove(&item);
                }
            }
            false
        });
    }

    fn close_if_requested(&self) {
        if self.state.borrow().life != Life::CloseRequested {
            return;
        }
        let id = self.id;
        state::post(&self.poster, move |state: &mut AppState| {
            if state.core.close_html_overlay(id) {
                state.settle();
            }
        });
    }

    // MARK: - input

    /// Typing and clicks are user activity, so auto-follow cannot switch sessions under them, and real input lifts a
    /// declined prompt's silence; a click on a pane page moves the split's focus to its pane, and one on a session-wide
    /// page beside a pane ask to the pane the ask leaves uncovered (upstream's `onUserInput`, `onFocus`, `onClick`).
    fn connect_input(self: &Rc<Self>) {
        let input = gtk::EventControllerLegacy::new();
        input.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = self.weak();
        input.connect_event(move |_, event| {
            let click = match event.event_type() {
                gdk::EventType::KeyPress | gdk::EventType::TouchBegin => Some(false),
                gdk::EventType::ButtonPress => Some(true),
                _ => None,
            };
            if let (Some(page), Some(click)) = (weak.upgrade(), click) {
                page.user_input(click);
            }
            glib::Propagation::Proceed
        });
        self.web_view.add_controller(input);
        let focus = gtk::EventControllerFocus::new();
        let weak = self.weak();
        focus.connect_enter(move |_| {
            if let Some(page) = weak.upgrade() {
                page.focused();
            }
        });
        self.web_view.add_controller(focus);
    }

    fn user_input(&self, click: bool) {
        {
            let mut state = self.state.borrow_mut();
            if !state.active() {
                return;
            }
            state.prompts_silenced = false;
        }
        let id = self.id;
        state::post(&self.poster, move |state: &mut AppState| {
            let Some(slot) = state.core.html_overlay_slot(id) else {
                return;
            };
            state.core.note_user_activity(slot.session);
            let moved = click && (state.core.html_page_focused(id) | state.core.html_page_clicked(id));
            if moved {
                state.settle();
            }
            state.arm_auto_follow();
        });
    }

    /// The web view took the keyboard: a pane page's split focus follows it, unless the focus moved on before the
    /// task ran.
    fn focused(&self) {
        if !self.active() {
            return;
        }
        let id = self.id;
        state::post(&self.poster, move |state: &mut AppState| {
            if registry::holds_focus(id) && state.core.html_page_focused(id) {
                state.settle();
            }
        });
    }

    // MARK: - release

    /// Ends the page (upstream's `close`): its prompt, its bridge and its load go, its panel leaves its parent, and the
    /// handlers still connected do nothing.
    pub(super) fn close(&self) {
        self.end_prompt();
        let themed = {
            let mut state = self.state.borrow_mut();
            state.life = Life::Released;
            state.overlay.themed()
        };
        if themed {
            self.content
                .unregister_script_message_handler(HANDLER_NAME, Some(WORLD));
        }
        self.web_view.stop_loading();
        self.panel.release();
    }
}

/// The page's settings: its own scripts only with `--js` (script markup off keeps agterm's user scripts running in
/// their worlds, as `WebKit`'s `allowsContentJavaScript` does), and nothing that opens a window, reads other files,
/// goes full screen or plays unasked.
fn settings(javascript: bool) -> Settings {
    let settings = Settings::new();
    settings.set_enable_javascript_markup(javascript);
    settings.set_javascript_can_open_windows_automatically(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings.set_enable_developer_extras(false);
    settings.set_enable_fullscreen(false);
    settings.set_media_playback_requires_user_gesture(true);
    settings
}

/// A failed load as the tracker reads it (upstream's `reportFailure`): a cancel superseded by the next load, a policy
/// interruption, or a failure.
fn failure_event(error: &glib::Error) -> LoadEvent {
    if error.matches(NetworkError::Cancelled) {
        return LoadEvent::Cancelled;
    }
    if error.matches(PolicyError::FrameLoadInterruptedByPolicyChange) || error.matches(PolicyError::CannotShowMimeType)
    {
        return LoadEvent::Interrupted(error.message().to_owned());
    }
    LoadEvent::Failed(error.message().to_owned())
}
