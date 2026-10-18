//! Construction of the [`HostApi`] table and the implementations behind its function
//! pointers. Every entry converts to safe types, works on the locked state, and performs
//! plugin calls only after the lock is released.
//!
//! From the DayZ loader's `win/hostapi.rs` (`eaebd9b`) without the address database, the
//! overlay and the input services, which adapter plugins provide as messages instead.

// FFI module: the functions here are called by plugins through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use uniloader_api::{
    ArgEntry, Bytes, CommandDesc, EnvEntry, HostApi, HotkeyDesc, LineFn, LogLevel, PluginHandle,
    PluginState, ReplyFn, SettingDesc, SettingFlags, SettingKind, Status, Str, API_VERSION,
};
use uniloader_core::settings;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::config::Paths;
use crate::process::Process;
use crate::state::CommandInfo;

use super::hostapi_registry as registry;
use super::{console, plugin_hooks, plugins, state};

/// Owned backing storage for every string the table hands out. Leaked once at startup so
/// the pointers stay valid for as long as any plugin can hold them.
struct Storage {
    loader_version: String,
    game_dir: String,
    exe_path: String,
    plugin_dir: String,
    config_dir: String,
    command_line: String,
    args: Vec<ArgEntry>,
    env: Vec<EnvEntry>,
    // Keeps the strings the `Str`s above point into alive.
    _owned: Vec<String>,
}

/// Build the host table for a directory layout and process snapshot.
pub(crate) fn build(paths: &Paths, process: &Process) -> &'static HostApi {
    let mut owned = Vec::new();
    let mut keep = |s: &str| -> Str {
        owned.push(s.to_owned());
        // The string is owned by `owned`, which the leaked `Storage` keeps alive forever.
        let last = owned.last().map_or("", String::as_str);
        Str::new(last)
    };
    let args: Vec<ArgEntry> = process
        .command_line
        .args()
        .iter()
        .map(|a| ArgEntry {
            name: keep(&a.name),
            value: keep(a.value.as_deref().unwrap_or("")),
            raw: keep(&a.raw),
            has_value: u32::from(a.value.is_some()),
        })
        .collect();
    let env: Vec<EnvEntry> = process
        .env
        .iter()
        .map(|(k, v)| EnvEntry {
            name: keep(k),
            value: keep(v),
        })
        .collect();
    // SAFETY: a null name asks for the executable's own module, which always exists.
    let module_base = unsafe { GetModuleHandleW(None) }.map_or(core::ptr::null_mut(), |m| m.0);
    let storage = Box::leak(Box::new(Storage {
        loader_version: env!("CARGO_PKG_VERSION").to_owned(),
        game_dir: paths.game_dir.to_string_lossy().into_owned(),
        exe_path: super::proxy::module_path(None)
            .to_string_lossy()
            .into_owned(),
        plugin_dir: paths.plugins_dir.to_string_lossy().into_owned(),
        config_dir: paths.config_dir.to_string_lossy().into_owned(),
        command_line: process.raw_command_line.clone(),
        args,
        env,
        _owned: owned,
    }));
    Box::leak(Box::new(HostApi {
        struct_size: core::mem::size_of::<HostApi>(),
        api_version: API_VERSION,
        host: core::ptr::null_mut(),
        loader_version: Str::new(&storage.loader_version),
        game_dir: Str::new(&storage.game_dir),
        exe_path: Str::new(&storage.exe_path),
        plugin_dir: Str::new(&storage.plugin_dir),
        config_dir: Str::new(&storage.config_dir),
        module_base,
        command_line: Str::new(&storage.command_line),
        args: storage.args.as_ptr(),
        arg_count: storage.args.len(),
        env: storage.env.as_ptr(),
        env_count: storage.env.len(),
        log: host_log,
        setting_register,
        setting_get,
        setting_set,
        hotkey_register,
        command_register,
        console_print,
        console_exec,
        console_capture,
        plugin_find,
        plugin_state,
        plugin_message,
        event_subscribe,
        event_publish,
        hook_patch,
        hook_vtable,
        hook_detour,
        hook_remove,
        plugin_enumerate: registry::plugin_enumerate,
        setting_enumerate: registry::setting_enumerate,
        hotkey_enumerate: registry::hotkey_enumerate,
        hotkey_rebind: registry::hotkey_rebind,
        service_register: registry::service_register,
        service_get: registry::service_get,
        guarded_call: registry::guarded_call,
        console_read: registry::console_read,
        console_names: registry::console_names,
    }))
}

/// Borrow a `Str` a plugin passed us. Invalid UTF-8 becomes the replacement character
/// rather than an error, so a C plugin's mistake cannot abort a call.
pub(super) fn text(s: Str) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    // SAFETY: the ABI requires ptr/len to describe readable memory for this call.
    let bytes = unsafe { core::slice::from_raw_parts(s.ptr, s.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn payload(b: Bytes) -> Vec<u8> {
    if b.ptr.is_null() || b.len == 0 {
        return Vec::new();
    }
    // SAFETY: the ABI requires ptr/len to describe readable memory for this call.
    unsafe { core::slice::from_raw_parts(b.ptr, b.len) }.to_vec()
}

/// Name of a plugin for log lines, `?` for an unknown handle.
fn plugin_name(plugin: PluginHandle) -> String {
    state()
        .plugin(plugin)
        .map_or_else(|| "?".to_owned(), |p| p.name().to_owned())
}

unsafe extern "C" fn host_log(_host: *mut c_void, plugin: PluginHandle, level: LogLevel, msg: Str) {
    let name = plugin_name(plugin);
    let message = text(msg);
    match level {
        LogLevel::ERROR => log::error!("[{name}] {message}"),
        LogLevel::WARN => log::warn!("[{name}] {message}"),
        LogLevel::DEBUG => log::debug!("[{name}] {message}"),
        LogLevel::TRACE => log::trace!("[{name}] {message}"),
        // `INFO`, plus any level a future ABI adds that this loader cannot interpret.
        _ => log::info!("[{name}] {message}"),
    }
}

unsafe extern "C" fn setting_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const SettingDesc,
) -> Status {
    if desc.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: checked non-null; the ABI requires it to point at a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<SettingDesc>() {
        return Status::UNSUPPORTED;
    }
    let desc = settings::Desc {
        key: text(d.key),
        title: text(d.title),
        description: text(d.description),
        kind: match d.kind {
            SettingKind::BOOL => settings::Kind::Bool,
            SettingKind::INT => settings::Kind::Int,
            SettingKind::FLOAT => settings::Kind::Float,
            SettingKind::ENUM => settings::Kind::Enum,
            SettingKind::STRING => settings::Kind::String,
            // A kind this loader does not know cannot be validated; refuse the setting.
            _ => return Status::UNSUPPORTED,
        },
        default: text(d.default),
        min: d.min,
        max: d.max,
        choices: text(d.choices)
            .split('|')
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .collect(),
        restart_required: d.flags.contains(SettingFlags::RESTART_REQUIRED),
        advanced: d.flags.contains(SettingFlags::ADVANCED),
        transient: d.flags.contains(SettingFlags::TRANSIENT),
    };
    state()
        .register_setting(plugin, desc)
        .map_or_else(|e| e, |()| Status::OK)
}

unsafe extern "C" fn setting_get(
    _host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    buf: *mut u8,
    cap: usize,
    out_len: *mut usize,
) -> Status {
    if out_len.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    let value = match state().get_setting(Some(plugin), &text(name)) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // SAFETY: checked non-null; the ABI requires a writable `usize`.
    unsafe { out_len.write(value.len()) };
    if value.len() > cap || buf.is_null() {
        return Status::BUFFER_TOO_SMALL;
    }
    // SAFETY: `buf` is writable for `cap` bytes and `value` fits, per the check above.
    unsafe { core::ptr::copy_nonoverlapping(value.as_ptr(), buf, value.len()) };
    Status::OK
}

unsafe extern "C" fn setting_set(
    _host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    value: Str,
) -> Status {
    let (name, value) = (text(name), text(value));
    let outcome = state().set_setting(Some(plugin), &name, &value);
    match outcome {
        Ok(notify) => {
            plugins::deliver(notify.into_iter().collect());
            Status::OK
        }
        Err((status, message)) => {
            log::warn!("set {name} = {value:?}: {message}");
            status
        }
    }
}

unsafe extern "C" fn hotkey_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const HotkeyDesc,
) -> Status {
    if desc.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: checked non-null; the ABI requires a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<HotkeyDesc>() {
        return Status::UNSUPPORTED;
    }
    state()
        .register_hotkey(
            plugin,
            &text(d.action),
            &text(d.title),
            &text(d.default_binding),
        )
        .map_or_else(|e| e, |_| Status::OK)
}

unsafe extern "C" fn command_register(
    _host: *mut c_void,
    plugin: PluginHandle,
    desc: *const CommandDesc,
) -> Status {
    if desc.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: checked non-null; the ABI requires a valid descriptor.
    let d = unsafe { &*desc };
    if d.struct_size < core::mem::size_of::<CommandDesc>() {
        return Status::UNSUPPORTED;
    }
    let info = CommandInfo {
        help: text(d.help),
        usage: text(d.usage),
    };
    state()
        .register_command(plugin, &text(d.name), info)
        .map_or_else(|e| e, |()| Status::OK)
}

unsafe extern "C" fn console_print(_host: *mut c_void, plugin: PluginHandle, line: Str) {
    let mut guard = state();
    let name = guard
        .plugin(plugin)
        .map_or_else(|| "?".to_owned(), |p| p.name().to_owned());
    // Same shape as a log line, because a plugin printing to the console is the same kind of
    // event as a plugin logging, and interleaved output is unreadable without a clock.
    let line = format!("{} [INFO ] [{name}] {}", console::clock(), text(line));
    console::print(&line);
    guard.console_print(line);
}

unsafe extern "C" fn console_exec(_host: *mut c_void, plugin: PluginHandle, line: Str) -> Status {
    super::run_console_line(Some(plugin), &text(line))
}

unsafe extern "C" fn console_capture(
    _host: *mut c_void,
    plugin: PluginHandle,
    line: Str,
    sink: Option<LineFn>,
    line_ctx: *mut c_void,
) -> Status {
    let (status, printed) = super::run_console_line_capture(Some(plugin), &text(line));
    if let Some(sink) = sink {
        for line in &printed {
            // SAFETY: the ABI requires `sink` to accept a borrowed line for the duration of
            // the call, which is exactly how long `line` lives here.
            unsafe { sink(line_ctx, Str::new(line)) };
        }
    }
    status
}

/// Any plugin that started this session, running or not; [`plugin_state`] tells which.
unsafe extern "C" fn plugin_find(_host: *mut c_void, name: Str, out: *mut PluginHandle) -> Status {
    if out.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    let Some(handle) = state().find_plugin(&text(name)) else {
        return Status::NOT_FOUND;
    };
    // SAFETY: checked non-null; the ABI requires a writable handle.
    unsafe { out.write(handle) };
    Status::OK
}

/// Running, or not. A plugin waiting for a dependency has no handle yet, so `WAITING` is
/// never the answer here, and a fault is not yet told apart from a pause.
unsafe extern "C" fn plugin_state(
    _host: *mut c_void,
    plugin: PluginHandle,
    out: *mut PluginState,
) -> Status {
    if out.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    let Some(enabled) = state().plugin(plugin).map(|p| p.enabled) else {
        return Status::NOT_FOUND;
    };
    let value = if enabled {
        PluginState::RUNNING
    } else {
        PluginState::DISABLED
    };
    // SAFETY: checked non-null; the ABI requires a writable out-param.
    unsafe { out.write(value) };
    Status::OK
}

unsafe extern "C" fn plugin_message(
    _host: *mut c_void,
    from: PluginHandle,
    to: PluginHandle,
    topic: Str,
    data: Bytes,
    reply: Option<ReplyFn>,
    reply_ctx: *mut c_void,
) -> Status {
    plugins::deliver_message(from, to, &text(topic), &payload(data), reply, reply_ctx)
}

unsafe extern "C" fn event_subscribe(
    _host: *mut c_void,
    plugin: PluginHandle,
    topic: Str,
) -> Status {
    state()
        .subscribe(plugin, &text(topic))
        .map_or_else(|e| e, |()| Status::OK)
}

unsafe extern "C" fn event_publish(
    _host: *mut c_void,
    from: PluginHandle,
    topic: Str,
    data: Bytes,
) -> Status {
    let topic = text(topic);
    let data = payload(data);
    let targets = state().subscribers(from, &topic);
    plugins::deliver_event(from, &targets, &topic, &data);
    Status::OK
}

unsafe extern "C" fn hook_patch(
    _host: *mut c_void,
    plugin: PluginHandle,
    address: *mut c_void,
    bytes: *const u8,
    len: usize,
    note: Str,
    out_id: *mut u64,
) -> Status {
    if bytes.is_null() || out_id.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: the ABI requires `bytes`/`len` to describe readable memory for this call.
    let patch = unsafe { core::slice::from_raw_parts(bytes, len) };
    match plugin_hooks::patch(plugin, address as usize, patch, &text(note)) {
        Ok(id) => {
            // SAFETY: checked non-null; the ABI requires a writable `u64`.
            unsafe { out_id.write(id) };
            Status::OK
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn hook_vtable(
    _host: *mut c_void,
    plugin: PluginHandle,
    object: *mut c_void,
    index: u32,
    replacement: *mut c_void,
    note: Str,
    out_original: *mut *mut c_void,
    out_id: *mut u64,
) -> Status {
    if out_original.is_null() || out_id.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    match plugin_hooks::vtable(plugin, object, index, replacement, &text(note)) {
        Ok((id, original)) => {
            // SAFETY: both checked non-null; the ABI requires writable out-params.
            unsafe {
                out_original.write(original);
                out_id.write(id);
            }
            Status::OK
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn hook_detour(
    _host: *mut c_void,
    plugin: PluginHandle,
    target: *mut c_void,
    replacement: *mut c_void,
    note: Str,
    out_trampoline: *mut *mut c_void,
    out_id: *mut u64,
) -> Status {
    if out_trampoline.is_null() || out_id.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    match plugin_hooks::detour(plugin, target, replacement, &text(note)) {
        Ok((id, trampoline)) => {
            // SAFETY: both checked non-null; the ABI requires writable out-params.
            unsafe {
                out_trampoline.write(trampoline);
                out_id.write(id);
            }
            Status::OK
        }
        Err(status) => status,
    }
}

unsafe extern "C" fn hook_remove(_host: *mut c_void, plugin: PluginHandle, id: u64) -> Status {
    plugin_hooks::remove(plugin, id)
}
