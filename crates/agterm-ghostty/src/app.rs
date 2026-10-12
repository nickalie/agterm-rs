//! Process-wide libghostty initialisation and the app: its runtime callbacks and its tick.
//!
//! libghostty calls the host through C callbacks. `wakeup` comes from any thread; every other one comes from the
//! thread that owns the [`App`] (inside [`App::tick`] or a surface call), so the [`Runtime`] lives in a thread local of
//! that thread and needs no `Send`. No panic crosses a callback: one is logged and the process aborts.

use std::cell::RefCell;
use std::ffi::{CString, c_char, c_void};
use std::os::unix::ffi::OsStringExt;
use std::panic::AssertUnwindSafe;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::OnceLock;

use agterm_ghostty_sys as sys;

use crate::action::{Action, SurfaceKey, Target};
use crate::clipboard::{
    ClipboardConfirm, ClipboardContent, ClipboardKind, ClipboardRead, ClipboardReadResult, ClipboardRequestKind,
    PendingRequest, copy_contents, copy_strings,
};
use crate::config::Config;
use crate::{ColorScheme, Error};

static INIT: OnceLock<Result<(), Error>> = OnceLock::new();

/// Initialise libghostty once for the process; later calls return the first call's outcome.
///
/// Every other entry point of this crate calls it first. Only `argv[0]` is handed over: libghostty scans its argv
/// for `+action` CLI verbs, and agterm's own arguments are not ghostty's.
///
/// libghostty snapshots the process environment here and hands that snapshot to every shell it spawns, so the
/// environment must be final before this runs and must not change afterwards: a variable added later reallocates
/// the C `environ` array under libghostty's copy of the pointer.
///
/// # Errors
///
/// [`Error::Init`] when `ghostty_init` fails, now or on the first call.
pub fn init() -> Result<(), Error> {
    *INIT.get_or_init(init_once)
}

fn init_once() -> Result<(), Error> {
    let argv0 = std::env::args_os()
        .next()
        .and_then(|arg| CString::new(arg.into_vec()).ok())
        .unwrap_or_else(|| c"agterm".to_owned());
    // Leaked on purpose: libghostty keeps the vector as its process argv for the rest of the process.
    let argv: &'static mut [*mut c_char; 2] = Box::leak(Box::new([argv0.into_raw(), std::ptr::null_mut()]));
    // SAFETY: argv is a null-terminated vector of one NUL-terminated string, alive for the whole process, and the
    // OnceLock guarantees ghostty_init runs once.
    let status = unsafe { sys::ghostty_init(1, argv.as_mut_ptr()) };
    if status == 0 { Ok(()) } else { Err(Error::Init(status)) }
}

/// What the host does for libghostty. Called on the thread that owns the [`App`], never re-entered for the same
/// callback while one runs unless the host itself calls back into libghostty.
pub trait Runtime {
    /// Perform `action` for `target`; `true` means handled. An unhandled action gets libghostty's own fallback.
    fn action(&self, target: Target, action: Action) -> bool;

    /// Whether `read` can be served: [`ClipboardReadResult::Started`] commits the host to
    /// [`Runtime::start_clipboard_read`], which follows at once.
    fn clipboard_read_result(&self, read: &ClipboardRead) -> ClipboardReadResult;

    /// Serve a read [`Runtime::clipboard_read_result`] accepted, now or later, through `pending`.
    fn start_clipboard_read(&self, read: ClipboardRead, pending: PendingRequest);

    /// Confirm or deny a request libghostty held back, now or later, through `pending`.
    fn confirm_clipboard_request(&self, confirm: ClipboardConfirm, pending: PendingRequest);

    /// Write `contents` (every representation) to `clipboard`. `confirm` asks the user first (`clipboard-write = ask`).
    fn write_clipboard(
        &self,
        surface: Option<SurfaceKey>,
        clipboard: ClipboardKind,
        contents: Vec<ClipboardContent>,
        confirm: bool,
    );

    /// The surface's process ended (or it asked to close). The host must not free the surface inside this call.
    fn close_surface(&self, surface: SurfaceKey, process_alive: bool);
}

/// How the host learns, from any thread, that libghostty has work for [`App::tick`].
pub type Wakeup = Box<dyn Fn() + Send + Sync>;

thread_local! {
    static RUNTIME: RefCell<Option<Rc<dyn Runtime>>> = const { RefCell::new(None) };
}

/// The thread's runtime, cloned out so no borrow is held while it runs (it may call back into libghostty).
fn runtime() -> Option<Rc<dyn Runtime>> {
    RUNTIME.with(|slot| slot.borrow().clone())
}

/// The libghostty app: one per process, owned by the thread that drives it. Not `Send`. Every surface holds the app
/// alive, so it is freed only after the last of them.
#[derive(Debug)]
pub struct App {
    inner: Rc<AppHandle>,
}

/// The owned `ghostty_app_t`, shared by the [`App`] and its surfaces.
#[derive(Debug)]
pub(crate) struct AppHandle {
    raw: NonNull<c_void>,
    /// The wakeup closure libghostty's runtime userdata points at; freed after the app.
    wakeup: NonNull<Wakeup>,
}

impl AppHandle {
    pub(crate) fn as_ptr(&self) -> sys::ghostty_app_t {
        self.raw.as_ptr()
    }
}

impl Drop for AppHandle {
    fn drop(&mut self) {
        // SAFETY: raw is live and freed once, here, after every surface (each holds this handle).
        unsafe { sys::ghostty_app_free(self.raw.as_ptr()) };
        // SAFETY: wakeup came from Box::into_raw in App::new; the app that used it is gone.
        drop(unsafe { Box::from_raw(self.wakeup.as_ptr()) });
        RUNTIME.with(|slot| slot.borrow_mut().take());
    }
}

impl App {
    /// Create the app with `config`, routing callbacks to `runtime` on this thread and wakeups to `wakeup`.
    ///
    /// # Errors
    ///
    /// [`Error::AppExists`] when this thread already owns an app, [`Error::Init`] when libghostty cannot be
    /// initialised, [`Error::Null`] when it cannot create the app.
    pub fn new(config: &Config, runtime: Rc<dyn Runtime>, wakeup: Wakeup) -> Result<Self, Error> {
        init()?;
        let taken = RUNTIME.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_some() {
                return true;
            }
            *slot = Some(runtime);
            false
        });
        if taken {
            return Err(Error::AppExists);
        }
        let wakeup = NonNull::from(Box::leak(Box::new(wakeup)));
        let runtime_config = sys::ghostty_runtime_config_s {
            userdata: wakeup.as_ptr().cast(),
            supports_selection_clipboard: true,
            wakeup_cb: Some(wakeup_cb),
            action_cb: Some(action_cb),
            read_clipboard_cb: Some(read_clipboard_cb),
            confirm_read_clipboard_cb: Some(confirm_read_clipboard_cb),
            write_clipboard_cb: Some(write_clipboard_cb),
            close_surface_cb: Some(close_surface_cb),
        };
        // SAFETY: runtime_config is valid for the call and libghostty copies it; config is a live, finalized config
        // it clones; the userdata box outlives the app (freed in Drop after ghostty_app_free).
        let raw = unsafe { sys::ghostty_app_new(&raw const runtime_config, config.as_ptr()) };
        let Some(raw) = NonNull::new(raw) else {
            // SAFETY: wakeup came from Box::leak above and libghostty never took it.
            drop(unsafe { Box::from_raw(wakeup.as_ptr()) });
            RUNTIME.with(|slot| slot.borrow_mut().take());
            return Err(Error::Null("ghostty_app_new"));
        };
        Ok(Self {
            inner: Rc::new(AppHandle { raw, wakeup }),
        })
    }

    /// The shared handle a surface keeps.
    pub(crate) fn handle(&self) -> Rc<AppHandle> {
        Rc::clone(&self.inner)
    }

    /// Drain libghostty's pending work: mailbox messages, actions, surface updates.
    pub fn tick(&self) {
        // SAFETY: a live app on its owning thread.
        unsafe { sys::ghostty_app_tick(self.inner.as_ptr()) };
    }

    /// Apply a new config to the app. libghostty answers with an app-target [`Action::ConfigChange`] carrying the
    /// config it applied.
    pub fn update_config(&self, config: &Config) {
        // SAFETY: a live app; config is live and libghostty clones it.
        unsafe { sys::ghostty_app_update_config(self.inner.as_ptr(), config.as_ptr()) };
    }

    /// Tell the app which side of a `theme = light:…,dark:…` pair is active.
    pub fn set_color_scheme(&self, scheme: ColorScheme) {
        // SAFETY: a live app.
        unsafe { sys::ghostty_app_set_color_scheme(self.inner.as_ptr(), scheme.raw()) };
    }
}

/// Runs a callback body; a panic is logged and aborts, since unwinding into libghostty is undefined.
fn guard<T>(callback: &'static str, body: impl FnOnce() -> T) -> T {
    match std::panic::catch_unwind(AssertUnwindSafe(body)) {
        Ok(value) => value,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            tracing::error!(callback, message, "panic in a libghostty callback, aborting");
            std::process::abort();
        }
    }
}

unsafe extern "C" fn wakeup_cb(userdata: *mut c_void) {
    guard("wakeup", || {
        if userdata.is_null() {
            return;
        }
        // SAFETY: the userdata is the app's wakeup box, alive until after ghostty_app_free; Wakeup is Sync.
        let wakeup = unsafe { &*userdata.cast::<Wakeup>() };
        wakeup();
    });
}

/// The key a surface was created with, read back from libghostty.
fn surface_key(surface: sys::ghostty_surface_t) -> Option<SurfaceKey> {
    if surface.is_null() {
        return None;
    }
    // SAFETY: a non-null surface handed to a callback is live for the callback.
    SurfaceKey::from_userdata(unsafe { sys::ghostty_surface_userdata(surface) })
}

unsafe extern "C" fn action_cb(
    _app: sys::ghostty_app_t,
    target: sys::ghostty_target_s,
    action: sys::ghostty_action_s,
) -> bool {
    guard("action", || {
        let Some(runtime) = runtime() else { return false };
        let target = match target.tag {
            sys::GHOSTTY_TARGET_SURFACE => {
                // SAFETY: the tag selects the surface member.
                match surface_key(unsafe { target.target.surface }) {
                    Some(key) => Target::Surface(key),
                    None => return false,
                }
            }
            _ => Target::App,
        };
        // SAFETY: libghostty is delivering this action now; its member matches its tag and its pointers are valid
        // for this callback.
        let action = unsafe { Action::decode(&action) };
        runtime.action(target, action)
    })
}

unsafe extern "C" fn read_clipboard_cb(
    userdata: *mut c_void,
    clipboard: sys::ghostty_clipboard_e,
    state: *mut c_void,
    mimes: *const *const c_char,
    mimes_len: usize,
    list: bool,
) -> sys::ghostty_clipboard_read_result_e {
    guard("read_clipboard", || {
        let unsupported = ClipboardReadResult::Unsupported.raw();
        let (Some(runtime), Some(surface), Some(state)) =
            (runtime(), SurfaceKey::from_userdata(userdata), NonNull::new(state))
        else {
            return unsupported;
        };
        let read = ClipboardRead {
            surface,
            clipboard: ClipboardKind::from_raw(clipboard),
            // SAFETY: libghostty passes mimes_len NUL-terminated strings, valid for this callback.
            mimes: unsafe { copy_strings(mimes, mimes_len) },
            list,
        };
        let result = runtime.clipboard_read_result(&read);
        if result == ClipboardReadResult::Started {
            runtime.start_clipboard_read(read, PendingRequest { surface, state });
        }
        result.raw()
    })
}

unsafe extern "C" fn confirm_read_clipboard_cb(
    userdata: *mut c_void,
    confirm: *const sys::ghostty_clipboard_confirm_s,
    state: *mut c_void,
    request: sys::ghostty_clipboard_request_e,
) {
    guard("confirm_read_clipboard", || {
        let (Some(runtime), Some(surface), Some(state)) =
            (runtime(), SurfaceKey::from_userdata(userdata), NonNull::new(state))
        else {
            return;
        };
        // SAFETY: null or a confirmation libghostty keeps alive for this callback.
        let confirm = unsafe { confirm.as_ref() };
        let confirm = ClipboardConfirm {
            surface,
            request: ClipboardRequestKind::from_raw(request),
            // SAFETY: the arrays and strings are valid for this callback per the header's contract.
            contents: confirm
                .map(|c| unsafe { copy_contents(c.contents, c.contents_len) })
                .unwrap_or_default(),
            // SAFETY: as above.
            available: confirm
                .map(|c| unsafe { copy_strings(c.available, c.available_len) })
                .unwrap_or_default(),
            // SAFETY: null or NUL-terminated, valid for this callback.
            name: confirm.and_then(|c| unsafe { crate::action::owned(c.name) }),
            can_remember: confirm.is_some_and(|c| c.can_remember),
        };
        runtime.confirm_clipboard_request(confirm, PendingRequest { surface, state });
    });
}

unsafe extern "C" fn write_clipboard_cb(
    userdata: *mut c_void,
    clipboard: sys::ghostty_clipboard_e,
    contents: *const sys::ghostty_clipboard_content_s,
    len: usize,
    confirm: bool,
) {
    guard("write_clipboard", || {
        let Some(runtime) = runtime() else { return };
        // SAFETY: libghostty passes len representations valid for this callback.
        let contents = unsafe { copy_contents(contents, len) };
        if contents.is_empty() {
            return;
        }
        let surface = SurfaceKey::from_userdata(userdata);
        runtime.write_clipboard(surface, ClipboardKind::from_raw(clipboard), contents, confirm);
    });
}

unsafe extern "C" fn close_surface_cb(userdata: *mut c_void, process_alive: bool) {
    guard("close_surface", || {
        let (Some(runtime), Some(surface)) = (runtime(), SurfaceKey::from_userdata(userdata)) else {
            return;
        };
        runtime.close_surface(surface, process_alive);
    });
}
