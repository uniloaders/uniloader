//! Entry point. `DllMain` fills the proxy's forward table, which must happen before the game
//! calls any export, and starts the loader on its own thread, which only runs once the OS
//! loader lock is released: loading plugin DLLs under that lock can deadlock.

// The entry point is an OS callback with a fixed signature.
#![allow(unsafe_code)]

use core::ffi::c_void;

use windows::Win32::Foundation::{HINSTANCE, HMODULE};
use windows::Win32::System::LibraryLoader::DisableThreadLibraryCalls;
use windows::Win32::System::SystemServices::{DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH};

/// `TRUE` as the Win32 `BOOL` this entry point returns.
const TRUE: i32 = 1;

/// Windows entry point.
///
/// # Safety
/// Called by the OS loader with the documented arguments.
#[no_mangle]
pub unsafe extern "system" fn DllMain(
    module: HINSTANCE,
    reason: u32,
    reserved: *mut c_void,
) -> i32 {
    match reason {
        DLL_PROCESS_ATTACH => {
            let module = HMODULE(module.0);
            // SAFETY: `module` is this DLL's handle, as the OS passed it.
            let _ = unsafe { DisableThreadLibraryCalls(module) };
            // SAFETY: process attach runs once, before any export of this DLL.
            unsafe { crate::win::proxy::load_forwards(module) };
            // The thread blocks on the loader lock until this returns, then initialises. A
            // failure has nowhere to be logged yet; the proxy still forwards, which is what
            // keeps the game running.
            let _ = std::thread::Builder::new()
                .name("uniloader-init".to_owned())
                .spawn(crate::win::initialize);
        }
        // A non-null `reserved` means the process is terminating rather than this DLL
        // being unloaded.
        DLL_PROCESS_DETACH => crate::win::shutdown(!reserved.is_null()),
        _ => {}
    }
    TRUE
}
