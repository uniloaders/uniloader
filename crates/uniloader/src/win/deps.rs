//! Checking the dependencies a plugin declares in its describe export.
//!
//! Plugin-to-plugin requirements are planned by [`uniloader_core::deps`]; this module
//! handles the kinds that need the operating system, and converts the rest into the planner's
//! vocabulary.

// FFI module: reads the plugin's dependency array and asks Windows to find libraries.
#![allow(unsafe_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use uniloader_api::{Dependency, DependencyKind, PluginInfo, Str};
use uniloader_core::deps::{self, PluginReq, VersionReq};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::SearchPathW;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;

/// One declared dependency in loader terms.
pub(crate) struct Declared {
    /// What `name` refers to.
    pub(crate) kind: DependencyKind,
    /// Plugin name, library file name or path.
    pub(crate) name: String,
    /// Version requirement text; only meaningful for [`DependencyKind::PLUGIN`].
    pub(crate) version: String,
    /// Whether the plugin can run without it.
    pub(crate) optional: bool,
}

impl Declared {
    /// One line for the console and the log.
    pub(crate) fn describe(&self) -> String {
        let kind = match self.kind {
            DependencyKind::PLUGIN => "plugin",
            DependencyKind::LIBRARY => "library",
            DependencyKind::FILE => "file",
            DependencyKind::SERVICE => "service",
            // A plugin written against a newer ABI, or against none at all.
            _ => "unknown dependency",
        };
        let version = if self.version.is_empty() {
            String::new()
        } else {
            format!(" {}", self.version)
        };
        let optional = if self.optional { " (optional)" } else { "" };
        format!("{kind} {}{version}{optional}", self.name)
    }
}

/// Read the dependency array out of a plugin's [`PluginInfo`].
pub(crate) fn declared(info: &PluginInfo) -> Vec<Declared> {
    if info.dependencies.is_null() || info.dependency_count == 0 {
        return Vec::new();
    }
    // SAFETY: the ABI requires `dependencies` to point at `dependency_count` entries that stay
    // valid while the DLL is loaded, and the DLL is never unloaded.
    let entries: &[Dependency] =
        unsafe { core::slice::from_raw_parts(info.dependencies, info.dependency_count) };
    entries
        .iter()
        .filter(|d| d.struct_size >= core::mem::size_of::<Dependency>())
        .map(|d| Declared {
            kind: d.kind,
            name: super::hostapi::text(d.name),
            version: super::hostapi::text(d.version),
            optional: d.optional != 0,
        })
        .collect()
}

/// Read the provided service names out of a plugin's [`PluginInfo`].
pub(crate) fn provided(info: &PluginInfo) -> Vec<String> {
    if info.provides.is_null() || info.provides_count == 0 {
        return Vec::new();
    }
    // SAFETY: the ABI requires `provides` to point at `provides_count` entries that stay valid
    // while the DLL is loaded, and the DLL is never unloaded.
    let entries: &[Str] =
        unsafe { core::slice::from_raw_parts(info.provides, info.provides_count) };
    entries
        .iter()
        .map(|s| super::hostapi::text(*s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Service name to the plugins that said they provide it, filled as plugins are described.
static PROVIDERS: Mutex<BTreeMap<String, Vec<String>>> = Mutex::new(BTreeMap::new());

fn providers_map() -> MutexGuard<'static, BTreeMap<String, Vec<String>>> {
    PROVIDERS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Remember which services a described plugin provides.
pub(crate) fn register_provider(plugin: &str, services: &[String]) {
    let mut map = providers_map();
    for service in services {
        let list = map.entry(service.clone()).or_default();
        if !list.iter().any(|p| p == plugin) {
            list.push(plugin.to_owned());
        }
    }
}

/// Forget a plugin that will never start, so it stops counting as a provider.
pub(crate) fn forget_provider(plugin: &str) {
    for list in providers_map().values_mut() {
        list.retain(|p| p != plugin);
    }
}

/// Plugins known to provide `service`, in the order they were described.
fn providers(service: &str) -> Vec<String> {
    providers_map().get(service).cloned().unwrap_or_default()
}

/// The plugin-to-plugin requirements, for [`uniloader_core::deps::plan`].
///
/// A requirement whose version text does not parse is reported and treated as "any version":
/// refusing to start the plugin over a typo in a requirement would be worse than ignoring it.
/// A service dependency becomes requirements on the plugins known to provide it, see
/// [`deps::service_requirements`].
pub(crate) fn plugin_requirements(plugin: &str, declared: &[Declared]) -> Vec<PluginReq> {
    let mut reqs = Vec::new();
    for d in declared {
        match d.kind {
            DependencyKind::PLUGIN => reqs.push(PluginReq {
                name: d.name.clone(),
                version: VersionReq::parse(&d.version).unwrap_or_else(|e| {
                    log::warn!(
                        "[{plugin}] dependency on {}: {e}; accepting any version",
                        d.name
                    );
                    VersionReq::parse("")
                        .unwrap_or_else(|_| unreachable!("the empty requirement parses"))
                }),
                optional: d.optional,
            }),
            DependencyKind::SERVICE => {
                let mut found = providers(&d.name);
                found.retain(|p| p != plugin);
                reqs.extend(deps::service_requirements(&d.name, &found, d.optional));
            }
            _ => {}
        }
    }
    reqs
}

/// Whether a library of this name can be found without loading it.
///
/// Already loaded counts, and so does anywhere on the process's search path. Nothing is
/// loaded here: the plugin decides when to do that, and a `LoadLibrary` from the loader would
/// run the library's `DllMain` at a point nobody asked for.
fn library_present(file: &str) -> bool {
    let wide: Vec<u16> = file.encode_utf16().chain(core::iter::once(0)).collect();
    // SAFETY: `wide` is NUL terminated; a missing module is an error, not undefined behaviour.
    if unsafe { GetModuleHandleW(PCWSTR(wide.as_ptr())) }.is_ok() {
        return true;
    }
    let mut buffer = [0u16; 260];
    // SAFETY: the buffer is writable and its length is passed; `None` for the other arguments
    // asks for the default search path and no extension to be appended.
    let written =
        unsafe { SearchPathW(None, PCWSTR(wide.as_ptr()), None, Some(&mut buffer), None) };
    written > 0
}

/// Whether a file dependency exists. A relative path is relative to the game directory.
fn file_present(game_dir: &Path, name: &str) -> bool {
    let path = Path::new(name);
    if path.is_absolute() {
        path.exists()
    } else {
        game_dir.join(path).exists()
    }
}

/// Check everything that is not another plugin. Returns the first unmet requirement's message.
///
/// An unrecognised dependency kind counts as unmet: the plugin was built against something
/// this loader does not understand, and guessing would be worse than refusing to start it.
pub(crate) fn unmet_external(game_dir: &Path, declared: &[Declared]) -> Option<String> {
    declared
        .iter()
        .filter(|d| !d.optional)
        .find_map(|d| match d.kind {
            DependencyKind::PLUGIN | DependencyKind::SERVICE => None,
            DependencyKind::LIBRARY => {
                (!library_present(&d.name)).then(|| format!("library {} not found", d.name))
            }
            DependencyKind::FILE => (!file_present(game_dir, &d.name))
                .then(|| format!("file {} does not exist", d.name)),
            _ => Some(format!(
                "declares a dependency kind this loader does not know ({:?} on {})",
                d.kind, d.name
            )),
        })
}
