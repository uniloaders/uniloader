//! The system DLL the loader stands in for.
//!
//! `uniloader.dll` is copied into the game directory under the name of a DLL the game imports
//! (`dwmapi.dll`, `dxgi.dll`, `version.dll`, ...). It exports the union of the named exports of
//! every DLL listed in `exports/*.txt`, each a naked `jmp` through one slot of [`TABLE`]. On
//! process attach [`load_forwards`] reads the loader's own file name, loads the system DLL of
//! that name by full path and fills every slot it has; the rest jump to [`not_implemented`].
//!
//! A naked jump leaves every argument register and the stack untouched, so the forwarders need
//! no signatures. The table is filled before `DllMain` returns, so before the game can call
//! any export.

// FFI module: naked export stubs, module handles and the forward table.
#![allow(unsafe_code)]

use core::arch::naked_asm;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::{HMODULE, MAX_PATH};
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

/// `E_NOTIMPL`, returned by [`not_implemented`].
const E_NOTIMPL: i32 = 0x8000_4001_u32.cast_signed();

macro_rules! forwards {
    ($($index:literal $name:ident)*) => {
        /// Export names in table order, NUL-terminated for `GetProcAddress`.
        const NAMES: &[&str] = &[$(concat!(stringify!($name), "\0")),*];

        /// Forward targets, one per export in [`NAMES`] order. Filled in `DllMain` before the
        /// OS loader returns, so before any export can run.
        static TABLE: [AtomicUsize; NAMES.len()] = [const { AtomicUsize::new(0) }; NAMES.len()];

        $(
            #[unsafe(naked)]
            #[no_mangle]
            #[allow(non_snake_case, clippy::missing_safety_doc)]
            #[doc = concat!("Forwards `", stringify!($name), "` to the system DLL.")]
            pub unsafe extern "system" fn $name() {
                naked_asm!(
                    "jmp qword ptr [rip + {table} + {offset}]",
                    table = sym TABLE,
                    offset = const $index * 8,
                );
            }
        )*
    };
}

include!(concat!(env!("OUT_DIR"), "/forwards.rs"));

/// What [`load_forwards`] found, for the startup log.
#[derive(Debug, Clone)]
pub(crate) struct Forwarding {
    /// This DLL's own file path.
    pub(crate) own_path: PathBuf,
    /// Lower-case file stem, which names the system DLL to forward to.
    pub(crate) proxy: String,
    /// Whether that name is one of the lists in `exports/`.
    pub(crate) known: bool,
    /// Whether the file is named like a system DLL at all, rather than `uniloader.dll` or an
    /// `.asi`, which forward nothing.
    pub(crate) wanted: bool,
    /// The system DLL could be loaded.
    pub(crate) loaded: bool,
    /// Table slots that resolved.
    pub(crate) resolved: usize,
}

static FORWARDING: OnceLock<Forwarding> = OnceLock::new();

/// Target of every export the system DLL does not have.
extern "system" fn not_implemented() -> i32 {
    E_NOTIMPL
}

/// Full path of a module, or of the executable for `None`.
pub(crate) fn module_path(module: Option<HMODULE>) -> PathBuf {
    let mut buf = [0u16; MAX_PATH as usize * 2];
    // SAFETY: `buf` is writable for its length; a null module means the executable.
    let len = unsafe { GetModuleFileNameW(module, &mut buf) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

/// Directory of the running executable.
pub(crate) fn game_dir() -> PathBuf {
    module_path(None)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Full path of `file` in the system directory, NUL-terminated.
fn system_path(file: &str) -> Vec<u16> {
    let mut dir = [0u16; MAX_PATH as usize];
    // SAFETY: `dir` is writable for its length.
    let len = unsafe { GetSystemDirectoryW(Some(&mut dir)) } as usize;
    let mut path: Vec<u16> = dir[..len.min(dir.len())].to_vec();
    path.extend(format!("\\{file}\0").encode_utf16());
    path
}

/// Load the system DLL named like this one by full path (a bare name would find this DLL
/// again) and fill [`TABLE`].
///
/// # Safety
/// Only from `DllMain` on process attach, before any export can run.
pub(crate) unsafe fn load_forwards(own: HMODULE) {
    let own_path = module_path(Some(own));
    let proxy = own_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let known = KNOWN.iter().any(|(dll, _)| *dll == proxy);
    let fallback = not_implemented as *const () as usize;
    // Not under a system DLL's name (injected, loaded as `uniloader.dll`, or as an `.asi` by
    // another loader): nothing to forward to, and asking System32 for it would only fail.
    let is_dll = own_path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("dll"));
    let wanted = known || (is_dll && !proxy.starts_with("uniloader"));
    let system = if wanted {
        let path = system_path(&format!("{proxy}.dll"));
        // SAFETY: `path` is NUL-terminated and outlives the call.
        unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) }.ok()
    } else {
        None
    };
    let mut resolved = 0;
    for (slot, name) in TABLE.iter().zip(NAMES) {
        let target = system.and_then(|module| {
            // SAFETY: `module` is loaded and `name` is NUL-terminated.
            unsafe { GetProcAddress(module, PCSTR(name.as_ptr())) }
        });
        resolved += usize::from(target.is_some());
        slot.store(target.map_or(fallback, |f| f as usize), Ordering::Release);
    }
    let _ = FORWARDING.set(Forwarding {
        own_path,
        proxy,
        known,
        wanted,
        loaded: system.is_some(),
        resolved,
    });
}

/// What the proxy forwards, once `DllMain` ran.
pub(crate) fn forwarding() -> Option<&'static Forwarding> {
    FORWARDING.get()
}

/// Log what the proxy forwards. Called once the logger exists.
pub(crate) fn log_summary() {
    let Some(f) = forwarding() else {
        log::warn!("proxy table was never filled");
        return;
    };
    let expected = KNOWN
        .iter()
        .find(|(dll, _)| *dll == f.proxy)
        .map_or(0, |(_, n)| *n);
    match (f.loaded, f.known) {
        (true, true) => log::info!(
            "proxy for {}.dll: {} of {expected} listed exports forwarded",
            f.proxy,
            f.resolved
        ),
        (true, false) => log::warn!(
            "proxy for {}.dll, which has no export list: {} exports of other lists forwarded",
            f.proxy,
            f.resolved
        ),
        (false, _) if !f.wanted => {
            log::info!("loaded as {}, not forwarding", f.own_path.display());
        }
        (false, _) => log::error!(
            "system {}.dll could not be loaded; its exports return E_NOTIMPL",
            f.proxy
        ),
    }
}
