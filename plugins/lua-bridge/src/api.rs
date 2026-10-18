//! The `uniloader` table scripts see.
//!
//! | Function | Does |
//! | --- | --- |
//! | `uniloader.log(msg)`, `.warn(msg)`, `.error(msg)` | Write to the loader log. |
//! | `print(...)` | Print to the loader console, tab separated like stock Lua. |
//! | `uniloader.exec(line)` | Run a console line; returns `ok, {lines}`. |
//! | `uniloader.get(name)`, `.set(name, value)` | Read or write any plugin's setting as text. |
//! | `uniloader.setting(key, title, default [, fn])` | Register a setting; the type follows `default`. `fn(value)` runs when it changes. |
//! | `uniloader.hotkey(action, title, binding, fn)` | Register a hotkey; `fn()` runs on press. |
//! | `uniloader.command(name, help, usage, fn)` | Register `lua.<name>`; `fn(args)` runs it. |
//! | `uniloader.subscribe(topic, fn)` | `fn(payload, topic)` for every broadcast on `topic`. |
//! | `uniloader.publish(topic, payload)` | Broadcast to every subscriber. |
//! | `uniloader.game_dir`, `.version` | The game directory and the loader version. |
//!
//! Registration functions only work while the scripts first run, which is inside the bridge's
//! own `start`; later they fail with the loader's "wrong phase" error.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use mlua::{Function, Lua, MultiValue, Table, Value, Variadic};
use uniloader_sdk::{Host, Setting};

/// Callbacks registered by scripts, by unqualified name.
///
/// Locked only to look a function up; the call itself happens after the guard is gone, so a
/// callback that runs a console line which lands back in this plugin cannot deadlock.
#[derive(Default)]
pub(crate) struct Handlers {
    hotkeys: Mutex<BTreeMap<String, Function>>,
    commands: Mutex<BTreeMap<String, Function>>,
    settings: Mutex<BTreeMap<String, Function>>,
    events: Mutex<BTreeMap<String, Vec<Function>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Handlers {
    pub(crate) fn hotkey(&self, action: &str) -> Option<Function> {
        lock(&self.hotkeys).get(action).cloned()
    }

    pub(crate) fn command(&self, name: &str) -> Option<Function> {
        lock(&self.commands).get(name).cloned()
    }

    pub(crate) fn setting(&self, key: &str) -> Option<Function> {
        lock(&self.settings).get(key).cloned()
    }

    pub(crate) fn subscribers(&self, topic: &str) -> Vec<Function> {
        lock(&self.events).get(topic).cloned().unwrap_or_default()
    }
}

/// A host error as a Lua error, so a script can `pcall` it.
fn lua_err(e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(e.to_string())
}

/// The setting a Lua default describes: its type picks the kind.
fn setting_for(key: &str, title: &str, default: &Value) -> mlua::Result<Setting> {
    Ok(match default {
        Value::Boolean(b) => Setting::bool(key, title, *b),
        Value::Integer(i) => Setting::int(key, title, *i, i64::MIN, i64::MAX),
        Value::Number(n) => Setting::float(key, title, *n, f64::MIN, f64::MAX),
        Value::String(s) => Setting::text(key, title, &s.to_str()?),
        other => {
            return Err(lua_err(format!(
                "setting {key}: default must be a boolean, number or string, not {}",
                other.type_name()
            )))
        }
    })
}

/// Logging and printing.
fn install_output(lua: &Lua, table: &Table, host: Host) -> mlua::Result<()> {
    table.set(
        "log",
        lua.create_function(|_, msg: String| {
            log::info!("{msg}");
            Ok(())
        })?,
    )?;
    table.set(
        "warn",
        lua.create_function(|_, msg: String| {
            log::warn!("{msg}");
            Ok(())
        })?,
    )?;
    table.set(
        "error",
        lua.create_function(|_, msg: String| {
            log::error!("{msg}");
            Ok(())
        })?,
    )?;
    let print = lua.create_function(move |_, values: MultiValue| {
        let parts: Vec<String> = values
            .iter()
            .map(|v| v.to_string().unwrap_or_else(|_| v.type_name().to_owned()))
            .collect();
        host.console_print(&parts.join("\t"));
        Ok(())
    })?;
    lua.globals().set("print", print)
}

/// Console lines and settings of any plugin.
fn install_console(lua: &Lua, table: &Table, host: Host) -> mlua::Result<()> {
    table.set(
        "exec",
        lua.create_function(move |lua, line: String| {
            let (status, lines) = host.console_capture(&line);
            Ok((status.is_ok(), lua.create_sequence_from(lines)?))
        })?,
    )?;
    table.set(
        "get",
        lua.create_function(move |_, name: String| host.get(&name).map_err(lua_err))?,
    )?;
    table.set(
        "set",
        lua.create_function(move |_, (name, value): (String, Value)| {
            let text = value.to_string()?;
            host.set(&name, &text).map_err(lua_err)
        })?,
    )
}

/// Registrations: settings, hotkeys, commands and subscriptions.
fn install_registrations(
    lua: &Lua,
    table: &Table,
    host: Host,
    handlers: &Arc<Handlers>,
) -> mlua::Result<()> {
    let h = Arc::clone(handlers);
    table.set(
        "setting",
        lua.create_function(
            move |_, (key, title, default, changed): (String, String, Value, Option<Function>)| {
                host.setting(&setting_for(&key, &title, &default)?)
                    .map_err(lua_err)?;
                if let Some(f) = changed {
                    lock(&h.settings).insert(key, f);
                }
                Ok(())
            },
        )?,
    )?;
    let h = Arc::clone(handlers);
    table.set(
        "hotkey",
        lua.create_function(
            move |_, (action, title, binding, f): (String, String, String, Function)| {
                host.hotkey(&action, &title, &binding).map_err(lua_err)?;
                lock(&h.hotkeys).insert(action, f);
                Ok(())
            },
        )?,
    )?;
    let h = Arc::clone(handlers);
    table.set(
        "command",
        lua.create_function(
            move |_, (name, help, usage, f): (String, String, String, Function)| {
                host.command(&name, &help, &usage).map_err(lua_err)?;
                lock(&h.commands).insert(name, f);
                Ok(())
            },
        )?,
    )?;
    let h = Arc::clone(handlers);
    table.set(
        "subscribe",
        lua.create_function(move |_, (topic, f): (String, Function)| {
            // One host subscription per topic; every script's function hangs off it.
            let first = !lock(&h.events).contains_key(&topic);
            if first {
                host.subscribe(&topic).map_err(lua_err)?;
            }
            lock(&h.events).entry(topic).or_default().push(f);
            Ok(())
        })?,
    )?;
    table.set(
        "publish",
        lua.create_function(
            move |_, (topic, payload): (String, Variadic<mlua::String>)| {
                let bytes = payload
                    .first()
                    .map(|s| s.as_bytes().to_vec())
                    .unwrap_or_default();
                host.publish(&topic, &bytes);
                Ok(())
            },
        )?,
    )
}

/// Create the `uniloader` global and replace `print`.
pub(crate) fn install(lua: &Lua, host: Host, handlers: &Arc<Handlers>) -> mlua::Result<()> {
    let table = lua.create_table()?;
    install_output(lua, &table, host)?;
    install_console(lua, &table, host)?;
    install_registrations(lua, &table, host, handlers)?;
    table.set("game_dir", host.game_dir().to_string_lossy().into_owned())?;
    table.set("version", host.loader_version())?;
    lua.globals().set("uniloader", table)
}
