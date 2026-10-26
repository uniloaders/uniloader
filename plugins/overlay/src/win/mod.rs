//! The Windows half: the DXGI hook, the `Direct3D` 11 renderer (also over `Direct3D` 12),
//! the `OpenGL` one (`gl`), the Vulkan one (`vk`), window input and clipboard.
//!
//! Everything that touches the device runs on the thread that calls `Present`, which is why
//! the renderer lives in a thread local: the immediate context is not thread safe, and this
//! way nothing else can reach it.

mod clipboard;
mod d3d12;
mod gl;
mod hooks;
mod render;
mod shell;
mod vk;
mod wnd;

use core::ffi::c_void;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;
use std::time::Instant;

use egui::{Pos2, Rect, Vec2};
use uniloader_overlay::{GraphicsApi, PresentInfo};
use uniloader_sdk::Host;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Resource};
use windows::Win32::Graphics::Dxgi::{IDXGISwapChain, DXGI_PRESENT_TEST, DXGI_SWAP_CHAIN_DESC};

use crate::ui::{calls, native, FrameInput, Gui};
use crate::{registry, shared};

/// Set once a swapchain turned out to be neither `Direct3D` 11 nor 12, so it is said once.
static UNKNOWN_DEVICE: AtomicBool = AtomicBool::new(false);
/// Set once `Direct3D` 11 on 12 failed: it is not tried again.
static ON12_FAILED: AtomicBool = AtomicBool::new(false);
/// Set once a swapchain this overlay can draw on was presented, and said through the
/// `drawing` setting.
static DRAWABLE: AtomicBool = AtomicBool::new(false);
/// When DXGI last presented, in milliseconds since [`EPOCH`] plus one; zero for never.
static DXGI_PRESENTED: AtomicU64 = AtomicU64::new(0);
static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);
/// How recent a DXGI present keeps the Vulkan path aside.
const DXGI_RECENT_MS: u64 = 2000;

/// DXGI presented within the last two seconds: under Proton the Vulkan present that follows
/// is DXVK's or vkd3d's for the same frame, which the DXGI hook already drew.
fn dxgi_recent() -> bool {
    let at = DXGI_PRESENTED.load(Ordering::Relaxed);
    at != 0 && millis_since_epoch().saturating_sub(at - 1) < DXGI_RECENT_MS
}

fn millis_since_epoch() -> u64 {
    u64::try_from(EPOCH.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Whether DXGI presented at all this session.
fn dxgi_presented() -> bool {
    DXGI_PRESENTED.load(Ordering::Relaxed) != 0
}

/// Whether this process runs under Wine (Proton included): its `ntdll` says so.
#[allow(unsafe_code, reason = "two plain lookups in a loaded module")]
fn under_wine() -> bool {
    use windows::core::{s, w};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    // SAFETY: a constant name of a module every process has.
    let Ok(ntdll) = (unsafe { GetModuleHandleW(w!("ntdll.dll")) }) else {
        return false;
    };
    // SAFETY: a loaded module and a constant export name.
    unsafe { GetProcAddress(ntdll, s!("wine_get_version")) }.is_some()
}

/// Whether the game's executable imports `dll` (by name, any case), directly or delay-loaded:
/// read from its import directories in memory.
#[allow(unsafe_code, reason = "reads of the mapped executable's own headers")]
fn exe_imports(dll: &str) -> bool {
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    // SAFETY: a null name asks for the executable's own module.
    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else {
        return false;
    };
    let base = module.0 as usize;
    let size = image_size(base);
    let read = |offset: usize| -> Option<u32> {
        (offset.checked_add(4)? <= size).then(|| {
            // SAFETY: `base` is the mapped executable, whose `size` bytes are readable, and
            // the read ends inside them (checked above).
            unsafe { core::ptr::read_unaligned((base + offset) as *const u32) }
        })
    };
    let Some(nt) = read(0x3C).map(|v| v as usize) else {
        return false;
    };
    // PE32+: data directories at optional header + 112; import is #1, delay import #13.
    let directories = nt + 24 + 112;
    let names = |directory: usize, entry_size: usize, name_at: usize| -> Vec<String> {
        let Some(rva) = read(directories + directory * 8).map(|v| v as usize) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut entry = rva;
        while rva != 0 && found.len() < 512 {
            match read(entry + name_at).map(|v| v as usize) {
                Some(name) if name != 0 => found.push(ascii_at(base, name, size)),
                _ => break,
            }
            entry += entry_size;
        }
        found
    };
    let imports = names(1, 20, 12).into_iter().chain(names(13, 32, 4));
    imports
        .into_iter()
        .any(|name| name.eq_ignore_ascii_case(dll))
}

/// `SizeOfImage` of the module at `base` (PE32+), or 0.
#[allow(unsafe_code, reason = "reads of a mapped module's headers")]
fn image_size(base: usize) -> usize {
    // SAFETY: `base` is a mapped module: its DOS header and NT headers are readable.
    unsafe {
        let nt = core::ptr::read_unaligned((base + 0x3C) as *const u32) as usize;
        if nt > 0x1000 {
            return 0;
        }
        core::ptr::read_unaligned((base + nt + 0x50) as *const u32) as usize
    }
}

/// The NUL terminated ASCII string at `rva` in the module at `base` of `size` bytes.
#[allow(unsafe_code, reason = "reads inside a mapped module")]
fn ascii_at(base: usize, rva: usize, size: usize) -> String {
    let end = size.min(rva.saturating_add(260));
    let bytes: Vec<u8> = (rva..end)
        .map(|at| {
            // SAFETY: inside the module's `size` mapped bytes (`end` is at most `size`).
            unsafe { core::ptr::read((base + at) as *const u8) }
        })
        .take_while(|b| *b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Says through the `drawing` setting whether the overlay draws in this game.
fn announce(host: &Host, drawing: bool) {
    if let Err(e) = host.set("drawing", if drawing { "true" } else { "false" }) {
        log::debug!("could not set drawing: {e}");
    }
}

/// The renderer's slot. When the thread ends it lets the renderer go without dropping it:
/// thread-local destructors run under the loader lock, and dropping a `Direct3D` 11 on 12
/// device there joins DXVK's worker threads, which need that lock to end. The game hung on
/// quit that way. The thread ending is the game shutting down, so nothing is lost.
struct Slot(Option<Overlay>);

impl Drop for Slot {
    fn drop(&mut self) {
        if let Some(overlay) = self.0.take() {
            std::mem::forget(overlay);
        }
    }
}

thread_local! {
    /// The renderer, made on the first frame that needs it.
    static OVERLAY: RefCell<Slot> = const { RefCell::new(Slot(None)) };
    /// Set while this thread is inside a frame: some implementations' `Present` calls
    /// `Present1` through the vtable, which would otherwise draw the overlay twice.
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
}

/// The device side of the overlay.
struct Overlay {
    gui: Gui,
    renderer: egui_directx11::Renderer,
    /// Before `backend`: the wrapped textures go before the device that wrapped them.
    natives: Natives,
    backend: Backend,
    started: Instant,
    last_drawn: Instant,
    /// The game's device this was made for ([`source_of`]).
    source: usize,
    /// When that device last presented, shown or not.
    own_present: Instant,
    /// The error the last frame failed with, said once until it changes.
    failing: Option<String>,
}

/// How long only other devices must present before the renderer follows them: a game that
/// made a new device (after losing one, or for new settings) presents only with that one,
/// while one that presents through two devices keeps presenting with the renderer's too.
const REPLACED_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

/// The device behind a swapchain, as an address to compare: its `Direct3D` 11 or 12 device,
/// 0 for neither.
fn source_of(chain: &IDXGISwapChain) -> usize {
    render::device_of(chain)
        .map(|d| d.as_raw() as usize)
        .or_else(|| d3d12::device_of(chain).map(|d| d.as_raw() as usize))
        .unwrap_or(0)
}

/// Notes a present on the device `source`; whether the renderer may draw on it. The renderer
/// is dropped (and made anew by the next draw) once only other devices presented for
/// [`REPLACED_AFTER`]: drawing a new device's backbuffer with the old one fails every frame,
/// and the references held keep the old device and its memory alive.
fn follow_device(slot: &mut Option<Overlay>, source: usize) -> bool {
    let Some(overlay) = slot.as_mut() else {
        return true;
    };
    if overlay.source == source {
        overlay.own_present = Instant::now();
        return true;
    }
    if overlay.own_present.elapsed() < REPLACED_AFTER {
        return false;
    }
    let on12 = matches!(overlay.backend, Backend::D3D12(_));
    *slot = None;
    if on12 {
        // The queue kept is the old device's: the new one's is taken on its next execute.
        d3d12::forget_queue();
        ON12_FAILED.store(false, Ordering::Relaxed);
    }
    log::info!("the game presents with a new device; the overlay renderer is made anew");
    true
}

/// Game textures panels show (`GPU_TEXTURE`), wrapped for the renderer while asked for.
#[derive(Default)]
struct Natives {
    shown: HashMap<usize, Native>,
    /// Handles that could not be wrapped (said once), not tried again while asked for.
    refused: HashSet<usize>,
}

/// One wrapped game texture: its `Direct3D` 11 resource (holding a reference to the
/// game's) and its id in the renderer.
struct Native {
    resource: ID3D11Resource,
    id: egui::TextureId,
}

impl Natives {
    /// After a pass: wraps what was asked for anew, lets go of what no longer is, tells
    /// the next passes what they can draw, and returns what this frame must acquire.
    fn prepare(
        &mut self,
        on12: &d3d12::On12,
        renderer: &mut egui_directx11::Renderer,
        asked: &[(usize, native::Owned)],
    ) -> Vec<Option<ID3D11Resource>> {
        let is_asked = |handle: &usize| asked.iter().any(|(h, _)| h == handle);
        self.shown.retain(|handle, native| {
            let keep = is_asked(handle);
            if !keep {
                renderer.unregister_user_texture(native.id);
            }
            keep
        });
        self.refused.retain(|handle| is_asked(handle));
        for (handle, resource) in asked {
            let handle = *handle;
            if self.shown.contains_key(&handle) || self.refused.contains(&handle) {
                continue;
            }
            match on12.wrap_texture(resource) {
                Ok((resource, view)) => {
                    let id = renderer.register_user_texture(view);
                    self.shown.insert(handle, Native { resource, id });
                }
                Err(e) => {
                    log::warn!("a panel's game texture cannot be shown: {e}");
                    self.refused.insert(handle);
                }
            }
        }
        native::set_registered(self.shown.iter().map(|(&h, n)| (h, n.id)).collect());
        self.shown
            .values()
            .map(|n| Some(n.resource.clone()))
            .collect()
    }
}

/// How long a hidden `Direct3D` 12 overlay keeps its renderer.
const IDLE: std::time::Duration = std::time::Duration::from_secs(3);

/// What the renderer draws with.
enum Backend {
    /// The game's own `Direct3D` 11 device.
    D3D11(ID3D11Device),
    /// A `Direct3D` 11 device over the game's `Direct3D` 12 queue.
    D3D12(d3d12::On12),
}

/// Hook DXGI. Called from the plugin's `start`.
pub(crate) fn install(host: &Host) {
    // Started before any present can arrive, so the first one measures from it.
    LazyLock::force(&EPOCH);
    hooks::install(host);
    gl::install();
    vk::install(host);
}

/// Let go of the game: the window procedure goes back and the `Direct3D` 12 queue reference
/// is released. The hooks keep forwarding to the originals, which are never cleared: they
/// stay reachable from the render thread until the loader has restored what they replaced,
/// and with `crate::host()` gone they do nothing else.
pub(crate) fn uninstall() {
    d3d12::forget_queue();
    wnd::unsubclass();
}

pub(crate) use wnd::{flush_input, inject};

/// One `Present`, before it reaches DXGI: present subscribers, then the overlay on top.
pub(crate) fn frame(swapchain: *mut c_void, sync_interval: u32, flags: u32) {
    // A test present draws nothing, so neither do we.
    if flags & DXGI_PRESENT_TEST.0 != 0 || IN_FRAME.with(Cell::get) {
        return;
    }
    DXGI_PRESENTED.store(millis_since_epoch() + 1, Ordering::Relaxed);
    let Some(host) = crate::host() else {
        // Stopped: the renderer lives on this (the render) thread, so it goes here.
        drop(OVERLAY.with_borrow_mut(|slot| slot.0.take()));
        return;
    };
    let Some(chain) = render::swapchain_of(swapchain) else {
        return;
    };
    IN_FRAME.with(|f| f.set(true));
    // The game's own code is below us: a panic unwinding into it would abort the process,
    // a fault would crash it. A fault disables the plugin (the guard skips it from then on).
    let outcome = uniloader_sdk::guard::call(|| {
        let source = source_of(&chain);
        if !DRAWABLE.load(Ordering::Relaxed) && source != 0 {
            DRAWABLE.store(true, Ordering::Relaxed);
            announce(host, true);
        }
        let desc = describe(&chain);
        wnd::subclass(desc.OutputWindow.0);
        subscribers(host, &chain, &desc, (sync_interval, flags));
        // Not ours: another device's swapchain, while the renderer's own still presents.
        let ours = OVERLAY.with_borrow_mut(|slot| follow_device(&mut slot.0, source));
        if ours && shared::is_visible() {
            OVERLAY.with_borrow_mut(|slot| draw(host, &mut slot.0, &chain));
        } else if ours {
            OVERLAY.with_borrow_mut(|slot| let_go_when_idle(&mut slot.0));
        }
    });
    IN_FRAME.with(|f| f.set(false));
    match outcome {
        Ok(()) | Err(uniloader_sdk::guard::Failed::Disabled) => {}
        Err(uniloader_sdk::guard::Failed::Panicked) => {
            log::error!("overlay frame panicked; the renderer is rebuilt on the next frame");
            drop(OVERLAY.with_borrow_mut(|slot| slot.0.take()));
        }
        Err(uniloader_sdk::guard::Failed::Faulted) => {
            log::error!("overlay frame faulted; the overlay is off for this session");
        }
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

/// Run the present subscribers, under the fault guard of each. `call` is the sync interval
/// and flags the game passed to `Present`.
fn subscribers(host: &Host, chain: &IDXGISwapChain, desc: &DXGI_SWAP_CHAIN_DESC, call: (u32, u32)) {
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
        sync_interval: call.0,
        flags: call.1,
    };
    for subscriber in &list {
        calls::present(host, subscriber, &info);
    }
}

/// Drops a `Direct3D` 11 on 12 renderer hidden for [`IDLE`], so a layer over the game's
/// queue is only alive while something is shown. It is made again when something is.
fn let_go_when_idle(slot: &mut Option<Overlay>) {
    let idle = slot
        .as_ref()
        .is_some_and(|o| matches!(o.backend, Backend::D3D12(_)) && o.last_drawn.elapsed() >= IDLE);
    if let Some(overlay) = slot.take_if(|_| idle) {
        // `On12` idles its context as it drops.
        drop(overlay);
        log::info!("overlay hidden; Direct3D 11 on 12 let go until it is shown again");
    }
}

/// Make the renderer if there is none yet, then draw one frame.
fn draw(host: &Host, slot: &mut Option<Overlay>, chain: &IDXGISwapChain) {
    if slot.is_none() {
        *slot = create(chain);
    }
    let Some(overlay) = slot else { return };
    overlay.last_drawn = Instant::now();
    match overlay.draw(host, chain) {
        Ok(()) => {
            if let Some(e) = overlay.failing.take() {
                log::info!("overlay frames draw again (they failed with: {e})");
            }
        }
        // Once per error, not every frame: a lost device fails each one the same way.
        Err(e) => {
            let text = e.to_string();
            if overlay.failing.as_deref() != Some(text.as_str()) {
                log::error!("overlay frame failed: {text} (said once while it lasts)");
                overlay.failing = Some(text);
            }
        }
    }
}

/// Build the renderer from the device behind the swapchain.
fn create(chain: &IDXGISwapChain) -> Option<Overlay> {
    let (backend, device) = if let Some(device) = render::device_of(chain) {
        (Backend::D3D11(device.clone()), device)
    } else if let Some(device12) = d3d12::device_of(chain) {
        if ON12_FAILED.load(Ordering::Relaxed) {
            return None;
        }
        match d3d12::On12::create(&device12) {
            // The game has not executed on a direct queue yet.
            Ok(None) => return None,
            Ok(Some(on12)) => {
                let device = on12.device.clone();
                (Backend::D3D12(on12), device)
            }
            Err(e) => {
                ON12_FAILED.store(true, Ordering::Relaxed);
                log::warn!("no Direct3D 11 on 12 ({e}); the overlay is not drawn");
                if let Some(host) = crate::host() {
                    announce(host, false);
                }
                return None;
            }
        }
    } else {
        if !UNKNOWN_DEVICE.swap(true, Ordering::Relaxed) {
            log::warn!(
                "the game's swapchain is neither Direct3D 11 nor 12; the overlay is not drawn"
            );
        }
        return None;
    };
    let renderer = egui_directx11::Renderer::new(&device)
        .inspect_err(|e| log::error!("could not create the overlay renderer: {e}"))
        .ok()?;
    let api = match backend {
        Backend::D3D11(_) => "Direct3D 11",
        Backend::D3D12(_) => "Direct3D 12, through 11 on 12",
    };
    log::info!("overlay ready ({api})");
    // Ids a renderer let go of earlier mean nothing to this one.
    native::set_registered(HashMap::new());
    Some(Overlay {
        gui: Gui::new(),
        renderer,
        natives: Natives::default(),
        backend,
        started: Instant::now(),
        last_drawn: Instant::now(),
        source: source_of(chain),
        own_present: Instant::now(),
        failing: None,
    })
}

impl Overlay {
    fn draw(&mut self, host: &Host, chain: &IDXGISwapChain) -> windows::core::Result<()> {
        let Self {
            gui,
            renderer,
            backend,
            natives,
            started,
            ..
        } = self;
        match backend {
            Backend::D3D11(device) => {
                let target = render::target(chain, device)?;
                let Some(context) = render::context(device) else {
                    return Ok(());
                };
                // Game textures are only wrapped over Direct3D 12.
                native::begin(false);
                let output = gui_pass(gui, *started, host, target.size);
                // The game keeps drawing with this context: its state goes back as it was,
                // also when the renderer panics halfway (the guard catches the unwind).
                let _restore = render::Restore::new(&context);
                renderer.render(&context, &target.view, gui.context(), output)
            }
            Backend::D3D12(on12) => {
                let wrapped = on12.wrap(chain)?;
                native::begin(true);
                let output = gui_pass(gui, *started, host, wrapped.target.size);
                let textures = natives.prepare(on12, renderer, &native::take_asked());
                on12.acquire(&textures);
                // Our own context: no game state on it to put back.
                let result =
                    renderer.render(&on12.context, &wrapped.target.view, gui.context(), output);
                on12.release(&textures);
                // Hands the buffer back before `Present`.
                drop(wrapped);
                result
            }
        }
    }
}

/// One egui pass for a backbuffer of `size`, input in, what to render out.
fn gui_pass(
    gui: &mut Gui,
    started: Instant,
    host: &Host,
    size: (u32, u32),
) -> egui_directx11::RendererOutput {
    #[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
    let (width, height) = (size.0 as f32, size.1 as f32);
    gui.rescale(height);
    wnd::set_screen(width, height);
    let raw = wnd::take_input(
        Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height)),
        started.elapsed().as_secs_f64(),
    );
    let pressed = if gui.recording() {
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
    let output = gui.run(host, input);
    let (renderer_output, platform, _viewports) = egui_directx11::split_output(output);
    // egui asks its host to copy; with no backend under us, that is this.
    for command in platform.commands {
        match command {
            egui::OutputCommand::CopyText(text) => clipboard::set_text(&text),
            egui::OutputCommand::OpenUrl(open) => shell::open_url(&open.url),
            // No image clipboard in a game.
            egui::OutputCommand::CopyImage(_) => {}
        }
    }
    renderer_output
}

#[cfg(test)]
mod tests {
    /// The test program's own imports, read the way the game's are: every Rust program on
    /// Windows imports kernel32, none of them Vulkan.
    #[test]
    fn the_executables_imports_are_read_from_its_image() {
        assert!(super::exe_imports("kernel32.dll"));
        assert!(super::exe_imports("KERNEL32.DLL"), "any case");
        assert!(!super::exe_imports("vulkan-1.dll"));
    }
}
