//! The `plugin load|stop|enable|disable` console operations.
//!
//! These are the lifecycle changes that are actually sound in-process:
//!
//! - **load** appends a plugin the loader did not start at launch, after the same dependency
//!   checks `load_all` applies, and pulls in whatever that plugin depends on first. A plugin
//!   built while the game runs can be tried without restarting it, as long as the DLL has a
//!   file name nothing loaded before.
//! - **stop** calls the plugin's stop export with [`StopReason::UNLOAD`], tears down the hooks
//!   it registered and stops delivering callbacks. The DLL stays in the process:
//!   `FreeLibrary` would pull the ground out from under the threads it started and every
//!   pointer the loader and other plugins hold.
//! - **enable** and **disable** pause and resume delivery, telling the plugin through
//!   `on_enable` and `on_disable`, which is the cheap way to take a misbehaving plugin out of
//!   the game without losing its state.
//!
//! Nothing here runs under the state lock while a plugin is being called, so a plugin may
//! issue these from its own command handler.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use uniloader_api::{Status, StopReason};
use uniloader_core::console::PluginOp;
use uniloader_core::deps::PluginReq;

use super::{deps as external, plugins, state};

/// How deep `plugin load` follows dependencies before deciding something is wrong.
///
/// Dependency cycles are caught by name, so this only bounds pathological chains; a plugin
/// graph that is 16 deep is a mistake either way.
const MAX_DEPTH: usize = 16;

/// Serialises lifecycle operations across threads: each saves and restores the single
/// `Phase`, so two at once (the console thread and a plugin's `console_exec` on the game
/// thread) would leave it wrong. Separate from the state lock, which is never held while a
/// plugin runs; this one is, so a nested operation on the same thread (a plugin's `start`
/// issuing `plugin load`) runs inside the outer one instead of waiting for it.
static LIFECYCLE: Mutex<()> = Mutex::new(());

thread_local! {
    /// Whether this thread is inside [`serialised`] already.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` as the only lifecycle operation in progress.
pub(super) fn serialised<T>(f: impl FnOnce() -> T) -> T {
    if INSIDE.with(Cell::get) {
        return f();
    }
    let _lock = LIFECYCLE.lock().unwrap_or_else(PoisonError::into_inner);
    INSIDE.with(|inside| inside.set(true));
    let _leave = Leave;
    f()
}

/// Perform one lifecycle operation. Returns the status and the lines to print.
///
/// Asked for from the console or by a plugin, so it does not wait for another operation
/// running on another thread: that one runs plugin code, which may be waiting for this
/// thread (the game or render thread). It answers busy instead; nested on the same thread
/// it runs inside the outer one, as [`serialised`] does.
pub(crate) fn perform(op: PluginOp, name: &str) -> (Status, Vec<String>) {
    if INSIDE.with(Cell::get) {
        return perform_now(op, name);
    }
    let _lock = match LIFECYCLE.try_lock() {
        Ok(lock) => lock,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return (
                Status::WRONG_PHASE,
                vec!["another plugin operation is in progress; try again".to_owned()],
            );
        }
    };
    INSIDE.with(|inside| inside.set(true));
    let _leave = Leave;
    perform_now(op, name)
}

/// Clears [`INSIDE`] however the operation ends.
struct Leave;

impl Drop for Leave {
    fn drop(&mut self) {
        INSIDE.with(|inside| inside.set(false));
    }
}

/// [`perform`] inside the lifecycle lock.
fn perform_now(op: PluginOp, name: &str) -> (Status, Vec<String>) {
    match op {
        PluginOp::Load => {
            let mut lines = Vec::new();
            let mut visiting = Vec::new();
            let status = match load(name, &mut visiting, &mut lines, 0) {
                Ok(()) => Status::OK,
                Err(e) => {
                    lines.push(e);
                    Status::ERROR
                }
            };
            // Loading one plugin can be what others were waiting for.
            for started in plugins::start_pending() {
                lines.push(format!("started {started}, which was waiting"));
            }
            (status, lines)
        }
        PluginOp::Stop => stop(name),
        PluginOp::Enable | PluginOp::Disable => delivery(op, name),
        // The console answers the rest without reaching the platform layer.
        PluginOp::List | PluginOp::Deps | PluginOp::Reload => (Status::UNSUPPORTED, Vec::new()),
    }
}

/// Resolve `name` to a DLL directly in the plugin directory, accepting it with or without
/// `.dll`, under the same rules `load_all` applies: a bare file name (no path, so a console
/// line cannot load a DLL from anywhere else), not on the disable list, and claiming to be a
/// plugin, so a helper DLL next to the plugins never has its `DllMain` run.
fn plugin_file(name: &str) -> Result<PathBuf, String> {
    let (dir, disabled) = {
        let guard = state();
        (
            guard.paths.plugins_dir.clone(),
            guard.config.disabled_plugins.clone(),
        )
    };
    if !is_bare_file_name(name) {
        return Err(format!("{name:?} is not a plain plugin name"));
    }
    let stem = name
        .get(..name.len().saturating_sub(4))
        .filter(|_| name.to_ascii_lowercase().ends_with(".dll"))
        .unwrap_or(name);
    if disabled.iter().any(|d| d.eq_ignore_ascii_case(stem)) {
        return Err(format!("{stem} is disabled in loader.toml"));
    }
    let file = [dir.join(name), dir.join(format!("{name}.dll"))]
        .into_iter()
        .find(|f| f.is_file() && f.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
        .ok_or_else(|| format!("no {stem}.dll in {}", dir.display()))?;
    if !plugins::claims_to_be_a_plugin(&file) {
        return Err(format!("{} is not a plugin", file.display()));
    }
    Ok(file)
}

/// A single path component: no separators, no `..`, no drive or stream prefix.
fn is_bare_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && !name.chars().any(char::is_control)
}

/// Check the requirements of a plugin about to start against what is already running.
pub(super) fn unmet_plugin_requirement(requirements: &[PluginReq]) -> Option<String> {
    let guard = state();
    for requirement in requirements {
        let found = guard
            .find_plugin(&requirement.name)
            .and_then(|h| guard.plugin(h));
        match found {
            None if requirement.optional => {}
            None => {
                return Some(format!(
                    "requires plugin {}, which is not loaded",
                    requirement.name
                ))
            }
            // Stopped or faulted for the session, or its start failed: it will not run.
            Some(record) if record.ended.is_some() && !requirement.optional => {
                return Some(format!(
                    "requires plugin {}, which is not running ({})",
                    requirement.name,
                    record.state().as_str()
                ))
            }
            Some(record) if !requirement.version.matches(&record.info.version) => {
                return Some(format!(
                    "requires plugin {} {}, but {} is loaded",
                    requirement.name,
                    requirement.version.as_str(),
                    record.info.version
                ))
            }
            Some(record) => {
                if !record.enabled {
                    log::warn!(
                        "{} is required but currently stopped; starting anyway",
                        record.name()
                    );
                }
            }
        }
    }
    None
}

/// Load, validate and start one plugin, loading what it depends on first.
///
/// `visiting` is the chain being loaded, so a dependency cycle between two DLLs that were not
/// part of the startup plan is reported instead of recursing forever.
fn load(
    name: &str,
    visiting: &mut Vec<String>,
    lines: &mut Vec<String>,
    depth: usize,
) -> Result<(), String> {
    if visiting.iter().any(|n| n == name) {
        return Err(format!(
            "dependency cycle: {} -> {name}",
            visiting.join(" -> ")
        ));
    }
    if depth > MAX_DEPTH {
        return Err(format!("dependency chain deeper than {MAX_DEPTH}: {name}"));
    }
    if state().find_plugin(name).is_some() {
        return Err(format!(
            "{name} is already known to this session; a plugin name can only be used once per \
             launch, even after `plugin stop`"
        ));
    }

    // A plugin that described itself at startup and is waiting for a dependency is already
    // loaded and parsed; prefer it over going back to the file.
    let described = if let Some(waiting) = plugins::take_pending(name) {
        waiting
    } else {
        let file = plugin_file(name)?;
        plugins::describe_one(&file).map_err(|e| format!("{}: {e}", file.display()))?
    };
    let plugin = described.name().to_owned();
    state().clear_pending(&plugin);

    let game_dir = state().paths.game_dir.clone();
    if let Some(reason) = external::unmet_external(&game_dir, &described.declared) {
        return Err(format!("{plugin}: {reason}"));
    }

    let requirements = external::plugin_requirements(&plugin, &described.declared);
    visiting.push(plugin.clone());
    let missing = missing_dependencies(&requirements);
    for dependency in missing {
        load(&dependency, visiting, lines, depth + 1)
            .map_err(|e| format!("{plugin}: needs {dependency}: {e}"))?;
    }
    visiting.pop();

    if let Some(reason) = unmet_plugin_requirement(&requirements) {
        return Err(format!("{plugin}: {reason}"));
    }
    let active = plugins::start_one(&described).map_err(|e| format!("{plugin}: {e}"))?;
    lines.push(format!("started {plugin} {}", described.info.version));
    plugins::append(active);
    Ok(())
}

/// Mandatory plugin requirements that are not running yet, in declaration order.
fn missing_dependencies(requirements: &[PluginReq]) -> Vec<String> {
    let guard = state();
    requirements
        .iter()
        .filter(|r| !r.optional)
        .filter(|r| guard.find_plugin(&r.name).is_none())
        .map(|r| r.name.clone())
        .collect()
}

/// Call the plugin's stop export and stop delivering callbacks to it.
fn stop(name: &str) -> (Status, Vec<String>) {
    let Some(plugin) = plugins::find_by_name(name) else {
        let waiting = plugins::pending_names();
        if waiting.iter().any(|n| n == name) {
            return (
                Status::OK,
                vec![format!("{name} is waiting for a dependency, not running")],
            );
        }
        return (Status::NOT_FOUND, vec![format!("no plugin named {name}")]);
    };
    // A paused plugin is still to be stopped: only an ended one is not.
    if state().has_ended(plugin.handle) {
        return (Status::OK, vec![format!("{name} is already stopped")]);
    }
    plugin.suspend();
    plugin.call_stop(StopReason::UNLOAD);
    (
        Status::OK,
        vec![format!(
            "stopped {name}; its DLL stays loaded until the game exits"
        )],
    )
}

/// Resume or pause callback delivery.
fn delivery(op: PluginOp, name: &str) -> (Status, Vec<String>) {
    let Some(plugin) = plugins::find_by_name(name) else {
        return (Status::NOT_FOUND, vec![format!("no plugin named {name}")]);
    };
    let enable = op == PluginOp::Enable;
    if enable && state().has_ended(plugin.handle) {
        return (
            Status::WRONG_PHASE,
            vec![format!(
                "{name} is stopped for this session; it starts again with the next game start"
            )],
        );
    }
    let changed = if enable {
        plugin.enable()
    } else {
        plugin.suspend()
    };
    let state = if enable { "enabled" } else { "disabled" };
    let line = if changed {
        format!("{name} {state}")
    } else {
        format!("{name} was already {state}")
    };
    (Status::OK, vec![line])
}
