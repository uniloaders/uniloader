//! `Direct3D` 12: finding its swapchain class and the game's queue, and drawing through
//! `Direct3D` 11 on 12.
//!
//! Under Proton a `Direct3D` 12 game presents through vkd3d-proton's swapchain, a class of its
//! own, so the vtable the `Direct3D` 11 probe patched is never called. A throwaway `Direct3D`
//! 12 swapchain finds that class the same way ([`probe`]). Drawing needs no renderer of its
//! own: a `Direct3D` 11 device layered over the game's command queue
//! (`D3D11On12CreateDevice`, which Windows has and DXVK implements over vkd3d-proton) runs
//! the same egui renderer into the backbuffer, wrapped for the one frame ([`On12`]).
//!
//! A swapchain does not say which queue presents it, so the first direct queue that
//! executes command lists is taken: `ExecuteCommandLists` is hooked through the throwaway
//! queue, whose class the game's queues share.

// FFI module: COM objects, vtable slots and calls through raw function pointers.
#![allow(unsafe_code)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use uniloader_sdk::Host;
use windows::core::{IUnknown, Interface};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
};
use windows::Win32::Graphics::Direct3D11on12::{
    D3D11On12CreateDevice, ID3D11On12Device, D3D11_RESOURCE_FLAGS,
};
use windows::Win32::Graphics::Direct3D12::{
    D3D12CreateDevice, ID3D12CommandQueue, ID3D12Device, ID3D12Resource,
    D3D12_COMMAND_LIST_TYPE_DIRECT, D3D12_COMMAND_QUEUE_DESC, D3D12_RESOURCE_STATE_PRESENT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory2, IDXGISwapChain, IDXGISwapChain3, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use super::render;

/// `ID3D12CommandQueue::ExecuteCommandLists`: `IUnknown` (3), `ID3D12Object` (4),
/// `ID3D12DeviceChild` (1), then `UpdateTileMappings` and `CopyTileMappings`.
const EXECUTE_COMMAND_LISTS: u32 = 10;

type ExecuteFn = unsafe extern "system" fn(*mut c_void, u32, *const *mut c_void);

/// The function the slot held before; zero means "not hooked".
static REAL_EXECUTE: AtomicUsize = AtomicUsize::new(0);
/// The game's direct queue, holding one reference of ours; zero until one executes.
static QUEUE: AtomicUsize = AtomicUsize::new(0);

/// A throwaway `Direct3D` 12 swapchain on `hwnd`, and the queue it was made with.
pub(super) fn probe(hwnd: HWND) -> windows::core::Result<(IDXGISwapChain, ID3D12CommandQueue)> {
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: `device` is a valid out-pointer for the created device.
    unsafe { D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &raw mut device) }?;
    let device = device.ok_or_else(windows::core::Error::empty)?;
    let queue_desc = D3D12_COMMAND_QUEUE_DESC {
        Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
        ..D3D12_COMMAND_QUEUE_DESC::default()
    };
    // SAFETY: a live device and a complete descriptor.
    let queue: ID3D12CommandQueue = unsafe { device.CreateCommandQueue(&raw const queue_desc) }?;
    // SAFETY: no arguments; the factory is released with its last reference.
    let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1() }?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: 64,
        Height: 64,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        ..DXGI_SWAP_CHAIN_DESC1::default()
    };
    // SAFETY: a live queue and window, and a complete descriptor.
    let chain =
        unsafe { factory.CreateSwapChainForHwnd(&queue, hwnd, &raw const desc, None, None) }?;
    Ok((chain.cast()?, queue))
}

/// Hooks `ExecuteCommandLists` through `queue`, to learn the game's queue.
pub(super) fn hook_queue(host: &Host, queue: &ID3D12CommandQueue) {
    let note = "ID3D12CommandQueue::ExecuteCommandLists";
    // SAFETY: `queue` is a live command queue, whose vtable has the slot, and
    // `execute_command_lists` has its signature.
    let hooked = unsafe {
        host.hook_vtable(
            queue.as_raw(),
            EXECUTE_COMMAND_LISTS,
            execute_command_lists as *mut c_void,
            note,
        )
    };
    match hooked {
        Ok((_, previous)) => {
            REAL_EXECUTE.store(previous as usize, Ordering::Release);
            log::info!("hooked {note}");
        }
        Err(e) => log::warn!("could not hook {note}: {e}"),
    }
}

/// Stops forwarding, and lets go of the queue.
pub(super) fn uninstall() {
    REAL_EXECUTE.store(0, Ordering::Release);
    let queue = QUEUE.swap(0, Ordering::AcqRel);
    if queue != 0 {
        // SAFETY: the reference taken in `execute_command_lists`, given back once.
        drop(unsafe { ID3D12CommandQueue::from_raw(queue as *mut c_void) });
    }
}

/// The game's direct queue, once one has executed.
fn game_queue() -> Option<ID3D12CommandQueue> {
    let raw = QUEUE.load(Ordering::Acquire) as *mut c_void;
    if raw.is_null() {
        return None;
    }
    // SAFETY: the queue holds a reference of ours until `uninstall`; this adds another.
    unsafe { ID3D12CommandQueue::from_raw_borrowed(&raw) }.cloned()
}

unsafe extern "system" fn execute_command_lists(
    this: *mut c_void,
    count: u32,
    lists: *const *mut c_void,
) {
    let address = REAL_EXECUTE.load(Ordering::Acquire);
    if address == 0 {
        return;
    }
    if QUEUE.load(Ordering::Acquire) == 0 {
        // SAFETY: `this` is the queue the game called through; borrowed for the call.
        if let Some(queue) = unsafe { ID3D12CommandQueue::from_raw_borrowed(&this) } {
            // SAFETY: a live queue; only its description is read.
            if unsafe { queue.GetDesc() }.Type == D3D12_COMMAND_LIST_TYPE_DIRECT {
                let owned = queue.clone().into_raw() as usize;
                if QUEUE
                    .compare_exchange(0, owned, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    // SAFETY: the reference just taken, which lost the race.
                    drop(unsafe { ID3D12CommandQueue::from_raw(owned as *mut c_void) });
                }
            }
        }
    }
    // SAFETY: the slot held an `ExecuteFn`, and a function pointer is a `usize` wide.
    let real: ExecuteFn = unsafe { core::mem::transmute::<usize, ExecuteFn>(address) };
    // SAFETY: forwarding the caller's arguments to the original.
    unsafe { real(this, count, lists) };
}

/// The `Direct3D` 12 device behind a swapchain, when it is one.
pub(super) fn device_of(chain: &IDXGISwapChain) -> Option<ID3D12Device> {
    // SAFETY: a live swapchain; `GetDevice` only queries an interface from it.
    unsafe { chain.GetDevice::<ID3D12Device>() }.ok()
}

/// A `Direct3D` 11 device over the game's queue.
pub(super) struct On12 {
    /// The layer's own interface: wrapping resources.
    layer: ID3D11On12Device,
    pub(super) device: ID3D11Device,
    pub(super) context: ID3D11DeviceContext,
}

/// One backbuffer, wrapped for `Direct3D` 11 for the frame.
pub(super) struct Wrapped {
    resource: ID3D11Resource,
    pub(super) target: render::Target,
}

impl On12 {
    /// Layers a `Direct3D` 11 device over `device` and the game's queue; `None` (said once
    /// by the caller) until the game has executed on a direct queue.
    pub(super) fn create(device: &ID3D12Device) -> windows::core::Result<Option<Self>> {
        let Some(queue) = game_queue() else {
            return Ok(None);
        };
        let queues = [Some(queue.cast::<IUnknown>()?)];
        let (mut device11, mut context) = (None, None);
        // SAFETY: a live device and queue; the out-pointers are valid for the call.
        unsafe {
            D3D11On12CreateDevice(
                device,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
                None,
                Some(&queues),
                0,
                Some(&raw mut device11),
                Some(&raw mut context),
                None,
            )
        }?;
        let device = device11.ok_or_else(windows::core::Error::empty)?;
        let context = context.ok_or_else(windows::core::Error::empty)?;
        Ok(Some(Self {
            layer: device.cast()?,
            device,
            context,
        }))
    }

    /// Wraps the backbuffer the game is about to present, and acquires it.
    pub(super) fn wrap(&self, chain: &IDXGISwapChain) -> windows::core::Result<Wrapped> {
        let chain3: IDXGISwapChain3 = chain.cast()?;
        // SAFETY: a live swapchain; the index names one of its buffers.
        let buffer: ID3D12Resource =
            unsafe { chain3.GetBuffer(chain3.GetCurrentBackBufferIndex()) }?;
        let flags = D3D11_RESOURCE_FLAGS {
            BindFlags: D3D11_BIND_RENDER_TARGET.0.cast_unsigned(),
            ..D3D11_RESOURCE_FLAGS::default()
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: a live resource of the device under the layer, and valid flags; the buffer
        // is in the present state before and after.
        unsafe {
            self.layer.CreateWrappedResource(
                &buffer,
                &raw const flags,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_PRESENT,
                &raw mut texture,
            )
        }?;
        let texture = texture.ok_or_else(windows::core::Error::empty)?;
        let target = render::target_of(&texture, &self.device)?;
        let resource: ID3D11Resource = texture.cast()?;
        let acquired = [Some(resource.clone())];
        // SAFETY: a resource wrapped by this device, acquired once and released in `finish`.
        unsafe { self.layer.AcquireWrappedResources(&acquired) };
        Ok(Wrapped { resource, target })
    }

    /// Hands the backbuffer back to `Direct3D` 12 and submits what was drawn, so nothing of
    /// ours holds it once `Present` (or a resize) comes.
    pub(super) fn finish(&self, wrapped: Wrapped) {
        let Wrapped { resource, target } = wrapped;
        drop(target);
        // SAFETY: the resource acquired in `wrap`.
        unsafe { self.layer.ReleaseWrappedResources(&[Some(resource)]) };
        // SAFETY: a live immediate context.
        unsafe { self.context.Flush() };
    }
}

/// Whether two COM objects share a vtable, that is, are of one class.
pub(super) fn same_class(a: &IUnknown, b: &IUnknown) -> bool {
    // SAFETY: both are live COM objects, whose first word is their vtable pointer.
    unsafe { *a.as_raw().cast::<*const c_void>() == *b.as_raw().cast::<*const c_void>() }
}
