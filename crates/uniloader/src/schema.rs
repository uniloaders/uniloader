//! The schema files config editors read: what each plugin registered, written whenever a
//! plugin has started, and the loader's own description (see [`uniloader_core::schema`]).

use std::path::Path;

use uniloader_api::PluginHandle;
use uniloader_core::schema;

use crate::config::Paths;
use crate::state::State;

/// What `plugin` registered, as its schema file says it. `builtin` marks the loader's own
/// entry. `None` for an unknown handle.
#[must_use]
pub fn plugin(state: &State, handle: PluginHandle, builtin: bool) -> Option<schema::Plugin> {
    let record = state.plugin(handle)?;
    let name = record.name();
    let settings = record
        .settings
        .iter()
        .map(|(desc, _)| schema::Setting {
            desc: desc.clone(),
            hashed: state
                .hashed
                .contains(&format!("{name}.{}", desc.key))
                .then_some(schema::Hashed {
                    min_chars: crate::rcon::MIN_PASSWORD,
                }),
        })
        .collect();
    let binds = state
        .binds
        .entries()
        .filter(|entry| entry.owner() == name)
        .map(|entry| schema::Bind {
            name: entry.name.clone(),
            title: entry.title.clone(),
            default: entry.default_binding(),
            script: match &entry.target {
                uniloader_core::binds::Target::Hotkey => None,
                uniloader_core::binds::Target::Script(script) => Some(script.clone()),
            },
        })
        .collect();
    let commands = record
        .commands
        .iter()
        .map(|(command, info)| schema::Command {
            name: command.clone(),
            help: info.help.clone(),
            usage: info.usage.clone(),
        })
        .collect();
    Some(schema::Plugin {
        format: schema::FORMAT,
        name: name.to_owned(),
        title: record.info.title.clone(),
        version: record.info.version.clone(),
        description: record.info.description.clone(),
        file: record.file.clone(),
        builtin,
        settings,
        binds,
        commands,
    })
}

/// The loader's own schema file for a game whose executable is called `game_exe`.
#[must_use]
pub fn loader(game_exe: &str) -> schema::Loader {
    schema::Loader {
        format: schema::FORMAT,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        game_exe: game_exe.to_owned(),
    }
}

/// Write `plugin`'s schema file. Called without the state lock held; a failure is logged,
/// since an editor without the file still edits the values as plain text.
pub fn save_plugin(paths: &Paths, plugin: &schema::Plugin) {
    let file = paths.schema_dir().join(format!("{}.json", plugin.name));
    save(&file, plugin.to_json());
}

/// Write the loader's schema file; see [`save_plugin`].
pub fn save_loader(paths: &Paths, loader: &schema::Loader) {
    save(
        &paths.schema_dir().join(schema::LOADER_FILE),
        loader.to_json(),
    );
}

fn save(file: &Path, text: Result<String, impl std::fmt::Display>) {
    let written = text
        .map_err(|e| std::io::Error::other(e.to_string()))
        .and_then(|text| crate::state::write_atomic(file, &text));
    if let Err(e) = written {
        log::warn!("could not write {}: {e}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LoaderConfig;
    use crate::described::Described;
    use crate::state::{Builtin, CommandInfo, Phase};
    use uniloader_core::settings::{Desc, Kind};

    fn desc(key: &str, kind: Kind, default: &str) -> Desc {
        Desc {
            key: key.into(),
            title: key.into(),
            description: String::new(),
            kind,
            default: default.into(),
            min: 0.0,
            max: 0.0,
            choices: Vec::new(),
            restart_required: false,
            advanced: false,
            transient: false,
            secret: false,
        }
    }

    #[test]
    fn a_plugin_schema_lists_what_it_registered() {
        let dir = std::env::temp_dir().join(format!("uniloader-schema-{}", std::process::id()));
        let paths = Paths::from_game_dir(&dir);
        let mut state = State::new(paths.clone(), LoaderConfig::default());
        let info = Described {
            name: "demo".into(),
            title: "Demo".into(),
            version: "1.2.3".into(),
            ..Described::default()
        };
        let builtin = Builtin {
            file: "demo.dll".into(),
            settings: vec![
                desc("level", Kind::Int, "3"),
                Desc {
                    secret: true,
                    ..desc("password", Kind::String, "")
                },
            ],
            hotkeys: vec![("peek".into(), "Peek".into(), "f8".into())],
            commands: vec![(
                "hello".into(),
                CommandInfo {
                    help: "Say hello.".into(),
                    usage: "[name]".into(),
                },
            )],
            hashed: vec!["password".into()],
        };
        let handle = state
            .add_builtin(info, builtin)
            .unwrap_or_else(|e| panic!("{e:?}"));
        state.phase = Phase::Running;
        let schema = plugin(&state, handle, false).unwrap_or_else(|| panic!("no schema"));
        assert_eq!(schema.name, "demo");
        assert_eq!(schema.file, "demo.dll");
        assert_eq!(schema.version, "1.2.3");
        assert_eq!(schema.settings.len(), 2);
        assert_eq!(schema.settings[0].desc.default, "3");
        assert_eq!(schema.settings[0].hashed, None);
        assert_eq!(
            schema.settings[1].hashed,
            Some(schema::Hashed {
                min_chars: crate::rcon::MIN_PASSWORD
            })
        );
        assert_eq!(
            schema.binds,
            [schema::Bind {
                name: "demo.peek".into(),
                title: "Peek".into(),
                default: "f8".into(),
                script: None,
            }]
        );
        assert_eq!(schema.commands[0].name, "hello");
        assert_eq!(schema.commands[0].usage, "[name]");

        save_plugin(&paths, &schema);
        save_loader(&paths, &loader("Game.exe"));
        let read = |name: &str| {
            std::fs::read_to_string(paths.schema_dir().join(name)).unwrap_or_else(|e| panic!("{e}"))
        };
        assert_eq!(schema::Plugin::parse(&read("demo.json")).ok(), Some(schema));
        assert_eq!(
            schema::Loader::parse(&read("loader.json"))
                .map(|l| l.game_exe)
                .ok(),
            Some("Game.exe".to_owned())
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
