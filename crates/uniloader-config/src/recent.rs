//! The config folders opened before, newest first, kept in the user's own config directory
//! (`%APPDATA%\uniloader-config\recent.toml`, `~/.config/uniloader-config/recent.toml`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many locations are remembered.
const KEEP: usize = 10;

/// The remembered locations.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    /// Config folders, newest first.
    pub paths: Vec<PathBuf>,
}

impl Recent {
    /// The file the list lives in, if the platform has a per-user config directory.
    #[must_use]
    pub fn default_file() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("uniloader-config").join("recent.toml"))
    }

    /// Read the list; a missing or unreadable file is an empty list (it is only a
    /// convenience).
    #[must_use]
    pub fn load(file: &Path) -> Self {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write the list.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::files::write_atomic(file, &text)
    }

    /// Put `path` first, dropping an older mention and the oldest beyond the limit.
    pub fn push(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(KEEP);
    }

    /// Forget `path`.
    pub fn remove(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn newest_first_without_repeats_and_bounded() {
        let mut recent = Recent::default();
        for i in 0..12 {
            recent.push(Path::new(&format!("/g{i}")));
        }
        recent.push(Path::new("/g5"));
        assert_eq!(recent.paths.len(), KEEP);
        assert_eq!(recent.paths[0], Path::new("/g5"));
        assert_eq!(recent.paths[1], Path::new("/g11"));
        assert_eq!(
            recent
                .paths
                .iter()
                .filter(|p| *p == Path::new("/g5"))
                .count(),
            1
        );
        recent.remove(Path::new("/g5"));
        assert_eq!(recent.paths[0], Path::new("/g11"));
    }

    #[test]
    fn the_list_survives_a_restart() {
        let t = TestDir::new("recent");
        let file = t.path().join("sub/recent.toml");
        assert_eq!(Recent::load(&file), Recent::default());
        let mut recent = Recent::default();
        recent.push(Path::new("/games/a b/uniloader/config"));
        recent.save(&file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(Recent::load(&file), recent);
        TestDir::write(&file, "not toml [");
        assert_eq!(Recent::load(&file), Recent::default());
    }
}
