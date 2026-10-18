//! What the loader knows (modules, settings, hotkeys), and services between plugins.

// The plugin side of the FFI boundary for the registry functions; every unsafe block
// documents the loader contract it relies on.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::marker::PhantomData;

use uniloader_api::{
    HotkeyInfo, ModuleKind, PluginEntry, PluginHandle, PluginState, ServiceInfo, SettingInfo,
    Status, Str,
};

use crate::host::{check, str_from, Host, PluginError, PluginRef};
use crate::settings::SettingKind;

/// One module the loader loaded, or a plugin waiting for a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleEntry {
    /// The plugin, or `None` for `.asi` files, libraries and waiting plugins.
    pub plugin: Option<PluginRef>,
    /// What it is.
    pub kind: ModuleKind,
    /// Its state.
    pub state: PluginState,
    /// Plugin name, or file stem.
    pub name: String,
    /// Human readable name.
    pub title: String,
    /// Version text, possibly empty.
    pub version: String,
    /// Authors.
    pub authors: String,
    /// Description, or why a waiting plugin waits.
    pub description: String,
    /// File relative to the game directory.
    pub file: String,
}

/// One setting of any plugin, with its value.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingEntry {
    /// Owning plugin.
    pub plugin: String,
    /// Key within the plugin.
    pub key: String,
    /// Label.
    pub title: String,
    /// Help text.
    pub description: String,
    /// Type.
    pub kind: SettingKind,
    /// Lower bound for numbers; equal to `max` when unbounded.
    pub min: f64,
    /// Upper bound for numbers.
    pub max: f64,
    /// Choices of an enum setting.
    pub choices: Vec<String>,
    /// Default as text.
    pub default: String,
    /// Current value as text.
    pub value: String,
    /// Needs a restart to take effect.
    pub restart_required: bool,
    /// Not persisted.
    pub transient: bool,
    /// Hidden behind an "advanced" toggle in editors.
    pub advanced: bool,
}

/// One hotkey action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyEntry {
    /// Qualified name, `<plugin>.<action>`.
    pub name: String,
    /// Label.
    pub title: String,
    /// Current binding, empty when unbound.
    pub binding: String,
    /// What the plugin asked for.
    pub default_binding: String,
}

/// A service another plugin provides, typed by its client crate.
pub struct Service<T: 'static> {
    /// The provider's function table.
    pub table: &'static T,
    /// Version the provider registered.
    pub version: u32,
    /// Who provides it.
    pub provider: PluginRef,
    _not_send: PhantomData<*const T>,
}

impl<T> Clone for Service<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Service<T> {}

impl<T> core::fmt::Debug for Service<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Service")
            .field("version", &self.version)
            .field("provider", &self.provider)
            .finish_non_exhaustive()
    }
}

/// A `loader.plugin_state` event, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStateEvent {
    /// The plugin that changed.
    pub plugin: PluginRef,
    /// What it is now.
    pub state: PluginState,
    /// Its name.
    pub name: String,
}

impl PluginStateEvent {
    /// Decode the payload of [`uniloader_api::topics::PLUGIN_STATE`].
    #[must_use]
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let handle = u32::from_le_bytes(payload.get(0..4)?.try_into().ok()?);
        let state = u32::from_le_bytes(payload.get(4..8)?.try_into().ok()?);
        Some(PluginStateEvent {
            plugin: PluginRef(PluginHandle(handle)),
            state: PluginState(state),
            name: String::from_utf8_lossy(payload.get(8..)?).into_owned(),
        })
    }
}

/// Sink pushing a converted entry into the caller's vector.
///
/// # Safety
/// `ctx` must be the `Vec<O>` the enumerating call passed, and `item` a valid entry.
unsafe extern "C" fn collect<I, O>(ctx: *mut c_void, item: *const I)
where
    O: for<'a> From<&'a I>,
{
    if ctx.is_null() || item.is_null() {
        return;
    }
    // SAFETY: the only caller is the loader, with the pointer we handed it, and an entry
    // valid for the duration of this call.
    let (out, item) = unsafe { (&mut *ctx.cast::<Vec<O>>(), &*item) };
    out.push(O::from(item));
}

impl From<&PluginEntry> for ModuleEntry {
    fn from(e: &PluginEntry) -> Self {
        ModuleEntry {
            plugin: (e.handle != PluginHandle::NONE).then_some(PluginRef(e.handle)),
            kind: e.kind,
            state: e.state,
            name: str_from(e.name),
            title: str_from(e.title),
            version: str_from(e.version),
            authors: str_from(e.authors),
            description: str_from(e.description),
            file: str_from(e.file),
        }
    }
}

impl From<&SettingInfo> for SettingEntry {
    fn from(s: &SettingInfo) -> Self {
        let choices: Vec<String> = str_from(s.choices)
            .split('|')
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
            .collect();
        let kind = match s.kind {
            uniloader_api::SettingKind::BOOL => SettingKind::Bool,
            uniloader_api::SettingKind::INT => SettingKind::Int,
            uniloader_api::SettingKind::FLOAT => SettingKind::Float,
            uniloader_api::SettingKind::ENUM => SettingKind::Enum,
            // Text, and any kind a newer loader adds: text is the one every value has.
            _ => SettingKind::String,
        };
        SettingEntry {
            plugin: str_from(s.plugin),
            key: str_from(s.key),
            title: str_from(s.title),
            description: str_from(s.description),
            kind,
            min: s.min,
            max: s.max,
            choices,
            default: str_from(s.default),
            value: str_from(s.value),
            restart_required: s
                .flags
                .contains(uniloader_api::SettingFlags::RESTART_REQUIRED),
            transient: s.flags.contains(uniloader_api::SettingFlags::TRANSIENT),
            advanced: s.flags.contains(uniloader_api::SettingFlags::ADVANCED),
        }
    }
}

impl From<&HotkeyInfo> for HotkeyEntry {
    fn from(h: &HotkeyInfo) -> Self {
        HotkeyEntry {
            name: str_from(h.name),
            title: str_from(h.title),
            binding: str_from(h.binding),
            default_binding: str_from(h.default_binding),
        }
    }
}

/// Trampoline for [`Host::guarded`]: runs the closure stored behind `ctx` once.
unsafe extern "C" fn run_once<F: FnOnce()>(ctx: *mut c_void) {
    // SAFETY: `ctx` is the `Option<F>` of the `guarded` call in progress.
    let slot = unsafe { &mut *ctx.cast::<Option<F>>() };
    if let Some(f) = slot.take() {
        // A panic must not unwind out of an `extern "C"` function; the loader's guard sees
        // only hardware faults, so report a panic as one by catching it here.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
            log::error!("a guarded call panicked");
        }
    }
}

impl Host {
    /// Every started plugin, every waiting one, then `.asi` files and chain-loaded libraries.
    #[must_use]
    pub fn modules(&self) -> Vec<ModuleEntry> {
        let mut out: Vec<ModuleEntry> = Vec::new();
        // SAFETY: `out` outlives the call and the sink only receives that pointer.
        let _ = unsafe {
            (self.api.plugin_enumerate)(
                self.api.host,
                Some(collect::<PluginEntry, ModuleEntry>),
                core::ptr::from_mut(&mut out).cast(),
            )
        };
        out
    }

    /// Every setting of `plugin`, or of all plugins for `None`.
    #[must_use]
    pub fn settings(&self, plugin: Option<&str>) -> Vec<SettingEntry> {
        let mut out: Vec<SettingEntry> = Vec::new();
        // SAFETY: as in `modules`; the name outlives the call.
        let _ = unsafe {
            (self.api.setting_enumerate)(
                self.api.host,
                Str::new(plugin.unwrap_or("")),
                Some(collect::<SettingInfo, SettingEntry>),
                core::ptr::from_mut(&mut out).cast(),
            )
        };
        out
    }

    /// Every hotkey action with its binding.
    #[must_use]
    pub fn hotkeys(&self) -> Vec<HotkeyEntry> {
        let mut out: Vec<HotkeyEntry> = Vec::new();
        // SAFETY: as in `modules`.
        let _ = unsafe {
            (self.api.hotkey_enumerate)(
                self.api.host,
                Some(collect::<HotkeyInfo, HotkeyEntry>),
                core::ptr::from_mut(&mut out).cast(),
            )
        };
        out
    }

    /// Bind any hotkey action (`<plugin>.<action>`) to `binding`; empty unbinds it. Saved.
    ///
    /// # Errors
    /// Unknown action or a binding that does not parse.
    pub fn rebind(&self, name: &str, binding: &str) -> Result<(), PluginError> {
        // SAFETY: both strings outlive the call.
        check(unsafe {
            (self.api.hotkey_rebind)(
                self.api.host,
                self.handle,
                Str::new(name),
                Str::new(binding),
            )
        })
    }

    /// Provide a service under `name`.
    ///
    /// # Errors
    /// The name is invalid or taken.
    ///
    /// # Safety
    /// `T` must be the `#[repr(C)]` layout the service's consumers expect for `version`, and
    /// every function in it must be callable from any thread.
    pub unsafe fn provide<T: Sync>(
        &self,
        name: &str,
        version: u32,
        table: &'static T,
    ) -> Result<(), PluginError> {
        // SAFETY: `table` is `'static`; the layout contract is this function's own.
        check(unsafe {
            (self.api.service_register)(
                self.api.host,
                self.handle,
                Str::new(name),
                version,
                core::ptr::from_ref(table).cast(),
            )
        })
    }

    /// Look a service up. `None` when nobody provides it or its provider is not running.
    ///
    /// # Safety
    /// `T` must be the layout the provider registered for the returned version; check
    /// [`Service::version`] before using fields a later version added.
    #[must_use]
    pub unsafe fn service<T>(&self, name: &str) -> Option<Service<T>> {
        let mut info = ServiceInfo {
            struct_size: core::mem::size_of::<ServiceInfo>(),
            table: core::ptr::null(),
            version: 0,
            provider: PluginHandle::NONE,
        };
        // SAFETY: `info` is a valid out-pointer with its size set; `name` outlives the call.
        let status = unsafe {
            (self.api.service_get)(self.api.host, self.handle, Str::new(name), &raw mut info)
        };
        if status != Status::OK || info.table.is_null() {
            return None;
        }
        Some(Service {
            // SAFETY: the provider registered a `'static` table of this layout, which the
            // caller vouches for.
            table: unsafe { &*info.table.cast::<T>() },
            version: info.version,
            provider: PluginRef(info.provider),
            _not_send: PhantomData,
        })
    }

    /// Run `f` on behalf of plugin `target` under the loader's fault guard: a hardware fault
    /// inside disables `target`, not the caller. For services calling into consumers.
    ///
    /// # Errors
    /// `target` is not running (then `f` is not called) or `f` faulted.
    pub fn guarded<F: FnOnce()>(&self, target: PluginRef, f: F) -> Result<(), PluginError> {
        let mut slot = Some(f);
        // SAFETY: `slot` outlives the call and `run_once::<F>` only receives that pointer.
        check(unsafe {
            (self.api.guarded_call)(
                self.api.host,
                target.0,
                Some(run_once::<F>),
                core::ptr::from_mut(&mut slot).cast(),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_state_payloads_round_trip() {
        let mut payload = 3u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&PluginState::FAULTED.0.to_le_bytes());
        payload.extend_from_slice(b"coop");
        let event = PluginStateEvent::parse(&payload).unwrap_or_else(|| panic!("parse"));
        assert_eq!(event.plugin, PluginRef(PluginHandle(3)));
        assert_eq!(event.state, PluginState::FAULTED);
        assert_eq!(event.name, "coop");
        assert_eq!(PluginStateEvent::parse(&[1, 2, 3]), None);
    }
}
