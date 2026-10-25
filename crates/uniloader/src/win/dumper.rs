//! Dumps written by a second process: `rundll32 <loader>,UniloaderDumper <pid>`, started with
//! the game. A process that writes its own dump while it crashes depends on locks the crashing
//! thread may hold (the heap's, the loader's) and on staying alive until dbghelp is done; a
//! friend's Windows game left a 0-byte `crash.dmp` that way (2026-10-25). The helper holds the
//! game's process handle and writes the dump from outside when asked. When it is not running
//! (it could not start, or the platform's dbghelp cannot dump another process),
//! [`super::crash`] writes the dump itself as before.
//!
//! The two talk through named objects of the session, `Local\uniloader-dump-<pid>-…`: a
//! mapping that holds one [`Request`], an event that asks, one that says done, and one the
//! helper sets once it is ready. The helper ends when the game does. In the helper process
//! the loader starts nothing else ([`is_helper`]).

// FFI module: the shared mapping, the events and the helper process.
#![allow(unsafe_code)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use windows::core::{BOOL, PCSTR, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HINSTANCE, HWND, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows::Win32::System::Diagnostics::Debug::{
    EXCEPTION_POINTERS, MINIDUMP_EXCEPTION_INFORMATION,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, OpenFileMappingW, FILE_MAP_ALL_ACCESS, PAGE_READWRITE,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{
    CreateEventW, CreateProcessW, GetCurrentProcessId, OpenEventW, OpenProcess, SetEvent,
    WaitForMultipleObjects, WaitForSingleObject, CREATE_NO_WINDOW, EVENT_ALL_ACCESS, INFINITE,
    PROCESS_DUP_HANDLE, PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
    PROCESS_VM_READ, STARTUPINFOW,
};

/// UTF-16 units of the dump's path, NUL included.
const PATH_LEN: usize = 1024;
/// How long the game waits for the helper's dump.
pub(super) const DUMP_WAIT_MS: u32 = 60_000;
/// The export rundll32 calls.
const ENTRY: &str = "UniloaderDumper";

/// What the game asks for, in the shared mapping.
#[repr(C)]
struct Request {
    /// The thread that raised the exception.
    thread: u32,
    /// Whether `pointers` is set.
    with_exception: u32,
    /// The game's `EXCEPTION_POINTERS`, an address in the game's process.
    pointers: u64,
    /// The helper's answer: 1 written.
    written: u32,
    /// Where the dump goes, NUL terminated.
    path: [u16; PATH_LEN],
}

/// The game's side: handle values (a `HANDLE` is not `Sync`) and the mapping's view.
struct Link {
    view: usize,
    ask: usize,
    done: usize,
    ready: usize,
    helper: usize,
}

static LINK: OnceLock<Link> = OnceLock::new();
/// One request at a time.
static BUSY: AtomicBool = AtomicBool::new(false);

fn handle(value: usize) -> HANDLE {
    HANDLE(std::ptr::with_exposed_provenance_mut(value))
}

fn value(handle: HANDLE) -> usize {
    handle.0.expose_provenance()
}

/// `Local\uniloader-dump-<pid>-<what>`, NUL terminated.
fn name(pid: u32, what: &str) -> Vec<u16> {
    format!("Local\\uniloader-dump-{pid}-{what}\0")
        .encode_utf16()
        .collect()
}

/// Whether this process is a helper (rundll32 running [`ENTRY`]): the loader starts nothing
/// in it.
pub(crate) fn is_helper() -> bool {
    super::proxy::module_path(None)
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("rundll32.exe"))
}

/// Creates the shared objects and starts the helper for this process; `dll` is the loader's
/// own file. Whether it started (it says it is ready a moment later).
pub(super) fn start(dll: &Path) -> bool {
    // SAFETY: plain query.
    let pid = unsafe { GetCurrentProcessId() };
    let size = u32::try_from(size_of::<Request>()).unwrap_or(0);
    // SAFETY: a named mapping backed by the paging file, sized for one request.
    let map = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            size,
            PCWSTR(name(pid, "map").as_ptr()),
        )
    };
    let Ok(map) = map else { return false };
    // SAFETY: the mapping created above, mapped whole; kept for the process.
    let view = unsafe { MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
    if view.Value.is_null() {
        return false;
    }
    // SAFETY: named events; auto-reset ask and done, manual-reset ready.
    let events = unsafe {
        (
            CreateEventW(None, false, false, PCWSTR(name(pid, "ask").as_ptr())),
            CreateEventW(None, false, false, PCWSTR(name(pid, "done").as_ptr())),
            CreateEventW(None, true, false, PCWSTR(name(pid, "ready").as_ptr())),
        )
    };
    let (Ok(ask), Ok(done), Ok(ready)) = events else {
        return false;
    };
    let Some(helper) = spawn(dll, pid) else {
        return false;
    };
    LINK.set(Link {
        view: view.Value.expose_provenance(),
        ask: value(ask),
        done: value(done),
        ready: value(ready),
        helper: value(helper),
    })
    .is_ok()
}

/// Starts `rundll32 "<dll>",UniloaderDumper <pid>` without a window; its process handle.
fn spawn(dll: &Path, pid: u32) -> Option<HANDLE> {
    let mut dir = [0u16; 260];
    // SAFETY: `dir` is writable for its length.
    let len = unsafe { GetSystemDirectoryW(Some(&mut dir)) } as usize;
    if len == 0 || len >= dir.len() {
        return None;
    }
    let rundll = PathBuf::from(String::from_utf16_lossy(&dir[..len])).join("rundll32.exe");
    let line = format!(
        "\"{}\" \"{}\",{ENTRY} {pid}",
        rundll.display(),
        dll.display()
    );
    let app: Vec<u16> = rundll.as_os_str().encode_wide().chain([0]).collect();
    let mut line: Vec<u16> = line.encode_utf16().chain([0]).collect();
    let startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(0),
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: NUL terminated application and (writable) command line; no handles inherited.
    let started = unsafe {
        CreateProcessW(
            PCWSTR(app.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &raw const startup,
            &raw mut info,
        )
    };
    if let Err(e) = started {
        log::warn!("reports: cannot start the dump helper: {e}");
        return None;
    }
    // SAFETY: the thread handle CreateProcess gave us, not needed.
    let _ = unsafe { CloseHandle(info.hThread) };
    Some(info.hProcess)
}

/// Whether the helper said it is ready and still runs.
pub(super) fn ready() -> bool {
    let Some(link) = LINK.get() else { return false };
    // SAFETY: live handles; zero-timeout waits.
    unsafe {
        WaitForSingleObject(handle(link.ready), 0) == WAIT_OBJECT_0
            && WaitForSingleObject(handle(link.helper), 0) == WAIT_TIMEOUT
    }
}

/// Has the helper write a dump of this process to `path`, for the exception (its pointers and
/// the thread that raised it) or none. `None`: no helper, or it could not, so the caller
/// writes the dump itself; `Some(written)` otherwise (`false` after a wait that timed out,
/// when the helper may still be writing to `path`).
pub(super) fn dump(
    path: &Path,
    exception: Option<(*const EXCEPTION_POINTERS, u32)>,
) -> Option<bool> {
    let link = LINK.get()?;
    if !ready() {
        return None;
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.len() >= PATH_LEN || BUSY.swap(true, Ordering::AcqRel) {
        return None;
    }
    let request = std::ptr::with_exposed_provenance_mut::<Request>(link.view);
    // SAFETY: the view is mapped for the process and sized for one `Request`; only this
    // function writes it, one call at a time (`BUSY`), while the helper waits for `ask`.
    let answer = unsafe {
        let r = &mut *request;
        r.thread = exception.map_or(0, |(_, t)| t);
        r.with_exception = u32::from(exception.is_some());
        r.pointers = exception.map_or(0, |(p, _)| p.expose_provenance() as u64);
        r.written = 0;
        r.path[..wide.len()].copy_from_slice(&wide);
        r.path[wide.len()] = 0;
        let _ = SetEvent(handle(link.ask));
        let waited = WaitForSingleObject(handle(link.done), DUMP_WAIT_MS);
        if waited == WAIT_OBJECT_0 {
            (r.written == 1).then_some(true)
        } else {
            Some(false)
        }
    };
    BUSY.store(false, Ordering::Release);
    answer
}

/// The helper's entry, called by rundll32 with the game's process id as its command line.
/// Why it stopped goes to `uniloader/logs/dump-helper.log` beside the loader.
///
/// # Safety
/// Called by rundll32 with a NUL terminated ANSI command line.
#[no_mangle]
pub unsafe extern "system" fn UniloaderDumper(
    _window: HWND,
    _instance: HINSTANCE,
    command_line: PCSTR,
    _show: i32,
) {
    // SAFETY: rundll32 passes a NUL terminated string.
    let text = unsafe { command_line.to_string() }.unwrap_or_default();
    let outcome = text
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("bad process id {text:?}: {e}"))
        .and_then(serve);
    let line = match outcome {
        Ok(()) => format!("the game ({}) ended\n", text.trim()),
        Err(why) => format!("stopped: {why}\n"),
    };
    if let Some(dir) = super::proxy::forwarding().and_then(|f| f.own_path.parent()) {
        let logs = crate::config::Paths::resolve(dir).logs_dir;
        let _ = std::fs::create_dir_all(&logs);
        let _ = std::fs::write(logs.join("dump-helper.log"), line);
    }
}

/// Writes the dumps the game `pid` asks for until it ends; why it could not.
fn serve(pid: u32) -> Result<(), String> {
    let minidump = super::crash::load_minidump().ok_or("dbghelp has no MiniDumpWriteDump")?;
    // SAFETY: opening the game's process for reading its memory and threads, and for
    // waiting on it.
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_DUP_HANDLE | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .map_err(|e| format!("cannot open the game's process: {e}"))?;
    let open = |what: &str| {
        // SAFETY: a NUL terminated name of the game's event.
        unsafe { OpenEventW(EVENT_ALL_ACCESS, false, PCWSTR(name(pid, what).as_ptr())) }
            .map_err(|e| format!("cannot open the {what} event: {e}"))
    };
    let (ask, done, ready) = (open("ask")?, open("done")?, open("ready")?);
    // SAFETY: the game's mapping, by its name.
    let map = unsafe {
        OpenFileMappingW(
            FILE_MAP_ALL_ACCESS.0,
            false,
            PCWSTR(name(pid, "map").as_ptr()),
        )
    }
    .map_err(|e| format!("cannot open the shared mapping: {e}"))?;
    // SAFETY: the mapping opened above, mapped whole.
    let view = unsafe { MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, 0) };
    if view.Value.is_null() {
        return Err("cannot map the shared mapping".to_owned());
    }
    let request = view.Value.cast::<Request>();
    // SAFETY: a live event.
    let _ = unsafe { SetEvent(ready) };
    loop {
        // SAFETY: live handles; the game's process handle signals when it ends.
        let woke = unsafe { WaitForMultipleObjects(&[ask, process], false, INFINITE) };
        if woke == WAIT_FAILED {
            return Err(format!(
                "cannot wait: {}",
                windows::core::Error::from_thread()
            ));
        }
        if woke != WAIT_OBJECT_0 {
            return Ok(());
        }
        // SAFETY: the game wrote the request before setting `ask` and waits for `done`.
        let r = unsafe { &mut *request };
        let len = r.path.iter().position(|c| *c == 0).unwrap_or(PATH_LEN);
        let path = PathBuf::from(String::from_utf16_lossy(&r.path[..len]));
        let exception = (r.with_exception == 1).then(|| MINIDUMP_EXCEPTION_INFORMATION {
            ThreadId: r.thread,
            ExceptionPointers: std::ptr::with_exposed_provenance_mut(
                usize::try_from(r.pointers).unwrap_or(0),
            ),
            // The pointers are the game's, read from its memory.
            ClientPointers: BOOL(1),
        });
        let written = super::crash::write_dump_of(minidump, process, pid, &path, exception);
        r.written = u32::from(written);
        // SAFETY: a live event.
        let _ = unsafe { SetEvent(done) };
    }
}
