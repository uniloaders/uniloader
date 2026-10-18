//! The universal DXGI hook.
//!
//! `IDXGISwapChain::Present`, `Present1` and `ResizeBuffers` are virtual functions of one
//! class per DXGI implementation, and every swapchain of that class shares the vtable. So a
//! throwaway `Direct3D` 11 device and swapchain on a hidden window is enough to find the table;
//! patching its slots through the loader then covers every swapchain the game makes, before
//! or after this plugin loaded, whichever DLL the loader proxies. The loader restores the
//! slots when the plugin stops.
//!
//! A game whose swapchain is another class (vkd3d-proton's `Direct3D` 12, for example) is not
//! covered; it never calls these slots, so nothing breaks either.

// FFI module: vtable slots, COM objects and calls through raw function pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use uniloader_sdk::Host;
use windows::core::{Interface, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISwapChain, IDXGISwapChain1, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, UnregisterClassW,
    WINDOW_EX_STYLE, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};

/// Vtable slots, counted from the start of the interface's vtable.
mod slot {
    /// `IDXGISwapChain::Present`.
    pub(super) const PRESENT: u32 = 8;
    /// `IDXGISwapChain::ResizeBuffers`.
    pub(super) const RESIZE_BUFFERS: u32 = 13;
    /// `IDXGISwapChain1::Present1`.
    pub(super) const PRESENT1: u32 = 22;
}

type PresentFn = unsafe extern "system" fn(*mut c_void, u32, u32) -> HRESULT;
type Present1Fn = unsafe extern "system" fn(*mut c_void, u32, u32, *const c_void) -> HRESULT;
type ResizeBuffersFn =
    unsafe extern "system" fn(*mut c_void, u32, u32, u32, DXGI_FORMAT, u32) -> HRESULT;

/// The functions the slots held before, as addresses; zero means "not hooked".
static REAL_PRESENT: AtomicUsize = AtomicUsize::new(0);
static REAL_PRESENT1: AtomicUsize = AtomicUsize::new(0);
static REAL_RESIZE: AtomicUsize = AtomicUsize::new(0);

/// Class name of the hidden probe window.
const PROBE_CLASS: &str = "uniloader-overlay-probe";

/// Find the swapchain vtable and hook it. Logs and gives up on failure: no overlay is
/// better than a broken game.
pub(crate) fn install(host: &Host) {
    match probe() {
        Ok(chain) => hook(host, &chain),
        Err(e) => log::warn!("no DXGI swapchain to learn the vtable from ({e}); overlay off"),
    }
}

/// Hook the three slots through a live swapchain of the class.
fn hook(host: &Host, chain: &IDXGISwapChain) {
    let raw = chain.as_raw();
    let slots: [(u32, *mut c_void, &AtomicUsize, &str); 2] = [
        (
            slot::PRESENT,
            present as *mut c_void,
            &REAL_PRESENT,
            "IDXGISwapChain::Present",
        ),
        (
            slot::RESIZE_BUFFERS,
            resize_buffers as *mut c_void,
            &REAL_RESIZE,
            "IDXGISwapChain::ResizeBuffers",
        ),
    ];
    for (index, replacement, original, note) in slots {
        // `raw` is a live swapchain whose vtable has these slots, and each replacement has
        // the exact signature of its slot.
        install_slot(host, raw, index, replacement, original, note);
    }
    // Present1 lives in the same table when the class implements IDXGISwapChain1, which
    // every implementation since Windows 8 does.
    match chain.cast::<IDXGISwapChain1>() {
        Ok(chain1) => install_slot(
            host,
            chain1.as_raw(),
            slot::PRESENT1,
            present1 as *mut c_void,
            &REAL_PRESENT1,
            "IDXGISwapChain1::Present1",
        ),
        Err(e) => log::info!("no IDXGISwapChain1 ({e}); Present1 is not hooked"),
    }
}

fn install_slot(
    host: &Host,
    object: *mut c_void,
    index: u32,
    replacement: *mut c_void,
    original: &AtomicUsize,
    note: &str,
) {
    // SAFETY: `object` is a live COM object whose vtable has `index`, and `replacement` has
    // the signature of that slot; see the callers.
    match unsafe { host.hook_vtable(object, index, replacement, note) } {
        Ok((_, previous)) => {
            original.store(previous as usize, Ordering::Release);
            log::info!("hooked {note}");
        }
        Err(e) => log::warn!("could not hook {note}: {e}"),
    }
}

/// Forget the originals, so a call already in flight after the loader restored the slots
/// goes nowhere new. The loader removes the hooks themselves.
pub(crate) fn uninstall() {
    REAL_PRESENT.store(0, Ordering::Release);
    REAL_PRESENT1.store(0, Ordering::Release);
    REAL_RESIZE.store(0, Ordering::Release);
}

unsafe extern "system" fn probe_procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwarding our own arguments to the default procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

/// A hidden window, a device and a swapchain, just long enough to read the vtable from.
fn probe() -> windows::core::Result<IDXGISwapChain> {
    let class = wide(PROBE_CLASS);
    // SAFETY: the module handle of the process image; never freed.
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let wc = WNDCLASSEXW {
        cbSize: u32::try_from(core::mem::size_of::<WNDCLASSEXW>()).unwrap_or(0),
        lpfnWndProc: Some(probe_procedure),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..WNDCLASSEXW::default()
    };
    // SAFETY: a valid class description whose name outlives the registration's use here.
    // Registering twice (a plugin restart) fails harmlessly and the old class is used.
    unsafe { RegisterClassExW(&raw const wc) };
    // SAFETY: a window of the class just registered; never shown.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            64,
            64,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }?;
    let result = swapchain_on(hwnd);
    // SAFETY: the window and class created above; the swapchain only needed them while it
    // was made, and its vtable outlives both.
    unsafe {
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance.into()));
    }
    result
}

/// Make a `Direct3D` 11 swapchain on `hwnd`, on the GPU or failing that on WARP.
fn swapchain_on(hwnd: HWND) -> windows::core::Result<IDXGISwapChain> {
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: 64,
            Height: 64,
            RefreshRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..DXGI_MODE_DESC::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 1,
        OutputWindow: hwnd,
        Windowed: BOOL(1),
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        Flags: 0,
    };
    let mut last = windows::core::Error::empty();
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        match create(driver, &desc) {
            Ok(chain) => return Ok(chain),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn create(
    driver: D3D_DRIVER_TYPE,
    desc: &DXGI_SWAP_CHAIN_DESC,
) -> windows::core::Result<IDXGISwapChain> {
    let mut chain: Option<IDXGISwapChain> = None;
    // SAFETY: every out-pointer is valid for the call and the descriptor is complete.
    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(core::ptr::from_ref(desc)),
            Some(&raw mut chain),
            None,
            None,
            None,
        )
    }?;
    chain.ok_or_else(windows::core::Error::empty)
}

/// The original function behind a slot, reinterpreted as `F`.
///
/// # Safety
/// `F` must be the signature of the slot the address was read from.
unsafe fn original<F: Copy>(slot: &AtomicUsize) -> Option<F> {
    let address = slot.load(Ordering::Acquire);
    if address == 0 {
        return None;
    }
    // SAFETY: the address came out of a vtable slot holding a function of type `F`, and a
    // function pointer has the size of a `usize`.
    Some(unsafe { core::mem::transmute_copy::<usize, F>(&address) })
}

unsafe extern "system" fn present(this: *mut c_void, sync_interval: u32, flags: u32) -> HRESULT {
    // SAFETY: the slot held a `PresentFn`.
    let Some(real) = (unsafe { original::<PresentFn>(&REAL_PRESENT) }) else {
        return HRESULT(0);
    };
    super::frame(this, sync_interval, flags);
    // SAFETY: forwarding the caller's arguments to the original.
    unsafe { real(this, sync_interval, flags) }
}

unsafe extern "system" fn present1(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    parameters: *const c_void,
) -> HRESULT {
    // SAFETY: the slot held a `Present1Fn`.
    let Some(real) = (unsafe { original::<Present1Fn>(&REAL_PRESENT1) }) else {
        return HRESULT(0);
    };
    super::frame(this, sync_interval, flags);
    // SAFETY: forwarding the caller's arguments to the original.
    unsafe { real(this, sync_interval, flags, parameters) }
}

unsafe extern "system" fn resize_buffers(
    this: *mut c_void,
    buffer_count: u32,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    flags: u32,
) -> HRESULT {
    // SAFETY: the slot held a `ResizeBuffersFn`.
    let Some(real) = (unsafe { original::<ResizeBuffersFn>(&REAL_RESIZE) }) else {
        return HRESULT(0);
    };
    // SAFETY: forwarding the caller's arguments to the original.
    let hr = unsafe { real(this, buffer_count, width, height, format, flags) };
    if hr.is_ok() {
        log::debug!("swapchain resized to {width}x{height}");
    }
    hr
}
