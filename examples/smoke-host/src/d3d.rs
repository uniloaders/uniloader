//! A `Direct3D` 11 swapchain presenting frames, so the overlay's DXGI hook has something to
//! draw on. The window is never shown: presenting to a hidden window still goes through the
//! swapchain's vtable, which is all the hook needs, and nothing appears on the desktop.

// FFI module: window and device creation.
#![allow(unsafe_code)]

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDeviceAndSwapChain, ID3D11Device, ID3D11DeviceContext, ID3D11RenderTargetView,
    ID3D11Texture2D, D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGISwapChain, DXGI_PRESENT, DXGI_SWAP_CHAIN_DESC, DXGI_SWAP_EFFECT_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PeekMessageW,
    RegisterClassExW, TranslateMessage, MSG, PM_REMOVE, WINDOW_EX_STYLE, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};

unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwarding our own arguments to the default procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Present `frames` frames about 60 times a second. Returns how many presents succeeded.
pub(crate) fn present_frames(frames: u32) -> windows::core::Result<u32> {
    let class: Vec<u16> = "smoke-host".encode_utf16().chain(Some(0)).collect();
    // SAFETY: the process image's module handle.
    let instance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let wc = WNDCLASSEXW {
        cbSize: u32::try_from(core::mem::size_of::<WNDCLASSEXW>()).unwrap_or(0),
        lpfnWndProc: Some(procedure),
        hInstance: instance.into(),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..WNDCLASSEXW::default()
    };
    // SAFETY: a complete class description whose name outlives its use.
    unsafe { RegisterClassExW(&raw const wc) };
    // SAFETY: a window of the class just registered, never shown.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            1280,
            720,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }?;
    let result = present_on(hwnd, frames);
    // SAFETY: the window created above.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    result
}

fn present_on(hwnd: HWND, frames: u32) -> windows::core::Result<u32> {
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: 1280,
            Height: 720,
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
    let mut chain: Option<IDXGISwapChain> = None;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: every out-pointer is valid and the descriptor complete.
    unsafe {
        D3D11CreateDeviceAndSwapChain(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            None,
            D3D11_SDK_VERSION,
            Some(&raw const desc),
            Some(&raw mut chain),
            Some(&raw mut device),
            None,
            Some(&raw mut context),
        )
    }?;
    let missing = windows::core::Error::empty;
    let (chain, device, context) = (
        chain.ok_or_else(missing)?,
        device.ok_or_else(missing)?,
        context.ok_or_else(missing)?,
    );
    // SAFETY: buffer 0 of a live swapchain.
    let backbuffer: ID3D11Texture2D = unsafe { chain.GetBuffer(0) }?;
    let mut view: Option<ID3D11RenderTargetView> = None;
    // SAFETY: a live texture and a valid out-pointer.
    unsafe { device.CreateRenderTargetView(&backbuffer, None, Some(&raw mut view)) }?;
    let view = view.ok_or_else(missing)?;
    let mut presented = 0;
    for _ in 0..frames {
        pump();
        // SAFETY: a live view on this device's own backbuffer.
        unsafe { context.ClearRenderTargetView(&view, &CLEAR) };
        // SAFETY: a live swapchain.
        if unsafe { chain.Present(0, DXGI_PRESENT(0)) }.is_ok() {
            presented += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    // Read after the last present: the overlay draws inside `Present`, onto this buffer.
    match crate::capture::read(&device, &context, &backbuffer) {
        Ok(frame) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let clear = CLEAR.map(|c| (c * 255.0).round() as u8);
            println!("drawn pixels {}", frame.differing(clear));
            if let Err(e) = frame.save_bmp(std::path::Path::new("capture.bmp")) {
                println!("capture not saved: {e}");
            }
        }
        Err(e) => println!("no capture: {e}"),
    }
    Ok(presented)
}

/// The colour every frame is cleared to before it is presented.
const CLEAR: [f32; 4] = [0.0, 0.25, 0.5, 1.0];

/// Drain this thread's message queue, as a game's loop does between frames.
fn pump() {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid out-pointer; the messages are for this thread's own window.
    unsafe {
        while PeekMessageW(&raw mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
}
