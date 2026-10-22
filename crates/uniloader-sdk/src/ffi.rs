//! Export glue generated into every plugin by `export_plugin!`: builds the callback table,
//! owns the plugin instance and guards each callback against panics.

// Second and last unsafe module of the SDK: this is where loader pointers become references.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use uniloader_api::{
    self as api, Bytes, HostApi, LogLevel, PluginCallbacks, PluginHandle, PluginInfo, ReplyFn,
    Status, StopReason, Str, API_VERSION,
};

use crate::host::{bytes_from, str_from, Host, PluginError, PluginRef};
use crate::logger::HostLogger;
use crate::plugin::Plugin;

/// Marker trait proving a type can be exported; blanket-implemented for every [`Plugin`].
pub trait Exports: Plugin {}
impl<P: Plugin> Exports for P {}

/// Storage for the single instance of a plugin type inside its DLL.
pub struct Slot<P: Plugin> {
    state: OnceLock<Box<State<P>>>,
}

struct State<P> {
    host: Host,
    plugin: P,
}

impl<P: Plugin> Slot<P> {
    /// Create an empty slot (used by the macro in a `static`).
    #[must_use]
    pub const fn new() -> Self {
        Slot {
            state: OnceLock::new(),
        }
    }
}

impl<P: Plugin> Default for Slot<P> {
    fn default() -> Self {
        Self::new()
    }
}

/// `PluginInfo` holds raw string pointers, so it is not automatically `Sync`.
struct StaticInfo(PluginInfo);

/// The dependency list in ABI form, leaked so the loader may read it at any time.
///
/// Leaking is the point: the ABI requires the array to outlive the describe call, and a
/// plugin DLL is never unloaded while the game runs.
fn dependencies<P: Plugin>() -> (*const api::Dependency, usize) {
    if P::DEPENDENCIES.is_empty() {
        return (core::ptr::null(), 0);
    }
    let list: &'static [api::Dependency] = Vec::leak(
        P::DEPENDENCIES
            .iter()
            .map(|d| d.to_api())
            .collect::<Vec<_>>(),
    );
    (list.as_ptr(), list.len())
}

/// The provided service names in ABI form, leaked for the same reason as the dependencies.
fn provides<P: Plugin>() -> (*const Str, usize) {
    if P::PROVIDES.is_empty() {
        return (core::ptr::null(), 0);
    }
    let list: &'static [Str] = Vec::leak(P::PROVIDES.iter().map(|s| Str::new(s)).collect());
    (list.as_ptr(), list.len())
}

/// The authors in ABI form, leaked like the dependencies.
fn authors<P: Plugin>() -> (*const api::Author, usize) {
    leak(
        P::AUTHORS
            .iter()
            .map(|a| api::Author {
                name: Str::new(a.name),
                url: Str::new(a.link),
            })
            .collect(),
    )
}

/// The target filters in ABI form, leaked like the dependencies, values included.
fn targets<P: Plugin>() -> (*const api::TargetFilter, usize) {
    leak(
        P::TARGETS
            .iter()
            .map(|t| {
                let (values, value_count) =
                    leak(t.values.iter().map(|v| Str::new(v)).collect::<Vec<_>>());
                api::TargetFilter {
                    property: Str::new(t.property),
                    values,
                    value_count,
                }
            })
            .collect(),
    )
}

/// Leak a list for the ABI: a pointer and length, or null and 0 when it is empty.
fn leak<T: 'static>(list: Vec<T>) -> (*const T, usize) {
    if list.is_empty() {
        return (core::ptr::null(), 0);
    }
    let list: &'static [T] = Vec::leak(list);
    (list.as_ptr(), list.len())
}

// SAFETY: every `Str` inside points at a `&'static str` compiled into the plugin and the
// arrays are leaked, so everything reachable is immutable and lives for the whole
// process; reading it from any thread is safe.
unsafe impl Sync for StaticInfo {}
// SAFETY: see the `Sync` impl; `OnceLock` additionally requires `Send` of its contents.
unsafe impl Send for StaticInfo {}

static INFO: OnceLock<StaticInfo> = OnceLock::new();

/// Body of the generated describe export.
pub fn describe<P: Plugin>() -> *const PluginInfo {
    let info = INFO.get_or_init(|| {
        let (dependencies, dependency_count) = dependencies::<P>();
        let (provides, provides_count) = provides::<P>();
        let (authors, author_count) = authors::<P>();
        let (targets, target_count) = targets::<P>();
        StaticInfo(PluginInfo {
            struct_size: core::mem::size_of::<PluginInfo>(),
            api_version: API_VERSION,
            name: Str::new(P::NAME),
            title: Str::new(P::TITLE),
            version: Str::new(P::VERSION),
            description: Str::new(P::DESCRIPTION),
            authors,
            author_count,
            homepage: Str::new(P::HOMEPAGE),
            issues: Str::new(P::ISSUES),
            download: Str::new(P::DOWNLOAD),
            update: api::UpdateSource {
                github: Str::new(P::UPDATE_GITHUB),
                url: Str::new(P::UPDATE_URL),
            },
            targets,
            target_count,
            target_policy: if P::REFUSE_ON_MISMATCH {
                api::TargetPolicy::REFUSE
            } else {
                api::TargetPolicy::WARN
            },
            dependencies,
            dependency_count,
            provides,
            provides_count,
        })
    });
    core::ptr::from_ref(&info.0)
}

/// Body of the generated start export.
///
/// # Safety
/// `host` must point to a table valid for the plugin's lifetime and `callbacks` to writable memory.
pub unsafe fn start<P: Plugin>(
    slot: &'static Slot<P>,
    host: *const HostApi,
    handle: PluginHandle,
    callbacks: *mut PluginCallbacks,
) -> Status {
    if host.is_null() || callbacks.is_null() {
        return Status::INVALID_ARGUMENT;
    }
    // SAFETY: checked non-null; the loader keeps the table alive for our lifetime.
    let api = unsafe { &*host };
    if api.api_version != API_VERSION || api.struct_size < core::mem::size_of::<HostApi>() {
        return Status::UNSUPPORTED;
    }
    let host = Host::new(api, handle);
    HostLogger::install(host);

    let plugin = match catch_unwind(AssertUnwindSafe(|| P::start(host))) {
        Ok(Ok(plugin)) => plugin,
        Ok(Err(e)) => {
            host.log(LogLevel::ERROR, &format!("start failed: {e}"));
            return Status::ERROR;
        }
        Err(_) => {
            host.log(LogLevel::ERROR, "start panicked");
            return Status::ERROR;
        }
    };
    if slot.state.set(Box::new(State { host, plugin })).is_err() {
        host.log(LogLevel::ERROR, "start called twice");
        return Status::WRONG_PHASE;
    }
    let Some(state) = slot.state.get() else {
        return Status::ERROR;
    };
    // SAFETY: the loader hands us a writable PluginCallbacks of at least its struct_size.
    unsafe {
        callbacks.write(PluginCallbacks {
            struct_size: core::mem::size_of::<PluginCallbacks>(),
            ctx: core::ptr::from_ref::<State<P>>(state).cast_mut().cast(),
            on_hotkey: Some(on_hotkey::<P>),
            on_setting_changed: Some(on_setting_changed::<P>),
            on_command: Some(on_command::<P>),
            on_message: Some(on_message::<P>),
            on_event: Some(on_event::<P>),
            on_enable: Some(on_enable::<P>),
            on_disable: Some(on_disable::<P>),
            on_hotkey_release: Some(on_hotkey_release::<P>),
        });
    }
    Status::OK
}

/// Body of the generated stop export.
///
/// # Safety
/// `ctx` must be the pointer written by [`start`].
pub unsafe fn stop<P: Plugin>(slot: &'static Slot<P>, ctx: *mut c_void, reason: StopReason) {
    let Some(state) = slot.state.get() else {
        return;
    };
    if core::ptr::from_ref::<State<P>>(state)
        .cast_mut()
        .cast::<c_void>()
        != ctx
    {
        return;
    }
    guard(state, "stop", |s| s.plugin.stop(&s.host, reason));
}

/// Run a callback, turning a panic into a log line instead of unwinding into the loader.
fn guard<P: Plugin, R>(state: &State<P>, what: &str, f: impl FnOnce(&State<P>) -> R) -> Option<R> {
    catch_unwind(AssertUnwindSafe(|| f(state)))
        .map_err(|_| {
            state.host.log(LogLevel::ERROR, &format!("panic in {what}"));
        })
        .ok()
}

/// # Safety
/// `ctx` must be the `State<P>` written by [`start`].
unsafe fn state<'a, P: Plugin>(ctx: *mut c_void) -> &'a State<P> {
    // SAFETY: the loader only passes back the ctx we gave it, which points into a Box that
    // lives in a static slot for the rest of the process.
    unsafe { &*ctx.cast::<State<P>>().cast_const() }
}

unsafe extern "C" fn on_enable<P: Plugin>(ctx: *mut c_void) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    guard(s, "on_enable", |s| s.plugin.on_enable(&s.host));
}

unsafe extern "C" fn on_disable<P: Plugin>(ctx: *mut c_void) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    guard(s, "on_disable", |s| s.plugin.on_disable(&s.host));
}

unsafe extern "C" fn on_hotkey<P: Plugin>(ctx: *mut c_void, action: Str) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let action = str_from(action);
    guard(s, "on_hotkey", |s| s.plugin.on_hotkey(&s.host, &action));
}

unsafe extern "C" fn on_hotkey_release<P: Plugin>(ctx: *mut c_void, action: Str) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let action = str_from(action);
    guard(s, "on_hotkey_release", |s| {
        s.plugin.on_hotkey_release(&s.host, &action);
    });
}

unsafe extern "C" fn on_setting_changed<P: Plugin>(ctx: *mut c_void, key: Str, value: Str) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let (key, value) = (str_from(key), str_from(value));
    guard(s, "on_setting_changed", |s| {
        s.plugin.on_setting_changed(&s.host, &key, &value);
    });
}

fn status_of(result: Option<&Result<(), PluginError>>) -> Status {
    match result {
        Some(Ok(())) => Status::OK,
        Some(Err(PluginError::Unsupported)) => Status::UNSUPPORTED,
        Some(Err(PluginError::Status(s))) => *s,
        Some(Err(PluginError::Message(_))) | None => Status::ERROR,
    }
}

unsafe extern "C" fn on_command<P: Plugin>(ctx: *mut c_void, name: Str, args: Str) -> Status {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let (name, args) = (str_from(name), str_from(args));
    let result = guard(s, "on_command", |s| {
        s.plugin.on_command(&s.host, &name, &args).inspect_err(|e| {
            if let PluginError::Message(m) = e {
                s.host.console_print(&format!("{name}: {m}"));
            }
        })
    });
    status_of(result.as_ref())
}

unsafe extern "C" fn on_message<P: Plugin>(
    ctx: *mut c_void,
    from: PluginHandle,
    topic: Str,
    payload: Bytes,
    reply: Option<ReplyFn>,
    reply_ctx: *mut c_void,
) -> Status {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let topic = str_from(topic);
    let payload = bytes_from(payload);
    let result = guard(s, "on_message", |s| {
        s.plugin
            .on_message(&s.host, PluginRef(from), &topic, payload)
    });
    match result {
        Some(Ok(Some(bytes))) => {
            if let Some(reply) = reply {
                // SAFETY: the loader guarantees `reply`/`reply_ctx` are valid during this call.
                unsafe { reply(reply_ctx, Bytes::new(&bytes)) };
            }
            Status::OK
        }
        Some(Ok(None)) => Status::OK,
        other => status_of(other.map(|r| r.map(|_| ())).as_ref()),
    }
}

unsafe extern "C" fn on_event<P: Plugin>(
    ctx: *mut c_void,
    from: PluginHandle,
    topic: Str,
    payload: Bytes,
) {
    // SAFETY: documented pointer contracts of this callback.
    let s = unsafe { state::<P>(ctx) };
    let topic = str_from(topic);
    let payload = bytes_from(payload);
    guard(s, "on_event", |s| {
        s.plugin.on_event(&s.host, PluginRef(from), &topic, payload);
    });
}
