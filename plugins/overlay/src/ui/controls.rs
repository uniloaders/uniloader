//! Controls for the richer setting kinds: a key binding to record, a colour picker, a
//! calendar for dates, a dropdown of checkboxes for flag sets and a path with a browser.
//! Each returns the new value in the loader's text form when the user changed it.

use egui::color_picker::{color_edit_button_srgba, Alpha};
use egui::{Color32, ComboBox, DragValue, Popup, PopupCloseBehavior, RichText, TextEdit, Ui};
use uniloader_core::values;
use uniloader_sdk::{SettingEntry, SettingKind};

use super::browser;
use super::recorder::{self, Target};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `<plugin>.<key>`.
fn qualified(entry: &SettingEntry) -> String {
    format!("{}.{}", entry.plugin, entry.key)
}

/// A binding as a button, "click to change", with Clear beside it; `true` when Clear was
/// clicked. The key itself arrives through the [`recorder`].
pub(crate) fn binding_button(ui: &mut Ui, target: &Target, shown: &str) -> bool {
    ui.horizontal(|ui| {
        let recording = recorder::is(target);
        let text = if recording {
            RichText::new("press a key…").strong()
        } else if shown == "none" {
            RichText::new("none").monospace().weak()
        } else {
            RichText::new(shown).monospace()
        };
        let hint = if recording {
            recorder::HINT
        } else {
            "Click to change"
        };
        if ui.button(text).on_hover_text(hint).clicked() {
            recorder::toggle(target.clone());
        }
        let cleared = ui.small_button("Clear").clicked();
        if cleared {
            recorder::stop();
        }
        cleared
    })
    .inner
}

/// A key setting: its binding, recorded on click.
pub(crate) fn key(ui: &mut Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let cleared = binding_button(ui, &Target::Setting(qualified(entry)), &entry.value);
    ui.label(label);
    if recorder::is(&Target::Setting(qualified(entry))) {
        ui.label(RichText::new(recorder::HINT).weak().small());
    }
    (cleared && entry.value != "none").then(|| "none".to_owned())
}

/// A password or token: a masked field that starts empty, because the loader never hands the
/// value out, beside `<set>` or `<unset>`. Written when the field loses focus with text in it.
pub(crate) fn secret(ui: &mut Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let id = ui.id().with(("secret-draft", qualified(entry)));
    let mut draft: String = ui.data_mut(|d| d.get_temp(id)).unwrap_or_default();
    let response = ui
        .horizontal(|ui| {
            let response = ui.add(
                TextEdit::singleline(&mut draft)
                    .password(true)
                    .desired_width(140.0),
            );
            ui.label(RichText::new(&entry.value).monospace().weak());
            ui.label(label);
            response
        })
        .inner;
    if response.lost_focus() && !draft.is_empty() {
        ui.data_mut(|d| d.remove::<String>(id));
        return Some(draft);
    }
    ui.data_mut(|d| d.insert_temp(id, draft));
    None
}

/// A colour: a swatch that opens a picker. Written once the mouse is let go, not for every
/// step of a drag.
pub(crate) fn colour(ui: &mut Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let id = ui.id().with("colour-draft");
    let current = values::colour_rgba(&entry.value).unwrap_or([255; 4]);
    let draft: Option<[u8; 4]> = ui.data(|d| d.get_temp(id));
    let [r, g, b, a] = draft.unwrap_or(current);
    let mut picked = Color32::from_rgba_unmultiplied(r, g, b, a);
    let response = ui
        .horizontal(|ui| {
            let response = color_edit_button_srgba(ui, &mut picked, Alpha::OnlyBlend);
            ui.label(RichText::new(&entry.value).monospace());
            ui.label(label);
            response
        })
        .inner;
    if response.changed() {
        ui.data_mut(|d| d.insert_temp(id, picked.to_srgba_unmultiplied()));
    }
    if ui.input(|i| i.pointer.any_down()) {
        return None;
    }
    let done: Option<[u8; 4]> = ui.data_mut(|d| d.remove_temp(id));
    done.filter(|d| *d != current).map(values::colour_text)
}

/// A flag set: a dropdown of checkboxes, open until a click outside.
pub(crate) fn flags(ui: &mut Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let on: Vec<&str> = values::flag_names(&entry.value).collect();
    let shown = if on.is_empty() {
        "none".to_owned()
    } else {
        on.join(", ")
    };
    let mut changed = None;
    ComboBox::from_label(label)
        .selected_text(shown)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            for choice in &entry.choices {
                let mut checked = on.contains(&choice.as_str());
                if ui.checkbox(&mut checked, choice).changed() {
                    let next: Vec<&str> = entry
                        .choices
                        .iter()
                        .map(String::as_str)
                        .filter(|c| if c == choice { checked } else { on.contains(c) })
                        .collect();
                    changed = Some(next.join(","));
                }
            }
        });
    changed
}

/// A path: the text, and a button that opens the overlay's own file or folder browser (a
/// system dialog would stall the game's frame and open behind a full-screen game).
pub(crate) fn path(ui: &mut Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let folder = entry.kind == SettingKind::Directory;
    let mut text = entry.value.clone();
    ui.horizontal(|ui| {
        let response = ui.add(TextEdit::singleline(&mut text).desired_width(200.0));
        let hint = if folder {
            "Choose a folder"
        } else {
            "Choose a file"
        };
        if ui.small_button("…").on_hover_text(hint).clicked() {
            browser::open(qualified(entry), folder, &entry.choices, &entry.value);
        }
        ui.label(label);
        response
            .lost_focus()
            .then_some(text)
            .filter(|t| *t != entry.value)
    })
    .inner
}

/// A date, a time, or both: the date on a button that opens a calendar, the time as hour,
/// minute (and second) fields.
pub(crate) fn when(ui: &mut Ui, entry: &SettingEntry, label: &str) -> Option<String> {
    let (date_text, time_text) = match entry.kind {
        SettingKind::Date => (Some(entry.value.as_str()), None),
        SettingKind::Time => (None, Some(entry.value.as_str())),
        _ => match entry.value.split_once(' ') {
            Some((d, t)) => (Some(d), Some(t)),
            None => (Some(entry.value.as_str()), Some("00:00")),
        },
    };
    ui.horizontal(|ui| {
        let date = date_text.map(|d| values::date_parts(d).unwrap_or([2000, 1, 1]));
        let time = time_text.map(|t| values::time_parts(t).unwrap_or((0, 0, None)));
        let new_date = date.and_then(|d| date_button(ui, d));
        let new_time = time.and_then(|t| time_fields(ui, t));
        ui.label(label);
        if new_date.is_none() && new_time.is_none() {
            return None;
        }
        let d = new_date
            .or(date)
            .map(|[y, m, d]| format!("{y:04}-{m:02}-{d:02}"));
        let t = new_time.or(time).map(|(h, m, s)| match s {
            Some(s) => format!("{h:02}:{m:02}:{s:02}"),
            None => format!("{h:02}:{m:02}"),
        });
        match (d, t) {
            (Some(d), Some(t)) => Some(format!("{d} {t}")),
            (Some(v), None) | (None, Some(v)) => Some(v),
            (None, None) => None,
        }
    })
    .inner
}

/// The date on a button; the calendar under it while open. The picked date, when one was.
fn date_button(ui: &mut Ui, [y, m, d]: [u32; 3]) -> Option<[u32; 3]> {
    let response = ui.button(RichText::new(format!("{y:04}-{m:02}-{d:02}")).monospace());
    let shown_id = response.id.with("month");
    let mut picked = None;
    Popup::from_toggle_button_response(&response)
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let mut shown: (u32, u32) = ui.data(|data| data.get_temp(shown_id)).unwrap_or((y, m));
            picked = calendar(ui, &mut shown, [y, m, d]);
            ui.data_mut(|data| data.insert_temp(shown_id, shown));
            if picked.is_some() {
                ui.close();
            }
        });
    picked
}

/// One month of days, Monday first, with the month to either side a click away. The day
/// clicked, when one was.
fn calendar(ui: &mut Ui, shown: &mut (u32, u32), selected: [u32; 3]) -> Option<[u32; 3]> {
    let (year, month) = *shown;
    ui.horizontal(|ui| {
        if ui.small_button("<").clicked() {
            *shown = if month == 1 {
                (year.saturating_sub(1).max(1), 12)
            } else {
                (year, month - 1)
            };
        }
        let name = MONTHS
            .get(month.saturating_sub(1) as usize)
            .copied()
            .unwrap_or("");
        ui.label(RichText::new(format!("{name} {year}")).strong());
        if ui.small_button(">").clicked() {
            *shown = if month == 12 {
                ((year + 1).min(9999), 1)
            } else {
                (year, month + 1)
            };
        }
    });
    let mut picked = None;
    egui::Grid::new("calendar")
        .spacing([2.0, 2.0])
        .show(ui, |ui| {
            for day in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
                ui.label(RichText::new(day).weak().small());
            }
            ui.end_row();
            let first = values::weekday(year, month, 1);
            for _ in 0..first {
                ui.label("");
            }
            for day in 1..=values::days_in(year, month) {
                let is_selected = [year, month, day] == selected;
                let text = RichText::new(format!("{day:>2}")).monospace();
                if ui.selectable_label(is_selected, text).clicked() {
                    picked = Some([year, month, day]);
                }
                if (first + day).is_multiple_of(7) {
                    ui.end_row();
                }
            }
        });
    picked
}

/// Hour, minute and (when the value has them) second fields; the new time when one changed.
fn time_fields(ui: &mut Ui, (h, m, s): (u32, u32, Option<u32>)) -> Option<(u32, u32, Option<u32>)> {
    let (mut h, mut m, mut s) = (h, m, s);
    let mut changed = ui.add(DragValue::new(&mut h).range(0..=23)).changed();
    ui.label(":");
    changed |= ui.add(DragValue::new(&mut m).range(0..=59)).changed();
    if let Some(seconds) = s.as_mut() {
        ui.label(":");
        changed |= ui.add(DragValue::new(seconds).range(0..=59)).changed();
    }
    changed.then_some((h, m, s))
}
