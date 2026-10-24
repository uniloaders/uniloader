//! Windows side of the loader: the proxy, plugin DLLs, the host API, the hotkey thread and the
//! optional console window.
//!
//! Lock discipline: the [`state()`] mutex is never held across a call into a plugin. State
//! methods return what to notify and the callers here deliver it after dropping the guard.
//!
//! From the DayZ loader's `win/mod.rs` (`eaebd9b`) without DXGI, the overlay, game data and
//! input listening; those belong in adapter plugins.

mod caller;
mod console;
mod crash;
mod deps;
mod detour;
mod dispatch;
mod exec;
mod foreign;
mod guard;
mod hostapi;
mod hostapi_registry;
mod http;
mod input;
mod lifecycle;
mod panics;
mod plugin_hooks;
mod plugins;
pub(crate) mod proxy;
mod rcon;
mod report;
mod speed;
mod suspects;
mod update;

use std::sync::{Mutex, MutexGuard, OnceLock};

use uniloader_api::{HostApi, Status};

use crate::config::{LoaderConfig, Paths};
use crate::console::Ctx;
use crate::logging;
use crate::process::{flags, Process};
use crate::state::{Phase, State};

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

/// The loader state if nobody holds it: for the freeze watchdog and the crash path, where
/// the thread that hangs or faulted may be the one holding it.
pub(crate) fn try_state() -> Option<MutexGuard<'static, State>> {
    match STATE.get()?.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
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
    panics::install();
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
    // Before the plugins, so the name is the loader's and a plugin can read its settings in
    // `start`.
    let own_file = proxy::forwarding()
        .and_then(|f| f.own_path.file_name())
        .map_or_else(
            || "uniloader.dll".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        );
    speed::register(&own_file);
    identify_game();
    // After the loader's entry (the setting) and the game's facts (for `info.txt`), before
    // any plugin code runs.
    report::start(&paths);
    plugins::load_all(&paths, &config.disabled_plugins);
    // After the plugins, so a plugin's hooks are in place before anything else runs code.
    foreign::load_all(&paths, config.asi, &config.load);
    let mut guard = state();
    guard.phase = Phase::Running;
    guard.hook_lines = plugin_hooks::console_lines;
    guard.quit = quit;
    log_binds(&guard);
    // The loader's own entry is not a plugin anyone installed.
    let builtin = usize::from(speed::owner().is_some());
    let running = guard.plugins.iter().filter(|p| p.enabled).count() - builtin;
    log::info!(
        "{running} of {} plugins running",
        guard.plugins.len() - builtin
    );
    drop(guard);
    // After the plugins started: one that holds the speed at 1 has had its say.
    speed::apply_stored();
    exec::autoexec();
    rcon::reconfigure();
    update::start();
    // Last, so a command typed or a key pressed in the first instant cannot race the
    // registries.
    console::start_input();
    input::start_polling();
    running > 0
}

/// Read the game's properties for target filters, and log who it is.
fn identify_game() {
    let exe = proxy::module_path(None);
    let facts = uniloader_core::facts::gather(&exe, false);
    let shown = |name: &str| facts.get(name).map(ToString::to_string);
    let mut parts: Vec<String> = ["pe.version.CompanyName", "pe.version.ProductName"]
        .into_iter()
        .filter_map(shown)
        .collect();
    if let Some(engine) = shown("unreal.version").or_else(|| shown("unity.version")) {
        parts.push(format!("engine {engine}"));
    }
    if let Some(build) = shown("steam.build_id") {
        parts.push(format!("Steam build {build}"));
    }
    log::info!(
        "game: {} {} ({} properties for target filters, `facts` lists them)",
        shown("exe.name").unwrap_or_default(),
        parts.join(", "),
        facts.len()
    );
    let paths = {
        let mut guard = state();
        guard.facts = facts;
        guard.paths.clone()
    };
    let exe_name = exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    crate::schema::save_loader(&paths, &crate::schema::loader(&exe_name));
}

/// Log every bind: what each key runs and where that comes from.
///
/// A scan code bind also reports what this keyboard layout makes of it, because "the key
/// under Escape does nothing" is otherwise impossible to diagnose from a log.
fn log_binds(guard: &State) {
    use uniloader_core::binds::KeyState;
    for row in guard.binds.rows() {
        let layout = row
            .chord
            .scancode
            .and_then(|sc| {
                input::AsyncKeys
                    .vk_for_scancode(sc)
                    .map(|vk| format!(" (sc{sc:x} is vk 0x{vk:02x} on this layout)"))
            })
            .unwrap_or_default();
        log::info!(
            "bind {} = {:?} ({}){layout}",
            row.chord,
            row.script,
            row.origin
        );
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

/// One tick of the input thread: drain the log into the console, turn key changes into
/// binds while the game has focus, and run the scripts that are due.
pub(crate) fn tick() {
    let focused = input::game_has_focus();
    let events = {
        let mut guard = state();
        // The console shows the log whether or not anything reads it, so a line logged a
        // moment ago is in the scrollback when someone asks.
        guard.console_drain();
        let events = if focused {
            guard.binds.poll(&input::AsyncKeys)
        } else {
            // Released while away, so a key held when focus left does not fire on return,
            // and a `+` bind held then ends.
            guard.binds.reset()
        };
        guard.allowed_binds(events)
    };
    exec::deliver_keys(events);
    exec::BUFFER.drain(&exec::Platform);
}

/// Run a script as if typed and deliver whatever it produced, discarding the output.
pub(crate) fn run_console_line(ctx: Ctx, line: &str) -> Status {
    exec::run_script(ctx, line).0
}

/// Run a script and return both the status and everything it printed.
pub(crate) fn run_console_line_capture(ctx: Ctx, line: &str) -> (Status, Vec<String>) {
    exec::run_script(ctx, line)
}

/// Called from `DllMain` on process detach. `terminating` is true when the process is
/// exiting: its other threads were killed wherever they were, possibly holding one of the
/// loader's locks or a plugin's, and the OS loader lock is held. Nothing runs then; plugins
/// are stopped only by `quit` or `plugin stop`, before the process ends.
pub(crate) fn shutdown(terminating: bool) {
    report::ending();
    if !terminating {
        plugins::stop_all();
        log::info!("loader shut down");
    }
}
