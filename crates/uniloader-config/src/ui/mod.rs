//! The window: a start screen that asks where the config is, and the editor once one is open.

mod binds_view;
mod edit;
mod editor;
pub mod keycap;
mod loader_view;
mod plugins_view;
mod setting_row;

use std::path::{Path, PathBuf};

use eframe::egui::{self, RichText, Ui};

use crate::locate::{self, Location};
use crate::project::Project;
use crate::recent::Recent;
use editor::{Action, Editor};

/// The application.
pub struct App {
    recent: Recent,
    recent_file: Option<PathBuf>,
    screen: Screen,
}

enum Screen {
    /// Asking where the config is; the message says why.
    Start(Option<String>),
    Editor(Box<Editor>),
}

impl App {
    /// Open `location`, or the start screen with `message` when there is none.
    #[must_use]
    pub fn new(location: Option<Location>, message: Option<String>) -> Self {
        let recent_file = Recent::default_file();
        let recent = recent_file.as_deref().map(Recent::load).unwrap_or_default();
        let mut app = App {
            recent,
            recent_file,
            screen: Screen::Start(message),
        };
        if let Some(location) = location {
            app.open(location);
        }
        app
    }

    fn open(&mut self, location: Location) {
        let editor = Editor::new(Project::open(location));
        self.recent.push(editor.config_dir());
        self.save_recent();
        self.screen = Screen::Editor(Box::new(editor));
    }

    fn open_path(&mut self, path: &Path) {
        match locate::locate(path) {
            Some(location) => self.open(location),
            None => {
                self.screen = Screen::Start(Some(format!(
                    "No uniloader config at {}. Choose the game's folder (where the game's .exe \
                     and the plugins folder are), its uniloader folder, or uniloader/config.",
                    path.display()
                )));
            }
        }
    }

    fn save_recent(&mut self) {
        if let Some(file) = &self.recent_file {
            // Only a convenience: the editor works without it.
            let _ = self.recent.save(file);
        }
    }

    fn start(&mut self, ui: &mut Ui, message: Option<&str>) {
        let mut chosen: Option<PathBuf> = None;
        let mut forget: Option<PathBuf> = None;
        egui::CentralPanel::default_margins().show(ui, |ui| {
            ui.heading("uniloader config");
            ui.add_space(8.0);
            ui.label(message.unwrap_or(
                "No uniloader config next to this program. Choose the game's folder (where the \
                 game's .exe and the plugins folder are), its uniloader folder, or a file in \
                 uniloader/config.",
            ));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Choose folder…").clicked() {
                    chosen = rfd::FileDialog::new().pick_folder();
                }
                if ui.button("Choose file…").clicked() {
                    chosen = rfd::FileDialog::new()
                        .add_filter(
                            "uniloader files",
                            &["toml", "cfg", "txt", "dll", "exe", "json"],
                        )
                        .pick_file();
                }
            });
            if !self.recent.paths.is_empty() {
                ui.add_space(12.0);
                ui.label(RichText::new("Recent").strong());
                for path in &self.recent.paths {
                    ui.horizontal(|ui| {
                        if ui.link(path.display().to_string()).clicked() {
                            chosen = Some(path.clone());
                        }
                        if ui.small_button("Forget").clicked() {
                            forget = Some(path.clone());
                        }
                    });
                }
            }
        });
        if let Some(path) = forget {
            self.recent.remove(&path);
            self.save_recent();
        }
        if let Some(path) = chosen {
            self.open_path(&path);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        match &mut self.screen {
            Screen::Start(message) => {
                let message = message.clone();
                self.start(ui, message.as_deref());
            }
            Screen::Editor(editor) => {
                if editor.ui(ui) == Action::OpenOther {
                    self.screen = Screen::Start(None);
                }
            }
        }
    }
}
