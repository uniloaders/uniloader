//! Safe wrapper around the loader's [`HostApi`] table.

// This module is the plugin side of the FFI boundary; every unsafe block documents the
// loader contract it relies on. No other SDK module except `ffi` needs unsafe.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use uniloader_api::{
    ArgEntry, BindDesc, Bytes, Caller, CommandDesc, EnvEntry, HostApi, HotkeyDesc, LogLevel,
    PluginHandle, PluginState, SettingDesc, SettingFlags, Status, Str,
};
pub use uniloader_core::cmdline::{Arg, CommandLine};

use crate::settings::Setting;

/// Error type for plugin code. Carries a loader status or a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// A loader call failed with this status.
    Status(Status),
    /// The operation is not implemented.
    Unsupported,
    /// Free text, shown in the console and log.
    Message(String),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PluginError::Status(s) => write!(f, "loader returned status {}", s.0),
            PluginError::Unsupported => f.write_str("not supported"),
            PluginError::Message(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<String> for PluginError {
    fn from(m: String) -> Self {
        PluginError::Message(m)
    }
}

impl From<&str> for PluginError {
    fn from(m: &str) -> Self {
        PluginError::Message(m.to_owned())
    }
}

/// Reply sink handed to the receiving plugin: stores the bytes in the caller's `Option`.
unsafe extern "C" fn collect_reply(ctx: *mut c_void, payload: Bytes) {
    // SAFETY: `ctx` is the `&mut Option<Vec<u8>>` of the `message` call in progress, which
    // the ABI guarantees is alive until that call returns.
    let slot = unsafe { &mut *ctx.cast::<Option<Vec<u8>>>() };
    *slot = Some(bytes_from(payload).to_vec());
}

pub(crate) fn check(status: Status) -> Result<(), PluginError> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(PluginError::Status(status))
    }
}

/// Handle of another loaded plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PluginRef(pub(crate) PluginHandle);

impl PluginRef {
    /// The raw handle, for a service ABI that identifies its callers.
    #[must_use]
    pub const fn raw(self) -> PluginHandle {
        self.0
    }

    /// Wrap a raw handle a service ABI received.
    #[must_use]
    pub const fn from_raw(handle: PluginHandle) -> Self {
        PluginRef(handle)
    }
}

/// The loader, as seen by one plugin. Cheap to copy; valid for the plugin's lifetime.
#[derive(Clone, Copy)]
pub struct Host {
    pub(crate) api: &'static HostApi,
    pub(crate) handle: PluginHandle,
}

// SAFETY: the loader documents every HostApi function as thread safe, and the table itself
// is immutable for the plugin's lifetime.
unsafe impl Send for Host {}
// SAFETY: see the Send impl.
unsafe impl Sync for Host {}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("handle", &self.handle)
            .finish_non_exhaustive()
    }
}

/// Copy a [`Str`] another plugin passed through a service ABI into an owned `String`.
///
/// Invalid UTF-8 is replaced rather than rejected.
///
/// # Safety
/// `s` must be null or point at `len` readable bytes for the duration of the call, which is
/// what every ABI that hands one over promises.
#[must_use]
pub unsafe fn read_str(s: Str) -> String {
    str_from(s)
}

/// Copy a borrowed [`Str`] the loader passed us into an owned `String`.
pub(crate) fn str_from(s: Str) -> String {
    String::from_utf8_lossy(bytes_from(Bytes {
        ptr: s.ptr,
        len: s.len,
    }))
    .into_owned()
}

/// View a host-provided array.
///
/// # Safety
/// `ptr` must be null or point to `len` initialised `T` valid for `'a`.
unsafe fn slice_from<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: guaranteed by this function's contract, which the loader upholds.
    unsafe { core::slice::from_raw_parts(ptr, len) }
}

/// View a borrowed [`Bytes`] the loader passed us.
pub(crate) fn bytes_from<'a>(b: Bytes) -> &'a [u8] {
    if b.ptr.is_null() || b.len == 0 {
        return &[];
    }
    // SAFETY: the loader guarantees ptr/len describe readable memory for the current call;
    // callers only use the slice within that call.
    unsafe { core::slice::from_raw_parts(b.ptr, b.len) }
}

impl Host {
    pub(crate) fn new(api: &'static HostApi, handle: PluginHandle) -> Self {
        Host { api, handle }
    }

    /// This plugin's handle as seen by other plugins.
    #[must_use]
    pub fn me(&self) -> PluginRef {
        PluginRef(self.handle)
    }

    /// The loader's context for this plugin ([`HostApi::host`]), which names it: what a
    /// service asks for to know its caller, see [`Host::plugin_of`].
    #[must_use]
    pub fn context(&self) -> Caller {
        Caller(self.api.host)
    }

    /// The plugin a caller's [`Host::context`] belongs to; `None` for a pointer that is no
    /// plugin's. For services, which must not trust a handle their caller names.
    #[must_use]
    pub fn plugin_of(&self, caller: Caller) -> Option<PluginRef> {
        let mut out = PluginHandle(0);
        // SAFETY: the function only compares `caller`, and `out` is writable.
        let status = unsafe { (self.api.plugin_of)(self.api.host, caller, &raw mut out) };
        (status == Status::OK).then_some(PluginRef(out))
    }

    /// The loader's version.
    #[must_use]
    pub fn loader_version(&self) -> String {
        str_from(self.api.loader_version)
    }

    /// Directory of the game executable.
    #[must_use]
    pub fn game_dir(&self) -> PathBuf {
        PathBuf::from(str_from(self.api.game_dir))
    }

    /// Full path of the game executable.
    #[must_use]
    pub fn exe_path(&self) -> PathBuf {
        PathBuf::from(str_from(self.api.exe_path))
    }

    /// Directory the plugins are loaded from.
    #[must_use]
    pub fn plugin_dir(&self) -> PathBuf {
        PathBuf::from(str_from(self.api.plugin_dir))
    }

    /// Directory holding plugin config files.
    #[must_use]
    pub fn config_dir(&self) -> PathBuf {
        PathBuf::from(str_from(self.api.config_dir))
    }

    /// Base address the game executable is mapped at.
    #[must_use]
    pub fn module_base(&self) -> *mut c_void {
        self.api.module_base
    }

    /// The game's command line verbatim, without the executable path.
    #[must_use]
    pub fn command_line(&self) -> String {
        str_from(self.api.command_line)
    }

    /// The game's command line, parsed. Covers `--flag`, `-key=value`, `/key value` and
    /// positionals; see [`CommandLine`] for lookups.
    #[must_use]
    pub fn args(&self) -> CommandLine {
        // SAFETY: the loader documents args/arg_count as a snapshot valid for our lifetime.
        let entries: &[ArgEntry] = unsafe { slice_from(self.api.args, self.api.arg_count) };
        CommandLine::parse(entries.iter().map(|e| str_from(e.raw)))
    }

    /// Every environment variable of the game process, as a snapshot taken when the loader
    /// initialised. Variable names keep their original case.
    #[must_use]
    pub fn env(&self) -> BTreeMap<String, String> {
        // SAFETY: the loader documents env/env_count as a snapshot valid for our lifetime.
        let entries: &[EnvEntry] = unsafe { slice_from(self.api.env, self.api.env_count) };
        entries
            .iter()
            .map(|e| (str_from(e.name), str_from(e.value)))
            .collect()
    }

    /// Write to the shared loader log. Prefer the `log` macros, which end up here.
    pub fn log(&self, level: LogLevel, msg: &str) {
        // SAFETY: valid table pointer; `msg` outlives the call.
        unsafe { (self.api.log)(self.api.host, self.handle, level, Str::new(msg)) }
    }

    /// Register a setting. Only allowed during `start`.
    ///
    /// # Errors
    /// Invalid key, duplicate key, bad default, or called outside `start`.
    pub fn setting(&self, setting: &Setting) -> Result<(), PluginError> {
        let choices = setting.choices.join("|");
        let mut flags = SettingFlags::NONE.0;
        if setting.restart_required {
            flags |= SettingFlags::RESTART_REQUIRED.0;
        }
        if setting.transient {
            flags |= SettingFlags::TRANSIENT.0;
        }
        if setting.advanced {
            flags |= SettingFlags::ADVANCED.0;
        }
        if setting.secret {
            flags |= SettingFlags::SECRET.0;
        }
        let desc = SettingDesc {
            struct_size: core::mem::size_of::<SettingDesc>(),
            key: Str::new(&setting.key),
            title: Str::new(&setting.title),
            description: Str::new(&setting.description),
            kind: setting.kind.to_abi(),
            default: Str::new(&setting.default),
            min: setting.min,
            max: setting.max,
            choices: Str::new(&choices),
            flags: SettingFlags(flags),
        };
        // SAFETY: `desc` and every string it borrows live until the call returns.
        check(unsafe { (self.api.setting_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Read a setting as text. `name` is a key of this plugin or `<plugin>.<key>`.
    ///
    /// # Errors
    /// Unknown setting.
    pub fn get(&self, name: &str) -> Result<String, PluginError> {
        let mut buf = vec![0u8; 256];
        loop {
            let mut len = 0usize;
            // SAFETY: `buf` is writable for `buf.len()` bytes and `len` is a valid out-pointer.
            let status = unsafe {
                (self.api.setting_get)(
                    self.api.host,
                    self.handle,
                    Str::new(name),
                    buf.as_mut_ptr(),
                    buf.len(),
                    &raw mut len,
                )
            };
            if status.is_ok() {
                buf.truncate(len);
                return Ok(String::from_utf8_lossy(&buf).into_owned());
            }
            if status != Status::BUFFER_TOO_SMALL || len <= buf.len() {
                return Err(PluginError::Status(status));
            }
            buf.resize(len, 0);
        }
    }

    /// Read a setting and parse it.
    ///
    /// # Errors
    /// Unknown setting or a value that does not parse as `T`.
    pub fn get_as<T: core::str::FromStr>(&self, name: &str) -> Result<T, PluginError> {
        let text = self.get(name)?;
        text.parse()
            .map_err(|_| PluginError::Message(format!("{name}: cannot parse {text:?}")))
    }

    /// Write a setting as text. Validated by the loader; fires `on_setting_changed`.
    ///
    /// # Errors
    /// Unknown setting or invalid value.
    pub fn set(&self, name: &str, value: &str) -> Result<(), PluginError> {
        // SAFETY: both strings outlive the call.
        check(unsafe {
            (self.api.setting_set)(self.api.host, self.handle, Str::new(name), Str::new(value))
        })
    }

    /// Register a hotkey action with a default binding such as `f12`. Only during `start`.
    ///
    /// # Errors
    /// Invalid name, duplicate, unparsable default, or called outside `start`.
    pub fn hotkey(
        &self,
        action: &str,
        title: &str,
        default_binding: &str,
    ) -> Result<(), PluginError> {
        let desc = HotkeyDesc {
            struct_size: core::mem::size_of::<HotkeyDesc>(),
            action: Str::new(action),
            title: Str::new(title),
            default_binding: Str::new(default_binding),
        };
        // SAFETY: `desc` and its strings live until the call returns.
        check(unsafe { (self.api.hotkey_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Suggest a bind: `script` runs on `default_binding` (for example `ctrl+add`) until the
    /// user moves or removes it. `name` shares the namespace of hotkey actions. Only during
    /// `start`.
    ///
    /// # Errors
    /// Invalid name, duplicate, unparsable keys or script, or called outside `start`.
    pub fn bind_default(
        &self,
        name: &str,
        title: &str,
        default_binding: &str,
        script: &str,
    ) -> Result<(), PluginError> {
        let desc = BindDesc {
            struct_size: core::mem::size_of::<BindDesc>(),
            name: Str::new(name),
            title: Str::new(title),
            default_binding: Str::new(default_binding),
            script: Str::new(script),
        };
        // SAFETY: `desc` and its strings live until the call returns.
        check(unsafe { (self.api.bind_default)(self.api.host, self.handle, &raw const desc) })
    }

    /// Report keys the game itself uses (`chords`, comma separated), with what they do in
    /// `label`, for example `game: Destination`. For adapter plugins that know the game's
    /// input mapping; binds on them still work and are pointed out.
    ///
    /// # Errors
    /// A key list that does not parse.
    pub fn reserve_keys(&self, label: &str, chords: &str) -> Result<(), PluginError> {
        // SAFETY: both strings outlive the call.
        check(unsafe {
            (self.api.keys_reserve)(
                self.api.host,
                self.handle,
                Str::new(label),
                Str::new(chords),
            )
        })
    }

    /// Register a console command. Only during `start`.
    ///
    /// # Errors
    /// Invalid name, duplicate, or called outside `start`.
    pub fn command(&self, name: &str, help: &str, usage: &str) -> Result<(), PluginError> {
        let desc = CommandDesc {
            struct_size: core::mem::size_of::<CommandDesc>(),
            name: Str::new(name),
            help: Str::new(help),
            usage: Str::new(usage),
        };
        // SAFETY: `desc` and its strings live until the call returns.
        check(unsafe { (self.api.command_register)(self.api.host, self.handle, &raw const desc) })
    }

    /// Print a line to the console.
    pub fn console_print(&self, line: &str) {
        // SAFETY: `line` outlives the call.
        unsafe { (self.api.console_print)(self.api.host, self.handle, Str::new(line)) }
    }

    /// Execute a console line as if typed.
    ///
    /// # Errors
    /// Whatever the console reports (unknown name, bad value, command failure).
    pub fn console_exec(&self, line: &str) -> Result<(), PluginError> {
        // SAFETY: `line` outlives the call.
        check(unsafe { (self.api.console_exec)(self.api.host, self.handle, Str::new(line)) })
    }

    /// Execute a console line and collect everything it printed. The status is returned
    /// alongside the output instead of as an error, because a failed command's message is
    /// in those lines.
    pub fn console_capture(&self, line: &str) -> (Status, Vec<String>) {
        let mut lines: Vec<String> = Vec::new();
        let ctx = core::ptr::from_mut(&mut lines).cast::<c_void>();
        // SAFETY: `line` outlives the call, and `ctx` points at `lines`, which outlives it
        // too; `collect_line` only ever receives this pointer.
        let status = unsafe {
            (self.api.console_capture)(
                self.api.host,
                self.handle,
                Str::new(line),
                Some(collect_line),
                ctx,
            )
        };
        (status, lines)
    }

    /// Find another plugin by name.
    #[must_use]
    pub fn find_plugin(&self, name: &str) -> Option<PluginRef> {
        let mut out = PluginHandle::NONE;
        // SAFETY: `out` is a valid out-pointer; `name` outlives the call.
        let status = unsafe { (self.api.plugin_find)(self.api.host, Str::new(name), &raw mut out) };
        status.is_ok().then_some(PluginRef(out))
    }

    /// The state of another plugin.
    #[must_use]
    pub fn plugin_state(&self, plugin: PluginRef) -> Option<PluginState> {
        let mut out = PluginState::WAITING;
        // SAFETY: `out` is a valid out-pointer.
        let status = unsafe { (self.api.plugin_state)(self.api.host, plugin.0, &raw mut out) };
        status.is_ok().then_some(out)
    }

    /// Send a synchronous message to another plugin and collect its reply, if any.
    ///
    /// # Errors
    /// Target gone, target does not handle messages, or target reported failure.
    pub fn message(
        &self,
        to: PluginRef,
        topic: &str,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>, PluginError> {
        let mut reply: Option<Vec<u8>> = None;
        // SAFETY: all borrowed data outlives the call; `reply` is only touched through
        // `collect_reply` during the call, as the ABI guarantees.
        let status = unsafe {
            (self.api.plugin_message)(
                self.api.host,
                self.handle,
                to.0,
                Str::new(topic),
                Bytes::new(payload),
                Some(collect_reply),
                core::ptr::addr_of_mut!(reply).cast(),
            )
        };
        check(status)?;
        Ok(reply)
    }

    /// Subscribe to a broadcast topic. Only during `start`.
    ///
    /// # Errors
    /// Called outside `start`.
    pub fn subscribe(&self, topic: &str) -> Result<(), PluginError> {
        // SAFETY: `topic` outlives the call.
        check(unsafe { (self.api.event_subscribe)(self.api.host, self.handle, Str::new(topic)) })
    }

    /// Broadcast to every subscriber of `topic`.
    pub fn publish(&self, topic: &str, payload: &[u8]) {
        // SAFETY: borrowed data outlives the call.
        let _ = unsafe {
            (self.api.event_publish)(
                self.api.host,
                self.handle,
                Str::new(topic),
                Bytes::new(payload),
            )
        };
    }

    /// Overwrite the bytes at `address`, with the loader keeping the originals and putting
    /// them back when the plugin stops.
    ///
    /// # Errors
    /// The address is not committed memory, the length is zero or absurd, or something has
    /// already hooked that address.
    ///
    /// # Safety
    /// The caller is patching the game's code: `address` must be the instruction boundary
    /// they mean, and `bytes` must be valid code for it.
    pub unsafe fn patch(
        &self,
        address: *mut c_void,
        bytes: &[u8],
        note: &str,
    ) -> Result<Hook, PluginError> {
        let mut id = 0u64;
        // SAFETY: `bytes` and `note` outlive the call; `id` is a valid out-pointer. The
        // caller's obligations are this function's own safety contract.
        check(unsafe {
            (self.api.hook_patch)(
                self.api.host,
                self.handle,
                address,
                bytes.as_ptr(),
                bytes.len(),
                Str::new(note),
                &raw mut id,
            )
        })?;
        Ok(Hook(id))
    }

    /// Replace one entry of the virtual table `object` points at, returning the hook and the
    /// pointer that was there, which is how the replacement reaches the original.
    ///
    /// # Errors
    /// A null pointer, a slot that is not writable, or a slot already hooked.
    ///
    /// # Safety
    /// `object` must point at an object with a virtual table of at least `index + 1` entries,
    /// and `replacement` must have the signature that entry is called with.
    pub unsafe fn hook_vtable(
        &self,
        object: *mut c_void,
        index: u32,
        replacement: *mut c_void,
        note: &str,
    ) -> Result<(Hook, *mut c_void), PluginError> {
        let (mut original, mut id) = (core::ptr::null_mut(), 0u64);
        // SAFETY: `note` outlives the call and both out-pointers are valid; the pointer
        // contracts are this function's own safety contract.
        check(unsafe {
            (self.api.hook_vtable)(
                self.api.host,
                self.handle,
                object,
                index,
                replacement,
                Str::new(note),
                &raw mut original,
                &raw mut id,
            )
        })?;
        Ok((Hook(id), original))
    }

    /// Detour `target` to `replacement`, returning the hook and a trampoline that calls the
    /// original function.
    ///
    /// # Errors
    /// A null pointer, an address that is not committed code, a prologue the loader cannot
    /// relocate, or an address something already hooked.
    ///
    /// # Safety
    /// `target` must be a function entry point and `replacement` must have its exact
    /// signature and calling convention; nothing can check that for the caller.
    pub unsafe fn detour(
        &self,
        target: *mut c_void,
        replacement: *mut c_void,
        note: &str,
    ) -> Result<(Hook, *mut c_void), PluginError> {
        let (mut trampoline, mut id) = (core::ptr::null_mut(), 0u64);
        // SAFETY: `note` outlives the call and both out-pointers are valid; the signature
        // match is this function's own safety contract.
        check(unsafe {
            (self.api.hook_detour)(
                self.api.host,
                self.handle,
                target,
                replacement,
                Str::new(note),
                &raw mut trampoline,
                &raw mut id,
            )
        })?;
        Ok((Hook(id), trampoline))
    }

    /// Undo one hook now. The loader undoes whatever is left when the plugin stops, so this
    /// is only needed to remove a hook earlier than that.
    ///
    /// # Errors
    /// The hook does not exist or belongs to another plugin.
    pub fn remove_hook(&self, hook: Hook) -> Result<(), PluginError> {
        // SAFETY: valid table pointer.
        check(unsafe { (self.api.hook_remove)(self.api.host, self.handle, hook.0) })
    }
}

/// Sink for [`Host::console_capture`]: pushes each line into the caller's vector.
///
/// # Safety
/// `ctx` must be the `Vec<String>` pointer `console_capture` passed, and `line` must be
/// readable for the duration of the call, which the ABI requires.
pub(crate) unsafe extern "C" fn collect_line(ctx: *mut c_void, line: Str) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: the only caller is the loader, with the pointer we handed it.
    let lines = unsafe { &mut *ctx.cast::<Vec<String>>() };
    lines.push(str_from(line));
}

/// A hook the loader installed for this plugin, and the handle to remove it early.
///
/// Dropping one does nothing: the loader owns the hook and removes it when the plugin stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hook(u64);
