//! Render target handling and the device state the overlay must put back.
//!
//! `egui-directx11` sets up its own pipeline state and documents that restoring the previous
//! one is the caller's job. Here that is not optional: the game keeps drawing with the same
//! device context on the next frame, and `Direct3D` 11 state is sticky, so an overlay that
//! leaves its own shaders bound corrupts the game's rendering.

// FFI module: Direct3D 11 interfaces and raw pointers.
#![allow(unsafe_code)]

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11BlendState, ID3D11Buffer, ID3D11DepthStencilView, ID3D11Device, ID3D11DeviceContext,
    ID3D11InputLayout, ID3D11PixelShader, ID3D11RasterizerState, ID3D11RenderTargetView,
    ID3D11SamplerState, ID3D11ShaderResourceView, ID3D11Texture2D, ID3D11VertexShader,
    D3D11_RENDER_TARGET_VIEW_DESC, D3D11_RENDER_TARGET_VIEW_DESC_0, D3D11_RTV_DIMENSION_TEXTURE2D,
    D3D11_TEX2D_RTV, D3D11_TEXTURE2D_DESC, D3D11_VIEWPORT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
};
use windows::Win32::Graphics::Dxgi::IDXGISwapChain;

/// A render target view of a swapchain's backbuffer, with the size it had when it was made.
pub(crate) struct Target {
    /// The view the overlay draws through.
    pub(crate) view: ID3D11RenderTargetView,
    /// Backbuffer size in pixels.
    pub(crate) size: (u32, u32),
}

/// Build a render target view for the swapchain's backbuffer.
///
/// The view's format is deliberately the non-sRGB form of whatever the backbuffer is: egui
/// blends in gamma space, and `egui-directx11` requires a target that is not sRGB-aware, or
/// every colour comes out wrong.
pub(crate) fn target(
    swapchain: &IDXGISwapChain,
    device: &ID3D11Device,
) -> windows::core::Result<Target> {
    // SAFETY: `swapchain` is a live swapchain; buffer 0 always exists.
    let backbuffer: ID3D11Texture2D = unsafe { swapchain.GetBuffer(0) }?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: `backbuffer` is a live texture and `desc` is a valid out-pointer.
    unsafe { backbuffer.GetDesc(&raw mut desc) };
    let view_desc = D3D11_RENDER_TARGET_VIEW_DESC {
        Format: gamma_format(desc.Format),
        ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 },
        },
    };
    let mut view = None;
    // SAFETY: the texture and the descriptor are both valid, and `view` is a valid
    // out-pointer for the created view.
    unsafe {
        device.CreateRenderTargetView(&backbuffer, Some(&raw const view_desc), Some(&raw mut view))
    }?;
    view.map(|view| Target {
        view,
        size: (desc.Width, desc.Height),
    })
    .ok_or_else(windows::core::Error::from_thread)
}

/// The non-sRGB view format for a backbuffer format, or the format itself when it has none.
fn gamma_format(format: DXGI_FORMAT) -> DXGI_FORMAT {
    match format {
        DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => DXGI_FORMAT_B8G8R8A8_UNORM,
        other => other,
    }
}

/// Everything `egui-directx11` overwrites, as it was before.
///
/// Only the stages the renderer documents are saved. The hull, domain, geometry and compute
/// stages are untouched by it, and the depth stencil state is only read, never set.
pub(crate) struct Saved {
    input_layout: Option<ID3D11InputLayout>,
    vertex_buffer: Option<ID3D11Buffer>,
    vertex_stride: u32,
    vertex_offset: u32,
    index_buffer: Option<ID3D11Buffer>,
    index_format: DXGI_FORMAT,
    index_offset: u32,
    topology: D3D_PRIMITIVE_TOPOLOGY,
    vertex_shader: Option<ID3D11VertexShader>,
    pixel_shader: Option<ID3D11PixelShader>,
    sampler: Option<ID3D11SamplerState>,
    resource: Option<ID3D11ShaderResourceView>,
    rasterizer: Option<ID3D11RasterizerState>,
    viewports: Vec<D3D11_VIEWPORT>,
    render_target: Option<ID3D11RenderTargetView>,
    depth: Option<ID3D11DepthStencilView>,
    blend: Option<ID3D11BlendState>,
    blend_factor: [f32; 4],
    blend_mask: u32,
}

/// Read back the pipeline state the overlay is about to change.
pub(crate) fn save(ctx: &ID3D11DeviceContext) -> Saved {
    let mut saved = Saved {
        input_layout: None,
        vertex_buffer: None,
        vertex_stride: 0,
        vertex_offset: 0,
        index_buffer: None,
        index_format: DXGI_FORMAT::default(),
        index_offset: 0,
        topology: D3D_PRIMITIVE_TOPOLOGY::default(),
        vertex_shader: None,
        pixel_shader: None,
        sampler: None,
        resource: None,
        rasterizer: None,
        viewports: Vec::new(),
        render_target: None,
        depth: None,
        blend: None,
        blend_factor: [0.0; 4],
        blend_mask: 0,
    };
    // SAFETY: every call below only reads state out of a live device context into locals of
    // the type the signature asks for. The `Get` calls add a reference to what they return,
    // which the `Option<Interface>` locals own and release.
    unsafe {
        saved.input_layout = ctx.IAGetInputLayout().ok();
        ctx.IAGetVertexBuffers(
            0,
            1,
            Some(&raw mut saved.vertex_buffer),
            Some(&raw mut saved.vertex_stride),
            Some(&raw mut saved.vertex_offset),
        );
        ctx.IAGetIndexBuffer(
            Some(&raw mut saved.index_buffer),
            Some(&raw mut saved.index_format),
            Some(&raw mut saved.index_offset),
        );
        saved.topology = ctx.IAGetPrimitiveTopology();
        ctx.VSGetShader(&raw mut saved.vertex_shader, None, None);
        ctx.PSGetShader(&raw mut saved.pixel_shader, None, None);
        let mut samplers = [None];
        ctx.PSGetSamplers(0, Some(&mut samplers));
        saved.sampler = samplers[0].take();
        let mut resources = [None];
        ctx.PSGetShaderResources(0, Some(&mut resources));
        saved.resource = resources[0].take();
        saved.rasterizer = ctx.RSGetState().ok();
        let mut count = 0;
        ctx.RSGetViewports(&raw mut count, None);
        if count > 0 {
            saved.viewports = vec![D3D11_VIEWPORT::default(); count as usize];
            ctx.RSGetViewports(&raw mut count, Some(saved.viewports.as_mut_ptr()));
        }
        let mut targets = [None];
        ctx.OMGetRenderTargets(Some(&mut targets), Some(&raw mut saved.depth));
        saved.render_target = targets[0].take();
        ctx.OMGetBlendState(
            Some(&raw mut saved.blend),
            Some(&mut saved.blend_factor),
            Some(&raw mut saved.blend_mask),
        );
    }
    saved
}

/// Put the state back exactly as [`save`] found it.
pub(crate) fn restore(ctx: &ID3D11DeviceContext, saved: &Saved) {
    // SAFETY: every value below came out of this context's own `Get` call, so each is either
    // null or a live object of the right type for the matching `Set` call.
    unsafe {
        ctx.IASetInputLayout(saved.input_layout.as_ref());
        ctx.IASetVertexBuffers(
            0,
            1,
            Some(&raw const saved.vertex_buffer),
            Some(&raw const saved.vertex_stride),
            Some(&raw const saved.vertex_offset),
        );
        ctx.IASetIndexBuffer(
            saved.index_buffer.as_ref(),
            saved.index_format,
            saved.index_offset,
        );
        ctx.IASetPrimitiveTopology(saved.topology);
        ctx.VSSetShader(saved.vertex_shader.as_ref(), None);
        ctx.PSSetShader(saved.pixel_shader.as_ref(), None);
        ctx.PSSetSamplers(0, Some(core::slice::from_ref(&saved.sampler)));
        ctx.PSSetShaderResources(0, Some(core::slice::from_ref(&saved.resource)));
        ctx.RSSetState(saved.rasterizer.as_ref());
        if !saved.viewports.is_empty() {
            ctx.RSSetViewports(Some(&saved.viewports));
        }
        ctx.OMSetRenderTargets(
            Some(core::slice::from_ref(&saved.render_target)),
            saved.depth.as_ref(),
        );
        ctx.OMSetBlendState(
            saved.blend.as_ref(),
            Some(&saved.blend_factor),
            saved.blend_mask,
        );
    }
}

/// The immediate context of a device.
pub(crate) fn context(device: &ID3D11Device) -> Option<ID3D11DeviceContext> {
    // SAFETY: `device` is live; the call only hands back its immediate context.
    unsafe { device.GetImmediateContext() }.ok()
}

/// The device behind a swapchain, as a borrowed interface.
pub(crate) fn device_of(swapchain: &IDXGISwapChain) -> Option<ID3D11Device> {
    // SAFETY: `swapchain` is live; `GetDevice` only queries an interface from it.
    unsafe { swapchain.GetDevice::<ID3D11Device>() }.ok()
}

/// Borrow a raw `IDXGISwapChain` pointer without taking a reference.
pub(crate) fn swapchain_of(raw: *mut core::ffi::c_void) -> Option<IDXGISwapChain> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` came from the overlay's own `Present` hook, so it is a live swapchain for
    // the duration of that call; `from_raw_borrowed` does not take ownership.
    unsafe { IDXGISwapChain::from_raw_borrowed(&raw) }.cloned()
}
