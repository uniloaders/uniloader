//! What is open and who has the input: a handful of atomics every thread may read.
//!
//! Hotkeys arrive on the loader's hotkey thread, service calls on any thread, and drawing
//! happens on the render thread; none of them shares more than these flags.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use uniloader_overlay::{UiLevel, UiNotice};

use crate::{notices, registry};

/// Whether the overlay currently has the keyboard and mouse.
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// Whether the overlay is willing to hold the input while windows are open. Cleared by the
/// `overlay.mouse` hotkey so a panel can be watched while playing.
static GRABBED: AtomicBool = AtomicBool::new(true);
/// Whether there is anything at all to draw.
static VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the console window is open.
static CONSOLE_OPEN: AtomicBool = AtomicBool::new(false);
/// Whether the settings editor is open.
static EDITOR_OPEN: AtomicBool = AtomicBool::new(false);
/// How many plugin panels are open.
static PANELS_OPEN: AtomicUsize = AtomicUsize::new(0);
/// Set when the virtual pointer should jump to the middle of the screen.
static CENTRE: AtomicBool = AtomicBool::new(false);

/// Whether the overlay has the keyboard and mouse.
pub(crate) fn is_capturing() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

/// Whether anything is on screen.
pub(crate) fn is_visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

/// Whether the console is open.
pub(crate) fn console_open() -> bool {
    CONSOLE_OPEN.load(Ordering::Relaxed)
}

/// Whether the settings editor is open.
pub(crate) fn editor_open() -> bool {
    EDITOR_OPEN.load(Ordering::Relaxed)
}

/// Whether any plugin panel is open.
pub(crate) fn panels_open() -> bool {
    PANELS_OPEN.load(Ordering::Relaxed) > 0
}

/// Close the console, from its own window.
pub(crate) fn close_console() {
    CONSOLE_OPEN.store(false, Ordering::Relaxed);
    refresh();
}

/// Close the settings editor, from its own window.
pub(crate) fn close_editor() {
    EDITOR_OPEN.store(false, Ordering::Relaxed);
    refresh();
}

/// Drops the input collected so far: a window opened or closed. The key that opened it also
/// produced a character, which belongs to the game (the window procedure drops that one).
fn flush_input() {
    #[cfg(windows)]
    crate::win::flush_input();
}

/// Take the pending recentre request.
pub(crate) fn take_centre() -> bool {
    CENTRE.swap(false, Ordering::Relaxed)
}

/// Open or close the console.
pub(crate) fn toggle_console() {
    let open = !CONSOLE_OPEN.load(Ordering::Relaxed);
    CONSOLE_OPEN.store(open, Ordering::Relaxed);
    log::debug!("console {}", if open { "opened" } else { "closed" });
    if open {
        // A console nobody can type into is not a console.
        GRABBED.store(true, Ordering::Relaxed);
    }
    flush_input();
    refresh();
}

/// Open or close the settings editor.
pub(crate) fn toggle_editor() {
    let open = !EDITOR_OPEN.load(Ordering::Relaxed);
    EDITOR_OPEN.store(open, Ordering::Relaxed);
    log::debug!("settings editor {}", if open { "opened" } else { "closed" });
    if open {
        GRABBED.store(true, Ordering::Relaxed);
    }
    flush_input();
    refresh();
}

/// Take the mouse and keyboard, or give them back to the game with the windows left up.
pub(crate) fn toggle_grab() {
    let grabbed = !GRABBED.load(Ordering::Relaxed);
    GRABBED.store(grabbed, Ordering::Relaxed);
    if grabbed {
        CENTRE.store(true, Ordering::Relaxed);
    }
    log::info!(
        "overlay {} the mouse",
        if grabbed { "took" } else { "released" }
    );
    // Said out loud: the overlay looks the same either way, and a panel that stopped
    // reacting to clicks is otherwise indistinguishable from one that hung.
    if VISIBLE.load(Ordering::Relaxed) {
        let text = if grabbed {
            "The overlay has the mouse and keyboard."
        } else {
            "The game has the mouse and keyboard. Panels stay on screen."
        };
        notices::passing(None, UiNotice::TOAST, UiLevel::INFO, "Input", text, 0.0);
    }
    recount();
}

/// Recount what is open after a window opened or closed.
pub(crate) fn refresh() {
    let open = registry::open_count();
    let was = PANELS_OPEN.swap(open, Ordering::Relaxed);
    let nothing_open = open == 0 && !console_open() && !editor_open();
    if nothing_open {
        // So the hotkey never has to be pressed twice to get a working panel next time.
        GRABBED.store(true, Ordering::Relaxed);
    } else if open > was && !GRABBED.swap(true, Ordering::Relaxed) {
        // A window that has just appeared is one somebody wants to use.
        CENTRE.store(true, Ordering::Relaxed);
    }
    if open > was {
        flush_input();
    }
    recount();
}

/// Recompute what is on screen and what has the input, and tell the loader who holds the
/// keyboard. Only windows and a modal dialog capture; a toast merely draws.
pub(crate) fn recount() {
    let windows = panels_open() || console_open() || editor_open();
    let grabbed = GRABBED.load(Ordering::Relaxed);
    let capturing = (windows && grabbed) || notices::modal();
    let was = CAPTURING.swap(capturing, Ordering::Relaxed);
    VISIBLE.store(windows || notices::any(), Ordering::Relaxed);
    if was != capturing {
        if let Some(host) = crate::host() {
            host.capture_input(capturing);
        }
    }
}
