//! How the popup draws its pages ([`super::form`]): the native panel (`crate::modal::panel`) holding a title row with
//! its close button, the tab strip (upstream's toolbar tabs: a glyph over a label, the shown one washed), and the shown
//! page scrolling under it, each section a titled rounded box of rows separated by hairlines, in the chrome's colors.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::color_picker::ColorPicker;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::slider::Slider;
use gpui_kit::component::switch::Switch;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Bounds, Context, Div, FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent,
    MouseButton, ParentElement as _, Pixels, Role, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, px, svg,
};
use ngterm_core::chrome::Rgb;
use ngterm_core::model::StatusShape;

use super::form::{Action, Control, Item, Picker, Section, SliderKey};
use super::{Edits, SettingsView, TITLE, Tab};
use crate::modal::{self, ButtonLook};

/// The row text size (upstream's system font in a grouped form).
const TEXT: f32 = 13.;
/// A caption's size (upstream's `SettingHint`, `.system(size: 11)`).
const HINT: f32 = 11.;
/// A path's or a diagnostic's size (upstream's `.system(size: 12).monospaced()`).
const MONO: f32 = 12.;
/// A row's least height.
const ROW_HEIGHT: f32 = 34.;
/// A slider's track width.
const SLIDER_WIDTH: f32 = 180.;
/// The readout beside a slider (upstream's `.frame(width: 42)`, 64 for the tint's words).
const READOUT_WIDTH: f32 = 64.;
/// The column every shape picker takes, so the color wells line up (upstream's `shapePickerWidth`).
const SHAPE_COLUMN: f32 = 80.;
/// The height past which a long picker's menu scrolls.
const MENU_HEIGHT: f32 = 380.;
/// Pickers with more rows than this scroll their menu.
const LONG_MENU: usize = 12;
/// A tab's glyph.
const TAB_GLYPH: f32 = 20.;
/// The title row's height.
const HEADER_HEIGHT: f32 = 36.;
/// The close button's side, and the space balancing it on the title's other side.
const CLOSE_BUTTON: f32 = 24.;
/// The opacity of secondary text: hints, a path not set, "No issues.", the labels of controls that take no input.
const DIM: f64 = 0.55;
/// A disabled control's opacity.
const DISABLED: f32 = 0.45;

/// What drawing needs: the colors, and where edits go.
#[derive(Clone)]
struct Look {
    colors: ngterm_core::chrome::ChromeColors,
    edits: Edits,
}

impl Look {
    /// The chrome text at `alpha`.
    fn ink(&self, alpha: f64) -> Hsla {
        crate::chrome::color(self.colors.foreground, alpha)
    }

    /// The selected-state fill (libadwaita's accent, the app's suggested buttons').
    fn accent() -> Hsla {
        crate::chrome::color(modal::ACCENT, 1.0)
    }
}

/// The whole popup: the native panel, which holds the keyboard in the popup's key context (Escape closes it) and
/// keeps its clicks from the layer around it (which closes it on one outside).
pub fn panel(view: &SettingsView, cx: &mut Context<SettingsView>) -> impl IntoElement {
    let look = Look {
        colors: crate::chrome::current(cx),
        edits: view.edits.clone(),
    };
    modal::panel(look.colors)
        .id("settings")
        .accessibility_id("settings")
        .role(Role::Dialog)
        .aria_label(TITLE)
        .debug_selector(|| "settings".to_owned())
        .key_context(super::KEY_CONTEXT)
        .track_focus(&view.focus)
        .on_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
            if modal::nav_key(&event.keystroke) == Some(modal::NavKey::Cancel) {
                cx.stop_propagation();
                view.dismiss(cx);
            }
        }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .size_full()
        .flex()
        .flex_col()
        .text_size(px(TEXT))
        .child(header(&look, cx))
        .child(tabs(view.tab, &look, cx))
        .child(page(view, &look))
}

/// The title row: the title centered, the close button trailing.
fn header(look: &Look, cx: &mut Context<SettingsView>) -> Div {
    let close = div()
        .id("settings-close")
        .accessibility_id("settings-close")
        .role(Role::Button)
        .aria_label("Close")
        .debug_selector(|| "settings-close".to_owned())
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(CLOSE_BUTTON))
        .rounded(px(6.))
        .cursor_pointer()
        .hover({
            let hover = look.ink(0.12);
            move |style| style.bg(hover)
        })
        .child(glyph(IconName::X, look.ink(0.75), px(14.)))
        .on_click(cx.listener(|view, _, _, cx| view.dismiss(cx)));
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .h(px(HEADER_HEIGHT))
        .px(px(8.))
        .child(div().flex_shrink_0().w(px(CLOSE_BUTTON)))
        .child(
            div()
                .flex_1()
                .flex()
                .justify_center()
                .font_weight(FontWeight::SEMIBOLD)
                .child(TITLE),
        )
        .child(close)
}

/// The tab strip: every tab, the shown one washed.
fn tabs(shown: Tab, look: &Look, cx: &mut Context<SettingsView>) -> Div {
    div()
        .flex()
        .flex_row()
        .flex_shrink_0()
        .justify_center()
        .gap(px(2.))
        .py(px(6.))
        .border_b_1()
        .border_color(crate::chrome::separator(&look.colors))
        .children(Tab::ALL.into_iter().map(|tab| {
            let id = SharedString::from(format!("settings-tab-{}", tab.name()));
            let selector = id.to_string();
            let selected = tab == shown;
            let wash = look.ink(0.1);
            let hover = look.ink(0.05);
            div()
                .id(id.clone())
                .accessibility_id(id)
                .role(Role::Tab)
                .aria_selected(selected)
                .aria_label(tab.title())
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .px(px(9.))
                .py(px(4.))
                .rounded(px(6.))
                .when(selected, |tab| tab.bg(wash))
                .when(!selected, |tab| tab.hover(move |style| style.bg(hover)))
                .child(glyph(
                    tab.glyph(),
                    look.ink(if selected { 1.0 } else { 0.75 }),
                    px(TAB_GLYPH),
                ))
                .child(div().text_size(px(HINT)).child(tab.title()))
                .on_click(cx.listener(move |view, _, _, cx| view.select(tab, cx)))
        }))
}

/// A Lucide glyph in `ink` at `size`.
fn glyph(name: IconName, ink: Hsla, size: Pixels) -> impl IntoElement {
    svg().path(name.path()).size(size).flex_shrink_0().text_color(ink)
}

/// The shown page, scrolling.
fn page(view: &SettingsView, look: &Look) -> Stateful<Div> {
    let sections = view.sections();
    div()
        .id("settings-page")
        .debug_selector(|| "settings-page".to_owned())
        .flex_1()
        .min_h(px(0.))
        .overflow_y_scroll()
        .track_scroll(&view.scroll)
        .px(px(20.))
        .py(px(16.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(18.))
                .children(sections.into_iter().map(|section| render_section(section, view, look))),
        )
}

/// One section: its title over the box of its rows.
fn render_section(section: Section, view: &SettingsView, look: &Look) -> AnyElement {
    let Section { title, items, columns } = section;
    let rows: Vec<AnyElement> = if columns > 1 {
        paired(items, view, look)
    } else {
        items
            .into_iter()
            .enumerate()
            .map(|(index, item)| {
                separated(
                    index > 0 && !matches!(item.control, Control::Hint),
                    row(item, view, look),
                    look,
                )
            })
            .collect()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .when_some(title, |section, title| {
            section.child(div().px(px(4.)).font_weight(FontWeight::SEMIBOLD).child(title))
        })
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(8.))
                .bg(look.ink(0.04))
                .border_1()
                .border_color(look.ink(0.08))
                .children(rows),
        )
        .into_any_element()
}

/// A row with a hairline above it unless it leads its box or is a caption.
fn separated(line: bool, row: AnyElement, look: &Look) -> AnyElement {
    div()
        .when(line, |row| row.border_t_1().border_color(look.ink(0.08)))
        .child(row)
        .into_any_element()
}

/// The rows two per row around a divider, an odd last one beside an empty half (upstream's `twoColumnSection`).
fn paired(items: Vec<Item>, view: &SettingsView, look: &Look) -> Vec<AnyElement> {
    let mut items = items.into_iter();
    let mut rows = Vec::new();
    while let Some(first) = items.next() {
        let second = items.next();
        let line = !rows.is_empty();
        let pair = div()
            .flex()
            .flex_row()
            .items_center()
            .child(div().flex_1().min_w(px(0.)).child(row(first, view, look)))
            .child(div().w(px(1.)).h(px(20.)).bg(look.ink(0.12)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .children(second.map(|second| row(second, view, look))),
            )
            .into_any_element();
        rows.push(separated(line, pair, look));
    }
    rows
}

/// One row: the label leading, the control trailing; a path or the diagnostics take the row.
fn row(item: Item, view: &SettingsView, look: &Look) -> AnyElement {
    let Item {
        id,
        title,
        control,
        enabled,
    } = item;
    let label = div()
        .flex_1()
        .min_w(px(0.))
        .when(!enabled, |label| label.text_color(look.ink(DIM)))
        .child(title.clone());
    let control = match control {
        Control::Hint => return hint(id, title, look),
        Control::Path { text, unset, buttons } => {
            return path(&id, text, unset, buttons, view.selectable(&id), look);
        }
        Control::Diagnostics { lines } => {
            let selectable = view.selectable(&id).cloned();
            return diagnostics(id, lines, selectable.as_ref(), look);
        }
        Control::Button(action) => return frame(div().child(push_button(action, look))),
        Control::Toggle { on, set } => toggle(&id, &title, on, enabled, set, look),
        Control::Picker(picker) => picker_button(&id, &title, picker, look),
        Control::Stepper { value, range, set } => stepper((&id, &title), value, &range, enabled, &set, look),
        Control::Slider { key, readout, .. } => slider(view, key, &title, readout, enabled, look),
        Control::Glyph { status, shape, .. } => glyph_controls(view, status, &id, shape, look),
    };
    frame(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .child(label)
            .child(control),
    )
}

/// A row's padding and least height.
fn frame(content: Div) -> AnyElement {
    content
        .min_h(px(ROW_HEIGHT))
        .px(px(12.))
        .py(px(5.))
        .flex()
        .items_center()
        .into_any_element()
}

/// A caption under the row above it.
fn hint(id: String, text: String, look: &Look) -> AnyElement {
    div()
        .id(SharedString::from(id.clone()))
        .accessibility_id(SharedString::from(id.clone()))
        .role(Role::Label)
        .aria_label(SharedString::from(text.clone()))
        .debug_selector(move || id.clone())
        .px(px(12.))
        .pb(px(6.))
        .text_size(px(HINT))
        .text_color(look.ink(DIM))
        .child(text)
        .into_any_element()
}

/// A switch, in the accent while on, named `title`. A gpui-kit switch takes no accessibility id of its own, so the group
/// around it carries the row's.
fn toggle(
    id: &str,
    title: &str,
    on: bool,
    enabled: bool,
    set: Rc<dyn Fn(bool) -> ngterm_core::settings_model::SettingsChange>,
    look: &Look,
) -> AnyElement {
    let edits = look.edits.clone();
    let selector = id.to_owned();
    div()
        .id(SharedString::from(format!("{id}-group")))
        .accessibility_id(SharedString::from(id.to_owned()))
        .role(Role::Group)
        .aria_label(SharedString::from(title.to_owned()))
        .debug_selector(move || selector.clone())
        .child(
            Switch::new(SharedString::from(id.to_owned()))
                .accessibility_label(SharedString::from(title.to_owned()))
                .checked(on)
                .color(Look::accent())
                .disabled(!enabled)
                .on_click(move |checked, _, _| edits.send(set(*checked))),
        )
        .into_any_element()
}

/// A picker's popup button, named `title`: the shown row's label (a shape's silhouette) and a caret; its menu checks the
/// shown row.
fn picker_button(id: &str, title: &str, picker: Picker, look: &Look) -> AnyElement {
    let picker = Rc::new(picker);
    let ink = look.ink(1.0);
    let face = match picker.shapes {
        Some(tint) => shape_face(&picker, tint).into_any_element(),
        None => div()
            .text_color(ink)
            .child(picker.label().to_owned())
            .into_any_element(),
    };
    let edits = look.edits.clone();
    let selector = id.to_owned();
    Button::new(SharedString::from(id.to_owned()))
        .accessibility_id(SharedString::from(id.to_owned()))
        .accessibility_label(SharedString::from(title.to_owned()))
        .debug_selector(move || selector.clone())
        .ghost()
        .h(px(24.))
        .px(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(look.ink(0.15))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .child(face)
                .child(glyph(IconName::ChevronDown, look.ink(0.6), px(12.))),
        )
        .dropdown_menu_with_anchor(gpui_kit::Anchor::TopRight, move |menu, _, _| {
            options_menu(menu, &picker, &edits)
        })
        .into_any_element()
}

/// The shown shape's silhouette, tinted.
fn shape_face(picker: &Picker, tint: crate::status_glyph::Rgb) -> impl IntoElement {
    let shape = picker
        .selected
        .and_then(|index| StatusShape::ALL.get(index).copied())
        .unwrap_or(StatusShape::Circle);
    silhouette(shape, tint)
}

/// `shape`'s silhouette in its 16-point slot, tinted (the sidebar's glyph).
fn silhouette(shape: StatusShape, tint: crate::status_glyph::Rgb) -> impl IntoElement {
    let color = crate::status_glyph::hsla(tint);
    let slot = px(crate::row_icon::SLOT);
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window: &mut Window, _: &mut App| {
            crate::row_icon::paint_mask(window, bounds, color, crate::row_icon::Mask::Status(shape));
        },
    )
    .size(slot)
    .flex_none()
}

/// A picker's menu: a row per option, the shown one checked; a long list scrolls.
fn options_menu(menu: PopupMenu, picker: &Rc<Picker>, edits: &Edits) -> PopupMenu {
    let long = picker.options.len() > LONG_MENU;
    (0..picker.options.len()).fold(menu.scrollable(long).max_h(px(MENU_HEIGHT)), |menu, index| {
        menu.item(option_item(picker, index, edits))
    })
}

/// Option `index` of `picker`'s menu.
fn option_item(picker: &Rc<Picker>, index: usize, edits: &Edits) -> PopupMenuItem {
    let label = picker.options[index].clone();
    let item = match (picker.shapes, StatusShape::ALL.get(index).copied()) {
        (Some(tint), Some(shape)) => PopupMenuItem::element(move |_, _| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .child(silhouette(shape, tint))
                .child(label.clone())
        }),
        _ => PopupMenuItem::new(label),
    };
    let (picker, edits) = (Rc::clone(picker), edits.clone());
    item.checked(picker.selected == Some(index))
        .on_click(move |_, window, cx| picked(&picker, index, &edits, window, cx))
}

/// Row `index` was picked: its edit goes out, and a sound row previews (upstream's pickers play the picked sound).
fn picked(picker: &Picker, index: usize, edits: &Edits, window: &Window, cx: &App) {
    if let Some(change) = picker.pick(index) {
        edits.send(change);
    }
    if picker.previews_sound
        && index > 0
        && let Some(name) = picker.options.get(index)
    {
        crate::status_sound::play(name, || crate::status_sound::ring(window, cx));
    }
}

/// A stepper's two buttons, each greyed at its bound, grouped under the row's id and title.
fn stepper(
    (id, title): (&str, &str),
    value: f64,
    range: &std::ops::RangeInclusive<f64>,
    enabled: bool,
    set: &Rc<dyn Fn(f64) -> ngterm_core::settings_model::SettingsChange>,
    look: &Look,
) -> AnyElement {
    let selector = id.to_owned();
    div()
        .id(SharedString::from(id.to_owned()))
        .accessibility_id(SharedString::from(id.to_owned()))
        .role(Role::Group)
        .aria_label(SharedString::from(title.to_owned()))
        .debug_selector(move || selector.clone())
        .flex()
        .flex_row()
        .gap(px(4.))
        .child(step_button(
            format!("{id}-decrement"),
            (IconName::Minus, "Decrease"),
            enabled && value > *range.start(),
            value - 1.0,
            set,
            look,
        ))
        .child(step_button(
            format!("{id}-increment"),
            (IconName::Plus, "Increase"),
            enabled && value < *range.end(),
            value + 1.0,
            set,
            look,
        ))
        .into_any_element()
}

/// One stepper button, its glyph and the name it is announced by, making `set`'s edit for `to`.
fn step_button(
    id: String,
    (name, label): (IconName, &'static str),
    enabled: bool,
    to: f64,
    set: &Rc<dyn Fn(f64) -> ngterm_core::settings_model::SettingsChange>,
    look: &Look,
) -> impl IntoElement {
    let (fill, ink) = modal::button_colors(ButtonLook::Plain, look.colors);
    let hover = look.ink(0.16);
    let selector = id.clone();
    let (edits, set) = (look.edits.clone(), Rc::clone(set));
    let id = SharedString::from(id);
    div()
        .id(id.clone())
        .accessibility_id(id)
        .role(Role::Button)
        .aria_label(label)
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(6.))
        .bg(fill)
        .child(glyph(name, ink, px(14.)))
        .when(!enabled, |button| button.opacity(DISABLED))
        .when(enabled, |button| {
            button
                .hover(move |style| style.bg(hover))
                .on_click(move |_, _, _| edits.send(set(to)))
        })
}

/// A slider in the accent with its readout, grouped under `title`.
fn slider(view: &SettingsView, key: SliderKey, title: &str, readout: String, enabled: bool, look: &Look) -> AnyElement {
    let Some(state) = view.slider(key) else {
        return div().into_any_element();
    };
    let id = key.id();
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .child(
            div()
                .id(id)
                .accessibility_id(id)
                .role(Role::Group)
                .aria_label(SharedString::from(title.to_owned()))
                .debug_selector(move || id.to_owned())
                .w(px(SLIDER_WIDTH))
                .child(
                    Slider::new(state)
                        .bg(Look::accent())
                        .text_color(crate::chrome::color(Rgb::WHITE, 1.0))
                        .disabled(!enabled),
                ),
        )
        .child(
            div()
                .w(px(READOUT_WIDTH))
                .flex()
                .justify_end()
                .when(!enabled, |readout| readout.text_color(look.ink(DIM)))
                .child(readout),
        )
        .into_any_element()
}

/// A path taking the row, dimmed while unset, then its buttons.
fn path(
    id: &str,
    text: String,
    unset: bool,
    buttons: Vec<Action>,
    selectable: Option<&crate::selectable::Selectable>,
    look: &Look,
) -> AnyElement {
    let selector = id.to_owned();
    let color = look.ink(if unset { DIM } else { 1.0 });
    // the selectable text carries the id for accessibility itself; the plain text's frame carries it
    let plain = selectable.is_none().then(|| SharedString::from(text.clone()));
    // upstream's monospaced, selectable `Text`; plain text until the popup made its field
    let shown = match selectable {
        Some(selectable) => selectable.element(id, color, px(MONO), true),
        None => div().truncate().child(text).into_any_element(),
    };
    frame(
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .id(SharedString::from(id.to_owned()))
                    .when_some(plain, |frame, text| {
                        frame
                            .accessibility_id(SharedString::from(id.to_owned()))
                            .role(Role::Label)
                            .aria_label(text)
                    })
                    .debug_selector(move || selector.clone())
                    .flex_1()
                    .min_w(px(0.))
                    .font_family("monospace")
                    .text_size(px(MONO))
                    .text_color(color)
                    .child(shown),
            )
            .children(buttons.into_iter().map(|action| push_button(action, look))),
    )
}

/// A file's diagnostics as selectable lines (upstream's `.textSelection(.enabled)`), or "No issues.".
fn diagnostics(
    id: String,
    lines: Vec<String>,
    selectable: Option<&crate::selectable::Selectable>,
    look: &Look,
) -> AnyElement {
    let empty = lines.is_empty();
    let shown = (!empty)
        .then_some(selectable)
        .flatten()
        .map(|selectable| selectable.element(&id, look.ink(DIM), px(MONO), true));
    let selector = id.clone();
    // the selectable lines carry the id for accessibility themselves; else the frame carries it
    let plain = shown.is_none().then(|| {
        SharedString::from(if empty {
            "No issues.".to_owned()
        } else {
            lines.join("\n")
        })
    });
    let body = div()
        .id(SharedString::from(id.clone()))
        .when_some(plain, |body, text| {
            body.accessibility_id(SharedString::from(id))
                .role(Role::Label)
                .aria_label(text)
        })
        .debug_selector(move || selector.clone())
        .flex()
        .flex_col()
        .gap(px(4.))
        .text_color(look.ink(DIM));
    let body = match (empty, shown) {
        (true, _) => body.child("No issues."),
        (false, Some(shown)) => body.font_family("monospace").text_size(px(MONO)).child(shown),
        (false, None) => body.font_family("monospace").text_size(px(MONO)).children(lines),
    };
    frame(div().w_full().py(px(4.)).child(body))
}

/// A push button in the app's native look, running its action.
fn push_button(action: Action, look: &Look) -> Stateful<Div> {
    let Action { id, label, run } = action;
    let (fill, ink) = modal::button_colors(ButtonLook::Plain, look.colors);
    let hover = look.ink(0.16);
    let edits = look.edits.clone();
    div()
        .id(id)
        .accessibility_id(id)
        .role(Role::Button)
        .aria_label(label)
        .debug_selector(move || id.to_owned())
        .flex_shrink_0()
        .px(px(10.))
        .py(px(3.))
        .rounded(px(6.))
        .bg(fill)
        .text_color(ink)
        .hover(move |style| style.bg(hover))
        .child(label)
        .on_click(move |_, window, cx| edits.run(&run, window, cx))
}

/// A glyph row's color well and shape picker, the picker in a fixed column so the wells line up.
fn glyph_controls(
    view: &SettingsView,
    status: ngterm_core::model::AgentStatus,
    id: &str,
    shape: Picker,
    look: &Look,
) -> AnyElement {
    let well = view.well(status).map(|state| {
        let id = format!("settings-status-{}", status.wire_name());
        let selector = id.clone();
        // the color picker takes no accessibility id of its own: the group around it carries it
        div()
            .id(SharedString::from(format!("{id}-group")))
            .accessibility_id(SharedString::from(id))
            .role(Role::Group)
            .aria_label("Color")
            .debug_selector(move || selector.clone())
            .child(ColorPicker::new(state).accessibility_label("Color"))
    });
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.))
        .children(well)
        .child(
            div()
                .w(px(SHAPE_COLUMN))
                .flex()
                .justify_end()
                .child(picker_button(id, "Shape", shape, look)),
        )
        .into_any_element()
}
