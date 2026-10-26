//! egui on `OpenGL` 3.0 and later, through glow, in whatever context the game made current.
//!
//! The game keeps drawing with the same context on the next frame, and `OpenGL` state is all
//! global to it, so every piece of state the painter touches is read first and put back after
//! ([`Saved`]). The painter draws into the default framebuffer, which is the window's back
//! buffer `SwapBuffers` is about to show.
//!
//! Colours: egui hands over premultiplied sRGB, vertices and textures alike, and the back
//! buffer of a plain pixel format stores sRGB values as they are; so nothing is converted, and
//! `GL_FRAMEBUFFER_SRGB` is switched off while drawing in case the game enabled it.

// FFI module: every glow call is unsafe.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::num::NonZeroU32;

use egui::epaint::{ImageDelta, Primitive, Vertex};
use egui::{TextureFilter, TextureId, TextureWrapMode};
use glow::HasContext;

const VERTEX: &str = r"
uniform vec2 u_screen;
in vec2 a_pos;
in vec2 a_uv;
in vec4 a_color;
out vec2 v_uv;
out vec4 v_color;
void main() {
    gl_Position = vec4(2.0 * a_pos.x / u_screen.x - 1.0, 1.0 - 2.0 * a_pos.y / u_screen.y, 0.0, 1.0);
    v_uv = a_uv;
    v_color = a_color;
}
";

const FRAGMENT: &str = r"
uniform sampler2D u_sampler;
in vec2 v_uv;
in vec4 v_color;
out vec4 frag;
void main() {
    frag = v_color * texture(u_sampler, v_uv);
}
";

/// The GPU side: one program, one vertex layout, egui's textures.
pub(super) struct Painter {
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    u_screen: Option<glow::UniformLocation>,
    u_sampler: Option<glow::UniformLocation>,
    textures: HashMap<TextureId, glow::Texture>,
    /// Sampler objects exist from 3.3; one bound by the game would override our filtering.
    samplers: bool,
}

impl Painter {
    /// A painter for the current context, with functions from `load`.
    ///
    /// # Safety
    /// A context must be current on this thread, and `load` must return its functions.
    pub(super) unsafe fn new(
        load: impl FnMut(&core::ffi::CStr) -> *const core::ffi::c_void,
    ) -> Result<Self, String> {
        // SAFETY: the caller's contract.
        let gl = unsafe { glow::Context::from_loader_function_cstr(load) };
        let version = gl.version();
        if version.is_embedded || version.major < 3 {
            return Err(format!(
                "OpenGL {}.{} is older than 3.0",
                version.major, version.minor
            ));
        }
        let (major, minor) = (version.major, version.minor);
        // Core profiles refuse GLSL 1.30; 1.50 needs 3.2.
        let glsl = if (major, minor) >= (3, 2) {
            "#version 150\n"
        } else {
            "#version 130\n"
        };
        // SAFETY: the context is current (the caller's contract) and every object made here
        // belongs to it.
        unsafe {
            let program = link(&gl, glsl)?;
            let vao = gl.create_vertex_array()?;
            let vbo = gl.create_buffer()?;
            let ebo = gl.create_buffer()?;
            Ok(Self {
                u_screen: gl.get_uniform_location(program, "u_screen"),
                u_sampler: gl.get_uniform_location(program, "u_sampler"),
                samplers: (major, minor) >= (3, 3),
                gl,
                program,
                vao,
                vbo,
                ebo,
                textures: HashMap::new(),
            })
        }
    }

    /// Draw one frame of egui output onto the default framebuffer of `size` pixels.
    pub(super) fn paint(
        &mut self,
        ctx: &egui::Context,
        output: egui_directx11::RendererOutput,
        size: (u32, u32),
    ) {
        let ppp = output.pixels_per_point;
        let primitives = ctx.tessellate(output.shapes, ppp);
        // SAFETY: the context this painter was made in is current: the caller checks that
        // it is the same one before every frame.
        unsafe {
            let saved = Saved::read(&self.gl, self.samplers);
            for (id, delta) in &output.textures_delta.set {
                self.set_texture(*id, delta);
            }
            self.prepare(size, ppp);
            for clipped in &primitives {
                if let Primitive::Mesh(mesh) = &clipped.primitive {
                    self.draw_mesh(mesh, clipped.clip_rect, size, ppp);
                }
            }
            for id in &output.textures_delta.free {
                if let Some(texture) = self.textures.remove(id) {
                    self.gl.delete_texture(texture);
                }
            }
            saved.restore(&self.gl, self.samplers);
        }
    }

    /// Pipeline state for egui: premultiplied blending, scissoring, nothing else.
    unsafe fn prepare(&self, size: (u32, u32), ppp: f32) {
        let gl = &self.gl;
        let (width, height) = to_i32(size);
        // SAFETY: the caller's contract (a current context).
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.viewport(0, 0, width, height);
            gl.enable(glow::BLEND);
            gl.blend_equation_separate(glow::FUNC_ADD, glow::FUNC_ADD);
            gl.blend_func_separate(
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE_MINUS_DST_ALPHA,
                glow::ONE,
            );
            gl.enable(glow::SCISSOR_TEST);
            for cap in [
                glow::CULL_FACE,
                glow::DEPTH_TEST,
                glow::STENCIL_TEST,
                glow::FRAMEBUFFER_SRGB,
                glow::RASTERIZER_DISCARD,
            ] {
                gl.disable(cap);
            }
            // A game that left channels masked (after a depth-only pass) would get a
            // half-drawn overlay, or none.
            gl.color_mask(true, true, true, true);
            gl.use_program(Some(self.program));
            #[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
            gl.uniform_2_f32(
                self.u_screen.as_ref(),
                size.0 as f32 / ppp,
                size.1 as f32 / ppp,
            );
            gl.uniform_1_i32(self.u_sampler.as_ref(), 0);
            gl.active_texture(glow::TEXTURE0);
            if self.samplers {
                gl.bind_sampler(0, None);
            }
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ebo));
            let stride = i32::try_from(core::mem::size_of::<Vertex>()).unwrap_or(20);
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 8);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 4, glow::UNSIGNED_BYTE, true, stride, 16);
        }
    }

    unsafe fn draw_mesh(&self, mesh: &egui::Mesh, clip: egui::Rect, size: (u32, u32), ppp: f32) {
        let Some(texture) = self.textures.get(&mesh.texture_id) else {
            return;
        };
        let Some((x, y, w, h)) = scissor(clip, size, ppp) else {
            return;
        };
        let Ok(count) = i32::try_from(mesh.indices.len()) else {
            return;
        };
        let gl = &self.gl;
        // SAFETY: the caller's contract; the slices are plain data of the given lengths, and
        // `Vertex` is `repr(C)`.
        unsafe {
            let vertices = core::slice::from_raw_parts(
                mesh.vertices.as_ptr().cast::<u8>(),
                core::mem::size_of_val(mesh.vertices.as_slice()),
            );
            let indices = core::slice::from_raw_parts(
                mesh.indices.as_ptr().cast::<u8>(),
                core::mem::size_of_val(mesh.indices.as_slice()),
            );
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vertices, glow::STREAM_DRAW);
            gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, indices, glow::STREAM_DRAW);
            gl.bind_texture(glow::TEXTURE_2D, Some(*texture));
            gl.scissor(x, y, w, h);
            gl.draw_elements(glow::TRIANGLES, count, glow::UNSIGNED_INT, 0);
        }
    }

    /// Create a texture, or replace all or part of one.
    unsafe fn set_texture(&mut self, id: TextureId, delta: &ImageDelta) {
        let egui::ImageData::Color(image) = &delta.image;
        let [w, h] = image.size;
        let (Ok(w), Ok(h)) = (i32::try_from(w), i32::try_from(h)) else {
            return;
        };
        let pixels: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(egui::Color32::to_array)
            .collect();
        let gl = &self.gl;
        // SAFETY: the caller's contract; `pixels` holds `w * h` RGBA8 texels.
        unsafe {
            let texture = match self.textures.get(&id) {
                Some(texture) => *texture,
                None => match gl.create_texture() {
                    Ok(texture) => {
                        self.textures.insert(id, texture);
                        texture
                    }
                    Err(e) => {
                        log::warn!("overlay texture: {e}");
                        return;
                    }
                },
            };
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            let options = delta.options;
            let filter = |f: TextureFilter| match f {
                TextureFilter::Nearest => glow::NEAREST,
                TextureFilter::Linear => glow::LINEAR,
            };
            let wrap = match options.wrap_mode {
                TextureWrapMode::ClampToEdge => glow::CLAMP_TO_EDGE,
                TextureWrapMode::Repeat => glow::REPEAT,
                TextureWrapMode::MirroredRepeat => glow::MIRRORED_REPEAT,
            };
            for (name, value) in [
                (glow::TEXTURE_MAG_FILTER, filter(options.magnification)),
                (glow::TEXTURE_MIN_FILTER, filter(options.minification)),
                (glow::TEXTURE_WRAP_S, wrap),
                (glow::TEXTURE_WRAP_T, wrap),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, i32::try_from(value).unwrap_or(0));
            }
            // The pixels come from memory: a pixel unpack buffer the game left bound would be
            // read instead, at their address as an offset.
            gl.bind_buffer(glow::PIXEL_UNPACK_BUFFER, None);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, 0);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, 0);
            let data = glow::PixelUnpackData::Slice(Some(&pixels));
            match delta.pos {
                None => gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    i32::try_from(glow::RGBA8).unwrap_or(0),
                    w,
                    h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    data,
                ),
                Some([x, y]) => gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    i32::try_from(x).unwrap_or(0),
                    i32::try_from(y).unwrap_or(0),
                    w,
                    h,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    data,
                ),
            }
        }
    }
}

/// Compile and link the program for GLSL `version` (the `#version` line).
unsafe fn link(gl: &glow::Context, version: &str) -> Result<glow::Program, String> {
    // SAFETY: the caller's contract (a current context).
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, source) in [
            (glow::VERTEX_SHADER, VERTEX),
            (glow::FRAGMENT_SHADER, FRAGMENT),
        ] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &format!("{version}{source}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let why = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                gl.delete_program(program);
                return Err(format!("overlay shader: {why}"));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        for (index, name) in ["a_pos", "a_uv", "a_color"].into_iter().enumerate() {
            gl.bind_attrib_location(program, u32::try_from(index).unwrap_or(0), name);
        }
        gl.bind_frag_data_location(program, 0, "frag");
        gl.link_program(program);
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        if gl.get_program_link_status(program) {
            Ok(program)
        } else {
            let why = gl.get_program_info_log(program);
            gl.delete_program(program);
            Err(format!("overlay program: {why}"))
        }
    }
}

fn to_i32(size: (u32, u32)) -> (i32, i32) {
    (
        i32::try_from(size.0).unwrap_or(i32::MAX),
        i32::try_from(size.1).unwrap_or(i32::MAX),
    )
}

/// The scissor box of a clip rectangle in points, in `OpenGL`'s bottom-up pixels; `None`
/// when nothing of it is on screen.
fn scissor(clip: egui::Rect, size: (u32, u32), ppp: f32) -> Option<(i32, i32, i32, i32)> {
    #[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
    let (width, height) = (size.0 as f32, size.1 as f32);
    let min_x = (clip.min.x * ppp).round().clamp(0.0, width);
    let min_y = (clip.min.y * ppp).round().clamp(0.0, height);
    let max_x = (clip.max.x * ppp).round().clamp(min_x, width);
    let max_y = (clip.max.y * ppp).round().clamp(min_y, height);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    // Whole pixels within the screen after the clamps above.
    #[allow(clippy::cast_possible_truncation)]
    Some((
        min_x as i32,
        (height - max_y) as i32,
        (max_x - min_x) as i32,
        (max_y - min_y) as i32,
    ))
}

/// The game's state the painter changes, read before and put back after.
struct Saved {
    program: i32,
    vao: i32,
    array_buffer: i32,
    framebuffer: i32,
    active_texture: i32,
    texture: i32,
    sampler: i32,
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    blend: [i32; 6],
    /// Alignment, row length, skipped pixels and rows.
    unpack: [i32; 4],
    unpack_buffer: i32,
    color_mask: [bool; 4],
    enabled: [(u32, bool); 7],
}

impl Saved {
    unsafe fn read(gl: &glow::Context, samplers: bool) -> Self {
        // SAFETY: the caller's contract (a current context); the out arrays have the length
        // of each query.
        unsafe {
            let get = |p| gl.get_parameter_i32(p);
            let active_texture = get(glow::ACTIVE_TEXTURE);
            gl.active_texture(glow::TEXTURE0);
            let mut saved = Self {
                program: get(glow::CURRENT_PROGRAM),
                vao: get(glow::VERTEX_ARRAY_BINDING),
                array_buffer: get(glow::ARRAY_BUFFER_BINDING),
                framebuffer: get(glow::DRAW_FRAMEBUFFER_BINDING),
                active_texture,
                texture: get(glow::TEXTURE_BINDING_2D),
                sampler: if samplers {
                    get(glow::SAMPLER_BINDING)
                } else {
                    0
                },
                viewport: [0; 4],
                scissor_box: [0; 4],
                blend: [
                    get(glow::BLEND_SRC_RGB),
                    get(glow::BLEND_DST_RGB),
                    get(glow::BLEND_SRC_ALPHA),
                    get(glow::BLEND_DST_ALPHA),
                    get(glow::BLEND_EQUATION_RGB),
                    get(glow::BLEND_EQUATION_ALPHA),
                ],
                unpack: [
                    get(glow::UNPACK_ALIGNMENT),
                    get(glow::UNPACK_ROW_LENGTH),
                    get(glow::UNPACK_SKIP_PIXELS),
                    get(glow::UNPACK_SKIP_ROWS),
                ],
                unpack_buffer: get(glow::PIXEL_UNPACK_BUFFER_BINDING),
                color_mask: [true; 4],
                enabled: [
                    glow::BLEND,
                    glow::SCISSOR_TEST,
                    glow::CULL_FACE,
                    glow::DEPTH_TEST,
                    glow::STENCIL_TEST,
                    glow::FRAMEBUFFER_SRGB,
                    glow::RASTERIZER_DISCARD,
                ]
                .map(|cap| (cap, gl.is_enabled(cap))),
            };
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut saved.viewport);
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut saved.scissor_box);
            let mut mask = [1i32; 4];
            gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut mask);
            saved.color_mask = mask.map(|on| on != 0);
            saved
        }
    }

    unsafe fn restore(&self, gl: &glow::Context, samplers: bool) {
        let id = |raw: i32| NonZeroU32::new(u32::try_from(raw).unwrap_or(0));
        let unsigned = |raw: i32| u32::try_from(raw).unwrap_or(0);
        // SAFETY: the caller's contract; every name was read from this context.
        unsafe {
            gl.use_program(id(self.program).map(glow::NativeProgram));
            gl.bind_vertex_array(id(self.vao).map(glow::NativeVertexArray));
            gl.bind_buffer(
                glow::ARRAY_BUFFER,
                id(self.array_buffer).map(glow::NativeBuffer),
            );
            gl.bind_framebuffer(
                glow::DRAW_FRAMEBUFFER,
                id(self.framebuffer).map(glow::NativeFramebuffer),
            );
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, id(self.texture).map(glow::NativeTexture));
            if samplers {
                gl.bind_sampler(0, id(self.sampler).map(glow::NativeSampler));
            }
            gl.active_texture(unsigned(self.active_texture));
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            let [x, y, w, h] = self.scissor_box;
            gl.scissor(x, y, w, h);
            let [src_rgb, dst_rgb, src_a, dst_a, eq_rgb, eq_a] = self.blend.map(unsigned);
            gl.blend_func_separate(src_rgb, dst_rgb, src_a, dst_a);
            gl.blend_equation_separate(eq_rgb, eq_a);
            let [alignment, row_length, skip_pixels, skip_rows] = self.unpack;
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, alignment);
            gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, row_length);
            gl.pixel_store_i32(glow::UNPACK_SKIP_PIXELS, skip_pixels);
            gl.pixel_store_i32(glow::UNPACK_SKIP_ROWS, skip_rows);
            gl.bind_buffer(
                glow::PIXEL_UNPACK_BUFFER,
                id(self.unpack_buffer).map(glow::NativeBuffer),
            );
            let [red, green, blue, alpha] = self.color_mask;
            gl.color_mask(red, green, blue, alpha);
            for (cap, on) in self.enabled {
                if on {
                    gl.enable(cap);
                } else {
                    gl.disable(cap);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scissor_box_is_bottom_up_and_clamped() {
        let clip = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 70.0));
        assert_eq!(scissor(clip, (800, 600), 1.0), Some((10, 530, 100, 50)));
        assert_eq!(scissor(clip, (800, 600), 2.0), Some((20, 460, 200, 100)));
        let off = egui::Rect::from_min_max(egui::pos2(900.0, 0.0), egui::pos2(950.0, 10.0));
        assert_eq!(scissor(off, (800, 600), 1.0), None);
    }
}
