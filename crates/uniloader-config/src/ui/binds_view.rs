//! Key binds: the keys plugins suggest (moved or removed by the user), the user's own binds
//! to console scripts, and aliases.

use eframe::egui::{Button, ComboBox, Grid, RichText, ScrollArea, TextEdit, Ui};
use uniloader_core::binds;
use uniloader_core::keys::Chord;

use super::edit::{self, Edit, Record};
use super::keycap::{self, Recorded};
use crate::bind_table::{BindTable, Known};

/// The bind being added or edited.
#[derive(Debug, Default)]
pub struct Form {
    /// The user bind being edited, `None` when adding.
    editing: Option<Chord>,
    key: String,
    script: String,
    alias_name: String,
    alias_script: String,
}

const BIND_ID: &str = "#bind";
const ALIAS_ID: &str = "#alias";

/// The whole binds page.
pub fn show(ui: &mut Ui, edit: &mut Edit, form: &mut Form, table: &mut BindTable, known: &Known) {
    ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        if !table.warnings.is_empty() {
            for warning in &table.warnings {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            ui.separator();
        }
        ui.heading("Plugin keys");
        ui.label(
            RichText::new(
                "Keys the plugins suggest. Move one to other keys, or clear it; a plugin changing \
                 its suggestion later does not undo your choice.",
            )
            .weak(),
        );
        entries(ui, edit, table);
        ui.add_space(12.0);
        ui.heading("Your binds");
        ui.label(
            RichText::new(
                "A key that runs console commands, separated by ; (for example \
                 echo hi; uniloader.speed_up). A bind takes its key from a plugin's suggestion.",
            )
            .weak(),
        );
        user_binds(ui, edit, form, table, known);
        ui.add_space(12.0);
        ui.heading("Aliases");
        ui.label(
            RichText::new(
                "A name for a script. +name runs while a key is held and -name when it is let go.",
            )
            .weak(),
        );
        aliases(ui, edit, form, table, known);
    });
}

fn entries(ui: &mut Ui, edit: &mut Edit, table: &mut BindTable) {
    let rows: Vec<(String, String, String, String)> = table
        .entries()
        .map(|e| {
            (
                e.name.clone(),
                e.title.clone(),
                e.binding(),
                e.default_binding(),
            )
        })
        .collect();
    if rows.is_empty() {
        ui.label(
            RichText::new("No plugin suggested a key (or none has run with this loader yet).")
                .weak(),
        );
        return;
    }
    Grid::new("entries").num_columns(4).striped(true).show(ui, |ui| {
        for (name, title, binding, default) in rows {
            let shadowed = table.shadowed(&name);
            ui.horizontal(|ui| {
                ui.label(&title).on_hover_text(&name);
                if !shadowed.is_empty() {
                    let keys: Vec<String> = shadowed.iter().map(ToString::to_string).collect();
                    ui.colored_label(ui.visuals().warn_fg_color, "taken")
                        .on_hover_text(format!(
                            "{} {} held by an earlier plugin or one of your binds, so it does nothing here.",
                            keys.join(", "),
                            if keys.len() == 1 { "is" } else { "are" }
                        ));
                }
            });
            let id = format!("entry:{name}");
            let target = Record::Entry(name.clone());
            let recording = edit.is_recording(&target);
            if keycap::button(ui, &binding, recording) {
                edit.toggle_recording(target.clone());
            }
            let draft = edit.draft(&id, || binding.clone());
            let typed = ui
                .add(TextEdit::singleline(draft).desired_width(160.0))
                .on_hover_text("Keys separated by commas; none for no key.")
                .changed()
                .then(|| draft.clone());
            let reset = ui
                .add_enabled(binding != default, Button::new("Reset").small())
                .on_hover_text(format!("Back to {default}."));
            ui.end_row();
            let mut input = typed;
            if recording {
                match keycap::poll(ui) {
                    Recorded::Waiting => {}
                    Recorded::Cancelled => edit.recording = None,
                    Recorded::Bound(chord) => {
                        edit.recording = None;
                        edit.forget(&id);
                        input = Some(chord);
                    }
                }
            }
            if let Some(input) = input {
                let outcome = table.rebind(&name, &input).map(|()| Vec::new());
                edit.outcome(&id, outcome);
            }
            if reset.clicked() {
                table.reset_entry(&name);
                edit.forget(&id);
            }
            if edit.error(&id).is_some() {
                ui.label("");
                edit::messages(ui, edit, &id);
                ui.end_row();
            }
        }
    });
}

fn user_binds(ui: &mut Ui, edit: &mut Edit, form: &mut Form, table: &mut BindTable, known: &Known) {
    let binds = table.user_binds();
    if !binds.is_empty() {
        Grid::new("user-binds")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for (chord, script) in binds {
                    ui.label(RichText::new(chord.to_string()).monospace());
                    ui.label(RichText::new(&script).monospace());
                    ui.horizontal(|ui| {
                        if ui.small_button("Edit").clicked() {
                            form.editing = Some(chord);
                            form.key = chord.to_string();
                            form.script.clone_from(&script);
                            edit.forget(BIND_ID);
                        }
                        if ui.small_button("Remove").clicked() {
                            table.remove_bind(chord);
                            if form.editing == Some(chord) {
                                *form = Form::default();
                            }
                        }
                    });
                    ui.end_row();
                }
            });
    }
    bind_form(ui, edit, form, table, known);
}

fn bind_form(ui: &mut Ui, edit: &mut Edit, form: &mut Form, table: &mut BindTable, known: &Known) {
    ui.horizontal(|ui| {
        let recording = edit.is_recording(&Record::Bind);
        if keycap::button(ui, &form.key, recording) {
            edit.toggle_recording(Record::Bind);
        }
        if recording {
            match keycap::poll(ui) {
                Recorded::Waiting => {}
                Recorded::Cancelled => edit.recording = None,
                Recorded::Bound(chord) => {
                    edit.recording = None;
                    form.key = chord;
                }
            }
        }
        ui.add(
            TextEdit::singleline(&mut form.key)
                .hint_text("key")
                .desired_width(100.0),
        );
        ui.add(
            TextEdit::singleline(&mut form.script)
                .hint_text("commands")
                .desired_width(300.0),
        );
        command_picker(ui, "bind-picker", &mut form.script, known);
        let label = if form.editing.is_some() {
            "Apply"
        } else {
            "Add"
        };
        if ui.button(label).clicked() {
            let outcome = table.bind(form.editing, &form.key, &form.script, known);
            if outcome.is_ok() {
                form.editing = None;
                form.key.clear();
                form.script.clear();
            }
            edit.outcome(BIND_ID, outcome);
        }
        if form.editing.is_some() && ui.button("Cancel").clicked() {
            *form = Form::default();
            edit.forget(BIND_ID);
        }
    });
    edit::messages(ui, edit, BIND_ID);
}

fn aliases(ui: &mut Ui, edit: &mut Edit, form: &mut Form, table: &mut BindTable, known: &Known) {
    let list: Vec<(String, String)> = table
        .aliases()
        .map(|(n, s)| (n.to_owned(), s.to_owned()))
        .collect();
    if !list.is_empty() {
        Grid::new("aliases")
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for (name, script) in list {
                    ui.label(RichText::new(&name).monospace());
                    ui.label(RichText::new(&script).monospace());
                    ui.horizontal(|ui| {
                        if ui.small_button("Edit").clicked() {
                            form.alias_name.clone_from(&name);
                            form.alias_script = script;
                        }
                        if ui.small_button("Remove").clicked() {
                            table.remove_alias(&name);
                        }
                    });
                    ui.end_row();
                }
            });
    }
    ui.horizontal(|ui| {
        ui.add(
            TextEdit::singleline(&mut form.alias_name)
                .hint_text("name")
                .desired_width(100.0),
        );
        ui.add(
            TextEdit::singleline(&mut form.alias_script)
                .hint_text("commands")
                .desired_width(300.0),
        );
        command_picker(ui, "alias-picker", &mut form.alias_script, known);
        if ui.button("Set").clicked() {
            let outcome = if binds::alias_name(&form.alias_name.trim().to_ascii_lowercase()) {
                table.set_alias(&form.alias_name, &form.alias_script, known)
            } else {
                Err(
                    "Alias names are letters, digits and _, with an optional leading + or -."
                        .to_owned(),
                )
            };
            if outcome.is_ok() {
                form.alias_name.clear();
                form.alias_script.clear();
            }
            edit.outcome(ALIAS_ID, outcome);
        }
    });
    edit::messages(ui, edit, ALIAS_ID);
}

/// A dropdown of every command, setting and action the plugins registered; picking one
/// appends it to `script`.
fn command_picker(ui: &mut Ui, id: &str, script: &mut String, known: &Known) {
    ComboBox::from_id_salt(id)
        .selected_text("Insert…")
        .width(120.0)
        .show_ui(ui, |ui| {
            for name in known.names() {
                if ui.selectable_label(false, &name).clicked() {
                    if !script.trim().is_empty() {
                        script.push_str("; ");
                    }
                    script.push_str(&name);
                }
            }
        });
}
