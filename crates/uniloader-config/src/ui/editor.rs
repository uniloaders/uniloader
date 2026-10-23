//! The editor for one install: a bar with the location, the game's state and Save; tabs for
//! plugins, binds and the loader's options.

use eframe::egui::{self, Align2, Button, RichText, Ui};

use super::binds_view;
use super::edit::Edit;
use super::loader_view;
use super::plugins_view;
use crate::project::Project;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Plugins,
    Binds,
    Loader,
}

/// What the editor asks of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep going.
    None,
    /// Show the start screen to open another install.
    OpenOther,
}

/// The editor's state.
pub struct Editor {
    project: Project,
    tab: Tab,
    selected: usize,
    edit: Edit,
    bind_form: binds_view::Form,
    /// The game's executable, when the loader recorded it, and whether it ran at the last
    /// check.
    game: Option<(String, bool)>,
    /// The outcome of the last save or reload, and whether it is an error.
    status: Option<(String, bool)>,
    /// Files someone else changed since they were read, waiting for the user's decision.
    conflicts: Vec<std::path::PathBuf>,
    /// The window was asked to close with unsaved changes.
    closing: bool,
}

impl Editor {
    /// Open `project`.
    #[must_use]
    pub fn new(project: Project) -> Self {
        let mut editor = Editor {
            project,
            tab: Tab::Plugins,
            selected: 0,
            edit: Edit::default(),
            bind_form: binds_view::Form::default(),
            game: None,
            status: None,
            conflicts: Vec::new(),
            closing: false,
        };
        editor.check_game();
        editor
    }

    /// Where the config is, for the recent list.
    #[must_use]
    pub fn config_dir(&self) -> &std::path::Path {
        &self.project.location.config_dir
    }

    fn check_game(&mut self) {
        self.game = self
            .project
            .loader_schema
            .as_ref()
            .map(|l| (l.game_exe.clone(), crate::running::is_running(&l.game_exe)));
    }

    fn reload(&mut self) {
        self.project = Project::open(self.project.location.clone());
        self.edit.forget_all();
        self.bind_form = binds_view::Form::default();
        self.selected = self
            .selected
            .min(self.project.plugins.len().saturating_sub(1));
        self.check_game();
        self.status = Some(("Reloaded from disk.".to_owned(), false));
    }

    fn save(&mut self, overwrite: bool) {
        self.check_game();
        if !overwrite {
            self.conflicts = self.project.conflicts();
            if !self.conflicts.is_empty() {
                return;
            }
        }
        self.conflicts.clear();
        self.status = Some(match self.project.save() {
            Ok(written) => {
                let names: Vec<String> = written
                    .iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect();
                (format!("Saved {}.", names.join(", ")), false)
            }
            Err((_, error)) => (format!("Not saved: {error}"), true),
        });
    }

    /// Draw the editor.
    pub fn ui(&mut self, ui: &mut Ui) -> Action {
        self.handle_close(ui.ctx());
        let mut action = Action::None;
        egui::Panel::top("bar").show(ui, |ui| action = self.bar(ui));
        match self.tab {
            Tab::Plugins => {
                egui::Panel::left("plugins")
                    .resizable(true)
                    .default_size(220.0)
                    .show(ui, |ui| {
                        if let Some(index) =
                            plugins_view::list(ui, &mut self.project, self.selected)
                        {
                            self.selected = index;
                        }
                    });
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    plugins_view::detail(ui, &mut self.edit, &mut self.project, self.selected);
                });
            }
            Tab::Binds => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    match &mut self.project.binds {
                        Ok(doc) => binds_view::show(
                            ui,
                            &mut self.edit,
                            &mut self.bind_form,
                            &mut doc.value,
                            &self.project.known,
                        ),
                        Err(error) => {
                            ui.colored_label(ui.visuals().error_fg_color, error.as_str());
                        }
                    }
                });
            }
            Tab::Loader => {
                egui::CentralPanel::default_margins().show(ui, |ui| {
                    loader_view::show(ui, &mut self.edit, &mut self.project);
                });
            }
        }
        self.conflict_dialog(ui.ctx());
        self.close_dialog(ui.ctx());
        action
    }

    fn bar(&mut self, ui: &mut Ui) -> Action {
        let mut action = Action::None;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (tab, label) in [
                (Tab::Plugins, "Plugins"),
                (Tab::Binds, "Key binds"),
                (Tab::Loader, "Loader options"),
            ] {
                ui.selectable_value(&mut self.tab, tab, label);
            }
            ui.separator();
            let changed = self.project.changed();
            if ui
                .add_enabled(changed, Button::new(RichText::new("Save").strong()))
                .on_hover_text("Write the changed files (Ctrl+S).")
                .clicked()
                || (changed
                    && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)))
            {
                self.save(false);
            }
            if ui
                .button("Reload")
                .on_hover_text("Read the files again, dropping unsaved changes.")
                .clicked()
            {
                self.reload();
            }
            if ui.button("Open other…").clicked() {
                action = Action::OpenOther;
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(self.project.location.config_dir.display().to_string())
                    .monospace()
                    .weak(),
            );
        });
        self.notices(ui);
        ui.add_space(4.0);
        action
    }

    fn notices(&mut self, ui: &mut Ui) {
        let warn = ui.visuals().warn_fg_color;
        if let Some((exe, true)) = self.game.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    warn,
                    format!(
                        "{exe} is running. The loader reads these files when the game starts, and \
                         rewrites binds.cfg and a plugin's settings file when they change in game, \
                         which would undo changes saved now. Close the game first; saved changes \
                         apply at the next start (the loader has no reload)."
                    ),
                );
                if ui.small_button("Check again").clicked() {
                    self.check_game();
                }
            });
        } else if self.project.loader_schema.is_none() {
            ui.colored_label(
                warn,
                "The game has not run with this loader yet (no schema files), so plugin settings \
                 and keys are not known. Start it once, then reload.",
            );
        }
        for problem in &self.project.problems {
            ui.colored_label(warn, problem);
        }
        if let Some((text, error)) = &self.status {
            let colour = if *error {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().text_color()
            };
            ui.colored_label(colour, text);
        }
    }

    fn conflict_dialog(&mut self, ctx: &egui::Context) {
        if self.conflicts.is_empty() {
            return;
        }
        let mut choice = None;
        dialog(ctx, "Changed on disk", |ui| {
            ui.label("These files changed on disk since they were opened (the game or another editor wrote them):");
            for path in &self.conflicts {
                ui.label(RichText::new(path.display().to_string()).monospace());
            }
            ui.horizontal(|ui| {
                if ui.button("Overwrite them").clicked() {
                    choice = Some(true);
                }
                if ui.button("Reload, dropping my changes").clicked() {
                    choice = Some(false);
                }
                if ui.button("Cancel").clicked() {
                    self.conflicts.clear();
                }
            });
        });
        match choice {
            Some(true) => self.save(true),
            Some(false) => {
                self.conflicts.clear();
                self.reload();
            }
            None => {}
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.project.changed() && !self.closing
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.closing = true;
        }
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        if !self.closing {
            return;
        }
        let mut close = false;
        dialog(ctx, "Unsaved changes", |ui| {
            ui.label("Some changes are not saved.");
            ui.horizontal(|ui| {
                if ui.button("Save and close").clicked() {
                    self.save(false);
                    close = !self.project.changed();
                    self.closing = close;
                }
                if ui.button("Close without saving").clicked() {
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    self.closing = false;
                }
            });
        });
        if close {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// A small window in the middle of the screen.
fn dialog(ctx: &egui::Context, title: &str, contents: impl FnOnce(&mut Ui)) {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, contents);
}
