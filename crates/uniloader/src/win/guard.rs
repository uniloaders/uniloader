//! Isolation of plugin calls.
//!
//! A plugin is foreign code in the game's process, so a bad one can do two things the
//! loader must survive: unwind (a Rust plugin whose own guard was bypassed, or a C++ plugin
//! letting an exception escape) and fault (access violation, divide by zero, stack
//! overflow). [`call`] contains both: `catch_unwind` for the first and a structured
//! exception handler for the second. The caller disables the plugin afterwards.
//!
//! The structured handler needs MSVC's `__try`, which the mingw toolchain does not provide.
//! The shipped DLL is built for the MSVC target and gets both layers; a mingw build gets the
//! unwind layer only and logs that at startup.

// This module exists to call foreign code; `call` is unsafe because its argument performs
// the FFI call.
#![allow(unsafe_code)]

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Why a plugin call did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    /// The plugin unwound out of the call.
    Panic,
    /// The plugin raised a hardware or structured exception: its code and the address of
    /// the faulting instruction.
    Exception(u32, usize),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Panic => f.write_str("the call unwound"),
            Fault::Exception(code, address) => {
                write!(f, "exception {code:#010x} at {address:#x}")?;
                if let Some((module, offset)) = inner::module_of(*address) {
                    write!(f, " ({module}+{offset:#x})")?;
                }
                Ok(())
            }
        }
    }
}

/// The file name of the module holding `address` and the offset into it.
pub(super) fn module_of(address: usize) -> Option<(String, usize)> {
    inner::module_of(address)
}

/// Whether hardware faults are contained as well as panics.
pub(crate) const CONTAINS_HARDWARE_FAULTS: bool = cfg!(all(windows, target_env = "msvc"));

/// Run a plugin call, containing panics and, on the MSVC target, hardware faults.
///
/// # Safety
/// `f` performs the FFI call; its own safety requirements are the caller's to uphold.
pub(crate) unsafe fn call<R>(f: impl FnOnce() -> R) -> Result<R, Fault> {
    let mut slot: Option<R> = None;
    let mut panicked = false;
    // `protect` needs `FnMut` because the handler may be entered more than once by the ABI
    // it wraps; taking the `FnOnce` out of an `Option` keeps the single call guarantee.
    let mut once = Some(f);
    // `catch_unwind` goes inside the structured handler: microseh calls the closure through
    // a C frame that cannot unwind, so a panic that reached it aborted the process (found
    // 2026-10-20 by running the Windows test binaries in the Windows VM).
    let outcome = inner::protect(|| {
        if let Some(f) = once.take() {
            match catch_unwind(AssertUnwindSafe(f)) {
                Ok(value) => slot = Some(value),
                Err(_) => panicked = true,
            }
        }
    });
    match outcome {
        Ok(()) if panicked => Err(Fault::Panic),
        Ok(()) => slot.ok_or(Fault::Panic),
        Err((code, address)) => {
            if code == super::crash::STACK_OVERFLOW {
                inner::overflow_contained();
            }
            Err(Fault::Exception(code, address))
        }
    }
}

#[cfg(all(windows, target_env = "msvc"))]
mod inner {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };

    /// Run `f` under a structured exception handler, reporting the exception code and
    /// address.
    pub(super) fn protect(f: impl FnMut()) -> Result<(), (u32, usize)> {
        microseh::try_seh(f).map_err(|e| (e.raw_code(), e.address() as usize))
    }

    /// A stack overflow was caught and the stack unwound: the thread's guard page was used
    /// up by it, so the next overflow on this thread would end the process with no handler
    /// and no report. The CRT's `_resetstkoflw` puts it back; it is looked up at run time
    /// (`ucrtbase.dll`, which every Rust MSVC build loads, else `msvcrt.dll`) so a C runtime
    /// without it costs the re-arm, not the loader. The crash count the overflow handler
    /// raised goes back too: the game lives on, and the watchdog and the next real crash
    /// must not stay muted (the report written for the overflow stays, it names the plugin).
    pub(super) fn overflow_contained() {
        use windows::core::{s, w};
        use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        type Reset = unsafe extern "C" fn() -> i32;
        let reset = [w!("ucrtbase.dll"), w!("msvcrt.dll")]
            .into_iter()
            .find_map(|name| {
                // SAFETY: a constant NUL terminated name; no reference is taken.
                let module = unsafe { GetModuleHandleW(name) }.ok()?;
                // SAFETY: a loaded module and a constant export name.
                unsafe { GetProcAddress(module, s!("_resetstkoflw")) }
            });
        let restored = reset.is_some_and(|f| {
            // SAFETY: `_resetstkoflw` takes nothing and returns whether it worked; it is
            // called after the unwind, outside any handler, as it must be.
            unsafe { core::mem::transmute::<unsafe extern "system" fn() -> isize, Reset>(f)() != 0 }
        });
        super::super::crash::overflow_contained();
        log::error!(
            "a stack overflow was contained; the thread's guard page {}",
            if restored {
                "is back"
            } else {
                "could not be restored, so another overflow on it ends the game"
            }
        );
    }

    /// The file name of the module holding `address` and the offset into it.
    pub(super) fn module_of(address: usize) -> Option<(String, usize)> {
        let mut module = HMODULE::default();
        // SAFETY: FROM_ADDRESS reads `address` as an address only; UNCHANGED_REFCOUNT takes no
        // reference that would need releasing.
        unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                windows::core::PCWSTR(address as *const u16),
                &raw mut module,
            )
        }
        .ok()?;
        let path = super::super::proxy::module_path(Some(module));
        let name = path.file_name()?.to_string_lossy().into_owned();
        Some((name, address.wrapping_sub(module.0 as usize)))
    }
}

#[cfg(not(all(windows, target_env = "msvc")))]
mod inner {
    /// Without MSVC `__try` there is no structured handler, so a fault reaches the game.
    pub(super) fn protect(mut f: impl FnMut()) -> Result<(), (u32, usize)> {
        f();
        Ok(())
    }

    /// No module lookup without the structured handler (no fault is ever reported).
    pub(super) fn module_of(_address: usize) -> Option<(String, usize)> {
        None
    }

    /// Never reached: nothing is contained without the structured handler.
    pub(super) fn overflow_contained() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_values_and_contains_panics() {
        // SAFETY: the closures are plain Rust, not FFI.
        assert_eq!(unsafe { call(|| 7) }, Ok(7));
        // SAFETY: as above.
        let result: Result<(), Fault> = unsafe { call(|| panic!("plugin bug")) };
        assert_eq!(result, Err(Fault::Panic));
    }
}
