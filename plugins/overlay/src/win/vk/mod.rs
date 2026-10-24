//! Vulkan: detours on the driver's present, swapchain, queue and device functions, and egui
//! drawn into the swapchain image inside `vkQueuePresentKHR`.
//!
//! A throwaway instance and device ([`probe`]) tell which functions a game's calls end up in,
//! the same way the DXGI hook learns its vtable: `vkGetDeviceProcAddr` hands every device of
//! a driver the same functions, and the loader's exports dispatch to them too, so detouring
//! those addresses sees the game's calls however it made them. The handles the overlay needs
//! later (which device a queue belongs to, a swapchain's format and window) are recorded as
//! the game creates them, which is why the hooks must be in before the game makes its device:
//! `vulkan-1.dll` is hooked as soon as it is loaded, and watched for like `opengl32.dll`.
//!
//! Under Proton every `Direct3D` game presents through Vulkan too (DXVK, vkd3d-proton), right
//! after its DXGI `Present`. Those frames are the DXGI hook's, so the Vulkan path stands aside
//! while DXGI presents are coming in ([`super::dxgi_recent`]).
//!
//! The frame itself ([`render`]): a command buffer that waits for the game's semaphores,
//! draws egui into the image, and signals a semaphore of ours, which the present then waits
//! for instead.

// FFI module: detours, raw Vulkan handles and calls through function pointers.
#![allow(unsafe_code)]

mod render;
mod textures;

use core::ffi::{c_void, CStr};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use ash::vk::{self, Handle};
use uniloader_overlay::{GraphicsApi, PresentInfo};
use uniloader_sdk::Host;
use windows::core::{PCSTR, PCWSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use super::wnd;
use crate::{registry, shared};

/// How long a game is watched for loading `vulkan-1.dll` after the plugin started.
const WATCH_FOR: Duration = Duration::from_secs(60);
const WATCH_EVERY: Duration = Duration::from_millis(250);

/// Set once the hooks are in (or failed for good).
static DONE: AtomicBool = AtomicBool::new(false);
/// Set once a frame was drawable, and said through the `drawing` setting.
static DRAWABLE: AtomicBool = AtomicBool::new(false);
/// Set once a present on an unknown queue or swapchain was reported.
static UNKNOWN: AtomicBool = AtomicBool::new(false);

thread_local! {
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
    /// The slot this thread is drawing with, its lock held: a destroy detour re-entered for
    /// it (a plugin's callback during the draw) queues its work in [`LATER`] instead of
    /// waiting for a lock its own thread holds.
    static DRAWING: Cell<usize> = const { Cell::new(0) };
    /// Destroys that came while [`DRAWING`]; done after the draw.
    static LATER: std::cell::RefCell<Vec<Later>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A destroy deferred until the draw that was running ends.
enum Later {
    /// `vkDestroySwapchainKHR`: forget what the renderer had for it.
    Forget(vk::SwapchainKHR),
    /// `vkDestroyDevice`: the renderer's objects went with the device; drop it unused.
    DeviceGone,
}

/// Whether this thread is drawing with `slot` (so its lock is held right here).
fn drawing_with(slot: &Slot) -> bool {
    DRAWING.with(Cell::get) == Arc::as_ptr(slot) as usize
}

/// Marks this thread as drawing with a slot until dropped, also when the draw panics.
struct Drawing;

impl Drawing {
    fn start(slot: &Slot) -> Self {
        DRAWING.with(|d| d.set(Arc::as_ptr(slot) as usize));
        Drawing
    }
}

impl Drop for Drawing {
    fn drop(&mut self) {
        DRAWING.with(|d| d.set(0));
    }
}

/// The originals behind each detour, as addresses; zero when not hooked.
struct Real {
    create_device: AtomicUsize,
    destroy_device: AtomicUsize,
    get_device_queue: AtomicUsize,
    get_device_queue2: AtomicUsize,
    create_surface: AtomicUsize,
    create_swapchain: AtomicUsize,
    destroy_swapchain: AtomicUsize,
    queue_present: AtomicUsize,
}

static REAL: Real = Real {
    create_device: AtomicUsize::new(0),
    destroy_device: AtomicUsize::new(0),
    get_device_queue: AtomicUsize::new(0),
    get_device_queue2: AtomicUsize::new(0),
    create_surface: AtomicUsize::new(0),
    create_swapchain: AtomicUsize::new(0),
    destroy_swapchain: AtomicUsize::new(0),
    queue_present: AtomicUsize::new(0),
};

/// A swapchain as the game created it.
#[derive(Clone, Copy)]
pub(super) struct Swapchain {
    pub(super) device: vk::Device,
    pub(super) format: vk::Format,
    pub(super) extent: vk::Extent2D,
    pub(super) usage: vk::ImageUsageFlags,
    /// The window of its surface, when the surface was made after the hooks.
    hwnd: Option<isize>,
}

/// What the hooks recorded, and the renderers per device. Keys are raw handles.
#[derive(Default)]
struct Tracked {
    devices: HashMap<u64, vk::PhysicalDevice>,
    queues: HashMap<u64, (vk::Device, u32)>,
    surfaces: HashMap<u64, isize>,
    swapchains: HashMap<u64, Swapchain>,
    /// `None` once making one failed, so it is not tried every frame.
    renderers: HashMap<u64, Slot>,
}

/// One device's renderer, behind a lock of its own: a frame draws with only this held, never
/// [`TRACKED`], because drawing calls into plugins (panels, commands, settings) and one of
/// them making a Vulkan object on this thread would re-enter a detour that takes `TRACKED`.
/// Taken after `TRACKED` is released, never while it is held.
type Slot = Arc<Mutex<Option<render::Renderer>>>;

static TRACKED: LazyLock<Mutex<Tracked>> = LazyLock::new(Mutex::default);

fn tracked() -> MutexGuard<'static, Tracked> {
    TRACKED.lock().unwrap_or_else(PoisonError::into_inner)
}

fn lock(slot: &Slot) -> MutexGuard<'_, Option<render::Renderer>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `vulkan-1.dll`'s two entry points, all the overlay loads functions through.
#[derive(Clone, Copy)]
pub(super) struct Loader {
    module: HMODULE,
    pub(super) get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub(super) get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

// SAFETY: a module handle and two function pointers, usable from any thread.
unsafe impl Send for Loader {}
// SAFETY: as above; nothing in it changes.
unsafe impl Sync for Loader {}

static LOADER: std::sync::OnceLock<Loader> = std::sync::OnceLock::new();

pub(super) fn loader() -> Option<&'static Loader> {
    LOADER.get()
}

impl Loader {
    fn find() -> Option<Self> {
        let name: Vec<u16> = "vulkan-1.dll".encode_utf16().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated name; the game keeps the module loaded.
        let module = unsafe { GetModuleHandleW(PCWSTR(name.as_ptr())) }.ok()?;
        let export = |name: &CStr| {
            // SAFETY: a loaded module and a NUL-terminated name.
            unsafe { GetProcAddress(module, PCSTR(name.as_ptr().cast())) }
        };
        let instance_proc = export(c"vkGetInstanceProcAddr")?;
        let device_proc = export(c"vkGetDeviceProcAddr")?;
        // SAFETY: the exports of these names have these signatures.
        unsafe {
            Some(Self {
                module,
                get_instance_proc_addr: core::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    vk::PFN_vkGetInstanceProcAddr,
                >(instance_proc),
                get_device_proc_addr: core::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    vk::PFN_vkGetDeviceProcAddr,
                >(device_proc),
            })
        }
    }

    /// An export of `vulkan-1.dll`: the instance-level functions the overlay calls on the
    /// game's physical device dispatch through the handle, so the loader's do.
    pub(super) fn export(&self, name: &CStr) -> *const c_void {
        // SAFETY: a loaded module and a NUL-terminated name.
        unsafe { GetProcAddress(self.module, PCSTR(name.as_ptr().cast())) }
            .map_or(core::ptr::null(), |f| f as *const c_void)
    }

    /// A device function of `device`.
    pub(super) fn device_fn(&self, device: vk::Device, name: &CStr) -> *const c_void {
        // SAFETY: a live device and a NUL-terminated name.
        unsafe { (self.get_device_proc_addr)(device, name.as_ptr()) }
            .map_or(core::ptr::null(), |f| f as *const c_void)
    }
}

/// Hook Vulkan now if `vulkan-1.dll` is loaded, else watch for it a while.
pub(crate) fn install(host: &Host) {
    if try_install(host) {
        return;
    }
    let watcher = uniloader_sdk::guard::spawn("overlay-vk-watch", || {
        let until = Instant::now() + WATCH_FOR;
        while Instant::now() < until {
            std::thread::sleep(WATCH_EVERY);
            if crate::host().is_some_and(try_install) {
                return;
            }
        }
    });
    if let Err(e) = watcher {
        log::debug!("no thread to watch for Vulkan ({e})");
    }
}

/// `true` once the hooks are in, or failed for good.
fn try_install(host: &Host) -> bool {
    if DONE.load(Ordering::Acquire) {
        return true;
    }
    let Some(found) = Loader::find() else {
        return false;
    };
    let loader = *LOADER.get_or_init(|| found);
    DONE.store(true, Ordering::Release);
    let addresses = match probe(&loader) {
        Ok(a) => a,
        Err(e) => {
            log::warn!("no Vulkan device to learn from ({e}); no Vulkan overlay");
            return true;
        }
    };
    let hooks: [(*mut c_void, *mut c_void, &AtomicUsize, &str); 8] = [
        (
            addresses.create_device,
            create_device as *mut c_void,
            &REAL.create_device,
            "vkCreateDevice",
        ),
        (
            addresses.destroy_device,
            destroy_device as *mut c_void,
            &REAL.destroy_device,
            "vkDestroyDevice",
        ),
        (
            addresses.get_device_queue,
            get_device_queue as *mut c_void,
            &REAL.get_device_queue,
            "vkGetDeviceQueue",
        ),
        (
            addresses.get_device_queue2,
            get_device_queue2 as *mut c_void,
            &REAL.get_device_queue2,
            "vkGetDeviceQueue2",
        ),
        (
            addresses.create_surface,
            create_surface as *mut c_void,
            &REAL.create_surface,
            "vkCreateWin32SurfaceKHR",
        ),
        (
            addresses.create_swapchain,
            create_swapchain as *mut c_void,
            &REAL.create_swapchain,
            "vkCreateSwapchainKHR",
        ),
        (
            addresses.destroy_swapchain,
            destroy_swapchain as *mut c_void,
            &REAL.destroy_swapchain,
            "vkDestroySwapchainKHR",
        ),
        (
            addresses.queue_present,
            queue_present as *mut c_void,
            &REAL.queue_present,
            "vkQueuePresentKHR",
        ),
    ];
    for (target, replacement, real, name) in hooks {
        if target.is_null() {
            log::info!("no {name} to hook");
            continue;
        }
        // SAFETY: `target` is the driver's (or loader's) function of that name, and each
        // replacement below has its exact signature.
        match unsafe { host.detour(target, replacement, name) } {
            Ok((_, trampoline)) => real.store(trampoline as usize, Ordering::Release),
            Err(e) => log::warn!("could not hook {name}: {e}"),
        }
    }
    if REAL.queue_present.load(Ordering::Acquire) != 0 {
        log::info!("hooked vkQueuePresentKHR (Vulkan)");
    }
    true
}

/// The functions a game's calls end up in.
struct Addresses {
    create_device: *mut c_void,
    destroy_device: *mut c_void,
    get_device_queue: *mut c_void,
    get_device_queue2: *mut c_void,
    create_surface: *mut c_void,
    create_swapchain: *mut c_void,
    destroy_swapchain: *mut c_void,
    queue_present: *mut c_void,
}

/// Learn the addresses from a throwaway instance and device, then destroy both.
fn probe(loader: &Loader) -> Result<Addresses, String> {
    // SAFETY: vkGetInstanceProcAddr of the loaded vulkan-1.dll.
    let entry = unsafe {
        ash::Entry::from_static_fn(ash::StaticFn {
            get_instance_proc_addr: loader.get_instance_proc_addr,
        })
    };
    let app = vk::ApplicationInfo::default()
        .application_name(c"uniloader overlay probe")
        .api_version(vk::API_VERSION_1_1);
    let extensions = [c"VK_KHR_surface".as_ptr(), c"VK_KHR_win32_surface".as_ptr()];
    let create = vk::InstanceCreateInfo::default()
        .application_info(&app)
        .enabled_extension_names(&extensions);
    // SAFETY: a complete create info whose pointers outlive the call.
    let instance = unsafe { entry.create_instance(&create, None) }.map_err(|e| e.to_string())?;
    let result = probe_device(loader, &instance);
    // SAFETY: the instance made above; its device is gone by now.
    unsafe { instance.destroy_instance(None) };
    result
}

fn probe_device(loader: &Loader, instance: &ash::Instance) -> Result<Addresses, String> {
    let instance_fn = |name: &CStr| -> *mut c_void {
        // SAFETY: a live instance and a NUL-terminated name.
        unsafe { (loader.get_instance_proc_addr)(instance.handle(), name.as_ptr()) }
            .map_or(core::ptr::null_mut(), |f| f as *mut c_void)
    };
    // SAFETY: a live instance.
    let physical = unsafe { instance.enumerate_physical_devices() }
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or("no GPU")?;
    let priorities = [1.0f32];
    let queues = [vk::DeviceQueueCreateInfo::default()
        .queue_family_index(0)
        .queue_priorities(&priorities)];
    let extensions = [c"VK_KHR_swapchain".as_ptr()];
    let create = vk::DeviceCreateInfo::default()
        .queue_create_infos(&queues)
        .enabled_extension_names(&extensions);
    // SAFETY: a physical device of this instance and a complete create info.
    let device =
        unsafe { instance.create_device(physical, &create, None) }.map_err(|e| e.to_string())?;
    let device_fn = |name: &CStr| loader.device_fn(device.handle(), name).cast_mut();
    let addresses = Addresses {
        create_device: instance_fn(c"vkCreateDevice"),
        destroy_device: device_fn(c"vkDestroyDevice"),
        get_device_queue: device_fn(c"vkGetDeviceQueue"),
        get_device_queue2: device_fn(c"vkGetDeviceQueue2"),
        create_surface: instance_fn(c"vkCreateWin32SurfaceKHR"),
        create_swapchain: device_fn(c"vkCreateSwapchainKHR"),
        destroy_swapchain: device_fn(c"vkDestroySwapchainKHR"),
        queue_present: device_fn(c"vkQueuePresentKHR"),
    };
    // SAFETY: the device made above, unused from here on.
    unsafe { device.destroy_device(None) };
    Ok(addresses)
}

/// The original behind `real`, as `F`.
///
/// # Safety
/// `F` must be the signature of the function detoured into `real`.
unsafe fn original<F: Copy>(real: &AtomicUsize) -> Option<F> {
    let address = real.load(Ordering::Acquire);
    // SAFETY: a trampoline to a function of type `F`; a function pointer is a `usize` wide.
    (address != 0).then(|| unsafe { core::mem::transmute_copy::<usize, F>(&address) })
}

/// The overlay's bookkeeping inside a Vulkan hook. It runs while the plugin is disabled too
/// (skipping it would leave a destroyed device or swapchain tracked), so it only catches a
/// panic, which must not unwind into the game.
fn bookkeeping(hook: &str, f: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).is_err() {
        log::error!("{hook} hook: the overlay's bookkeeping panicked");
    }
}

unsafe extern "system" fn create_device(
    physical: vk::PhysicalDevice,
    info: *const vk::DeviceCreateInfo<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::Device,
) -> vk::Result {
    // SAFETY: detoured from vkCreateDevice.
    let Some(real) = (unsafe { original::<vk::PFN_vkCreateDevice>(&REAL.create_device) }) else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    // SAFETY: forwarding the caller's arguments.
    let result = unsafe { real(physical, info, allocator, out) };
    if result == vk::Result::SUCCESS && !out.is_null() {
        // SAFETY: written by the successful call.
        let device = unsafe { *out };
        bookkeeping("vkCreateDevice", || {
            tracked().devices.insert(device.as_raw(), physical);
        });
    }
    result
}

unsafe extern "system" fn destroy_device(
    device: vk::Device,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    // SAFETY: detoured from vkDestroyDevice.
    let Some(real) = (unsafe { original::<vk::PFN_vkDestroyDevice>(&REAL.destroy_device) }) else {
        return;
    };
    bookkeeping("vkDestroyDevice", || {
        let slot = {
            let mut t = tracked();
            t.devices.remove(&device.as_raw());
            t.queues.retain(|_, (d, _)| *d != device);
            t.swapchains.retain(|_, s| s.device != device);
            t.renderers.remove(&device.as_raw())
        };
        if let Some(slot) = slot {
            if drawing_with(&slot) {
                LATER.with_borrow_mut(|later| later.push(Later::DeviceGone));
            } else if let Some(mut renderer) = lock(&slot).take() {
                renderer.destroy();
            }
        }
    });
    // SAFETY: forwarding the caller's arguments.
    unsafe { real(device, allocator) };
}

unsafe extern "system" fn get_device_queue(
    device: vk::Device,
    family: u32,
    index: u32,
    out: *mut vk::Queue,
) {
    // SAFETY: detoured from vkGetDeviceQueue.
    let Some(real) = (unsafe { original::<vk::PFN_vkGetDeviceQueue>(&REAL.get_device_queue) })
    else {
        return;
    };
    // SAFETY: forwarding the caller's arguments.
    unsafe { real(device, family, index, out) };
    // SAFETY: written by the call when it is not null.
    if let Some(queue) = unsafe { out.as_ref() } {
        bookkeeping("vkGetDeviceQueue", || {
            tracked().queues.insert(queue.as_raw(), (device, family));
        });
    }
}

unsafe extern "system" fn get_device_queue2(
    device: vk::Device,
    info: *const vk::DeviceQueueInfo2<'_>,
    out: *mut vk::Queue,
) {
    // SAFETY: detoured from vkGetDeviceQueue2.
    let Some(real) = (unsafe { original::<vk::PFN_vkGetDeviceQueue2>(&REAL.get_device_queue2) })
    else {
        return;
    };
    // SAFETY: forwarding the caller's arguments.
    unsafe { real(device, info, out) };
    // SAFETY: the caller's create info, and the queue written by the call.
    if let (Some(info), Some(queue)) = unsafe { (info.as_ref(), out.as_ref()) } {
        bookkeeping("vkGetDeviceQueue2", || {
            tracked()
                .queues
                .insert(queue.as_raw(), (device, info.queue_family_index));
        });
    }
}

unsafe extern "system" fn create_surface(
    instance: vk::Instance,
    info: *const vk::Win32SurfaceCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SurfaceKHR,
) -> vk::Result {
    // SAFETY: detoured from vkCreateWin32SurfaceKHR.
    let Some(real) = (unsafe { original::<vk::PFN_vkCreateWin32SurfaceKHR>(&REAL.create_surface) })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    // SAFETY: forwarding the caller's arguments.
    let result = unsafe { real(instance, info, allocator, out) };
    if result == vk::Result::SUCCESS {
        // SAFETY: the caller's create info, and the surface written by the call.
        if let (Some(info), Some(surface)) = unsafe { (info.as_ref(), out.as_ref()) } {
            bookkeeping("vkCreateWin32SurfaceKHR", || {
                tracked().surfaces.insert(surface.as_raw(), info.hwnd);
            });
        }
    }
    result
}

unsafe extern "system" fn create_swapchain(
    device: vk::Device,
    info: *const vk::SwapchainCreateInfoKHR<'_>,
    allocator: *const vk::AllocationCallbacks<'_>,
    out: *mut vk::SwapchainKHR,
) -> vk::Result {
    // SAFETY: detoured from vkCreateSwapchainKHR.
    let Some(real) = (unsafe { original::<vk::PFN_vkCreateSwapchainKHR>(&REAL.create_swapchain) })
    else {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    };
    // SAFETY: forwarding the caller's arguments.
    let result = unsafe { real(device, info, allocator, out) };
    if result == vk::Result::SUCCESS {
        // SAFETY: the caller's create info, and the swapchain written by the call.
        if let (Some(info), Some(swapchain)) = unsafe { (info.as_ref(), out.as_ref()) } {
            bookkeeping("vkCreateSwapchainKHR", || {
                let mut t = tracked();
                let hwnd = t.surfaces.get(&info.surface.as_raw()).copied();
                t.swapchains.insert(
                    swapchain.as_raw(),
                    Swapchain {
                        device,
                        format: info.image_format,
                        extent: info.image_extent,
                        usage: info.image_usage,
                        hwnd,
                    },
                );
            });
        }
    }
    result
}

unsafe extern "system" fn destroy_swapchain(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks<'_>,
) {
    // SAFETY: detoured from vkDestroySwapchainKHR.
    let Some(real) =
        (unsafe { original::<vk::PFN_vkDestroySwapchainKHR>(&REAL.destroy_swapchain) })
    else {
        return;
    };
    bookkeeping("vkDestroySwapchainKHR", || {
        let slot = {
            let mut t = tracked();
            t.swapchains.remove(&swapchain.as_raw());
            t.renderers.get(&device.as_raw()).cloned()
        };
        if let Some(slot) = slot {
            if drawing_with(&slot) {
                LATER.with_borrow_mut(|later| later.push(Later::Forget(swapchain)));
            } else if let Some(renderer) = lock(&slot).as_mut() {
                renderer.forget(swapchain);
            }
        }
    });
    // SAFETY: forwarding the caller's arguments.
    unsafe { real(device, swapchain, allocator) };
}

unsafe extern "system" fn queue_present(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR<'_>,
) -> vk::Result {
    // SAFETY: detoured from vkQueuePresentKHR.
    let Some(real) = (unsafe { original::<vk::PFN_vkQueuePresentKHR>(&REAL.queue_present) }) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    // SAFETY: the caller's present info, valid for the call.
    let Some(present) = (unsafe { info.as_ref() }) else {
        // SAFETY: forwarding the caller's arguments.
        return unsafe { real(queue, info) };
    };
    let mut ours = None;
    if !IN_FRAME.with(Cell::get) {
        IN_FRAME.with(|f| f.set(true));
        // The game's own code is below us: a panic unwinding into it would abort the
        // process, a fault would crash it (a fault disables the plugin).
        match uniloader_sdk::guard::call(|| frame(queue, present)) {
            Ok(semaphore) => ours = semaphore,
            Err(
                why @ (uniloader_sdk::guard::Failed::Panicked
                | uniloader_sdk::guard::Failed::Faulted),
            ) => {
                log::error!("Vulkan overlay frame: {why:?}");
            }
            Err(_) => {}
        }
        IN_FRAME.with(|f| f.set(false));
    }
    match ours {
        Some(semaphore) => {
            // Our submission waited for the game's semaphores; the present waits for it.
            let mut replaced = *present;
            replaced.wait_semaphore_count = 1;
            replaced.p_wait_semaphores = &raw const semaphore;
            // SAFETY: the caller's present, waiting on our semaphore instead.
            unsafe { real(queue, &raw const replaced) }
        }
        // SAFETY: forwarding the caller's arguments.
        None => unsafe { real(queue, info) },
    }
}

/// One present: subscribers, then the overlay into the first swapchain's image. Returns the
/// semaphore the present must wait for instead of the game's, when something was submitted.
fn frame(queue: vk::Queue, present: &vk::PresentInfoKHR<'_>) -> Option<vk::Semaphore> {
    let host = crate::host()?;
    if super::dxgi_recent() || present.swapchain_count == 0 {
        return None;
    }
    // SAFETY: the present info's arrays hold `swapchain_count` entries.
    let (swapchain, index) = unsafe { (*present.p_swapchains, *present.p_image_indices) };
    let known = {
        let t = tracked();
        t.queues
            .get(&queue.as_raw())
            .copied()
            .zip(t.swapchains.get(&swapchain.as_raw()).copied())
    };
    let Some(((device, family), chain)) = known else {
        if !UNKNOWN.swap(true, Ordering::Relaxed) {
            log::warn!("a Vulkan present on a queue or swapchain made before the overlay loaded; not drawn");
        }
        return None;
    };
    if !DRAWABLE.swap(true, Ordering::Relaxed) {
        super::announce(host, true);
    }
    if let Some(hwnd) = chain.hwnd {
        wnd::subclass(hwnd as *mut c_void);
    }
    subscribers(host, swapchain, &chain);
    if !shared::is_visible() {
        return None;
    }
    let waits = if present.p_wait_semaphores.is_null() {
        &[][..]
    } else {
        // SAFETY: the present info's array holds `wait_semaphore_count` entries.
        unsafe {
            core::slice::from_raw_parts(
                present.p_wait_semaphores,
                present.wait_semaphore_count as usize,
            )
        }
    };
    let slot = {
        let mut t = tracked();
        let physical = *t.devices.get(&device.as_raw())?;
        Arc::clone(
            t.renderers
                .entry(device.as_raw())
                .or_insert_with(|| Arc::new(Mutex::new(render::Renderer::new(device, physical)))),
        )
    };
    let mut renderer = lock(&slot);
    let drawn = {
        let _drawing = Drawing::start(&slot);
        renderer.as_mut()?.draw(
            host,
            &render::Present {
                queue,
                family,
                swapchain,
                chain,
                index,
                waits,
            },
        )
    };
    let mut device_gone = false;
    for later in LATER.take() {
        match later {
            Later::Forget(gone) => {
                if let Some(renderer) = renderer.as_mut() {
                    renderer.forget(gone);
                }
            }
            Later::DeviceGone => device_gone = true,
        }
    }
    if device_gone {
        log::warn!("Vulkan device destroyed during the overlay's own frame; renderer dropped");
        drop(renderer.take());
        return None;
    }
    drawn
}

/// Run the present subscribers for a Vulkan frame.
fn subscribers(host: &Host, swapchain: vk::SwapchainKHR, chain: &Swapchain) {
    let list = registry::present_subscribers();
    if list.is_empty() {
        return;
    }
    let info = PresentInfo {
        struct_size: core::mem::size_of::<PresentInfo>(),
        api: GraphicsApi::VULKAN,
        swapchain: swapchain.as_raw() as *mut c_void,
        device: chain.device.as_raw() as *mut c_void,
        hwnd: chain
            .hwnd
            .map_or(core::ptr::null_mut(), |h| h as *mut c_void),
        width: chain.extent.width,
        height: chain.extent.height,
        sync_interval: 0,
        flags: 0,
    };
    for subscriber in &list {
        crate::ui::calls::present(host, subscriber, &info);
    }
}
