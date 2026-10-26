//! The places that run a plugin's code without the loader calling it: hooks the game calls
//! (detours, vtable slots, window procedures, script function thunks), service functions
//! other plugins call, and the plugin's own threads. The loader guards its own callbacks; a
//! panic or hardware fault anywhere else ends the game. These run such code under the same
//! guard ([`Host::guarded`] on the plugin itself):
//!
//! - a panic is caught (and logged by the panic hook with where it was raised);
//! - a hardware fault (an access violation, a bad pointer read) is contained on the MSVC
//!   build the loader ships; the loader disables the plugin, so its callbacks stop too.
//!
//! What follows is up to the caller: a hook runs the game's original instead
//! ([`HookGuard`], which also stays off from then on), a service returns its error value,
//! a thread ends. Cost: one call into the loader and a structured exception frame, a few
//! nanoseconds; fine for anything called up to some thousands of times a frame.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use crate::Host;

/// This plugin's host, set when it starts.
static HOST: OnceLock<Host> = OnceLock::new();

pub(crate) fn install(host: Host) {
    let _ = HOST.set(host);
}

/// This plugin's host, from anywhere (a hook, a thread); `None` before `start`.
#[must_use]
pub fn host() -> Option<Host> {
    HOST.get().copied()
}

/// Why a guarded call returned no value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failed {
    /// It panicked.
    Panicked,
    /// It faulted; the loader disabled the plugin.
    Faulted,
    /// The plugin is disabled or stopped; it was not run.
    Disabled,
}

/// Runs `f` under the loader's guard for this plugin: its value, or why there is none.
/// Outside a loaded plugin (tests, before `start`) only a panic is caught.
///
/// # Errors
/// [`Failed`]: `f` panicked or faulted, or could not be run.
pub fn call<R>(f: impl FnOnce() -> R) -> Result<R, Failed> {
    guarded(false, f)
}

/// [`call`], or with `always` run even while the plugin is paused or after it stopped (its
/// own threads); then [`Failed::Disabled`] never comes back.
fn guarded<R>(always: bool, f: impl FnOnce() -> R) -> Result<R, Failed> {
    let Some(host) = HOST.get() else {
        return catch_unwind(AssertUnwindSafe(f)).map_err(|_| Failed::Panicked);
    };
    let mut out = None;
    let mut panicked = false;
    let body = || match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => out = Some(value),
        Err(_) => panicked = true,
    };
    let ran = if always {
        host.guarded_run(host.me(), body)
    } else {
        host.guarded(host.me(), body)
    };
    match (ran, out) {
        (Ok(()), Some(value)) => Ok(value),
        (Ok(()), None) if panicked => Err(Failed::Panicked),
        (Err(crate::PluginError::Status(s)), _) if s == crate::__private::Status::NOT_FOUND => {
            Err(Failed::Disabled)
        }
        _ => Err(Failed::Faulted),
    }
}

/// One hook the game calls. Its body runs under [`call`]; once it panicked or faulted it
/// is skipped for good (the caller runs the game's original), so a broken hook costs its
/// feature, not the game.
///
/// ```ignore
/// static GUARD: HookGuard = HookGuard::new("present");
/// unsafe extern "system" fn present(this: *mut c_void, sync: u32, flags: u32) -> i32 {
///     let _ = GUARD.run(|| draw(this));
///     unsafe { ORIGINAL(this, sync, flags) }
/// }
/// ```
#[derive(Debug)]
pub struct HookGuard {
    name: &'static str,
    tripped: AtomicBool,
}

impl HookGuard {
    /// A guard for the hook called `name` in the log.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            tripped: AtomicBool::new(false),
        }
    }

    /// Runs `body`; `None` when it is off, the plugin is disabled, or it failed now.
    pub fn run<R>(&self, body: impl FnOnce() -> R) -> Option<R> {
        if self.tripped.load(Ordering::Relaxed) {
            return None;
        }
        match call(body) {
            Ok(value) => Some(value),
            // Disabled: it may be enabled again.
            Err(Failed::Disabled) => None,
            Err(Failed::Panicked) => {
                self.trip("panicked");
                None
            }
            Err(Failed::Faulted) => {
                self.trip("faulted");
                None
            }
        }
    }

    /// Whether the hook was turned off by a failure.
    #[must_use]
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Relaxed)
    }

    fn trip(&self, why: &str) {
        if !self.tripped.swap(true, Ordering::Relaxed) {
            log::error!(
                "hook {} {why}; it is off for the rest of the session",
                self.name
            );
        }
    }
}

/// Runs a service function (one another plugin calls through a table) under [`call`]:
/// its value, or `fallback` (logged) when it failed, so neither a panic unwinds into the
/// caller nor a fault is blamed on it.
pub fn service<R>(name: &str, fallback: R, f: impl FnOnce() -> R) -> R {
    call(f).unwrap_or_else(|why| {
        log::error!("service function {name}: {why:?}");
        fallback
    })
}

/// [`std::thread::Builder::spawn`] with the thread's body under the loader's guard: a panic
/// or fault ends the thread, not the game, and `join` says so as it does for a panic.
///
/// Unlike [`call`], the body runs while the plugin is paused and after it stopped: a
/// plugin's threads are its own, and one its `stop` starts (closing connections, say) has to
/// be able to finish.
///
/// # Errors
/// The thread could not be created.
pub fn spawn<T: Send + 'static>(
    name: &str,
    body: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<T>> {
    let label = name.to_owned();
    std::thread::Builder::new()
        .name(label.clone())
        .spawn(move || match guarded(true, body) {
            Ok(value) => value,
            Err(why) => {
                log::error!("thread {label} ended: {why:?}");
                // Ends this thread the way a panic does, so `join` reports it.
                std::panic::resume_unwind(Box::new(format!("thread {label}: {why:?}")))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{call, service, spawn, Failed, HookGuard};

    #[test]
    fn values_pass_and_panics_are_caught() {
        assert_eq!(call(|| 7), Ok(7));
        assert_eq!(
            call(|| -> u8 { panic!("test panic") }),
            Err(Failed::Panicked)
        );
        assert_eq!(service("t", 1, || -> u8 { panic!("test panic") }), 1);
    }

    #[test]
    fn a_hook_that_panicked_stays_off() {
        static GUARD: HookGuard = HookGuard::new("test hook");
        assert_eq!(GUARD.run(|| 1), Some(1));
        assert_eq!(GUARD.run(|| -> u8 { panic!("test panic") }), None);
        assert!(GUARD.is_tripped());
        assert_eq!(GUARD.run(|| 2), None);
    }

    #[test]
    fn a_thread_that_panicked_says_so_on_join() {
        let fine = spawn("test-fine", || 5).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fine.join().ok(), Some(5));
        let broken = spawn("test-broken", || -> u8 { panic!("test panic") })
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(broken.join().is_err());
    }
}
