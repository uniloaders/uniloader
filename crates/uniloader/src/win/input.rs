//! Keyboard state, focus checks and the thread that polls hotkeys.
//!
//! From the DayZ loader's `win/input.rs` (`eaebd9b`). There the hotkeys were polled once per
//! `Present`; a generic loader has no frame to hang off, so a thread polls instead.

// FFI module: thin wrappers over user32 queries.
#![allow(unsafe_code)]

use std::time::Duration;

use uniloader_core::hotkeys::KeyState;
use uniloader_core::keys::Modifiers;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

/// How often hotkeys are polled. Fast enough that a tap is never missed, slow enough to be
/// invisible in a profile.
const POLL: Duration = Duration::from_millis(16);

/// Live key state read through `GetAsyncKeyState`.
pub(crate) struct AsyncKeys;

fn down(vk: u16) -> bool {
    // SAFETY: GetAsyncKeyState has no preconditions.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    // The high bit reports "currently down"; the low bit ("pressed since last call") is
    // shared between all callers in the process and therefore useless here.
    state < 0
}

impl KeyState for AsyncKeys {
    fn is_down(&self, vk: u16) -> bool {
        down(vk)
    }

    fn vk_for_scancode(&self, scancode: u16) -> Option<u16> {
        // MAPVK_VSC_TO_VK_EX asks the *current* layout where that physical key is: scan code
        // 0x29 is VK_OEM_3 on a US layout and VK_OEM_5 on a German one.
        //
        // SAFETY: MapVirtualKeyW has no preconditions.
        let vk = unsafe { MapVirtualKeyW(u32::from(scancode), MAPVK_VSC_TO_VK_EX) };
        u16::try_from(vk).ok().filter(|vk| *vk != 0)
    }

    fn modifiers(&self) -> Modifiers {
        Modifiers {
            ctrl: down(VK_CONTROL.0),
            alt: down(VK_MENU.0),
            shift: down(VK_SHIFT.0),
        }
    }
}

/// Whether the foreground window belongs to this process. Hotkeys only fire while it does.
///
/// By process rather than by window: the loader does not know which of the game's windows is
/// "the" game window, and a launcher, a splash screen and the game proper all count.
pub(crate) fn game_has_focus() -> bool {
    // SAFETY: both calls have no preconditions; a null window yields process id 0.
    let pid = unsafe {
        let window = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&raw mut pid));
        pid
    };
    pid == std::process::id()
}

/// Start the hotkey thread. It runs for the life of the process.
pub(crate) fn start_polling() {
    let spawned = std::thread::Builder::new()
        .name("uniloader-hotkeys".to_owned())
        .spawn(|| loop {
            super::tick();
            std::thread::sleep(POLL);
        });
    if let Err(e) = spawned {
        log::error!("hotkeys disabled, cannot start their thread: {e}");
    }
}
