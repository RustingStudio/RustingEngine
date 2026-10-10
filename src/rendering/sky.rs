//! Procedural daylight sky: its settings for the background pass, and a
//! small equirectangular bake of it that lights the scene in place of an
//! environment map.

use std::sync::Arc;

use vulkano::command_buffer::{
    AutoCommandBufferBuilder, PrimaryAutoCommandBuffer,
};
use vulkano::descriptor_set::allocator::StandardDescriptorSetAllocator;
use vulkano::descriptor_set::{DescriptorSet, WriteDescriptorSet};
use vulkano::device::Queue;
use vulkano::format::Format;
use vulkano::image::sampler::{
    Filter, Sampler, SamplerAddressMode, SamplerCreateInfo, SamplerMipmapMode,
    LOD_CLAMP_NONE,
};
use vulkano::image::view::{ImageView, ImageViewCreateInfo};
use vulkano::image::{
    Image, ImageCreateInfo, ImageSubresourceRange, ImageUsage,
};
use vulkano::memory::allocator::{
    AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
};
use vulkano::pipeline::{ComputePipeline, Pipeline, PipelineBindPoint};

use super::scene_renderer::{
    create_compute_pipeline, name_object, record_mip_chain, SceneRenderError,
};
use crate::runtime::{
    DirectionalLight, ExtractedDirectionalLight, ProceduralSky,
};

fn error(error: impl std::fmt::Display) -> SceneRenderError {
    SceneRenderError(error.to_string())
}

/// Size of the baked sky. Lighting reads its blurry mips, so it can be
/// small.
const BAKE_EXTENT: [u32; 2] = [128, 64];

/// The sky as the shaders read it: direction toward the sun and the cosine
/// of the disc's radius, the sun's color times brightness and the haze,
/// and the ground albedo with 1 in w.
pub(super) type SkyParams = [[f32; 4]; 3];

/// Sky settings for `sky` lit by the shadow-casting directional light,
/// else the first one, else a default sun of brightness 1 high in the
/// south-west.
pub(super) fn sky_params(
    sky: &ProceduralSky,
    lights: &[ExtractedDirectionalLight],
) -> SkyParams {
    let sun = lights
        .iter()
        .find(|light| light.light.shadows)
        .or_else(|| lights.first());
    let (direction, color) = sun.map_or(
        (nalgebra::Vector3::new(0.3, 0.8, 0.5).normalize(), [1.0; 3]),
        |sun| {
            let matrix = nalgebra::Matrix4::from(sun.transform.matrix);
            let forward = matrix.transform_vector(&-nalgebra::Vector3::z());
            let brightness =
                sun.light.illuminance / DirectionalLight::LUX_PER_UNIT;
            (
                -forward.normalize(),
                sun.light.color.map(|channel| channel * brightness),
            )
        },
    );
    let [r, g, b] = color.map(|channel| (channel * sky.intensity).max(0.0));
    let [gr, gg, gb] = sky.ground_color.map(|channel| channel.max(0.0));
    [
        [
            direction.x,
            direction.y,
            direction.z,
            (sky.sun_size.max(0.0) * 0.5).to_radians().cos(),
        ],
        [r, g, b, sky.haze.clamp(0.0, 1.0)],
        [gr, gg, gb, 1.0],
    ]
}

/// The sky baked into a mipmapped equirectangular image, top row +Y like
/// an environment map. It is baked again only when its settings change.
pub(super) struct SkyBake {
    pipeline: Arc<ComputePipeline>,
    set: Arc<DescriptorSet>,
    /// Every mip, for lighting.
    pub(super) view: Arc<ImageView>,
    pub(super) sampler: Arc<Sampler>,
    baked: Option<SkyParams>,
}

impl SkyBake {
    pub(super) fn new(
        queue: &Arc<Queue>,
        allocator: &Arc<StandardMemoryAllocator>,
        descriptor_allocator: &Arc<StandardDescriptorSetAllocator>,
    ) -> Result<Self, SceneRenderError> {
        let device = queue.device().clone();
        let pipeline =
            create_compute_pipeline(queue, bake_shader::load(device.clone()))?;
        let [width, height] = BAKE_EXTENT;
        let image = Image::new(
            allocator.clone(),
            ImageCreateInfo {
                format: Format::R16G16B16A16_SFLOAT,
                extent: [width, height, 1],
                mip_levels: width.max(height).ilog2() + 1,
                // Transfer source and destination for the mip chain.
                usage: ImageUsage::STORAGE
                    | ImageUsage::SAMPLED
                    | ImageUsage::TRANSFER_SRC
                    | ImageUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                ..Default::default()
            },
        )
        .map_err(error)?;
        name_object(&*image, "Procedural sky");
        let top = ImageView::new(
            image.clone(),
            ImageViewCreateInfo {
                subresource_range: ImageSubresourceRange {
                    mip_levels: 0..1,
                    ..image.subresource_range()
                },
                ..ImageViewCreateInfo::from_image(&image)
            },
        )
        .map_err(error)?;
        let set = DescriptorSet::new(
            descriptor_allocator.clone(),
            pipeline.layout().set_layouts()[0].clone(),
            [WriteDescriptorSet::image_view(0, top)],
            [],
        )
        .map_err(error)?;
        let sampler = Sampler::new(
            device,
            SamplerCreateInfo {
                mag_filter: Filter::Linear,
                min_filter: Filter::Linear,
                mipmap_mode: SamplerMipmapMode::Linear,
                address_mode: [
                    SamplerAddressMode::Repeat,
                    SamplerAddressMode::ClampToEdge,
                    SamplerAddressMode::ClampToEdge,
                ],
                lod: 0.0..=LOD_CLAMP_NONE,
                ..Default::default()
            },
        )
        .map_err(error)?;
        Ok(Self {
            pipeline,
            set,
            view: ImageView::new_default(image).map_err(error)?,
            sampler,
            baked: None,
        })
    }

    /// Records a bake of `params` and its mip chain, unless the image
    /// already holds them.
    pub(super) fn record(
        &mut self,
        commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        params: SkyParams,
    ) -> Result<(), SceneRenderError> {
        if self.baked == Some(params) {
            return Ok(());
        }
        self.baked = Some(params);
        let [sun, light, ground] = params;
        let [width, height] = BAKE_EXTENT;
        commands
            .bind_pipeline_compute(self.pipeline.clone())
            .map_err(error)?
            .bind_descriptor_sets(
                PipelineBindPoint::Compute,
                self.pipeline.layout().clone(),
                0,
                self.set.clone(),
            )
            .map_err(error)?
            .push_constants(
                self.pipeline.layout().clone(),
                0,
                bake_shader::Params { sun, light, ground },
            )
            .map_err(error)?;
        // Safety: the shader writes only texels inside the image.
        unsafe {
            commands
                .dispatch([width / 8, height / 8, 1])
                .map_err(error)?;
        }
        record_mip_chain(commands, self.view.image())
    }
}

/// Writes the sky seen along each texel's direction, without the sun disc.
#[rustfmt::skip]
mod bake_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        src: r#"
#version 450
layout(local_size_x = 8, local_size_y = 8, local_size_z = 1) in;
layout(set = 0, binding = 0, rgba16f) uniform writeonly image2D target;
layout(push_constant) uniform Params {
    vec4 sun;
    vec4 light;
    vec4 ground;
} params;
#include "sky.glsl"
const float PI = 3.14159265;
void main() {
    ivec2 texel = ivec2(gl_GlobalInvocationID.xy);
    // Inverse of the environment lookup: u around Y from -X, v from +Y.
    vec2 uv = (vec2(texel) + 0.5) / vec2(imageSize(target));
    float around = (uv.x - 0.5) * 2.0 * PI;
    float down = uv.y * PI;
    vec3 direction = vec3(
        sin(down) * cos(around), cos(down), sin(down) * sin(around));
    vec3 color = sky_radiance(direction, params.sun.xyz, params.light.rgb,
        params.light.w, params.ground.rgb);
    imageStore(target, texel, vec4(color, 1.0));
}
"#
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sun_is_the_shadowed_light_and_sets_the_sky_brightness() {
        let light = |shadows, direction: [f32; 3], illuminance| {
            let direction = nalgebra::Vector3::from(direction).normalize();
            ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-nalgebra::Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: DirectionalLight {
                    color: [1.0, 0.5, 0.25],
                    illuminance,
                    shadows,
                },
            }
        };
        let sky = ProceduralSky {
            intensity: 2.0,
            sun_size: 60.0,
            ..ProceduralSky::default()
        };
        // The light shines down -Y; the sun is overhead.
        let lights = [
            light(false, [1.0, 0.0, 0.0], 100_000.0),
            light(true, [0.0, -1.0, 0.0], 50_000.0),
        ];
        let [sun, color, ground] = sky_params(&sky, &lights);
        assert!((sun[1] - 1.0).abs() < 1e-5, "{sun:?}");
        assert!((sun[3] - 30.0_f32.to_radians().cos()).abs() < 1e-6);
        assert_eq!(color, [1.0, 0.5, 0.25, 0.2]);
        assert_eq!(ground[3], 1.0);
        // Without shadows the first light is the sun.
        let [sun, color, _] = sky_params(&sky, &lights[..1]);
        assert!((sun[0] + 1.0).abs() < 1e-5, "{sun:?}");
        assert_eq!(color[0], 2.0);
    }
}
