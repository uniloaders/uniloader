//! The hidden windows the smoke host presents to, one class per graphics API.

// FFI module: window class and window creation.
#![allow(unsafe_code)]

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, RegisterClassExW, WINDOW_EX_STYLE, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};

pub(crate) const WIDTH: i32 = 1280;
pub(crate) const HEIGHT: i32 = 720;

unsafe extern "system" fn procedure(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: forwarding our own arguments to the default procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A [`WIDTH`] by [`HEIGHT`] window of a class named `class`, never shown.
pub(crate) fn hidden(class: &str) -> windows::core::Result<HWND> {
    let class: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
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
    // SAFETY: a window of the class just registered.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}
