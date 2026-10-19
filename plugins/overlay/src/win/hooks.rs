//! The universal DXGI hook.
//!
//! `IDXGISwapChain::Present`, `Present1` and `ResizeBuffers` are virtual functions of one
//! class per DXGI implementation, and every swapchain of that class shares the vtable. So a
//! throwaway `Direct3D` 11 device and swapchain on a hidden window is enough to find the table;
//! patching its slots through the loader then covers every swapchain the game makes, before
//! or after this plugin loaded, whichever DLL the loader proxies. The loader restores the
//! slots when the plugin stops.
//!
//! A `Direct3D` 12 swapchain can be another class (vkd3d-proton's under Proton), so a
//! throwaway `Direct3D` 12 swapchain is made too, and its class hooked as well when it
//! differs. Each class keeps its own originals: the slot functions are generic over it.

// FFI module: vtable slots, COM objects and calls through raw function pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use uniloader_sdk::Host;
use windows::core::{IUnknown, Interface, BOOL, HRESULT, PCWSTR};
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

/// The swapchain classes hooked: the `Direct3D` 11 one, and a `Direct3D` 12 one of its own.
const CLASSES: usize = 2;
/// The `Direct3D` 12 class, when it is not the `Direct3D` 11 one.
const D3D12_CLASS: usize = 1;

/// The functions the slots held before, per class, as addresses; zero means "not hooked".
static REAL_PRESENT: [AtomicUsize; CLASSES] = [const { AtomicUsize::new(0) }; CLASSES];
static REAL_PRESENT1: [AtomicUsize; CLASSES] = [const { AtomicUsize::new(0) }; CLASSES];
static REAL_RESIZE: [AtomicUsize; CLASSES] = [const { AtomicUsize::new(0) }; CLASSES];

/// Class name of the hidden probe window.
const PROBE_CLASS: &str = "uniloader-overlay-probe";

/// Find the swapchain vtables and hook them. Logs and gives up on failure: no overlay is
/// better than a broken game.
pub(crate) fn install(host: &Host) {
    match with_probe_window(|hwnd| Ok((swapchain_on(hwnd)?, super::d3d12::probe(hwnd)))) {
        Ok((chain, d3d12)) => {
            hook::<0>(host, &chain, "");
            match d3d12 {
                Ok((chain12, queue)) => {
                    super::d3d12::hook_queue(host, &queue);
                    let (a, b) = (chain.cast::<IUnknown>(), chain12.cast::<IUnknown>());
                    if let (Ok(a), Ok(b)) = (a, b) {
                        if super::d3d12::same_class(&a, &b) {
                            log::info!("Direct3D 12 presents through the same swapchain class");
                        } else {
                            hook::<D3D12_CLASS>(host, &chain12, " (Direct3D 12)");
                        }
                    }
                }
                Err(e) => log::info!("no Direct3D 12 swapchain to learn from ({e})"),
            }
        }
        Err(e) => log::warn!("no DXGI swapchain to learn the vtable from ({e}); overlay off"),
    }
}

/// Hook the three slots through a live swapchain of class `C`.
fn hook<const C: usize>(host: &Host, chain: &IDXGISwapChain, which: &str) {
    let raw = chain.as_raw();
    let present_note = format!("IDXGISwapChain::Present{which}");
    let resize_note = format!("IDXGISwapChain::ResizeBuffers{which}");
    let slots: [(u32, *mut c_void, &AtomicUsize, &str); 2] = [
        (
            slot::PRESENT,
            present::<C> as *mut c_void,
            &REAL_PRESENT[C],
            &present_note,
        ),
        (
            slot::RESIZE_BUFFERS,
            resize_buffers::<C> as *mut c_void,
            &REAL_RESIZE[C],
            &resize_note,
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
            present1::<C> as *mut c_void,
            &REAL_PRESENT1[C],
            &format!("IDXGISwapChain1::Present1{which}"),
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

/// Runs `make` with a hidden window, just long enough to make swapchains to read vtables
/// from.
fn with_probe_window<T>(
    make: impl FnOnce(HWND) -> windows::core::Result<T>,
) -> windows::core::Result<T> {
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
    let result = make(hwnd);
    // SAFETY: the window and class created above; the swapchains only needed them while
    // they were made, and their vtables outlive both.
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

unsafe extern "system" fn present<const C: usize>(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
) -> HRESULT {
    // SAFETY: the slot held a `PresentFn`.
    let Some(real) = (unsafe { original::<PresentFn>(&REAL_PRESENT[C]) }) else {
        return HRESULT(0);
    };
    super::frame(this, sync_interval, flags);
    // SAFETY: forwarding the caller's arguments to the original.
    unsafe { real(this, sync_interval, flags) }
}

unsafe extern "system" fn present1<const C: usize>(
    this: *mut c_void,
    sync_interval: u32,
    flags: u32,
    parameters: *const c_void,
) -> HRESULT {
    // SAFETY: the slot held a `Present1Fn`.
    let Some(real) = (unsafe { original::<Present1Fn>(&REAL_PRESENT1[C]) }) else {
        return HRESULT(0);
    };
    super::frame(this, sync_interval, flags);
    // SAFETY: forwarding the caller's arguments to the original.
    unsafe { real(this, sync_interval, flags, parameters) }
}

unsafe extern "system" fn resize_buffers<const C: usize>(
    this: *mut c_void,
    buffer_count: u32,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    flags: u32,
) -> HRESULT {
    // SAFETY: the slot held a `ResizeBuffersFn`.
    let Some(real) = (unsafe { original::<ResizeBuffersFn>(&REAL_RESIZE[C]) }) else {
        return HRESULT(0);
    };
    // SAFETY: forwarding the caller's arguments to the original.
    let hr = unsafe { real(this, buffer_count, width, height, format, flags) };
    if hr.is_ok() {
        log::debug!("swapchain resized to {width}x{height}");
    }
    hr
}
