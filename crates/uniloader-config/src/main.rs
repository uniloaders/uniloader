//! uniloader-config: edits a game's uniloader configuration in a window. Plugins on or off,
//! their settings with a control per type, key binds and aliases, and the loader's options.
//!
//! Started next to a config (in the game's folder, its `uniloader` folder or
//! `uniloader/config`) it opens that one; given a path as its argument, that one; otherwise
//! it asks. See the crate's README for how it learns what each plugin's settings are.

// A window program: no console window on Windows.
#![windows_subsystem = "windows"]

mod bind_table;
mod files;
mod loader_file;
mod locate;
mod plugins;
mod project;
mod recent;
mod running;
mod settings_file;
#[cfg(test)]
mod test_dir;
mod ui;

use std::path::PathBuf;

fn main() -> eframe::Result {
    let (location, message) = initial();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("uniloader config")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "uniloader-config",
        options,
        Box::new(|_| Ok(Box::new(ui::App::new(location, message)))),
    )
}

/// The config to open at start: the argument's, else the one next to this program.
fn initial() -> (Option<locate::Location>, Option<String>) {
    if let Some(arg) = std::env::args_os().nth(1) {
        let path = PathBuf::from(arg);
        return match locate::locate(&path) {
            Some(location) => (Some(location), None),
            None => (
                None,
                Some(format!("No uniloader config at {}.", path.display())),
            ),
        };
    }
    let here = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from));
    (here.as_deref().and_then(locate::locate), None)
}
