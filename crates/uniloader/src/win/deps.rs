//! Checking the dependencies a plugin declares in its describe export.
//!
//! Plugin-to-plugin requirements are planned by [`uniloader_core::deps`]; this module
//! handles the kinds that need the operating system, and converts the rest into the planner's
//! vocabulary.

// FFI module: reads the plugin's dependency array and asks Windows to find libraries.
#![allow(unsafe_code)]

use std::path::Path;

use uniloader_api::{Dependency, DependencyKind, PluginInfo};
use uniloader_core::deps::{PluginReq, VersionReq};
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
///
/// A plugin built against an older ABI has no such fields, which `struct_size` detects.
pub(crate) fn declared(info: &PluginInfo) -> Vec<Declared> {
    let has_field = info.struct_size >= core::mem::size_of::<PluginInfo>();
    if !has_field || info.dependencies.is_null() || info.dependency_count == 0 {
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

/// The plugin-to-plugin requirements, for [`uniloader_core::deps::plan`].
///
/// A requirement whose version text does not parse is reported and treated as "any version":
/// refusing to start the plugin over a typo in a requirement would be worse than ignoring it.
pub(crate) fn plugin_requirements(plugin: &str, declared: &[Declared]) -> Vec<PluginReq> {
    declared
        .iter()
        .filter(|d| d.kind == DependencyKind::PLUGIN)
        .map(|d| PluginReq {
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
        })
        .collect()
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
            DependencyKind::PLUGIN => None,
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
