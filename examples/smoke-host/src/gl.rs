//! An `OpenGL` window presenting frames through `SwapBuffers`, so the overlay's
//! `wglSwapBuffers` detour has something to draw on. Like the `Direct3D` 11 window it is
//! never shown.

// FFI module: window, pixel format and WGL calls.
#![allow(unsafe_code)]

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, HDC};
use windows::Win32::Graphics::OpenGL::{
    glClear, glClearColor, glFinish, glReadBuffer, glReadPixels, wglCreateContext,
    wglDeleteContext, wglMakeCurrent, ChoosePixelFormat, SetPixelFormat, SwapBuffers, GL_BACK,
    GL_COLOR_BUFFER_BIT, GL_RGBA, GL_UNSIGNED_BYTE, HGLRC, PFD_DOUBLEBUFFER, PFD_DRAW_TO_WINDOW,
    PFD_MAIN_PLANE, PFD_SUPPORT_OPENGL, PFD_TYPE_RGBA, PIXELFORMATDESCRIPTOR,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyWindow, GetClientRect};

use crate::capture::Frame;

/// The colour every frame is cleared to, as bytes.
const CLEAR: [u8; 4] = [0, 64, 128, 255];

/// Present `frames` frames about 60 times a second. Returns how many swaps succeeded.
pub(crate) fn present_frames(frames: u32) -> windows::core::Result<u32> {
    let hwnd = crate::window::hidden("smoke-host-gl")?;
    let mut client = RECT::default();
    // SAFETY: the window created above and a valid out-pointer.
    unsafe { GetClientRect(hwnd, &raw mut client) }?;
    let size = (client.right - client.left, client.bottom - client.top);
    // SAFETY: the window created above.
    let hdc = unsafe { GetDC(Some(hwnd)) };
    let result = present_on(hdc, frames, size);
    // SAFETY: the DC and window from above.
    unsafe {
        ReleaseDC(Some(hwnd), hdc);
        let _ = DestroyWindow(hwnd);
    }
    result
}

fn present_on(hdc: HDC, frames: u32, size: (i32, i32)) -> windows::core::Result<u32> {
    let format = PIXELFORMATDESCRIPTOR {
        nSize: u16::try_from(core::mem::size_of::<PIXELFORMATDESCRIPTOR>()).unwrap_or(0),
        nVersion: 1,
        dwFlags: PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER,
        iPixelType: PFD_TYPE_RGBA,
        cColorBits: 32,
        cAlphaBits: 8,
        cDepthBits: 24,
        iLayerType: PFD_MAIN_PLANE.0.cast_unsigned(),
        ..PIXELFORMATDESCRIPTOR::default()
    };
    // SAFETY: a window DC and a complete descriptor.
    let index = unsafe { ChoosePixelFormat(hdc, &raw const format) };
    // SAFETY: as above, with the format chosen for it.
    unsafe { SetPixelFormat(hdc, index, &raw const format) }?;
    // SAFETY: a DC with a pixel format.
    let context = unsafe { wglCreateContext(hdc) }?;
    // SAFETY: the context just made, for this DC.
    unsafe { wglMakeCurrent(hdc, context) }?;
    let mut swapped = 0;
    for _ in 0..frames {
        crate::d3d::pump();
        // SAFETY: a current context.
        unsafe {
            glClearColor(0.0, 0.25, 0.5, 1.0);
            glClear(GL_COLOR_BUFFER_BIT);
        }
        // SAFETY: the DC the context draws to.
        if unsafe { SwapBuffers(hdc) }.is_ok() {
            swapped += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    // The overlay draws inside the swap, onto the buffer that is then shown. A hidden window
    // has no front buffer to read (it comes back black), but the swap copies, so the back
    // buffer still holds the last frame.
    let frame = read(GL_BACK, size);
    println!("gl drawn pixels {}", frame.differing(CLEAR));
    if let Err(e) = frame.save_bmp(std::path::Path::new("capture-gl.bmp")) {
        println!("gl capture not saved: {e}");
    }
    // SAFETY: the context made above, made not current first.
    unsafe {
        let _ = wglMakeCurrent(hdc, HGLRC::default());
        let _ = wglDeleteContext(context);
    }
    Ok(swapped)
}

/// Read one colour buffer of the current context, top row first.
fn read(buffer: u32, size: (i32, i32)) -> Frame {
    let (width, height) = (size.0.max(0).cast_unsigned(), size.1.max(0).cast_unsigned());
    let row = width as usize * 4;
    let mut bottom_up = vec![0u8; row * height as usize];
    // SAFETY: a current context; the buffer holds `width * height` RGBA8 pixels.
    unsafe {
        glFinish();
        glReadBuffer(buffer);
        glReadPixels(
            0,
            0,
            size.0,
            size.1,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            bottom_up.as_mut_ptr().cast(),
        );
    }
    let rgba = bottom_up
        .chunks_exact(row)
        .rev()
        .flatten()
        .copied()
        .collect();
    Frame {
        width,
        height,
        rgba,
    }
}
