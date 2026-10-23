//! Which plugins an install has: the DLLs in `plugins/` the loader would load, matched with
//! the schema files the loader wrote the last time each one started.

use std::path::{Path, PathBuf};

use uniloader_core::schema;

use crate::locate::Location;

/// One plugin as the editor lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Plugin {
    /// Its name: the stem of its settings file. The schema's name, or the DLL's stem when
    /// the plugin has never started.
    pub name: String,
    /// The DLL's file stem, which `loader.toml`'s `disabled_plugins` lists; `None` for the
    /// loader's own entry and for a plugin whose DLL is gone.
    pub stem: Option<String>,
    /// What it registered when it last started.
    pub schema: Option<schema::Plugin>,
}

impl Plugin {
    /// The name to show.
    #[must_use]
    pub fn title(&self) -> &str {
        self.schema
            .as_ref()
            .map(|s| s.title.as_str())
            .filter(|t| !t.is_empty())
            .unwrap_or(&self.name)
    }

    /// The loader's own entry, which is switched off with `loader.toml`'s `enabled` rather
    /// than the disable list.
    #[must_use]
    pub fn builtin(&self) -> bool {
        self.schema.as_ref().is_some_and(|s| s.builtin)
    }
}

/// What [`discover`] found.
#[derive(Debug, Default)]
pub struct Found {
    /// Plugins: the loader's entry first, then by name.
    pub plugins: Vec<Plugin>,
    /// `.asi` files in the plugin folder, loaded when `loader.toml` says `asi = true`.
    pub asi: Vec<String>,
    /// Schema files that could not be read, one line each.
    pub problems: Vec<String>,
}

/// DLLs in `dir` that claim to be plugins, the way the loader decides it without loading
/// them: the describe export's name appears in the file. Alphabetical.
pub fn plugin_files(dir: &Path) -> impl Iterator<Item = PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| has_extension(p, "dll") && claims_to_be_a_plugin(p))
        .collect();
    files.sort();
    files.into_iter()
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

fn claims_to_be_a_plugin(file: &Path) -> bool {
    let export = uniloader_api::DESCRIBE_EXPORT.as_bytes();
    std::fs::read(file).is_ok_and(|bytes| bytes.windows(export.len()).any(|w| w == export))
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Read the schema files and the plugin folder of `location`.
#[must_use]
pub fn discover(location: &Location) -> Found {
    let mut found = Found::default();
    let mut schemas = read_schemas(&location.schema_dir(), &mut found.problems);
    for file in plugin_files(&location.plugins_dir) {
        let stem = stem(&file);
        let schema = schemas
            .iter()
            .position(|s| stem.eq_ignore_ascii_case(&self::stem(Path::new(&s.file))))
            .map(|i| schemas.remove(i));
        found.plugins.push(Plugin {
            name: schema
                .as_ref()
                .map_or_else(|| stem.clone(), |s| s.name.clone()),
            stem: Some(stem),
            schema,
        });
    }
    // Left over: the loader's entry, and plugins whose DLL was removed or renamed since.
    found
        .plugins
        .extend(schemas.into_iter().map(|schema| Plugin {
            name: schema.name.clone(),
            stem: None,
            schema: Some(schema),
        }));
    found
        .plugins
        .sort_by(|a, b| (!a.builtin(), &a.name).cmp(&(!b.builtin(), &b.name)));
    found.asi = std::fs::read_dir(&location.plugins_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| has_extension(p, "asi"))
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    found.asi.sort();
    found
}

fn read_schemas(dir: &Path, problems: &mut Vec<String>) -> Vec<schema::Plugin> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| has_extension(p, "json"))
        .filter(|p| p.file_name().is_some_and(|n| n != schema::LOADER_FILE))
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|file| {
            let parsed = std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| schema::Plugin::parse(&text).map_err(|e| e.to_string()));
            parsed
                .map_err(|e| problems.push(format!("{}: {e}", file.display())))
                .ok()
        })
        .collect()
}

/// The loader's schema file, if the game ran with a loader that writes one.
#[must_use]
pub fn loader_schema(location: &Location) -> Option<schema::Loader> {
    let text = std::fs::read_to_string(location.schema_dir().join(schema::LOADER_FILE)).ok()?;
    schema::Loader::parse(&text).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::test_dir::TestDir;

    /// A schema with no settings, binds or commands.
    pub fn bare(name: &str, file: &str, builtin: bool) -> schema::Plugin {
        schema::Plugin {
            format: schema::FORMAT,
            name: name.into(),
            title: String::new(),
            version: "1.0".into(),
            description: String::new(),
            file: file.into(),
            builtin,
            settings: Vec::new(),
            binds: Vec::new(),
            commands: Vec::new(),
        }
    }

    pub fn write_schema(location: &Location, schema: &schema::Plugin) {
        TestDir::write(
            &location.schema_dir().join(format!("{}.json", schema.name)),
            &schema.to_json().unwrap_or_else(|e| panic!("{e}")),
        );
    }

    #[test]
    fn plugins_are_matched_with_their_schemas_by_file() {
        let t = TestDir::new("discover");
        let location = Location {
            config_dir: t.path().join("uniloader/config"),
            plugins_dir: t.path().join("plugins"),
        };
        let export = uniloader_api::DESCRIBE_EXPORT;
        TestDir::write(&location.plugins_dir.join("Coop.dll"), export);
        TestDir::write(&location.plugins_dir.join("new.dll"), export);
        TestDir::write(&location.plugins_dir.join("helper.dll"), "MZ no export");
        TestDir::write(&location.plugins_dir.join("old.asi"), "");
        write_schema(&location, &bare("tsw-coop", "coop.dll", false));
        write_schema(&location, &bare("uniloader", "dwmapi.dll", true));
        write_schema(&location, &bare("gone", "gone.dll", false));
        TestDir::write(&location.schema_dir().join("broken.json"), "{");
        TestDir::write(
            &location.schema_dir().join(schema::LOADER_FILE),
            "{\"format\":1,\"version\":\"0.1.0\",\"game_exe\":\"Game.exe\"}",
        );

        let found = discover(&location);
        let listed: Vec<(&str, Option<&str>, bool)> = found
            .plugins
            .iter()
            .map(|p| (p.name.as_str(), p.stem.as_deref(), p.schema.is_some()))
            .collect();
        assert_eq!(
            listed,
            [
                ("uniloader", None, true),
                ("gone", None, true),
                ("new", Some("new"), false),
                ("tsw-coop", Some("Coop"), true),
            ]
        );
        assert!(found.plugins[0].builtin());
        assert_eq!(found.asi, ["old.asi"]);
        assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
        assert!(found.problems[0].contains("broken.json"));
        assert_eq!(
            loader_schema(&location).map(|l| l.game_exe).as_deref(),
            Some("Game.exe")
        );
    }
}
