//! What a crash or a freeze leaves behind: minidumps and the game window's watchdog.
//!
//! - Dumps: `MiniDumpWriteDump` from `dbghelp.dll`, loaded from the system directory at
//!   start (never imported, and not loaded in a crashing process). The stacks and registers
//!   of every thread, the module list, the threads' details and the few bytes the stacks
//!   point at; no heap beyond that, so a dump is small and holds little of the player's
//!   data. A helper process writes it from outside the game ([`super::dumper`]); without
//!   one the game writes it itself. Before the dump, `info.txt` gets the faulting thread's
//!   registers, its stack and the module list in words ([`super::crashinfo`]).
//! - Crashes: a game engine usually catches its own crashes and writes its own report (UE's
//!   `Saved/Crashes`), which [`super::report`] picks up from what plugins attach. For a crash
//!   it does not catch, an unhandled-exception filter writes `crash.dmp`; the engine may set
//!   its own filter later, so the watchdog puts ours back in front of it, calling the one it
//!   replaced afterwards.
//! - Stack overflows: the overflowing thread has too little stack left for dbghelp, so a
//!   vectored handler hands the dump to a thread started for that and waits for it.
//!   A fail-fast (`__fastfail`, a Rust abort) reaches no handler at all; the panic hook
//!   ([`super::panics`]) logs a panic before it aborts.
//! - Freezes: the watchdog asks the game's main window to answer every second
//!   (`SendMessageTimeout` with `WM_NULL`). Silent for [`FIRST_DUMP`], it writes
//!   `freeze-1.dmp`, at [`SECOND_DUMP`] `freeze-2.dmp` (the same place twice: a hang; moved:
//!   just slow). If the window answers again, the dumps are thrown away; if the game is
//!   ended while frozen, the next start reports them. While a plugin says the game is
//!   loading (`report loading on`, for [`LOADING_CAP`] at most), silence is not counted.
//! - A normal end: the main window going away marks the session closing, so the next start
//!   knows the missing `DLL_PROCESS_DETACH` was not a crash; a main window that shows up
//!   after that (a splash screen went first) takes it back. `ExitProcess` marks it too
//!   ([`super::report::ending`]), for a quit faster than the watchdog's second.

// FFI module: dbghelp through a function pointer, the exception filter, window queries.
#![allow(unsafe_code)]

use std::fs;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use windows::core::{s, BOOL, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Diagnostics::Debug::{
    AddVectoredExceptionHandler, SetUnhandledExceptionFilter, EXCEPTION_POINTERS,
    LPTOP_LEVEL_EXCEPTION_FILTER, MINIDUMP_EXCEPTION_INFORMATION,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, SetEvent,
    WaitForSingleObject, INFINITE,
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
/// Silent at least this long, then answering: logged (loads and hitches), no dump.
const NOTE_SILENCE: Duration = Duration::from_secs(5);
/// How long the window has to answer one ping.
const PING_MS: u32 = 2_000;
/// How often the watchdog looks.
const EVERY: Duration = Duration::from_secs(1);
/// How often our exception filter is put back in front.
const REARM_EVERY: Duration = Duration::from_secs(5);
/// `MiniDumpWithUnloadedModules | MiniDumpWithIndirectlyReferencedMemory |
/// MiniDumpWithProcessThreadData | MiniDumpWithThreadInfo`.
const DUMP_TYPE: i32 = 0x20 | 0x40 | 0x100 | 0x1000;
/// `EXCEPTION_CONTINUE_SEARCH`.
const CONTINUE_SEARCH: i32 = 0;
/// `STATUS_STACK_OVERFLOW`.
pub(super) const STACK_OVERFLOW: u32 = 0xC000_00FD;
/// How long an overflowing thread waits for its dump: the helper's own wait plus time for
/// `info.txt`, so the process is still alive while the helper reads it.
const OVERFLOW_DUMP_MS: u32 = super::dumper::DUMP_WAIT_MS + 15_000;
/// The longest a load keeps the watchdog quiet; a load said to go on longer counts as a
/// freeze from then on.
pub(super) const LOADING_CAP: Duration = Duration::from_secs(300);

pub(super) type MiniDumpFn = unsafe extern "system" fn(
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
/// Crash handlers running now (the filter, or a stack overflow waiting for its dump).
static HANDLING: AtomicUsize = AtomicUsize::new(0);
/// When the last crash handler finished, on [`clock`]; 0: none did.
static HANDLED_AT: AtomicU64 = AtomicU64::new(0);
/// How long after a crash handler finished the process may take to end. Still answering
/// after that, it lived on (Wine ends only the thread of an overflow on a loader thread; a
/// game can survive a crash on a thread of its own): the watchdog watches again, and a next
/// crash gets its own report.
const SURVIVED_AFTER: Duration = Duration::from_secs(30);
/// The overflow dumper's events ([`start_overflow_dumper`]).
static DUMPER: OnceLock<(usize, usize)> = OnceLock::new();
/// The stack overflow being dumped: its exception and its thread.
static OVERFLOW: AtomicPtr<EXCEPTION_POINTERS> = AtomicPtr::new(core::ptr::null_mut());
static OVERFLOW_THREAD: AtomicU32 = AtomicU32::new(0);
/// When the game said its load ends at the latest, in milliseconds on [`clock`]; 0: not
/// loading.
static LOADING_UNTIL: AtomicU64 = AtomicU64::new(0);
static CLOCK: OnceLock<Instant> = OnceLock::new();

/// Loads dbghelp, remembers where reports go and puts the exception handlers in place.
pub(super) fn start(reports: &Path) {
    let _ = REPORTS.set(reports.to_path_buf());
    let loaded = MINIDUMP.get_or_init(load_minidump).is_some();
    if !loaded {
        log::warn!("reports: dbghelp.dll has no MiniDumpWriteDump; reports carry logs only");
    }
    rearm();
    start_overflow_dumper();
    let own = super::proxy::forwarding().map(|f| f.own_path.clone());
    if loaded && !own.as_deref().is_some_and(super::dumper::start) {
        log::warn!("reports: no dump helper; the game writes its own dumps");
    }
}

/// Milliseconds since the first look at this clock.
fn clock() -> u64 {
    let start = *CLOCK.get_or_init(Instant::now);
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The game is loading (`on`), for [`LOADING_CAP`] at the most, or is done: a silent window
/// is not a freeze while it loads.
pub(super) fn loading(on: bool) {
    let until = if on {
        clock().saturating_add(u64::try_from(LOADING_CAP.as_millis()).unwrap_or(u64::MAX))
    } else {
        0
    };
    LOADING_UNTIL.store(until, Ordering::Release);
}

/// Whether the game said it is loading, within [`LOADING_CAP`].
pub(super) fn is_loading() -> bool {
    clock() < LOADING_UNTIL.load(Ordering::Acquire)
}

pub(super) fn load_minidump() -> Option<MiniDumpFn> {
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

/// Writes a dump of this process to `path`, for the exception in `exception` (its pointers
/// and the id of the thread that raised it) or none; whether it was written. The helper
/// process writes it when it runs, else this process does.
pub(super) fn write_dump(path: &Path, exception: Option<(*const EXCEPTION_POINTERS, u32)>) -> bool {
    if let Some(written) = super::dumper::dump(path, exception) {
        return written;
    }
    let Some(Some(dump)) = MINIDUMP.get() else {
        return false;
    };
    let info = exception.map(|(pointers, thread)| MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: thread,
        ExceptionPointers: pointers.cast_mut(),
        ClientPointers: BOOL(0),
    });
    // SAFETY: plain queries; a pseudo handle of this process.
    let (process, pid) = unsafe { (GetCurrentProcess(), GetCurrentProcessId()) };
    // dbghelp reads the exception record `info` points at, which the OS keeps alive while the
    // filter (or the stack overflow handler waiting for this thread) runs.
    write_dump_of(*dump, process, pid, path, info)
}

/// Writes a dump of the process `process` (id `pid`) to `path` with `dump`, for the
/// exception `info` describes; whether it was written.
pub(super) fn write_dump_of(
    dump: MiniDumpFn,
    process: HANDLE,
    pid: u32,
    path: &Path,
    info: Option<MINIDUMP_EXCEPTION_INFORMATION>,
) -> bool {
    let Ok(file) = fs::File::create(path) else {
        return false;
    };
    // SAFETY: the process and file handles are live for the call; `info` lives on this stack
    // frame, its pointers are the caller's to keep alive.
    let written = unsafe {
        dump(
            process,
            pid,
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

/// The exception's code and address.
///
/// # Safety
/// `pointers` is null or what the OS passed to an exception handler that is still running.
unsafe fn code_and_address(pointers: *const EXCEPTION_POINTERS) -> (u32, usize) {
    // SAFETY: the caller's contract; the record lives while the handler runs.
    unsafe {
        pointers
            .as_ref()
            .and_then(|p| p.ExceptionRecord.as_ref())
            .map_or((0, 0), |r| {
                (
                    u32::from_ne_bytes(r.ExceptionCode.0.to_ne_bytes()),
                    r.ExceptionAddress as usize,
                )
            })
    }
}

/// A crash folder with the dump and what faulted, for the exception `pointers` on `thread`.
///
/// # Safety
/// As [`code_and_address`]; `thread` raised it and waits in its handler.
unsafe fn report_crash(pointers: *const EXCEPTION_POINTERS, thread: u32) {
    use super::crashinfo;
    let Some(reports) = REPORTS.get() else { return };
    // SAFETY: the caller's contract.
    let (code, address) = unsafe { code_and_address(pointers) };
    // SAFETY: as above; the context is the faulting thread's, which waits in its handler.
    let context = unsafe { pointers.as_ref().and_then(|p| p.ContextRecord.as_ref()) };
    // The walk reads the crashed stack, which may point anywhere: a fault in it is contained
    // and names no suspect, rather than ending the process before the report exists.
    let suspect = context.and_then(|c| {
        // SAFETY: plain Rust over the context the caller vouches for.
        unsafe { super::guard::call(|| super::suspects::on_stack(c)) }
            .ok()
            .flatten()
    });
    let modules = crashinfo::modules();
    let mut words = context.map_or_else(String::new, |c| {
        format!(
            "{}\n{}\n",
            crashinfo::registers(c),
            crashinfo::stack(c, &modules)
        )
    });
    words.push_str(&crashinfo::module_list(&modules));
    let crash = super::report::Crash {
        code,
        address,
        place: crashinfo::named(&modules, address as u64),
        suspect: suspect.as_deref(),
        words: &words,
    };
    super::report::crashed(reports, &crash, |path| {
        write_dump(path, Some((pointers, thread)))
    });
}

/// The unhandled-exception filter: a crash folder with the dump and what faulted, then the
/// filter that was there before.
unsafe extern "system" fn on_crash(pointers: *const EXCEPTION_POINTERS) -> i32 {
    if CRASHES.fetch_add(1, Ordering::AcqRel) == 0 {
        handling(true);
        // SAFETY: the OS passes a valid record for the duration of the filter, raised on this
        // thread.
        unsafe { report_crash(pointers, GetCurrentThreadId()) };
        handling(false);
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

/// The thread that writes the dump of a stack overflow, waiting for one, and the handler
/// that wakes it; started with the filter. Events as handle values (a `HANDLE` is not
/// `Sync`): (an overflow happened, its dump is written).
fn start_overflow_dumper() {
    // SAFETY: two unnamed auto-reset events, kept for the process.
    let events = unsafe {
        (
            CreateEventW(None, false, false, PCWSTR::null()),
            CreateEventW(None, false, false, PCWSTR::null()),
        )
    };
    let (Ok(asked), Ok(done)) = events else {
        log::warn!("reports: no stack overflow dumps (no events)");
        return;
    };
    let (asked, done) = (asked.0.expose_provenance(), done.0.expose_provenance());
    let spawned = std::thread::Builder::new()
        .name("uniloader-overflow-dumper".to_owned())
        .spawn(move || loop {
            // SAFETY: a live event handle; waiting forever is this thread's job.
            unsafe { WaitForSingleObject(handle(asked), INFINITE) };
            let pointers = OVERFLOW.load(Ordering::Acquire);
            let thread = OVERFLOW_THREAD.load(Ordering::Acquire);
            // SAFETY: the overflowing thread waits in its handler until `done` is set, which
            // keeps its exception record alive.
            unsafe {
                report_crash(pointers, thread);
                let _ = SetEvent(handle(done));
            }
        });
    if let Err(e) = spawned {
        log::warn!("reports: no stack overflow dumps: {e}");
        return;
    }
    let _ = DUMPER.set((asked, done));
    // SAFETY: a plain function as the first vectored handler, kept for the process.
    if unsafe { AddVectoredExceptionHandler(1, Some(on_exception)) }.is_null() {
        log::warn!("reports: no stack overflow dumps (no handler)");
    }
}

/// A stack overflow the fault guard contained: the game goes on, so the crash the overflow
/// handler counted is not the session's end, and the watchdog and the filter work again.
pub(super) fn overflow_contained() {
    CRASHES.store(0, Ordering::Release);
}

fn handle(value: usize) -> HANDLE {
    HANDLE(std::ptr::with_exposed_provenance_mut(value))
}

/// The first vectored handler: on a stack overflow the faulting thread has a few pages of
/// stack left, too few for dbghelp, so it hands the dump to the dumper thread and waits.
/// Every other exception goes on untouched; the filter sees the crashes.
unsafe extern "system" fn on_exception(pointers: *mut EXCEPTION_POINTERS) -> i32 {
    // SAFETY: the OS passes a valid record for the duration of the handler.
    let (code, _) = unsafe { code_and_address(pointers) };
    if code != STACK_OVERFLOW {
        return CONTINUE_SEARCH;
    }
    let Some(&(asked, done)) = DUMPER.get() else {
        return CONTINUE_SEARCH;
    };
    if CRASHES.fetch_add(1, Ordering::AcqRel) == 0 {
        handling(true);
        OVERFLOW.store(pointers, Ordering::Release);
        // SAFETY: plain query; live event handles; the wait times out.
        unsafe {
            OVERFLOW_THREAD.store(GetCurrentThreadId(), Ordering::Release);
            let _ = SetEvent(handle(asked));
            WaitForSingleObject(handle(done), OVERFLOW_DUMP_MS);
        }
        handling(false);
    }
    CONTINUE_SEARCH
}

/// A crash handler starts (`true`) or is done. Atomics and a clock read only: it runs on a
/// crashed thread.
fn handling(started: bool) {
    if started {
        HANDLING.fetch_add(1, Ordering::AcqRel);
    } else {
        HANDLED_AT.store(clock().max(1), Ordering::Release);
        HANDLING.fetch_sub(1, Ordering::AcqRel);
    }
}

/// For the watchdog: whether a crash is being handled, or was so recently that the process
/// may still be ending. A crash the process outlived (its window `hwnd` answers well after the
/// handler was done; one stuck in the engine's own crash reporter does not) is forgotten
/// here, so the watchdog and the filter work again; `true` while it has to stay quiet.
fn crash_in_hand(hwnd: HWND) -> bool {
    if CRASHES.load(Ordering::Acquire) == 0 {
        return false;
    }
    if HANDLING.load(Ordering::Acquire) > 0 {
        return true;
    }
    let done = HANDLED_AT.load(Ordering::Acquire);
    let grace = u64::try_from(SURVIVED_AFTER.as_millis()).unwrap_or(u64::MAX);
    if done == 0 || clock().saturating_sub(done) < grace || !answers(hwnd) {
        return true;
    }
    CRASHES.store(0, Ordering::Release);
    log::warn!(
        "reports: the game lived on after a crash (its report is written); the freeze \
         watchdog watches again, and a next crash gets its own report"
    );
    false
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
    /// Answering again after being silent this long (dumps were written, or it was at least
    /// [`NOTE_SILENCE`]).
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
                // A crash is being handled (its dump, the engine's own crash reporter): the
                // silent window is that, not a freeze.
                if crash_in_hand(hwnd) {
                    silent_since = None;
                    continue;
                }
                // A freeze that began before the load was said keeps counting.
                if dumps == 0 && is_loading() {
                    silent_since = None;
                    continue;
                }
                if answers(hwnd) {
                    if let Some(since) = silent_since.take() {
                        if dumps > 0 || since.elapsed() >= NOTE_SILENCE {
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
