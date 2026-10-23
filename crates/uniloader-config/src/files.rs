//! Reading and atomically replacing config files, and noticing when someone else changed
//! one since it was read (the game, while it runs, or a text editor).

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A text file as it was read: its contents and when it was last modified, `None` for a
/// file that did not exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    /// Where it is.
    pub path: PathBuf,
    /// Its text; empty for a missing file.
    pub text: String,
    stamp: Option<SystemTime>,
}

impl Loaded {
    /// Read `path`; a missing file reads as empty.
    ///
    /// # Errors
    /// Any failure other than the file not existing.
    pub fn read(path: &Path) -> io::Result<Self> {
        let (text, stamp) = match std::fs::read_to_string(path) {
            Ok(text) => (text, modified(path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (String::new(), None),
            Err(e) => return Err(e),
        };
        Ok(Loaded {
            path: path.to_path_buf(),
            text,
            stamp,
        })
    }

    /// Whether the file on disk is no longer the one that was read.
    #[must_use]
    pub fn changed_on_disk(&self) -> bool {
        modified(&self.path) != self.stamp
    }

    /// Replace the file with `text` and remember it as read.
    ///
    /// # Errors
    /// The file could not be written.
    pub fn write(&mut self, text: &str) -> io::Result<()> {
        write_atomic(&self.path, text)?;
        text.clone_into(&mut self.text);
        self.stamp = modified(&self.path);
        Ok(())
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Write `text` to `path` through a temporary file in the same folder and a rename, so a
/// crash or a full disk leaves the old file rather than half of the new one. Creates the
/// folder.
///
/// # Errors
/// Any filesystem failure; the temporary file is removed again.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other(format!("{} has no folder", path.display())))?;
    std::fs::create_dir_all(parent)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    let temporary = parent.join(name);
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    #[test]
    fn a_write_replaces_the_file_and_leaves_no_temporary() {
        let t = TestDir::new("atomic");
        let file = t.path().join("new/dir/binds.cfg");
        let mut loaded = Loaded::read(&file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(loaded.text, "");
        loaded
            .write("bind f8 x\n")
            .unwrap_or_else(|e| panic!("{e}"));
        loaded
            .write("bind f9 y\n")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(TestDir::read(&file), "bind f9 y\n");
        let names: Vec<_> = std::fs::read_dir(file.parent().unwrap_or(t.path()))
            .unwrap_or_else(|e| panic!("{e}"))
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["binds.cfg"]);
        assert!(!loaded.changed_on_disk());
    }

    #[test]
    fn a_change_by_someone_else_is_noticed() {
        let t = TestDir::new("stamp");
        let file = t.path().join("a.toml");
        let missing = Loaded::read(&file).unwrap_or_else(|e| panic!("{e}"));
        TestDir::write(&file, "a = \"1\"\n");
        assert!(missing.changed_on_disk(), "created since");
        let loaded = Loaded::read(&file).unwrap_or_else(|e| panic!("{e}"));
        assert!(!loaded.changed_on_disk());
        std::fs::remove_file(&file).unwrap_or_else(|e| panic!("{e}"));
        assert!(loaded.changed_on_disk(), "removed since");
    }
}
