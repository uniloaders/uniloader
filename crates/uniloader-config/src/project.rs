//! One install opened for editing: every file the editor may change, what was read from
//! disk, and what saving writes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use uniloader_core::schema;

use crate::bind_table::{BindTable, Known};
use crate::files::Loaded;
use crate::loader_file::LoaderFile;
use crate::locate::Location;
use crate::plugins::{self, Plugin};
use crate::settings_file::SettingsFile;

/// A value edited in memory and the file it came from.
#[derive(Debug)]
pub struct Doc<T> {
    /// The file as read (or last saved).
    file: Loaded,
    /// The edited value.
    pub value: T,
    /// The value's text when it was read or saved; the value is changed when its text
    /// differs, so undoing an edit by hand leaves nothing to save.
    baseline: String,
}

/// Anything a [`Doc`] can hold: something that writes itself as text.
pub trait Text {
    /// The file's new text.
    fn text(&self) -> String;
}

impl Text for LoaderFile {
    fn text(&self) -> String {
        LoaderFile::text(self)
    }
}

impl Text for SettingsFile {
    fn text(&self) -> String {
        SettingsFile::text(self)
    }
}

impl Text for BindTable {
    fn text(&self) -> String {
        BindTable::text(self)
    }
}

impl<T: Text> Doc<T> {
    fn new(file: Loaded, value: T) -> Self {
        let baseline = value.text();
        Doc {
            file,
            value,
            baseline,
        }
    }

    /// Whether there is something to save.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.value.text() != self.baseline
    }

    /// Where it is saved.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.file.path
    }

    fn conflict(&self) -> bool {
        self.changed() && self.file.changed_on_disk()
    }

    fn save(&mut self) -> std::io::Result<bool> {
        if !self.changed() {
            return Ok(false);
        }
        let text = self.value.text();
        self.file.write(&text)?;
        self.baseline = text;
        Ok(true)
    }
}

/// A plugin's settings file, or why it cannot be edited.
pub type Settings = Result<Doc<SettingsFile>, String>;

/// Everything opened for one install.
#[derive(Debug)]
pub struct Project {
    /// Where it is.
    pub location: Location,
    /// The plugins, the loader's entry first.
    pub plugins: Vec<Plugin>,
    /// `.asi` files in the plugin folder.
    pub asi: Vec<String>,
    /// What the loader recorded about the game, if it wrote a schema.
    pub loader_schema: Option<schema::Loader>,
    /// `loader.toml`, or why it cannot be edited.
    pub loader: Result<Doc<LoaderFile>, String>,
    /// Each plugin's settings file, by plugin name.
    pub settings: BTreeMap<String, Settings>,
    /// `binds.cfg`.
    pub binds: Result<Doc<BindTable>, String>,
    /// Names the plugins registered.
    pub known: Known,
    /// Files that could not be read when opening, one line each.
    pub problems: Vec<String>,
}

impl Project {
    /// Read everything the editor shows for `location`.
    #[must_use]
    pub fn open(location: Location) -> Self {
        let found = plugins::discover(&location);
        let schemas: Vec<&schema::Plugin> = found
            .plugins
            .iter()
            .filter_map(|p| p.schema.as_ref())
            .collect();
        let known = Known::from_schemas(schemas.iter().copied());
        let loader = read(&location.loader_file(), |text| {
            LoaderFile::parse(text).map_err(|e| e.to_string())
        });
        let binds = read(&location.binds_file(), |text| {
            Ok(BindTable::load(text, schemas.iter().copied()))
        });
        let settings = found
            .plugins
            .iter()
            .map(|plugin| {
                let file = location.settings_file(&plugin.name);
                let doc = read(&file, |text| {
                    SettingsFile::parse(text).map_err(|e| e.to_string())
                });
                (plugin.name.clone(), doc)
            })
            .collect();
        Project {
            loader_schema: plugins::loader_schema(&location),
            location,
            asi: found.asi,
            plugins: found.plugins,
            loader,
            settings,
            binds,
            known,
            problems: found.problems,
        }
    }

    /// Whether anything is changed and not saved.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.docs().any(|(changed, _, _)| changed)
    }

    /// Changed files that someone else changed on disk since they were read.
    #[must_use]
    pub fn conflicts(&self) -> Vec<PathBuf> {
        self.docs()
            .filter(|(_, conflict, _)| *conflict)
            .map(|(_, _, path)| path)
            .collect()
    }

    /// (changed, conflicting, path) of every file that was read.
    fn docs(&self) -> impl Iterator<Item = (bool, bool, PathBuf)> + '_ {
        fn state<T: Text>(doc: &Doc<T>) -> (bool, bool, PathBuf) {
            (doc.changed(), doc.conflict(), doc.path().to_path_buf())
        }
        let loader = self.loader.as_ref().ok().map(state);
        let binds = self.binds.as_ref().ok().map(state);
        let settings = self
            .settings
            .values()
            .filter_map(|s| s.as_ref().ok().map(state));
        loader.into_iter().chain(binds).chain(settings)
    }

    /// Write every changed file. Returns the files written.
    ///
    /// # Errors
    /// The first file that could not be written, with the files written before it.
    pub fn save(&mut self) -> Result<Vec<PathBuf>, (Vec<PathBuf>, String)> {
        let mut written = Vec::new();
        let mut save = |result: std::io::Result<bool>, path: PathBuf| match result {
            Ok(true) => {
                written.push(path);
                Ok(())
            }
            Ok(false) => Ok(()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        };
        let mut outcome = Ok(());
        if let Ok(doc) = &mut self.loader {
            outcome = outcome.and_then(|()| save(doc.save(), doc.path().to_path_buf()));
        }
        if let Ok(doc) = &mut self.binds {
            outcome = outcome.and_then(|()| save(doc.save(), doc.path().to_path_buf()));
        }
        for doc in self.settings.values_mut().filter_map(|s| s.as_mut().ok()) {
            outcome = outcome.and_then(|()| save(doc.save(), doc.path().to_path_buf()));
        }
        match outcome {
            Ok(()) => Ok(written),
            Err(e) => Err((written, e)),
        }
    }
}

/// Read a file and parse it; a missing file parses as empty text.
fn read<T: Text>(
    path: &std::path::Path,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<Doc<T>, String> {
    let file = Loaded::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value = parse(&file.text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Doc::new(file, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::tests::{bare, write_schema};
    use crate::test_dir::TestDir;

    fn install(t: &TestDir) -> Location {
        let location = Location {
            config_dir: t.path().join("uniloader/config"),
            plugins_dir: t.path().join("plugins"),
        };
        TestDir::write(
            &location.plugins_dir.join("demo.dll"),
            uniloader_api::DESCRIBE_EXPORT,
        );
        write_schema(&location, &bare("demo", "demo.dll", false));
        TestDir::write(&location.loader_file(), "# mine\nasi = false\n");
        TestDir::write(&location.settings_file("demo"), "level = \"3\" # note\n");
        location
    }

    #[test]
    fn only_changed_files_are_written_and_comments_stay() {
        let t = TestDir::new("project");
        let location = install(&t);
        let mut project = Project::open(location.clone());
        assert!(project.problems.is_empty(), "{:?}", project.problems);
        assert!(!project.changed());
        // binds.cfg does not exist and nothing changed in it: not written.
        assert_eq!(project.save(), Ok(Vec::new()));
        assert!(!location.binds_file().exists());

        if let Ok(doc) = &mut project.loader {
            doc.value.set_disabled("demo", true);
        }
        if let Some(Ok(doc)) = project.settings.get_mut("demo") {
            doc.value.set("level", "4");
        }
        assert!(project.changed());
        let written = project.save().unwrap_or_else(|(_, e)| panic!("{e}"));
        assert_eq!(
            written,
            [location.loader_file(), location.settings_file("demo")]
        );
        assert!(!project.changed());
        assert_eq!(
            TestDir::read(&location.loader_file()),
            "# mine\nasi = false\ndisabled_plugins = [\"demo\"]\n"
        );
        assert_eq!(
            TestDir::read(&location.settings_file("demo")),
            "level = \"4\" # note\n"
        );
    }

    #[test]
    fn a_file_changed_behind_its_back_is_a_conflict() {
        let t = TestDir::new("conflict");
        let location = install(&t);
        let mut project = Project::open(location.clone());
        // Changed on disk, unchanged here: no conflict, nothing to lose.
        TestDir::write(&location.settings_file("demo"), "level = \"9\"\n");
        assert_eq!(project.conflicts(), Vec::<PathBuf>::new());
        if let Some(Ok(doc)) = project.settings.get_mut("demo") {
            doc.value.set("level", "5");
        }
        assert_eq!(project.conflicts(), [location.settings_file("demo")]);
    }

    #[test]
    fn an_unreadable_file_is_reported_not_overwritten() {
        let t = TestDir::new("broken");
        let location = install(&t);
        TestDir::write(&location.settings_file("demo"), "not toml [");
        let project = Project::open(location.clone());
        let error = project
            .settings
            .get("demo")
            .and_then(|s| s.as_ref().err())
            .cloned()
            .unwrap_or_default();
        assert!(error.contains("demo.toml"), "{error}");
    }
}
