//! A temporary directory for tests, removed when dropped.

use std::path::{Path, PathBuf};

pub struct TestDir(PathBuf);

impl TestDir {
    /// A fresh, empty directory named after the test.
    pub fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("uniloader-config-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        TestDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Write `text` to `path`, creating its folders.
    pub fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("{e}"));
        }
        std::fs::write(path, text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }

    /// The text of `path`.
    pub fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
