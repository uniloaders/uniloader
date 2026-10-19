//! A Vulkan swapchain presenting frames, so the overlay's `vkQueuePresentKHR` detour has
//! something to draw on. Like the other windows it is never shown.
//!
//! `vulkan-1.dll` is loaded first and the device made a moment later, as a game that loads
//! Vulkan on demand does: the overlay must hook the device functions before the device
//! exists to know its queues and swapchains.
//!
//! After the last frame one image is acquired again and copied out: the overlay drew into
//! every presented image, and an acquired image still holds what was last presented from it
//! on the implementations this runs on. When it does not, hardly any pixel is the clear
//! colour, and the count is reported as unreliable.

// FFI module: Vulkan calls.
#![allow(unsafe_code)]

use std::time::Duration;

use ash::vk;
use windows::core::{w, PCSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;

use crate::capture::Frame;

/// The colour every frame is cleared to, as RGBA bytes.
const CLEAR: [u8; 4] = [0, 64, 128, 255];
const TIMEOUT_NS: u64 = 1_000_000_000;
const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

/// Present `frames` frames about 60 times a second. Returns how many presents succeeded.
pub(crate) fn present_frames(frames: u32) -> Result<u32, String> {
    // SAFETY: a system library name.
    let module = unsafe { LoadLibraryW(w!("vulkan-1.dll")) }.map_err(|e| e.to_string())?;
    // The overlay looks for vulkan-1.dll four times a second.
    std::thread::sleep(Duration::from_millis(1000));
    // SAFETY: the module loaded above and a NUL-terminated name.
    let proc_addr =
        unsafe { GetProcAddress(module, PCSTR(c"vkGetInstanceProcAddr".as_ptr().cast())) }
            .ok_or("no vkGetInstanceProcAddr")?;
    // SAFETY: the export of that name has that signature.
    let entry = unsafe {
        ash::Entry::from_static_fn(ash::StaticFn {
            get_instance_proc_addr: core::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                vk::PFN_vkGetInstanceProcAddr,
            >(proc_addr),
        })
    };
    let hwnd = crate::window::hidden("smoke-host-vk").map_err(|e| e.to_string())?;
    let result = Gpu::new(&entry, hwnd)
        .map_err(|e| e.to_string())
        .and_then(|gpu| {
            let presented = gpu.run(frames);
            gpu.destroy();
            presented
        });
    // SAFETY: the window created above.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    result
}

/// Everything the window's frames are made with.
struct Gpu {
    instance: ash::Instance,
    surface_fn: ash::khr::surface::Instance,
    surface: vk::SurfaceKHR,
    device: ash::Device,
    swapchain_fn: ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    extent: vk::Extent2D,
    /// The swapchain stores BGRA rather than RGBA.
    bgra: bool,
    memory: vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    acquired: vk::Semaphore,
    /// One per image: the present of an image waits for its own.
    rendered: Vec<vk::Semaphore>,
}

impl Gpu {
    fn new(entry: &ash::Entry, hwnd: HWND) -> ash::prelude::VkResult<Self> {
        let app = vk::ApplicationInfo::default()
            .application_name(c"smoke-host")
            .api_version(vk::API_VERSION_1_1);
        let extensions = [c"VK_KHR_surface".as_ptr(), c"VK_KHR_win32_surface".as_ptr()];
        // SAFETY: a complete create info whose pointers outlive the call.
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default()
                    .application_info(&app)
                    .enabled_extension_names(&extensions),
                None,
            )
        }?;
        let surface_fn = ash::khr::surface::Instance::new(entry, &instance);
        let win32 = ash::khr::win32_surface::Instance::new(entry, &instance);
        // SAFETY: the process image's module handle.
        let hinstance = unsafe { GetModuleHandleW(None) }.map_or(0, |m| m.0 as isize);
        // SAFETY: a live window of this process.
        let surface = unsafe {
            win32.create_win32_surface(
                &vk::Win32SurfaceCreateInfoKHR::default()
                    .hinstance(hinstance)
                    .hwnd(hwnd.0 as isize),
                None,
            )
        }?;
        // Torn down with the instance when anything below fails.
        let partial = (instance.clone(), surface_fn.clone(), surface);
        Self::with_surface(instance, surface_fn, surface).inspect_err(|_| {
            // SAFETY: the surface and instance made above, unused.
            unsafe {
                partial.1.destroy_surface(partial.2, None);
                partial.0.destroy_instance(None);
            }
        })
    }

    /// The rest, on the instance's surface. A failure part way leaks what was made: the
    /// process ends right after.
    fn with_surface(
        instance: ash::Instance,
        surface_fn: ash::khr::surface::Instance,
        surface: vk::SurfaceKHR,
    ) -> ash::prelude::VkResult<Self> {
        // SAFETY: a live instance.
        let physicals = unsafe { instance.enumerate_physical_devices() }?;
        let (physical, family) = physicals
            .iter()
            .find_map(|&physical| {
                // SAFETY: a physical device of this instance.
                let families =
                    unsafe { instance.get_physical_device_queue_family_properties(physical) };
                families.iter().enumerate().find_map(|(i, f)| {
                    let i = u32::try_from(i).ok()?;
                    // SAFETY: as above, with this instance's surface.
                    let present = unsafe {
                        surface_fn.get_physical_device_surface_support(physical, i, surface)
                    }
                    .unwrap_or(false);
                    (present && f.queue_flags.contains(vk::QueueFlags::GRAPHICS))
                        .then_some((physical, i))
                })
            })
            .ok_or(vk::Result::ERROR_INITIALIZATION_FAILED)?;
        let priorities = [1.0f32];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let extensions = [c"VK_KHR_swapchain".as_ptr()];
        // SAFETY: a physical device of this instance and a complete create info.
        let device = unsafe {
            instance.create_device(
                physical,
                &vk::DeviceCreateInfo::default()
                    .queue_create_infos(&queues)
                    .enabled_extension_names(&extensions),
                None,
            )
        }?;
        // SAFETY: a physical device of this instance.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        // SAFETY: the queue created with the device.
        let queue = unsafe { device.get_device_queue(family, 0) };
        let swapchain_fn = ash::khr::swapchain::Device::new(&instance, &device);
        let (swapchain, extent, format) =
            make_swapchain(&surface_fn, &swapchain_fn, physical, surface)?;
        // SAFETY: the swapchain made above, and complete create infos on the live device.
        unsafe {
            let images = swapchain_fn.get_swapchain_images(swapchain)?;
            let pool = device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                    .queue_family_index(family),
                None,
            )?;
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
            let acquired = device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)?;
            let rendered = images
                .iter()
                .map(|_| device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Self {
                instance,
                surface_fn,
                surface,
                device,
                swapchain_fn,
                swapchain,
                images,
                extent,
                bgra: format == vk::Format::B8G8R8A8_UNORM,
                memory,
                queue,
                pool,
                cmd,
                fence,
                acquired,
                rendered,
            })
        }
    }

    /// Clear and present `frames` frames, then read one image back.
    fn run(&self, frames: u32) -> Result<u32, String> {
        let mut presented = 0;
        for _ in 0..frames {
            crate::d3d::pump();
            if self.frame().map_err(|e| e.to_string())? {
                presented += 1;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        match self.read_back() {
            Ok(frame) => {
                let drawn = frame.differing(CLEAR);
                let total = (frame.width * frame.height) as usize;
                // The overlay's windows cover much of the frame, but not nine tenths of it.
                if drawn * 10 > total * 9 {
                    println!("vk read back unreliable ({drawn} of {total} pixels differ)");
                } else {
                    println!("vk drawn pixels {drawn}");
                }
                if let Err(e) = frame.save_bmp(std::path::Path::new("capture-vk.bmp")) {
                    println!("vk capture not saved: {e}");
                }
            }
            Err(e) => println!("vk read back failed: {e}"),
        }
        Ok(presented)
    }

    /// One frame: clear an image and present it. `false` when the present failed.
    fn frame(&self) -> ash::prelude::VkResult<bool> {
        let d = &self.device;
        // SAFETY: objects of this device; the fence orders each frame after the last one's
        // submission, so the command buffer and the acquire semaphore are free again.
        unsafe {
            d.wait_for_fences(&[self.fence], true, TIMEOUT_NS)?;
            let (index, _) = self.swapchain_fn.acquire_next_image(
                self.swapchain,
                TIMEOUT_NS,
                self.acquired,
                vk::Fence::null(),
            )?;
            let image = self.images[index as usize];
            let rendered = self.rendered[index as usize];
            d.reset_fences(&[self.fence])?;
            self.begin()?;
            self.barrier(
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            );
            d.cmd_clear_color_image(
                self.cmd,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue {
                    float32: CLEAR.map(|c| f32::from(c) / 255.0),
                },
                &[COLOR],
            );
            self.barrier(
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::PRESENT_SRC_KHR,
            );
            d.end_command_buffer(self.cmd)?;
            let waits = [self.acquired];
            let stages = [vk::PipelineStageFlags::TRANSFER];
            let cmds = [self.cmd];
            let signals = [rendered];
            d.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default()
                    .wait_semaphores(&waits)
                    .wait_dst_stage_mask(&stages)
                    .command_buffers(&cmds)
                    .signal_semaphores(&signals)],
                self.fence,
            )?;
            let swapchains = [self.swapchain];
            let indices = [index];
            let presented = self.swapchain_fn.queue_present(
                self.queue,
                &vk::PresentInfoKHR::default()
                    .wait_semaphores(&signals)
                    .swapchains(&swapchains)
                    .image_indices(&indices),
            );
            Ok(presented.is_ok())
        }
    }

    /// Acquire an image once more and copy it out.
    fn read_back(&self) -> ash::prelude::VkResult<Frame> {
        let d = &self.device;
        let (width, height) = (self.extent.width, self.extent.height);
        let size = u64::from(width) * u64::from(height) * 4;
        // SAFETY: objects of this device; every wait below completes before the next use.
        unsafe {
            d.queue_wait_idle(self.queue)?;
            d.wait_for_fences(&[self.fence], true, TIMEOUT_NS)?;
            d.reset_fences(&[self.fence])?;
            let (index, _) = self.swapchain_fn.acquire_next_image(
                self.swapchain,
                TIMEOUT_NS,
                vk::Semaphore::null(),
                self.fence,
            )?;
            d.wait_for_fences(&[self.fence], true, TIMEOUT_NS)?;
            d.reset_fences(&[self.fence])?;
            let image = self.images[index as usize];
            let buffer = d.create_buffer(
                &vk::BufferCreateInfo::default()
                    .size(size)
                    .usage(vk::BufferUsageFlags::TRANSFER_DST),
                None,
            )?;
            let requirements = d.get_buffer_memory_requirements(buffer);
            let wanted =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let Some(kind) = (0..self.memory.memory_type_count).find(|&i| {
                requirements.memory_type_bits & (1 << i) != 0
                    && self.memory.memory_types[i as usize]
                        .property_flags
                        .contains(wanted)
            }) else {
                d.destroy_buffer(buffer, None);
                return Err(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY);
            };
            let memory = d.allocate_memory(
                &vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(kind),
                None,
            )?;
            let copied = self.copy_out(image, buffer, memory, size);
            d.destroy_buffer(buffer, None);
            d.free_memory(memory, None);
            let mut rgba = copied?;
            if self.bgra {
                for pixel in rgba.as_chunks_mut::<4>().0 {
                    pixel.swap(0, 2);
                }
            }
            Ok(Frame {
                width,
                height,
                rgba,
            })
        }
    }

    /// Copy `image` (last presented) into `buffer` and return its bytes.
    ///
    /// # Safety
    /// `buffer` is unbound, `memory` host visible and at least `size` bytes, and the device
    /// is idle.
    unsafe fn copy_out(
        &self,
        image: vk::Image,
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
        size: u64,
    ) -> ash::prelude::VkResult<Vec<u8>> {
        let d = &self.device;
        // SAFETY: the caller's contract.
        unsafe {
            d.bind_buffer_memory(buffer, memory, 0)?;
            self.begin()?;
            self.barrier(
                image,
                vk::ImageLayout::PRESENT_SRC_KHR,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            );
            d.cmd_copy_image_to_buffer(
                self.cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[vk::BufferImageCopy {
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_extent: vk::Extent3D {
                        width: self.extent.width,
                        height: self.extent.height,
                        depth: 1,
                    },
                    ..vk::BufferImageCopy::default()
                }],
            );
            d.end_command_buffer(self.cmd)?;
            let cmds = [self.cmd];
            d.queue_submit(
                self.queue,
                &[vk::SubmitInfo::default().command_buffers(&cmds)],
                self.fence,
            )?;
            d.wait_for_fences(&[self.fence], true, TIMEOUT_NS)?;
            let mapped = d.map_memory(memory, 0, size, vk::MemoryMapFlags::empty())?;
            let len = usize::try_from(size).unwrap_or(0);
            let bytes = core::slice::from_raw_parts(mapped.cast::<u8>(), len).to_vec();
            d.unmap_memory(memory);
            Ok(bytes)
        }
    }

    /// Reset and begin the command buffer.
    ///
    /// # Safety
    /// Its last submission is complete.
    unsafe fn begin(&self) -> ash::prelude::VkResult<()> {
        // SAFETY: the caller's contract; the pool allows resetting single buffers.
        unsafe {
            self.device
                .reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
            self.device.begin_command_buffer(
                self.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
        }
    }

    /// Record a full barrier moving `image` from `from` to `to`.
    ///
    /// # Safety
    /// The command buffer is recording.
    unsafe fn barrier(&self, image: vk::Image, from: vk::ImageLayout, to: vk::ImageLayout) {
        let access = vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE;
        // SAFETY: the caller's contract.
        unsafe {
            self.device.cmd_pipeline_barrier(
                self.cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[vk::ImageMemoryBarrier::default()
                    .src_access_mask(access)
                    .dst_access_mask(access)
                    .old_layout(from)
                    .new_layout(to)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(COLOR)],
            );
        }
    }

    fn destroy(self) {
        let d = &self.device;
        // SAFETY: the device idles first, so nothing below is in use.
        unsafe {
            let _ = d.device_wait_idle();
            for semaphore in self.rendered.iter().chain([&self.acquired]) {
                d.destroy_semaphore(*semaphore, None);
            }
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.pool, None);
            self.swapchain_fn.destroy_swapchain(self.swapchain, None);
            d.destroy_device(None);
            self.surface_fn.destroy_surface(self.surface, None);
            self.instance.destroy_instance(None);
        }
    }
}

/// A swapchain on `surface`: its handle, size and format.
fn make_swapchain(
    surface_fn: &ash::khr::surface::Instance,
    swapchain_fn: &ash::khr::swapchain::Device,
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> ash::prelude::VkResult<(vk::SwapchainKHR, vk::Extent2D, vk::Format)> {
    // SAFETY: a physical device and a surface of the same instance.
    let (caps, formats) = unsafe {
        (
            surface_fn.get_physical_device_surface_capabilities(physical, surface)?,
            surface_fn.get_physical_device_surface_formats(physical, surface)?,
        )
    };
    let format = formats
        .iter()
        .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
        .or_else(|| formats.first())
        .copied()
        .ok_or(vk::Result::ERROR_FORMAT_NOT_SUPPORTED)?;
    let extent = if caps.current_extent.width == u32::MAX {
        vk::Extent2D {
            width: crate::window::WIDTH.cast_unsigned(),
            height: crate::window::HEIGHT.cast_unsigned(),
        }
    } else {
        caps.current_extent
    };
    let count = if caps.max_image_count == 0 {
        caps.min_image_count + 1
    } else {
        (caps.min_image_count + 1).min(caps.max_image_count)
    };
    // SAFETY: a complete create info for this device's surface.
    let swapchain = unsafe {
        swapchain_fn.create_swapchain(
            &vk::SwapchainCreateInfoKHR::default()
                .surface(surface)
                .min_image_count(count)
                .image_format(format.format)
                .image_color_space(format.color_space)
                .image_extent(extent)
                .image_array_layers(1)
                .image_usage(
                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::TRANSFER_DST
                        | vk::ImageUsageFlags::TRANSFER_SRC,
                )
                .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
                .pre_transform(caps.current_transform)
                .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
                .present_mode(vk::PresentModeKHR::FIFO)
                .clipped(false),
            None,
        )
    }?;
    Ok((swapchain, extent, format.format))
}
