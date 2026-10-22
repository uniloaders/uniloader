//! Function table the loader hands to every plugin.

use core::ffi::c_void;

use crate::registry::{
    ConsoleLineFn, GuardedFn, HotkeyInfoFn, KeyModifiers, PluginEntryFn, ServiceInfo, SettingInfoFn,
};
use crate::types::{
    ArgEntry, BindDesc, Bytes, Caller, CommandDesc, EnvEntry, HotkeyDesc, LogLevel, PluginHandle,
    PluginState, SettingDesc, Status, Str,
};

/// Callback a message receiver uses to answer synchronously.
pub type ReplyFn = unsafe extern "C" fn(reply_ctx: *mut c_void, payload: Bytes);

/// Callback receiving one line of console output, in the order it was printed.
pub type LineFn = unsafe extern "C" fn(line_ctx: *mut c_void, line: Str);

/// Services the loader offers. Every function takes `host` as its first argument.
///
/// All functions are thread safe. Registration functions return [`Status::WRONG_PHASE`]
/// outside the plugin's `start` call.
#[repr(C)]
pub struct HostApi {
    /// `size_of::<HostApi>()` as compiled into the loader.
    pub struct_size: usize,
    /// Equals [`crate::API_VERSION`].
    pub api_version: u32,
    /// Opaque loader state, pass back unchanged. Each plugin's table has its own: the
    /// functions that act in a plugin's name (messages, events, hooks, services, input
    /// claims) refuse a handle that is not the caller's with [`Status::INVALID_ARGUMENT`].
    pub host: *mut c_void,
    /// The loader's own version.
    pub loader_version: Str,

    /// Directory of the game executable. Valid for the plugin's lifetime.
    pub game_dir: Str,
    /// Full path of the game executable.
    pub exe_path: Str,
    /// Directory the plugins are loaded from (`<game>/plugins`).
    pub plugin_dir: Str,
    /// Directory holding plugin config files (`<game>/uniloader/config`).
    pub config_dir: Str,
    /// Base address the game executable is mapped at.
    pub module_base: *mut c_void,

    /// The game's command line, verbatim and without the executable path.
    pub command_line: Str,
    /// Parsed command line entries in order. Valid for the plugin's lifetime.
    pub args: *const ArgEntry,
    /// Number of entries in [`HostApi::args`].
    pub arg_count: usize,
    /// Every environment variable of the game process, sorted by name, as a snapshot taken
    /// when the loader initialised. Valid for the plugin's lifetime.
    pub env: *const EnvEntry,
    /// Number of entries in [`HostApi::env`].
    pub env_count: usize,

    /// Write a line to the shared loader log, prefixed with the plugin name.
    pub log:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, level: LogLevel, msg: Str),

    /// Register a persisted setting / console variable.
    pub setting_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const SettingDesc,
    ) -> Status,
    /// Read a setting of any plugin as text. `name` is `key` for own settings or
    /// `<plugin>.<key>` for another plugin's. Writes the value into `buf` and its length
    /// into `out_len`; returns [`Status::BUFFER_TOO_SMALL`] with the needed length otherwise.
    pub setting_get: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        buf: *mut u8,
        cap: usize,
        out_len: *mut usize,
    ) -> Status,
    /// Write a setting as text; validated against the descriptor, persisted unless transient,
    /// then the owning plugin's `on_setting_changed` fires.
    pub setting_set: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        value: Str,
    ) -> Status,

    /// Register a hotkey action. The binding comes from the loader's hotkey config, falling
    /// back to the descriptor default. Hotkeys fire only while the game window has focus.
    pub hotkey_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const HotkeyDesc,
    ) -> Status,

    /// Register a console command dispatched to `on_command`.
    pub command_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const CommandDesc,
    ) -> Status,
    /// Print a line to the console (and the log).
    pub console_print: unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, line: Str),
    /// Execute a console line as if typed by the user.
    pub console_exec:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, line: Str) -> Status,
    /// Run a console line and receive what it printed, line by line, before returning.
    /// `sink` is called once per line while the call is on the stack and must not be kept.
    pub console_capture: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        line: Str,
        sink: Option<LineFn>,
        line_ctx: *mut c_void,
    ) -> Status,

    /// Look up another plugin by name.
    pub plugin_find:
        unsafe extern "C" fn(host: *mut c_void, name: Str, out: *mut PluginHandle) -> Status,
    /// The state of a plugin found with [`HostApi::plugin_find`].
    pub plugin_state: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        out: *mut PluginState,
    ) -> Status,
    /// Send a synchronous message to one plugin. The receiver may answer through `reply`
    /// before returning; `reply` is never called after this function returns.
    pub plugin_message: unsafe extern "C" fn(
        host: *mut c_void,
        from: PluginHandle,
        to: PluginHandle,
        topic: Str,
        payload: Bytes,
        reply: Option<ReplyFn>,
        reply_ctx: *mut c_void,
    ) -> Status,
    /// Subscribe to a broadcast topic; delivered through `on_event`.
    pub event_subscribe:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, topic: Str) -> Status,
    /// Broadcast to every subscriber of `topic` except the sender. Delivery is synchronous,
    /// on the publishing thread.
    pub event_publish: unsafe extern "C" fn(
        host: *mut c_void,
        from: PluginHandle,
        topic: Str,
        payload: Bytes,
    ) -> Status,

    /// Overwrite `len` bytes at `address`, remembering what was there.
    ///
    /// The loader restores the original bytes when the plugin stops, so a patch cannot
    /// outlive the plugin that made it. `note` names the patch in the log. Writes the hook
    /// id to `out_id`.
    pub hook_patch: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        address: *mut c_void,
        bytes: *const u8,
        len: usize,
        note: Str,
        out_id: *mut u64,
    ) -> Status,
    /// Replace entry `index` of the virtual table `object` points at.
    ///
    /// Writes the replaced pointer to `out_original`, which is what the plugin calls to reach
    /// the original implementation, and the hook id to `out_id`. The loader puts the original
    /// pointer back when the plugin stops.
    pub hook_vtable: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        object: *mut c_void,
        index: u32,
        replacement: *mut c_void,
        note: Str,
        out_original: *mut *mut c_void,
        out_id: *mut u64,
    ) -> Status,
    /// Detour `target` to `replacement`, in place.
    ///
    /// The relocated prologue is written to `out_trampoline`: calling that reaches the
    /// original function. The loader removes the detour when the plugin stops. Returns
    /// [`Status::ALREADY_EXISTS`] when any plugin already detoured that address, and
    /// [`Status::UNSUPPORTED`] when the prologue cannot be relocated.
    pub hook_detour: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        target: *mut c_void,
        replacement: *mut c_void,
        note: Str,
        out_trampoline: *mut *mut c_void,
        out_id: *mut u64,
    ) -> Status,
    /// Undo one hook by id, before the plugin stops. Hooks left behind are undone for it.
    /// Returns [`Status::NOT_FOUND`] for an unknown id or one belonging to another plugin.
    pub hook_remove:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, id: u64) -> Status,

    /// Every started plugin, then every plugin waiting for a dependency, then the `.asi`
    /// files and chain-loaded libraries, each handed to `sink` once.
    pub plugin_enumerate: unsafe extern "C" fn(
        host: *mut c_void,
        sink: Option<PluginEntryFn>,
        ctx: *mut c_void,
    ) -> Status,
    /// Every setting of the plugin named `plugin_name`, or of all plugins when it is empty.
    pub setting_enumerate: unsafe extern "C" fn(
        host: *mut c_void,
        plugin_name: Str,
        sink: Option<SettingInfoFn>,
        ctx: *mut c_void,
    ) -> Status,
    /// Every hotkey action.
    pub hotkey_enumerate: unsafe extern "C" fn(
        host: *mut c_void,
        sink: Option<HotkeyInfoFn>,
        ctx: *mut c_void,
    ) -> Status,
    /// Bind any plugin's hotkey action (qualified name) to `binding` in key grammar, which
    /// may list several chords separated by commas; empty unbinds it. Saved to
    /// `binds.cfg` as a `rebind` line; a binding equal to the default removes it.
    pub hotkey_rebind: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        binding: Str,
    ) -> Status,

    /// Provide a service: a function table other plugins look up by `name`
    /// (`[a-z0-9_.-]+`). `table` must stay valid for the life of the process. Returns
    /// [`Status::ALREADY_EXISTS`] when another plugin provides that name.
    pub service_register: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        version: u32,
        table: *const c_void,
    ) -> Status,
    /// Look a service up. Returns [`Status::NOT_FOUND`] when nobody provides it or its
    /// provider is not running. `out.struct_size` must be set by the caller.
    pub service_get: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        name: Str,
        out: *mut ServiceInfo,
    ) -> Status,
    /// Run `f(ctx)` on behalf of plugin `target` under the loader's fault guard.
    ///
    /// For a service calling back into a consumer: a panic or hardware fault inside `f`
    /// disables `target`, exactly as if a loader callback had faulted, and returns
    /// [`Status::ERROR`]. Returns [`Status::NOT_FOUND`] without calling `f` when `target` is
    /// not running. Unlike the functions that act in the caller's own name, `target` is not
    /// checked against the caller (blaming another plugin is the point); the loader cannot
    /// tell whose code `f` is, so a service must only pass the target's own callbacks.
    pub guarded_call: unsafe extern "C" fn(
        host: *mut c_void,
        target: PluginHandle,
        f: Option<GuardedFn>,
        ctx: *mut c_void,
    ) -> Status,
    /// Read the console scrollback (command output, echoed input and log records, oldest
    /// first) printed after position `since`, each line handed to `sink`. Writes the position
    /// to pass next time to `out_next`. Start with 0; lines that fell out of the bounded
    /// history are gone. For a plugin that shows the console, such as an overlay.
    pub console_read: unsafe extern "C" fn(
        host: *mut c_void,
        since: u64,
        sink: Option<ConsoleLineFn>,
        ctx: *mut c_void,
        out_next: *mut u64,
    ) -> Status,
    /// Every name the console completes: built-in commands, plugin commands, settings
    /// (qualified), hotkey actions and plugin names, each handed to `sink` once.
    pub console_names:
        unsafe extern "C" fn(host: *mut c_void, sink: Option<LineFn>, ctx: *mut c_void) -> Status,

    /// Report a key press the plugin saw in the game window's messages.
    ///
    /// Hotkeys bound by position (`sc29`) cannot be polled, because which virtual key sits at
    /// a scan code is not reliable to ask; they fire from presses reported here. For a plugin
    /// that subclasses the game window, such as an overlay.
    pub hotkey_press: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        scancode: u16,
        modifiers: KeyModifiers,
    ) -> Status,
    /// Claim the keyboard (`capture` true) or give the claim up.
    ///
    /// While a plugin holds it, only that plugin's own hotkeys fire, so typing into an
    /// overlay does not also trigger other plugins' bindings. Claims stack: the newest holds
    /// the keyboard, and giving one up hands it back to the one before. A stopped plugin's
    /// claim ends.
    pub input_capture:
        unsafe extern "C" fn(host: *mut c_void, plugin: PluginHandle, capture: bool) -> Status,
    /// Which plugin `caller` (another plugin's [`HostApi::host`]) belongs to, into `out`.
    ///
    /// For services: a consumer passes its own `host` with each call, and the service asks
    /// here who that is instead of trusting a handle the consumer names.
    /// [`Status::NOT_FOUND`] for a pointer that is no plugin's.
    pub plugin_of:
        unsafe extern "C" fn(host: *mut c_void, caller: Caller, out: *mut PluginHandle) -> Status,
    /// Suggest a bind: `desc.script` runs on `desc.default_binding` until the user moves it
    /// (`rebind`) or removes it (`unbind`), which the loader saves. When another plugin
    /// already suggests a key, the first one keeps it. Only during `start`.
    pub bind_default: unsafe extern "C" fn(
        host: *mut c_void,
        plugin: PluginHandle,
        desc: *const BindDesc,
    ) -> Status,
}
