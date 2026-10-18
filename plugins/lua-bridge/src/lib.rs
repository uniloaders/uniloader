//! The Lua bridge: a uniloader plugin that runs `plugins/lua/*.lua`.
//!
//! Every script runs once, in file name order, inside this plugin's `start`, which is the one
//! moment registrations are allowed; so a script registers its settings, hotkeys, commands and
//! subscriptions at its top level, exactly like a native plugin does in `start`. Everything a
//! script registers lives in this plugin's namespace: a hotkey `greet` is `lua.greet`, a
//! setting `speed` is `lua.speed`.
//!
//! The API is the global table `uniloader`; see [`api`] for the list. A script that fails to
//! load is logged and skipped, and an error inside a callback is logged without disabling the
//! bridge, so one broken script cannot take the others down.

pub mod api;

use std::path::{Path, PathBuf};

use mlua::{Function, Lua};
use uniloader_sdk::{export_plugin, Host, Plugin, PluginError, PluginRef};

use api::Handlers;

/// The Lua state and the callbacks scripts registered.
pub struct LuaBridge {
    lua: Lua,
    handlers: std::sync::Arc<Handlers>,
}

/// `*.lua` in `dir`, sorted, so the order scripts run in is reproducible.
fn scripts(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lua")))
        .collect();
    files.sort();
    files
}

/// Run one script, logging rather than propagating a failure.
fn run_script(lua: &Lua, file: &Path) -> bool {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let source = match std::fs::read(file) {
        Ok(source) => source,
        Err(e) => {
            log::error!("{name}: {e}");
            return false;
        }
    };
    match lua.load(source).set_name(format!("@{name}")).exec() {
        Ok(()) => {
            log::info!("ran {name}");
            true
        }
        Err(e) => {
            log::error!("{name}: {e}");
            false
        }
    }
}

/// Call one Lua callback, logging an error instead of propagating it.
fn call(what: &str, function: Option<Function>, args: impl mlua::IntoLuaMulti) {
    if let Some(function) = function {
        if let Err(e) = function.call::<()>(args) {
            log::error!("{what}: {e}");
        }
    }
}

impl Plugin for LuaBridge {
    const NAME: &'static str = "lua";
    const TITLE: &'static str = "Lua bridge";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const AUTHORS: &'static str = "Bluscream";
    const DESCRIPTION: &'static str = "Runs Lua scripts from plugins/lua/ as plugins.";

    fn start(host: Host) -> Result<Self, PluginError> {
        let lua = Lua::new();
        let handlers = std::sync::Arc::new(Handlers::default());
        api::install(&lua, host, &handlers)
            .map_err(|e| PluginError::from(format!("cannot set up the Lua API: {e}")))?;
        let dir = host.plugin_dir().join("lua");
        let files = scripts(&dir);
        if files.is_empty() {
            log::info!("no scripts in {}", dir.display());
        }
        let ran = files.iter().filter(|f| run_script(&lua, f)).count();
        log::info!("{ran} of {} scripts ran", files.len());
        Ok(LuaBridge { lua, handlers })
    }

    fn on_hotkey(&self, _host: &Host, action: &str) {
        call(
            &format!("hotkey {action}"),
            self.handlers.hotkey(action),
            (),
        );
    }

    fn on_command(&self, _host: &Host, name: &str, args: &str) -> Result<(), PluginError> {
        let Some(function) = self.handlers.command(name) else {
            return Err(PluginError::from(format!("no script handles {name}")));
        };
        function
            .call::<()>(args)
            .map_err(|e| PluginError::from(format!("{name}: {e}")))
    }

    fn on_setting_changed(&self, _host: &Host, key: &str, value: &str) {
        call(&format!("setting {key}"), self.handlers.setting(key), value);
    }

    fn on_event(&self, _host: &Host, _from: PluginRef, topic: &str, payload: &[u8]) {
        let Ok(payload) = self.lua.create_string(payload) else {
            return;
        };
        for function in self.handlers.subscribers(topic) {
            call(
                &format!("event {topic}"),
                Some(function),
                (payload.clone(), topic),
            );
        }
    }
}

export_plugin!(LuaBridge);
