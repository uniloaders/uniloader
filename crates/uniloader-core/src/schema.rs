//! What a plugin registered, written by the loader for tools that cannot load the plugin.
//!
//! Settings, binds and commands only exist while a plugin runs: it registers them in its
//! start. A config editor running outside the game (on another OS, even) cannot ask the
//! plugin, so the loader writes one file per plugin, `config/schema/<plugin>.json`, every time
//! the plugin starts, and one for itself, `config/schema/loader.json`. The files describe the
//! plugin as it was when the game last ran; the values stay in `config/<plugin>.toml`.

use serde::{Deserialize, Serialize};

use crate::settings::Desc;

/// Directory under the config directory that holds the schema files.
pub const DIR: &str = "schema";

/// File name of the loader's own description, in [`DIR`].
pub const LOADER_FILE: &str = "loader.json";

/// Format version. A reader refuses files of another version rather than guess.
pub const FORMAT: u32 = 1;

/// Everything one plugin registered in its start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plugin {
    /// [`FORMAT`].
    pub format: u32,
    /// Unique name; also the stem of its settings file, `config/<name>.toml`.
    pub name: String,
    /// Human readable name.
    pub title: String,
    /// Version text.
    pub version: String,
    /// One-line description.
    pub description: String,
    /// The DLL it was loaded from, a file name in the plugin directory. `loader.toml`'s
    /// `disabled_plugins` lists the stems of these.
    pub file: String,
    /// The loader's own entry: always running, it cannot be disabled like a plugin.
    pub builtin: bool,
    /// Settings in registration order.
    pub settings: Vec<Setting>,
    /// Bind entries it suggests (`host.hotkey`, `host.bind_default`).
    pub binds: Vec<Bind>,
    /// Console commands it registered.
    pub commands: Vec<Command>,
}

/// One setting: the descriptor the loader validates against, and how it is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    /// The descriptor, as the plugin registered it.
    #[serde(flatten)]
    pub desc: Desc,
    /// Set when the file holds an Argon2id hash of what was set (a password the loader
    /// only verifies) instead of the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashed: Option<Hashed>,
}

/// How a hashed setting is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hashed {
    /// Fewest characters a value may have; shorter ones are refused.
    pub min_chars: usize,
}

/// A bind entry a plugin suggests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bind {
    /// Qualified name, `<plugin>.<name>`; what `rebind` takes.
    pub name: String,
    /// Short label.
    pub title: String,
    /// The suggested keys in the key grammar, `none` for none.
    pub default: String,
    /// The console script it runs, or `None` for the plugin's own hotkey action.
    pub script: Option<String>,
}

/// A console command a plugin registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Unqualified name; typed as `<plugin>.<name>`.
    pub name: String,
    /// One-line help.
    pub help: String,
    /// Argument syntax.
    pub usage: String,
}

/// The loader's description of the game it last ran in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loader {
    /// [`FORMAT`].
    pub format: u32,
    /// The loader's version.
    pub version: String,
    /// File name of the game's executable, for telling whether the game is running.
    pub game_exe: String,
}

/// Why a schema file was refused.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// Not JSON of the expected shape.
    #[error("not a uniloader schema: {0}")]
    Json(#[from] serde_json::Error),
    /// Written by a loader with another format.
    #[error("schema format {0}, this program reads format {FORMAT}")]
    Format(u32),
    /// A plugin name that is not a valid plugin name, so it cannot name a file.
    #[error("invalid plugin name {0:?}")]
    Name(String),
}

impl Plugin {
    /// Parse a plugin's schema file.
    ///
    /// # Errors
    /// See [`SchemaError`].
    pub fn parse(text: &str) -> Result<Self, SchemaError> {
        let plugin: Plugin = serde_json::from_str(text)?;
        if plugin.format != FORMAT {
            return Err(SchemaError::Format(plugin.format));
        }
        match crate::names::plugin_name(&plugin.name) {
            Ok(()) => Ok(plugin),
            Err(_) => Err(SchemaError::Name(plugin.name)),
        }
    }

    /// The file's text.
    ///
    /// # Errors
    /// Serialisation failure, which a well-formed value never hits.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// The setting with this key.
    #[must_use]
    pub fn setting(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|s| s.desc.key == key)
    }
}

impl Loader {
    /// Parse the loader's schema file.
    ///
    /// # Errors
    /// See [`SchemaError`].
    pub fn parse(text: &str) -> Result<Self, SchemaError> {
        let loader: Loader = serde_json::from_str(text)?;
        if loader.format != FORMAT {
            return Err(SchemaError::Format(loader.format));
        }
        Ok(loader)
    }

    /// The file's text.
    ///
    /// # Errors
    /// Serialisation failure, which a well-formed value never hits.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Kind;

    fn sample() -> Plugin {
        Plugin {
            format: FORMAT,
            name: "kitchen".into(),
            title: "Kitchen sink".into(),
            version: "1.0.0".into(),
            description: "Uses everything.".into(),
            file: "kitchen_sink.dll".into(),
            builtin: false,
            settings: vec![
                Setting {
                    desc: Desc {
                        key: "scale".into(),
                        title: "Scale".into(),
                        description: "How big.".into(),
                        kind: Kind::Float,
                        default: "1".into(),
                        min: 0.5,
                        max: 4.0,
                        choices: Vec::new(),
                        restart_required: false,
                        advanced: false,
                        transient: false,
                        secret: false,
                    },
                    hashed: None,
                },
                Setting {
                    desc: Desc {
                        key: "password".into(),
                        title: "Password".into(),
                        description: String::new(),
                        kind: Kind::String,
                        default: String::new(),
                        min: 0.0,
                        max: 0.0,
                        choices: Vec::new(),
                        restart_required: true,
                        advanced: true,
                        transient: false,
                        secret: true,
                    },
                    hashed: Some(Hashed { min_chars: 12 }),
                },
            ],
            binds: vec![Bind {
                name: "kitchen.peek".into(),
                title: "Peek".into(),
                default: "f8".into(),
                script: None,
            }],
            commands: vec![Command {
                name: "selftest".into(),
                help: "Run the self test.".into(),
                usage: String::new(),
            }],
        }
    }

    #[test]
    fn a_plugin_round_trips() {
        let plugin = sample();
        let text = plugin.to_json().unwrap_or_else(|e| panic!("{e}"));
        // The descriptor is flattened, and kinds are lower case words.
        assert!(text.contains("\"kind\": \"float\""), "{text}");
        assert!(text.contains("\"min_chars\": 12"), "{text}");
        assert_eq!(Plugin::parse(&text).ok(), Some(plugin.clone()));
        assert!(plugin.setting("password").is_some_and(|s| s.desc.secret));
        assert!(plugin.setting("nope").is_none());
    }

    #[test]
    fn foreign_formats_and_names_are_refused() {
        let mut plugin = sample();
        plugin.format = FORMAT + 1;
        let text = plugin.to_json().unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(Plugin::parse(&text), Err(SchemaError::Format(_))));
        let mut plugin = sample();
        plugin.name = "../evil".into();
        let text = plugin.to_json().unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(Plugin::parse(&text), Err(SchemaError::Name(_))));
        assert!(matches!(Plugin::parse("{}"), Err(SchemaError::Json(_))));
    }

    #[test]
    fn the_loader_file_round_trips() {
        let loader = Loader {
            format: FORMAT,
            version: "0.1.0".into(),
            game_exe: "TrainSimWorld.exe".into(),
        };
        let text = loader.to_json().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(Loader::parse(&text).ok(), Some(loader));
    }
}
