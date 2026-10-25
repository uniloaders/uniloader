//! `OpenGL`: a detour on `opengl32!wglSwapBuffers`, and egui drawn into the back buffer
//! just before it is shown.
//!
//! `gdi32!SwapBuffers` ends in `wglSwapBuffers` too, so one detour sees every frame however
//! the game presents. `opengl32.dll` is only hooked when the game has it: one that is not
//! loaded at start is looked for for a while (games that load it at run time, through SDL or
//! GLFW), and never loaded by the overlay itself, so a `Direct3D` game is not touched.
//!
//! The renderer belongs to the context that was current when it was made. A frame from
//! another context (a game that recreated its context, or presents from two) builds a new
//! one; the old one's objects are left to its context, which may be gone already.

// FFI module: the detour, Win32 and WGL calls.
#![allow(unsafe_code)]

mod painter;

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use uniloader_overlay::{GraphicsApi, PresentInfo};
use uniloader_sdk::Host;
use windows::core::{BOOL, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, RECT};
use windows::Win32::Graphics::Gdi::{WindowFromDC, HDC};
use windows::Win32::Graphics::OpenGL::{wglGetCurrentContext, wglGetProcAddress, HGLRC};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;

use super::wnd;
use crate::ui::Gui;
use crate::{registry, shared};

type SwapBuffersFn = unsafe extern "system" fn(HDC) -> BOOL;

/// The trampoline to the real `wglSwapBuffers`; zero when not hooked.
static REAL_SWAP: AtomicUsize = AtomicUsize::new(0);
/// Set once a context turned out unusable (older than 3.0, no shader), so it is said once.
static REFUSED: AtomicBool = AtomicBool::new(false);
/// Set once a frame was drawable, and said through the `drawing` setting.
static DRAWABLE: AtomicBool = AtomicBool::new(false);

/// How long a game is watched for loading `opengl32.dll` after the plugin started.
const WATCH_FOR: Duration = Duration::from_secs(90);
const WATCH_EVERY: Duration = Duration::from_millis(250);
/// How long after the plugin started a DXGI present still says "not an `OpenGL` game".
const DXGI_GRACE: Duration = Duration::from_secs(15);

thread_local! {
    static OVERLAY: RefCell<Option<GlOverlay>> = const { RefCell::new(None) };
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
}

struct GlOverlay {
    /// The context the painter's objects live in.
    context: HGLRC,
    gui: Gui,
    painter: painter::Painter,
    started: Instant,
}

/// Hook `wglSwapBuffers` once it is clear the game draws with `OpenGL`: `opengl32.dll` is
/// loaded and no DXGI present came for [`DXGI_GRACE`]. A `Direct3D` game often has the
/// library loaded by another program (an overlay, a recorder) that hooked `wglSwapBuffers`
/// itself; detouring it there would only patch code inside that program's hook chain. Under
/// Wine's own `Direct3D` (wined3d) DXGI presents end in `wglSwapBuffers`, which would also
/// draw the overlay twice. The renderer is made on the first frame drawn, so hooking late
/// costs a game nothing but the overlay's first seconds.
pub(crate) fn install() {
    let watcher = uniloader_sdk::guard::spawn("overlay-gl-watch", || {
        let started = Instant::now();
        while started.elapsed() < WATCH_FOR {
            std::thread::sleep(WATCH_EVERY);
            if super::dxgi_presented() {
                log::debug!("the game presents through DXGI; wglSwapBuffers is left alone");
                return;
            }
            if started.elapsed() >= DXGI_GRACE && crate::host().is_some_and(hook) {
                return;
            }
        }
    });
    if let Err(e) = watcher {
        log::debug!("no thread to watch for OpenGL ({e})");
    }
}

fn opengl32() -> Option<HMODULE> {
    let name: Vec<u16> = "opengl32.dll".encode_utf16().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated name; the handle is not reference counted, which is fine
    // for a module the game keeps loaded.
    unsafe { GetModuleHandleW(PCWSTR(name.as_ptr())) }.ok()
}

/// Detour `wglSwapBuffers`; `true` once it is hooked (or failed for good).
fn hook(host: &Host) -> bool {
    if REAL_SWAP.load(Ordering::Acquire) != 0 {
        return true;
    }
    let Some(module) = opengl32() else {
        return false;
    };
    // SAFETY: a loaded module and a NUL-terminated export name.
    let Some(target) =
        (unsafe { GetProcAddress(module, PCSTR(c"wglSwapBuffers".as_ptr().cast())) })
    else {
        log::warn!("opengl32.dll has no wglSwapBuffers; no OpenGL overlay");
        return true;
    };
    let replacement = swap_buffers as SwapBuffersFn;
    // SAFETY: `target` is wglSwapBuffers, and `swap_buffers` has its exact signature.
    match unsafe {
        host.detour(
            target as *mut c_void,
            replacement as *mut c_void,
            "wglSwapBuffers",
        )
    } {
        Ok((_, trampoline)) => {
            REAL_SWAP.store(trampoline as usize, Ordering::Release);
            log::info!("hooked wglSwapBuffers (OpenGL)");
        }
        Err(e) => log::warn!("could not hook wglSwapBuffers: {e}"),
    }
    true
}

unsafe extern "system" fn swap_buffers(hdc: HDC) -> BOOL {
    let real = REAL_SWAP.load(Ordering::Acquire);
    if real == 0 {
        return BOOL(0);
    }
    // A game that began presenting through DXGI after all: that hook draws.
    if !IN_FRAME.with(Cell::get) && !super::dxgi_recent() {
        IN_FRAME.with(|f| f.set(true));
        // The game's own code is below us: a panic unwinding into it would abort the
        // process, a fault would crash it (a fault disables the plugin).
        if uniloader_sdk::guard::call(|| frame(hdc)) == Err(uniloader_sdk::guard::Failed::Panicked)
        {
            log::error!("OpenGL overlay frame panicked; the renderer is rebuilt on the next frame");
            OVERLAY.with_borrow_mut(Option::take);
        }
        IN_FRAME.with(|f| f.set(false));
    }
    // SAFETY: the trampoline calls the original wglSwapBuffers, which has this signature.
    let real: SwapBuffersFn = unsafe { core::mem::transmute::<usize, SwapBuffersFn>(real) };
    // SAFETY: forwarding the caller's argument to the original.
    unsafe { real(hdc) }
}

/// One frame about to be shown on `hdc`, with the game's context current.
fn frame(hdc: HDC) {
    let Some(host) = crate::host() else { return };
    // SAFETY: plain queries of the calling thread's WGL and GDI state.
    let (context, hwnd) = unsafe { (wglGetCurrentContext(), WindowFromDC(hdc)) };
    if context.is_invalid() || hwnd.is_invalid() {
        return;
    }
    let size = client_size(hwnd);
    if size.0 == 0 || size.1 == 0 {
        return;
    }
    if !DRAWABLE.swap(true, Ordering::Relaxed) {
        super::announce(host, true);
    }
    wnd::subclass(hwnd.0);
    subscribers(host, hdc, context, hwnd, size);
    if !shared::is_visible() {
        return;
    }
    OVERLAY.with_borrow_mut(|slot| {
        if slot.as_ref().is_some_and(|o| o.context != context) {
            log::info!("the game made another OpenGL context current; the overlay follows it");
            // Its objects belong to the other context: left to it, not deleted from this one.
            if let Some(old) = slot.take() {
                std::mem::forget(old);
            }
        }
        if slot.is_none() {
            *slot = create(context);
        }
        if let Some(overlay) = slot {
            let output = super::gui_pass(&mut overlay.gui, overlay.started, host, size);
            overlay.painter.paint(overlay.gui.context(), output, size);
        }
    });
}

fn create(context: HGLRC) -> Option<GlOverlay> {
    if REFUSED.load(Ordering::Relaxed) {
        return None;
    }
    let module = opengl32();
    let load = |name: &core::ffi::CStr| -> *const c_void {
        let name = PCSTR(name.as_ptr().cast());
        // SAFETY: a NUL-terminated name and a current context, as wglGetProcAddress needs.
        let address = unsafe { wglGetProcAddress(name) }.map_or(0, |f| f as usize);
        // Some implementations answer 1, 2, 3 or -1 for "not an extension function".
        if address > 3 && address != usize::MAX {
            return address as *const c_void;
        }
        // The OpenGL 1.1 functions only come from opengl32.dll itself.
        module
            // SAFETY: a loaded module and a NUL-terminated name.
            .and_then(|m| unsafe { GetProcAddress(m, name) })
            .map_or(core::ptr::null(), |f| f as *const c_void)
    };
    // SAFETY: `context` is current on this thread (checked by the caller) and `load`
    // returns its functions.
    match unsafe { painter::Painter::new(load) } {
        Ok(painter) => {
            log::info!("overlay ready (OpenGL)");
            Some(GlOverlay {
                context,
                gui: Gui::new(),
                painter,
                started: Instant::now(),
            })
        }
        Err(e) => {
            REFUSED.store(true, Ordering::Relaxed);
            log::warn!("no OpenGL overlay: {e}");
            if let Some(host) = crate::host() {
                super::announce(host, false);
            }
            None
        }
    }
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT::default();
    // SAFETY: a window handle and a valid out-pointer.
    if unsafe { GetClientRect(hwnd, &raw mut rect) }.is_err() {
        return (0, 0);
    }
    (
        u32::try_from(rect.right - rect.left).unwrap_or(0),
        u32::try_from(rect.bottom - rect.top).unwrap_or(0),
    )
}

/// Run the present subscribers for an `OpenGL` frame.
fn subscribers(host: &Host, hdc: HDC, context: HGLRC, hwnd: HWND, size: (u32, u32)) {
    let list = registry::present_subscribers();
    if list.is_empty() {
        return;
    }
    let info = PresentInfo {
        struct_size: core::mem::size_of::<PresentInfo>(),
        api: GraphicsApi::OPENGL,
        swapchain: hdc.0,
        device: context.0,
        hwnd: hwnd.0,
        width: size.0,
        height: size.1,
        sync_interval: 0,
        flags: 0,
    };
    for subscriber in &list {
        crate::ui::calls::present(host, subscriber, &info);
    }
}
