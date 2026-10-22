//! Opening a link the user clicked (a plugin's home page, issue tracker or download) in the
//! default browser.

// FFI module: one shell call.
#![allow(unsafe_code)]

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// Open `url` in the default browser. Only `https://` and `http://`: a plugin declares these
/// links, and anything else (a file, a program) is not what a link in a plugin list may do.
pub(crate) fn open_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        log::warn!("not opening {url:?}: only http and https links are opened");
        return;
    }
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = url.encode_utf16().chain([0]).collect();
    // SAFETY: both strings are NUL terminated and outlive the call; no window owns it.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success, per the documentation.
    if result.0 as usize <= 32 {
        log::warn!(
            "could not open {url}: ShellExecute returned {}",
            result.0 as usize
        );
    } else {
        log::info!("opened {url} in the browser");
    }
}
