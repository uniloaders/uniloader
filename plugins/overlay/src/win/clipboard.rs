//! The Windows clipboard, for the console.
//!
//! egui asks its host to copy and paste through `PlatformOutput`, which a normal backend such
//! as `eframe` implements. The overlay has no backend — it draws inside the game's `Present`
//! — so without this a selection in the console could be highlighted and copied to nowhere.
//!
//! Under Proton the Wine clipboard is bridged to the desktop's, so a line copied out of the
//! in-game console can be pasted into a bug report on the host. That is most of the point of
//! having it.

// FFI module: the clipboard is a global, handle-based API.
#![allow(unsafe_code)]

use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_UNICODETEXT;

/// Holds the clipboard open for one operation and closes it however the body ends.
struct Open;

impl Open {
    /// Open the clipboard, or report that another process has it.
    fn new() -> Option<Open> {
        // SAFETY: no window is passed, which makes this process the owner for the duration;
        // the call fails rather than blocking when someone else holds the clipboard.
        unsafe { OpenClipboard(None) }
            .inspect_err(|e| log::debug!("clipboard busy: {e}"))
            .ok()
            .map(|()| Open)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: the clipboard was opened by this process; closing it cannot fail in a way
        // that matters here.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Put `text` on the clipboard as UTF-16.
pub(crate) fn set_text(text: &str) {
    let wide: Vec<u16> = text.encode_utf16().chain(core::iter::once(0)).collect();
    let bytes = wide.len() * core::mem::size_of::<u16>();
    let Some(_open) = Open::new() else { return };
    // SAFETY: emptying the clipboard is valid while this process holds it open, and frees
    // whatever was there, which is what the API requires before setting new data.
    if let Err(e) = unsafe { EmptyClipboard() } {
        log::warn!("could not clear the clipboard: {e}");
        return;
    }
    // SAFETY: a moveable allocation of a known, non-zero size.
    let handle = match unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) } {
        Ok(handle) => handle,
        Err(e) => {
            log::warn!("could not allocate for the clipboard: {e}");
            return;
        }
    };
    // SAFETY: the handle came from `GlobalAlloc` and is unlocked, so locking it yields a
    // pointer to at least `bytes` writable bytes.
    let destination = unsafe { GlobalLock(handle) };
    if destination.is_null() {
        log::warn!("could not lock the clipboard allocation");
        return;
    }
    // SAFETY: `destination` points at `bytes` writable bytes, which is exactly the size of
    // `wide`, and the two allocations cannot overlap.
    unsafe {
        core::ptr::copy_nonoverlapping(wide.as_ptr(), destination.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(handle);
    }
    // SAFETY: the clipboard is open and empty, and the handle is a `GMEM_MOVEABLE`
    // allocation of UTF-16 text, which is what `CF_UNICODETEXT` means. On success the
    // clipboard owns the allocation and it must not be freed here.
    if let Err(e) = unsafe { SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(handle.0))) }
    {
        log::warn!("could not put text on the clipboard: {e}");
    }
}

/// Longest paste accepted, in UTF-16 units.
///
/// Only bounds the damage if the clipboard ever hands over a string that is not terminated:
/// the walk below would otherwise read until it left the allocation.
const MAX_PASTE: usize = 1 << 20;

/// The clipboard's text, or `None` when it holds something else.
pub(crate) fn text() -> Option<String> {
    let format = u32::from(CF_UNICODETEXT.0);
    // SAFETY: a plain query taking no handles.
    if unsafe { IsClipboardFormatAvailable(format) }.is_err() {
        return None;
    }
    let _open = Open::new()?;
    // SAFETY: the clipboard is open and the format was just reported as available. The
    // returned handle belongs to the clipboard and is only read, never freed.
    let handle = unsafe { GetClipboardData(format) }.ok()?;
    // SAFETY: the handle is a moveable allocation holding a NUL terminated UTF-16 string.
    let locked = unsafe { GlobalLock(HGLOBAL(handle.0)) };
    if locked.is_null() {
        return None;
    }
    let mut units = Vec::new();
    let mut cursor = locked.cast::<u16>();
    // SAFETY: the allocation holds a NUL terminated UTF-16 string, so the walk stops inside
    // it, and `MAX_PASTE` stops it even if it does not; unlocking matches the lock above.
    unsafe {
        while units.len() < MAX_PASTE {
            let unit = *cursor;
            if unit == 0 {
                break;
            }
            units.push(unit);
            cursor = cursor.add(1);
        }
        let _ = GlobalUnlock(HGLOBAL(handle.0));
    }
    Some(String::from_utf16_lossy(&units))
}
