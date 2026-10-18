//! Flat `key = "value"` TOML files for plugin settings and hotkey bindings.
//!
//! Every value is stored as a string: the typed descriptor, not the file, is the source of
//! truth for the kind, so a hand-edited file can never desynchronise the type.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use thiserror::Error;

/// Why a store file could not be read or written.
#[derive(Debug, Error)]
pub enum StoreError {
    /// Filesystem failure.
    #[error("{path}: {source}")]
    Io {
        /// File involved.
        path: String,
        /// Underlying error.
        source: io::Error,
    },
    /// Not valid TOML.
    #[error("{path}: {source}")]
    Parse {
        /// File involved.
        path: String,
        /// Underlying error.
        source: toml::de::Error,
    },
}

/// Read a flat table. A missing file is an empty table; non-string values are stringified.
///
/// # Errors
/// I/O failure other than "not found", or a TOML syntax error.
pub fn read(path: &Path) -> Result<BTreeMap<String, String>, StoreError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(StoreError::Io {
                path: path.display().to_string(),
                source,
            })
        }
    };
    let table: toml::Table = text.parse().map_err(|source| StoreError::Parse {
        path: path.display().to_string(),
        source,
    })?;
    Ok(table
        .into_iter()
        .map(|(k, v)| (k, value_to_string(&v)))
        .collect())
}

fn value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Write a flat table, creating parent directories. Keys are sorted for stable diffs.
///
/// # Errors
/// Filesystem failure.
pub fn write(path: &Path, values: &BTreeMap<String, String>) -> Result<(), StoreError> {
    let io_err = |source| StoreError::Io {
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_err)?;
    }
    let table: toml::Table = values
        .iter()
        .map(|(k, v)| (k.clone(), toml::Value::String(v.clone())))
        .collect();
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, table.to_string()).map_err(io_err)?;
    fs::rename(&tmp, path).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("uniloader-core-{}", std::process::id()));
        dir.join(name)
    }

    #[test]
    fn missing_file_is_empty() {
        assert!(read(&temp_file("nothing.toml"))
            .unwrap_or_else(|e| panic!("{e}"))
            .is_empty());
    }

    #[test]
    fn round_trip_and_stringified_numbers() {
        let path = temp_file("rt.toml");
        let mut values = BTreeMap::new();
        values.insert("stereo.ipd".to_owned(), "0.064".to_owned());
        values.insert("a".to_owned(), "quote \" inside".to_owned());
        write(&path, &values).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read(&path).unwrap_or_else(|e| panic!("{e}")), values);

        fs::write(&path, "n = 42\nb = true\n").unwrap_or_else(|e| panic!("{e}"));
        let got = read(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(got["n"], "42");
        assert_eq!(got["b"], "true");

        fs::write(&path, "not toml [").unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(read(&path), Err(StoreError::Parse { .. })));
        let _ = fs::remove_dir_all(path.parent().unwrap_or(&path));
    }
}
