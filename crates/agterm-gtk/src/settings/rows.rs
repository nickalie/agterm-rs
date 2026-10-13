//! The dialog's control rows in libadwaita idioms: a switch row for a toggle, a combo row for a picker, a spin row for a
//! stepper, an action row carrying a scale for a slider. Each row posts its edit and registers how it follows the
//! settings; its widget name is upstream's accessibility identifier.

use std::cell::RefCell;
use std::ops::RangeInclusive;
use std::rc::Rc;

use adw::prelude::*;
use agterm_core::settings_model::SettingsChange;

use super::{Rows, Snapshot, refreshing, send};

/// A preferences page named `name` (the page's id), titled and iconed.
#[must_use]
pub fn page(title: &str, name: &str, icon: &str) -> adw::PreferencesPage {
    adw::PreferencesPage::builder()
        .title(title)
        .name(name)
        .icon_name(icon)
        .build()
}

/// A titled group.
#[must_use]
pub fn group(title: &str) -> adw::PreferencesGroup {
    adw::PreferencesGroup::builder().title(title).build()
}

/// A toggle.
pub fn switch(
    rows: &mut Rows,
    id: &str,
    title: &str,
    get: impl Fn(&Snapshot) -> bool + 'static,
    set: impl Fn(bool) -> SettingsChange + 'static,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).name(id).build();
    let poster = rows.poster().clone();
    row.connect_active_notify(move |row| {
        if !refreshing() {
            send(&poster, set(row.is_active()));
        }
    });
    let shown = row.clone();
    rows.refresh(move |snapshot| shown.set_active(get(snapshot)));
    row
}

/// A picker over options a snapshot lists, which may change with it (the theme pickers drop or gain their default
/// row); the selection is the option equal to `current`, none when it is not offered.
pub fn choice<T: Clone + PartialEq + 'static>(
    rows: &mut Rows,
    id: &str,
    title: &str,
    options: impl Fn(&Snapshot) -> Vec<(String, T)> + 'static,
    current: impl Fn(&Snapshot) -> T + 'static,
    set: impl Fn(T) -> SettingsChange + 'static,
) -> adw::ComboRow {
    let model = gtk::StringList::new(&[]);
    let row = adw::ComboRow::builder().title(title).name(id).model(&model).build();
    row.set_expression(Some(gtk::PropertyExpression::new(
        gtk::StringObject::static_type(),
        None::<gtk::Expression>,
        "string",
    )));
    let offered: Rc<RefCell<Vec<(String, T)>>> = Rc::default();
    let poster = rows.poster().clone();
    let picked = Rc::clone(&offered);
    row.connect_selected_notify(move |row| {
        if refreshing() {
            return;
        }
        let value = picked
            .borrow()
            .get(row.selected() as usize)
            .map(|(_, value)| value.clone());
        if let Some(value) = value {
            send(&poster, set(value));
        }
    });
    let shown = row.clone();
    rows.refresh(move |snapshot| {
        let fresh = options(snapshot);
        if *offered.borrow() != fresh {
            let labels: Vec<&str> = fresh.iter().map(|(label, _)| label.as_str()).collect();
            model.splice(0, model.n_items(), &labels);
            shown.set_enable_search(fresh.len() > 10);
            *offered.borrow_mut() = fresh;
        }
        let selected = current(snapshot);
        let index = offered
            .borrow()
            .iter()
            .position(|(_, value)| *value == selected)
            .and_then(|index| u32::try_from(index).ok())
            .unwrap_or(gtk::INVALID_LIST_POSITION);
        shown.set_selected(index);
    });
    row
}

/// A picker over a fixed list of labelled values.
pub fn picker<T: Clone + PartialEq + 'static>(
    rows: &mut Rows,
    id: &str,
    title: &str,
    options: &[(&str, T)],
    current: impl Fn(&Snapshot) -> T + 'static,
    set: impl Fn(T) -> SettingsChange + 'static,
) -> adw::ComboRow {
    let options: Vec<(String, T)> = options
        .iter()
        .map(|(label, value)| ((*label).to_owned(), value.clone()))
        .collect();
    choice(rows, id, title, move |_| options.clone(), current, set)
}

/// A stepper over `range` in whole steps.
pub fn stepper(
    rows: &mut Rows,
    id: &str,
    title: &str,
    range: &RangeInclusive<f64>,
    get: impl Fn(&Snapshot) -> f64 + 'static,
    set: impl Fn(f64) -> SettingsChange + 'static,
) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(*range.start(), *range.end(), 1.0);
    row.set_title(title);
    row.set_widget_name(id);
    let poster = rows.poster().clone();
    row.connect_value_notify(move |row| {
        if !refreshing() {
            send(&poster, set(row.value()));
        }
    });
    let shown = row.clone();
    rows.refresh(move |snapshot| shown.set_value(get(snapshot)));
    row
}

/// A slider over `range` in whole steps, with a readout of the stored value beside it.
pub struct Slider<G, L, S> {
    pub range: RangeInclusive<f64>,
    pub get: G,
    pub label: L,
    pub set: S,
}

/// A slider row (upstream's `Slider` with its trailing value text).
pub fn slider<G, L, S>(rows: &mut Rows, id: &str, title: &str, slider: Slider<G, L, S>) -> adw::ActionRow
where
    G: Fn(&Snapshot) -> f64 + 'static,
    L: Fn(&Snapshot) -> String + 'static,
    S: Fn(f64) -> SettingsChange + 'static,
{
    let Slider { range, get, label, set } = slider;
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, *range.start(), *range.end(), 1.0);
    scale.set_round_digits(0);
    scale.set_draw_value(false);
    scale.set_hexpand(true);
    scale.set_width_request(180);
    scale.set_valign(gtk::Align::Center);
    scale.set_widget_name(id);
    let readout = gtk::Label::builder().width_chars(9).xalign(1.0).build();
    readout.add_css_class("numeric");
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(&scale);
    row.add_suffix(&readout);
    let poster = rows.poster().clone();
    scale.connect_value_changed(move |scale| {
        if !refreshing() {
            send(&poster, set(scale.value()));
        }
    });
    rows.refresh(move |snapshot| {
        scale.set_value(get(snapshot));
        readout.set_label(&label(snapshot));
    });
    row
}

/// A row whose subtitle follows the settings.
pub fn info(
    rows: &mut Rows,
    id: &str,
    title: &str,
    subtitle: impl Fn(&Snapshot) -> String + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).name(id).build();
    row.set_subtitle_selectable(true);
    let shown = row.clone();
    rows.refresh(move |snapshot| shown.set_subtitle(&glib_escape(&subtitle(snapshot))));
    row
}

/// A button for a row's suffix.
#[must_use]
pub fn button(label: &str, id: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .name(id)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button
}

/// Shows `widget` only while `visible` holds.
pub fn show_when(rows: &mut Rows, widget: &impl IsA<gtk::Widget>, visible: impl Fn(&Snapshot) -> bool + 'static) {
    let widget = widget.clone().upcast::<gtk::Widget>();
    rows.refresh(move |snapshot| widget.set_visible(visible(snapshot)));
}

/// Lets `widget` take input only while `enabled` holds.
pub fn enable_when(rows: &mut Rows, widget: &impl IsA<gtk::Widget>, enabled: impl Fn(&Snapshot) -> bool + 'static) {
    let widget = widget.clone().upcast::<gtk::Widget>();
    rows.refresh(move |snapshot| widget.set_sensitive(enabled(snapshot)));
}

/// A row title or subtitle is Pango markup: user text is escaped.
#[must_use]
pub fn glib_escape(text: &str) -> String {
    gtk::glib::markup_escape_text(text).to_string()
}

/// Lets the user pick a directory, starting at `initial`, then hands it on (upstream's `NSOpenPanel` for
/// directories).
pub fn choose_directory(
    anchor: &gtk::Widget,
    title: &str,
    initial: Option<&str>,
    chosen: impl FnOnce(String) + 'static,
) {
    let dialog = gtk::FileDialog::builder()
        .title(title)
        .accept_label("Choose")
        .modal(true)
        .build();
    if let Some(initial) = initial.filter(|dir| !dir.is_empty()) {
        dialog.set_initial_folder(Some(&gtk::gio::File::for_path(initial)));
    }
    let parent = anchor.root().and_downcast::<gtk::Window>();
    dialog.select_folder(
        parent.as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |result| match result {
            Ok(folder) => {
                if let Some(path) = folder.path() {
                    chosen(path.to_string_lossy().into_owned());
                }
            }
            Err(err) => tracing::debug!(%err, "no directory chosen"),
        },
    );
}
