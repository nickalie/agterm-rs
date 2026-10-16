//! What a page is made of: a list of [`Section`]s (upstream's grouped `Form` sections), each a titled box of
//! [`Item`]s, one control per row as in upstream's `SwiftUI` forms, the label leading and the control trailing; a hint
//! is a caption row under the control it explains. A page describes its controls from the snapshot (what each shows,
//! whether it takes input, the edit each new value makes), so a test reads the page as the popup draws it
//! (`super::draw`) and drives an edit without a pointer.

use std::ops::RangeInclusive;
use std::rc::Rc;

use agterm_core::model::AgentStatus;
use agterm_core::persist::AppSettings;
use agterm_core::settings_model::{
    AppearanceChange, BLUR_RANGE, GeneralChange, SCROLL_SPEED_RANGE, STRENGTH_RANGE, SettingsChange,
    sidebar_shift_label,
};

use crate::folder_chooser::FolderRequest;

/// An edit made from a control's new value.
pub type Setter<T> = Rc<dyn Fn(T) -> SettingsChange>;

/// One page section: an optional title over a box of rows, laid out one per row, or two per row (upstream's Interface
/// tab).
pub struct Section {
    pub title: Option<&'static str>,
    pub items: Vec<Item>,
    pub columns: usize,
}

impl Section {
    #[must_use]
    pub fn new(title: &'static str, items: Vec<Item>) -> Self {
        Self {
            title: Some(title),
            items,
            columns: 1,
        }
    }

    /// A section with no title (upstream's untitled `Section`, Reset to defaults).
    #[must_use]
    pub fn untitled(items: Vec<Item>) -> Self {
        Self {
            title: None,
            items,
            columns: 1,
        }
    }

    /// Lays the rows out two per row.
    #[must_use]
    pub fn two_columns(mut self) -> Self {
        self.columns = 2;
        self
    }
}

/// One control row.
pub struct Item {
    /// The control's element id (upstream's accessibility identifier).
    pub id: String,
    /// Its label; a stepper's carries its value (upstream's `Text("Font size: \(…)")`).
    pub title: String,
    pub control: Control,
    /// Whether it takes input (upstream's `.disabled(…)`).
    pub enabled: bool,
}

impl Item {
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>, control: Control) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            control,
            enabled: true,
        }
    }

    /// Takes input only while `enabled` holds.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// A caption under the control above it (upstream's `SettingHint`); `id` names it when upstream does.
    #[must_use]
    pub fn hint(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(id, text, Control::Hint)
    }
}

/// What a row's control is, and what it shows now.
pub enum Control {
    /// A toggle (upstream's `Toggle`), on or off.
    Toggle { on: bool, set: Setter<bool> },
    /// A popup picker (upstream's `Picker`).
    Picker(Picker),
    /// A stepper (upstream's `Stepper`): its value and bounds, stepping by one.
    Stepper {
        value: f64,
        range: RangeInclusive<f64>,
        set: Setter<f64>,
    },
    /// A slider (upstream's `Slider` with its trailing readout); its position lives in the popup's slider state.
    Slider {
        key: SliderKey,
        value: f64,
        readout: String,
    },
    /// A path (upstream's monospaced, selectable `Text`; not selectable here), dimmed when nothing is set, with buttons
    /// after it.
    Path {
        text: String,
        unset: bool,
        buttons: Vec<Action>,
    },
    /// An agent status's glyph row: its color well and its shape picker (upstream's `glyphRow`).
    /// The well's color lives in the popup's color picker state, the shapes' tint in [`Picker::shapes`].
    Glyph { status: AgentStatus, shape: Picker },
    /// A file's diagnostics, one line each, "No issues." when there are none.
    Diagnostics { lines: Vec<String> },
    /// A push button.
    Button(Action),
    /// A caption: the row's title is the text.
    Hint,
}

/// A picker's rows and the one shown.
pub struct Picker {
    pub options: Vec<String>,
    /// The row equal to the stored value; none when the value is not offered.
    pub selected: Option<usize>,
    set: Rc<dyn Fn(usize) -> Option<SettingsChange>>,
    /// Picking a row past the first ("None") previews it as a sound (upstream's sound pickers).
    pub previews_sound: bool,
    /// The rows are status shapes, drawn as their silhouettes in this tint (upstream's shape picker).
    pub shapes: Option<crate::status_glyph::Rgb>,
}

impl Picker {
    /// A picker over labelled `options`, showing the one equal to `current`; a pick makes `set`'s edit.
    #[must_use]
    pub fn new<T: Clone + PartialEq + 'static>(
        options: Vec<(String, T)>,
        current: &T,
        set: impl Fn(T) -> SettingsChange + 'static,
    ) -> Self {
        let selected = options.iter().position(|(_, value)| value == current);
        let (labels, values): (Vec<String>, Vec<T>) = options.into_iter().unzip();
        Self {
            options: labels,
            selected,
            set: Rc::new(move |index| values.get(index).cloned().map(&set)),
            previews_sound: false,
            shapes: None,
        }
    }

    /// The edit picking row `index` makes; `None` past the rows.
    #[must_use]
    pub fn pick(&self, index: usize) -> Option<SettingsChange> {
        (self.set)(index)
    }

    /// The shown row's label, empty when none is.
    #[must_use]
    pub fn label(&self) -> &str {
        self.selected
            .and_then(|index| self.options.get(index))
            .map_or("", String::as_str)
    }
}

/// What a button does.
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
    pub run: Run,
}

/// A button's work.
#[derive(Clone)]
pub enum Run {
    /// One edit.
    Change(SettingsChange),
    /// A core call (the Reloads).
    Core(fn(&mut agterm_core::Core)),
    /// A directory chooser whose pick makes the edit.
    Choose {
        request: FolderRequest,
        change: fn(String) -> SettingsChange,
    },
}

/// The sliders, each with a state the popup keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderKey {
    ScrollSpeed,
    BackgroundOpacity,
    BackgroundBlur,
    SidebarShift,
    InactivePaneMute,
}

impl SliderKey {
    pub const ALL: [Self; 5] = [
        Self::ScrollSpeed,
        Self::BackgroundOpacity,
        Self::BackgroundBlur,
        Self::SidebarShift,
        Self::InactivePaneMute,
    ];

    /// The slider's element id.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::ScrollSpeed => "settings-scroll-speed",
            Self::BackgroundOpacity => "settings-bg-opacity",
            Self::BackgroundBlur => "settings-bg-blur",
            Self::SidebarShift => "settings-sidebar-shift",
            Self::InactivePaneMute => "settings-inactive-pane-mute",
        }
    }

    /// The slider's bounds, the core's ranges its edits are clamped to: whole steps, the opacity in percent.
    #[must_use]
    pub fn bounds(self) -> (f64, f64) {
        let whole = |range: &RangeInclusive<i64>| (whole_f64(*range.start()), whole_f64(*range.end()));
        match self {
            Self::ScrollSpeed => (*SCROLL_SPEED_RANGE.start(), *SCROLL_SPEED_RANGE.end()),
            Self::BackgroundOpacity => (0.0, 100.0),
            Self::BackgroundBlur => whole(&BLUR_RANGE),
            Self::SidebarShift | Self::InactivePaneMute => whole(&STRENGTH_RANGE),
        }
    }

    /// The position `settings` put it at.
    #[must_use]
    pub fn value(self, settings: &AppSettings) -> f64 {
        match self {
            Self::ScrollSpeed => settings.effective_scroll_speed(),
            Self::BackgroundOpacity => (settings.background_opacity.unwrap_or(1.0) * 100.0).round(),
            Self::BackgroundBlur => whole_f64(settings.background_blur.unwrap_or(0)),
            Self::SidebarShift => whole_f64(settings.effective_sidebar_shift()),
            Self::InactivePaneMute => whole_f64(settings.effective_inactive_pane_mute()),
        }
    }

    /// The readout beside it.
    #[must_use]
    pub fn readout(self, settings: &AppSettings) -> String {
        match self {
            Self::ScrollSpeed => format!("{}x", settings.effective_scroll_speed()),
            Self::BackgroundOpacity => format!("{}%", self.value(settings)),
            Self::BackgroundBlur => settings.background_blur.unwrap_or(0).to_string(),
            Self::SidebarShift => sidebar_shift_label(settings.effective_sidebar_shift()),
            Self::InactivePaneMute => settings.effective_inactive_pane_mute().to_string(),
        }
    }

    /// The edit a move to `position` makes.
    #[must_use]
    pub fn change(self, position: f64) -> SettingsChange {
        match self {
            Self::ScrollSpeed => SettingsChange::General(GeneralChange::ScrollSpeed(position)),
            Self::BackgroundOpacity => {
                SettingsChange::Appearance(AppearanceChange::BackgroundOpacity(position.round() / 100.0))
            }
            Self::BackgroundBlur => SettingsChange::Appearance(AppearanceChange::BackgroundBlur(whole(position))),
            Self::SidebarShift => SettingsChange::Appearance(AppearanceChange::SidebarBackgroundShift(whole(position))),
            Self::InactivePaneMute => SettingsChange::Appearance(AppearanceChange::InactivePaneMute(whole(position))),
        }
    }

    /// Its row, titled `title`.
    #[must_use]
    pub fn item(self, title: &str, settings: &AppSettings) -> Item {
        Item::new(
            self.id(),
            title,
            Control::Slider {
                key: self,
                value: self.value(settings),
                readout: self.readout(settings),
            },
        )
    }
}

/// A whole number as a slider position.
fn whole_f64(value: i64) -> f64 {
    i32::try_from(value).map_or(0.0, f64::from)
}

/// A slider position as a whole number; the sliders move in whole steps within 0...100.
#[must_use]
pub fn whole(position: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a slider position, rounded and within 0...100"
    )]
    let whole = position.round() as i64;
    whole
}

/// A toggle row.
#[must_use]
pub fn toggle(id: &str, title: &str, on: bool, set: impl Fn(bool) -> SettingsChange + 'static) -> Item {
    Item::new(id, title, Control::Toggle { on, set: Rc::new(set) })
}

/// A picker row over labelled `options`.
#[must_use]
pub fn picker<T: Clone + PartialEq + 'static>(
    id: &str,
    title: &str,
    options: &[(&str, T)],
    current: &T,
    set: impl Fn(T) -> SettingsChange + 'static,
) -> Item {
    let options = options
        .iter()
        .map(|(label, value)| ((*label).to_owned(), value.clone()))
        .collect();
    Item::new(id, title, Control::Picker(Picker::new(options, current, set)))
}

/// A stepper row titled `title: value`.
#[must_use]
pub fn stepper(
    id: &str,
    title: &str,
    value: f64,
    range: RangeInclusive<f64>,
    set: impl Fn(f64) -> SettingsChange + 'static,
) -> Item {
    Item::new(
        id,
        format!("{title}: {value}"),
        Control::Stepper {
            value,
            range,
            set: Rc::new(set),
        },
    )
}
