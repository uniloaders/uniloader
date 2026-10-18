//! DLLs that are not uniloader plugins: `.asi` files from the plugin directory and the
//! libraries `loader.toml` lists under `load`.
//!
//! `.asi` files are what the Ultimate ASI Loader loads: plain DLLs that start themselves from
//! `DllMain`, some of which also export `InitializeASI` and expect it to be called once. The
//! `load` list is for chain-loading something the loader displaced, such as UE4SS when the
//! loader took the place of UE4SS's `dwmapi.dll` proxy. Neither kind gets the host API; both
//! are listed next to the plugins with their version, so `plugins` shows everything the loader
//! put into the game.

// FFI module: loads DLLs and calls one optional export.
#![allow(unsafe_code)]

use std::path::{Path, PathBuf};

use windows::core::{s, PCWSTR};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};

use crate::config::Paths;
use crate::state::Foreign;

/// `InitializeASI`, as the Ultimate ASI Loader calls it.
type InitializeFn = unsafe extern "C" fn();

fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// `*.asi` in the plugin directory, alphabetically.
fn asi_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("asi")))
        .collect();
    files.sort();
    files
}

/// Load one DLL and describe it for the listing. `initialize` calls `InitializeASI` when the
/// DLL exports it.
fn load_one(paths: &Paths, file: &Path, kind: &'static str, initialize: bool) -> Option<Foreign> {
    let shown = file
        .strip_prefix(&paths.game_dir)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/");
    let version = std::fs::read(file)
        .ok()
        .and_then(|bytes| uniloader_core::pe::product_version(&bytes))
        .unwrap_or_default();
    let path = wide(file);
    // SAFETY: `path` is NUL terminated; the altered search path lets the DLL find its own
    // dependencies next to itself, which is how UE4SS ships.
    let module =
        match unsafe { LoadLibraryExW(PCWSTR(path.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) }
        {
            Ok(m) => m,
            Err(e) => {
                log::error!("cannot load {shown}: {e}");
                return None;
            }
        };
    if initialize {
        // SAFETY: valid module handle and a NUL terminated name.
        if let Some(init) = unsafe { GetProcAddress(module, s!("InitializeASI")) } {
            // SAFETY: `InitializeASI` takes and returns nothing, by the ASI convention.
            let init = unsafe {
                core::mem::transmute::<unsafe extern "system" fn() -> isize, InitializeFn>(init)
            };
            // SAFETY: calling the export is what the convention asks for; the guard contains
            // a fault inside it.
            if let Err(fault) = unsafe { super::guard::call(|| init()) } {
                log::error!("{shown}: InitializeASI {fault}");
            }
        }
    }
    let name = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    if version.is_empty() {
        log::info!("loaded {kind} {shown}, no version resource");
    } else {
        log::info!("loaded {kind} {shown} {version}");
    }
    Some(Foreign {
        name,
        file: shown,
        version,
        kind,
    })
}

/// Load the `.asi` files (when enabled) and the `load` list, and record them in the state.
pub(crate) fn load_all(paths: &Paths, asi: bool, listed: &[String]) {
    let mut loaded = Vec::new();
    if asi {
        for file in asi_files(&paths.plugins_dir) {
            loaded.extend(load_one(paths, &file, "asi", true));
        }
    }
    for entry in listed {
        let file = paths.game_dir.join(entry);
        if file.is_file() {
            loaded.extend(load_one(paths, &file, "library", false));
        } else {
            log::warn!("loader.toml lists {entry}, which does not exist");
        }
    }
    super::state().foreign.extend(loaded);
}
