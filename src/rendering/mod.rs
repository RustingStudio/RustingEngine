pub mod benchmark;
pub mod camera;
pub mod capture;
pub mod compute_profile;
/// Compatibility path for physics profiles; the legacy pipeline registry is gone.
pub mod compute_registry {
    pub use super::compute_profile::{
        physics_execution, ComputeShaderType, PhysicsExecution, ShaderBindings,
    };
}
pub mod debug_overlay;
#[cfg(feature = "ui")]
pub mod egui_painter;
pub mod frame_pacer;
pub mod frame_passes;
pub mod gpu_cloth;
mod gpu_particles;
mod gpu_skinning;
mod lightmap;
mod post_effects;
pub mod readback;
pub mod render_scale;
pub mod scene_renderer;
pub mod shader_graph;
mod sky;
pub mod swapchain;

use std::sync::Arc;
use vulkano::device::physical::{PhysicalDevice, PhysicalDeviceType};
use vulkano::device::{
    Device, DeviceCreateInfo, Queue, QueueCreateInfo, QueueFlags,
};
use vulkano::instance::debug::ValidationFeatureEnable;
use vulkano::instance::debug::{
    DebugUtilsMessageSeverity, DebugUtilsMessageType,
    DebugUtilsMessengerCallback, DebugUtilsMessengerCreateInfo,
};
use vulkano::instance::{Instance, InstanceCreateInfo, InstanceExtensions};
use vulkano::VulkanLibrary;

/// Error returned when `RUSTING_VULKAN_DEVICE` does not match any available
/// physical device.
#[derive(Debug)]
pub struct DeviceSelectionError {
    pub selector: String,
    pub available: Vec<String>,
}

impl std::fmt::Display for DeviceSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RUSTING_VULKAN_DEVICE={:?} matches no available Vulkan device; available devices: [{}]",
            self.selector,
            self.available.join(", ")
        )
    }
}

impl std::error::Error for DeviceSelectionError {}

/// Selects a physical device index from `names` by exact index or by
/// case-insensitive name substring. Returns every available name in the
/// error when nothing matches, instead of falling back silently.
pub fn select_device_index(
    names: &[String],
    selector: &str,
) -> Result<usize, DeviceSelectionError> {
    if let Ok(index) = selector.parse::<usize>() {
        if index < names.len() {
            return Ok(index);
        }
    }

    let needle = selector.to_lowercase();
    names
        .iter()
        .position(|name| name.to_lowercase().contains(&needle))
        .ok_or_else(|| DeviceSelectionError {
            selector: selector.to_string(),
            available: names.to_vec(),
        })
}

/// Picks one of `candidates` honoring `RUSTING_VULKAN_DEVICE` when set,
/// otherwise preferring a discrete GPU over an integrated one over anything
/// else. Returns a [`DeviceSelectionError`] listing every candidate name
/// when the selector matches nothing. `candidates` must not be empty.
fn select_physical_device(
    candidates: Vec<(Arc<PhysicalDevice>, u32)>,
) -> Result<(Arc<PhysicalDevice>, u32), DeviceSelectionError> {
    Ok(match std::env::var("RUSTING_VULKAN_DEVICE") {
        Ok(selector) => {
            let names: Vec<String> = candidates
                .iter()
                .map(|(p, _)| p.properties().device_name.clone())
                .collect();
            let index = select_device_index(&names, &selector)?;
            candidates.into_iter().nth(index).unwrap()
        }
        Err(_) => candidates
            .into_iter()
            .min_by_key(|(p, _)| match p.properties().device_type {
                PhysicalDeviceType::DiscreteGpu => 0,
                PhysicalDeviceType::IntegratedGpu => 1,
                _ => 2,
            })
            .unwrap(),
    })
}

/// A Vulkan instance, physical device, and logical device created without a
/// surface, a swapchain, or a winit window — for offscreen rendering,
/// compute dispatch, and tests on machines with no display.
pub struct HeadlessVulkanBase {
    pub instance: Arc<Instance>,
    pub device: Arc<Device>,
    pub queue: Arc<Queue>,
}

pub fn init_vulkan_headless() -> HeadlessVulkanBase {
    let base =
        try_init_vulkan_headless().unwrap_or_else(|error| panic!("{error}"));
    let properties = base.device.physical_device().properties();
    println!(
        "Selected headless Vulkan device: {} (vendor 0x{:x}, driver {}, api {})",
        properties.device_name,
        properties.vendor_id,
        properties.driver_version,
        properties.api_version,
    );
    base
}

/// Opens the device a headless run would pick and has it fill a small
/// buffer, so a driver that lists a device but cannot run work fails here.
/// Returns the device name.
pub fn probe_vulkan() -> Result<String, String> {
    use vulkano::buffer::{Buffer, BufferCreateInfo, BufferUsage};
    use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
    use vulkano::command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage,
        PrimaryCommandBufferAbstract,
    };
    use vulkano::memory::allocator::{
        AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
    };
    use vulkano::sync::GpuFuture;
    let base = try_init_vulkan_headless()?;
    let name = base
        .device
        .physical_device()
        .properties()
        .device_name
        .clone();
    let failed = |error: &dyn std::fmt::Display| format!("{name}: {error}");
    let buffer = Buffer::new_slice::<u32>(
        Arc::new(StandardMemoryAllocator::new_default(base.device.clone())),
        BufferCreateInfo {
            usage: BufferUsage::TRANSFER_DST,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_HOST
                | MemoryTypeFilter::HOST_RANDOM_ACCESS,
            ..Default::default()
        },
        64,
    )
    .map_err(|error| failed(&error))?;
    let mut builder = AutoCommandBufferBuilder::primary(
        Arc::new(StandardCommandBufferAllocator::new(
            base.device.clone(),
            Default::default(),
        )),
        base.queue.queue_family_index(),
        CommandBufferUsage::OneTimeSubmit,
    )
    .map_err(|error| failed(&error))?;
    builder
        .fill_buffer(buffer.clone(), 0x5275_7374)
        .map_err(|error| failed(&error))?;
    builder
        .build()
        .map_err(|error| failed(&error))?
        .execute(base.queue.clone())
        .map_err(|error| failed(&error))?
        .then_signal_fence_and_flush()
        .map_err(|error| failed(&error))?
        .wait(Some(std::time::Duration::from_secs(5)))
        .map_err(|error| failed(&error))?;
    let read = buffer.read().map_err(|error| failed(&error))?;
    if read.iter().all(|&word| word == 0x5275_7374) {
        Ok(name)
    } else {
        Err(format!(
            "{name}: the device ran the work but returned wrong data"
        ))
    }
}

/// Like [`init_vulkan_headless`], but reports a missing driver or device as
/// an error and prints nothing, for tools whose stdout is machine output.
pub fn try_init_vulkan_headless() -> Result<HeadlessVulkanBase, String> {
    let library = VulkanLibrary::new()
        .map_err(|error| format!("no Vulkan driver found: {error}"))?;
    // Debug labels are recorded whenever the driver offers them, so headless
    // GPU tests also exercise the renderer's label scopes.
    let enabled_extensions = InstanceExtensions {
        ext_debug_utils: library.supported_extensions().ext_debug_utils,
        ..InstanceExtensions::empty()
    };
    let instance = Instance::new(
        library,
        InstanceCreateInfo {
            enabled_extensions,
            ..Default::default()
        },
    )
    .map_err(|error| format!("failed to create a Vulkan instance: {error}"))?;

    let candidates: Vec<_> = instance
        .enumerate_physical_devices()
        .map_err(|error| format!("failed to list Vulkan devices: {error}"))?
        .filter_map(|p| {
            p.queue_family_properties()
                .iter()
                .enumerate()
                .position(|(_, q)| {
                    q.queue_flags
                        .intersects(QueueFlags::GRAPHICS | QueueFlags::COMPUTE)
                })
                .map(|i| (p, i as u32))
        })
        .collect();

    if candidates.is_empty() {
        return Err("no Vulkan device supports graphics or compute".into());
    }
    let (physical_device, queue_family_index) =
        select_physical_device(candidates)
            .map_err(|error| error.to_string())?;

    let enabled_features = optional_device_features(&physical_device);
    let create = || {
        Device::new(
            physical_device.clone(),
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                enabled_features,
                ..Default::default()
            },
        )
    };
    // ponytail: NVIDIA reports InitializationFailed while too many devices
    // are alive at once (12 parallel GPU tests); a short bounded retry lets
    // the others finish. Share one device per test process if this grows.
    let mut result = create();
    for attempt in 1..=10 {
        match result {
            Err(vulkano::Validated::Error(
                vulkano::VulkanError::InitializationFailed,
            )) => {
                std::thread::sleep(std::time::Duration::from_millis(
                    50 * attempt,
                ));
                result = create();
            }
            _ => break,
        }
    }
    let (device, mut queues) = result.map_err(|error| {
        format!("failed to create a headless Vulkan device: {error}")
    })?;

    let queue = queues.next().unwrap();

    Ok(HeadlessVulkanBase {
        instance,
        device,
        queue,
    })
}

/// Optional features the renderer uses when the device has them. The
/// renderer reads back what was enabled from `Device::enabled_features`.
fn optional_device_features(
    physical_device: &vulkano::device::physical::PhysicalDevice,
) -> vulkano::device::DeviceFeatures {
    vulkano::device::DeviceFeatures {
        sampler_anisotropy: physical_device
            .supported_features()
            .sampler_anisotropy,
        ..vulkano::device::DeviceFeatures::empty()
    }
}

/// `VulkanoConfig::default()` plus the validation layer (see
/// `validation_instance_info`), the `RUSTING_VULKAN_DEVICE` device choice,
/// and the optional features the renderer uses when the device
/// `VulkanoContext` will pick has them. `VulkanoContext`
/// panics on a requested feature the device lacks, so the pick is probed
/// first with a short-lived instance.
// ponytail: creates a second instance at startup (a few ms); drop the probe
// if vulkano-util gains a per-device feature callback.
#[cfg(feature = "window")]
pub fn vulkano_config() -> vulkano_util::context::VulkanoConfig {
    let mut config = vulkano_util::context::VulkanoConfig {
        print_device_name: true,
        ..Default::default()
    };
    let Ok(library) = VulkanLibrary::new() else {
        return config;
    };
    config.instance_create_info = InstanceCreateInfo {
        flags: config.instance_create_info.flags,
        ..validation_instance_info(&library)
    };
    let Ok(devices) = Instance::new(library, InstanceCreateInfo::default())
        .and_then(|instance| Ok(instance.enumerate_physical_devices()?))
    else {
        return config;
    };
    let candidates: Vec<_> =
        devices.filter(|p| (config.device_filter_fn)(p)).collect();
    let picked = match std::env::var("RUSTING_VULKAN_DEVICE") {
        Ok(selector) => {
            let names: Vec<String> = candidates
                .iter()
                .map(|p| p.properties().device_name.clone())
                .collect();
            let index = select_device_index(&names, &selector)
                .unwrap_or_else(|err| panic!("{err}"));
            let name = names[index].clone();
            config.device_filter_fn =
                Arc::new(move |p| p.properties().device_name == name);
            candidates.into_iter().nth(index)
        }
        Err(_) => candidates
            .into_iter()
            .min_by_key(|p| (config.device_priority_fn)(p)),
    };
    if let Some(physical_device) = picked {
        config.device_features = optional_device_features(&physical_device);
    }
    config
}

/// Instance settings that turn on the Khronos validation layer in debug
/// builds or with the `validation` feature, when the layer is installed.
/// Set `RUSTING_GPU_VALIDATION=1` to add GPU-assisted validation.
fn validation_instance_info(library: &VulkanLibrary) -> InstanceCreateInfo {
    let mut required_extensions = InstanceExtensions::empty();
    let validation_enabled = (cfg!(debug_assertions)
        || cfg!(feature = "validation"))
        && library.layer_properties().is_ok_and(|mut layers| {
            layers.any(|layer| layer.name() == "VK_LAYER_KHRONOS_validation")
        });
    let validation_callback = unsafe {
        DebugUtilsMessengerCallback::new(|severity, message_type, data| {
            eprintln!(
                "[Vulkan {severity:?} {message_type:?}] {}: {}",
                data.message_id_name.unwrap_or("unknown"),
                data.message
            );
        })
    };
    let debug_create_info = DebugUtilsMessengerCreateInfo {
        message_severity: DebugUtilsMessageSeverity::ERROR
            | DebugUtilsMessageSeverity::WARNING,
        message_type: DebugUtilsMessageType::GENERAL
            | DebugUtilsMessageType::VALIDATION
            | DebugUtilsMessageType::PERFORMANCE,
        ..DebugUtilsMessengerCreateInfo::user_callback(validation_callback)
    };

    let mut enabled_validation_features = Vec::new();
    if validation_enabled
        && library.supported_extensions().ext_validation_features
    {
        required_extensions.ext_validation_features = true;
        enabled_validation_features
            .push(ValidationFeatureEnable::SynchronizationValidation);
        let gpu_validation_enabled = std::env::var("RUSTING_GPU_VALIDATION")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE"));
        if gpu_validation_enabled {
            enabled_validation_features
                .push(ValidationFeatureEnable::GpuAssisted);
            enabled_validation_features
                .push(ValidationFeatureEnable::GpuAssistedReserveBindingSlot);
        }
    }
    let debug_utils_enabled =
        validation_enabled && library.supported_extensions().ext_debug_utils;
    if debug_utils_enabled {
        required_extensions.ext_debug_utils = true;
    }

    InstanceCreateInfo {
        enabled_extensions: required_extensions,
        enabled_layers: validation_enabled
            .then(|| "VK_LAYER_KHRONOS_validation".to_owned())
            .into_iter()
            .collect(),
        debug_utils_messengers: debug_utils_enabled
            .then_some(debug_create_info)
            .into_iter()
            .collect(),
        enabled_validation_features,
        ..Default::default()
    }
}

/// Shared GPU-test fixtures for the crate: the headless device is created at
/// most once per test binary run instead of once per test, and a
/// compute-dispatch fixture covers the common "upload known input, dispatch,
/// read back, assert" shape.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{init_vulkan_headless, HeadlessVulkanBase};
    use crate::rendering::readback::read_back_buffer;
    use std::sync::{Arc, OnceLock};
    use vulkano::buffer::{
        Buffer, BufferContents, BufferCreateInfo, BufferUsage,
    };
    use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
    use vulkano::command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage,
    };
    use vulkano::descriptor_set::allocator::StandardDescriptorSetAllocator;
    use vulkano::descriptor_set::{DescriptorSet, WriteDescriptorSet};
    use vulkano::memory::allocator::{
        AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
    };
    use vulkano::pipeline::compute::ComputePipelineCreateInfo;
    use vulkano::pipeline::layout::PipelineDescriptorSetLayoutCreateInfo;
    use vulkano::pipeline::{
        ComputePipeline, Pipeline, PipelineBindPoint, PipelineLayout,
        PipelineShaderStageCreateInfo,
    };
    use vulkano::shader::EntryPoint;
    use vulkano::sync::GpuFuture;

    static HEADLESS_DEVICE: OnceLock<HeadlessVulkanBase> = OnceLock::new();

    /// Returns the shared headless Vulkan device, creating it on first use.
    /// Panics if no Vulkan driver is present; callers should skip instead by
    /// checking `vulkano::VulkanLibrary::new()` before calling this.
    pub(crate) fn headless_device() -> &'static HeadlessVulkanBase {
        HEADLESS_DEVICE.get_or_init(init_vulkan_headless)
    }

    /// Uploads `input` to a single storage buffer bound at set 0 binding 0,
    /// dispatches `entry_point` over `workgroups`, and reads the buffer back
    /// — the common "upload known input, dispatch, read back" shape shared
    /// by compute-dispatch tests.
    pub(crate) fn dispatch_and_read_back<T>(
        base: &HeadlessVulkanBase,
        entry_point: EntryPoint,
        input: Vec<T>,
        workgroups: [u32; 3],
    ) -> Vec<T>
    where
        T: BufferContents + Clone,
    {
        let memory_allocator =
            Arc::new(StandardMemoryAllocator::new_default(base.device.clone()));
        let command_buffer_allocator =
            Arc::new(StandardCommandBufferAllocator::new(
                base.device.clone(),
                Default::default(),
            ));
        let descriptor_set_allocator =
            Arc::new(StandardDescriptorSetAllocator::new(
                base.device.clone(),
                Default::default(),
            ));

        let buffer = Buffer::from_iter(
            memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_SRC,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            input,
        )
        .unwrap();

        let stage = PipelineShaderStageCreateInfo::new(entry_point);
        let layout = PipelineLayout::new(
            base.device.clone(),
            PipelineDescriptorSetLayoutCreateInfo::from_stages([&stage])
                .into_pipeline_layout_create_info(base.device.clone())
                .unwrap(),
        )
        .unwrap();
        let pipeline = ComputePipeline::new(
            base.device.clone(),
            None,
            ComputePipelineCreateInfo::stage_layout(stage, layout),
        )
        .unwrap();

        let set = DescriptorSet::new(
            descriptor_set_allocator,
            pipeline.layout().set_layouts()[0].clone(),
            [WriteDescriptorSet::buffer(0, buffer.clone())],
            [],
        )
        .unwrap();

        let mut builder = AutoCommandBufferBuilder::primary(
            command_buffer_allocator.clone(),
            base.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .unwrap();
        builder
            .bind_pipeline_compute(pipeline.clone())
            .unwrap()
            .bind_descriptor_sets(
                PipelineBindPoint::Compute,
                pipeline.layout().clone(),
                0,
                set,
            )
            .unwrap();
        unsafe { builder.dispatch(workgroups) }.unwrap();
        let command_buffer = builder.build().unwrap();

        let future = vulkano::sync::now(base.device.clone())
            .then_execute(base.queue.clone(), command_buffer)
            .unwrap()
            .then_signal_fence_and_flush()
            .unwrap();
        future.wait(None).unwrap();

        read_back_buffer(
            &base.device,
            &base.queue,
            &memory_allocator,
            &command_buffer_allocator,
            buffer,
        )
    }

    /// Compares `actual` against a golden PNG at `golden_path`, allowing up
    /// to `tolerance` difference per color channel per pixel. On mismatch (or
    /// a dimension difference) writes `actual.png`, `expected.png`, and
    /// `diff.png` under `artifact_dir` before panicking, so the mismatch can
    /// be inspected instead of only reported as a number.
    pub(crate) fn assert_matches_golden_image(
        actual: &image::RgbaImage,
        golden_path: &std::path::Path,
        artifact_dir: &std::path::Path,
        tolerance: u8,
    ) {
        let expected = image::open(golden_path)
            .unwrap_or_else(|error| {
                panic!("golden image {golden_path:?} missing or unreadable: {error}")
            })
            .to_rgba8();

        if actual.dimensions() != expected.dimensions() {
            std::fs::create_dir_all(artifact_dir).unwrap();
            actual.save(artifact_dir.join("actual.png")).unwrap();
            expected.save(artifact_dir.join("expected.png")).unwrap();
            panic!(
                "golden image mismatch: actual size {:?} != expected size {:?}; artifacts written to {artifact_dir:?}",
                actual.dimensions(),
                expected.dimensions(),
            );
        }

        let mut diff = image::RgbaImage::new(actual.width(), actual.height());
        let mut max_diff = 0u8;
        for ((actual_pixel, expected_pixel), diff_pixel) in actual
            .pixels()
            .zip(expected.pixels())
            .zip(diff.pixels_mut())
        {
            for channel in 0..4 {
                let delta =
                    actual_pixel[channel].abs_diff(expected_pixel[channel]);
                max_diff = max_diff.max(delta);
                diff_pixel[channel] = delta;
            }
            diff_pixel[3] = 255;
        }

        if max_diff > tolerance {
            std::fs::create_dir_all(artifact_dir).unwrap();
            actual.save(artifact_dir.join("actual.png")).unwrap();
            expected.save(artifact_dir.join("expected.png")).unwrap();
            diff.save(artifact_dir.join("diff.png")).unwrap();
            panic!(
                "golden image mismatch: max per-channel difference {max_diff} exceeds tolerance {tolerance}; artifacts written to {artifact_dir:?}"
            );
        }
    }

    #[test]
    fn identical_image_matches_its_own_golden_with_zero_tolerance() {
        let scratch =
            std::env::temp_dir().join("rusting_engine_golden_test_identical");
        std::fs::create_dir_all(&scratch).unwrap();
        let golden_path = scratch.join("golden.png");
        let artifact_dir = scratch.join("artifacts");

        let image = image::RgbaImage::from_fn(4, 4, |x, y| {
            image::Rgba([(x * 16) as u8, (y * 16) as u8, 0, 255])
        });
        image.save(&golden_path).unwrap();

        assert_matches_golden_image(&image, &golden_path, &artifact_dir, 0);
        assert!(!artifact_dir.exists());

        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn a_small_difference_within_tolerance_passes() {
        let scratch =
            std::env::temp_dir().join("rusting_engine_golden_test_tolerance");
        std::fs::create_dir_all(&scratch).unwrap();
        let golden_path = scratch.join("golden.png");
        let artifact_dir = scratch.join("artifacts");

        let golden = image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([100, 100, 100, 255]),
        );
        golden.save(&golden_path).unwrap();
        let actual = image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([102, 100, 100, 255]),
        );

        assert_matches_golden_image(&actual, &golden_path, &artifact_dir, 5);
        assert!(!artifact_dir.exists());

        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn a_mismatch_beyond_tolerance_panics_and_writes_artifact_images() {
        let scratch =
            std::env::temp_dir().join("rusting_engine_golden_test_mismatch");
        std::fs::create_dir_all(&scratch).unwrap();
        let golden_path = scratch.join("golden.png");
        let artifact_dir = scratch.join("artifacts");

        let golden =
            image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        golden.save(&golden_path).unwrap();
        let actual =
            image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));

        let result = std::panic::catch_unwind(|| {
            assert_matches_golden_image(
                &actual,
                &golden_path,
                &artifact_dir,
                5,
            );
        });

        assert!(
            result.is_err(),
            "expected a mismatch beyond tolerance to panic"
        );
        assert!(artifact_dir.join("actual.png").exists());
        assert!(artifact_dir.join("expected.png").exists());
        assert!(artifact_dir.join("diff.png").exists());

        std::fs::remove_dir_all(&scratch).unwrap();
    }
}

#[cfg(test)]
mod headless_device_tests {
    use super::*;

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn headless_device_creation_succeeds_with_a_vulkan_driver_present() {
        if VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let base = init_vulkan_headless();
        assert!(!base
            .device
            .physical_device()
            .properties()
            .device_name
            .is_empty());
    }
}

#[cfg(test)]
mod debug_utils_tests {
    use super::*;
    use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
    use vulkano::command_buffer::{
        AutoCommandBufferBuilder, CommandBufferUsage,
    };
    use vulkano::instance::debug::DebugUtilsLabel;

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn object_names_and_command_buffer_labels_are_accepted_by_the_driver() {
        let Ok(library) = VulkanLibrary::new() else {
            eprintln!("skipping: no Vulkan driver present");
            return;
        };
        if !library.supported_extensions().ext_debug_utils {
            eprintln!("skipping: driver has no ext_debug_utils");
            return;
        }

        let instance = Instance::new(
            library,
            InstanceCreateInfo {
                enabled_extensions: vulkano::instance::InstanceExtensions {
                    ext_debug_utils: true,
                    ..InstanceExtensions::empty()
                },
                ..Default::default()
            },
        )
        .expect("Failed to create instance with ext_debug_utils");

        let candidates: Vec<_> = instance
            .enumerate_physical_devices()
            .unwrap()
            .filter_map(|p| {
                p.queue_family_properties()
                    .iter()
                    .enumerate()
                    .position(|(_, q)| {
                        q.queue_flags.intersects(
                            QueueFlags::GRAPHICS | QueueFlags::COMPUTE,
                        )
                    })
                    .map(|i| (p, i as u32))
            })
            .collect();
        let (physical_device, queue_family_index) =
            select_physical_device(candidates)
                .unwrap_or_else(|error| panic!("{error}"));

        let (device, mut queues) = Device::new(
            physical_device,
            DeviceCreateInfo {
                queue_create_infos: vec![QueueCreateInfo {
                    queue_family_index,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .expect("Failed to create device");
        let queue = queues.next().unwrap();

        device
            .set_debug_utils_object_name(&queue, Some("test queue"))
            .expect("naming the queue must succeed");

        let command_buffer_allocator = StandardCommandBufferAllocator::new(
            device.clone(),
            Default::default(),
        );
        let mut builder = AutoCommandBufferBuilder::primary(
            Arc::new(command_buffer_allocator),
            queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .unwrap();
        builder
            .begin_debug_utils_label(DebugUtilsLabel {
                label_name: "test scope".to_string(),
                ..Default::default()
            })
            .expect("beginning a label must succeed");
        unsafe {
            builder
                .end_debug_utils_label()
                .expect("ending a label after a matching begin must succeed");
        }
        let command_buffer = builder.build().unwrap();

        use vulkano::sync::GpuFuture;
        vulkano::sync::now(device.clone())
            .then_execute(queue, command_buffer)
            .unwrap()
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
    }
}

#[cfg(test)]
mod device_selection_tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![
            "llvmpipe (LLVM 18.0.0, 256 bits)".to_string(),
            "NVIDIA GeForce RTX 3080".to_string(),
            "Intel(R) UHD Graphics 630".to_string(),
        ]
    }

    #[test]
    fn selects_by_exact_index() {
        assert_eq!(select_device_index(&names(), "1").unwrap(), 1);
    }

    #[test]
    fn selects_by_case_insensitive_name_substring() {
        assert_eq!(select_device_index(&names(), "nvidia").unwrap(), 1);
        assert_eq!(select_device_index(&names(), "llvmpipe").unwrap(), 0);
    }

    #[test]
    fn out_of_range_index_falls_back_to_name_match_and_then_fails() {
        // "99" is out of range as an index and matches no device name.
        let err = select_device_index(&names(), "99").unwrap_err();
        assert_eq!(err.selector, "99");
        assert_eq!(err.available, names());
    }

    #[test]
    fn no_match_lists_every_available_device_in_the_error() {
        let err = select_device_index(&names(), "does-not-exist").unwrap_err();
        assert_eq!(err.available, names());
        assert!(err.to_string().contains("llvmpipe"));
        assert!(err.to_string().contains("NVIDIA GeForce RTX 3080"));
        assert!(err.to_string().contains("Intel(R) UHD Graphics 630"));
    }
}
