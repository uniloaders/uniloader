//! The host functions that read the registries (plugins, settings, hotkeys), rebind hotkeys,
//! and connect plugins through services and guarded calls.
//!
//! Enumeration copies what it needs under the state lock, releases it, and only then calls
//! the sink, so a sink may call back into the host.

// FFI module: the functions here are called by plugins through raw pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;

use uniloader_api::{
    Author, ConsoleKind, ConsoleLine, ConsoleLineFn, GuardedFn, HotkeyInfo, HotkeyInfoFn,
    KeyModifiers, LineFn, LogLevel, ModuleKind, PluginEntry, PluginEntryFn, PluginHandle,
    PluginState, ServiceInfo, SettingFlags, SettingInfo, SettingInfoFn, SettingKind, Status, Str,
};
use uniloader_core::keys::Modifiers;
use uniloader_core::settings;

use super::hostapi::text;
use super::{plugins, state};
use crate::described::{Described, Latest};
use crate::scrollback;

/// An owned [`PluginEntry`].
struct Entry {
    handle: PluginHandle,
    kind: ModuleKind,
    state: PluginState,
    info: Described,
    file: String,
    latest: Option<Latest>,
    mismatch: String,
}

fn collect_entries() -> Vec<Entry> {
    let guard = state();
    let mut out: Vec<Entry> = guard
        .plugins
        .iter()
        .enumerate()
        .map(|(i, p)| Entry {
            handle: PluginHandle(u32::try_from(i + 1).unwrap_or(0)),
            kind: ModuleKind::PLUGIN,
            state: p.state(),
            info: p.info.clone(),
            file: p.file.clone(),
            latest: p.latest.clone(),
            mismatch: p.mismatch.join("\n"),
        })
        .collect();
    out.extend(guard.pending.iter().map(|(name, reason)| Entry {
        handle: PluginHandle::NONE,
        kind: ModuleKind::PLUGIN,
        state: PluginState::WAITING,
        info: Described {
            name: name.clone(),
            title: name.clone(),
            description: reason.clone(),
            ..Described::default()
        },
        file: String::new(),
        latest: None,
        mismatch: String::new(),
    }));
    out.extend(guard.foreign.iter().map(|f| Entry {
        handle: PluginHandle::NONE,
        kind: if f.kind == "asi" {
            ModuleKind::ASI
        } else {
            ModuleKind::LIBRARY
        },
        state: PluginState::RUNNING,
        info: Described {
            name: f.name.clone(),
            title: f.name.clone(),
            version: f.version.clone(),
            ..Described::default()
        },
        file: f.file.clone(),
        latest: None,
        mismatch: String::new(),
    }));
    out
}

pub(super) unsafe extern "C" fn plugin_enumerate(
    _host: *mut c_void,
    sink: Option<PluginEntryFn>,
    ctx: *mut c_void,
) -> Status {
    let Some(sink) = sink else {
        return Status::INVALID_ARGUMENT;
    };
    for e in collect_entries() {
        let info = &e.info;
        let authors: Vec<Author> = info
            .authors
            .iter()
            .map(|(name, url)| Author {
                name: Str::new(name),
                url: Str::new(url),
            })
            .collect();
        let (latest, download, newer) = match &e.latest {
            Some(l) if !l.download.is_empty() => (l.version.as_str(), l.download.as_str(), l.newer),
            Some(l) => (l.version.as_str(), info.links.download.as_str(), l.newer),
            None => ("", info.links.download.as_str(), false),
        };
        let entry = PluginEntry {
            struct_size: core::mem::size_of::<PluginEntry>(),
            handle: e.handle,
            kind: e.kind,
            state: e.state,
            name: Str::new(&info.name),
            title: Str::new(&info.title),
            version: Str::new(&info.version),
            description: Str::new(&info.description),
            file: Str::new(&e.file),
            authors: authors.as_ptr(),
            author_count: authors.len(),
            homepage: Str::new(&info.links.homepage),
            issues: Str::new(&info.links.issues),
            download: Str::new(download),
            latest: Str::new(latest),
            update_available: u32::from(newer),
            mismatch: Str::new(&e.mismatch),
        };
        // SAFETY: `entry`, `authors` and their strings live until the sink returns, as
        // documented.
        unsafe { sink(ctx, &raw const entry) };
    }
    Status::OK
}

fn kind_of(kind: settings::Kind) -> SettingKind {
    match kind {
        settings::Kind::Bool => SettingKind::BOOL,
        settings::Kind::Int => SettingKind::INT,
        settings::Kind::Float => SettingKind::FLOAT,
        settings::Kind::Enum => SettingKind::ENUM,
        settings::Kind::String => SettingKind::STRING,
        settings::Kind::File => SettingKind::FILE,
        settings::Kind::Directory => SettingKind::DIRECTORY,
        settings::Kind::Colour => SettingKind::COLOUR,
        settings::Kind::Date => SettingKind::DATE,
        settings::Kind::Time => SettingKind::TIME,
        settings::Kind::DateTime => SettingKind::DATETIME,
        settings::Kind::Flags => SettingKind::FLAGS,
        settings::Kind::Key => SettingKind::KEY,
    }
}

fn flags_of(desc: &settings::Desc) -> SettingFlags {
    let mut bits = 0;
    if desc.restart_required {
        bits |= SettingFlags::RESTART_REQUIRED.0;
    }
    if desc.transient {
        bits |= SettingFlags::TRANSIENT.0;
    }
    if desc.secret {
        bits |= SettingFlags::SECRET.0;
    }
    if desc.advanced {
        bits |= SettingFlags::ADVANCED.0;
    }
    SettingFlags(bits)
}

pub(super) unsafe extern "C" fn setting_enumerate(
    _host: *mut c_void,
    plugin_name: Str,
    sink: Option<SettingInfoFn>,
    ctx: *mut c_void,
) -> Status {
    let Some(sink) = sink else {
        return Status::INVALID_ARGUMENT;
    };
    let only = text(plugin_name);
    let owned: Vec<(String, settings::Desc, String)> = {
        let guard = state();
        guard
            .plugins
            .iter()
            .filter(|p| only.is_empty() || p.name() == only)
            .flat_map(|p| {
                p.settings
                    .iter()
                    .map(|(desc, value)| {
                        let value = if desc.secret {
                            crate::secret::shown(value)
                        } else {
                            value
                        };
                        (p.name().to_owned(), desc.clone(), value.to_owned())
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    for (plugin, desc, value) in &owned {
        let choices = settings::choices_text(&desc.choices, &desc.labels);
        let info = SettingInfo {
            struct_size: core::mem::size_of::<SettingInfo>(),
            plugin: Str::new(plugin),
            key: Str::new(&desc.key),
            title: Str::new(&desc.title),
            description: Str::new(&desc.description),
            kind: kind_of(desc.kind),
            default: Str::new(&desc.default),
            min: desc.min,
            max: desc.max,
            choices: Str::new(&choices),
            flags: flags_of(desc),
            value: Str::new(value),
        };
        // SAFETY: `info` and its strings live until the sink returns, as documented.
        unsafe { sink(ctx, &raw const info) };
    }
    Status::OK
}

pub(super) unsafe extern "C" fn hotkey_enumerate(
    _host: *mut c_void,
    sink: Option<HotkeyInfoFn>,
    ctx: *mut c_void,
) -> Status {
    let Some(sink) = sink else {
        return Status::INVALID_ARGUMENT;
    };
    let owned: Vec<[String; 4]> = state()
        .binds
        .entries()
        .map(|e| {
            [
                e.name.clone(),
                e.title.clone(),
                e.binding(),
                e.default_binding(),
            ]
        })
        .collect();
    for [name, title, binding, default] in &owned {
        let info = HotkeyInfo {
            struct_size: core::mem::size_of::<HotkeyInfo>(),
            name: Str::new(name),
            title: Str::new(title),
            binding: Str::new(binding),
            default_binding: Str::new(default),
        };
        // SAFETY: `info` and its strings live until the sink returns, as documented.
        unsafe { sink(ctx, &raw const info) };
    }
    Status::OK
}

pub(super) unsafe extern "C" fn hotkey_rebind(
    _host: *mut c_void,
    _plugin: PluginHandle,
    name: Str,
    binding: Str,
) -> Status {
    let saved = state().rebind_hotkey(&text(name), &text(binding));
    match saved {
        Ok(persist) => {
            persist.write();
            Status::OK
        }
        Err(status) => status,
    }
}

pub(super) unsafe extern "C" fn service_register(
    host: *mut c_void,
    plugin: PluginHandle,
    name: Str,
    version: u32,
    table: *const c_void,
) -> Status {
    if !super::caller::is(host, plugin, "service_register") {
        return Status::INVALID_ARGUMENT;
    }
    let name = text(name);
    let result = state().register_service(plugin, &name, version, table as usize);
    match result {
        Ok(()) => {
            // From the state, not the published list: providing happens during `start`,
            // before the plugin is published.
            let owner = state()
                .plugin(plugin)
                .map_or_else(|| "?".to_owned(), |p| p.name().to_owned());
            log::info!("[{owner}] provides {name} v{version}");
            Status::OK
        }
        Err(e) => e,
    }
}

pub(super) unsafe extern "C" fn service_get(
    _host: *mut c_void,
    _plugin: PluginHandle,
    name: Str,
    out: *mut ServiceInfo,
) -> Status {
    if out.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: checked non-null; the ABI requires a writable struct with `struct_size` set.
    let size = unsafe { (*out).struct_size };
    if size < core::mem::size_of::<ServiceInfo>() {
        return Status::UNSUPPORTED;
    }
    let Some(service) = state().service(&text(name)) else {
        return Status::NOT_FOUND;
    };
    // SAFETY: as above.
    unsafe {
        out.write(ServiceInfo {
            struct_size: core::mem::size_of::<ServiceInfo>(),
            table: service.table as *const c_void,
            version: service.version,
            provider: service.provider,
        });
    }
    Status::OK
}

fn console_kind(kind: scrollback::Kind) -> (ConsoleKind, LogLevel) {
    match kind {
        scrollback::Kind::Output => (ConsoleKind::OUTPUT, LogLevel::INFO),
        scrollback::Kind::Echo => (ConsoleKind::ECHO, LogLevel::INFO),
        scrollback::Kind::Log(level) => (
            ConsoleKind::LOG,
            match level {
                log::Level::Error => LogLevel::ERROR,
                log::Level::Warn => LogLevel::WARN,
                log::Level::Info => LogLevel::INFO,
                log::Level::Debug => LogLevel::DEBUG,
                log::Level::Trace => LogLevel::TRACE,
            },
        ),
    }
}

pub(super) unsafe extern "C" fn console_read(
    _host: *mut c_void,
    since: u64,
    sink: Option<ConsoleLineFn>,
    ctx: *mut c_void,
    out_next: *mut u64,
) -> Status {
    let (lines, next) = {
        let mut guard = state();
        // Whatever the logger queued belongs in the scrollback before anyone reads it.
        guard.console_drain();
        (guard.console_all_since(since), guard.console_mark())
    };
    if !out_next.is_null() {
        // SAFETY: checked non-null; the ABI requires a writable `u64`.
        unsafe { out_next.write(next) };
    }
    if let Some(sink) = sink {
        for line in &lines {
            let (kind, level) = console_kind(line.kind);
            let entry = ConsoleLine {
                struct_size: core::mem::size_of::<ConsoleLine>(),
                seq: line.seq,
                kind,
                level,
                clock: Str::new(&line.clock),
                target: Str::new(&line.target),
                text: Str::new(&line.text),
            };
            // SAFETY: `entry` and its strings live until the sink returns, as documented.
            unsafe { sink(ctx, &raw const entry) };
        }
    }
    Status::OK
}

pub(super) unsafe extern "C" fn console_names(
    _host: *mut c_void,
    sink: Option<LineFn>,
    ctx: *mut c_void,
) -> Status {
    let Some(sink) = sink else {
        return Status::INVALID_ARGUMENT;
    };
    let names = state().completion_names();
    for name in &names {
        // SAFETY: `name` lives until the sink returns, as documented.
        unsafe { sink(ctx, Str::new(name)) };
    }
    Status::OK
}

pub(super) unsafe extern "C" fn hotkey_press(
    _host: *mut c_void,
    _plugin: PluginHandle,
    scancode: u16,
    modifiers: KeyModifiers,
) -> Status {
    let held = Modifiers {
        ctrl: modifiers.0 & KeyModifiers::CTRL != 0,
        alt: modifiers.0 & KeyModifiers::ALT != 0,
        shift: modifiers.0 & KeyModifiers::SHIFT != 0,
    };
    let fired = {
        let mut guard = state();
        let pressed = guard.binds.press(scancode, held);
        guard.allowed_binds(pressed)
    };
    super::exec::deliver_keys(fired);
    Status::OK
}

pub(super) unsafe extern "C" fn input_capture(
    host: *mut c_void,
    plugin: PluginHandle,
    capture: bool,
) -> Status {
    if !super::caller::is(host, plugin, "input_capture") {
        return Status::INVALID_ARGUMENT;
    }
    let mut guard = state();
    if guard.plugin(plugin).is_none() {
        return Status::INVALID_ARGUMENT;
    }
    guard.claim_input(plugin, capture);
    Status::OK
}

pub(super) unsafe extern "C" fn guarded_call(
    _host: *mut c_void,
    target: PluginHandle,
    f: Option<GuardedFn>,
    ctx: *mut c_void,
) -> Status {
    let Some(f) = f else {
        return Status::INVALID_ARGUMENT;
    };
    plugins::guarded(target, || {
        // SAFETY: the caller vouches for `f` and `ctx`; the guard contains a fault inside.
        unsafe { f(ctx) };
    })
}
