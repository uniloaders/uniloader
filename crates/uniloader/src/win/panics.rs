//! The loader's panic hook: every panic in the loader's own code is logged with its message,
//! where it was raised, the thread and the return addresses as `module+offset` (the release
//! has no symbols at run time; `scripts/crashdump` names them from the release's `.sym`
//! files). A panic that is caught leaves the line in the log; one that ends the process
//! (a panic that cannot unwind aborts, which no exception filter sees) leaves it in the log
//! of the report the next start makes.
//!
//! Each plugin DLL has its own copy of the standard library and so its own hook; the SDK
//! installs one of its own in each.

// FFI module: the stack walk.
#![allow(unsafe_code)]

use std::panic::PanicHookInfo;

use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;

/// Return addresses logged with a panic.
const FRAMES: usize = 32;

/// Puts the hook in place, in front of the one that was there (the default prints to a
/// stderr nobody sees, but a hook someone else set is kept).
pub(super) fn install() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("{}", describe(info));
        previous(info);
    }));
}

/// The log line: message, place, thread, then one return address per line.
fn describe(info: &PanicHookInfo<'_>) -> String {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("(no message)");
    let place = info
        .location()
        .map_or_else(|| "?".to_owned(), ToString::to_string);
    let thread = std::thread::current();
    let mut text = format!(
        "panic in thread {}: {message} at {place}",
        thread.name().unwrap_or("(unnamed)")
    );
    let mut frames = [core::ptr::null_mut(); FRAMES];
    // SAFETY: fills at most `frames.len()` slots of a live array.
    let n = unsafe { RtlCaptureStackBackTrace(1, &mut frames, None) };
    for frame in frames.iter().take(usize::from(n)) {
        let address = frame.addr();
        text.push_str(&super::guard::module_of(address).map_or_else(
            || format!("\n  at {address:#x}"),
            |(module, offset)| {
                let id = code_id(address.wrapping_sub(offset)).unwrap_or_default();
                format!("\n  at {module}+{offset:#x} [{id}]")
            },
        ));
    }
    text
}

/// The module's code id as Breakpad spells it (`TimeDateStamp` as `%08X`, then
/// `SizeOfImage` as `%x`), which pairs it with its symbols whatever name it was loaded under
/// (the loader is often `dwmapi.dll`).
fn code_id(base: usize) -> Option<String> {
    let read = |offset: usize| {
        // SAFETY: `base` is a loaded module's base, so its headers are mapped; the offsets
        // stay within them (checked signature, `e_lfanew` bounded).
        unsafe {
            core::ptr::read_unaligned(std::ptr::with_exposed_provenance::<u32>(base + offset))
        }
    };
    if base == 0 || read(0) & 0xFFFF != 0x5A4D {
        return None;
    }
    let pe = usize::try_from(read(0x3C)).ok().filter(|&pe| pe < 0x1000)?;
    if read(pe) != 0x4550 {
        return None;
    }
    // COFF header: TimeDateStamp at +8; optional header from +24, SizeOfImage at its +56.
    Some(format!("{:08X}{:x}", read(pe + 8), read(pe + 24 + 56)))
}
