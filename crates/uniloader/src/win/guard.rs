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
    /// The plugin raised a hardware or structured exception.
    Exception(u32),
}

impl std::fmt::Display for Fault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Panic => f.write_str("the call unwound"),
            Fault::Exception(code) => write!(f, "exception {code:#010x}"),
        }
    }
}

/// Whether hardware faults are contained as well as panics.
pub(crate) const CONTAINS_HARDWARE_FAULTS: bool = cfg!(all(windows, target_env = "msvc"));

/// Run a plugin call, containing panics and, on the MSVC target, hardware faults.
///
/// # Safety
/// `f` performs the FFI call; its own safety requirements are the caller's to uphold.
pub(crate) unsafe fn call<R>(f: impl FnOnce() -> R) -> Result<R, Fault> {
    let mut slot: Option<R> = None;
    // `protect` needs `FnMut` because the handler may be entered more than once by the ABI
    // it wraps; taking the `FnOnce` out of an `Option` keeps the single call guarantee.
    let mut once = Some(f);
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        inner::protect(|| {
            if let Some(f) = once.take() {
                slot = Some(f());
            }
        })
    }));
    match outcome {
        Ok(Ok(())) => slot.ok_or(Fault::Panic),
        Ok(Err(code)) => Err(Fault::Exception(code)),
        Err(_) => Err(Fault::Panic),
    }
}

#[cfg(all(windows, target_env = "msvc"))]
mod inner {
    /// Run `f` under a structured exception handler, reporting the exception code.
    pub(super) fn protect(f: impl FnMut()) -> Result<(), u32> {
        microseh::try_seh(f).map_err(|e| e.code() as u32)
    }
}

#[cfg(not(all(windows, target_env = "msvc")))]
mod inner {
    /// Without MSVC `__try` there is no structured handler, so a fault reaches the game.
    pub(super) fn protect(mut f: impl FnMut()) -> Result<(), u32> {
        f();
        Ok(())
    }
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
