//! The three hook kinds, each installed on something of this plugin's own, checked, removed
//! and checked again.

// Hooking is unsafe by nature: this module patches its own code and data on purpose.
#![allow(unsafe_code)]

use uniloader_sdk::Host;

#[cfg(windows)]
mod targets {
    use core::arch::naked_asm;

    /// `a + b`, with a prologue the detour can relocate: two short instructions and a five
    /// byte `nop` before the `ret`, so the stolen bytes hold no branch.
    #[unsafe(naked)]
    pub(super) extern "C" fn add(_a: i32, _b: i32) -> i32 {
        naked_asm!(
            "mov eax, ecx",
            "add eax, edx",
            "nop dword ptr [rax + rax + 0]",
            "ret",
        );
    }

    /// The detour's replacement: `a * b`.
    pub(super) extern "C" fn multiply(a: i32, b: i32) -> i32 {
        a.wrapping_mul(b)
    }

    /// Bytes for the patch check.
    pub(super) static PATCHABLE: [u8; 8] = *b"original";

    /// A minimal object with a virtual table, for the vtable check.
    #[repr(C)]
    pub(super) struct Object {
        pub(super) vtable: &'static [extern "C" fn() -> i32; 2],
    }

    extern "C" fn one() -> i32 {
        1
    }

    extern "C" fn two() -> i32 {
        2
    }

    pub(super) extern "C" fn three() -> i32 {
        3
    }

    pub(super) static VTABLE: [extern "C" fn() -> i32; 2] = [one, two];
    pub(super) static OBJECT: Object = Object { vtable: &VTABLE };
}

#[cfg(windows)]
fn patch(check: &mut dyn FnMut(&str, bool), host: &Host) {
    use core::ffi::c_void;
    let read = || {
        // SAFETY: a static of 8 bytes, read through a volatile load because its contents
        // change behind the compiler's back.
        unsafe { core::ptr::read_volatile(&raw const targets::PATCHABLE) }
    };
    let address = (&raw const targets::PATCHABLE).cast_mut().cast::<c_void>();
    // SAFETY: four bytes of this plugin's own static, which nothing executes.
    let hook = unsafe { host.patch(address, b"PATC", "kitchen self-test patch") };
    check("patch installs", hook.is_ok());
    check("patch visible", &read() == b"PATCinal");
    check(
        "second patch at the same address refused",
        // SAFETY: as above; the loader must refuse it before writing.
        unsafe { host.patch(address, b"X", "duplicate") }.is_err(),
    );
    if let Ok(hook) = hook {
        check("patch removes", host.remove_hook(hook).is_ok());
    }
    check("patch undone", &read() == b"original");
}

#[cfg(windows)]
fn vtable(check: &mut dyn FnMut(&str, bool), host: &Host) {
    use core::ffi::c_void;
    let slot = || {
        // SAFETY: entry 1 of a static table of two function pointers; volatile because the
        // hook rewrites it.
        let f = unsafe { core::ptr::read_volatile(&raw const targets::VTABLE[1]) };
        f()
    };
    let object = (&raw const targets::OBJECT).cast_mut().cast::<c_void>();
    let replacement = targets::three as *mut c_void;
    // SAFETY: `OBJECT` starts with a pointer to a two-entry table, and `three` has the
    // entries' signature.
    let hooked = unsafe { host.hook_vtable(object, 1, replacement, "kitchen self-test vtable") };
    check("vtable hook installs", hooked.is_ok());
    check("vtable hook visible", slot() == 3);
    if let Ok((hook, original)) = hooked {
        // SAFETY: the loader returns the pointer that was in the slot, which is `two`.
        let original: extern "C" fn() -> i32 = unsafe { core::mem::transmute(original) };
        check("vtable hook returns the original", original() == 2);
        check("vtable hook removes", host.remove_hook(hook).is_ok());
    }
    check("vtable hook undone", slot() == 2);
}

#[cfg(windows)]
fn detour(check: &mut dyn FnMut(&str, bool), host: &Host) {
    use core::ffi::c_void;
    let call = |a, b| std::hint::black_box(targets::add as extern "C" fn(i32, i32) -> i32)(a, b);
    check("detour target works", call(2, 3) == 5);
    let target = targets::add as *mut c_void;
    let replacement = targets::multiply as *mut c_void;
    // SAFETY: both functions have the same signature; `add`'s prologue is relocatable.
    let hooked = unsafe { host.detour(target, replacement, "kitchen self-test detour") };
    check("detour installs", hooked.is_ok());
    check("detour visible", call(2, 3) == 6);
    if let Ok((hook, trampoline)) = hooked {
        // SAFETY: the trampoline has the original function's signature.
        let original: extern "C" fn(i32, i32) -> i32 = unsafe { core::mem::transmute(trampoline) };
        check("trampoline reaches the original", original(2, 3) == 5);
        check("detour removes", host.remove_hook(hook).is_ok());
    }
    check("detour undone", call(2, 3) == 5);
}

/// Run the hook checks; `check(what, ok)` records each result.
#[cfg(windows)]
pub(crate) fn run(check: &mut dyn FnMut(&str, bool), host: &Host) {
    patch(check, host);
    vtable(check, host);
    detour(check, host);
}

/// Hooks only exist in the Windows build.
#[cfg(not(windows))]
pub(crate) fn run(_check: &mut dyn FnMut(&str, bool), _host: &Host) {}
