//! The plugin list with its on/off switches, and the selected plugin's settings.

use eframe::egui::{self, RichText, ScrollArea, TextEdit, Ui};

use super::edit::{self, Edit};
use super::setting_row;
use crate::plugins::Plugin;
use crate::project::Project;
use crate::settings_file::SettingsFile;

/// The list on the left: one row per plugin, with its switch. Returns the plugin clicked.
pub fn list(ui: &mut Ui, project: &mut Project, selected: usize) -> Option<usize> {
    let mut clicked = None;
    let loader_on = project.loader.as_ref().map_or(true, |l| l.value.enabled());
    ScrollArea::vertical().show(ui, |ui| {
        for (index, plugin) in project.plugins.iter().enumerate() {
            ui.horizontal(|ui| {
                switch(ui, project_loader(&mut project.loader), plugin, loader_on);
                let mut text = RichText::new(plugin.title());
                if plugin.schema.is_none() {
                    text = text.italics();
                }
                if ui.selectable_label(index == selected, text).clicked() {
                    clicked = Some(index);
                }
            });
        }
        if !project.asi.is_empty() {
            ui.separator();
            ui.label(RichText::new(".asi files (loader options)").weak());
            for asi in &project.asi {
                ui.label(RichText::new(asi).monospace().weak());
            }
        }
    });
    clicked
}

type LoaderDoc = Result<crate::project::Doc<crate::loader_file::LoaderFile>, String>;

fn project_loader(loader: &mut LoaderDoc) -> Option<&mut crate::loader_file::LoaderFile> {
    loader.as_mut().ok().map(|doc| &mut doc.value)
}

/// The on/off switch of one plugin: the loader's disable list, by DLL file stem.
fn switch(
    ui: &mut Ui,
    loader: Option<&mut crate::loader_file::LoaderFile>,
    plugin: &Plugin,
    loader_on: bool,
) {
    let (Some(stem), Some(loader)) = (&plugin.stem, loader) else {
        let hint = if plugin.builtin() {
            "The loader itself: switch it off in Loader options."
        } else if plugin.stem.is_none() {
            "Its DLL is no longer in the plugins folder."
        } else {
            "loader.toml cannot be read, see Loader options."
        };
        ui.add_enabled(
            false,
            egui::Checkbox::without_text(&mut plugin.builtin().clone()),
        )
        .on_disabled_hover_text(hint);
        return;
    };
    let mut on = !loader.is_disabled(stem);
    let hint = if loader_on {
        format!("Load {stem}.dll when the game starts.")
    } else {
        "The loader is switched off, so no plugin loads.".to_owned()
    };
    if ui
        .add(egui::Checkbox::without_text(&mut on))
        .on_hover_text(hint)
        .changed()
    {
        loader.set_disabled(stem, !on);
    }
}

/// The selected plugin: what it is, and its settings.
pub fn detail(ui: &mut Ui, edit: &mut Edit, project: &mut Project, selected: usize) {
    let Some(plugin) = project.plugins.get(selected) else {
        ui.label("No plugins found in the plugins folder.");
        return;
    };
    about(ui, plugin);
    let file = project.settings.get_mut(&plugin.name);
    let file = match file {
        Some(Ok(doc)) => &mut doc.value,
        Some(Err(error)) => {
            ui.colored_label(ui.visuals().error_fg_color, error.as_str());
            ui.label("Fix or delete the file to edit these settings here.");
            return;
        }
        None => return,
    };
    ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| settings(ui, edit, plugin, file));
}

fn about(ui: &mut Ui, plugin: &Plugin) {
    ui.horizontal_wrapped(|ui| {
        ui.heading(plugin.title());
        if let Some(schema) = &plugin.schema {
            ui.label(RichText::new(format!("v{}", schema.version)).weak());
            if !schema.file.is_empty() {
                ui.label(RichText::new(&schema.file).monospace().weak());
            }
        }
    });
    if let Some(description) = plugin.schema.as_ref().map(|s| &s.description) {
        if !description.is_empty() {
            ui.label(description);
        }
    }
    if plugin.stem.is_none() && !plugin.builtin() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Its DLL is no longer in the plugins folder; these are its settings from the last time it ran.",
        );
    }
    ui.separator();
}

fn settings(ui: &mut Ui, edit: &mut Edit, plugin: &Plugin, file: &mut SettingsFile) {
    let Some(schema) = &plugin.schema else {
        ui.label(
            "This plugin has not run with this loader yet, so its settings are not known. Start \
             the game once with it switched on; until then its stored values can be edited as text.",
        );
        others(ui, edit, &plugin.name, &[], file);
        return;
    };
    if schema.settings.is_empty() {
        ui.label(RichText::new("No settings.").weak());
    }
    let hidden = schema.settings.iter().filter(|s| s.desc.advanced).count();
    if hidden > 0 {
        ui.checkbox(
            &mut edit.show_advanced,
            format!(
                "Show {hidden} advanced setting{}",
                if hidden == 1 { "" } else { "s" }
            ),
        );
    }
    for setting in &schema.settings {
        if setting.desc.advanced && !edit.show_advanced {
            continue;
        }
        ui.push_id(&setting.desc.key, |ui| {
            setting_row::show(ui, edit, &plugin.name, setting, file);
        });
    }
    let known: Vec<&str> = schema
        .settings
        .iter()
        .map(|s| s.desc.key.as_str())
        .collect();
    others(ui, edit, &plugin.name, &known, file);
}

/// Stored keys no setting describes, as plain text, and a row to add one.
fn others(ui: &mut Ui, edit: &mut Edit, plugin: &str, known: &[&str], file: &mut SettingsFile) {
    let extra: Vec<String> = file
        .keys()
        .into_iter()
        .filter(|k| !known.contains(&k.as_str()))
        .collect();
    if !extra.is_empty() {
        ui.label(RichText::new("Other stored values").strong());
        if !known.is_empty() {
            ui.label(
                RichText::new("No setting of the plugin has these names; the plugin ignores them.")
                    .weak(),
            );
        }
    }
    for key in extra {
        let id = format!("{plugin}.{key}#raw");
        ui.horizontal(|ui| {
            ui.label(RichText::new(&key).monospace());
            let current = file.get(&key).unwrap_or_default();
            let draft = edit.draft(&id, || current.clone());
            if ui
                .add(TextEdit::singleline(draft).desired_width(240.0))
                .changed()
            {
                let text = draft.clone();
                file.set(&key, &text);
            }
            if ui.small_button("Remove").clicked() {
                file.remove(&key);
                edit.forget(&id);
            }
        });
    }
    if known.is_empty() {
        add_value(ui, edit, plugin, file);
    }
}

fn add_value(ui: &mut Ui, edit: &mut Edit, plugin: &str, file: &mut SettingsFile) {
    let key_id = format!("{plugin}#new-key");
    let value_id = format!("{plugin}#new-value");
    ui.horizontal(|ui| {
        let key = edit.draft(&key_id, String::new).clone();
        ui.add(
            TextEdit::singleline(edit.draft(&key_id, String::new))
                .hint_text("key")
                .desired_width(140.0),
        );
        ui.add(
            TextEdit::singleline(edit.draft(&value_id, String::new))
                .hint_text("value")
                .desired_width(200.0),
        );
        if ui.button("Add").clicked() {
            let key = key.trim().to_owned();
            let outcome = uniloader_core::names::setting_key(&key)
                .map_err(|e| format!("{e}."))
                .map(|()| Vec::new());
            if outcome.is_ok() {
                let value = edit.draft(&value_id, String::new).clone();
                file.set(&key, &value);
                edit.forget(&value_id);
                edit.forget(&key_id);
            }
            edit.outcome(&key_id, outcome);
        }
    });
    edit::messages(ui, edit, &key_id);
}
