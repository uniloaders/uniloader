//! Discovery, loading and starting of plugin DLLs.
//!
//! Loading happens in two passes: every DLL is loaded and asked to describe itself, then the
//! declared dependencies decide who starts and in what order. A plugin therefore learns about
//! a missing dependency as a log line before it runs any code, not as a crash later.
//!
//! The started set is append only and published through an atomic pointer, so dispatch on the
//! game's threads needs no lock and a plugin that re-enters the host from a callback cannot
//! deadlock. Disabling a faulted plugin flips an atomic; nothing is ever removed, because the
//! handles plugins hold are indices into this list.

// FFI module: loads DLLs and calls their exports.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Mutex;

use uniloader_api::{
    topics, DescribeFn, PluginCallbacks, PluginHandle, PluginInfo, PluginState, StartFn, Status,
    StopFn, StopReason, TargetPolicy, API_VERSION, DESCRIBE_EXPORT, START_EXPORT, STOP_EXPORT,
};
use uniloader_core::deps::{self, Candidate, PluginReq};
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

use crate::config::Paths;
use crate::state::Phase;

use super::deps as external;

pub(crate) use super::dispatch::{deliver, deliver_event, deliver_message};

/// A loaded, started plugin.
pub(crate) struct Active {
    /// Handle the state registry assigned.
    pub(crate) handle: PluginHandle,
    /// Plugin name, for log messages.
    pub(crate) name: String,
    /// Callback table the plugin filled in.
    pub(crate) callbacks: PluginCallbacks,
    /// Stop export, called at shutdown.
    pub(crate) stop: Option<StopFn>,
    /// Cleared when the plugin faults; no further callbacks are delivered.
    pub(crate) enabled: AtomicBool,
    /// Set once the plugin stopped or faulted: its stop export runs at most once, and never
    /// after a fault. Read without the state lock, so process exit can use it.
    pub(crate) ended: AtomicBool,
}

// SAFETY: the pointers in `callbacks` are owned by the plugin DLL, which stays loaded for
// the process's lifetime, and the ABI requires every callback to be thread safe.
unsafe impl Sync for Active {}
// SAFETY: see the `Sync` impl.
unsafe impl Send for Active {}

/// The published list. Null until the first plugin starts.
static ACTIVE: AtomicPtr<Vec<&'static Active>> = AtomicPtr::new(core::ptr::null_mut());
/// Serialises publishing. Readers never take it.
static PUBLISH: Mutex<()> = Mutex::new(());

/// Started plugins. Empty until the first one starts.
pub(crate) fn active() -> &'static [&'static Active] {
    let ptr = ACTIVE.load(Ordering::Acquire);
    if ptr.is_null() {
        return &[];
    }
    // SAFETY: a non-null pointer here came from `Box::into_raw` on a leaked `Vec` that is
    // never freed or mutated afterwards, so the slice is valid for the process's lifetime.
    unsafe { &*ptr }.as_slice()
}

/// Publish `plugins` in addition to whatever is already running.
///
/// The old vector is leaked rather than freed: a reader on the render thread may be walking
/// it at this exact moment, and one leaked vector of references per load is nothing against
/// the cost of making dispatch take a lock.
fn publish(plugins: Vec<Active>) {
    let started: Vec<(PluginHandle, String)> =
        plugins.iter().map(|p| (p.handle, p.name.clone())).collect();
    {
        let _guard = PUBLISH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut combined: Vec<&'static Active> = active().to_vec();
        combined.extend(plugins.into_iter().map(|p| &*Box::leak(Box::new(p))));
        let boxed = Box::into_raw(Box::new(combined));
        ACTIVE.store(boxed, Ordering::Release);
    }
    // After publishing, so a subscriber reacting to the event can already find the plugin.
    for (handle, name) in started {
        announce(handle, &name, PluginState::RUNNING);
    }
}

/// Find a started plugin by handle.
pub(crate) fn find(handle: PluginHandle) -> Option<&'static Active> {
    active().iter().copied().find(|p| p.handle == handle)
}

/// Find a started plugin by name.
pub(crate) fn find_by_name(name: &str) -> Option<&'static Active> {
    active().iter().copied().find(|p| p.name == name)
}

/// Run `body` on behalf of `target` under the fault guard, disabling `target` if it faults.
pub(crate) fn guarded(target: PluginHandle, body: impl FnOnce()) -> Status {
    let Some(plugin) = find(target).filter(|p| p.is_enabled()) else {
        return Status::NOT_FOUND;
    };
    // SAFETY: the caller passes a closure performing one call into `target`; the guard
    // contains a fault inside it.
    match unsafe { super::guard::call(body) } {
        Ok(()) => Status::OK,
        Err(fault) => {
            plugin.disable(&format!("service callback {fault}"));
            Status::ERROR
        }
    }
}

/// Publish `loader.plugin_state` for one plugin. Never called with the state lock held.
fn announce(handle: PluginHandle, name: &str, plugin_state: PluginState) {
    let mut payload = Vec::with_capacity(8 + name.len());
    payload.extend_from_slice(&handle.0.to_le_bytes());
    payload.extend_from_slice(&plugin_state.0.to_le_bytes());
    payload.extend_from_slice(name.as_bytes());
    // Including the plugin itself: seeing its own `RUNNING` is how a plugin learns that
    // start is over and the others are up.
    let targets = super::state().subscribers(PluginHandle::NONE, topics::PLUGIN_STATE);
    deliver_event(PluginHandle::NONE, &targets, topics::PLUGIN_STATE, &payload);
}

impl Active {
    /// Whether callbacks may still be delivered to this plugin.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Resume callback delivery and tell the plugin. Returns whether this changed anything.
    pub(crate) fn enable(&self) -> bool {
        // A stopped or faulted plugin has torn down: calling it again is not safe.
        if super::state().has_ended(self.handle) {
            return false;
        }
        let changed = !self.enabled.swap(true, Ordering::Relaxed);
        if changed {
            super::state().set_enabled(self.handle, true);
            // After the atomic, so the plugin may already act on frames from here on.
            super::dispatch::enabled(self, true);
            announce(self.handle, &self.name, PluginState::RUNNING);
        }
        changed
    }

    /// Stop delivering callbacks after a fault, and record it in the registry.
    pub(crate) fn disable(&self, reason: &str) {
        if self.enabled.swap(false, Ordering::Relaxed) {
            self.ended.store(true, Ordering::Release);
            log::error!(
                "[{}] disabled for the rest of this session: {reason}",
                self.name
            );
            super::state().set_ended(self.handle, PluginState::FAULTED);
            announce(self.handle, &self.name, PluginState::FAULTED);
        }
    }

    /// Stop delivering callbacks on request, without the error log a fault produces.
    ///
    /// The plugin is told first and the atomic cleared afterwards, so `on_disable` is the last
    /// callback it sees rather than racing a frame that is already in flight.
    pub(crate) fn suspend(&self) -> bool {
        if !self.is_enabled() {
            return false;
        }
        super::dispatch::enabled(self, false);
        let changed = self.enabled.swap(false, Ordering::Relaxed);
        if changed {
            super::state().set_enabled(self.handle, false);
            announce(self.handle, &self.name, PluginState::DISABLED);
        }
        changed
    }

    /// Call the stop export, if the plugin has one, and tear down its hooks either way.
    ///
    /// The plugin runs first and the loader cleans up after it: a plugin that removes its own
    /// hook in `stop` is the normal case, and the registry treats an already-removed hook as
    /// nothing to do.
    ///
    /// Runs at most once per plugin, and not after a fault; callbacks stop being delivered
    /// before the export is called.
    pub(crate) fn call_stop(&self, reason: StopReason) {
        if self.ended.swap(true, Ordering::AcqRel) {
            return;
        }
        self.enabled.store(false, Ordering::Release);
        if let Some(stop) = self.stop {
            let ctx = self.callbacks.ctx;
            // SAFETY: `ctx` is the pointer the plugin gave us in `start`.
            if let Err(e) = unsafe { super::guard::call(|| stop(ctx, reason)) } {
                log::error!("[{}] {STOP_EXPORT} faulted: {e}", self.name);
            }
        }
        super::plugin_hooks::remove_all(self.handle, &self.name);
        super::state().set_ended(self.handle, PluginState::STOPPED);
        announce(self.handle, &self.name, PluginState::STOPPED);
    }
}

/// Whether a file even claims to be a plugin, decided without loading it.
///
/// `plugins/` is a directory in the game folder, so it collects things: a DLL a plugin ships
/// as its own dependency, or the predecessor project's proxy. `LoadLibrary` on one of those
/// runs its `DllMain`, which for a proxy DLL means installing hooks nobody asked for. An
/// export name appears literally in the file's export table, so looking for the describe
/// export's name in the bytes answers the question without mapping anything.
pub(super) fn claims_to_be_a_plugin(file: &Path) -> bool {
    match std::fs::read(file) {
        Ok(bytes) => bytes
            .windows(DESCRIBE_EXPORT.len())
            .any(|w| w == DESCRIBE_EXPORT.as_bytes()),
        Err(e) => {
            log::warn!("{}: {e}", file.display());
            false
        }
    }
}

/// DLL file names in discovery order: alphabetical, so the order is reproducible.
fn plugin_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::info!("no plugin directory at {}", dir.display());
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll")))
        .filter(|p| {
            let claims = claims_to_be_a_plugin(p);
            if !claims {
                log::info!(
                    "ignoring {}: it exports no {DESCRIBE_EXPORT}, so it is not a plugin",
                    p.display()
                );
            }
            claims
        })
        .collect();
    files.sort();
    files
}

struct Exports {
    describe: DescribeFn,
    start: StartFn,
    stop: Option<StopFn>,
}

/// Resolve the three plugin exports.
fn exports(module: HMODULE) -> Option<Exports> {
    let symbol = |name: &str| {
        let c = CString::new(name).ok()?;
        // SAFETY: valid module handle and a NUL terminated name.
        unsafe { GetProcAddress(module, PCSTR(c.as_ptr().cast())) }
    };
    let describe = symbol(DESCRIBE_EXPORT)?;
    let start = symbol(START_EXPORT)?;
    let stop = symbol(STOP_EXPORT);
    // SAFETY: the exports are documented to have these signatures; a DLL that exports them
    // with a different one is a broken plugin we cannot detect from here.
    unsafe {
        Some(Exports {
            describe: core::mem::transmute::<unsafe extern "system" fn() -> isize, DescribeFn>(
                describe,
            ),
            start: core::mem::transmute::<unsafe extern "system" fn() -> isize, StartFn>(start),
            stop: stop
                .map(|s| core::mem::transmute::<unsafe extern "system" fn() -> isize, StopFn>(s)),
        })
    }
}

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Described plugins waiting for a dependency, in the order they were discovered.
static PENDING: Mutex<Vec<Described>> = Mutex::new(Vec::new());

/// Lock the pending list, recovering a poisoned lock: it is plain data.
fn pending() -> std::sync::MutexGuard<'static, Vec<Described>> {
    PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A DLL that loaded and described itself, before any dependency decision.
pub(super) struct Described {
    /// File it came from.
    pub(super) file: PathBuf,
    /// What the describe export said.
    pub(super) info: crate::described::Described,
    /// What it declared it needs.
    pub(super) declared: Vec<external::Declared>,
    exports: Exports,
}

// SAFETY: the function pointers inside `exports` belong to a DLL that is never unloaded, and
// the ABI requires the exports to be callable from any thread, so moving this between threads
// cannot invalidate anything.
unsafe impl Send for Described {}

/// First pass: load a DLL and read its description. Nothing is started yet.
pub(super) fn describe_one(file: &Path) -> Result<Described, String> {
    let path = wide(file);
    // SAFETY: `path` is NUL terminated; the altered search path lets a plugin ship its own
    // dependencies next to itself.
    let module =
        unsafe { LoadLibraryExW(PCWSTR(path.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
            .map_err(|e| format!("cannot load: {e}"))?;
    let exports = exports(module).ok_or_else(|| {
        format!("not a plugin: missing {DESCRIBE_EXPORT}, {START_EXPORT} or {STOP_EXPORT}")
    })?;

    // SAFETY: the export was resolved above; calling it is what the ABI is for.
    let info = unsafe { super::guard::call(|| (exports.describe)()) }
        .map_err(|e| format!("{DESCRIBE_EXPORT} faulted: {e}"))?;
    if info.is_null() {
        return Err(format!("{DESCRIBE_EXPORT} returned null"));
    }
    // SAFETY: the ABI requires the returned struct to stay valid while the DLL is loaded.
    let info = unsafe { &*info };
    if info.api_version != API_VERSION {
        return Err(format!(
            "built for API version {}, loader speaks {API_VERSION}",
            info.api_version
        ));
    }
    if info.struct_size < core::mem::size_of::<PluginInfo>() {
        return Err(format!(
            "its describe struct is {} bytes, API version {API_VERSION} has {}",
            info.struct_size,
            core::mem::size_of::<PluginInfo>()
        ));
    }
    let text = super::hostapi::text;
    let name = text(info.name);
    let title = text(info.title);
    external::register_provider(&name, &external::provided(info));
    Ok(Described {
        file: file.to_path_buf(),
        info: crate::described::Described {
            title: if title.is_empty() {
                name.clone()
            } else {
                title
            },
            name,
            version: text(info.version),
            description: text(info.description),
            authors: authors(info),
            links: crate::described::Links {
                homepage: text(info.homepage),
                issues: text(info.issues),
                download: text(info.download),
            },
            update_github: text(info.update.github),
            update_url: text(info.update.url),
            targets: targets(info),
            refuse_mismatch: info.target_policy == TargetPolicy::REFUSE,
        },
        declared: external::declared(info),
        exports,
    })
}

/// A list from the describe struct: `count` entries at `ptr`, or none for a null pointer.
///
/// # Safety
/// A non-null `ptr` must point at `count` valid entries, as the ABI requires of the describe
/// struct's lists while the DLL is loaded.
unsafe fn list<'a, T>(ptr: *const T, count: usize) -> &'a [T] {
    if ptr.is_null() || count == 0 {
        return &[];
    }
    // SAFETY: the caller's contract.
    unsafe { core::slice::from_raw_parts(ptr, count) }
}

fn authors(info: &PluginInfo) -> Vec<(String, String)> {
    let text = super::hostapi::text;
    // SAFETY: the describe struct's lists are valid while the DLL is loaded.
    unsafe { list(info.authors, info.author_count) }
        .iter()
        .map(|a| (text(a.name), text(a.url)))
        .collect()
}

fn targets(info: &PluginInfo) -> Vec<uniloader_core::target::Filter> {
    let text = super::hostapi::text;
    // SAFETY: as in `authors`.
    unsafe { list(info.targets, info.target_count) }
        .iter()
        .map(|t| uniloader_core::target::Filter {
            property: text(t.property),
            // SAFETY: as in `authors`.
            values: unsafe { list(t.values, t.value_count) }
                .iter()
                .map(|v| text(*v))
                .collect(),
        })
        .collect()
}

impl Described {
    /// Unique name from the describe export.
    pub(super) fn name(&self) -> &str {
        &self.info.name
    }
}

/// Which of the plugin's target filters the game fails. Hashes the executable first when a
/// filter asks for `file.sha256` and nobody did before (outside the state lock: it reads the
/// whole file).
fn target_mismatch(info: &crate::described::Described) -> Vec<String> {
    let wants_hash = info
        .targets
        .iter()
        .any(|f| f.property.eq_ignore_ascii_case("file.sha256"));
    if wants_hash && !super::state().facts.contains_key("file.sha256") {
        let exe = super::proxy::module_path(None);
        if let Some(digest) = uniloader_core::facts::sha256(&exe) {
            super::state().facts.insert(
                "file.sha256".to_owned(),
                uniloader_core::target::Fact::Text(digest),
            );
        }
    }
    super::state().target_mismatch(info)
}

/// Publish `loader.plugin_mismatch` for every running plugin meant for another game. Never
/// called with the state lock held.
fn announce_mismatches() {
    let (targets, mismatched) = {
        let guard = super::state();
        let mismatched: Vec<Vec<u8>> = guard
            .plugins
            .iter()
            .filter(|p| p.enabled && !p.mismatch.is_empty())
            .map(|p| {
                [p.name(), &p.info.version, &p.mismatch.join("\n")]
                    .join("\0")
                    .into_bytes()
            })
            .collect();
        (
            guard.subscribers(PluginHandle::NONE, topics::PLUGIN_MISMATCH),
            mismatched,
        )
    };
    for payload in mismatched {
        deliver_event(
            PluginHandle::NONE,
            &targets,
            topics::PLUGIN_MISMATCH,
            &payload,
        );
    }
}

/// Second pass: register and start one described plugin.
pub(super) fn start_one(described: &Described) -> Result<Active, String> {
    let Described {
        file,
        info,
        declared,
        exports,
    } = described;
    let (name, version) = (&info.name, &info.version);
    let file_name = file
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    let mismatch = target_mismatch(info);
    if !mismatch.is_empty() {
        if info.refuse_mismatch {
            return Err(format!("meant for another game: {}", mismatch.join("; ")));
        }
        log::warn!(
            "[{name}] meant for another game, starting it anyway: {}",
            mismatch.join("; ")
        );
    }

    let handle = super::state()
        .add_plugin(info.clone(), &file_name)
        .map_err(|e| format!("rejected name {name:?}: {e:?}"))?;
    super::state().set_mismatch(handle, mismatch);
    log::info!("starting {name} {version} from {file_name}");
    for dependency in declared {
        log::debug!("[{name}] needs {}", dependency.describe());
    }

    let mut callbacks = PluginCallbacks::empty();
    let host = super::caller::table_for(handle, super::host_api());
    // Read before taking the lock: every setting the plugin registers looks its value up.
    let paths = super::state().paths.clone();
    let stored = crate::config::load_plugin_values(&paths, name);
    let previous = {
        let mut guard = super::state();
        guard.set_dependencies(handle, declared.iter().map(external::Declared::describe));
        let values = guard.start_values.replace((handle, stored));
        let previous = guard.phase;
        guard.phase = Phase::Starting(handle);
        (previous, values)
    };
    // SAFETY: `host` lives for the process's lifetime and `callbacks` is writable.
    let status =
        unsafe { super::guard::call(|| (exports.start)(host, handle, &raw mut callbacks)) };
    {
        let mut guard = super::state();
        (guard.phase, guard.start_values) = previous;
    }

    let failure = match status {
        Ok(Status::OK) => None,
        Ok(other) => Some(format!("{START_EXPORT} returned {other:?}")),
        Err(e) => Some(format!("{START_EXPORT} faulted: {e}")),
    };
    if let Some(failure) = failure {
        // A start that got far enough to publish a context may have registered a hook or
        // allocated; give it the chance to undo that, then undo what the loader holds.
        if !callbacks.ctx.is_null() {
            if let Some(stop) = exports.stop {
                let ctx = callbacks.ctx;
                // SAFETY: `ctx` is the pointer the plugin itself wrote into the table.
                let _ = unsafe { super::guard::call(|| stop(ctx, StopReason::START_FAILED)) };
            }
        }
        super::plugin_hooks::remove_all(handle, name);
        // Recorded as ended, so a dependent sees it as unmet rather than as a paused plugin,
        // and a service it promised resolves to nobody.
        super::state().set_ended(handle, PluginState::FAULTED);
        external::forget_provider(name);
        return Err(failure);
    }
    if callbacks.struct_size < core::mem::size_of::<PluginCallbacks>() {
        log::warn!("[{name}] filled a callback table smaller than this API version's");
    }
    super::state().set_enabled(handle, true);
    Ok(Active {
        handle,
        name: name.clone(),
        callbacks,
        stop: exports.stop,
        enabled: AtomicBool::new(true),
        ended: AtomicBool::new(false),
    })
}

/// Load every plugin in `paths.plugins_dir`, honouring the disable list.
pub(crate) fn load_all(paths: &Paths, disabled: &[String]) {
    super::lifecycle::serialised(|| load_all_now(paths, disabled));
}

/// [`load_all`] inside the lifecycle lock.
fn load_all_now(paths: &Paths, disabled: &[String]) {
    // What did not start and why, for `loader.plugin_not_running` at the end.
    let mut not_running: Vec<(String, String)> = Vec::new();
    let mut described = Vec::new();
    for file in plugin_files(&paths.plugins_dir) {
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if disabled.iter().any(|d| d.eq_ignore_ascii_case(&stem)) {
            log::info!("skipping {stem}: disabled in loader.toml");
            continue;
        }
        match describe_one(&file) {
            Ok(plugin) => described.push(plugin),
            Err(e) => {
                log::error!("{}: {e}", file.display());
                not_running.push((stem, e));
            }
        }
    }
    described.retain(
        |plugin| match external::unmet_external(&paths.game_dir, &plugin.declared) {
            Some(reason) => {
                log::error!("[{}] not started: {reason}", plugin.name());
                external::forget_provider(plugin.name());
                not_running.push((plugin.name().to_owned(), reason));
                false
            }
            None => true,
        },
    );

    let candidates: Vec<Candidate> = described
        .iter()
        .map(|plugin| Candidate {
            name: plugin.info.name.clone(),
            version: plugin.info.version.clone(),
            requires: external::plugin_requirements(plugin.name(), &plugin.declared),
        })
        .collect();
    let plan = deps::plan(&candidates);
    // Taken out one at a time, because a rejected plugin is kept rather than dropped.
    let mut described: Vec<Option<Described>> = described.into_iter().map(Some).collect();

    let mut started = Vec::new();
    for index in plan.order {
        let Some(plugin) = described[index].take() else {
            continue;
        };
        match start_one(&plugin) {
            Ok(active) => started.push(active),
            Err(e) => {
                log::error!("{}: {e}", plugin.file.display());
                not_running.push((plugin.name().to_owned(), e));
            }
        }
    }
    publish(started);

    for (index, reason) in plan.rejected {
        let Some(plugin) = described[index].take() else {
            continue;
        };
        if can_still_be_satisfied(&reason) {
            log::warn!("[{}] waiting: {reason}", plugin.name());
            super::state().set_pending(plugin.name(), &reason.to_string());
            pending().push(plugin);
        } else {
            log::error!("[{}] not started: {reason}", plugin.name());
            external::forget_provider(plugin.name());
            not_running.push((plugin.name().to_owned(), reason.to_string()));
        }
    }
    start_pending();
    // Those still waiting now wait for good (until a `plugin load`).
    let waiting = super::state().pending.clone();
    not_running.extend(waiting);
    announce_not_running(&not_running);
    announce_mismatches();
}

/// Publish `loader.plugin_not_running` for each of `plugins` (name, why). Never called with
/// the state lock held.
fn announce_not_running(plugins: &[(String, String)]) {
    if plugins.is_empty() {
        return;
    }
    let targets = super::state().subscribers(PluginHandle::NONE, topics::PLUGIN_NOT_RUNNING);
    for (name, reason) in plugins {
        deliver_event(
            PluginHandle::NONE,
            &targets,
            topics::PLUGIN_NOT_RUNNING,
            &not_running_payload(name, reason),
        );
    }
}

/// Name, NUL, reason: the payload [`topics::PLUGIN_NOT_RUNNING`] documents.
fn not_running_payload(name: &str, reason: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(name.len() + 1 + reason.len());
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Whether another plugin starting could still make this rejection go away.
///
/// A missing plugin can arrive later, through `plugin load`; a version mismatch, a cycle and a
/// duplicate name cannot change within one launch, so those are failures and not waits.
fn can_still_be_satisfied(reason: &deps::Rejection) -> bool {
    match reason {
        deps::Rejection::Missing(_) | deps::Rejection::DependencyRejected(_) => true,
        deps::Rejection::Version { .. }
        | deps::Rejection::Cycle(_)
        | deps::Rejection::DuplicateName(_) => false,
    }
}

/// Names of the plugins still waiting for a dependency.
pub(super) fn pending_names() -> Vec<String> {
    pending().iter().map(|p| p.info.name.clone()).collect()
}

/// Take one waiting plugin out of the list, by name.
pub(super) fn take_pending(name: &str) -> Option<Described> {
    let mut list = pending();
    let index = list.iter().position(|p| p.name() == name)?;
    Some(list.remove(index))
}

/// Start every waiting plugin whose requirements are now met, repeatedly, because starting
/// one can be what another was waiting for. Returns the names that started.
pub(super) fn start_pending() -> Vec<String> {
    let mut names = Vec::new();
    loop {
        // The pending lock is never held while the state lock is taken, so the two orders
        // can never meet.
        let requirements: Vec<(String, Vec<PluginReq>)> = pending()
            .iter()
            .map(|p| {
                (
                    p.info.name.clone(),
                    external::plugin_requirements(p.name(), &p.declared),
                )
            })
            .collect();
        let ready = requirements
            .into_iter()
            .find(|(_, reqs)| super::lifecycle::unmet_plugin_requirement(reqs).is_none())
            .map(|(name, _)| name);
        let Some(name) = ready else { return names };
        let Some(plugin) = take_pending(&name) else {
            return names;
        };
        super::state().clear_pending(&name);
        match start_one(&plugin) {
            Ok(active) => {
                log::info!("[{name}] started: its dependencies are available now");
                append(active);
                names.push(name);
            }
            // `start_one` already recorded it as faulted and forgot it as a provider.
            Err(e) => log::error!("[{name}] {e}"),
        }
    }
}

/// Append one plugin that was started after the initial load.
pub(super) fn append(plugin: Active) {
    publish(vec![plugin]);
}

/// Call the stop export of every plugin that has not ended (paused ones too), once. For
/// `quit`, which runs on a thread of its own with the process intact.
pub(crate) fn stop_all() {
    for &plugin in active() {
        plugin.call_stop(StopReason::EXIT);
    }
}
