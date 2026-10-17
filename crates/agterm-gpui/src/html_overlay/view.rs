//! The page's element (the GPUI side of a page, modeled on gpui-kit's `crates/webview`, Apache-2.0: an entity holding
//! the page and its focus handle, laid out at its parent's size, its bounds and visibility synced to the engine in
//! prepaint, a mouse-blocking hitbox over what it covers): the panel (`super::panel`), and over the content area the
//! engine's frames drawn through [`crate::external_texture`], with the page's own popups above them.
//!
//! - Frames: a dmabuf frame is imported once per buffer (its planes duplicated once, the import dropped when the buffer
//!   leaves the page's set) and shown once its implicit fence signals (WPE hands no rendering fence); an SHM frame
//!   updates its damage. Every outcome (presented, discarded, failed) tells the engine the buffer may be reused; a
//!   buffer that does not import switches the page to SHM frames ([`agterm_wpe::Host::fall_back_to_shm`]), logged with
//!   the note that video is not supported there. SHM is never chosen while dmabufs import.
//! - Geometry and scale: the content's bounds in logical pixels and the window's scale factor, sent when they change;
//!   the engine renders at their product.
//! - Input: presses, releases, motion (with enter and leave), the wheel and the touchpad go to the engine in the
//!   content's coordinates; keys come from the app's key interceptor while the page holds the keyboard
//!   (`super::key_down`), after the window's own keys and the keymap, and their releases from the element. A press
//!   focuses the page and counts as user activity; GPUI's own handlers still see it, so a GPUI popover open elsewhere
//!   closes on a click into the page. A paste key hands the desktop's clipboard text to the engine first (text only:
//!   files never reach a page); a middle click pastes the primary selection the same way.
//! - The input method: the element is GPUI's input handler while focused; a composition goes to the engine as a
//!   preedit, a commit as text, and the candidate window sits at the caret area the engine reports.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use agterm_wpe::{
    Area, BufferId, ContextMenu, EditingCommand, Event, Frame, FrameContent, Geometry, Input, Key, Modifiers,
    OptionMenu, Pointer, PointerKind, Scroll, ViewId,
};
use gpui_kit::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, DmaBufPlane, DmaBuffer, ElementInputHandler,
    EntityInputHandler, FocusHandle, Focusable, HitboxBehavior, InteractiveElement as _, IntoElement, KeyUpEvent,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, PixelRect,
    Pixels, Render, ScrollWheelEvent, SharedString, Styled as _, Subscription, UTF16Selection, Window, canvas, div,
    point, px, size,
};

use super::input;
use super::menus::{self, Menu};
use super::page::Page;
use super::panel;
use crate::external_texture::{Acquire, ExternalTexture, ShmRegion};

/// What a page's element was handed so far (diagnostics, and the headless page tests).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Dmabuf frames queued for the renderer.
    pub dmabuf: u64,
    /// SHM frames.
    pub shm: u64,
    /// The page fell back to SHM frames: a dmabuf did not import.
    pub fell_back: bool,
    /// Frames the renderer is done with (presented, discarded or failed), each answered to the engine.
    pub answered: u64,
}

/// The page's element (module docs).
pub struct PageView {
    stats: FrameStats,
    /// Frames answered, counted from the renderer's threads.
    answered: Arc<std::sync::atomic::AtomicU64>,
    page: Rc<Page>,
    focus: FocusHandle,
    texture: ExternalTexture,
    /// The imported buffers of the page's current set.
    buffers: HashMap<BufferId, Arc<DmaBuffer>>,
    cursor: CursorStyle,
    /// The content's bounds as last laid out.
    bounds: Option<Bounds<Pixels>>,
    /// The pointer is over the content.
    hovered: bool,
    /// The buttons held, as the engine's pressed-button bits.
    buttons: Modifiers,
    /// The keyboard modifiers last seen.
    modifiers: gpui_kit::Modifiers,
    /// The page's popup: a `<select>`'s rows or the context menu.
    menu: Option<Menu>,
    /// The input method's composition.
    preedit: Option<String>,
    /// Where the engine says the caret is, for the candidate window.
    caret: Option<Area>,
    released: bool,
    _subscriptions: Vec<Subscription>,
}

impl PageView {
    /// The element of `page` in `window`.
    pub(super) fn new(page: Rc<Page>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let focused = cx.on_focus(&focus, window, |view, _, _| view.focus_changed(true));
        let blurred = cx.on_blur(&focus, window, |view, _, _| view.focus_changed(false));
        page.set_element(cx.entity().downgrade(), window.window_handle());
        Self {
            stats: FrameStats::default(),
            answered: Arc::default(),
            page,
            focus,
            texture: ExternalTexture::new(),
            buffers: HashMap::new(),
            cursor: CursorStyle::Arrow,
            bounds: None,
            hovered: false,
            buttons: Modifiers::empty(),
            modifiers: gpui_kit::Modifiers::default(),
            menu: None,
            preedit: None,
            caret: None,
            released: false,
            _subscriptions: vec![focused, blurred],
        }
    }

    /// The page shown.
    pub(super) fn page(&self) -> &Rc<Page> {
        &self.page
    }

    /// The handle that takes the keyboard for the page.
    pub(super) fn focus(&self) -> &FocusHandle {
        &self.focus
    }

    /// The frames handed to the element so far.
    #[must_use]
    pub fn frame_stats(&self) -> FrameStats {
        FrameStats {
            answered: self.answered.load(std::sync::atomic::Ordering::Relaxed),
            ..self.stats
        }
    }

    /// The engine closed popup `token` itself.
    pub(super) fn menu_closed(&mut self, token: agterm_wpe::MenuToken, cx: &mut Context<Self>) {
        if matches!(&self.menu, Some(Menu::Options { token: shown, .. }) if *shown == token) {
            self.menu = None;
            cx.notify();
        }
    }

    fn view_id(&self) -> Option<ViewId> {
        self.page.view_id()
    }

    /// Runs `send` with the engine and the page's id, when both exist.
    fn send(&self, send: impl FnOnce(&dyn super::engine::Web, ViewId)) {
        if let (Some(web), Some(view)) = (self.page.web(), self.view_id()) {
            send(web.as_ref(), view);
        }
    }

    fn focus_changed(&mut self, focused: bool) {
        self.send(|web, view| web.set_focused(view, focused));
        if focused {
            self.page.focused();
        } else if self.preedit.take().is_some() {
            // a composition the keyboard left is cancelled, as GTK's input method contexts do on focus out
            self.send(|web, view| web.ime_preedit(view, String::new(), 0));
        }
    }

    /// The page is gone: nothing more is drawn or sent.
    pub(super) fn release(&mut self, cx: &mut Context<Self>) {
        self.released = true;
        self.texture.clear();
        self.buffers.clear();
        self.menu = None;
        cx.notify();
    }

    // MARK: - engine events

    /// One of the engine's events for this page's element.
    pub(super) fn event(&mut self, event: &Event, cx: &mut Context<Self>) {
        if self.released {
            return;
        }
        match event {
            Event::Frame(frame) => self.frame(frame),
            Event::BuffersRemoved { buffers, .. } => {
                for buffer in buffers {
                    self.buffers.remove(buffer);
                }
            }
            Event::Cursor { name, .. } => self.cursor = input::cursor(name),
            Event::OptionMenu(menu) => self.option_menu(menu),
            Event::ContextMenu(menu) => self.context_menu(menu),
            Event::ImeCursorArea { area, .. } => self.caret = Some(*area),
            _ => return,
        }
        cx.notify();
    }

    fn frame(&mut self, frame: &Frame) {
        let (Some(web), Some(view)) = (self.page.web(), self.view_id()) else {
            return;
        };
        let presenter = web.presenter();
        let buffer = frame.buffer;
        let answered = Arc::clone(&self.answered);
        let done = move || {
            answered.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            presenter(view, buffer);
        };
        match &frame.content {
            FrameContent::DmaBuf(dmabuf) => {
                let imported = match self.buffers.get(&buffer) {
                    Some(imported) => Arc::clone(imported),
                    None => match gpui_buffer(dmabuf) {
                        Ok(imported) => {
                            let imported = Arc::new(imported);
                            self.buffers.insert(buffer, Arc::clone(&imported));
                            imported
                        }
                        Err(err) => {
                            tracing::warn!(%err, "a page's dmabuf could not be duplicated");
                            done();
                            return;
                        }
                    },
                };
                self.texture.push_dmabuf(imported, Acquire::Implicit, move |_| done());
                self.stats.dmabuf += 1;
                if self.texture.take_fallback().is_some() {
                    self.stats.fell_back = true;
                    web.fall_back_to_shm(view);
                }
            }
            FrameContent::Shm(regions) => {
                let regions = regions.iter().map(|region| ShmRegion {
                    rect: PixelRect {
                        x: region.rect.x,
                        y: region.rect.y,
                        width: region.rect.width,
                        height: region.rect.height,
                    },
                    pixels: &region.pixels,
                });
                self.texture
                    .push_shm(frame.width, frame.height, regions, move |_| done());
                self.stats.shm += 1;
            }
        }
    }

    // MARK: - popups

    fn option_menu(&mut self, menu: &OptionMenu) {
        self.close_menu();
        self.menu = Some(Menu::options(menu));
    }

    fn context_menu(&mut self, menu: &ContextMenu) {
        self.close_menu();
        let can = self
            .page
            .overlay()
            .current
            .map(|page| (page.can_go_back, page.can_go_forward));
        self.menu = Menu::context(menu, can.unwrap_or_default());
    }

    /// Closes the popup unanswered.
    pub(super) fn close_menu(&mut self) {
        if let Some(Menu::Options { token, .. }) = self.menu.take()
            && let Some(web) = self.page.web()
        {
            web.choose_option(token, None);
        }
    }

    /// The user picked row `index` of the popup.
    pub(super) fn pick(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        match menu {
            Menu::Options { token, .. } => {
                if let (Some(web), Ok(index)) = (self.page.web(), u32::try_from(index)) {
                    web.choose_option(token, Some(index));
                }
            }
            Menu::Context { items, .. } => {
                if let Some(item) = items.get(index) {
                    self.run(item.action.clone(), cx);
                }
            }
        }
        cx.notify();
    }

    /// Runs a context menu's action.
    fn run(&self, action: menus::Action, cx: &mut Context<Self>) {
        match action {
            menus::Action::Editing(command) => {
                if matches!(command, EditingCommand::Paste | EditingCommand::PasteAsPlainText) {
                    self.hand_clipboard(cx.read_from_clipboard());
                }
                self.send(|web, view| web.editing_command(view, command));
            }
            menus::Action::Back => self.send(|web, view| web.go_back(view)),
            menus::Action::Forward => self.send(|web, view| web.go_forward(view)),
            menus::Action::Reload => self.send(|web, view| web.reload(view)),
            menus::Action::CopyText(text) => cx.write_to_clipboard(ClipboardItem::new_string(text)),
        }
    }

    /// Hands the desktop's clipboard text to the engine, for a paste: text only.
    fn hand_clipboard(&self, item: Option<ClipboardItem>) {
        let text = item.and_then(|item| item.text()).unwrap_or_default();
        if let Some(web) = self.page.web() {
            web.set_clipboard(text);
        }
    }

    // MARK: - keys

    /// A key the app's interceptor hands the focused page: the popup's navigation while one is up, else the engine's.
    /// Whether the page took it.
    pub(super) fn key_down(
        &mut self,
        keystroke: &gpui_kit::Keystroke,
        native: &crate::terminal::input::KeyStroke,
        source: &crate::terminal::input::Source,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.released {
            return false;
        }
        if self.menu.is_some() {
            return self.menu_key(keystroke, cx);
        }
        self.page.user_input(false);
        if matches!(source, crate::terminal::input::Source::Text) {
            let text = keystroke.key_char.clone().unwrap_or_default();
            self.send(|web, view| web.ime_commit(view, text));
            return true;
        }
        if native.composing {
            return false;
        }
        let Some(keyval) = input::keysym(keystroke) else {
            return true;
        };
        if input::pastes(keystroke) {
            self.hand_clipboard(cx.read_from_clipboard());
        }
        let key = Key {
            pressed: true,
            keyval,
            keycode: (native.keycode != 0).then_some(native.keycode),
            modifiers: input::modifiers(keystroke.modifiers) | self.buttons,
        };
        self.send(|web, view| web.input(view, Input::Key(key)));
        true
    }

    /// The popup's keys: the arrows move its highlight, Return picks, Escape closes; every key is the popup's.
    fn menu_key(&mut self, keystroke: &gpui_kit::Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(menu) = self.menu.as_mut() else {
            return false;
        };
        match keystroke.key.as_str() {
            "up" => menu.step(false),
            "down" => menu.step(true),
            "enter" | "space" => {
                if let Some(index) = menu.highlighted() {
                    self.pick(index, cx);
                    return true;
                }
            }
            "escape" => self.close_menu(),
            _ => {}
        }
        cx.notify();
        true
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(keyval) = input::keysym(&event.keystroke) else {
            return;
        };
        let key = Key {
            pressed: false,
            keyval,
            keycode: None,
            modifiers: input::modifiers(event.keystroke.modifiers) | self.buttons,
        };
        self.send(|web, view| web.input(view, Input::Key(key)));
        cx.stop_propagation();
    }

    /// A modifier went down or up: the engine gets its key, so a page's own Shift or Control handlers see it.
    fn modifiers_changed(&mut self, event: &ModifiersChangedEvent, _: &mut Window, _: &mut Context<Self>) {
        let (before, now) = (self.modifiers, event.modifiers);
        self.modifiers = now;
        let changes = [
            (before.shift, now.shift, 0xffe1),
            (before.control, now.control, 0xffe3),
            (before.alt, now.alt, 0xffe9),
            (before.platform, now.platform, 0xffeb),
        ];
        for (was, is, keyval) in changes {
            if was != is {
                let key = Key {
                    pressed: is,
                    keyval,
                    keycode: None,
                    modifiers: input::modifiers(now) | self.buttons,
                };
                self.send(|web, view| web.input(view, Input::Key(key)));
            }
        }
    }

    // MARK: - the pointer

    fn pointer(&self, kind: PointerKind, position: Point, modifiers: gpui_kit::Modifiers) {
        let Some(bounds) = self.bounds else { return };
        let (x, y) = input::local(position, bounds.origin);
        let pointer = Pointer {
            kind,
            x,
            y,
            modifiers: input::modifiers(modifiers) | self.buttons,
        };
        self.send(|web, view| web.input(view, Input::Pointer(pointer)));
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.released {
            return;
        }
        self.close_menu();
        window.focus(&self.focus, cx);
        self.page.user_input(true);
        let button = input::button(event.button);
        if event.button == MouseButton::Middle {
            self.hand_clipboard(cx.read_from_primary());
        }
        self.pointer(PointerKind::Down(button), event.position, event.modifiers);
        self.buttons |= input::button_bit(button);
        if event.button == MouseButton::Middle {
            self.send(|web, view| web.editing_command(view, EditingCommand::Paste));
        }
        cx.notify();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent) {
        let button = input::button(event.button);
        let bit = input::button_bit(button);
        if !self.buttons.contains(bit) {
            return;
        }
        self.buttons.remove(bit);
        self.pointer(PointerKind::Up(button), event.position, event.modifiers);
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool) {
        if hovered != self.hovered {
            self.hovered = hovered;
            let kind = if hovered {
                PointerKind::Enter
            } else {
                PointerKind::Leave
            };
            self.pointer(kind, event.position, event.modifiers);
        }
        if hovered || !self.buttons.is_empty() {
            self.pointer(PointerKind::Move, event.position, event.modifiers);
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent) {
        let Some(bounds) = self.bounds else { return };
        let (x, y) = input::local(event.position, bounds.origin);
        let (delta_x, delta_y, precise) = input::scroll(event.delta);
        let scroll = Scroll {
            x,
            y,
            delta_x,
            delta_y,
            precise,
            stop: event.touch_phase == gpui_kit::TouchPhase::Ended,
            modifiers: input::modifiers(event.modifiers) | self.buttons,
        };
        self.send(|web, view| web.input(view, Input::Scroll(scroll)));
    }

    // MARK: - layout and paint

    /// The content was laid out at `bounds` in a window of `scale`.
    fn laid_out(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        self.bounds = Some(bounds);
        if self.page.web().is_none() {
            // the window has drawn, so the engine may start now
            super::registry::realize_waiting();
        }
        let round = |value: Pixels| {
            let value = f32::from(value).round().max(1.0);
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a positive pixel count"
            )]
            let value = value as u32;
            value
        };
        self.page.set_geometry(Geometry {
            width: round(bounds.size.width),
            height: round(bounds.size.height),
            scale: f64::from(scale),
        });
    }

    /// The content: the frames, drawn under the page's hitbox, with the pointer's listeners.
    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let surface = self.texture.prepare(window);
        let view = cx.entity();
        let prepaint_view = view.downgrade();
        let paint_view = view.downgrade();
        let cursor = self.cursor;
        let focus = self.focus.clone();
        canvas(
            move |bounds, window, cx| {
                let scale = window.scale_factor();
                let _ = prepaint_view.update(cx, |view, _| view.laid_out(bounds, scale));
                window.insert_hitbox(bounds, HitboxBehavior::BlockMouse)
            },
            move |bounds, hitbox, window, cx| {
                if let Some(surface) = surface {
                    window.paint_external_surface(bounds, surface);
                }
                window.set_cursor_style(cursor, &hitbox);
                if let Some(view) = paint_view.upgrade() {
                    if focus.is_focused(window) {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, view.clone()), cx);
                    }
                    listen(&view, &hitbox, window);
                }
            },
        )
        .size_full()
        .into_any_element()
    }
}

/// A page's point, in window coordinates.
type Point = gpui_kit::Point<Pixels>;

/// The pointer's listeners of `view` over `hitbox`: presses and the wheel over it; motion and releases anywhere, so a
/// drag carries on past the edge. None stops the event: GPUI's own handlers (a popover's click outside) still see it.
fn listen(view: &gpui_kit::Entity<PageView>, hitbox: &gpui_kit::Hitbox, window: &mut Window) {
    let (target, over) = (view.downgrade(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble && over.is_hovered(window) {
            let _ = target.update(cx, |view, cx| view.mouse_down(event, window, cx));
        }
    });
    let target = view.downgrade();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase == DispatchPhase::Bubble {
            let _ = target.update(cx, |view, _| view.mouse_up(event));
        }
    });
    let (target, over) = (view.downgrade(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble {
            let hovered = over.is_hovered(window);
            let _ = target.update(cx, |view, _| view.mouse_move(event, hovered));
        }
    });
    let (target, over) = (view.downgrade(), hitbox.clone());
    window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble && over.should_handle_scroll(window) {
            let _ = target.update(cx, |view, _| view.scroll(event));
        }
    });
}

/// The engine's dmabuf as GPUI's, its planes duplicated (the engine's own copies go with its frame).
fn gpui_buffer(dmabuf: &agterm_wpe::DmaBuf) -> std::io::Result<DmaBuffer> {
    let planes = dmabuf
        .planes
        .iter()
        .map(|plane| {
            Ok(DmaBufPlane {
                fd: plane.fd.try_clone()?,
                offset: plane.offset,
                stride: plane.stride,
            })
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    Ok(DmaBuffer {
        width: dmabuf.width,
        height: dmabuf.height,
        fourcc: dmabuf.fourcc,
        modifier: dmabuf.modifier,
        planes,
    })
}

impl Focusable for PageView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for PageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.page.drawn_in(window.window_handle());
        let overlay = self.page.overlay();
        let colors = self.page.colors();
        let poster = self.page.poster().clone();
        let menu = self
            .menu
            .as_ref()
            .zip(self.bounds)
            .map(|(menu, bounds)| menus::element(menu, bounds, &colors, cx));
        let content = self.content(window, cx);
        div()
            .id(SharedString::from(format!("html-page-{}", overlay.id)))
            .debug_selector(|| "html-page".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(colors.background())
            .children(panel::strip(&overlay, &colors, &poster, cx))
            .child(
                div()
                    .id("html-content")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .track_focus(&self.focus)
                    .on_key_up(cx.listener(Self::key_up))
                    .on_modifiers_changed(cx.listener(Self::modifiers_changed))
                    .child(content)
                    .children(panel::failure(&overlay, &colors)),
            )
            .children(menu)
    }
}

impl EntityInputHandler for PageView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<UTF16Selection> {
        let end = self.preedit.as_deref().map_or(0, utf16_len);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.preedit.as_deref().map(|preedit| 0..utf16_len(preedit))
    }

    /// The input method ended its composition keeping its text: it goes in.
    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self.preedit.take() {
            self.send(|web, view| web.ime_commit(view, text));
        }
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.preedit = None;
        let text = text.to_owned();
        self.send(|web, view| web.ime_commit(view, text));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cursor = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        self.preedit = (!text.is_empty()).then(|| text.to_owned());
        let text = text.to_owned();
        self.send(|web, view| web.ime_preedit(view, text, cursor));
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        element: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let caret = self.caret?;
        #[expect(clippy::cast_precision_loss, reason = "page coordinates are far below 2^24")]
        let (x, y, width, height) = (caret.x as f32, caret.y as f32, caret.width as f32, caret.height as f32);
        Some(Bounds {
            origin: element.origin + point(px(x), px(y)),
            size: size(px(width.max(1.)), px(height.max(1.))),
        })
    }

    fn character_index_for_point(&mut self, _: Point, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

/// The length of `text` in UTF-16 units.
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests;
