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
mod types;

pub use callbacks::PluginCallbacks;
pub use host::{HostApi, LineFn, ReplyFn};
pub use types::{
    ArgEntry, Bytes, CommandDesc, Dependency, DependencyKind, EnvEntry, HotkeyDesc, LogLevel,
    PluginHandle, PluginInfo, PluginState, SettingDesc, SettingFlags, SettingKind, Status,
    StopReason, Str,
};

/// Version of this ABI. The loader refuses plugins describing a different version.
pub const API_VERSION: u32 = 1;

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
    /// A plugin changed state. Payload: the plugin's name, a NUL, then its
    /// [`PluginState`](crate::PluginState) as a little-endian `u32`.
    pub const PLUGIN_STATE: &str = "loader.plugin_state";
}
