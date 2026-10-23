//! `loader.toml`, edited in place so the user's comments and layout survive.
//!
//! The keys and their defaults are the loader's `LoaderConfig`
//! (`crates/uniloader/src/config.rs`). The loader refuses the whole file over one unknown key
//! or a value of the wrong type (and runs on defaults), so [`LoaderFile::problems`] reports
//! exactly those.

use toml_edit::{Array, DocumentMut, Item, Value};

/// The log levels the loader understands, quietest first.
pub const LOG_LEVELS: &[&str] = &["error", "warn", "info", "debug", "trace"];

/// Every key `loader.toml` may have.
const KEYS: &[&str] = &[
    "enabled",
    "disabled_plugins",
    "log_level",
    "asi",
    "load",
    "game_keys",
];

/// A parsed `loader.toml`.
#[derive(Debug, Clone)]
pub struct LoaderFile {
    doc: DocumentMut,
}

impl LoaderFile {
    /// Parse the file's text (empty for a missing file).
    ///
    /// # Errors
    /// The text is not TOML.
    pub fn parse(text: &str) -> Result<Self, toml_edit::TomlError> {
        Ok(LoaderFile { doc: text.parse()? })
    }

    /// The text to write.
    #[must_use]
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// `enabled`: `false` makes the loader forward the system DLL and load nothing.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.bool("enabled", true)
    }

    /// `asi`: load `plugins/*.asi`.
    #[must_use]
    pub fn asi(&self) -> bool {
        self.bool("asi", true)
    }

    /// `log_level`.
    #[must_use]
    pub fn log_level(&self) -> String {
        self.doc
            .get("log_level")
            .and_then(Item::as_str)
            .unwrap_or("info")
            .to_owned()
    }

    fn bool(&self, key: &str, default: bool) -> bool {
        self.doc.get(key).and_then(Item::as_bool).unwrap_or(default)
    }

    /// Set a boolean key.
    pub fn set_bool(&mut self, key: &str, value: bool) {
        self.doc[key] = toml_edit::value(value);
    }

    /// Set `log_level`.
    pub fn set_log_level(&mut self, level: &str) {
        self.doc["log_level"] = toml_edit::value(level);
    }

    /// A list of strings: `disabled_plugins`, `load` or `game_keys`.
    #[must_use]
    pub fn list(&self, key: &str) -> Vec<String> {
        self.doc
            .get(key)
            .and_then(Item::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replace a list, keeping the array's formatting when it exists. An empty list removes
    /// the key.
    pub fn set_list(&mut self, key: &str, values: &[String]) {
        if values.is_empty() {
            self.doc.remove(key);
            return;
        }
        if let Some(array) = self.doc.get_mut(key).and_then(Item::as_array_mut) {
            array.clear();
            array.extend(values.iter().map(String::as_str));
            return;
        }
        let array: Array = values.iter().map(String::as_str).collect();
        self.doc[key] = toml_edit::value(array);
    }

    /// Whether the plugin DLL with this file stem is on the disable list.
    #[must_use]
    pub fn is_disabled(&self, stem: &str) -> bool {
        self.list("disabled_plugins")
            .iter()
            .any(|d| d.eq_ignore_ascii_case(stem))
    }

    /// Put a plugin on the disable list or take it off.
    pub fn set_disabled(&mut self, stem: &str, disabled: bool) {
        let mut list: Vec<String> = self
            .list("disabled_plugins")
            .into_iter()
            .filter(|d| !d.eq_ignore_ascii_case(stem))
            .collect();
        if disabled {
            list.push(stem.to_owned());
        }
        self.set_list("disabled_plugins", &list);
    }

    /// What would make the loader ignore the file: unknown keys and values of the wrong
    /// type. One line each.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (key, item) in self.doc.iter() {
            let fits = match key {
                "enabled" | "asi" => item.as_bool().is_some(),
                "log_level" => item.as_str().is_some(),
                "disabled_plugins" | "load" | "game_keys" => item
                    .as_array()
                    .is_some_and(|a| a.iter().all(|v| v.as_str().is_some())),
                _ => {
                    problems.push(format!(
                        "loader.toml: unknown key {key:?} (the loader only knows {})",
                        KEYS.join(", ")
                    ));
                    continue;
                }
            };
            if !fits {
                problems.push(format!("loader.toml: {key} has a value of the wrong type"));
            }
        }
        let level = self.log_level();
        if !LOG_LEVELS.contains(&level.as_str()) {
            problems.push(format!(
                "loader.toml: log_level {level:?} is not one of {}; the loader uses info",
                LOG_LEVELS.join(", ")
            ));
        }
        for key in self.list("game_keys") {
            if let Err(e) = uniloader_core::keys::parse_list(&key) {
                problems.push(format!("loader.toml: game_keys: {e}"));
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> LoaderFile {
        LoaderFile::parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn defaults_match_the_loader() {
        let f = file("");
        assert!(f.enabled());
        assert!(f.asi());
        assert_eq!(f.log_level(), "info");
        assert_eq!(f.list("disabled_plugins"), Vec::<String>::new());
        assert_eq!(f.problems(), Vec::<String>::new());
    }

    #[test]
    fn disabling_keeps_comments_and_other_keys() {
        let mut f = file(
            "# Written by deploy.sh: UE4SS runs alongside uniloader.\n\
             load = [\"ue4ss/UE4SS.dll\"] # keep\n",
        );
        f.set_disabled("jump", true);
        f.set_disabled("Coop", true);
        assert!(f.is_disabled("JUMP"));
        assert!(f.is_disabled("coop"));
        f.set_disabled("jump", false);
        let text = f.text();
        assert!(text.starts_with("# Written by deploy.sh"), "{text}");
        assert!(
            text.contains("load = [\"ue4ss/UE4SS.dll\"] # keep"),
            "{text}"
        );
        assert!(text.contains("disabled_plugins = [\"Coop\"]"), "{text}");
        f.set_disabled("coop", false);
        assert!(!f.text().contains("disabled_plugins"), "{}", f.text());
    }

    #[test]
    fn values_round_trip() {
        let mut f = file("");
        f.set_bool("enabled", false);
        f.set_bool("asi", false);
        f.set_log_level("debug");
        f.set_list("game_keys", &["f1".into(), "ctrl+f12".into()]);
        let again = file(&f.text());
        assert!(!again.enabled());
        assert!(!again.asi());
        assert_eq!(again.log_level(), "debug");
        assert_eq!(again.list("game_keys"), ["f1", "ctrl+f12"]);
        assert!(again.problems().is_empty(), "{:?}", again.problems());
    }

    #[test]
    fn what_the_loader_would_refuse_is_reported() {
        let f =
            file("typo = 1\nenabled = \"yes\"\nlog_level = \"loud\"\ngame_keys = [\"nokey\"]\n");
        let problems = f.problems();
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems[0].contains("unknown key \"typo\""));
        assert!(problems[1].contains("enabled"));
        assert!(problems[2].contains("loud"));
        assert!(problems[3].contains("nokey"));
    }
}
