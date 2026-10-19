//! Windows side of the loader: the proxy, plugin DLLs, the host API, the hotkey thread and the
//! optional console window.
//!
//! Lock discipline: the [`state()`] mutex is never held across a call into a plugin. State
//! methods return what to notify and the callers here deliver it after dropping the guard.
//!
//! From the DayZ loader's `win/mod.rs` (`eaebd9b`) without DXGI, the overlay, game data and
//! input listening; those belong in adapter plugins.

mod console;
mod deps;
mod detour;
mod dispatch;
mod foreign;
mod guard;
mod hostapi;
mod hostapi_registry;
mod input;
mod lifecycle;
mod plugin_hooks;
mod plugins;
pub(crate) mod proxy;

use std::sync::{Mutex, MutexGuard, OnceLock};

use uniloader_api::{HostApi, PluginHandle, Status};

use crate::config::{LoaderConfig, Paths};
use crate::process::{flags, Process};
use crate::state::{Phase, State};
use crate::{console as console_commands, logging};

/// The host table holds raw pointers, so it is not automatically `Sync`.
// The two marker impls below are the only unsafe code in this module.
#[allow(unsafe_code)]
struct SharedHost(&'static HostApi);

// SAFETY: every pointer in the table either is null, points at leaked `'static` storage, or
// is a function pointer; the ABI requires all of them to be usable from any thread.
#[allow(unsafe_code)]
unsafe impl Sync for SharedHost {}
// SAFETY: see the `Sync` impl.
#[allow(unsafe_code)]
unsafe impl Send for SharedHost {}

static STATE: OnceLock<Mutex<State>> = OnceLock::new();
static HOST: OnceLock<SharedHost> = OnceLock::new();
static ENABLED: OnceLock<bool> = OnceLock::new();

/// Lock the loader state.
///
/// A poisoned lock is recovered rather than propagated: the state is plain data, and a panic
/// elsewhere must not turn every later hotkey poll into a second panic inside the game.
pub(crate) fn state() -> MutexGuard<'static, State> {
    STATE
        .get_or_init(|| Mutex::new(State::new(paths(), LoaderConfig::default())))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The host table handed to plugins.
pub(crate) fn host_api() -> &'static HostApi {
    HOST.get().map_or_else(
        || {
            // Only reachable if a plugin is started before `init` finished, which it is not.
            hostapi::build(&paths(), &Process::current())
        },
        |h| h.0,
    )
}

/// The loader's directories for this game, moved by `uniloader-root.txt` when present.
fn paths() -> Paths {
    Paths::resolve(&proxy::game_dir())
}

/// Initialise once; returns whether any plugin is running.
pub(crate) fn initialize() -> bool {
    *ENABLED.get_or_init(init)
}

/// Console window, config and logger, in that order, so everything after is logged.
fn start_logging(paths: &Paths, process: &Process) -> LoaderConfig {
    if process.flag(flags::CONSOLE) {
        console::attach();
    }
    let (mut config, config_error) = LoaderConfig::load(&paths.loader_file());
    if let Some(level) = process.command_line.value(flags::LOG_LEVEL) {
        level.clone_into(&mut config.log_level);
    }
    let console_sink: Option<logging::ConsoleSink> =
        console::is_active().then_some((console::print, crate::scrollback::clock));
    if let Err(e) = logging::init(
        &paths.logs_dir,
        config.level(),
        console_sink,
        Some(crate::scrollback::record),
    ) {
        console::print(&format!("could not open the log file: {e}"));
    }
    log::info!(
        "uniloader {} in {}",
        env!("CARGO_PKG_VERSION"),
        paths.game_dir.display()
    );
    if !paths.plugins_dir.starts_with(&paths.game_dir) {
        log::info!(
            "root moved by {}: plugins in {}",
            crate::config::ROOT_OVERRIDE,
            paths.plugins_dir.display()
        );
    }
    proxy::log_summary();
    if let Some(e) = config_error {
        log::error!("loader.toml ignored: {e}");
    }
    for line in process.summary() {
        log::info!("{line}");
    }
    log::info!("config: {config:?}");
    if !guard::CONTAINS_HARDWARE_FAULTS {
        log::warn!("this build contains plugin panics but not hardware faults");
    }
    config
}

fn init() -> bool {
    let paths = paths();
    let process = Process::current();
    let config = start_logging(&paths, &process);
    let host = hostapi::build(&paths, &process);
    if HOST.set(SharedHost(host)).is_err() {
        log::error!("host table initialised twice");
    }
    if STATE.set(Mutex::new(State::new(paths, config))).is_err() {
        log::error!("state initialised twice");
    }

    // `--noplugins` overrides the config file, so a broken plugin never blocks a launch.
    let (paths, off, config) = {
        let guard = state();
        let off = !guard.config.enabled || process.flag(flags::NO_PLUGINS);
        (guard.paths.clone(), off, guard.config.clone())
    };
    if off {
        log::info!("plugins disabled; forwarding only");
        return false;
    }
    plugins::load_all(&paths, &config.disabled_plugins);
    // After the plugins, so a plugin's hooks are in place before anything else runs code.
    foreign::load_all(&paths, config.asi, &config.load);
    let mut guard = state();
    guard.phase = Phase::Running;
    guard.hook_lines = plugin_hooks::console_lines;
    guard.quit = quit;
    log_hotkeys(&guard);
    let running = guard.plugins.iter().filter(|p| p.enabled).count();
    log::info!("{running} of {} plugins running", guard.plugins.len());
    drop(guard);
    // Last, so a command typed or a key pressed in the first instant cannot race the
    // registries.
    console::start_input();
    input::start_polling();
    running > 0
}

/// Log what every hotkey action ended up bound to.
///
/// A scan code binding also reports what this keyboard layout makes of it, because "the key
/// under Escape does nothing" is otherwise impossible to diagnose from a log.
fn log_hotkeys(guard: &State) {
    for entry in guard.hotkeys.iter() {
        let resolved: Vec<String> = entry
            .chords()
            .filter_map(|c| c.scancode)
            .filter_map(|sc| {
                uniloader_core::hotkeys::KeyState::vk_for_scancode(&input::AsyncKeys, sc)
                    .map(|vk| format!("sc{sc:x} is vk 0x{vk:02x} on this layout"))
            })
            .collect();
        let binding = entry.binding();
        if resolved.is_empty() {
            log::info!("hotkey {} = {binding}", entry.name);
        } else {
            log::info!(
                "hotkey {} = {binding} ({})",
                entry.name,
                resolved.join(", ")
            );
        }
    }
}

/// `quit`: stop the plugins, then end the process the way a game's own exit does.
fn quit() -> Vec<String> {
    log::info!("quit requested from the console");
    std::thread::spawn(|| {
        plugins::stop_all();
        std::process::exit(0);
    });
    vec!["stopping plugins and closing the game".to_owned()]
}

/// One poll of the hotkey thread: drain the log into the console, and fire hotkeys while the
/// game has focus.
pub(crate) fn tick() {
    let focused = input::game_has_focus();
    let fired = {
        let mut guard = state();
        // The console shows the log whether or not anything reads it, so a line logged a
        // moment ago is in the scrollback when someone asks.
        guard.console_drain();
        if focused {
            let polled = guard.hotkeys.poll(&input::AsyncKeys);
            guard.allowed_hotkeys(polled)
        } else {
            // Released while away, so a key held when focus left does not fire on return.
            guard.hotkeys.reset();
            Vec::new()
        }
    };
    for action in fired {
        log::debug!("hotkey {action}");
        dispatch::hotkey(&action);
    }
}

/// Execute a console line and deliver whatever it produced, discarding the output.
pub(crate) fn run_console_line(caller: Option<PluginHandle>, line: &str) -> Status {
    run_console_line_capture(caller, line).0
}

/// Execute a console line and return both the status and everything it printed.
pub(crate) fn run_console_line_capture(
    caller: Option<PluginHandle>,
    line: &str,
) -> (Status, Vec<String>) {
    let (outcome, printed) = {
        let mut guard = state();
        let before = guard.console_mark();
        let outcome = console_commands::execute(&mut guard, caller, line);
        let printed = guard.console_since(before);
        (outcome, printed)
    };
    for line in &printed {
        console::print(line);
    }
    // The console echoes the typed line so the window and the log show what was asked. A
    // caller capturing output already knows its own line, so it is not part of the answer.
    let echo = format!("> {}", line.trim());
    let printed: Vec<String> = printed.into_iter().filter(|l| *l != echo).collect();
    for persist in outcome.persist {
        persist.write();
    }
    dispatch::deliver(outcome.notify);
    if let Some((op, name)) = outcome.lifecycle {
        // Performed out here, with no lock held: it loads a DLL and calls into it.
        let (status, lines) = lifecycle::perform(op, &name);
        {
            let mut guard = state();
            for line in &lines {
                guard.console_print(line.clone());
            }
        }
        for line in &lines {
            console::print(line);
        }
        return (status, [printed, lines].concat());
    }
    match outcome.command {
        // A plugin command prints through `console_print`, which lands in the buffer.
        Some((plugin, name, args)) => {
            let before = state().console_mark();
            let status = dispatch::command(plugin, &name, &args);
            let after = state().console_since(before);
            (status, [printed, after].concat())
        }
        None => (outcome.status, printed),
    }
}

/// Called from `DllMain` on process detach. `terminating` is true when the process is
/// exiting: its other threads were killed wherever they were, possibly holding one of the
/// loader's locks or a plugin's, and the OS loader lock is held. Nothing runs then; plugins
/// are stopped only by `quit` or `plugin stop`, before the process ends.
pub(crate) fn shutdown(terminating: bool) {
    if !terminating {
        plugins::stop_all();
        log::info!("loader shut down");
    }
}
