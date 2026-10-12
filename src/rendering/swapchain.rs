use std::sync::Arc;
use vulkano::device::Device;
use vulkano::format::Format;
use vulkano::image::view::ImageView;
use vulkano::image::{Image, ImageCreateInfo, ImageUsage};
use vulkano::memory::allocator::{
    AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
};
use vulkano::render_pass::{Framebuffer, FramebufferCreateInfo, RenderPass};
use vulkano::swapchain::{
    CompositeAlpha, PresentMode, Surface, Swapchain, SwapchainCreateInfo,
};
use winit::window::Window;

pub fn create_swapchain_and_images(
    device: &Arc<Device>,
    surface: &Arc<Surface>,
    window: &Window,
) -> (Arc<Swapchain>, Vec<Arc<Image>>) {
    let caps = device
        .physical_device()
        .surface_capabilities(surface, Default::default())
        .unwrap();
    let formats = device
        .physical_device()
        .surface_formats(surface, Default::default())
        .unwrap();
    let (format, color_space) = formats
        .iter()
        .copied()
        .find(|(format, _)| {
            matches!(format, Format::B8G8R8A8_SRGB | Format::R8G8B8A8_SRGB)
        })
        .unwrap_or(formats[0]);
    let present_modes = device
        .physical_device()
        .surface_present_modes(surface, Default::default())
        .unwrap();
    let present_mode = [
        PresentMode::Mailbox,
        PresentMode::Immediate,
        PresentMode::Fifo,
    ]
    .into_iter()
    .find(|mode| present_modes.contains(mode))
    .expect("Vulkan surfaces must support FIFO presentation");
    let min_image_count = caps
        .max_image_count
        .map_or(caps.min_image_count + 1, |max| {
            (caps.min_image_count + 1).min(max)
        });

    let (sw, img) = Swapchain::new(
        device.clone(),
        surface.clone(),
        SwapchainCreateInfo {
            min_image_count,
            image_format: format,
            image_color_space: color_space,
            // Vulkan requires the surface's current extent when it has one.
            image_extent: caps
                .current_extent
                .unwrap_or_else(|| window.inner_size().into()),
            image_usage: ImageUsage::COLOR_ATTACHMENT, // We'll draw to these images
            composite_alpha: if caps
                .supported_composite_alpha
                .contains_enum(CompositeAlpha::Opaque)
            {
                CompositeAlpha::Opaque
            } else {
                caps.supported_composite_alpha.into_iter().next().unwrap()
            },
            present_mode,
            ..Default::default()
        },
    )
    .unwrap();
    (sw, img)
}

pub fn create_render_pass(
    device: Arc<Device>,
    swapchain: &Arc<Swapchain>,
) -> std::sync::Arc<RenderPass> {
    create_render_pass_for_format(device, swapchain.image_format())
}

/// The swapchain format the windowed renderers draw to. The renderers encode
/// gamma themselves, so they want a UNORM format: the UNORM form of the
/// surface's preferred format when that is RGBA, `B8G8R8A8_UNORM` otherwise.
// ponytail: vulkano_util hands the swapchain callback only the surface's
// first format; a surface offering neither 8-bit UNORM layout still fails
// at swapchain creation. Query the full list if such a surface turns up.
pub fn swapchain_format(preferred: Format) -> Format {
    match preferred {
        Format::R8G8B8A8_SRGB | Format::R8G8B8A8_UNORM => {
            Format::R8G8B8A8_UNORM
        }
        _ => Format::B8G8R8A8_UNORM,
    }
}

/// Color format used by offscreen render targets — the same choice the
/// windowed swapchain path prefers when a surface offers it.
pub const OFFSCREEN_COLOR_FORMAT: Format = Format::B8G8R8A8_SRGB;

pub fn create_render_pass_for_format(
    device: Arc<Device>,
    color_format: Format,
) -> std::sync::Arc<RenderPass> {
    vulkano::ordered_passes_renderpass!(
        device.clone(),
        attachments: {
            color: {
                format: color_format,
                samples: 1,
                load_op: Clear,
                store_op: Store,
            },
            depth: {
                format: vulkano::format::Format::D16_UNORM,
                samples: 1,
                load_op: Clear,
                store_op: DontCare,
            }
        },
        passes: [ {
            color: [color],
            depth_stencil: {depth},
            input: []
        } ],
    )
    .unwrap()
}

/// Color and depth images plus a matching framebuffer for rendering with no
/// surface or swapchain — same formats as the windowed path
/// ([`OFFSCREEN_COLOR_FORMAT`], `Format::D16_UNORM`). The color image is
/// transfer-source capable so it can be read back after rendering.
pub struct OffscreenTarget {
    pub color_image: Arc<Image>,
    pub depth_image: Arc<Image>,
    pub framebuffer: Arc<Framebuffer>,
}

pub fn create_offscreen_target(
    memory_allocator: &Arc<StandardMemoryAllocator>,
    render_pass: &Arc<RenderPass>,
    extent: [u32; 2],
) -> OffscreenTarget {
    let color_image = Image::new(
        memory_allocator.clone(),
        ImageCreateInfo {
            format: OFFSCREEN_COLOR_FORMAT,
            extent: [extent[0], extent[1], 1],
            usage: ImageUsage::COLOR_ATTACHMENT | ImageUsage::TRANSFER_SRC,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .unwrap();
    let color_view = ImageView::new_default(color_image.clone()).unwrap();

    let depth_image = Image::new(
        memory_allocator.clone(),
        ImageCreateInfo {
            format: Format::D16_UNORM,
            extent: [extent[0], extent[1], 1],
            usage: ImageUsage::DEPTH_STENCIL_ATTACHMENT
                | ImageUsage::TRANSIENT_ATTACHMENT,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .unwrap();
    let depth_view = ImageView::new_default(depth_image.clone()).unwrap();

    let framebuffer = Framebuffer::new(
        render_pass.clone(),
        FramebufferCreateInfo {
            attachments: vec![color_view, depth_view],
            ..Default::default()
        },
    )
    .unwrap();

    OffscreenTarget {
        color_image,
        depth_image,
        framebuffer,
    }
}

pub fn create_framebuffers(
    images: &[Arc<Image>],
    render_pass: &Arc<RenderPass>,
    memory_allocator: &Arc<StandardMemoryAllocator>,
) -> Vec<Arc<Framebuffer>> {
    let extent = images[0].extent();
    let dims = [extent[0], extent[1]];

    // Depth buffer is required for 3D sorting
    let depth_image = Image::new(
        memory_allocator.clone(),
        ImageCreateInfo {
            format: Format::D16_UNORM,
            extent: [dims[0], dims[1], 1],
            usage: ImageUsage::DEPTH_STENCIL_ATTACHMENT
                | ImageUsage::TRANSIENT_ATTACHMENT,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .unwrap();
    let depth_view = ImageView::new_default(depth_image).unwrap();

    images
        .iter()
        .map(|image| {
            let view = ImageView::new_default(image.clone()).unwrap();
            Framebuffer::new(
                render_pass.clone(),
                FramebufferCreateInfo {
                    attachments: vec![view, depth_view.clone()],
                    ..Default::default()
                },
            )
            .unwrap()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vulkano::VulkanLibrary;

    #[test]
    fn swapchain_format_is_the_unorm_form_of_the_surface_preference() {
        for (preferred, chosen) in [
            (Format::B8G8R8A8_SRGB, Format::B8G8R8A8_UNORM),
            (Format::B8G8R8A8_UNORM, Format::B8G8R8A8_UNORM),
            // Surfaces with no BGRA format (many Android drivers) prefer
            // RGBA, and forcing BGRA there fails swapchain creation.
            (Format::R8G8B8A8_SRGB, Format::R8G8B8A8_UNORM),
            (Format::R8G8B8A8_UNORM, Format::R8G8B8A8_UNORM),
            (Format::A2B10G10R10_UNORM_PACK32, Format::B8G8R8A8_UNORM),
        ] {
            assert_eq!(swapchain_format(preferred), chosen, "{preferred:?}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn offscreen_target_renders_with_no_surface_or_window() {
        if VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let base = crate::rendering::test_support::headless_device();
        let memory_allocator =
            Arc::new(StandardMemoryAllocator::new_default(base.device.clone()));
        let render_pass = create_render_pass_for_format(
            base.device.clone(),
            OFFSCREEN_COLOR_FORMAT,
        );
        let target =
            create_offscreen_target(&memory_allocator, &render_pass, [64, 64]);

        assert_eq!(target.color_image.extent(), [64, 64, 1]);
        assert_eq!(target.color_image.format(), OFFSCREEN_COLOR_FORMAT);
        assert_eq!(target.depth_image.format(), Format::D16_UNORM);
    }
}
