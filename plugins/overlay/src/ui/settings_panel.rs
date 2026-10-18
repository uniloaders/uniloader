//! The settings editor: every plugin's settings and hotkeys in one window.
//!
//! Plugins get this for free: a setting registered because the plugin needs the value, or a
//! hotkey registered because it needs the action, shows up here with no UI code. Writes go
//! through the loader — `set` for a setting, `rebind` for a binding — so a slider here and a
//! typed console command cannot disagree.

use egui::{Context, Grid, RichText, ScrollArea};
use uniloader_sdk::{HotkeyEntry, SettingEntry};

use super::chrome::{self, Chrome, Placed};
use super::recorder::{self, Target};
use super::{controls, frame};

/// One plugin's settings and hotkeys.
pub(crate) struct Section {
    /// Plugin name.
    pub(crate) plugin: String,
    /// Whether it is running.
    pub(crate) running: bool,
    /// Its settings with their values.
    pub(crate) settings: Vec<SettingEntry>,
    /// Its hotkey actions.
    pub(crate) hotkeys: Vec<HotkeyEntry>,
}

/// Group the registries into sections, in the order the plugins were loaded.
pub(crate) fn sections(
    plugins: &[(String, bool)],
    settings: Vec<SettingEntry>,
    hotkeys: Vec<HotkeyEntry>,
) -> Vec<Section> {
    let mut out: Vec<Section> = plugins
        .iter()
        .map(|(plugin, running)| Section {
            plugin: plugin.clone(),
            running: *running,
            settings: Vec::new(),
            hotkeys: Vec::new(),
        })
        .collect();
    for setting in settings {
        if let Some(section) = out.iter_mut().find(|s| s.plugin == setting.plugin) {
            section.settings.push(setting);
        }
    }
    for hotkey in hotkeys {
        let owner = hotkey.name.split_once('.').map_or("", |(owner, _)| owner);
        if let Some(section) = out.iter_mut().find(|s| s.plugin == owner) {
            section.hotkeys.push(hotkey);
        }
    }
    out.retain(|s| !s.settings.is_empty() || !s.hotkeys.is_empty());
    out
}

/// Everything one frame of the editor is drawn from.
pub(crate) struct View<'a> {
    pub(crate) sections: &'a [Section],
    /// Whether advanced settings are shown.
    pub(crate) advanced: bool,
}

/// What one frame of the editor decided, applied by the caller after the frame.
#[derive(Default)]
pub(crate) struct Edits {
    /// Qualified setting name and its new value.
    pub(crate) settings: Vec<(String, String)>,
    /// Action name and its new binding in key grammar; empty unbinds.
    pub(crate) bindings: Vec<(String, String)>,
    /// Set when the user toggled advanced settings.
    pub(crate) advanced: Option<bool>,
}

/// Draw the window. It keeps nothing between frames: the binding being recorded is the
/// [`recorder`]'s.
pub(crate) fn show(ctx: &Context, view: &View<'_>, chrome: &Chrome<'_>) -> (Edits, Placed) {
    let mut edits = Edits::default();
    let placed = chrome::show(ctx, chrome, |ui| {
        let mut advanced = view.advanced;
        if ui
            .checkbox(&mut advanced, "Advanced settings")
            .on_hover_text("Show the settings plugins marked as rarely needed")
            .changed()
        {
            edits.advanced = Some(advanced);
        }
        ui.separator();
        ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for section in view.sections {
                    show_section(ui, section, view.advanced, &mut edits);
                }
                if view.sections.is_empty() {
                    ui.label("No plugin has registered a setting or a hotkey.");
                }
            });
    });
    (edits, placed)
}

/// One plugin: its settings, then its hotkeys.
fn show_section(ui: &mut egui::Ui, section: &Section, advanced: bool, edits: &mut Edits) {
    let heading = if section.running {
        RichText::new(&section.plugin).heading()
    } else {
        RichText::new(format!("{} (stopped)", section.plugin))
            .heading()
            .weak()
    };
    egui::CollapsingHeader::new(heading)
        .id_salt(&section.plugin)
        .default_open(true)
        .show(ui, |ui| {
            reset_all(ui, section, edits);
            let mut hidden = 0_usize;
            for entry in &section.settings {
                if entry.advanced && !advanced {
                    hidden += 1;
                    continue;
                }
                if let Some(new) = frame::row(ui, entry, &frame::label_of(entry)) {
                    edits
                        .settings
                        .push((format!("{}.{}", entry.plugin, entry.key), new));
                }
                if !entry.description.is_empty() {
                    ui.label(RichText::new(&entry.description).weak().small());
                }
            }
            if hidden > 0 {
                let plural = if hidden == 1 { "setting" } else { "settings" };
                ui.label(
                    RichText::new(format!("{hidden} advanced {plural} hidden"))
                        .weak()
                        .small(),
                );
            }
            if !section.settings.is_empty() && !section.hotkeys.is_empty() {
                ui.separator();
            }
            if !section.hotkeys.is_empty() {
                hotkeys(ui, section, edits);
            }
        });
}

/// The hotkey table: what it does, what it is bound to, and the buttons to change it.
fn hotkeys(ui: &mut egui::Ui, section: &Section, edits: &mut Edits) {
    Grid::new(format!("{}-hotkeys", section.plugin))
        .num_columns(3)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for row in &section.hotkeys {
                let title = if row.title.is_empty() {
                    row.name.as_str()
                } else {
                    row.title.as_str()
                };
                ui.label(title);
                let target = Target::Hotkey(row.name.clone());
                let shown = if row.binding.is_empty() {
                    "none"
                } else {
                    row.binding.as_str()
                };
                let cleared = controls::binding_button(ui, &target, shown);
                if cleared {
                    edits.bindings.push((row.name.clone(), String::new()));
                }
                let changed = row.binding != row.default_binding;
                if ui
                    .add_enabled(changed, egui::Button::new("Default").small())
                    .clicked()
                {
                    recorder::stop();
                    edits
                        .bindings
                        .push((row.name.clone(), row.default_binding.clone()));
                }
                ui.end_row();
            }
        });
}

/// "Reset all": every setting and binding of the plugin back to its default; only while one
/// differs.
fn reset_all(ui: &mut egui::Ui, section: &Section, edits: &mut Edits) {
    let settings: Vec<&SettingEntry> = section
        .settings
        .iter()
        .filter(|s| s.value != s.default)
        .collect();
    let bindings: Vec<&HotkeyEntry> = section
        .hotkeys
        .iter()
        .filter(|h| h.binding != h.default_binding)
        .collect();
    if settings.is_empty() && bindings.is_empty() {
        return;
    }
    let changed = settings.len() + bindings.len();
    let hint = format!("Puts the {changed} changed back to the plugin's defaults");
    if ui.small_button("Reset all").on_hover_text(hint).clicked() {
        recorder::stop();
        for s in settings {
            edits
                .settings
                .push((format!("{}.{}", s.plugin, s.key), s.default.clone()));
        }
        for h in bindings {
            edits
                .bindings
                .push((h.name.clone(), h.default_binding.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_group_by_plugin_and_skip_empty_ones() {
        let hotkey = HotkeyEntry {
            name: "overlay.console".to_owned(),
            title: String::new(),
            binding: "f1".to_owned(),
            default_binding: "f1".to_owned(),
        };
        let plugins = [("overlay".to_owned(), true), ("quiet".to_owned(), true)];
        let grouped = sections(&plugins, Vec::new(), vec![hotkey]);
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].plugin, "overlay");
    }
}
