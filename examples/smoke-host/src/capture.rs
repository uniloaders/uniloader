//! Read a frame back from the GPU and save it, so the smoke test can see what the overlay
//! drew: how many pixels differ from the colour the host cleared to, and a `capture.bmp` a
//! person can look at.

// FFI module: staging textures and mapped memory.
#![allow(unsafe_code)]

use std::path::Path;

use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};

/// One frame as tightly packed RGBA rows, top row first.
pub(crate) struct Frame {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// Copy `texture` (an RGBA8 render target) into memory.
pub(crate) fn read(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    texture: &ID3D11Texture2D,
) -> windows::core::Result<Frame> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: a live texture and a valid out-pointer.
    unsafe { texture.GetDesc(&raw mut desc) };
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0.cast_unsigned(),
        MiscFlags: 0,
        ..desc
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    // SAFETY: a complete descriptor and a valid out-pointer.
    unsafe { device.CreateTexture2D(&raw const staging_desc, None, Some(&raw mut staging)) }?;
    let staging = staging.ok_or_else(windows::core::Error::empty)?;
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: both textures are live and of the same size and format; the map is undone below.
    unsafe {
        context.CopyResource(&staging, texture);
        context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&raw mut mapped))?;
    }
    let row = desc.Width as usize * 4;
    let mut rgba = Vec::with_capacity(row * desc.Height as usize);
    for y in 0..desc.Height as usize {
        // SAFETY: the mapped memory holds `Height` rows of `RowPitch` bytes, each starting
        // with `Width` RGBA pixels.
        let line = unsafe {
            core::slice::from_raw_parts(
                mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                row,
            )
        };
        rgba.extend_from_slice(line);
    }
    // SAFETY: mapped above.
    unsafe { context.Unmap(&staging, 0) };
    Ok(Frame {
        width: desc.Width,
        height: desc.Height,
        rgba,
    })
}

impl Frame {
    /// How many pixels are not `clear` (RGBA bytes).
    pub(crate) fn differing(&self, clear: [u8; 4]) -> usize {
        let (pixels, _) = self.rgba.as_chunks::<4>();
        pixels
            .iter()
            .filter(|pixel| pixel[..3] != clear[..3])
            .count()
    }

    /// Write a 32-bit BMP.
    pub(crate) fn save_bmp(&self, path: &Path) -> std::io::Result<()> {
        let pixels = u32::try_from(self.rgba.len()).unwrap_or(u32::MAX);
        let mut out = Vec::with_capacity(54 + self.rgba.len());
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(54 + pixels).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&self.width.to_le_bytes());
        // Negative height: rows top first, as they are stored here.
        out.extend_from_slice(&(-i32::try_from(self.height).unwrap_or(0)).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&pixels.to_le_bytes());
        out.extend_from_slice(&[0; 16]);
        let (pixels, _) = self.rgba.as_chunks::<4>();
        for [r, g, b, _] in pixels {
            out.extend_from_slice(&[*b, *g, *r, 255]);
        }
        std::fs::write(path, out)
    }
}
