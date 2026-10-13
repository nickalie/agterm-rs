//! One terminal: a libghostty surface rendered in a `GtkGLArea`, with its keyboard, input method, pointer, scroll and
//! focus wiring. Behavior follows upstream's `GhosttySurfaceView*.swift` and `libghostty.md`; the `GtkGLArea` hosting
//! is the Linux glue (the patched OpenGL platform draws on this thread into the area's current context).
//!
//! Lifecycle: the libghostty surface is created on the first `resize` (upstream creates only at a nonzero size) and
//! freed exactly once, by [`TerminalSurface::teardown`], which unrealizing the area calls with the context current.
//! The embedding API cannot move a renderer to a new GL context, so a live surface's area must stay realized:
//! containers keep it mounted and hide it, never re-parent it.
//!
//! Ownership: the area's `destroy` handler holds the one long-lived strong reference to the surface's state, and the
//! state holds the area only weakly, so a container dropping the area disposes it, `destroy` releases the state and
//! the key leaves the registry. A [`TerminalSurface`] handle holds the area strongly while it lives, which keeps a
//! surface that is not in a container yet alive.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use agterm_ghostty::input::{KeyAction, KeyEvent, Mods, MouseState, ScrollMods};
use agterm_ghostty::{ClipboardContent, MouseShape, PendingRequest, Surface, SurfaceKey, SurfaceOptions};
use gtk::prelude::*;
use gtk::{gdk, glib};

use crate::ghostty_app;
use crate::input::{self, PressedKeys};

/// What the terminal runs. `None` fields take the config's values.
#[derive(Debug, Clone, Default)]
pub struct SurfaceSpec {
    pub working_directory: Option<PathBuf>,
    /// Runs through `/bin/sh -c` instead of the configured command.
    pub command: Option<String>,
    pub env: Vec<(String, String)>,
    pub wait_after_command: bool,
}

/// Where the input method is inside a key event (ghostty's GTK runtime tracks the same: input methods differ in the
/// order they emit commit and preedit-end).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum KeyPhase {
    #[default]
    Outside,
    Composing,
    NotComposing,
}

#[derive(Debug, Default)]
struct KeyState {
    phase: KeyPhase,
    composing: bool,
    /// Text the input method committed during the current key event, sent with the key.
    committed: String,
    pressed: PressedKeys,
}

#[derive(Debug, Default)]
struct PointerState {
    shape: Option<MouseShape>,
    hidden: bool,
}

type ExitHandler = Box<dyn FnOnce()>;
type TitleHandler = Box<dyn Fn(&str)>;

struct Inner {
    key: SurfaceKey,
    /// Weak: the area owns this state through its `destroy` handler, so a strong reference here would be a cycle.
    area: glib::WeakRef<gtk::GLArea>,
    im: gtk::IMMulticontext,
    spec: SurfaceSpec,
    ghostty: RefCell<Option<Surface>>,
    torn_down: Cell<bool>,
    keys: RefCell<KeyState>,
    pointer: RefCell<PointerState>,
    pwd: RefCell<Option<String>>,
    on_title: RefCell<Option<TitleHandler>>,
    on_exit: RefCell<Option<ExitHandler>>,
    exit_handled: Cell<bool>,
    /// The first frame was drawn (logged once).
    drew: Cell<bool>,
}

thread_local! {
    static SURFACES: RefCell<HashMap<SurfaceKey, Weak<Inner>>> = RefCell::new(HashMap::new());
    static NEXT_KEY: Cell<usize> = const { Cell::new(1) };
}

/// The live surface libghostty knows by `key`, if it still exists and is not torn down.
pub fn lookup(key: SurfaceKey) -> Option<TerminalSurface> {
    let weak = SURFACES.with(|surfaces| surfaces.borrow().get(&key).cloned())?;
    TerminalSurface::upgrade(&weak).filter(|surface| !surface.inner.torn_down.get())
}

/// Tear down every live surface, ending their processes: the quit path, which leaves the widgets to the process exit.
pub fn teardown_all() {
    let live: Vec<Weak<Inner>> = SURFACES.with(|surfaces| surfaces.borrow().values().cloned().collect());
    for surface in live.iter().filter_map(TerminalSurface::upgrade) {
        surface.teardown();
    }
}

/// How many surfaces the registry holds (each one's area is alive).
#[cfg(test)]
fn registered() -> usize {
    SURFACES.with(|surfaces| surfaces.borrow().len())
}

fn next_key() -> SurfaceKey {
    NEXT_KEY.with(|next| {
        let value = next.get();
        next.set(value.saturating_add(1));
        SurfaceKey::new(NonZeroUsize::new(value).unwrap_or(NonZeroUsize::MAX))
    })
}

/// A terminal widget. Cheap to clone; the area owns the surface, and its `destroy` releases it (module docs).
#[derive(Clone)]
pub struct TerminalSurface {
    inner: Rc<Inner>,
    area: gtk::GLArea,
}

impl TerminalSurface {
    #[must_use]
    pub fn new(spec: SurfaceSpec) -> Self {
        let area = gtk::GLArea::builder()
            .allowed_apis(gdk::GLAPI::GL)
            .has_depth_buffer(false)
            .has_stencil_buffer(false)
            .focusable(true)
            .focus_on_click(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        let inner = Rc::new(Inner {
            key: next_key(),
            area: area.downgrade(),
            im: gtk::IMMulticontext::new(),
            spec,
            ghostty: RefCell::new(None),
            torn_down: Cell::new(false),
            keys: RefCell::new(KeyState::default()),
            pointer: RefCell::new(PointerState::default()),
            pwd: RefCell::new(None),
            on_title: RefCell::new(None),
            on_exit: RefCell::new(None),
            exit_handled: Cell::new(false),
            drew: Cell::new(false),
        });
        SURFACES.with(|surfaces| surfaces.borrow_mut().insert(inner.key, Rc::downgrade(&inner)));
        let surface = Self { inner, area };
        surface.connect_area();
        surface.connect_keyboard();
        surface.connect_pointer();
        surface.connect_focus();
        // the widget owns the state from here: the handler's reference goes when dispose drops the area's handlers
        let owner = Rc::clone(&surface.inner);
        surface.area.connect_destroy(move |area| {
            let surface = Self {
                inner: Rc::clone(&owner),
                area: area.clone(),
            };
            surface.teardown();
            SURFACES.with(|surfaces| surfaces.borrow_mut().remove(&surface.inner.key));
        });
        surface
    }

    /// The widget to place in a container.
    #[must_use]
    pub fn widget(&self) -> &gtk::GLArea {
        &self.area
    }

    /// Called with each title a program sets (or the user's static one).
    pub fn connect_title(&self, handler: impl Fn(&str) + 'static) {
        *self.inner.on_title.borrow_mut() = Some(Box::new(handler));
    }

    /// Called once when the terminal's process ends.
    pub fn connect_exit(&self, handler: impl FnOnce() + 'static) {
        *self.inner.on_exit.borrow_mut() = Some(Box::new(handler));
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.inner)
    }

    /// A handle again, while both the state and its area are alive.
    fn upgrade(weak: &Weak<Inner>) -> Option<Self> {
        let inner = weak.upgrade()?;
        let area = inner.area.upgrade()?;
        Some(Self { inner, area })
    }

    /// Run `f` on the libghostty surface, if it exists. The borrow is shared, so callbacks libghostty makes during
    /// `f` may use the surface too.
    fn with_surface<R>(&self, f: impl FnOnce(&Surface) -> R) -> Option<R> {
        self.inner.ghostty.borrow().as_ref().map(f)
    }

    fn connect_area(&self) {
        let area = &self.area;
        let weak = self.weak();
        area.connect_realize(move |area| {
            let Some(surface) = Self::upgrade(&weak) else { return };
            area.make_current();
            if let Some(err) = area.error() {
                tracing::error!(%err, "the terminal's GtkGLArea has no OpenGL context");
            }
            surface.inner.im.set_client_widget(Some(area));
        });
        let weak = self.weak();
        area.connect_unrealize(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.teardown();
            }
        });
        let weak = self.weak();
        area.connect_resize(move |_, width, height| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.resized(width, height);
            }
        });
        let weak = self.weak();
        area.connect_render(move |_, _| {
            if let Some(surface) = Self::upgrade(&weak)
                && surface.with_surface(Surface::draw).is_some()
                && !surface.inner.drew.replace(true)
            {
                tracing::debug!(key = surface.inner.key.get(), "terminal drew its first frame");
            }
            glib::Propagation::Stop
        });
        let weak = self.weak();
        area.connect_map(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.with_surface(|s| s.set_occlusion(true));
            }
        });
        let weak = self.weak();
        area.connect_unmap(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.with_surface(|s| s.set_occlusion(false));
            }
        });
    }

    /// The area's drawable changed size (or scale): create the surface the first time, else push the new size.
    fn resized(&self, width: i32, height: i32) {
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return;
        };
        if width == 0 || height == 0 {
            return;
        }
        let scale = f64::from(self.area.scale_factor());
        if self.inner.ghostty.borrow().is_none() {
            self.create(scale);
        }
        self.with_surface(|surface| {
            surface.set_content_scale(scale, scale);
            surface.set_size(width, height);
            // a size push that keeps the grid draws nothing new (libghostty.md)
            surface.refresh();
        });
    }

    fn create(&self, scale: f64) {
        if self.inner.torn_down.get() {
            return;
        }
        let area = &self.area;
        area.make_current();
        if area.error().is_some() {
            return;
        }
        let spec = &self.inner.spec;
        let options = SurfaceOptions {
            scale_factor: scale,
            working_directory: spec.working_directory.as_deref(),
            command: spec.command.as_deref(),
            env: &spec.env,
            wait_after_command: spec.wait_after_command,
            ..SurfaceOptions::default()
        };
        match ghostty_app::create_surface(self.inner.key, &options) {
            Ok(surface) => {
                tracing::debug!(key = self.inner.key.get(), scale, "terminal surface created");
                surface.set_focus(area.has_focus());
                *self.inner.ghostty.borrow_mut() = Some(surface);
                if let Some(title) = ghostty_app::static_title() {
                    self.apply_title(&title);
                }
            }
            Err(err) => tracing::error!(%err, "could not create the terminal surface"),
        }
    }

    /// Free the libghostty surface and end its process, once; later calls do nothing.
    pub fn teardown(&self) {
        if self.inner.torn_down.replace(true) {
            return;
        }
        // libghostty frees its GL objects in the area's context; an area never realized has no context and no surface
        if self.area.is_realized() {
            self.area.make_current();
        }
        let Ok(mut slot) = self.inner.ghostty.try_borrow_mut() else {
            tracing::error!("terminal surface torn down while in use; it leaks");
            return;
        };
        drop(slot.take());
        self.inner.im.set_client_widget(None::<&gtk::Widget>);
    }

    /// Whether the libghostty surface exists: created at the area's first nonzero size and not torn down since.
    pub fn is_realized(&self) -> bool {
        !self.inner.torn_down.get() && self.inner.ghostty.try_borrow().is_ok_and(|surface| surface.is_some())
    }

    pub fn grab_focus(&self) {
        self.area.grab_focus();
    }

    pub fn queue_render(&self) {
        self.area.queue_render();
    }

    pub fn apply_title(&self, title: &str) {
        if title.is_empty() {
            return;
        }
        if let Some(handler) = self.inner.on_title.borrow().as_ref() {
            handler(title);
        }
    }

    pub fn apply_pwd(&self, pwd: String) {
        *self.inner.pwd.borrow_mut() = Some(pwd);
    }

    pub fn set_mouse_shape(&self, shape: MouseShape) {
        self.inner.pointer.borrow_mut().shape = Some(shape);
        self.apply_cursor();
    }

    pub fn set_mouse_hidden(&self, hidden: bool) {
        self.inner.pointer.borrow_mut().hidden = hidden;
        self.apply_cursor();
    }

    fn apply_cursor(&self) {
        let pointer = self.inner.pointer.borrow();
        let name = if pointer.hidden {
            "none"
        } else {
            pointer.shape.unwrap_or(MouseShape::Text).css_name()
        };
        self.area.set_cursor_from_name(Some(name));
    }

    /// The child exited: a command surface that should vanish closes now and suppresses libghostty's prompt (`true`);
    /// any other keeps the prompt, and its close arrives through the close callback.
    pub fn child_exited(&self) -> bool {
        let spec = &self.inner.spec;
        if spec.command.is_none() || spec.wait_after_command {
            return false;
        }
        let weak = self.weak();
        glib::idle_add_local_once(move || {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.handle_process_exit();
            }
        });
        true
    }

    /// The process ended: run the exit handler once.
    pub fn handle_process_exit(&self) {
        if self.inner.exit_handled.replace(true) {
            return;
        }
        let handler = self.inner.on_exit.borrow_mut().take();
        if let Some(handler) = handler {
            handler();
        }
    }

    /// Complete a clipboard request this surface owns.
    pub fn complete_clipboard_request(
        &self,
        pending: PendingRequest,
        contents: &[ClipboardContent],
        available: &[&str],
        confirmed: bool,
    ) {
        let refused = self.with_surface(|s| s.complete_clipboard_request(pending, contents, available, confirmed));
        if matches!(refused, Some(Err(_))) {
            tracing::error!("clipboard completion for another surface dropped");
        }
    }

    /// Deny a clipboard request this surface owns.
    pub fn deny_clipboard_request(&self, pending: PendingRequest) {
        let refused = self.with_surface(|s| s.deny_clipboard_request(pending));
        if matches!(refused, Some(Err(_))) {
            tracing::error!("clipboard denial for another surface dropped");
        }
    }

    fn connect_focus(&self) {
        let focus = gtk::EventControllerFocus::new();
        let weak = self.weak();
        focus.connect_enter(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.focus_changed(true);
            }
        });
        let weak = self.weak();
        focus.connect_leave(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.focus_changed(false);
            }
        });
        self.area.add_controller(focus);
    }

    fn focus_changed(&self, focused: bool) {
        self.with_surface(|s| s.set_focus(focused));
        if focused {
            self.inner.im.focus_in();
        } else {
            self.inner.im.focus_out();
            // the releases of keys held now go to whatever takes focus
            self.inner.keys.borrow_mut().pressed.clear();
        }
    }

    fn connect_keyboard(&self) {
        let keys = gtk::EventControllerKey::new();
        let weak = self.weak();
        keys.connect_key_pressed(move |controller, keyval, keycode, state| {
            let handled = Self::upgrade(&weak)
                .is_some_and(|surface| surface.key_event(controller, KeyAction::Press, keyval, keycode, state));
            glib::Propagation::from(handled)
        });
        let weak = self.weak();
        keys.connect_key_released(move |controller, keyval, keycode, state| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.key_event(controller, KeyAction::Release, keyval, keycode, state);
            }
        });
        self.area.add_controller(keys);
        self.connect_input_method();
    }

    fn connect_input_method(&self) {
        let im = &self.inner.im;
        let weak = self.weak();
        im.connect_preedit_start(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                let mut keys = surface.inner.keys.borrow_mut();
                keys.composing = true;
                keys.committed.clear();
            }
        });
        let weak = self.weak();
        im.connect_preedit_changed(move |im| {
            if let Some(surface) = Self::upgrade(&weak) {
                // fcitx5-hangul can commit and keep composing without a new preedit-start
                surface.inner.keys.borrow_mut().composing = true;
                let (text, _, _) = im.preedit_string();
                surface.with_surface(|s| s.preedit(Some(text.as_str())));
            }
        });
        let weak = self.weak();
        im.connect_preedit_end(move |_| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.inner.keys.borrow_mut().composing = false;
                surface.with_surface(|s| s.preedit(None));
            }
        });
        let weak = self.weak();
        im.connect_commit(move |_, text| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.im_commit(text);
            }
        });
    }

    /// Text the input method committed: inside a plain key event it rides with the key (so libghostty knows which key
    /// made it); a composition's result or text from elsewhere (an on-screen keyboard) goes in on its own.
    fn im_commit(&self, text: &str) {
        {
            let mut keys = self.inner.keys.borrow_mut();
            if keys.phase == KeyPhase::NotComposing {
                keys.committed.push_str(text);
                return;
            }
            // a commit ends the composition
            keys.composing = false;
        }
        // well-behaved input methods end the preedit themselves, some (ibus 1.5.29) do not
        self.with_surface(|s| s.preedit(None));
        // keycode 0 is no key: the text goes in without a key encoding
        let event = KeyEvent {
            action: KeyAction::Press,
            mods: Mods::empty(),
            consumed_mods: Mods::empty(),
            keycode: 0,
            text: Some(text.to_owned()),
            unshifted_codepoint: 0,
            composing: false,
        };
        self.with_surface(|s| s.send_key(&event));
    }

    /// Pass the event through the input method; `true` when it consumed the key (ghostty's GTK runtime's rules).
    fn filtered_by_input_method(&self, event: &gdk::Event) -> bool {
        {
            let mut keys = self.inner.keys.borrow_mut();
            keys.phase = if keys.composing {
                KeyPhase::Composing
            } else {
                KeyPhase::NotComposing
            };
        }
        if let Some(point) = self.with_surface(Surface::ime_point) {
            #[expect(clippy::cast_possible_truncation, reason = "widget coordinates fit an i32 by far")]
            let rect = gdk::Rectangle::new(point.x as i32, point.y as i32, 1, 1);
            self.inner.im.set_cursor_location(&rect);
        }
        let handled = self.inner.im.filter_keypress(event);
        let mut keys = self.inner.keys.borrow_mut();
        let was_composing = keys.phase == KeyPhase::Composing;
        keys.phase = KeyPhase::Outside;
        // composing, or finishing a composition (its commit already went in), or a key the method swallowed whole
        handled && (keys.composing || was_composing || keys.committed.is_empty())
    }

    fn key_event(
        &self,
        controller: &gtk::EventControllerKey,
        action: KeyAction,
        keyval: gdk::Key,
        keycode: u32,
        state: gdk::ModifierType,
    ) -> bool {
        let Some(event) = controller.current_event() else {
            return false;
        };
        // before the input method: a release it swallows must still lift the key
        let action = self.inner.keys.borrow_mut().pressed.track(action, keycode);
        if self.filtered_by_input_method(&event) {
            self.inner.keys.borrow_mut().committed.clear();
            return true;
        }
        let (text, composing) = {
            let mut keys = self.inner.keys.borrow_mut();
            let committed = std::mem::take(&mut keys.committed);
            let text = if committed.is_empty() {
                input::keyval_text(keyval)
            } else {
                Some(committed)
            };
            (text, keys.composing)
        };
        let key_event = event.downcast_ref::<gdk::KeyEvent>();
        let num_lock = event.device().is_some_and(|device| device.is_num_locked());
        let consumed = key_event.map_or(gdk::ModifierType::empty(), |e| {
            e.consumed_modifiers() & gdk::MODIFIER_MASK
        });
        let key = KeyEvent {
            action,
            mods: input::event_mods(state, keyval, action, num_lock),
            consumed_mods: input::translate_mods(consumed),
            keycode: key_event.map_or(keycode, |e| input::remapped_keycode(keycode, keyval, e.is_modifier())),
            text,
            unshifted_codepoint: key_event.map_or(0, |e| unshifted_codepoint(&self.area, e, keycode)),
            composing,
        };
        let used = self.with_surface(|s| s.send_key(&key)).unwrap_or(false);
        if used && action != KeyAction::Release && composing {
            self.inner.im.reset();
            self.with_surface(|s| s.preedit(None));
        }
        used
    }

    fn connect_pointer(&self) {
        let click = gtk::GestureClick::builder().button(0).build();
        let weak = self.weak();
        click.connect_pressed(move |gesture, _, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.click(gesture, MouseState::Press, x, y);
            }
        });
        let weak = self.weak();
        click.connect_released(move |gesture, _, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                surface.click(gesture, MouseState::Release, x, y);
            }
        });
        self.area.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let weak = self.weak();
        motion.connect_motion(move |controller, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                let mods = input::translate_mods(controller.current_event_state());
                surface.with_surface(|s| s.mouse_pos(x, y, mods));
            }
        });
        let weak = self.weak();
        motion.connect_enter(move |controller, x, y| {
            if let Some(surface) = Self::upgrade(&weak) {
                let mods = input::translate_mods(controller.current_event_state());
                surface.with_surface(|s| s.mouse_pos(x, y, mods));
            }
        });
        let weak = self.weak();
        motion.connect_leave(move |controller| {
            let state = controller.current_event_state();
            // mid-drag the position stays where the selection is going
            if input::any_button_held(state) {
                return;
            }
            if let Some(surface) = Self::upgrade(&weak) {
                surface.with_surface(|s| s.mouse_pos(-1.0, -1.0, input::translate_mods(state)));
            }
        });
        self.area.add_controller(motion);
        self.connect_scroll();
    }

    fn click(&self, gesture: &gtk::GestureClick, state: MouseState, x: f64, y: f64) {
        let Some(button) = input::mouse_button(gesture.current_button()) else {
            return;
        };
        // only the left button moves focus; right and middle reach libghostty's bindings without it (upstream)
        if state == MouseState::Press && button == agterm_ghostty::input::MouseButton::Left {
            self.grab_focus();
        }
        let mods = input::translate_mods(gesture.current_event_state());
        self.with_surface(|s| {
            s.mouse_pos(x, y, mods);
            // the terminal has no context menu, so whether libghostty used the click does not matter
            let _ = s.mouse_button(state, button, mods);
        });
    }

    fn connect_scroll(&self) {
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = self.weak();
        scroll.connect_scroll(move |controller, dx, dy| {
            let Some(surface) = Self::upgrade(&weak) else {
                return glib::Propagation::Proceed;
            };
            let precision = controller.unit() == gdk::ScrollUnit::Surface;
            let scale = f64::from(surface.area.scale_factor());
            let (x, y) = input::scroll_delta(dx, dy, scale, precision);
            surface.with_surface(|s| s.mouse_scroll(x, y, ScrollMods { precision }));
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);
    }
}

/// The key's character with no modifier in the active layout (level 0 of the event's group), for the Kitty protocol
/// and unicode bindings; 0 when it has none.
fn unshifted_codepoint(widget: &impl IsA<gtk::Widget>, event: &gdk::KeyEvent, keycode: u32) -> u32 {
    let layout = event.layout();
    widget
        .display()
        .map_keycode(keycode)
        .and_then(|entries| {
            entries
                .into_iter()
                .find(|(key, _)| key.group() == i32::try_from(layout).unwrap_or(-1) && key.level() == 0)
        })
        .and_then(|(_, keyval)| keyval.to_unicode())
        .map_or(0, u32::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test owns GTK: it initialises on the calling thread, and a second thread may not.
    #[test]
    fn the_area_owns_the_surface_without_a_cycle() {
        if let Err(err) = gtk::init() {
            eprintln!("skipped: GTK has no display ({err})");
            return;
        }

        // a handle keeps a surface that is in no container yet
        let unplaced = TerminalSurface::new(SurfaceSpec::default());
        let key = unplaced.inner.key;
        let state = unplaced.weak();
        assert!(lookup(key).is_some());
        drop(unplaced);
        assert!(lookup(key).is_none(), "the last handle freed the unplaced surface");
        assert!(state.upgrade().is_none());
        assert_eq!(registered(), 0);

        // once placed, the container owns it, and dropping the area frees everything
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let terminal = TerminalSurface::new(SurfaceSpec::default());
        let key = terminal.inner.key;
        let state = terminal.weak();
        container.append(terminal.widget());
        drop(terminal);
        let area = lookup(key).map(|surface| surface.widget().clone());
        assert!(area.is_some(), "the container keeps the surface");
        assert_eq!(registered(), 1);
        if let Some(area) = area {
            container.remove(&area);
        }
        assert!(lookup(key).is_none());
        assert_eq!(registered(), 0, "destroy took the key out of the registry");
        assert!(state.upgrade().is_none(), "nothing holds the surface's state");

        let placed = TerminalSurface::new(SurfaceSpec::default());
        let key = placed.inner.key;
        container.append(placed.widget());
        teardown_all();
        assert!(placed.inner.torn_down.get());
        assert!(lookup(key).is_none(), "a torn-down surface is not live");
    }
}
