//! The Windows half: the DXGI hook, the `Direct3D` 11 renderer, window input and clipboard.
//!
//! Everything that touches the device runs on the thread that calls `Present`, which is why
//! the renderer lives in a thread local: the immediate context is not thread safe, and this
//! way nothing else can reach it.

mod clipboard;
mod hooks;
mod render;
mod wnd;

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use egui::{Pos2, Rect, Vec2};
use uniloader_overlay::{GraphicsApi, PresentInfo};
use uniloader_sdk::Host;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Graphics::Dxgi::{IDXGISwapChain, DXGI_PRESENT_TEST, DXGI_SWAP_CHAIN_DESC};

use crate::ui::{calls, FrameInput, Gui};
use crate::{registry, shared};

/// Set once a swapchain turned out not to be `Direct3D` 11, so it is said once.
static NOT_D3D11: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The renderer, made on the first frame that needs it.
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };
    /// Set while this thread is inside a frame: some implementations' `Present` calls
    /// `Present1` through the vtable, which would otherwise draw the overlay twice.
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
}

/// The device side of the overlay.
struct Overlay {
    gui: Gui,
    renderer: egui_directx11::Renderer,
    device: ID3D11Device,
    started: Instant,
}

/// Hook DXGI. Called from the plugin's `start`.
pub(crate) fn install(host: &Host) {
    hooks::install(host);
}

/// Let go of the game: the window procedure goes back, and the hooks stop forwarding to us.
pub(crate) fn uninstall() {
    hooks::uninstall();
    wnd::unsubclass();
}

/// One `Present`, before it reaches DXGI: present subscribers, then the overlay on top.
pub(crate) fn frame(swapchain: *mut c_void, flags: u32) {
    // A test present draws nothing, so neither do we.
    if flags & DXGI_PRESENT_TEST.0 != 0 || IN_FRAME.with(Cell::get) {
        return;
    }
    let Some(host) = crate::host() else { return };
    let Some(chain) = render::swapchain_of(swapchain) else {
        return;
    };
    IN_FRAME.with(|f| f.set(true));
    // The game's own code is below us: a panic unwinding into it would abort the process.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let desc = describe(&chain);
        wnd::subclass(desc.OutputWindow.0);
        subscribers(host, &chain, &desc);
        if shared::is_visible() {
            OVERLAY.with_borrow_mut(|slot| draw(host, slot, &chain));
        }
    }));
    IN_FRAME.with(|f| f.set(false));
    if outcome.is_err() {
        log::error!("overlay frame panicked; the renderer is rebuilt on the next frame");
        OVERLAY.with_borrow_mut(Option::take);
    }
}

#[allow(unsafe_code, reason = "one COM call on a live swapchain")]
fn describe(chain: &IDXGISwapChain) -> DXGI_SWAP_CHAIN_DESC {
    // SAFETY: a live swapchain; the call only reads its description.
    unsafe { chain.GetDesc() }.unwrap_or_else(|e| {
        log::debug!("swapchain description unavailable: {e}");
        DXGI_SWAP_CHAIN_DESC::default()
    })
}

/// Run the present subscribers, under the fault guard of each.
fn subscribers(host: &Host, chain: &IDXGISwapChain, desc: &DXGI_SWAP_CHAIN_DESC) {
    let list = registry::present_subscribers();
    if list.is_empty() {
        return;
    }
    let device = render::device_of(chain);
    let info = PresentInfo {
        struct_size: core::mem::size_of::<PresentInfo>(),
        api: GraphicsApi::DXGI,
        swapchain: chain.as_raw(),
        device: device
            .as_ref()
            .map_or(core::ptr::null_mut(), Interface::as_raw),
        hwnd: desc.OutputWindow.0,
        width: desc.BufferDesc.Width,
        height: desc.BufferDesc.Height,
    };
    for subscriber in &list {
        calls::present(host, subscriber, &info);
    }
}

/// Make the renderer if there is none yet, then draw one frame.
fn draw(host: &Host, slot: &mut Option<Overlay>, chain: &IDXGISwapChain) {
    if slot.is_none() {
        *slot = create(chain);
    }
    let Some(overlay) = slot else { return };
    if let Err(e) = overlay.draw(host, chain) {
        log::error!("overlay frame failed: {e}");
    }
}

/// Build the renderer from the device behind the swapchain.
fn create(chain: &IDXGISwapChain) -> Option<Overlay> {
    let Some(device) = render::device_of(chain) else {
        if !NOT_D3D11.swap(true, Ordering::Relaxed) {
            log::warn!("the game's swapchain is not Direct3D 11; the overlay is not drawn");
        }
        return None;
    };
    let renderer = egui_directx11::Renderer::new(&device)
        .inspect_err(|e| log::error!("could not create the overlay renderer: {e}"))
        .ok()?;
    log::info!("overlay ready");
    Some(Overlay {
        gui: Gui::new(),
        renderer,
        device,
        started: Instant::now(),
    })
}

impl Overlay {
    fn draw(&mut self, host: &Host, chain: &IDXGISwapChain) -> windows::core::Result<()> {
        let target = render::target(chain, &self.device)?;
        let Some(context) = render::context(&self.device) else {
            return Ok(());
        };
        #[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
        let (width, height) = (target.size.0 as f32, target.size.1 as f32);
        self.gui.rescale(height);
        wnd::set_screen(width, height);
        let raw = wnd::take_input(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height)),
            self.started.elapsed().as_secs_f64(),
        );
        let pressed = if self.gui.recording() {
            wnd::take_last_key()
        } else {
            // Not wanted, but taken anyway, so a key pressed long ago does not end the next
            // recording the moment it starts.
            let _ = wnd::take_last_key();
            None
        };
        let input = FrameInput {
            raw,
            pointer: wnd::pointer(),
            pressed,
        };
        let output = self.gui.run(host, input);
        let (renderer_output, platform, _viewports) = egui_directx11::split_output(output);
        // egui asks its host to copy; with no backend under us, that is this.
        for command in platform.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                clipboard::set_text(&text);
            }
        }
        let saved = render::save(&context);
        let result =
            self.renderer
                .render(&context, &target.view, self.gui.context(), renderer_output);
        render::restore(&context, &saved);
        result
    }
}
