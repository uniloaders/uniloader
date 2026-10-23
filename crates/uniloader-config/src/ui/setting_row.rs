//! One setting with the control its kind calls for, its description and default, a reset,
//! and the reason when a value is refused.

use eframe::egui::color_picker::{color_edit_button_srgba, Alpha};
use eframe::egui::{Color32, ComboBox, DragValue, RichText, Slider, TextEdit, Ui};
use uniloader_core::schema::Setting;
use uniloader_core::settings::{Desc, Kind};
use uniloader_core::values;

use super::edit::{self, Edit, Record};
use super::keycap::{self, Recorded};
use crate::settings_file::{self, SettingsFile};

/// Draw `setting` of `plugin` and apply what the user changes to `file`.
pub fn show(
    ui: &mut Ui,
    edit: &mut Edit,
    plugin: &str,
    setting: &Setting,
    file: &mut SettingsFile,
) {
    let desc = &setting.desc;
    let id = format!("{plugin}.{}", desc.key);
    let stored = file.get(&desc.key);
    let problem = stored
        .as_deref()
        .and_then(|s| settings_file::stored_problem(setting, s));
    let current = match (&stored, &problem) {
        (Some(value), None) => value.clone(),
        _ => desc.default.clone(),
    };

    header(ui, setting);
    let input = if desc.transient {
        ui.label(
            RichText::new("Changed in game only; never saved, so there is nothing to edit here.")
                .weak(),
        );
        None
    } else if desc.secret {
        secret(ui, edit, &id, setting, &current)
    } else {
        control(ui, edit, &id, setting, &current)
    };
    if let Some(input) = input {
        let outcome = settings_file::prepare(setting, &input);
        if let Ok(value) = &outcome {
            file.set(&desc.key, value);
        }
        edit.outcome(&id, outcome.map(|_| Vec::new()));
    }
    edit::messages(ui, edit, &id);
    if let Some(problem) = problem {
        ui.colored_label(ui.visuals().warn_fg_color, problem);
    }
    if !desc.transient {
        footer(ui, edit, &id, setting, stored.is_some(), file);
    }
    ui.separator();
}

fn header(ui: &mut Ui, setting: &Setting) {
    let desc = &setting.desc;
    ui.horizontal_wrapped(|ui| {
        let title = if desc.title.is_empty() {
            &desc.key
        } else {
            &desc.title
        };
        ui.label(RichText::new(title).strong());
        ui.label(RichText::new(&desc.key).monospace().weak());
        let tag = |ui: &mut Ui, text: &str, hint: &str| {
            ui.label(RichText::new(text).small().weak())
                .on_hover_text(hint);
        };
        if desc.advanced {
            tag(ui, "advanced", "Rarely needs changing.");
        }
        if desc.restart_required {
            tag(ui, "restart", "The plugin reads it at start only.");
        }
        if desc.secret {
            tag(ui, "secret", "Never shown; only whether it is set.");
        }
    });
    if !desc.description.is_empty() {
        ui.label(RichText::new(&desc.description).weak());
    }
}

fn footer(
    ui: &mut Ui,
    edit: &mut Edit,
    id: &str,
    setting: &Setting,
    stored: bool,
    file: &mut SettingsFile,
) {
    ui.horizontal(|ui| {
        let default = settings_file::shown(setting, &setting.desc.default);
        let default = if default.is_empty() {
            "(empty)"
        } else {
            &default
        };
        ui.label(RichText::new(format!("Default: {default}")).small().weak());
        let reset = ui
            .add_enabled(stored, eframe::egui::Button::new("Reset").small())
            .on_hover_text("Remove the stored value; the plugin uses its default.")
            .on_disabled_hover_text("Nothing stored: the plugin uses its default.");
        if reset.clicked() {
            file.remove(&setting.desc.key);
            edit.forget(id);
        }
    });
}

/// The control for a value that may be shown. Returns the user's input when it changed.
fn control(
    ui: &mut Ui,
    edit: &mut Edit,
    id: &str,
    setting: &Setting,
    current: &str,
) -> Option<String> {
    let desc = &setting.desc;
    let bounded = desc.min < desc.max;
    match desc.kind {
        Kind::Bool => {
            let mut on = current == "true";
            let label = if on { "On" } else { "Off" };
            ui.checkbox(&mut on, label)
                .changed()
                .then(|| on.to_string())
        }
        Kind::Int => {
            let mut n: i64 = current.parse().unwrap_or_default();
            // The bounds are whole numbers for an integer setting; f64 -> i64 saturates.
            #[allow(clippy::cast_possible_truncation)]
            let (min, max) = (desc.min as i64, desc.max as i64);
            let response = if bounded {
                ui.add(Slider::new(&mut n, min..=max))
            } else {
                ui.add(DragValue::new(&mut n))
            };
            response.changed().then(|| n.to_string())
        }
        Kind::Float => {
            let mut n: f64 = current.parse().unwrap_or_default();
            let response = if bounded {
                ui.add(Slider::new(&mut n, desc.min..=desc.max))
            } else {
                ui.add(DragValue::new(&mut n).speed(0.01))
            };
            response.changed().then(|| n.to_string())
        }
        Kind::Enum => {
            let mut picked = current.to_owned();
            ComboBox::from_id_salt(id)
                .selected_text(desc.label(current))
                .show_ui(ui, |ui| {
                    for choice in &desc.choices {
                        ui.selectable_value(&mut picked, choice.clone(), desc.label(choice));
                    }
                });
            (picked != current).then_some(picked)
        }
        Kind::Flags => flags(ui, desc, current),
        Kind::Colour => colour(ui, edit, id, current),
        Kind::Key => key(ui, edit, id, current),
        Kind::File | Kind::Directory => {
            let browse = ui
                .horizontal(|ui| {
                    let typed = text(ui, edit, id, current, "");
                    let browsed = ui
                        .button("Browse…")
                        .clicked()
                        .then(|| pick(desc.kind, &desc.choices))
                        .flatten();
                    browsed.or(typed)
                })
                .inner;
            if browse.is_some() {
                edit.forget(id);
            }
            browse
        }
        Kind::Date => text(ui, edit, id, current, "YYYY-MM-DD"),
        Kind::Time => text(ui, edit, id, current, "HH:MM"),
        Kind::DateTime => text(ui, edit, id, current, "YYYY-MM-DD HH:MM"),
        Kind::String => text(ui, edit, id, current, ""),
    }
}

/// A text field holding a draft; returns the draft whenever it changed, so the value is
/// taken as soon as it is valid and the error says why while it is not.
fn text(ui: &mut Ui, edit: &mut Edit, id: &str, current: &str, hint: &str) -> Option<String> {
    let draft = edit.draft(id, || current.to_owned());
    let response = ui.add(
        TextEdit::singleline(draft)
            .hint_text(hint)
            .desired_width(280.0),
    );
    response.changed().then(|| draft.clone())
}

fn flags(ui: &mut Ui, desc: &Desc, current: &str) -> Option<String> {
    let choices = &desc.choices;
    let set: Vec<&str> = values::flag_names(current).collect();
    let mut changed = None;
    ui.horizontal_wrapped(|ui| {
        for choice in choices {
            let mut on = set.iter().any(|s| s.eq_ignore_ascii_case(choice));
            if ui.checkbox(&mut on, desc.label(choice)).changed() {
                let next: Vec<&str> = choices
                    .iter()
                    .map(String::as_str)
                    .filter(|c| {
                        if *c == choice {
                            on
                        } else {
                            set.iter().any(|s| s.eq_ignore_ascii_case(c))
                        }
                    })
                    .collect();
                changed = Some(next.join(","));
            }
        }
    });
    changed
}

fn colour(ui: &mut Ui, edit: &mut Edit, id: &str, current: &str) -> Option<String> {
    let [r, g, b, a] = values::colour_rgba(current).unwrap_or([255; 4]);
    let mut picked = Color32::from_rgba_unmultiplied(r, g, b, a);
    ui.horizontal(|ui| {
        let swatch = color_edit_button_srgba(ui, &mut picked, Alpha::OnlyBlend).changed();
        let typed = text(ui, edit, id, current, "#rrggbb");
        if swatch {
            edit.forget(id);
            return Some(values::colour_text(picked.to_srgba_unmultiplied()));
        }
        typed
    })
    .inner
}

fn key(ui: &mut Ui, edit: &mut Edit, id: &str, current: &str) -> Option<String> {
    let target = Record::Setting(id.to_owned());
    let recording = edit.is_recording(&target);
    let mut input = None;
    ui.horizontal(|ui| {
        if keycap::button(ui, current, recording) {
            edit.toggle_recording(target.clone());
        }
        if let Some(typed) = text(ui, edit, id, current, "f8, ctrl+shift+c") {
            input = Some(typed);
        }
    });
    if recording {
        ui.label(RichText::new(keycap::HINT).small().weak());
        match keycap::poll(ui) {
            Recorded::Waiting => {}
            Recorded::Cancelled => edit.recording = None,
            Recorded::Bound(binding) => {
                edit.recording = None;
                edit.forget(id);
                input = Some(binding);
            }
        }
    }
    input
}

/// A secret: `<set>` or `<unset>`, a masked field for a new value, and Clear.
fn secret(
    ui: &mut Ui,
    edit: &mut Edit,
    id: &str,
    setting: &Setting,
    current: &str,
) -> Option<String> {
    let mut input = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(settings_file::shown(setting, current)).monospace());
        let draft = edit.draft(id, String::new);
        let field = ui.add(
            TextEdit::singleline(draft)
                .password(true)
                .hint_text("new value")
                .desired_width(200.0),
        );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Enter));
        let set = ui.add_enabled(!draft.is_empty(), eframe::egui::Button::new("Set"));
        if (set.clicked() || enter) && !draft.is_empty() {
            input = Some(std::mem::take(draft));
        }
        if ui
            .add_enabled(!current.is_empty(), eframe::egui::Button::new("Clear"))
            .clicked()
        {
            draft.clear();
            input = Some(String::new());
        }
    });
    if setting.hashed.is_some() {
        ui.label(
            RichText::new("Stored as a hash: the loader can check it but nobody can read it back.")
                .small()
                .weak(),
        );
    }
    input
}

/// A native picker for a file (offering `extensions`) or a folder, as the game sees paths.
fn pick(kind: Kind, extensions: &[String]) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let path = if kind == Kind::Directory {
        dialog.pick_folder()
    } else if extensions.is_empty() {
        dialog.pick_file()
    } else {
        dialog.add_filter("Files", extensions).pick_file()
    }?;
    Some(game_path(&path))
}

/// A path as a game running under Wine or Proton opens it: the host's `/` is drive `Z:`.
fn game_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        text.into_owned()
    } else {
        format!("Z:{}", text.replace('/', "\\"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_paths_become_wine_paths() {
        let shown = super::game_path(std::path::Path::new("/home/me/a b/x.png"));
        if cfg!(windows) {
            assert_eq!(shown, "/home/me/a b/x.png");
        } else {
            assert_eq!(shown, "Z:\\home\\me\\a b\\x.png");
        }
    }
}
