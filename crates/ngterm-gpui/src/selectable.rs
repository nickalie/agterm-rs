//! Text the user can select and copy but not edit (upstream's `.textSelection(.enabled)`): the Settings popup's paths
//! and diagnostics, a page's load error. GPUI text has no selection of its own, so each is a read-only field of
//! gpui-kit's, drawn without a field's frame so it reads as text: one line ([`Selectable::Line`], scrolled sideways
//! past its width, as upstream truncates a path) or several ([`Selectable::Lines`], growing with them). A click puts the
//! caret in it, a drag or Ctrl+A selects, Ctrl+C copies; typing changes nothing.

use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Entity, Hsla, IntoElement, Pixels, SharedString, Styled as _, Window,
};

/// The most lines a multi-line text grows to before it scrolls.
const MAX_LINES: usize = 12;

/// A read-only selectable text's state.
#[derive(Clone)]
pub enum Selectable {
    Line(Entity<InputState>),
    Lines(Entity<TextareaState>),
}

impl Selectable {
    /// A one-line text holding `text`.
    pub fn line(text: &str, window: &mut Window, cx: &mut App) -> Self {
        let text = text.to_owned();
        Self::Line(cx.new(|cx| InputState::new(window, cx).default_value(text)))
    }

    /// A text of `text`'s lines.
    pub fn lines(text: &str, window: &mut Window, cx: &mut App) -> Self {
        let text = text.to_owned();
        Self::Lines(cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, MAX_LINES)
                .default_value(text)
        }))
    }

    /// The text it holds.
    #[must_use]
    pub fn text(&self, cx: &App) -> SharedString {
        match self {
            Self::Line(state) => state.read(cx).value(),
            Self::Lines(state) => state.read(cx).value(),
        }
    }

    /// Holds `text` from now on; nothing when it already does (so a selection survives an unchanged sync).
    pub fn set(&self, text: &str, window: &mut Window, cx: &mut App) {
        if self.text(cx).as_ref() == text {
            return;
        }
        let text = text.to_owned();
        match self {
            Self::Line(state) => state.update(cx, |state, cx| state.set_value(text, window, cx)),
            Self::Lines(state) => state.update(cx, |state, cx| state.set_value(text, window, cx)),
        }
    }

    /// The text drawn in `color` at `size`, monospaced when `mono`, as element `id` (and its accessibility id).
    #[must_use]
    pub fn element(&self, id: &str, color: Hsla, size: Pixels, mono: bool) -> AnyElement {
        let id = SharedString::from(id.to_owned());
        match self {
            Self::Line(state) => {
                let field = Input::new(state)
                    .readonly(true)
                    .appearance(false)
                    .accessibility_id(id)
                    .p_0()
                    .text_size(size)
                    .text_color(color);
                if mono { field.font_family("monospace") } else { field }.into_any_element()
            }
            Self::Lines(state) => {
                let field = Textarea::new(state)
                    .readonly(true)
                    .appearance(false)
                    .accessibility_id(id)
                    .p_0()
                    .text_size(size)
                    .text_color(color);
                if mono { field.font_family("monospace") } else { field }.into_any_element()
            }
        }
    }
}
