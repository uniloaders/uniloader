//! "Don't show again": the dialogs a user dismissed for good, for every overlay provider.
//!
//! One key per line in `uniloader/config/dont-show-again.txt`, each `<plugin>/<key>` with the
//! key the plugin gave ([`crate::DialogDesc::remember`]). A file rather than a provider's
//! setting, so the egui overlay and a game's own UI (which may each draw the dialogs) share
//! it, and a person can read or empty it. Deleting the file shows everything again.

use std::path::{Path, PathBuf};

/// The file's name in the loader's config directory.
pub const FILE: &str = "dont-show-again.txt";

/// The file under `config_dir`.
#[must_use]
pub fn file(config_dir: &Path) -> PathBuf {
    config_dir.join(FILE)
}

/// The stored form of `plugin`'s key `remember`: one line, no surrounding blanks.
#[must_use]
pub fn key(plugin: &str, remember: &str) -> String {
    let clean = |s: &str| s.trim().replace(['\r', '\n'], " ");
    format!("{}/{}", clean(plugin), clean(remember))
}

/// Whether `key` (from [`key`]) was dismissed for good.
#[must_use]
pub fn is_dismissed(config_dir: &Path, key: &str) -> bool {
    std::fs::read_to_string(file(config_dir))
        .is_ok_and(|text| text.lines().any(|line| line.trim() == key))
}

/// Dismiss `key` for good.
///
/// # Errors
/// The file could not be written.
pub fn dismiss(config_dir: &Path, key: &str) -> std::io::Result<()> {
    if is_dismissed(config_dir, key) {
        return Ok(());
    }
    let path = file(config_dir);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(key);
    text.push('\n');
    std::fs::create_dir_all(config_dir)?;
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissed_keys_are_remembered_per_plugin() {
        let dir = std::env::temp_dir().join(format!("uniloader-remember-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let k = key("jump", " outdated/1.2.0\n");
        assert_eq!(k, "jump/outdated/1.2.0");
        assert!(!is_dismissed(&dir, &k));
        assert!(dismiss(&dir, &k).is_ok());
        assert!(dismiss(&dir, &k).is_ok(), "twice is once");
        assert!(is_dismissed(&dir, &k));
        assert!(!is_dismissed(&dir, &key("coop", "outdated/1.2.0")));
        assert_eq!(
            std::fs::read_to_string(file(&dir)).ok().as_deref(),
            Some("jump/outdated/1.2.0\n")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
