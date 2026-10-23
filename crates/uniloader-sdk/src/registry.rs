//! What the loader knows (modules, settings, hotkeys), and services between plugins.

// The plugin side of the FFI boundary for the registry functions; every unsafe block
// documents the loader contract it relies on.
#![allow(unsafe_code)]

use core::ffi::c_void;
use core::marker::PhantomData;

use uniloader_api::{
    ConsoleKind, ConsoleLine, HotkeyInfo, KeyModifiers, LogLevel, ModuleKind, PluginEntry,
    PluginHandle, PluginState, ServiceInfo, SettingInfo, Status, Str,
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
    /// Description, or why a waiting plugin waits.
    pub description: String,
    /// File relative to the game directory.
    pub file: String,
    /// Authors.
    pub authors: Vec<AuthorEntry>,
    /// Home page, or empty.
    pub homepage: String,
    /// Where to report problems, or empty.
    pub issues: String,
    /// Where to download it: the update check's link when it found one, else the declared
    /// one, or empty.
    pub download: String,
    /// The latest version the update check found, or empty.
    pub latest: String,
    /// Whether `latest` is newer than `version`.
    pub update_available: bool,
    /// Why the running game does not match the plugin's target filters, one line per filter;
    /// empty when it matches.
    pub mismatch: String,
}

/// One author of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorEntry {
    /// Name as shown.
    pub name: String,
    /// A profile or home page, or empty.
    pub url: String,
}

/// One line of console scrollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleEntry {
    /// Position in everything ever printed; strictly increasing.
    pub seq: u64,
    /// Output, echo or log record.
    pub kind: ConsoleKind,
    /// Level of a log record; [`LogLevel::INFO`] otherwise.
    pub level: LogLevel,
    /// Local `HH:MM:SS` when it was printed.
    pub clock: String,
    /// Module that logged it, empty for command output.
    pub target: String,
    /// The text, without any prefix.
    pub text: String,
}

/// Sink for [`Host::console_read`].
///
/// # Safety
/// `ctx` must be the `Vec<ConsoleEntry>` pointer `console_read` passed.
unsafe extern "C" fn collect_console(ctx: *mut c_void, line: *const ConsoleLine) {
    if ctx.is_null() || line.is_null() {
        return;
    }
    // SAFETY: the loader passes a valid line for the duration of the call.
    let line = unsafe { &*line };
    // SAFETY: the only caller is the loader, with the pointer we handed it.
    let lines = unsafe { &mut *ctx.cast::<Vec<ConsoleEntry>>() };
    lines.push(ConsoleEntry {
        seq: line.seq,
        kind: line.kind,
        level: line.level,
        clock: str_from(line.clock),
        target: str_from(line.target),
        text: str_from(line.text),
    });
}

/// One setting of any plugin, with its value.
// Four independent yes/no properties mirroring the ABI's `SettingFlags` bits; each is read
// by name all over the loader and the editors, which a bit set would only make noisier.
#[allow(clippy::struct_excessive_bools)]
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
    /// Choices of an enum or flags setting.
    pub choices: Vec<String>,
    /// What to show for each of `choices` (empty: the choice itself); see [`SettingEntry::label`].
    pub labels: Vec<String>,
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
    /// A password or token: `value` is `<set>` or `<unset>`; editors show a password field.
    pub secret: bool,
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

/// A `loader.plugin_not_running` event, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotRunningEvent {
    /// The plugin that did not start.
    pub name: String,
    /// Why, as the loader logged it.
    pub reason: String,
}

impl NotRunningEvent {
    /// Decode the payload of [`uniloader_api::topics::PLUGIN_NOT_RUNNING`].
    #[must_use]
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let split = payload.iter().position(|&b| b == 0)?;
        Some(NotRunningEvent {
            name: String::from_utf8_lossy(&payload[..split]).into_owned(),
            reason: String::from_utf8_lossy(&payload[split + 1..]).into_owned(),
        })
    }

    /// The payload the loader sends for `name` and `reason`.
    #[must_use]
    pub fn payload(name: &str, reason: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(name.len() + 1 + reason.len());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(reason.as_bytes());
        out
    }
}

/// The NUL separated UTF-8 fields of a loader event payload.
fn fields(payload: &[u8]) -> Vec<String> {
    payload
        .split(|&b| b == 0)
        .map(|f| String::from_utf8_lossy(f).into_owned())
        .collect()
}

/// A `loader.plugin_mismatch` event, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MismatchEvent {
    /// The plugin meant for another game.
    pub name: String,
    /// Its version.
    pub version: String,
    /// Why, one line per filter that failed.
    pub reasons: Vec<String>,
}

impl MismatchEvent {
    /// Decode the payload of [`uniloader_api::topics::PLUGIN_MISMATCH`].
    #[must_use]
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let mut f = fields(payload).into_iter();
        Some(MismatchEvent {
            name: f.next().filter(|n| !n.is_empty())?,
            version: f.next()?,
            reasons: f.next()?.lines().map(str::to_owned).collect(),
        })
    }
}

/// A `loader.update_available` event, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateEvent {
    /// The plugin with a newer version.
    pub name: String,
    /// The version running.
    pub version: String,
    /// The latest version.
    pub latest: String,
    /// Where to download it, or empty.
    pub download: String,
}

impl UpdateEvent {
    /// Decode the payload of [`uniloader_api::topics::UPDATE_AVAILABLE`].
    #[must_use]
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let mut f = fields(payload).into_iter();
        Some(UpdateEvent {
            name: f.next().filter(|n| !n.is_empty())?,
            version: f.next()?,
            latest: f.next()?,
            download: f.next().unwrap_or_default(),
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
            description: str_from(e.description),
            file: str_from(e.file),
            authors: if e.authors.is_null() {
                Vec::new()
            } else {
                // SAFETY: the loader passes `author_count` valid entries for the duration of
                // the call, as documented on `PluginEntry`.
                unsafe { core::slice::from_raw_parts(e.authors, e.author_count) }
                    .iter()
                    .map(|a| AuthorEntry {
                        name: str_from(a.name),
                        url: str_from(a.url),
                    })
                    .collect()
            },
            homepage: str_from(e.homepage),
            issues: str_from(e.issues),
            download: str_from(e.download),
            latest: str_from(e.latest),
            update_available: e.update_available != 0,
            mismatch: str_from(e.mismatch),
        }
    }
}

impl SettingEntry {
    /// What to show for `choice`: its label, or the choice itself.
    #[must_use]
    pub fn label<'a>(&'a self, choice: &'a str) -> &'a str {
        self.choices
            .iter()
            .position(|c| c == choice)
            .and_then(|i| self.labels.get(i))
            .filter(|l| !l.is_empty())
            .map_or(choice, String::as_str)
    }
}

impl From<&SettingInfo> for SettingEntry {
    fn from(s: &SettingInfo) -> Self {
        let (choices, labels) = uniloader_core::settings::parse_choices(&str_from(s.choices));
        let kind = SettingKind::from_abi(s.kind);
        SettingEntry {
            plugin: str_from(s.plugin),
            key: str_from(s.key),
            title: str_from(s.title),
            description: str_from(s.description),
            kind,
            min: s.min,
            max: s.max,
            choices,
            labels,
            default: str_from(s.default),
            value: str_from(s.value),
            restart_required: s
                .flags
                .contains(uniloader_api::SettingFlags::RESTART_REQUIRED),
            transient: s.flags.contains(uniloader_api::SettingFlags::TRANSIENT),
            advanced: s.flags.contains(uniloader_api::SettingFlags::ADVANCED),
            secret: s.flags.contains(uniloader_api::SettingFlags::SECRET),
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

    /// Console lines (command output, echoed input and log records) printed after position
    /// `since`, and the position to pass next time. Start with 0.
    #[must_use]
    pub fn console_read(&self, since: u64) -> (Vec<ConsoleEntry>, u64) {
        let mut lines: Vec<ConsoleEntry> = Vec::new();
        let mut next = since;
        // SAFETY: `lines` and `next` outlive the call; the sink only receives `lines`.
        let _ = unsafe {
            (self.api.console_read)(
                self.api.host,
                since,
                Some(collect_console),
                core::ptr::from_mut(&mut lines).cast(),
                &raw mut next,
            )
        };
        (lines, next)
    }

    /// Report a key press seen in the game window's messages, so hotkeys bound by scan code
    /// (`sc29`) fire. `modifiers` is a [`KeyModifiers`] bit set.
    pub fn hotkey_press(&self, scancode: u16, modifiers: KeyModifiers) {
        // SAFETY: plain values; the loader validates nothing it could misread.
        let _ = unsafe { (self.api.hotkey_press)(self.api.host, self.handle, scancode, modifiers) };
    }

    /// Claim the keyboard, so only this plugin's own hotkeys fire, or give it back.
    pub fn capture_input(&self, capture: bool) {
        // SAFETY: plain values.
        let _ = unsafe { (self.api.input_capture)(self.api.host, self.handle, capture) };
    }

    /// Every name the console completes, sorted.
    #[must_use]
    pub fn console_names(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        // SAFETY: `names` outlives the call; the sink only receives that pointer.
        let _ = unsafe {
            (self.api.console_names)(
                self.api.host,
                Some(crate::host::collect_line),
                core::ptr::from_mut(&mut names).cast(),
            )
        };
        names
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
    fn not_running_payloads_round_trip() {
        let payload = NotRunningEvent::payload("coop", "needs tsw >=0.1");
        let event = NotRunningEvent::parse(&payload).unwrap_or_else(|| panic!("parse"));
        assert_eq!(event.name, "coop");
        assert_eq!(event.reason, "needs tsw >=0.1");
        assert_eq!(NotRunningEvent::parse(b"no separator"), None);
    }

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

    #[test]
    fn loader_events_decode() {
        let m = MismatchEvent::parse(b"jump\x000.3.0\x00exe.name is A\nunreal.version is 5");
        assert_eq!(
            m.map(|m| (m.name, m.reasons.len())),
            Some(("jump".to_owned(), 2))
        );
        let u = UpdateEvent::parse(b"jump\x000.3.0\x00v0.4.0\x00https://x");
        assert_eq!(u.map(|u| u.latest), Some("v0.4.0".to_owned()));
        assert_eq!(UpdateEvent::parse(b""), None);
        assert_eq!(MismatchEvent::parse(b"jump"), None);
    }
}
