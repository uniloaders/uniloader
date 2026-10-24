//! Stable C ABI between uniloader and its plugins.
//!
//! Everything in this crate is `#[repr(C)]`, dependency free and must stay source and
//! binary compatible within one [`API_VERSION`]. Plugins written in other languages mirror
//! these definitions; Rust plugins use the `uniloader-sdk` crate instead of this one.
//!
//! Conventions:
//! - Strings are UTF-8 slices ([`Str`]) that are valid for the duration of the call only,
//!   unless a field documents otherwise. Nobody frees them.
//! - Every struct that may grow starts with `struct_size` so both sides can detect fields
//!   the other side does not know about. Fields are only ever appended.
//! - Every call returns a [`Status`]; nothing panics or throws across the boundary.
//! - Values that look like enums are transparent integers with constants (see `types`).
//!
//! Derived from the DayZ plugin loader's `dayz-plugin-api` (ABI 9, `eaebd9b`) without its
//! DayZ- and DXGI-specific parts: frame, overlay, input-stream and game-event callbacks and
//! the symbol database. Those become adapter plugins that publish events.

#![no_std]

use core::ffi::c_void;

mod callbacks;
mod host;
mod registry;
mod types;

pub use callbacks::PluginCallbacks;
pub use host::{HostApi, LineFn, ReplyFn};
pub use registry::{
    ConsoleKind, ConsoleLine, ConsoleLineFn, GuardedFn, HotkeyInfo, HotkeyInfoFn, KeyModifiers,
    ModuleKind, PluginEntry, PluginEntryFn, ServiceInfo, SettingInfo, SettingInfoFn,
};
pub use types::{
    ArgEntry, Author, BindDesc, Bytes, Caller, CommandDesc, Dependency, DependencyKind, EnvEntry,
    HotkeyDesc, LogLevel, PluginHandle, PluginInfo, PluginState, SettingDesc, SettingFlags,
    SettingKind, Status, StopReason, Str, TargetFilter, TargetPolicy, UpdateSource,
};

/// Version of this ABI. The loader refuses plugins describing a different version.
pub const API_VERSION: u32 = 2;

/// Name of the export every plugin must provide: `extern "C" fn() -> *const PluginInfo`.
pub const DESCRIBE_EXPORT: &str = "uniloader_plugin_describe";
/// Name of the start export: `extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status`.
pub const START_EXPORT: &str = "uniloader_plugin_start";
/// Name of the stop export: `extern "C" fn(*mut c_void, StopReason)` receiving the plugin
/// context and why it is being stopped.
pub const STOP_EXPORT: &str = "uniloader_plugin_stop";

/// Signature of [`DESCRIBE_EXPORT`]. The returned struct must stay valid while the DLL is loaded.
pub type DescribeFn = unsafe extern "C" fn() -> *const PluginInfo;
/// Signature of [`START_EXPORT`]. The plugin fills `callbacks` and may keep `host` for its lifetime.
pub type StartFn =
    unsafe extern "C" fn(*const HostApi, PluginHandle, *mut PluginCallbacks) -> Status;
/// Signature of [`STOP_EXPORT`]. Called once with the context the plugin stored in its
/// callbacks, and the reason the loader is stopping it.
pub type StopFn = unsafe extern "C" fn(*mut c_void, StopReason);

/// Topics the loader itself publishes, under the reserved `loader.` namespace.
pub mod topics {
    /// A plugin changed state: it started, was paused or resumed, stopped or faulted.
    /// Payload: its [`PluginHandle`](crate::PluginHandle) and its
    /// [`PluginState`](crate::PluginState), each a little-endian `u32`, then its name.
    ///
    /// Services use it to forget what a stopped consumer registered with them.
    pub const PLUGIN_STATE: &str = "loader.plugin_state";
    /// A plugin found at start did not start: it waits for a dependency, needs another
    /// version, or failed. Sent once per plugin after the loader's start pass, to the plugins
    /// running then, so a UI can tell the player. Payload: its name, a NUL byte, then why
    /// (UTF-8).
    pub const PLUGIN_NOT_RUNNING: &str = "loader.plugin_not_running";
    /// The game speed (the setting `uniloader.speed`) changed and the clocks now run at it.
    /// Payload: the factor, a little-endian `f64`; 1.0 is normal speed.
    ///
    /// A plugin that cannot work at another speed, a multiplayer session for one, sets
    /// `uniloader.speed` back to 1 when it receives this.
    pub const SPEED: &str = "loader.speed";
    /// A plugin's target filters do not match the running game. Sent once per such plugin
    /// after the loader's start pass, to the plugins running then, so a UI can tell the
    /// player. Payload, NUL separated UTF-8: its name, its version, then why (one line per
    /// filter that failed).
    pub const PLUGIN_MISMATCH: &str = "loader.plugin_mismatch";
    /// The update check found a newer version of a plugin. Payload, NUL separated UTF-8: its
    /// name, its version, the latest version, then where to download it (may be empty).
    pub const UPDATE_AVAILABLE: &str = "loader.update_available";
    /// A crash, freeze or unexpected-exit report is ready for the player to decide about.
    /// Payload, NUL separated UTF-8: the zip's name, its kind (`crash`, `freeze`, `exit`,
    /// `manual`), the headline to show, the host it would be sent to (empty: it cannot be
    /// sent, only saved), and where it was saved already (empty unless the setting saves
    /// without asking). Answer with the console commands `report send <zip>` or
    /// `report desktop <zip>`.
    pub const REPORT_READY: &str = "loader.report_ready";
    /// What became of a report. Payload, NUL separated UTF-8: the zip's name, `sent`,
    /// `saved` or `failed`, then the detail (the server's report id, the file saved, or why
    /// it failed).
    pub const REPORT_DONE: &str = "loader.report_done";
}
