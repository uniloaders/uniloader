//! What a crash or a freeze leaves behind: minidumps and the game window's watchdog.
//!
//! - Dumps: `MiniDumpWriteDump` from `dbghelp.dll`, loaded from the system directory at
//!   start (never imported, and not loaded in a crashing process). The stacks and registers
//!   of every thread, the module list and the threads' details; no heap, so a dump is small
//!   and holds little of the player's data.
//! - Crashes: a game engine usually catches its own crashes and writes its own report (UE's
//!   `Saved/Crashes`), which [`super::report`] picks up from what plugins attach. For a crash
//!   it does not catch, an unhandled-exception filter writes `crash.dmp`; the engine may set
//!   its own filter later, so the watchdog puts ours back in front of it, calling the one it
//!   replaced afterwards.
//! - Freezes: the watchdog asks the game's main window to answer every second
//!   (`SendMessageTimeout` with `WM_NULL`). Silent for [`FIRST_DUMP`], it writes
//!   `freeze-1.dmp`, at [`SECOND_DUMP`] `freeze-2.dmp` (the same place twice: a hang; moved:
//!   just slow). If the window answers again (a long load), the dumps are thrown away; if the
//!   game is ended while frozen, the next start reports them.
//! - A normal end: the main window going away marks the session closing, so the next start
//!   knows the missing `DLL_PROCESS_DETACH` was not a crash; a main window that shows up
//!   after that (a splash screen went first) takes it back. `ExitProcess` marks it too
//!   ([`super::report::ending`]), for a quit faster than the watchdog's second.

// FFI module: dbghelp through a function pointer, the exception filter, window queries.
#![allow(unsafe_code)]

use std::fs;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{s, BOOL, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Diagnostics::Debug::{
    SetUnhandledExceptionFilter, EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER,
    MINIDUMP_EXCEPTION_INFORMATION,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindow, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    SendMessageTimeoutW, GW_OWNER, SMTO_ABORTIFHUNG, WM_NULL,
};

/// Replaces the file at `path` (NUL terminated UTF-16) with `bytes` without allocating or
/// taking a lock: safe from `DllMain` while the process exits.
pub(super) fn write_raw(path: &[u16], bytes: &[u8]) {
    use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, WriteFile, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
    };
    if path.last() != Some(&0) {
        return;
    }
    // SAFETY: `path` is NUL terminated (checked); the handle is closed below.
    let Ok(file) = (unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }) else {
        return;
    };
    // SAFETY: `file` is the handle opened above, `bytes` a live slice.
    unsafe {
        let _ = WriteFile(file, Some(bytes), None, None);
        let _ = CloseHandle(file);
    }
}

/// Silent this long: the first freeze dump.
pub(super) const FIRST_DUMP: Duration = Duration::from_secs(20);
/// Silent this long: the second.
pub(super) const SECOND_DUMP: Duration = Duration::from_secs(40);
/// How long the window has to answer one ping.
const PING_MS: u32 = 2_000;
/// How often the watchdog looks.
const EVERY: Duration = Duration::from_secs(1);
/// How often our exception filter is put back in front.
const REARM_EVERY: Duration = Duration::from_secs(5);
/// `MiniDumpWithUnloadedModules | MiniDumpWithProcessThreadData | MiniDumpWithThreadInfo`.
const DUMP_TYPE: i32 = 0x20 | 0x100 | 0x1000;
/// `EXCEPTION_CONTINUE_SEARCH`.
const CONTINUE_SEARCH: i32 = 0;

type MiniDumpFn = unsafe extern "system" fn(
    HANDLE,
    u32,
    HANDLE,
    i32,
    *const MINIDUMP_EXCEPTION_INFORMATION,
    *const core::ffi::c_void,
    *const core::ffi::c_void,
) -> BOOL;

static MINIDUMP: OnceLock<Option<MiniDumpFn>> = OnceLock::new();
/// The reports folder, for the exception filter.
static REPORTS: OnceLock<PathBuf> = OnceLock::new();
/// The filter ours replaced, called after ours (a function pointer, or null).
static PREVIOUS: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(core::ptr::null_mut());
/// Crashes written this session: one dump is enough, a second fault in the filter is not.
static CRASHES: AtomicUsize = AtomicUsize::new(0);

/// Loads dbghelp, remembers where reports go and puts the exception filter in place.
pub(super) fn start(reports: &Path) {
    let _ = REPORTS.set(reports.to_path_buf());
    let loaded = MINIDUMP.get_or_init(load_minidump).is_some();
    if !loaded {
        log::warn!("reports: dbghelp.dll has no MiniDumpWriteDump; reports carry logs only");
    }
    rearm();
}

fn load_minidump() -> Option<MiniDumpFn> {
    let name: Vec<u16> = "dbghelp.dll\0".encode_utf16().collect();
    // SAFETY: NUL terminated name; the search is limited to the system directory.
    let module =
        unsafe { LoadLibraryExW(PCWSTR(name.as_ptr()), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .ok()?;
    // SAFETY: the export has the documented `MiniDumpWriteDump` signature; dbghelp stays
    // loaded for the process.
    unsafe {
        GetProcAddress(module, s!("MiniDumpWriteDump"))
            .map(|f| core::mem::transmute::<unsafe extern "system" fn() -> isize, MiniDumpFn>(f))
    }
}

/// Writes a dump of this process to `path`, for the exception in `exception` (on the thread
/// that raised it) or none; whether it was written.
pub(super) fn write_dump(path: &Path, exception: Option<*const EXCEPTION_POINTERS>) -> bool {
    let Some(Some(dump)) = MINIDUMP.get() else {
        return false;
    };
    let Ok(file) = fs::File::create(path) else {
        return false;
    };
    let info = exception.map(|pointers| MINIDUMP_EXCEPTION_INFORMATION {
        // SAFETY: plain query of the calling thread.
        ThreadId: unsafe { GetCurrentThreadId() },
        ExceptionPointers: pointers.cast_mut(),
        ClientPointers: BOOL(0),
    });
    // SAFETY: the process and file handles are live for the call; `info` lives on this stack
    // frame; dbghelp reads the exception record it points at, which the OS keeps alive while
    // the filter runs.
    let written = unsafe {
        dump(
            GetCurrentProcess(),
            GetCurrentProcessId(),
            HANDLE(file.as_raw_handle()),
            DUMP_TYPE,
            info.as_ref().map_or(core::ptr::null(), core::ptr::from_ref),
            core::ptr::null(),
            core::ptr::null(),
        )
    }
    .as_bool();
    drop(file);
    if !written {
        let _ = fs::remove_file(path);
    }
    written
}

/// Puts our filter in front again when something replaced it, keeping what it replaced to
/// call after ours.
fn rearm() {
    let ours: LPTOP_LEVEL_EXCEPTION_FILTER = Some(on_crash);
    // SAFETY: replacing the process's top-level filter; ours is a plain function.
    let previous = unsafe { SetUnhandledExceptionFilter(ours) };
    if let Some(f) = previous {
        let ours: unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32 = on_crash;
        if !core::ptr::fn_addr_eq(f, ours) {
            PREVIOUS.store(f as *mut core::ffi::c_void, Ordering::Release);
        }
    }
}

/// The unhandled-exception filter: a crash folder with the dump and what faulted, then the
/// filter that was there before.
unsafe extern "system" fn on_crash(pointers: *const EXCEPTION_POINTERS) -> i32 {
    if CRASHES.fetch_add(1, Ordering::AcqRel) == 0 {
        if let Some(reports) = REPORTS.get() {
            // SAFETY: the OS passes a valid record for the duration of the filter.
            let (code, address) = unsafe {
                pointers
                    .as_ref()
                    .and_then(|p| p.ExceptionRecord.as_ref())
                    .map_or((0, 0), |r| {
                        (
                            u32::from_ne_bytes(r.ExceptionCode.0.to_ne_bytes()),
                            r.ExceptionAddress as usize,
                        )
                    })
            };
            super::report::crashed(reports, code, address, |path| {
                write_dump(path, Some(pointers))
            });
        }
    }
    let previous = PREVIOUS.load(Ordering::Acquire);
    if previous.is_null() {
        return CONTINUE_SEARCH;
    }
    // SAFETY: `previous` came from `SetUnhandledExceptionFilter`, so it is a filter function.
    unsafe {
        let filter = core::mem::transmute::<
            *mut core::ffi::c_void,
            unsafe extern "system" fn(*const EXCEPTION_POINTERS) -> i32,
        >(previous);
        filter(pointers)
    }
}

/// The process's main window: its largest visible, unowned top-level window.
fn main_window() -> Option<HWND> {
    struct Search {
        pid: u32,
        best: Option<(HWND, i64)>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Search` passed below, alive for the enumeration.
        let search = unsafe { &mut *(data.0 as *mut Search) };
        let mut pid = 0u32;
        // SAFETY: queries of a window handle the enumeration gave us.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
            if pid != search.pid
                || !IsWindowVisible(hwnd).as_bool()
                || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid())
            {
                return BOOL(1);
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &raw mut rect).is_ok() {
                let area = i64::from(rect.right - rect.left) * i64::from(rect.bottom - rect.top);
                if search.best.is_none_or(|(_, a)| area > a) {
                    search.best = Some((hwnd, area));
                }
            }
        }
        BOOL(1)
    }
    let mut search = Search {
        // SAFETY: plain query.
        pid: unsafe { GetCurrentProcessId() },
        best: None,
    };
    // SAFETY: `each` only uses `search` during the call.
    let _ = unsafe {
        EnumWindows(
            Some(each),
            LPARAM(core::ptr::from_mut(&mut search) as isize),
        )
    };
    search.best.map(|(h, _)| h)
}

/// Whether `hwnd` answers within [`PING_MS`].
fn answers(hwnd: HWND) -> bool {
    let mut result = 0usize;
    // SAFETY: a window message with no pointers in it; the call times out.
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            PING_MS,
            Some(&raw mut result),
        )
    }
    .0 != 0
}

/// What the watchdog tells the reports.
pub(super) enum Seen {
    /// Silent this long: dump number `n` is due.
    Frozen { n: u32, silent: Duration },
    /// Answering again after being silent this long.
    Recovered(Duration),
    /// The main window went away: the game is closing.
    Closing,
    /// A main window again after [`Seen::Closing`]: what closed was a start-up window.
    Reopened,
}

/// Runs the watchdog on its own thread, telling `tell` what it sees.
pub(super) fn watch(tell: fn(&Seen)) {
    let spawned = std::thread::Builder::new()
        .name("uniloader-watchdog".to_owned())
        .spawn(move || {
            let mut window: Option<HWND> = None;
            let mut had_window = false;
            let mut closed = false;
            let mut silent_since: Option<Instant> = None;
            let mut dumps = 0u32;
            let mut rearmed = Instant::now();
            loop {
                std::thread::sleep(EVERY);
                if rearmed.elapsed() >= REARM_EVERY {
                    rearm();
                    rearmed = Instant::now();
                }
                // SAFETY: `IsWindow` accepts any handle value.
                if window.is_some_and(|w| !unsafe { IsWindow(Some(w)) }.as_bool()) {
                    window = None;
                    if had_window {
                        tell(&Seen::Closing);
                        had_window = false;
                        closed = true;
                    }
                }
                if window.is_none() {
                    window = main_window();
                    had_window |= window.is_some();
                    if closed && window.is_some() {
                        tell(&Seen::Reopened);
                        closed = false;
                    }
                }
                let Some(hwnd) = window else { continue };
                if answers(hwnd) {
                    if let Some(since) = silent_since.take() {
                        if dumps > 0 {
                            tell(&Seen::Recovered(since.elapsed()));
                        }
                        dumps = 0;
                    }
                    continue;
                }
                let since = *silent_since.get_or_insert_with(Instant::now);
                let silent = since.elapsed();
                let due = match dumps {
                    0 => silent >= FIRST_DUMP,
                    1 => silent >= SECOND_DUMP,
                    _ => false,
                };
                if due {
                    dumps += 1;
                    tell(&Seen::Frozen { n: dumps, silent });
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("reports: no watchdog: {e}");
    }
}
