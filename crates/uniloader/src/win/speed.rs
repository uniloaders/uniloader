//! Game speed: the process's clocks run at `uniloader.speed` times real time.
//!
//! Cheat Engine's speedhack, for the same reason it works there: every engine measures frame
//! time with one of a handful of clock functions, so scaling those scales the game without
//! knowing anything about it. Hooked are `QueryPerformanceCounter`, `GetTickCount64`,
//! `GetTickCount` and, once `winmm.dll` is loaded, `timeGetTime`. The wall clock
//! (`GetSystemTime*`) stays real: saves are stamped with it and servers compare against it.
//!
//! Nothing is hooked until the speed first differs from 1, so a game that never changes it
//! runs exactly as it would without this. Once in place the hooks stay; at 1 they return real
//! time plus whatever the game gained or lost while it ran faster or slower.
//!
//! The hooks are called constantly from every thread, so they take no lock, allocate nothing
//! and log nothing: read the real clock, then the current [`Anchors`], then compute. A change
//! publishes a new `Anchors` and leaks the old one, because a reader may still be holding it
//! (a few dozen bytes per change, and changes come from a person pressing a key).

// FFI module: rewrites system clock functions and is called through them.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use uniloader_api::{topics, PluginHandle, Status};
use uniloader_core::thunk::{self, Jump};
use uniloader_core::timescale::{self, Anchor};
use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Performance::QueryPerformanceFrequency;

use crate::speed::{Action, NAME, SPEED};

type CounterFn = unsafe extern "system" fn(*mut i64) -> i32;
type Ticks64Fn = unsafe extern "system" fn() -> u64;
type Ticks32Fn = unsafe extern "system" fn() -> u32;

/// The scaled line of each clock. Immutable once published.
#[derive(Debug, Clone, Copy)]
struct Anchors {
    /// `QueryPerformanceCounter` ticks.
    counter: Anchor,
    /// `GetTickCount64` milliseconds, which `GetTickCount` is the low half of.
    ticks: Anchor,
    /// `timeGetTime` milliseconds, widened to 64 bits.
    multimedia: Anchor,
}

/// Null until the hooks are installed.
static ANCHORS: AtomicPtr<Anchors> = AtomicPtr::new(core::ptr::null_mut());

/// How each clock's real value is read: the trampoline once it is hooked, the function itself
/// before or if hooking failed, 0 when the function was not found.
static COUNTER: AtomicUsize = AtomicUsize::new(0);
static TICKS: AtomicUsize = AtomicUsize::new(0);
static MULTIMEDIA: AtomicUsize = AtomicUsize::new(0);

/// How far ahead of the change a new line starts, in milliseconds (see [`Anchor::rebase`]).
/// Two periods of the 15.6 ms system tick: a millisecond clock can step by one period between
/// reading it and publishing, and a millisecond counter that holds still that long already
/// does so between every tick.
const TICK_MARGIN: i64 = 32;

/// What changing the speed needs to know, behind the lock that serialises changes.
struct Changer {
    /// The loader's plugin handle, which owns the hooks.
    owner: PluginHandle,
    /// The speed the clocks run at now.
    factor: f64,
    /// Whether the kernel32 clocks are hooked.
    installed: bool,
    /// Whether `timeGetTime` is hooked.
    multimedia: bool,
    /// One millisecond in performance counter ticks.
    counter_margin: i64,
}

static CHANGER: Mutex<Option<Changer>> = Mutex::new(None);
static OWNER: OnceLock<PluginHandle> = OnceLock::new();

fn anchors() -> Option<&'static Anchors> {
    let ptr = ANCHORS.load(Ordering::Acquire);
    // SAFETY: a non-null pointer came from `Box::into_raw` and is never freed.
    unsafe { ptr.as_ref() }
}

fn counter_original() -> Option<CounterFn> {
    let address = COUNTER.load(Ordering::Acquire);
    // SAFETY: zero or the address of a function with this signature; `Option<fn>` has the
    // null niche, so zero is `None`.
    unsafe { core::mem::transmute::<usize, Option<CounterFn>>(address) }
}

fn ticks_original() -> Option<Ticks64Fn> {
    let address = TICKS.load(Ordering::Acquire);
    // SAFETY: as in `counter_original`.
    unsafe { core::mem::transmute::<usize, Option<Ticks64Fn>>(address) }
}

fn multimedia_original() -> Option<Ticks32Fn> {
    let address = MULTIMEDIA.load(Ordering::Acquire);
    // SAFETY: as in `counter_original`.
    unsafe { core::mem::transmute::<usize, Option<Ticks32Fn>>(address) }
}

/// Real readings of each clock, `None` for one that was not found.
fn read_counter() -> Option<i64> {
    let original = counter_original()?;
    let mut value = 0i64;
    // SAFETY: the real function, writing to a local.
    let ok = unsafe { original(&raw mut value) };
    (ok != 0).then_some(value)
}

fn read_ticks() -> Option<i64> {
    // SAFETY: the real function; it takes nothing.
    ticks_original().map(|original| i64::try_from(unsafe { original() }).unwrap_or(i64::MAX))
}

fn read_multimedia() -> Option<u32> {
    // SAFETY: the real function; it takes nothing.
    multimedia_original().map(|original| unsafe { original() })
}

unsafe extern "system" fn counter_hook(counter: *mut i64) -> i32 {
    let Some(original) = counter_original() else {
        return 0;
    };
    // SAFETY: the caller's argument, passed on unchanged.
    let ok = unsafe { original(counter) };
    // The real clock first, then the anchors: see `Anchor::rebase` for why the order matters.
    if let (Some(anchors), false) = (anchors(), counter.is_null()) {
        // SAFETY: the caller's buffer, which the original just wrote.
        unsafe { counter.write(anchors.counter.map(counter.read())) };
    }
    ok
}

fn scaled_ticks() -> i64 {
    let real = read_ticks().unwrap_or(0);
    anchors().map_or(real, |a| a.ticks.map(real))
}

extern "system" fn ticks64_hook() -> u64 {
    u64::try_from(scaled_ticks()).unwrap_or(0)
}

extern "system" fn ticks32_hook() -> u32 {
    timescale::narrow_u32(scaled_ticks())
}

extern "system" fn multimedia_hook() -> u32 {
    let real = read_multimedia().unwrap_or(0);
    match anchors() {
        Some(a) => {
            let wide = timescale::widen_u32(a.multimedia.real, real);
            timescale::narrow_u32(a.multimedia.map(wide))
        }
        None => real,
    }
}

/// A function in a loaded module, past any thunks in front of it.
fn resolve(module: PCWSTR, name: &str) -> Option<usize> {
    // SAFETY: a NUL terminated module name; this only looks up an already loaded module.
    let module = unsafe { GetModuleHandleW(module) }.ok()?;
    let symbol = std::ffi::CString::new(name).ok()?;
    // SAFETY: a valid module handle and a NUL terminated name.
    let mut address = unsafe { GetProcAddress(module, PCSTR(symbol.as_ptr().cast())) }? as usize;
    // A few hops at most: an import stub, then a forwarder into the DLL that implements it.
    for _ in 0..4 {
        let readable = super::plugin_hooks::accessible_from(address).min(16);
        if readable == 0 {
            return None;
        }
        // SAFETY: `readable` bytes from `address` are committed, accessible memory.
        let code = unsafe { core::slice::from_raw_parts(address as *const u8, readable) };
        let Some(jump) = thunk::jump(code, address as u64) else {
            return Some(address);
        };
        address = match jump {
            Jump::To(to) => usize::try_from(to).ok()?,
            Jump::Through(slot) => {
                let slot = usize::try_from(slot).ok()?;
                if super::plugin_hooks::accessible_from(slot) < 8 {
                    return None;
                }
                // SAFETY: eight readable bytes, checked above; a jump slot holds an address.
                unsafe { (slot as *const usize).read_unaligned() }
            }
        };
    }
    // Still a jump after that many: the detour refuses it and says so.
    Some(address)
}

/// Detour `target` with `replacement`, storing the original's way in into `slot` first. Logs and
/// leaves `slot` alone on failure: the clock is then read unscaled, which is the safe way to
/// be wrong.
fn hook(
    owner: PluginHandle,
    name: &str,
    target: usize,
    replacement: *const (),
    slot: Option<&'static AtomicUsize>,
) -> bool {
    let installed = super::plugin_hooks::detour_with(
        owner,
        NAME,
        target as *mut c_void,
        replacement.cast_mut().cast::<c_void>(),
        &format!("game speed: {name}"),
        |trampoline| {
            if let Some(slot) = slot {
                slot.store(trampoline as usize, Ordering::Release);
            }
        },
    );
    match installed {
        Ok(_) => true,
        Err(status) => {
            log::warn!(
                "game speed: cannot hook {name} at {target:#X} ({status:?}); it keeps real time"
            );
            false
        }
    }
}

/// Hook the kernel32 clocks, starting each on the identity line through its present value.
fn install(changer: &mut Changer) {
    changer.installed = true;
    let kernel32 = w!("kernel32.dll");
    let counter = resolve(kernel32, "QueryPerformanceCounter");
    let ticks64 = resolve(kernel32, "GetTickCount64");
    let ticks32 = resolve(kernel32, "GetTickCount");
    // Before any hook exists, these read the real functions.
    COUNTER.store(counter.unwrap_or(0), Ordering::Release);
    TICKS.store(ticks64.unwrap_or(0), Ordering::Release);
    let mut frequency = 0i64;
    // SAFETY: writes one integer to a local.
    if unsafe { QueryPerformanceFrequency(&raw mut frequency) }.is_ok() {
        changer.counter_margin = (frequency / 1000).max(1);
    }
    let start = Anchors {
        counter: Anchor::identity(read_counter().unwrap_or(0)),
        ticks: Anchor::identity(read_ticks().unwrap_or(0)),
        multimedia: Anchor::identity(0),
    };
    ANCHORS.store(Box::into_raw(Box::new(start)), Ordering::Release);

    let owner = changer.owner;
    let mut hooked = Vec::new();
    if let Some(target) = counter {
        if hook(
            owner,
            "QueryPerformanceCounter",
            target,
            counter_hook as *const (),
            Some(&COUNTER),
        ) {
            hooked.push("QueryPerformanceCounter");
        }
    }
    if let Some(target) = ticks64 {
        if hook(
            owner,
            "GetTickCount64",
            target,
            ticks64_hook as *const (),
            Some(&TICKS),
        ) {
            hooked.push("GetTickCount64");
        }
    }
    // Answered from the 64-bit clock, so only hooked when that one was found. Where both are
    // one function (the 32-bit result is the low half of the 64-bit one in `eax`), the 64-bit
    // hook already answers both.
    if let Some(target) = ticks32.filter(|t| ticks64.is_some_and(|wide| wide != *t)) {
        if hook(
            owner,
            "GetTickCount",
            target,
            ticks32_hook as *const (),
            None,
        ) {
            hooked.push("GetTickCount");
        }
    }
    let missing: Vec<&str> = [
        ("QueryPerformanceCounter", counter),
        ("GetTickCount64", ticks64),
        ("GetTickCount", ticks32),
    ]
    .iter()
    .filter(|(_, at)| at.is_none())
    .map(|(name, _)| *name)
    .collect();
    if !missing.is_empty() {
        log::warn!("game speed: not found: {}", missing.join(", "));
    }
    log::info!("game speed: hooked {}", hooked.join(", "));
}

/// Hook `timeGetTime` if `winmm.dll` is loaded by now. Asked again at every change, so a game
/// that loads it late is covered from the next change on.
fn install_multimedia(changer: &mut Changer) {
    if changer.multimedia {
        return;
    }
    let Some(target) = resolve(w!("winmm.dll"), "timeGetTime") else {
        return;
    };
    changer.multimedia = true;
    MULTIMEDIA.store(target, Ordering::Release);
    // The line through its present value, before the hook can read it.
    if let (Some(old), Some(now)) = (anchors().copied(), read_multimedia()) {
        let next = Anchors {
            multimedia: Anchor::identity(i64::from(now)),
            ..old
        };
        ANCHORS.store(Box::into_raw(Box::new(next)), Ordering::Release);
    }
    if hook(
        changer.owner,
        "timeGetTime",
        target,
        multimedia_hook as *const (),
        Some(&MULTIMEDIA),
    ) {
        log::info!("game speed: hooked timeGetTime");
    }
}

/// Continue every clock's line from now at `factor`.
fn rebase(changer: &Changer, factor: f64) {
    let Some(old) = anchors().copied() else {
        return;
    };
    let slope = timescale::fixed(factor);
    let next = Anchors {
        counter: read_counter().map_or(old.counter, |now| {
            old.counter.rebase(now, changer.counter_margin, slope)
        }),
        ticks: read_ticks().map_or(old.ticks, |now| old.ticks.rebase(now, TICK_MARGIN, slope)),
        multimedia: read_multimedia().map_or(old.multimedia, |now| {
            let now = timescale::widen_u32(old.multimedia.real, now);
            old.multimedia.rebase(now, TICK_MARGIN, slope)
        }),
    };
    ANCHORS.store(Box::into_raw(Box::new(next)), Ordering::Release);
}

/// Record the loader's own plugin entry, which carries the speed setting. Called once, before
/// any plugin loads, so the name is the loader's and plugins can read the setting in `start`.
pub(super) fn register(file: &str) {
    let (info, builtin) = crate::speed::builtin(file);
    let (added, schema, paths) = {
        let mut guard = super::state();
        let added = guard.add_builtin(info, builtin);
        let schema = added
            .ok()
            .and_then(|handle| crate::schema::plugin(&guard, handle, true));
        (added, schema, guard.paths.clone())
    };
    if let Some(schema) = schema {
        crate::schema::save_plugin(&paths, &schema);
    }
    match added {
        Ok(handle) => {
            if OWNER.set(handle).is_err() {
                log::error!("the loader's entry was registered twice");
            }
        }
        Err(e) => log::error!("cannot register the loader's own settings: {e:?}"),
    }
}

/// The loader's plugin handle, once [`register`] ran.
pub(super) fn owner() -> Option<PluginHandle> {
    OWNER.get().copied()
}

/// Run the clocks at the speed in `value`, the setting's canonical text, and tell the plugins
/// that asked. Called for every change of the setting, with no lock held.
pub(super) fn apply(value: &str) {
    let Ok(factor) = value.parse::<f64>() else {
        return;
    };
    let Some(owner) = owner() else {
        return;
    };
    {
        let mut guard = CHANGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changer = guard.get_or_insert_with(|| Changer {
            owner,
            factor: 1.0,
            installed: false,
            multimedia: false,
            counter_margin: 1,
        });
        // Exact comparison on purpose: the setting is canonical text, so an unchanged value
        // parses to the identical number.
        #[allow(clippy::float_cmp)]
        let unchanged = factor == changer.factor;
        if unchanged {
            return;
        }
        if !changer.installed {
            install(changer);
        }
        install_multimedia(changer);
        rebase(changer, factor);
        changer.factor = factor;
    }
    log::info!("game speed {factor}x");
    let targets = super::state().subscribers(PluginHandle::NONE, topics::SPEED);
    super::plugins::deliver_event(
        PluginHandle::NONE,
        &targets,
        topics::SPEED,
        &factor.to_le_bytes(),
    );
}

/// Apply the stored speed at startup. A saved speed other than 1 is easy to forget, so it is
/// said in the log.
pub(super) fn apply_stored() {
    let key = format!("{NAME}.{SPEED}");
    let Ok(value) = super::state().get_setting(None, &key) else {
        return;
    };
    if value != "1" {
        log::warn!("{key} is {value}: the game runs at {value}x speed (`{key} 1` resets it)");
    }
    apply(&value);
}

/// A speed hotkey or console command: change the setting, which applies it. Returns the line
/// to print.
pub(super) fn action(name: &str) -> Result<String, Status> {
    let action = Action::parse(name).ok_or(Status::NOT_FOUND)?;
    let key = format!("{NAME}.{SPEED}");
    let current = super::state()
        .get_setting(None, &key)
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(1.0);
    let value = action.apply(current).to_string();
    let changed = super::state().set_setting(None, &key, &value);
    let changed = changed.map_err(|(status, message)| {
        log::warn!("{name}: {message}");
        status
    })?;
    if let Some(persist) = changed.persist {
        persist.write();
    }
    super::dispatch::deliver(changed.notify.into_iter().collect());
    Ok(format!("{key} = {value}"))
}
