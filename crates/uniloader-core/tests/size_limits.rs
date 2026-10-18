//! Enforces the file size limits from the coding rules across every workspace crate.

use std::path::{Path, PathBuf};

const HARD: usize = 1000;
const WARN: usize = 600;

fn source_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            source_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_source_file_exceeds_limits() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for sub in ["crates", "examples", "plugins"] {
        source_files(&root.join(sub), &mut files);
    }
    assert!(
        !files.is_empty(),
        "no source files found under {}",
        root.display()
    );
    let mut over = Vec::new();
    for path in files {
        let lines = std::fs::read_to_string(&path).map_or(0, |t| t.lines().count());
        if lines > HARD {
            over.push(format!(
                "{} has {lines} lines (limit {HARD})",
                path.display()
            ));
        } else if lines > WARN {
            eprintln!(
                "note: {} is {lines} lines, approaching the {HARD} limit",
                path.display()
            );
        }
    }
    assert!(
        over.is_empty(),
        "files over the size limit:\n{}",
        over.join("\n")
    );
}
