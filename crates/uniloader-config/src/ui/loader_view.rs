//! `loader.toml`: the loader's own switches.

use eframe::egui::{ComboBox, RichText, ScrollArea, TextEdit, Ui};

use super::edit::{self, Edit};
use crate::loader_file::{LoaderFile, LOG_LEVELS};
use crate::project::Project;

/// The loader options page.
pub fn show(ui: &mut Ui, edit: &mut Edit, project: &mut Project) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        if let Some(loader) = &project.loader_schema {
            ui.label(
                RichText::new(format!(
                    "Last run: uniloader {} in {}.",
                    loader.version, loader.game_exe
                ))
                .weak(),
            );
        }
        let file = match &mut project.loader {
            Ok(doc) => &mut doc.value,
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                ui.label(
                    "The loader ignores this file and runs on defaults. Fix it in a text editor.",
                );
                return;
            }
        };
        for problem in file.problems() {
            ui.colored_label(ui.visuals().warn_fg_color, problem);
        }
        switches(ui, file);
        lists(ui, edit, file);
        if !project.asi.is_empty() {
            ui.label(format!(
                ".asi files in the plugins folder: {}",
                project.asi.join(", ")
            ));
        }
    });
}

fn switches(ui: &mut Ui, file: &mut LoaderFile) {
    let mut enabled = file.enabled();
    if ui
        .checkbox(&mut enabled, "Load plugins")
        .on_hover_text("Off: the loader only forwards the system DLL it stands in for.")
        .changed()
    {
        file.set_bool("enabled", enabled);
    }
    let mut asi = file.asi();
    if ui
        .checkbox(&mut asi, "Load .asi files from the plugins folder")
        .changed()
    {
        file.set_bool("asi", asi);
    }
    ui.horizontal(|ui| {
        ui.label("Log level");
        let mut level = file.log_level();
        ComboBox::from_id_salt("log-level")
            .selected_text(&level)
            .show_ui(ui, |ui| {
                for choice in LOG_LEVELS {
                    ui.selectable_value(&mut level, (*choice).to_owned(), *choice);
                }
            });
        if level != file.log_level() {
            file.set_log_level(&level);
        }
    });
}

fn lists(ui: &mut Ui, edit: &mut Edit, file: &mut LoaderFile) {
    let lists = [
        (
            "load",
            "Also load these DLLs, relative to the game folder, one per line (for example \
             ue4ss/UE4SS.dll when the loader took the place of its proxy).",
        ),
        (
            "game_keys",
            "Keys the game itself uses, one per line; a bind on one is pointed out in the log.",
        ),
        (
            "disabled_plugins",
            "Plugins not loaded, by DLL name without .dll; the switches in Plugins edit this.",
        ),
    ];
    for (key, help) in lists {
        ui.add_space(8.0);
        ui.label(RichText::new(key).monospace().strong());
        ui.label(RichText::new(help).weak());
        let current = file.list(key).join("\n");
        let id = format!("loader:{key}");
        let draft = edit.draft(&id, || current.clone());
        if *draft != current && !ui.memory(|m| m.has_focus(ui.id().with(key))) {
            // Changed elsewhere (a plugin switch): show the file's list.
            draft.clone_from(&current);
        }
        let response = ui.add(
            TextEdit::multiline(draft)
                .id(ui.id().with(key))
                .desired_rows(2)
                .desired_width(400.0),
        );
        if response.changed() {
            let values: Vec<String> = draft
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            let outcome = if key == "game_keys" {
                values
                    .iter()
                    .try_for_each(|k| {
                        uniloader_core::keys::parse_list(k)
                            .map(|_| ())
                            .map_err(|e| format!("{e}."))
                    })
                    .map(|()| Vec::new())
            } else {
                Ok(Vec::new())
            };
            if outcome.is_ok() {
                file.set_list(key, &values);
            }
            edit.outcome(&id, outcome);
        }
        edit::messages(ui, edit, &id);
    }
}
