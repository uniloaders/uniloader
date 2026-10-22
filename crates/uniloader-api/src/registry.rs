//! What the loader knows, as plugins can ask for it: the loaded modules, every setting, every
//! hotkey, and the services plugins provide to each other.
//!
//! Enumeration hands each entry to a sink callback while the call is on the stack; the
//! strings in an entry are valid for that one sink call only.

use core::ffi::c_void;

use crate::types::{Author, LogLevel, PluginHandle, PluginState, SettingFlags, SettingKind, Str};

/// What kind of module a [`PluginEntry`] describes.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleKind(pub u32);

impl ModuleKind {
    /// A uniloader plugin.
    pub const PLUGIN: ModuleKind = ModuleKind(0);
    /// An `.asi` file from the plugin directory. Never called, only listed.
    pub const ASI: ModuleKind = ModuleKind(1);
    /// A library chain-loaded through `loader.toml`'s `load` list, such as UE4SS.
    pub const LIBRARY: ModuleKind = ModuleKind(2);
}

/// One module the loader loaded, or one plugin waiting for a dependency.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PluginEntry {
    /// `size_of::<PluginEntry>()` as compiled into the loader.
    pub struct_size: usize,
    /// Handle, or [`PluginHandle::NONE`] for anything that is not a started plugin.
    pub handle: PluginHandle,
    /// What it is.
    pub kind: ModuleKind,
    /// Plugin state; [`PluginState::RUNNING`] for `.asi` files and libraries.
    pub state: PluginState,
    /// Unique plugin name, or the file stem for other modules.
    pub name: Str,
    /// Human readable name.
    pub title: Str,
    /// Version text; for other modules the product version of their version resource.
    pub version: Str,
    /// One-line description, or why a waiting plugin waits.
    pub description: Str,
    /// File, relative to the game directory.
    pub file: Str,
    /// Authors, or null when `author_count` is 0.
    pub authors: *const Author,
    /// Number of entries in `authors`.
    pub author_count: usize,
    /// Home page, or empty.
    pub homepage: Str,
    /// Where to report problems, or empty.
    pub issues: Str,
    /// Where to download the latest release: what the update check found, else what the
    /// plugin declared, or empty.
    pub download: Str,
    /// The latest version the update check found, or empty when it did not look or found
    /// nothing.
    pub latest: Str,
    /// Non-zero when `latest` is newer than `version`.
    pub update_available: u32,
    /// Why the running game does not match the plugin's target filters (one line per filter
    /// that failed), or empty when it matches or declares none.
    pub mismatch: Str,
}

/// Receives one [`PluginEntry`].
pub type PluginEntryFn = unsafe extern "C" fn(ctx: *mut c_void, entry: *const PluginEntry);

/// One registered setting with its current value.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SettingInfo {
    /// `size_of::<SettingInfo>()` as compiled into the loader.
    pub struct_size: usize,
    /// Owning plugin's name.
    pub plugin: Str,
    /// Key within the plugin; the console name is `<plugin>.<key>`.
    pub key: Str,
    /// Short label.
    pub title: Str,
    /// Longer help text.
    pub description: Str,
    /// Value type.
    pub kind: SettingKind,
    /// Default value as text.
    pub default: Str,
    /// Lower bound for numeric kinds.
    pub min: f64,
    /// Upper bound for numeric kinds.
    pub max: f64,
    /// `|`-separated choices for enum settings.
    pub choices: Str,
    /// Flags from the descriptor.
    pub flags: SettingFlags,
    /// Current value as text.
    pub value: Str,
}

/// Receives one [`SettingInfo`].
pub type SettingInfoFn = unsafe extern "C" fn(ctx: *mut c_void, setting: *const SettingInfo);

/// One hotkey action and what it is bound to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HotkeyInfo {
    /// `size_of::<HotkeyInfo>()` as compiled into the loader.
    pub struct_size: usize,
    /// Qualified action name, `<plugin>.<action>`.
    pub name: Str,
    /// Short label.
    pub title: Str,
    /// Current binding in key grammar, empty when unbound.
    pub binding: Str,
    /// The binding the plugin asked for.
    pub default_binding: Str,
}

/// Receives one [`HotkeyInfo`].
pub type HotkeyInfoFn = unsafe extern "C" fn(ctx: *mut c_void, hotkey: *const HotkeyInfo);

/// A service a plugin provides: a function table under a name, with a version.
///
/// The table's layout is a contract between the provider and its consumers, usually written
/// down as a small client crate; the loader only stores the pointer. A consumer declares a
/// plugin dependency on the provider so it starts after it, then looks the service up.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ServiceInfo {
    /// `size_of::<ServiceInfo>()` as compiled into the caller.
    pub struct_size: usize,
    /// The table. Valid for the life of the process; the provider's DLL is never unloaded.
    pub table: *const c_void,
    /// Version the provider registered. Same major number, same layout prefix.
    pub version: u32,
    /// The providing plugin.
    pub provider: PluginHandle,
}

/// Function run by [`crate::HostApi::guarded_call`].
pub type GuardedFn = unsafe extern "C" fn(ctx: *mut c_void);

/// Modifier keys held with a key press, as bits.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers(pub u32);

impl KeyModifiers {
    /// Either Control key.
    pub const CTRL: u32 = 1;
    /// Either Alt key.
    pub const ALT: u32 = 2;
    /// Either Shift key.
    pub const SHIFT: u32 = 4;
}

/// Where a console line came from.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConsoleKind(pub u32);

impl ConsoleKind {
    /// Output of a command.
    pub const OUTPUT: ConsoleKind = ConsoleKind(0);
    /// A line the user typed, echoed back.
    pub const ECHO: ConsoleKind = ConsoleKind(1);
    /// A log record; [`ConsoleLine::level`] says which level.
    pub const LOG: ConsoleKind = ConsoleKind(2);
}

/// One line of console scrollback, as [`crate::HostApi::console_read`] hands it out.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ConsoleLine {
    /// `size_of::<ConsoleLine>()` as compiled into the loader.
    pub struct_size: usize,
    /// Position in everything ever printed; strictly increasing.
    pub seq: u64,
    /// Output, echo or log record.
    pub kind: ConsoleKind,
    /// Level of a log record; [`LogLevel::INFO`] otherwise.
    pub level: LogLevel,
    /// Local wall clock when the line was printed, `HH:MM:SS`.
    pub clock: Str,
    /// Module that logged it, empty for command output.
    pub target: Str,
    /// The text, without any prefix.
    pub text: Str,
}

/// Receives one [`ConsoleLine`]. The line and its strings live until the sink returns.
pub type ConsoleLineFn = unsafe extern "C" fn(ctx: *mut c_void, line: *const ConsoleLine);
