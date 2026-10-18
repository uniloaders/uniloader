//! Loader directories and `loader.toml`.

use std::path::{Path, PathBuf};

use log::LevelFilter;
use serde::Deserialize;
use uniloader_core::store;

/// Name of the loader's own directory in the game directory.
pub const LOADER_DIR: &str = "uniloader";

/// Where everything lives, derived from the game directory.
///
/// Every field names a directory, so the shared `_dir` suffix is the point rather than
/// redundancy; dropping it would leave bare words like `game` and `config`.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub struct Paths {
    /// Directory of the game executable.
    pub game_dir: PathBuf,
    /// `<game>/plugins`: plugin DLLs and `.asi` files.
    ///
    /// Deliberately not under the loader's own directory: this is the one directory a person
    /// puts files into, so it sits where they can find it, while everything the loader owns
    /// stays out of the game directory's way.
    pub plugins_dir: PathBuf,
    /// `<game>/uniloader/config`: `loader.toml`, `hotkeys.toml`, `<plugin>.toml`.
    pub config_dir: PathBuf,
    /// `<game>/uniloader/logs`.
    pub logs_dir: PathBuf,
}

impl Paths {
    /// Standard layout under `game_dir`.
    #[must_use]
    pub fn from_game_dir(game_dir: &Path) -> Self {
        let root = game_dir.join(LOADER_DIR);
        Paths {
            game_dir: game_dir.to_path_buf(),
            plugins_dir: game_dir.join("plugins"),
            config_dir: root.join("config"),
            logs_dir: root.join("logs"),
        }
    }

    /// Config file of one plugin.
    #[must_use]
    pub fn plugin_config(&self, plugin: &str) -> PathBuf {
        self.config_dir.join(format!("{plugin}.toml"))
    }

    /// User hotkey overrides.
    #[must_use]
    pub fn hotkeys_file(&self) -> PathBuf {
        self.config_dir.join("hotkeys.toml")
    }

    /// The loader's own settings.
    #[must_use]
    pub fn loader_file(&self) -> PathBuf {
        self.config_dir.join("loader.toml")
    }
}

/// `loader.toml`. Every field is optional.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct LoaderConfig {
    /// `false` turns the loader into a pure forwarder of the system DLL it stands in for.
    pub enabled: bool,
    /// Plugin file names (without the extension) that are never loaded.
    pub disabled_plugins: Vec<String>,
    /// `error`, `warn`, `info`, `debug` or `trace`.
    pub log_level: String,
    /// Load `.asi` files from the plugin directory, the way the Ultimate ASI Loader does.
    pub asi: bool,
    /// Other DLLs to load after the plugins, relative to the game directory, for example
    /// `ue4ss/UE4SS.dll` when the loader took the place of UE4SS's own proxy.
    pub load: Vec<String>,
}

impl Default for LoaderConfig {
    fn default() -> Self {
        LoaderConfig {
            enabled: true,
            disabled_plugins: Vec::new(),
            log_level: "info".to_owned(),
            asi: true,
            load: Vec::new(),
        }
    }
}

impl LoaderConfig {
    /// Parse the file, or defaults when it does not exist. A malformed file is reported and
    /// treated as defaults so a typo can never stop the game from starting.
    #[must_use]
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<LoaderConfig>(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(e) => (Self::default(), Some(format!("{}: {e}", path.display()))),
        }
    }

    /// Log level filter, falling back to `info` for unknown words.
    #[must_use]
    pub fn level(&self) -> LevelFilter {
        self.log_level.parse().unwrap_or(LevelFilter::Info)
    }
}

/// Load a plugin's stored settings (missing file is empty).
#[must_use]
pub fn load_plugin_values(
    paths: &Paths,
    plugin: &str,
) -> std::collections::BTreeMap<String, String> {
    let path = paths.plugin_config(plugin);
    match store::read(&path) {
        Ok(values) => values,
        Err(e) => {
            log::warn!("ignoring unreadable config: {e}");
            std::collections::BTreeMap::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_layout() {
        let p = Paths::from_game_dir(Path::new("/g"));
        assert_eq!(p.plugins_dir, Path::new("/g/plugins"));
        assert_eq!(p.logs_dir, Path::new("/g/uniloader/logs"));
        assert_eq!(
            p.plugin_config("vr"),
            Path::new("/g/uniloader/config/vr.toml")
        );
        assert_eq!(
            p.hotkeys_file(),
            Path::new("/g/uniloader/config/hotkeys.toml")
        );
        assert_eq!(
            p.loader_file(),
            Path::new("/g/uniloader/config/loader.toml")
        );
    }

    #[test]
    fn config_defaults_and_rejects_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("uniloader-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let path = dir.join("loader.toml");
        assert_eq!(LoaderConfig::load(&path), (LoaderConfig::default(), None));
        std::fs::write(
            &path,
            "enabled = false\nlog_level = \"debug\"\nasi = false\nload = [\"ue4ss/UE4SS.dll\"]\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (cfg, err) = LoaderConfig::load(&path);
        assert!(err.is_none());
        assert!(!cfg.enabled);
        assert!(!cfg.asi);
        assert_eq!(cfg.load, ["ue4ss/UE4SS.dll"]);
        assert_eq!(cfg.level(), LevelFilter::Debug);
        std::fs::write(&path, "typo = 1\n").unwrap_or_else(|e| panic!("{e}"));
        let (cfg, err) = LoaderConfig::load(&path);
        assert_eq!(cfg, LoaderConfig::default());
        assert!(err.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
