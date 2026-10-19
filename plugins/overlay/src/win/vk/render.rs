//! The Vulkan renderer of one game device: a pipeline per swapchain format, and per swapchain
//! image a command buffer, a fence, a semaphore and the vertex buffers of its last frame.
//!
//! The image arrives in `PRESENT_SRC_KHR`, as the game hands it to the present. A render
//! pass that loads it, draws egui on top and leaves it in the same layout needs no barrier
//! of its own beyond the subpass dependency; the game's semaphores are waited for by our
//! submission, which signals the semaphore the present then waits for.

// FFI module: Vulkan calls.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::time::Instant;

use ash::prelude::VkResult;
use ash::vk::{self, Handle};
use egui::epaint::Primitive;
use uniloader_sdk::Host;

use super::textures::{Buffer, Gpu, Textures};
use super::Swapchain;
use crate::ui::Gui;

const VERTEX_SPV: &[u8] = include_bytes!("egui.vert.spv");
const FRAGMENT_SPV: &[u8] = include_bytes!("egui.frag.spv");
/// How long a frame waits for its image's previous overlay submission before skipping.
const FENCE_WAIT_NS: u64 = 100_000_000;
/// `egui::epaint::Vertex`: two `f32` pairs and four bytes of colour.
const VERTEX_SIZE: u32 = 20;

/// One `vkQueuePresentKHR`, as far as the overlay draws into it.
pub(super) struct Present<'a> {
    pub(super) queue: vk::Queue,
    pub(super) family: u32,
    pub(super) swapchain: vk::SwapchainKHR,
    pub(super) chain: Swapchain,
    pub(super) index: u32,
    pub(super) waits: &'a [vk::Semaphore],
}

/// Per swapchain image.
struct Slot {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    done: vk::Semaphore,
    vertices: Option<Buffer>,
    indices: Option<Buffer>,
    /// The frame number of a submission that may still run.
    pending: Option<u64>,
}

/// Per swapchain.
struct Target {
    views: Vec<vk::ImageView>,
    framebuffers: Vec<vk::Framebuffer>,
    slots: Vec<Slot>,
    pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    extent: vk::Extent2D,
    /// The format stores linear values (sRGB, float): the shader converts.
    linear: bool,
}

pub(super) struct Renderer {
    gpu: Gpu,
    swapchain_fn: ash::khr::swapchain::DeviceFn,
    families: Vec<vk::QueueFamilyProperties>,
    textures: Textures,
    pipeline_layout: vk::PipelineLayout,
    shaders: [vk::ShaderModule; 2],
    /// Render pass and pipeline per swapchain format (raw value).
    passes: HashMap<i32, (vk::RenderPass, vk::Pipeline)>,
    /// Command pool per queue family.
    pools: HashMap<u32, vk::CommandPool>,
    /// Per swapchain (raw handle).
    targets: HashMap<u64, Target>,
    gui: Gui,
    started: Instant,
    /// Number of the next submission.
    frame: u64,
}

// SAFETY: Vulkan handles and function tables, used under the tracking lock only.
unsafe impl Send for Renderer {}

impl Renderer {
    /// A renderer on the game's `device`; `None` (logged) when it cannot be made.
    pub(super) fn new(device: vk::Device, physical: vk::PhysicalDevice) -> Option<Self> {
        match Self::make(device, physical) {
            Ok(renderer) => {
                log::info!("overlay ready (Vulkan)");
                Some(renderer)
            }
            Err(e) => {
                log::warn!("no Vulkan overlay on this device: {e}");
                None
            }
        }
    }

    fn make(device: vk::Device, physical: vk::PhysicalDevice) -> Result<Self, String> {
        let loader = super::loader().ok_or("vulkan-1.dll is gone")?;
        let load = |name: &core::ffi::CStr| loader.device_fn(device, name);
        // SAFETY: the game's live device, with functions from its own vkGetDeviceProcAddr.
        let ash_device = unsafe { ash::Device::load_with(load, device) };
        let swapchain_fn = ash::khr::swapchain::DeviceFn::load(load);
        let (memory, families) = physical_info(loader, physical)?;
        let gpu = Gpu {
            device: ash_device,
            memory,
        };
        let textures = Textures::new(&gpu).map_err(|e| e.to_string())?;
        let ranges = [vk::PushConstantRange {
            stage_flags: vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            offset: 0,
            size: 16,
        }];
        let layouts = [textures.layout];
        let vertex = ash::util::read_spv(&mut std::io::Cursor::new(VERTEX_SPV))
            .map_err(|e| e.to_string())?;
        let fragment = ash::util::read_spv(&mut std::io::Cursor::new(FRAGMENT_SPV))
            .map_err(|e| e.to_string())?;
        // SAFETY: complete create infos on the live device.
        let (pipeline_layout, shaders) = unsafe {
            let layout = gpu.device.create_pipeline_layout(
                &vk::PipelineLayoutCreateInfo::default()
                    .set_layouts(&layouts)
                    .push_constant_ranges(&ranges),
                None,
            );
            let vs = gpu
                .device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&vertex), None);
            let fs = gpu
                .device
                .create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&fragment), None);
            (
                layout.map_err(|e| e.to_string())?,
                [
                    vs.map_err(|e| e.to_string())?,
                    fs.map_err(|e| e.to_string())?,
                ],
            )
        };
        Ok(Self {
            gpu,
            swapchain_fn,
            families,
            textures,
            pipeline_layout,
            shaders,
            passes: HashMap::new(),
            pools: HashMap::new(),
            targets: HashMap::new(),
            gui: Gui::new(),
            started: Instant::now(),
            frame: 1,
        })
    }

    /// Draw the overlay into the image being presented; the semaphore the present must wait
    /// for instead of the game's, or `None` when nothing was submitted.
    pub(super) fn draw(&mut self, host: &Host, p: &Present<'_>) -> Option<vk::Semaphore> {
        let graphics = self
            .families
            .get(p.family as usize)
            .is_some_and(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS));
        if !graphics
            || !p
                .chain
                .usage
                .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        {
            return None;
        }
        let key = p.swapchain.as_raw();
        if !self.targets.contains_key(&key) {
            match self.target(p) {
                Ok(target) => {
                    self.targets.insert(key, target);
                }
                Err(e) => {
                    log::warn!("Vulkan overlay: no target for the swapchain ({e})");
                    return None;
                }
            }
        }
        self.collect();
        let frame = self.frame;
        let target = self.targets.get_mut(&key)?;
        let slot = target.slots.get_mut(p.index as usize)?;
        // SAFETY: a fence of this device.
        if unsafe {
            self.gpu
                .device
                .wait_for_fences(&[slot.fence], true, FENCE_WAIT_NS)
        }
        .is_err()
        {
            return None;
        }
        slot.pending = None;
        let size = (target.extent.width, target.extent.height);
        let output = super::super::gui_pass(&mut self.gui, self.started, host, size);
        let pass = Recording {
            gpu: &self.gpu,
            layout: self.pipeline_layout,
            render_pass: target.pass,
            pipeline: target.pipeline,
            framebuffer: *target.framebuffers.get(p.index as usize)?,
            extent: target.extent,
            linear: target.linear,
        };
        if let Err(e) = record(&pass, slot, &mut self.textures, &self.gui, output, frame) {
            log::warn!("Vulkan overlay frame not recorded: {e}");
            return None;
        }
        let stages = vec![vk::PipelineStageFlags::ALL_COMMANDS; p.waits.len()];
        let cmds = [slot.cmd];
        let signals = [slot.done];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(p.waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&signals);
        // SAFETY: the game's queue, which it holds for the present we are inside of, and a
        // fence that is signalled (waited above) until this reset.
        let submitted = unsafe {
            self.gpu
                .device
                .reset_fences(&[slot.fence])
                .and_then(|()| self.gpu.device.queue_submit(p.queue, &[submit], slot.fence))
        };
        if let Err(e) = submitted {
            log::warn!("Vulkan overlay submit failed: {e}");
            return None;
        }
        slot.pending = Some(frame);
        self.frame += 1;
        Some(slot.done)
    }

    /// Release what no pending submission can use any more.
    fn collect(&mut self) {
        let device = &self.gpu.device;
        let mut oldest_pending = u64::MAX;
        for slot in self.targets.values_mut().flat_map(|t| t.slots.iter_mut()) {
            if let Some(frame) = slot.pending {
                // SAFETY: a fence of this device.
                if unsafe { device.get_fence_status(slot.fence) } == Ok(true) {
                    slot.pending = None;
                } else {
                    oldest_pending = oldest_pending.min(frame);
                }
            }
        }
        self.textures
            .collect(&self.gpu, |frame| frame >= oldest_pending);
    }

    /// Views, framebuffers and slots for the swapchain being presented.
    fn target(&mut self, p: &Present<'_>) -> VkResult<Target> {
        let (pass, pipeline) = self.pass_for(p.chain.format)?;
        let pool = self.pool_for(p.family)?;
        let images = self.images(p.chain.device, p.swapchain)?;
        let device = &self.gpu.device;
        let mut target = Target {
            views: Vec::new(),
            framebuffers: Vec::new(),
            slots: Vec::new(),
            pass,
            pipeline,
            extent: p.chain.extent,
            linear: is_linear(p.chain.format),
        };
        // SAFETY: complete create infos on the live device; on failure the partial target
        // is destroyed by `destroy_target`.
        let made = unsafe {
            images.iter().try_for_each(|&image| -> VkResult<()> {
                let view = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(p.chain.format)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: vk::ImageAspectFlags::COLOR,
                            base_mip_level: 0,
                            level_count: 1,
                            base_array_layer: 0,
                            layer_count: 1,
                        }),
                    None,
                )?;
                target.views.push(view);
                let attachments = [view];
                target.framebuffers.push(
                    device.create_framebuffer(
                        &vk::FramebufferCreateInfo::default()
                            .render_pass(pass)
                            .attachments(&attachments)
                            .width(p.chain.extent.width)
                            .height(p.chain.extent.height)
                            .layers(1),
                        None,
                    )?,
                );
                let cmd = device.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )?[0];
                let fence = device.create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )?;
                let done = device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
                target.slots.push(Slot {
                    cmd,
                    fence,
                    done,
                    vertices: None,
                    indices: None,
                    pending: None,
                });
                Ok(())
            })
        };
        match made {
            Ok(()) => Ok(target),
            Err(e) => {
                destroy_target(&self.gpu, target, None);
                Err(e)
            }
        }
    }

    fn images(&self, device: vk::Device, swapchain: vk::SwapchainKHR) -> VkResult<Vec<vk::Image>> {
        let get = self.swapchain_fn.get_swapchain_images_khr;
        let mut count = 0;
        // SAFETY: the game's swapchain on its device; the two-call idiom.
        unsafe {
            get(device, swapchain, &raw mut count, core::ptr::null_mut()).result()?;
            let mut images = vec![vk::Image::null(); count as usize];
            get(device, swapchain, &raw mut count, images.as_mut_ptr()).result()?;
            images.truncate(count as usize);
            Ok(images)
        }
    }

    fn pool_for(&mut self, family: u32) -> VkResult<vk::CommandPool> {
        if let Some(pool) = self.pools.get(&family) {
            return Ok(*pool);
        }
        // SAFETY: a complete create info on the live device.
        let pool = unsafe {
            self.gpu.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(family),
                None,
            )
        }?;
        self.pools.insert(family, pool);
        Ok(pool)
    }

    fn pass_for(&mut self, format: vk::Format) -> VkResult<(vk::RenderPass, vk::Pipeline)> {
        if let Some(made) = self.passes.get(&format.as_raw()) {
            return Ok(*made);
        }
        let made = make_pass(&self.gpu.device, format, self.pipeline_layout, self.shaders)?;
        self.passes.insert(format.as_raw(), made);
        Ok(made)
    }

    /// The game destroys `swapchain`: its views and slots go first.
    pub(super) fn forget(&mut self, swapchain: vk::SwapchainKHR) {
        if let Some(target) = self.targets.remove(&swapchain.as_raw()) {
            let pool = self.pools.values().next().copied();
            destroy_target(&self.gpu, target, pool);
        }
    }

    /// The game destroys the device: everything goes.
    pub(super) fn destroy(&mut self) {
        let device = &self.gpu.device;
        // SAFETY: the live device; waiting first makes every object below unused.
        unsafe {
            let _ = device.device_wait_idle();
        }
        let pool = self.pools.values().next().copied();
        for (_, target) in self.targets.drain() {
            destroy_target(&self.gpu, target, pool);
        }
        self.textures.destroy(&self.gpu);
        // SAFETY: objects of the idle device.
        unsafe {
            for (_, (pass, pipeline)) in self.passes.drain() {
                device.destroy_pipeline(pipeline, None);
                device.destroy_render_pass(pass, None);
            }
            for (_, pool) in self.pools.drain() {
                device.destroy_command_pool(pool, None);
            }
            for shader in self.shaders {
                device.destroy_shader_module(shader, None);
            }
            device.destroy_pipeline_layout(self.pipeline_layout, None);
        }
    }
}

/// Memory types and queue families of the game's GPU, through the loader's exports.
fn physical_info(
    loader: &super::Loader,
    physical: vk::PhysicalDevice,
) -> Result<
    (
        vk::PhysicalDeviceMemoryProperties,
        Vec<vk::QueueFamilyProperties>,
    ),
    String,
> {
    let memory_fn = loader.export(c"vkGetPhysicalDeviceMemoryProperties");
    let families_fn = loader.export(c"vkGetPhysicalDeviceQueueFamilyProperties");
    if memory_fn.is_null() || families_fn.is_null() {
        return Err("vulkan-1.dll lacks the physical device queries".into());
    }
    // SAFETY: exports of these names have these signatures; the two-call idiom.
    unsafe {
        let memory_fn: vk::PFN_vkGetPhysicalDeviceMemoryProperties =
            core::mem::transmute(memory_fn);
        let families_fn: vk::PFN_vkGetPhysicalDeviceQueueFamilyProperties =
            core::mem::transmute(families_fn);
        let mut memory = vk::PhysicalDeviceMemoryProperties::default();
        memory_fn(physical, &raw mut memory);
        let mut count = 0;
        families_fn(physical, &raw mut count, core::ptr::null_mut());
        let mut families = vec![vk::QueueFamilyProperties::default(); count as usize];
        families_fn(physical, &raw mut count, families.as_mut_ptr());
        families.truncate(count as usize);
        Ok((memory, families))
    }
}

/// Formats whose stores encode linear values.
fn is_linear(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::B8G8R8A8_SRGB
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::A8B8G8R8_SRGB_PACK32
            | vk::Format::R16G16B16A16_SFLOAT
    )
}

fn destroy_target(gpu: &Gpu, target: Target, pool: Option<vk::CommandPool>) {
    let device = &gpu.device;
    let fences: Vec<vk::Fence> = target.slots.iter().map(|s| s.fence).collect();
    // SAFETY: objects of this device; waiting on the fences first makes them unused.
    unsafe {
        if !fences.is_empty() {
            let _ = device.wait_for_fences(&fences, true, 1_000_000_000);
        }
        for slot in target.slots {
            device.destroy_fence(slot.fence, None);
            device.destroy_semaphore(slot.done, None);
            if let Some(pool) = pool {
                device.free_command_buffers(pool, &[slot.cmd]);
            }
            for buffer in [slot.vertices, slot.indices].into_iter().flatten() {
                gpu.destroy_buffer(buffer);
            }
        }
        for framebuffer in target.framebuffers {
            device.destroy_framebuffer(framebuffer, None);
        }
        for view in target.views {
            device.destroy_image_view(view, None);
        }
    }
}

fn make_pass(
    device: &ash::Device,
    format: vk::Format,
    layout: vk::PipelineLayout,
    shaders: [vk::ShaderModule; 2],
) -> VkResult<(vk::RenderPass, vk::Pipeline)> {
    let attachments = [vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::LOAD)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
        .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)];
    let colour = [vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    }];
    let subpasses = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&colour)];
    let dependencies = [vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(
            vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        )];
    // SAFETY: a complete create info on the live device.
    let pass = unsafe {
        device.create_render_pass(
            &vk::RenderPassCreateInfo::default()
                .attachments(&attachments)
                .subpasses(&subpasses)
                .dependencies(&dependencies),
            None,
        )
    }?;
    match make_pipeline(device, pass, layout, shaders) {
        Ok(pipeline) => Ok((pass, pipeline)),
        Err(e) => {
            // SAFETY: the pass made above, unused.
            unsafe { device.destroy_render_pass(pass, None) };
            Err(e)
        }
    }
}

fn make_pipeline(
    device: &ash::Device,
    pass: vk::RenderPass,
    layout: vk::PipelineLayout,
    shaders: [vk::ShaderModule; 2],
) -> VkResult<vk::Pipeline> {
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(shaders[0])
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(shaders[1])
            .name(c"main"),
    ];
    let bindings = [vk::VertexInputBindingDescription {
        binding: 0,
        stride: VERTEX_SIZE,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attributes = [
        (0, vk::Format::R32G32_SFLOAT, 0),
        (1, vk::Format::R32G32_SFLOAT, 8),
        (2, vk::Format::R8G8B8A8_UNORM, 16),
    ]
    .map(
        |(location, format, offset)| vk::VertexInputAttributeDescription {
            location,
            binding: 0,
            format,
            offset,
        },
    );
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    // Premultiplied alpha, as egui hands it over.
    let blend_attachments = [vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .src_color_blend_factor(vk::BlendFactor::ONE)
        .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
        .color_blend_op(vk::BlendOp::ADD)
        .src_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_DST_ALPHA)
        .dst_alpha_blend_factor(vk::BlendFactor::ONE)
        .alpha_blend_op(vk::BlendOp::ADD)
        .color_write_mask(vk::ColorComponentFlags::RGBA)];
    let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blend)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(pass)
        .subpass(0);
    // SAFETY: a complete create info whose pointers outlive the call.
    unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) }
        .map_err(|(_, e)| e)
        .and_then(|pipelines| {
            pipelines
                .into_iter()
                .next()
                .ok_or(vk::Result::ERROR_UNKNOWN)
        })
}

/// What one frame's recording draws with.
struct Recording<'a> {
    gpu: &'a Gpu,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    pipeline: vk::Pipeline,
    framebuffer: vk::Framebuffer,
    extent: vk::Extent2D,
    linear: bool,
}

/// One mesh's place in the frame's buffers.
struct Draw {
    texture: egui::TextureId,
    clip: egui::Rect,
    first_index: u32,
    index_count: u32,
    vertex_offset: i32,
}

/// Record the frame into the slot's command buffer.
fn record(
    pass: &Recording<'_>,
    slot: &mut Slot,
    textures: &mut Textures,
    gui: &Gui,
    output: egui_directx11::RendererOutput,
    frame: u64,
) -> VkResult<()> {
    let device = &pass.gpu.device;
    let cmd = slot.cmd;
    // SAFETY: the slot's command buffer, whose last submission is complete (its fence was
    // waited for), from a pool that allows resetting it.
    unsafe {
        device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        device.begin_command_buffer(
            cmd,
            &vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
    }
    for (id, delta) in &output.textures_delta.set {
        if let Err(e) = textures.update(pass.gpu, cmd, (*id, delta), frame) {
            log::warn!("Vulkan overlay texture upload failed: {e}");
        }
    }
    let ppp = output.pixels_per_point;
    let primitives = gui.context().tessellate(output.shapes, ppp);
    let (vertices, indices, draws) = gather(&primitives);
    upload(
        pass.gpu,
        &mut slot.vertices,
        &vertices,
        vk::BufferUsageFlags::VERTEX_BUFFER,
    )?;
    upload(
        pass.gpu,
        &mut slot.indices,
        &indices,
        vk::BufferUsageFlags::INDEX_BUFFER,
    )?;
    #[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
    let (width, height) = (pass.extent.width as f32, pass.extent.height as f32);
    let mut push = [0u8; 16];
    for (i, value) in [
        width / ppp,
        height / ppp,
        if pass.linear { 1.0 } else { 0.0 },
    ]
    .into_iter()
    .enumerate()
    {
        push[i * 4..i * 4 + 4].copy_from_slice(&value.to_ne_bytes());
    }
    // SAFETY: a recording command buffer and objects of this device; the buffers hold the
    // data the draws index.
    unsafe {
        device.cmd_begin_render_pass(
            cmd,
            &vk::RenderPassBeginInfo::default()
                .render_pass(pass.render_pass)
                .framebuffer(pass.framebuffer)
                .render_area(vk::Rect2D {
                    offset: vk::Offset2D::default(),
                    extent: pass.extent,
                }),
            vk::SubpassContents::INLINE,
        );
        if let (Some(vb), Some(ib), false) = (&slot.vertices, &slot.indices, draws.is_empty()) {
            device.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pass.pipeline);
            device.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width,
                    height,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            device.cmd_bind_vertex_buffers(cmd, 0, &[vb.handle], &[0]);
            device.cmd_bind_index_buffer(cmd, ib.handle, 0, vk::IndexType::UINT32);
            device.cmd_push_constants(
                cmd,
                pass.layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                &push,
            );
            for draw in &draws {
                let (Some(set), Some(scissor)) = (
                    textures.set(draw.texture),
                    scissor(draw.clip, pass.extent, ppp),
                ) else {
                    continue;
                };
                device.cmd_set_scissor(cmd, 0, &[scissor]);
                device.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    pass.layout,
                    0,
                    &[set],
                    &[],
                );
                device.cmd_draw_indexed(
                    cmd,
                    draw.index_count,
                    1,
                    draw.first_index,
                    draw.vertex_offset,
                    0,
                );
            }
        }
        device.cmd_end_render_pass(cmd);
    }
    for id in &output.textures_delta.free {
        textures.free(*id, frame);
    }
    // SAFETY: the command buffer begun above.
    unsafe { device.end_command_buffer(cmd) }
}

/// All meshes' vertices and indices, back to back, and where each mesh is.
fn gather(primitives: &[egui::ClippedPrimitive]) -> (Vec<u8>, Vec<u8>, Vec<Draw>) {
    let (mut vertices, mut indices, mut draws) = (Vec::new(), Vec::new(), Vec::new());
    let (mut vertex_count, mut index_count) = (0usize, 0usize);
    for clipped in primitives {
        let Primitive::Mesh(mesh) = &clipped.primitive else {
            continue;
        };
        if mesh.indices.is_empty() {
            continue;
        }
        let (Ok(vertex_offset), Ok(first_index), Ok(count)) = (
            i32::try_from(vertex_count),
            u32::try_from(index_count),
            u32::try_from(mesh.indices.len()),
        ) else {
            break;
        };
        for v in &mesh.vertices {
            for value in [v.pos.x, v.pos.y, v.uv.x, v.uv.y] {
                vertices.extend_from_slice(&value.to_ne_bytes());
            }
            vertices.extend_from_slice(&v.color.to_array());
        }
        for index in &mesh.indices {
            indices.extend_from_slice(&index.to_ne_bytes());
        }
        draws.push(Draw {
            texture: mesh.texture_id,
            clip: clipped.clip_rect,
            first_index,
            index_count: count,
            vertex_offset,
        });
        vertex_count += mesh.vertices.len();
        index_count += mesh.indices.len();
    }
    (vertices, indices, draws)
}

/// Write `bytes` into `slot`, growing (replacing) the buffer when it is too small. The
/// slot's last submission is complete, so the old buffer can go at once.
fn upload(
    gpu: &Gpu,
    slot: &mut Option<Buffer>,
    bytes: &[u8],
    usage: vk::BufferUsageFlags,
) -> VkResult<()> {
    let needed = bytes.len() as u64;
    if slot.as_ref().is_none_or(|b| b.size < needed) {
        if let Some(old) = slot.take() {
            gpu.destroy_buffer(old);
        }
        *slot = Some(gpu.buffer(needed.next_power_of_two().max(4096), usage)?);
    }
    match slot {
        Some(buffer) => gpu.write(buffer, bytes),
        None => Ok(()),
    }
}

/// The scissor of a clip rectangle in points; `None` when nothing of it is on screen.
fn scissor(clip: egui::Rect, extent: vk::Extent2D, ppp: f32) -> Option<vk::Rect2D> {
    #[allow(clippy::cast_precision_loss, reason = "pixel sizes")]
    let (width, height) = (extent.width as f32, extent.height as f32);
    let min_x = (clip.min.x * ppp).round().clamp(0.0, width);
    let min_y = (clip.min.y * ppp).round().clamp(0.0, height);
    let max_x = (clip.max.x * ppp).round().clamp(min_x, width);
    let max_y = (clip.max.y * ppp).round().clamp(min_y, height);
    if max_x <= min_x || max_y <= min_y {
        return None;
    }
    // Whole pixels within the target after the clamps above.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some(vk::Rect2D {
        offset: vk::Offset2D {
            x: min_x as i32,
            y: min_y as i32,
        },
        extent: vk::Extent2D {
            width: (max_x - min_x) as u32,
            height: (max_y - min_y) as u32,
        },
    })
}
