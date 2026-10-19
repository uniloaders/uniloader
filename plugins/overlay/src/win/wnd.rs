//! The window subclass that feeds the overlay its input.
//!
//! A UI needs window messages — text, the wheel, press and release in order — so the game's
//! output window is subclassed, the messages egui understands are translated, and while the
//! overlay is capturing they are kept from the game, so typing a console command does not
//! also walk the player forward. Every key press is also reported to the loader, which is
//! how hotkeys bound by scan code fire at all.
//!
//! The pointer is tracked twice on purpose. `WM_MOUSEMOVE` gives an absolute position, and
//! raw input gives deltas, which is all there is once the game has captured and recentred the
//! cursor. While the game holds the mouse it warps the cursor to the middle every frame, and
//! each warp arrives as a `WM_MOUSEMOVE` at the centre; so the first relative packet latches
//! [`Pending::relative`] and absolute positions are ignored from then on.

// FFI module: a window procedure, raw input and `SetWindowLongPtrW`.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicIsize, AtomicPtr, Ordering};
use std::sync::Mutex;

use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use uniloader_sdk::KeyModifiers;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTHEADER, RID_INPUT,
    RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, GetWindowLongPtrW, IsWindow, SetWindowLongPtrW, GWLP_WNDPROC, WM_CHAR,
    WM_INPUT, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETFOCUS,
    WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use super::clipboard;
use crate::shared;

/// The window procedure that was there before. Zero means not subclassed.
static PREVIOUS: AtomicIsize = AtomicIsize::new(0);
/// The subclassed window.
static WINDOW: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());

/// What the window procedure collected since the last frame.
struct Pending {
    events: Vec<Event>,
    modifiers: Modifiers,
    pointer: Pos2,
    focused: bool,
    screen: Vec2,
    /// The most recent key press as `(virtual key, scan code)`, for the hotkey recorder.
    last_key: Option<(u16, u16)>,
    /// Set once a relative mouse packet has been seen; see the module comment.
    relative: bool,
}

static PENDING: Mutex<Pending> = Mutex::new(Pending {
    events: Vec::new(),
    modifiers: Modifiers::NONE,
    pointer: Pos2::ZERO,
    focused: true,
    screen: Vec2::new(1920.0, 1080.0),
    last_key: None,
    relative: false,
});

fn pending() -> std::sync::MutexGuard<'static, Pending> {
    PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Subclass `hwnd` once. Later calls do nothing, so a recreated swapchain is harmless.
pub(crate) fn subclass(hwnd: *mut c_void) {
    if hwnd.is_null() || PREVIOUS.load(Ordering::Acquire) != 0 {
        return;
    }
    let replacement = procedure as *const () as isize;
    // SAFETY: `hwnd` is the swapchain's output window, and `procedure` has the signature
    // `GWLP_WNDPROC` requires.
    let previous = unsafe { SetWindowLongPtrW(HWND(hwnd), GWLP_WNDPROC, replacement) };
    if previous == 0 {
        log::warn!("could not subclass the game window; the overlay will not take input");
        return;
    }
    PREVIOUS.store(previous, Ordering::Release);
    WINDOW.store(hwnd, Ordering::Release);
    log::info!("subclassed the game window for overlay input");
}

/// Put the original window procedure back, if ours is still the one installed. One that
/// someone subclassed on top of us is left alone: unhooking under it would cut it off.
pub(crate) fn unsubclass() {
    let hwnd = WINDOW.swap(core::ptr::null_mut(), Ordering::AcqRel);
    let previous = PREVIOUS.load(Ordering::Acquire);
    if hwnd.is_null() || previous == 0 {
        return;
    }
    // SAFETY: a query on a handle that may be stale, which `IsWindow` exists to answer.
    if !unsafe { IsWindow(Some(HWND(hwnd))) }.as_bool() {
        // The game destroyed its window first; there is nothing left to restore.
        PREVIOUS.store(0, Ordering::Release);
        return;
    }
    // SAFETY: `hwnd` is the window subclassed above; reading its procedure has no effects.
    let current = unsafe { GetWindowLongPtrW(HWND(hwnd), GWLP_WNDPROC) };
    if current == procedure as *const () as isize {
        // SAFETY: restoring the procedure `SetWindowLongPtrW` handed us for this window.
        unsafe { SetWindowLongPtrW(HWND(hwnd), GWLP_WNDPROC, previous) };
        PREVIOUS.store(0, Ordering::Release);
        log::info!("restored the game window procedure");
    } else {
        log::warn!("the game window was subclassed again after the overlay; leaving it hooked");
    }
}

/// Tell the input layer how big the backbuffer is, so the virtual pointer stays on screen.
pub(crate) fn set_screen(width: f32, height: f32) {
    pending().screen = Vec2::new(width, height);
}

/// Take everything collected since the last call as one frame of egui input.
pub(crate) fn take_input(screen: Rect, time: f64) -> RawInput {
    if shared::take_flush() {
        pending().events.clear();
    }
    if shared::take_centre() {
        centre_pointer();
    }
    let mut guard = pending();
    let events = core::mem::take(&mut guard.events);
    RawInput {
        screen_rect: Some(screen),
        time: Some(time),
        modifiers: guard.modifiers,
        events,
        focused: guard.focused,
        ..RawInput::default()
    }
}

/// The last key pressed while the overlay had the input, taken out on read.
pub(crate) fn take_last_key() -> Option<(u16, u16, uniloader_core::keys::Modifiers)> {
    let mut guard = pending();
    let modifiers = uniloader_core::keys::Modifiers {
        ctrl: guard.modifiers.ctrl,
        alt: guard.modifiers.alt,
        shift: guard.modifiers.shift,
    };
    guard
        .last_key
        .take()
        .map(|(vk, scancode)| (vk, scancode, modifiers))
}

/// Post `input` to the game window as the messages real input would make (`overlay.input`).
pub(crate) fn inject(input: &crate::inject::Input) -> Result<(), String> {
    use crate::inject::Input;
    use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    let hwnd = WINDOW.load(Ordering::Acquire);
    if hwnd.is_null() {
        return Err("no game window yet".to_owned());
    }
    let hwnd = Some(HWND(hwnd));
    let post = |msg: u32, wparam: usize, lparam: isize| {
        // SAFETY: posting to a window handle has no memory effects; a stale handle fails.
        unsafe { PostMessageW(hwnd, msg, WPARAM(wparam), LPARAM(lparam)) }
            .map_err(|e| format!("posting to the game window: {e}"))
    };
    match input {
        Input::Type(text) => {
            for unit in text.encode_utf16() {
                post(WM_CHAR, usize::from(unit), 1)?;
            }
        }
        Input::Key(vk) => {
            // SAFETY: a table lookup.
            let scancode = unsafe { MapVirtualKeyW(u32::from(*vk), MAPVK_VK_TO_VSC) };
            #[allow(clippy::cast_possible_wrap, reason = "a bit field, not a number")]
            let down = (1 | (scancode << 16)) as isize;
            // Key up: previous state down (bit 30) and transition (bit 31).
            #[allow(clippy::cast_possible_wrap, reason = "a bit field, not a number")]
            let up = (1 | (scancode << 16) | 0xC000_0000) as isize;
            post(WM_KEYDOWN, usize::from(*vk), down)?;
            post(WM_KEYUP, usize::from(*vk), up)?;
        }
        Input::Click(x, y) => {
            // Placed directly: once relative mouse input is latched, absolute moves are
            // ignored (see the module comment).
            {
                let mut guard = pending();
                let screen = guard.screen;
                let pos = Pos2::new(*x, *y).clamp(Pos2::ZERO, Pos2::new(screen.x, screen.y));
                guard.pointer = pos;
                push(&mut guard.events, Event::PointerMoved(pos));
            }
            post(WM_LBUTTONDOWN, 1, 0)?;
            post(WM_LBUTTONUP, 0, 0)?;
        }
    }
    Ok(())
}

/// Where the overlay should draw its cursor.
pub(crate) fn pointer() -> Pos2 {
    pending().pointer
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // After `stop` (when another subclasser kept us in the chain) only forwarding is left.
    if crate::host().is_some() {
        let capturing = shared::is_capturing();
        let input = record(msg, wparam, lparam, capturing);
        // A release whose press reached the game goes to the game too, even when capture
        // began in between: swallowing it would leave the key held down there.
        let owed = forwarded::release_owed(msg, wparam);
        if capturing && input && !owed {
            // Swallowed: the overlay has the keyboard and mouse while it is open.
            return LRESULT(0);
        }
        if !capturing {
            forwarded::note(msg, wparam);
        }
    }
    let previous = PREVIOUS.load(Ordering::Acquire);
    if previous == 0 {
        // Unhooked between the check above and now; nothing to forward to but the default.
        // SAFETY: forwarding our own arguments to the default procedure.
        return unsafe {
            windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, msg, wparam, lparam)
        };
    }
    // SAFETY: `previous` is the procedure `SetWindowLongPtrW` returned for this window, which
    // is what `CallWindowProcW` expects; the arguments are the ones we were given.
    unsafe {
        CallWindowProcW(
            Some(core::mem::transmute::<
                isize,
                unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            >(previous)),
            hwnd,
            msg,
            wparam,
            lparam,
        )
    }
}

/// Which keys and mouse buttons are down in the game: pressed while the overlay did not
/// capture, so their releases are owed to the game whatever the overlay does by then.
mod forwarded {
    use std::sync::atomic::{AtomicU64, Ordering};

    use windows::Win32::Foundation::WPARAM;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{
        WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
        WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    /// One bit per virtual-key code (mouse buttons have theirs too).
    static DOWN: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

    /// The key a press or release message is about, and whether it is a press.
    fn key(msg: u32, wparam: WPARAM) -> Option<(u8, bool)> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "virtual-key codes are a byte"
        )]
        let vk = wparam.0 as u8;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the button codes are 1, 2 and 4"
        )]
        let button = |code: u16| code as u8;
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => Some((vk, true)),
            WM_KEYUP | WM_SYSKEYUP => Some((vk, false)),
            WM_LBUTTONDOWN => Some((button(VK_LBUTTON.0), true)),
            WM_LBUTTONUP => Some((button(VK_LBUTTON.0), false)),
            WM_RBUTTONDOWN => Some((button(VK_RBUTTON.0), true)),
            WM_RBUTTONUP => Some((button(VK_RBUTTON.0), false)),
            WM_MBUTTONDOWN => Some((button(VK_MBUTTON.0), true)),
            WM_MBUTTONUP => Some((button(VK_MBUTTON.0), false)),
            _ => None,
        }
    }

    fn word_and_bit(vk: u8) -> (&'static AtomicU64, u64) {
        (&DOWN[usize::from(vk / 64)], 1 << (vk % 64))
    }

    /// Record a message the game is about to receive.
    pub(super) fn note(msg: u32, wparam: WPARAM) {
        if let Some((vk, pressed)) = key(msg, wparam) {
            let (word, bit) = word_and_bit(vk);
            if pressed {
                word.fetch_or(bit, Ordering::Relaxed);
            } else {
                word.fetch_and(!bit, Ordering::Relaxed);
            }
        }
    }

    /// Whether this is the release of a key the game saw go down; clears it when so.
    pub(super) fn release_owed(msg: u32, wparam: WPARAM) -> bool {
        match key(msg, wparam) {
            Some((vk, false)) => {
                let (word, bit) = word_and_bit(vk);
                word.fetch_and(!bit, Ordering::Relaxed) & bit != 0
            }
            _ => false,
        }
    }
}

/// Queue an egui event, only while the overlay is shown: nothing drains the queue while it is
/// hidden, and a 1000 Hz mouse would grow it without end.
fn push(events: &mut Vec<Event>, event: Event) {
    if shared::is_visible() {
        events.push(event);
    }
}

/// Translate one message. Returns whether it was an input message, which decides whether a
/// capturing overlay swallows it.
fn record(msg: u32, wparam: WPARAM, lparam: LPARAM, capturing: bool) -> bool {
    match msg {
        WM_MOUSEMOVE => {
            let mut guard = pending();
            if !guard.relative {
                guard.pointer = client_position(lparam);
                let pos = guard.pointer;
                push(&mut guard.events, Event::PointerMoved(pos));
            }
            true
        }
        WM_INPUT => {
            // Only while capturing: parsing every mouse move the game makes costs a syscall.
            if capturing {
                if let Some(delta) = raw_mouse_delta(lparam) {
                    let mut guard = pending();
                    guard.relative = true;
                    let screen = guard.screen;
                    let moved =
                        (guard.pointer + delta).clamp(Pos2::ZERO, Pos2::new(screen.x, screen.y));
                    guard.pointer = moved;
                    push(&mut guard.events, Event::PointerMoved(moved));
                }
            }
            true
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
        | WM_MBUTTONUP => {
            let (button, pressed) = match msg {
                WM_LBUTTONDOWN => (PointerButton::Primary, true),
                WM_LBUTTONUP => (PointerButton::Primary, false),
                WM_RBUTTONDOWN => (PointerButton::Secondary, true),
                WM_RBUTTONUP => (PointerButton::Secondary, false),
                WM_MBUTTONDOWN => (PointerButton::Middle, true),
                _ => (PointerButton::Middle, false),
            };
            let mut guard = pending();
            let (pos, modifiers) = (guard.pointer, guard.modifiers);
            push(
                &mut guard.events,
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                },
            );
            true
        }
        WM_MOUSEWHEEL => {
            // A signed 16-bit value in the high word of wparam.
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            let ticks = f32::from((wparam.0 >> 16) as i16) / 120.0;
            let mut guard = pending();
            let modifiers = guard.modifiers;
            push(
                &mut guard.events,
                Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: Vec2::new(0.0, ticks),
                    modifiers,
                    phase: egui::TouchPhase::Move,
                },
            );
            true
        }
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            keyboard(msg, wparam, lparam);
            true
        }
        WM_CHAR => {
            #[allow(clippy::cast_possible_truncation)]
            let unit = wparam.0 as u32;
            // Control characters are covered by the key events; a lone surrogate is dropped.
            if let Some(c) = char::from_u32(unit).filter(|c| !c.is_control()) {
                push(&mut pending().events, Event::Text(c.to_string()));
            }
            true
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            let focused = msg == WM_SETFOCUS;
            let mut guard = pending();
            guard.focused = focused;
            push(&mut guard.events, Event::WindowFocused(focused));
            if !focused {
                // Modifier releases are missed while another window has focus.
                guard.modifiers = Modifiers::NONE;
            }
            false
        }
        _ => false,
    }
}

/// Record one key message: modifier state, the recorder, the loader's scan-code hotkeys and
/// the egui event.
fn keyboard(msg: u32, wparam: WPARAM, lparam: LPARAM) {
    let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
    #[allow(clippy::cast_possible_truncation)]
    let vk = wparam.0 as u16;
    // Bits 16..24 of `lparam` are the scan code, bit 24 marks the extended set, bit 30 says
    // the key was already down (auto-repeat).
    #[allow(clippy::cast_sign_loss, reason = "a bit field, not a number")]
    let bits = lparam.0 as u64;
    #[allow(clippy::cast_possible_truncation)]
    let mut scancode = ((bits >> 16) & 0xFF) as u16;
    if bits & (1 << 24) != 0 {
        scancode |= 0xE000;
    }
    let repeat = bits & 0x4000_0000 != 0;
    let modifiers = {
        let mut guard = pending();
        update_modifiers(&mut guard.modifiers, vk, pressed);
        if pressed {
            guard.last_key = Some((vk, scancode));
        }
        guard.modifiers
    };
    if pressed && !repeat {
        // No lock held: the loader runs the hotkey's plugin before this returns.
        if let Some(host) = crate::host() {
            host.hotkey_press(scancode, key_modifiers(modifiers));
        }
    }
    let mut guard = pending();
    // egui expects the pasted text as an event rather than reading the clipboard itself.
    if pressed && modifiers.ctrl && vk == u16::from(b'V') {
        if let Some(text) = clipboard::text() {
            push(&mut guard.events, Event::Paste(text));
        }
    }
    if let Some(key) = key_of(vk) {
        push(
            &mut guard.events,
            Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            },
        );
    }
}

fn key_modifiers(modifiers: Modifiers) -> KeyModifiers {
    let mut bits = 0;
    if modifiers.ctrl {
        bits |= KeyModifiers::CTRL;
    }
    if modifiers.alt {
        bits |= KeyModifiers::ALT;
    }
    if modifiers.shift {
        bits |= KeyModifiers::SHIFT;
    }
    KeyModifiers(bits)
}

/// Mouse position from a message's `lparam`, in client coordinates: two signed 16-bit halves.
fn client_position(lparam: LPARAM) -> Pos2 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let packed = lparam.0 as u32;
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let (x, y) = ((packed & 0xffff) as i16, (packed >> 16) as i16);
    Pos2::new(f32::from(x), f32::from(y))
}

/// The relative movement in a `WM_INPUT` message, when it is a mouse one.
fn raw_mouse_delta(lparam: LPARAM) -> Option<Vec2> {
    let mut data = RAWINPUT::default();
    let mut size = u32::try_from(core::mem::size_of::<RAWINPUT>()).ok()?;
    let header = u32::try_from(core::mem::size_of::<RAWINPUTHEADER>()).ok()?;
    // SAFETY: `lparam` is the RAWINPUT handle the message carries, and `data` is a correctly
    // sized buffer; `GetRawInputData` writes at most `size` bytes.
    let read = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut c_void),
            RID_INPUT,
            Some((&raw mut data).cast::<c_void>()),
            &raw mut size,
            header,
        )
    };
    if read == u32::MAX || data.header.dwType != RIM_TYPEMOUSE.0 {
        return None;
    }
    // SAFETY: the type field says this union holds the mouse variant.
    let mouse = unsafe { data.data.mouse };
    // Tablets and remote sessions send absolute positions through raw input; those are left
    // to `WM_MOUSEMOVE`.
    if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0 {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some(Vec2::new(mouse.lLastX as f32, mouse.lLastY as f32))
}

/// Put the virtual pointer in the middle of the screen, where a newly opened window wants it.
fn centre_pointer() {
    let mut guard = pending();
    let screen = guard.screen;
    guard.pointer = Pos2::new(screen.x / 2.0, screen.y / 2.0);
    let pos = guard.pointer;
    push(&mut guard.events, Event::PointerMoved(pos));
}

fn update_modifiers(modifiers: &mut Modifiers, vk: u16, pressed: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_MENU, VK_SHIFT};
    if vk == VK_SHIFT.0 {
        modifiers.shift = pressed;
    } else if vk == VK_CONTROL.0 {
        modifiers.ctrl = pressed;
        modifiers.command = pressed;
    } else if vk == VK_MENU.0 {
        modifiers.alt = pressed;
    }
}

/// Virtual key to egui key. Unmapped keys produce no key event; their characters still
/// arrive as `WM_CHAR`.
fn key_of(vk: u16) -> Option<Key> {
    use windows::Win32::UI::Input::KeyboardAndMouse as vk_const;
    const NAMED: [(u16, Key); 15] = [
        (vk_const::VK_LEFT.0, Key::ArrowLeft),
        (vk_const::VK_RIGHT.0, Key::ArrowRight),
        (vk_const::VK_UP.0, Key::ArrowUp),
        (vk_const::VK_DOWN.0, Key::ArrowDown),
        (vk_const::VK_ESCAPE.0, Key::Escape),
        (vk_const::VK_TAB.0, Key::Tab),
        (vk_const::VK_BACK.0, Key::Backspace),
        (vk_const::VK_RETURN.0, Key::Enter),
        (vk_const::VK_SPACE.0, Key::Space),
        (vk_const::VK_INSERT.0, Key::Insert),
        (vk_const::VK_DELETE.0, Key::Delete),
        (vk_const::VK_HOME.0, Key::Home),
        (vk_const::VK_END.0, Key::End),
        (vk_const::VK_PRIOR.0, Key::PageUp),
        (vk_const::VK_NEXT.0, Key::PageDown),
    ];
    if let Some((_, key)) = NAMED.iter().find(|(code, _)| *code == vk) {
        return Some(*key);
    }
    // ASCII digits and letters share their virtual key codes with their characters, and
    // egui names them the same way, so the name round-trips.
    match vk {
        0x30..=0x39 | 0x41..=0x5A => {
            Key::from_name(&char::from(u8::try_from(vk).ok()?).to_string())
        }
        0x70..=0x87 => Key::from_name(&format!("F{}", vk - 0x6F)),
        _ => None,
    }
}
