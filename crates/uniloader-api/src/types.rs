//! Plain value types shared by the host and callback tables.
//!
//! Every enum-like value is a transparent integer with associated constants rather than a
//! Rust `enum`: a side built against a newer ABI may hand over a value this side has never
//! heard of, and reading that into a Rust enum would be undefined behaviour instead of
//! something the receiver can refuse.

/// Borrowed UTF-8 string. Valid for the duration of the call it is passed to.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Str {
    /// Pointer to the first byte; may be null only when `len` is 0.
    pub ptr: *const u8,
    /// Length in bytes.
    pub len: usize,
}

impl Str {
    /// The empty string.
    pub const EMPTY: Str = Str {
        ptr: core::ptr::null(),
        len: 0,
    };

    /// Borrow a Rust string for one call.
    #[must_use]
    pub const fn new(s: &str) -> Self {
        Str {
            ptr: s.as_ptr(),
            len: s.len(),
        }
    }
}

/// Borrowed byte payload for messages and events.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Bytes {
    /// Pointer to the first byte; may be null only when `len` is 0.
    pub ptr: *const u8,
    /// Length in bytes.
    pub len: usize,
}

impl Bytes {
    /// The empty payload.
    pub const EMPTY: Bytes = Bytes {
        ptr: core::ptr::null(),
        len: 0,
    };

    /// Borrow a byte slice for one call.
    #[must_use]
    pub const fn new(b: &[u8]) -> Self {
        Bytes {
            ptr: b.as_ptr(),
            len: b.len(),
        }
    }
}

/// A plugin's loader context ([`crate::HostApi::host`]), passed to a service so the service
/// can ask the loader who is calling ([`crate::HostApi::plugin_of`]). Only compared, never
/// dereferenced.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Caller(pub *mut core::ffi::c_void);

// SAFETY: the pointer is only ever compared, never dereferenced, so sharing it between
// threads cannot race on anything.
#[allow(unsafe_code)]
unsafe impl Send for Caller {}
// SAFETY: see the `Send` impl.
#[allow(unsafe_code)]
unsafe impl Sync for Caller {}

/// Opaque identifier the loader assigns to each loaded plugin.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginHandle(pub u32);

impl PluginHandle {
    /// Handle that never refers to a plugin.
    pub const NONE: PluginHandle = PluginHandle(0);
}

/// Result of every boundary call.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status(pub i32);

impl Status {
    /// Success.
    pub const OK: Status = Status(0);
    /// Generic failure; details are in the log.
    pub const ERROR: Status = Status(1);
    /// The callee does not implement this operation.
    pub const UNSUPPORTED: Status = Status(2);
    /// The named plugin, setting, command, hotkey or hook does not exist.
    pub const NOT_FOUND: Status = Status(3);
    /// An argument was malformed (bad UTF-8, out of range, wrong kind).
    pub const INVALID_ARGUMENT: Status = Status(4);
    /// The name is already registered by this or another plugin.
    pub const ALREADY_EXISTS: Status = Status(5);
    /// The call is only allowed during a different phase (for example in `start`).
    pub const WRONG_PHASE: Status = Status(6);
    /// The output buffer is too small; the required size was written to the length out-param.
    pub const BUFFER_TOO_SMALL: Status = Status(7);

    /// Whether this is [`Status::OK`].
    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }
}

/// Log severities, matching the `log` crate ordering.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLevel(pub u32);

impl LogLevel {
    /// Unrecoverable problem.
    pub const ERROR: LogLevel = LogLevel(1);
    /// Recoverable problem.
    pub const WARN: LogLevel = LogLevel(2);
    /// Lifecycle information.
    pub const INFO: LogLevel = LogLevel(3);
    /// Diagnostic detail.
    pub const DEBUG: LogLevel = LogLevel(4);
    /// Very verbose tracing.
    pub const TRACE: LogLevel = LogLevel(5);
}

/// Why a plugin's stop export is being called.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopReason(pub u32);

impl StopReason {
    /// The game is shutting down through the loader's `quit`. Everything is about to
    /// disappear anyway, so do the least that is correct: a long teardown here delays the
    /// process exit for no benefit. When the game exits on its own, no stop export runs:
    /// by then the OS has ended every other thread, wherever it was, and holds its loader
    /// lock, so calling into a plugin could hang the exit.
    ///
    /// `stop` is called at most once, and never after a fault. Callbacks already running on
    /// another thread when it is called may still finish afterwards, so the `ctx` given in
    /// `start` must stay valid for the life of the process: release resources in `stop`, but
    /// do not free `ctx` itself.
    pub const EXIT: StopReason = StopReason(0);
    /// The plugin was stopped on request (`plugin stop`) while the game keeps running.
    /// Remove hooks, stop threads and release resources: whatever is left behind stays
    /// behind for the rest of the session.
    pub const UNLOAD: StopReason = StopReason(1);
    /// The plugin's `start` failed partway and the loader is undoing it.
    pub const START_FAILED: StopReason = StopReason(2);
}

/// What a [`Dependency`] names.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyKind(pub u32);

impl DependencyKind {
    /// Another plugin, by its [`PluginInfo::name`]. Decides load order.
    pub const PLUGIN: DependencyKind = DependencyKind(0);
    /// A DLL that must be findable on the loader's search path.
    pub const LIBRARY: DependencyKind = DependencyKind(1);
    /// A file that must exist, relative to the game directory or absolute.
    pub const FILE: DependencyKind = DependencyKind(2);
    /// A service, by name, provided by whichever plugin lists it in
    /// [`PluginInfo::provides`]. Every provider starts first; with exactly one provider the
    /// dependency behaves like [`DependencyKind::PLUGIN`] on it, with several each one only
    /// decides order, because any of them is enough.
    pub const SERVICE: DependencyKind = DependencyKind(3);
}

/// One requirement the loader checks before starting a plugin.
///
/// A plugin whose mandatory dependencies are not met is never started, and the reason is
/// logged once instead of surfacing later as a crash inside the plugin.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Dependency {
    /// `size_of::<Dependency>()`.
    pub struct_size: usize,
    /// What `name` refers to.
    pub kind: DependencyKind,
    /// Plugin name, library file name or file path.
    pub name: Str,
    /// Version requirement for [`DependencyKind::PLUGIN`], for example `>=1.2` or
    /// `>=1.2, <2`. Empty accepts any version, and other kinds ignore it.
    pub version: Str,
    /// Non-zero to carry on without it; the dependency then only affects load order.
    pub optional: u32,
}

/// Static description of a plugin, returned by the describe export.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PluginInfo {
    /// `size_of::<PluginInfo>()` as compiled into the plugin.
    pub struct_size: usize,
    /// Must equal [`crate::API_VERSION`].
    pub api_version: u32,
    /// Unique short name, `[a-z0-9_-]+`. Used as the settings namespace and config file name.
    pub name: Str,
    /// Human readable name for lists, for example `TSW Coop`.
    pub title: Str,
    /// Version, dot separated numbers first (`1.2.0`, `0.3.1-dev`).
    pub version: Str,
    /// Authors, comma separated.
    pub authors: Str,
    /// One-line description.
    pub description: Str,
    /// Requirements, or null when `dependency_count` is 0. Must stay valid while loaded.
    pub dependencies: *const Dependency,
    /// Number of entries in `dependencies`.
    pub dependency_count: usize,
    /// Names of the services this plugin provides once started, or null when
    /// `provides_count` is 0. Lets [`DependencyKind::SERVICE`] dependants start after it.
    /// Must stay valid while loaded. Appended: check `struct_size` before reading.
    pub provides: *const Str,
    /// Number of entries in `provides`.
    pub provides_count: usize,
}

/// Value type of a setting.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingKind(pub u32);

impl SettingKind {
    /// `true` / `false`.
    pub const BOOL: SettingKind = SettingKind(0);
    /// Integer within `[min, max]`.
    pub const INT: SettingKind = SettingKind(1);
    /// Floating point within `[min, max]`.
    pub const FLOAT: SettingKind = SettingKind(2);
    /// One of the `|`-separated `choices`.
    pub const ENUM: SettingKind = SettingKind(3);
    /// Free text.
    pub const STRING: SettingKind = SettingKind(4);
    /// A file path; `choices` are the `|`-separated extensions a file picker offers.
    pub const FILE: SettingKind = SettingKind(5);
    /// A folder path.
    pub const DIRECTORY: SettingKind = SettingKind(6);
    /// A colour, `#rrggbb` or `#rrggbbaa`.
    pub const COLOUR: SettingKind = SettingKind(7);
    /// A date, `YYYY-MM-DD`.
    pub const DATE: SettingKind = SettingKind(8);
    /// A time of day, `HH:MM` or `HH:MM:SS`.
    pub const TIME: SettingKind = SettingKind(9);
    /// A date and time, `YYYY-MM-DD HH:MM[:SS]`.
    pub const DATETIME: SettingKind = SettingKind(10);
    /// Any set of the `|`-separated `choices`; the value lists them separated by `,`.
    pub const FLAGS: SettingKind = SettingKind(11);
    /// A key binding list in the hotkey grammar (`f8`, `ctrl+shift+c, sc29`), `none` when
    /// unbound.
    pub const KEY: SettingKind = SettingKind(12);
}

/// Bit flags for [`SettingDesc::flags`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingFlags(pub u32);

impl SettingFlags {
    /// No flags.
    pub const NONE: SettingFlags = SettingFlags(0);
    /// Changing the value only takes effect after the game restarts.
    pub const RESTART_REQUIRED: SettingFlags = SettingFlags(1);
    /// Runtime variable only: never written to the plugin's config file.
    pub const TRANSIENT: SettingFlags = SettingFlags(2);
    /// Hidden from settings lists until the user turns advanced settings on.
    pub const ADVANCED: SettingFlags = SettingFlags(4);

    /// Whether every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: SettingFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// A setting (also a console variable) a plugin registers during start.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SettingDesc {
    /// `size_of::<SettingDesc>()`.
    pub struct_size: usize,
    /// Key within the plugin namespace, `[a-z0-9_.]+`. Console name is `<plugin>.<key>`.
    pub key: Str,
    /// Short label for UI.
    pub title: Str,
    /// Longer help text.
    pub description: Str,
    /// Value type.
    pub kind: SettingKind,
    /// Default value encoded as text (`true`, `42`, `0.5`, `choice`, `text`).
    pub default: Str,
    /// Lower bound for numeric kinds; ignored otherwise.
    pub min: f64,
    /// Upper bound for numeric kinds; ignored otherwise.
    pub max: f64,
    /// `|`-separated choices for [`SettingKind::ENUM`]; ignored otherwise.
    pub choices: Str,
    /// See [`SettingFlags`].
    pub flags: SettingFlags,
}

/// A hotkey action a plugin registers during start.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HotkeyDesc {
    /// `size_of::<HotkeyDesc>()`.
    pub struct_size: usize,
    /// Action name within the plugin namespace, `[a-z0-9_]+`.
    pub action: Str,
    /// Short label for UI.
    pub title: Str,
    /// Default binding in loader key grammar, for example `f12` or `ctrl+shift+r`.
    pub default_binding: Str,
}

/// A bind a plugin suggests during start: a script that runs on a key until the user moves
/// or removes it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BindDesc {
    /// `size_of::<BindDesc>()`.
    pub struct_size: usize,
    /// Name within the plugin namespace, `[a-z0-9_]+`; the user moves it with
    /// `rebind <plugin>.<name> <keys>`. Shares the namespace of hotkey actions.
    pub name: Str,
    /// Short label for UI.
    pub title: Str,
    /// Suggested keys in loader key grammar, comma separated; empty for none.
    pub default_binding: Str,
    /// Console script to run, for example `"uniloader.speed_up"` or `"set a.b 1; echo on"`.
    pub script: Str,
}

/// A console command a plugin registers during start.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CommandDesc {
    /// `size_of::<CommandDesc>()`.
    pub struct_size: usize,
    /// Command name within the plugin namespace, `[a-z0-9_]+`. Console name is `<plugin>.<name>`.
    pub name: Str,
    /// One-line help.
    pub help: Str,
    /// Argument syntax shown by `help`.
    pub usage: Str,
}

/// One command line entry as the loader parsed it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ArgEntry {
    /// Normalised name without the leading `-`, `--` or `/`, lowercase. Empty for positionals.
    pub name: Str,
    /// Value, or an empty string when the entry is a bare flag. See [`ArgEntry::has_value`].
    pub value: Str,
    /// The entry exactly as it appeared on the command line.
    pub raw: Str,
    /// Whether the entry carried a value at all (an empty `--key=` still counts).
    pub has_value: u32,
}

/// One environment variable of the game process.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct EnvEntry {
    /// Variable name, exactly as the process has it.
    pub name: Str,
    /// Variable value.
    pub value: Str,
}

/// A plugin's state as the loader sees it, for [`crate::HostApi::plugin_state`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginState(pub u32);

impl PluginState {
    /// Described, waiting for a dependency to start.
    pub const WAITING: PluginState = PluginState(0);
    /// Started and receiving callbacks.
    pub const RUNNING: PluginState = PluginState(1);
    /// Started, delivery paused (`plugin disable`).
    pub const DISABLED: PluginState = PluginState(2);
    /// Stopped (`plugin stop`); the DLL stays loaded.
    pub const STOPPED: PluginState = PluginState(3);
    /// A callback panicked or faulted; delivery stopped for good.
    pub const FAULTED: PluginState = PluginState(4);

    /// Lower-case name for logs and listings; `unknown` for a value a newer loader added.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self.0 {
            0 => "waiting",
            1 => "running",
            2 => "disabled",
            3 => "stopped",
            4 => "faulted",
            _ => "unknown",
        }
    }
}
