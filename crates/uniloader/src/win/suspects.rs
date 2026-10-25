//! Crash loops: which module a crash happened in, and skipping a module that crashed the
//! game twice in a row.
//!
//! - Every DLL the loader loads (plugins, `.asi` files, the `load` list) is noted with its
//!   address range.
//! - A crash unwinds the faulting thread's stack from the exception's context (the unwind
//!   tables of x64 code, `RtlVirtualUnwind`) and names the first noted module on it: the
//!   crash happened in it or in something it called. The crash report says so, and the
//!   name goes into `reports/suspects.txt`.
//! - The next start: a previous session that closed normally clears the list. One that
//!   crashed with the same module named for the last two crashes skips that module for
//!   this session (plugins say why in the plugin list); after a session without it, it is
//!   loaded again.
//!
//! A crash in the game's own code that a module caused earlier (corrupted state) names
//! nothing: only what is on the stack counts.

// FFI module: module ranges and the stack unwind.
#![allow(unsafe_code)]

use std::fs;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::Diagnostics::Debug::{
    RtlLookupFunctionEntry, RtlVirtualUnwind, CONTEXT, UNW_FLAG_NHANDLER,
};
use windows::Win32::System::ProcessStatus::{GetModuleInformation, MODULEINFO};
use windows::Win32::System::Threading::GetCurrentProcess;

/// The list in the reports folder.
pub(super) const FILE: &str = "suspects.txt";
/// Frames looked at.
const FRAMES: usize = 64;

/// Loaded modules: start, end, file name.
static MODULES: Mutex<Vec<(usize, usize, String)>> = Mutex::new(Vec::new());
/// File names skipped this session, lower case.
static SKIPPED: OnceLock<Vec<String>> = OnceLock::new();
/// The loader's own file name.
static OWN: OnceLock<String> = OnceLock::new();

/// Notes the loader's own module: named in reports, never counted as a crash loop.
pub(super) fn note_own() {
    use windows::Win32::System::LibraryLoader::{
        GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    let mut module = HMODULE::default();
    let inside: fn() = note_own;
    // SAFETY: FROM_ADDRESS reads the address of a function of ours as an address only;
    // UNCHANGED_REFCOUNT takes no reference.
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(inside as *const u16),
            &raw mut module,
        )
    };
    if found.is_err() {
        return;
    }
    let path = super::proxy::module_path(Some(module));
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return;
    };
    note(module, &name);
    let _ = OWN.set(name);
}

/// Notes a module the loader loaded, under its file name.
pub(super) fn note(module: HMODULE, name: &str) {
    let mut info = MODULEINFO::default();
    // SAFETY: a module handle of this process; `info` is a writable MODULEINFO.
    let found = unsafe {
        GetModuleInformation(
            GetCurrentProcess(),
            module,
            &raw mut info,
            u32::try_from(size_of::<MODULEINFO>()).unwrap_or(0),
        )
    };
    if found.is_err() {
        return;
    }
    let start = info.lpBaseOfDll as usize;
    let end = start + usize::try_from(info.SizeOfImage).unwrap_or(0);
    if let Ok(mut modules) = MODULES.lock() {
        modules.push((start, end, name.to_owned()));
    }
}

/// The first noted module on the stack of the thread whose state `context` holds. Takes no
/// lock it could wait for (a crashing thread may hold any).
pub(super) fn on_stack(context: &CONTEXT) -> Option<String> {
    let modules = MODULES.try_lock().ok()?;
    let named = |address: u64| {
        let address = usize::try_from(address).ok()?;
        modules
            .iter()
            .find(|(start, end, _)| (*start..*end).contains(&address))
            .map(|(_, _, name)| name.clone())
    };
    let mut context = *context;
    for _ in 0..FRAMES {
        if let Some(name) = named(context.Rip) {
            return Some(name);
        }
        if !unwind(&mut context) || context.Rip == 0 {
            return None;
        }
    }
    None
}

/// One frame up: `context` becomes its caller's. Whether that worked.
pub(super) fn unwind(context: &mut CONTEXT) -> bool {
    let mut base = 0u64;
    // SAFETY: a lookup in this process's loaded modules' tables.
    let entry = unsafe { RtlLookupFunctionEntry(context.Rip, &raw mut base, None) };
    if entry.is_null() {
        // A leaf function: the return address is on top of the stack.
        let Ok(rsp) = usize::try_from(context.Rsp) else {
            return false;
        };
        if rsp == 0 || rsp % 8 != 0 {
            return false;
        }
        // SAFETY: the thread's stack pointer, aligned, of a thread stopped in a handler.
        context.Rip = unsafe { std::ptr::with_exposed_provenance::<u64>(rsp).read() };
        context.Rsp += 8;
        return true;
    }
    let mut handler_data = core::ptr::null_mut();
    let mut frame = 0u64;
    // SAFETY: `entry` came from the lookup for `context.Rip` in the image at `base`;
    // `context` is ours to update.
    unsafe {
        RtlVirtualUnwind(
            UNW_FLAG_NHANDLER,
            base,
            context.Rip,
            entry,
            context,
            &raw mut handler_data,
            &raw mut frame,
            None,
        );
    }
    true
}

/// A crash in module `name`, or in none of ours ([`crate::reports::NO_SUSPECT`]): added to
/// the list in `reports`. The loader itself (noted too, for the report) is never skipped,
/// so it counts as none.
pub(super) fn record(reports: &Path, name: Option<&str>) {
    let path = reports.join(FILE);
    let mut text = fs::read_to_string(&path).unwrap_or_default();
    let own = OWN.get().map(String::as_str);
    text.push_str(
        name.filter(|n| Some(*n) != own)
            .unwrap_or(crate::reports::NO_SUSPECT),
    );
    text.push('\n');
    let _ = fs::write(path, text);
}

/// At start, before any module is loaded: clears the list after a normal end, or finds the
/// module to skip after a crash loop.
pub(super) fn start(reports: &Path, closed_normally: bool) {
    let path = reports.join(FILE);
    if closed_normally {
        let _ = fs::remove_file(path);
        let _ = SKIPPED.set(Vec::new());
        return;
    }
    let text = fs::read_to_string(&path).unwrap_or_default();
    let skip = crate::reports::crash_loop(&text);
    if let Some(name) = &skip {
        log::warn!(
            "{name} was on the stack of the last two crashes; it is not loaded this session"
        );
        // Kept with one entry, so one more crash in it makes a loop again at once.
        let _ = fs::write(path, format!("{name}\n"));
    }
    let _ = SKIPPED.set(skip.into_iter().map(|n| n.to_ascii_lowercase()).collect());
}

/// Whether the module file `name` is skipped this session.
pub(super) fn skipped(name: &str) -> bool {
    SKIPPED
        .get()
        .is_some_and(|s| s.iter().any(|n| n.eq_ignore_ascii_case(name)))
}

/// Why a skipped module is not loaded, for the plugin list.
pub(super) const REASON: &str =
    "not loaded this session: it was on the stack of the last two crashes";
