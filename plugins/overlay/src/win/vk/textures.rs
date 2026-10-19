//! egui's textures as Vulkan images, the host-visible buffers the renderer streams vertices
//! and uploads through, and the deferred release of both.
//!
//! Nothing here waits for the GPU. An upload is recorded into the frame's command buffer, and
//! whatever a frame may still be reading (a staging buffer, a texture egui freed) goes on the
//! garbage list with that frame's number, to be released once no submission up to that
//! number is pending any more.

// FFI module: Vulkan calls.
#![allow(unsafe_code)]

use std::collections::HashMap;

use ash::prelude::VkResult;
use ash::vk;
use egui::epaint::ImageDelta;
use egui::{TextureFilter, TextureId, TextureWrapMode};

/// The game's device as the overlay uses it.
pub(super) struct Gpu {
    pub(super) device: ash::Device,
    pub(super) memory: vk::PhysicalDeviceMemoryProperties,
}

/// A host-visible, coherent buffer.
pub(super) struct Buffer {
    pub(super) handle: vk::Buffer,
    memory: vk::DeviceMemory,
    pub(super) size: u64,
}

impl Gpu {
    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.memory.memory_type_count).find(|&i| {
            bits & (1 << i) != 0
                && self.memory.memory_types[i as usize]
                    .property_flags
                    .contains(flags)
        })
    }

    fn allocate(
        &self,
        requirements: vk::MemoryRequirements,
        flags: vk::MemoryPropertyFlags,
    ) -> VkResult<vk::DeviceMemory> {
        let index = self
            .memory_type(requirements.memory_type_bits, flags)
            .ok_or(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(index);
        // SAFETY: a complete allocate info on a live device.
        unsafe { self.device.allocate_memory(&info, None) }
    }

    /// A host-visible buffer of at least `size` bytes.
    pub(super) fn buffer(&self, size: u64, usage: vk::BufferUsageFlags) -> VkResult<Buffer> {
        let info = vk::BufferCreateInfo::default()
            .size(size.max(1))
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        // SAFETY: a complete create info; the buffer is bound to its memory before use and
        // destroyed with it.
        unsafe {
            let buffer = self.device.create_buffer(&info, None)?;
            let requirements = self.device.get_buffer_memory_requirements(buffer);
            let flags =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let memory = match self.allocate(requirements, flags) {
                Ok(memory) => memory,
                Err(e) => {
                    self.device.destroy_buffer(buffer, None);
                    return Err(e);
                }
            };
            if let Err(e) = self.device.bind_buffer_memory(buffer, memory, 0) {
                self.device.destroy_buffer(buffer, None);
                self.device.free_memory(memory, None);
                return Err(e);
            }
            Ok(Buffer {
                handle: buffer,
                memory,
                size: size.max(1),
            })
        }
    }

    /// Copy `bytes` to the start of `buffer`, which must be large enough.
    pub(super) fn write(&self, buffer: &Buffer, bytes: &[u8]) -> VkResult<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        // SAFETY: host-visible coherent memory of at least `bytes.len()` bytes, mapped for
        // the copy only.
        unsafe {
            let mapped = self.device.map_memory(
                buffer.memory,
                0,
                bytes.len() as u64,
                vk::MemoryMapFlags::empty(),
            )?;
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), mapped.cast::<u8>(), bytes.len());
            self.device.unmap_memory(buffer.memory);
        }
        Ok(())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "taken so the destroyed buffer cannot be used again"
    )]
    pub(super) fn destroy_buffer(&self, buffer: Buffer) {
        let Buffer { handle, memory, .. } = buffer;
        // SAFETY: a buffer of this device no submission uses any more (the caller's
        // contract, kept by the garbage list).
        unsafe {
            self.device.destroy_buffer(handle, None);
            self.device.free_memory(memory, None);
        }
    }
}

/// One egui texture on the GPU.
struct Texture {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    sampler: vk::Sampler,
    set: vk::DescriptorSet,
    size: [u32; 2],
}

enum Garbage {
    Buffer(Buffer),
    Texture(Texture),
}

/// egui's textures, their descriptor sets, and what waits to be released.
pub(super) struct Textures {
    map: HashMap<TextureId, Texture>,
    pub(super) layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    garbage: Vec<(u64, Garbage)>,
}

/// Descriptor sets, one per texture egui has at once.
const MAX_TEXTURES: u32 = 1024;

impl Textures {
    pub(super) fn new(gpu: &Gpu) -> VkResult<Self> {
        let bindings = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT)];
        let sizes = [vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(MAX_TEXTURES)];
        // SAFETY: complete create infos on a live device.
        unsafe {
            let layout = gpu.device.create_descriptor_set_layout(
                &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                None,
            )?;
            let pool = gpu.device.create_descriptor_pool(
                &vk::DescriptorPoolCreateInfo::default()
                    .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                    .max_sets(MAX_TEXTURES)
                    .pool_sizes(&sizes),
                None,
            );
            match pool {
                Ok(pool) => Ok(Self {
                    map: HashMap::new(),
                    layout,
                    pool,
                    garbage: Vec::new(),
                }),
                Err(e) => {
                    gpu.device.destroy_descriptor_set_layout(layout, None);
                    Err(e)
                }
            }
        }
    }

    /// The descriptor set drawing with `id` binds.
    pub(super) fn set(&self, id: TextureId) -> Option<vk::DescriptorSet> {
        self.map.get(&id).map(|t| t.set)
    }

    /// Record the upload of `delta` into `cmd`, which frame `frame` submits.
    pub(super) fn update(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        (id, delta): (TextureId, &ImageDelta),
        frame: u64,
    ) -> VkResult<()> {
        let egui::ImageData::Color(image) = &delta.image;
        let size = [
            u32::try_from(image.size[0]).unwrap_or(0),
            u32::try_from(image.size[1]).unwrap_or(0),
        ];
        if size[0] == 0 || size[1] == 0 {
            return Ok(());
        }
        let pixels: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(egui::Color32::to_array)
            .collect();
        let (offset, fresh) = if let Some([x, y]) = delta.pos {
            if !self.map.contains_key(&id) {
                return Ok(());
            }
            let offset = [i32::try_from(x).unwrap_or(0), i32::try_from(y).unwrap_or(0)];
            (offset, false)
        } else {
            let reuse = self.map.get(&id).is_some_and(|t| t.size == size);
            if !reuse {
                if let Some(old) = self.map.remove(&id) {
                    self.garbage.push((frame, Garbage::Texture(old)));
                }
                let texture = self.create(gpu, size, delta.options)?;
                self.map.insert(id, texture);
            }
            ([0, 0], !reuse)
        };
        let Some(image) = self.map.get(&id).map(|t| t.image) else {
            return Ok(());
        };
        let staging = gpu.buffer(pixels.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC)?;
        if let Err(e) = gpu.write(&staging, &pixels) {
            gpu.destroy_buffer(staging);
            return Err(e);
        }
        record_upload(gpu, cmd, staging.handle, image, (offset, size), fresh);
        self.garbage.push((frame, Garbage::Buffer(staging)));
        Ok(())
    }

    /// egui is done with `id`; it goes once frame `frame` is no longer pending.
    pub(super) fn free(&mut self, id: TextureId, frame: u64) {
        if let Some(texture) = self.map.remove(&id) {
            self.garbage.push((frame, Garbage::Texture(texture)));
        }
    }

    /// Release what no pending frame can still use: `in_use(f)` says whether a submission
    /// numbered `f` or earlier may still run.
    pub(super) fn collect(&mut self, gpu: &Gpu, in_use: impl Fn(u64) -> bool) {
        let mut kept = Vec::new();
        for (frame, item) in self.garbage.drain(..) {
            if in_use(frame) {
                kept.push((frame, item));
            } else {
                release(gpu, self.pool, item);
            }
        }
        self.garbage = kept;
    }

    /// Release everything; the device must be idle.
    pub(super) fn destroy(&mut self, gpu: &Gpu) {
        for (_, item) in self.garbage.drain(..) {
            release(gpu, self.pool, item);
        }
        for (_, texture) in self.map.drain() {
            release(gpu, self.pool, Garbage::Texture(texture));
        }
        // SAFETY: objects of this device, unused once it is idle.
        unsafe {
            gpu.device.destroy_descriptor_pool(self.pool, None);
            gpu.device.destroy_descriptor_set_layout(self.layout, None);
        }
    }

    fn create(
        &self,
        gpu: &Gpu,
        size: [u32; 2],
        options: egui::TextureOptions,
    ) -> VkResult<Texture> {
        let format = vk::Format::R8G8B8A8_UNORM;
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: size[0],
                height: size[1],
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let filter = |f: TextureFilter| match f {
            TextureFilter::Nearest => vk::Filter::NEAREST,
            TextureFilter::Linear => vk::Filter::LINEAR,
        };
        let address = match options.wrap_mode {
            TextureWrapMode::ClampToEdge => vk::SamplerAddressMode::CLAMP_TO_EDGE,
            TextureWrapMode::Repeat => vk::SamplerAddressMode::REPEAT,
            TextureWrapMode::MirroredRepeat => vk::SamplerAddressMode::MIRRORED_REPEAT,
        };
        let device = &gpu.device;
        // SAFETY: complete create infos on a live device; on failure everything made so far
        // is destroyed again.
        unsafe {
            let image = device.create_image(&info, None)?;
            let memory = match gpu.allocate(
                device.get_image_memory_requirements(image),
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ) {
                Ok(memory) => memory,
                Err(e) => {
                    device.destroy_image(image, None);
                    return Err(e);
                }
            };
            let made = device.bind_image_memory(image, memory, 0).and_then(|()| {
                let view = device.create_image_view(
                    &vk::ImageViewCreateInfo::default()
                        .image(image)
                        .view_type(vk::ImageViewType::TYPE_2D)
                        .format(format)
                        .subresource_range(color_range()),
                    None,
                )?;
                let sampler = device.create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(filter(options.magnification))
                        .min_filter(filter(options.minification))
                        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
                        .address_mode_u(address)
                        .address_mode_v(address)
                        .address_mode_w(address)
                        .max_lod(0.0),
                    None,
                )?;
                let layouts = [self.layout];
                let set = device.allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.pool)
                        .set_layouts(&layouts),
                )?[0];
                Ok((view, sampler, set))
            });
            let (view, sampler, set) = match made {
                Ok(parts) => parts,
                Err(e) => {
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                    return Err(e);
                }
            };
            let image_info = [vk::DescriptorImageInfo::default()
                .sampler(sampler)
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let write = vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
                .image_info(&image_info);
            device.update_descriptor_sets(&[write], &[]);
            Ok(Texture {
                image,
                memory,
                view,
                sampler,
                set,
                size,
            })
        }
    }
}

fn release(gpu: &Gpu, pool: vk::DescriptorPool, item: Garbage) {
    match item {
        Garbage::Buffer(buffer) => gpu.destroy_buffer(buffer),
        // SAFETY: objects of this device that no pending submission uses.
        Garbage::Texture(t) => unsafe {
            let _ = gpu.device.free_descriptor_sets(pool, &[t.set]);
            gpu.device.destroy_sampler(t.sampler, None);
            gpu.device.destroy_image_view(t.view, None);
            gpu.device.destroy_image(t.image, None);
            gpu.device.free_memory(t.memory, None);
        },
    }
}

fn color_range() -> vk::ImageSubresourceRange {
    vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    }
}

/// Copy `staging` into the `region` (offset, size) of `image`, leaving it ready to sample.
fn record_upload(
    gpu: &Gpu,
    cmd: vk::CommandBuffer,
    staging: vk::Buffer,
    image: vk::Image,
    (offset, size): ([i32; 2], [u32; 2]),
    fresh: bool,
) {
    let (old, src_stage, src_access) = if fresh {
        (
            vk::ImageLayout::UNDEFINED,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::AccessFlags::empty(),
        )
    } else {
        (
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::AccessFlags::SHADER_READ,
        )
    };
    let to_transfer = vk::ImageMemoryBarrier::default()
        .src_access_mask(src_access)
        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .old_layout(old)
        .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range());
    let to_shader = vk::ImageMemoryBarrier::default()
        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
        .dst_access_mask(vk::AccessFlags::SHADER_READ)
        .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
        .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(color_range());
    let region = vk::BufferImageCopy::default()
        .image_subresource(vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        })
        .image_offset(vk::Offset3D {
            x: offset[0],
            y: offset[1],
            z: 0,
        })
        .image_extent(vk::Extent3D {
            width: size[0],
            height: size[1],
            depth: 1,
        });
    let device = &gpu.device;
    // SAFETY: a command buffer in the recording state, and objects of this device.
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            src_stage,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_transfer],
        );
        device.cmd_copy_buffer_to_image(
            cmd,
            staging,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[to_shader],
        );
    }
}
