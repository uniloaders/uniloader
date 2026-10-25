//! The `--loader-console` window: a real Win32 console that carries the loader log, the in-game
//! console output and whatever the game writes to its own standard streams, and reads
//! commands typed into it.

// FFI module: console allocation and stream redirection.
#![allow(unsafe_code)]

use std::io::Write;

use crate::console::{Ctx, Source};
use std::sync::Mutex;
use std::sync::OnceLock;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, GetConsoleWindow, SetConsoleCtrlHandler, SetConsoleOutputCP, SetConsoleTitleW,
    SetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};

/// UTF-8 code page, so log lines with non-ASCII text are not mangled.
const CP_UTF8: u32 = 65001;

static ACTIVE: OnceLock<bool> = OnceLock::new();

/// Whether a console window is attached to this process.
pub(crate) fn is_active() -> bool {
    *ACTIVE.get().unwrap_or(&false)
}

/// Attach a console and point the process's standard streams at it.
///
/// The game is a GUI subsystem binary, so it has no console and its `stdout`/`stderr`
/// writes go nowhere. Allocating one and rebinding the standard handles makes both the
/// loader's output and the game's own appear in the same window.
pub(crate) fn attach() -> bool {
    *ACTIVE.get_or_init(|| {
        // SAFETY: AllocConsole has no preconditions; it fails if a console already exists.
        let allocated = unsafe { AllocConsole() }.is_ok();
        // SAFETY: GetConsoleWindow has no preconditions.
        if !allocated && unsafe { GetConsoleWindow() }.is_invalid() {
            return false;
        }
        // SAFETY: both take plain values; failures are cosmetic and ignored deliberately.
        unsafe {
            let _ = SetConsoleOutputCP(CP_UTF8);
            let _ = SetConsoleTitleW(w!("uniloader"));
            // Ctrl+C in this window would kill the game; swallow the signals instead.
            let _ = SetConsoleCtrlHandler(None, true);
        }
        bind_streams();
        true
    })
}

/// Point the process standard handles at the console device.
fn bind_streams() {
    for (handle, name, write) in [
        (STD_OUTPUT_HANDLE, w!("CONOUT$"), true),
        (STD_ERROR_HANDLE, w!("CONOUT$"), true),
        (STD_INPUT_HANDLE, w!("CONIN$"), false),
    ] {
        let access = if write {
            GENERIC_WRITE.0
        } else {
            GENERIC_READ.0
        };
        // SAFETY: `name` is a static NUL terminated wide string; the remaining arguments are
        // the documented values for opening the console device.
        let file = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };
        match file {
            // SAFETY: `h` is a handle we just opened and intentionally leak: the standard
            // handle must stay valid for the process's lifetime.
            Ok(h) => unsafe {
                if let Err(e) = SetStdHandle(handle, h) {
                    let _ = writeln!(std::io::stderr(), "could not rebind {name:?}: {e}");
                }
            },
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "could not open console device: {e}");
            }
        }
    }
}

/// The one lock every console write goes through.
///
/// Wine's console device does not write a line atomically, so two threads writing at once
/// interleave mid-line and the window fills with spliced text. Every writer the loader owns
/// — the log, plugin output, command results — goes through here, as one `write_all` of one
/// buffer under one lock. The game's own writes to the same handle are outside our reach,
/// but there are few of them.
static WRITING: Mutex<()> = Mutex::new(());

/// Write one line to the console window, if there is one.
pub(crate) fn print(line: &str) {
    if !is_active() {
        return;
    }
    let mut buffer = String::with_capacity(line.len() + 1);
    buffer.push_str(line);
    buffer.push('\n');
    let _guard = WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut out = std::io::stdout();
    let _ = out.write_all(buffer.as_bytes());
    let _ = out.flush();
}

/// Start the thread that reads typed lines and executes them.
///
/// Deliberately not part of [`attach`]: that runs before the loader's state exists, and the
/// first line typed would otherwise build a second, default state. Called once at the end of
/// initialisation instead, so everything a command can name is already registered.
pub(crate) fn start_input() {
    if !is_active() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("uniloader-console".to_owned())
        .spawn(read_lines);
    match spawned {
        Ok(_) => print("type `help` for the commands this loader knows."),
        Err(e) => log::warn!("no console input: {e}"),
    }
}

/// Read lines until the console goes away, running each one.
///
/// A panic in here must not take the game with it, so the body is guarded and the thread
/// simply stops: output keeps working even when input does not.
fn read_lines() {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            // End of input: the console was closed, nothing more will arrive.
            Ok(0) => return,
            Ok(_) => {}
            Err(e) => {
                log::warn!("console input stopped: {e}");
                return;
            }
        }
        let typed = line.trim().to_owned();
        if typed.is_empty() {
            continue;
        }
        let outcome = std::panic::catch_unwind(|| {
            super::run_console_line(Ctx::from(Source::Console), &typed)
        });
        if outcome.is_err() {
            log::error!("console command {typed:?} panicked");
        }
    }
}
