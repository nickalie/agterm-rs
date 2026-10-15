//! A terminal's mouse, as Ghostty's surface handles it (`Surface.mouseButtonCallback`, `cursorPosCallback`,
//! `scrollCallback`, `selectionScrollTick`), on the owner view only:
//!
//! - A program that turned mouse reporting on gets presses, releases and motion (Shift held keeps the mouse for
//!   selecting unless `mouse-shift-capture` lets the program have Shift too); the selection then goes.
//!   Read-only mode (`toggle_readonly`) keeps the mouse from the program; `toggle_mouse_reporting` and
//!   `mouse-reporting` decide whether it may have it (the config applied when the pane starts or the config changes,
//!   the pane's current setting read on each event).
//! - Otherwise the left button selects through Ghostty's gesture (`Pane::gesture`: clicks counted within
//!   `click-repeat-interval` and a cell's width, word and line clicks, Ctrl/Super triple-click for command output,
//!   Ctrl+Alt for a block, autoscroll every 15 ms past an edge); Shift extends the selection; the release copies it
//!   per `copy-on-select` (`Shared::copy_on_select`). ⚠️ `cursor-click-to-move = true` is not carried out (the C API
//!   has no prompt-click state); agterm's default is `false`.
//! - The middle button follows `middle-click-action` (`primary-paste`: the primary selection, or the clipboard under
//!   `copy-on-select = clipboard`, as Ghostty reads it); the right button follows `right-click-action` (`paste`,
//!   `copy`, `copy-or-paste`, `ignore`, or `context-menu`: the word or link under it selected and copied per
//!   `copy-on-select`, with no menu to show).
//! - Links ([`super::links`]) are refreshed on every move and modifier change, opened on a click; while the program
//!   has the mouse, Shift is not counted (Ghostty's `mouseModsWithCapture`).
//! - The wheel goes to `Pane::wheel` (alternate scroll, a report, or the viewport) in whole steps: precise deltas
//!   accumulate per cell height, `mouse-scroll-multiplier` (default `precision:1,discrete:3`) applies.
//! - The pointer's shape is the program's (OSC 22; the I-beam by default), a hand over a live link. Hiding it while
//!   typing (`mouse-hide-while-typing`) is GPUI's app-wide cursor hide mode, set from the config.
//!
//! Positions are device pixels from the view's top left (the pane's surface coordinates).

use std::time::{Duration, Instant};

use agterm_core::ghostty_config::binds::{Action, CopyFormat, Mods};
use agterm_core::ghostty_config::{CopyOnSelect, MiddleClickAction, MouseShiftCapture, RightClickAction};
use agterm_term::{
    Autoscroll, ClipboardLocation, GestureEvent, GridPoint, MouseAction, MouseButton as PaneButton, MouseInput,
    MouseShape, Pane, PasteSource, WheelInput,
};
use gpui_kit::{
    Bounds, Context, CursorStyle, DispatchPhase, ElementInputHandler, Entity, Hitbox, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent, Task, Window,
};

use super::input::mods;
use super::links::{self, Link};
use super::{TerminalEvent, TerminalView, keyboard};

impl TerminalView {
    /// The owner's input for this frame: the pointer's shape over the grid, the input method while focused, and the
    /// pointer's listeners (presses and the wheel over the grid; motion and releases anywhere, so a drag carries on
    /// past the edge).
    pub(super) fn paint_input(
        &mut self,
        view: &Entity<Self>,
        bounds: Bounds<Pixels>,
        hitbox: &Hitbox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.set_cursor_style(self.pointer_style(), hitbox);
        keyboard::painted_in(self.shared.key(), window.window_handle());
        if self.focus.is_focused(window) {
            window.handle_input(&self.focus, ElementInputHandler::new(bounds, view.clone()), cx);
        }
        let target = view.downgrade();
        let over = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && over.is_hovered(window) {
                let _ = target.update(cx, |view, cx| view.mouse_down(event, bounds, window, cx));
            }
        });
        let target = view.downgrade();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble {
                let _ = target.update(cx, |view, cx| view.mouse_up(event, bounds, cx));
            }
        });
        let target = view.downgrade();
        let over = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = over.is_hovered(window);
                let _ = target.update(cx, |view, cx| view.mouse_move(event, bounds, hovered, window, cx));
            }
        });
        let target = view.downgrade();
        let over = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && over.is_hovered(window) {
                let _ = target.update(cx, |view, cx| view.scroll_wheel(event, bounds, cx));
            }
        });
    }
}

/// Ghostty's selection autoscroll interval (`termio/Thread.zig`'s `selection_scroll_ms`).
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(15);
/// GPUI scales the platform's wheel steps and precise deltas by this before handing them out (`SCROLL_LINES`).
const GPUI_SCROLL_FACTOR: f64 = 3.0;

/// The owner view's pointer state.
#[derive(Default)]
pub(super) struct PointerState {
    /// The buttons held, as this view saw them: left, middle, right.
    held: [bool; 3],
    /// The last pointer position, device pixels from the view's top left.
    last: Option<(f32, f32)>,
    /// The modifiers last seen with the pointer.
    mods: Mods,
    /// The live link under the pointer.
    pub(super) link: Option<Link>,
    /// What the link was looked up for, so a move inside one cell does not look again.
    link_key: Option<(GridPoint, Mods)>,
    /// Precise wheel deltas not yet a whole row and column.
    wheel: (f64, f64),
    /// When the left button last went down for a selection.
    last_click: Option<Instant>,
    /// `focus-follows-mouse` asked for focus since the pointer entered or the focus changed.
    focus_asked: bool,
    autoscroll: Option<Task<()>>,
}

/// `mouse-scroll-multiplier`: `(precision, discrete)`, Ghostty's `precision:1,discrete:3` unless set (a bare number
/// sets both), clamped to Ghostty's range.
#[must_use]
pub fn scroll_multiplier(value: Option<&str>) -> (f64, f64) {
    let mut multiplier = (1.0, 3.0);
    if let Some(value) = value {
        for part in value.split(',') {
            let (name, number) = part.split_once(':').unwrap_or(("", part));
            let Ok(number) = number.trim().parse::<f64>() else {
                continue;
            };
            match name.trim() {
                "precision" => multiplier.0 = number,
                "discrete" => multiplier.1 = number,
                "" => multiplier = (number, number),
                _ => {}
            }
        }
    }
    (multiplier.0.clamp(0.01, 10_000.0), multiplier.1.clamp(0.01, 10_000.0))
}

/// The GPUI cursor for the program's pointer shape.
#[must_use]
pub fn cursor_style(shape: MouseShape) -> CursorStyle {
    match shape {
        MouseShape::Default | MouseShape::Text => CursorStyle::IBeam,
        MouseShape::VerticalText => CursorStyle::IBeamCursorForVerticalLayout,
        MouseShape::Pointer => CursorStyle::PointingHand,
        MouseShape::Crosshair | MouseShape::Cell => CursorStyle::Crosshair,
        MouseShape::ContextMenu => CursorStyle::ContextualMenu,
        MouseShape::Alias => CursorStyle::DragLink,
        MouseShape::Copy => CursorStyle::DragCopy,
        MouseShape::NoDrop | MouseShape::NotAllowed => CursorStyle::OperationNotAllowed,
        MouseShape::Grab => CursorStyle::OpenHand,
        MouseShape::Grabbing | MouseShape::Move | MouseShape::AllScroll => CursorStyle::ClosedHand,
        MouseShape::ColResize => CursorStyle::ResizeColumn,
        MouseShape::RowResize => CursorStyle::ResizeRow,
        MouseShape::EwResize => CursorStyle::ResizeLeftRight,
        MouseShape::NsResize => CursorStyle::ResizeUpDown,
        MouseShape::NResize => CursorStyle::ResizeUp,
        MouseShape::SResize => CursorStyle::ResizeDown,
        MouseShape::EResize => CursorStyle::ResizeRight,
        MouseShape::WResize => CursorStyle::ResizeLeft,
        MouseShape::NeResize | MouseShape::SwResize | MouseShape::NeswResize => CursorStyle::ResizeUpRightDownLeft,
        MouseShape::NwResize | MouseShape::SeResize | MouseShape::NwseResize => CursorStyle::ResizeUpLeftDownRight,
        MouseShape::Help | MouseShape::Progress | MouseShape::Wait | MouseShape::ZoomIn | MouseShape::ZoomOut => {
            CursorStyle::Arrow
        }
    }
}

fn button_index(button: MouseButton) -> Option<usize> {
    match button {
        MouseButton::Left => Some(0),
        MouseButton::Middle => Some(1),
        MouseButton::Right => Some(2),
        MouseButton::Navigate(_) => None,
    }
}

fn pane_button(index: usize) -> PaneButton {
    match index {
        1 => PaneButton::Middle,
        2 => PaneButton::Right,
        _ => PaneButton::Left,
    }
}

impl TerminalView {
    /// The live pane.
    fn live_pane(&self) -> Option<std::rc::Rc<Pane>> {
        self.shared.pane().filter(|_| self.shared.is_realized())
    }

    /// A window position in device pixels from the view's top left.
    fn surface_position(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> (f32, f32) {
        let scale = self.shared.scale().max(1.0);
        let offset = position - bounds.origin;
        (f32::from(offset.x) * scale, f32::from(offset.y) * scale)
    }

    /// Whether the program may have Shift with the mouse (`mouse-shift-capture`).
    fn shift_capture(&self) -> bool {
        matches!(
            self.shared.config().mouse.shift_capture,
            MouseShiftCapture::True | MouseShiftCapture::Always
        )
    }

    /// Whether pointer events go to the program now, Shift's override considered.
    fn reporting(&self, pane: &Pane, mods: Mods) -> bool {
        let shift_override = mods.contains(Mods::SHIFT) && !self.shift_capture();
        self.tracking(pane) && !shift_override
    }

    /// Whether the program has the mouse: it asked for reports, the pane's reporting setting lets it, and the
    /// terminal is not read-only.
    fn tracking(&self, pane: &Pane) -> bool {
        !self.shared.is_readonly() && pane.mouse_tracking()
    }

    /// The modifiers links are looked up with: Shift dropped while the program has the mouse and may not capture
    /// Shift (Ghostty's `mouseModsWithCapture`), since Shift then only takes the mouse back.
    fn link_mods(&self, pane: &Pane, mods: Mods) -> Mods {
        if mods.contains(Mods::SHIFT) && !self.shift_capture() && self.tracking(pane) {
            mods - Mods::SHIFT
        } else {
            mods
        }
    }

    /// `click-repeat-interval` (0: 500 ms, Ghostty's default off macOS).
    fn repeat_interval(&self) -> Duration {
        match self.shared.config().mouse.click_repeat_interval {
            0 => agterm_term::CLICK_REPEAT_INTERVAL,
            ms => Duration::from_millis(u64::from(ms)),
        }
    }

    /// Performs a binding action on this terminal (the right-click actions are Ghostty's copy and paste).
    fn perform(&self, action: &Action) -> bool {
        super::TerminalSurface::new(std::rc::Rc::clone(&self.shared)).perform(action)
    }

    /// The cursor the pointer shows over the grid.
    pub(super) fn pointer_style(&self) -> CursorStyle {
        if self.pointer.link.is_some() {
            return CursorStyle::PointingHand;
        }
        self.painted
            .as_ref()
            .map_or(CursorStyle::IBeam, |frame| cursor_style(frame.mouse_shape))
    }

    pub(super) fn mouse_down(
        &mut self,
        event: &MouseDownEvent,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = button_index(event.button) else {
            return;
        };
        // only the left button moves focus; right and middle act without it (upstream)
        if index == 0 {
            window.focus(&self.focus, cx);
        }
        let position = self.surface_position(event.position, bounds);
        self.pointer.held[index] = true;
        self.pointer.last = Some(position);
        let held = mods(&event.modifiers);
        let changed = held != self.pointer.mods;
        self.pointer.mods = held;
        let Some(pane) = self.live_pane() else {
            return;
        };
        if changed {
            // the click's own modifiers decide the link under it (a modifier released unseen leaves no live link)
            let inside = bounds.contains(&event.position);
            self.refresh_link(&pane, inside.then_some(position), cx);
        }
        self.press(&pane, index, position, cx);
        cx.notify();
    }

    fn press(&mut self, pane: &Pane, index: usize, (x, y): (f32, f32), cx: &mut Context<Self>) {
        let held = self.pointer.mods;
        // Shift-click extends the selection a single click made, past the repeat interval
        if index == 0 && held.contains(Mods::SHIFT) && !self.shift_capture() && self.extends_selection(pane) {
            self.drag_selection(pane, (x, y), cx);
            return;
        }
        if self.reporting(pane, held) {
            if pane.selection().is_some() {
                let _ = pane.clear_selection();
            }
            pane.reset_gesture();
            self.report(pane, MouseAction::Press, Some(pane_button(index)), (x, y));
            return;
        }
        match index {
            0 => self.left_press(pane, (x, y)),
            1 => self.middle_press(pane, cx),
            _ => self.right_press(pane, (x, y)),
        }
    }

    /// Whether a Shift-click extends the selection (Ghostty: a click sequence is live, a selection exists, and the
    /// last click is older than the repeat interval).
    fn extends_selection(&self, pane: &Pane) -> bool {
        let since = self.pointer.last_click.map(|at| at.elapsed());
        pane.gesture_state().click_count > 0
            && pane.selection().is_some()
            && since.is_some_and(|since| since > self.repeat_interval())
    }

    fn left_press(&mut self, pane: &Pane, (x, y): (f32, f32)) {
        let now = Instant::now();
        self.pointer.last_click = Some(now);
        let output = self.pointer.mods.intersects(Mods::CTRL | Mods::SUPER);
        let gesture = pane.gesture(GestureEvent::Press {
            x,
            y,
            time: now,
            output,
            repeat_interval: self.repeat_interval(),
        });
        let Ok(gesture) = gesture else {
            return;
        };
        // a double click on a URL selects the whole URL, not the word (no URL is a link under `link-url = false`)
        if gesture.click_count == 2 && self.link_url() {
            let at = pane.cell_at(x, y);
            let url = self.painted.as_ref().and_then(|frame| {
                let row = u16::try_from(at.y).ok()?;
                links::url_at(frame, at.x, row)
            });
            if let Some(url) = url
                && let (Some(first), Some(last)) = (url.cells.first(), url.cells.last())
            {
                let start = GridPoint::viewport(first.1, u32::from(first.0));
                let end = GridPoint::viewport(last.2, u32::from(last.0));
                let _ = pane.select(start, end, false);
            }
        }
    }

    /// `middle-click-action`: `primary-paste` pastes the primary selection, or the clipboard under
    /// `copy-on-select = clipboard` (Ghostty's choice at `GHOSTTY_REV`); `ignore` touches nothing.
    fn middle_press(&self, pane: &Pane, cx: &mut Context<Self>) {
        let config = self.shared.config();
        if config.mouse.middle_click_action == MiddleClickAction::Ignore || self.shared.is_readonly() {
            return;
        }
        let (item, location) = if config.copy_on_select == CopyOnSelect::Clipboard {
            (cx.read_from_clipboard(), ClipboardLocation::Standard)
        } else {
            (cx.read_from_primary(), ClipboardLocation::Selection)
        };
        let Some(text) = item.and_then(|item| item.text()) else {
            return;
        };
        if let Err(err) = pane.paste(&text, PasteSource::Clipboard, location, true) {
            tracing::warn!(target: "agterm::surface", %err, "the middle-click paste failed");
        }
    }

    /// `right-click-action` (Ghostty's `mouseButtonCallback`): `copy` copies the selection (plain and HTML) to the
    /// clipboard and drops it, `paste` drops the selection and pastes the clipboard, `copy-or-paste` copies with a
    /// selection and pastes without one, `context-menu` selects what is under the pointer, `ignore` does nothing.
    fn right_press(&self, pane: &Pane, (x, y): (f32, f32)) {
        let has_selection = pane.selection().is_some();
        let copy = |view: &Self| {
            let _ = view.perform(&Action::CopyToClipboard(CopyFormat::Mixed));
            let _ = pane.clear_selection();
        };
        let paste = |view: &Self| {
            let _ = view.perform(&Action::PasteFromClipboard);
        };
        match self.shared.config().mouse.right_click_action {
            RightClickAction::Ignore => {}
            RightClickAction::ContextMenu => self.context_menu_select(pane, (x, y)),
            RightClickAction::Copy => {
                if has_selection {
                    copy(self);
                }
                let _ = pane.clear_selection();
            }
            RightClickAction::CopyOrPaste if has_selection => copy(self),
            RightClickAction::CopyOrPaste => paste(self),
            RightClickAction::Paste => {
                let _ = pane.clear_selection();
                paste(self);
            }
        }
    }

    /// `right-click-action = context-menu`: the word (or link) under the pointer selected and copied per
    /// `copy-on-select`, unless the selection holds it.
    fn context_menu_select(&self, pane: &Pane, (x, y): (f32, f32)) {
        let at = pane.cell_at(x, y);
        let Some(frame) = self.painted.as_ref() else {
            return;
        };
        let Ok(row) = u16::try_from(at.y) else {
            return;
        };
        let selected = frame
            .rows
            .get(usize::from(row))
            .and_then(|row| row.selection())
            .is_some_and(|(start, end)| (start..=end).contains(&at.x));
        if selected {
            return;
        }
        let link = links::link_at(
            pane,
            frame,
            at,
            self.link_mods(pane, self.pointer.mods),
            self.link_url(),
        );
        let selected = match link
            .as_ref()
            .and_then(|link| Some((link.cells.first()?, link.cells.last()?)))
        {
            Some((first, last)) => pane
                .select(
                    GridPoint::viewport(first.1, u32::from(first.0)),
                    GridPoint::viewport(last.2, u32::from(last.0)),
                    false,
                )
                .is_ok(),
            None => pane.select_word(at).unwrap_or(false),
        };
        if selected {
            self.shared.copy_on_select();
        }
    }

    /// `link-url`: URLs in the text are links.
    fn link_url(&self) -> bool {
        self.shared.config().link.url
    }

    pub(super) fn mouse_up(&mut self, event: &MouseUpEvent, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let Some(index) = button_index(event.button) else {
            return;
        };
        if !std::mem::take(&mut self.pointer.held[index]) {
            // a press this view never saw
            return;
        }
        let position = self.surface_position(event.position, bounds);
        self.pointer.last = Some(position);
        self.pointer.mods = mods(&event.modifiers);
        let Some(pane) = self.live_pane() else {
            return;
        };
        if index == 0 && self.left_release(&pane, position, cx) {
            cx.notify();
            return;
        }
        if self.reporting(&pane, self.pointer.mods) {
            self.report(&pane, MouseAction::Release, Some(pane_button(index)), position);
        }
        cx.notify();
    }

    /// The left button went up: the gesture learns it, the selection goes to the primary selection, a click on a
    /// live link opens it. Whether the link took the click.
    fn left_release(&mut self, pane: &Pane, (x, y): (f32, f32), cx: &mut Context<Self>) -> bool {
        let released = pane.gesture(GestureEvent::Release { x, y });
        self.pointer.autoscroll = None;
        // the selection is copied once the drag ends, not on every move (Ghostty)
        self.shared.copy_on_select();
        let dragged = released.is_ok_and(|gesture| gesture.dragged);
        let Some(link) = self.pointer.link.clone().filter(|_| !dragged) else {
            return false;
        };
        let at = pane.cell_at(x, y);
        if !u16::try_from(at.y).is_ok_and(|row| link.covers(at.x, row)) {
            return false;
        }
        self.terminal
            .update(cx, |_, cx| cx.emit(TerminalEvent::OpenUrl(link.url)));
        true
    }

    /// The pointer moved anywhere in the window; `hovered`: over this view's grid, not under anything drawn on top.
    pub(super) fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        bounds: Bounds<Pixels>,
        hovered: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let position = self.surface_position(event.position, bounds);
        self.pointer.last = Some(position);
        self.pointer.mods = mods(&event.modifiers);
        self.follow_focus(hovered, event.pressed_button.is_none(), window, cx);
        let Some(pane) = self.live_pane() else {
            return;
        };
        // read-only: the program never gets the mouse, which selects as it would without reporting
        let tracking = self.tracking(&pane);
        let shift_override = self.pointer.mods.contains(Mods::SHIFT) && !self.shift_capture();
        let inside = bounds.contains(&event.position);
        if !tracking || shift_override {
            self.refresh_link(&pane, inside.then_some(position), cx);
        }
        let button = self.pointer.held.iter().position(|held| *held).map(pane_button);
        // Shift keeps the mouse for selecting while a button is held (Kitty's rule); otherwise the program has it
        if tracking && !(shift_override && button.is_some()) {
            if inside || button.is_some() {
                self.report(&pane, MouseAction::Motion, button, position);
            }
            return;
        }
        if self.pointer.held[0] {
            self.drag_selection(&pane, position, cx);
        }
    }

    /// `focus-follows-mouse` (Ghostty's `mouseMoved`): the pointer over this view (`hovered`: not under an overlay),
    /// which does not hold the keyboard, in the active window, with no button held anywhere (`buttons_up`: a drag
    /// begun in another view does not move focus), asks the frontend for focus once
    /// ([`TerminalEvent::FocusFollowsMouse`]); leaving the view lets it ask again.
    fn follow_focus(&mut self, hovered: bool, buttons_up: bool, window: &Window, cx: &mut Context<Self>) {
        if !hovered {
            self.pointer.focus_asked = false;
            return;
        }
        let wanted = self.shared.config().mouse.focus_follows_mouse
            && window.is_window_active()
            && !self.focus.is_focused(window)
            && buttons_up;
        if wanted && !std::mem::replace(&mut self.pointer.focus_asked, true) {
            self.terminal
                .update(cx, |_, cx| cx.emit(TerminalEvent::FocusFollowsMouse));
        }
    }

    /// The view's focus changed: the pointer may ask for focus again.
    pub(super) fn focus_changed(&mut self) {
        self.pointer.focus_asked = false;
    }

    /// A drag with the left button: the gesture's selection, and autoscroll past an edge.
    fn drag_selection(&mut self, pane: &Pane, (x, y): (f32, f32), cx: &mut Context<Self>) {
        if pane.gesture_state().click_count == 0 {
            return;
        }
        let rectangle = self.rectangle();
        let Ok(gesture) = pane.gesture(GestureEvent::Drag { x, y, rectangle }) else {
            return;
        };
        if gesture.autoscroll == Autoscroll::None {
            self.pointer.autoscroll = None;
        } else if self.pointer.autoscroll.is_none() {
            self.start_autoscroll(cx);
        }
        cx.notify();
    }

    /// Ghostty's block selection: Ctrl (or Super) and Alt held.
    fn rectangle(&self) -> bool {
        self.pointer.mods.intersects(Mods::CTRL | Mods::SUPER) && self.pointer.mods.contains(Mods::ALT)
    }

    /// Ticks the gesture every 15 ms while it asks for autoscroll.
    fn start_autoscroll(&mut self, cx: &mut Context<Self>) {
        let timer = cx.background_executor().clone();
        self.pointer.autoscroll = Some(cx.spawn(async move |this, cx| {
            loop {
                timer.timer(AUTOSCROLL_INTERVAL).await;
                let ticked = this.update(cx, Self::autoscroll_tick);
                if !matches!(ticked, Ok(true)) {
                    break;
                }
            }
        }));
    }

    /// One autoscroll step; whether to go on.
    fn autoscroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let (Some(pane), Some((x, y))) = (self.live_pane(), self.pointer.last) else {
            self.pointer.autoscroll = None;
            return false;
        };
        let rectangle = self.rectangle();
        let going = pane
            .gesture(GestureEvent::Tick { x, y, rectangle })
            .is_ok_and(|gesture| gesture.autoscroll != Autoscroll::None && gesture.click_count > 0);
        cx.notify();
        if !going {
            self.pointer.autoscroll = None;
        }
        going
    }

    /// Reports a pointer event to the program.
    fn report(&self, pane: &Pane, action: MouseAction, button: Option<PaneButton>, (x, y): (f32, f32)) {
        let input = MouseInput {
            action,
            button,
            mods: self.pointer.mods,
            x,
            y,
        };
        if let Err(err) = pane.mouse(&input) {
            tracing::debug!(target: "agterm::surface", %err, "mouse report failed");
        }
    }

    /// Looks the live link under the pointer up again (`None`: the pointer left the grid) and underlines it.
    fn refresh_link(&mut self, pane: &Pane, position: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let mods = self.pointer.mods;
        let at = position.map(|(x, y)| pane.cell_at(x, y));
        let key = at.map(|at| (at, mods));
        if key == self.pointer.link_key && self.pointer.link.is_none() {
            return;
        }
        self.pointer.link_key = key;
        // a drag that left its first cell shows no link until the button is up
        let dragging = self.pointer.held[0] && pane.gesture_state().dragged;
        let link_mods = self.link_mods(pane, mods);
        let link = at.filter(|_| !dragging).and_then(|at| {
            let frame = self.painted.as_ref()?;
            links::link_at(pane, frame, at, link_mods, self.link_url())
        });
        if link != self.pointer.link {
            let cells = link.as_ref().map(|link| link.cells.clone()).unwrap_or_default();
            self.shared.set_hovered_url(link.as_ref().map(|link| link.url.clone()));
            self.pointer.link = link;
            self.set_hovered_links(cells, cx);
        }
    }

    /// The modifiers changed under a resting pointer: links may come alive or go.
    pub(super) fn modifiers_changed(&mut self, modifiers: gpui_kit::Modifiers, cx: &mut Context<Self>) {
        self.pointer.mods = mods(&modifiers);
        let Some(pane) = self.live_pane() else {
            return;
        };
        let position = self.pointer.last;
        self.refresh_link(&pane, position, cx);
    }

    pub(super) fn scroll_wheel(&mut self, event: &ScrollWheelEvent, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let Some(pane) = self.live_pane() else {
            return;
        };
        let (x, y) = self.surface_position(event.position, bounds);
        let (precision, discrete) = scroll_multiplier(self.shared.config().mouse.scroll_multiplier.as_deref());
        let (columns, rows) = match event.delta {
            ScrollDelta::Lines(lines) => {
                let step = |value: f32| f64::from(value) / GPUI_SCROLL_FACTOR * discrete;
                (step(-lines.x), step(lines.y))
            }
            ScrollDelta::Pixels(pixels) => {
                let scale = f64::from(self.shared.scale().max(1.0));
                let metrics = self.shared.metrics();
                let step = |value: Pixels, cell: u32| {
                    f64::from(f32::from(value)) * scale / GPUI_SCROLL_FACTOR * precision / f64::from(cell.max(1))
                };
                (step(-pixels.x, metrics.cell_width), step(pixels.y, metrics.cell_height))
            }
        };
        self.pointer.wheel.0 += columns;
        self.pointer.wheel.1 += rows;
        let whole = |value: f64| {
            #[expect(clippy::cast_possible_truncation, reason = "a wheel turn is a few rows")]
            let steps = value.trunc() as isize;
            steps
        };
        let (columns, rows) = (whole(self.pointer.wheel.0), whole(self.pointer.wheel.1));
        #[expect(clippy::cast_precision_loss, reason = "a wheel turn is a few rows")]
        {
            self.pointer.wheel.0 -= columns as f64;
            self.pointer.wheel.1 -= rows as f64;
        }
        if rows == 0 && columns == 0 {
            return;
        }
        let wheel = WheelInput {
            rows,
            columns,
            mods: mods(&event.modifiers),
            x,
            y,
        };
        if self.shared.is_readonly() {
            // read-only: no report, no cursor keys; the viewport still scrolls
            if rows != 0 {
                pane.scroll(agterm_term::Scroll::Rows(-rows));
            }
            cx.notify();
            return;
        }
        if let Err(err) = pane.wheel(&wheel) {
            tracing::debug!(target: "agterm::surface", %err, "wheel failed");
        }
        cx.notify();
    }
}
