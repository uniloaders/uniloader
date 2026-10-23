//! Finding a game's uniloader configuration from a path: the program's own folder, a folder
//! the user picked, or any file inside one.
//!
//! The layouts are the loader's (`crates/uniloader/src/config.rs`, `Paths`):
//!
//! ```text
//! <game>/plugins/  <game>/uniloader/config/          standard
//! <game>/uniloader-root.txt -> <root>/plugins/  <root>/config/   moved root
//! ```

use std::path::{Path, PathBuf};

/// The loader's own directory in the game directory.
const LOADER_DIR: &str = "uniloader";

/// The file next to the game's executable that moves the loader's root elsewhere.
pub const ROOT_OVERRIDE: &str = "uniloader-root.txt";

/// Files only a uniloader config directory has.
const CONFIG_MARKERS: &[&str] = &["loader.toml", "binds.cfg", "uniloader.toml", "schema"];

/// Where one game's loader keeps its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// `loader.toml`, `binds.cfg`, `<plugin>.toml` and `schema/`. May not exist yet, for an
    /// install the game has not been started with; saving creates it.
    pub config_dir: PathBuf,
    /// The plugin DLLs and `.asi` files.
    pub plugins_dir: PathBuf,
}

impl Location {
    /// `<config>/loader.toml`.
    #[must_use]
    pub fn loader_file(&self) -> PathBuf {
        self.config_dir.join("loader.toml")
    }

    /// `<config>/binds.cfg`.
    #[must_use]
    pub fn binds_file(&self) -> PathBuf {
        self.config_dir.join("binds.cfg")
    }

    /// `<config>/<plugin>.toml`.
    #[must_use]
    pub fn settings_file(&self, plugin: &str) -> PathBuf {
        self.config_dir.join(format!("{plugin}.toml"))
    }

    /// `<config>/schema`.
    #[must_use]
    pub fn schema_dir(&self) -> PathBuf {
        self.config_dir.join(uniloader_core::schema::DIR)
    }
}

/// The configuration `path` belongs to: `path` itself or one of its two nearest ancestors is
/// the game folder, the loader's root, or the config folder. A file counts as its folder.
#[must_use]
pub fn locate(path: &Path) -> Option<Location> {
    let dir = if path.is_file() { path.parent()? } else { path };
    dir.ancestors().take(3).find_map(at)
}

/// The configuration `dir` is the game folder, the root or the config folder of.
fn at(dir: &Path) -> Option<Location> {
    if let Ok(text) = std::fs::read_to_string(dir.join(ROOT_OVERRIDE)) {
        // The override wins, as in the loader; a root that does not resolve here (another
        // Wine drive) is not guessed at.
        return override_root(dir, &text).map(|root| root_location(&root));
    }
    let standard = Location {
        config_dir: dir.join(LOADER_DIR).join("config"),
        plugins_dir: dir.join("plugins"),
    };
    if standard.config_dir.is_dir() {
        return Some(standard);
    }
    let named_root = dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(LOADER_DIR));
    if dir.join("config").is_dir() && (named_root || dir.join("plugins").is_dir()) {
        return Some(root_location(dir));
    }
    // A game the loader has not run in yet: no config, but plugins waiting.
    if has_plugin(&standard.plugins_dir) {
        return Some(standard);
    }
    if CONFIG_MARKERS.iter().any(|m| dir.join(m).exists()) {
        return Some(root_location(dir.parent()?));
    }
    None
}

/// The layout under a loader root: `config/` in it, and `plugins/` in it when the root was
/// moved, or next to it in the game folder when it is the standard `<game>/uniloader`.
fn root_location(root: &Path) -> Location {
    let own = root.join("plugins");
    let plugins_dir = match root.parent() {
        Some(game) if !own.is_dir() => game.join("plugins"),
        _ => own,
    };
    Location {
        config_dir: root.join("config"),
        plugins_dir,
    }
}

/// Whether `dir` holds a DLL that is a uniloader plugin.
fn has_plugin(dir: &Path) -> bool {
    crate::plugins::plugin_files(dir).next().is_some()
}

/// The root an override file names: its first line that is neither empty nor a `#` comment,
/// relative to `game_dir` or absolute. Under Wine the file holds a Windows path; its `Z:`
/// drive is the host's `/`.
#[must_use]
pub fn override_root(game_dir: &Path, text: &str) -> Option<PathBuf> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    if cfg!(windows) {
        return Some(game_dir.join(line));
    }
    let bytes = line.as_bytes();
    let drive = (bytes.len() >= 2 && bytes[1] == b':').then(|| bytes[0].to_ascii_lowercase());
    match drive {
        Some(b'z') => Some(PathBuf::from(format!(
            "/{}",
            line[2..].replace('\\', "/").trim_start_matches('/')
        ))),
        Some(_) => None,
        None => Some(game_dir.join(line.replace('\\', "/"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    /// A file that claims to be a plugin the way the loader checks.
    fn plugin_dll(path: &Path) {
        TestDir::write(path, uniloader_api::DESCRIBE_EXPORT);
    }

    #[test]
    fn the_standard_layout_is_found_from_every_folder_in_it() {
        let t = TestDir::new("standard");
        let game = t.path().join("Game");
        TestDir::write(&game.join("uniloader/config/loader.toml"), "");
        let want = Location {
            config_dir: game.join("uniloader/config"),
            plugins_dir: game.join("plugins"),
        };
        for from in [
            game.clone(),
            game.join("uniloader"),
            game.join("uniloader/config"),
            game.join("uniloader/config/loader.toml"),
        ] {
            assert_eq!(locate(&from), Some(want.clone()), "from {}", from.display());
        }
    }

    #[test]
    fn a_game_never_started_with_the_loader_is_found_by_its_plugins() {
        let t = TestDir::new("fresh");
        let game = t.path().join("Game");
        plugin_dll(&game.join("plugins/coop.dll"));
        let found = locate(&game).unwrap_or_else(|| panic!("not found"));
        assert_eq!(found.config_dir, game.join("uniloader/config"));
        assert_eq!(found.plugins_dir, game.join("plugins"));
        // A plugins folder holding no plugin is not enough.
        let other = t.path().join("Other");
        TestDir::write(&other.join("plugins/helper.dll"), "MZ");
        assert_eq!(locate(&other), None);
    }

    #[test]
    fn a_moved_root_is_followed() {
        let t = TestDir::new("moved");
        let game = t.path().join("Game");
        let root = t.path().join("mods/ul");
        TestDir::write(&root.join("config/binds.cfg"), "");
        plugin_dll(&root.join("plugins/x.dll"));
        TestDir::write(
            &game.join(ROOT_OVERRIDE),
            &format!("# by a mod manager\n\n{}\n", root.display()),
        );
        let want = Location {
            config_dir: root.join("config"),
            plugins_dir: root.join("plugins"),
        };
        assert_eq!(locate(&game), Some(want.clone()));
        // From inside the moved root, without the override file.
        assert_eq!(locate(&root), Some(want.clone()));
        assert_eq!(locate(&root.join("config")), Some(want));
    }

    #[test]
    fn override_paths_resolve_like_the_loader_and_through_wine() {
        let game = Path::new("/g");
        assert_eq!(
            override_root(game, "mods/ul"),
            Some(PathBuf::from("/g/mods/ul"))
        );
        assert_eq!(override_root(game, "# only a comment\n"), None);
        if !cfg!(windows) {
            assert_eq!(
                override_root(game, "Z:\\home\\me\\mods\\tsw"),
                Some(PathBuf::from("/home/me/mods/tsw"))
            );
            assert_eq!(override_root(game, "C:\\mods"), None);
        }
    }

    #[test]
    fn unrelated_folders_are_not_a_config() {
        let t = TestDir::new("none");
        TestDir::write(&t.path().join("a/b/readme.txt"), "");
        assert_eq!(locate(&t.path().join("a/b")), None);
        assert_eq!(locate(&t.path().join("does-not-exist")), None);
    }
}
