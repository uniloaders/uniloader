//! The file and folder browser behind path settings: a window of the overlay's own, so it
//! works in a full-screen game and never stalls a frame the way a system dialog would.
//! One is open at a time; what it picks is written like any other setting change.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use egui::{Context, RichText, ScrollArea};

use super::frame;

/// One line of the listing.
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

/// The open browser.
struct Browser {
    /// The setting it picks for, `<plugin>.<key>`.
    target: String,
    /// Picks a folder rather than a file.
    folder: bool,
    /// File name extensions offered, lowercase, without the dot; all files when empty.
    extensions: Vec<String>,
    dir: PathBuf,
    listing: Vec<Entry>,
    /// Why the folder could not be read.
    error: Option<String>,
    /// The file clicked, when picking a file.
    selected: Option<PathBuf>,
    /// The drive letters there are, looked up once.
    drives: Vec<String>,
}

static BROWSER: Mutex<Option<Browser>> = Mutex::new(None);

fn browser() -> MutexGuard<'static, Option<Browser>> {
    BROWSER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Opens the browser for `target`, starting where `current` points (or next to the game).
pub(crate) fn open(target: String, folder: bool, extensions: &[String], current: &str) {
    let current = Path::new(current);
    let dir = if current.is_dir() {
        current.to_path_buf()
    } else {
        current
            .parent()
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| {
                std::env::current_exe()
                    .ok()?
                    .parent()
                    .map(Path::to_path_buf)
            })
            .unwrap_or_default()
    };
    let mut b = Browser {
        target,
        folder,
        extensions: extensions
            .iter()
            .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
            .collect(),
        dir: PathBuf::new(),
        listing: Vec::new(),
        error: None,
        selected: current.is_file().then(|| current.to_path_buf()),
        drives: drives(),
    };
    b.enter(dir);
    *browser() = Some(b);
}

/// Closes it without picking.
pub(crate) fn close() {
    *browser() = None;
}

/// Draws the browser while one is open.
pub(crate) fn show(ctx: &Context) {
    let mut guard = browser();
    let Some(b) = guard.as_mut() else {
        return;
    };
    let mut open = true;
    let mut done = false;
    let title = if b.folder {
        "Choose a folder"
    } else {
        "Choose a file"
    };
    egui::Window::new(title)
        .id(egui::Id::new("uniloader-browser"))
        .collapsible(false)
        .default_size([520.0, 380.0])
        .open(&mut open)
        .show(ctx, |ui| done = b.ui(ui));
    if done || !open {
        *guard = None;
    }
}

impl Browser {
    /// Shows `dir`'s contents: folders first, then the files on offer.
    fn enter(&mut self, dir: PathBuf) {
        self.listing.clear();
        self.error = None;
        match std::fs::read_dir(&dir) {
            Ok(read) => {
                for item in read.flatten() {
                    let path = item.path();
                    let is_dir = path.is_dir();
                    if !is_dir && (self.folder || !self.offers(&path)) {
                        continue;
                    }
                    let name = item.file_name().to_string_lossy().into_owned();
                    self.listing.push(Entry { name, path, is_dir });
                }
                self.listing
                    .sort_by_cached_key(|e| (!e.is_dir, e.name.to_lowercase()));
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.dir = dir;
    }

    /// Whether a file has one of the extensions on offer.
    fn offers(&self, path: &Path) -> bool {
        self.extensions.is_empty()
            || path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .is_some_and(|e| self.extensions.contains(&e))
    }

    /// The window's body; `true` once something was picked or the user cancelled.
    fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut go: Option<PathBuf> = None;
        ui.horizontal(|ui| {
            let parent = self.dir.parent().map(Path::to_path_buf);
            if ui
                .add_enabled(parent.is_some(), egui::Button::new("Up"))
                .clicked()
            {
                go = parent;
            }
            for drive in &self.drives {
                if ui.small_button(drive).clicked() {
                    go = Some(PathBuf::from(format!("{drive}\\")));
                }
            }
        });
        ui.label(RichText::new(self.dir.display().to_string()).monospace());
        if !self.extensions.is_empty() && !self.folder {
            let list = self.extensions.join(", ");
            ui.label(RichText::new(format!("Showing: {list}")).weak().small());
        }
        ui.separator();
        let mut picked: Option<PathBuf> = None;
        ScrollArea::vertical()
            .max_height(ui.available_height() - 48.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if let Some(error) = &self.error {
                    ui.label(RichText::new(error).weak());
                }
                for entry in &self.listing {
                    let selected = self.selected.as_ref() == Some(&entry.path);
                    let text = if entry.is_dir {
                        format!("📁 {}", entry.name)
                    } else {
                        entry.name.clone()
                    };
                    let response = ui.selectable_label(selected, text);
                    if entry.is_dir && response.clicked() {
                        go = Some(entry.path.clone());
                    } else if response.double_clicked() {
                        picked = Some(entry.path.clone());
                    } else if response.clicked() {
                        self.selected = Some(entry.path.clone());
                    }
                }
            });
        ui.separator();
        let mut cancelled = false;
        ui.horizontal(|ui| {
            let choice = if self.folder {
                Some(self.dir.clone())
            } else {
                self.selected.clone()
            };
            if ui
                .add_enabled(choice.is_some(), egui::Button::new("Choose"))
                .clicked()
            {
                picked = choice;
            }
            cancelled = ui.button("Cancel").clicked();
        });
        if let Some(dir) = go {
            self.enter(dir);
        }
        if let Some(path) = picked {
            frame::queue_set(self.target.clone(), path.display().to_string());
            return true;
        }
        cancelled
    }
}

/// The drive letters there are (`C:`), on Windows.
fn drives() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    (b'A'..=b'Z')
        .map(|l| format!("{}:", char::from(l)))
        .filter(|d| Path::new(&format!("{d}\\")).is_dir())
        .collect()
}
