//! One libghostty surface: a terminal with its child process and renderer.
//!
//! The OpenGL platform (agterm's Linux patch) renders into whatever GL context is current: the host makes its
//! context current before [`Surface::new`], [`Surface::draw`], [`Surface::release_gpu_resources`] and dropping the
//! surface, all on the app's thread.

use std::ffi::{CString, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;

use agterm_ghostty_sys as sys;

use crate::action::SurfaceKey;
use crate::app::{App, AppHandle};
use crate::clipboard::{ClipboardContent, PendingRequest};
use crate::config::Config;
use crate::input::{KeyEvent, Mods, MouseButton, MouseState, ScrollMods};
use crate::{ColorScheme, Error};

/// Where a new surface sits, which decides what it inherits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SurfaceContext {
    #[default]
    Window,
    Tab,
    Split,
}

impl SurfaceContext {
    fn raw(self) -> sys::ghostty_surface_context_e {
        match self {
            Self::Window => sys::GHOSTTY_SURFACE_CONTEXT_WINDOW,
            Self::Tab => sys::GHOSTTY_SURFACE_CONTEXT_TAB,
            Self::Split => sys::GHOSTTY_SURFACE_CONTEXT_SPLIT,
        }
    }
}

/// How to create a surface. `None` fields take the config's value.
#[derive(Debug, Clone, Default)]
pub struct SurfaceOptions<'a> {
    /// The scale of device pixels to logical ones.
    pub scale_factor: f64,
    pub font_size: Option<f32>,
    pub working_directory: Option<&'a Path>,
    /// A command run through `/bin/sh -c` instead of the configured one.
    pub command: Option<&'a str>,
    /// Text typed into the child once it starts.
    pub initial_input: Option<&'a str>,
    /// Variables added to the child's environment.
    pub env: &'a [(String, String)],
    /// Keep the surface on libghostty's "Process exited" prompt after `command` ends.
    pub wait_after_command: bool,
    pub context: SurfaceContext,
}

/// The C strings a surface config points at; libghostty may read them after creation, so the surface keeps them.
#[derive(Debug, Default)]
struct OptionBuffers {
    strings: Vec<CString>,
    env: Vec<sys::ghostty_env_var_s>,
}

impl OptionBuffers {
    fn keep(&mut self, value: &[u8]) -> Result<*const c_char, Error> {
        let string = CString::new(value).map_err(|_| Error::Nul)?;
        // a CString's heap buffer does not move when the CString itself moves into the vector
        let ptr = string.as_ptr();
        self.strings.push(string);
        Ok(ptr)
    }

    fn keep_optional(&mut self, value: Option<&[u8]>) -> Result<*const c_char, Error> {
        value.map_or(Ok(std::ptr::null()), |value| self.keep(value))
    }
}

/// The size libghostty laid the surface out at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceSize {
    pub columns: u16,
    pub rows: u16,
    pub width_px: u32,
    pub height_px: u32,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
}

/// Where an input method's candidate window goes: the cursor cell, in logical units from the surface's top left.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImePoint {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// An owned `ghostty_surface_t`. Not `Send`: it lives on the app's thread. Dropping it frees the surface and ends its
/// child, exactly once.
#[derive(Debug)]
pub struct Surface {
    raw: NonNull<c_void>,
    key: SurfaceKey,
    _buffers: OptionBuffers,
    /// Keeps the app alive until this surface is freed (fields drop after `Drop::drop`).
    _app: Rc<AppHandle>,
}

impl Surface {
    /// Create a surface in `app` that libghostty knows by `key`, rendering into the GL context current on this thread.
    ///
    /// # Errors
    ///
    /// [`Error::Nul`] when an option holds a NUL byte, [`Error::Null`] when libghostty cannot create the surface
    /// (no usable GL context, a display asleep, no memory).
    pub fn new(app: &App, key: SurfaceKey, options: &SurfaceOptions<'_>) -> Result<Self, Error> {
        let mut buffers = OptionBuffers::default();
        // SAFETY: ghostty_surface_config_new only fills a struct with defaults.
        let mut config = unsafe { sys::ghostty_surface_config_new() };
        config.platform_tag = sys::GHOSTTY_PLATFORM_OPENGL;
        config.platform = sys::ghostty_platform_u {
            opengl: sys::ghostty_platform_opengl_s {
                userdata: key.as_userdata(),
                make_current: None,
                present: None,
            },
        };
        config.userdata = key.as_userdata();
        config.scale_factor = options.scale_factor;
        config.font_size = options.font_size.unwrap_or(0.0);
        config.working_directory =
            buffers.keep_optional(options.working_directory.map(|p| p.as_os_str().as_bytes()))?;
        config.command = buffers.keep_optional(options.command.map(str::as_bytes))?;
        config.initial_input = buffers.keep_optional(options.initial_input.map(str::as_bytes))?;
        config.wait_after_command = options.wait_after_command;
        config.context = options.context.raw();
        for (name, value) in options.env {
            let key = buffers.keep(name.as_bytes())?;
            let value = buffers.keep(value.as_bytes())?;
            buffers.env.push(sys::ghostty_env_var_s { key, value });
        }
        if !buffers.env.is_empty() {
            config.env_vars = buffers.env.as_mut_ptr();
            config.env_var_count = buffers.env.len();
        }
        let app = app.handle();
        // SAFETY: app is live on this thread; config and every buffer it points into stay alive (moved into the
        // surface below; a Vec's heap buffer and a CString's do not move with them).
        let raw = unsafe { sys::ghostty_surface_new(app.as_ptr(), &raw const config) };
        let raw = NonNull::new(raw).ok_or(Error::Null("ghostty_surface_new"))?;
        Ok(Self {
            raw,
            key,
            _buffers: buffers,
            _app: app,
        })
    }

    /// The key this surface was created with.
    #[must_use]
    pub fn surface_key(&self) -> SurfaceKey {
        self.key
    }

    fn ptr(&self) -> sys::ghostty_surface_t {
        self.raw.as_ptr()
    }

    /// Draw a frame into the current GL context.
    pub fn draw(&self) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_draw(self.ptr()) };
    }

    /// Queue a redraw with unchanged contents (after a reparent, or a size push that kept the grid).
    pub fn refresh(&self) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_refresh(self.ptr()) };
    }

    /// The drawable size in device pixels.
    pub fn set_size(&self, width: u32, height: u32) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_set_size(self.ptr(), width, height) };
    }

    pub fn set_content_scale(&self, x: f64, y: f64) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_set_content_scale(self.ptr(), x, y) };
    }

    pub fn set_focus(&self, focused: bool) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_set_focus(self.ptr(), focused) };
    }

    /// Whether the terminal is visible. Every change is a terminal visibility edge (mode 2033 reports it).
    pub fn set_occlusion(&self, visible: bool) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_set_occlusion(self.ptr(), visible) };
    }

    /// Free the renderer's GPU resources (render targets, font atlas textures, cell and uniform buffers) while the
    /// surface is hidden; the next [`Surface::draw`] re-creates them at the size current then.
    ///
    /// Call it on the app's thread **with this surface's GL context current** (its `GtkGLArea`'s, as for
    /// [`Surface::draw`]): the objects are deleted in whatever context is current, so another context would lose its
    /// own objects instead. libghostty's renderer does this itself on occlusion only where it draws on its own
    /// thread (macOS); this OpenGL embedding draws on the app thread, so the host calls it (agterm's
    /// `0004-embedded-release-gpu-resources.patch`). Repeating it is harmless. It is not occlusion: the terminal and
    /// its visibility (mode 2033 reports) are untouched, so pair it with [`Surface::set_occlusion`] as needed, and do
    /// not draw a hidden surface, which would re-create everything.
    pub fn release_gpu_resources(&self) {
        // SAFETY: a live surface on its thread; the caller has its GL context current (documented above).
        unsafe { sys::ghostty_surface_release_gpu_resources(self.ptr()) };
    }

    pub fn set_color_scheme(&self, scheme: ColorScheme) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_set_color_scheme(self.ptr(), scheme.raw()) };
    }

    /// Apply a config to this surface alone.
    pub fn update_config(&self, config: &Config) {
        // SAFETY: a live surface on its thread; config is live and libghostty clones it.
        unsafe { sys::ghostty_surface_update_config(self.ptr(), config.as_ptr()) };
    }

    /// Send a key event; `true` when libghostty used it (encoded it or ran a binding).
    #[must_use]
    pub fn send_key(&self, event: &KeyEvent) -> bool {
        let text = event.text.as_deref().and_then(|text| CString::new(text).ok());
        let raw = sys::ghostty_input_key_s {
            action: event.action.raw(),
            mods: event.mods.bits(),
            consumed_mods: event.consumed_mods.bits(),
            keycode: event.keycode,
            text: text.as_ref().map_or(std::ptr::null(), |text| text.as_ptr()),
            unshifted_codepoint: event.unshifted_codepoint,
            composing: event.composing,
        };
        // SAFETY: a live surface on its thread; the text outlives the call.
        unsafe { sys::ghostty_surface_key(self.ptr(), raw) }
    }

    /// Send text as a paste (bracketed when the program asked for it).
    pub fn text(&self, text: &str) {
        // SAFETY: a live surface on its thread; text is valid for text.len() bytes.
        unsafe { sys::ghostty_surface_text(self.ptr(), text.as_ptr().cast(), text.len()) };
    }

    /// Show an input method's composition at the cursor; `None` clears it.
    pub fn preedit(&self, text: Option<&str>) {
        let text = text.unwrap_or_default();
        // SAFETY: a live surface on its thread; text is valid for text.len() bytes (a byte length, not characters).
        unsafe { sys::ghostty_surface_preedit(self.ptr(), text.as_ptr().cast(), text.len()) };
    }

    /// `true` when libghostty used the click (a mouse-reporting program, a selection, a link).
    #[must_use]
    pub fn mouse_button(&self, state: MouseState, button: MouseButton, mods: Mods) -> bool {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_mouse_button(self.ptr(), state.raw(), button.raw(), mods.bits()) }
    }

    /// The pointer position in logical units from the top left; `(-1, -1)` says it left the surface.
    pub fn mouse_pos(&self, x: f64, y: f64, mods: Mods) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_mouse_pos(self.ptr(), x, y, mods.bits()) };
    }

    /// Scroll by `dx`/`dy`: positive is right/up; notches, or pixels with `precision`.
    pub fn mouse_scroll(&self, dx: f64, dy: f64, mods: ScrollMods) {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_mouse_scroll(self.ptr(), dx, dy, mods.raw()) };
    }

    #[must_use]
    pub fn ime_point(&self) -> ImePoint {
        let (mut x, mut y, mut width, mut height) = (0.0, 0.0, 0.0, 0.0);
        // SAFETY: a live surface on its thread; the four outputs are valid f64s.
        unsafe { sys::ghostty_surface_ime_point(self.ptr(), &raw mut x, &raw mut y, &raw mut width, &raw mut height) };
        ImePoint { x, y, width, height }
    }

    /// Run a libghostty binding action (`copy_to_clipboard`, `increase_font_size:1`); `true` when it ran.
    #[must_use]
    pub fn binding_action(&self, action: &str) -> bool {
        // SAFETY: a live surface on its thread; action is valid for action.len() bytes.
        unsafe { sys::ghostty_surface_binding_action(self.ptr(), action.as_ptr().cast(), action.len()) }
    }

    #[must_use]
    pub fn has_selection(&self) -> bool {
        // SAFETY: a live surface on its thread.
        unsafe { sys::ghostty_surface_has_selection(self.ptr()) }
    }

    #[must_use]
    pub fn size(&self) -> SurfaceSize {
        // SAFETY: a live surface on its thread.
        let size = unsafe { sys::ghostty_surface_size(self.ptr()) };
        SurfaceSize {
            columns: size.columns,
            rows: size.rows,
            width_px: size.width_px,
            height_px: size.height_px,
            cell_width_px: size.cell_width_px,
            cell_height_px: size.cell_height_px,
        }
    }

    /// Complete a read or confirmation with `contents`; `confirmed` says the user approved it. A request that belongs
    /// to another surface is refused and handed back.
    ///
    /// # Errors
    ///
    /// The request, unconsumed, when it is not this surface's.
    pub fn complete_clipboard_request(
        &self,
        pending: PendingRequest,
        contents: &[ClipboardContent],
        available: &[&str],
        confirmed: bool,
    ) -> Result<(), PendingRequest> {
        if pending.surface != self.key {
            return Err(pending);
        }
        let mimes: Vec<CString> = contents
            .iter()
            .filter_map(|content| CString::new(content.mime.as_str()).ok())
            .collect();
        let items: Vec<sys::ghostty_clipboard_content_s> = contents
            .iter()
            .zip(&mimes)
            .map(|(content, mime)| sys::ghostty_clipboard_content_s {
                mime: mime.as_ptr(),
                data: content.data.as_ptr().cast(),
                len: content.data.len(),
            })
            .collect();
        let available: Vec<CString> = available.iter().filter_map(|mime| CString::new(*mime).ok()).collect();
        let available_ptrs: Vec<*const c_char> = available.iter().map(|mime| mime.as_ptr()).collect();
        let completion = sys::ghostty_clipboard_complete_s {
            contents: items.as_ptr(),
            contents_len: items.len(),
            available: available_ptrs.as_ptr(),
            available_len: available_ptrs.len(),
            confirmed,
            remember: false,
        };
        // SAFETY: a live surface on its thread; pending.state is the open request libghostty gave this surface and
        // is consumed here; every array and string outlives the call, which copies what it keeps.
        unsafe {
            sys::ghostty_surface_complete_clipboard_request(self.ptr(), &raw const completion, pending.state.as_ptr());
        }
        Ok(())
    }

    /// Deny a request: libghostty answers the program with the protocol's refusal and closes the request.
    ///
    /// # Errors
    ///
    /// The request, unconsumed, when it is not this surface's.
    pub fn deny_clipboard_request(&self, pending: PendingRequest) -> Result<(), PendingRequest> {
        if pending.surface != self.key {
            return Err(pending);
        }
        // SAFETY: a live surface on its thread; pending.state is its open request, consumed here.
        unsafe { sys::ghostty_surface_deny_clipboard_request(self.ptr(), pending.state.as_ptr()) };
        Ok(())
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        // SAFETY: raw is live, owned by self alone and freed once, here.
        unsafe { sys::ghostty_surface_free(self.raw.as_ptr()) };
    }
}
