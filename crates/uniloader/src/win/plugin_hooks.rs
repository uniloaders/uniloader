//! Hooks a plugin registers through the loader, so the loader can undo them.
//!
//! A plugin that patches the game itself owns a change nobody else knows about: when it
//! stops, or faults, or is simply buggy, the patch stays and the next thing to touch that
//! address finds something it did not write. Registering the hook here instead means the
//! loader holds the original bytes, the original vtable pointer or the detour object, and
//! removes them in reverse order when the plugin goes away.
//!
//! Three kinds, because they are what hooking a game actually needs:
//!
//! - **patch**: bytes at an address, for example turning a call into `nop`s.
//! - **vtable**: one slot of a virtual table, for example a swapchain's `Present`.
//! - **detour**: an inline jump at a function's entry, with a trampoline that reaches the
//!   original; see [`super::detour`] for how the prologue is relocated.
//!
//! What this cannot do is make a bad patch safe. The loader checks that an address is
//! committed memory and that nothing else has hooked it, and nothing more: a plugin that
//! patches the wrong instruction still breaks the game, it just does not break the *next*
//! session too.

// FFI module: writes to the game's own memory on a plugin's behalf.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use uniloader_api::{PluginHandle, Status};
use windows::Win32::System::Diagnostics::Debug::FlushInstructionCache;
use windows::Win32::System::Memory::{
    VirtualProtect, VirtualQuery, MEMORY_BASIC_INFORMATION, MEM_COMMIT, PAGE_EXECUTE_READWRITE,
    PAGE_GUARD, PAGE_NOACCESS, PAGE_PROTECTION_FLAGS,
};
use windows::Win32::System::Threading::GetCurrentProcess;

/// The longest patch a plugin may install in one go.
///
/// Not a technical limit; a patch longer than this is a code cave being written through the
/// wrong door, and the loader would rather say so than restore a page of somebody's bytes.
pub(super) const MAX_PATCH: usize = 256;

/// What was installed, and what it takes to undo it.
enum Installed {
    /// Original bytes to write back at `address`.
    Patch { address: usize, original: Vec<u8> },
    /// Original pointer to write back into the slot at `slot`.
    Vtable { slot: usize, original: *mut c_void },
    /// The entry bytes to write back over the jump.
    Detour { detour: super::detour::Detour },
}

/// One registered hook.
struct Entry {
    id: u64,
    plugin: PluginHandle,
    name: String,
    note: String,
    installed: Installed,
}

// SAFETY: the addresses are plain integers and the pointers are a vtable entry and a
// trampoline page that is never freed. Nothing here is dereferenced except under the registry
// lock, from whichever thread holds it.
unsafe impl Send for Entry {}

static HOOKS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn registry() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    HOOKS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether `len` bytes from `address` are committed memory this process may touch.
///
/// The cheapest honest check there is: it rules out a null, a stale or a wildly wrong
/// address, which is what a typo in a plugin looks like.
fn is_writable_region(address: usize, len: usize) -> bool {
    // The whole range has to be inside this one region; a patch that spans two is a patch
    // that is about to run off the end of a section.
    len != 0 && accessible_from(address) >= len
}

/// How many bytes from `address` lie in the same committed, accessible region; 0 when
/// `address` itself is not accessible.
pub(super) fn accessible_from(address: usize) -> usize {
    if address == 0 {
        return 0;
    }
    let mut info = MEMORY_BASIC_INFORMATION::default();
    let size = core::mem::size_of::<MEMORY_BASIC_INFORMATION>();
    // SAFETY: `info` is writable and `size` describes it; querying any address is allowed.
    let written = unsafe { VirtualQuery(Some(address as *const c_void), &raw mut info, size) };
    if written != size || info.State != MEM_COMMIT {
        return 0;
    }
    let bad = PAGE_NOACCESS | PAGE_GUARD;
    if info.Protect & bad != PAGE_PROTECTION_FLAGS(0) {
        return 0;
    }
    (info.BaseAddress as usize + info.RegionSize).saturating_sub(address)
}

/// For a refused detour: who hooked `target` first, when its entry (`readable` bytes) begins
/// with a jump (an overlay's hook, as a rule), so the log names the other program.
fn hooked_by(target: usize, readable: usize) -> String {
    // SAFETY: `readable` bytes from `target` are committed and accessible (checked by the
    // caller); only the first instruction's bytes are read.
    let code = unsafe { core::slice::from_raw_parts(target as *const u8, readable.min(16)) };
    let Some(jump) = super::detour::leading_jump(code, target as u64) else {
        return String::new();
    };
    let mut to = usize::try_from(jump.to).unwrap_or(0);
    if jump.through {
        if accessible_from(to) < 8 {
            return String::new();
        }
        // SAFETY: eight accessible bytes, checked above; unaligned reads are fine on x86.
        to = unsafe { (to as *const usize).read_unaligned() };
    }
    let place = super::guard::module_of(to).map_or_else(
        || "memory of no module".to_owned(),
        |(m, off)| format!("{m}+{off:#x}"),
    );
    format!("; its entry already jumps to {to:#x} ({place}): another program hooked it first")
}

/// Write bytes into the game's code, returning what was there.
///
/// # Safety
/// `address` must be the start of `bytes.len()` writable bytes, which [`is_writable_region`]
/// establishes, and overwriting them must be what the caller intends: this is a plugin
/// patching the game, and nothing can check that the instruction boundary is right.
unsafe fn write_over(address: usize, bytes: &[u8]) -> Result<Vec<u8>, Status> {
    let mut previous = PAGE_PROTECTION_FLAGS(0);
    let target = address as *mut c_void;
    // SAFETY: the range was checked to be committed; making it writable is the point.
    unsafe {
        VirtualProtect(
            target,
            bytes.len(),
            PAGE_EXECUTE_READWRITE,
            &raw mut previous,
        )
    }
    .map_err(|e| {
        log::error!("VirtualProtect failed at {address:#X}: {e}");
        Status::ERROR
    })?;
    // SAFETY: the range is now writable and `bytes.len()` long in both directions.
    let original = unsafe {
        let original = core::slice::from_raw_parts(target.cast::<u8>(), bytes.len()).to_vec();
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
        original
    };
    // SAFETY: restoring the protection we just changed.
    let restored = unsafe { VirtualProtect(target, bytes.len(), previous, &raw mut previous) };
    if let Err(e) = restored {
        log::warn!("could not restore page protection at {address:#X}: {e}");
    }
    // SAFETY: the pseudo-handle needs no release; flushing a range we just wrote.
    let _ = unsafe { FlushInstructionCache(GetCurrentProcess(), Some(target), bytes.len()) };
    Ok(original)
}

impl Installed {
    /// The bytes this hook wrote over, as `(start, length)`.
    fn span(&self) -> (usize, usize) {
        match self {
            Installed::Patch { address, original } => (*address, original.len()),
            Installed::Vtable { slot, .. } => (*slot, core::mem::size_of::<usize>()),
            Installed::Detour { detour } => (detour.target, detour.original.len()),
        }
    }

    fn verb(&self) -> &'static str {
        match self {
            Installed::Patch { .. } => "patched",
            Installed::Vtable { .. } => "replaced",
            Installed::Detour { .. } => "detoured",
        }
    }
}

/// Whether `len` bytes from `address` would land on a hook already there.
///
/// Refused: any hook that starts at the same address, and any hook of *another* plugin whose
/// bytes overlap. Two plugins writing into each other's bytes can only be undone in the
/// reverse order, and they stop in whatever order they like: a patch over the tail of a
/// detour's jump, removed after the detour, writes half a jump to a stopped plugin back into
/// the game. One plugin's own overlapping hooks are undone in reverse order by
/// [`remove_all`], so they stay allowed.
///
/// Callers hold the registry from this check until the hook is recorded, so two plugins
/// hooking one address at once cannot both pass it.
fn already_hooked(
    hooks: &[Entry],
    plugin: PluginHandle,
    address: usize,
    len: usize,
) -> Option<&'static str> {
    let end = address.saturating_add(len);
    hooks.iter().find_map(|entry| {
        let (start, length) = entry.installed.span();
        let overlaps = start < end && address < start.saturating_add(length);
        (start == address || (overlaps && entry.plugin != plugin)).then(|| entry.installed.verb())
    })
}

/// Record an installed hook in the held registry and return its id.
fn record(hooks: &mut Vec<Entry>, plugin: PluginHandle, note: &str, installed: Installed) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let name = super::plugins::find(plugin).map_or_else(String::new, |p| p.name.clone());
    log::debug!("[{name}] hook {id} installed: {note}");
    hooks.push(Entry {
        id,
        plugin,
        name,
        note: note.to_owned(),
        installed,
    });
    id
}

/// Install a byte patch on a plugin's behalf.
pub(super) fn patch(
    plugin: PluginHandle,
    address: usize,
    bytes: &[u8],
    note: &str,
) -> Result<u64, Status> {
    if bytes.is_empty() || bytes.len() > MAX_PATCH {
        return Err(Status::INVALID_ARGUMENT);
    }
    if !is_writable_region(address, bytes.len()) {
        log::error!("refusing to patch {address:#X}: not committed, writable memory");
        return Err(Status::INVALID_ARGUMENT);
    }
    let mut hooks = registry();
    if let Some(how) = already_hooked(&hooks, plugin, address, bytes.len()) {
        log::error!("refusing to patch {address:#X}: already {how} by another hook");
        return Err(Status::ALREADY_EXISTS);
    }
    // SAFETY: the range is committed and inside one region, checked above.
    let original = unsafe { write_over(address, bytes) }?;
    Ok(record(
        &mut hooks,
        plugin,
        note,
        Installed::Patch { address, original },
    ))
}

/// Replace one virtual table slot on a plugin's behalf, returning the old pointer.
pub(super) fn vtable(
    plugin: PluginHandle,
    object: *mut c_void,
    index: u32,
    replacement: *mut c_void,
    note: &str,
) -> Result<(u64, *mut c_void), Status> {
    if object.is_null() || replacement.is_null() {
        return Err(Status::INVALID_ARGUMENT);
    }
    let pointer_size = core::mem::size_of::<*mut c_void>();
    if !is_writable_region(object as usize, pointer_size) {
        return Err(Status::INVALID_ARGUMENT);
    }
    // SAFETY: an object with a virtual table starts with a pointer to it, which the check
    // above established is readable.
    let table = unsafe { object.cast::<*mut c_void>().read() } as usize;
    // A garbage table pointer near the top of the address space must be refused, not
    // overflow (which panics across the C boundary in a debug build).
    let slot = (index as usize)
        .checked_mul(pointer_size)
        .and_then(|offset| table.checked_add(offset))
        .ok_or(Status::INVALID_ARGUMENT)?;
    if !is_writable_region(slot, pointer_size) {
        log::error!("refusing to hook vtable slot {index}: {slot:#X} is not writable");
        return Err(Status::INVALID_ARGUMENT);
    }
    let mut hooks = registry();
    if let Some(how) = already_hooked(&hooks, plugin, slot, pointer_size) {
        log::error!("refusing to hook vtable slot {index}: already {how}");
        return Err(Status::ALREADY_EXISTS);
    }
    let bytes = (replacement as usize).to_ne_bytes();
    // SAFETY: the slot is one committed, writable pointer, checked above.
    let original_bytes = unsafe { write_over(slot, &bytes) }?;
    let original =
        usize::from_ne_bytes(original_bytes.try_into().map_err(|_| Status::ERROR)?) as *mut c_void;
    let id = record(
        &mut hooks,
        plugin,
        note,
        Installed::Vtable { slot, original },
    );
    Ok((id, original))
}

/// Install an inline detour on a plugin's behalf, returning the trampoline.
pub(super) fn detour(
    plugin: PluginHandle,
    target: *mut c_void,
    replacement: *mut c_void,
    note: &str,
) -> Result<(u64, *mut c_void), Status> {
    detour_with(plugin, "", target, replacement, note, |_| {})
}

/// [`detour`] for the loader's own hooks: `owner` names the hook in the `hooks` listing when
/// no plugin DLL stands behind `plugin`, and `ready` receives the trampoline before the jump
/// is written (see [`super::detour::install`]).
pub(super) fn detour_with(
    plugin: PluginHandle,
    owner: &str,
    target: *mut c_void,
    replacement: *mut c_void,
    note: &str,
    ready: impl FnOnce(*mut c_void),
) -> Result<(u64, *mut c_void), Status> {
    if target.is_null() || replacement.is_null() {
        return Err(Status::INVALID_ARGUMENT);
    }
    // Five bytes is the shortest jump; the prologue has to be at least that long and
    // relocatable, which `retour` decides.
    let readable = accessible_from(target as usize);
    if readable < 5 {
        log::error!("refusing to detour {target:p}: not committed, executable memory");
        return Err(Status::INVALID_ARGUMENT);
    }
    let mut hooks = registry();
    // The shortest jump here; the stolen length is checked again once it is known.
    if let Some(how) = already_hooked(&hooks, plugin, target as usize, 5) {
        log::error!("refusing to detour {target:p}: already {how}");
        return Err(Status::ALREADY_EXISTS);
    }
    // SAFETY: `readable` bytes from the target are committed, checked above. Whether the
    // replacement has the target's signature is the plugin's responsibility, exactly as it
    // would be if the plugin installed the detour itself.
    let installed = unsafe {
        super::detour::install(
            target as usize,
            readable,
            replacement as usize,
            ready,
            |at, bytes| {
                if let Some(how) = already_hooked(&hooks, plugin, at, bytes.len()) {
                    log::error!("the jump at {at:#X} would cover bytes already {how}");
                    return Err(Status::ALREADY_EXISTS);
                }
                // `install` only ever asks for the target's own entry bytes, inside the
                // readable window; the enclosing `unsafe` block carries the obligation.
                write_over(at, bytes)
            },
        )
    };

    let installed = installed.map_err(|e| {
        let hooked = hooked_by(target as usize, readable);
        log::error!("cannot detour {target:p}: {e}{hooked}");
        Status::UNSUPPORTED
    })?;
    let trampoline = installed.trampoline;
    let id = record(
        &mut hooks,
        plugin,
        note,
        Installed::Detour { detour: installed },
    );
    if let Some(entry) = hooks.last_mut().filter(|e| e.name.is_empty()) {
        owner.clone_into(&mut entry.name);
    }
    Ok((id, trampoline))
}

/// Undo one hook. `plugin` must own it.
pub(super) fn remove(plugin: PluginHandle, id: u64) -> Status {
    // Undone with the registry held, so a hook installed at the same address meanwhile
    // cannot record bytes that are about to be overwritten.
    let mut hooks = registry();
    let Some(index) = hooks.iter().position(|e| e.id == id && e.plugin == plugin) else {
        return Status::NOT_FOUND;
    };
    let entry = hooks.remove(index);
    undo(&entry);
    Status::OK
}

/// Undo every hook a plugin still holds, most recent first.
///
/// Reverse order matters: two patches that overlap, or a vtable slot hooked twice, only come
/// back correctly if they are undone in the order opposite to installation.
pub(crate) fn remove_all(plugin: PluginHandle, name: &str) {
    // Held until the bytes are back, as in `remove`.
    let mut hooks = registry();
    let mut mine: Vec<Entry> = Vec::new();
    let mut index = hooks.len();
    while index > 0 {
        index -= 1;
        if hooks[index].plugin == plugin {
            let mut entry = hooks.remove(index);
            // Recorded during `start`, before the plugin had a published name.
            if entry.name.is_empty() {
                name.clone_into(&mut entry.name);
            }
            mine.push(entry);
        }
    }
    if mine.is_empty() {
        return;
    }
    log::info!("[{name}] removing {} hook(s)", mine.len());
    for entry in &mine {
        undo(entry);
    }
}

/// Put back whatever one entry replaced.
fn undo(entry: &Entry) {
    match &entry.installed {
        Installed::Patch { address, original } => {
            // SAFETY: the same range this entry patched, which was committed then and is
            // still mapped now: the game's own image is never unmapped while it runs.
            match unsafe { write_over(*address, original) } {
                Ok(_) => log::debug!(
                    "[{}] hook {} restored: {}",
                    entry.name,
                    entry.id,
                    entry.note
                ),
                Err(e) => log::error!(
                    "[{}] could not restore {} at {address:#X}: {e:?}",
                    entry.name,
                    entry.note
                ),
            }
        }
        Installed::Vtable { slot, original } => {
            let bytes = (*original as usize).to_ne_bytes();
            // SAFETY: the same slot this entry replaced.
            if let Err(e) = unsafe { write_over(*slot, &bytes) } {
                log::error!(
                    "[{}] could not restore the vtable slot at {slot:#X}: {e:?}",
                    entry.name
                );
            }
        }
        Installed::Detour { detour } => {
            // SAFETY: the same entry bytes this detour replaced, in the game's own image,
            // which is never unmapped while it runs.
            if let Err(e) = unsafe { write_over(detour.target, &detour.original) } {
                log::error!(
                    "[{}] could not remove the detour at {:#X}: {e:?}",
                    entry.name,
                    detour.target
                );
            }
        }
    }
}

/// Lines for the console's `hooks` command: one per installed hook.
pub(crate) fn console_lines() -> Vec<String> {
    registry()
        .iter()
        .map(|entry| {
            let what = match entry.installed {
                Installed::Patch { address, .. } => format!("patch at {address:#X}"),
                Installed::Vtable { slot, .. } => format!("vtable slot at {slot:#X}"),
                Installed::Detour { ref detour } => format!("detour at {:#X}", detour.target),
            };
            // A hook installed during `start` is recorded before the plugin is published, so
            // its name may only be known now.
            let name = if entry.name.is_empty() {
                super::plugins::find(entry.plugin).map_or_else(String::new, |p| p.name.clone())
            } else {
                entry.name.clone()
            };
            format!("{} [{name}] {what}: {}", entry.id, entry.note)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch_at(plugin: u32, address: usize, len: usize) -> Entry {
        Entry {
            id: 0,
            plugin: PluginHandle(plugin),
            name: String::new(),
            note: String::new(),
            installed: Installed::Patch {
                address,
                original: vec![0; len],
            },
        }
    }

    #[test]
    fn another_plugins_bytes_are_off_limits_and_ones_own_overlap() {
        let hooks = [patch_at(1, 0x1000, 6)];
        // The same start is refused for anyone.
        assert_eq!(
            already_hooked(&hooks, PluginHandle(1), 0x1000, 2),
            Some("patched")
        );
        assert_eq!(
            already_hooked(&hooks, PluginHandle(2), 0x1000, 2),
            Some("patched")
        );
        // Bytes inside the patch: refused for another plugin, allowed for the owner.
        assert_eq!(
            already_hooked(&hooks, PluginHandle(2), 0x1003, 4),
            Some("patched")
        );
        assert_eq!(already_hooked(&hooks, PluginHandle(1), 0x1003, 4), None);
        // A range that ends where the patch starts, or starts where it ends, is clear.
        assert_eq!(already_hooked(&hooks, PluginHandle(2), 0x0FFB, 5), None);
        assert_eq!(already_hooked(&hooks, PluginHandle(2), 0x1006, 5), None);
        // One that runs into it from below is not.
        assert_eq!(
            already_hooked(&hooks, PluginHandle(2), 0x0FFC, 5),
            Some("patched")
        );
    }
}
