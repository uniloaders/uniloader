//! The settings editor: every plugin's settings and hotkeys in one window.
//!
//! Plugins get this for free: a setting registered because the plugin needs the value, or a
//! hotkey registered because it needs the action, shows up here with no UI code. Writes go
//! through the loader — `set` for a setting, `rebind` for a binding — so a slider here and a
//! typed console command cannot disagree.

use egui::{Context, Grid, RichText, ScrollArea};
use uniloader_core::keys::{self, Chord, Modifiers};
use uniloader_sdk::{HotkeyEntry, SettingEntry};

use super::chrome::{self, Chrome, Placed};
use super::frame;

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

/// What the editor remembers between frames.
#[derive(Default)]
pub(crate) struct SettingsPanel {
    /// The action whose key is being recorded.
    recording: Option<String>,
}

/// Everything one frame of the editor is drawn from.
pub(crate) struct View<'a> {
    pub(crate) sections: &'a [Section],
    /// The key the window procedure saw: virtual key, scan code, modifiers.
    pub(crate) pressed: Option<(u16, u16, Modifiers)>,
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

impl SettingsPanel {
    /// Whether a key is being recorded, so the caller hands over key presses.
    pub(crate) fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// Draw the window.
    pub(crate) fn show(
        &mut self,
        ctx: &Context,
        view: &View<'_>,
        chrome: &Chrome<'_>,
    ) -> (Edits, Placed) {
        let mut edits = Edits::default();
        if let (Some(action), Some((vk, scancode, modifiers))) =
            (self.recording.clone(), view.pressed)
        {
            self.finish_recording(&action, vk, scancode, modifiers, &mut edits);
        }
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
                        self.section(ui, section, view.advanced, &mut edits);
                    }
                    if view.sections.is_empty() {
                        ui.label("No plugin has registered a setting or a hotkey.");
                    }
                });
        });
        (edits, placed)
    }

    /// One plugin: its settings, then its hotkeys.
    fn section(&mut self, ui: &mut egui::Ui, section: &Section, advanced: bool, edits: &mut Edits) {
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
                    self.hotkeys(ui, section, edits);
                }
            });
    }

    /// The hotkey table: what it does, what it is bound to, and the buttons to change it.
    fn hotkeys(&mut self, ui: &mut egui::Ui, section: &Section, edits: &mut Edits) {
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
                    let recording = self.recording.as_deref() == Some(row.name.as_str());
                    let text = if recording {
                        RichText::new("press a key…").strong()
                    } else if row.binding.is_empty() {
                        RichText::new("none").monospace().weak()
                    } else {
                        RichText::new(&row.binding).monospace()
                    };
                    if ui.button(text).clicked() {
                        self.recording = if recording {
                            None
                        } else {
                            Some(row.name.clone())
                        };
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button("Clear").clicked() {
                            self.recording = None;
                            edits.bindings.push((row.name.clone(), String::new()));
                        }
                        let changed = row.binding != row.default_binding;
                        if ui
                            .add_enabled(changed, egui::Button::new("Default").small())
                            .clicked()
                        {
                            self.recording = None;
                            edits
                                .bindings
                                .push((row.name.clone(), row.default_binding.clone()));
                        }
                    });
                    ui.end_row();
                }
            });
        if self.recording.is_some() {
            ui.label(
                RichText::new("Escape cancels, and a modifier alone is ignored.")
                    .weak()
                    .small(),
            );
        }
    }

    /// Turn the key that was just pressed into a binding.
    fn finish_recording(
        &mut self,
        action: &str,
        vk: u16,
        scancode: u16,
        modifiers: Modifiers,
        edits: &mut Edits,
    ) {
        if let Some(binding) = recorded(vk, scancode, modifiers) {
            self.recording = None;
            if !binding.is_empty() {
                edits.bindings.push((action.to_owned(), binding));
            }
        }
    }
}

/// The binding a recorded key press stands for: `None` while only a modifier is held (still
/// waiting), an empty string for Escape (cancelled), otherwise the chord in key grammar.
///
/// A key the grammar can name is recorded by name, which reads well in `hotkeys.toml`;
/// anything else — the key under Escape reports `0xFC` under Wine — by position, so the
/// binding stays on that key whatever the layout calls it.
pub(crate) fn recorded(vk: u16, scancode: u16, modifiers: Modifiers) -> Option<String> {
    const VK_ESCAPE: u16 = 0x1B;
    const MODIFIER_KEYS: [u16; 9] = [0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5];
    if MODIFIER_KEYS.contains(&vk) {
        return None;
    }
    if vk == VK_ESCAPE {
        return Some(String::new());
    }
    let chord = Chord {
        vk,
        scancode: (!keys::has_name(vk)).then_some(scancode),
        modifiers,
    };
    Some(chord.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_waits_for_a_real_key_and_escape_cancels() {
        let none = Modifiers::default();
        assert_eq!(recorded(0x11, 0x1D, none), None, "Ctrl alone");
        assert_eq!(recorded(0x1B, 0x01, none), Some(String::new()));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(recorded(0x7A, 0x57, ctrl).as_deref(), Some("ctrl+f11"));
        let by_position = recorded(0xFC, 0x29, none).unwrap_or_default();
        assert!(by_position.starts_with("sc"), "{by_position}");
    }

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
