//! Forward scene renderer fed exclusively by the extracted render world.
//!
//! The renderer does not read gameplay ECS state. This boundary lets the same
//! prepared assets and draw path target a swapchain today and an offscreen
//! editor viewport in a later pass without changing scene ownership.

use std::cell::Cell;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use super::post_effects::{
    BloomChain, ExposureMeter, OcclusionParams, OcclusionTargets,
    PostPipelines, TemporalHistory,
};
use super::sky::{sky_params, SkyBake};

use nalgebra::{
    Matrix4, Orthographic3, Perspective3, Point3, Vector3, Vector4,
};
use vulkano::buffer::allocator::{
    SubbufferAllocator, SubbufferAllocatorCreateInfo,
};
use vulkano::buffer::{
    Buffer, BufferContents, BufferCreateInfo, BufferUsage, Subbuffer,
};
use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
use vulkano::command_buffer::{
    AutoCommandBufferBuilder, BlitImageInfo, BufferCopy, CommandBufferUsage,
    CopyBufferInfo, CopyBufferToImageInfo, CopyImageInfo,
    DispatchIndirectCommand, DrawIndexedIndirectCommand, ImageBlit,
    PrimaryAutoCommandBuffer, RenderPassBeginInfo, SubpassBeginInfo,
    SubpassContents,
};
use vulkano::descriptor_set::allocator::StandardDescriptorSetAllocator;
use vulkano::descriptor_set::layout::DescriptorSetLayout;
use vulkano::descriptor_set::{DescriptorSet, WriteDescriptorSet};
use vulkano::device::{Device, DeviceExtensions, DeviceOwned, Queue};
use vulkano::format::Format;
use vulkano::image::sampler::{
    BorderColor, Filter, Sampler, SamplerAddressMode, SamplerCreateInfo,
    SamplerMipmapMode,
};
use vulkano::image::view::{ImageView, ImageViewCreateInfo, ImageViewType};
use vulkano::image::{
    Image, ImageCreateInfo, ImageLayout, ImageSubresourceLayers,
    ImageSubresourceRange, ImageUsage, SampleCount, SampleCounts,
};
use vulkano::instance::debug::DebugUtilsLabel;
use vulkano::memory::allocator::{
    AllocationCreateInfo, MemoryTypeFilter, StandardMemoryAllocator,
};
use vulkano::memory::MemoryHeapFlags;
use vulkano::pipeline::compute::ComputePipelineCreateInfo;
use vulkano::pipeline::graphics::color_blend::{
    AttachmentBlend, BlendFactor, BlendOp, ColorBlendAttachmentState,
    ColorBlendState,
};
use vulkano::pipeline::graphics::depth_stencil::{
    CompareOp, DepthState, DepthStencilState,
};
use vulkano::pipeline::graphics::input_assembly::{
    InputAssemblyState, PrimitiveTopology,
};
use vulkano::pipeline::graphics::multisample::MultisampleState;
use vulkano::pipeline::graphics::rasterization::{
    CullMode, DepthBiasState, FrontFace, RasterizationState,
};
use vulkano::pipeline::graphics::subpass::PipelineSubpassType;
use vulkano::pipeline::graphics::vertex_input::{Vertex, VertexDefinition};
use vulkano::pipeline::graphics::viewport::{Scissor, Viewport, ViewportState};
use vulkano::pipeline::graphics::GraphicsPipelineCreateInfo;
use vulkano::pipeline::layout::{
    PipelineDescriptorSetLayoutCreateInfo, PipelineLayout,
};
use vulkano::pipeline::{
    ComputePipeline, DynamicState, GraphicsPipeline, Pipeline,
    PipelineBindPoint, PipelineShaderStageCreateInfo,
};
use vulkano::query::{
    QueryPool, QueryPoolCreateInfo, QueryResultFlags, QueryType,
};
use vulkano::render_pass::{
    AttachmentReference, Framebuffer, FramebufferCreateInfo, RenderPass,
    RenderPassCreateInfo, ResolveModes, Subpass,
};
use vulkano::shader::SpecializationConstant;
use vulkano::sync::future::FenceSignalFuture;
use vulkano::sync::{GpuFuture, PipelineStage};
use vulkano::DeviceSize;

use crate::assets::{
    AlphaMode, AssetServer, Handle, LodGroupAsset, LodMetric, MaterialAsset,
    MaterialModel, MeshAsset, TextureAsset, TextureColorSpace, TextureFilter,
    TextureSampler, TextureWrap, MAX_LOD_LEVELS,
};
use crate::rendering::debug_overlay::{DebugLine, RenderDebugOverlay};
use crate::rendering::frame_passes::{FramePass, FrameResource};
use crate::runtime::{
    Antialiasing, CpuFrameTimings, CullingMode, ExtractedCamera,
    GpuConditionInstruction, Projection, QualityProfile, RawGpuPhysicsEvent,
    ReflectionProbe, RenderBounds, RenderWorld, ShadowQuality, ToneMapping,
};

#[derive(Debug)]
pub struct SceneRenderError(pub(super) String);

impl Display for SceneRenderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for SceneRenderError {}

#[repr(C)]
#[derive(BufferContents, Vertex, Clone, Copy)]
struct SceneVertex {
    #[format(R32G32B32_SFLOAT)]
    position: [f32; 3],
    #[format(R32G32B32_SFLOAT)]
    normal: [f32; 3],
    #[format(R32G32_SFLOAT)]
    uv: [f32; 2],
    #[format(R32G32B32A32_SFLOAT)]
    tangent: [f32; 4],
    #[format(R32G32B32A32_SFLOAT)]
    color: [f32; 4],
}

/// One entry of the per-frame visible list: the instance a main-pass draw
/// reads, fetched per instance so each batch draws its visible instances in
/// one call.
#[repr(C)]
#[derive(BufferContents, Vertex, Clone, Copy, Debug, PartialEq, Eq)]
struct VisibleInstance {
    #[format(R32_UINT)]
    instance_index: u32,
}

/// Per-instance input of the GPU cull pass, indexed like the instance buffer.
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct CullInstance {
    /// Local bounding sphere `(center, radius)`; a negative radius is never
    /// culled.
    sphere: [f32; 4],
    /// x: draw command of LOD level 0, level `n` counts into `x + n`; y:
    /// first visible-list slot of that command's group, level `n` writes
    /// `y + n * instance count`; z: 1 for alpha-blended instances, which
    /// only the late occlusion phase draws.
    slot: [u32; 4],
    /// x: LOD value the first level starts at, 0 outside LOD groups; y: 1
    /// for the screen-size metric.
    lod: [f32; 4],
    /// Where each LOD level's range ends, finest first; `-inf` past the
    /// group's last level. Outside LOD groups, `[inf, -inf, -inf, -inf]`.
    lod_ends: [f32; 4],
}

/// `lod_ends` of an instance outside a LOD group: level 0 at any range.
const NO_LOD_ENDS: [f32; 4] = [
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::NEG_INFINITY,
    f32::NEG_INFINITY,
];

/// LOD level an instance with `lod` and `lod_ends` draws at `value`, or
/// `None` outside every level's range. Mirrors the cull shader.
fn lod_level(value: f32, lod: [f32; 4], ends: [f32; 4]) -> Option<u32> {
    if value < lod[0] {
        return None;
    }
    ends.iter()
        .position(|end| value < *end)
        .map(|level| level as u32)
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy)]
struct CullPushConstants {
    /// Clip matrix; the shader takes the frustum planes from its rows.
    clip: [[f32; 4]; 4],
    /// Scene viewport offset and extent in pixels of the depth pyramid.
    viewport: [f32; 4],
    /// xyz: camera position; w: [`lod_scale`].
    lod: [f32; 4],
    /// x: instance count; y: phase (see [`CullPhase`]); z: 1 to skip the
    /// frustum test.
    info: [u32; 4],
}

/// What one cull dispatch does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CullPhase {
    /// Frustum test only.
    Frustum = 0,
    /// Frustum-visible opaque instances that were visible last frame.
    Early = 1,
    /// Frustum and depth-pyramid test of everything; records visibility for
    /// the next frame and emits what the early phase did not draw.
    Late = 2,
}

/// Where a frame's main-pass visibility comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CullingPath {
    /// Every instance is drawn; nothing is tested.
    #[default]
    Direct,
    /// Frustum test on the CPU, uploaded as a compacted list.
    Cpu,
    /// Compute pass writes the visible list and indirect draw commands.
    Gpu,
    /// Two-phase GPU occlusion culling: last frame's visible set is drawn
    /// first, a depth pyramid is built from it, and a second cull pass
    /// draws what the pyramid does not hide.
    GpuOcclusion,
}

impl CullingPath {
    /// Whether the cull pass runs on the GPU.
    pub fn on_gpu(self) -> bool {
        matches!(self, CullingPath::Gpu | CullingPath::GpuOcclusion)
    }
}

/// Culling numbers for the profiler, from the last frame whose results are
/// known. The GPU path reads its counts and time back once that frame's
/// submission completes, so they arrive a frame or two late.
#[derive(
    bevy_ecs::prelude::Resource, Clone, Copy, Debug, Default, PartialEq,
)]
pub struct CullingStats {
    pub path: CullingPath,
    /// Instances the main pass considered. Each level of a LOD group is
    /// its own instance.
    pub submitted: usize,
    /// Instances the main pass drew.
    pub visible: usize,
    /// Instances frustum culling, occlusion culling, and LOD selection
    /// skipped.
    pub culled: usize,
    /// Culling cost: CPU wall time on the `Direct` and `Cpu` paths, the
    /// cull dispatches' GPU time on the GPU paths, including the depth
    /// pyramid on `GpuOcclusion`. `None` when the queue has no timestamp
    /// support.
    pub time: Option<Duration>,
}

/// Work counters for the profiler; see [`SceneRenderer::render_counters`].
#[derive(
    bevy_ecs::prelude::Resource, Clone, Copy, Debug, Default, PartialEq, Eq,
)]
pub struct RenderCounters {
    /// Draw commands recorded, including indirect draws the GPU cull pass
    /// may set to zero instances.
    pub draws: u32,
    /// Compute dispatches recorded.
    pub dispatches: u32,
    /// Mesh triangles drawn in the shadow and scene passes. Indirect draws
    /// add their counts once read back, a frame or two late.
    pub triangles: u64,
    /// Instances the main pass drew; see [`CullingStats::visible`].
    pub visible_instances: usize,
    /// Bytes the CPU wrote into GPU buffers for the frame, including new
    /// meshes, textures, and instance lists.
    pub upload_bytes: u64,
    /// GPU physics commands uploaded for the frame, and their bytes.
    pub physics_commands: u32,
    pub physics_command_bytes: u64,
    /// Physics compute dispatches recorded for the frame: per fixed step,
    /// the two contact passes, the built-in step and each custom shader.
    pub physics_dispatches: u32,
    /// GPU physics events (with their header) and body states read back
    /// since the previous frame, in bytes.
    pub physics_event_bytes: u64,
    pub physics_state_bytes: u64,
    /// Most frames rendered between submitting and collecting a physics
    /// readback collected since the previous frame; 0 when each was
    /// collected before the next frame, or none completed.
    pub physics_readback_latency_frames: u32,
    /// Device memory in the renderer's allocator pools, all memory types.
    // ponytail: vulkano does not track dedicated allocations (large
    // images) in its pools; count them at creation if they matter.
    pub gpu_memory_bytes: u64,
}

/// GPU time of each pass of the last frame whose results are known, in
/// recording order. Read back once that frame completes, so a frame or two
/// late. Empty when the queue cannot write timestamps.
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default, PartialEq)]
pub struct GpuPassTimes(pub Vec<(FramePass, Duration)>);

impl GpuPassTimes {
    /// Sum of every timed pass.
    pub fn total(&self) -> Duration {
        self.0.iter().map(|(_, time)| *time).sum()
    }
}

/// A frame whose pass timestamps are read once it completes.
struct PendingPassTimes {
    fence: FrameFence,
    pool: Arc<QueryPool>,
    passes: Vec<FramePass>,
}

/// A GPU-culled frame whose counts and time are read once it completes.
struct PendingCullReadback {
    fence: FrameFence,
    path: CullingPath,
    submitted: usize,
    /// Every draw-command set the frame drew with; their counts add up.
    commands: Vec<Subbuffer<[DrawIndexedIndirectCommand]>>,
    /// The frame's pass timestamps; the cull passes give its time.
    timestamps: Option<Arc<QueryPool>>,
}

/// Instances from which GPU culling beats the CPU loop and its upload.
// ponytail: untimed guess; tune from `CullingStats` times measured on real
// integrated and discrete GPUs.
pub const GPU_CULL_MIN_INSTANCES: usize = 4096;

/// Below this many instances, `Auto` draws everything: the few off-view
/// draws cost less than a cull dispatch or a visible-list upload.
// ponytail: untimed guess like `GPU_CULL_MIN_INSTANCES`; tune from the
// `CullingStats` break-even on real hardware.
pub const AUTO_DIRECT_MAX_INSTANCES: usize = 64;

/// Picks the culling path for a frame. `Auto` skips culling for scenes
/// under [`AUTO_DIRECT_MAX_INSTANCES`], even with GPU bodies, since direct
/// drawing is correct for any owner. LOD levels need a path that picks one
/// per object, so `has_lods` rules out `Direct`; `Disabled` then culls by
/// LOD only. GPU-owned bodies force the GPU path,
/// because only the GPU has their newest transforms. Otherwise large scenes
/// go to the GPU; integrated GPUs and `Eco`, where compute time is scarce,
/// need four times as many instances.
pub fn select_culling_path(
    mode: CullingMode,
    instances: usize,
    gpu_owned: usize,
    has_lods: bool,
    quality: QualityProfile,
    capabilities: &RendererCapabilities,
) -> CullingPath {
    let threshold =
        if capabilities.integrated_gpu || quality == QualityProfile::Eco {
            GPU_CULL_MIN_INSTANCES * 4
        } else {
            GPU_CULL_MIN_INSTANCES
        };
    match mode {
        CullingMode::Disabled if !has_lods => CullingPath::Direct,
        CullingMode::Auto
            if instances < AUTO_DIRECT_MAX_INSTANCES && !has_lods =>
        {
            CullingPath::Direct
        }
        CullingMode::FrustumAndOcclusion => CullingPath::GpuOcclusion,
        _ if gpu_owned > 0 || instances >= threshold => CullingPath::Gpu,
        _ => CullingPath::Cpu,
    }
}

/// Local bounding sphere `[center, radius]` of `bounds`.
fn bounding_sphere(bounds: RenderBounds) -> [f32; 4] {
    match bounds {
        RenderBounds::Sphere { center, radius } => {
            [center[0], center[1], center[2], radius]
        }
        RenderBounds::Aabb { min, max } => {
            let half = Vector3::from(max) - Vector3::from(min);
            let center = (Vector3::from(max) + Vector3::from(min)) / 2.0;
            [center.x, center.y, center.z, half.norm() / 2.0]
        }
    }
}

/// `bounding_sphere` moved by `model` the way the cull shader does it: the
/// radius grows with the largest axis scale. A negative radius stays
/// negative.
fn world_sphere(model: &[[f32; 4]; 4], sphere: [f32; 4]) -> [f32; 4] {
    let model = matrix_from_array(*model);
    let center =
        model.transform_point(&Point3::new(sphere[0], sphere[1], sphere[2]));
    let scale = (0..3)
        .map(|axis| model.fixed_view::<3, 1>(0, axis).norm())
        .fold(0.0_f32, f32::max);
    [center.x, center.y, center.z, sphere[3] * scale]
}

/// Projection term of the LOD value: `tan(fov / 2)` for a perspective
/// camera, minus half the view height for an orthographic one.
fn lod_scale(camera: Option<ExtractedCamera>) -> f32 {
    match camera.map(|camera| camera.projection) {
        Some(Projection::Perspective {
            vertical_fov_radians,
            ..
        }) => (vertical_fov_radians / 2.0).tan(),
        Some(Projection::Orthographic { vertical_size, .. }) => {
            -vertical_size / 2.0
        }
        None => (std::f32::consts::FRAC_PI_3 / 2.0).tan(),
    }
}

/// LOD value of a world sphere, growing with distance like
/// [`LodGroupAsset::ranges`]: the distance to `camera.xyz`, or the inverse
/// of the view-height fraction the sphere covers. `camera.w` is
/// [`lod_scale`]. A sphere without a radius counts as close. Mirrors
/// `lod_value` in the cull shader.
fn lod_value(camera: [f32; 4], sphere: [f32; 4], screen_size: bool) -> f32 {
    let distance = (Vector3::new(sphere[0], sphere[1], sphere[2])
        - Vector3::new(camera[0], camera[1], camera[2]))
    .norm();
    if !screen_size {
        distance
    } else if sphere[3] <= 0.0 {
        0.0
    } else if camera[3] > 0.0 {
        distance * camera[3] / sphere[3]
    } else {
        -camera[3] / sphere[3]
    }
}

/// One particle, read per instance by the particle pipelines.
#[repr(C)]
#[derive(BufferContents, Vertex, Clone, Copy, Debug, PartialEq)]
pub(super) struct ParticleVertex {
    #[format(R32G32B32_SFLOAT)]
    center: [f32; 3],
    #[format(R32_SFLOAT)]
    size: f32,
    #[format(R32G32B32A32_SFLOAT)]
    color: [f32; 4],
    #[format(R32G32B32_SFLOAT)]
    stretch: [f32; 3],
    #[format(R32_SFLOAT)]
    rotation: f32,
}

/// Vertex used by the editor-only debug pass.
#[repr(C)]
#[derive(BufferContents, Vertex, Clone, Copy)]
struct DebugVertex {
    #[format(R32G32B32_SFLOAT)]
    start: [f32; 3],
    #[format(R32G32B32_SFLOAT)]
    end: [f32; 3],
    #[format(R32G32B32A32_SFLOAT)]
    color: [f32; 4],
    #[format(R32G32_SFLOAT)]
    corner: [f32; 2],
    #[format(R32_SFLOAT)]
    thickness: f32,
}

/// Per-object data read by the graphics shaders: the main pass through the
/// visible list, the shadow pass with `gl_InstanceIndex`.
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct RenderInstanceUpload {
    model: [[f32; 4]; 4],
    /// Inverse-transpose of the model's 3x3 part, as three padded columns.
    /// GPU bodies ignore it and derive theirs from the live model.
    normal: [[f32; 4]; 3],
    color: [f32; 4],
    /// rgb: emissive factor; w: 1 for `MaterialModel::Unlit`, else 0.
    emissive: [f32; 4],
    /// x: metallic; y: roughness; z: 1 when a normal map is bound; w: 1
    /// for flat shading.
    surface: [f32; 4],
    /// x: GPU physics index or `u32::MAX`; y: alpha mode (0 opaque, 1 mask,
    /// 2 blend); z: mask cutoff as `f32` bits; w: bit 0 casts shadows, bit 1
    /// receives shadows, bits 8 and up hold the lightmap slot plus one.
    physics: [u32; 4],
    /// x: transmission; y: index of refraction; z: thickness; w: 1 for a
    /// terrain splat material.
    transmission: [f32; 4],
    /// xy: texture repeats per face; zw: texture offset.
    uv_transform: [f32; 4],
}

/// Rougher surfaces keep the environment reflection only; the shader fades
/// screen-space reflections out toward it.
const SSR_MAX_ROUGHNESS: f32 = 0.5;

/// Most reflection probes a frame samples; later ones are ignored.
const MAX_REFLECTION_PROBES: usize = 4;
/// Fog volumes the main pass reads; `fog.glsl` declares the same count.
const MAX_FOG_VOLUMES: usize = 8;
/// Decals the main pass paints; the fragment shader declares the same
/// count.
const MAX_DECALS: usize = 16;
/// Distinct decal images per frame, in set 2 binding 9.
const MAX_DECAL_IMAGES: usize = 4;
/// Side length in texels of each captured probe face.
const PROBE_FACE_SIZE: u32 = 128;
/// Most light probes a grid bakes; the fragment shader indexes up to this.
const MAX_LIGHT_PROBES: usize = 64;
/// Side length in texels of each face a light probe bakes from.
const LIGHT_PROBE_FACE_SIZE: u32 = 16;
/// Rows of the light probe buffer before the probes' ambient cubes: grid
/// center and probe count, half size and intensity, resolution.
const LIGHT_PROBE_HEADER: usize = 3;
/// Light probes the High profile recaptures each frame, round robin, so
/// moving lights and objects relight the grid within a few frames.
// ponytail: each recapture blocks on its readback; move the reduction to a
// compute pass if the stall shows up in frame pacing.
const LIGHT_PROBES_PER_FRAME: usize = 2;
/// View direction and up vector of each probe face, in layer order. The
/// fragment shader's `PROBE_FORWARD` and `PROBE_UP` match.
const PROBE_FACES: [([f32; 3], [f32; 3]); 6] = [
    ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ([-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, -1.0, 0.0], [0.0, 0.0, 1.0]),
    ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
    ([0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
];

/// Shadow cascades and the rest of the main pass's per-frame state.
#[repr(C)]
#[derive(BufferContents, Clone, Copy)]
struct ShadowUpload {
    /// Environment map: x is 1 when bound, y its intensity, z its last mip
    /// level.
    environment: [f32; 4],
    /// Opaque scene color copy: x is 1 when bound, y its last mip level, z
    /// is 1 when the depth pyramid holds the opaque depth for screen-space
    /// reflections.
    scene_color: [f32; 4],
    /// Scene viewport in target pixels: offset in xy, size in zw.
    viewport: [f32; 4],
    /// Per reflection probe, two rows: box center and intensity, then half
    /// size and last mip level. `environment.w` counts the probes.
    probes: [[f32; 4]; 2 * MAX_REFLECTION_PROBES],
    /// Inverse of the camera's view projection, for fog rays.
    inverse_view_projection: [[f32; 4]; 4],
    /// Fog color in rgb and density in w; density 0 turns fog off.
    fog: [f32; 4],
    /// Fog height, height falloff, sun scatter and sky affect.
    fog_shape: [f32; 4],
    /// x is 1 when binding 6 holds this frame's ambient occlusion; y is 1
    /// for single-tap hard shadows; z is 1 when binding 10 holds indirect
    /// light.
    ambient_occlusion: [f32; 4],
    /// Light-space transform of each cascade of the shadowed light, or of
    /// each cube face of a point light.
    cascades: [[[f32; 4]; 4]; SHADOW_VIEWS],
    /// Camera depth where each cascade ends.
    cascade_far: [f32; 4],
    /// Camera forward in xyz and `dot(eye, forward)` in w, so
    /// `dot(p, xyz) - w` is a point's camera depth. For a point light, its
    /// position in xyz.
    cascade_forward: [f32; 4],
    /// x is the cascade count, y the atlas tiles per side, z is 1 when the
    /// views are a point light's cube faces.
    cascade_info: [f32; 4],
    /// Camera forward in xyz and `dot(eye, forward)` in w, for the light
    /// cluster depth.
    cluster_view: [f32; 4],
    /// Cluster near plane in x and `ln(far / near)` in y.
    cluster_depth: [f32; 4],
    /// x is how much shadows block the fog's glow toward the sun; y counts
    /// the fog volumes.
    fog_shafts: [f32; 4],
    /// World-to-box matrix of each fog volume; the box spans -1..1.
    fog_volumes: [[[f32; 4]; 4]; MAX_FOG_VOLUMES],
    /// Color in rgb and density in w of each fog volume.
    fog_volume_colors: [[f32; 4]; MAX_FOG_VOLUMES],
    /// The procedural sky, as `sky::sky_params` lays it out; all zero
    /// without one. `sky[2].w` is 2 when the environment map is drawn as
    /// the background instead.
    sky: [[f32; 4]; 3],
    /// x counts the decals.
    decal_info: [f32; 4],
    /// World-to-box matrix of each decal; the box spans -1..1.
    decals: [[[f32; 4]; 4]; MAX_DECALS],
    /// Each decal's projection axis (the box's +Z in world space) and its
    /// image slot in w, -1 for none.
    decal_axes: [[f32; 4]; MAX_DECALS],
    /// Linear RGBA tint of each decal.
    decal_colors: [[f32; 4]; MAX_DECALS],
    /// Each decal's roughness in x and 1 in y when it paints one.
    decal_surfaces: [[f32; 4]; MAX_DECALS],
}

/// Side length in texels of the directional shadow map and the view
/// distance in front of the camera it covers, for a resolved profile.
/// Higher profiles cover more distance at a finer texel size; every Vulkan
/// device supports 4096-texel 2D images.
/// Shadow map size and reach for a shadow setting, where `Auto` follows
/// the resolved quality profile.
pub fn shadow_settings_for(
    shadows: ShadowQuality,
    profile: QualityProfile,
) -> (u32, f32) {
    shadow_settings(match shadows {
        ShadowQuality::Auto => profile,
        ShadowQuality::Low => QualityProfile::Eco,
        ShadowQuality::Medium => QualityProfile::Balanced,
        ShadowQuality::High => QualityProfile::High,
    })
}

pub fn shadow_settings(profile: QualityProfile) -> (u32, f32) {
    match profile {
        QualityProfile::Eco => (1024, 30.0),
        QualityProfile::Balanced => (2048, 50.0),
        QualityProfile::High | QualityProfile::Auto => (4096, 80.0),
    }
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy)]
struct CameraUniform {
    view_projection: [[f32; 4]; 4],
    /// w = 1: xyz is the camera position (perspective). w = 0: xyz is the
    /// direction toward an orthographic camera.
    eye: [f32; 4],
    /// Ambient seen by surfaces facing +Y (uniform ambient plus sky).
    ambient: [f32; 4],
    /// Ambient seen by surfaces facing -Y (uniform ambient plus ground).
    ground_ambient: [f32; 4],
    light_info: [u32; 4],
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct LightUpload {
    position_kind: [f32; 4],
    direction_range: [f32; 4],
    color_intensity: [f32; 4],
    /// Cosines of the inner and outer cone, source radius, and tangent of
    /// the outer cone.
    spot_angles: [f32; 4],
    /// The spot light's right axis and its cookie slot, -1 without one.
    cookie: [f32; 4],
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy)]
struct DebugPushConstants {
    view_projection: [[f32; 4]; 4],
    viewport_size: [f32; 2],
    _padding: [f32; 2],
}

fn debug_line_vertices(line: &DebugLine) -> [DebugVertex; 6] {
    let vertex = |corner| DebugVertex {
        start: line.start,
        end: line.end,
        color: line.color,
        corner,
        thickness: line.thickness,
    };
    [
        vertex([0.0, -1.0]),
        vertex([1.0, -1.0]),
        vertex([1.0, 1.0]),
        vertex([0.0, -1.0]),
        vertex([1.0, 1.0]),
        vertex([0.0, 1.0]),
    ]
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, PartialEq)]
struct GpuBodyState {
    model: [[f32; 4]; 4],
    velocity: [f32; 4],
    angular_velocity: [f32; 4],
    properties: [f32; 4],
    custom_values: [f32; 4],
    metadata: [u32; 4],
}

const _: () = assert!(
    std::mem::size_of::<GpuBodyState>() as u64
        == crate::runtime::PhysicsSyncMode::STATE_READBACK_BYTES
);

#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuConditionUpload {
    words: [u32; 4],
    values: [f32; 4],
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuRuleState {
    config: [u32; 4],
    timing: [f32; 4],
    state: [u32; 4],
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default)]
struct GpuEventHeader {
    count: u32,
    overflow: u32,
    reserved: [u32; 2],
    /// Contact-grid counters; see `EventHeader` in `physics_abi.glsl`.
    contacts: [u32; 4],
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default)]
struct GpuEventUpload {
    header: [u32; 4],
    timing: [u32; 4],
    payload: [f32; 4],
}

#[repr(C)]
#[derive(BufferContents, Clone, Copy)]
struct PhysicsPushConstants {
    dt: f32,
    elapsed: f32,
    body_count: u32,
    event_capacity: u32,
    tick_low: u32,
    tick_high: u32,
    gravity_x: f32,
    gravity_y: f32,
    gravity_z: f32,
    /// This step's commands: `command_count` entries from `command_first`.
    command_count: u32,
    collider_count: u32,
    grid_cell_size: f32,
    command_first: u32,
    field_count: u32,
    lock_mask: u32,
    /// [`crate::runtime::GpuSimulationLod`]; 0 observers means no LOD.
    observer_count: u32,
    lod_full_squared: f32,
    lod_reduced_squared: f32,
    lod_stride: u32,
}

/// A GPU body's own collider (binding 6), in [`crate::runtime::GpuCollider`]
/// shape encoding; kind 3 means no collider.
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuBodyShape {
    shape: [f32; 4],
    /// x = friction, y = restitution.
    material: [f32; 4],
    /// x = memberships, y = filters.
    layers: [u32; 4],
}

/// One [`crate::runtime::GpuCollider`] (binding 7), uploaded every frame.
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuColliderUpload {
    model: [[f32; 4]; 4],
    shape: [f32; 4],
    velocity: [f32; 4],
    material: [f32; 4],
    layers: [u32; 4],
}

impl From<&crate::runtime::GpuCollider> for GpuColliderUpload {
    fn from(collider: &crate::runtime::GpuCollider) -> Self {
        let [x, y, z] = collider.velocity;
        Self {
            model: collider.model,
            shape: collider.shape,
            velocity: [x, y, z, 0.0],
            material: [collider.friction, collider.restitution, 0.0, 0.0],
            layers: [
                collider.layers.memberships,
                collider.layers.filters,
                0,
                0,
            ],
        }
    }
}

/// One [`crate::runtime::GpuForceField`] (binding 14), uploaded every frame.
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuForceFieldUpload {
    model: [[f32; 4]; 4],
    shape: [f32; 4],
    /// xyz = direction, w = strength.
    direction: [f32; 4],
    /// x = `FieldKind` index, y = falloff distance, z = turbulence.
    params: [f32; 4],
}

impl From<&crate::runtime::GpuForceField> for GpuForceFieldUpload {
    fn from(found: &crate::runtime::GpuForceField) -> Self {
        let field = &found.field;
        let [x, y, z] = field.direction;
        Self {
            model: found.model,
            shape: found.shape,
            direction: [x, y, z, field.strength],
            params: [
                field.kind as u8 as f32,
                field.falloff_distance,
                field.turbulence,
                0.0,
            ],
        }
    }
}

/// Stackless bounding-volume tree over the collider upload order. `links.x`
/// is the first node after this subtree; `links.y` is the collider index for
/// a leaf, or `u32::MAX` for an internal node; `links.z` is the first collider
/// index after the subtree.
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuColliderNode {
    lower: [f32; 4],
    upper: [f32; 4],
    links: [u32; 4],
}

fn collider_tree(colliders: &[GpuColliderUpload]) -> Vec<GpuColliderNode> {
    fn build(
        colliders: &[GpuColliderUpload],
        nodes: &mut Vec<GpuColliderNode>,
        start: usize,
        end: usize,
    ) -> usize {
        let index = nodes.len();
        nodes.push(GpuColliderNode::default());
        let (lower, upper, collider) = if end - start == 1 {
            let source = &colliders[start];
            let radius = if source.shape[0] < 0.5 {
                source.shape[1..4].iter().map(|v| v * v).sum::<f32>().sqrt()
            } else if source.shape[0] < 1.5 {
                source.shape[1]
            } else {
                source.shape[1] + source.shape[2]
            };
            let center = &source.model[3];
            (
                [
                    center[0] - radius,
                    center[1] - radius,
                    center[2] - radius,
                    0.0,
                ],
                [
                    center[0] + radius,
                    center[1] + radius,
                    center[2] + radius,
                    0.0,
                ],
                start as u32,
            )
        } else {
            let middle = start + (end - start) / 2;
            let left = build(colliders, nodes, start, middle);
            let right = build(colliders, nodes, middle, end);
            let a = nodes[left];
            let b = nodes[right];
            (
                [
                    a.lower[0].min(b.lower[0]),
                    a.lower[1].min(b.lower[1]),
                    a.lower[2].min(b.lower[2]),
                    0.0,
                ],
                [
                    a.upper[0].max(b.upper[0]),
                    a.upper[1].max(b.upper[1]),
                    a.upper[2].max(b.upper[2]),
                    0.0,
                ],
                u32::MAX,
            )
        };
        nodes[index] = GpuColliderNode {
            lower,
            upper,
            links: [nodes.len() as u32, collider, end as u32, 0],
        };
        index
    }

    let mut nodes = Vec::with_capacity(colliders.len().saturating_mul(2));
    if !colliders.is_empty() {
        build(colliders, &mut nodes, 0, colliders.len());
    }
    nodes
}

/// One [`GpuBodyCommand`] for the physics shader. `header` is the body's
/// state index, the command kind, and the expected generation.
///
/// [`GpuBodyCommand`]: crate::runtime::GpuBodyCommand
#[repr(C)]
#[derive(BufferContents, Clone, Copy, Debug, Default, PartialEq)]
struct GpuCommandUpload {
    header: [u32; 4],
    values: [[f32; 4]; 4],
}

impl GpuCommandUpload {
    fn new(
        index: u32,
        generation: u32,
        command: &crate::runtime::GpuBodyCommand,
    ) -> Self {
        use crate::runtime::GpuBodyCommand;
        let vector = |value: [f32; 3]| [value[0], value[1], value[2], 0.0];
        let (kind, values) = match *command {
            GpuBodyCommand::Teleport(transform) => {
                (0, crate::runtime::sim_math::transform_matrix(&transform))
            }
            GpuBodyCommand::SetVelocity { linear, angular } => {
                (1, [vector(linear), vector(angular), [0.0; 4], [0.0; 4]])
            }
            GpuBodyCommand::Impulse(impulse) => {
                (2, [vector(impulse), [0.0; 4], [0.0; 4], [0.0; 4]])
            }
            GpuBodyCommand::Force(force) => {
                (3, [vector(force), [0.0; 4], [0.0; 4], [0.0; 4]])
            }
            GpuBodyCommand::SetCustomValues(custom) => {
                (4, [custom, [0.0; 4], [0.0; 4], [0.0; 4]])
            }
            // Handled on the CPU as a readback copy; never uploaded.
            GpuBodyCommand::ReadState => (5, [[0.0; 4]; 4]),
        };
        Self {
            header: [index, kind, generation, 0],
            values,
        }
    }
}

/// A base-color texture uploaded for the ECS renderer, with the set-1
/// descriptor set that binds it and its sampler.
struct PreparedTexture {
    handle: Handle<TextureAsset>,
    /// `None` when the texture data was malformed and failed to upload.
    view: Option<Arc<ImageView>>,
    sampler: Arc<Sampler>,
    source_revision: u64,
}

/// A camera image for one screen material; see
/// [`SceneRenderer::render_screens`].
struct ScreenFeed {
    renderer: Box<SceneRenderer>,
    view: Arc<ImageView>,
    sampler: Arc<Sampler>,
    size: [u32; 2],
}

/// Set 1 of one material: base color, normal, metallic-roughness, occlusion,
/// and emissive maps, white where the material has none.
struct PreparedMaterial {
    handle: Handle<MaterialAsset>,
    set: Arc<DescriptorSet>,
    /// Views the set was written with; a re-uploaded texture has a new view.
    views: [usize; MATERIAL_TEXTURES],
    /// The material's surface shader; empty for the built-in one.
    shader: String,
}

const MATERIAL_TEXTURES: usize = 5;

/// Pixels waiting to be copied into their image by the next recorded frame.
type PendingTextureUpload = (Subbuffer<[u8]>, Arc<Image>);

struct PreparedMesh {
    handle: Handle<MeshAsset>,
    vertices: Subbuffer<[SceneVertex]>,
    indices: Subbuffer<[u32]>,
    source_revision: u64,
    /// Local box around the uploaded vertices, which may be the fallback
    /// mesh's while the real one loads.
    bounds: Option<RenderBounds>,
}

/// Consecutive instances that share one mesh and material.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreparedRenderBatch {
    /// The mesh, or the finest level of its LOD group.
    mesh_key: u64,
    material: Handle<MaterialAsset>,
    first_instance: u32,
    instance_count: u32,
    /// Draw of LOD level 0 in `PreparedRenderInstances::draw_meshes`; each
    /// further level is the next draw.
    first_draw: u32,
    /// LOD levels the batch draws, 1 outside LOD groups.
    levels: u32,
}

/// One alpha-blended instance, drawn alone so it can be sorted per frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BlendedInstance {
    instance: u32,
    mesh_key: u64,
    material: Handle<MaterialAsset>,
    position: [f32; 3],
}

/// Cached instance data rebuilt only when extracted render data changes.
struct PreparedRenderInstances {
    renderables_revision: u64,
    /// `renderables_layout_revision` the instances were packed for; while
    /// it holds, a move only patches transforms. 0 never patches.
    layout_revision: u64,
    /// CPU copy of `instances`, patched when objects move.
    packed: Vec<RenderInstanceUpload>,
    /// Index into `RenderWorld::renderables` for each instance.
    sources: Vec<u32>,
    /// Mesh-space bounds for each instance that `bounds` culls.
    local_bounds: Vec<Option<RenderBounds>>,
    flashes: Vec<(bevy_ecs::entity::Entity, [f32; 4])>,
    material_overrides:
        Vec<(bevy_ecs::entity::Entity, crate::runtime::MaterialOverride)>,
    /// `RenderWorld::lightmaps` the slots in `physics.w` were packed for.
    lightmaps: Vec<(bevy_ecs::entity::Entity, crate::runtime::Lightmap)>,
    physics_revision: u64,
    material_revisions: Vec<(Handle<MaterialAsset>, u64)>,
    instances: Subbuffer<[RenderInstanceUpload]>,
    /// Opaque and masked batches.
    batches: Vec<PreparedRenderBatch>,
    /// Mesh of each batch draw: one per LOD level of a batch, from its
    /// `first_draw`.
    draw_meshes: Vec<u64>,
    /// Blended instances, stored after every batched instance.
    blended: Vec<BlendedInstance>,
    /// Whether any blended instance transmits light, so the frame copies the
    /// opaque scene color for it to sample.
    refractive: bool,
    /// Batches smooth enough for screen-space reflections, drawn again over
    /// the opaque copy.
    glossy_batches: Vec<usize>,
    /// Uploaded mesh revisions the bounds below were computed from.
    mesh_revisions: Vec<(u64, u64)>,
    /// World bounds by instance index. `None` never culls: GPU-physics
    /// bodies, whose CPU transform is stale, and objects without a mesh box.
    bounds: Vec<Option<RenderBounds>>,
    /// Main-pass visible list for the last camera; reset on every rebuild.
    visibility: Option<PreparedVisibility>,
    /// Cull-pass input by instance index.
    cull_source: Vec<CullInstance>,
    /// `cull_source` uploaded on the first GPU-culled frame.
    cull: Option<Subbuffer<[CullInstance]>>,
    /// Per-instance visibility the late occlusion phase writes and the next
    /// frame's early phase reads. Zeroed with `cull`, so the first frame
    /// after a rebuild draws everything in the late phase.
    occlusion: Option<Subbuffer<[u32]>>,
    /// Instances drawn from GPU physics state.
    gpu_owned: usize,
    /// World sphere by instance index for instances in a LOD group, whose
    /// range sits in their `cull_source` entry.
    lod_spheres: Vec<Option<[f32; 4]>>,
    /// `lod_signature` the instances were expanded with.
    lod_signature: Vec<(u64, u64)>,
    /// Keys of the screen materials, each batched alone for its feed.
    screens: Vec<u64>,
}

impl PreparedRenderInstances {
    /// Patches the transforms of the instances whose object moved, with
    /// the same layout, and drops the visible list. Returns the bytes to
    /// upload. Batches, order and materials stay as they were.
    // ponytail: compares every instance's matrix; keep a moved list from
    // extraction if 100k+ mostly static objects make this scan show up.
    fn move_instances(&mut self, render_world: &RenderWorld) -> DeviceSize {
        for (index, &source) in self.sources.iter().enumerate() {
            let matrix =
                render_world.renderables[source as usize].transform.matrix;
            let instance = &mut self.packed[index];
            if instance.model == matrix {
                continue;
            }
            instance.model = matrix;
            instance.normal = normal_columns(Matrix4::from(matrix));
            if let Some(local) = self.local_bounds[index] {
                self.bounds[index] = Some(local.transformed(&matrix));
            }
            if self.lod_spheres[index].is_some() {
                self.lod_spheres[index] =
                    Some(world_sphere(&matrix, self.cull_source[index].sphere));
            }
        }
        for blended in &mut self.blended {
            blended.position =
                light_position(self.packed[blended.instance as usize].model);
        }
        self.renderables_revision = render_world.renderables_revision;
        self.visibility = None;
        std::mem::size_of_val(self.packed.as_slice()) as DeviceSize
    }
}

/// Instances the main pass draws, compacted per batch.
struct PreparedVisibility {
    /// Clip matrix the list was picked for and whether the frustum test
    /// ran, or `None` when the camera did not matter.
    clip: Option<([[f32; 4]; 4], bool)>,
    list: Subbuffer<[VisibleInstance]>,
    /// `(first, count)` in `list` for each batch, in batch order.
    batches: Vec<(u32, u32)>,
    /// Visible blended instances whose `instance` is their slot in `list`.
    blended: Vec<BlendedInstance>,
    culled: usize,
}

#[derive(Clone)]
struct PreparedGpuPhysics {
    source_revision: u64,
    source: Vec<crate::runtime::ExtractedGpuPhysicsBody>,
    body_indices: HashMap<bevy_ecs::entity::Entity, u32>,
    states: Subbuffer<[GpuBodyState]>,
    instructions: Subbuffer<[GpuConditionUpload]>,
    rules: Subbuffer<[GpuRuleState]>,
    shapes: Subbuffer<[GpuBodyShape]>,
    /// Live state of surviving bodies, copied from the previous states buffer
    /// before the next dispatch so an edit does not reset their motion.
    carry: Option<(Subbuffer<[GpuBodyState]>, Vec<BufferCopy>)>,
    /// Copies of each state-synchronized body into one packed readback
    /// slice, and whether each one returns its custom values too.
    readback: Vec<BufferCopy>,
    readback_full: Arc<[bool]>,
    /// Push constant `grid_cell_size`; see [`contact_grid_cell_size`].
    grid_cell_size: f32,
}

/// Device buffers of the GPU body-contact passes
/// (`physics_contacts.comp`), sized for `capacity` bodies. They grow by
/// doubling and are shared by every frame, like the states buffer.
struct PhysicsContactGrid {
    capacity: usize,
    counts: Subbuffer<[u32]>,
    slots: Subbuffer<[u32]>,
    fallback: Subbuffer<[u32]>,
    snapshot: Subbuffer<[GpuBodyState]>,
    /// Count and indices of the bodies too big for one cell.
    oversized: Subbuffer<[u32]>,
    /// One workgroup per oversized body, written by the contact pass.
    oversized_dispatch: Subbuffer<[DispatchIndirectCommand]>,
}

impl PhysicsContactGrid {
    /// `CELL_SLOTS` in `physics_contacts.comp`.
    const CELL_SLOTS: u64 = 16;

    fn new(
        allocator: &Arc<StandardMemoryAllocator>,
        bodies: usize,
        hash_budget: DeviceSize,
    ) -> Result<Self, SceneRenderError> {
        let capacity = bodies.max(1).next_power_of_two();
        let cells = Self::hash_cells(capacity, hash_budget);
        let info = || BufferCreateInfo {
            usage: BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_DST,
            ..Default::default()
        };
        let device = || AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        };
        let error =
            |error: vulkano::Validated<_>| SceneRenderError(error.to_string());
        Ok(Self {
            capacity,
            counts: Buffer::new_slice(
                allocator.clone(),
                info(),
                device(),
                cells,
            )
            .map_err(error)?,
            slots: Buffer::new_slice(
                allocator.clone(),
                info(),
                device(),
                cells * Self::CELL_SLOTS,
            )
            .map_err(error)?,
            fallback: Buffer::new_slice(
                allocator.clone(),
                info(),
                device(),
                capacity as u64 + 1,
            )
            .map_err(error)?,
            snapshot: Buffer::new_slice(
                allocator.clone(),
                info(),
                device(),
                capacity as u64,
            )
            .map_err(error)?,
            oversized: Buffer::new_slice(
                allocator.clone(),
                info(),
                device(),
                capacity as u64 + 1,
            )
            .map_err(error)?,
            oversized_dispatch: Buffer::new_slice(
                allocator.clone(),
                BufferCreateInfo {
                    usage: BufferUsage::STORAGE_BUFFER
                        | BufferUsage::INDIRECT_BUFFER,
                    ..Default::default()
                },
                device(),
                1,
            )
            .map_err(error)?,
        })
    }
}

impl PhysicsContactGrid {
    /// Hash cells for `capacity` bodies: twice as many cells as bodies keeps
    /// most cells short, but the counts and slots never take more than
    /// `budget` bytes. Bodies that do not fit a smaller hash spill into the
    /// fallback list, which always holds every body, so a tight budget costs
    /// time, never contacts. The shader needs a power of two.
    fn hash_cells(capacity: usize, budget: DeviceSize) -> DeviceSize {
        let cell_bytes = 4 * (1 + Self::CELL_SLOTS);
        let affordable = (budget / cell_bytes).max(1);
        let affordable = 1 << affordable.ilog2();
        (capacity as DeviceSize * 2).min(affordable)
    }
}

/// Shape word `x` below 2.5 is a solid shape; mirrors `bounding_radius` in
/// `physics_shapes.glsl`.
fn shape_bounding_radius(shape: [f32; 4]) -> Option<f32> {
    match shape[0] {
        kind if kind < 0.5 => Some(
            (shape[1] * shape[1] + shape[2] * shape[2] + shape[3] * shape[3])
                .sqrt(),
        ),
        kind if kind < 1.5 => Some(shape[1]),
        kind if kind < 2.5 => Some(shape[1] + shape[2]),
        _ => None,
    }
}

/// Cell edge of the GPU contact grid: the diameter of the largest body that
/// is at most four times the median radius. Bigger bodies go to the
/// fallback list, so one huge body does not make every cell huge.
fn contact_grid_cell_size(radii: impl Iterator<Item = f32>) -> f32 {
    let mut radii = radii.collect::<Vec<_>>();
    if radii.is_empty() {
        return 1.0;
    }
    radii.sort_by(f32::total_cmp);
    let limit = radii[radii.len() / 2] * 4.0;
    let largest = radii
        .iter()
        .copied()
        .filter(|radius| *radius <= limit)
        .fold(0.0, f32::max);
    (largest * 2.0).max(0.01)
}

struct PreparedLights {
    revision: u64,
    budget: usize,
    buffer: Subbuffer<[LightUpload]>,
    count: u32,
    ambient: [f32; 4],
    ground_ambient: [f32; 4],
    /// Upload index and direction of the one directional light that casts
    /// shadows: the first uploaded one with `shadows` enabled, directional
    /// lights before spot lights.
    shadow: Option<(u32, ShadowView)>,
    /// Uploaded directional lights; they come first and light every pixel.
    directional: u32,
    /// Upload index and bounding sphere (center, range) of each point and
    /// spot light, binned into the light clusters every frame.
    local: Vec<(u32, [f32; 4])>,
    /// Images of the cookie slots, see [`light_cookies`].
    cookies: Vec<Handle<TextureAsset>>,
}

/// Cookie slots in the lit pass; see `LightCookie`.
const MAX_LIGHT_COOKIES: usize = 4;

/// Distinct cookie images of the spot lights, in light order, at most
/// [`MAX_LIGHT_COOKIES`]; a light's cookie slot is its image's index here.
fn light_cookies(render_world: &RenderWorld) -> Vec<Handle<TextureAsset>> {
    let mut cookies = Vec::new();
    for cookie in render_world
        .spot_lights
        .iter()
        .filter_map(|light| light.cookie)
    {
        if cookies.len() < MAX_LIGHT_COOKIES && !cookies.contains(&cookie) {
            cookies.push(cookie);
        }
    }
    cookies
}

/// A decal's box-to-world matrix, settings and image slot.
type FrameDecal<'a> = (&'a [[f32; 4]; 4], &'a crate::runtime::Decal, f32);

/// The decals the main pass paints, at most `limit` in entity order, with
/// their image slot (-1 for none), and the distinct images in slot order. A
/// decal whose image finds no free slot is dropped.
fn frame_decals(
    render_world: &RenderWorld,
    limit: usize,
) -> (Vec<FrameDecal<'_>>, Vec<Handle<TextureAsset>>) {
    let mut images = Vec::new();
    let mut decals = Vec::new();
    for (matrix, decal) in &render_world.decals {
        if decals.len() == limit {
            break;
        }
        let slot = match decal.handle {
            None => -1.0,
            Some(image) => {
                match images.iter().position(|&slot| slot == image) {
                    Some(slot) => slot as f32,
                    None if images.len() < MAX_DECAL_IMAGES => {
                        images.push(image);
                        (images.len() - 1) as f32
                    }
                    None => continue,
                }
            }
        };
        decals.push((matrix, decal, slot));
    }
    (decals, images)
}

/// Screen tiles across and down and depth slices of the light clusters.
const CLUSTER_GRID: [usize; 3] = [16, 9, 24];
const CLUSTER_COUNT: usize =
    CLUSTER_GRID[0] * CLUSTER_GRID[1] * CLUSTER_GRID[2];
/// Local lights one cluster lists; more are left out of that cluster.
// ponytail: fixed cap per cluster bounds the upload at about 3.5 MB; sort by
// brightness before capping if dense light crowds show missing lights.
const MAX_LIGHTS_PER_CLUSTER: usize = 256;

/// Bins point and spot lights into a view-space grid of screen tiles and
/// exponential depth slices, so a pixel shades only the lights whose range
/// reaches its cluster. The list starts with an (offset, count) pair per
/// cluster; offsets index the same list.
// ponytail: CPU binning of world-space boxes around each light sphere, rebuilt
// every frame; move to a compute pass if thousands of lights cost frame time.
fn build_light_clusters(
    local: &[(u32, [f32; 4])],
    clip: &Matrix4<f32>,
    eye: [f32; 3],
    forward: [f32; 3],
    [near, far]: [f32; 2],
) -> Vec<u32> {
    let [columns, rows, slices] = CLUSTER_GRID;
    let eye = Vector3::from(eye);
    let forward = Vector3::from(forward);
    let slice = |depth: f32| {
        if depth <= near {
            0
        } else {
            let t = (depth / near).ln() / (far / near).ln();
            ((t * slices as f32) as usize).min(slices - 1)
        }
    };
    let cell = |ndc: f32, count: usize| {
        (((ndc * 0.5 + 0.5) * count as f32).floor().max(0.0) as usize)
            .min(count - 1)
    };
    let mut ranges = Vec::with_capacity(local.len());
    for &(index, [x, y, z, range]) in local {
        let center = Vector3::new(x, y, z);
        let depth = (center - eye).dot(&forward);
        if depth + range < near.min(0.0) || depth - range > far {
            continue;
        }
        let (mut low, mut high) = ([-1.0_f32; 2], [1.0_f32; 2]);
        let mut behind = false;
        let mut bounds = ([f32::MAX; 2], [f32::MIN; 2]);
        for corner in 0..8 {
            let offset = Vector3::new(
                if corner & 1 == 0 { -range } else { range },
                if corner & 2 == 0 { -range } else { range },
                if corner & 4 == 0 { -range } else { range },
            );
            let point = clip * (center + offset).push(1.0);
            if point.w <= 1e-4 {
                behind = true;
                break;
            }
            for axis in 0..2 {
                let ndc = point[axis] / point.w;
                bounds.0[axis] = bounds.0[axis].min(ndc);
                bounds.1[axis] = bounds.1[axis].max(ndc);
            }
        }
        if !behind {
            if (0..2).any(|axis| bounds.0[axis] > 1.0 || bounds.1[axis] < -1.0)
            {
                continue;
            }
            (low, high) = bounds;
        }
        ranges.push((
            index,
            [cell(low[0], columns), cell(high[0], columns)],
            [cell(low[1], rows), cell(high[1], rows)],
            [slice(depth - range), slice(depth + range)],
        ));
    }
    let cluster = |x: usize, y: usize, z: usize| (z * rows + y) * columns + x;
    let mut counts = vec![0_usize; CLUSTER_COUNT];
    for (_, xs, ys, zs) in &ranges {
        for z in zs[0]..=zs[1] {
            for y in ys[0]..=ys[1] {
                for x in xs[0]..=xs[1] {
                    let count = &mut counts[cluster(x, y, z)];
                    *count = (*count + 1).min(MAX_LIGHTS_PER_CLUSTER);
                }
            }
        }
    }
    let mut list = vec![0_u32; 2 * CLUSTER_COUNT];
    let mut offset = list.len();
    for (index, count) in counts.iter().enumerate() {
        list[2 * index] = offset as u32;
        offset += count;
    }
    list.resize(offset, 0);
    let mut filled = vec![0_usize; CLUSTER_COUNT];
    for (light, xs, ys, zs) in &ranges {
        for z in zs[0]..=zs[1] {
            for y in ys[0]..=ys[1] {
                for x in xs[0]..=xs[1] {
                    let index = cluster(x, y, z);
                    if filled[index] < counts[index] {
                        let at = list[2 * index] as usize + filled[index];
                        list[at] = *light;
                        filled[index] += 1;
                    }
                }
            }
        }
    }
    for (index, count) in counts.iter().enumerate() {
        list[2 * index + 1] = *count as u32;
    }
    list
}

/// Near and far planes the light clusters slice depth between.
fn cluster_depth_range(camera: Option<ExtractedCamera>) -> [f32; 2] {
    let [near, far] = match camera.map(|camera| camera.projection) {
        Some(Projection::Perspective { near, far, .. }) => [near, far],
        Some(Projection::Orthographic { near, far, .. }) => [near, far],
        None => [0.1, 1_000.0],
    };
    let near = near.max(0.01);
    [near, far.max(near * 2.0)]
}

/// Resources reused when one swapchain image comes around again.
struct PreparedFrame {
    graphics_set: Arc<DescriptorSet>,
    framebuffer: Arc<Framebuffer>,
    /// Buffers `graphics_set` was written with. The set is rebuilt when any
    /// prepared buffer is reallocated, whatever caused the reallocation.
    bound: GraphicsBuffers,
    /// Value of `SceneRenderer::frames_rendered` when this target was last
    /// drawn; stale entries belong to a recreated swapchain.
    last_used: u64,
}

type GraphicsBuffers = (
    Subbuffer<[GpuBodyState]>,
    Subbuffer<[RenderInstanceUpload]>,
    Subbuffer<[LightUpload]>,
);

fn same_buffer<T: ?Sized>(a: &Subbuffer<T>, b: &Subbuffer<T>) -> bool {
    Arc::ptr_eq(a.buffer(), b.buffer())
        && a.offset() == b.offset()
        && a.size() == b.size()
}

type FrameFence = Arc<FenceSignalFuture<Box<dyn GpuFuture>>>;

/// Submitted frames allowed in flight before [`SceneRenderer::render`] waits.
pub const FRAMES_IN_FLIGHT: usize = 2;

/// Fixed physics ticks dispatched per rendered frame; later ticks wait for
/// the next frame.
const MAX_PHYSICS_STEPS_PER_FRAME: u64 = 8;
/// Body-contact rounds per fixed tick. One Jacobi round cannot hold a deep
/// pile up; each extra round costs one grid and one contact dispatch.
const CONTACT_ROUNDS: u32 = 4;
/// Largest event buffer per frame (48 bytes per event, 12 MiB in total).
/// Below this, the buffer always fits every rule firing on every tick.
// ponytail: fixed budget; derive it from device memory if scenes need more.
const MAX_PHYSICS_EVENTS: usize = 1 << 18;

/// Submission state of one in-flight frame, reused round-robin.
struct FrameContext {
    /// Signals when this context's last submission finished on the GPU.
    /// Holding it also keeps that submission's resources alive until then.
    fence: Option<FrameFence>,
    /// Host-visible per-frame buffers (debug vertices, physics readback).
    /// Its arenas are reused once this context's frame no longer holds them.
    transient: SubbufferAllocator,
    /// A start and end timestamp per [`FramePass`], or `None` when the
    /// queue cannot write timestamps. See [`PassRecorder`].
    timestamps: Option<Arc<QueryPool>>,
}

/// Packed state of synchronized bodies, the fixed tick it was copied at,
/// and which entries return custom values.
type PendingStateReadback = (Subbuffer<[GpuBodyState]>, u64, Arc<[bool]>);

struct PendingPhysicsReadback {
    fence: FrameFence,
    /// `SceneRenderer::frame_serial` of the submitting frame.
    submitted_frame: u64,
    header: Subbuffer<GpuEventHeader>,
    events: Subbuffer<[GpuEventUpload]>,
    states: Option<PendingStateReadback>,
    /// `physics_hash.comp` output and the first and last tick it covers.
    hashes: (Subbuffer<[[u32; 2]]>, u64, u64),
}

/// Pixel-space area of a render target occupied by the 3D scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneViewport {
    pub offset: [u32; 2],
    pub extent: [u32; 2],
}

/// Per-frame options that differ between a normal game render and Scene View.
///
/// The generic overlay type keeps editor code out of the renderer API. Games
/// use [`Self::game`] and therefore cannot accidentally draw editor helpers.
#[derive(Clone, Copy, Debug)]
pub struct SceneRenderOptions<'a> {
    /// Portion of the target image used by this 3D view.
    pub viewport: SceneViewport,
    /// Optional temporary lines drawn after scene meshes.
    pub debug_overlay: Option<&'a RenderDebugOverlay>,
    /// What the scene pass shows; games always use [`SceneDebugView::Lit`].
    pub debug_view: SceneDebugView,
    /// Scene effects this view draws when the scene has them. Games draw
    /// all of them; the editor's Scene view can switch each one off.
    pub effects: SceneEffects,
    /// Draws through this camera instead of the render world's active one.
    pub camera: Option<ExtractedCamera>,
    /// Replaces the render world's color grading, as a camera screen's
    /// own look does.
    pub grading: Option<crate::runtime::ColorGrading>,
    /// Multiplies the render world's tone mapping exposure.
    pub exposure: f32,
}

/// Atmosphere effects a view may leave out, like Blender's viewport
/// "Scene World" toggle but per effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SceneEffects {
    pub fog: bool,
    pub bloom: bool,
    pub ambient_occlusion: bool,
}

impl Default for SceneEffects {
    fn default() -> Self {
        Self {
            fog: true,
            bloom: true,
            ambient_occlusion: true,
        }
    }
}

/// Diagnostic output of the scene pass, like Godot's viewport debug draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SceneDebugView {
    /// Normal lit rendering.
    #[default]
    Lit = 0,
    /// Base color and textures only, without lighting.
    Unshaded = 1,
    /// World-space normals after normal mapping, as `normal * 0.5 + 0.5`.
    Normals = 2,
}

impl SceneDebugView {
    pub const ALL: [Self; 3] = [Self::Lit, Self::Unshaded, Self::Normals];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lit => "Lit",
            Self::Unshaded => "Unshaded",
            Self::Normals => "Normals",
        }
    }
}

impl<'a> SceneRenderOptions<'a> {
    /// Creates the normal, editor-free game rendering configuration.
    #[must_use]
    pub fn game(extent: [u32; 2]) -> Self {
        Self {
            viewport: SceneViewport::full(extent),
            debug_overlay: None,
            debug_view: SceneDebugView::Lit,
            effects: SceneEffects::default(),
            camera: None,
            grading: None,
            exposure: 1.0,
        }
    }
}

impl SceneViewport {
    #[must_use]
    pub const fn full(extent: [u32; 2]) -> Self {
        Self {
            offset: [0, 0],
            extent,
        }
    }

    fn clamped_to(self, target_extent: [u32; 2]) -> Self {
        let offset = [
            self.offset[0].min(target_extent[0]),
            self.offset[1].min(target_extent[1]),
        ];
        Self {
            offset,
            extent: [
                self.extent[0].min(target_extent[0].saturating_sub(offset[0])),
                self.extent[1].min(target_extent[1].saturating_sub(offset[1])),
            ],
        }
    }
}

/// Largest number of lights uploaded per frame; extra lights are dropped and
/// counted in [`RenderCapacityDiagnostics::dropped_lights`].
pub const MAX_LIGHTS: usize = 1024;

/// Capacity and asset fallbacks the renderer took instead of failing the
/// frame.
/// Compiler messages of the shaders that failed to build in the latest
/// frame, each starting with its kind (`surface:`, `sky:`, `particle:`,
/// `post:` or `condition:`), sorted. Shaders rebuild when their source
/// changes, so fixing one clears its message on the next frame.
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ShaderErrors(pub Vec<String>);

#[derive(
    bevy_ecs::prelude::Resource, Clone, Copy, Debug, Default, PartialEq, Eq,
)]
pub struct RenderCapacityDiagnostics {
    /// Lights beyond [`MAX_LIGHTS`] left out of the latest light upload.
    pub dropped_lights: usize,
    /// Decals past the quality profile's [`decal_budget`] or without a free
    /// image slot, left out of the latest frame.
    pub dropped_decals: usize,
    /// Particles past the quality profile's [`particle_budget`] left out of
    /// the latest frame, CPU particles and GPU slots together.
    pub dropped_particles: usize,
    /// Lights in the latest light upload: directional, point and spot.
    pub lights: usize,
    /// Distinct missing meshes of visible objects; they draw the fallback
    /// cube.
    pub missing_meshes: usize,
    /// Distinct missing materials of visible objects; they draw magenta.
    pub missing_materials: usize,
    /// Referenced maps that are missing or failed to upload. A missing base
    /// color map draws magenta, a missing normal map draws flat.
    pub missing_textures: usize,
    /// GPU physics events lost to event-buffer overflow since creation.
    pub physics_events_dropped: u64,
    /// GPU physics commands rejected since creation because their body was
    /// removed (stale generation) or is not GPU-simulated.
    pub physics_commands_rejected: u64,
    /// GPU bodies that found their contact-grid cell full, summed over the
    /// steps and contact rounds of the latest completed physics frame. They still collide,
    /// through the slower fallback list.
    pub physics_grid_overflow: u32,
    /// GPU bodies too big for one contact-grid cell in the latest completed
    /// physics frame, summed over its steps and contact rounds. Each one
    /// tests every body.
    pub physics_oversized_bodies: u32,
    /// Contact-grid neighbours visited only because two cells share a hash
    /// slot, in the latest completed physics frame.
    pub physics_grid_hash_collisions: u32,
    /// Body pair tests made through the contact fallback list in the latest
    /// completed physics frame; the extra work overflow and oversized bodies
    /// cost.
    pub physics_fallback_tests: u32,
}

/// Minimal opaque forward pass with depth buffering and prepared mesh caching.
pub struct SceneRenderer {
    queue: Arc<Queue>,
    memory_allocator: Arc<StandardMemoryAllocator>,
    command_allocator: Arc<StandardCommandBufferAllocator>,
    descriptor_allocator: Arc<StandardDescriptorSetAllocator>,
    /// Single-sample main passes and pipelines. Their pipeline layouts are
    /// shared with `msaa_passes`, so every descriptor set works with both.
    passes: MainPasses,
    /// The same passes with a multisampled scene subpass, when the device
    /// supports it; see [`RendererCapabilities::msaa_samples`].
    msaa_passes: Option<MainPasses>,
    /// Sample count `msaa_passes` were built for; rebuilt when a frame asks
    /// for another count.
    msaa_pass_samples: u32,
    /// Swapchain format the main passes write, kept to rebuild them.
    output_format: Format,
    /// Multisampled HDR color and depth that resolve into `hdr` and `depth`.
    /// Only allocated while frames use `msaa_passes`.
    msaa_targets: Option<(Arc<ImageView>, Arc<ImageView>)>,
    /// Scene sample count the cached framebuffers were built for.
    scene_samples: u32,
    physics_pipeline: Arc<ComputePipeline>,
    /// Body-contact grid and contact passes, run before `physics_pipeline`
    /// each step.
    physics_grid_pipeline: Arc<ComputePipeline>,
    physics_contact_pipeline: Arc<ComputePipeline>,
    /// Contacts of bodies too big for one grid cell, a workgroup each.
    physics_oversized_pipeline: Arc<ComputePipeline>,
    /// Hashes GPU body state after each step (`physics_hash.comp`).
    physics_hash_pipeline: Arc<ComputePipeline>,
    physics_contact_grid: Option<PhysicsContactGrid>,
    /// Bytes the contact grid's hash may take: an eighth of the per-frame
    /// upload budget, so a sixteenth of the largest device-local heap.
    contact_hash_budget: DeviceSize,
    /// Depth-only pass into `shadow_map`; shares the main pipeline layout.
    shadow_pipeline: Arc<GraphicsPipeline>,
    shadow_framebuffer: Arc<Framebuffer>,
    shadow_map: Arc<ImageView>,
    shadow_sampler: Arc<Sampler>,
    depth: Arc<ImageView>,
    /// Float scene color before tone mapping; same size as `depth`.
    hdr: Arc<ImageView>,
    /// Opaque part of `hdr`, copied on frames with refracting materials.
    scene_color: Arc<ImageView>,
    scene_color_sampler: Arc<Sampler>,
    /// Six faces per captured reflection probe, as array layers with mips;
    /// sampled with `scene_color_sampler`.
    probe_maps: Arc<ImageView>,
    /// Probes `probe_maps` was captured for.
    probe_keys: Vec<([f32; 3], ReflectionProbe)>,
    /// Header rows, then six rows of irradiance over pi per light probe
    /// (+X, -X, +Y, -Y, +Z, -Z); set 2 binding 11.
    light_probes: Subbuffer<[[f32; 4]]>,
    /// Grid `light_probes` was baked for.
    light_probe_key: Option<([f32; 3], crate::runtime::LightProbeGrid)>,
    /// CPU copy of `light_probes`, patched by real-time relighting.
    light_probe_rows: Vec<[f32; 4]>,
    /// Next probe real-time relighting recaptures.
    light_probe_cursor: usize,
    /// Buffers of each GPU particle emitter drawn last frame.
    gpu_particles: HashMap<
        bevy_ecs::entity::Entity,
        super::gpu_particles::GpuParticleState,
    >,
    gpu_particle_pipeline: Option<Arc<ComputePipeline>>,
    /// Unchanging inputs of each GPU-skinned copy, by copy key.
    skin_inputs: HashMap<u64, super::gpu_skinning::SkinInputs>,
    skinning_pipeline: Option<Arc<ComputePipeline>>,
    /// `RenderWorld::elapsed_seconds` of the last frame, for particle steps.
    particle_clock: f32,
    /// Renderer `capture_faces` reuses while the format and size match.
    capturer: Option<(Format, u32, Box<SceneRenderer>)>,
    /// Baked lightmaps; see `lightmap` for the layout.
    lightmaps: Subbuffer<[[f32; 4]]>,
    /// Hash of what `lightmaps` was baked from, 0 for nothing.
    lightmap_key: u64,
    /// Reads `hdr` as the input attachment of the tone-mapping subpass,
    /// and the bloom glow.
    tonemap_set: Arc<DescriptorSet>,
    /// Bloom and ambient occlusion compute pipelines.
    post_pipelines: PostPipelines,
    /// Rebuilt with `hdr`.
    bloom_chain: BloomChain,
    /// Rebuilt with `hdr`.
    exposure: ExposureMeter,
    /// Rebuilt with `depth`.
    occlusion: OcclusionTargets,
    /// View projection of the frame `scene_color` holds, while it holds
    /// one; indirect light reprojects into it.
    scene_color_view_projection: Option<Matrix4<f32>>,
    /// Depth-only opaque pass into `depth` that ambient occlusion traces;
    /// shares the main pipeline layout.
    depth_prepass_pipeline: Arc<GraphicsPipeline>,
    /// Rebuilt with `depth`.
    depth_prepass_framebuffer: Arc<Framebuffer>,
    depth_extent: [u32; 2],
    /// Per-change instance uploads. Arenas are reused once no in-flight
    /// frame references them and double in size when an upload outgrows them.
    instance_allocator: SubbufferAllocator,
    /// Largest per-frame instance upload accepted; see
    /// [`transient_upload_budget`].
    instance_budget: DeviceSize,
    prepared_meshes: HashMap<u64, PreparedMesh>,
    prepared_meshes_revision: (u64, u64),
    /// `lod_signature` `visible_meshes` was collected with.
    prepared_meshes_lods: Vec<(u64, u64)>,
    prepared_textures: HashMap<u64, PreparedTexture>,
    prepared_materials: HashMap<u64, PreparedMaterial>,
    /// Camera images by the key of the material they replace maps of.
    screen_feeds: HashMap<u64, ScreenFeed>,
    /// Calls of [`Self::render_screens`], for `update_every`.
    screen_frame: u64,
    /// Feeds the last [`Self::render_screens`] drew: camera and feed key.
    screens_drawn: Vec<(bevy_ecs::entity::Entity, u64)>,
    /// 1x1 white texture bound in place of every missing material map.
    white_texture: (Arc<ImageView>, Arc<Sampler>),
    /// The procedural sky's lighting image, made on first use.
    sky_bake: Option<SkyBake>,
    /// Per-slot stand-ins for a map that is referenced but missing or
    /// malformed: magenta base color, flat normal, white for the rest.
    missing_textures: [(Arc<ImageView>, Arc<Sampler>); MATERIAL_TEXTURES],
    /// Set 1 with every map white, for materials not prepared yet.
    white_material: Arc<DescriptorSet>,
    samplers: HashMap<TextureSampler, Arc<Sampler>>,
    pending_texture_uploads: Vec<PendingTextureUpload>,
    visible_meshes: Vec<Handle<MeshAsset>>,
    prepared_instances: Option<PreparedRenderInstances>,
    prepared_physics: Option<PreparedGpuPhysics>,
    prepared_lights: Option<PreparedLights>,
    prepared_frames: HashMap<usize, PreparedFrame>,
    frames_rendered: u64,
    pending_physics: Vec<PendingPhysicsReadback>,
    /// Frames rendered so far; dates physics readbacks.
    frame_serial: u64,
    /// Readback traffic collected since the last frame, moved into
    /// `counters` when a frame is recorded.
    readback_counters: RenderCounters,
    completed_physics_events: Vec<RawGpuPhysicsEvent>,
    completed_physics_states: Vec<crate::runtime::GpuStateSample>,
    completed_physics_hashes: Vec<(u64, u64)>,
    /// Events lost to a full event buffer and not yet reported to gameplay.
    physics_events_lost: u64,
    max_physics_events: usize,
    /// Commands extracted but not yet applied: they wait for a frame that
    /// runs a fixed step.
    // ponytail: unbounded while physics is disabled; cap it if games queue
    // commands for long pauses.
    /// Commands with the GPU tick each applies before.
    queued_physics_commands: Vec<(
        u64,
        crate::runtime::PhysicsId,
        crate::runtime::GpuBodyCommand,
    )>,
    physics_commands_serial: u64,
    queued_read_all: bool,
    /// Custom condition shaders by resolved source. `Err` keeps the compiler
    /// message so a broken shader is reported once, not rebuilt every frame.
    condition_shaders: HashMap<String, Result<Arc<ComputePipeline>, String>>,
    /// Opaque, blended and overlay pipelines of each material surface shader
    /// by source and sample count, or why it failed to build.
    surface_pipelines: SurfacePipelines,
    /// Alpha and additive particle pipelines per particle shader.
    particle_shaders: ParticlePipelines,
    /// Source, sample count and tone-map pipeline of the last post shader.
    post_shader: ShaderPipeline,
    /// The same for the sky shader.
    sky_shader: ShaderPipeline,
    last_physics_tick: u64,
    last_frame_passes: Vec<FramePass>,
    /// Image the next frame is stretched over after it renders, for
    /// `RenderSettings::render_scale`.
    /// Image the finished target is blitted onto, and the part of it.
    upscale_to: Option<(Arc<ImageView>, Option<SceneViewport>)>,
    /// Nearest-neighbour filtering for the `upscale_to` blit.
    upscale_nearest: bool,
    /// Strength of the sharp upscale that replaces the linear stretch of
    /// [`Self::upscale_next_frame`]; 0 keeps the blit.
    upscale_sharpness: f32,
    /// Built for the first sharp upscale's target format.
    sharp_upscale: Option<(Arc<RenderPass>, Arc<GraphicsPipeline>)>,
    last_frame_culled: Option<usize>,
    last_culling_path: CullingPath,
    culling_stats: CullingStats,
    pending_cull: Vec<PendingCullReadback>,
    pending_pass_times: Vec<PendingPassTimes>,
    gpu_pass_times: GpuPassTimes,
    preparation_time: Duration,
    recording_time: Duration,
    counters: RenderCounters,
    /// Triangles of the newest read-back GPU-culled frame's indirect draws.
    gpu_culled_triangles: u64,
    cull_pipeline: Arc<ComputePipeline>,
    depth_pyramid_copy_pipeline: Arc<ComputePipeline>,
    depth_pyramid_reduce_pipeline: Arc<ComputePipeline>,
    /// Rebuilt with `depth`.
    depth_pyramid: DepthPyramid,
    /// Last frame's view projection without jitter, while motion blur or
    /// TAA is on.
    previous_clip: Option<Matrix4<f32>>,
    /// Built on the first TAA frame.
    temporal: Option<TemporalHistory>,
    /// Picks each TAA frame's jitter.
    temporal_frame: u32,
    /// Last GPU-culled frame's draw commands, early set first on occlusion
    /// frames, for readback in tests.
    #[cfg(test)]
    last_draw_commands: Vec<Subbuffer<[DrawIndexedIndirectCommand]>>,
    capacity: RenderCapacityDiagnostics,
    frame_contexts: [FrameContext; FRAMES_IN_FLIGHT],
    frame_index: usize,
    capabilities: RendererCapabilities,
}

impl SceneRenderer {
    pub fn new(
        queue: Arc<Queue>,
        memory_allocator: Arc<StandardMemoryAllocator>,
        output_format: Format,
        initial_extent: [u32; 2],
    ) -> Result<Self, SceneRenderError> {
        let limits = DeviceLimits::of(queue.device().physical_device());
        let capabilities = RendererCapabilities::detect(queue.device());
        let shortfalls = limits.shortfalls(&LOW_END_BASELINE);
        if !shortfalls.is_empty() {
            return Err(SceneRenderError(format!(
                "{} is below the renderer baseline: {}",
                queue.device().physical_device().properties().device_name,
                shortfalls.join("; ")
            )));
        }
        let passes = create_main_passes(&queue, output_format, 1, None)?;
        let msaa_passes = (capabilities.msaa_samples > 1)
            .then(|| {
                create_main_passes(
                    &queue,
                    output_format,
                    capabilities.msaa_samples,
                    Some(&passes),
                )
            })
            .transpose()?;
        let pipeline = passes.pipeline.clone();
        let tonemap_pipeline = passes.tonemap_pipeline.clone();
        let physics_pipeline = create_compute_pipeline(
            &queue,
            physics_shader::load(queue.device().clone()),
        )?;
        let physics_grid_pipeline = create_compute_pipeline(
            &queue,
            physics_grid_shader::load(queue.device().clone()),
        )?;
        let physics_contact_pipeline = create_compute_pipeline(
            &queue,
            physics_contact_shader::load(queue.device().clone()),
        )?;
        let physics_oversized_pipeline = create_compute_pipeline(
            &queue,
            physics_oversized_shader::load(queue.device().clone()),
        )?;
        let physics_hash_pipeline = create_compute_pipeline(
            &queue,
            physics_hash_shader::load(queue.device().clone()),
        )?;
        let cull_pipeline = create_compute_pipeline(
            &queue,
            cull_shader::load(queue.device().clone()),
        )?;
        let depth_pyramid_copy_pipeline = create_compute_pipeline(
            &queue,
            depth_pyramid_copy_shader::load(queue.device().clone()),
        )?;
        let depth_pyramid_reduce_pipeline = create_compute_pipeline(
            &queue,
            depth_pyramid_reduce_shader::load(queue.device().clone()),
        )?;
        let (shadow_pipeline, shadow_framebuffer, shadow_map, shadow_sampler) =
            create_shadow_pass(&queue, &memory_allocator, &pipeline)?;
        let depth = create_depth(&memory_allocator, initial_extent, 1)?;
        let hdr = create_hdr(&memory_allocator, initial_extent, 1)?;
        let scene_color =
            create_scene_color(&memory_allocator, initial_extent)?;
        let probe_maps = create_probe_maps(&memory_allocator, 1, 1)?;
        let light_probes = create_light_probe_buffer(
            &memory_allocator,
            vec![[0.0; 4]; LIGHT_PROBE_HEADER + 6],
        )?;
        let lightmaps =
            create_light_probe_buffer(&memory_allocator, vec![[0.0; 4]])?;
        let scene_color_sampler = Sampler::new(
            queue.device().clone(),
            SamplerCreateInfo {
                address_mode: [SamplerAddressMode::ClampToEdge; 3],
                lod: 0.0..=vulkano::image::sampler::LOD_CLAMP_NONE,
                ..SamplerCreateInfo::simple_repeat_linear()
            },
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let instance_allocator = SubbufferAllocator::new(
            memory_allocator.clone(),
            SubbufferAllocatorCreateInfo {
                arena_size: INSTANCE_ARENA_BYTES,
                buffer_usage: BufferUsage::STORAGE_BUFFER
                    | BufferUsage::VERTEX_BUFFER
                    | BufferUsage::INDIRECT_BUFFER,
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
        );
        let instance_budget = transient_upload_budget(
            queue
                .device()
                .physical_device()
                .memory_properties()
                .memory_heaps
                .iter()
                .map(|heap| (heap.size, heap.flags)),
        )
        // The instance buffer is bound as one storage-buffer range.
        .min(DeviceSize::from(limits.max_storage_buffer_range));
        let timestamp_queue =
            queue.device().physical_device().queue_family_properties()
                [queue.queue_family_index() as usize]
                .timestamp_valid_bits
                .is_some();
        let frame_contexts = std::array::from_fn(|_| FrameContext {
            fence: None,
            timestamps: timestamp_queue
                .then(|| {
                    QueryPool::new(
                        queue.device().clone(),
                        QueryPoolCreateInfo {
                            query_count: 2 * FramePass::ALL.len() as u32,
                            ..QueryPoolCreateInfo::query_type(
                                QueryType::Timestamp,
                            )
                        },
                    )
                    .ok()
                })
                .flatten(),
            transient: SubbufferAllocator::new(
                memory_allocator.clone(),
                SubbufferAllocatorCreateInfo {
                    arena_size: TRANSIENT_ARENA_BYTES,
                    buffer_usage: BufferUsage::STORAGE_BUFFER
                        | BufferUsage::VERTEX_BUFFER
                        | BufferUsage::TRANSFER_DST,
                    memory_type_filter: MemoryTypeFilter::PREFER_HOST
                        | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                    ..Default::default()
                },
            ),
        });
        let descriptor_allocator =
            Arc::new(StandardDescriptorSetAllocator::new(
                queue.device().clone(),
                Default::default(),
            ));
        let default_sampler =
            texture_sampler(&queue, TextureSampler::default())?;
        let mut pending_texture_uploads = Vec::new();
        let white_view = create_texture(
            &memory_allocator,
            &TextureAsset {
                size: [1, 1],
                rgba8: vec![255; 4],
                color_space: TextureColorSpace::Linear,
                sampler: TextureSampler::default(),
            },
            &mut pending_texture_uploads,
        )?;
        let white_texture = (white_view, default_sampler.clone());
        let mut solid = |rgba8: [u8; 4]| {
            create_texture(
                &memory_allocator,
                &TextureAsset {
                    size: [1, 1],
                    rgba8: rgba8.to_vec(),
                    color_space: TextureColorSpace::Linear,
                    sampler: TextureSampler::default(),
                },
                &mut pending_texture_uploads,
            )
            .map(|view| (view, default_sampler.clone()))
        };
        let missing_textures = [
            solid([255, 0, 255, 255])?,
            solid([128, 128, 255, 255])?,
            white_texture.clone(),
            white_texture.clone(),
            white_texture.clone(),
        ];
        let post_pipelines = PostPipelines::new(&queue)?;
        let bloom_chain = BloomChain::new(
            &memory_allocator,
            &descriptor_allocator,
            &post_pipelines,
            &hdr,
        )?;
        let exposure = ExposureMeter::new(
            &memory_allocator,
            &descriptor_allocator,
            &post_pipelines,
            &hdr,
        )?;
        let occlusion =
            OcclusionTargets::new(&memory_allocator, initial_extent)?;
        let depth_prepass_pipeline = create_depth_prepass(&queue, &pipeline)?;
        let depth_prepass_framebuffer =
            create_depth_prepass_framebuffer(&depth_prepass_pipeline, &depth)?;
        let tonemap_set = create_tonemap_set(
            &descriptor_allocator,
            &tonemap_pipeline,
            &hdr,
            &bloom_chain,
            &exposure,
            &scene_color,
            &scene_color_sampler,
        )?;
        let depth_pyramid = create_depth_pyramid(
            &memory_allocator,
            &descriptor_allocator,
            &depth_pyramid_copy_pipeline,
            &depth_pyramid_reduce_pipeline,
            &depth,
        )?;
        passes.name("");
        if let Some(msaa) = &msaa_passes {
            msaa.name(" (MSAA)");
        }
        name_object(&*physics_pipeline, "GPU physics");
        name_object(&*physics_grid_pipeline, "GPU physics contact grid");
        name_object(&*physics_contact_pipeline, "GPU physics contacts");
        name_object(
            &*physics_oversized_pipeline,
            "GPU physics oversized contacts",
        );
        name_object(&*physics_hash_pipeline, "GPU physics state hash");
        name_object(&*cull_pipeline, "Culling");
        name_object(&*depth_pyramid_copy_pipeline, "Depth pyramid copy");
        name_object(&*depth_pyramid_reduce_pipeline, "Depth pyramid reduce");
        name_object(&*shadow_pipeline, "Shadow");
        for (index, context) in frame_contexts.iter().enumerate() {
            if let Some(pool) = &context.timestamps {
                name_object(&**pool, &format!("Pass timestamps {index}"));
            }
        }
        let white_material = create_material_set(
            &descriptor_allocator,
            &pipeline.layout().set_layouts()[1],
            std::array::from_fn(|_| white_texture.clone()),
        )?;
        Ok(Self {
            instance_allocator,
            instance_budget,
            contact_hash_budget: instance_budget / 8,
            command_allocator: Arc::new(StandardCommandBufferAllocator::new(
                queue.device().clone(),
                Default::default(),
            )),
            descriptor_allocator,
            prepared_textures: HashMap::new(),
            prepared_materials: HashMap::new(),
            screen_feeds: HashMap::new(),
            screen_frame: 0,
            screens_drawn: Vec::new(),
            white_texture,
            sky_bake: None,
            missing_textures,
            white_material,
            samplers: HashMap::from([(
                TextureSampler::default(),
                default_sampler,
            )]),
            pending_texture_uploads,
            queue,
            memory_allocator,
            passes,
            msaa_passes,
            msaa_pass_samples: capabilities.msaa_samples,
            output_format,
            msaa_targets: None,
            scene_samples: 1,
            physics_pipeline,
            physics_grid_pipeline,
            physics_contact_pipeline,
            physics_oversized_pipeline,
            physics_hash_pipeline,
            physics_contact_grid: None,
            cull_pipeline,
            depth_pyramid_copy_pipeline,
            depth_pyramid_reduce_pipeline,
            depth_pyramid,
            previous_clip: None,
            temporal: None,
            temporal_frame: 0,
            shadow_pipeline,
            shadow_framebuffer,
            shadow_map,
            shadow_sampler,
            depth,
            hdr,
            scene_color,
            scene_color_sampler,
            probe_maps,
            probe_keys: Vec::new(),
            light_probes,
            light_probe_key: None,
            light_probe_rows: Vec::new(),
            light_probe_cursor: 0,
            capturer: None,
            gpu_particles: HashMap::new(),
            gpu_particle_pipeline: None,
            skin_inputs: HashMap::new(),
            skinning_pipeline: None,
            particle_clock: 0.0,
            lightmaps,
            lightmap_key: 0,
            tonemap_set,
            post_pipelines,
            bloom_chain,
            exposure,
            occlusion,
            scene_color_view_projection: None,
            depth_prepass_pipeline,
            depth_prepass_framebuffer,
            depth_extent: initial_extent,
            prepared_meshes: HashMap::new(),
            prepared_meshes_revision: (0, 0),
            prepared_meshes_lods: Vec::new(),
            visible_meshes: Vec::new(),
            prepared_instances: None,
            prepared_physics: None,
            prepared_lights: None,
            prepared_frames: HashMap::new(),
            frames_rendered: 0,
            pending_physics: Vec::new(),
            frame_serial: 0,
            readback_counters: RenderCounters::default(),
            completed_physics_events: Vec::new(),
            completed_physics_states: Vec::new(),
            completed_physics_hashes: Vec::new(),
            physics_events_lost: 0,
            max_physics_events: MAX_PHYSICS_EVENTS,
            queued_physics_commands: Vec::new(),
            physics_commands_serial: 0,
            queued_read_all: false,
            condition_shaders: HashMap::new(),
            surface_pipelines: HashMap::new(),
            particle_shaders: HashMap::new(),
            post_shader: None,
            sky_shader: None,
            last_physics_tick: 0,
            last_frame_passes: Vec::new(),
            upscale_to: None,
            upscale_nearest: false,
            upscale_sharpness: 0.0,
            sharp_upscale: None,
            last_frame_culled: Some(0),
            last_culling_path: CullingPath::Direct,
            culling_stats: CullingStats::default(),
            pending_cull: Vec::new(),
            pending_pass_times: Vec::new(),
            gpu_pass_times: GpuPassTimes::default(),
            preparation_time: Duration::ZERO,
            recording_time: Duration::ZERO,
            counters: RenderCounters::default(),
            gpu_culled_triangles: 0,
            #[cfg(test)]
            last_draw_commands: Vec::new(),
            capacity: RenderCapacityDiagnostics::default(),
            frame_contexts,
            frame_index: 0,
            capabilities,
        })
    }

    /// The passes the last `render` recorded, in order. Each one is also a
    /// debug-utils label when the instance enables `ext_debug_utils`.
    pub fn last_frame_passes(&self) -> &[FramePass] {
        &self.last_frame_passes
    }

    /// Instances the last frame skipped in the main pass by frustum culling,
    /// or `None` when the GPU culled and the count stayed on the GPU.
    /// The shadow pass draws them all so off-view casters still cast.
    pub fn last_frame_culled(&self) -> Option<usize> {
        self.last_frame_culled
    }

    /// Culling counts and cost for the profiler; see [`CullingStats`].
    /// Collects completed GPU readbacks first, without waiting.
    pub fn culling_stats(&mut self) -> CullingStats {
        self.collect_cull_readbacks();
        self.culling_stats
    }

    /// GPU time per pass for the profiler; see [`GpuPassTimes`]. Collects
    /// completed readbacks first, without waiting.
    pub fn gpu_pass_times(&mut self) -> GpuPassTimes {
        self.collect_cull_readbacks();
        self.gpu_pass_times.clone()
    }

    /// Work counters of the last frame. Collects completed readbacks first,
    /// without waiting.
    pub fn render_counters(&mut self) -> RenderCounters {
        self.collect_cull_readbacks();
        let gpu_memory_bytes = self
            .memory_allocator
            .pools()
            .iter()
            .flat_map(|pool| pool.blocks())
            .map(|block| block.device_memory().allocation_size())
            .sum();
        RenderCounters {
            triangles: self.counters.triangles + self.gpu_culled_triangles,
            visible_instances: self.culling_stats.visible,
            gpu_memory_bytes,
            ..self.counters
        }
    }

    /// Writes the last frame's CPU preparation and command recording times.
    pub fn write_cpu_timings(&self, timings: &mut CpuFrameTimings) {
        timings.preparation = self.preparation_time;
        timings.recording = self.recording_time;
    }

    /// Reads back GPU-culled frames and pass timestamps whose submission has
    /// completed. The newest completed frame wins.
    fn collect_cull_readbacks(&mut self) {
        let period = self
            .queue
            .device()
            .physical_device()
            .properties()
            .timestamp_period;
        let mut index = 0;
        while index < self.pending_pass_times.len() {
            if !self.pending_pass_times[index]
                .fence
                .is_signaled()
                .unwrap_or(false)
            {
                index += 1;
                continue;
            }
            let pending = self.pending_pass_times.remove(index);
            self.gpu_pass_times = GpuPassTimes(
                pending
                    .passes
                    .iter()
                    .filter_map(|&pass| {
                        Some((pass, pass_time(&pending.pool, pass, period)?))
                    })
                    .collect(),
            );
        }
        let mut index = 0;
        while index < self.pending_cull.len() {
            if !self.pending_cull[index]
                .fence
                .is_signaled()
                .unwrap_or(false)
            {
                index += 1;
                continue;
            }
            // Pending entries are in submission order, so a later completed
            // one overwrites an earlier one.
            let pending = self.pending_cull.remove(index);
            let mut visible = 0;
            let mut triangles = 0;
            for commands in &pending.commands {
                let Ok(commands) = commands.read() else {
                    continue;
                };
                for command in commands.iter() {
                    visible += command.instance_count as usize;
                    triangles += u64::from(command.index_count / 3)
                        * u64::from(command.instance_count);
                }
            }
            self.gpu_culled_triangles = triangles;
            // Occlusion frames add the pyramid and the late cull.
            let time = pending.timestamps.and_then(|pool| {
                let culling = pass_time(&pool, FramePass::Culling, period)?;
                Some(
                    [FramePass::DepthPyramid, FramePass::OcclusionCulling]
                        .into_iter()
                        .filter_map(|pass| pass_time(&pool, pass, period))
                        .sum::<Duration>()
                        + culling,
                )
            });
            self.culling_stats = CullingStats {
                path: pending.path,
                submitted: pending.submitted,
                visible,
                culled: pending.submitted.saturating_sub(visible),
                time,
            };
        }
    }

    /// Makes the next [`Self::render`] stretch its finished target over
    /// `target` with a linear blit, in the same command buffer.
    pub fn upscale_next_frame(&mut self, target: Arc<ImageView>) {
        self.upscale_to = Some((target, None));
    }

    /// Makes the next [`Self::render`] blit its viewport onto `region` of
    /// `target`, in the same command buffer.
    /// Nearest-neighbour (`true`) or linear filtering for the stretch of
    /// [`Self::upscale_next_frame`] and [`Self::blit_next_frame`].
    pub fn set_upscale_nearest(&mut self, nearest: bool) {
        self.upscale_nearest = nearest;
    }

    /// Above 0, [`Self::upscale_next_frame`] stretches with an
    /// edge-preserving filter and contrast-adaptive sharpening of this
    /// strength (up to 1) instead of the linear blit.
    pub fn set_upscale_sharpness(&mut self, sharpness: f32) {
        self.upscale_sharpness = sharpness.clamp(0.0, 1.0);
    }

    pub fn blit_next_frame(
        &mut self,
        target: Arc<ImageView>,
        region: SceneViewport,
    ) {
        self.upscale_to = Some((target, Some(region)));
    }

    /// Clears `target` to black after `before`.
    pub fn clear(
        &self,
        before: Box<dyn GpuFuture>,
        target: Arc<ImageView>,
    ) -> Result<Box<dyn GpuFuture>, SceneRenderError> {
        let error =
            |error: &dyn std::fmt::Display| SceneRenderError(error.to_string());
        let mut commands = AutoCommandBufferBuilder::primary(
            self.command_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|e| error(&e))?;
        commands
            .clear_color_image(
                vulkano::command_buffer::ClearColorImageInfo::image(
                    target.image().clone(),
                ),
            )
            .map_err(|e| error(&e))?;
        let commands = commands.build().map_err(|e| error(&e))?;
        Ok(before
            .then_execute(self.queue.clone(), commands)
            .map_err(|e| error(&e))?
            .boxed())
    }

    /// The culling path the last `render` took.
    pub fn last_culling_path(&self) -> CullingPath {
        self.last_culling_path
    }

    /// Feeds the last [`Self::render_screens`] drew; skipped ones keep
    /// their previous image.
    #[must_use]
    pub fn screens_drawn(&self) -> usize {
        self.screens_drawn.len()
    }

    /// Camera, work counters and GPU time of each feed the last
    /// [`Self::render_screens`] drew. Read after that frame's work ends.
    pub fn screen_stats(
        &mut self,
    ) -> Vec<(
        bevy_ecs::entity::Entity,
        RenderCounters,
        std::time::Duration,
    )> {
        let drawn = self.screens_drawn.clone();
        drawn
            .into_iter()
            .filter_map(|(camera, key)| {
                let feed = self.screen_feeds.get_mut(&key)?;
                let gpu = feed.renderer.gpu_pass_times().total();
                Some((camera, feed.renderer.render_counters(), gpu))
            })
            .collect()
    }

    /// Draws the camera of each screen in the render world into its own image, which
    /// the next [`Self::render`] shows on the screen's material. Call it
    /// before the frame's views.
    ///
    /// A feed is drawn on its first frame, then every `update_every`
    /// frames (staggered by screen order) while it is enabled and its mesh
    /// is inside the view of the active camera or of a viewport camera
    /// drawn into `extent`. Otherwise it keeps its last image.
    // ponytail: a full child renderer per screen (own pipelines and targets);
    // share pipelines between them if scenes with many screens cost too much.
    pub fn render_screens(
        &mut self,
        before: Box<dyn GpuFuture>,
        render_world: &RenderWorld,
        assets: &AssetServer,
        extent: [u32; 2],
    ) -> Result<Box<dyn GpuFuture>, SceneRenderError> {
        use crate::rendering::swapchain::OFFSCREEN_COLOR_FORMAT;
        let failed =
            |error: &dyn std::fmt::Display| SceneRenderError(error.to_string());
        self.screen_feeds.retain(|key, feed| {
            render_world.screens.iter().any(|screen| {
                screen.material.key() == *key && screen.size == feed.size
            })
        });
        let mut future = before;
        // Feeds draw GPU bodies from this renderer's states, one frame
        // behind, instead of simulating every body again per screen.
        let shared_physics = self
            .prepared_physics
            .as_ref()
            .filter(|physics| {
                physics.source_revision == render_world.gpu_physics_revision
            })
            .map(|physics| PreparedGpuPhysics {
                carry: None,
                ..physics.clone()
            });
        let mut views: Vec<_> = render_world
            .views
            .iter()
            .map(|(camera, rect)| {
                let size = [
                    (extent[0] as f32 * rect[2]).max(1.0) as u32,
                    (extent[1] as f32 * rect[3]).max(1.0) as u32,
                ];
                frustum_planes(&view_projection(Some(*camera), size))
            })
            .collect();
        if let Some(camera) = render_world.active_camera {
            views.push(frustum_planes(&view_projection(Some(camera), extent)));
        }
        let frame = self.screen_frame;
        self.screen_frame += 1;
        self.screens_drawn.clear();
        for (index, screen) in render_world.screens.iter().enumerate() {
            let key = screen.material.key();
            let fresh = !self.screen_feeds.contains_key(&key);
            let due = (frame + index as u64)
                .is_multiple_of(u64::from(screen.update_every.max(1)));
            let seen = || {
                let Some((min, max)) = assets.mesh_bounds(screen.mesh) else {
                    return true;
                };
                let bounds = world_aabb(screen.transform.matrix, min, max);
                views.is_empty()
                    || views
                        .iter()
                        .any(|planes| bounds_in_frustum(planes, &bounds))
            };
            if !fresh && (!screen.enabled || !due || !seen()) {
                continue;
            }
            self.screens_drawn.push((screen.camera.entity, key));
            if fresh {
                let image = Image::new(
                    self.memory_allocator.clone(),
                    ImageCreateInfo {
                        format: OFFSCREEN_COLOR_FORMAT,
                        extent: [screen.size[0], screen.size[1], 1],
                        usage: ImageUsage::COLOR_ATTACHMENT
                            | ImageUsage::SAMPLED,
                        ..Default::default()
                    },
                    AllocationCreateInfo {
                        memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                        ..Default::default()
                    },
                )
                .map_err(|error| failed(&error))?;
                let feed = ScreenFeed {
                    renderer: Box::new(SceneRenderer::new(
                        self.queue.clone(),
                        self.memory_allocator.clone(),
                        OFFSCREEN_COLOR_FORMAT,
                        screen.size,
                    )?),
                    view: ImageView::new_default(image)
                        .map_err(|error| failed(&error))?,
                    sampler: texture_sampler(
                        &self.queue,
                        TextureSampler {
                            mag_filter: TextureFilter::Linear,
                            min_filter: TextureFilter::Linear,
                            mipmap_filter: TextureFilter::Linear,
                            wrap: [TextureWrap::ClampToEdge; 2],
                        },
                    )?,
                    size: screen.size,
                };
                self.screen_feeds.insert(key, feed);
            }
            let feed = self.screen_feeds.get_mut(&key).expect("inserted above");
            if let Some(physics) = &shared_physics {
                feed.renderer.prepared_physics = Some(physics.clone());
                feed.renderer.last_physics_tick = render_world.physics_tick;
            }
            let options = SceneRenderOptions {
                camera: Some(screen.camera),
                grading: screen.grading,
                exposure: screen.exposure,
                ..SceneRenderOptions::game(screen.size)
            };
            future = feed.renderer.render(
                future,
                feed.view.clone(),
                screen.size,
                options,
                render_world,
                assets,
            )?;
        }
        Ok(future)
    }

    pub fn render(
        &mut self,
        before: Box<dyn GpuFuture>,
        target: Arc<ImageView>,
        extent: [u32; 2],
        options: SceneRenderOptions<'_>,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<Box<dyn GpuFuture>, SceneRenderError> {
        self.render_frame(
            before,
            target,
            extent,
            options,
            render_world,
            assets,
            false,
        )
    }

    /// Advances GPU physics to the render world's tick, with its commands
    /// and readback, as [`Self::render`] would, but draws nothing. Headless
    /// tools use it on ticks no image is taken, so GPU bodies simulate the
    /// same ticks faster than drawing allows.
    // ponytail: still runs the CPU-side draw preparation (instances, culling
    // setup); skip it too if it shows up in physics-only profiles.
    pub fn step_physics(
        &mut self,
        before: Box<dyn GpuFuture>,
        target: Arc<ImageView>,
        extent: [u32; 2],
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<Box<dyn GpuFuture>, SceneRenderError> {
        self.render_frame(
            before,
            target,
            extent,
            SceneRenderOptions::game(extent),
            render_world,
            assets,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_frame(
        &mut self,
        before: Box<dyn GpuFuture>,
        target: Arc<ImageView>,
        extent: [u32; 2],
        options: SceneRenderOptions<'_>,
        render_world: &RenderWorld,
        assets: &AssetServer,
        physics_only: bool,
    ) -> Result<Box<dyn GpuFuture>, SceneRenderError> {
        // A viewport reported past the target (rounding, a resize race) is
        // cropped by the scissor; projecting the clamped extent would squash it.
        let visible = options.viewport.clamped_to(extent);
        let viewport = SceneViewport {
            offset: visible.offset,
            extent: options.viewport.extent,
        };
        if extent[0] == 0
            || extent[1] == 0
            || visible.extent[0] == 0
            || visible.extent[1] == 0
        {
            // Time spent minimized is skipped, not simulated in one step on
            // restore.
            self.last_physics_tick = render_world.physics_tick;
            self.upscale_to = None;
            return Ok(before);
        }
        for context in &mut self.frame_contexts {
            // Completed frames release their resources without a wait.
            if context
                .fence
                .as_ref()
                .is_some_and(|fence| fence.is_signaled().unwrap_or(false))
            {
                context.fence = None;
            }
        }
        if let Some(fence) = self.frame_contexts[self.frame_index].fence.take()
        {
            // Blocks only when the GPU is FRAMES_IN_FLIGHT frames behind.
            fence
                .wait(None)
                .map_err(|error| SceneRenderError(error.to_string()))?;
        }
        // Before this context's timestamp queries are reset and reused.
        self.collect_cull_readbacks();
        let probes = &render_world.reflection_probes[..render_world
            .reflection_probes
            .len()
            .min(MAX_REFLECTION_PROBES)];
        if self.probe_keys != probes {
            self.capture_probes(probes, render_world, assets)?;
        }
        if self.light_probe_key != render_world.light_probe_grid {
            self.bake_light_probes(render_world, assets)?;
        } else if resolve_quality(render_world.quality, &self.capabilities)
            == QualityProfile::High
        {
            self.relight_light_probes(render_world, assets)?;
        }
        self.counters = RenderCounters::default();
        let preparation_start = std::time::Instant::now();
        self.prepare_visible_meshes(render_world, assets)?;
        if render_world.gpu_physics_commands_serial
            != self.physics_commands_serial
        {
            self.physics_commands_serial =
                render_world.gpu_physics_commands_serial;
            if render_world.gpu_physics_reset {
                // Rebuilding without a previous table carries no state.
                // In-flight frames keep the old buffers alive themselves.
                self.prepared_physics = None;
                self.queued_physics_commands.clear();
            }
            let ticks = &render_world.gpu_physics_command_ticks;
            self.queued_physics_commands.extend(
                render_world.gpu_physics_commands.iter().enumerate().map(
                    |(index, &(id, command))| {
                        (ticks.get(index).copied().unwrap_or(0), id, command)
                    },
                ),
            );
            self.queued_read_all |= render_world.gpu_physics_read_all;
        }
        self.prepare_gpu_physics(render_world)?;
        self.prepare_lights(render_world)?;
        self.prepare_render_instances(render_world, assets)?;
        let lightmap_key = lightmap_key(render_world, assets);
        if self.lightmap_key != lightmap_key {
            self.bake_lightmaps(lightmap_key, render_world, assets)?;
        }
        let camera = options.camera.or(render_world.active_camera);
        let steady_clip = view_projection(camera, viewport.extent);
        let taa = options.debug_view == SceneDebugView::Lit
            && render_world.antialiasing == Antialiasing::Taa;
        // TAA moves the whole view by under a pixel each frame and blends
        // the frames into smooth edges.
        let clip = if taa {
            self.temporal_frame = self.temporal_frame.wrapping_add(1);
            let index = self.temporal_frame % 8 + 1;
            let mut jitter = Matrix4::identity();
            jitter[(0, 3)] = (halton(index, 2) - 0.5) * 2.0
                / viewport.extent[0].max(1) as f32;
            jitter[(1, 3)] = (halton(index, 3) - 0.5) * 2.0
                / viewport.extent[1].max(1) as f32;
            jitter * steady_clip
        } else {
            if let Some(history) = &mut self.temporal {
                history.view = None;
            }
            steady_clip
        };
        // The projection alone turns stored depth back into distance.
        let lens_depth = {
            let lens = view_projection(
                camera.map(|camera| ExtractedCamera {
                    transform: Default::default(),
                    ..camera
                }),
                viewport.extent,
            );
            [lens[(2, 2)], lens[(2, 3)], lens[(3, 2)], lens[(3, 3)]]
        };
        let cluster_range = cluster_depth_range(camera);
        let prepared = self.prepared_instances.as_ref().unwrap();
        let quality = resolve_quality(render_world.quality, &self.capabilities);
        let path = select_culling_path(
            render_world.culling,
            prepared.cull_source.len(),
            prepared.gpu_owned,
            prepared.lod_spheres.iter().any(Option::is_some),
            quality,
            &self.capabilities,
        );
        let samples = scene_sample_count(
            render_world.antialiasing,
            quality,
            &self.capabilities,
        );
        if samples > 1 && samples != self.msaa_pass_samples {
            // In-flight frames keep the old passes alive through their
            // command buffers; ensure_depth drops the cached framebuffers.
            let msaa = create_main_passes(
                &self.queue,
                self.output_format,
                samples,
                Some(&self.passes),
            )?;
            msaa.name(" (MSAA)");
            self.msaa_passes = Some(msaa);
            self.msaa_pass_samples = samples;
        }
        self.ensure_depth(extent, samples)?;
        let active = match &self.msaa_passes {
            Some(msaa) if samples > 1 => msaa.clone(),
            _ => self.passes.clone(),
        };
        let post_shader =
            self.post_pipeline(&active, samples, &render_world.post_shader);
        let sky_shader = self.sky_pipeline(
            &active,
            samples,
            &render_world.sky_shader,
            &render_world.fog_shader,
        );
        let particle_shaders = particle_pipelines(
            &mut self.particle_shaders,
            &self.queue,
            &active,
            samples,
            &render_world.particle_shaders,
        );
        // Resolve attachments are neither cleared nor loaded.
        let clears = |hdr: Option<vulkano::format::ClearValue>, depth| {
            let mut values = vec![None, hdr, depth];
            if samples > 1 {
                values.extend([None, None]);
            }
            values
        };
        let frustum = render_world.culling != CullingMode::Disabled;
        let (eye, forward) = camera_eye_forward(camera);
        let (camera_right, camera_up) = camera_right_up(camera);
        let lod_camera = [eye[0], eye[1], eye[2], lod_scale(camera)];
        let cull_start = std::time::Instant::now();
        match path {
            CullingPath::Direct => {
                self.prepare_visibility(clip, false, lod_camera)?
            }
            CullingPath::Cpu => {
                self.prepare_visibility(clip, frustum, lod_camera)?
            }
            CullingPath::Gpu | CullingPath::GpuOcclusion => {
                self.prepare_cull_instances()?
            }
        }
        if !path.on_gpu() {
            let prepared = self.prepared_instances.as_ref().unwrap();
            let culled = prepared.visibility.as_ref().unwrap().culled;
            self.culling_stats = CullingStats {
                path,
                submitted: prepared.bounds.len(),
                visible: prepared.bounds.len() - culled,
                culled,
                time: Some(cull_start.elapsed()),
            };
        }
        self.last_culling_path = path;
        let quality = resolve_quality(render_world.quality, &self.capabilities);
        let (decal_limit, particle_limit) =
            (decal_budget(quality), particle_budget(quality));
        self.prepare_materials(
            assets,
            render_world
                .environment
                .map(|(texture, ..)| texture)
                .into_iter()
                .chain(light_cookies(render_world))
                .chain(frame_decals(render_world, decal_limit).1)
                .chain(render_world.color_lut.map(|(texture, _)| texture))
                .collect(),
        )?;
        let particle_steps = self.prepare_gpu_particles(render_world)?;
        let skin_steps = self.prepare_skinning(render_world, assets)?;
        let particle_total = particle_total(render_world);
        let particle_share = particle_share(particle_total, particle_limit);

        let carry = self.prepared_physics.as_mut().unwrap().carry.take();
        let physics = self.prepared_physics.as_ref().unwrap();
        let lights = self.prepared_lights.as_ref().unwrap();
        let new_ticks = render_world
            .physics_tick
            .saturating_sub(self.last_physics_tick);
        let physics_ran = render_world.physics_enabled
            && new_ticks > 0
            && !physics.source.is_empty();
        // One dispatch per fixed tick keeps the integration step at
        // `fixed_delta`. Ticks past the cap wait for the next frame instead
        // of being dropped, so the GPU simulates every tick the CPU counts.
        let steps = new_ticks.min(MAX_PHYSICS_STEPS_PER_FRAME);
        let first_tick = self.last_physics_tick + 1;
        let last_tick = self.last_physics_tick + steps;

        let render_instances = self.prepared_instances.as_ref().unwrap();
        let frame_key = Arc::as_ptr(&target) as usize;
        self.frames_rendered += 1;
        // A recreated swapchain (resize, vsync toggle, out-of-date) brings
        // new image views. Drop entries no image has used for a while, so the
        // old framebuffers stop keeping the old swapchain alive. Held entries
        // keep their view alive, so a pointer key is never reused while cached.
        let frames_rendered = self.frames_rendered;
        self.prepared_frames
            .retain(|_, frame| frames_rendered - frame.last_used < 16);
        let bound: GraphicsBuffers = (
            physics.states.clone(),
            render_instances.instances.clone(),
            lights.buffer.clone(),
        );
        let stale = self.prepared_frames.get(&frame_key).is_none_or(|frame| {
            !same_buffer(&frame.bound.0, &bound.0)
                || !same_buffer(&frame.bound.1, &bound.1)
                || !same_buffer(&frame.bound.2, &bound.2)
        });
        if stale {
            let graphics_set = DescriptorSet::new(
                self.descriptor_allocator.clone(),
                active.pipeline.layout().set_layouts()[0].clone(),
                [
                    WriteDescriptorSet::buffer(0, bound.0.clone()),
                    WriteDescriptorSet::buffer(1, bound.1.clone()),
                    WriteDescriptorSet::buffer(2, bound.2.clone()),
                ],
                [],
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
            let framebuffer = match self.prepared_frames.remove(&frame_key) {
                Some(frame) => frame.framebuffer,
                None => Framebuffer::new(
                    active.render_pass.clone(),
                    FramebufferCreateInfo {
                        attachments: match &self.msaa_targets {
                            Some((hdr, depth)) => vec![
                                target.clone(),
                                hdr.clone(),
                                depth.clone(),
                                self.hdr.clone(),
                                self.depth.clone(),
                            ],
                            None => vec![
                                target.clone(),
                                self.hdr.clone(),
                                self.depth.clone(),
                            ],
                        },
                        ..Default::default()
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?,
            };
            self.prepared_frames.insert(
                frame_key,
                PreparedFrame {
                    graphics_set,
                    framebuffer,
                    bound,
                    last_used: frames_rendered,
                },
            );
        }
        let frame = self.prepared_frames.get_mut(&frame_key).unwrap();
        frame.last_used = frames_rendered;
        let graphics_set = frame.graphics_set.clone();
        let framebuffer = frame.framebuffer.clone();
        let orthographic = camera.is_some_and(|camera| {
            matches!(camera.projection, Projection::Orthographic { .. })
        });
        let camera = CameraUniform {
            view_projection: clip.into(),
            eye: if orthographic {
                [-forward[0], -forward[1], -forward[2], 0.0]
            } else {
                [eye[0], eye[1], eye[2], 1.0]
            },
            // w: seconds since start, a surface shader's `i.time`.
            ambient: [
                lights.ambient[0],
                lights.ambient[1],
                lights.ambient[2],
                render_world.elapsed_seconds,
            ],
            ground_ambient: lights.ground_ambient,
            // y: shadowed light index plus one, or 0 for none.
            light_info: [
                lights.count,
                lights.shadow.map_or(0, |(index, _)| index + 1),
                options.debug_view as u32,
                lights.directional,
            ],
        };
        // The GPU paths leave `visibility` as the last CPU-culled frame's.
        let visibility = (!path.on_gpu())
            .then(|| render_instances.visibility.as_ref().unwrap());
        self.last_frame_culled = visibility.map(|visibility| visibility.culled);
        // One output set per cull dispatch that feeds draws: the early one
        // first on occlusion frames. The last set feeds the pass that also
        // draws blended instances.
        let occlusion = path == CullingPath::GpuOcclusion;
        let mut cull_sets: Vec<GpuCullSet> = Vec::new();
        if !path.on_gpu() {
            // ponytail: a GPU frame read back after this one can bring its
            // triangles back for a frame; tag readbacks with a frame number
            // if the stale value matters.
            self.gpu_culled_triangles = 0;
        }
        if path.on_gpu() {
            for _ in 0..if occlusion { 2 } else { 1 } {
                let (list, draw_commands) = gpu_cull_buffers(
                    &self.instance_allocator,
                    render_instances,
                    &self.prepared_meshes,
                )?;
                self.counters.upload_bytes += draw_commands.size();
                let set = DescriptorSet::new(
                    self.descriptor_allocator.clone(),
                    self.cull_pipeline.layout().set_layouts()[0].clone(),
                    [
                        WriteDescriptorSet::buffer(0, physics.states.clone()),
                        WriteDescriptorSet::buffer(
                            1,
                            render_instances.instances.clone(),
                        ),
                        WriteDescriptorSet::buffer(
                            2,
                            render_instances.cull.clone().unwrap(),
                        ),
                        WriteDescriptorSet::buffer(3, draw_commands.clone()),
                        WriteDescriptorSet::buffer(4, list.clone()),
                        WriteDescriptorSet::buffer(
                            5,
                            render_instances.occlusion.clone().unwrap(),
                        ),
                        WriteDescriptorSet::image_view_sampler(
                            6,
                            self.depth_pyramid.view.clone(),
                            self.depth_pyramid.sampler.clone(),
                        ),
                    ],
                    [],
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
                cull_sets.push((list, draw_commands, set));
            }
        }
        #[cfg(test)]
        {
            self.last_draw_commands = cull_sets
                .iter()
                .map(|(_, commands, _)| commands.clone())
                .collect();
        }
        let gpu_cull = cull_sets.last();
        let early_cull = occlusion.then(|| &cull_sets[0]);
        let (shadow_size, shadow_distance) =
            shadow_settings_for(render_world.shadows, quality);
        if self.shadow_framebuffer.extent()[0] != shadow_size {
            // In-flight frames keep the old map alive through their command
            // buffers.
            (self.shadow_framebuffer, self.shadow_map) = create_shadow_target(
                self.shadow_framebuffer.render_pass(),
                &self.memory_allocator,
                shadow_size,
            )?;
        }
        // A directional light renders one cascade per tile of a 2x2 atlas,
        // a point light one cube face per tile of a 3x3 atlas; a spot light
        // takes the whole map.
        let (cascades, cascade_far, tiles) = match lights.shadow {
            Some((_, ShadowView::Directional(direction))) => {
                let tile_size = shadow_size / 2;
                let mut near = 0.0;
                let mut cascades = Vec::new();
                let mut far = [0.0; 4];
                for (index, end) in far.iter_mut().enumerate() {
                    *end = cascade_end(index, shadow_distance);
                    let (center, radius) = frustum_slice_sphere(
                        &steady_clip,
                        eye,
                        forward,
                        near,
                        *end,
                    );
                    cascades.push(cascade_view_projection(
                        center,
                        radius,
                        direction,
                        shadow_distance,
                        tile_size,
                    ));
                    near = *end;
                }
                (cascades, far, 2)
            }
            Some((
                _,
                ShadowView::Spot {
                    position,
                    direction,
                    outer_angle,
                    range,
                },
            )) => (
                vec![spot_shadow_view_projection(
                    position,
                    direction,
                    outer_angle,
                    range,
                )],
                [f32::MAX, 0.0, 0.0, 0.0],
                1,
            ),
            Some((_, ShadowView::Point { position, range })) => (
                point_shadow_view_projections(position, range, shadow_size / 3),
                [f32::MAX, 0.0, 0.0, 0.0],
                3,
            ),
            None => (Vec::new(), [0.0; 4], 1),
        };
        let point_shadow = match lights.shadow {
            Some((_, ShadowView::Point { position, .. })) => Some(position),
            _ => None,
        };
        // Refracting materials and screen-space reflections sample a copy
        // of the opaque scene, so the scene pass splits before blended
        // draws. Debug views skip it; Eco skips reflections.
        let reflections = options.debug_view == SceneDebugView::Lit
            && !render_world.reflections_disabled
            && quality != QualityProfile::Eco;
        let (refraction, glossy) = self.prepared_instances.as_ref().map_or(
            (false, false),
            |prepared| {
                (prepared.refractive, !prepared.glossy_batches.is_empty())
            },
        );
        let lit = options.debug_view == SceneDebugView::Lit;
        // ponytail: refraction still needs the copy even with reflections off.
        let mut split = lit && (refraction || (reflections && glossy));
        // Atmosphere belongs to the lit look; debug views show raw shading.
        let effects = options.effects;
        let fog = render_world
            .fog
            .filter(|fog| lit && effects.fog && fog.density > 0.0);
        let (decals, decal_images) = frame_decals(render_world, decal_limit);
        self.capacity.dropped_decals = render_world.decals.len() - decals.len();
        let fog_volumes: &[_] = if lit && effects.fog {
            let volumes = &render_world.fog_volumes;
            &volumes[..volumes.len().min(MAX_FOG_VOLUMES)]
        } else {
            &[]
        };
        let bloom = render_world
            .bloom
            .filter(|bloom| lit && effects.bloom && bloom.intensity > 0.0);
        let auto_exposure = render_world.auto_exposure.filter(|_| lit);
        if auto_exposure.is_none() {
            self.exposure.reset();
        }
        let grade = if lit {
            options
                .grading
                .or(render_world.color_grading)
                .unwrap_or_default()
        } else {
            crate::runtime::ColorGrading::default()
        };
        // A camera screen's own grading replaces the scene's look, LUT
        // included. A strip that is not N squares of N x N is skipped.
        let color_lut = render_world
            .color_lut
            .filter(|(_, intensity)| {
                lit && options.grading.is_none() && *intensity > 0.0
            })
            .and_then(|(handle, intensity)| {
                let view =
                    self.prepared_textures.get(&handle.key())?.view.clone()?;
                let [width, height, _] = view.image().extent();
                (height >= 2 && width == height * height)
                    .then_some((view, intensity.min(1.0)))
            });
        // Effects that read neighboring pixels sample a copy of the HDR
        // image, made after the render pass is split as for bloom.
        let depth_of_field = render_world
            .depth_of_field
            .filter(|lens| lit && lens.blur > 0.0 && lens.focus_distance > 0.0);
        let motion_blur = render_world
            .motion_blur
            .filter(|blur| lit && blur.intensity > 0.0);
        // Maps this frame's clip space to last frame's; without a last
        // frame nothing moved.
        let reproject = self
            .previous_clip
            .filter(|_| motion_blur.is_some() || taa)
            .and_then(|previous| Some(previous * steady_clip.try_inverse()?))
            .unwrap_or_else(Matrix4::identity);
        self.previous_clip =
            (motion_blur.is_some() || taa).then_some(steady_clip);
        let motion = self
            .instance_allocator
            .allocate_sized::<tonemap_fragment_shader::Motion>()
            .map_err(|error| SceneRenderError(error.to_string()))?;
        *motion
            .write()
            .map_err(|error| SceneRenderError(error.to_string()))? =
            tonemap_fragment_shader::Motion {
                reproject: reproject.into(),
                strength: motion_blur.map_or(0.0, |blur| blur.intensity),
            };
        let fxaa = lit && render_world.antialiasing == Antialiasing::Fxaa;
        // A post shader may read any pixel through `scene_at`.
        let sampled = fxaa
            || post_shader.is_some()
            || taa
            || grade.chromatic_aberration > 0.0
            || grade.color_bleed > 0.0
            || grade.distortion > 0.0
            || depth_of_field.is_some()
            || motion_blur.is_some();
        // Bloom, the copy and auto exposure read the finished HDR outside
        // the render pass.
        let post = bloom.is_some() || sampled || auto_exposure.is_some();
        let ambient_occlusion =
            render_world.ambient_occlusion.filter(|occlusion| {
                lit && effects.ambient_occlusion
                    && quality != QualityProfile::Eco
                    && occlusion.radius > 0.0
                    && occlusion.intensity > 0.0
            });
        // Indirect light gathers from last frame's opaque copy, so these
        // frames make one; High only.
        let indirect_light = ambient_occlusion
            .map(|occlusion| occlusion.indirect.max(0.0))
            .filter(|&strength| {
                strength > 0.0 && quality == QualityProfile::High
            });
        split |= indirect_light.is_some();
        // The copy made this frame, if any, is the one next frame reads.
        let previous_view_projection =
            indirect_light.and(self.scene_color_view_projection);
        if split {
            self.scene_color_view_projection = Some(clip);
        }
        let sky = render_world
            .procedural_sky
            .filter(|_| lit)
            .map(|sky| sky_params(&sky, &render_world.directional_lights));
        let environment = render_world
            .environment
            .and_then(|(texture, intensity, background)| {
                let prepared = self.prepared_textures.get(&texture.key())?;
                Some((
                    (prepared.view.clone()?, prepared.sampler.clone()),
                    intensity,
                    background && lit,
                ))
            })
            .map(Ok)
            .or_else(|| {
                // Without an environment map, the sky lights the scene.
                sky?;
                let bake = match &mut self.sky_bake {
                    Some(bake) => bake,
                    None => match SkyBake::new(
                        &self.queue,
                        &self.memory_allocator,
                        &self.descriptor_allocator,
                    ) {
                        Ok(bake) => self.sky_bake.insert(bake),
                        Err(error) => return Some(Err(error)),
                    },
                };
                Some(Ok((
                    (bake.view.clone(), bake.sampler.clone()),
                    1.0,
                    false,
                )))
            })
            .transpose()?;
        let environment_background = environment
            .as_ref()
            .is_some_and(|(.., background)| *background);
        let shadow_upload = self.frame_contexts[self.frame_index]
            .transient
            .allocate_sized::<ShadowUpload>()
            .map_err(|error| SceneRenderError(error.to_string()))?;
        self.counters.upload_bytes += shadow_upload.size();
        *shadow_upload
            .write()
            .map_err(|error| SceneRenderError(error.to_string()))? =
            ShadowUpload {
                environment: {
                    let mut row = environment.as_ref().map_or(
                        [0.0; 4],
                        |((view, _), intensity, _)| {
                            let last_mip = view.image().mip_levels() - 1;
                            [1.0, *intensity, last_mip as f32, 0.0]
                        },
                    );
                    row[3] = self.probe_keys.len() as f32;
                    row
                },
                scene_color: if split {
                    let last_mip = self.scene_color.image().mip_levels() - 1;
                    [
                        1.0,
                        last_mip as f32,
                        f32::from(u8::from(reflections)),
                        0.0,
                    ]
                } else {
                    [0.0; 4]
                },
                viewport: [
                    viewport.offset[0] as f32,
                    viewport.offset[1] as f32,
                    viewport.extent[0] as f32,
                    viewport.extent[1] as f32,
                ],
                probes: {
                    let last_mip = self.probe_maps.image().mip_levels() - 1;
                    let mut rows = [[0.0; 4]; 2 * MAX_REFLECTION_PROBES];
                    for (index, (center, probe)) in
                        self.probe_keys.iter().enumerate()
                    {
                        let [x, y, z] = *center;
                        let [ex, ey, ez] = probe.extents;
                        rows[2 * index] = [x, y, z, probe.intensity];
                        rows[2 * index + 1] = [ex, ey, ez, last_mip as f32];
                    }
                    rows
                },
                inverse_view_projection: clip
                    .try_inverse()
                    .unwrap_or_else(Matrix4::identity)
                    .into(),
                fog: fog.map_or([0.0; 4], |fog| {
                    let [r, g, b] = fog.color;
                    [r.max(0.0), g.max(0.0), b.max(0.0), fog.density]
                }),
                fog_shape: fog.map_or([0.0; 4], |fog| {
                    [
                        fog.height,
                        fog.height_falloff.max(0.0),
                        fog.sun_scatter.max(0.0),
                        fog.sky_affect.clamp(0.0, 1.0),
                    ]
                }),
                ambient_occlusion: [
                    f32::from(u8::from(ambient_occlusion.is_some())),
                    f32::from(u8::from(render_world.hard_shadows)),
                    f32::from(u8::from(previous_view_projection.is_some())),
                    0.0,
                ],
                cascades: std::array::from_fn(|index| {
                    cascades
                        .get(index)
                        .copied()
                        .unwrap_or_else(Matrix4::identity)
                        .into()
                }),
                cascade_far,
                cascade_forward: match point_shadow {
                    Some(position) => {
                        [position[0], position[1], position[2], 0.0]
                    }
                    None => [
                        forward[0],
                        forward[1],
                        forward[2],
                        Vector3::from(eye).dot(&Vector3::from(forward)),
                    ],
                },
                cascade_info: [
                    cascades.len() as f32,
                    tiles as f32,
                    f32::from(u8::from(point_shadow.is_some())),
                    0.0,
                ],
                cluster_view: [
                    forward[0],
                    forward[1],
                    forward[2],
                    Vector3::from(eye).dot(&Vector3::from(forward)),
                ],
                cluster_depth: [
                    cluster_range[0],
                    (cluster_range[1] / cluster_range[0]).ln(),
                    0.0,
                    0.0,
                ],
                fog_shafts: [
                    fog.map_or(0.0, |fog| fog.light_shafts.clamp(0.0, 1.0)),
                    fog_volumes.len() as f32,
                    0.0,
                    0.0,
                ],
                fog_volumes: std::array::from_fn(|index| {
                    fog_volumes
                        .get(index)
                        .and_then(|(matrix, _)| {
                            Matrix4::from(*matrix).try_inverse()
                        })
                        .unwrap_or_else(Matrix4::identity)
                        .into()
                }),
                fog_volume_colors: std::array::from_fn(|index| {
                    fog_volumes.get(index).map_or([0.0; 4], |(_, volume)| {
                        let [r, g, b] = volume.color;
                        [r.max(0.0), g.max(0.0), b.max(0.0), volume.density]
                    })
                }),
                sky: {
                    let mut rows = sky.unwrap_or_default();
                    if environment_background {
                        rows[2][3] = 2.0;
                    }
                    rows
                },
                decal_info: [decals.len() as f32, 0.0, 0.0, 0.0],
                decals: std::array::from_fn(|index| {
                    decals
                        .get(index)
                        .and_then(|(matrix, ..)| {
                            Matrix4::from(**matrix).try_inverse()
                        })
                        .unwrap_or_else(Matrix4::identity)
                        .into()
                }),
                decal_axes: std::array::from_fn(|index| {
                    decals.get(index).map_or([0.0; 4], |(matrix, _, slot)| {
                        let axis = Matrix4::from(**matrix)
                            .transform_vector(&Vector3::z())
                            .try_normalize(0.0)
                            .unwrap_or_else(Vector3::z);
                        [axis.x, axis.y, axis.z, *slot]
                    })
                }),
                decal_colors: std::array::from_fn(|index| {
                    decals.get(index).map_or([0.0; 4], |(_, decal, _)| {
                        decal.color.map(|channel| channel.max(0.0))
                    })
                }),
                decal_surfaces: std::array::from_fn(|index| {
                    decals
                        .get(index)
                        .and_then(|(_, decal, _)| decal.roughness)
                        .map_or([0.0; 4], |roughness| {
                            [roughness.clamp(0.0, 1.0), 1.0, 0.0, 0.0]
                        })
                }),
            };
        let cluster_list = build_light_clusters(
            &self.prepared_lights.as_ref().unwrap().local,
            &clip,
            eye,
            forward,
            cluster_range,
        );
        let light_clusters = self.frame_contexts[self.frame_index]
            .transient
            .allocate_slice::<u32>(cluster_list.len() as DeviceSize)
            .map_err(|error| SceneRenderError(error.to_string()))?;
        self.counters.upload_bytes += light_clusters.size();
        light_clusters
            .write()
            .map_err(|error| SceneRenderError(error.to_string()))?
            .copy_from_slice(&cluster_list);
        let shadow_set = DescriptorSet::new(
            self.descriptor_allocator.clone(),
            active.pipeline.layout().set_layouts()[2].clone(),
            [
                WriteDescriptorSet::image_view_sampler(
                    0,
                    self.shadow_map.clone(),
                    self.shadow_sampler.clone(),
                ),
                WriteDescriptorSet::buffer(1, shadow_upload),
                {
                    let (view, sampler) = environment.map_or_else(
                        || self.white_texture.clone(),
                        |(texture, ..)| texture,
                    );
                    WriteDescriptorSet::image_view_sampler(2, view, sampler)
                },
                {
                    let (view, sampler) = if split {
                        (
                            self.scene_color.clone(),
                            self.scene_color_sampler.clone(),
                        )
                    } else {
                        self.white_texture.clone()
                    };
                    WriteDescriptorSet::image_view_sampler(3, view, sampler)
                },
                {
                    let (view, sampler) = if split && reflections {
                        (
                            self.depth_pyramid.view.clone(),
                            self.depth_pyramid.sampler.clone(),
                        )
                    } else {
                        self.white_texture.clone()
                    };
                    WriteDescriptorSet::image_view_sampler(4, view, sampler)
                },
                WriteDescriptorSet::image_view_sampler(
                    5,
                    self.probe_maps.clone(),
                    self.scene_color_sampler.clone(),
                ),
                {
                    let (view, sampler) = if ambient_occlusion.is_some() {
                        (
                            self.occlusion.view.clone(),
                            self.post_pipelines.nearest.clone(),
                        )
                    } else {
                        self.white_texture.clone()
                    };
                    WriteDescriptorSet::image_view_sampler(6, view, sampler)
                },
                WriteDescriptorSet::buffer(7, light_clusters),
                WriteDescriptorSet::buffer(11, self.light_probes.clone()),
                WriteDescriptorSet::buffer(12, self.lightmaps.clone()),
                WriteDescriptorSet::image_view_sampler_array(8, 0, {
                    let cookies = self
                        .prepared_lights
                        .as_ref()
                        .map_or(&[][..], |lights| &lights.cookies);
                    (0..MAX_LIGHT_COOKIES).map(|slot| {
                        cookies
                            .get(slot)
                            .and_then(|cookie| {
                                let prepared = self
                                    .prepared_textures
                                    .get(&cookie.key())?;
                                Some((
                                    prepared.view.clone()?,
                                    prepared.sampler.clone(),
                                ))
                            })
                            .unwrap_or_else(|| self.white_texture.clone())
                    })
                }),
                {
                    let (view, sampler) = if previous_view_projection.is_some()
                    {
                        (
                            self.occlusion.indirect.clone(),
                            self.post_pipelines.nearest.clone(),
                        )
                    } else {
                        self.white_texture.clone()
                    };
                    WriteDescriptorSet::image_view_sampler(10, view, sampler)
                },
                WriteDescriptorSet::image_view_sampler_array(
                    9,
                    0,
                    (0..MAX_DECAL_IMAGES).map(|slot| {
                        decal_images
                            .get(slot)
                            .and_then(|image| {
                                let prepared =
                                    self.prepared_textures.get(&image.key())?;
                                Some((
                                    prepared.view.clone()?,
                                    prepared.sampler.clone(),
                                ))
                            })
                            .unwrap_or_else(|| self.white_texture.clone())
                    }),
                ),
            ],
            [],
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let occlusion_params = ambient_occlusion
            .map(|settings| {
                let upload = self.frame_contexts[self.frame_index]
                    .transient
                    .allocate_sized::<OcclusionParams>()
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                self.counters.upload_bytes += upload.size();
                let forward = Vector3::from(forward).normalize();
                *upload
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))? =
                    OcclusionParams {
                        view_projection: clip.into(),
                        inverse_view_projection: clip
                            .try_inverse()
                            .unwrap_or_else(Matrix4::identity)
                            .into(),
                        viewport: [
                            viewport.offset[0] as f32,
                            viewport.offset[1] as f32,
                            viewport.extent[0] as f32,
                            viewport.extent[1] as f32,
                        ],
                        forward: [
                            forward.x,
                            forward.y,
                            forward.z,
                            forward.dot(&Vector3::from(eye)),
                        ],
                        settings: [
                            settings.radius,
                            settings.intensity,
                            previous_view_projection
                                .and(indirect_light)
                                .unwrap_or(0.0),
                            0.0,
                        ],
                        previous_view_projection: previous_view_projection
                            .unwrap_or_else(Matrix4::identity)
                            .into(),
                    };
                Ok::<_, SceneRenderError>(upload)
            })
            .transpose()?;

        let mut reads = Vec::new();
        // First upload and count of each step's commands.
        let mut command_ranges = vec![(0u32, 0u32); steps as usize];
        let command_uploads = if physics_ran
            && !self.queued_physics_commands.is_empty()
        {
            let bodies: HashMap<u32, (u32, u32)> = physics
                .source
                .iter()
                .enumerate()
                .map(|(index, body)| {
                    (
                        body.physics_id.slot,
                        (index as u32, body.physics_id.generation),
                    )
                })
                .collect();
            let (due, later) =
                std::mem::take(&mut self.queued_physics_commands)
                    .into_iter()
                    .partition::<Vec<_>, _>(|(tick, ..)| *tick <= last_tick);
            self.queued_physics_commands = later;
            let mut uploads = Vec::new();
            for (tick, id, command) in due {
                // Late commands apply before this frame's first step.
                let step = tick.saturating_sub(first_tick) as usize;
                match bodies.get(&id.slot) {
                    Some(&(index, generation))
                        if generation == id.generation =>
                    {
                        if command == crate::runtime::GpuBodyCommand::ReadState
                        {
                            reads.push(index);
                        } else {
                            uploads.push((
                                step,
                                GpuCommandUpload::new(
                                    index, generation, &command,
                                ),
                            ));
                        }
                    }
                    _ => self.capacity.physics_commands_rejected += 1,
                }
            }
            // Each step's commands are contiguous, and the shader finds a
            // body's commands in them by binary search; the stable sort
            // keeps each body's commands in submission order.
            uploads.sort_by_key(|(step, upload)| (*step, upload.header[0]));
            for (position, (step, _)) in uploads.iter().enumerate() {
                let range = &mut command_ranges[*step];
                if range.1 == 0 {
                    range.0 = position as u32;
                }
                range.1 += 1;
            }
            uploads.into_iter().map(|(_, upload)| upload).collect()
        } else {
            Vec::new()
        };
        // One-shot reads add to the bodies whose sync mode reads every tick.
        let (readback, readback_full) =
            if physics_ran && (self.queued_read_all || !reads.is_empty()) {
                let stride = size_of::<GpuBodyState>() as u64;
                let mut wanted: Vec<bool> = physics
                    .source
                    .iter()
                    .map(|body| {
                        self.queued_read_all || body.sync.reads_back_state()
                    })
                    .collect();
                for &index in &reads {
                    wanted[index as usize] = true;
                }
                self.queued_read_all = false;
                let (mut regions, mut full) = (Vec::new(), Vec::new());
                for (index, body) in physics.source.iter().enumerate() {
                    if wanted[index] {
                        regions.push(BufferCopy {
                            src_offset: index as u64 * stride,
                            dst_offset: regions.len() as u64 * stride,
                            size: stride,
                            ..Default::default()
                        });
                        // Requested reads return everything, custom values too.
                        full.push(
                        body.sync
                            != crate::runtime::PhysicsSyncMode::SelectedState,
                    );
                    }
                }
                (std::borrow::Cow::Owned(regions), full.into())
            } else {
                (
                    std::borrow::Cow::Borrowed(physics.readback.as_slice()),
                    physics.readback_full.clone(),
                )
            };
        let condition_pipelines = if physics_ran {
            let shaders = render_world
                .gpu_solver_shaders
                .iter()
                .cloned()
                .map(crate::runtime::ExtractedConditionShader::from)
                .chain(render_world.gpu_condition_shaders.iter().cloned())
                .collect::<Vec<_>>();
            condition_pipelines(
                &mut self.condition_shaders,
                &self.queue,
                shaders,
            )
        } else {
            Vec::new()
        };
        // Physics runs at the fixed rate, which is commonly much lower than
        // render FPS. Do not allocate readback buffers on interpolation-only
        // frames where no compute dispatch will use them. Each rule fires at
        // most once per tick, so rules times steps always fits every event.
        let event_capacity = physics_ran.then(|| {
            let rules = physics
                .source
                .iter()
                .map(|body| body.rules.len())
                .sum::<usize>();
            let steps = steps as usize;
            // Custom shaders get `events_per_body` per body and tick each.
            let custom = condition_pipelines
                .iter()
                .map(|(_, shader)| shader.events_per_body as usize)
                .sum::<usize>()
                * physics.source.len();
            ((rules + custom) * steps)
                .max(64)
                .min(self.max_physics_events)
        });
        // ponytail: one collider snapshot per frame, shared by every fixed
        // step in it; kinematic colliders move in whole-frame jumps.
        let colliders = if physics_ran {
            render_world
                .gpu_colliders
                .iter()
                .map(GpuColliderUpload::from)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let collider_nodes = collider_tree(&colliders);
        let force_fields = if physics_ran {
            render_world
                .gpu_force_fields
                .iter()
                .map(GpuForceFieldUpload::from)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let observers = match render_world.gpu_lod {
            Some(_) if physics_ran => render_world
                .gpu_observers
                .iter()
                .map(|&[x, y, z]| [x, y, z, 0.0])
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let lod = render_world.gpu_lod.unwrap_or_default();
        let physics_resources = if let Some(event_capacity) = event_capacity {
            let transient = &self.frame_contexts[self.frame_index].transient;
            let event_header = transient
                .allocate_sized::<GpuEventHeader>()
                .map_err(|error| SceneRenderError(error.to_string()))?;
            self.counters.upload_bytes += event_header.size();
            *event_header
                .write()
                .map_err(|error| SceneRenderError(error.to_string()))? =
                GpuEventHeader::default();
            // Two hash lanes per tick slot; see `physics_hash.comp`.
            let tick_hashes = transient
                .allocate_slice::<[u32; 2]>(MAX_PHYSICS_STEPS_PER_FRAME)
                .map_err(|error| SceneRenderError(error.to_string()))?;
            tick_hashes
                .write()
                .map_err(|error| SceneRenderError(error.to_string()))?
                .fill([0; 2]);
            let hash_layout =
                self.physics_hash_pipeline.layout().set_layouts()[0].clone();
            let hash_writes = [
                WriteDescriptorSet::buffer(0, physics.states.clone()),
                WriteDescriptorSet::buffer(2, physics.rules.clone()),
                WriteDescriptorSet::buffer(13, tick_hashes.clone()),
            ]
            .into_iter()
            .filter(|write| {
                hash_layout.bindings().contains_key(&write.binding())
            });
            let hash_set = DescriptorSet::new(
                self.descriptor_allocator.clone(),
                hash_layout.clone(),
                hash_writes,
                [],
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
            let event_buffer = transient
                .allocate_slice::<GpuEventUpload>(event_capacity as u64)
                .map_err(|error| SceneRenderError(error.to_string()))?;
            // A zero-sized buffer cannot be bound, so always allocate one.
            let command_buffer = transient
                .allocate_slice::<GpuCommandUpload>(
                    command_uploads.len().max(1) as u64,
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            if !command_uploads.is_empty() {
                command_buffer
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&command_uploads);
                self.counters.upload_bytes += command_buffer.size();
                self.counters.physics_commands = command_uploads.len() as u32;
                self.counters.physics_command_bytes = command_buffer.size();
            }
            let colliders_buffer = transient
                .allocate_slice::<GpuColliderUpload>(
                    colliders.len().max(1) as u64
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            if !colliders.is_empty() {
                colliders_buffer
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&colliders);
                self.counters.upload_bytes += colliders_buffer.size();
            }
            let collider_tree_buffer = transient
                .allocate_slice::<GpuColliderNode>(
                    collider_nodes.len().max(1) as u64
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            if !collider_nodes.is_empty() {
                collider_tree_buffer
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&collider_nodes);
                self.counters.upload_bytes += collider_tree_buffer.size();
            }
            let force_field_buffer = transient
                .allocate_slice::<GpuForceFieldUpload>(
                    force_fields.len().max(1) as u64,
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            if !force_fields.is_empty() {
                force_field_buffer
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&force_fields);
                self.counters.upload_bytes += force_field_buffer.size();
            }
            let observer_buffer = transient
                .allocate_slice::<[f32; 4]>(observers.len().max(1) as u64)
                .map_err(|error| SceneRenderError(error.to_string()))?;
            if !observers.is_empty() {
                observer_buffer
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&observers);
                self.counters.upload_bytes += observer_buffer.size();
            }
            let physics_set = DescriptorSet::new(
                self.descriptor_allocator.clone(),
                self.physics_pipeline.layout().set_layouts()[0].clone(),
                [
                    WriteDescriptorSet::buffer(0, physics.states.clone()),
                    WriteDescriptorSet::buffer(1, physics.instructions.clone()),
                    WriteDescriptorSet::buffer(2, physics.rules.clone()),
                    WriteDescriptorSet::buffer(3, event_header.clone()),
                    WriteDescriptorSet::buffer(4, event_buffer.clone()),
                    WriteDescriptorSet::buffer(5, command_buffer),
                    WriteDescriptorSet::buffer(6, physics.shapes.clone()),
                    WriteDescriptorSet::buffer(7, colliders_buffer),
                    WriteDescriptorSet::buffer(12, collider_tree_buffer),
                    WriteDescriptorSet::buffer(14, force_field_buffer),
                    WriteDescriptorSet::buffer(15, observer_buffer.clone()),
                ],
                [],
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
            if self
                .physics_contact_grid
                .as_ref()
                .is_none_or(|grid| grid.capacity < physics.source.len())
            {
                self.physics_contact_grid = Some(PhysicsContactGrid::new(
                    &self.memory_allocator,
                    physics.source.len(),
                    self.contact_hash_budget,
                )?);
            }
            let grid = self.physics_contact_grid.as_ref().unwrap();
            let contact_sets = [
                &self.physics_grid_pipeline,
                &self.physics_contact_pipeline,
                &self.physics_oversized_pipeline,
            ]
            .map(|pipeline| {
                let layout = pipeline.layout().set_layouts()[0].clone();
                let writes = [
                    WriteDescriptorSet::buffer(0, physics.states.clone()),
                    WriteDescriptorSet::buffer(3, event_header.clone()),
                    WriteDescriptorSet::buffer(6, physics.shapes.clone()),
                    WriteDescriptorSet::buffer(8, grid.counts.clone()),
                    WriteDescriptorSet::buffer(9, grid.slots.clone()),
                    WriteDescriptorSet::buffer(10, grid.fallback.clone()),
                    WriteDescriptorSet::buffer(11, grid.snapshot.clone()),
                    WriteDescriptorSet::buffer(
                        13,
                        grid.oversized_dispatch.clone(),
                    ),
                    WriteDescriptorSet::buffer(14, grid.oversized.clone()),
                    WriteDescriptorSet::buffer(15, observer_buffer.clone()),
                ]
                .into_iter()
                .filter(|write| {
                    layout.bindings().contains_key(&write.binding())
                });
                DescriptorSet::new(
                    self.descriptor_allocator.clone(),
                    layout.clone(),
                    writes,
                    [],
                )
                .map(|set| (pipeline.clone(), set))
                .map_err(|error| SceneRenderError(error.to_string()))
            });
            let [grid_set, contact_set, oversized_set] = contact_sets;
            let contact_sets = [grid_set?, contact_set?, oversized_set?];
            // A custom shader's layout holds only the bindings it uses.
            let condition_sets = condition_pipelines
                .iter()
                .map(|(pipeline, shader)| {
                    let layout = pipeline.layout().set_layouts()[0].clone();
                    let params = transient
                        .allocate_slice::<[f32; 4]>(
                            shader.params.len().max(1) as u64
                        )
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                    if !shader.params.is_empty() {
                        params
                            .write()
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?
                            .copy_from_slice(&shader.params);
                        self.counters.upload_bytes += params.size();
                    }
                    let writes = [
                        WriteDescriptorSet::buffer(0, physics.states.clone()),
                        WriteDescriptorSet::buffer(3, event_header.clone()),
                        WriteDescriptorSet::buffer(4, event_buffer.clone()),
                        WriteDescriptorSet::buffer(14, params),
                    ]
                    .into_iter()
                    .filter(|write| {
                        layout.bindings().contains_key(&write.binding())
                    });
                    DescriptorSet::new(
                        self.descriptor_allocator.clone(),
                        layout.clone(),
                        writes,
                        [],
                    )
                    .map(|set| (pipeline.clone(), set))
                    .map_err(|error| SceneRenderError(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let state_readback = (!readback.is_empty())
                .then(|| {
                    transient
                        .allocate_slice::<GpuBodyState>(readback.len() as u64)
                })
                .transpose()
                .map_err(|error| SceneRenderError(error.to_string()))?;
            Some((
                physics_set,
                event_header,
                event_buffer,
                state_readback,
                condition_sets,
                contact_sets,
                (hash_set, tick_hashes),
            ))
        } else {
            None
        };

        let recording_start = std::time::Instant::now();
        self.preparation_time =
            recording_start.duration_since(preparation_start);
        let mut commands = AutoCommandBufferBuilder::primary(
            self.command_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        // A `Cell` so the recording closures can count too.
        let recorded = Cell::new(RenderCounters::default());
        let labels = self
            .queue
            .device()
            .instance()
            .enabled_extensions()
            .ext_debug_utils;
        if labels {
            let _ = commands.begin_debug_utils_label(DebugUtilsLabel {
                label_name: "SceneRenderer::render".to_string(),
                ..Default::default()
            });
        }
        let timestamps =
            self.frame_contexts[self.frame_index].timestamps.clone();
        if let Some(pool) = &timestamps {
            // Safety: the context's previous frame has completed, and every
            // query is reset here before this frame writes it.
            unsafe {
                commands
                    .reset_query_pool(pool.clone(), 0..pool.query_count())
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
        }
        let mut passes = PassRecorder {
            labels,
            timestamps,
            passes: Vec::new(),
        };
        if let (Some(params), Some(bake)) = (sky, &mut self.sky_bake) {
            bake.record(&mut commands, params)?;
        }
        let uploads =
            !self.pending_texture_uploads.is_empty() || carry.is_some();
        if uploads {
            passes.begin(&mut commands, FramePass::Uploads)?;
        }
        // ponytail: an error between here and submit loses these copies and
        // leaves their images unwritten; re-queue them if that ever shows up.
        for (staging, image) in self.pending_texture_uploads.drain(..) {
            self.counters.upload_bytes += staging.size();
            commands
                .copy_buffer_to_image(CopyBufferToImageInfo::buffer_image(
                    staging,
                    image.clone(),
                ))
                .map_err(|error| SceneRenderError(error.to_string()))?;
            record_mip_chain(&mut commands, &image)?;
        }
        if let Some((old_states, regions)) = carry {
            commands
                .copy_buffer(CopyBufferInfo {
                    regions: regions.into(),
                    ..CopyBufferInfo::buffers(
                        old_states,
                        physics.states.clone(),
                    )
                })
                .map_err(|error| SceneRenderError(error.to_string()))?;
        }
        if uploads {
            passes.end(&mut commands)?;
        }
        if physics_ran {
            passes.begin(&mut commands, FramePass::Physics)?;
            let resources = physics_resources.as_ref().unwrap();
            let groups = physics.source.len().div_ceil(256) as u32;
            for step in 0..steps {
                let tick = first_tick + step;
                let push = PhysicsPushConstants {
                    dt: render_world.fixed_delta_seconds,
                    // Simulated time at the end of this tick, not the
                    // frame clock, so rules and cooldowns do not depend on
                    // frame timing.
                    elapsed: (tick as f64
                        * f64::from(render_world.fixed_delta_seconds))
                        as f32,
                    body_count: physics.source.len() as u32,
                    event_capacity: event_capacity.unwrap() as u32,
                    tick_low: tick as u32,
                    tick_high: (tick >> 32) as u32,
                    gravity_x: render_world.physics_gravity[0],
                    gravity_y: render_world.physics_gravity[1],
                    gravity_z: render_world.physics_gravity[2],
                    command_first: command_ranges[step as usize].0,
                    command_count: command_ranges[step as usize].1,
                    collider_count: colliders.len() as u32,
                    field_count: force_fields.len() as u32,
                    lock_mask: render_world.physics_lock_mask,
                    observer_count: observers.len() as u32,
                    lod_full_squared: lod.full_radius * lod.full_radius,
                    lod_reduced_squared: lod.reduced_radius
                        * lod.reduced_radius,
                    lod_stride: lod.reduced_stride.max(1),
                    grid_cell_size: physics.grid_cell_size,
                };
                let grid = self.physics_contact_grid.as_ref().unwrap();
                // Contacts between bodies (several rounds, each rebuilding
                // the grid), the built-in step, then each custom condition
                // shader.
                for _ in 0..CONTACT_ROUNDS {
                    commands
                        .fill_buffer(grid.counts.clone(), 0)
                        .map_err(|error| SceneRenderError(error.to_string()))?
                        .fill_buffer(grid.fallback.clone().slice(0..1), 0)
                        .map_err(|error| SceneRenderError(error.to_string()))?
                        .fill_buffer(grid.oversized.clone().slice(0..1), 0)
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                    // Grid, contacts, then the oversized bodies through the
                    // workgroup count the contact pass wrote.
                    for (index, (pipeline, set)) in
                        resources.5.iter().enumerate()
                    {
                        commands
                            .bind_pipeline_compute(pipeline.clone())
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?
                            .bind_descriptor_sets(
                                PipelineBindPoint::Compute,
                                pipeline.layout().clone(),
                                0,
                                set.clone(),
                            )
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?
                            .push_constants(pipeline.layout().clone(), 0, push)
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?;
                        count_work(&recorded, 0, 1, 0);
                        self.counters.physics_dispatches += 1;
                        let dispatch = if index == 2 {
                            unsafe {
                                commands.dispatch_indirect(
                                    grid.oversized_dispatch.clone(),
                                )
                            }
                        } else {
                            unsafe { commands.dispatch([groups, 1, 1]) }
                        };
                        dispatch.map_err(|error| {
                            SceneRenderError(error.to_string())
                        })?;
                    }
                }
                let passes =
                    std::iter::once((&self.physics_pipeline, &resources.0))
                        .chain(
                            resources
                                .4
                                .iter()
                                .map(|(pipeline, set)| (pipeline, set)),
                        );
                for (pipeline, set) in passes {
                    commands
                        .bind_pipeline_compute(pipeline.clone())
                        .map_err(|error| SceneRenderError(error.to_string()))?
                        .bind_descriptor_sets(
                            PipelineBindPoint::Compute,
                            pipeline.layout().clone(),
                            0,
                            set.clone(),
                        )
                        .map_err(|error| SceneRenderError(error.to_string()))?
                        .push_constants(pipeline.layout().clone(), 0, push)
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                    count_work(&recorded, 0, 1, 0);
                    self.counters.physics_dispatches += 1;
                    unsafe {
                        commands.dispatch([groups, 1, 1]).map_err(|error| {
                            SceneRenderError(error.to_string())
                        })?;
                    }
                }
                let hash = &self.physics_hash_pipeline;
                commands
                    .bind_pipeline_compute(hash.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .bind_descriptor_sets(
                        PipelineBindPoint::Compute,
                        hash.layout().clone(),
                        0,
                        resources.6 .0.clone(),
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .push_constants(hash.layout().clone(), 0, push)
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                count_work(&recorded, 0, 1, 0);
                self.counters.physics_dispatches += 1;
                unsafe {
                    commands
                        .dispatch([groups, 1, 1])
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                }
            }
            self.last_physics_tick = last_tick;
            if let Some(target) = &resources.3 {
                commands
                    .copy_buffer(CopyBufferInfo {
                        regions: readback.to_vec().into(),
                        ..CopyBufferInfo::buffers(
                            physics.states.clone(),
                            target.clone(),
                        )
                    })
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
            passes.end(&mut commands)?;
        } else if new_ticks > 0 {
            // Disabled time must not be simulated later when physics resumes.
            self.last_physics_tick = render_world.physics_tick;
        }
        if physics_only {
            self.upscale_to = None;
            if labels {
                // Safety: closes the frame's outer label opened above.
                let _ = unsafe { commands.end_debug_utils_label() };
            }
            let command_buffer = commands
                .build()
                .map_err(|error| SceneRenderError(error.to_string()))?;
            self.frame_serial += 1;
            let future = before
                .then_execute(self.queue.clone(), command_buffer)
                .map_err(|error| SceneRenderError(error.to_string()))?;
            #[allow(clippy::arc_with_non_send_sync)]
            let fence = Arc::new(future.boxed().then_signal_fence());
            self.frame_contexts[self.frame_index].fence = Some(fence.clone());
            self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
            if physics_ran {
                let (_, header, events, states, _, _, hashes) =
                    physics_resources.unwrap();
                self.pending_physics.push(PendingPhysicsReadback {
                    fence: fence.clone(),
                    submitted_frame: self.frame_serial,
                    header,
                    events,
                    states: states.map(|buffer| {
                        (buffer, last_tick, readback_full.clone())
                    }),
                    hashes: (hashes.1, first_tick, last_tick),
                });
            }
            return Ok(fence.boxed());
        }
        let cull_count = render_instances.cull_source.len() as u32;
        let dispatch_cull = |commands: &mut AutoCommandBufferBuilder<
            PrimaryAutoCommandBuffer,
        >,
                             set: &Arc<DescriptorSet>,
                             phase: CullPhase|
         -> Result<(), SceneRenderError> {
            commands
                .bind_pipeline_compute(self.cull_pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Compute,
                    self.cull_pipeline.layout().clone(),
                    0,
                    set.clone(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .push_constants(
                    self.cull_pipeline.layout().clone(),
                    0,
                    CullPushConstants {
                        clip: clip.into(),
                        viewport: [
                            viewport.offset[0] as f32,
                            viewport.offset[1] as f32,
                            viewport.extent[0] as f32,
                            viewport.extent[1] as f32,
                        ],
                        lod: lod_camera,
                        info: [
                            cull_count,
                            phase as u32,
                            u32::from(!frustum),
                            0,
                        ],
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            count_work(&recorded, 0, 1, 0);
            unsafe {
                commands
                    .dispatch([cull_count.div_ceil(256).max(1), 1, 1])
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
            Ok(())
        };
        self.record_compute(
            &mut commands,
            self.skinning_pipeline.as_ref(),
            &skin_steps,
        )?;
        self.record_compute(
            &mut commands,
            self.gpu_particle_pipeline.as_ref(),
            &particle_steps,
        )?;
        if let Some((_, _, set)) = cull_sets.first() {
            passes.begin(&mut commands, FramePass::Culling)?;
            let phase = if occlusion {
                CullPhase::Early
            } else {
                CullPhase::Frustum
            };
            dispatch_cull(&mut commands, set, phase)?;
            passes.end(&mut commands)?;
        }
        let scene_viewport = Viewport {
            offset: [viewport.offset[0] as f32, viewport.offset[1] as f32],
            extent: [viewport.extent[0] as f32, viewport.extent[1] as f32],
            depth_range: 0.0..=1.0,
        };
        let scene_scissor = Scissor {
            offset: visible.offset,
            extent: visible.extent,
        };
        // The shadow map is cleared even without a shadowed light, so the
        // main pass always samples initialized depth.
        passes.begin(&mut commands, FramePass::Shadow)?;
        commands
            .begin_render_pass(
                RenderPassBeginInfo {
                    clear_values: vec![Some(1.0_f32.into())],
                    ..RenderPassBeginInfo::framebuffer(
                        self.shadow_framebuffer.clone(),
                    )
                },
                SubpassBeginInfo {
                    contents: SubpassContents::Inline,
                    ..Default::default()
                },
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
        let tile_size = shadow_size / tiles;
        for (index, light_view_projection) in cascades.iter().enumerate() {
            // ponytail: masked casters cast as fully opaque and blended ones
            // not at all; add an alpha-tested shadow shader when scenes need
            // foliage shadows.
            // ponytail: every cascade draws every caster; cull per cascade
            // when shadow draws show up in profiles (Eco could use 2).
            let offset = [
                (index as u32 % tiles) * tile_size,
                (index as u32 / tiles) * tile_size,
            ];
            commands
                .bind_pipeline_graphics(self.shadow_pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Graphics,
                    active.pipeline.layout().clone(),
                    0,
                    graphics_set.clone(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .push_constants(
                    active.pipeline.layout().clone(),
                    0,
                    CameraUniform {
                        view_projection: (*light_view_projection).into(),
                        ..camera
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_viewport(
                    0,
                    [Viewport {
                        offset: [offset[0] as f32, offset[1] as f32],
                        extent: [tile_size as f32; 2],
                        depth_range: 0.0..=1.0,
                    }]
                    .into_iter()
                    .collect(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_scissor(
                    0,
                    [Scissor {
                        offset,
                        extent: [tile_size; 2],
                    }]
                    .into_iter()
                    .collect(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            for batch in &render_instances.batches {
                let Some(mesh) = self.prepared_meshes.get(&batch.mesh_key)
                else {
                    continue;
                };
                commands
                    .bind_vertex_buffers(0, mesh.vertices.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .bind_index_buffer(mesh.indices.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                count_work(
                    &recorded,
                    1,
                    0,
                    mesh.indices.len() / 3 * u64::from(batch.instance_count),
                );
                unsafe {
                    commands
                        .draw_indexed(
                            mesh.indices.len() as u32,
                            batch.instance_count,
                            0,
                            0,
                            batch.first_instance,
                        )
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                }
            }
        }
        commands
            .end_render_pass(Default::default())
            .map_err(|error| SceneRenderError(error.to_string()))?;
        passes.end(&mut commands)?;
        let surface = surface_pipelines(
            &mut self.surface_pipelines,
            &self.queue,
            &active,
            samples,
            &self.prepared_materials,
            &render_world.fog_shader,
        );
        // `pipeline`, or the material's surface shader pipeline for `pass`.
        let surface_pipeline =
            |material: Handle<MaterialAsset>,
             pass: Option<usize>,
             pipeline: &Arc<GraphicsPipeline>| {
                pass.and_then(|pass| {
                    surface
                        .get(&material.key())
                        .map(|pipelines| &pipelines[pass])
                })
                .unwrap_or(pipeline)
                .clone()
            };
        let texture_set = |material: Handle<MaterialAsset>| {
            self.prepared_materials
                .get(&material.key())
                .map_or(&self.white_material, |prepared| &prepared.set)
                .clone()
        };
        let indirect = |cull: Option<&GpuCullSet>, group: usize| {
            cull.map(|(_, draw_commands, _)| {
                draw_commands.clone().slice(group as u64..group as u64 + 1)
            })
        };
        // Binds the scene state and draws the opaque batches, or only the
        // listed ones, with `pipeline` from `cull`'s outputs, or from the CPU
        // list without one. Returns the bound material set.
        let draw_batches = |commands: &mut AutoCommandBufferBuilder<
            PrimaryAutoCommandBuffer,
        >,
                            cull: Option<&GpuCullSet>,
                            pipeline: &Arc<GraphicsPipeline>,
                            only: Option<&[usize]>|
         -> Result<
            Option<Arc<DescriptorSet>>,
            SceneRenderError,
        > {
            commands
                .bind_pipeline_graphics(pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Graphics,
                    active.pipeline.layout().clone(),
                    0,
                    graphics_set.clone(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Graphics,
                    active.pipeline.layout().clone(),
                    2,
                    shadow_set.clone(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .push_constants(active.pipeline.layout().clone(), 0, camera)
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_viewport(0, [scene_viewport.clone()].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_scissor(0, [scene_scissor].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            let mut bound_texture: Option<Arc<DescriptorSet>> = None;
            // Surface shaders replace the opaque and overlay pipelines; the
            // depth prepass keeps the built-in one.
            let pass = if Arc::ptr_eq(pipeline, &active.pipeline) {
                Some(0)
            } else if Arc::ptr_eq(pipeline, &active.reflection_pipeline) {
                Some(2)
            } else {
                None
            };
            let mut bound_pipeline = pipeline.clone();
            let instance_total = render_instances.cull_source.len() as u32;
            for (group, batch) in render_instances.batches.iter().enumerate() {
                if only.is_some_and(|only| !only.contains(&group)) {
                    continue;
                }
                for draw in batch.first_draw..batch.first_draw + batch.levels {
                    let level = draw - batch.first_draw;
                    // The GPU list reserves each batch's full range per level; the
                    // CPU list holds only the visible instances.
                    let (list, first, count) = match (cull, visibility) {
                        (Some((list, ..)), _) => (
                            list,
                            batch.first_instance + level * instance_total,
                            batch.instance_count,
                        ),
                        (None, Some(visibility)) => {
                            let (first, count) =
                                visibility.batches[draw as usize];
                            (&visibility.list, first, count)
                        }
                        (None, None) => {
                            unreachable!("every path prepares a list")
                        }
                    };
                    if count == 0 {
                        continue;
                    }
                    let Some(mesh) = self
                        .prepared_meshes
                        .get(&render_instances.draw_meshes[draw as usize])
                    else {
                        continue;
                    };
                    let wanted =
                        surface_pipeline(batch.material, pass, pipeline);
                    if !Arc::ptr_eq(&wanted, &bound_pipeline) {
                        commands
                            .bind_pipeline_graphics(wanted.clone())
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?;
                        bound_pipeline = wanted;
                    }
                    let set = texture_set(batch.material);
                    if !bound_texture
                        .as_ref()
                        .is_some_and(|bound| Arc::ptr_eq(bound, &set))
                    {
                        commands
                            .bind_descriptor_sets(
                                PipelineBindPoint::Graphics,
                                active.pipeline.layout().clone(),
                                1,
                                set.clone(),
                            )
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?;
                        bound_texture = Some(set);
                    }
                    draw_instances(
                        commands,
                        mesh,
                        list,
                        first,
                        count,
                        indirect(cull, draw as usize),
                        &recorded,
                    )?;
                }
            }
            Ok(bound_texture)
        };
        if let Some(params) = occlusion_params {
            // Occlusion frames only have last frame's visible set culled
            // this early.
            // ponytail: objects that just came into view miss the prepass
            // for one frame and neither cast nor receive occlusion; cull a
            // frustum set for the prepass if that flicker shows.
            commands
                .begin_render_pass(
                    RenderPassBeginInfo {
                        clear_values: vec![Some(1.0_f32.into())],
                        ..RenderPassBeginInfo::framebuffer(
                            self.depth_prepass_framebuffer.clone(),
                        )
                    },
                    SubpassBeginInfo {
                        contents: SubpassContents::Inline,
                        ..Default::default()
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            passes.begin(&mut commands, FramePass::DepthPrepass)?;
            draw_batches(
                &mut commands,
                early_cull.or(gpu_cull),
                &self.depth_prepass_pipeline,
                None,
            )?;
            passes.end(&mut commands)?;
            commands
                .end_render_pass(Default::default())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            passes.begin(&mut commands, FramePass::AmbientOcclusion)?;
            let dispatches = self.occlusion.record(
                &mut commands,
                &self.descriptor_allocator,
                &self.post_pipelines,
                &self.depth,
                (self.scene_color.clone(), self.scene_color_sampler.clone()),
                params,
            )?;
            count_work(&recorded, 0, dispatches, 0);
            passes.end(&mut commands)?;
        }
        let background = Some(render_world.background_color.into());
        if let Some(early) = early_cull {
            // Last frame's visible opaque set lays down depth for the
            // pyramid; the late pass continues its HDR and depth.
            commands
                .begin_render_pass(
                    RenderPassBeginInfo {
                        render_pass: active.early_render_pass.clone(),
                        clear_values: clears(background, Some(1.0_f32.into())),
                        ..RenderPassBeginInfo::framebuffer(framebuffer.clone())
                    },
                    SubpassBeginInfo {
                        contents: SubpassContents::Inline,
                        ..Default::default()
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            passes.begin(&mut commands, FramePass::Scene)?;
            draw_batches(&mut commands, Some(early), &active.pipeline, None)?;
            passes.end(&mut commands)?;
            commands
                .next_subpass(Default::default(), SubpassBeginInfo::default())
                .map_err(|error| SceneRenderError(error.to_string()))?
                // The NVIDIA driver loses the device (Xid 69) when a
                // multisampled pipeline is still bound as a single-sample
                // subpass ends. Binding any subpass-1 pipeline avoids it.
                .bind_pipeline_graphics(active.tonemap_pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .end_render_pass(Default::default())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            passes.begin(&mut commands, FramePass::DepthPyramid)?;
            for (level, (set, size)) in
                self.depth_pyramid.mips.iter().enumerate()
            {
                let pipeline = if level == 0 {
                    &self.depth_pyramid_copy_pipeline
                } else {
                    &self.depth_pyramid_reduce_pipeline
                };
                commands
                    .bind_pipeline_compute(pipeline.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .bind_descriptor_sets(
                        PipelineBindPoint::Compute,
                        pipeline.layout().clone(),
                        0,
                        set.clone(),
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                count_work(&recorded, 0, 1, 0);
                unsafe {
                    commands
                        .dispatch([size[0].div_ceil(8), size[1].div_ceil(8), 1])
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                }
            }
            passes.end(&mut commands)?;
            passes.begin(&mut commands, FramePass::OcclusionCulling)?;
            dispatch_cull(
                &mut commands,
                &gpu_cull.unwrap().2,
                CullPhase::Late,
            )?;
            passes.end(&mut commands)?;
        }
        // Frames that split for the scene color copy, or end the pass for
        // bloom, keep HDR and depth after the draws and continue them in the
        // late pass.
        let (main_pass, clear_values) = match (occlusion, split || post) {
            (true, false) => {
                (active.late_render_pass.clone(), clears(None, None))
            }
            (true, true) => {
                (active.middle_render_pass.clone(), clears(None, None))
            }
            (false, false) => (
                active.render_pass.clone(),
                clears(background, Some(1.0_f32.into())),
            ),
            (false, true) => (
                active.early_render_pass.clone(),
                clears(background, Some(1.0_f32.into())),
            ),
        };
        commands
            .begin_render_pass(
                RenderPassBeginInfo {
                    render_pass: main_pass,
                    clear_values,
                    ..RenderPassBeginInfo::framebuffer(framebuffer.clone())
                },
                SubpassBeginInfo {
                    contents: SubpassContents::Inline,
                    ..Default::default()
                },
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
        // Labels stay inside one subpass so none spans a subpass boundary.
        let scene_pass = if occlusion {
            FramePass::LateScene
        } else {
            FramePass::Scene
        };
        passes.begin(&mut commands, scene_pass)?;
        let mut bound_texture =
            draw_batches(&mut commands, gpu_cull, &active.pipeline, None)?;
        if fog.is_some_and(|fog| fog.sky_affect > 0.0)
            || !fog_volumes.is_empty()
            || sky.is_some()
            || environment_background
            || sky_shader.is_some()
        {
            // Same layout as the scene pipelines, so their sets and camera
            // stay bound.
            commands
                .bind_pipeline_graphics(
                    sky_shader.unwrap_or_else(|| active.sky_pipeline.clone()),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            count_work(&recorded, 1, 0, 1);
            unsafe {
                commands
                    .draw(3, 1, 0, 0)
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
        }
        if split {
            passes.end(&mut commands)?;
            commands
                .next_subpass(Default::default(), SubpassBeginInfo::default())
                .map_err(|error| SceneRenderError(error.to_string()))?
                // See the early pass: the NVIDIA driver needs a subpass-1
                // pipeline bound when the pass ends.
                .bind_pipeline_graphics(active.tonemap_pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .end_render_pass(Default::default())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            passes.begin(&mut commands, FramePass::SceneColor)?;
            commands
                .copy_image(CopyImageInfo::images(
                    self.hdr.image().clone(),
                    self.scene_color.image().clone(),
                ))
                .map_err(|error| SceneRenderError(error.to_string()))?;
            record_mip_chain(&mut commands, self.scene_color.image())?;
            if reflections {
                // Mip 0 of the pyramid is the depth the reflection march reads.
                let (set, size) = &self.depth_pyramid.mips[0];
                let pipeline = &self.depth_pyramid_copy_pipeline;
                commands
                    .bind_pipeline_compute(pipeline.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .bind_descriptor_sets(
                        PipelineBindPoint::Compute,
                        pipeline.layout().clone(),
                        0,
                        set.clone(),
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                count_work(&recorded, 0, 1, 0);
                unsafe {
                    commands
                        .dispatch([size[0].div_ceil(8), size[1].div_ceil(8), 1])
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                }
            }
            passes.end(&mut commands)?;
            commands
                .begin_render_pass(
                    RenderPassBeginInfo {
                        // Bloom ends this pass again after the blended draws.
                        render_pass: if post {
                            active.middle_render_pass.clone()
                        } else {
                            active.late_render_pass.clone()
                        },
                        clear_values: clears(None, None),
                        ..RenderPassBeginInfo::framebuffer(framebuffer.clone())
                    },
                    SubpassBeginInfo {
                        contents: SubpassContents::Inline,
                        ..Default::default()
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            passes.begin(&mut commands, FramePass::Transparent)?;
            bound_texture = match &render_instances.glossy_batches {
                glossy if reflections && !glossy.is_empty() => draw_batches(
                    &mut commands,
                    gpu_cull,
                    &active.reflection_pipeline,
                    Some(glossy),
                )?,
                // The tone-map pipeline replaced the scene pipelines.
                _ => None,
            };
        }
        // On the GPU paths every blended instance keeps a draw whose count
        // the cull pass sets to 0 or 1.
        let (blended_list, mut blended) = match (gpu_cull, visibility) {
            (Some((list, ..)), _) => (list, render_instances.blended.clone()),
            (None, Some(visibility)) => {
                (&visibility.list, visibility.blended.clone())
            }
            (None, None) => unreachable!("every path prepares a list"),
        };
        let blended_base = render_instances
            .blended
            .first()
            .map_or(0, |item| item.instance as usize);
        if !blended.is_empty() {
            sort_back_to_front(&mut blended, eye, forward);
            commands
                .bind_pipeline_graphics(active.blend_pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            let mut bound_pipeline = active.blend_pipeline.clone();
            for item in blended {
                let Some(mesh) = self.prepared_meshes.get(&item.mesh_key)
                else {
                    continue;
                };
                let wanted = surface_pipeline(
                    item.material,
                    Some(1),
                    &active.blend_pipeline,
                );
                if !Arc::ptr_eq(&wanted, &bound_pipeline) {
                    commands
                        .bind_pipeline_graphics(wanted.clone())
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                    bound_pipeline = wanted;
                }
                let set = texture_set(item.material);
                if !bound_texture
                    .as_ref()
                    .is_some_and(|bound| Arc::ptr_eq(bound, &set))
                {
                    commands
                        .bind_descriptor_sets(
                            PipelineBindPoint::Graphics,
                            active.blend_pipeline.layout().clone(),
                            1,
                            set.clone(),
                        )
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                    bound_texture = Some(set);
                }
                let group = gpu_cull.is_some().then(|| {
                    render_instances.draw_meshes.len() + item.instance as usize
                        - blended_base
                });
                draw_instances(
                    &mut commands,
                    mesh,
                    blended_list,
                    item.instance,
                    1,
                    group.and_then(|group| indirect(gpu_cull, group)),
                    &recorded,
                )?;
            }
        }
        let mut particles_drawn = 0;
        if !render_world.particles.is_empty() {
            let (vertices, draws) = particle_draws(
                &render_world.particles,
                eye,
                forward,
                particle_share,
            );
            particles_drawn += vertices.len();
            let upload = self.frame_contexts[self.frame_index]
                .transient
                .allocate_slice::<ParticleVertex>(vertices.len() as u64)
                .map_err(|error| SceneRenderError(error.to_string()))?;
            upload
                .write()
                .map_err(|error| SceneRenderError(error.to_string()))?
                .copy_from_slice(&vertices);
            self.counters.upload_bytes += upload.size();
            commands
                .set_viewport(0, [scene_viewport.clone()].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_scissor(0, [scene_scissor].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_vertex_buffers(0, upload)
                .map_err(|error| SceneRenderError(error.to_string()))?;
            for (additive, sprite, first, count, entity) in draws {
                let pipeline = entity
                    .and_then(|entity| particle_shaders.get(&entity))
                    .unwrap_or(&active.particle_pipelines)
                    [usize::from(additive)]
                .clone();
                commands
                    .bind_pipeline_graphics(pipeline.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .push_constants(
                        pipeline.layout().clone(),
                        0,
                        particle_vertex_shader::Particles {
                            view_projection: camera.view_projection,
                            eye: camera.eye,
                            right: [
                                camera_right[0],
                                camera_right[1],
                                camera_right[2],
                                0.0,
                            ],
                            up: [camera_up[0], camera_up[1], camera_up[2], 0.0],
                            params: [
                                sprite,
                                render_world.elapsed_seconds.to_bits(),
                                0,
                                0,
                            ],
                        },
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                count_work(&recorded, 1, 0, u64::from(count) * 2);
                unsafe {
                    commands
                        .draw(6, count, 0, first)
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                }
            }
        }
        for batch in &render_world.gpu_particles {
            let Some(state) = self.gpu_particles.get(&batch.entity) else {
                continue;
            };
            // Each slot owns a run of billboards (its head, then trail
            // points); a prefix of slots keeps whole trails.
            let slots = batch.max_particles as u64;
            let kept = particles_kept(slots as usize, particle_share) as u64;
            particles_drawn += kept as usize;
            let billboards = kept * (state.billboards.len() / slots.max(1));
            let additive =
                batch.blend == crate::runtime::ParticleBlend::Additive;
            let pipeline = particle_shaders
                .get(&batch.entity)
                .unwrap_or(&active.particle_pipelines)[usize::from(additive)]
            .clone();
            commands
                .set_viewport(0, [scene_viewport.clone()].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_scissor(0, [scene_scissor].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_vertex_buffers(0, state.billboards.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_pipeline_graphics(pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .push_constants(
                    pipeline.layout().clone(),
                    0,
                    particle_vertex_shader::Particles {
                        view_projection: camera.view_projection,
                        eye: camera.eye,
                        right: [
                            camera_right[0],
                            camera_right[1],
                            camera_right[2],
                            0.0,
                        ],
                        up: [camera_up[0], camera_up[1], camera_up[2], 0.0],
                        params: [
                            batch.sprite as u32,
                            render_world.elapsed_seconds.to_bits(),
                            0,
                            0,
                        ],
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            count_work(&recorded, 1, 0, billboards * 2);
            unsafe {
                commands
                    .draw(6, billboards as u32, 0, 0)
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
        }
        self.capacity.dropped_particles = particle_total - particles_drawn;
        // Debug views show raw shader output, so they skip the curve.
        let tone = match options.debug_view {
            SceneDebugView::Lit => {
                let mut tone = render_world.tone_mapping.unwrap_or_default();
                tone.exposure *= options.exposure;
                tone
            }
            _ => ToneMapping::default(),
        };
        // The whole target is tone mapped, so area outside the viewport shows
        // the background like the old clear did.
        passes.end(&mut commands)?;
        if post {
            // Bloom samples the finished HDR outside the render pass, then
            // an empty late pass continues to tone mapping.
            commands
                .next_subpass(Default::default(), SubpassBeginInfo::default())
                .map_err(|error| SceneRenderError(error.to_string()))?
                // See the early pass: the NVIDIA driver needs a subpass-1
                // pipeline bound when the pass ends.
                .bind_pipeline_graphics(active.tonemap_pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .end_render_pass(Default::default())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            if let Some(settings) = bloom {
                passes.begin(&mut commands, FramePass::Bloom)?;
                let dispatches = self.bloom_chain.record(
                    &mut commands,
                    &self.post_pipelines,
                    &settings,
                )?;
                count_work(&recorded, 0, dispatches, 0);
                passes.end(&mut commands)?;
            }
            if let Some(settings) = auto_exposure {
                passes.begin(&mut commands, FramePass::Exposure)?;
                self.exposure.record(
                    &mut commands,
                    &self.post_pipelines,
                    &settings,
                )?;
                count_work(&recorded, 0, 1, 0);
                passes.end(&mut commands)?;
            }
            if sampled {
                passes.begin(&mut commands, FramePass::SceneColor)?;
                commands
                    .copy_image(CopyImageInfo::images(
                        self.hdr.image().clone(),
                        self.scene_color.image().clone(),
                    ))
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                if depth_of_field.is_some() || motion_blur.is_some() || taa {
                    // Mip 0 of the pyramid is the depth the lens, motion
                    // blur and TAA read.
                    let (set, size) = &self.depth_pyramid.mips[0];
                    let pipeline = &self.depth_pyramid_copy_pipeline;
                    commands
                        .bind_pipeline_compute(pipeline.clone())
                        .map_err(|error| SceneRenderError(error.to_string()))?
                        .bind_descriptor_sets(
                            PipelineBindPoint::Compute,
                            pipeline.layout().clone(),
                            0,
                            set.clone(),
                        )
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                    count_work(&recorded, 0, 1, 0);
                    unsafe {
                        commands
                            .dispatch([
                                size[0].div_ceil(8),
                                size[1].div_ceil(8),
                                1,
                            ])
                            .map_err(|error| {
                                SceneRenderError(error.to_string())
                            })?;
                    }
                }
                if taa {
                    let history = match &mut self.temporal {
                        Some(history)
                            if Arc::ptr_eq(
                                &history.source,
                                &self.scene_color,
                            ) =>
                        {
                            history
                        }
                        stale => stale.insert(TemporalHistory::new(
                            &self.memory_allocator,
                            &self.descriptor_allocator,
                            &self.post_pipelines,
                            &self.scene_color,
                            &self.depth_pyramid.view,
                        )?),
                    };
                    history.record(
                        &mut commands,
                        &self.post_pipelines,
                        reproject.into(),
                        [
                            viewport.offset[0],
                            viewport.offset[1],
                            viewport.extent[0],
                            viewport.extent[1],
                        ],
                    )?;
                    count_work(&recorded, 0, 1, 0);
                }
                passes.end(&mut commands)?;
            }
            commands
                .begin_render_pass(
                    RenderPassBeginInfo {
                        render_pass: active.late_render_pass.clone(),
                        clear_values: clears(None, None),
                        ..RenderPassBeginInfo::framebuffer(framebuffer.clone())
                    },
                    SubpassBeginInfo {
                        contents: SubpassContents::Inline,
                        ..Default::default()
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
        }
        commands
            .next_subpass(Default::default(), SubpassBeginInfo::default())
            .map_err(|error| SceneRenderError(error.to_string()))?;
        passes.begin(&mut commands, FramePass::ToneMap)?;
        commands
            .bind_pipeline_graphics(
                post_shader.unwrap_or_else(|| active.tonemap_pipeline.clone()),
            )
            .map_err(|error| SceneRenderError(error.to_string()))?
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                active.tonemap_pipeline.layout().clone(),
                0,
                self.tonemap_set.clone(),
            )
            .map_err(|error| SceneRenderError(error.to_string()))?
            .bind_descriptor_sets(
                PipelineBindPoint::Graphics,
                active.tonemap_pipeline.layout().clone(),
                1,
                DescriptorSet::new(
                    self.descriptor_allocator.clone(),
                    active.tonemap_pipeline.layout().set_layouts()[1].clone(),
                    [
                        WriteDescriptorSet::image_view_sampler(
                            0,
                            color_lut.as_ref().map_or_else(
                                || self.white_texture.0.clone(),
                                |(view, ..)| view.clone(),
                            ),
                            self.scene_color_sampler.clone(),
                        ),
                        WriteDescriptorSet::image_view_sampler(
                            1,
                            self.depth_pyramid.view.clone(),
                            self.depth_pyramid.sampler.clone(),
                        ),
                        WriteDescriptorSet::buffer(2, motion),
                    ],
                    [],
                )
                .map_err(|error| SceneRenderError(error.to_string()))?,
            )
            .map_err(|error| SceneRenderError(error.to_string()))?
            .push_constants(
                active.tonemap_pipeline.layout().clone(),
                0,
                tonemap_fragment_shader::ToneMap {
                    exposure: tone.exposure,
                    mapper: tone.mapper as u32,
                    bloom: bloom.map_or(0.0, |bloom| bloom.intensity),
                    contrast: grade.contrast,
                    inv_extent: [
                        1.0 / extent[0] as f32,
                        1.0 / extent[1] as f32,
                    ],
                    saturation: grade.saturation,
                    vignette: grade.vignette,
                    shadows: [
                        grade.shadows[0],
                        grade.shadows[1],
                        grade.shadows[2],
                        1.0,
                    ],
                    highlights: [
                        grade.highlights[0],
                        grade.highlights[1],
                        grade.highlights[2],
                        1.0,
                    ],
                    film: [
                        grade.grain,
                        grade.chromatic_aberration,
                        grade.scanlines,
                        grade.color_bleed,
                    ],
                    // Effects move with the fixed tick, so a replay or
                    // capture at the same tick looks the same.
                    tape: [
                        grade.noise_band,
                        grade.distortion,
                        render_world.physics_tick as f32
                            * render_world.fixed_delta_seconds,
                        (render_world.physics_tick % (1 << 24)) as f32,
                    ],
                    flags: u32::from(sampled)
                        | u32::from(auto_exposure.is_some()) << 1
                        | u32::from(fxaa) << 2,
                    lut: color_lut
                        .as_ref()
                        .map_or(0.0, |(_, intensity)| *intensity),
                    focus: depth_of_field
                        .map_or(0.0, |lens| lens.focus_distance),
                    blur: depth_of_field.map_or(0.0, |lens| lens.blur),
                    depth: lens_depth,
                },
            )
            .map_err(|error| SceneRenderError(error.to_string()))?
            .set_viewport(
                0,
                [Viewport {
                    offset: [0.0, 0.0],
                    extent: [extent[0] as f32, extent[1] as f32],
                    depth_range: 0.0..=1.0,
                }]
                .into_iter()
                .collect(),
            )
            .map_err(|error| SceneRenderError(error.to_string()))?
            .set_scissor(
                0,
                [Scissor {
                    offset: [0, 0],
                    extent,
                }]
                .into_iter()
                .collect(),
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
        count_work(&recorded, 1, 0, 0);
        unsafe {
            commands
                .draw(3, 1, 0, 0)
                .map_err(|error| SceneRenderError(error.to_string()))?;
        }
        passes.end(&mut commands)?;
        // Debug geometry is drawn after tone mapping in the same render pass,
        // so it keeps its exact colors and uses the scene depth, camera, and
        // viewport. The optional input is never provided by the game runner.
        if let Some(overlay) = options.debug_overlay {
            passes.begin(&mut commands, FramePass::DebugOverlay)?;
            commands
                .set_viewport(0, [scene_viewport].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_scissor(0, [scene_scissor].into_iter().collect())
                .map_err(|error| SceneRenderError(error.to_string()))?;
            for (on_top, pipeline) in [
                (false, active.debug_pipeline.clone()),
                (true, active.debug_on_top_pipeline.clone()),
            ] {
                let vertices = overlay
                    .lines
                    .iter()
                    .filter(|line| line.on_top == on_top)
                    .flat_map(debug_line_vertices)
                    .collect::<Vec<_>>();
                if vertices.is_empty() {
                    continue;
                }
                let upload = self.frame_contexts[self.frame_index]
                    .transient
                    .allocate_slice::<DebugVertex>(vertices.len() as u64)
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                upload
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&vertices);
                self.counters.upload_bytes += upload.size();
                let vertices = upload;
                commands
                    .bind_pipeline_graphics(pipeline.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .push_constants(
                        pipeline.layout().clone(),
                        0,
                        DebugPushConstants {
                            view_projection: camera.view_projection,
                            viewport_size: [
                                viewport.extent[0] as f32,
                                viewport.extent[1] as f32,
                            ],
                            _padding: [0.0; 2],
                        },
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .bind_vertex_buffers(0, vertices.clone())
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                count_work(&recorded, 1, 0, 0);
                unsafe {
                    commands
                        .draw(vertices.len() as u32, 1, 0, 0)
                        .map_err(|error| SceneRenderError(error.to_string()))?;
                }
            }
            passes.end(&mut commands)?;
        }
        commands
            .end_render_pass(Default::default())
            .map_err(|error| SceneRenderError(error.to_string()))?;
        let upscale = self.upscale_to.take();
        let sharp = upscale.as_ref().is_some_and(|(_, region)| {
            region.is_none()
                && !self.upscale_nearest
                && self.upscale_sharpness > 0.0
        });
        if let Some((upscaled, _)) = upscale.as_ref().filter(|_| sharp) {
            let format = upscaled.format();
            let (render_pass, pipeline) = match &self.sharp_upscale {
                Some((pass, pipeline))
                    if pass.attachments()[0].format == format =>
                {
                    (pass.clone(), pipeline.clone())
                }
                _ => {
                    let built = create_sharp_upscale(&self.queue, format)?;
                    self.sharp_upscale = Some(built.clone());
                    built
                }
            };
            // ponytail: a framebuffer per frame; cache per target view if
            // its creation shows up in recording time.
            let framebuffer = Framebuffer::new(
                render_pass,
                FramebufferCreateInfo {
                    attachments: vec![upscaled.clone()],
                    ..Default::default()
                },
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
            let [width, height, _] = upscaled.image().extent();
            let set = DescriptorSet::new(
                self.descriptor_allocator.clone(),
                pipeline.layout().set_layouts()[0].clone(),
                [WriteDescriptorSet::image_view_sampler(
                    0,
                    target.clone(),
                    self.scene_color_sampler.clone(),
                )],
                [],
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
            commands
                .begin_render_pass(
                    RenderPassBeginInfo {
                        clear_values: vec![None],
                        ..RenderPassBeginInfo::framebuffer(framebuffer)
                    },
                    SubpassBeginInfo {
                        contents: SubpassContents::Inline,
                        ..Default::default()
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_pipeline_graphics(pipeline.clone())
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_viewport(
                    0,
                    [Viewport {
                        offset: [0.0, 0.0],
                        extent: [width as f32, height as f32],
                        depth_range: 0.0..=1.0,
                    }]
                    .into_iter()
                    .collect(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .set_scissor(
                    0,
                    [Scissor {
                        offset: [0, 0],
                        extent: [width, height],
                    }]
                    .into_iter()
                    .collect(),
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Graphics,
                    pipeline.layout().clone(),
                    0,
                    set,
                )
                .map_err(|error| SceneRenderError(error.to_string()))?
                .push_constants(
                    pipeline.layout().clone(),
                    0,
                    sharp_upscale_shader::Params {
                        inv_target: [1.0 / width as f32, 1.0 / height as f32],
                        sharpness: self.upscale_sharpness,
                    },
                )
                .map_err(|error| SceneRenderError(error.to_string()))?;
            unsafe {
                commands
                    .draw(3, 1, 0, 0)
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
            commands
                .end_render_pass(Default::default())
                .map_err(|error| SceneRenderError(error.to_string()))?;
        } else if let Some((upscaled, region)) = upscale {
            let mut info = BlitImageInfo {
                filter: if self.upscale_nearest {
                    Filter::Nearest
                } else {
                    Filter::Linear
                },
                ..BlitImageInfo::images(
                    target.image().clone(),
                    upscaled.image().clone(),
                )
            };
            if let Some(region) = region {
                let corners = |area: SceneViewport| {
                    let [x, y] = area.offset;
                    let [w, h] = area.extent;
                    [[x, y, 0], [x + w, y + h, 1]]
                };
                info.regions = [ImageBlit {
                    src_subresource: target.image().subresource_layers(),
                    src_offsets: corners(visible),
                    dst_subresource: upscaled.image().subresource_layers(),
                    dst_offsets: corners(region),
                    ..Default::default()
                }]
                .into();
            }
            commands
                .blit_image(info)
                .map_err(|error| SceneRenderError(error.to_string()))?;
        }
        if labels {
            // Safety: closes the frame's outer label opened above.
            let _ = unsafe { commands.end_debug_utils_label() };
        }
        self.last_frame_passes = passes.passes.clone();
        let command_buffer = commands
            .build()
            .map_err(|error| SceneRenderError(error.to_string()))?;
        self.recording_time = recording_start.elapsed();
        let readback = std::mem::take(&mut self.readback_counters);
        self.counters = RenderCounters {
            upload_bytes: self.counters.upload_bytes,
            physics_commands: self.counters.physics_commands,
            physics_command_bytes: self.counters.physics_command_bytes,
            physics_dispatches: self.counters.physics_dispatches,
            physics_event_bytes: readback.physics_event_bytes,
            physics_state_bytes: readback.physics_state_bytes,
            physics_readback_latency_frames: readback
                .physics_readback_latency_frames,
            ..recorded.get()
        };
        self.frame_serial += 1;
        let future = before
            .then_execute(self.queue.clone(), command_buffer)
            .map_err(|error| SceneRenderError(error.to_string()))?;
        // Vulkano implements GpuFuture for Arc<FenceSignalFuture>. This
        // renderer stays on the window thread and never sends it elsewhere.
        #[allow(clippy::arc_with_non_send_sync)]
        let fence = Arc::new(future.boxed().then_signal_fence());
        self.frame_contexts[self.frame_index].fence = Some(fence.clone());
        if let Some(pool) = passes.timestamps {
            self.pending_pass_times.push(PendingPassTimes {
                fence: fence.clone(),
                pool,
                passes: passes.passes,
            });
        }
        if path.on_gpu() {
            self.pending_cull.push(PendingCullReadback {
                fence: fence.clone(),
                path,
                submitted: render_instances.cull_source.len(),
                commands: cull_sets
                    .into_iter()
                    .map(|(_, commands, _)| commands)
                    .collect(),
                timestamps: self.frame_contexts[self.frame_index]
                    .timestamps
                    .clone(),
            });
        }
        self.frame_index = (self.frame_index + 1) % FRAMES_IN_FLIGHT;
        if physics_ran {
            let (_, event_header, event_buffer, state_readback, _, _, hashes) =
                physics_resources.unwrap();
            self.pending_physics.push(PendingPhysicsReadback {
                fence: fence.clone(),
                submitted_frame: self.frame_serial,
                header: event_header,
                events: event_buffer,
                states: state_readback
                    .map(|buffer| (buffer, last_tick, readback_full.clone())),
                hashes: (hashes.1, first_tick, last_tick),
            });
        }
        Ok(fence.boxed())
    }

    /// Renders the six faces of each probe from its center into a new
    /// `probe_maps`.
    // Faces are tone mapped with Linear, so captured light clamps at 1.
    fn capture_probes(
        &mut self,
        probes: &[([f32; 3], ReflectionProbe)],
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<(), SceneRenderError> {
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        // Set first, so a failed capture is not retried every frame.
        self.probe_keys = probes.to_vec();
        if probes.is_empty() {
            return Ok(());
        }
        let size = PROBE_FACE_SIZE;
        let maps = create_probe_maps(
            &self.memory_allocator,
            6 * probes.len() as u32,
            size,
        )?;
        let centers = probes.iter().map(|(center, _)| *center);
        let faces = self.capture_faces(
            centers,
            size,
            HDR_COLOR_FORMAT,
            render_world,
            assets,
        )?;
        let mut commands = AutoCommandBufferBuilder::primary(
            self.command_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|e| error(&e))?;
        for (layer, face) in faces.into_iter().enumerate() {
            commands
                .copy_image(CopyImageInfo {
                    regions: [vulkano::command_buffer::ImageCopy {
                        src_subresource: face.subresource_layers(),
                        dst_subresource: ImageSubresourceLayers {
                            mip_level: 0,
                            array_layers: layer as u32..layer as u32 + 1,
                            ..maps.image().subresource_layers()
                        },
                        extent: [size, size, 1],
                        ..Default::default()
                    }]
                    .into(),
                    ..CopyImageInfo::images(face, maps.image().clone())
                })
                .map_err(|e| error(&e))?;
        }
        record_mip_chain(&mut commands, maps.image())?;
        self.submit_and_wait(commands)?;
        self.probe_maps = maps;
        Ok(())
    }

    /// Bakes `render_world.light_probe_grid` into `light_probes`: six faces
    /// per probe, read back and reduced to an ambient cube on the CPU.
    fn bake_light_probes(
        &mut self,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<(), SceneRenderError> {
        // Set first, so a failed bake is not retried every frame.
        self.light_probe_key = render_world.light_probe_grid;
        self.light_probe_rows.clear();
        let Some((center, grid)) = render_world.light_probe_grid else {
            self.light_probes = create_light_probe_buffer(
                &self.memory_allocator,
                vec![[0.0; 4]; LIGHT_PROBE_HEADER + 6],
            )?;
            return Ok(());
        };
        let resolution = light_probe_resolution(grid.resolution);
        let positions = light_probe_positions(center, grid.extents, resolution);
        let [rx, ry, rz] = resolution;
        let mut rows = vec![
            [center[0], center[1], center[2], positions.len() as f32],
            [
                grid.extents[0],
                grid.extents[1],
                grid.extents[2],
                grid.intensity,
            ],
            [rx as f32, ry as f32, rz as f32, 0.0],
        ];
        rows.extend(self.light_probe_cubes(
            &positions,
            render_world,
            assets,
        )?);
        self.light_probes =
            create_light_probe_buffer(&self.memory_allocator, rows.clone())?;
        self.light_probe_rows = rows;
        self.light_probe_cursor = 0;
        Ok(())
    }

    /// Recaptures the next `LIGHT_PROBES_PER_FRAME` probes of the baked
    /// grid, so its light follows moving lights and objects.
    fn relight_light_probes(
        &mut self,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<(), SceneRenderError> {
        let Some((center, grid)) = render_world.light_probe_grid else {
            return Ok(());
        };
        let positions = light_probe_positions(
            center,
            grid.extents,
            light_probe_resolution(grid.resolution),
        );
        // Nothing to patch after a failed bake.
        if self.light_probe_rows.len()
            != LIGHT_PROBE_HEADER + 6 * positions.len()
        {
            return Ok(());
        }
        let picked = (0..LIGHT_PROBES_PER_FRAME.min(positions.len()))
            .map(|step| (self.light_probe_cursor + step) % positions.len())
            .collect::<Vec<_>>();
        self.light_probe_cursor =
            (self.light_probe_cursor + picked.len()) % positions.len();
        let picked_positions = picked
            .iter()
            .map(|&probe| positions[probe])
            .collect::<Vec<_>>();
        let cubes =
            self.light_probe_cubes(&picked_positions, render_world, assets)?;
        for (&probe, cube) in picked.iter().zip(cubes.chunks(6)) {
            let first = LIGHT_PROBE_HEADER + 6 * probe;
            self.light_probe_rows[first..first + 6].copy_from_slice(cube);
        }
        // A fresh buffer, so frames still in flight keep reading the old one.
        self.light_probes = create_light_probe_buffer(
            &self.memory_allocator,
            self.light_probe_rows.clone(),
        )?;
        Ok(())
    }

    /// Captures six faces at each of `positions`, reads them back and
    /// reduces each probe to six ambient cube rows.
    fn light_probe_cubes(
        &mut self,
        positions: &[[f32; 3]],
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<Vec<[f32; 4]>, SceneRenderError> {
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        let size = LIGHT_PROBE_FACE_SIZE;
        // 32-bit floats, so the CPU reads the texels directly.
        let faces = self.capture_faces(
            positions.iter().copied(),
            size,
            Format::R32G32B32A32_SFLOAT,
            render_world,
            assets,
        )?;
        let texels = (size * size) as usize;
        let readback = Buffer::new_slice::<[f32; 4]>(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::TRANSFER_DST,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_HOST
                    | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                ..Default::default()
            },
            (texels * faces.len()) as u64,
        )
        .map_err(|e| error(&e))?;
        let mut commands = AutoCommandBufferBuilder::primary(
            self.command_allocator.clone(),
            self.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .map_err(|e| error(&e))?;
        for (index, face) in faces.into_iter().enumerate() {
            let start = (index * texels) as u64;
            commands
                .copy_image_to_buffer(
                    vulkano::command_buffer::CopyImageToBufferInfo::image_buffer(
                        face,
                        readback.clone().slice(start..start + texels as u64),
                    ),
                )
                .map_err(|e| error(&e))?;
        }
        self.submit_and_wait(commands)?;
        let texels = readback.read().map_err(|e| error(&e))?;
        Ok(texels
            .chunks(6 * (size * size) as usize)
            .flat_map(|probe| {
                ambient_cube(probe, size).map(|[r, g, b]| [r, g, b, 0.0])
            })
            .collect())
    }

    /// Prepares one compute step of every GPU particle emitter, creating
    /// buffers for new emitters and dropping those of vanished ones.
    // ponytail: steps by frame time, so particles freeze while
    // `elapsed_seconds` does (an editor scene that is stopped).
    fn prepare_gpu_particles(
        &mut self,
        render_world: &RenderWorld,
    ) -> Result<Vec<GpuParticleStep>, SceneRenderError> {
        use super::gpu_particles::{new_spawns, shader, GpuParticleState};
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        let batches = &render_world.gpu_particles;
        self.gpu_particles.retain(|entity, _| {
            batches.iter().any(|batch| batch.entity == *entity)
        });
        let dt = (render_world.elapsed_seconds - self.particle_clock)
            .clamp(0.0, 0.1);
        self.particle_clock = render_world.elapsed_seconds;
        let mut steps = Vec::new();
        if batches.is_empty() {
            return Ok(steps);
        }
        let pipeline = match &self.gpu_particle_pipeline {
            Some(pipeline) => pipeline.clone(),
            None => {
                let pipeline = create_compute_pipeline(
                    &self.queue,
                    shader::load(self.queue.device().clone()),
                )?;
                self.gpu_particle_pipeline = Some(pipeline.clone());
                pipeline
            }
        };
        let colliders = render_world
            .gpu_colliders
            .iter()
            .map(GpuColliderUpload::from)
            .collect::<Vec<_>>();
        let fields = render_world
            .gpu_force_fields
            .iter()
            .map(GpuForceFieldUpload::from)
            .collect::<Vec<_>>();
        let transient = &self.frame_contexts[self.frame_index].transient;
        let collider_buffer = transient
            .allocate_slice::<GpuColliderUpload>(colliders.len().max(1) as u64)
            .map_err(|e| error(&e))?;
        let field_buffer = transient
            .allocate_slice::<GpuForceFieldUpload>(fields.len().max(1) as u64)
            .map_err(|e| error(&e))?;
        collider_buffer.write().map_err(|e| error(&e))?[..colliders.len()]
            .copy_from_slice(&colliders);
        field_buffer.write().map_err(|e| error(&e))?[..fields.len()]
            .copy_from_slice(&fields);
        self.counters.upload_bytes +=
            collider_buffer.size() + field_buffer.size();
        for batch in batches {
            // A sub-emitter reads its parent's slots, stepped just before.
            let parent = batch.parent.and_then(|(parent, burst)| {
                let state = self.gpu_particles.get(&parent)?;
                Some((state.particles.clone(), burst, 3 + state.trail_points))
            });
            let state = match self.gpu_particles.entry(batch.entity) {
                std::collections::hash_map::Entry::Occupied(entry)
                    if entry.get().slots == batch.max_particles
                        && entry.get().trail_points == batch.trail_points =>
                {
                    entry.into_mut()
                }
                entry => {
                    let state = GpuParticleState::new(
                        &self.memory_allocator,
                        batch.max_particles,
                        batch.trail_points,
                    )?;
                    match entry {
                        std::collections::hash_map::Entry::Occupied(
                            mut entry,
                        ) => {
                            entry.insert(state);
                            entry.into_mut()
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(state)
                        }
                    }
                }
            };
            let slots = state.slots;
            let (first, count) = if parent.is_some() {
                (0, 0)
            } else {
                new_spawns(state.spawned, batch.spawned, slots)
            };
            state.spawned = batch.spawned;
            let settings = self.frame_contexts[self.frame_index]
                .transient
                .allocate_slice::<[f32; 4]>(batch.rows.len() as u64)
                .map_err(|e| error(&e))?;
            settings
                .write()
                .map_err(|e| error(&e))?
                .copy_from_slice(&batch.rows);
            self.counters.upload_bytes += settings.size();
            let set = DescriptorSet::new(
                self.descriptor_allocator.clone(),
                pipeline.layout().set_layouts()[0].clone(),
                [
                    WriteDescriptorSet::buffer(0, state.particles.clone()),
                    WriteDescriptorSet::buffer(1, state.billboards.clone()),
                    WriteDescriptorSet::buffer(2, settings.clone()),
                    WriteDescriptorSet::buffer(
                        3,
                        parent.as_ref().map_or(settings, |p| p.0.clone()),
                    ),
                    WriteDescriptorSet::buffer(7, collider_buffer.clone()),
                    WriteDescriptorSet::buffer(14, field_buffer.clone()),
                ],
                [],
            )
            .map_err(|e| error(&e))?;
            steps.push((
                set,
                shader::Step {
                    spawn: [first as u32, count, slots, batch.shape],
                    seed: [
                        batch.stream as u32,
                        (batch.stream >> 32) as u32,
                        colliders.len() as u32,
                        fields.len() as u32,
                    ],
                    time: [
                        dt,
                        render_world.elapsed_seconds,
                        parent.as_ref().map_or(0.0, |p| p.1 as f32),
                        parent.as_ref().map_or(0.0, |p| p.2 as f32),
                    ],
                },
                slots.div_ceil(64),
            ));
        }
        Ok(steps)
    }

    /// One dispatch per GPU skin job, writing its copy's prepared vertices.
    /// A copy whose cooked mesh or vertex buffer is missing is skipped.
    fn prepare_skinning(
        &mut self,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<Vec<SkinStep>, SceneRenderError> {
        use super::gpu_skinning::{shader, SkinInputs};
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        let jobs = &render_world.skin_jobs;
        self.skin_inputs
            .retain(|key, _| jobs.iter().any(|job| job.mesh.key() == *key));
        let mut steps = Vec::new();
        if jobs.is_empty() {
            return Ok(steps);
        }
        let pipeline = match &self.skinning_pipeline {
            Some(pipeline) => pipeline.clone(),
            None => {
                let pipeline = create_compute_pipeline(
                    &self.queue,
                    shader::load(self.queue.device().clone()),
                )?;
                self.skinning_pipeline = Some(pipeline.clone());
                pipeline
            }
        };
        for job in jobs {
            let (Some(mesh), Some(prepared), Some((skin, morph))) = (
                assets.meshes.get(job.base),
                self.prepared_meshes.get(&job.mesh.key()),
                assets.deformers.cooked(job.base),
            ) else {
                continue;
            };
            let revision = assets.meshes.revision(job.base).unwrap_or(0);
            let inputs = match self.skin_inputs.entry(job.mesh.key()) {
                std::collections::hash_map::Entry::Occupied(entry)
                    if entry.get().revision == revision =>
                {
                    entry.into_mut()
                }
                entry => {
                    let inputs = SkinInputs::new(
                        &self.memory_allocator,
                        revision,
                        mesh,
                        skin.as_ref(),
                        morph,
                    )?;
                    self.counters.upload_bytes += inputs.base.size()
                        + inputs.joints.size()
                        + inputs.weights.size()
                        + inputs.morph.size();
                    match entry {
                        std::collections::hash_map::Entry::Occupied(
                            mut entry,
                        ) => {
                            entry.insert(inputs);
                            entry.into_mut()
                        }
                        std::collections::hash_map::Entry::Vacant(entry) => {
                            entry.insert(inputs)
                        }
                    }
                }
            };
            // The copy is prepared from the cooked vertices, so the counts
            // match unless the cooked mesh changed under it.
            if prepared.vertices.len() != u64::from(inputs.vertex_count) {
                continue;
            }
            let transient = &self.frame_contexts[self.frame_index].transient;
            let matrices = transient
                .allocate_slice::<[[f32; 4]; 4]>(
                    job.matrices.len().max(1) as u64
                )
                .map_err(|e| error(&e))?;
            matrices.write().map_err(|e| error(&e))?[..job.matrices.len()]
                .copy_from_slice(&job.matrices);
            let targets = inputs.target_count as usize;
            let shape_weights = transient
                .allocate_slice::<f32>(targets.max(1) as u64)
                .map_err(|e| error(&e))?;
            {
                let mut write = shape_weights.write().map_err(|e| error(&e))?;
                write.fill(0.0);
                for (slot, weight) in write.iter_mut().zip(&job.morph_weights) {
                    *slot = *weight;
                }
            }
            self.counters.upload_bytes +=
                matrices.size() + shape_weights.size();
            let set = DescriptorSet::new(
                self.descriptor_allocator.clone(),
                pipeline.layout().set_layouts()[0].clone(),
                [
                    WriteDescriptorSet::buffer(0, inputs.base.clone()),
                    WriteDescriptorSet::buffer(1, prepared.vertices.clone()),
                    WriteDescriptorSet::buffer(2, inputs.joints.clone()),
                    WriteDescriptorSet::buffer(3, inputs.weights.clone()),
                    WriteDescriptorSet::buffer(4, inputs.morph.clone()),
                    WriteDescriptorSet::buffer(5, matrices),
                    WriteDescriptorSet::buffer(6, shape_weights),
                ],
                [],
            )
            .map_err(|e| error(&e))?;
            // Weights beyond the shapes are ignored, as on the CPU.
            let any_shape =
                job.morph_weights.iter().any(|&weight| weight != 0.0);
            let joints = if skin.is_some() {
                job.matrices.len()
            } else {
                0
            };
            steps.push((
                set,
                shader::Counts {
                    counts: [
                        inputs.vertex_count,
                        joints as u32,
                        targets as u32,
                        u32::from(any_shape),
                    ],
                },
                inputs.vertex_count.div_ceil(64),
            ));
        }
        Ok(steps)
    }

    /// Records compute steps of one pipeline in order: GPU skinning's and
    /// the GPU particles'.
    fn record_compute<T: vulkano::buffer::BufferContents + Copy>(
        &self,
        commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        pipeline: Option<&Arc<ComputePipeline>>,
        steps: &[(Arc<DescriptorSet>, T, u32)],
    ) -> Result<(), SceneRenderError> {
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        let Some(pipeline) = pipeline else {
            return Ok(());
        };
        for (set, constants, groups) in steps {
            commands
                .bind_pipeline_compute(pipeline.clone())
                .map_err(|e| error(&e))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Compute,
                    pipeline.layout().clone(),
                    0,
                    set.clone(),
                )
                .map_err(|e| error(&e))?
                .push_constants(pipeline.layout().clone(), 0, *constants)
                .map_err(|e| error(&e))?;
            unsafe { commands.dispatch([*groups, 1, 1]) }
                .map_err(|e| error(&e))?;
        }
        Ok(())
    }

    /// Lays out and bakes `render_world.lightmaps` into `lightmaps`, one
    /// compute dispatch and submit per slot so no submit runs long.
    fn bake_lightmaps(
        &mut self,
        key: u64,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<(), SceneRenderError> {
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        // Set first, so a failed bake is not retried every frame.
        self.lightmap_key = key;
        let layout = super::lightmap::lightmap_layout(render_world, assets);
        self.lightmaps =
            create_light_probe_buffer(&self.memory_allocator, layout.rows)?;
        let Some(lights) = self.prepared_lights.as_ref() else {
            return Ok(());
        };
        if layout.slots.is_empty() {
            return Ok(());
        }
        let triangle_count = (layout.triangles.len() / 4) as u32;
        let triangles = create_light_probe_buffer(
            &self.memory_allocator,
            layout.triangles,
        )?;
        let pipeline = create_compute_pipeline(
            &self.queue,
            super::lightmap::bake_shader::load(self.queue.device().clone()),
        )?;
        let set = DescriptorSet::new(
            self.descriptor_allocator.clone(),
            pipeline.layout().set_layouts()[0].clone(),
            [
                WriteDescriptorSet::buffer(0, self.lightmaps.clone()),
                WriteDescriptorSet::buffer(1, triangles),
                WriteDescriptorSet::buffer(2, lights.buffer.clone()),
            ],
            [],
        )
        .map_err(|e| error(&e))?;
        for (slot, texels) in layout.slots {
            let mut commands = AutoCommandBufferBuilder::primary(
                self.command_allocator.clone(),
                self.queue.queue_family_index(),
                CommandBufferUsage::OneTimeSubmit,
            )
            .map_err(|e| error(&e))?;
            commands
                .bind_pipeline_compute(pipeline.clone())
                .map_err(|e| error(&e))?
                .bind_descriptor_sets(
                    PipelineBindPoint::Compute,
                    pipeline.layout().clone(),
                    0,
                    set.clone(),
                )
                .map_err(|e| error(&e))?
                .push_constants(
                    pipeline.layout().clone(),
                    0,
                    super::lightmap::bake_shader::Bake {
                        sky: lights.ambient,
                        ground: lights.ground_ambient,
                        info: [slot, lights.count, triangle_count, 0],
                    },
                )
                .map_err(|e| error(&e))?;
            unsafe { commands.dispatch([texels.div_ceil(64), 1, 1]) }
                .map_err(|e| error(&e))?;
            self.submit_and_wait(commands)?;
        }
        Ok(())
    }

    /// Renders six 90-degree faces from each of `centers`, in
    /// `PROBE_FACES` order, into new `size` square images of `format`. A
    /// temporary renderer draws them, so this renderer's caches, GPU physics
    /// and occlusion history stay untouched. Probes, effects and history
    /// that would not match a static capture are turned off.
    // ponytail: the temporary renderer builds every pipeline again, so a
    // probe moved every frame stutters; keep one around if probes animate.
    fn capture_faces(
        &mut self,
        centers: impl IntoIterator<Item = [f32; 3]>,
        size: u32,
        format: Format,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<Vec<Arc<Image>>, SceneRenderError> {
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        let mut capturer = match self.capturer.take() {
            Some((cached_format, cached_size, capturer))
                if cached_format == format && cached_size == size =>
            {
                capturer
            }
            _ => Box::new(SceneRenderer::new(
                self.queue.clone(),
                self.memory_allocator.clone(),
                format,
                [size; 2],
            )?),
        };
        let mut world = render_world.clone();
        world.reflection_probes.clear();
        world.light_probe_grid = None;
        world.physics_enabled = false;
        world.physics_tick = 0;
        world.gpu_physics_commands.clear();
        world.gpu_physics_command_ticks.clear();
        world.tone_mapping = None;
        // The scene adds its own bloom over the reflection, and occlusion
        // at the probe's viewpoint would not match the reflected surface.
        // Fog stays: it is part of what the surroundings look like.
        world.bloom = None;
        world.auto_exposure = None;
        world.ambient_occlusion = None;
        world.depth_of_field = None;
        world.motion_blur = None;
        // Each face would blend in the face before it.
        if world.antialiasing == Antialiasing::Taa {
            world.antialiasing = Antialiasing::Off;
        }
        // Occlusion would test each face against the previous face's depth.
        world.culling = CullingMode::Frustum;
        let device = self.queue.device().clone();
        // Faces render into their own images: vulkano cannot track a
        // framebuffer view of one layer of a mipmapped array.
        let mut faces = Vec::new();
        for center in centers {
            for &basis in &PROBE_FACES {
                world.active_camera = Some(crate::runtime::ExtractedCamera {
                    entity: bevy_ecs::entity::Entity::PLACEHOLDER,
                    transform: probe_face_transform(center, basis),
                    projection: Projection::Perspective {
                        vertical_fov_radians: std::f32::consts::FRAC_PI_2,
                        near: 0.05,
                        far: 1_000.0,
                    },
                    priority: 0,
                });
                let face = Image::new(
                    self.memory_allocator.clone(),
                    ImageCreateInfo {
                        format,
                        extent: [size, size, 1],
                        usage: ImageUsage::COLOR_ATTACHMENT
                            | ImageUsage::TRANSFER_SRC,
                        ..Default::default()
                    },
                    AllocationCreateInfo::default(),
                )
                .map_err(|e| error(&e))?;
                capturer
                    .render(
                        vulkano::sync::now(device.clone()).boxed(),
                        ImageView::new_default(face.clone())
                            .map_err(|e| error(&e))?,
                        [size; 2],
                        SceneRenderOptions::game([size; 2]),
                        &world,
                        assets,
                    )?
                    .then_signal_fence_and_flush()
                    .map_err(|e| error(&e))?
                    .wait(None)
                    .map_err(|e| error(&e))?;
                faces.push(face);
            }
        }
        self.capturer = Some((format, size, capturer));
        Ok(faces)
    }

    /// Submits `commands` on the queue and blocks until they finish.
    fn submit_and_wait(
        &self,
        commands: AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    ) -> Result<(), SceneRenderError> {
        let error = |error: &dyn Display| SceneRenderError(error.to_string());
        vulkano::sync::now(self.queue.device().clone())
            .then_execute(
                self.queue.clone(),
                commands.build().map_err(|e| error(&e))?,
            )
            .map_err(|e| error(&e))?
            .then_signal_fence_and_flush()
            .map_err(|e| error(&e))?
            .wait(None)
            .map_err(|e| error(&e))
    }

    fn prepare_visible_meshes(
        &mut self,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<(), SceneRenderError> {
        let lods = lod_signature(assets);
        // A world filled by hand (tests, tools) never bumps the layout
        // revision; follow every list change for it instead.
        let revision = match render_world.renderables_layout_revision {
            0 => (0, render_world.renderables_revision),
            layout => (layout, 0),
        };
        if self.prepared_meshes_revision != revision
            || self.prepared_meshes_lods != lods
        {
            let groups = lod_groups(assets);
            self.visible_meshes = render_world
                .renderables
                .iter()
                .flat_map(|renderable| {
                    groups.get(&renderable.mesh.key()).map_or_else(
                        || vec![renderable.mesh],
                        |group| {
                            group
                                .levels
                                .iter()
                                .map(|level| level.mesh)
                                .collect()
                        },
                    )
                })
                .collect();
            self.visible_meshes.sort_unstable_by_key(|mesh| mesh.key());
            self.visible_meshes.dedup_by_key(|mesh| mesh.key());
            self.prepared_meshes_revision = revision;
            self.prepared_meshes_lods = lods;
        }
        for mesh_handle in self.visible_meshes.iter().copied() {
            let key = mesh_handle.key();
            let revision = assets.meshes.revision(mesh_handle).unwrap_or(0);
            if self
                .prepared_meshes
                .get(&key)
                .is_some_and(|mesh| mesh.source_revision == revision)
            {
                continue;
            }
            let mesh = assets
                .meshes
                .get(mesh_handle)
                .or_else(|| assets.meshes.get(assets.fallback_mesh))
                .ok_or_else(|| {
                    SceneRenderError("fallback mesh is missing".into())
                })?;
            let mut prepared =
                self.prepare_mesh(mesh_handle, mesh, revision)?;
            // A GPU-skinned copy moves without changing its revision, so
            // its cooked bounds would cull it wrongly.
            // ponytail: never culled; bound by the joints if that costs.
            if render_world
                .skin_jobs
                .iter()
                .any(|job| job.mesh == mesh_handle)
            {
                prepared.bounds = None;
            }
            let name = asset_name(
                "Mesh",
                mesh_handle.key(),
                assets.meshes.path(mesh_handle),
            );
            name_object(
                &**prepared.vertices.buffer(),
                &format!("{name} vertices"),
            );
            name_object(
                &**prepared.indices.buffer(),
                &format!("{name} indices"),
            );
            self.counters.upload_bytes +=
                prepared.vertices.size() + prepared.indices.size();
            self.prepared_meshes.insert(key, prepared);
        }
        self.capacity.missing_meshes = self
            .visible_meshes
            .iter()
            .filter(|mesh| !assets.meshes.contains(**mesh))
            .count();
        // Meshes removed from the server free their GPU buffers. In-flight
        // command buffers keep their own references until they complete.
        let visible = &self.visible_meshes;
        self.prepared_meshes.retain(|_, prepared| {
            assets.meshes.contains(prepared.handle)
                || visible.contains(&prepared.handle)
        });
        Ok(())
    }

    /// Uploads the base-color textures of the materials in use. Textures
    /// still loading are skipped and sample white until they publish.
    /// Uploads every map of the materials in use and builds their set 1.
    /// The environment map and light cookies (`lit`) upload with them.
    fn prepare_materials(
        &mut self,
        assets: &AssetServer,
        lit: Vec<Handle<TextureAsset>>,
    ) -> Result<(), SceneRenderError> {
        let materials = self
            .prepared_instances
            .as_ref()
            .map(|prepared| {
                prepared
                    .material_revisions
                    .iter()
                    .map(|(material, _)| *material)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let slots = |material: &MaterialAsset| {
            [
                material.base_color_texture,
                material.normal_texture,
                material.metallic_roughness_texture,
                material.occlusion_texture,
                material.emissive_texture,
            ]
        };
        let used = materials
            .iter()
            .filter_map(|material| assets.materials.get(*material))
            .flat_map(|material| slots(material).into_iter().flatten())
            .chain(lit)
            .collect::<Vec<_>>();
        for &handle in &used {
            let revision = assets.textures.revision(handle).unwrap_or(0);
            if self
                .prepared_textures
                .get(&handle.key())
                .is_some_and(|prepared| prepared.source_revision == revision)
            {
                continue;
            }
            let Some(texture) = assets.textures.get(handle) else {
                continue;
            };
            let sampler = match self.samplers.get(&texture.sampler) {
                Some(sampler) => sampler.clone(),
                None => {
                    let sampler =
                        texture_sampler(&self.queue, texture.sampler)?;
                    self.samplers.insert(texture.sampler, sampler.clone());
                    sampler
                }
            };
            // A malformed texture uses the slot's missing-map stand-in
            // instead of failing the frame.
            let view = create_texture(
                &self.memory_allocator,
                texture,
                &mut self.pending_texture_uploads,
            )
            .ok();
            if let Some(view) = &view {
                name_object(
                    &**view.image(),
                    &asset_name(
                        "Texture",
                        handle.key(),
                        assets.textures.path(handle),
                    ),
                );
            }
            self.prepared_textures.insert(
                handle.key(),
                PreparedTexture {
                    handle,
                    view,
                    sampler,
                    source_revision: revision,
                },
            );
        }
        self.prepared_textures.retain(|_, prepared| {
            assets.textures.contains(prepared.handle)
                || used.contains(&prepared.handle)
        });

        let mut missing = used.clone();
        missing.retain(|texture| {
            self.prepared_textures
                .get(&texture.key())
                .is_none_or(|prepared| prepared.view.is_none())
        });
        missing.sort_unstable_by_key(|texture| texture.key());
        missing.dedup();
        self.capacity.missing_textures = missing.len();

        for &handle in &materials {
            let Some(material) = assets.materials.get(handle) else {
                continue;
            };
            let mut slot_index = 0;
            let mut textures = slots(material).map(|slot| {
                let fallback = &self.missing_textures[slot_index];
                slot_index += 1;
                let Some(texture) = slot else {
                    return self.white_texture.clone();
                };
                self.prepared_textures
                    .get(&texture.key())
                    .and_then(|prepared| {
                        Some((prepared.view.clone()?, prepared.sampler.clone()))
                    })
                    .unwrap_or_else(|| fallback.clone())
            });
            if let Some(feed) = self.screen_feeds.get(&handle.key()) {
                // Base color and emissive show the camera image.
                for slot in [0, 4] {
                    textures[slot] = (feed.view.clone(), feed.sampler.clone());
                }
            }
            let views = textures
                .each_ref()
                .map(|(view, _)| Arc::as_ptr(view) as usize);
            if self.prepared_materials.get(&handle.key()).is_some_and(
                |prepared| {
                    prepared.views == views
                        && prepared.shader == material.shader
                },
            ) {
                continue;
            }
            let set = create_material_set(
                &self.descriptor_allocator,
                &self.passes.pipeline.layout().set_layouts()[1],
                textures,
            )?;
            self.prepared_materials.insert(
                handle.key(),
                PreparedMaterial {
                    handle,
                    set,
                    views,
                    shader: material.shader.clone(),
                },
            );
        }
        self.prepared_materials.retain(|_, prepared| {
            assets.materials.contains(prepared.handle)
                || materials.contains(&prepared.handle)
        });
        Ok(())
    }

    fn prepare_lights(
        &mut self,
        render_world: &RenderWorld,
    ) -> Result<(), SceneRenderError> {
        let budget = light_budget(resolve_quality(
            render_world.quality,
            &self.capabilities,
        ));
        if self.prepared_lights.as_ref().is_some_and(|prepared| {
            prepared.revision == render_world.lights_revision
                && prepared.budget == budget
        }) {
            return Ok(());
        }

        let mut uploads = Vec::with_capacity(budget);
        let mut shadow = None;
        for extracted in &render_world.directional_lights {
            if uploads.len() == budget {
                break;
            }
            let direction = light_direction(extracted.transform.matrix);
            if shadow.is_none() && extracted.light.shadows {
                shadow = Some((
                    uploads.len() as u32,
                    ShadowView::Directional(direction),
                ));
            }
            uploads.push(LightUpload {
                position_kind: [0.0, 0.0, 0.0, 0.0],
                direction_range: [
                    direction[0],
                    direction[1],
                    direction[2],
                    0.0,
                ],
                color_intensity: [
                    extracted.light.color[0],
                    extracted.light.color[1],
                    extracted.light.color[2],
                    extracted.light.illuminance
                        / crate::runtime::DirectionalLight::LUX_PER_UNIT,
                ],
                spot_angles: [0.0; 4],
                cookie: [0.0, 0.0, 0.0, -1.0],
            });
        }
        // A point light shadows only when no directional or spot light does.
        let mut point_shadow = None;
        for extracted in &render_world.point_lights {
            if uploads.len() == budget {
                break;
            }
            let position = light_position(extracted.transform.matrix);
            if point_shadow.is_none() && extracted.light.shadows {
                point_shadow = Some((
                    uploads.len() as u32,
                    ShadowView::Point {
                        position,
                        range: extracted.light.range.max(0.01),
                    },
                ));
            }
            uploads.push(LightUpload {
                position_kind: [position[0], position[1], position[2], 1.0],
                direction_range: [
                    0.0,
                    0.0,
                    0.0,
                    extracted.light.range.max(0.01),
                ],
                color_intensity: [
                    extracted.light.color[0],
                    extracted.light.color[1],
                    extracted.light.color[2],
                    extracted.light.intensity / 1_000.0,
                ],
                spot_angles: [0.0, 0.0, extracted.light.radius.max(0.0), 0.0],
                cookie: [0.0, 0.0, 0.0, -1.0],
            });
        }
        let cookies = light_cookies(render_world);
        for extracted in &render_world.spot_lights {
            if uploads.len() == budget {
                break;
            }
            let position = light_position(extracted.transform.matrix);
            let direction = light_direction(extracted.transform.matrix);
            if shadow.is_none() && extracted.light.shadows {
                shadow = Some((
                    uploads.len() as u32,
                    ShadowView::Spot {
                        position,
                        direction,
                        outer_angle: extracted.light.outer_angle,
                        range: extracted.light.range.max(0.01),
                    },
                ));
            }
            uploads.push(LightUpload {
                position_kind: [position[0], position[1], position[2], 2.0],
                direction_range: [
                    direction[0],
                    direction[1],
                    direction[2],
                    extracted.light.range.max(0.01),
                ],
                color_intensity: [
                    extracted.light.color[0],
                    extracted.light.color[1],
                    extracted.light.color[2],
                    extracted.light.intensity / 1_000.0,
                ],
                spot_angles: [
                    extracted.light.inner_angle.cos(),
                    extracted.light.outer_angle.cos(),
                    extracted.light.radius.max(0.0),
                    extracted.light.outer_angle.tan(),
                ],
                cookie: {
                    let [x, y, z, _] = extracted.transform.matrix[0];
                    let length = (x * x + y * y + z * z).sqrt().max(1e-6);
                    let slot = extracted.cookie.and_then(|cookie| {
                        cookies.iter().position(|&slot| slot == cookie)
                    });
                    [
                        x / length,
                        y / length,
                        z / length,
                        slot.map_or(-1.0, |slot| slot as f32),
                    ]
                },
            });
        }
        let shadow = shadow.or(point_shadow);
        let directional = uploads
            .iter()
            .take_while(|upload| upload.position_kind[3] == 0.0)
            .count() as u32;
        let local = uploads
            .iter()
            .enumerate()
            .skip(directional as usize)
            .map(|(index, upload)| {
                let [x, y, z, _] = upload.position_kind;
                (index as u32, [x, y, z, upload.direction_range[3]])
            })
            .collect();
        self.capacity.dropped_lights = render_world.directional_lights.len()
            + render_world.point_lights.len()
            + render_world.spot_lights.len()
            - uploads.len();
        self.capacity.lights = uploads.len();
        let count = uploads.len() as u32;
        if uploads.is_empty() {
            uploads.push(LightUpload::default());
        }
        let buffer = Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::STORAGE_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            uploads,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        self.counters.upload_bytes += buffer.size();
        name_object(&**buffer.buffer(), "Lights");
        let uniform = match (render_world.ambient_light, render_world.sky_light)
        {
            (None, None) => [0.12; 3],
            (ambient, _) => ambient.map_or([0.0; 3], |light| {
                light.color.map(|channel| channel * light.intensity)
            }),
        };
        let hemisphere = |pick: fn(&crate::runtime::SkyLight) -> [f32; 3]| {
            let sky = render_world.sky_light.as_ref();
            let color = sky.map_or([0.0; 3], |sky| {
                pick(sky).map(|channel| channel * sky.intensity)
            });
            [
                uniform[0] + color[0],
                uniform[1] + color[1],
                uniform[2] + color[2],
                1.0,
            ]
        };
        let ambient = hemisphere(|sky| sky.sky_color);
        let ground_ambient = hemisphere(|sky| sky.ground_color);
        self.prepared_lights = Some(PreparedLights {
            revision: render_world.lights_revision,
            budget,
            buffer,
            count,
            ambient,
            ground_ambient,
            shadow,
            directional,
            local,
            cookies,
        });
        Ok(())
    }

    /// Packs render objects into large GPU instance batches.
    ///
    /// Ten thousand cubes with the same mesh and material become one Vulkan
    /// draw call. GPU-owned objects store only their physics-buffer index here,
    /// so their changing transforms never need a CPU instance upload.
    /// When objects only moved (same layout revision), it patches their
    /// transforms in the packed copy and uploads that, without sorting.
    fn prepare_render_instances(
        &mut self,
        render_world: &RenderWorld,
        assets: &AssetServer,
    ) -> Result<(), SceneRenderError> {
        let physics_indices =
            &self.prepared_physics.as_ref().unwrap().body_indices;
        let lod_signature = lod_signature(assets);
        let mut screens = render_world
            .screens
            .iter()
            .map(|screen| screen.material.key())
            .collect::<Vec<_>>();
        screens.sort_unstable();
        let unchanged = |prepared: &PreparedRenderInstances| {
            prepared.flashes == render_world.flashes
                && prepared.material_overrides
                    == render_world.material_overrides
                && prepared.lightmaps == render_world.lightmaps
                && prepared.lod_signature == lod_signature
                && prepared.screens == screens
                && prepared.physics_revision
                    == render_world.gpu_physics_revision
                && prepared.material_revisions.iter().all(
                    |(material, revision)| {
                        assets.materials.revision(*material).unwrap_or(0)
                            == *revision
                    },
                )
                && prepared.mesh_revisions.iter().all(|(mesh, revision)| {
                    self.prepared_meshes.get(mesh).is_some_and(|prepared| {
                        prepared.source_revision == *revision
                    })
                })
        };
        if let Some(prepared) = self
            .prepared_instances
            .as_mut()
            .filter(|prepared| unchanged(prepared))
        {
            if prepared.renderables_revision
                == render_world.renderables_revision
            {
                return Ok(());
            }
            if prepared.layout_revision != 0
                && prepared.layout_revision
                    == render_world.renderables_layout_revision
            {
                let bytes = prepared.move_instances(render_world);
                let upload = self
                    .instance_allocator
                    .allocate_slice::<RenderInstanceUpload>(
                        prepared.packed.len() as DeviceSize,
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?;
                upload
                    .write()
                    .map_err(|error| SceneRenderError(error.to_string()))?
                    .copy_from_slice(&prepared.packed);
                self.counters.upload_bytes += bytes;
                prepared.instances = upload;
                return Ok(());
            }
        }

        let mut materials = render_world
            .renderables
            .iter()
            .map(|renderable| renderable.material)
            .collect::<Vec<_>>();
        materials.sort_unstable_by_key(|material| material.key());
        materials.dedup_by_key(|material| material.key());
        self.capacity.missing_materials = materials
            .iter()
            .filter(|material| !assets.materials.contains(**material))
            .count();
        let material_revisions = materials
            .into_iter()
            .map(|material| {
                (material, assets.materials.revision(material).unwrap_or(0))
            })
            .collect();

        // An opaque object whose mesh starts a LOD group stays one instance
        // of its batch, which picks the level per instance and frame. A
        // blended object becomes one instance per level, each drawn only
        // inside its level's range. Only the finest level casts shadows.
        // ponytail: the shadow pass draws that level at every range; pick
        // shadow LODs per light when distant casters cost too much.
        let groups = lod_groups(assets);
        let is_blended = |material| {
            assets.materials.get(material).is_some_and(|material| {
                material.alpha_mode == AlphaMode::Blend
                    || material.transmission > 0.0
            })
        };
        let mut renderables =
            Vec::with_capacity(render_world.renderables.len());
        let mut lod_ranges = Vec::with_capacity(render_world.renderables.len());
        let mut expanded_sources =
            Vec::with_capacity(render_world.renderables.len());
        for (source, renderable) in render_world.renderables.iter().enumerate()
        {
            let source = source as u32;
            let Some(group) = groups.get(&renderable.mesh.key()) else {
                renderables.push(*renderable);
                lod_ranges.push(None);
                expanded_sources.push(source);
                continue;
            };
            let screen =
                f32::from(u8::from(group.metric == LodMetric::ScreenSize));
            let ranges = group.ranges();
            if !is_blended(renderable.material) {
                let mut ends = [f32::NEG_INFINITY; MAX_LOD_LEVELS];
                for (end, range) in ends.iter_mut().zip(&ranges) {
                    *end = range[1];
                }
                renderables.push(*renderable);
                lod_ranges.push(Some(([0.0, screen, 0.0, 0.0], ends)));
                expanded_sources.push(source);
                continue;
            }
            for (level, (lod, range)) in group
                .levels
                .iter()
                .zip(ranges)
                .take(MAX_LOD_LEVELS)
                .enumerate()
            {
                renderables.push(crate::runtime::ExtractedRenderable {
                    mesh: lod.mesh,
                    cast_shadows: renderable.cast_shadows && level == 0,
                    ..*renderable
                });
                let mut ends = [f32::NEG_INFINITY; MAX_LOD_LEVELS];
                ends[0] = range[1];
                lod_ranges.push(Some(([range[0], screen, 0.0, 0.0], ends)));
                expanded_sources.push(source);
            }
        }
        let (order, mut batches, blended_start) =
            render_batch_order(&renderables, is_blended, |material| {
                let mut group = material_group(assets.materials.get(material));
                // A screen binds its own feed, so it shares no batch.
                if screens.binary_search(&material.key()).is_ok() {
                    group.0[0] = Some(!material.key());
                }
                group
            });
        let mut instances = Vec::with_capacity(order.len().max(1));
        for &index in &order {
            let renderable = renderables[index];
            let material = assets.materials.get(renderable.material);
            let mut color = material
                .map_or([1.0, 0.0, 1.0, 1.0], |material| material.base_color);
            let (mut emissive, mut surface) =
                material.map_or(([0.0, 0.0, 0.0, 1.0], [0.0; 4]), |material| {
                    let unlit = material.model == MaterialModel::Unlit;
                    (
                        [
                            material.emissive[0],
                            material.emissive[1],
                            material.emissive[2],
                            f32::from(u8::from(unlit)),
                        ],
                        [
                            material.metallic,
                            material.roughness,
                            f32::from(u8::from(
                                material.normal_texture.is_some(),
                            )),
                            f32::from(u8::from(material.flat_shading)),
                        ],
                    )
                });
            let mut uv_transform =
                material.map_or([1.0, 1.0, 0.0, 0.0], |material| {
                    let [x, y] = material.uv_scale;
                    let [u, v] = material.uv_offset;
                    [x, y, u, v]
                });
            if let Ok(found) = render_world
                .material_overrides
                .binary_search_by_key(&renderable.entity, |(entity, _)| *entity)
            {
                let values = &render_world.material_overrides[found].1;
                color = values.base_color.unwrap_or(color);
                if let Some([r, g, b]) = values.emissive {
                    emissive[..3].copy_from_slice(&[r, g, b]);
                }
                surface[0] = values.metallic.unwrap_or(surface[0]);
                surface[1] = values.roughness.unwrap_or(surface[1]);
                if let Some([x, y]) = values.uv_scale {
                    uv_transform[..2].copy_from_slice(&[x, y]);
                }
                if let Some([u, v]) = values.uv_offset {
                    uv_transform[2..].copy_from_slice(&[u, v]);
                }
            }
            if let Ok(found) = render_world
                .flashes
                .binary_search_by_key(&renderable.entity, |(entity, _)| *entity)
            {
                let tint = render_world.flashes[found].1;
                for channel in 0..3 {
                    color[channel] +=
                        (tint[channel] - color[channel]) * tint[3];
                    emissive[channel] +=
                        (tint[channel] - emissive[channel]) * tint[3];
                }
            }
            let (alpha_mode, cutoff) =
                match material.map(|material| material.alpha_mode) {
                    Some(AlphaMode::Mask { cutoff }) => (1, cutoff),
                    Some(AlphaMode::Blend) => (2, 0.0),
                    Some(AlphaMode::Opaque) | None => (0, 0.0),
                };
            instances.push(RenderInstanceUpload {
                model: renderable.transform.matrix,
                normal: normal_columns(Matrix4::from(
                    renderable.transform.matrix,
                )),
                color,
                emissive,
                surface,
                physics: [
                    physics_indices
                        .get(&renderable.entity)
                        .copied()
                        .unwrap_or(u32::MAX),
                    alpha_mode,
                    f32::to_bits(cutoff),
                    u32::from(renderable.cast_shadows)
                        | u32::from(renderable.receive_shadows) << 1
                        | lightmap_slot(render_world, renderable.entity) << 8,
                ],
                transmission: material.map_or([0.0; 4], |material| {
                    [
                        material.transmission,
                        material.ior,
                        material.thickness,
                        f32::from(u8::from(material.splat)),
                    ]
                }),
                uv_transform,
            });
        }
        // ponytail: GPU-physics bodies sort by their last CPU transform;
        // read back positions if blended GPU bodies ever need exact order.
        let mut mesh_revisions = Vec::new();
        let mut cull_source = Vec::with_capacity(order.len());
        let mut lod_spheres = Vec::with_capacity(order.len());
        let mut local_bounds = Vec::with_capacity(order.len());
        let bounds = order
            .iter()
            .map(|&index| {
                let renderable = &renderables[index];
                let mesh = self.prepared_meshes.get(&renderable.mesh.key());
                if let Some(mesh) = mesh {
                    mesh_revisions
                        .push((renderable.mesh.key(), mesh.source_revision));
                }
                let local = renderable
                    .bounds
                    .or_else(|| mesh.and_then(|mesh| mesh.bounds));
                // ponytail: the GPU tests a sphere around the box, which
                // keeps some instances the CPU box test drops; send boxes
                // if that overdraw ever shows up.
                let sphere =
                    local.map_or([0.0, 0.0, 0.0, -1.0], bounding_sphere);
                let (lod, lod_ends) =
                    lod_ranges[index].unwrap_or(([0.0; 4], NO_LOD_ENDS));
                cull_source.push(CullInstance {
                    sphere,
                    slot: [0; 4],
                    lod,
                    lod_ends,
                });
                lod_spheres.push(lod_ranges[index].map(|_| {
                    world_sphere(&renderable.transform.matrix, sphere)
                }));
                if physics_indices.contains_key(&renderable.entity) {
                    local_bounds.push(None);
                    return None;
                }
                local_bounds.push(local);
                local.map(|local| {
                    local.transformed(&renderable.transform.matrix)
                })
            })
            .collect();
        mesh_revisions.sort_unstable();
        mesh_revisions.dedup();
        // Each batch draws once per LOD level of its mesh; level draws sit
        // in a row from the batch's `first_draw`.
        let mut draw_meshes = Vec::with_capacity(batches.len());
        for batch in &mut batches {
            batch.first_draw = draw_meshes.len() as u32;
            match groups.get(&batch.mesh_key) {
                Some(group) => draw_meshes.extend(
                    group
                        .levels
                        .iter()
                        .take(MAX_LOD_LEVELS)
                        .map(|level| level.mesh.key()),
                ),
                None => draw_meshes.push(batch.mesh_key),
            }
            batch.levels = draw_meshes.len() as u32 - batch.first_draw;
            let first = batch.first_instance as usize;
            for instance in
                &mut cull_source[first..first + batch.instance_count as usize]
            {
                instance.slot = [batch.first_draw, batch.first_instance, 0, 0];
            }
        }
        for (offset, instance) in
            cull_source[blended_start..].iter_mut().enumerate()
        {
            let slot = (blended_start + offset) as u32;
            instance.slot = [(draw_meshes.len() + offset) as u32, slot, 1, 0];
        }
        let gpu_owned = instances
            .iter()
            .filter(|instance| instance.physics[0] != u32::MAX)
            .count();
        let blended = order[blended_start..]
            .iter()
            .enumerate()
            .map(|(offset, &index)| {
                let renderable = renderables[index];
                BlendedInstance {
                    instance: (blended_start + offset) as u32,
                    mesh_key: renderable.mesh.key(),
                    material: renderable.material,
                    position: light_position(renderable.transform.matrix),
                }
            })
            .collect();
        if instances.is_empty() {
            instances.push(RenderInstanceUpload::default());
        }
        let bytes = std::mem::size_of_val(instances.as_slice()) as DeviceSize;
        if bytes > self.instance_budget {
            return Err(SceneRenderError(format!(
                "{} render instances need {bytes} bytes, over the {} byte \
                 device-local upload budget",
                instances.len(),
                self.instance_budget
            )));
        }
        let upload = self
            .instance_allocator
            .allocate_slice::<RenderInstanceUpload>(
                instances.len() as DeviceSize
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
        upload
            .write()
            .map_err(|error| SceneRenderError(error.to_string()))?
            .copy_from_slice(&instances);
        self.counters.upload_bytes += upload.size();
        let refractive = instances[blended_start..]
            .iter()
            .any(|instance| instance.transmission[0] > 0.0);
        let glossy_batches = batches
            .iter()
            .enumerate()
            .filter(|(_, batch)| {
                assets
                    .materials
                    .get(batch.material)
                    .is_some_and(|material| {
                        material.model == MaterialModel::Pbr
                            && material.roughness < SSR_MAX_ROUGHNESS
                    })
            })
            .map(|(group, _)| group)
            .collect();
        let sources =
            order.iter().map(|&index| expanded_sources[index]).collect();
        let packed = instances;
        let instances = upload;
        self.prepared_instances = Some(PreparedRenderInstances {
            renderables_revision: render_world.renderables_revision,
            layout_revision: render_world.renderables_layout_revision,
            packed,
            sources,
            local_bounds,
            flashes: render_world.flashes.clone(),
            material_overrides: render_world.material_overrides.clone(),
            lightmaps: render_world.lightmaps.clone(),
            physics_revision: render_world.gpu_physics_revision,
            material_revisions,
            refractive,
            glossy_batches,
            instances,
            batches,
            draw_meshes,
            blended,
            mesh_revisions,
            bounds,
            visibility: None,
            cull_source,
            cull: None,
            occlusion: None,
            gpu_owned,
            lod_spheres,
            lod_signature,
            screens,
        });
        Ok(())
    }

    /// Uploads the cull-pass input once per instance rebuild.
    fn prepare_cull_instances(&mut self) -> Result<(), SceneRenderError> {
        let prepared = self.prepared_instances.as_mut().unwrap();
        if prepared.cull.is_some() {
            return Ok(());
        }
        let upload = self
            .instance_allocator
            .allocate_slice::<CullInstance>(
                prepared.cull_source.len().max(1) as DeviceSize
            )
            .map_err(|error| SceneRenderError(error.to_string()))?;
        {
            let mut write = upload
                .write()
                .map_err(|error| SceneRenderError(error.to_string()))?;
            write[0] = CullInstance::default();
            write[..prepared.cull_source.len()]
                .copy_from_slice(&prepared.cull_source);
        }
        let occlusion = self
            .instance_allocator
            .allocate_slice::<u32>(upload.len())
            .map_err(|error| SceneRenderError(error.to_string()))?;
        occlusion
            .write()
            .map_err(|error| SceneRenderError(error.to_string()))?
            .fill(0);
        self.counters.upload_bytes += upload.size() + occlusion.size();
        prepared.cull = Some(upload);
        prepared.occlusion = Some(occlusion);
        Ok(())
    }

    /// Keeps the prepared instances inside their LOD range for `lod_camera`
    /// and, with `frustum`, inside `clip`, and uploads the compacted
    /// main-pass list. Reused while the camera and the instances stay
    /// unchanged.
    fn prepare_visibility(
        &mut self,
        clip: Matrix4<f32>,
        frustum: bool,
        lod_camera: [f32; 4],
    ) -> Result<(), SceneRenderError> {
        let prepared = self.prepared_instances.as_mut().unwrap();
        let lods = prepared.lod_spheres.iter().any(Option::is_some);
        let clip_key = (frustum || lods).then(|| (clip.into(), frustum));
        if prepared
            .visibility
            .as_ref()
            .is_some_and(|visibility| visibility.clip == clip_key)
        {
            return Ok(());
        }
        let planes = frustum.then(|| frustum_planes(&clip));
        let (list, batches, blended) =
            compact_visible(&prepared.batches, &prepared.blended, |instance| {
                let index = instance as usize;
                let cull = &prepared.cull_source[index];
                let level = match prepared.lod_spheres[index] {
                    Some(sphere) => lod_level(
                        lod_value(lod_camera, sphere, cull.lod[1] != 0.0),
                        cull.lod,
                        cull.lod_ends,
                    )?,
                    None => 0,
                };
                match (&planes, prepared.bounds[index]) {
                    (Some(planes), Some(bounds))
                        if !bounds_in_frustum(planes, &bounds) =>
                    {
                        None
                    }
                    _ => Some(level),
                }
            });
        let culled = prepared.bounds.len() - list.len();
        // A zero-length buffer is invalid; the padding entry is never drawn.
        let upload = self
            .instance_allocator
            .allocate_slice::<VisibleInstance>(list.len().max(1) as DeviceSize)
            .map_err(|error| SceneRenderError(error.to_string()))?;
        {
            let mut write = upload
                .write()
                .map_err(|error| SceneRenderError(error.to_string()))?;
            write[0] = VisibleInstance { instance_index: 0 };
            write[..list.len()].copy_from_slice(&list);
        }
        self.counters.upload_bytes += upload.size();
        prepared.visibility = Some(PreparedVisibility {
            clip: clip_key,
            list: upload,
            batches,
            blended,
            culled,
        });
        Ok(())
    }

    /// Optional GPU features detected when the renderer was created.
    pub fn capabilities(&self) -> &RendererCapabilities {
        &self.capabilities
    }

    /// Capacity fallbacks taken so far; see [`RenderCapacityDiagnostics`].
    pub fn capacity_diagnostics(&self) -> RenderCapacityDiagnostics {
        self.capacity
    }

    /// Returns physics events only after their GPU submission has completed.
    ///
    /// The signal is checked first, so this never waits for unfinished work.
    pub fn take_completed_physics_events(&mut self) -> Vec<RawGpuPhysicsEvent> {
        self.collect_physics_readbacks();
        std::mem::take(&mut self.completed_physics_events)
    }

    /// Blocks the calling thread until every submitted physics frame has
    /// finished on the GPU, so the next `take_completed_physics_*` call
    /// returns all of their events and state.
    ///
    /// **Slow:** this stalls the CPU on the GPU and removes the frame
    /// overlap that keeps GPU physics cheap. Use it only for tooling, save
    /// states, and tests, never on the per-frame gameplay path.
    pub fn block_until_physics_readbacks_complete(&mut self) {
        for pending in &self.pending_physics {
            // A failed wait (lost device) leaves the readback pending; the
            // normal collection path then skips it.
            let _ = pending.fence.wait(None);
        }
        self.collect_physics_readbacks();
    }

    /// Compiler messages of material surface shaders that failed to build.
    /// Their materials draw with the built-in surface instead.
    #[must_use]
    pub fn surface_shader_errors(&self) -> Vec<&str> {
        self.surface_pipelines
            .values()
            .filter_map(|result| result.as_ref().err().map(String::as_str))
            .collect()
    }

    /// Every shader error of the latest frame; see [`ShaderErrors`].
    #[must_use]
    pub fn shader_errors(&self) -> ShaderErrors {
        let mut errors: Vec<String> = [
            ("surface", self.surface_shader_errors()),
            ("sky", self.sky_shader_error().into_iter().collect()),
            ("particle", self.particle_shader_errors()),
            ("post", self.post_shader_error().into_iter().collect()),
            ("condition", self.condition_shader_errors()),
        ]
        .into_iter()
        .flat_map(|(kind, errors)| {
            errors
                .into_iter()
                .map(move |error| format!("{kind}: {error}"))
        })
        .collect();
        errors.sort();
        ShaderErrors(errors)
    }

    /// Compiler messages of particle shaders that failed to build. Their
    /// emitters draw with the built-in particle look instead.
    #[must_use]
    pub fn particle_shader_errors(&self) -> Vec<&str> {
        self.particle_shaders
            .values()
            .filter_map(|result| result.as_ref().err().map(String::as_str))
            .collect()
    }

    /// Compiler message of the scene's post shader if it failed to build;
    /// the image then shows without it.
    #[must_use]
    pub fn post_shader_error(&self) -> Option<&str> {
        self.post_shader
            .as_ref()
            .and_then(|(_, _, result)| result.as_ref().err())
            .map(String::as_str)
    }

    /// Tone-map pipeline running `source`, a post shader, for `passes` at
    /// `samples`; `None` without a shader or when it fails to build.
    fn post_pipeline(
        &mut self,
        passes: &MainPasses,
        samples: u32,
        source: &str,
    ) -> Option<Arc<GraphicsPipeline>> {
        let queue = self.queue.clone();
        cached_pipeline(&mut self.post_shader, source, samples, |source| {
            let glsl = post_fragment_glsl(source)?;
            create_tonemap_pipeline(
                queue.clone(),
                passes.render_pass.clone(),
                Some(passes.tonemap_pipeline.layout().clone()),
                Some(compile_glsl(queue.device(), "fragment", &glsl)?),
            )
            .map_err(|error| error.0)
        })
    }

    /// Compiler message of the scene's sky shader if it failed to build;
    /// the built-in sky then shows instead.
    #[must_use]
    pub fn sky_shader_error(&self) -> Option<&str> {
        self.sky_shader
            .as_ref()
            .and_then(|(_, _, result)| result.as_ref().err())
            .map(String::as_str)
    }

    /// Sky pipeline running `sky`, a sky shader, and `fog`, a fog shader,
    /// for `passes` at `samples`; `None` without either or when it fails to
    /// build.
    fn sky_pipeline(
        &mut self,
        passes: &MainPasses,
        samples: u32,
        sky: &str,
        fog: &str,
    ) -> Option<Arc<GraphicsPipeline>> {
        let queue = self.queue.clone();
        let key = if sky.is_empty() && fog.is_empty() {
            String::new()
        } else {
            format!("{sky}\0{fog}")
        };
        cached_pipeline(&mut self.sky_shader, &key, samples, |_| {
            let glsl = sky_fragment_glsl(sky, fog)?;
            create_sky_pipeline(
                &queue,
                passes.render_pass.clone(),
                samples,
                passes.pipeline.layout().clone(),
                Some(compile_glsl(queue.device(), "fragment", &glsl)?),
            )
            .map_err(|error| error.0)
        })
    }

    /// Compiler messages of custom condition shaders that failed to build.
    /// Those shaders are skipped; the rest of physics keeps running.
    #[must_use]
    pub fn condition_shader_errors(&self) -> Vec<&str> {
        self.condition_shaders
            .values()
            .filter_map(|result| result.as_ref().err().map(String::as_str))
            .collect()
    }

    /// Returns how many events completed submissions lost to a full event
    /// buffer since the last call. The buffer only fills past its memory
    /// budget; gameplay should then resynchronize, for example by reading
    /// body state, because edge-triggered events may be missing.
    pub fn take_physics_events_lost(&mut self) -> u64 {
        self.collect_physics_readbacks();
        std::mem::take(&mut self.physics_events_lost)
    }

    /// Returns the state of [`PhysicsSyncMode::SelectedState`] and
    /// `FullState` bodies from GPU submissions that have completed.
    ///
    /// Like events, this never waits for unfinished work, so the state is
    /// usually one to three frames old; each sample carries its tick.
    ///
    /// [`PhysicsSyncMode::SelectedState`]: crate::runtime::PhysicsSyncMode
    pub fn take_completed_physics_states(
        &mut self,
    ) -> Vec<crate::runtime::GpuStateSample> {
        self.collect_physics_readbacks();
        std::mem::take(&mut self.completed_physics_states)
    }

    /// `(tick, hash)` of GPU body state after each tick of completed
    /// submissions, in tick order; see `StateHashes::gpu`. Like events,
    /// these arrive one to three frames after their tick.
    pub fn take_completed_physics_state_hashes(&mut self) -> Vec<(u64, u64)> {
        self.collect_physics_readbacks();
        let mut hashes = std::mem::take(&mut self.completed_physics_hashes);
        hashes.sort_unstable_by_key(|(tick, _)| *tick);
        hashes
    }

    fn collect_physics_readbacks(&mut self) {
        let mut index = 0;
        while index < self.pending_physics.len() {
            let signaled = self.pending_physics[index]
                .fence
                .is_signaled()
                .unwrap_or(false);
            if !signaled {
                index += 1;
                continue;
            }
            let pending = self.pending_physics.swap_remove(index);
            // The zero timeout only cleans an already-signaled submission.
            if pending.fence.wait(Some(Duration::ZERO)).is_err() {
                continue;
            }
            let latency = (self.frame_serial - pending.submitted_frame) as u32;
            let traffic = &mut self.readback_counters;
            traffic.physics_readback_latency_frames =
                traffic.physics_readback_latency_frames.max(latency);
            if let Some((buffer, tick, full)) = &pending.states {
                traffic.physics_state_bytes += buffer.size();
                if let Ok(states) = buffer.read() {
                    self.completed_physics_states.extend(
                        states.iter().zip(full.iter()).map(|(state, full)| {
                            state_sample(state, *tick, *full)
                        }),
                    );
                }
            }
            let (hashes, first, last) = &pending.hashes;
            if let Ok(hashes) = hashes.read() {
                self.completed_physics_hashes.extend((*first..=*last).map(
                    |tick| {
                        let [high, low] = hashes
                            [(tick % MAX_PHYSICS_STEPS_PER_FRAME) as usize];
                        (tick, u64::from(high) << 32 | u64::from(low))
                    },
                ));
            }
            let Ok(header) = pending.header.read() else {
                continue;
            };
            let count =
                (header.count as usize).min(pending.events.len() as usize);
            self.readback_counters.physics_event_bytes +=
                (size_of::<GpuEventHeader>()
                    + count * size_of::<GpuEventUpload>())
                    as u64;
            [
                self.capacity.physics_grid_overflow,
                self.capacity.physics_oversized_bodies,
                self.capacity.physics_grid_hash_collisions,
                self.capacity.physics_fallback_tests,
            ] = header.contacts;
            if header.overflow > 0 {
                self.capacity.physics_events_dropped +=
                    u64::from(header.overflow);
                self.physics_events_lost += u64::from(header.overflow);
                eprintln!(
                    "GPU physics event buffer overflowed by at least {} events",
                    header.overflow
                );
            }
            drop(header);
            let Ok(events) = pending.events.read() else {
                continue;
            };
            self.completed_physics_events.extend(
                events.iter().take(count).map(|event| RawGpuPhysicsEvent {
                    body_slot: event.header[0],
                    body_generation: event.header[1],
                    event_id: event.header[2],
                    flags: event.header[3],
                    tick_low: event.timing[0],
                    tick_high: event.timing[1],
                    payload_kind: event.timing[2],
                    reserved: event.timing[3],
                    payload: event.payload,
                }),
            );
        }
    }

    fn prepare_gpu_physics(
        &mut self,
        render_world: &RenderWorld,
    ) -> Result<(), SceneRenderError> {
        if self.prepared_physics.as_ref().is_some_and(|prepared| {
            prepared.source_revision == render_world.gpu_physics_revision
        }) {
            return Ok(());
        }

        let mut states =
            Vec::with_capacity(render_world.gpu_physics.len().max(1));
        let mut instructions = Vec::new();
        let mut rules = Vec::new();
        let mut shapes = Vec::with_capacity(states.capacity());
        let mut body_indices = HashMap::new();
        let previous = self.prepared_physics.take();
        let previous_index: HashMap<_, _> = previous
            .as_ref()
            .map(|previous| {
                previous
                    .source
                    .iter()
                    .enumerate()
                    .map(|(index, body)| (body.entity, (index, body)))
                    .collect()
            })
            .unwrap_or_default();
        // Everything before `metadata` is simulation state; metadata holds
        // rule offsets that shift whenever another body's rules change.
        let stride = size_of::<GpuBodyState>() as u64;
        let live_bytes = std::mem::offset_of!(GpuBodyState, metadata) as u64;
        let mut carry_regions = Vec::new();
        let mut readback = Vec::new();
        let mut readback_full = Vec::new();
        for body in &render_world.gpu_physics {
            let body_index = states.len() as u32;
            if body.sync.reads_back_state() {
                readback.push(BufferCopy {
                    src_offset: u64::from(body_index) * stride,
                    dst_offset: readback.len() as u64 * stride,
                    size: stride,
                    ..Default::default()
                });
                readback_full.push(
                    body.sync == crate::runtime::PhysicsSyncMode::FullState,
                );
            }
            let rule_offset = rules.len() as u32;
            body_indices.insert(body.entity, body_index);
            let (collider, layers) = body
                .collider
                .map_or((None, Default::default()), |(collider, layers)| {
                    (Some(collider), layers)
                });
            shapes.push(GpuBodyShape {
                shape: crate::runtime::gpu_shape_words(
                    collider.as_ref(),
                    body.transform.scale,
                ),
                material: collider.map_or([0.0; 4], |collider| {
                    [collider.friction, collider.restitution, 0.0, 0.0]
                }),
                layers: [layers.memberships, layers.filters, 0, 0],
            });
            // ponytail: rule cooldowns still reset on any edit; carry the
            // rules buffer too if that becomes visible.
            if let Some((old_index, _)) =
                previous_index.get(&body.entity).filter(|(_, old)| {
                    old.physics_id == body.physics_id
                        && old.transform == body.transform
                        && old.rigid_body == body.rigid_body
                        && old.solver == body.solver
                        && old.custom_shader == body.custom_shader
                })
            {
                carry_regions.push(BufferCopy {
                    src_offset: *old_index as u64 * stride,
                    dst_offset: body_index as u64 * stride,
                    size: live_bytes,
                    ..Default::default()
                });
            }
            for rule in &body.rules {
                let instruction_offset = instructions.len() as u32;
                instructions.extend(rule.instructions.iter().map(
                    |instruction: &GpuConditionInstruction| {
                        GpuConditionUpload {
                            words: [
                                instruction.opcode,
                                instruction.operand,
                                instruction.flags,
                                instruction.reserved,
                            ],
                            values: instruction.values,
                        }
                    },
                ));
                rules.push(GpuRuleState {
                    config: [
                        instruction_offset,
                        rule.instructions.len() as u32,
                        rule.event_id.0,
                        rule.mode as u32,
                    ],
                    timing: [rule.cooldown_seconds, -1.0e20, 0.0, 0.0],
                    state: [rule.payload as u32, 0, 0, 0],
                });
            }
            states.push(GpuBodyState {
                model: crate::runtime::sim_math::transform_matrix(
                    &body.transform,
                ),
                velocity: [
                    body.rigid_body.linear_velocity[0],
                    body.rigid_body.linear_velocity[1],
                    body.rigid_body.linear_velocity[2],
                    0.0,
                ],
                angular_velocity: [
                    body.rigid_body.angular_velocity[0],
                    body.rigid_body.angular_velocity[1],
                    body.rigid_body.angular_velocity[2],
                    0.0,
                ],
                properties: [
                    body.rigid_body.mass,
                    body.rigid_body.gravity_scale,
                    match body.rigid_body.kind {
                        crate::runtime::RigidBodyKind::Fixed => 0.0,
                        crate::runtime::RigidBodyKind::Dynamic => 1.0,
                        crate::runtime::RigidBodyKind::Kinematic => 2.0,
                    },
                    body.custom_shader.as_deref().map_or(0.0, |path| {
                        crate::runtime::custom_solver_id(path) as f32
                    }),
                ],
                // The first custom value carries the selected solver to the
                // shared ECS physics shader. Space is solver value 4.
                custom_values: [
                    match body.solver {
                        crate::runtime::PhysicsSolver::Full => 0.0,
                        crate::runtime::PhysicsSolver::Simplified => 1.0,
                        crate::runtime::PhysicsSolver::NoCollision => 2.0,
                        crate::runtime::PhysicsSolver::Custom => 3.0,
                        crate::runtime::PhysicsSolver::Space => 4.0,
                    },
                    0.0,
                    0.0,
                    0.0,
                ],
                metadata: [
                    body.physics_id.slot,
                    body.physics_id.generation,
                    rule_offset,
                    body.rules.len() as u32,
                ],
            });
        }
        if states.is_empty() {
            states.push(GpuBodyState {
                model: Matrix4::<f32>::identity().into(),
                velocity: [0.0; 4],
                angular_velocity: [0.0; 4],
                properties: [0.0; 4],
                custom_values: [0.0; 4],
                metadata: [0; 4],
            });
        }
        if shapes.is_empty() {
            shapes.push(GpuBodyShape::default());
        }
        if instructions.is_empty() {
            instructions.push(GpuConditionUpload::default());
        }
        if rules.is_empty() {
            rules.push(GpuRuleState::default());
        }

        // Solvers 2 (NoCollision) and 3 (Custom) skip body contacts.
        let grid_cell_size = contact_grid_cell_size(
            shapes
                .iter()
                .zip(&states)
                .filter(|(_, state)| {
                    !matches!(state.custom_values[0] as u32, 2 | 3)
                })
                .filter_map(|(shape, _)| shape_bounding_radius(shape.shape)),
        );
        let storage = |usage| BufferCreateInfo {
            usage,
            ..Default::default()
        };
        let upload = AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
            ..Default::default()
        };
        let states = Buffer::from_iter(
            self.memory_allocator.clone(),
            storage(
                BufferUsage::STORAGE_BUFFER
                    | BufferUsage::TRANSFER_SRC
                    | BufferUsage::TRANSFER_DST,
            ),
            upload.clone(),
            states,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let instructions = Buffer::from_iter(
            self.memory_allocator.clone(),
            storage(BufferUsage::STORAGE_BUFFER),
            upload.clone(),
            instructions,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let rules = Buffer::from_iter(
            self.memory_allocator.clone(),
            storage(BufferUsage::STORAGE_BUFFER),
            upload.clone(),
            rules,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let shapes = Buffer::from_iter(
            self.memory_allocator.clone(),
            storage(BufferUsage::STORAGE_BUFFER),
            upload,
            shapes,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let carry = match previous {
            Some(previous) if !carry_regions.is_empty() => {
                // In-flight frames may still write the old buffer.
                for context in &mut self.frame_contexts {
                    if let Some(fence) = context.fence.take() {
                        fence.wait(None).map_err(|error| {
                            SceneRenderError(error.to_string())
                        })?;
                    }
                }
                Some((previous.states, carry_regions))
            }
            _ => None,
        };
        self.counters.upload_bytes +=
            states.size() + instructions.size() + rules.size() + shapes.size();
        name_object(&**states.buffer(), "GPU physics states");
        name_object(&**instructions.buffer(), "GPU physics instructions");
        name_object(&**rules.buffer(), "GPU physics rules");
        self.prepared_physics = Some(PreparedGpuPhysics {
            source_revision: render_world.gpu_physics_revision,
            source: render_world.gpu_physics.clone(),
            body_indices,
            states,
            instructions,
            rules,
            shapes,
            carry,
            readback,
            readback_full: readback_full.into(),
            grid_cell_size,
        });
        self.last_physics_tick = self
            .last_physics_tick
            .min(render_world.physics_tick.saturating_sub(1));
        Ok(())
    }

    fn prepare_mesh(
        &self,
        handle: Handle<MeshAsset>,
        mesh: &MeshAsset,
        source_revision: u64,
    ) -> Result<PreparedMesh, SceneRenderError> {
        // Storage too: GPU skinning writes deformed copies in place.
        let vertices = Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::VERTEX_BUFFER | BufferUsage::STORAGE_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            mesh.vertices.iter().map(|vertex| SceneVertex {
                position: vertex.position,
                normal: vertex.normal,
                uv: vertex.uv,
                tangent: vertex.tangent,
                color: vertex.color,
            }),
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        let indices = Buffer::from_iter(
            self.memory_allocator.clone(),
            BufferCreateInfo {
                usage: BufferUsage::INDEX_BUFFER,
                ..Default::default()
            },
            AllocationCreateInfo {
                memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                    | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
                ..Default::default()
            },
            mesh.indices.iter().copied(),
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        Ok(PreparedMesh {
            handle,
            vertices,
            indices,
            source_revision,
            bounds: crate::runtime::picking::mesh_bounds(mesh).map(
                |(min, max)| RenderBounds::Aabb {
                    min: min.into(),
                    max: max.into(),
                },
            ),
        })
    }

    /// Recreates the scene targets for a new extent or sample count. With
    /// more than one sample, the multisampled targets resolve into `hdr` and
    /// `depth`, so tone mapping, editor lines, and the depth pyramid always
    /// read single-sample images.
    fn ensure_depth(
        &mut self,
        extent: [u32; 2],
        samples: u32,
    ) -> Result<(), SceneRenderError> {
        if samples != self.scene_samples
            || (samples > 1 && self.msaa_targets.is_none())
            || (extent != self.depth_extent && samples > 1)
        {
            self.msaa_targets = (samples > 1)
                .then(|| -> Result<_, SceneRenderError> {
                    Ok((
                        create_hdr(&self.memory_allocator, extent, samples)?,
                        create_depth(&self.memory_allocator, extent, samples)?,
                    ))
                })
                .transpose()?;
            self.scene_samples = samples;
            // Cached framebuffers use the old attachments and render pass.
            self.prepared_frames.clear();
        }
        if extent != self.depth_extent {
            self.depth = create_depth(&self.memory_allocator, extent, 1)?;
            self.hdr = create_hdr(&self.memory_allocator, extent, 1)?;
            self.scene_color =
                create_scene_color(&self.memory_allocator, extent)?;
            self.depth_pyramid = create_depth_pyramid(
                &self.memory_allocator,
                &self.descriptor_allocator,
                &self.depth_pyramid_copy_pipeline,
                &self.depth_pyramid_reduce_pipeline,
                &self.depth,
            )?;
            self.bloom_chain = BloomChain::new(
                &self.memory_allocator,
                &self.descriptor_allocator,
                &self.post_pipelines,
                &self.hdr,
            )?;
            self.exposure = ExposureMeter::new(
                &self.memory_allocator,
                &self.descriptor_allocator,
                &self.post_pipelines,
                &self.hdr,
            )?;
            self.occlusion =
                OcclusionTargets::new(&self.memory_allocator, extent)?;
            self.scene_color_view_projection = None;
            self.depth_prepass_framebuffer = create_depth_prepass_framebuffer(
                &self.depth_prepass_pipeline,
                &self.depth,
            )?;
            self.tonemap_set = create_tonemap_set(
                &self.descriptor_allocator,
                &self.passes.tonemap_pipeline,
                &self.hdr,
                &self.bloom_chain,
                &self.exposure,
                &self.scene_color,
                &self.scene_color_sampler,
            )?;
            self.depth_extent = extent;
            // Cached framebuffers still point at the old depth and HDR images.
            self.prepared_frames.clear();
        }
        Ok(())
    }
}

/// Every LOD group and its revision. Instances are expanded again when it
/// changes.
fn lod_signature(assets: &AssetServer) -> Vec<(u64, u64)> {
    assets
        .lod_groups
        .iter()
        .map(|(handle, _)| {
            (
                handle.key(),
                assets.lod_groups.revision(handle).unwrap_or(0),
            )
        })
        .collect()
}

/// LOD groups by the key of their finest mesh; the first loaded wins.
fn lod_groups(assets: &AssetServer) -> HashMap<u64, &LodGroupAsset> {
    let mut groups = HashMap::new();
    for (_, group) in assets.lod_groups.iter() {
        if let Some(level) = group.levels.first() {
            groups.entry(level.mesh.key()).or_insert(group);
        }
    }
    groups
}

/// What a draw needs from a material besides the per-instance data: its
/// textures, whether screen-space reflections draw it, and a hash of its
/// surface shader (0 for none). Materials that agree share a batch, so a
/// scene of many flat colors draws once per mesh.
type MaterialGroup = ([Option<u64>; MATERIAL_TEXTURES], bool, u64);

fn material_group(material: Option<&MaterialAsset>) -> MaterialGroup {
    use std::hash::{Hash, Hasher};
    let Some(material) = material else {
        return ([None; MATERIAL_TEXTURES], false, 0);
    };
    let shader = if material.shader.is_empty() {
        0
    } else {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        material.shader.hash(&mut hasher);
        hasher.finish() | 1
    };
    let key = |texture: &Option<Handle<TextureAsset>>| texture.map(Handle::key);
    (
        [
            key(&material.base_color_texture),
            key(&material.normal_texture),
            key(&material.metallic_roughness_texture),
            key(&material.occlusion_texture),
            key(&material.emissive_texture),
        ],
        material.model == MaterialModel::Pbr
            && material.roughness < SSR_MAX_ROUGHNESS,
        shader,
    )
}

/// Sorts objects so equal meshes and material groups can use one instanced
/// draw. Blended objects go last and are not batched; the returned index is
/// where they start in the order. A batch names its first material, whose
/// texture set the whole batch binds.
fn render_batch_order(
    renderables: &[crate::runtime::ExtractedRenderable],
    is_blended: impl Fn(Handle<MaterialAsset>) -> bool,
    group_of: impl Fn(Handle<MaterialAsset>) -> MaterialGroup,
) -> (Vec<usize>, Vec<PreparedRenderBatch>, usize) {
    let blended = renderables
        .iter()
        .map(|renderable| is_blended(renderable.material))
        .collect::<Vec<_>>();
    let mut order = (0..renderables.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| {
        let renderable = &renderables[*index];
        (
            blended[*index],
            renderable.mesh.key(),
            group_of(renderable.material),
            // Blended objects stay one draw each.
            if blended[*index] {
                renderable.material.key()
            } else {
                0
            },
        )
    });
    let blended_start = order.partition_point(|index| !blended[*index]);
    let mut batches = Vec::<PreparedRenderBatch>::new();
    let mut previous_key = None;
    for (instance, index) in order[..blended_start].iter().copied().enumerate()
    {
        let renderable = renderables[index];
        let key = (renderable.mesh.key(), group_of(renderable.material));
        if previous_key != Some(key) {
            batches.push(PreparedRenderBatch {
                mesh_key: key.0,
                material: renderable.material,
                first_instance: instance as u32,
                instance_count: 0,
                first_draw: batches.len() as u32,
                levels: 1,
            });
            previous_key = Some(key);
        }
        batches.last_mut().unwrap().instance_count += 1;
    }
    (order, batches, blended_start)
}

/// Compacts the instances `in_view` keeps, with the LOD level it picks,
/// into one list: each batch draw's visible instances stay contiguous and
/// get a `(first, count)` range, in draw order, and each kept blended
/// instance gets its own slot.
fn compact_visible(
    batches: &[PreparedRenderBatch],
    blended: &[BlendedInstance],
    in_view: impl Fn(u32) -> Option<u32>,
) -> (Vec<VisibleInstance>, Vec<(u32, u32)>, Vec<BlendedInstance>) {
    let mut list = Vec::new();
    let mut ranges = Vec::with_capacity(batches.len());
    let mut picks = Vec::new();
    for batch in batches {
        picks.clear();
        picks.extend(
            (batch.first_instance..batch.first_instance + batch.instance_count)
                .filter_map(|instance| Some((in_view(instance)?, instance))),
        );
        for level in 0..batch.levels {
            let first = list.len() as u32;
            list.extend(picks.iter().filter(|(pick, _)| *pick == level).map(
                |&(_, instance_index)| VisibleInstance { instance_index },
            ));
            ranges.push((first, list.len() as u32 - first));
        }
    }
    let blended = blended
        .iter()
        .filter(|item| in_view(item.instance).is_some())
        .map(|item| {
            list.push(VisibleInstance {
                instance_index: item.instance,
            });
            BlendedInstance {
                instance: list.len() as u32 - 1,
                ..*item
            }
        })
        .collect();
    (list, ranges, blended)
}

/// Gribb-Hartmann planes `(normal, distance)` of a Vulkan clip matrix: left,
/// right, bottom, top, near (clip z >= 0), far. Inside is `n·p + d >= 0`.
fn frustum_planes(clip: &Matrix4<f32>) -> [[f32; 4]; 6] {
    let row = |index: usize| {
        let row = clip.row(index);
        [row[0], row[1], row[2], row[3]]
    };
    let [x, y, z, w] = [row(0), row(1), row(2), row(3)];
    let add = |a: [f32; 4], b: [f32; 4]| std::array::from_fn(|i| a[i] + b[i]);
    let sub = |a: [f32; 4], b: [f32; 4]| std::array::from_fn(|i| a[i] - b[i]);
    [add(w, x), sub(w, x), add(w, y), sub(w, y), z, sub(w, z)]
}

/// World box around the local box `min..max` under `matrix`.
fn world_aabb(
    matrix: [[f32; 4]; 4],
    min: [f32; 3],
    max: [f32; 3],
) -> RenderBounds {
    let matrix = matrix_from_array(matrix);
    let mut low = [f32::INFINITY; 3];
    let mut high = [f32::NEG_INFINITY; 3];
    for corner in 0..8 {
        let local = nalgebra::Point3::new(
            if corner & 1 == 0 { min[0] } else { max[0] },
            if corner & 2 == 0 { min[1] } else { max[1] },
            if corner & 4 == 0 { min[2] } else { max[2] },
        );
        let world = matrix.transform_point(&local);
        for axis in 0..3 {
            low[axis] = low[axis].min(world[axis]);
            high[axis] = high[axis].max(world[axis]);
        }
    }
    RenderBounds::Aabb {
        min: low,
        max: high,
    }
}

/// False only when the bounds lie wholly outside one plane: conservative,
/// so a box near a frustum corner may pass.
fn bounds_in_frustum(planes: &[[f32; 4]; 6], bounds: &RenderBounds) -> bool {
    planes.iter().all(|plane| match *bounds {
        RenderBounds::Sphere { center, radius } => {
            let length = (plane[0] * plane[0]
                + plane[1] * plane[1]
                + plane[2] * plane[2])
                .sqrt();
            (0..3).map(|i| plane[i] * center[i]).sum::<f32>() + plane[3]
                >= -radius * length
        }
        RenderBounds::Aabb { min, max } => {
            // The corner farthest along the plane normal.
            (0..3)
                .map(|i| {
                    plane[i] * if plane[i] >= 0.0 { max[i] } else { min[i] }
                })
                .sum::<f32>()
                + plane[3]
                >= 0.0
        }
    })
}

/// Orders blended instances farthest-first along the camera view direction.
fn sort_back_to_front(
    instances: &mut [BlendedInstance],
    eye: [f32; 3],
    forward: [f32; 3],
) {
    let depth = |instance: &BlendedInstance| {
        (0..3)
            .map(|axis| (instance.position[axis] - eye[axis]) * forward[axis])
            .sum::<f32>()
    };
    instances.sort_by(|a, b| depth(b).total_cmp(&depth(a)));
}

/// Builds a sampler for one texture's filter and wrap modes. Textures get a
/// full mip chain on upload, and the min filter picks how mips blend.
/// Linear minification filters anisotropically when the device enabled
/// `samplerAnisotropy`.
fn texture_sampler(
    queue: &Arc<Queue>,
    sampler: TextureSampler,
) -> Result<Arc<Sampler>, SceneRenderError> {
    let filter = |filter| match filter {
        TextureFilter::Nearest => Filter::Nearest,
        TextureFilter::Linear => Filter::Linear,
    };
    let wrap = |wrap| match wrap {
        TextureWrap::Repeat => SamplerAddressMode::Repeat,
        TextureWrap::MirroredRepeat => SamplerAddressMode::MirroredRepeat,
        TextureWrap::ClampToEdge => SamplerAddressMode::ClampToEdge,
    };
    Sampler::new(
        queue.device().clone(),
        SamplerCreateInfo {
            mag_filter: filter(sampler.mag_filter),
            min_filter: filter(sampler.min_filter),
            // Blend mips for smooth filtering; the default LOD range of
            // 0..=0 would never leave the full-size level.
            mipmap_mode: match sampler.min_filter {
                TextureFilter::Nearest => SamplerMipmapMode::Nearest,
                TextureFilter::Linear => SamplerMipmapMode::Linear,
            },
            lod: 0.0..=vulkano::image::sampler::LOD_CLAMP_NONE,
            anisotropy: sampler_anisotropy(
                queue.device().enabled_features().sampler_anisotropy,
                queue
                    .device()
                    .physical_device()
                    .properties()
                    .max_sampler_anisotropy,
                sampler.min_filter,
            ),
            address_mode: [
                wrap(sampler.wrap[0]),
                wrap(sampler.wrap[1]),
                SamplerAddressMode::Repeat,
            ],
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Anisotropy level for a texture sampler: up to 16x, capped by the
/// device limit, and only for linear minification with the feature enabled.
/// Nearest filtering asks for blocky texels, so it stays unfiltered.
fn sampler_anisotropy(
    enabled: bool,
    device_limit: f32,
    min_filter: TextureFilter,
) -> Option<f32> {
    (enabled && min_filter == TextureFilter::Linear && device_limit > 1.0)
        .then_some(device_limit.min(16.0))
}

/// Creates a sampled image for `texture`, queues its pixel copy for the next
/// frame, and returns the set-1 descriptor set that binds it.
/// Opens the debug label for `pass` and records it in `passes`. Labels are
/// only emitted when the instance has `ext_debug_utils`.
/// Records the frame's pass order, a debug label around each pass when
/// labels are on, and a start and end timestamp per pass when the queue has
/// timestamps. Pass `p` writes queries `2p` and `2p + 1`. Passes do not
/// nest, and each runs at most once per frame.
struct PassRecorder {
    labels: bool,
    timestamps: Option<Arc<QueryPool>>,
    passes: Vec<FramePass>,
}

impl PassRecorder {
    fn begin(
        &mut self,
        commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        pass: FramePass,
    ) -> Result<(), SceneRenderError> {
        self.passes.push(pass);
        if self.labels {
            let _ = commands.begin_debug_utils_label(DebugUtilsLabel {
                label_name: pass.label().to_string(),
                ..Default::default()
            });
        }
        self.timestamp(commands, 2 * pass as u32)
    }

    fn end(
        &mut self,
        commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    ) -> Result<(), SceneRenderError> {
        let pass = *self.passes.last().expect("`end` follows a `begin`");
        self.timestamp(commands, 2 * pass as u32 + 1)?;
        if self.labels {
            // Safety: pairs with the `begin` of the same pass.
            let _ = unsafe { commands.end_debug_utils_label() };
        }
        Ok(())
    }

    fn timestamp(
        &self,
        commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
        query: u32,
    ) -> Result<(), SceneRenderError> {
        if let Some(pool) = &self.timestamps {
            // Safety: `render` resets the pool at the start of the command
            // buffer, and each query is written once per frame.
            unsafe {
                commands
                    .write_timestamp(
                        pool.clone(),
                        query,
                        PipelineStage::AllCommands,
                    )
                    .map_err(|error| SceneRenderError(error.to_string()))?;
            }
        }
        Ok(())
    }
}

/// GPU time of `pass` from its timestamp pair, or `None` when the frame did
/// not run it. `period` is nanoseconds per tick.
fn pass_time(
    pool: &QueryPool,
    pass: FramePass,
    period: f32,
) -> Option<Duration> {
    let mut ticks = [0_u64; 2];
    let first = 2 * pass as u32;
    pool.get_results(first..first + 2, &mut ticks, QueryResultFlags::empty())
        .ok()
        .filter(|&available| available)?;
    // ponytail: ignores `timestamp_valid_bits` wrap-around; a wrapped pair
    // reads as zero for one frame.
    let ticks = ticks[1].saturating_sub(ticks[0]);
    Some(Duration::from_nanos(
        (ticks as f64 * f64::from(period)) as u64,
    ))
}

/// Draws `count` instances of `mesh` from `list[first..]`. With `indirect`,
/// the GPU-written command sets the real count and `count` only bounds the
/// bound range. The slice start stands in for `firstInstance`, which
/// indirect draws may not set without `drawIndirectFirstInstance`.
/// Fills mips 1 and up of `image`, each a linear downscale of the one above.
pub(super) fn record_mip_chain(
    commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    image: &Arc<Image>,
) -> Result<(), SceneRenderError> {
    let [width, height, _] = image.extent();
    let size =
        |level: u32| [(width >> level).max(1), (height >> level).max(1), 1];
    let layers = |mip_level| ImageSubresourceLayers {
        mip_level,
        ..image.subresource_layers()
    };
    for level in 1..image.mip_levels() {
        commands
            .blit_image(BlitImageInfo {
                regions: [ImageBlit {
                    src_subresource: layers(level - 1),
                    src_offsets: [[0; 3], size(level - 1)],
                    dst_subresource: layers(level),
                    dst_offsets: [[0; 3], size(level)],
                    ..Default::default()
                }]
                .into(),
                filter: Filter::Linear,
                ..BlitImageInfo::images(image.clone(), image.clone())
            })
            .map_err(|error| SceneRenderError(error.to_string()))?;
    }
    Ok(())
}

fn draw_instances(
    commands: &mut AutoCommandBufferBuilder<PrimaryAutoCommandBuffer>,
    mesh: &PreparedMesh,
    list: &Subbuffer<[VisibleInstance]>,
    first: u32,
    count: u32,
    indirect: Option<Subbuffer<[DrawIndexedIndirectCommand]>>,
    recorded: &Cell<RenderCounters>,
) -> Result<(), SceneRenderError> {
    // Indirect draws add their triangles once read back.
    let triangles = if indirect.is_some() {
        0
    } else {
        mesh.indices.len() / 3 * u64::from(count)
    };
    count_work(recorded, 1, 0, triangles);
    let range = u64::from(first)..u64::from(first + count);
    commands
        .bind_vertex_buffers(
            0,
            (mesh.vertices.clone(), list.clone().slice(range)),
        )
        .map_err(|error| SceneRenderError(error.to_string()))?
        .bind_index_buffer(mesh.indices.clone())
        .map_err(|error| SceneRenderError(error.to_string()))?;
    unsafe {
        match indirect {
            Some(indirect) => commands.draw_indexed_indirect(indirect),
            None => {
                commands.draw_indexed(mesh.indices.len() as u32, count, 0, 0, 0)
            }
        }
        .map_err(|error| SceneRenderError(error.to_string()))?;
    }
    Ok(())
}

/// Adds recorded draws, dispatches, and triangles to `counters`.
fn count_work(
    counters: &Cell<RenderCounters>,
    draws: u32,
    dispatches: u32,
    triangles: u64,
) {
    let mut value = counters.get();
    value.draws += draws;
    value.dispatches += dispatches;
    value.triangles += triangles;
    counters.set(value);
}

/// Names a Vulkan object for graphics debuggers (RenderDoc, Nsight) and
/// validation messages. Does nothing without `ext_debug_utils`.
// ponytail: transient suballocator arenas (instances, visibility lists,
// cull commands) stay unnamed; name their arenas if a capture needs them.
pub(super) fn name_object<T: vulkano::VulkanObject + DeviceOwned>(
    object: &T,
    name: &str,
) {
    let device = object.device();
    if device.instance().enabled_extensions().ext_debug_utils {
        let result = device.set_debug_utils_object_name(object, Some(name));
        // A name only helps debugging; a rejected one must not stop a frame.
        debug_assert!(result.is_ok(), "naming {name} failed: {result:?}");
    }
}

/// `"<kind> <path>"` for a loaded asset, `"<kind> #<key>"` for one built in
/// code.
/// Converts one read-back GPU body into the runtime's state sample.
fn state_sample(
    state: &GpuBodyState,
    tick: u64,
    full: bool,
) -> crate::runtime::GpuStateSample {
    let xyz = |value: [f32; 4]| [value[0], value[1], value[2]];
    crate::runtime::GpuStateSample {
        physics_id: crate::runtime::PhysicsId {
            slot: state.metadata[0],
            generation: state.metadata[1],
        },
        tick,
        transform: crate::runtime::sim_math::transform_from_matrix(state.model),
        linear_velocity: xyz(state.velocity),
        angular_velocity: xyz(state.angular_velocity),
        custom_values: full.then_some(state.custom_values),
    }
}

fn asset_name(kind: &str, key: u64, path: Option<&std::path::Path>) -> String {
    match path {
        Some(path) => format!("{kind} {}", path.display()),
        None => format!("{kind} #{key}"),
    }
}

type GpuCullBuffers = (
    Subbuffer<[VisibleInstance]>,
    Subbuffer<[DrawIndexedIndirectCommand]>,
);

/// A cull dispatch's outputs and the descriptor set it writes them through.
type GpuCullSet = (
    Subbuffer<[VisibleInstance]>,
    Subbuffer<[DrawIndexedIndirectCommand]>,
    Arc<DescriptorSet>,
);

/// Per-frame outputs of the GPU cull pass: the visible list, one instance
/// buffer's length per LOD level so each group and level writes into its
/// own range, and one draw command per batch draw and per blended instance,
/// in that order, with a zero instance count for the pass to count up.
fn gpu_cull_buffers(
    allocator: &SubbufferAllocator,
    instances: &PreparedRenderInstances,
    meshes: &HashMap<u64, PreparedMesh>,
) -> Result<GpuCullBuffers, SceneRenderError> {
    let draws = instances
        .draw_meshes
        .iter()
        .copied()
        .chain(instances.blended.iter().map(|item| item.mesh_key))
        .map(|mesh_key| DrawIndexedIndirectCommand {
            index_count: meshes
                .get(&mesh_key)
                .map_or(0, |mesh| mesh.indices.len() as u32),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    // Fresh suballocations, so no in-flight frame reads what the host
    // writes or the pass overwrites.
    let draw_commands = allocator
        .allocate_slice::<DrawIndexedIndirectCommand>(
            draws.len().max(1) as DeviceSize
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
    {
        let mut write = draw_commands
            .write()
            .map_err(|error| SceneRenderError(error.to_string()))?;
        write[0] = DrawIndexedIndirectCommand::default();
        write[..draws.len()].copy_from_slice(&draws);
    }
    let list = allocator
        .allocate_slice::<VisibleInstance>(
            (instances.cull_source.len()
                * instances
                    .batches
                    .iter()
                    .map(|batch| batch.levels)
                    .max()
                    .unwrap_or(1) as usize)
                .max(1) as DeviceSize,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
    Ok((list, draw_commands))
}

fn create_texture(
    memory_allocator: &Arc<StandardMemoryAllocator>,
    texture: &TextureAsset,
    pending: &mut Vec<PendingTextureUpload>,
) -> Result<Arc<ImageView>, SceneRenderError> {
    let [width, height] = texture.size;
    let (format, texel_bytes) = match texture.color_space {
        TextureColorSpace::Srgb => (Format::R8G8B8A8_SRGB, 4),
        TextureColorSpace::Linear => (Format::R8G8B8A8_UNORM, 4),
        TextureColorSpace::Hdr => (Format::R16G16B16A16_SFLOAT, 8),
    };
    if width == 0
        || height == 0
        || texture.rgba8.len() != width as usize * height as usize * texel_bytes
    {
        return Err(SceneRenderError(format!(
            "texture of {width}x{height} has {} bytes of {format:?} data",
            texture.rgba8.len()
        )));
    }
    let staging = Buffer::from_iter(
        memory_allocator.clone(),
        BufferCreateInfo {
            usage: BufferUsage::TRANSFER_SRC,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_HOST
                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
            ..Default::default()
        },
        texture.rgba8.iter().copied(),
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    let image = Image::new(
        memory_allocator.clone(),
        ImageCreateInfo {
            format,
            extent: [width, height, 1],
            mip_levels: 32 - width.max(height).leading_zeros(),
            // Transfer source for the mip chain blits.
            usage: ImageUsage::TRANSFER_SRC
                | ImageUsage::TRANSFER_DST
                | ImageUsage::SAMPLED,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    let view = ImageView::new_default(image.clone())
        .map_err(|error| SceneRenderError(error.to_string()))?;
    pending.push((staging, image));
    Ok(view)
}

/// Writes one material's set 1, maps in shader binding order.
fn create_material_set(
    descriptor_allocator: &Arc<StandardDescriptorSetAllocator>,
    layout: &Arc<DescriptorSetLayout>,
    textures: [(Arc<ImageView>, Arc<Sampler>); MATERIAL_TEXTURES],
) -> Result<Arc<DescriptorSet>, SceneRenderError> {
    DescriptorSet::new(
        descriptor_allocator.clone(),
        layout.clone(),
        textures
            .into_iter()
            .enumerate()
            .map(|(binding, (view, sampler))| {
                WriteDescriptorSet::image_view_sampler(
                    binding as u32,
                    view,
                    sampler,
                )
            }),
        [],
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

fn create_depth(
    allocator: &Arc<StandardMemoryAllocator>,
    extent: [u32; 2],
    samples: u32,
) -> Result<Arc<ImageView>, SceneRenderError> {
    let image = Image::new(
        allocator.clone(),
        ImageCreateInfo {
            format: Format::D32_SFLOAT,
            extent: [extent[0].max(1), extent[1].max(1), 1],
            samples: sample_count(samples)?,
            // The single-sample depth is sampled by the depth-pyramid copy.
            usage: if samples > 1 {
                ImageUsage::DEPTH_STENCIL_ATTACHMENT
            } else {
                ImageUsage::DEPTH_STENCIL_ATTACHMENT | ImageUsage::SAMPLED
            },
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(
        &*image,
        if samples > 1 {
            "Scene depth (MSAA)"
        } else {
            "Scene depth"
        },
    );
    ImageView::new_default(image)
        .map_err(|error| SceneRenderError(error.to_string()))
}

/// Farthest-depth mip chain of the early occlusion pass's depth, which the
/// late cull pass tests bounds against.
struct DepthPyramid {
    /// Every mip, sampled by the cull pass.
    view: Arc<ImageView>,
    sampler: Arc<Sampler>,
    /// Per mip, in order: the set of its dispatch (the depth copy for mip 0,
    /// a reduction of the mip below for the rest) and the mip's size.
    mips: Vec<(Arc<DescriptorSet>, [u32; 2])>,
}

// ponytail: mip 0 is full resolution, so the chain has one level more than a
// half-resolution pyramid; start at half size if the build shows up in
// `CullingStats` times.
fn create_depth_pyramid(
    allocator: &Arc<StandardMemoryAllocator>,
    descriptor_allocator: &Arc<StandardDescriptorSetAllocator>,
    copy_pipeline: &ComputePipeline,
    reduce_pipeline: &ComputePipeline,
    depth: &Arc<ImageView>,
) -> Result<DepthPyramid, SceneRenderError> {
    let [width, height, _] = depth.image().extent();
    let mip_levels = 32 - width.max(height).leading_zeros();
    let image = Image::new(
        allocator.clone(),
        ImageCreateInfo {
            format: Format::R32_SFLOAT,
            extent: [width, height, 1],
            mip_levels,
            // Transfer source for readback in tests.
            usage: ImageUsage::STORAGE
                | ImageUsage::SAMPLED
                | ImageUsage::TRANSFER_SRC,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(&*image, "Depth pyramid");
    let sampler = Sampler::new(
        allocator.device().clone(),
        SamplerCreateInfo {
            lod: 0.0..=vulkano::image::sampler::LOD_CLAMP_NONE,
            ..SamplerCreateInfo::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    let mip_view = |level: u32| {
        ImageView::new(
            image.clone(),
            ImageViewCreateInfo {
                subresource_range: ImageSubresourceRange {
                    mip_levels: level..level + 1,
                    ..image.subresource_range()
                },
                ..ImageViewCreateInfo::from_image(&image)
            },
        )
        .map_err(|error| SceneRenderError(error.to_string()))
    };
    let mut mips = Vec::with_capacity(mip_levels as usize);
    for level in 0..mip_levels {
        let (pipeline, source) = if level == 0 {
            (
                copy_pipeline,
                WriteDescriptorSet::image_view_sampler(
                    0,
                    depth.clone(),
                    sampler.clone(),
                ),
            )
        } else {
            (
                reduce_pipeline,
                WriteDescriptorSet::image_view(0, mip_view(level - 1)?),
            )
        };
        let set = DescriptorSet::new(
            descriptor_allocator.clone(),
            pipeline.layout().set_layouts()[0].clone(),
            [source, WriteDescriptorSet::image_view(1, mip_view(level)?)],
            [],
        )
        .map_err(|error| SceneRenderError(error.to_string()))?;
        mips.push((set, [(width >> level).max(1), (height >> level).max(1)]));
    }
    Ok(DepthPyramid {
        view: ImageView::new_default(image)
            .map_err(|error| SceneRenderError(error.to_string()))?,
        sampler,
        mips,
    })
}

/// Scene color format before tone mapping. Blendable and usable as a color
/// and input attachment on every Vulkan device.
const HDR_COLOR_FORMAT: Format = Format::R16G16B16A16_SFLOAT;

fn create_hdr(
    allocator: &Arc<StandardMemoryAllocator>,
    extent: [u32; 2],
    samples: u32,
) -> Result<Arc<ImageView>, SceneRenderError> {
    let image = Image::new(
        allocator.clone(),
        ImageCreateInfo {
            format: HDR_COLOR_FORMAT,
            extent: [extent[0].max(1), extent[1].max(1), 1],
            samples: sample_count(samples)?,
            // The single-sample HDR is the tone-mapping input and the source
            // of the scene color copy.
            // Bloom samples it too.
            usage: if samples > 1 {
                ImageUsage::COLOR_ATTACHMENT
            } else {
                ImageUsage::COLOR_ATTACHMENT
                    | ImageUsage::INPUT_ATTACHMENT
                    | ImageUsage::TRANSFER_SRC
                    | ImageUsage::SAMPLED
            },
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(
        &*image,
        if samples > 1 {
            "Scene HDR (MSAA)"
        } else {
            "Scene HDR"
        },
    );
    ImageView::new_default(image)
        .map_err(|error| SceneRenderError(error.to_string()))
}

/// Mipmapped copy of the opaque scene HDR that refracting materials sample.
fn create_scene_color(
    allocator: &Arc<StandardMemoryAllocator>,
    extent: [u32; 2],
) -> Result<Arc<ImageView>, SceneRenderError> {
    let [width, height] = [extent[0].max(1), extent[1].max(1)];
    let image = Image::new(
        allocator.clone(),
        ImageCreateInfo {
            format: HDR_COLOR_FORMAT,
            extent: [width, height, 1],
            mip_levels: 32 - width.max(height).leading_zeros(),
            usage: ImageUsage::TRANSFER_SRC
                | ImageUsage::TRANSFER_DST
                | ImageUsage::SAMPLED,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(&*image, "Scene color copy");
    ImageView::new_default(image)
        .map_err(|error| SceneRenderError(error.to_string()))
}

/// Array image of `layers` square faces with a full mip chain, viewed as a 2D
/// array even with one layer.
fn create_probe_maps(
    allocator: &Arc<StandardMemoryAllocator>,
    layers: u32,
    size: u32,
) -> Result<Arc<ImageView>, SceneRenderError> {
    let image = Image::new(
        allocator.clone(),
        ImageCreateInfo {
            format: HDR_COLOR_FORMAT,
            extent: [size, size, 1],
            array_layers: layers,
            mip_levels: 32 - size.leading_zeros(),
            usage: ImageUsage::COLOR_ATTACHMENT
                | ImageUsage::TRANSFER_SRC
                | ImageUsage::TRANSFER_DST
                | ImageUsage::SAMPLED,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(&*image, "Reflection probes");
    ImageView::new(
        image.clone(),
        ImageViewCreateInfo {
            view_type: ImageViewType::Dim2dArray,
            ..ImageViewCreateInfo::from_image(&image)
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Storage buffer holding the light probe rows.
fn create_light_probe_buffer(
    allocator: &Arc<StandardMemoryAllocator>,
    rows: Vec<[f32; 4]>,
) -> Result<Subbuffer<[[f32; 4]]>, SceneRenderError> {
    Buffer::from_iter(
        allocator.clone(),
        BufferCreateInfo {
            usage: BufferUsage::STORAGE_BUFFER,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE
                | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE,
            ..Default::default()
        },
        rows,
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Lightmap slot plus one of `entity`, 0 without one.
fn lightmap_slot(
    render_world: &RenderWorld,
    entity: bevy_ecs::entity::Entity,
) -> u32 {
    render_world
        .lightmaps
        .binary_search_by_key(&entity, |(entity, _)| *entity)
        .ok()
        .filter(|&slot| slot < super::lightmap::MAX_LIGHTMAPS)
        .map_or(0, |slot| slot as u32 + 1)
}

/// Hash of everything a lightmap bake reads: the lightmapped objects, their
/// meshes and materials, and the lights. 0 when nothing is lightmapped.
fn lightmap_key(render_world: &RenderWorld, assets: &AssetServer) -> u64 {
    use std::hash::{Hash, Hasher};
    if render_world.lightmaps.is_empty() {
        return 0;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    render_world.lightmaps.len().hash(&mut hasher);
    for renderable in &render_world.renderables {
        let Some(slot) = render_world
            .lightmaps
            .binary_search_by_key(&renderable.entity, |(entity, _)| *entity)
            .ok()
        else {
            continue;
        };
        slot.hash(&mut hasher);
        render_world.lightmaps[slot].1.resolution.hash(&mut hasher);
        renderable
            .transform
            .matrix
            .as_flattened()
            .iter()
            .for_each(|value| {
                value.to_bits().hash(&mut hasher);
            });
        renderable.mesh.key().hash(&mut hasher);
        assets.meshes.revision(renderable.mesh).hash(&mut hasher);
        renderable.material.key().hash(&mut hasher);
        assets
            .materials
            .revision(renderable.material)
            .hash(&mut hasher);
    }
    format!(
        "{:?}",
        (
            &render_world.directional_lights,
            &render_world.point_lights,
            &render_world.spot_lights,
            render_world.ambient_light,
            render_world.sky_light,
        )
    )
    .hash(&mut hasher);
    hasher.finish().max(1)
}

/// Probes per axis: each clamped to 1..=8, then the largest axis shrunk
/// until there are at most `MAX_LIGHT_PROBES`.
fn light_probe_resolution(resolution: [u32; 3]) -> [u32; 3] {
    let mut resolution = resolution.map(|count| count.clamp(1, 8));
    while resolution.iter().product::<u32>() as usize > MAX_LIGHT_PROBES {
        let largest = (0..3).max_by_key(|&axis| resolution[axis]).unwrap_or(0);
        resolution[largest] -= 1;
    }
    resolution
}

/// World position of each probe, x fastest, then y, then z; the fragment
/// shader's `probe_light` indexes the same way. One probe on an axis sits
/// at the center, more spread from wall to wall.
fn light_probe_positions(
    center: [f32; 3],
    extents: [f32; 3],
    resolution: [u32; 3],
) -> Vec<[f32; 3]> {
    let along = |axis: usize, index: u32| {
        if resolution[axis] == 1 {
            center[axis]
        } else {
            center[axis] - extents[axis]
                + 2.0 * extents[axis] * index as f32
                    / (resolution[axis] - 1) as f32
        }
    };
    let mut positions = Vec::new();
    for z in 0..resolution[2] {
        for y in 0..resolution[1] {
            for x in 0..resolution[0] {
                positions.push([along(0, x), along(1, y), along(2, z)]);
            }
        }
    }
    positions
}

/// Irradiance over pi toward +X, -X, +Y, -Y, +Z and -Z from the six
/// `size` square faces of one probe in `PROBE_FACES` order (rgba texels,
/// row 0 at the top of each face).
fn ambient_cube(texels: &[[f32; 4]], size: u32) -> [[f32; 3]; 6] {
    let axes = PROBE_FACES.map(|(forward, _)| Vector3::from(forward));
    let mut sums = [Vector3::zeros(); 6];
    let face_texels = (size * size) as usize;
    for (face, &(forward, up)) in PROBE_FACES.iter().enumerate() {
        let forward = Vector3::from(forward);
        let up = Vector3::from(up);
        let right = forward.cross(&up);
        for (index, texel) in texels[face * face_texels..][..face_texels]
            .iter()
            .enumerate()
        {
            let column = (index as u32 % size) as f32 + 0.5;
            let row = (index as u32 / size) as f32 + 0.5;
            let x = 2.0 * column / size as f32 - 1.0;
            let y = 2.0 * row / size as f32 - 1.0;
            let direction = forward + right * x - up * y;
            let length = direction.norm();
            // Solid angle of a texel on the unit cube face.
            let solid_angle =
                (2.0 / size as f32).powi(2) / (length * length * length);
            let radiance = Vector3::new(texel[0], texel[1], texel[2])
                .map(|channel| if channel.is_finite() { channel } else { 0.0 });
            for (axis, sum) in axes.iter().zip(&mut sums) {
                let cosine = direction.dot(axis) / length;
                if cosine > 0.0 {
                    *sum += radiance * cosine * solid_angle;
                }
            }
        }
    }
    sums.map(|sum| (sum / std::f32::consts::PI).into())
}

/// Camera transform at `center` looking along `forward`, the camera's -Z.
fn probe_face_transform(
    center: [f32; 3],
    (forward, up): ([f32; 3], [f32; 3]),
) -> crate::runtime::GlobalTransform {
    let forward = Vector3::from(forward);
    let up = Vector3::from(up);
    let right = forward.cross(&up);
    let back = -forward;
    crate::runtime::GlobalTransform {
        matrix: [
            [right.x, right.y, right.z, 0.0],
            [up.x, up.y, up.z, 0.0],
            [back.x, back.y, back.z, 0.0],
            [center[0], center[1], center[2], 1.0],
        ],
    }
}

fn create_tonemap_set(
    allocator: &Arc<StandardDescriptorSetAllocator>,
    pipeline: &Arc<GraphicsPipeline>,
    hdr: &Arc<ImageView>,
    bloom: &BloomChain,
    exposure: &ExposureMeter,
    scene_color: &Arc<ImageView>,
    scene_color_sampler: &Arc<Sampler>,
) -> Result<Arc<DescriptorSet>, SceneRenderError> {
    DescriptorSet::new(
        allocator.clone(),
        pipeline.layout().set_layouts()[0].clone(),
        [
            WriteDescriptorSet::image_view(0, hdr.clone()),
            WriteDescriptorSet::image_view_sampler(
                1,
                bloom.view.clone(),
                bloom.sampler.clone(),
            ),
            WriteDescriptorSet::image_view_sampler(
                2,
                scene_color.clone(),
                scene_color_sampler.clone(),
            ),
            WriteDescriptorSet::buffer(3, exposure.buffer.clone()),
        ],
        [],
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// The three compatible main render passes and every pipeline drawn in
/// them, for one scene sample count.
#[derive(Clone)]
struct MainPasses {
    render_pass: Arc<RenderPass>,
    /// Occlusion frames: last frame's visible set, keeping HDR and depth.
    early_render_pass: Arc<RenderPass>,
    /// Occlusion frames: the rest of the scene over the early pass, then
    /// tone mapping.
    late_render_pass: Arc<RenderPass>,
    /// Refraction frames with occlusion culling: the late opaque draws
    /// between the early pass and the scene color copy.
    middle_render_pass: Arc<RenderPass>,
    pipeline: Arc<GraphicsPipeline>,
    blend_pipeline: Arc<GraphicsPipeline>,
    /// Adds screen-space reflections over glossy opaque surfaces.
    reflection_pipeline: Arc<GraphicsPipeline>,
    debug_pipeline: Arc<GraphicsPipeline>,
    debug_on_top_pipeline: Arc<GraphicsPipeline>,
    /// Particles blended by alpha, then added as light.
    particle_pipelines: [Arc<GraphicsPipeline>; 2],
    tonemap_pipeline: Arc<GraphicsPipeline>,
    /// Height fog over the background after the opaque draws.
    sky_pipeline: Arc<GraphicsPipeline>,
}

impl MainPasses {
    /// Names every render pass and pipeline, with `suffix` after each name.
    fn name(&self, suffix: &str) {
        name_object(&*self.render_pass, &format!("Scene pass{suffix}"));
        name_object(
            &*self.early_render_pass,
            &format!("Early scene pass{suffix}"),
        );
        name_object(
            &*self.late_render_pass,
            &format!("Late scene pass{suffix}"),
        );
        name_object(
            &*self.middle_render_pass,
            &format!("Middle scene pass{suffix}"),
        );
        name_object(&*self.pipeline, &format!("Opaque{suffix}"));
        name_object(&*self.blend_pipeline, &format!("Blended{suffix}"));
        name_object(
            &*self.reflection_pipeline,
            &format!("Reflections{suffix}"),
        );
        name_object(&*self.debug_pipeline, &format!("Debug lines{suffix}"));
        name_object(
            &*self.debug_on_top_pipeline,
            &format!("Debug lines on top{suffix}"),
        );
        name_object(
            &*self.particle_pipelines[0],
            &format!("Particles{suffix}"),
        );
        name_object(
            &*self.particle_pipelines[1],
            &format!("Additive particles{suffix}"),
        );
        name_object(&*self.tonemap_pipeline, &format!("Tone map{suffix}"));
        name_object(&*self.sky_pipeline, &format!("Sky fog{suffix}"));
    }
}

/// Builds the main passes. Subpass 0 lights the scene into a float HDR
/// target. Subpass 1 tone maps it into the output and draws editor lines,
/// which stay exact. Occlusion frames split this into an early pass that
/// keeps HDR and depth and a late pass that continues them; all three
/// passes are compatible, so they share framebuffers and pipelines.
///
/// With `samples` above 1, subpass 0 draws into multisampled HDR and depth
/// that resolve into the single-sample `hdr` and `depth` at its end, so
/// subpass 1 and the depth pyramid are unchanged. `shared` gives the
/// single-sample passes whose pipeline layouts are reused.
fn create_main_passes(
    queue: &Arc<Queue>,
    output_format: Format,
    samples: u32,
    shared: Option<&MainPasses>,
) -> Result<MainPasses, SceneRenderError> {
    macro_rules! main_pass {
        ($color:ident, $hdr_load:ident, $hdr_store:ident, $depth_load:ident, $depth_store:ident) => {
            if samples > 1 {
                // ponytail: depth resolves from sample 0, so the occlusion
                // pyramid can be slightly too near at silhouette pixels and
                // pop a barely visible object for a frame; resolve with MAX
                // where `independent_resolve` allows if that shows.
                vulkano::ordered_passes_renderpass!(
                    queue.device().clone(),
                    attachments: {
                        color: {
                            format: output_format,
                            samples: 1,
                            load_op: DontCare,
                            store_op: $color,
                        },
                        hdr_ms: {
                            format: HDR_COLOR_FORMAT,
                            samples: samples,
                            load_op: $hdr_load,
                            store_op: $hdr_store,
                        },
                        depth_ms: {
                            format: Format::D32_SFLOAT,
                            samples: samples,
                            load_op: $depth_load,
                            store_op: $depth_store,
                        },
                        hdr: {
                            format: HDR_COLOR_FORMAT,
                            samples: 1,
                            load_op: DontCare,
                            store_op: $hdr_store,
                        },
                        depth: {
                            format: Format::D32_SFLOAT,
                            samples: 1,
                            load_op: DontCare,
                            store_op: $depth_store,
                        }
                    },
                    passes: [
                        {
                            color: [hdr_ms],
                            color_resolve: [hdr],
                            depth_stencil: {depth_ms},
                            depth_stencil_resolve: {depth},
                            depth_resolve_mode: SampleZero,
                            stencil_resolve_mode: SampleZero,
                            input: []
                        },
                        {
                            color: [color],
                            depth_stencil: {depth},
                            input: [hdr]
                        }
                    ]
                )
                .map_err(|error| SceneRenderError(error.to_string()))
                .and_then(resolve_in_attachment_layouts)
            } else {
                vulkano::ordered_passes_renderpass!(
                    queue.device().clone(),
                    attachments: {
                        color: {
                            format: output_format,
                            samples: 1,
                            load_op: DontCare,
                            store_op: $color,
                        },
                        hdr: {
                            format: HDR_COLOR_FORMAT,
                            samples: 1,
                            load_op: $hdr_load,
                            store_op: $hdr_store,
                        },
                        depth: {
                            format: Format::D32_SFLOAT,
                            samples: 1,
                            load_op: $depth_load,
                            store_op: $depth_store,
                        }
                    },
                    passes: [
                        {
                            color: [hdr],
                            depth_stencil: {depth},
                            input: []
                        },
                        {
                            color: [color],
                            depth_stencil: {depth},
                            input: [hdr]
                        }
                    ]
                )
                .map_err(|error| SceneRenderError(error.to_string()))
            }?
        };
    }
    let render_pass = main_pass!(Store, Clear, DontCare, Clear, DontCare);
    let early_render_pass = main_pass!(DontCare, Clear, Store, Clear, Store);
    let late_render_pass = main_pass!(Store, Load, DontCare, Load, DontCare);
    let middle_render_pass = main_pass!(DontCare, Load, Store, Load, Store);
    let [pipeline, blend_pipeline, reflection_pipeline] = create_pipelines(
        queue.clone(),
        render_pass.clone(),
        samples,
        shared.map(|passes| passes.pipeline.layout().clone()),
        None,
    )?;
    Ok(MainPasses {
        particle_pipelines: [
            create_particle_pipeline(
                queue,
                &render_pass,
                samples,
                false,
                None,
            )?,
            create_particle_pipeline(queue, &render_pass, samples, true, None)?,
        ],
        debug_pipeline: create_debug_pipeline(
            queue.clone(),
            render_pass.clone(),
            true,
        )?,
        debug_on_top_pipeline: create_debug_pipeline(
            queue.clone(),
            render_pass.clone(),
            false,
        )?,
        tonemap_pipeline: create_tonemap_pipeline(
            queue.clone(),
            render_pass.clone(),
            shared.map(|passes| passes.tonemap_pipeline.layout().clone()),
            None,
        )?,
        sky_pipeline: create_sky_pipeline(
            queue,
            render_pass.clone(),
            samples,
            pipeline.layout().clone(),
            None,
        )?,
        render_pass,
        early_render_pass,
        late_render_pass,
        middle_render_pass,
        pipeline,
        blend_pipeline,
        reflection_pipeline,
    })
}

/// Rebuilds `render_pass` with resolve attachments in attachment layouts.
/// vulkano's pass macro puts them in `TransferDstOptimal`, which
/// multisample resolve does not write through.
fn resolve_in_attachment_layouts(
    render_pass: Arc<RenderPass>,
) -> Result<Arc<RenderPass>, SceneRenderError> {
    let mut subpasses = render_pass.subpasses().to_vec();
    let mut attachments = render_pass.attachments().to_vec();
    let mut fix = |reference: &mut AttachmentReference, layout| {
        reference.layout = layout;
        let attachment = &mut attachments[reference.attachment as usize];
        for slot in
            [&mut attachment.initial_layout, &mut attachment.final_layout]
        {
            if *slot == ImageLayout::TransferDstOptimal {
                *slot = layout;
            }
        }
    };
    for subpass in &mut subpasses {
        for reference in subpass.color_resolve_attachments.iter_mut().flatten()
        {
            fix(reference, ImageLayout::ColorAttachmentOptimal);
        }
        if let Some(reference) = &mut subpass.depth_stencil_resolve_attachment {
            fix(reference, ImageLayout::DepthStencilAttachmentOptimal);
        }
    }
    RenderPass::new(
        render_pass.device().clone(),
        RenderPassCreateInfo {
            flags: render_pass.flags(),
            attachments,
            subpasses,
            dependencies: render_pass.dependencies().to_vec(),
            correlated_view_masks: render_pass.correlated_view_masks().to_vec(),
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

fn sample_count(samples: u32) -> Result<SampleCount, SceneRenderError> {
    SampleCount::try_from(samples).map_err(|()| {
        SceneRenderError(format!("{samples} is not a Vulkan sample count"))
    })
}

/// Fullscreen pass in subpass 1 that maps `hdr` into the output format,
/// with `fragment` in place of the built-in tone-map shader if given.
fn create_tonemap_pipeline(
    queue: Arc<Queue>,
    render_pass: Arc<RenderPass>,
    layout: Option<Arc<PipelineLayout>>,
    fragment: Option<Arc<vulkano::shader::ShaderModule>>,
) -> Result<Arc<GraphicsPipeline>, SceneRenderError> {
    let vertex = tonemap_vertex_shader::load(queue.device().clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("tonemap vertex entry point is missing".into())
        })?;
    let fragment = fragment
        .map_or_else(
            || tonemap_fragment_shader::load(queue.device().clone()),
            Ok,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("tonemap fragment entry point is missing".into())
        })?;
    let stages = [
        PipelineShaderStageCreateInfo::new(vertex),
        PipelineShaderStageCreateInfo::new(fragment),
    ];
    let layout = match layout {
        Some(layout) => layout,
        None => PipelineLayout::new(
            queue.device().clone(),
            PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
                .into_pipeline_layout_create_info(queue.device().clone())
                .map_err(|error| SceneRenderError(error.to_string()))?,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?,
    };
    let subpass = Subpass::from(render_pass, 1)
        .ok_or_else(|| SceneRenderError("tonemap subpass is missing".into()))?;
    GraphicsPipeline::new(
        queue.device().clone(),
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(Default::default()),
            input_assembly_state: Some(InputAssemblyState::default()),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState::default()),
            multisample_state: Some(MultisampleState::default()),
            depth_stencil_state: Some(DepthStencilState::default()),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState::default(),
            )),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Render pass into a `format` target and the pipeline of the sharp
/// upscale drawn in it.
fn create_sharp_upscale(
    queue: &Arc<Queue>,
    format: Format,
) -> Result<(Arc<RenderPass>, Arc<GraphicsPipeline>), SceneRenderError> {
    let error = |error: &dyn Display| SceneRenderError(error.to_string());
    let device = queue.device().clone();
    let render_pass = vulkano::single_pass_renderpass!(
        device.clone(),
        attachments: {
            color: {
                format: format,
                samples: 1,
                load_op: DontCare,
                store_op: Store,
            }
        },
        pass: {
            color: [color],
            depth_stencil: {}
        }
    )
    .map_err(|e| error(&e))?;
    name_object(&*render_pass, "Sharp upscale");
    let entry = |module: Result<Arc<vulkano::shader::ShaderModule>, _>| {
        module
            .map_err(|e: vulkano::Validated<vulkano::VulkanError>| error(&e))?
            .entry_point("main")
            .ok_or_else(|| {
                SceneRenderError("sharp upscale entry point is missing".into())
            })
    };
    let stages = [
        PipelineShaderStageCreateInfo::new(entry(
            tonemap_vertex_shader::load(device.clone()),
        )?),
        PipelineShaderStageCreateInfo::new(entry(sharp_upscale_shader::load(
            device.clone(),
        ))?),
    ];
    let layout = PipelineLayout::new(
        device.clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
            .into_pipeline_layout_create_info(device.clone())
            .map_err(|e| error(&e))?,
    )
    .map_err(|e| error(&e))?;
    let subpass = Subpass::from(render_pass.clone(), 0)
        .ok_or_else(|| SceneRenderError("sharp upscale subpass".into()))?;
    let pipeline = GraphicsPipeline::new(
        device,
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(Default::default()),
            input_assembly_state: Some(InputAssemblyState::default()),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState::default()),
            multisample_state: Some(MultisampleState::default()),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState::default(),
            )),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(|e| error(&e))?;
    name_object(&*pipeline, "Sharp upscale");
    Ok((render_pass, pipeline))
}

/// Sky fog over the background in subpass 0: drawn at the far plane with
/// `LessOrEqual`, so only pixels no surface covered pass. The background
/// is kept by the fog's transmittance in alpha and the in-scattered light
/// added: `color = fog.rgb + background * fog.a`.
fn create_sky_pipeline(
    queue: &Arc<Queue>,
    render_pass: Arc<RenderPass>,
    samples: u32,
    layout: Arc<PipelineLayout>,
    fragment: Option<Arc<vulkano::shader::ShaderModule>>,
) -> Result<Arc<GraphicsPipeline>, SceneRenderError> {
    let stage = |module: Result<Arc<vulkano::shader::ShaderModule>, _>| {
        module
            .map_err(|error: vulkano::Validated<vulkano::VulkanError>| {
                SceneRenderError(error.to_string())
            })?
            .entry_point("main")
            .map(PipelineShaderStageCreateInfo::new)
            .ok_or_else(|| {
                SceneRenderError("sky fog entry point is missing".into())
            })
    };
    let subpass = Subpass::from(render_pass, 0)
        .ok_or_else(|| SceneRenderError("scene subpass is missing".into()))?;
    GraphicsPipeline::new(
        queue.device().clone(),
        None,
        GraphicsPipelineCreateInfo {
            stages: [
                stage(sky_vertex_shader::load(queue.device().clone()))?,
                stage(fragment.map_or_else(
                    || sky_fragment_shader::load(queue.device().clone()),
                    Ok,
                ))?,
            ]
            .into_iter()
            .collect(),
            vertex_input_state: Some(Default::default()),
            input_assembly_state: Some(InputAssemblyState::default()),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState::default()),
            multisample_state: Some(MultisampleState {
                rasterization_samples: sample_count(samples)?,
                ..Default::default()
            }),
            depth_stencil_state: Some(DepthStencilState {
                depth: Some(DepthState {
                    write_enable: false,
                    compare_op: CompareOp::LessOrEqual,
                }),
                ..Default::default()
            }),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState {
                    blend: Some(AttachmentBlend {
                        src_color_blend_factor: BlendFactor::One,
                        dst_color_blend_factor: BlendFactor::SrcAlpha,
                        color_blend_op: BlendOp::Add,
                        src_alpha_blend_factor: BlendFactor::Zero,
                        dst_alpha_blend_factor: BlendFactor::One,
                        alpha_blend_op: BlendOp::Add,
                    }),
                    ..Default::default()
                },
            )),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Single-sample depth-only pass into the scene depth that ambient
/// occlusion traces, and its pipeline on the main layout. The pass leaves
/// depth in the layout the trace samples it in.
fn create_depth_prepass(
    queue: &Arc<Queue>,
    main_pipeline: &Arc<GraphicsPipeline>,
) -> Result<Arc<GraphicsPipeline>, SceneRenderError> {
    let device = queue.device().clone();
    let render_pass = vulkano::single_pass_renderpass!(
        device.clone(),
        attachments: {
            depth: {
                format: Format::D32_SFLOAT,
                samples: 1,
                load_op: Clear,
                store_op: Store,
                initial_layout: ImageLayout::Undefined,
                final_layout: FramePass::DepthPrepass
                    .next_layout(FrameResource::SceneDepth)
                    .expect("ambient occlusion samples the prepass depth"),
            }
        },
        pass: {
            color: [],
            depth_stencil: {depth}
        }
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(&*render_pass, "Depth prepass");
    let vertex = vertex_shader::load(device.clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("scene vertex entry point is missing".into())
        })?;
    let fragment = depth_prepass_fragment_shader::load(device.clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("prepass fragment entry point is missing".into())
        })?;
    let subpass = Subpass::from(render_pass, 0)
        .ok_or_else(|| SceneRenderError("prepass subpass is missing".into()))?;
    let pipeline = GraphicsPipeline::new(
        device,
        None,
        GraphicsPipelineCreateInfo {
            stages: [
                PipelineShaderStageCreateInfo::new(vertex.clone()),
                PipelineShaderStageCreateInfo::new(fragment),
            ]
            .into_iter()
            .collect(),
            vertex_input_state: Some(
                [SceneVertex::per_vertex(), VisibleInstance::per_instance()]
                    .definition(&vertex)
                    .map_err(|error| SceneRenderError(error.to_string()))?,
            ),
            input_assembly_state: Some(InputAssemblyState::default()),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState {
                cull_mode: CullMode::Back,
                front_face: FrontFace::CounterClockwise,
                ..Default::default()
            }),
            multisample_state: Some(MultisampleState::default()),
            depth_stencil_state: Some(DepthStencilState {
                depth: Some(DepthState::simple()),
                ..Default::default()
            }),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(main_pipeline.layout().clone())
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(&*pipeline, "Depth prepass");
    Ok(pipeline)
}

/// Framebuffer of the depth prepass over `depth`.
fn create_depth_prepass_framebuffer(
    pipeline: &Arc<GraphicsPipeline>,
    depth: &Arc<ImageView>,
) -> Result<Arc<Framebuffer>, SceneRenderError> {
    let PipelineSubpassType::BeginRenderPass(subpass) = pipeline.subpass()
    else {
        unreachable!("the prepass pipeline uses a render pass");
    };
    Framebuffer::new(
        subpass.render_pass().clone(),
        FramebufferCreateInfo {
            attachments: vec![depth.clone()],
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Creates the opaque/mask, alpha-blend and reflection overlay pipelines.
/// They share one layout so descriptor sets and push constants stay bound
/// between them. `fragment` replaces the built-in scene fragment shader,
/// for a material's surface shader.
fn create_pipelines(
    queue: Arc<Queue>,
    render_pass: Arc<RenderPass>,
    samples: u32,
    layout: Option<Arc<PipelineLayout>>,
    fragment: Option<Arc<vulkano::shader::ShaderModule>>,
) -> Result<[Arc<GraphicsPipeline>; 3], SceneRenderError> {
    let vertex = vertex_shader::load(queue.device().clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("scene vertex entry point is missing".into())
        })?;
    let fragment_module = match fragment {
        Some(module) => module,
        None => fragment_shader::load(queue.device().clone())
            .map_err(|error| SceneRenderError(error.to_string()))?,
    };
    // `PASS` in the fragment shader.
    let stages = |pass: u32| {
        let fragment = fragment_module
            .specialize(
                [(0, SpecializationConstant::U32(pass))]
                    .into_iter()
                    .collect(),
            )
            .map_err(|error| SceneRenderError(error.to_string()))?
            .entry_point("main")
            .ok_or_else(|| {
                SceneRenderError("scene fragment entry point is missing".into())
            })?;
        Ok::<_, SceneRenderError>([
            PipelineShaderStageCreateInfo::new(vertex.clone()),
            PipelineShaderStageCreateInfo::new(fragment),
        ])
    };
    let layout = match layout {
        Some(layout) => layout,
        None => PipelineLayout::new(
            queue.device().clone(),
            PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages(0)?)
                .into_pipeline_layout_create_info(queue.device().clone())
                .map_err(|error| SceneRenderError(error.to_string()))?,
        )
        .map_err(|error| SceneRenderError(error.to_string()))?,
    };
    let subpass = Subpass::from(render_pass, 0)
        .ok_or_else(|| SceneRenderError("scene subpass is missing".into()))?;
    let rasterization_samples = sample_count(samples)?;
    // Pass 0 opaque, 1 blended, 2 reflection overlay; see `PASS`.
    let create = |pass: u32| {
        GraphicsPipeline::new(
            queue.device().clone(),
            None,
            GraphicsPipelineCreateInfo {
                stages: stages(pass)?.into_iter().collect(),
                vertex_input_state: Some(
                    [
                        SceneVertex::per_vertex(),
                        VisibleInstance::per_instance(),
                    ]
                    .definition(&vertex)
                    .map_err(|error| SceneRenderError(error.to_string()))?,
                ),
                input_assembly_state: Some(InputAssemblyState::default()),
                viewport_state: Some(ViewportState::default()),
                rasterization_state: Some(RasterizationState {
                    cull_mode: CullMode::Back,
                    // Source meshes are CCW when viewed from outside. The clip
                    // correction makes that convention match Vulkan framebuffer
                    // space; changing this to clockwise renders the cube inside-out.
                    front_face: FrontFace::CounterClockwise,
                    ..Default::default()
                }),
                multisample_state: Some(MultisampleState {
                    rasterization_samples,
                    ..Default::default()
                }),
                // Blended surfaces test against opaque depth but do not write
                // it, so farther blended surfaces drawn later still show.
                // The overlay redraws opaque surfaces at their own depth.
                depth_stencil_state: Some(DepthStencilState {
                    depth: Some(DepthState {
                        write_enable: pass == 0,
                        compare_op: if pass == 2 {
                            CompareOp::LessOrEqual
                        } else {
                            CompareOp::Less
                        },
                    }),
                    ..Default::default()
                }),
                color_blend_state: Some(
                    ColorBlendState::with_attachment_states(
                        1,
                        ColorBlendAttachmentState {
                            // Premultiplied: the shader scales diffuse and
                            // emissive by alpha but not reflections, so
                            // clear glass still mirrors its surroundings.
                            // The overlay adds a signed difference.
                            blend: match pass {
                                1 => Some(AttachmentBlend {
                                    src_color_blend_factor: BlendFactor::One,
                                    src_alpha_blend_factor: BlendFactor::One,
                                    ..AttachmentBlend::alpha()
                                }),
                                2 => Some(AttachmentBlend::additive()),
                                _ => None,
                            },
                            ..Default::default()
                        },
                    ),
                ),
                dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                    .into_iter()
                    .collect(),
                subpass: Some(PipelineSubpassType::BeginRenderPass(
                    subpass.clone(),
                )),
                ..GraphicsPipelineCreateInfo::layout(layout.clone())
            },
        )
        .map_err(|error| SceneRenderError(error.to_string()))
    };
    Ok([create(0)?, create(1)?, create(2)?])
}

/// Source, sample count and pipeline of the last scene-wide custom shader.
type ShaderPipeline =
    Option<(String, u32, Result<Arc<GraphicsPipeline>, String>)>;

/// The pipeline `build` makes for `source` at `samples`, cached in `cache`
/// until either changes; `None` for an empty source or a failed build,
/// whose error is printed once and stays in `cache`.
fn cached_pipeline(
    cache: &mut ShaderPipeline,
    source: &str,
    samples: u32,
    build: impl FnOnce(&str) -> Result<Arc<GraphicsPipeline>, String>,
) -> Option<Arc<GraphicsPipeline>> {
    if source.is_empty() {
        *cache = None;
        return None;
    }
    if !cache.as_ref().is_some_and(|(cached, cached_samples, _)| {
        cached == source && *cached_samples == samples
    }) {
        let result = build(source);
        if let Err(error) = &result {
            eprintln!("shader skipped:\n{error}");
        }
        *cache = Some((source.to_owned(), samples, result));
    }
    cache
        .as_ref()
        .and_then(|(_, _, result)| result.as_ref().ok().cloned())
}

/// Pipelines per surface shader, fog shader and sample count.
type SurfacePipelines =
    HashMap<(String, String, u32), Result<[Arc<GraphicsPipeline>; 3], String>>;

/// Opaque, blended and overlay pipelines per material key for materials
/// with a working surface shader, built for `passes` at `samples`. With
/// `fog`, the scene's fog shader, every material gets them, running that
/// fog. Shaders no prepared material uses any more leave the cache.
fn surface_pipelines(
    cache: &mut SurfacePipelines,
    queue: &Arc<Queue>,
    passes: &MainPasses,
    samples: u32,
    materials: &HashMap<u64, PreparedMaterial>,
    fog: &str,
) -> HashMap<u64, [Arc<GraphicsPipeline>; 3]> {
    cache.retain(|(source, cached_fog, _), _| {
        cached_fog == fog
            && materials
                .values()
                .any(|material| material.shader == *source)
    });
    materials
        .iter()
        .filter(|(_, material)| !material.shader.is_empty() || !fog.is_empty())
        .filter_map(|(&key, material)| {
            cache
                .entry((material.shader.clone(), fog.to_owned(), samples))
                .or_insert_with(|| {
                    let result = surface_fragment_glsl(&material.shader, fog)
                        .and_then(|glsl| {
                            compile_glsl(queue.device(), "fragment", &glsl)
                        })
                        .and_then(|module| {
                            create_pipelines(
                                queue.clone(),
                                passes.render_pass.clone(),
                                samples,
                                Some(passes.pipeline.layout().clone()),
                                Some(module),
                            )
                            .map_err(|error| error.0)
                        });
                    if let Err(error) = &result {
                        eprintln!("surface shader skipped:\n{error}");
                    }
                    result
                })
                .as_ref()
                .ok()
                .map(|pipelines| (key, pipelines.clone()))
        })
        .collect()
}

type ParticlePipelines =
    HashMap<(String, u32), Result<[Arc<GraphicsPipeline>; 2], String>>;

/// Alpha and additive particle pipelines per emitter of `shaders`, an
/// emitter's particle shader source, built for `passes` at `samples`.
/// Emitters whose shader fails to build are left out. Shaders no emitter
/// uses any more leave the cache.
fn particle_pipelines(
    cache: &mut ParticlePipelines,
    queue: &Arc<Queue>,
    passes: &MainPasses,
    samples: u32,
    shaders: &HashMap<bevy_ecs::entity::Entity, String>,
) -> HashMap<bevy_ecs::entity::Entity, [Arc<GraphicsPipeline>; 2]> {
    cache.retain(|(source, _), _| shaders.values().any(|s| s == source));
    shaders
        .iter()
        .filter_map(|(&entity, source)| {
            cache
                .entry((source.clone(), samples))
                .or_insert_with(|| {
                    let result = particle_fragment_glsl(source)
                        .and_then(|glsl| {
                            compile_glsl(queue.device(), "fragment", &glsl)
                        })
                        .and_then(|module| {
                            let build = |additive| {
                                create_particle_pipeline(
                                    queue,
                                    &passes.render_pass,
                                    samples,
                                    additive,
                                    Some(module.clone()),
                                )
                                .map_err(|error| error.0)
                            };
                            Ok([build(false)?, build(true)?])
                        });
                    if let Err(error) = &result {
                        eprintln!("particle shader skipped:\n{error}");
                    }
                    result
                })
                .as_ref()
                .ok()
                .map(|pipelines| (entity, pipelines.clone()))
        })
        .collect()
}

/// Pipeline, framebuffer, shadow map, and comparison sampler.
type ShadowPass = (
    Arc<GraphicsPipeline>,
    Arc<Framebuffer>,
    Arc<ImageView>,
    Arc<Sampler>,
);

/// Creates the depth-only shadow pipeline, its fixed-size shadow map and
/// framebuffer, and the comparison sampler the main pass reads it with.
/// The shadow pass leaves the map in the layout the scene pass samples it
/// in, so the render pass performs the transition instead of a barrier.
fn shadow_map_final_layout() -> ImageLayout {
    FramePass::Shadow
        .next_layout(FrameResource::ShadowMap)
        .expect("the scene pass samples the shadow map")
}

fn create_shadow_pass(
    queue: &Arc<Queue>,
    memory_allocator: &Arc<StandardMemoryAllocator>,
    main_pipeline: &Arc<GraphicsPipeline>,
) -> Result<ShadowPass, SceneRenderError> {
    let device = queue.device().clone();
    let render_pass = vulkano::single_pass_renderpass!(
        device.clone(),
        attachments: {
            depth: {
                format: Format::D32_SFLOAT,
                samples: 1,
                load_op: Clear,
                store_op: Store,
                initial_layout: ImageLayout::Undefined,
                final_layout: shadow_map_final_layout(),
            }
        },
        pass: {
            color: [],
            depth_stencil: {depth}
        }
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    let (size, _) = shadow_settings(QualityProfile::Balanced);
    let (framebuffer, shadow_map) =
        create_shadow_target(&render_pass, memory_allocator, size)?;
    // Outside the map the border depth of 1.0 compares as lit.
    let sampler = Sampler::new(
        device.clone(),
        SamplerCreateInfo {
            address_mode: [SamplerAddressMode::ClampToBorder; 3],
            border_color: BorderColor::FloatOpaqueWhite,
            compare: Some(CompareOp::LessOrEqual),
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    let vertex = shadow_vertex_shader::load(device.clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("shadow vertex entry point is missing".into())
        })?;
    let subpass = Subpass::from(render_pass, 0)
        .ok_or_else(|| SceneRenderError("shadow subpass is missing".into()))?;
    let pipeline = GraphicsPipeline::new(
        device,
        None,
        GraphicsPipelineCreateInfo {
            stages: [PipelineShaderStageCreateInfo::new(vertex.clone())]
                .into_iter()
                .collect(),
            vertex_input_state: Some(
                SceneVertex::per_vertex()
                    .definition(&vertex)
                    .map_err(|error| SceneRenderError(error.to_string()))?,
            ),
            input_assembly_state: Some(InputAssemblyState::default()),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState {
                cull_mode: CullMode::Back,
                front_face: FrontFace::CounterClockwise,
                // Slope-scaled bias keeps lit surfaces from shadowing
                // themselves at grazing light angles.
                depth_bias: Some(DepthBiasState {
                    constant_factor: 1.25,
                    clamp: 0.0,
                    slope_factor: 1.75,
                }),
                ..Default::default()
            }),
            multisample_state: Some(MultisampleState::default()),
            depth_stencil_state: Some(DepthStencilState {
                depth: Some(DepthState::simple()),
                ..Default::default()
            }),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(main_pipeline.layout().clone())
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    Ok((pipeline, framebuffer, shadow_map, sampler))
}

/// Creates a `size`-texel square shadow map and its framebuffer.
fn create_shadow_target(
    render_pass: &Arc<RenderPass>,
    memory_allocator: &Arc<StandardMemoryAllocator>,
    size: u32,
) -> Result<(Arc<Framebuffer>, Arc<ImageView>), SceneRenderError> {
    let image = Image::new(
        memory_allocator.clone(),
        ImageCreateInfo {
            format: Format::D32_SFLOAT,
            extent: [size, size, 1],

            usage: ImageUsage::DEPTH_STENCIL_ATTACHMENT | ImageUsage::SAMPLED,
            ..Default::default()
        },
        AllocationCreateInfo {
            memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    name_object(&*image, "Shadow map");
    let shadow_map = ImageView::new_default(image)
        .map_err(|error| SceneRenderError(error.to_string()))?;
    let framebuffer = Framebuffer::new(
        render_pass.clone(),
        FramebufferCreateInfo {
            attachments: vec![shadow_map.clone()],
            ..Default::default()
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    Ok((framebuffer, shadow_map))
}

/// Unlit camera-facing quads for particles, in the HDR subpass. They test
/// against scene depth without writing it. `additive` adds light; otherwise
/// the premultiplied color covers what is behind by its alpha.
fn create_particle_pipeline(
    queue: &Arc<Queue>,
    render_pass: &Arc<RenderPass>,
    samples: u32,
    additive: bool,
    fragment: Option<Arc<vulkano::shader::ShaderModule>>,
) -> Result<Arc<GraphicsPipeline>, SceneRenderError> {
    let device = queue.device().clone();
    let vertex = particle_vertex_shader::load(device.clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("particle vertex entry point is missing".into())
        })?;
    let fragment = fragment
        .map_or_else(|| particle_fragment_shader::load(device.clone()), Ok)
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("particle fragment entry point is missing".into())
        })?;
    let stages = [
        PipelineShaderStageCreateInfo::new(vertex.clone()),
        PipelineShaderStageCreateInfo::new(fragment),
    ];
    let layout = PipelineLayout::new(
        device.clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
            .into_pipeline_layout_create_info(device.clone())
            .map_err(|error| SceneRenderError(error.to_string()))?,
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    let subpass = Subpass::from(render_pass.clone(), 0).ok_or_else(|| {
        SceneRenderError("particle subpass is missing".into())
    })?;
    let blend = if additive {
        AttachmentBlend {
            src_color_blend_factor: BlendFactor::One,
            dst_color_blend_factor: BlendFactor::One,
            src_alpha_blend_factor: BlendFactor::Zero,
            dst_alpha_blend_factor: BlendFactor::One,
            ..AttachmentBlend::additive()
        }
    } else {
        AttachmentBlend {
            src_color_blend_factor: BlendFactor::One,
            src_alpha_blend_factor: BlendFactor::One,
            ..AttachmentBlend::alpha()
        }
    };
    GraphicsPipeline::new(
        device,
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(
                ParticleVertex::per_instance()
                    .definition(&vertex)
                    .map_err(|error| SceneRenderError(error.to_string()))?,
            ),
            input_assembly_state: Some(InputAssemblyState::default()),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState {
                cull_mode: CullMode::None,
                ..Default::default()
            }),
            multisample_state: Some(MultisampleState {
                rasterization_samples: sample_count(samples)?,
                ..Default::default()
            }),
            depth_stencil_state: Some(DepthStencilState {
                depth: Some(DepthState {
                    write_enable: false,
                    compare_op: CompareOp::Less,
                }),
                ..Default::default()
            }),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState {
                    blend: Some(blend),
                    ..Default::default()
                },
            )),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// One GPU particle emitter's compute step: descriptor set, push
/// constants and workgroups.
type GpuParticleStep =
    (Arc<DescriptorSet>, super::gpu_particles::shader::Step, u32);

/// One GPU skin job's compute step: descriptor set, push constants and
/// workgroups.
type SkinStep = (Arc<DescriptorSet>, super::gpu_skinning::shader::Counts, u32);

/// One instanced particle draw: additive, sprite, first instance, count.
type ParticleDraw = (bool, u32, u32, u32, Option<bevy_ecs::entity::Entity>);

/// Particle instances of every batch, alpha batches far to near and each
/// one's particles far to near, then additive batches. Returns the vertices
/// and, per draw, whether it is additive, its sprite, first instance and
/// count.
/// Each batch draws its first [`particles_kept`] particles at `share`: the
/// oldest, so the kept set holds still from frame to frame.
// ponytail: a prefix leans toward old particles; keep by spawn number if
// thinned emitters look lopsided.
fn particle_draws(
    batches: &[crate::runtime::ParticleBatch],
    eye: [f32; 3],
    forward: [f32; 3],
    share: f64,
) -> (Vec<ParticleVertex>, Vec<ParticleDraw>) {
    use crate::runtime::{ParticleBlend, ParticleSprite};
    let depth = |p: [f32; 3]| {
        (0..3)
            .map(|axis| (p[axis] - eye[axis]) * forward[axis])
            .sum::<f32>()
    };
    let mut order = batches
        .iter()
        .map(|batch| {
            let count = batch.instances.len().max(1) as f32;
            let mean = batch
                .instances
                .iter()
                .fold(0.0, |sum, instance| sum + depth(instance.position))
                / count;
            (batch.blend == ParticleBlend::Additive, -mean, batch)
        })
        .collect::<Vec<_>>();
    order.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let mut vertices = Vec::new();
    let mut draws = Vec::new();
    for (additive, _, batch) in order {
        let first = vertices.len() as u32;
        let kept = particles_kept(batch.instances.len(), share);
        let mut instances = batch.instances[..kept].to_vec();
        if !additive {
            instances.sort_by(|a, b| {
                depth(b.position).total_cmp(&depth(a.position))
            });
        }
        vertices.extend(instances.iter().map(|instance| ParticleVertex {
            center: instance.position,
            size: instance.size,
            color: instance.color,
            stretch: instance.stretch,
            rotation: instance.rotation,
        }));
        let sprite = match batch.sprite {
            ParticleSprite::Soft => 0,
            ParticleSprite::Disc => 1,
            ParticleSprite::Square => 2,
        };
        draws.push((
            additive,
            sprite,
            first,
            instances.len() as u32,
            batch.entity,
        ));
    }
    (vertices, draws)
}

/// Camera right and up in world space, for billboards.
fn camera_right_up(camera: Option<ExtractedCamera>) -> ([f32; 3], [f32; 3]) {
    camera.map_or(([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]), |camera| {
        let m = camera.transform.matrix;
        let axis = |c: usize| {
            Vector3::new(m[c][0], m[c][1], m[c][2])
                .try_normalize(1e-6)
                .map_or([0.0; 3], Into::into)
        };
        (axis(0), axis(1))
    })
}

/// Makes a minimal unlit line pipeline for Scene View helpers.
///
/// We use standard one-pixel Vulkan lines here. Interactive handles can later
/// use triangle geometry when they need thick, portable shapes.
fn create_debug_pipeline(
    queue: Arc<Queue>,
    render_pass: Arc<RenderPass>,
    depth_test: bool,
) -> Result<Arc<GraphicsPipeline>, SceneRenderError> {
    let vertex = debug_vertex_shader::load(queue.device().clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("debug vertex entry point is missing".into())
        })?;
    let fragment = debug_fragment_shader::load(queue.device().clone())
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("debug fragment entry point is missing".into())
        })?;
    let stages = [
        PipelineShaderStageCreateInfo::new(vertex.clone()),
        PipelineShaderStageCreateInfo::new(fragment),
    ];
    let layout = PipelineLayout::new(
        queue.device().clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages(&stages)
            .into_pipeline_layout_create_info(queue.device().clone())
            .map_err(|error| SceneRenderError(error.to_string()))?,
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    // Drawn after tone mapping, so helper colors are exact.
    let subpass = Subpass::from(render_pass, 1)
        .ok_or_else(|| SceneRenderError("debug subpass is missing".into()))?;
    GraphicsPipeline::new(
        queue.device().clone(),
        None,
        GraphicsPipelineCreateInfo {
            stages: stages.into_iter().collect(),
            vertex_input_state: Some(
                DebugVertex::per_vertex()
                    .definition(&vertex)
                    .map_err(|error| SceneRenderError(error.to_string()))?,
            ),
            input_assembly_state: Some(InputAssemblyState {
                topology: PrimitiveTopology::TriangleList,
                ..Default::default()
            }),
            viewport_state: Some(ViewportState::default()),
            rasterization_state: Some(RasterizationState {
                cull_mode: CullMode::None,
                ..Default::default()
            }),
            multisample_state: Some(MultisampleState::default()),
            depth_stencil_state: Some(if depth_test {
                DepthStencilState {
                    // Spatial helpers hide behind meshes without changing the
                    // scene depth buffer.
                    depth: Some(DepthState {
                        write_enable: false,
                        ..DepthState::simple()
                    }),
                    ..Default::default()
                }
            } else {
                DepthStencilState::default()
            }),
            color_blend_state: Some(ColorBlendState::with_attachment_states(
                1,
                ColorBlendAttachmentState {
                    // Line colors carry alpha, for example fading grid lines.
                    blend: Some(AttachmentBlend::alpha()),
                    ..Default::default()
                },
            )),
            dynamic_state: [DynamicState::Viewport, DynamicState::Scissor]
                .into_iter()
                .collect(),
            subpass: Some(PipelineSubpassType::BeginRenderPass(subpass)),
            ..GraphicsPipelineCreateInfo::layout(layout)
        },
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

pub(super) fn create_compute_pipeline(
    queue: &Arc<Queue>,
    module: Result<
        Arc<vulkano::shader::ShaderModule>,
        vulkano::Validated<vulkano::VulkanError>,
    >,
) -> Result<Arc<ComputePipeline>, SceneRenderError> {
    let shader = module
        .map_err(|error| SceneRenderError(error.to_string()))?
        .entry_point("main")
        .ok_or_else(|| {
            SceneRenderError("compute entry point is missing".into())
        })?;
    let stage = PipelineShaderStageCreateInfo::new(shader);
    let layout = PipelineLayout::new(
        queue.device().clone(),
        PipelineDescriptorSetLayoutCreateInfo::from_stages([&stage])
            .into_pipeline_layout_create_info(queue.device().clone())
            .map_err(|error| SceneRenderError(error.to_string()))?,
    )
    .map_err(|error| SceneRenderError(error.to_string()))?;
    ComputePipeline::new(
        queue.device().clone(),
        None,
        ComputePipelineCreateInfo::stage_layout(stage, layout),
    )
    .map_err(|error| SceneRenderError(error.to_string()))
}

/// Compiles new condition shaders and returns the usable ones in order,
/// each with its shader.
fn condition_pipelines(
    cache: &mut HashMap<String, Result<Arc<ComputePipeline>, String>>,
    queue: &Arc<Queue>,
    shaders: Vec<crate::runtime::ExtractedConditionShader>,
) -> Vec<(
    Arc<ComputePipeline>,
    crate::runtime::ExtractedConditionShader,
)> {
    cache.retain(|source, _| {
        shaders.iter().any(|shader| shader.source == *source)
    });
    shaders
        .into_iter()
        .filter_map(|shader| {
            let source = &shader.source;
            cache
                .entry(source.clone())
                .or_insert_with(|| {
                    let result = compile_condition_shader(queue, source);
                    if let Err(error) = &result {
                        eprintln!("GPU condition shader skipped:\n{error}");
                    }
                    result
                })
                .as_ref()
                .ok()
                .cloned()
                .map(|pipeline| (pipeline, shader))
        })
        .collect()
}

/// Builds a [`GpuConditionShader`](crate::runtime::GpuConditionShader) with
/// the system `glslc`. Runtime compilation needs no extra crate; the
/// shaderc that `vulkano-shaders` links is build-time only.
fn compile_condition_shader(
    queue: &Arc<Queue>,
    source: &str,
) -> Result<Arc<ComputePipeline>, String> {
    let glsl = format!(
        "#version 450\nlayout(local_size_x = 256) in;\n{}\n\
         layout(set = 0, binding = 14) readonly buffer ConditionParams \
         {{ vec4 values[]; }} condition_params;\n{source}\n\
         void main() {{\n\
         uint body_index = gl_GlobalInvocationID.x;\n\
         if (body_index >= pc.body_count) return;\n\
         PhysicsState body = bodies.data[body_index];\n\
         condition(body);\n\
         bodies.data[body_index] = body;\n\
         }}\n",
        include_str!("../shaders/physics_abi.glsl")
    );
    let module = compile_glsl(queue.device(), "compute", &glsl)?;
    create_compute_pipeline(queue, Ok(module)).map_err(|error| error.0)
}

/// Compiles GLSL for `stage` ("compute", "fragment") with the system
/// `glslc`, or the compiler `RUSTING_GLSLC` names. Errors are its messages.
fn compile_glsl(
    device: &Arc<Device>,
    stage: &str,
    glsl: &str,
) -> Result<Arc<vulkano::shader::ShaderModule>, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let compiler =
        std::env::var_os("RUSTING_GLSLC").unwrap_or_else(|| "glslc".into());
    let mut child = Command::new(&compiler)
        .args([&format!("-fshader-stage={stage}"), "-o", "-", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {compiler:?}: {error}"))?;
    // glslc reads all input before writing, so this cannot deadlock.
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(glsl.as_bytes())
        .map_err(|error| error.to_string())?;
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let words = vulkano::shader::spirv::bytes_to_words(&output.stdout)
        .map_err(|error| error.to_string())?;
    // Safety: the SPIR-V comes straight from glslc, which only emits valid
    // modules, and vulkano validates entry points and layouts when the
    // pipeline is built.
    unsafe {
        vulkano::shader::ShaderModule::new(
            device.clone(),
            vulkano::shader::ShaderModuleCreateInfo::new(&words),
        )
    }
    .map_err(|error| error.to_string())
}

/// Version of the Rusting shader language (`#rusting surface 1`,
/// `#rusting post 1`) this engine speaks; see `src/shaders/surface.glsl`
/// and `src/shaders/post.glsl`.
pub const SHADER_VERSION: u32 = 1;

/// The fog hook: `source`, a `#rusting fog 1` shader, or empty for none.
fn fog_hook(source: &str) -> Hook<'_> {
    (
        "fog",
        source,
        (
            "fog_shader.glsl",
            include_str!("../shaders/fog_shader.glsl"),
        ),
    )
}

/// The scene fragment shader with `source`, a material's
/// `#rusting surface 1` shader, as its `surface` hook and `fog`, the
/// scene's fog shader, as its `fog` hook; either may be empty.
fn surface_fragment_glsl(source: &str, fog: &str) -> Result<String, String> {
    hooked_glsl(
        include_str!("../shaders/fragment/scene.frag"),
        &[
            (
                "surface",
                source,
                ("surface.glsl", include_str!("../shaders/surface.glsl")),
            ),
            fog_hook(fog),
        ],
    )
}

/// The tone-map fragment shader with `source`, a `#rusting post 1`
/// shader, as its `post` hook.
fn post_fragment_glsl(source: &str) -> Result<String, String> {
    hooked_glsl(
        include_str!("../shaders/fragment/tonemap.frag"),
        &[(
            "post",
            source,
            ("post.glsl", include_str!("../shaders/post.glsl")),
        )],
    )
}

/// The sky fragment shader with `source`, a `#rusting sky 1` shader, as
/// its `sky` hook and `fog`, the scene's fog shader, as its `fog` hook;
/// either may be empty.
fn sky_fragment_glsl(source: &str, fog: &str) -> Result<String, String> {
    hooked_glsl(
        include_str!("../shaders/fragment/sky.frag"),
        &[
            (
                "sky",
                source,
                (
                    "sky_shader.glsl",
                    include_str!("../shaders/sky_shader.glsl"),
                ),
            ),
            fog_hook(fog),
        ],
    )
}

/// The particle fragment shader with `source`, a `#rusting particle 1`
/// shader, as its `particle` hook.
fn particle_fragment_glsl(source: &str) -> Result<String, String> {
    hooked_glsl(
        include_str!("../shaders/fragment/particle.frag"),
        &[(
            "particle",
            source,
            (
                "particle_shader.glsl",
                include_str!("../shaders/particle_shader.glsl"),
            ),
        )],
    )
}

/// A hook of an engine shader: its kind, the user's `#rusting {kind} 1`
/// source (empty keeps the built-in default), and the hook's file name and
/// text.
type Hook<'a> = (&'a str, &'a str, (&'a str, &'a str));

/// `frag` with its includes inlined and the include of each hook's file
/// replaced by that file's text and the body of the hook's source.
fn hooked_glsl(frag: &str, hooks: &[Hook<'_>]) -> Result<String, String> {
    let mut glsl = frag.to_owned();
    for (file, text) in [
        ("fog.glsl", include_str!("../shaders/fog.glsl")),
        ("sky.glsl", include_str!("../shaders/sky.glsl")),
    ] {
        glsl = glsl.replacen(&format!("#include \"{file}\""), text, 1);
    }
    for &(kind, source, (hook_file, hook)) in hooks {
        let hook_line = format!("#include \"{hook_file}\"");
        if !glsl.contains(&hook_line) {
            return Err(format!("the engine shader lost its {hook_line}"));
        }
        let text = if source.is_empty() {
            hook.to_owned()
        } else {
            format!(
                "#define RUSTING_CUSTOM_{}\n{hook}\n{}\n",
                kind.to_uppercase(),
                hook_body(kind, source)?
            )
        };
        glsl = glsl.replacen(&hook_line, &text, 1);
    }
    Ok(glsl)
}

/// The body of `source`, a `#rusting {kind} 1` shader, after its header,
/// with a `#line` so compiler messages count lines of `source`.
fn hook_body(kind: &str, source: &str) -> Result<String, String> {
    let mut lines = source.lines();
    let header = lines
        .by_ref()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    let mut words = header.split_whitespace();
    if words.next() != Some("#rusting") || words.next() != Some(kind) {
        return Err(format!(
            "a {kind} shader starts with `#rusting {kind} {SHADER_VERSION}`, \
             not {header:?}"
        ));
    }
    match words.next().map(str::parse::<u32>) {
        Some(Ok(SHADER_VERSION)) if words.next().is_none() => {}
        _ => {
            return Err(format!(
                "this engine runs {kind} shader version {SHADER_VERSION}, \
                 not {header:?}"
            ));
        }
    }
    let header_line = source
        .lines()
        .position(|line| line == header)
        .unwrap_or_default();
    Ok(format!(
        "#line {}\n{}",
        header_line + 2,
        lines.collect::<Vec<_>>().join("\n")
    ))
}

/// Element `index` of the Halton sequence in `base`, in [0, 1).
fn halton(mut index: u32, base: u32) -> f32 {
    let mut fraction = 1.0;
    let mut value = 0.0;
    while index > 0 {
        fraction /= base as f32;
        value += fraction * (index % base) as f32;
        index /= base;
    }
    value
}

fn view_projection(
    camera: Option<ExtractedCamera>,
    extent: [u32; 2],
) -> Matrix4<f32> {
    let aspect = extent[0] as f32 / extent[1].max(1) as f32;
    if let Some(camera) = camera {
        let world_from_camera = matrix_from_array(camera.transform.matrix);
        let view = world_from_camera
            .try_inverse()
            .unwrap_or_else(Matrix4::identity);
        let projection = match camera.projection {
            Projection::Perspective {
                vertical_fov_radians,
                near,
                far,
            } => Perspective3::new(aspect, vertical_fov_radians, near, far)
                .to_homogeneous(),
            Projection::Orthographic {
                vertical_size,
                near,
                far,
            } => Orthographic3::new(
                -vertical_size * aspect * 0.5,
                vertical_size * aspect * 0.5,
                -vertical_size * 0.5,
                vertical_size * 0.5,
                near,
                far,
            )
            .to_homogeneous(),
        };
        vulkan_clip_correction() * projection * view
    } else {
        vulkan_clip_correction()
            * Perspective3::new(
                aspect,
                std::f32::consts::FRAC_PI_3,
                0.1,
                1_000.0,
            )
            .to_homogeneous()
    }
}

/// Converts nalgebra's OpenGL clip convention to Vulkan's inverted Y axis and
/// zero-to-one depth range.
fn vulkan_clip_correction() -> Matrix4<f32> {
    Matrix4::new(
        1.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0,
        0.0, 1.0,
    )
}

fn matrix_from_array(matrix: [[f32; 4]; 4]) -> Matrix4<f32> {
    Matrix4::from_column_slice(&matrix.concat())
}

/// Active camera position and view direction; the default camera looks down
/// -Z from the origin.
fn camera_eye_forward(camera: Option<ExtractedCamera>) -> ([f32; 3], [f32; 3]) {
    camera.map_or(([0.0; 3], [0.0, 0.0, -1.0]), |camera| {
        (
            light_position(camera.transform.matrix),
            light_direction(camera.transform.matrix),
        )
    })
}

/// Directional shadow cascades, one per tile of a 2x2 shadow map atlas.
const SHADOW_CASCADES: usize = 4;
/// Light views the shadow upload holds: the six cube faces of a point light.
const SHADOW_VIEWS: usize = 6;

/// Camera depth where cascade `index` ends: mostly logarithmic splits, so
/// the near cascades are small and sharp, blended with even ones so the
/// far cascades are not too thin.
fn cascade_end(index: usize, distance: f32) -> f32 {
    let near = 0.5_f32.min(distance);
    let t = (index + 1) as f32 / SHADOW_CASCADES as f32;
    let logarithmic = near * (distance / near).powf(t);
    0.75 * logarithmic + 0.25 * distance * t
}

/// Bounding sphere of the camera frustum between depths `near` and `far`
/// along `forward`. The radius does not change as the camera turns, so the
/// cascade's texel size stays fixed.
fn frustum_slice_sphere(
    clip: &Matrix4<f32>,
    eye: [f32; 3],
    forward: [f32; 3],
    near: f32,
    far: f32,
) -> (Point3<f32>, f32) {
    let inverse = clip.try_inverse().unwrap_or_else(Matrix4::identity);
    let eye = Point3::from(eye);
    let forward = Vector3::from(forward);
    let depth = |point: &Point3<f32>| (point - eye).dot(&forward);
    let mut corners = Vec::with_capacity(8);
    for [x, y] in [[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]] {
        let start = inverse.transform_point(&Point3::new(x, y, 0.0));
        let end = inverse.transform_point(&Point3::new(x, y, 1.0));
        let (start_depth, end_depth) = (depth(&start), depth(&end));
        for slice in [near, far] {
            let t = ((slice - start_depth)
                / (end_depth - start_depth).max(f32::EPSILON))
            .clamp(0.0, 1.0);
            corners.push(start + (end - start) * t);
        }
    }
    let center = Point3::from(
        corners
            .iter()
            .map(|corner| corner.coords)
            .sum::<Vector3<f32>>()
            / corners.len() as f32,
    );
    let radius = corners
        .iter()
        .map(|corner| (corner - center).norm())
        .fold(0.0, f32::max)
        .max(0.01);
    (center, radius)
}

/// Orthographic light-space transform of one cascade: a box around the
/// sphere, moved in whole texels so shadow edges do not shimmer as the
/// camera moves. Casters up to `reach` further toward the light still land
/// in the map.
fn cascade_view_projection(
    center: Point3<f32>,
    radius: f32,
    direction: [f32; 3],
    reach: f32,
    texels: u32,
) -> Matrix4<f32> {
    let direction = Vector3::from(direction).normalize();
    let up = if direction.y.abs() > 0.99 {
        Vector3::z()
    } else {
        Vector3::y()
    };
    let view =
        Matrix4::look_at_rh(&Point3::origin(), &Point3::from(direction), &up);
    let light = view.transform_point(&center);
    let texel = 2.0 * radius / texels.max(1) as f32;
    let x = (light.x / texel).round() * texel;
    let y = (light.y / texel).round() * texel;
    // The view looks down -z, so distance from the light plane is -z.
    let projection = Orthographic3::new(
        x - radius,
        x + radius,
        y - radius,
        y + radius,
        -light.z - radius - reach,
        -light.z + radius,
    )
    .to_homogeneous();
    vulkan_clip_correction() * projection * view
}

/// What the shadow map looks along.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ShadowView {
    /// Orthographic, along this world direction, around the camera.
    Directional([f32; 3]),
    /// Perspective, from the spot light over its cone.
    Spot {
        position: [f32; 3],
        direction: [f32; 3],
        outer_angle: f32,
        range: f32,
    },
    /// Six perspective cube faces around the point light.
    Point { position: [f32; 3], range: f32 },
}

/// Light matrices of a point light's cube faces, in the order +X, -X, +Y,
/// -Y, +Z, -Z the fragment shader picks them by.
fn point_shadow_view_projections(
    position: [f32; 3],
    range: f32,
    texels: u32,
) -> Vec<Matrix4<f32>> {
    let eye = Point3::from(position);
    // Each face reaches two texels past 90 degrees, so the edge PCF taps
    // stay on the face's own tile.
    let fov = 2.0 * (1.0 + 4.0 / texels.max(8) as f32).atan();
    let near = (range * 0.002).max(0.02);
    let projection = Perspective3::new(1.0, fov, near, range).to_homogeneous();
    [
        (Vector3::x(), Vector3::y()),
        (-Vector3::x(), Vector3::y()),
        (Vector3::y(), Vector3::z()),
        (-Vector3::y(), Vector3::z()),
        (Vector3::z(), Vector3::y()),
        (-Vector3::z(), Vector3::y()),
    ]
    .into_iter()
    .map(|(direction, up)| {
        let view = Matrix4::look_at_rh(&eye, &(eye + direction), &up);
        vulkan_clip_correction() * projection * view
    })
    .collect()
}

/// Perspective light matrix covering a spot light's cone out to its range.
fn spot_shadow_view_projection(
    position: [f32; 3],
    direction: [f32; 3],
    outer_angle: f32,
    range: f32,
) -> Matrix4<f32> {
    let eye = Point3::from(position);
    let direction = Vector3::from(direction);
    let up = if direction.y.abs() > 0.99 {
        Vector3::z()
    } else {
        Vector3::y()
    };
    let view = Matrix4::look_at_rh(&eye, &(eye + direction), &up);
    // A little wider than the cone so the edge PCF taps stay on the map.
    let fov = (2.0 * outer_angle + 0.1).clamp(0.1, 3.0);
    let near = (range * 0.002).max(0.02);
    let projection = Perspective3::new(1.0, fov, near, range).to_homogeneous();
    vulkan_clip_correction() * projection * view
}

fn light_position(matrix: [[f32; 4]; 4]) -> [f32; 3] {
    let matrix = matrix_from_array(matrix);
    [matrix[(0, 3)], matrix[(1, 3)], matrix[(2, 3)]]
}

/// Starting arena size for instance uploads (about 2,700 instances).
const INSTANCE_ARENA_BYTES: DeviceSize = 256 * 1024;
/// First arena size of each frame context's transient allocator.
const TRANSIENT_ARENA_BYTES: DeviceSize = 64 * 1024;

/// Device properties the renderer depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceLimits {
    pub api_version: vulkano::Version,
    pub max_compute_work_group_invocations: u32,
    pub max_compute_work_group_size_x: u32,
    pub max_push_constants_size: u32,
    pub max_storage_buffer_range: u32,
    /// `D32_SFLOAT` usable as an optimal-tiling depth attachment.
    pub depth_attachment: bool,
}

/// Lowest device `SceneRenderer` accepts, sized for Intel UHD 620-class
/// integrated GPUs. Every value is the Vulkan-required minimum except
/// compute invocations, which the physics shader's `local_size_x = 256`
/// needs, and the version (1.1, exposed by every current Intel, AMD, NVIDIA
/// and Mesa driver for that hardware generation).
pub const LOW_END_BASELINE: DeviceLimits = DeviceLimits {
    api_version: vulkano::Version::V1_1,
    max_compute_work_group_invocations: 256,
    max_compute_work_group_size_x: 256,
    max_push_constants_size: 128,
    max_storage_buffer_range: 1 << 27,
    depth_attachment: true,
};

impl DeviceLimits {
    pub fn of(device: &vulkano::device::physical::PhysicalDevice) -> Self {
        let properties = device.properties();
        Self {
            api_version: properties.api_version,
            max_compute_work_group_invocations: properties
                .max_compute_work_group_invocations,
            max_compute_work_group_size_x: properties
                .max_compute_work_group_size[0],
            max_push_constants_size: properties.max_push_constants_size,
            max_storage_buffer_range: properties.max_storage_buffer_range,
            depth_attachment: device
                .format_properties(Format::D32_SFLOAT)
                .is_ok_and(|format| {
                    format.optimal_tiling_features.intersects(
                        vulkano::format::FormatFeatures::DEPTH_STENCIL_ATTACHMENT,
                    )
                }),
        }
    }

    /// Names every property below `baseline`; empty when the device meets it.
    pub fn shortfalls(&self, baseline: &DeviceLimits) -> Vec<String> {
        let mut missing = Vec::new();
        if self.api_version < baseline.api_version {
            missing.push(format!(
                "Vulkan {} < {}",
                self.api_version, baseline.api_version
            ));
        }
        for (name, have, need) in [
            (
                "maxComputeWorkGroupInvocations",
                self.max_compute_work_group_invocations,
                baseline.max_compute_work_group_invocations,
            ),
            (
                "maxComputeWorkGroupSize[0]",
                self.max_compute_work_group_size_x,
                baseline.max_compute_work_group_size_x,
            ),
            (
                "maxPushConstantsSize",
                self.max_push_constants_size,
                baseline.max_push_constants_size,
            ),
            (
                "maxStorageBufferRange",
                self.max_storage_buffer_range,
                baseline.max_storage_buffer_range,
            ),
        ] {
            if have < need {
                missing.push(format!("{name} {have} < {need}"));
            }
        }
        if baseline.depth_attachment && !self.depth_attachment {
            missing.push("D32_SFLOAT depth attachment unsupported".into());
        }
        missing
    }
}

/// One optional device capability. `supported` is what the GPU offers;
/// `enabled` is what the logical device turned on. Passes may only use it
/// when both hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capability {
    pub supported: bool,
    pub enabled: bool,
}

impl Capability {
    pub fn usable(self) -> bool {
        self.supported && self.enabled
    }
}

/// Optional GPU features above [`LOW_END_BASELINE`] that later passes can
/// use, with the baseline path as their fallback.
#[derive(bevy_ecs::prelude::Resource, Clone, Debug, PartialEq, Eq)]
pub struct RendererCapabilities {
    pub device_name: String,
    pub integrated_gpu: bool,
    /// Size of the largest device-local memory heap.
    pub device_local_bytes: DeviceSize,
    pub multi_draw_indirect: Capability,
    /// `vkCmdDrawIndexedIndirectCount`, core in 1.2 or via the KHR extension.
    pub draw_indirect_count: Capability,
    /// Descriptor indexing features needed for a bindless texture array.
    pub bindless_textures: Capability,
    /// `VK_EXT_memory_budget` for live memory usage.
    pub memory_budget: Capability,
    /// Anisotropic texture filtering (`samplerAnisotropy`).
    pub sampler_anisotropy: Capability,
    /// GPU timestamps on graphics and compute queues; needs no enabling.
    pub timestamp_queries: bool,
    /// Scene MSAA sample count: 4 or 2 when the device can multisample the
    /// HDR and depth targets and resolve depth (Vulkan 1.2 or
    /// `VK_KHR_depth_stencil_resolve`), otherwise 1.
    pub msaa_samples: u32,
    /// The device can also run 2x MSAA; Vulkan only guarantees 4x.
    pub msaa_2x: bool,
}

/// Largest scene MSAA count up to 4 both target kinds support, or 1 without
/// a depth resolve.
// ponytail: capped at 4x; 8x costs twice the target memory for little gain
// at editor resolutions.
fn msaa_sample_count(
    color: SampleCounts,
    depth: SampleCounts,
    depth_resolve: bool,
) -> u32 {
    let both = color.intersection(depth);
    if !depth_resolve {
        1
    } else if both.intersects(SampleCounts::SAMPLE_4) {
        4
    } else if both.intersects(SampleCounts::SAMPLE_2) {
        2
    } else {
        1
    }
}

/// Whether frames at a resolved profile use MSAA when the device has it.
/// `Eco` skips it to save target memory and bandwidth.
pub fn msaa_enabled(profile: QualityProfile) -> bool {
    profile != QualityProfile::Eco
}

/// Scene sample count for an anti-aliasing setting on this device. A count
/// the device lacks falls back to the next lower one.
pub fn scene_sample_count(
    antialiasing: Antialiasing,
    profile: QualityProfile,
    capabilities: &RendererCapabilities,
) -> u32 {
    let wanted = match antialiasing {
        Antialiasing::Auto if msaa_enabled(profile) => 4,
        Antialiasing::Auto
        | Antialiasing::Off
        | Antialiasing::Fxaa
        | Antialiasing::Taa => 1,
        Antialiasing::Msaa2 => 2,
        Antialiasing::Msaa4 => 4,
    };
    match wanted.min(capabilities.msaa_samples) {
        2 if !capabilities.msaa_2x => 1,
        samples => samples,
    }
}

/// Which optional features a feature and extension set provides, in the
/// order of the [`RendererCapabilities`] fields.
fn optional_features(
    features: &vulkano::device::DeviceFeatures,
    extensions: &DeviceExtensions,
) -> [bool; 5] {
    [
        features.multi_draw_indirect,
        features.draw_indirect_count || extensions.khr_draw_indirect_count,
        features.runtime_descriptor_array
            && features.descriptor_binding_partially_bound
            && features.shader_sampled_image_array_non_uniform_indexing
            && features.descriptor_binding_variable_descriptor_count,
        extensions.ext_memory_budget,
        features.sampler_anisotropy,
    ]
}

impl RendererCapabilities {
    pub fn detect(device: &vulkano::device::Device) -> Self {
        let physical = device.physical_device();
        let supported = optional_features(
            physical.supported_features(),
            physical.supported_extensions(),
        );
        let enabled = optional_features(
            device.enabled_features(),
            device.enabled_extensions(),
        );
        let [multi_draw_indirect, draw_indirect_count, bindless_textures, memory_budget, sampler_anisotropy] =
            std::array::from_fn(|index| Capability {
                supported: supported[index],
                enabled: enabled[index],
            });
        let properties = physical.properties();
        let depth_resolve =
            (device.api_version() >= vulkano::Version::V1_2
                || device.enabled_extensions().khr_depth_stencil_resolve)
                && properties.supported_depth_resolve_modes.is_some_and(
                    |modes| modes.intersects(ResolveModes::SAMPLE_ZERO),
                );
        Self {
            device_name: properties.device_name.clone(),
            integrated_gpu: properties.device_type
                == vulkano::device::physical::PhysicalDeviceType::IntegratedGpu,
            device_local_bytes: physical
                .memory_properties()
                .memory_heaps
                .iter()
                .filter(|heap| {
                    heap.flags.intersects(MemoryHeapFlags::DEVICE_LOCAL)
                })
                .map(|heap| heap.size)
                .max()
                .unwrap_or(0),
            multi_draw_indirect,
            draw_indirect_count,
            bindless_textures,
            memory_budget,
            sampler_anisotropy,
            timestamp_queries: properties.timestamp_compute_and_graphics,
            msaa_samples: msaa_sample_count(
                properties.framebuffer_color_sample_counts,
                properties.framebuffer_depth_sample_counts,
                depth_resolve,
            ),
            msaa_2x: depth_resolve
                && properties
                    .framebuffer_color_sample_counts
                    .intersection(properties.framebuffer_depth_sample_counts)
                    .intersects(SampleCounts::SAMPLE_2),
        }
    }
}

/// Turns a requested profile into a concrete one. `Auto` picks `Eco` on
/// integrated GPUs, `Balanced` below 4 GiB of device-local memory, and
/// `High` otherwise.
// ponytail: static device heuristic; switch to measured frame time once
// frame pacing is profiled on real hardware.
pub fn resolve_quality(
    requested: QualityProfile,
    capabilities: &RendererCapabilities,
) -> QualityProfile {
    match requested {
        QualityProfile::Auto if capabilities.integrated_gpu => {
            QualityProfile::Eco
        }
        QualityProfile::Auto if capabilities.device_local_bytes < 4 << 30 => {
            QualityProfile::Balanced
        }
        QualityProfile::Auto => QualityProfile::High,
        concrete => concrete,
    }
}

/// Lights uploaded per frame for a resolved profile. Lights past the budget
/// are dropped and reported in [`RenderCapacityDiagnostics`].
pub fn light_budget(profile: QualityProfile) -> usize {
    match profile {
        QualityProfile::Eco => MAX_LIGHTS / 4,
        QualityProfile::Balanced => MAX_LIGHTS / 2,
        QualityProfile::High | QualityProfile::Auto => MAX_LIGHTS,
    }
}

/// Particles drawn per frame for a resolved profile, CPU and GPU emitters
/// together; past it every emitter draws the same share of its particles.
pub fn particle_budget(profile: QualityProfile) -> usize {
    let most = crate::runtime::MAX_GPU_PARTICLES as usize;
    match profile {
        QualityProfile::Eco => most / 8,
        QualityProfile::Balanced => most / 2,
        QualityProfile::High | QualityProfile::Auto => most,
    }
}

/// Decals painted per frame for a resolved profile; later ones are dropped.
pub fn decal_budget(profile: QualityProfile) -> usize {
    match profile {
        QualityProfile::Eco => MAX_DECALS / 4,
        QualityProfile::Balanced => MAX_DECALS / 2,
        QualityProfile::High | QualityProfile::Auto => MAX_DECALS,
    }
}

/// The frame's CPU particles and GPU emitter slots.
fn particle_total(render_world: &RenderWorld) -> usize {
    render_world
        .particles
        .iter()
        .map(|batch| batch.instances.len())
        .chain(
            render_world
                .gpu_particles
                .iter()
                .map(|batch| batch.max_particles as usize),
        )
        .sum()
}

/// Share of each emitter's particles drawn: all of them when
/// [`particle_total`] fits in `budget`, else budget / total.
fn particle_share(total: usize, budget: usize) -> f64 {
    if total <= budget {
        1.0
    } else {
        budget as f64 / total as f64
    }
}

/// How many of `count` particles an emitter draws at `share`.
fn particles_kept(count: usize, share: f64) -> usize {
    (count as f64 * share) as usize
}

/// Caps one frame's transient uploads at half the largest device-local heap,
/// leaving the rest for meshes, images, and other applications.
// ponytail: static heap size, not live usage; query VK_EXT_memory_budget if
// scenes start sharing the GPU with other heavy workloads.
fn transient_upload_budget(
    heaps: impl IntoIterator<Item = (DeviceSize, MemoryHeapFlags)>,
) -> DeviceSize {
    heaps
        .into_iter()
        .filter(|(_, flags)| flags.intersects(MemoryHeapFlags::DEVICE_LOCAL))
        .map(|(size, _)| size / 2)
        .max()
        .unwrap_or(DeviceSize::MAX)
}

fn light_direction(matrix: [[f32; 4]; 4]) -> [f32; 3] {
    let direction =
        matrix_from_array(matrix) * Vector4::new(0.0, 0.0, -1.0, 0.0);
    let length = (direction.x * direction.x
        + direction.y * direction.y
        + direction.z * direction.z)
        .sqrt();
    if length > f32::EPSILON {
        [
            direction.x / length,
            direction.y / length,
            direction.z / length,
        ]
    } else {
        [0.0, 0.0, -1.0]
    }
}

fn normal_columns(model: Matrix4<f32>) -> [[f32; 4]; 3] {
    let linear = model.fixed_view::<3, 3>(0, 0).into_owned();
    let normal = linear
        .try_inverse()
        .map_or_else(nalgebra::Matrix3::identity, |inverse| {
            inverse.transpose()
        });
    [
        [normal[(0, 0)], normal[(1, 0)], normal[(2, 0)], 0.0],
        [normal[(0, 1)], normal[(1, 1)], normal[(2, 1)], 0.0],
        [normal[(0, 2)], normal[(1, 2)], normal[(2, 2)], 0.0],
    ]
}

#[rustfmt::skip]
mod vertex_shader {
    vulkano_shaders::shader! {
                            ty: "vertex",
                            src: r"
#version 450
layout(location = 0) in vec3 position;
layout(location = 1) in vec3 normal;
layout(location = 2) in vec2 uv;
layout(location = 3) in vec4 tangent;
layout(location = 5) in vec4 color;
layout(location = 0) out vec3 v_normal;
layout(push_constant) uniform Camera {
    mat4 view_projection;
    vec4 eye;
    vec4 ambient;
    vec4 ground_ambient;
    uvec4 light_info;
} camera;
struct PhysicsState {
    mat4 model;
    vec4 velocity;
    vec4 angular_velocity;
    vec4 properties;
    vec4 custom_values;
    uvec4 metadata;
};
layout(set = 0, binding = 0) readonly buffer PhysicsStates {
    PhysicsState data[];
} physics_states;
struct RenderInstance {
    mat4 model;
    mat3x4 normal;
    vec4 color;
    vec4 emissive;
    vec4 surface;
    uvec4 physics;
    vec4 transmission;
    vec4 uv_transform;
};
layout(set = 0, binding = 1) readonly buffer RenderInstances {
    RenderInstance data[];
} render_instances;
layout(location = 1) out vec4 v_color;
layout(location = 2) out vec3 v_world_position;
layout(location = 3) flat out uvec3 v_alpha;
layout(location = 4) out vec2 v_uv;
layout(location = 5) out vec4 v_tangent;
layout(location = 6) flat out vec4 v_emissive;
layout(location = 7) flat out vec4 v_surface;
layout(location = 8) flat out vec4 v_transmission;
// Index into render_instances from the per-frame visible list.
layout(location = 4) in uint instance_index;
void main() {
    RenderInstance instance = render_instances.data[instance_index];
    mat4 model = instance.physics.x == 0xffffffffu
        ? instance.model
        : physics_states.data[instance.physics.x].model;
    vec4 world_position = model * vec4(position, 1.0);
    gl_Position = camera.view_projection * world_position;
    mat3 normal_matrix;
    if (instance.physics.x == 0xffffffffu) {
        normal_matrix = mat3(instance.normal);
    } else {
        // GPU bodies are rotation times scale with no shear, so dividing
        // each column by its squared length gives the inverse-transpose.
        mat3 linear = mat3(model);
        normal_matrix = mat3(
            linear[0] / dot(linear[0], linear[0]),
            linear[1] / dot(linear[1], linear[1]),
            linear[2] / dot(linear[2], linear[2]));
    }
    v_normal = normal_matrix * normal;
    v_color = instance.color * color;
    v_world_position = world_position.xyz;
    v_alpha = instance.physics.yzw;
    v_uv = uv * instance.uv_transform.xy + instance.uv_transform.zw;
    // Tangents follow the surface, so they take the model's linear part.
    v_tangent = vec4(mat3(model) * tangent.xyz, tangent.w);
    v_emissive = instance.emissive;
    v_surface = instance.surface;
    v_transmission = instance.transmission;
}
"
                        }
}

#[rustfmt::skip]
mod fragment_shader {
    vulkano_shaders::shader! {
                            ty: "fragment",
                            include: ["src/shaders"],
                            path: "src/shaders/fragment/scene.frag"
                        }
}

#[rustfmt::skip]
mod shadow_vertex_shader {
    vulkano_shaders::shader! {
                            ty: "vertex",
                            src: r"
#version 450
layout(location = 0) in vec3 position;
layout(push_constant) uniform Camera {
    mat4 view_projection;
    vec4 eye;
    vec4 ambient;
    vec4 ground_ambient;
    uvec4 light_info;
} camera;
struct PhysicsState {
    mat4 model;
    vec4 velocity;
    vec4 angular_velocity;
    vec4 properties;
    vec4 custom_values;
    uvec4 metadata;
};
layout(set = 0, binding = 0) readonly buffer PhysicsStates {
    PhysicsState data[];
} physics_states;
struct RenderInstance {
    mat4 model;
    mat3x4 normal;
    vec4 color;
    vec4 emissive;
    vec4 surface;
    uvec4 physics;
    vec4 transmission;
    vec4 uv_transform;
};
layout(set = 0, binding = 1) readonly buffer RenderInstances {
    RenderInstance data[];
} render_instances;
void main() {
    RenderInstance instance = render_instances.data[gl_InstanceIndex];
    if ((instance.physics.w & 1u) == 0u) {
        // Every vertex at one point: a zero-area triangle draws nothing.
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
        return;
    }
    mat4 model = instance.physics.x == 0xffffffffu
        ? instance.model
        : physics_states.data[instance.physics.x].model;
    gl_Position = camera.view_projection * model * vec4(position, 1.0);
}
"
                        }
}

#[rustfmt::skip]
mod debug_vertex_shader {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
#version 450
layout(location = 0) in vec3 start;
layout(location = 1) in vec3 end;
layout(location = 2) in vec4 color;
layout(location = 3) in vec2 corner;
layout(location = 4) in float thickness;
layout(location = 0) out vec4 v_color;
layout(push_constant) uniform Camera {
    mat4 view_projection;
    vec2 viewport_size;
    vec2 padding;
} camera;
void main() {
    vec4 start_clip = camera.view_projection * vec4(start, 1.0);
    vec4 end_clip = camera.view_projection * vec4(end, 1.0);
    // Clip to the near plane (z = 0 in Vulkan clip space) before the divide,
    // so an endpoint behind the camera does not flip the screen direction.
    if (start_clip.z < 0.0 && end_clip.z < 0.0) {
        gl_Position = vec4(0.0, 0.0, -1.0, 1.0);
        v_color = vec4(0.0);
        return;
    }
    if (start_clip.z < 0.0) {
        start_clip = mix(start_clip, end_clip, start_clip.z / (start_clip.z - end_clip.z));
    } else if (end_clip.z < 0.0) {
        end_clip = mix(end_clip, start_clip, end_clip.z / (end_clip.z - start_clip.z));
    }
    vec2 start_ndc = start_clip.xy / start_clip.w;
    vec2 end_ndc = end_clip.xy / end_clip.w;
    vec2 screen_direction = (end_ndc - start_ndc) * camera.viewport_size;
    float direction_length = length(screen_direction);
    vec2 normal = direction_length > 0.0001
        ? vec2(-screen_direction.y, screen_direction.x) / direction_length
        : vec2(0.0, 1.0);
    vec4 clip = mix(start_clip, end_clip, corner.x);
    clip.xy += normal * corner.y * thickness / camera.viewport_size * clip.w;
    gl_Position = clip;
    v_color = color;
}
"
    }
}

#[rustfmt::skip]
mod particle_vertex_shader {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
#version 450
layout(location = 0) in vec3 center;
layout(location = 1) in float size;
layout(location = 2) in vec4 color;
layout(location = 3) in vec3 stretch;
layout(location = 4) in float rotation;
layout(location = 0) out vec4 v_color;
layout(location = 1) out vec2 v_corner;
layout(location = 2) flat out uint v_sprite;
layout(location = 3) flat out float v_time;
layout(push_constant) uniform Particles {
    mat4 view_projection;
    // w = 1: xyz is the camera position. w = 0: xyz points toward an
    // orthographic camera.
    vec4 eye;
    vec4 right;
    vec4 up;
    // x: sprite, y: seconds since the game started as float bits.
    uvec4 params;
} particles;
const vec2 CORNERS[6] = vec2[](
    vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0),
    vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0)
);
void main() {
    vec2 corner = CORNERS[gl_VertexIndex];
    float half_size = size * 0.5;
    float stretch_length = length(stretch);
    vec3 offset;
    if (stretch_length > 1e-5) {
        // A streak along the velocity, turned to face the camera.
        vec3 axis = stretch / stretch_length;
        vec3 to_eye = particles.eye.w > 0.5
            ? normalize(particles.eye.xyz - center)
            : particles.eye.xyz;
        vec3 side = cross(axis, to_eye);
        float side_length = length(side);
        side = side_length > 1e-5 ? side / side_length : particles.right.xyz;
        offset = side * corner.x * half_size
            + axis * corner.y * (half_size + stretch_length * 0.5);
    } else {
        float s = sin(rotation);
        float c = cos(rotation);
        vec2 turned = vec2(c * corner.x - s * corner.y, s * corner.x + c * corner.y);
        offset = (particles.right.xyz * turned.x + particles.up.xyz * turned.y) * half_size;
    }
    gl_Position = particles.view_projection * vec4(center + offset, 1.0);
    v_color = color;
    v_corner = corner;
    v_sprite = particles.params.x;
    v_time = uintBitsToFloat(particles.params.y);
}
"
    }
}

mod particle_fragment_shader {
    vulkano_shaders::shader! {
        ty: "fragment",
        include: ["src/shaders"],
        path: "src/shaders/fragment/particle.frag"
    }
}

#[rustfmt::skip]
mod debug_fragment_shader {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
#version 450
layout(location = 0) in vec4 v_color;
layout(location = 0) out vec4 f_color;
void main() {
    f_color = v_color;
}
"
    }
}

#[rustfmt::skip]
mod tonemap_vertex_shader {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
#version 450
void main() {
    // One triangle that covers the whole viewport.
    vec2 uv = vec2((gl_VertexIndex << 1) & 2, gl_VertexIndex & 2);
    gl_Position = vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}
"
    }
}

/// Upscale in the spirit of FSR 1: Catmull-Rom filtering, kept inside the
/// four nearest source texels so edges do not ring, then contrast-adaptive
/// sharpening from the source texels around the pixel.
#[rustfmt::skip]
mod sharp_upscale_shader {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
#version 450
layout(set = 0, binding = 0) uniform sampler2D source;
layout(push_constant) uniform Params {
    vec2 inv_target;
    float sharpness;
} params;
layout(location = 0) out vec4 color;

vec3 at(vec2 uv) {
    return textureLod(source, uv, 0.0).rgb;
}

// Catmull-Rom from nine bilinear taps (Jimenez 2016).
vec3 catmull_rom(vec2 uv, vec2 size) {
    vec2 pos = uv * size;
    vec2 center = floor(pos - 0.5) + 0.5;
    vec2 f = pos - center;
    vec2 w0 = f * (-0.5 + f * (1.0 - 0.5 * f));
    vec2 w1 = 1.0 + f * f * (-2.5 + 1.5 * f);
    vec2 w2 = f * (0.5 + f * (2.0 - 1.5 * f));
    vec2 w3 = f * f * (-0.5 + 0.5 * f);
    vec2 w12 = w1 + w2;
    vec2 t0 = (center - 1.0) / size;
    vec2 t12 = (center + w2 / w12) / size;
    vec2 t3 = (center + 2.0) / size;
    return at(vec2(t0.x, t0.y)) * w0.x * w0.y
        + at(vec2(t12.x, t0.y)) * w12.x * w0.y
        + at(vec2(t3.x, t0.y)) * w3.x * w0.y
        + at(vec2(t0.x, t12.y)) * w0.x * w12.y
        + at(vec2(t12.x, t12.y)) * w12.x * w12.y
        + at(vec2(t3.x, t12.y)) * w3.x * w12.y
        + at(vec2(t0.x, t3.y)) * w0.x * w3.y
        + at(vec2(t12.x, t3.y)) * w12.x * w3.y
        + at(vec2(t3.x, t3.y)) * w3.x * w3.y;
}

void main() {
    vec2 uv = gl_FragCoord.xy * params.inv_target;
    vec2 size = vec2(textureSize(source, 0));
    vec2 texel = 1.0 / size;
    // The four source texels around the pixel bound the result.
    vec2 base = (floor(uv * size - 0.5) + 0.5) * texel;
    vec3 a = at(base);
    vec3 b = at(base + vec2(texel.x, 0.0));
    vec3 c = at(base + vec2(0.0, texel.y));
    vec3 d = at(base + texel);
    vec3 lo = min(min(a, b), min(c, d));
    vec3 hi = max(max(a, b), max(c, d));
    vec3 middle = clamp(catmull_rom(uv, size), lo, hi);
    // AMD CAS: a negative lobe on the cross neighbors, weaker where the
    // neighborhood already spans much of the range.
    vec3 n = at(uv - vec2(0.0, texel.y));
    vec3 s = at(uv + vec2(0.0, texel.y));
    vec3 e = at(uv + vec2(texel.x, 0.0));
    vec3 w = at(uv - vec2(texel.x, 0.0));
    vec3 low = min(middle, min(min(n, s), min(e, w)));
    vec3 high = max(middle, max(max(n, s), max(e, w)));
    vec3 amount = sqrt(clamp(min(low, 1.0 - high) / max(high, 1e-4), 0.0, 1.0));
    vec3 lobe = -amount / mix(8.0, 5.0, params.sharpness);
    vec3 sharp = (middle + lobe * (n + s + e + w)) / (1.0 + 4.0 * lobe);
    color = vec4(clamp(sharp, 0.0, 1.0), 1.0);
}
"
    }
}

#[rustfmt::skip]
mod tonemap_fragment_shader {
    vulkano_shaders::shader! {
        ty: "fragment",
        include: ["src/shaders"],
        path: "src/shaders/fragment/tonemap.frag"
    }
}

/// Fullscreen triangle at the far plane for the sky fog pass, with its
/// clip-space position for the fragment shader.
#[rustfmt::skip]
mod sky_vertex_shader {
    vulkano_shaders::shader! {
        ty: "vertex",
        src: r"
#version 450
layout(location = 0) out vec2 v_ndc;
void main() {
    vec2 ndc = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2))
        * 2.0 - 1.0;
    v_ndc = ndc;
    gl_Position = vec4(ndc, 1.0, 1.0);
}
"
    }
}

/// Fogs the background where no surface was drawn. The output alpha is
/// the share of the background that shows through; see `create_sky_pipeline`.
#[rustfmt::skip]
mod sky_fragment_shader {
    vulkano_shaders::shader! {
        ty: "fragment",
        include: ["src/shaders"],
        path: "src/shaders/fragment/sky.frag"
    }
}

/// Depth-only scene pass for ambient occlusion: discards masked texels
/// like the lit pass so cutout holes do not occlude.
#[rustfmt::skip]
mod depth_prepass_fragment_shader {
    vulkano_shaders::shader! {
        ty: "fragment",
        src: r"
#version 450
layout(location = 1) in vec4 v_color;
layout(location = 3) flat in uvec3 v_alpha;
layout(location = 4) in vec2 v_uv;
layout(set = 1, binding = 0) uniform sampler2D base_color_texture;
void main() {
    float alpha = v_color.a * texture(base_color_texture, v_uv).a;
    if (v_alpha.x == 1u && alpha < uintBitsToFloat(v_alpha.y)) {
        discard;
    }
}
"
    }
}

/// Frustum-culls every instance against its newest transform (GPU physics
/// state for GPU bodies) and appends the visible ones to their draw's range
/// of the visible list, counting them in the draw's `instanceCount`.
#[rustfmt::skip]
mod cull_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        src: r"
#version 450
layout(local_size_x = 256, local_size_y = 1, local_size_z = 1) in;

struct PhysicsState {
    mat4 model;
    vec4 velocity;
    vec4 angular_velocity;
    vec4 properties;
    vec4 custom_values;
    uvec4 metadata;
};
layout(set = 0, binding = 0) readonly buffer PhysicsStates {
    PhysicsState data[];
} physics_states;
struct RenderInstance {
    mat4 model;
    mat3x4 normal;
    vec4 color;
    vec4 emissive;
    vec4 surface;
    uvec4 physics;
    vec4 transmission;
    vec4 uv_transform;
};
layout(set = 0, binding = 1) readonly buffer RenderInstances {
    RenderInstance data[];
} render_instances;
struct CullInstance {
    vec4 sphere;
    uvec4 slot;
    vec4 lod;
    vec4 lod_ends;
};
layout(set = 0, binding = 2) readonly buffer CullInstances {
    CullInstance data[];
} cull_instances;
struct DrawCommand {
    uint index_count;
    uint instance_count;
    uint first_index;
    int vertex_offset;
    uint first_instance;
};
layout(set = 0, binding = 3) buffer DrawCommands {
    DrawCommand data[];
} draw_commands;
layout(set = 0, binding = 4) writeonly buffer VisibleList {
    uint data[];
} visible;
layout(set = 0, binding = 5) buffer Occlusion {
    uint data[];
} occlusion;
layout(set = 0, binding = 6) uniform sampler2D depth_pyramid;
layout(push_constant) uniform Cull {
    mat4 clip;
    // Scene viewport offset and extent in depth-pyramid pixels.
    vec4 viewport;
    // xyz: camera position; w: tan(fov / 2), or minus half the view height
    // for an orthographic camera.
    vec4 lod;
    // x: instance count; y: phase (0 frustum, 1 early, 2 late); z: 1 to
    // skip the frustum test.
    uvec4 info;
} cull;

vec4 clip_row(int row) {
    return vec4(cull.clip[0][row], cull.clip[1][row], cull.clip[2][row], cull.clip[3][row]);
}

bool in_frustum(vec3 center, float radius) {
    vec4 x = clip_row(0);
    vec4 y = clip_row(1);
    vec4 z = clip_row(2);
    vec4 w = clip_row(3);
    vec4 planes[6] = vec4[6](w + x, w - x, w + y, w - y, z, w - z);
    for (int plane = 0; plane < 6; ++plane) {
        vec4 p = planes[plane];
        if (dot(p.xyz, center) + p.w < -radius * length(p.xyz)) {
            return false;
        }
    }
    return true;
}

// Distance to the camera, or the inverse of the view-height fraction the
// sphere covers; grows with distance. Mirrors `lod_value` in Rust.
float lod_value(vec3 center, float radius, bool screen_size) {
    float d = distance(center, cull.lod.xyz);
    if (!screen_size) {
        return d;
    }
    if (radius <= 0.0) {
        return 0.0;
    }
    return (cull.lod.w > 0.0 ? d * cull.lod.w : -cull.lod.w) / radius;
}

// True when the depth pyramid proves the sphere lies behind nearer depth
// everywhere it can cover on screen.
bool occluded(vec3 center, float radius) {
    vec2 low = vec2(1.0);
    vec2 high = vec2(-1.0);
    float nearest = 1.0;
    for (int corner = 0; corner < 8; ++corner) {
        vec3 offset = vec3(
            (corner & 1) != 0 ? radius : -radius,
            (corner & 2) != 0 ? radius : -radius,
            (corner & 4) != 0 ? radius : -radius);
        vec4 p = cull.clip * vec4(center + offset, 1.0);
        if (p.w <= 1e-5) {
            // Crosses the camera plane: the screen rectangle is unbounded.
            return false;
        }
        vec3 ndc = p.xyz / p.w;
        low = min(low, ndc.xy);
        high = max(high, ndc.xy);
        nearest = min(nearest, ndc.z);
    }
    ivec2 size = textureSize(depth_pyramid, 0);
    ivec2 first = clamp(ivec2(floor(cull.viewport.xy + (low * 0.5 + 0.5) * cull.viewport.zw)),
        ivec2(0), size - 1);
    ivec2 last = clamp(ivec2(floor(cull.viewport.xy + (high * 0.5 + 0.5) * cull.viewport.zw)),
        ivec2(0), size - 1);
    // The mip where the rectangle spans at most two texels per axis.
    int span = max(last.x - first.x, last.y - first.y) + 1;
    int mip = min(int(ceil(log2(float(span)))), textureQueryLevels(depth_pyramid) - 1);
    ivec2 mip_last = textureSize(depth_pyramid, mip) - 1;
    ivec2 low_texel = min(first >> mip, mip_last);
    ivec2 high_texel = min(last >> mip, mip_last);
    float farthest = 0.0;
    for (int y = low_texel.y; y <= high_texel.y; ++y) {
        for (int x = low_texel.x; x <= high_texel.x; ++x) {
            farthest = max(farthest, texelFetch(depth_pyramid, ivec2(x, y), mip).r);
        }
    }
    return nearest > farthest;
}

void main() {
    uint index = gl_GlobalInvocationID.x;
    if (index >= cull.info.x) {
        return;
    }
    uint phase = cull.info.y;
    CullInstance instance = cull_instances.data[index];
    RenderInstance render = render_instances.data[index];
    mat4 model = render.physics.x == 0xffffffffu
        ? render.model
        : physics_states.data[render.physics.x].model;
    vec3 center = (model * vec4(instance.sphere.xyz, 1.0)).xyz;
    float scale = sqrt(max(
        max(dot(model[0].xyz, model[0].xyz), dot(model[1].xyz, model[1].xyz)),
        dot(model[2].xyz, model[2].xyz)));
    float radius = instance.sphere.w * scale;
    float lod = lod_value(center, radius, instance.lod.y != 0.0);
    // The first level whose range holds the value; 4 outside every range.
    // Mirrors `lod_level` in Rust.
    uint level = 4u;
    if (lod >= instance.lod.x) {
        for (uint i = 0u; i < 4u; ++i) {
            if (lod < instance.lod_ends[i]) {
                level = i;
                break;
            }
        }
    }
    // `frustum` means in view before occlusion: in a LOD level's range and,
    // with bounds and the test enabled, in the frustum.
    bool bounded = instance.sphere.w >= 0.0;
    bool frustum = level < 4u
        && (!bounded || cull.info.z != 0u || in_frustum(center, radius));
    bool shown = frustum && (phase != 2u || !bounded || !occluded(center, radius));
    // The early phase draws last frame's visible opaque instances; the late
    // phase recomputes this before overwriting the history.
    bool early = phase != 0u && frustum && occlusion.data[index] != 0u
        && instance.slot.z == 0u;
    if (phase == 2u) {
        occlusion.data[index] = shown ? 1u : 0u;
    }
    bool emit = phase == 0u ? shown : (phase == 1u ? early : shown && !early);
    if (!emit) {
        return;
    }
    uint slot = atomicAdd(draw_commands.data[instance.slot.x + level].instance_count, 1u);
    visible.data[instance.slot.y + level * cull.info.x + slot] = index;
}
"
            }
}

/// Copies the scene depth into mip 0 of the depth pyramid.
#[rustfmt::skip]
mod depth_pyramid_copy_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        src: r"
#version 450
layout(local_size_x = 8, local_size_y = 8, local_size_z = 1) in;
layout(set = 0, binding = 0) uniform sampler2D source;
layout(set = 0, binding = 1, r32f) uniform writeonly image2D target;

void main() {
    ivec2 texel = ivec2(gl_GlobalInvocationID.xy);
    if (any(greaterThanEqual(texel, imageSize(target)))) {
        return;
    }
    imageStore(target, texel, vec4(texelFetch(source, texel, 0).r));
}
"
    }
}

/// Writes one depth-pyramid mip as the farthest depth of the texels it
/// covers in the mip below.
#[rustfmt::skip]
mod depth_pyramid_reduce_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        src: r"
#version 450
layout(local_size_x = 8, local_size_y = 8, local_size_z = 1) in;
layout(set = 0, binding = 0, r32f) uniform readonly image2D source;
layout(set = 0, binding = 1, r32f) uniform writeonly image2D target;

void main() {
    ivec2 texel = ivec2(gl_GlobalInvocationID.xy);
    ivec2 size = imageSize(target);
    if (any(greaterThanEqual(texel, size))) {
        return;
    }
    ivec2 first = texel * 2;
    // Mips halve rounding down, so the last row and column also cover the
    // odd source texel left over.
    ivec2 last = mix(first + 1, imageSize(source) - 1, equal(texel, size - 1));
    float farthest = 0.0;
    for (int y = first.y; y <= last.y; ++y) {
        for (int x = first.x; x <= last.x; ++x) {
            farthest = max(farthest, imageLoad(source, ivec2(x, y)).r);
        }
    }
    imageStore(target, texel, vec4(farthest));
}
"
    }
}

#[rustfmt::skip]
mod physics_grid_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        path: "src/shaders/compute/physics_contacts.comp",
        define: [("GRID_PASS", "1")],
    }
}

mod physics_contact_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        path: "src/shaders/compute/physics_contacts.comp",
    }
}

mod physics_oversized_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        path: "src/shaders/compute/physics_contacts.comp",
        define: [("OVERSIZED_PASS", "1")],
    }
}

mod physics_hash_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        path: "src/shaders/compute/physics_hash.comp",
    }
}

mod physics_shader {
    vulkano_shaders::shader! {
        ty: "compute",
        include: ["src/shaders"],
        path: "src/shaders/compute/physics.comp",
    }
}

#[cfg(test)]
mod tests {
    use nalgebra::Vector4;

    use super::*;

    fn perspective(aspect: f32, near: f32, far: f32) -> Matrix4<f32> {
        vulkan_clip_correction()
            * Perspective3::new(aspect, std::f32::consts::FRAC_PI_3, near, far)
                .to_homogeneous()
    }

    fn ndc(matrix: &Matrix4<f32>, point: Vector4<f32>) -> Vector4<f32> {
        let clip = matrix * point;
        clip / clip.w
    }

    #[test]
    fn perspective_makes_near_geometry_larger_than_far_geometry() {
        let projection = perspective(16.0 / 9.0, 0.1, 100.0);
        let near = ndc(&projection, Vector4::new(1.0, 0.0, -2.0, 1.0));
        let far = ndc(&projection, Vector4::new(1.0, 0.0, -4.0, 1.0));
        assert!(near.x.abs() > far.x.abs());
    }

    #[test]
    fn perspective_maps_depth_to_vulkan_zero_to_one_range() {
        let near_plane = 0.1;
        let far_plane = 100.0;
        let projection = perspective(1.0, near_plane, far_plane);
        let near = ndc(&projection, Vector4::new(0.0, 0.0, -near_plane, 1.0));
        let far = ndc(&projection, Vector4::new(0.0, 0.0, -far_plane, 1.0));
        assert!(near.z.abs() < 0.000_01, "near depth was {}", near.z);
        assert!((far.z - 1.0).abs() < 0.000_01, "far depth was {}", far.z);
    }

    #[test]
    fn projection_preserves_square_pixel_aspect() {
        let extent = [1000.0, 500.0];
        let projection = perspective(extent[0] / extent[1], 0.1, 100.0);
        let x = ndc(&projection, Vector4::new(1.0, 0.0, -4.0, 1.0));
        let y = ndc(&projection, Vector4::new(0.0, 1.0, -4.0, 1.0));
        let horizontal_pixels = x.x.abs() * extent[0];
        let vertical_pixels = y.y.abs() * extent[1];
        assert!((horizontal_pixels - vertical_pixels).abs() < 0.001);
    }

    #[test]
    fn matrix_upload_round_trip_preserves_columns() {
        let matrix =
            Matrix4::new_translation(&nalgebra::Vector3::new(2.0, 3.0, 4.0));
        let uploaded: [[f32; 4]; 4] = matrix.into();
        assert_eq!(matrix_from_array(uploaded), matrix);
    }

    #[test]
    fn collider_tree_prunes_distant_groups_in_stable_order() {
        let colliders = (0..128)
            .map(|index| GpuColliderUpload {
                model: crate::Transform::new([index as f32 * 10.0, 0.0, 0.0])
                    .to_matrix(),
                shape: [1.0, 0.5, 0.0, 0.0],
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let nodes = collider_tree(&colliders);
        assert_eq!(nodes.len(), colliders.len() * 2 - 1);
        assert_eq!(nodes[0].links[0] as usize, nodes.len());

        let mut node_index = 0;
        let mut visited = 0;
        let mut candidates = Vec::new();
        let center = 420.0_f32;
        let radius = 0.5_f32;
        while node_index < nodes.len() {
            visited += 1;
            let node = nodes[node_index];
            let distance =
                (center - center.clamp(node.lower[0], node.upper[0])).abs();
            if distance > radius {
                node_index = node.links[0] as usize;
            } else if node.links[1] == u32::MAX {
                node_index += 1;
            } else {
                candidates.push(node.links[1]);
                node_index = node.links[0] as usize;
            }
        }
        assert_eq!(candidates, vec![42]);
        assert!(visited < 20, "visited {visited} of {} nodes", nodes.len());
    }

    /// These compare our hand-written CPU upload structs against the
    /// `vulkano_shaders`-generated types for the same GLSL struct/block,
    /// reflected straight from the compiled SPIR-V, instead of hardcoded
    /// magic-number offsets that can drift silently from the shader source.
    #[test]
    fn render_instance_layout_matches_shader_struct() {
        use std::mem::{offset_of, size_of};

        type Reflected = super::vertex_shader::RenderInstance;
        assert_eq!(size_of::<RenderInstanceUpload>(), size_of::<Reflected>());
        assert_eq!(
            offset_of!(RenderInstanceUpload, color),
            offset_of!(Reflected, color)
        );
        assert_eq!(
            offset_of!(RenderInstanceUpload, emissive),
            offset_of!(Reflected, emissive)
        );
        assert_eq!(
            offset_of!(RenderInstanceUpload, surface),
            offset_of!(Reflected, surface)
        );
        assert_eq!(
            offset_of!(RenderInstanceUpload, physics),
            offset_of!(Reflected, physics)
        );
        assert_eq!(
            offset_of!(RenderInstanceUpload, uv_transform),
            offset_of!(Reflected, uv_transform)
        );
    }

    #[test]
    fn light_gpu_layouts_match_shader_structs() {
        use std::mem::{offset_of, size_of};

        type ReflectedCamera = super::vertex_shader::Camera;
        assert_eq!(size_of::<CameraUniform>(), size_of::<ReflectedCamera>());
        assert_eq!(
            offset_of!(CameraUniform, ambient),
            offset_of!(ReflectedCamera, ambient)
        );
        assert_eq!(
            offset_of!(CameraUniform, ground_ambient),
            offset_of!(ReflectedCamera, ground_ambient)
        );
        assert_eq!(
            offset_of!(CameraUniform, light_info),
            offset_of!(ReflectedCamera, light_info)
        );

        type ReflectedLight = super::fragment_shader::Light;
        assert_eq!(size_of::<LightUpload>(), size_of::<ReflectedLight>());
        assert_eq!(
            offset_of!(LightUpload, direction_range),
            offset_of!(ReflectedLight, direction_range)
        );
        assert_eq!(
            offset_of!(LightUpload, color_intensity),
            offset_of!(ReflectedLight, color_intensity)
        );
        assert_eq!(
            offset_of!(LightUpload, spot_angles),
            offset_of!(ReflectedLight, spot_angles)
        );
        assert_eq!(
            offset_of!(LightUpload, cookie),
            offset_of!(ReflectedLight, cookie)
        );
    }

    #[test]
    fn frustum_test_keeps_bounds_that_touch_the_view_and_drops_the_rest() {
        // Default camera: at the origin looking down -Z, 60 degree vertical
        // field of view, near 0.1, far 1000, square aspect.
        let planes = frustum_planes(&view_projection(None, [100, 100]));
        let sphere = |center, radius| RenderBounds::Sphere { center, radius };
        let aabb = |min, max| RenderBounds::Aabb { min, max };
        // The left plane passes x = -10 tan 30 at z = -10; a center 0.5
        // beyond it sits 0.5 cos 30 = 0.433 from the plane.
        let edge = -10.0 * (30.0_f32).to_radians().tan() - 0.5;
        for (bounds, expected) in [
            (sphere([0.0, 0.0, -10.0], 1.0), true),
            (sphere([0.0, 0.0, 10.0], 1.0), false),
            (sphere([0.0, 0.0, -1100.0], 1.0), false),
            (sphere([edge, 0.0, -10.0], 0.5), true),
            (sphere([edge, 0.0, -10.0], 0.3), false),
            (aabb([5.0, -1.0, -11.0], [6.0, 1.0, -9.0]), true),
            (aabb([7.0, -1.0, -11.0], [8.0, 1.0, -9.0]), false),
            (aabb([-1.0, -1.0, -0.5], [1.0, 1.0, 1.0]), true),
            (aabb([-1.0, -1.0, -0.05], [1.0, 1.0, 1.0]), false),
            (aabb([-1.0, 9.0, -11.0], [1.0, 10.0, -9.0]), false),
            // In front of the camera but short of the near plane.
            (aabb([-0.01, -0.01, -0.07], [0.01, 0.01, -0.05]), false),
        ] {
            assert_eq!(
                bounds_in_frustum(&planes, &bounds),
                expected,
                "{bounds:?}"
            );
        }
    }

    #[test]
    fn compaction_keeps_batches_contiguous_and_slots_blended_instances() {
        let assets = AssetServer::default();
        let batch =
            |first_instance, instance_count, levels| PreparedRenderBatch {
                mesh_key: 0,
                material: assets.fallback_material,
                first_instance,
                instance_count,
                first_draw: 0,
                levels,
            };
        let blended = |instance| BlendedInstance {
            instance,
            mesh_key: 0,
            material: assets.fallback_material,
            position: [0.0; 3],
        };
        // Instance 3 picks the second LOD level of its batch.
        let culled = [2, 5, 8];
        let (list, ranges, kept) = compact_visible(
            &[batch(0, 3, 1), batch(3, 3, 2), batch(6, 1, 1)],
            &[blended(7), blended(8), blended(9)],
            |instance| {
                (!culled.contains(&instance))
                    .then_some(u32::from(instance == 3))
            },
        );
        let list = list
            .iter()
            .map(|entry| entry.instance_index)
            .collect::<Vec<_>>();
        assert_eq!(list, [0, 1, 4, 3, 6, 7, 9]);
        // One range per batch draw: the middle batch draws each level.
        assert_eq!(ranges, [(0, 2), (2, 1), (3, 1), (4, 1)]);
        // Kept blended instances name their slot, which holds them.
        let slots = kept.iter().map(|item| item.instance).collect::<Vec<_>>();
        assert_eq!(slots, [5, 6]);
        assert_eq!([list[5], list[6]], [7, 9]);
    }

    #[test]
    fn lod_level_picks_the_range_holding_the_value() {
        let lod = [0.0; 4];
        let ends = [5.0, 9.0, f32::INFINITY, f32::NEG_INFINITY];
        assert_eq!(lod_level(0.0, lod, ends), Some(0));
        assert_eq!(lod_level(5.0, lod, ends), Some(1));
        assert_eq!(lod_level(1e9, lod, ends), Some(2));
        let last = [10.0, 0.0, 0.0, 0.0];
        let ends = [
            20.0,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ];
        assert_eq!(lod_level(9.0, last, ends), None, "before the start");
        assert_eq!(lod_level(20.0, last, ends), None, "past the last end");
        assert_eq!(lod_level(3.0, [0.0; 4], NO_LOD_ENDS), Some(0));
    }

    #[test]
    fn ten_thousand_equal_cubes_become_one_render_batch() {
        let assets = AssetServer::default();
        let renderables = (0..10_000)
            .map(|index| crate::runtime::ExtractedRenderable {
                entity: bevy_ecs::entity::Entity::from_raw_u32(index).unwrap(),
                transform: crate::runtime::GlobalTransform::default(),
                mesh: assets.fallback_mesh,
                material: assets.fallback_material,
                cast_shadows: true,
                receive_shadows: true,
                bounds: None,
            })
            .collect::<Vec<_>>();

        let (order, batches, blended_start) = render_batch_order(
            &renderables,
            |_| false,
            |m| ([Some(m.key()), None, None, None, None], false, 0),
        );

        assert_eq!(order.len(), 10_000);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].first_instance, 0);
        assert_eq!(batches[0].instance_count, 10_000);
        assert_eq!(blended_start, 10_000);
    }

    #[test]
    fn materials_that_differ_only_in_color_share_a_batch() {
        let mut assets = AssetServer::default();
        let colored = (0..4)
            .map(|index| {
                assets.materials.insert(MaterialAsset {
                    base_color: [index as f32 / 4.0, 0.0, 0.0, 1.0],
                    roughness: 0.8,
                    ..MaterialAsset::default()
                })
            })
            .collect::<Vec<_>>();
        // Glossy surfaces draw again for reflections, so they batch apart.
        let glossy = assets.materials.insert(MaterialAsset {
            roughness: 0.2,
            ..MaterialAsset::default()
        });
        let renderables = colored
            .iter()
            .chain([&glossy])
            .enumerate()
            .map(|(index, material)| crate::runtime::ExtractedRenderable {
                entity: bevy_ecs::entity::Entity::from_raw_u32(index as u32)
                    .unwrap(),
                transform: crate::runtime::GlobalTransform::default(),
                mesh: assets.fallback_mesh,
                material: *material,
                cast_shadows: true,
                receive_shadows: true,
                bounds: None,
            })
            .collect::<Vec<_>>();

        let (_, batches, _) = render_batch_order(
            &renderables,
            |_| false,
            |material| material_group(assets.materials.get(material)),
        );

        let counts = batches.iter().map(|b| b.instance_count);
        assert_eq!(counts.collect::<Vec<_>>(), [4, 1]);
    }

    #[test]
    fn blended_objects_render_last_unbatched_and_back_to_front() {
        let mut assets = AssetServer::default();
        let glass = assets.materials.insert(MaterialAsset {
            alpha_mode: AlphaMode::Blend,
            ..MaterialAsset::default()
        });
        let renderable = |index: u32, material, z: f32| {
            crate::runtime::ExtractedRenderable {
                entity: bevy_ecs::entity::Entity::from_raw_u32(index).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&nalgebra::Vector3::new(
                        0.0, 0.0, z,
                    ))
                    .into(),
                },
                mesh: assets.fallback_mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
                bounds: None,
            }
        };
        let renderables = [
            renderable(1, glass, -1.0),
            renderable(2, assets.fallback_material, 0.0),
            renderable(3, glass, -5.0),
            renderable(4, assets.fallback_material, 0.0),
        ];

        let (order, batches, blended_start) = render_batch_order(
            &renderables,
            |material| material == glass,
            |m| ([Some(m.key()), None, None, None, None], false, 0),
        );

        assert_eq!(blended_start, 2);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].instance_count, 2);
        assert!(order[..2].iter().all(|index| [1, 3].contains(index)));
        let mut blended = order[2..]
            .iter()
            .enumerate()
            .map(|(offset, &index)| BlendedInstance {
                instance: 2 + offset as u32,
                mesh_key: 0,
                material: glass,
                position: light_position(renderables[index].transform.matrix),
            })
            .collect::<Vec<_>>();
        sort_back_to_front(&mut blended, [0.0; 3], [0.0, 0.0, -1.0]);
        assert_eq!(blended[0].position[2], -5.0, "farthest drawn first");
        assert_eq!(blended[1].position[2], -1.0);
    }

    #[test]
    fn ambient_cubes_integrate_the_light_from_each_side() {
        let size = 8;
        let face = (size * size) as usize;
        // Uniform radiance: every side receives irradiance pi.
        let cube = ambient_cube(&vec![[0.5, 1.0, 2.0, 1.0]; 6 * face], size);
        for [r, g, b] in cube {
            assert!((r - 0.5).abs() < 0.02, "{r}");
            assert!((g - 1.0).abs() < 0.02 && (b - 2.0).abs() < 0.04);
        }
        // Light only through the +Y face (index 2): +Y gets most, -Y none,
        // the four sides an equal share.
        let mut texels = vec![[0.0; 4]; 6 * face];
        texels[2 * face..3 * face].fill([1.0; 4]);
        let cube = ambient_cube(&texels, size).map(|[r, ..]| r);
        assert!(cube[2] > 0.5 && cube[3] == 0.0, "{cube:?}");
        for side in [0, 1, 4, 5] {
            assert!((cube[side] - cube[0]).abs() < 1e-4);
            assert!(cube[side] > 0.0 && cube[side] < cube[2] / 2.0);
        }
    }

    #[test]
    fn light_probe_grids_clamp_and_spread_wall_to_wall() {
        assert_eq!(light_probe_resolution([0, 3, 99]), [1, 3, 8]);
        let clamped = light_probe_resolution([8, 8, 8]);
        assert!(clamped.iter().product::<u32>() as usize <= MAX_LIGHT_PROBES);
        assert_eq!(clamped, [4, 4, 4]);
        let positions =
            light_probe_positions([1.0, 2.0, 3.0], [2.0, 1.0, 1.0], [3, 1, 2]);
        assert_eq!(
            positions,
            [
                [-1.0, 2.0, 2.0],
                [1.0, 2.0, 2.0],
                [3.0, 2.0, 2.0],
                [-1.0, 2.0, 4.0],
                [1.0, 2.0, 4.0],
                [3.0, 2.0, 4.0],
            ]
        );
    }

    /// Full-screen slabs at the given depths seen by an orthographic camera
    /// at z = 5, rendered into an 8x8 offscreen image.
    struct SlabScene {
        base: &'static crate::rendering::HeadlessVulkanBase,
        memory_allocator: Arc<StandardMemoryAllocator>,
        renderer: SceneRenderer,
        assets: AssetServer,
        render_world: RenderWorld,
        image: Arc<Image>,
        extent: [u32; 2],
        debug_view: SceneDebugView,
        effects: SceneEffects,
    }

    impl SlabScene {
        fn new(slabs: &[(f32, MaterialAsset)]) -> Self {
            Self::with_extent(slabs, [8, 8])
        }

        fn with_extent(
            slabs: &[(f32, MaterialAsset)],
            extent: [u32; 2],
        ) -> Self {
            use crate::rendering::swapchain::OFFSCREEN_COLOR_FORMAT;
            let base = crate::rendering::test_support::headless_device();
            let memory_allocator = Arc::new(
                StandardMemoryAllocator::new_default(base.device.clone()),
            );
            let renderer = SceneRenderer::new(
                base.queue.clone(),
                memory_allocator.clone(),
                OFFSCREEN_COLOR_FORMAT,
                extent,
            )
            .unwrap();
            let mut assets = AssetServer::default();
            let mut render_world = RenderWorld::default();
            render_world.ambient_light = Some(crate::runtime::AmbientLight {
                color: [1.0; 3],
                intensity: 1.0,
            });
            render_world.background_color = [0.0, 0.0, 0.0, 1.0];
            // Test scenes are far below `AUTO_DIRECT_MAX_INSTANCES`, so
            // `Auto` would never cull them.
            render_world.culling = CullingMode::Frustum;
            // Revision 0 means "nothing extracted yet" to the renderer caches.
            render_world.renderables_revision = 1;
            render_world.lights_revision = 1;
            render_world.active_camera =
                Some(crate::runtime::ExtractedCamera {
                    entity: bevy_ecs::entity::Entity::from_raw_u32(1000)
                        .unwrap(),
                    transform: crate::runtime::GlobalTransform {
                        matrix: Matrix4::new_translation(
                            &nalgebra::Vector3::new(0.0, 0.0, 5.0),
                        )
                        .into(),
                    },
                    projection: Projection::Orthographic {
                        vertical_size: 2.0,
                        near: 0.1,
                        far: 100.0,
                    },
                    priority: 0,
                });
            for (index, (z, material)) in slabs.iter().enumerate() {
                let material = assets.materials.insert(material.clone());
                render_world.renderables.push(
                    crate::runtime::ExtractedRenderable {
                        entity: bevy_ecs::entity::Entity::from_raw_u32(
                            index as u32 + 1,
                        )
                        .unwrap(),
                        transform: crate::runtime::GlobalTransform {
                            matrix: (Matrix4::new_translation(
                                &nalgebra::Vector3::new(0.0, 0.0, *z),
                            ) * Matrix4::new_nonuniform_scaling(
                                &nalgebra::Vector3::new(4.0, 4.0, 0.1),
                            ))
                            .into(),
                        },
                        mesh: assets.fallback_mesh,
                        material,
                        cast_shadows: false,
                        receive_shadows: false,
                        bounds: None,
                    },
                );
            }
            let image = Image::new(
                memory_allocator.clone(),
                ImageCreateInfo {
                    format: OFFSCREEN_COLOR_FORMAT,
                    extent: [extent[0], extent[1], 1],
                    usage: ImageUsage::COLOR_ATTACHMENT
                        | ImageUsage::TRANSFER_SRC,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_DEVICE,
                    ..Default::default()
                },
            )
            .unwrap();
            Self {
                base,
                memory_allocator,
                renderer,
                assets,
                render_world,
                image,
                extent,
                debug_view: SceneDebugView::Lit,
                effects: SceneEffects::default(),
            }
        }

        fn render(&mut self, before: Box<dyn GpuFuture>) -> Box<dyn GpuFuture> {
            self.renderer
                .render(
                    before,
                    ImageView::new_default(self.image.clone()).unwrap(),
                    self.extent,
                    SceneRenderOptions {
                        debug_view: self.debug_view,
                        effects: self.effects,
                        ..SceneRenderOptions::game(self.extent)
                    },
                    &self.render_world,
                    &self.assets,
                )
                .unwrap()
        }

        fn now(&self) -> Box<dyn GpuFuture> {
            vulkano::sync::now(self.base.device.clone()).boxed()
        }

        /// Returns the center pixel as `[b, g, r, a]`.
        fn center_pixel(&self) -> [u8; 4] {
            let extent = self.extent;
            let pixels = self.pixels();
            let center =
                ((extent[1] / 2 * extent[0] + extent[0] / 2) * 4) as usize;
            pixels[center..center + 4].try_into().unwrap()
        }

        /// Returns every pixel, row by row, as `[b, g, r, a]` bytes.
        fn pixels(&self) -> Vec<u8> {
            crate::rendering::readback::read_back_image(
                &self.base.device,
                &self.base.queue,
                &self.memory_allocator,
                &Arc::new(StandardCommandBufferAllocator::new(
                    self.base.device.clone(),
                    Default::default(),
                )),
                &self.image,
            )
        }
    }

    /// Renders `scene` and returns the pixel at `[column, row]` as
    /// `[r, g, b]`.
    fn render_pixel(scene: &mut SlabScene, [x, y]: [u32; 2]) -> [u8; 3] {
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let index = ((y * scene.extent[0] + x) * 4) as usize;
        let [b, g, r, _]: [u8; 4] =
            scene.pixels()[index..index + 4].try_into().unwrap();
        [r, g, b]
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn fog_hides_distant_surfaces_and_the_sky() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The white slab is about 5 m from the camera.
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        let center = [4, 4];
        let [r, g, b] = render_pixel(&mut scene, center);
        assert!(r > 100 && g > 100 && b > 100, "clear: {r} {g} {b}");
        let fog = crate::runtime::Fog {
            color: [1.0, 0.0, 0.0],
            density: 1.0,
            height_falloff: 0.0,
            ..crate::runtime::Fog::default()
        };
        scene.render_world.fog = Some(fog);
        let [r, g, b] = render_pixel(&mut scene, center);
        assert!(r > 150 && g < 50 && b < 50, "dense fog: {r} {g} {b}");
        // Fog far below the surface has thinned out to nothing.
        scene.render_world.fog = Some(crate::runtime::Fog {
            height: -10.0,
            height_falloff: 5.0,
            ..fog
        });
        let [r, g, b] = render_pixel(&mut scene, center);
        assert!(r > 100 && g > 100 && b > 100, "low fog: {r} {g} {b}");
        // Debug views show raw shading.
        scene.render_world.fog = Some(fog);
        scene.debug_view = SceneDebugView::Unshaded;
        let [r, g, b] = render_pixel(&mut scene, center);
        assert!(g > 100 && b > 100, "unshaded: {r} {g} {b}");
        // The editor can switch fog off in its Scene view.
        scene.debug_view = SceneDebugView::Lit;
        scene.effects.fog = false;
        let [r, g, b] = render_pixel(&mut scene, center);
        assert!(g > 100 && b > 100, "fog switched off: {r} {g} {b}");

        let mut sky = SlabScene::new(&[]);
        sky.render_world.fog = Some(fog);
        let [r, g, b] = render_pixel(&mut sky, center);
        assert!(r > 150 && g < 20 && b < 20, "fogged sky: {r} {g} {b}");
        sky.render_world.fog = Some(crate::runtime::Fog {
            sky_affect: 0.0,
            ..fog
        });
        assert_eq!(render_pixel(&mut sky, center), [0, 0, 0]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn fog_volumes_fill_only_their_turned_box() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A red box 8 m wide and 1 m tall, turned a quarter around Z, so
        // it covers the middle columns of the view in front of the white
        // slab at z = 0, and not the edge column.
        let volume = |z: f32| {
            let matrix = Matrix4::new_translation(&Vector3::new(0.0, 0.0, z))
                * nalgebra::Rotation3::from_axis_angle(
                    &Vector3::z_axis(),
                    std::f32::consts::FRAC_PI_2,
                )
                .to_homogeneous()
                * Matrix4::new_nonuniform_scaling(&Vector3::new(4.0, 0.5, 1.5));
            (
                matrix.into(),
                crate::runtime::FogVolume {
                    color: [1.0, 0.0, 0.0],
                    density: 4.0,
                    extents: [4.0, 0.5, 1.5],
                },
            )
        };
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        scene.render_world.fog_volumes = vec![volume(2.5)];
        let [r, g, b] = render_pixel(&mut scene, [4, 4]);
        assert!(r > 150 && g < 30 && b < 30, "inside: {r} {g} {b}");
        let [r, g, b] = render_pixel(&mut scene, [0, 4]);
        assert!(r > 100 && g > 100 && b > 100, "beside: {r} {g} {b}");
        // Behind the slab it hides nothing.
        scene.render_world.fog_volumes = vec![volume(-2.5)];
        let [r, g, b] = render_pixel(&mut scene, [4, 4]);
        assert!(g > 100 && b > 100, "behind: {r} {g} {b}");
        // The editor's fog switch turns volumes off too.
        scene.render_world.fog_volumes = vec![volume(2.5)];
        scene.effects.fog = false;
        let [r, g, b] = render_pixel(&mut scene, [4, 4]);
        assert!(g > 100 && b > 100, "fog switched off: {r} {g} {b}");

        let mut sky = SlabScene::new(&[]);
        sky.render_world.fog_volumes = vec![volume(2.5)];
        let [r, g, b] = render_pixel(&mut sky, [4, 4]);
        assert!(
            r > 150 && g < 30 && b < 30,
            "in front of the sky: {r} {g} {b}"
        );
        assert_eq!(render_pixel(&mut sky, [0, 4]), [0, 0, 0]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn the_procedural_sky_follows_the_sun_and_lights_the_scene() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let sun =
            |toward: Vector3<f32>| crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &-toward.normalize(),
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 100_000.0,
                    shadows: false,
                },
            };
        // The camera looks down -Z at the horizon.
        let mut sky = SlabScene::new(&[]);
        sky.render_world.procedural_sky =
            Some(crate::runtime::ProceduralSky::default());
        sky.render_world.directional_lights = vec![sun(Vector3::y())];
        let [r, _, b] = render_pixel(&mut sky, [4, 4]);
        assert!(b > r + 10, "noon: {r} {b}");
        // Low in front of the camera, the sun reddens the sky.
        sky.render_world.directional_lights =
            vec![sun(Vector3::new(0.0, 0.05, -1.0))];
        let [r, _, b] = render_pixel(&mut sky, [4, 4]);
        assert!(r > b + 10, "sunset: {r} {b}");
        // Its disc is far brighter than the sky beside it.
        sky.render_world.procedural_sky = Some(crate::runtime::ProceduralSky {
            sun_size: 90.0,
            ..crate::runtime::ProceduralSky::default()
        });
        sky.render_world.directional_lights =
            vec![sun(Vector3::new(0.0, 0.2, -1.0))];
        let [r, g, _] = render_pixel(&mut sky, [4, 4]);
        assert!(r > 250 && g > 150, "sun disc: {r} {g}");

        // Without ambient light, the sky still lights a white slab facing
        // the horizon.
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        scene.render_world.ambient_light = None;
        let [r0, g0, b0] = render_pixel(&mut scene, [4, 4]);
        scene.render_world.procedural_sky =
            Some(crate::runtime::ProceduralSky::default());
        let [r, g, b] = render_pixel(&mut scene, [4, 4]);
        assert!(
            r > r0 + 20 && g > g0 + 20 && b > b0 + 20,
            "sky light: {r} {g} {b}, without: {r0} {g0} {b0}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn light_shafts_keep_fog_in_a_casters_shadow_dark() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The camera looks down -Z into the sun, which shines up +Z from
        // under a black slab at z = -3: all fog in view lies in its shadow.
        let mut scene = SlabScene::new(&[(
            -3.0,
            MaterialAsset {
                base_color: [0.0, 0.0, 0.0, 1.0],
                ..MaterialAsset::default()
            },
        )]);
        scene.render_world.ambient_light = None;
        scene.render_world.renderables[0].cast_shadows = true;
        let direction = Vector3::new(0.1, 0.0, 1.0).normalize();
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 500_000.0,
                    shadows: true,
                },
            },
        );
        let fog = |light_shafts| crate::runtime::Fog {
            color: [0.0; 3],
            density: 1.0,
            height_falloff: 0.0,
            sun_scatter: 1.0,
            light_shafts,
            ..crate::runtime::Fog::default()
        };
        let center = [4, 4];
        scene.render_world.fog = Some(fog(0.0));
        let [glow, ..] = render_pixel(&mut scene, center);
        assert!(glow > 150, "fog glows toward the sun, got {glow}");
        scene.render_world.fog = Some(fog(1.0));
        let [shaft, ..] = render_pixel(&mut scene, center);
        assert!(shaft < 30, "shadowed fog stays dark, got {shaft}");
        // Without a shadow map there is nothing to block the glow.
        scene.render_world.directional_lights[0].light.shadows = false;
        scene.render_world.lights_revision += 1;
        let [unshadowed, ..] = render_pixel(&mut scene, center);
        assert!(unshadowed > 150, "no shadows, no shafts: {unshadowed}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn auto_exposure_brings_the_scene_to_its_key() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        use crate::runtime::{AutoExposure, ToneMapping};
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                model: MaterialModel::Unlit,
                base_color: [0.02, 0.02, 0.02, 1.0],
                ..MaterialAsset::default()
            },
        )]);
        // The slab fills the whole view, so the average is 0.02.
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(10.0, 10.0, 0.1))
                .into();
        let center = [4, 4];
        let fixed = |scene: &mut SlabScene, exposure: f32| {
            scene.render_world.auto_exposure = None;
            scene.render_world.tone_mapping = Some(ToneMapping {
                exposure,
                ..ToneMapping::default()
            });
            let pixel = render_pixel(scene, center);
            scene.render_world.tone_mapping = None;
            pixel
        };
        let unexposed = fixed(&mut scene, 1.0);
        let times_three = fixed(&mut scene, 3.0);
        let times_six = fixed(&mut scene, 6.0);
        assert!(unexposed[0] < times_three[0] && times_three[0] < times_six[0]);
        let close = |a: [u8; 3], b: [u8; 3]| {
            a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 1)
        };
        // The first frame adapts at once: 0.06 / 0.02.
        scene.render_world.auto_exposure = Some(AutoExposure {
            key: 0.06,
            speed: 0.0,
            max_exposure: 10.0,
            ..AutoExposure::default()
        });
        let adapted = render_pixel(&mut scene, center);
        assert!(close(adapted, times_three), "{adapted:?} {times_three:?}");
        assert!(scene
            .renderer
            .last_frame_passes()
            .contains(&FramePass::Exposure));
        // At speed 0 a new key does not move the adapted exposure.
        scene.render_world.auto_exposure = Some(AutoExposure {
            key: 0.12,
            speed: 0.0,
            max_exposure: 10.0,
            ..AutoExposure::default()
        });
        let held = render_pixel(&mut scene, center);
        assert!(close(held, times_three), "{held:?} {times_three:?}");
        // A frame without it starts over, and the limits clamp.
        assert!(close(fixed(&mut scene, 1.0), unexposed));
        scene.render_world.auto_exposure = Some(AutoExposure {
            key: 0.12,
            speed: 0.0,
            max_exposure: 10.0,
            ..AutoExposure::default()
        });
        let readapted = render_pixel(&mut scene, center);
        assert!(close(readapted, times_six), "{readapted:?} {times_six:?}");
        fixed(&mut scene, 1.0);
        scene.render_world.auto_exposure = Some(AutoExposure {
            key: 0.12,
            max_exposure: 3.0,
            ..AutoExposure::default()
        });
        let clamped = render_pixel(&mut scene, center);
        assert!(close(clamped, times_three), "{clamped:?} {times_three:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn bloom_spreads_bright_light_into_its_surroundings() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    base_color: [50.0, 50.0, 50.0, 1.0],
                    ..MaterialAsset::default()
                },
            )],
            [32, 32],
        );
        // A bright square about three pixels wide in the center.
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(0.2, 0.2, 0.1))
                .into();
        let beside = [22, 16];
        assert_eq!(render_pixel(&mut scene, beside), [0, 0, 0]);
        scene.render_world.bloom = Some(crate::runtime::Bloom::default());
        let [r, g, b] = render_pixel(&mut scene, beside);
        assert!(r > 5 && r == g && g == b, "glow: {r} {g} {b}");
        assert!(scene
            .renderer
            .last_frame_passes()
            .contains(&FramePass::Bloom));
        scene.effects.bloom = false;
        assert_eq!(render_pixel(&mut scene, beside), [0, 0, 0]);
        scene.effects.bloom = true;
        // Below the threshold nothing glows.
        scene.render_world.bloom = Some(crate::runtime::Bloom {
            threshold: 100.0,
            ..crate::runtime::Bloom::default()
        });
        assert_eq!(render_pixel(&mut scene, beside), [0, 0, 0]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn indirect_light_bleeds_a_bright_surface_onto_its_neighbour() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A white wall, and a glowing red slab 0.3 m in front of its left
        // half.
        let mut scene = SlabScene::with_extent(
            &[
                (0.0, MaterialAsset::default()),
                (
                    0.3,
                    MaterialAsset {
                        model: MaterialModel::Unlit,
                        base_color: [4.0, 0.0, 0.0, 1.0],
                        ..MaterialAsset::default()
                    },
                ),
            ],
            [32, 32],
        );
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(-2.0, 0.0, 0.3))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    4.0, 4.0, 0.1,
                )))
            .into();
        scene.render_world.quality = QualityProfile::High;
        // No ambient light, so the wall shows only what bounces onto it.
        scene.render_world.ambient_light = None;
        let near = [17, 16];
        // The first frame makes the copy the second gathers from.
        let redness = |scene: &mut SlabScene, indirect| {
            scene.render_world.ambient_occlusion =
                Some(crate::runtime::AmbientOcclusion {
                    radius: 1.0,
                    intensity: 1.0,
                    indirect,
                });
            render_pixel(scene, near);
            let [r, g, _] = render_pixel(scene, near);
            i32::from(r) - i32::from(g)
        };
        let plain = redness(&mut scene, 0.0);
        let bled = redness(&mut scene, 2.0);
        assert!(bled > plain + 20, "red {plain} -> {bled}");
        assert!(scene
            .renderer
            .last_frame_passes()
            .contains(&FramePass::SceneColor));
        // Balanced keeps the occlusion but not the indirect light.
        scene.render_world.quality = QualityProfile::Balanced;
        assert!(redness(&mut scene, 2.0) <= plain + 2);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn ambient_occlusion_darkens_surfaces_next_to_occluders() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A wall, and a second slab 0.3 m in front of its left half.
        let mut scene = SlabScene::with_extent(
            &[
                (0.0, MaterialAsset::default()),
                (0.3, MaterialAsset::default()),
            ],
            [32, 32],
        );
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(-2.0, 0.0, 0.3))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    4.0, 4.0, 0.1,
                )))
            .into();
        // Eco turns ambient occlusion off.
        scene.render_world.quality = QualityProfile::High;
        let (near, far) = ([17, 16], [30, 16]);
        let before = [
            render_pixel(&mut scene, near),
            render_pixel(&mut scene, far),
        ];
        assert_eq!(before[0], before[1], "no occlusion without the component");
        scene.render_world.ambient_occlusion =
            Some(crate::runtime::AmbientOcclusion {
                radius: 0.5,
                intensity: 1.0,
                indirect: 0.0,
            });
        let [near, far] = [
            render_pixel(&mut scene, near),
            render_pixel(&mut scene, far),
        ];
        assert!(
            u32::from(near[1]) + 10 < u32::from(far[1]),
            "crease {near:?} is darker than open wall {far:?}"
        );
        assert_eq!(far, before[1], "open wall is unoccluded");
        let passes = scene.renderer.last_frame_passes();
        assert!(passes.contains(&FramePass::DepthPrepass));
        assert!(passes.contains(&FramePass::AmbientOcclusion));
        scene.effects.ambient_occlusion = false;
        assert_eq!(render_pixel(&mut scene, [17, 16])[1], before[0][1]);
        scene.effects.ambient_occlusion = true;
        scene.render_world.quality = QualityProfile::Eco;
        let eco = render_pixel(&mut scene, [17, 16]);
        assert!(!scene
            .renderer
            .last_frame_passes()
            .contains(&FramePass::AmbientOcclusion));
        assert_eq!(eco[1], before[0][1], "Eco skips ambient occlusion");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn atmosphere_effects_combine_with_every_scene_path() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        use crate::runtime::{Antialiasing, CullingMode};
        // Glossy metal splits the pass for screen-space reflections.
        let materials = [
            MaterialAsset::default(),
            MaterialAsset {
                metallic: 1.0,
                roughness: 0.1,
                ..MaterialAsset::default()
            },
        ];
        for material in materials {
            for culling in
                [CullingMode::Frustum, CullingMode::FrustumAndOcclusion]
            {
                for antialiasing in [Antialiasing::Off, Antialiasing::Msaa4] {
                    let mut scene = SlabScene::with_extent(
                        &[(0.0, material.clone()), (0.3, material.clone())],
                        [32, 32],
                    );
                    scene.render_world.renderables[1].transform.matrix =
                        (Matrix4::new_translation(&Vector3::new(
                            -2.0, 0.0, 0.3,
                        )) * Matrix4::new_nonuniform_scaling(&Vector3::new(
                            4.0, 4.0, 0.1,
                        )))
                        .into();
                    scene.render_world.quality = QualityProfile::High;
                    scene.render_world.culling = culling;
                    scene.render_world.antialiasing = antialiasing;
                    scene.render_world.fog = Some(crate::runtime::Fog {
                        density: 0.05,
                        ..crate::runtime::Fog::default()
                    });
                    scene.render_world.bloom =
                        Some(crate::runtime::Bloom::default());
                    scene.render_world.ambient_occlusion =
                        Some(crate::runtime::AmbientOcclusion {
                            radius: 0.5,
                            intensity: 1.0,
                            indirect: 1.0,
                        });
                    // Occlusion culling needs a frame to fill its history.
                    for _ in 0..2 {
                        render_pixel(&mut scene, [0, 0]);
                    }
                    let label =
                        format!("{culling:?} {antialiasing:?} {material:?}");
                    let passes = scene.renderer.last_frame_passes();
                    for pass in [
                        FramePass::DepthPrepass,
                        FramePass::AmbientOcclusion,
                        FramePass::Bloom,
                        FramePass::ToneMap,
                    ] {
                        assert!(passes.contains(&pass), "{label}: {pass:?}");
                    }
                    let near = render_pixel(&mut scene, [17, 16]);
                    let far = render_pixel(&mut scene, [30, 16]);
                    assert!(
                        near[1] < far[1],
                        "{label}: crease {near:?} is darker than {far:?}"
                    );
                }
            }
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn msaa_smooths_edges_except_on_eco() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Large enough that 2x MSAA, whose two samples sit on a diagonal,
        // also splits some edge pixels.
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    ..MaterialAsset::default()
                },
            )],
            [32, 32],
        );
        // A white slab whose edge crosses the view at a slant.
        let rotation = Matrix4::new_rotation(Vector3::new(0.0, 0.0, 0.5));
        scene.render_world.renderables[0].transform.matrix = (rotation
            * Matrix4::new_translation(&Vector3::new(2.0, 0.0, 0.0))
            * Matrix4::new_nonuniform_scaling(&Vector3::new(4.0, 4.0, 0.1)))
        .into();
        let shades = |scene: &mut SlabScene, quality| {
            scene.render_world.quality = quality;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let mut greens = scene
                .pixels()
                .chunks(4)
                .map(|pixel| pixel[1])
                .collect::<Vec<_>>();
            greens.sort_unstable();
            greens.dedup();
            greens.len()
        };
        assert_eq!(
            shades(&mut scene, QualityProfile::Eco),
            2,
            "hard edge on Eco"
        );
        let samples = scene.renderer.capabilities().msaa_samples;
        let high = shades(&mut scene, QualityProfile::High);
        if samples > 1 {
            assert!(high > 2, "{samples}x MSAA blends edge pixels");
        } else {
            assert_eq!(high, 2, "no MSAA on this device");
        }
        assert_eq!(scene.renderer.scene_samples, samples);
        assert_eq!(
            shades(&mut scene, QualityProfile::Eco),
            2,
            "switching back drops MSAA"
        );
        assert!(scene.renderer.msaa_targets.is_none());

        // Explicit settings override the profile, and 2x rebuilds the
        // multisampled passes when the device has it.
        scene.render_world.antialiasing = Antialiasing::Off;
        assert_eq!(shades(&mut scene, QualityProfile::High), 2, "MSAA off");
        let capabilities = scene.renderer.capabilities().clone();
        for setting in [Antialiasing::Msaa2, Antialiasing::Msaa4] {
            scene.render_world.antialiasing = setting;
            let expected =
                scene_sample_count(setting, QualityProfile::Eco, &capabilities);
            let smooth = shades(&mut scene, QualityProfile::Eco);
            assert_eq!(scene.renderer.scene_samples, expected, "{setting:?}");
            assert_eq!(smooth > 2, expected > 1, "{setting:?}");
        }
    }

    #[test]
    fn antialiasing_settings_pick_a_count_the_device_has() {
        let device = |msaa_samples, msaa_2x| RendererCapabilities {
            device_name: String::new(),
            integrated_gpu: false,
            device_local_bytes: 8 << 30,
            multi_draw_indirect: Capability::default(),
            draw_indirect_count: Capability::default(),
            bindless_textures: Capability::default(),
            memory_budget: Capability::default(),
            sampler_anisotropy: Capability::default(),
            timestamp_queries: false,
            msaa_samples,
            msaa_2x,
        };
        let count = |setting, profile, caps: &RendererCapabilities| {
            scene_sample_count(setting, profile, caps)
        };
        let full = device(4, true);
        assert_eq!(count(Antialiasing::Auto, QualityProfile::High, &full), 4);
        assert_eq!(count(Antialiasing::Auto, QualityProfile::Eco, &full), 1);
        assert_eq!(count(Antialiasing::Off, QualityProfile::High, &full), 1);
        assert_eq!(count(Antialiasing::Msaa2, QualityProfile::Eco, &full), 2);
        assert_eq!(count(Antialiasing::Msaa4, QualityProfile::Eco, &full), 4);
        let no_2x = device(4, false);
        assert_eq!(count(Antialiasing::Msaa2, QualityProfile::High, &no_2x), 1);
        let only_2x = device(2, true);
        assert_eq!(
            count(Antialiasing::Msaa4, QualityProfile::High, &only_2x),
            2
        );
        let none = device(1, false);
        assert_eq!(count(Antialiasing::Msaa4, QualityProfile::High, &none), 1);
    }

    #[test]
    fn light_clusters_list_only_the_lights_that_reach_them() {
        // Camera at the origin looking down -Z, 90 degree view, square.
        let clip = vulkan_clip_correction()
            * Perspective3::new(1.0, std::f32::consts::FRAC_PI_2, 1.0, 100.0)
                .to_homogeneous();
        let eye = [0.0; 3];
        let forward = [0.0, 0.0, -1.0];
        let lights = [
            (5, [-8.0, 0.0, -10.0, 1.0]), // left of center, near
            (6, [0.0, 0.0, -60.0, 2.0]),  // center, far
            (7, [0.0, 0.0, 20.0, 3.0]),   // behind the camera
            (8, [0.0, 0.0, 0.0, 2.0]),    // around the camera
            (9, [500.0, 0.0, -10.0, 1.0]), // far off to the right
        ];
        let list =
            build_light_clusters(&lights, &clip, eye, forward, [1.0, 100.0]);
        let [columns, rows, slices] = CLUSTER_GRID;
        let at = |x: usize, y: usize, z: usize| {
            let cluster = (z * rows + y) * columns + x;
            let first = list[2 * cluster] as usize;
            let count = list[2 * cluster + 1] as usize;
            list[first..first + count].to_vec()
        };
        let slice = |depth: f32| {
            ((depth.ln() / 100.0_f32.ln() * slices as f32) as usize)
                .min(slices - 1)
        };
        // The near-left light: x = -8 at depth 10 is ndc -0.8, column 1.
        assert_eq!(at(1, rows / 2, slice(10.0)), [5]);
        assert!(at(columns - 2, rows / 2, slice(10.0)).is_empty());
        assert!(at(1, rows / 2, slice(60.0)).is_empty());
        // The far light only in its own depth slice, at the center.
        assert_eq!(at(columns / 2, rows / 2, slice(60.0)), [6]);
        assert!(at(columns / 2, rows / 2, slice(30.0)).is_empty());
        // The light around the eye reaches every tile of the first slice;
        // the ones behind and off screen reach nothing.
        assert_eq!(at(0, 0, 0), [8]);
        assert_eq!(at(columns - 1, rows - 1, 0), [8]);
        assert!(!list[2 * CLUSTER_COUNT..].contains(&7));
        assert!(!list[2 * CLUSTER_COUNT..].contains(&9));
    }

    #[test]
    fn shadow_settings_override_the_profile() {
        assert_eq!(
            shadow_settings_for(ShadowQuality::Auto, QualityProfile::Eco),
            (1024, 30.0)
        );
        assert_eq!(
            shadow_settings_for(ShadowQuality::High, QualityProfile::Eco),
            (4096, 80.0)
        );
        assert_eq!(
            shadow_settings_for(ShadowQuality::Low, QualityProfile::High),
            (1024, 30.0)
        );
        assert_eq!(
            shadow_settings_for(ShadowQuality::Medium, QualityProfile::High),
            (2048, 50.0)
        );
    }

    #[test]
    fn asset_names_use_the_path_or_the_handle_key() {
        assert_eq!(
            asset_name(
                "Mesh",
                7,
                Some(std::path::Path::new("models/crate.glb"))
            ),
            "Mesh models/crate.glb"
        );
        assert_eq!(asset_name("Texture", 7, None), "Texture #7");
    }

    #[test]
    fn msaa_takes_the_largest_shared_count_up_to_four_with_a_depth_resolve() {
        let counts = SampleCounts::SAMPLE_1
            | SampleCounts::SAMPLE_2
            | SampleCounts::SAMPLE_4
            | SampleCounts::SAMPLE_8;
        assert_eq!(msaa_sample_count(counts, counts, true), 4);
        assert_eq!(msaa_sample_count(counts, counts, false), 1);
        let two = SampleCounts::SAMPLE_1 | SampleCounts::SAMPLE_2;
        assert_eq!(msaa_sample_count(counts, two, true), 2);
        assert_eq!(msaa_sample_count(SampleCounts::SAMPLE_1, counts, true), 1);
    }

    fn render_center_pixel(slabs: &[(f32, MaterialAsset)]) -> [u8; 4] {
        let mut scene = SlabScene::new(slabs);
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        scene.center_pixel()
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn base_color_texture_tints_the_surface_and_reloads_next_frame() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let texture = |rgba: [u8; 4]| TextureAsset {
            size: [2, 2],
            rgba8: rgba.repeat(4),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        };
        // Unlit, so the environment's specular does not tint the texel.
        let unlit = MaterialAsset {
            model: MaterialModel::Unlit,
            ..Default::default()
        };
        let mut scene = SlabScene::new(&[(0.0, unlit)]);
        let green = scene.assets.textures.insert(texture([0, 255, 0, 255]));
        let material = scene.render_world.renderables[0].material;
        scene
            .assets
            .materials
            .get_mut(material)
            .unwrap()
            .base_color_texture = Some(green);
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert!(
            g > 200 && r < 30 && b < 30,
            "green texture, got {b} {g} {r}"
        );

        // A replaced texture value gets a new revision and is re-uploaded.
        *scene.assets.textures.get_mut(green).unwrap() =
            texture([255, 0, 0, 255]);
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert!(r > 200 && g < 30 && b < 30, "red texture, got {b} {g} {r}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn vertex_colors_tint_the_material_color() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let unlit = MaterialAsset {
            model: MaterialModel::Unlit,
            ..Default::default()
        };
        let mut scene = SlabScene::new(&[(0.0, unlit)]);
        let mesh = scene.render_world.renderables[0].mesh;
        for vertex in &mut scene.assets.meshes.get_mut(mesh).unwrap().vertices {
            vertex.color = [0.0, 0.0, 1.0, 1.0];
        }
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert!(
            b > 200 && g < 30 && r < 30,
            "blue vertices, got {b} {g} {r}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn flat_shading_lights_a_flat_face_like_its_vertex_normals() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Alpha-tested, so the face normal's derivatives sit next to a
        // discard in the same shader.
        let material = |flat_shading| MaterialAsset {
            flat_shading,
            alpha_mode: AlphaMode::Mask { cutoff: 0.5 },
            ..Default::default()
        };
        let smooth = render_center_pixel(&[(0.0, material(false))]);
        let flat = render_center_pixel(&[(0.0, material(true))]);
        assert!(
            smooth[..3].iter().any(|&channel| channel > 20),
            "{smooth:?}"
        );
        for (smooth, flat) in smooth.iter().zip(flat) {
            assert!(smooth.abs_diff(flat) <= 2, "{smooth:?} vs {flat:?}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn devices_enable_anisotropy_when_the_driver_supports_it() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let base = crate::rendering::init_vulkan_headless();
        let anisotropy =
            RendererCapabilities::detect(&base.device).sampler_anisotropy;
        assert_eq!(anisotropy.enabled, anisotropy.supported);
        // A linear sampler gets a level, so the validation layer checks the
        // feature is really on for every textured GPU test.
        texture_sampler(&base.queue, TextureSampler::default()).unwrap();
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn material_without_texture_samples_white() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let [b, g, r, _] =
            render_center_pixel(&[(0.0, MaterialAsset::default())]);
        assert!(b > 200 && g > 200 && r > 200, "got {b} {g} {r}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn lit_surfaces_do_not_shadow_themselves() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // One floor that both casts and receives, and nothing above it: with
        // shadows on it must look exactly like shadows off. Self-shadowing
        // ("shadow acne") shows as dark stripes and triangles.
        let mut scene = SlabScene::with_extent(
            &[(0.0, MaterialAsset::default())],
            [64, 64],
        );
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.renderables[0].cast_shadows = true;
        scene.render_world.renderables[0].receive_shadows = true;
        for tilt in [10.0_f32, 45.0, 70.0, 80.0] {
            let direction = Vector3::new(
                tilt.to_radians().sin(),
                0.3,
                -tilt.to_radians().cos(),
            )
            .normalize();
            scene.render_world.directional_lights =
                vec![crate::runtime::ExtractedDirectionalLight {
                    entity: bevy_ecs::entity::Entity::from_raw_u32(2000)
                        .unwrap(),
                    transform: crate::runtime::GlobalTransform {
                        matrix: nalgebra::Rotation3::rotation_between(
                            &-Vector3::z(),
                            &direction,
                        )
                        .unwrap()
                        .to_homogeneous()
                        .into(),
                    },
                    light: crate::runtime::DirectionalLight {
                        color: [1.0; 3],
                        illuminance: 100_000.0,
                        shadows: false,
                    },
                }];
            let reds = |scene: &mut SlabScene, shadows| {
                scene.render_world.directional_lights[0].light.shadows =
                    shadows;
                scene.render_world.lights_revision += 1;
                let before = scene.now();
                scene
                    .render(before)
                    .then_signal_fence_and_flush()
                    .unwrap()
                    .wait(None)
                    .unwrap();
                scene
                    .pixels()
                    .chunks(4)
                    .map(|pixel| pixel[2])
                    .collect::<Vec<_>>()
            };
            let lit = reds(&mut scene, false);
            let shadowed = reds(&mut scene, true);
            let worst = lit
                .iter()
                .zip(&shadowed)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            let darkened = lit
                .iter()
                .zip(&shadowed)
                .filter(|(a, b)| a.abs_diff(**b) > 4)
                .count();
            assert!(
                worst <= 4,
                "light tilted {tilt} degrees: {darkened} of {} pixels shadow \
                 themselves, worst by {worst}",
                lit.len()
            );
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn spot_light_shadow_stops_at_a_wall_only_when_enabled() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Floor slab at z = 0; a spot light at z = 6 shines down -Z on it
        // through a wall slab at z = 3 that covers the view center.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (1.0, MaterialAsset::default()),
        ]);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.renderables[1].cast_shadows = true;
        // The wall sits off to the side of the camera's view but in the
        // light's cone: the light is at x = 4 and leans to the center.
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(2.0, 0.0, 3.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    1.0, 1.0, 0.1,
                )))
            .into();
        let position = Vector3::new(4.0, 0.0, 6.0);
        let direction = (-position).normalize();
        scene.render_world.spot_lights.push(
            crate::runtime::ExtractedSpotLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2001).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: (Matrix4::new_translation(&position)
                        * nalgebra::Rotation3::rotation_between(
                            &-Vector3::z(),
                            &direction,
                        )
                        .unwrap()
                        .to_homogeneous())
                    .into(),
                },
                light: crate::runtime::SpotLight {
                    color: [1.0; 3],
                    intensity: 40_000.0,
                    range: 20.0,
                    inner_angle: 0.4,
                    outer_angle: 0.6,
                    shadows: true,
                    radius: 0.0,
                },
                cookie: None,
            },
        );
        let frame = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.center_pixel()[2]
        };
        let shadowed = frame(&mut scene);
        scene.render_world.spot_lights[0].light.shadows = false;
        let lit = frame(&mut scene);
        assert!(lit > 150, "spot light without shadows lights it, got {lit}");
        assert!(
            shadowed < 30,
            "the wall shadows the floor behind it, got {shadowed}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn point_light_shadow_falls_through_each_cube_face() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A point light shines on the floor at z = 0 past an occluder
        // halfway between them, off the camera's view. The two light
        // positions put the floor center behind the -X and the -Z cube face.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (1.0, MaterialAsset::default()),
        ]);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.point_lights.push(
            crate::runtime::ExtractedPointLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2002).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::identity().into(),
                },
                light: crate::runtime::PointLight {
                    color: [1.0; 3],
                    intensity: 40_000.0,
                    range: 20.0,
                    shadows: true,
                    radius: 0.0,
                },
            },
        );
        let frame = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.center_pixel()[2]
        };
        for light in [Vector3::new(4.0, 0.0, 3.0), Vector3::new(0.0, 3.0, 4.0)]
        {
            scene.render_world.point_lights[0].transform.matrix =
                Matrix4::new_translation(&light).into();
            scene.render_world.renderables[1].transform.matrix =
                (Matrix4::new_translation(&(light * 0.5))
                    * Matrix4::new_nonuniform_scaling(&Vector3::new(
                        0.5, 0.5, 0.5,
                    )))
                .into();
            scene.render_world.point_lights[0].light.shadows = true;
            let shadowed = frame(&mut scene);
            scene.render_world.point_lights[0].light.shadows = false;
            let lit = frame(&mut scene);
            assert!(lit > 150, "unshadowed point light lights it, got {lit}");
            // The floor does not shadow itself.
            scene.render_world.point_lights[0].light.shadows = true;
            scene.render_world.renderables[1].cast_shadows = false;
            let clear = frame(&mut scene);
            scene.render_world.renderables[1].cast_shadows = true;
            assert!(
                u16::from(clear) + 10 > u16::from(lit),
                "{clear} without caster vs {lit}"
            );
            assert!(
                shadowed < 30,
                "occluder shadows the floor from {light:?}, got {shadowed}"
            );
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn decals_paint_their_image_on_surfaces_facing_the_projection() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // An unlit grey floor under a decal of four colored quadrants that
        // fills the 2 m view; each screen quadrant shows its image quadrant.
        let extent = [16, 16];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    base_color: [0.2, 0.2, 0.2, 1.0],
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        let quadrant = |x: u32, y: u32| match (x < 8, y < 8) {
            (true, true) => [255, 0, 0, 255],
            (false, true) => [0, 255, 0, 255],
            (true, false) => [0, 0, 255, 255],
            (false, false) => [255, 255, 255, 255],
        };
        let image = scene.assets.textures.insert(TextureAsset {
            size: [16, 16],
            rgba8: (0..16)
                .flat_map(|y| (0..16).flat_map(move |x| quadrant(x, y)))
                .collect(),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        let decal = |matrix: Matrix4<f32>, decal| {
            let scale = Vector3::new(1.0, 1.0, 0.5);
            (matrix.prepend_nonuniform_scaling(&scale).into(), decal)
        };
        let image_decal = crate::runtime::Decal {
            handle: Some(image),
            ..crate::runtime::Decal::default()
        };
        let render = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            move |x: u32, y: u32| {
                let at = ((y * extent[0] + x) * 4) as usize;
                [pixels[at + 2], pixels[at + 1], pixels[at]]
            }
        };
        let bright = |pixel: [u8; 3]| pixel.map(|c| c > 150);

        scene.render_world.decals =
            vec![decal(Matrix4::identity(), image_decal.clone())];
        let pixel = render(&mut scene);
        assert_eq!(bright(pixel(4, 4)), [true, false, false], "top left");
        assert_eq!(bright(pixel(11, 4)), [false, true, false], "top right");
        assert_eq!(bright(pixel(4, 11)), [false, false, true], "bottom left");
        assert_eq!(bright(pixel(11, 11)), [true; 3], "bottom right");

        // Turned to project up, away from the floor: nothing is painted.
        scene.render_world.decals = vec![decal(
            Matrix4::from_euler_angles(std::f32::consts::PI, 0.0, 0.0),
            image_decal,
        )];
        let pixel = render(&mut scene);
        let grey = pixel(4, 4);
        assert!(grey.iter().all(|&c| c > 50 && c < 150), "{grey:?}");

        // A plain half-transparent red decal over the right half only.
        scene.render_world.decals = vec![decal(
            Matrix4::new_translation(&Vector3::new(1.0, 0.0, 0.0)),
            crate::runtime::Decal {
                color: [1.0, 0.0, 0.0, 0.5],
                ..crate::runtime::Decal::default()
            },
        )];
        let pixel = render(&mut scene);
        assert_eq!(pixel(4, 8), grey, "left half untouched");
        let right = pixel(11, 8);
        assert!(right[0] > grey[0] + 40 && right[1] < grey[1], "{right:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn decal_roughness_makes_a_matte_floor_shine_under_it() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A black floor lit only by a point light just above it shows its
        // specular highlight. A black decal over the whole view keeps the
        // color; its roughness alone sharpens or dulls the highlight.
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    base_color: [0.0, 0.0, 0.0, 1.0],
                    roughness: 1.0,
                    ..MaterialAsset::default()
                },
            )],
            [32, 32],
        );
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.point_lights.push(
            crate::runtime::ExtractedPointLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2004).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        0.0, 0.0, 0.5,
                    ))
                    .into(),
                },
                light: crate::runtime::PointLight {
                    color: [1.0; 3],
                    intensity: 20_000.0,
                    range: 10.0,
                    shadows: false,
                    radius: 0.0,
                },
            },
        );
        let peak = |scene: &mut SlabScene, roughness: Option<f32>| {
            scene.render_world.decals = vec![(
                Matrix4::new_nonuniform_scaling(&Vector3::new(2.0, 2.0, 0.5))
                    .into(),
                crate::runtime::Decal {
                    color: [0.0, 0.0, 0.0, 1.0],
                    roughness,
                    ..crate::runtime::Decal::default()
                },
            )];
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene
                .pixels()
                .chunks(4)
                .map(|pixel| pixel[..3].iter().copied().max().unwrap())
                .max()
                .unwrap()
        };
        let matte = peak(&mut scene, None);
        let wet = peak(&mut scene, Some(0.15));
        let dulled = peak(&mut scene, Some(1.0));
        eprintln!("peaks: matte {matte}, wet {wet}, dulled {dulled}");
        assert!(wet > matte.saturating_add(60), "{wet} vs {matte}");
        assert!(dulled.abs_diff(matte) <= 2, "{dulled} vs {matte}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn effect_budgets_shrink_with_the_quality_profile() {
        use crate::runtime::{
            gpu_particle_batch, ParticleBatch, ParticleBlend, ParticleEmitter,
            ParticleInstance, ParticleSimulation, ParticleSprite,
            ParticleSystem,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Six red decals in a row across a grey floor, and more particles
        // than Eco draws: 140000 empty CPU particles and the last one a
        // green square above the row, plus an idle GPU emitter's slots.
        let extent = [32, 32];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    base_color: [0.2, 0.2, 0.2, 1.0],
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        let xs: Vec<f32> = (0..6).map(|i| -0.75 + 0.3 * i as f32).collect();
        scene.render_world.decals = xs
            .iter()
            .map(|&x| {
                let matrix =
                    Matrix4::new_translation(&Vector3::new(x, 0.0, 0.0))
                        .prepend_nonuniform_scaling(&Vector3::new(
                            0.12, 0.12, 0.5,
                        ));
                let decal = crate::runtime::Decal {
                    color: [1.0, 0.0, 0.0, 1.0],
                    ..crate::runtime::Decal::default()
                };
                (matrix.into(), decal)
            })
            .collect();
        let mut instances = vec![ParticleInstance::default(); 140_000];
        *instances.last_mut().unwrap() = ParticleInstance {
            position: [0.0, 0.5, 0.5],
            size: 0.5,
            color: [0.0, 1.0, 0.0, 1.0],
            ..ParticleInstance::default()
        };
        scene.render_world.particles = vec![ParticleBatch {
            entity: None,
            blend: ParticleBlend::Additive,
            sprite: ParticleSprite::Square,
            instances,
        }];
        let emitter = ParticleEmitter {
            simulation: ParticleSimulation::Gpu,
            max_particles: 1 << 17,
            ..ParticleEmitter::default()
        };
        scene.render_world.gpu_particles = vec![gpu_particle_batch(
            bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
            &emitter,
            &ParticleSystem::default(),
            None,
            None,
        )];
        let total = 140_000 + (1 << 17);
        // Pixels are 1/16 m.
        let column = |x: f32| ((x + 1.0) * 16.0) as u32;
        for (quality, decals, particle) in [
            (QualityProfile::High, 6, true),
            (QualityProfile::Balanced, 6, true),
            (QualityProfile::Eco, 4, false),
        ] {
            scene.render_world.quality = quality;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let pixel_at = |x: u32, y: u32| {
                let at = ((y * extent[0] + x) * 4) as usize;
                [pixels[at + 2], pixels[at + 1], pixels[at]]
            };
            let pixel = |x: u32| pixel_at(x, 16);
            for (index, &x) in xs.iter().enumerate() {
                let [r, g, _] = pixel(column(x));
                assert_eq!(
                    r > g.saturating_add(40),
                    index < decals,
                    "{quality:?}: decal {index}"
                );
            }
            assert_eq!(
                // y = 0.5 is row 8 or 24, whichever way the view is up.
                pixel_at(16, 8)[1].max(pixel_at(16, 24)[1]) > 200,
                particle,
                "{quality:?}: last CPU particle"
            );
            let capacity = scene.renderer.capacity_diagnostics();
            assert_eq!(capacity.dropped_decals, 6 - decals, "{quality:?}");
            let budget = particle_budget(quality);
            let dropped = capacity.dropped_particles;
            if total <= budget {
                assert_eq!(dropped, 0, "{quality:?}");
            } else {
                // Each emitter rounds its share down.
                assert!(
                    (total - budget..=total - budget + 2).contains(&dropped),
                    "{quality:?}: {dropped} dropped"
                );
            }
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn spot_light_cookie_projects_its_image_onto_the_floor() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A spot light straight above the floor projects a cookie of four
        // colored quadrants; with the light's up along the camera's up,
        // each screen quadrant shows the matching image quadrant.
        let extent = [16, 16];
        let mut scene =
            SlabScene::with_extent(&[(0.0, MaterialAsset::default())], extent);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        let quadrant = |x: u32, y: u32| match (x < 8, y < 8) {
            (true, true) => [255, 0, 0, 255],
            (false, true) => [0, 255, 0, 255],
            (true, false) => [0, 0, 255, 255],
            (false, false) => [255, 255, 255, 255],
        };
        let cookie = scene.assets.textures.insert(TextureAsset {
            size: [16, 16],
            rgba8: (0..16)
                .flat_map(|y| (0..16).flat_map(move |x| quadrant(x, y)))
                .collect(),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        scene.render_world.spot_lights.push(
            crate::runtime::ExtractedSpotLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2003).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        0.0, 0.0, 3.0,
                    ))
                    .into(),
                },
                light: crate::runtime::SpotLight {
                    color: [1.0; 3],
                    intensity: 20_000.0,
                    range: 20.0,
                    inner_angle: 0.55,
                    outer_angle: 0.6,
                    shadows: false,
                    radius: 0.0,
                },
                cookie: Some(cookie),
            },
        );
        scene.render_world.lights_revision += 1;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let pixels = scene.pixels();
        // [r, g, b] lit or not, from the [b, g, r, a] readback.
        let lit = |x: u32, y: u32| {
            let at = ((y * extent[0] + x) * 4) as usize;
            [pixels[at + 2], pixels[at + 1], pixels[at]].map(|c| c > 100)
        };
        assert_eq!(lit(4, 4), [true, false, false], "top left is red");
        assert_eq!(lit(11, 4), [false, true, false], "top right is green");
        assert_eq!(lit(4, 11), [false, false, true], "bottom left is blue");
        assert_eq!(lit(11, 11), [true, true, true], "bottom right is white");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn color_lut_remaps_the_final_image() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A 4-square strip that swaps red and blue turns a red slab blue;
        // at half intensity it shows both.
        let extent = [16, 16];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        let level = |i: u32| (i * 255 / 3) as u8;
        let strip = scene.assets.textures.insert(TextureAsset {
            size: [16, 4],
            rgba8: (0..4)
                .flat_map(|g| {
                    (0..16).flat_map(move |x| {
                        [level(x / 4), level(g), level(x % 4), 255]
                    })
                })
                .collect(),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        let center = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let at = ((8 * extent[0] + 8) * 4) as usize;
            // [r, b] from the [b, g, r, a] readback.
            [pixels[at + 2], pixels[at]]
        };
        let [red, blue] = center(&mut scene);
        assert!(red > 150 && blue < 30, "plain slab is red: {red}, {blue}");
        scene.render_world.color_lut = Some((strip, 1.0));
        let [red, blue] = center(&mut scene);
        assert!(
            red < 30 && blue > 150,
            "full lut turns it blue: {red}, {blue}"
        );
        scene.render_world.color_lut = Some((strip, 0.5));
        let [red, blue] = center(&mut scene);
        assert!(red > 60 && blue > 60, "half lut mixes both: {red}, {blue}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn depth_of_field_blurs_only_away_from_the_focus() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // An unlit slab 5 m from the camera, white on the left half and
        // black on the right. In focus it matches the frame without lens
        // blur; nearer or farther focus softens the edge.
        let extent = [64, 64];
        let mut scene =
            SlabScene::with_extent(&[(0.0, MaterialAsset::default())], extent);
        let edge = scene.assets.textures.insert(TextureAsset {
            size: [128, 4],
            rgba8: (0..4)
                .flat_map(|_| {
                    (0..128).flat_map(|x| {
                        if x < 64 {
                            [255; 4]
                        } else {
                            [0, 0, 0, 255]
                        }
                    })
                })
                .collect(),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        scene.render_world.renderables[0].material =
            scene.assets.materials.insert(MaterialAsset {
                model: MaterialModel::Unlit,
                base_color_texture: Some(edge),
                ..MaterialAsset::default()
            });
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.pixels()
        };
        // Pixels on the middle row neither black nor white.
        let soft = |pixels: &[u8]| {
            (0..extent[0])
                .filter(|x| {
                    let green = pixels[((32 * extent[0] + x) * 4 + 1) as usize];
                    (30..=225).contains(&green)
                })
                .count()
        };
        for projection in [
            Projection::Orthographic {
                vertical_size: 2.0,
                near: 0.1,
                far: 100.0,
            },
            Projection::Perspective {
                vertical_fov_radians: 2.0 * (1.0f32 / 5.0).atan(),
                near: 0.1,
                far: 100.0,
            },
        ] {
            scene
                .render_world
                .active_camera
                .as_mut()
                .unwrap()
                .projection = projection;
            scene.render_world.depth_of_field = None;
            let sharp = frame(&mut scene);
            assert!(soft(&sharp) <= 3, "{projection:?}: {}", soft(&sharp));
            let mut lens = crate::runtime::DepthOfField {
                focus_distance: 5.0,
                blur: 0.05,
            };
            scene.render_world.depth_of_field = Some(lens);
            assert_eq!(frame(&mut scene), sharp, "{projection:?} in focus");
            for focus in [2.5, 50.0] {
                lens.focus_distance = focus;
                scene.render_world.depth_of_field = Some(lens);
                let blurred = soft(&frame(&mut scene));
                assert!(
                    blurred >= 4,
                    "{projection:?} focus {focus}: {blurred}"
                );
            }
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn motion_blur_smears_only_while_the_camera_moves() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The black and white edge of an unlit slab stays sharp while the
        // camera holds still and softens on the frame it slides sideways.
        let extent = [64, 64];
        let mut scene =
            SlabScene::with_extent(&[(0.0, MaterialAsset::default())], extent);
        let edge = scene.assets.textures.insert(TextureAsset {
            size: [128, 4],
            rgba8: (0..4)
                .flat_map(|_| {
                    (0..128).flat_map(|x| {
                        if x < 64 {
                            [255; 4]
                        } else {
                            [0, 0, 0, 255]
                        }
                    })
                })
                .collect(),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        scene.render_world.renderables[0].material =
            scene.assets.materials.insert(MaterialAsset {
                model: MaterialModel::Unlit,
                base_color_texture: Some(edge),
                ..MaterialAsset::default()
            });
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.pixels()
        };
        let soft = |pixels: &[u8]| {
            (0..extent[0])
                .filter(|x| {
                    let green = pixels[((32 * extent[0] + x) * 4 + 1) as usize];
                    (30..=225).contains(&green)
                })
                .count()
        };
        let slide = |scene: &mut SlabScene, x: f32| {
            scene.render_world.active_camera.as_mut().unwrap().transform =
                crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        x, 0.0, 5.0,
                    ))
                    .into(),
                };
        };
        let sharp = frame(&mut scene);
        assert!(soft(&sharp) <= 3, "{}", soft(&sharp));
        scene.render_world.motion_blur =
            Some(crate::runtime::MotionBlur { intensity: 1.0 });
        // The first frame has no last frame to move from.
        assert_eq!(frame(&mut scene), sharp, "first frame");
        assert_eq!(frame(&mut scene), sharp, "still camera");
        slide(&mut scene, 0.2);
        let moving = soft(&frame(&mut scene));
        assert!(moving >= 4, "sliding camera: {moving}");
        let held = frame(&mut scene);
        assert!(soft(&held) <= 3, "held again: {}", soft(&held));
        // A jump of half the screen is a cut.
        slide(&mut scene, -0.8);
        let cut = soft(&frame(&mut scene));
        assert!(cut <= 3, "cut: {cut}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn fxaa_smooths_a_stair_stepped_edge() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A white unlit slab turned slightly shows a stair-stepped edge
        // against the dark background. FXAA puts in-between shades along
        // it and leaves flat areas alone.
        let extent = [64, 64];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    base_color: [1.0; 4],
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        scene.render_world.renderables[0].transform =
            crate::runtime::GlobalTransform {
                matrix: (Matrix4::from_axis_angle(&Vector3::z_axis(), 0.3)
                    * Matrix4::new_translation(&Vector3::new(1.5, 0.0, 0.0))
                    * Matrix4::new_nonuniform_scaling(&Vector3::new(
                        2.0, 4.0, 0.1,
                    )))
                .into(),
            };
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.pixels()
        };
        let greens = |pixels: &[u8]| -> Vec<u8> {
            pixels.chunks(4).map(|pixel| pixel[1]).collect()
        };
        scene.render_world.antialiasing = Antialiasing::Off;
        let hard = greens(&frame(&mut scene));
        scene.render_world.antialiasing = Antialiasing::Fxaa;
        let smooth = greens(&frame(&mut scene));
        let white = *hard.iter().max().unwrap();
        let dark = *hard.iter().min().unwrap();
        let between = |shades: &[u8]| {
            shades
                .iter()
                .filter(|&&g| {
                    u16::from(g) > u16::from(dark) + 20
                        && u16::from(g) + 20 < u16::from(white)
                })
                .count()
        };
        assert!(between(&hard) < 4, "no AA: {}", between(&hard));
        assert!(between(&smooth) > 30, "FXAA: {}", between(&smooth));
        // Far from the edge nothing changes.
        assert_eq!(hard[0], smooth[0], "background");
        let inside = (32 * extent[0] + 62) as usize;
        assert_eq!(hard[inside], smooth[inside], "slab");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn taa_smooths_a_still_edge_and_leaves_no_ghost_when_it_moves() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The stair-stepped slab of the FXAA test: TAA's jittered frames
        // blend into in-between shades along the edge once the camera
        // holds still. When the slab then leaves, the history is clamped
        // to the new frame's colors, so no trace of it stays behind.
        let extent = [64, 64];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    base_color: [1.0; 4],
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        let placed = |x: f32| crate::runtime::GlobalTransform {
            matrix: (Matrix4::from_axis_angle(&Vector3::z_axis(), 0.3)
                * Matrix4::new_translation(&Vector3::new(x, 0.0, 0.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    2.0, 4.0, 0.1,
                )))
            .into(),
        };
        scene.render_world.renderables[0].transform = placed(1.5);
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene
                .pixels()
                .chunks(4)
                .map(|pixel| pixel[1])
                .collect::<Vec<u8>>()
        };
        scene.render_world.antialiasing = Antialiasing::Off;
        let hard = frame(&mut scene);
        scene.render_world.antialiasing = Antialiasing::Taa;
        let first = frame(&mut scene);
        let mut smooth = first.clone();
        for _ in 0..24 {
            smooth = frame(&mut scene);
        }
        let white = *hard.iter().max().unwrap();
        let dark = *hard.iter().min().unwrap();
        let between = |shades: &[u8]| {
            shades
                .iter()
                .filter(|&&g| {
                    u16::from(g) > u16::from(dark) + 20
                        && u16::from(g) + 20 < u16::from(white)
                })
                .count()
        };
        assert!(between(&hard) < 4, "no AA: {}", between(&hard));
        assert!(between(&first) < 4, "first TAA frame: {}", between(&first));
        assert!(between(&smooth) > 30, "TAA: {}", between(&smooth));
        assert_eq!(hard[0], smooth[0], "background");
        let inside = (32 * extent[0] + 62) as usize;
        assert_eq!(hard[inside], smooth[inside], "slab");
        scene.render_world.renderables[0].transform = placed(50.0);
        scene.render_world.renderables_revision += 1;
        let gone = frame(&mut scene);
        let brightest = *gone.iter().max().unwrap();
        assert!(
            u16::from(brightest) < u16::from(dark) + 20,
            "ghost of the slab: {brightest}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn sphere_light_widens_the_highlight_on_glossy_surfaces() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A black glossy floor shows only the specular reflection of a
        // point light just above it. A sphere light of the same intensity
        // reflects as a disc about its radius wide, not a pinpoint.
        let extent = [32, 32];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    base_color: [0.0, 0.0, 0.0, 1.0],
                    roughness: 0.15,
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.point_lights.push(
            crate::runtime::ExtractedPointLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2004).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        0.0, 0.0, 0.5,
                    ))
                    .into(),
                },
                light: crate::runtime::PointLight {
                    color: [1.0; 3],
                    intensity: 20_000.0,
                    range: 10.0,
                    shadows: false,
                    radius: 0.0,
                },
            },
        );
        let bright = |scene: &mut SlabScene, radius: f32| {
            scene.render_world.point_lights[0].light.radius = radius;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene
                .pixels()
                .chunks(4)
                .filter(|pixel| pixel[..3].iter().any(|&c| c > 100))
                .count()
        };
        let point = bright(&mut scene, 0.0);
        let sphere = bright(&mut scene, 0.4);
        eprintln!("bright pixels: point {point}, sphere {sphere}");
        assert!(point > 0, "the point light shows a highlight");
        assert!(
            sphere > point * 4,
            "the sphere light's highlight is wider: {sphere} vs {point}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn directional_light_shadow_darkens_receivers_only_when_enabled() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Floor slab at z = 0 fills the view; the occluder slab at x = 5 is
        // outside the view, and the light leans so its shadow lands on the
        // view center.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (1.0, MaterialAsset::default()),
        ]);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(5.0, 0.0, 1.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    4.0, 4.0, 0.1,
                )))
            .into();
        let direction = Vector3::new(-5.0, 0.0, -1.0).normalize();
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 500_000.0,
                    shadows: true,
                },
            },
        );
        let frame = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.center_pixel()[2]
        };
        let shadowed = frame(&mut scene);
        assert!(
            shadowed < 30,
            "occluder shadows the floor, got r={shadowed}"
        );

        // The occluder's mesh box lies outside the view: the main pass
        // culls it, the shadow pass still draws it.
        assert_eq!(scene.renderer.last_frame_culled(), Some(1));
        let stats = scene.renderer.culling_stats();
        assert_eq!(
            (stats.path, stats.submitted, stats.visible, stats.culled),
            (CullingPath::Cpu, 2, 1, 1)
        );
        assert!(stats.time.is_some());
        scene.render_world.culling = CullingMode::Disabled;
        let shadowed = frame(&mut scene);
        assert_eq!(scene.renderer.last_frame_culled(), Some(0));
        assert!(shadowed < 30, "unculled caster shadows, got r={shadowed}");
        scene.render_world.culling = CullingMode::Frustum;

        // A local override that misses the view culls a drawn object.
        scene.render_world.renderables[1].cast_shadows = false;
        let lit = frame(&mut scene);
        scene.render_world.renderables[0].bounds = Some(RenderBounds::Sphere {
            center: [0.0, 0.0, 500.0],
            radius: 1.0,
        });
        let culled = frame(&mut scene);
        assert_eq!(scene.renderer.last_frame_culled(), Some(2));
        assert!(
            culled + 100 < lit,
            "culled floor is not drawn, {culled} vs {lit}"
        );
        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.renderables[0].bounds = None;

        scene.render_world.renderables[1].cast_shadows = false;
        let lit = frame(&mut scene);
        assert!(lit > 150, "non-caster leaves the floor lit, got r={lit}");

        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.renderables[0].receive_shadows = false;
        let lit = frame(&mut scene);
        assert!(lit > 150, "non-receiver ignores shadows, got r={lit}");

        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.directional_lights[0].light.shadows = false;
        let lit = frame(&mut scene);
        assert!(lit > 150, "light without shadows lights it, got r={lit}");

        // From 40 units away the floor lies past the `Eco` shadow distance
        // but inside the `Balanced` and `High` ones, and each profile
        // renders into its own map size.
        scene.render_world.directional_lights[0].light.shadows = true;
        scene.render_world.active_camera.as_mut().unwrap().transform =
            crate::runtime::GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(0.0, 0.0, 40.0))
                    .into(),
            };
        for (quality, size, shadowed) in [
            (QualityProfile::Eco, 1024, false),
            (QualityProfile::Balanced, 2048, true),
            (QualityProfile::High, 4096, true),
            (QualityProfile::Eco, 1024, false),
        ] {
            scene.render_world.quality = quality;
            let red = frame(&mut scene);
            assert_eq!(red < 30, shadowed, "{quality:?}, got r={red}");
            assert_eq!(scene.renderer.shadow_framebuffer.extent(), [size; 2]);
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn near_cascade_resolves_a_thin_shadow() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A rod a few centimetres thick shadows the floor one unit below
        // the camera. One map over the whole 50-unit `Balanced` shadow
        // distance has texels wider than the rod; the near cascade has
        // texels a few millimetres wide.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (0.5, MaterialAsset::default()),
        ]);
        scene.render_world.quality = QualityProfile::Balanced;
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(2.5, 0.0, 0.5))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    1.0, 0.03, 0.03,
                )))
            .into();
        let camera = scene.render_world.active_camera.as_mut().unwrap();
        camera.transform.matrix =
            Matrix4::new_translation(&Vector3::new(0.0, 0.0, 1.0)).into();
        camera.projection = Projection::Orthographic {
            vertical_size: 0.01,
            near: 0.1,
            far: 100.0,
        };
        let direction = Vector3::new(-5.0, 0.0, -1.0).normalize();
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 500_000.0,
                    shadows: true,
                },
            },
        );
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let red = scene.center_pixel()[2];
        assert!(red < 30, "rod shadows the floor, got r={red}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn hard_shadows_leave_no_soft_edge() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The occluder at x 5..9, z 1 throws its shadow edge near x = 0 on
        // the floor; a 2-unit-wide view across that edge spans several
        // shadow texels, so 3x3 filtering shows a ramp there.
        let mut scene = SlabScene::with_extent(
            &[
                (0.0, MaterialAsset::default()),
                (1.0, MaterialAsset::default()),
            ],
            [64, 4],
        );
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(7.0, 0.0, 1.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    4.0, 4.0, 0.1,
                )))
            .into();
        let camera = scene.render_world.active_camera.as_mut().unwrap();
        camera.transform.matrix =
            Matrix4::new_translation(&Vector3::new(0.0, 0.0, 5.0)).into();
        camera.projection = Projection::Orthographic {
            vertical_size: 0.125,
            near: 0.1,
            far: 100.0,
        };
        let direction = Vector3::new(-5.0, 0.0, -1.0).normalize();
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 500_000.0,
                    shadows: true,
                },
            },
        );
        // Red of each pixel along the middle row.
        let row = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            (0..64)
                .map(|x| pixels[(2 * 64 + x) * 4 + 2])
                .collect::<Vec<_>>()
        };
        let between =
            |row: &[u8]| row.iter().filter(|r| (20..=230).contains(*r)).count();
        let soft = row(&mut scene);
        scene.render_world.hard_shadows = true;
        let hard = row(&mut scene);
        for row in [&soft, &hard] {
            assert!(
                row.iter().any(|r| *r < 20) && row.iter().any(|r| *r > 230),
                "the view crosses the shadow edge: {row:?}"
            );
        }
        assert!(between(&hard) <= 1, "hard edge: {hard:?}");
        assert!(between(&soft) > between(&hard), "soft edge: {soft:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_move_patches_instances_without_repacking() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Two unlit cubes, both drawn; then the centered one moves out of
        // the view under the same layout revision.
        let unlit = MaterialAsset {
            model: MaterialModel::Unlit,
            ..MaterialAsset::default()
        };
        let mut scene = SlabScene::new(&[(0.0, unlit.clone()), (0.0, unlit)]);
        scene.render_world.renderables_layout_revision = 1;
        scene.render_world.renderables[0].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(-0.75, 0.0, 0.0))
                * Matrix4::new_scaling(0.2))
            .into();
        scene.render_world.renderables[1].transform.matrix =
            Matrix4::new_scaling(0.5).into();
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.center_pixel()
        };
        assert!(frame(&mut scene)[0] > 200);
        assert_eq!(scene.renderer.last_frame_culled(), Some(0));
        let sources = scene
            .renderer
            .prepared_instances
            .as_ref()
            .unwrap()
            .sources
            .clone();

        let moved: [[f32; 4]; 4] =
            Matrix4::new_translation(&Vector3::new(3.0, 0.0, 0.0)).into();
        scene.render_world.renderables[1].transform.matrix = moved;
        scene.render_world.renderables_revision += 1;
        let center = frame(&mut scene);
        assert!(center[0] < 20, "the moved cube left the center: {center:?}");
        // Patched in place: same order, new matrix and bounds, so the CPU
        // frustum test now culls the moved cube.
        let prepared = scene.renderer.prepared_instances.as_ref().unwrap();
        assert_eq!(prepared.sources, sources);
        let slot = sources.iter().position(|&source| source == 1).unwrap();
        assert_eq!(prepared.packed[slot].model, moved);
        assert_eq!(scene.renderer.last_frame_culled(), Some(1));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn culling_follows_the_camera_and_mesh_edits_without_scene_changes() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Two unlit cubes in one batch: the first left of the ±1
        // orthographic view, the second small and centered.
        let unlit = MaterialAsset {
            model: MaterialModel::Unlit,
            ..MaterialAsset::default()
        };
        let mut scene = SlabScene::new(&[(0.0, unlit.clone()), (0.0, unlit)]);
        let material = scene.render_world.renderables[0].material;
        scene.render_world.renderables[1].material = material;
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_translation(&Vector3::new(-3.0, 0.0, 0.0)).into();
        scene.render_world.renderables[1].transform.matrix =
            Matrix4::new_scaling(0.5).into();
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.center_pixel()
        };
        let center = frame(&mut scene);
        assert_eq!(scene.renderer.last_frame_culled(), Some(1));
        assert!(
            center[0] > 200,
            "the kept instance, not the culled one, draws: {center:?}"
        );

        // Growing the mesh is an asset edit, not a scene change: the
        // renderer re-derives the bounds from the re-uploaded mesh.
        for vertex in &mut scene
            .assets
            .meshes
            .get_mut(scene.assets.fallback_mesh)
            .unwrap()
            .vertices
        {
            vertex.position[0] *= 10.0;
            vertex.position[1] *= 10.0;
        }
        frame(&mut scene);
        assert_eq!(scene.renderer.last_frame_culled(), Some(0));

        // Moving only the camera re-culls the unchanged instances.
        scene
            .render_world
            .active_camera
            .as_mut()
            .unwrap()
            .transform
            .matrix =
            Matrix4::new_translation(&Vector3::new(50.0, 0.0, 5.0)).into();
        frame(&mut scene);
        assert_eq!(scene.renderer.last_frame_culled(), Some(2));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn pbr_maps_and_unlit_model_shape_the_lit_color() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        use crate::assets::Assets;
        // Renders one camera-facing slab and returns the center [r, g, b].
        let render = |ambient: f32,
                      light: Option<Vector3<f32>>,
                      material: &dyn Fn(
            &mut Assets<TextureAsset>,
        ) -> MaterialAsset| {
            let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
            let handle = scene.render_world.renderables[0].material;
            let material = material(&mut scene.assets.textures);
            *scene.assets.materials.get_mut(handle).unwrap() = material;
            scene.render_world.ambient_light =
                Some(crate::runtime::AmbientLight {
                    color: [ambient; 3],
                    intensity: 1.0,
                });
            if let Some(direction) = light {
                scene.render_world.directional_lights.push(
                    crate::runtime::ExtractedDirectionalLight {
                        entity: bevy_ecs::entity::Entity::from_raw_u32(2000)
                            .unwrap(),
                        transform: crate::runtime::GlobalTransform {
                            matrix: nalgebra::Rotation3::rotation_between(
                                &-Vector3::z(),
                                &direction.normalize(),
                            )
                            .unwrap()
                            .to_homogeneous()
                            .into(),
                        },
                        light: crate::runtime::DirectionalLight {
                            color: [1.0; 3],
                            illuminance: 50_000.0,
                            shadows: false,
                        },
                    },
                );
            }
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };
        let texture = |rgba: [u8; 4], color_space| TextureAsset {
            size: [1, 1],
            rgba8: rgba.to_vec(),
            color_space,
            sampler: TextureSampler::default(),
        };
        let facing = Some(-Vector3::z());
        let red = [1.0, 0.0, 0.0, 1.0];

        let unlit = render(0.0, facing, &|_| MaterialAsset {
            model: MaterialModel::Unlit,
            base_color: red,
            ..MaterialAsset::default()
        });
        assert_eq!(unlit, [255, 0, 0], "unlit shows base color as is");
        let dark = render(0.0, None, &|_| MaterialAsset {
            base_color: red,
            ..MaterialAsset::default()
        });
        assert_eq!(dark, [0, 0, 0], "PBR without light is black");

        let emissive =
            |textures: &mut Assets<TextureAsset>, masked: bool| MaterialAsset {
                emissive: [0.0, 1.0, 0.0],
                emissive_texture: masked.then(|| {
                    textures.insert(texture(
                        [255, 0, 0, 255],
                        TextureColorSpace::Srgb,
                    ))
                }),
                ..MaterialAsset::default()
            };
        assert_eq!(render(0.0, None, &|t| emissive(t, false))[1], 255);
        assert_eq!(
            render(0.0, None, &|t| emissive(t, true)),
            [0, 0, 0],
            "emissive map multiplies the factor"
        );

        let occluded = |textures: &mut Assets<TextureAsset>| MaterialAsset {
            occlusion_texture: Some(
                textures
                    .insert(texture([0, 0, 0, 255], TextureColorSpace::Linear)),
            ),
            ..MaterialAsset::default()
        };
        assert_eq!(render(1.0, None, &|_| MaterialAsset::default()), [255; 3]);
        assert_eq!(
            render(1.0, None, &occluded),
            [0, 0, 0],
            "occlusion map darkens ambient"
        );

        // A rough white metal has no diffuse lobe, so it is darker than a
        // dielectric under the same light. A metallic-roughness map with
        // blue 0 turns the metal back into the dielectric.
        let surface = |metallic: f32, map: Option<[u8; 4]>| {
            move |textures: &mut Assets<TextureAsset>| MaterialAsset {
                metallic,
                roughness: 1.0,
                metallic_roughness_texture: map.map(|rgba| {
                    textures.insert(texture(rgba, TextureColorSpace::Linear))
                }),
                ..MaterialAsset::default()
            }
        };
        let dielectric = render(0.0, facing, &surface(0.0, None))[0];
        let metal = render(0.0, facing, &surface(1.0, None))[0];
        let unmasked =
            render(0.0, facing, &surface(1.0, Some([0, 255, 0, 255])))[0];
        assert!(
            metal + 40 < dielectric,
            "metal {metal} vs dielectric {dielectric}"
        );
        assert!(
            unmasked.abs_diff(dielectric) <= 2,
            "map blue 0 removes metalness: {unmasked} vs {dielectric}"
        );

        // A normal map tilting the surface toward a grazing light brightens
        // it. The slab's tangent is +X, so tangent-space (1, 0, 1) faces
        // the light.
        let grazing = Some(Vector3::new(-1.0, 0.0, -1.0));
        let tilted =
            |textures: &mut Assets<TextureAsset>, mapped: bool| MaterialAsset {
                roughness: 1.0,
                normal_texture: mapped.then(|| {
                    textures.insert(texture(
                        [218, 128, 218, 255],
                        TextureColorSpace::Linear,
                    ))
                }),
                ..MaterialAsset::default()
            };
        let flat = render(0.0, grazing, &|t| tilted(t, false))[0];
        let mapped = render(0.0, grazing, &|t| tilted(t, true))[0];
        assert!(mapped > flat + 15, "normal map {mapped} vs flat {flat}");
    }

    #[test]
    fn optional_features_need_every_bindless_bit_and_accept_either_indirect_count_source(
    ) {
        use vulkano::device::DeviceFeatures;
        let none = DeviceExtensions::empty();
        assert_eq!(
            optional_features(&DeviceFeatures::empty(), &none),
            [false; 5]
        );
        let partial_bindless = DeviceFeatures {
            runtime_descriptor_array: true,
            descriptor_binding_partially_bound: true,
            shader_sampled_image_array_non_uniform_indexing: true,
            ..DeviceFeatures::empty()
        };
        assert!(!optional_features(&partial_bindless, &none)[2]);
        let bindless = DeviceFeatures {
            descriptor_binding_variable_descriptor_count: true,
            ..partial_bindless
        };
        assert!(optional_features(&bindless, &none)[2]);
        let khr_count = DeviceExtensions {
            khr_draw_indirect_count: true,
            ext_memory_budget: true,
            ..DeviceExtensions::empty()
        };
        assert_eq!(
            optional_features(&DeviceFeatures::empty(), &khr_count),
            [false, true, false, true, false]
        );
        let core_count = DeviceFeatures {
            draw_indirect_count: true,
            multi_draw_indirect: true,
            ..DeviceFeatures::empty()
        };
        assert_eq!(
            optional_features(&core_count, &none),
            [true, true, false, false, false]
        );
        assert!(!Capability {
            supported: true,
            enabled: false
        }
        .usable());
        let anisotropy = DeviceFeatures {
            sampler_anisotropy: true,
            ..DeviceFeatures::empty()
        };
        assert_eq!(
            optional_features(&anisotropy, &none),
            [false, false, false, false, true]
        );
    }

    #[test]
    fn anisotropy_needs_the_feature_and_linear_filtering_and_caps_at_16() {
        use crate::assets::TextureFilter::{Linear, Nearest};
        assert_eq!(sampler_anisotropy(true, 16.0, Linear), Some(16.0));
        assert_eq!(sampler_anisotropy(true, 64.0, Linear), Some(16.0));
        assert_eq!(sampler_anisotropy(true, 8.0, Linear), Some(8.0));
        assert_eq!(sampler_anisotropy(false, 16.0, Linear), None);
        assert_eq!(sampler_anisotropy(true, 16.0, Nearest), None);
        assert_eq!(sampler_anisotropy(true, 1.0, Linear), None);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn renderer_reports_detected_capabilities_and_enables_none_it_does_not_use()
    {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let scene = SlabScene::new(&[]);
        let capabilities = scene.renderer.capabilities();
        let physical = scene.base.device.physical_device();
        assert_eq!(capabilities.device_name, physical.properties().device_name);
        assert!(capabilities.device_local_bytes > 0);
        assert_eq!(
            capabilities.multi_draw_indirect.supported,
            physical.supported_features().multi_draw_indirect
        );
        // Nothing uses these yet, so the device must not pay for them.
        for capability in [
            capabilities.multi_draw_indirect,
            capabilities.draw_indirect_count,
            capabilities.bindless_textures,
        ] {
            assert!(!capability.enabled, "{capabilities:?}");
        }
        eprintln!("{capabilities:#?}");
    }

    #[test]
    fn culling_path_follows_mode_ownership_count_device_and_quality() {
        let capabilities = |integrated_gpu| RendererCapabilities {
            device_name: String::new(),
            integrated_gpu,
            device_local_bytes: 8 << 30,
            multi_draw_indirect: Capability::default(),
            draw_indirect_count: Capability::default(),
            bindless_textures: Capability::default(),
            memory_budget: Capability::default(),
            sampler_anisotropy: Capability::default(),
            timestamp_queries: false,
            msaa_samples: 1,
            msaa_2x: false,
        };
        let path = |mode, instances, gpu_owned, quality, integrated| {
            select_culling_path(
                mode,
                instances,
                gpu_owned,
                false,
                quality,
                &capabilities(integrated),
            )
        };
        let (auto, high) = (CullingMode::Auto, QualityProfile::High);
        let big = GPU_CULL_MIN_INSTANCES;
        let small = AUTO_DIRECT_MAX_INSTANCES;
        // `Auto` draws small scenes directly, GPU bodies included; an
        // explicit `Frustum` still culls them.
        assert_eq!(path(auto, small - 1, 0, high, false), CullingPath::Direct);
        assert_eq!(path(auto, small - 1, 1, high, false), CullingPath::Direct);
        assert_eq!(path(auto, small, 0, high, false), CullingPath::Cpu);
        let frustum = CullingMode::Frustum;
        assert_eq!(path(frustum, 10, 0, high, false), CullingPath::Cpu);
        assert_eq!(path(frustum, 10, 1, high, false), CullingPath::Gpu);
        // Occlusion needs the GPU path at any size and on any device.
        let occlusion = CullingMode::FrustumAndOcclusion;
        assert_eq!(
            path(occlusion, 1, 0, high, true),
            CullingPath::GpuOcclusion
        );
        assert_eq!(
            path(occlusion, 1, 1, QualityProfile::Eco, false),
            CullingPath::GpuOcclusion
        );
        assert!(
            CullingPath::GpuOcclusion.on_gpu() && CullingPath::Gpu.on_gpu()
        );
        assert!(!CullingPath::Cpu.on_gpu() && !CullingPath::Direct.on_gpu());
        assert_eq!(path(auto, big - 1, 0, high, false), CullingPath::Cpu);
        assert_eq!(path(auto, big, 0, high, false), CullingPath::Gpu);
        assert_eq!(
            path(CullingMode::Frustum, big, 0, high, false),
            CullingPath::Gpu
        );
        // GPU-owned bodies need the GPU whatever the count.
        assert_eq!(path(auto, small, 1, high, false), CullingPath::Gpu);
        assert_eq!(path(auto, small, 1, high, true), CullingPath::Gpu);
        // Integrated GPUs and Eco raise the bar four times.
        assert_eq!(path(auto, big, 0, high, true), CullingPath::Cpu);
        assert_eq!(
            path(auto, big, 0, QualityProfile::Eco, false),
            CullingPath::Cpu
        );
        assert_eq!(path(auto, big * 4, 0, high, true), CullingPath::Gpu);
        // Disabled draws everything, even GPU bodies.
        assert_eq!(
            path(CullingMode::Disabled, big * 4, 1, high, false),
            CullingPath::Direct
        );
        // LOD levels need a path that picks one: never `Direct`.
        let lod_path = |mode, gpu_owned| {
            select_culling_path(
                mode,
                small - 1,
                gpu_owned,
                true,
                high,
                &capabilities(false),
            )
        };
        assert_eq!(lod_path(auto, 0), CullingPath::Cpu);
        assert_eq!(lod_path(CullingMode::Disabled, 0), CullingPath::Cpu);
        assert_eq!(lod_path(CullingMode::Disabled, 1), CullingPath::Gpu);
        assert_eq!(lod_path(occlusion, 0), CullingPath::GpuOcclusion);
    }

    #[test]
    fn lod_value_grows_with_distance_like_group_ranges() {
        // A perspective camera at the origin with a 90 degree view.
        let camera = [0.0, 0.0, 0.0, 1.0];
        let sphere = [0.0, 0.0, -10.0, 1.0];
        assert_eq!(lod_value(camera, sphere, false), 10.0);
        // Radius 1 at distance 10 spans a tenth of the view height.
        assert_eq!(lod_value(camera, sphere, true), 10.0);
        let group = LodGroupAsset {
            metric: LodMetric::ScreenSize,
            levels: [0.5, 0.2, 0.05]
                .map(|until| crate::assets::LodLevel {
                    mesh: AssetServer::default().fallback_mesh,
                    until,
                })
                .to_vec(),
        };
        let ranges = group.ranges();
        let level = |value| {
            ranges
                .iter()
                .position(|[start, end]| start <= &value && &value < end)
        };
        assert_eq!(level(lod_value(camera, sphere, true)), Some(2));
        assert_eq!(
            level(lod_value(camera, [0.0, 0.0, -3.0, 1.0], true)),
            Some(1)
        );
        assert_eq!(
            level(lod_value(camera, [0.0, 0.0, -1.0, 1.0], true)),
            Some(0)
        );
        assert_eq!(
            level(lod_value(camera, [0.0, 0.0, -50.0, 1.0], true)),
            None
        );
        // Orthographic size ignores distance: half height 2 over radius 1.
        assert_eq!(lod_value([0.0, 0.0, 0.0, -2.0], sphere, true), 2.0);
        // Without bounds an object counts as close.
        assert_eq!(lod_value(camera, [0.0, 0.0, -10.0, -1.0], true), 0.0);
        // The CPU sphere matches the shader's: the largest scale sizes it.
        let model = (Matrix4::new_translation(&Vector3::new(1.0, 0.0, 0.0))
            * Matrix4::new_nonuniform_scaling(&Vector3::new(2.0, 3.0, 1.0)))
        .into();
        assert_eq!(
            world_sphere(&model, [1.0, 0.0, 0.0, 1.0]),
            [3.0, 0.0, 0.0, 3.0]
        );
    }

    #[test]
    fn cull_gpu_layouts_match_shader_structs() {
        assert_eq!(std::mem::size_of::<CullInstance>(), 64);
        assert_eq!(std::mem::size_of::<DrawIndexedIndirectCommand>(), 20);
        // Clip matrix, viewport, LOD camera, and info: under the 128-byte
        // guaranteed push-constant range.
        assert_eq!(std::mem::size_of::<CullPushConstants>(), 112);
        // The shader's phase numbers.
        assert_eq!(
            [CullPhase::Frustum, CullPhase::Early, CullPhase::Late]
                .map(|phase| phase as u32),
            [0, 1, 2]
        );
        assert_eq!(
            bounding_sphere(RenderBounds::Aabb {
                min: [-1.0, -2.0, -2.0],
                max: [1.0, 2.0, 2.0],
            }),
            [0.0, 0.0, 0.0, 3.0]
        );
    }

    #[test]
    fn auto_quality_resolves_from_device_class_and_concrete_profiles_pass_through(
    ) {
        let capabilities =
            |integrated_gpu, gib: DeviceSize| RendererCapabilities {
                device_name: String::new(),
                integrated_gpu,
                device_local_bytes: gib << 30,
                multi_draw_indirect: Capability::default(),
                draw_indirect_count: Capability::default(),
                bindless_textures: Capability::default(),
                memory_budget: Capability::default(),
                sampler_anisotropy: Capability::default(),
                timestamp_queries: false,
                msaa_samples: 1,
                msaa_2x: false,
            };
        let auto = |caps| resolve_quality(QualityProfile::Auto, &caps);
        assert_eq!(auto(capabilities(true, 16)), QualityProfile::Eco);
        assert_eq!(auto(capabilities(false, 2)), QualityProfile::Balanced);
        assert_eq!(auto(capabilities(false, 12)), QualityProfile::High);
        // The memory cut is at exactly 4 GiB.
        assert_eq!(auto(capabilities(false, 3)), QualityProfile::Balanced);
        assert_eq!(auto(capabilities(false, 4)), QualityProfile::High);
        assert_eq!(
            resolve_quality(QualityProfile::High, &capabilities(true, 1)),
            QualityProfile::High
        );
        let budgets = [
            QualityProfile::Eco,
            QualityProfile::Balanced,
            QualityProfile::High,
        ]
        .map(light_budget);
        assert!(budgets.is_sorted() && budgets[2] == MAX_LIGHTS);
    }

    #[test]
    fn baseline_shortfalls_name_every_missing_property() {
        assert!(LOW_END_BASELINE.shortfalls(&LOW_END_BASELINE).is_empty());
        let weak = DeviceLimits {
            api_version: vulkano::Version::V1_0,
            max_compute_work_group_invocations: 128,
            max_compute_work_group_size_x: 128,
            depth_attachment: false,
            ..LOW_END_BASELINE
        };
        let shortfalls = weak.shortfalls(&LOW_END_BASELINE);
        assert_eq!(shortfalls.len(), 4, "{shortfalls:?}");
        assert!(shortfalls[0].starts_with("Vulkan 1.0"));
        assert_eq!(shortfalls[1], "maxComputeWorkGroupInvocations 128 < 256");
        assert!(shortfalls[3].contains("D32_SFLOAT"));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn test_device_meets_the_low_end_baseline() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let base = crate::rendering::test_support::headless_device();
        let limits = DeviceLimits::of(base.device.physical_device());
        assert_eq!(limits.shortfalls(&LOW_END_BASELINE), Vec::<String>::new());
    }

    #[test]
    fn transient_budget_is_half_the_largest_device_local_heap() {
        assert_eq!(
            transient_upload_budget([
                (64 << 30, MemoryHeapFlags::empty()),
                (12 << 30, MemoryHeapFlags::DEVICE_LOCAL),
                (256 << 20, MemoryHeapFlags::DEVICE_LOCAL),
            ]),
            6 << 30
        );
        assert_eq!(transient_upload_budget([]), DeviceSize::MAX);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn instance_uploads_reuse_arenas_grow_on_demand_and_respect_budget() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        let frame = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let instances =
                &scene.renderer.prepared_instances.as_ref().unwrap();
            let buffer = instances.instances.buffer().clone();
            buffer
        };
        let first = frame(&mut scene);
        let second = frame(&mut scene);
        assert!(
            Arc::ptr_eq(&first, &second),
            "small re-uploads suballocate the same arena, not a new buffer"
        );

        let template = scene.render_world.renderables[0];
        scene.render_world.renderables = (0..5_000u32)
            .map(|index| crate::runtime::ExtractedRenderable {
                entity: bevy_ecs::entity::Entity::from_raw_u32(index + 1)
                    .unwrap(),
                ..template
            })
            .collect();
        let grown = frame(&mut scene);
        let needed =
            5_000 * std::mem::size_of::<RenderInstanceUpload>() as DeviceSize;
        assert!(!Arc::ptr_eq(&first, &grown));
        assert!(grown.size() >= needed, "arena grew to fit the upload");
        let [b, g, r, _] = scene.center_pixel();
        assert_eq!([b, g, r], [255, 255, 255]);

        scene.renderer.instance_budget = needed - 1;
        scene.render_world.renderables_revision += 1;
        let before = scene.now();
        let error = scene
            .renderer
            .render(
                before,
                ImageView::new_default(scene.image.clone()).unwrap(),
                scene.extent,
                SceneRenderOptions::game(scene.extent),
                &scene.render_world,
                &scene.assets,
            )
            .err()
            .expect("over-budget upload is an explicit error");
        assert!(error.0.contains("budget"), "{error:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn frame_contexts_bound_frames_in_flight_and_wait_only_on_reuse() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        // Frames are handed back unflushed, as a caller may hold them before
        // presenting, so their fences stay unsignaled until someone flushes.
        let mut held = Vec::new();
        let mut fences = Vec::new();
        for slot in 0..FRAMES_IN_FLIGHT {
            let before = scene.now();
            held.push(scene.render(before));
            fences.push(
                scene.renderer.frame_contexts[slot].fence.clone().unwrap(),
            );
        }
        assert_eq!(scene.renderer.frame_index, 0, "ring wrapped");
        assert!(
            fences.iter().all(|f| !f.is_signaled().unwrap()),
            "unfinished frames keep their fences"
        );

        let before = scene.now();
        let next = scene.render(before);
        assert!(
            fences[0].is_signaled().unwrap(),
            "reusing context 0 first submitted and waited for its frame"
        );
        assert!(
            !fences[1].is_signaled().unwrap(),
            "contexts that are not reused are never waited on"
        );
        assert!(!Arc::ptr_eq(
            scene.renderer.frame_contexts[0].fence.as_ref().unwrap(),
            &fences[0]
        ));
        next.then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        drop(held);
        let [b, g, r, _] = scene.center_pixel();
        assert_eq!([b, g, r], [255, 255, 255]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn debug_lines_blend_with_their_alpha() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        scene.render_world.background_color = [1.0, 1.0, 1.0, 1.0];
        let mut overlay = RenderDebugOverlay::default();
        overlay.lines.push(DebugLine {
            start: [-1.0, 0.0, 0.0],
            end: [1.0, 0.0, 0.0],
            color: [1.0, 0.0, 0.0, 0.5],
            thickness: 4.0,
            on_top: true,
        });
        let before = scene.now();
        scene
            .renderer
            .render(
                before,
                ImageView::new_default(scene.image.clone()).unwrap(),
                scene.extent,
                SceneRenderOptions {
                    debug_overlay: Some(&overlay),
                    ..SceneRenderOptions::game(scene.extent)
                },
                &scene.render_world,
                &scene.assets,
            )
            .unwrap()
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert_eq!(r, 255);
        // Blending happens in linear space on the sRGB target, so half
        // alpha stores about 188 rather than 128. Opaque red would store 0.
        assert!((150..=220).contains(&b) && b == g, "{b} {g}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn frames_record_the_declared_passes_with_their_layouts() {
        use crate::rendering::frame_passes::{FramePass, FrameResource};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let mut overlay = RenderDebugOverlay::default();
        overlay.lines.push(DebugLine {
            start: [-1.0, 0.0, 0.0],
            end: [1.0, 0.0, 0.0],
            color: [1.0, 0.0, 0.0, 1.0],
            thickness: 4.0,
            on_top: true,
        });
        // The second frame has no uploads or physics ticks left.
        for _ in 0..2 {
            let before = scene.now();
            scene
                .renderer
                .render(
                    before,
                    ImageView::new_default(scene.image.clone()).unwrap(),
                    scene.extent,
                    SceneRenderOptions {
                        debug_overlay: Some(&overlay),
                        ..SceneRenderOptions::game(scene.extent)
                    },
                    &scene.render_world,
                    &scene.assets,
                )
                .unwrap()
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
        }
        assert_eq!(
            scene.renderer.last_frame_passes(),
            [
                FramePass::Shadow,
                FramePass::Scene,
                FramePass::ToneMap,
                FramePass::DebugOverlay
            ]
        );
        // Each recorded pass has its own GPU time from its timestamp pair.
        let times = scene.renderer.gpu_pass_times();
        if scene.renderer.frame_contexts[0].timestamps.is_some() {
            assert_eq!(
                times.0.iter().map(|(pass, _)| *pass).collect::<Vec<_>>(),
                scene.renderer.last_frame_passes()
            );
            assert!(times.total() > Duration::ZERO, "{times:?}");
        } else {
            assert!(times.0.is_empty());
        }
        // The empty scene records the tone map and one debug-line draw.
        let counters = scene.renderer.render_counters();
        assert_eq!(
            (counters.draws, counters.dispatches, counters.triangles),
            (2, 0, 0),
            "{counters:?}"
        );
        assert!(counters.upload_bytes > 0, "{counters:?}");
        assert!(counters.gpu_memory_bytes > 0, "{counters:?}");
        let mut timings = CpuFrameTimings::default();
        scene.renderer.write_cpu_timings(&mut timings);
        assert!(timings.preparation > Duration::ZERO, "{timings:?}");
        assert!(timings.recording > Duration::ZERO, "{timings:?}");
        let shadow = scene.renderer.shadow_framebuffer.render_pass();
        assert_eq!(
            shadow.attachments()[0].final_layout,
            FramePass::Shadow
                .next_layout(FrameResource::ShadowMap)
                .unwrap()
        );
        let main = &scene.renderer.passes.render_pass;
        let hdr_read = FramePass::ToneMap.accesses()[0];
        assert_eq!(hdr_read.resource, FrameResource::HdrColor);
        assert_eq!(
            main.subpasses()[1].input_attachments[0]
                .as_ref()
                .unwrap()
                .layout,
            hdr_read.layout.unwrap()
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn oversized_viewport_is_cropped_not_squashed() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let mut overlay = RenderDebugOverlay::default();
        overlay.lines.push(DebugLine {
            start: [-1.0, 0.0, 0.0],
            end: [1.0, 0.0, 0.0],
            color: [1.0, 0.0, 0.0, 1.0],
            thickness: 4.0,
            on_top: true,
        });
        // Twice the target height: the line at the view's center now falls
        // on the target's bottom edge, not through its center.
        let before = scene.now();
        scene
            .renderer
            .render(
                before,
                ImageView::new_default(scene.image.clone()).unwrap(),
                scene.extent,
                SceneRenderOptions {
                    viewport: SceneViewport {
                        offset: [0, 0],
                        extent: [scene.extent[0], scene.extent[1] * 2],
                    },
                    debug_overlay: Some(&overlay),
                    debug_view: SceneDebugView::Lit,
                    effects: SceneEffects::default(),
                    camera: None,
                    grading: None,
                    exposure: 1.0,
                },
                &scene.render_world,
                &scene.assets,
            )
            .unwrap()
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert_ne!([b, g, r], [0, 0, 255]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn removed_mesh_assets_leave_the_gpu_cache() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        let cube = scene.assets.meshes.get(scene.assets.fallback_mesh).cloned();
        let mesh = scene.assets.meshes.insert(cube.unwrap());
        scene.render_world.renderables[0].mesh = mesh;
        scene.render_world.renderables_revision += 1;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert!(scene.renderer.prepared_meshes.contains_key(&mesh.key()));

        scene.render_world.renderables[0].mesh = scene.assets.fallback_mesh;
        scene.render_world.renderables_revision += 1;
        scene.assets.meshes.remove(mesh).unwrap();
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert!(!scene.renderer.prepared_meshes.contains_key(&mesh.key()));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn transient_uploads_come_from_per_context_arenas_across_reuse() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let mut overlay = RenderDebugOverlay::default();
        overlay.lines.push(DebugLine {
            start: [-1.0, 0.0, 0.0],
            end: [1.0, 0.0, 0.0],
            color: [1.0, 0.0, 0.0, 1.0],
            thickness: 4.0,
            on_top: true,
        });
        // Two full laps, so every context's arena is reused after its wait.
        for frame in 0..FRAMES_IN_FLIGHT * 2 {
            let before = scene.now();
            scene
                .renderer
                .render(
                    before,
                    ImageView::new_default(scene.image.clone()).unwrap(),
                    scene.extent,
                    SceneRenderOptions {
                        debug_overlay: Some(&overlay),
                        ..SceneRenderOptions::game(scene.extent)
                    },
                    &scene.render_world,
                    &scene.assets,
                )
                .unwrap()
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            assert_eq!([b, g, r], [0, 0, 255], "debug line in frame {frame}");
        }
        let arena = |context: &FrameContext| {
            context
                .transient
                .allocate_sized::<u32>()
                .unwrap()
                .buffer()
                .clone()
        };
        assert!(
            !Arc::ptr_eq(
                &arena(&scene.renderer.frame_contexts[0]),
                &arena(&scene.renderer.frame_contexts[1])
            ),
            "contexts never share an arena"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn physics_events_read_back_from_transient_arenas_every_tick() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuCondition,
            GpuEventId, GpuEventMode, GpuEventPayload,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000).unwrap(),
            physics_id: Default::default(),
            transform: crate::Transform::new([0.0, 5.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            sync: Default::default(),
            custom_shader: None,
            rules: vec![ExtractedGpuPhysicsRule {
                event_id: GpuEventId(7),
                instructions: GpuCondition::position_y()
                    .greater_than(0.0)
                    .compile()
                    .unwrap(),
                mode: GpuEventMode::WhileTrue,
                payload: GpuEventPayload::None,
                cooldown_seconds: 0.0,
            }],
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.fixed_delta_seconds = 1.0 / 60.0;
        // More ticks than contexts, so readback arenas are reused.
        for tick in 1..=(FRAMES_IN_FLIGHT as u64 * 2) {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let events = scene.renderer.take_completed_physics_events();
            assert_eq!(events.len(), 1, "tick {tick}: {events:?}");
            assert_eq!(events[0].event_id, 7);
            assert_eq!(events[0].tick_low, tick as u32);
        }
        assert_eq!(
            scene.renderer.capacity_diagnostics().physics_events_dropped,
            0
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_timers_count_simulated_ticks_not_the_frame_clock() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuCondition,
            GpuEventId, GpuEventMode, GpuEventPayload,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000).unwrap(),
            physics_id: Default::default(),
            transform: crate::Transform::new([0.0, 5.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            sync: Default::default(),
            custom_shader: None,
            rules: vec![ExtractedGpuPhysicsRule {
                event_id: GpuEventId(7),
                instructions: GpuCondition::timer_elapsed(0.04)
                    .compile()
                    .unwrap(),
                mode: GpuEventMode::WhileTrue,
                payload: GpuEventPayload::None,
                cooldown_seconds: 0.0,
            }],
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.fixed_delta_seconds = 1.0 / 60.0;
        // A frame clock far past the timer; only ticks 3 and 4 (0.05 s
        // and later) are.
        world.elapsed_seconds = 1000.0;
        for tick in 1..=4 {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let events = scene.renderer.take_completed_physics_events();
            assert_eq!(events.len(), usize::from(tick >= 3), "tick {tick}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn event_buffer_fits_every_step_and_reports_losses_past_budget() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuCondition,
            GpuEventId, GpuEventMode, GpuEventPayload, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        const BODIES: u32 = 100;
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = (0..BODIES)
            .map(|slot| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new([slot as f32 * 2.0, 5.0, 0.0]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                sync: Default::default(),
                rules: vec![ExtractedGpuPhysicsRule {
                    event_id: GpuEventId(1),
                    instructions: GpuCondition::position_y()
                        .greater_than(0.0)
                        .compile()
                        .unwrap(),
                    mode: GpuEventMode::WhileTrue,
                    payload: GpuEventPayload::None,
                    cooldown_seconds: 0.0,
                }],
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.fixed_delta_seconds = 1.0 / 60.0;
        let frame = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            (
                scene.renderer.take_completed_physics_events().len(),
                scene.renderer.take_physics_events_lost(),
            )
        };

        // A hitch runs three ticks in one frame: every firing still fits.
        assert_eq!(frame(&mut scene, 3), (3 * BODIES as usize, 0));
        scene.renderer.max_physics_events = 60;
        assert_eq!(frame(&mut scene, 4), (60, u64::from(BODIES) - 60));
        assert_eq!(scene.renderer.take_physics_events_lost(), 0);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn ten_thousand_gpu_bodies_report_condition_events_without_state_readback()
    {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuCondition,
            GpuEventId, GpuEventMode, GpuEventPayload, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        const BODIES: u32 = 10_000;
        // Every body falls; the rule fires once when a body enters y < 0.
        // Odd bodies start just above the line and cross it within the
        // first ticks, even bodies start high and never do.
        let rule = ExtractedGpuPhysicsRule {
            event_id: GpuEventId(9),
            instructions: GpuCondition::position_y()
                .less_than(0.0)
                .compile()
                .unwrap(),
            mode: GpuEventMode::OnEnter,
            payload: GpuEventPayload::Position,
            cooldown_seconds: 0.0,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = (0..BODIES)
            .map(|slot| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new([
                    (slot % 100) as f32 * 2.0,
                    if slot % 2 == 1 { 0.05 } else { 100.0 },
                    (slot / 100) as f32 * 2.0,
                ]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                sync: Default::default(),
                rules: vec![rule.clone()],
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let mut events = Vec::new();
        for tick in 1..=30 {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            events.extend(scene.renderer.take_completed_physics_events());
        }

        // Exactly one event per odd body, carrying its crossing position.
        assert_eq!(events.len(), BODIES as usize / 2);
        let mut slots = events
            .iter()
            .map(|event| event.body_slot)
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), BODIES as usize / 2);
        assert!(slots.iter().all(|slot| slot % 2 == 1));
        assert!(events.iter().all(|event| {
            event.event_id == 9
                && event.payload[1] < 0.0
                && event.payload[0] == (event.body_slot % 100) as f32 * 2.0
        }));
        assert_eq!(
            scene.renderer.capacity_diagnostics().physics_events_dropped,
            0
        );
        // Only event buffers came back: no body state was copied.
        let counters = scene.renderer.render_counters();
        assert_eq!(counters.physics_state_bytes, 0);
        assert!(counters.physics_event_bytes > 0);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn commands_apply_on_their_tick_however_frames_batch_ticks() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuBodyCommand, PhysicsId, PhysicsSyncMode,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let id = PhysicsId {
            slot: 0,
            generation: 1,
        };
        let velocity = |linear| GpuBodyCommand::SetVelocity {
            linear,
            angular: [0.0; 3],
        };
        // Renders one frame per listed physics tick and returns the last
        // state read back and the GPU hash of every tick.
        let run = |frames: &[u64], first_speed: f32| {
            let mut scene = SlabScene::new(&[]);
            let world = &mut scene.render_world;
            world.gpu_physics = vec![ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3100).unwrap(),
                physics_id: id,
                transform: crate::Transform::default(),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                rules: Vec::new(),
                sync: PhysicsSyncMode::FullState,
            }];
            world.gpu_physics_revision = 1;
            world.physics_enabled = true;
            world.physics_gravity = [0.0; 3];
            world.fixed_delta_seconds = 1.0 / 60.0;
            world.gpu_physics_commands = vec![
                (id, velocity([first_speed, 0.0, 0.0])),
                (id, velocity([0.0, 2.0, 0.0])),
            ];
            world.gpu_physics_command_ticks = vec![3, 6];
            let mut hashes = Vec::new();
            world.gpu_physics_commands_serial = 1;
            let mut last = None;
            for &tick in frames {
                scene.render_world.physics_tick = tick;
                let before = scene.now();
                scene
                    .render(before)
                    .then_signal_fence_and_flush()
                    .unwrap()
                    .wait(None)
                    .unwrap();
                scene.renderer.block_until_physics_readbacks_complete();
                last = scene.renderer.take_completed_physics_states().pop();
                hashes.extend(
                    scene.renderer.take_completed_physics_state_hashes(),
                );
            }
            let state = last.unwrap();
            (state.tick, state.transform, state.linear_velocity, hashes)
        };
        let steady = run(&(1..=11).collect::<Vec<_>>(), 1.0);
        // One tick moves 1/60 m at 1 m/s: ticks 3 to 5 in x, 6 to 11 in y.
        assert_eq!(steady.0, 11);
        assert!((steady.1.position[0] - 3.0 / 60.0).abs() < 1e-6);
        assert!((steady.1.position[1] - 12.0 / 60.0).abs() < 1e-6);
        // Ten ticks in one frame: eight run, the last two wait a frame.
        assert_eq!(
            steady.3.iter().map(|(tick, _)| *tick).collect::<Vec<_>>(),
            (1..=11).collect::<Vec<_>>()
        );
        assert_eq!(run(&[1, 11, 11], 1.0), steady);
        assert_eq!(run(&[1, 4, 7, 11], 1.0), steady);
        // One ULP of the command applied before tick 3 changes the hash of
        // tick 3 onward and nothing before it.
        let nudged = run(&[1, 4, 7, 11], f32::from_bits(1.0f32.to_bits() + 1));
        for ((tick, before), (_, after)) in steady.3.iter().zip(&nudged.3) {
            assert_eq!(before == after, *tick < 3, "tick {tick}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn commands_apply_once_before_the_step_and_reject_stale_bodies() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuBodyCommand, PhysicsId, PhysicsSyncMode,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let id = |slot| PhysicsId {
            slot,
            generation: 1,
        };
        let body = |slot| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                .unwrap(),
            physics_id: id(slot),
            transform: crate::Transform::new([slot as f32 * 4.0, 0.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            custom_shader: None,
            rules: Vec::new(),
            sync: PhysicsSyncMode::FullState,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![body(0), body(1), body(2)];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        world.gpu_physics_commands = vec![
            (id(0), GpuBodyCommand::Impulse([2.0, 0.0, 0.0])),
            (
                id(1),
                GpuBodyCommand::Teleport(crate::Transform::new([
                    4.0, 50.0, 0.0,
                ])),
            ),
            // One tick of 60 N on 1 kg adds 1 m/s.
            (id(0), GpuBodyCommand::Force([60.0, 0.0, 0.0])),
            (id(1), GpuBodyCommand::SetCustomValues([1.0, 2.0, 3.0, 4.0])),
            (
                PhysicsId {
                    slot: 2,
                    generation: 0,
                },
                GpuBodyCommand::Teleport(crate::Transform::default()),
            ),
            (id(77), GpuBodyCommand::Impulse([1.0; 3])),
        ];
        world.gpu_physics_commands_serial = 1;

        let frame = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.renderer.take_completed_physics_states()
        };
        let states = frame(&mut scene, 1);
        assert_eq!(scene.renderer.render_counters().physics_commands, 4);
        assert_eq!(
            scene
                .renderer
                .capacity_diagnostics()
                .physics_commands_rejected,
            2
        );
        assert_eq!(states[0].linear_velocity, [3.0, 0.0, 0.0]);
        assert!((states[0].transform.position[0] - 3.0 / 60.0).abs() < 1e-6);
        assert_eq!(states[1].transform.position, [4.0, 50.0, 0.0]);
        assert_eq!(states[1].custom_values, Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(states[2].transform.position, [8.0, 0.0, 0.0]);

        // The same batch is not applied again on later frames.
        let states = frame(&mut scene, 2);
        assert_eq!(states[0].linear_velocity, [3.0, 0.0, 0.0]);
        assert_eq!(scene.renderer.render_counters().physics_commands, 0);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn commands_alter_only_selected_bodies_mid_simulation() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuBodyCommand, PhysicsId, PhysicsSyncMode,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let id = |slot| PhysicsId {
            slot,
            generation: 0,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = (0..4)
            .map(|slot| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: id(slot),
                transform: crate::Transform::new([
                    slot as f32 * 4.0,
                    100.0,
                    0.0,
                ]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                rules: Vec::new(),
                sync: PhysicsSyncMode::FullState,
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;

        let frame = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.renderer.take_completed_physics_states()
        };
        let mut states = Vec::new();
        for tick in 1..=10 {
            states = frame(&mut scene, tick);
        }
        let falling = states[0].linear_velocity;
        assert!(falling[1] < -1.0, "bodies fall before any command");
        assert!(states.iter().all(|s| s.linear_velocity == falling));

        // Mid-run commands for bodies 1 and 3 only.
        let world = &mut scene.render_world;
        world.gpu_physics_commands = vec![
            (id(1), GpuBodyCommand::Impulse([5.0, 0.0, 0.0])),
            (
                id(3),
                GpuBodyCommand::SetVelocity {
                    linear: [0.0, 20.0, 0.0],
                    angular: [0.0; 3],
                },
            ),
        ];
        world.gpu_physics_commands_serial = 1;
        let before = states.clone();
        let states = frame(&mut scene, 11);
        assert_eq!(scene.renderer.render_counters().physics_commands, 2);
        let gravity_step = -9.81 / 60.0;
        for slot in [0, 2] {
            let v = states[slot].linear_velocity;
            assert_eq!(v[0], 0.0);
            assert!((v[1] - (falling[1] + gravity_step)).abs() < 1e-4);
            assert!(
                states[slot].transform.position[1]
                    < before[slot].transform.position[1]
            );
        }
        assert!((states[1].linear_velocity[0] - 5.0).abs() < 1e-5);
        assert!(
            (states[1].linear_velocity[1] - (falling[1] + gravity_step)).abs()
                < 1e-4
        );
        assert!(
            (states[3].linear_velocity[1] - (20.0 + gravity_step)).abs() < 1e-4
        );
        assert!(
            states[3].transform.position[1] > before[3].transform.position[1]
        );

        // The simulation keeps running from the altered state.
        let states = frame(&mut scene, 12);
        assert!(states[1].transform.position[0] > 4.0);
        assert!(states[3].linear_velocity[1] > 19.0);
        assert!(states[0].linear_velocity[1] < falling[1]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn one_shot_state_reads_return_requested_bodies_once() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuBodyCommand, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let id = |slot| PhysicsId {
            slot,
            generation: 0,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = (0..3)
            .map(|slot| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: id(slot),
                transform: crate::Transform::new([slot as f32 * 4.0, 0.0, 0.0]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                rules: Vec::new(),
                // Events only: nothing is read back unless requested.
                sync: Default::default(),
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.fixed_delta_seconds = 1.0 / 60.0;

        let frame = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
            let states = scene.renderer.take_completed_physics_states();
            states
                .iter()
                .map(|state| {
                    (state.physics_id.slot, state.custom_values.is_some())
                })
                .collect::<Vec<_>>()
        };
        scene.render_world.gpu_physics_commands =
            vec![(id(1), GpuBodyCommand::ReadState)];
        scene.render_world.gpu_physics_commands_serial = 1;
        assert_eq!(frame(&mut scene, 1), [(1, true)]);

        scene.render_world.gpu_physics_commands.clear();
        scene.render_world.gpu_physics_read_all = true;
        scene.render_world.gpu_physics_commands_serial = 2;
        assert_eq!(frame(&mut scene, 2), [(0, true), (1, true), (2, true)]);

        assert_eq!(frame(&mut scene, 3), []);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn reset_restarts_from_authored_state_and_restore_resumes_a_snapshot() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuPhysicsCommands, GpuStateMirror,
            PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let id = PhysicsId {
            slot: 0,
            generation: 0,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000).unwrap(),
            physics_id: id,
            transform: crate::Transform::new([0.0, 0.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            custom_shader: None,
            rules: Vec::new(),
            sync: Default::default(),
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -60.0, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;

        // Reads every body each frame and returns the one body's state.
        let frame = |scene: &mut SlabScene, tick, batch: GpuPhysicsCommands| {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_commands = batch.commands;
            world.gpu_physics_reset = batch.reset_to_authored;
            world.gpu_physics_read_all = true;
            world.gpu_physics_commands_serial += 1;
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
            let states = scene.renderer.take_completed_physics_states();
            assert_eq!(states.len(), 1);
            states[0]
        };
        // Gravity adds -1 m/s per tick.
        for tick in 1..=2 {
            frame(&mut scene, tick, GpuPhysicsCommands::default());
        }
        let snapshot = frame(&mut scene, 3, GpuPhysicsCommands::default());
        assert!((snapshot.linear_velocity[1] + 3.0).abs() < 1e-4);

        let stop = GpuPhysicsCommands {
            reset_to_authored: true,
            ..Default::default()
        };
        let restarted = frame(&mut scene, 4, stop.clone());
        assert!((restarted.linear_velocity[1] + 1.0).abs() < 1e-4);

        let mut resume = stop;
        resume.restore(
            id,
            &GpuStateMirror {
                tick: snapshot.tick,
                transform: snapshot.transform,
                linear_velocity: snapshot.linear_velocity,
                angular_velocity: snapshot.angular_velocity,
                custom_values: snapshot.custom_values,
            },
        );
        let resumed = frame(&mut scene, 5, resume);
        assert!((resumed.linear_velocity[1] + 4.0).abs() < 1e-4);
        let expected_y = snapshot.transform.position[1] - 4.0 / 60.0;
        assert!(
            (resumed.transform.position[1] - expected_y).abs() < 1e-4,
            "{resumed:?}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_bodies_rest_on_cpu_colliders_unless_filtered_or_no_collision() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            ExtractedGpuPhysicsRule, GpuCollider, GpuCondition, GpuEventId,
            GpuEventMode, GpuEventPayload, PhysicsId, PhysicsSolver,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let unit_box = Collider {
            shape: ColliderShape::Box {
                half_extents: [0.5; 3],
            },
            ..Collider::default()
        };
        let body =
            |slot: u32, x: f32, solver, layers| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new([x, 1.0, 0.0]),
                // A spin must not keep the box awake.
                rigid_body: crate::runtime::RigidBody {
                    angular_velocity: [0.0, 6.0, 0.0],
                    ..Default::default()
                },
                solver,
                collider: Some((unit_box, layers)),
                custom_shader: None,
                rules: vec![ExtractedGpuPhysicsRule {
                    event_id: GpuEventId(11),
                    instructions: GpuCondition::colliding().compile().unwrap(),
                    mode: GpuEventMode::OnEnter,
                    payload: GpuEventPayload::None,
                    cooldown_seconds: 0.0,
                }],
                sync: Default::default(),
            };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        let ghost_layers = CollisionLayers {
            memberships: 0b10,
            filters: u32::MAX,
        };
        world.gpu_physics = vec![
            body(0, -3.0, PhysicsSolver::Full, CollisionLayers::default()),
            body(
                1,
                0.0,
                PhysicsSolver::NoCollision,
                CollisionLayers::default(),
            ),
            body(2, 3.0, PhysicsSolver::Full, ghost_layers),
        ];
        // Distant colliders precede the ground in upload order, so the
        // indexed path must skip them and still find the final collider.
        world.gpu_colliders = (0..32)
            .map(|index| GpuCollider {
                model: crate::Transform::new([
                    100.0 + index as f32 * 10.0,
                    -0.5,
                    0.0,
                ])
                .to_matrix(),
                shape: [0.0, 0.5, 0.5, 0.5],
                velocity: [0.0; 3],
                friction: 0.5,
                restitution: 0.0,
                layers: CollisionLayers::default(),
            })
            .collect();
        // Static ground on layer 1 only, top face at y = 0.
        world.gpu_colliders.push(GpuCollider {
            model: crate::Transform::new([0.0, -0.5, 0.0]).to_matrix(),
            shape: [0.0, 10.0, 0.5, 10.0],
            velocity: [0.0; 3],
            friction: 0.5,
            restitution: 0.0,
            layers: CollisionLayers {
                memberships: 0b01,
                filters: 0b01,
            },
        });
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let mut events = Vec::new();
        for tick in 1..=150 {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == 150;
            world.gpu_physics_commands_serial += u64::from(tick == 150);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
            events.extend(scene.renderer.take_completed_physics_events());
        }
        let mut states = scene.renderer.take_completed_physics_states();
        states.sort_by_key(|state| state.physics_id.slot);
        assert_eq!(states.len(), 3);
        let resting = states[0];
        assert!(
            (resting.transform.position[1] - 0.5).abs() < 0.02,
            "{resting:?}"
        );
        assert!(resting.linear_velocity[1].abs() < 0.2, "{resting:?}");
        assert_eq!(resting.angular_velocity, [0.0; 3], "spun box sleeps");
        assert!(states[1].transform.position[1] < -5.0, "no collision falls");
        assert!(states[2].transform.position[1] < -5.0, "filtered out falls");
        // Only the resting box ever touched, and OnEnter fires once.
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!((events[0].body_slot, events[0].event_id), (0, 11));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn force_fields_push_gpu_bodies_inside_them() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            FieldKind, ForceField, GpuForceField, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let ball = |slot: u32, x: f32| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(5000 + slot)
                .unwrap(),
            physics_id: PhysicsId {
                slot,
                generation: 0,
            },
            transform: crate::Transform::new([x, 0.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: Some((
                Collider {
                    shape: ColliderShape::Sphere { radius: 0.5 },
                    ..Collider::default()
                },
                CollisionLayers::default(),
            )),
            custom_shader: None,
            rules: Vec::new(),
            sync: Default::default(),
        };
        let field = |x: f32, field| GpuForceField {
            model: crate::Transform::new([x, 0.0, 0.0]).to_matrix(),
            shape: [0.0, 5.0, 50.0, 5.0],
            field,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        // In an up field, beside it, and in gusty sideways wind.
        world.gpu_physics = vec![ball(0, 0.0), ball(1, 20.0), ball(2, 40.0)];
        world.gpu_force_fields = vec![
            field(
                0.0,
                ForceField {
                    strength: 20.0,
                    ..ForceField::default()
                },
            ),
            field(
                40.0,
                ForceField {
                    kind: FieldKind::Wind,
                    strength: 3.0,
                    direction: [1.0, 0.0, 0.0],
                    turbulence: 1.0,
                    ..ForceField::default()
                },
            ),
        ];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        for tick in 1..=60 {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == 60;
            world.gpu_physics_commands_serial += u64::from(tick == 60);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
        }
        let mut states = scene.renderer.take_completed_physics_states();
        states.sort_by_key(|state| state.physics_id.slot);
        assert_eq!(states.len(), 3);
        // 20 up - 9.81 down for one second.
        let lifted = states[0].linear_velocity[1];
        assert!((lifted - 10.19).abs() < 0.2, "{:?}", states[0]);
        assert!(states[1].linear_velocity[1] < -9.5, "{:?}", states[1]);
        // Gusts average near 3 m/s² but never sit exactly on it.
        let blown = states[2].linear_velocity[0];
        assert!(blown > 1.0 && (blown - 3.0).abs() > 0.01, "{:?}", states[2]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn simulation_lod_slows_distant_gpu_bodies_and_freezes_far_ones() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuSimulationLod, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let ball = |slot: u32, x: f32| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(7000 + slot)
                .unwrap(),
            physics_id: PhysicsId {
                slot,
                generation: 0,
            },
            transform: crate::Transform::new([x, 0.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            custom_shader: None,
            rules: Vec::new(),
            sync: Default::default(),
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        // Full rate, every fourth tick, and frozen.
        world.gpu_physics = vec![ball(0, 0.0), ball(1, 20.0), ball(2, 50.0)];
        world.gpu_lod = Some(GpuSimulationLod {
            full_radius: 10.0,
            reduced_radius: 30.0,
            reduced_stride: 4,
        });
        world.gpu_observers = vec![[0.0; 3]];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let mut states = Vec::new();
        for tick in 1..=61 {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick >= 60;
            world.gpu_physics_commands_serial += u64::from(tick >= 60);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
            if tick >= 60 {
                states.push(scene.renderer.take_completed_physics_states());
            }
        }
        for states in &mut states {
            states.sort_by_key(|state| state.physics_id.slot);
            assert_eq!(states.len(), 3);
        }
        let fall =
            |tick: usize, slot: usize| states[tick][slot].linear_velocity[1];
        // 60 ticks: the near ball took 60 steps, the mid one 15 of four
        // ticks each (slot 1 steps when tick + 1 is a multiple of 4), so
        // both fell for one second.
        assert!((fall(0, 0) + 9.81).abs() < 1e-3, "{:?}", states[0]);
        assert!((fall(0, 1) + 9.81).abs() < 1e-3, "{:?}", states[0]);
        // Tick 61: the near ball steps, the mid one waits.
        assert!(fall(1, 0) < fall(0, 0));
        assert_eq!(fall(1, 1), fall(0, 1));
        // The far ball never moved.
        for states in &states {
            assert_eq!(states[2].linear_velocity, [0.0; 3]);
            assert_eq!(states[2].transform.position, [50.0, 0.0, 0.0]);
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_axis_locks_keep_2d_bodies_in_their_plane() {
        use crate::runtime::{
            AxisLock, Collider, ColliderShape, CollisionLayers,
            ExtractedGpuPhysicsBody, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let ball = |slot: u32, position: [f32; 3]| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(6000 + slot)
                .unwrap(),
            physics_id: PhysicsId {
                slot,
                generation: 0,
            },
            transform: crate::Transform::new(position),
            rigid_body: crate::runtime::RigidBody {
                angular_velocity: [1.0, 2.0, 3.0],
                ..Default::default()
            },
            solver: Default::default(),
            collider: Some((
                Collider {
                    shape: ColliderShape::Sphere { radius: 0.5 },
                    ..Collider::default()
                },
                CollisionLayers::default(),
            )),
            custom_shader: None,
            rules: Vec::new(),
            sync: Default::default(),
        };
        let run = |locks: AxisLock| {
            let mut scene = SlabScene::new(&[]);
            let world = &mut scene.render_world;
            // Two overlapping balls offset in z, under gravity with a
            // sideways z part: free, both push and fall out of the plane.
            world.gpu_physics =
                vec![ball(0, [0.0, 0.0, 0.0]), ball(1, [0.6, 0.0, 0.3])];
            world.gpu_physics_revision = 1;
            world.physics_enabled = true;
            world.physics_gravity = [0.0, -9.81, 2.0];
            world.physics_lock_mask = locks.mask();
            world.fixed_delta_seconds = 1.0 / 60.0;
            for tick in 1..=30 {
                let world = &mut scene.render_world;
                world.physics_tick = tick;
                world.gpu_physics_read_all = tick == 30;
                world.gpu_physics_commands_serial += u64::from(tick == 30);
                let before = scene.now();
                let _in_flight =
                    scene.render(before).then_signal_fence_and_flush().unwrap();
                scene.renderer.block_until_physics_readbacks_complete();
            }
            let mut states = scene.renderer.take_completed_physics_states();
            states.sort_by_key(|state| state.physics_id.slot);
            assert_eq!(states.len(), 2);
            states
        };
        let free = run(AxisLock::default());
        assert!(free[0].transform.position[2] > 0.1, "{:?}", free[0]);
        let flat = run(AxisLock::PLANE_XY);
        for (state, start) in flat.iter().zip([0.0, 0.3]) {
            assert_eq!(state.transform.position[2], start, "{state:?}");
            assert_eq!(state.linear_velocity[2], 0.0, "{state:?}");
            assert_eq!(state.angular_velocity[..2], [0.0, 0.0], "{state:?}");
            assert!(state.transform.position[1] < -1.0, "{state:?}");
        }
        // The balls still push apart in x inside the plane.
        let gap = flat[1].transform.position[0] - flat[0].transform.position[0];
        assert!(gap > 0.7, "{flat:?}");
    }

    #[test]
    fn contact_grid_hash_stays_inside_its_memory_budget() {
        // Plenty of memory: two cells per body.
        assert_eq!(PhysicsContactGrid::hash_cells(1024, 1 << 30), 2048);
        // 68 bytes per cell: 64 KiB affords 963 cells, rounded down to 512.
        assert_eq!(PhysicsContactGrid::hash_cells(1024, 64 << 10), 512);
        // Even no budget keeps one cell; the rest go to the fallback list.
        assert_eq!(PhysicsContactGrid::hash_cells(1024, 0), 1);
    }

    #[test]
    fn contact_grid_cells_ignore_outlier_bodies() {
        assert_eq!(contact_grid_cell_size(std::iter::empty()), 1.0);
        let radii = [0.1, 0.1, 0.1, 0.4, 3.0];
        assert_eq!(contact_grid_cell_size(radii.into_iter()), 0.8);
        assert_eq!(shape_bounding_radius([2.0, 1.0, 0.5, 0.0]), Some(1.5));
        assert_eq!(shape_bounding_radius([3.0, 0.0, 0.0, 0.0]), None);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_bodies_collide_with_each_other_through_grid_and_fallback() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            GpuCollider, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let sphere = |slot: u32, position: [f32; 3], radius: f32| {
            ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(4000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new(position),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: Some((
                    Collider {
                        shape: ColliderShape::Sphere { radius },
                        ..Collider::default()
                    },
                    CollisionLayers::default(),
                )),
                custom_shader: None,
                rules: Vec::new(),
                sync: Default::default(),
            }
        };
        let mut bodies = vec![
            // A two-sphere stack.
            sphere(0, [0.0, 0.4, 0.0], 0.4),
            sphere(1, [0.0, 1.25, 0.0], 0.4),
            // A body too big for one cell with a small one on top.
            sphere(2, [20.0, 3.0, 0.0], 3.0),
            sphere(3, [20.0, 6.45, 0.0], 0.4),
        ];
        // Twenty overlapping pebbles in one 0.8 m cell, which holds sixteen.
        for index in 0..20 {
            let (column, row) = (index % 5, index / 5);
            bodies.push(sphere(
                4 + index,
                [-9.5 + 0.15 * column as f32, 0.1, 0.1 + 0.15 * row as f32],
                0.1,
            ));
        }
        let run = |contact_hash_budget| {
            let mut scene = SlabScene::new(&[]);
            scene.renderer.contact_hash_budget = contact_hash_budget;
            let world = &mut scene.render_world;
            world.gpu_physics = bodies.clone();
            // Static ground with its top face at y = 0.
            world.gpu_colliders = vec![GpuCollider {
                model: crate::Transform::new([0.0, -0.5, 0.0]).to_matrix(),
                shape: [0.0, 50.0, 0.5, 50.0],
                velocity: [0.0; 3],
                friction: 0.5,
                restitution: 0.0,
                layers: CollisionLayers::default(),
            }];
            world.gpu_physics_revision = 1;
            world.physics_enabled = true;
            world.physics_gravity = [0.0, -9.81, 0.0];
            world.fixed_delta_seconds = 1.0 / 60.0;
            for tick in 1..=120 {
                let world = &mut scene.render_world;
                world.physics_tick = tick;
                world.gpu_physics_read_all = tick == 120;
                world.gpu_physics_commands_serial += u64::from(tick == 120);
                let before = scene.now();
                let _in_flight =
                    scene.render(before).then_signal_fence_and_flush().unwrap();
                scene.renderer.block_until_physics_readbacks_complete();
            }
            let mut states = scene.renderer.take_completed_physics_states();
            states.sort_by_key(|state| state.physics_id.slot);
            (
                states,
                scene.renderer.capacity_diagnostics(),
                scene.renderer.render_counters(),
            )
        };
        let (states, diagnostics, counters) = run(DeviceSize::MAX);
        // CONTACT_ROUNDS grid, contact and oversized-body passes, the
        // built-in step, and the state hash; frame 119's readback (an empty
        // event header) landed in frame 120's counters.
        assert_eq!(counters.physics_dispatches, 3 * CONTACT_ROUNDS + 2);
        assert_eq!(counters.physics_event_bytes, 32);
        // The test blocks on each frame, so nothing waits a frame.
        assert_eq!(counters.physics_readback_latency_frames, 0);
        assert_eq!(states.len(), 24);
        let height = |slot: usize| states[slot].transform.position[1];
        for (slot, expected) in [(0, 0.4), (1, 1.2), (2, 3.0), (3, 6.4)] {
            assert!(
                (height(slot) - expected).abs() < 0.05,
                "body {slot} rests at {}",
                height(slot)
            );
        }
        // The pebbles pushed each other apart, including the ones that did
        // not fit their cell.
        for a in 4..24 {
            assert!(
                (height(a) - 0.1).abs() < 0.03,
                "pebble {a} at {}",
                height(a)
            );
            for b in a + 1..24 {
                let [ax, _, az] = states[a].transform.position;
                let [bx, _, bz] = states[b].transform.position;
                let gap = ((ax - bx).powi(2) + (az - bz).powi(2)).sqrt();
                assert!(gap > 0.18, "pebbles {a} and {b} are {gap} apart");
            }
        }
        assert!(diagnostics.physics_grid_overflow > 0, "{diagnostics:?}");
        assert_eq!(diagnostics.physics_oversized_bodies, CONTACT_ROUNDS);
        assert!(diagnostics.physics_fallback_tests > 0, "{diagnostics:?}");
        // Lists fill in atomic order, yet a second run matches bit for bit.
        let (again, _, _) = run(DeviceSize::MAX);
        let poses = |states: &[crate::runtime::GpuStateSample]| {
            states
                .iter()
                .map(|state| (state.transform, state.linear_velocity))
                .collect::<Vec<_>>()
        };
        assert_eq!(poses(&states), poses(&again));
        // With no memory for the hash, one cell is left and every other body
        // spills into the fallback list: slower, but the same contacts.
        let (starved, diagnostics, _) = run(0);
        assert_eq!(poses(&states), poses(&starved));
        // Fifteen bodies fit a cell and the one cell holds eight.
        assert_eq!(
            diagnostics.physics_grid_overflow,
            7 * CONTACT_ROUNDS,
            "{diagnostics:?}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn benchmark_scenes_run_on_the_gpu_and_repeat_exactly() {
        use crate::runtime::{
            Collider, CollisionLayers, ExtractedGpuPhysicsBody, GpuCollider,
            PhysicsBenchmark, PhysicsId, PhysicsSolver,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let run = |scene: PhysicsBenchmark| {
            let mut slab = SlabScene::new(&[]);
            let world = &mut slab.render_world;
            world.gpu_physics = scene
                .bodies(1_000)
                .into_iter()
                .zip(0..)
                .map(|(body, slot)| ExtractedGpuPhysicsBody {
                    entity: bevy_ecs::entity::Entity::from_raw_u32(5000 + slot)
                        .unwrap(),
                    physics_id: PhysicsId {
                        slot,
                        generation: 0,
                    },
                    transform: body.transform,
                    rigid_body: crate::runtime::RigidBody {
                        linear_velocity: body.linear_velocity,
                        ..Default::default()
                    },
                    solver: body.solver,
                    collider: Some((
                        Collider::default(),
                        CollisionLayers::default(),
                    )),
                    custom_shader: None,
                    rules: Vec::new(),
                    sync: Default::default(),
                })
                .collect();
            world.gpu_colliders = vec![GpuCollider {
                model: crate::Transform::new([0.0, -0.5, 0.0]).to_matrix(),
                shape: [0.0, 500.0, 0.5, 500.0],
                velocity: [0.0; 3],
                friction: 0.5,
                restitution: 0.0,
                layers: CollisionLayers::default(),
            }];
            world.gpu_physics_revision = 1;
            world.physics_enabled = true;
            world.physics_gravity = [0.0, -9.81, 0.0];
            world.fixed_delta_seconds = 1.0 / 60.0;
            for tick in 1..=90 {
                let world = &mut slab.render_world;
                world.physics_tick = tick;
                world.gpu_physics_read_all = tick == 90;
                world.gpu_physics_commands_serial += u64::from(tick == 90);
                let before = slab.now();
                let _in_flight =
                    slab.render(before).then_signal_fence_and_flush().unwrap();
                slab.renderer.block_until_physics_readbacks_complete();
            }
            let mut states = slab.renderer.take_completed_physics_states();
            states.sort_by_key(|state| state.physics_id.slot);
            states
        };
        for scene in PhysicsBenchmark::ALL {
            let states = run(scene);
            let bodies = scene.bodies(1_000);
            assert_eq!(states.len(), 1_000, "{scene:?}");
            for (state, body) in states.iter().zip(&bodies) {
                let [x, y, z] = state.transform.position;
                assert!(
                    x.is_finite() && y.is_finite() && z.is_finite(),
                    "{scene:?} body {:?} at {:?}",
                    state.physics_id,
                    state.transform.position
                );
                // Colliding bodies stay on the ground; NoCollision ones fall
                // through it.
                if body.solver != PhysicsSolver::NoCollision {
                    assert!(
                        y > 0.3,
                        "{scene:?} body {:?} sank to {y}",
                        state.physics_id
                    );
                }
            }
            if scene == PhysicsBenchmark::Stacking {
                // Every tower still stands ten cubes high.
                for top in states.iter().skip(9).step_by(10) {
                    let y = top.transform.position[1];
                    assert!((y - 9.5).abs() < 0.25, "tower top at {y}");
                }
            }
        }
        let poses = |states: &[crate::runtime::GpuStateSample]| {
            states
                .iter()
                .map(|state| (state.transform, state.linear_velocity))
                .collect::<Vec<_>>()
        };
        for scene in [PhysicsBenchmark::Debris, PhysicsBenchmark::Mixed] {
            assert_eq!(poses(&run(scene)), poses(&run(scene)), "{scene:?}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn kinematic_cpu_colliders_push_gpu_bodies() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            GpuCollider, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000).unwrap(),
            physics_id: PhysicsId::default(),
            transform: crate::Transform::new([0.0; 3]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: Some((
                Collider {
                    shape: ColliderShape::Sphere { radius: 0.5 },
                    friction: 0.0,
                    ..Collider::default()
                },
                CollisionLayers::default(),
            )),
            custom_shader: None,
            rules: Vec::new(),
            sync: Default::default(),
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        const SPEED: f32 = 3.0;
        for tick in 1..=60_u64 {
            let world = &mut scene.render_world;
            // A kinematic wall sweeping +X, as the CPU step would report it.
            let x = -1.2 + SPEED * tick as f32 / 60.0;
            world.gpu_colliders = vec![GpuCollider {
                model: crate::Transform::new([x, 0.0, 0.0]).to_matrix(),
                shape: [0.0, 0.5, 2.0, 2.0],
                velocity: [SPEED, 0.0, 0.0],
                friction: 0.0,
                restitution: 0.0,
                layers: CollisionLayers::default(),
            }];
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == 60;
            world.gpu_physics_commands_serial += u64::from(tick == 60);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
        }
        let states = scene.renderer.take_completed_physics_states();
        let wall_face = -1.2 + SPEED + 0.5;
        let x = states[0].transform.position[0];
        assert!((x - (wall_face + 0.5)).abs() < 0.1, "{:?}", states[0]);
        assert!((states[0].linear_velocity[0] - SPEED).abs() < 0.1);
    }

    /// 3600 balls (r = 0.09) nine layers deep in a 4 x 4 m pit of 0.5 m by 4 m
    /// static box walls, run for `ticks` with sideways gravity `push`.
    fn run_ball_pit(
        push: f32,
        ticks: u64,
    ) -> Vec<crate::runtime::GpuStateSample> {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            GpuCollider, PhysicsId,
        };
        let mut bodies = Vec::new();
        for layer in 0..9 {
            for row in 0..20 {
                for column in 0..20 {
                    let slot = bodies.len() as u32;
                    // A small fixed jitter so columns do not stack exactly.
                    let jitter = (slot * 37 % 7) as f32 * 0.003 - 0.009;
                    let at = |index: i32| -1.9 + 0.2 * index as f32 + jitter;
                    bodies.push(ExtractedGpuPhysicsBody {
                        entity: bevy_ecs::entity::Entity::from_raw_u32(
                            10_000 + slot,
                        )
                        .unwrap(),
                        physics_id: PhysicsId {
                            slot,
                            generation: 0,
                        },
                        transform: crate::Transform::new([
                            at(column),
                            0.1 + 0.2 * layer as f32,
                            at(row),
                        ]),
                        rigid_body: Default::default(),
                        solver: Default::default(),
                        collider: Some((
                            Collider {
                                shape: ColliderShape::Sphere { radius: 0.09 },
                                ..Collider::default()
                            },
                            CollisionLayers::default(),
                        )),
                        custom_shader: None,
                        rules: Vec::new(),
                        sync: Default::default(),
                    });
                }
            }
        }
        let wall = |center: [f32; 3], half: [f32; 3]| GpuCollider {
            model: crate::Transform::new(center).to_matrix(),
            shape: [0.0, half[0], half[1], half[2]],
            velocity: [0.0; 3],
            friction: 0.5,
            restitution: 0.0,
            layers: CollisionLayers::default(),
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = bodies;
        world.gpu_colliders = vec![
            wall([0.0, -0.5, 0.0], [10.0, 0.5, 10.0]),
            wall([2.25, 2.0, 0.0], [0.25, 2.0, 2.5]),
            wall([-2.25, 2.0, 0.0], [0.25, 2.0, 2.5]),
            wall([0.0, 2.0, 2.25], [2.0, 2.0, 0.25]),
            wall([0.0, 2.0, -2.25], [2.0, 2.0, 0.25]),
        ];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [push, -9.81, push * 0.5];
        world.fixed_delta_seconds = 1.0 / 60.0;
        for tick in 1..=ticks {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == ticks;
            world.gpu_physics_commands_serial += u64::from(tick == ticks);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
        }
        scene.renderer.take_completed_physics_states()
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_settled_gpu_pile_stays_asleep() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let measure = |ticks| {
            let states = run_ball_pit(0.0, ticks);
            let asleep = states
                .iter()
                .filter(|state| state.linear_velocity == [0.0; 3])
                .count();
            let top = states
                .iter()
                .map(|state| state.transform.position[1])
                .fold(0.0_f32, f32::max);
            (asleep, top)
        };
        // Before sleeping balls ignored tiny pushes, the pile crept and
        // woke: 3032 asleep at tick 200, 2181 at 300, the top rising.
        let (early, early_top) = measure(240);
        let (late, late_top) = measure(400);
        assert!(early > 3500, "only {early} of 3600 asleep at tick 240");
        assert!(late >= early, "{early} asleep at tick 240, {late} at 400");
        assert!(
            (late_top - early_top).abs() < 0.002,
            "the pile top moved from {early_top} to {late_top}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_deep_gpu_ball_pit_settles_and_its_walls_hold() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let states = run_ball_pit(0.0, 180);
        assert_eq!(states.len(), 3600);
        let resting = states
            .iter()
            .filter(|state| {
                let [x, y, z] = state.linear_velocity;
                (x * x + z * z).sqrt() < 0.1 && y.abs() < 0.1
            })
            .count();
        // Sideways gravity presses the pile into the +x/+z corner.
        let pressed = run_ball_pit(8.0, 180);
        let escaped = pressed
            .iter()
            .filter(|state| {
                let [x, y, z] = state.transform.position;
                x.abs() > 2.0 || z.abs() > 2.0 || y < 0.0
            })
            .count();
        // The old single-round solver left 646 at rest and let 17 through
        // the corners; this one rests 3045 on an RTX 3060.
        assert!(resting > 2800, "only {resting} of 3600 balls rest");
        assert_eq!(escaped, 0, "{escaped} balls left the pit");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_collider_tree_rechecks_later_branches_after_a_push() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            GpuCollider, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000).unwrap(),
            physics_id: PhysicsId::default(),
            transform: crate::Transform::new([0.4, 0.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: Some((
                Collider {
                    shape: ColliderShape::Sphere { radius: 0.5 },
                    ..Collider::default()
                },
                CollisionLayers::default(),
            )),
            custom_shader: None,
            rules: Vec::new(),
            sync: Default::default(),
        }];
        world.gpu_colliders = [(0.0, [1.0, 0.6, 0.6]), (2.0, [0.5, 0.6, 0.6])]
            .map(|(x, extents)| GpuCollider {
                model: crate::Transform::new([x, 0.0, 0.0]).to_matrix(),
                shape: [0.0, extents[0], extents[1], extents[2]],
                velocity: [0.0; 3],
                friction: 0.0,
                restitution: 0.0,
                layers: CollisionLayers::default(),
            })
            .to_vec();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        world.physics_tick = 1;
        world.gpu_physics_read_all = true;
        world.gpu_physics_commands_serial = 1;

        let before = scene.now();
        let _in_flight =
            scene.render(before).then_signal_fence_and_flush().unwrap();
        scene.renderer.block_until_physics_readbacks_complete();
        let states = scene.renderer.take_completed_physics_states();
        assert_eq!(states.len(), 1);
        assert!(
            (states[0].transform.position[0] - 1.0).abs() < 1e-4,
            "{states:?}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn conditions_use_any_field_and_see_custom_value_commands() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuBodyCommand,
            GpuCondition, GpuEventId, GpuEventMode, GpuEventPayload,
            GpuStateField, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let id = |slot| PhysicsId {
            slot,
            generation: 0,
        };
        let rule = ExtractedGpuPhysicsRule {
            event_id: GpuEventId(9),
            instructions: GpuCondition::field(GpuStateField::PositionX)
                .greater_than(10.0)
                .or(GpuCondition::custom(2).greater_or_equal(3.0))
                .compile()
                .unwrap(),
            mode: GpuEventMode::OnEnter,
            payload: GpuEventPayload::None,
            cooldown_seconds: 0.0,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = [20.0, 0.0]
            .into_iter()
            .zip(0..)
            .map(|(x, slot)| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: id(slot),
                transform: crate::Transform::new([x, 0.0, 0.0]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                rules: vec![rule.clone()],
                sync: Default::default(),
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let frame = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene
                .renderer
                .take_completed_physics_events()
                .iter()
                .map(|event| (event.body_slot, event.tick_low))
                .collect::<Vec<_>>()
        };

        assert_eq!(frame(&mut scene, 1), [(0, 1)]);
        scene.render_world.gpu_physics_commands = vec![(
            id(1),
            GpuBodyCommand::SetCustomValues([0.0, 0.0, 3.0, 0.0]),
        )];
        scene.render_world.gpu_physics_commands_serial = 1;
        assert_eq!(frame(&mut scene, 2), [(1, 2)]);
        assert_eq!(frame(&mut scene, 3), []);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn custom_condition_shaders_emit_events_and_skip_broken_sources() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, GpuConditionShader, GpuEventRegistry,
            PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut registry = GpuEventRegistry::default();
        let counting = GpuConditionShader {
            events: vec!["fell".into()],
            glsl: "void condition(inout PhysicsState body) {\n\
                   // 2 * 0.5 from params, so 1 per tick.\n\
                   body.custom_values.y += 2.0 * condition_params.values[0].y;\n\
                   if (body.model[3].y < -5.0)\n\
                   emit_event(body, EVENTS[0], 7u, body.custom_values);\n\
                   }"
            .into(),
            ..Default::default()
        }
        .resolve(&mut registry);
        let broken = GpuConditionShader {
            events: Vec::new(),
            glsl: "void condition(inout PhysicsState body) { nope }".into(),
            ..Default::default()
        }
        .resolve(&mut registry);
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = [0.0, -10.0]
            .into_iter()
            .zip(0..)
            .map(|(y, slot)| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new([0.0, y, 0.0]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                rules: Vec::new(),
                sync: Default::default(),
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.gpu_condition_shaders = vec![counting.into(), broken.into()];
        world.gpu_condition_shaders[0].params = vec![[0.0, 0.5, 0.0, 0.0]];
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let frame = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene
                .renderer
                .take_completed_physics_events()
                .iter()
                .map(|event| {
                    (
                        event.body_slot,
                        event.event_id,
                        event.tick_low,
                        event.payload_kind,
                        event.payload[1],
                    )
                })
                .collect::<Vec<_>>()
        };

        // The shader's writes persist: the counter grows once per tick.
        assert_eq!(frame(&mut scene, 1), [(1, 1, 1, 7, 1.0)]);
        assert_eq!(
            frame(&mut scene, 3),
            [(1, 1, 2, 7, 2.0), (1, 1, 3, 7, 3.0)]
        );
        let errors = scene.renderer.condition_shader_errors();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("nope"), "{}", errors[0]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn custom_solvers_move_only_their_own_bodies() {
        use crate::runtime::{
            custom_solver_source, ExtractedGpuPhysicsBody, PhysicsId,
            PhysicsSolver,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let rise = "void solve(inout PhysicsState body) {\n\
                    body.model[3].y += 1.0;\n\
                    }";
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        // Slot 0 uses the rising solver, slot 1 a solver whose shader is
        // missing (so nothing moves it), and slot 2 the built-in Full solver.
        world.gpu_physics = [
            (PhysicsSolver::Custom, Some("rise.glsl")),
            (PhysicsSolver::Custom, Some("missing.glsl")),
            (PhysicsSolver::Full, None),
        ]
        .into_iter()
        .zip(0..)
        .map(|((solver, path), slot)| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3100 + slot)
                .unwrap(),
            physics_id: PhysicsId {
                slot,
                generation: 0,
            },
            transform: crate::Transform::new([slot as f32 * 3.0, 0.0, 0.0]),
            rigid_body: Default::default(),
            solver,
            collider: None,
            custom_shader: path.map(Into::into),
            rules: Vec::new(),
            sync: crate::runtime::PhysicsSyncMode::SelectedState,
        })
        .collect();
        world.gpu_physics_revision = 1;
        world.gpu_solver_shaders =
            vec![custom_solver_source("rise.glsl", rise)];
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -10.0, 0.0];
        world.fixed_delta_seconds = 0.5;
        world.physics_tick = 2;
        scene
            .render(scene.now())
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        scene.renderer.block_until_physics_readbacks_complete();
        let mut heights = scene
            .renderer
            .take_completed_physics_states()
            .into_iter()
            .map(|state| (state.physics_id.slot, state.transform.position[1]))
            .collect::<Vec<_>>();
        heights.sort_by_key(|(slot, _)| *slot);
        assert_eq!(heights.len(), 3, "{heights:?}");
        // Two ticks: +1 each from the solver, no gravity.
        assert!((heights[0].1 - 2.0).abs() < 1e-4, "{heights:?}");
        assert!(heights[1].1.abs() < 1e-4, "{heights:?}");
        assert!(heights[2].1 < -1.0, "{heights:?}");
        assert!(scene.renderer.condition_shader_errors().is_empty());
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn only_state_synchronized_bodies_read_back_their_state() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, PhysicsId, PhysicsSyncMode,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let body = |slot, sync| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                .unwrap(),
            physics_id: PhysicsId {
                slot,
                generation: 7,
            },
            transform: crate::Transform::new([slot as f32, 5.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            custom_shader: None,
            rules: Vec::new(),
            sync,
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![
            body(0, PhysicsSyncMode::Events),
            body(1, PhysicsSyncMode::SelectedState),
            body(2, PhysicsSyncMode::FullState),
        ];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;

        let mut previous_y = 5.0;
        for tick in 1..=(FRAMES_IN_FLIGHT as u64 * 2) {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let states = scene.renderer.take_completed_physics_states();
            let ids: Vec<_> =
                states.iter().map(|state| state.physics_id.slot).collect();
            assert_eq!(ids, [1, 2], "tick {tick}");
            assert!(states.iter().all(|state| state.tick == tick));
            assert_eq!(states[0].physics_id.generation, 7);
            assert_eq!(states[0].custom_values, None);
            assert!(states[1].custom_values.is_some());
            // The copy is taken after the dispatch: gravity has moved it.
            let [x, y, _] = states[0].transform.position;
            assert_eq!(x, 1.0);
            assert!(y < previous_y, "tick {tick}: {y} >= {previous_y}");
            assert!(states[0].linear_velocity[1] < 0.0);
            previous_y = y;
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn culled_gpu_bodies_keep_simulating() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuCondition,
            GpuEventId, GpuEventMode, GpuEventPayload,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                model: MaterialModel::Unlit,
                ..MaterialAsset::default()
            },
        )]);
        let world = &mut scene.render_world;
        // Far above the ±1 view, so every frame culls it.
        world.renderables[0].transform.matrix =
            Matrix4::new_translation(&Vector3::new(0.0, 5.0, 0.0)).into();
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: world.renderables[0].entity,
            physics_id: Default::default(),
            transform: crate::Transform::new([0.0, 5.0, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            sync: Default::default(),
            // Fires only once gravity has moved the body.
            custom_shader: None,
            rules: vec![ExtractedGpuPhysicsRule {
                event_id: GpuEventId(9),
                instructions: GpuCondition::position_y()
                    .less_than(5.0)
                    .compile()
                    .unwrap(),
                mode: GpuEventMode::WhileTrue,
                payload: GpuEventPayload::None,
                cooldown_seconds: 0.0,
            }],
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        for tick in 1..=4 {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let commands = scene.renderer.last_draw_commands[0].clone();
            assert_eq!(commands.read().unwrap()[0].instance_count, 0);
            let events = scene.renderer.take_completed_physics_events();
            assert_eq!(events.len(), 1, "tick {tick}: {events:?}");
            assert_eq!(events[0].event_id, 9);
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_bodies_are_not_culled_by_their_stale_cpu_transform() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                model: MaterialModel::Unlit,
                ..MaterialAsset::default()
            },
        )]);
        // The CPU transform is off-view; the GPU state, which the vertex
        // shader draws, sits in the view center.
        let renderable = &mut scene.render_world.renderables[0];
        renderable.transform.matrix =
            Matrix4::new_translation(&Vector3::new(-3.0, 0.0, 0.0)).into();
        scene.render_world.gpu_physics =
            vec![crate::runtime::ExtractedGpuPhysicsBody {
                entity: renderable.entity,
                physics_id: Default::default(),
                transform: crate::Transform::new([0.0, 0.0, 0.0]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                sync: Default::default(),
                rules: Vec::new(),
            }];
        scene.render_world.gpu_physics_revision = 1;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        // A GPU body sends the frame down the GPU path, which culls by the
        // GPU state and keeps the body.
        assert_eq!(scene.renderer.last_culling_path(), CullingPath::Gpu);
        assert_eq!(scene.renderer.last_frame_culled(), None);
        let center = scene.center_pixel();
        assert!(center[0] > 200, "GPU body draws: {center:?}");

        // `Auto` draws this one-instance scene directly, with no dispatch.
        scene.render_world.culling = CullingMode::Auto;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert_eq!(scene.renderer.last_culling_path(), CullingPath::Direct);
        assert!(!scene
            .renderer
            .last_frame_passes()
            .contains(&FramePass::Culling));
        assert_eq!(scene.renderer.last_frame_culled(), Some(0));
        let center = scene.center_pixel();
        assert!(center[0] > 200, "GPU body draws directly: {center:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_culling_counts_visible_instances_from_gpu_transforms_into_indirect_draws(
    ) {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let unlit = MaterialAsset {
            model: MaterialModel::Unlit,
            ..MaterialAsset::default()
        };
        let blend = MaterialAsset {
            alpha_mode: AlphaMode::Blend,
            ..unlit.clone()
        };
        let mut scene = SlabScene::new(&[
            (0.0, unlit.clone()),
            (0.0, unlit),
            (0.0, blend.clone()),
            (0.0, blend),
        ]);
        let renderables = &mut scene.render_world.renderables;
        renderables[1].material = renderables[0].material;
        let at = |x: f32, scale: f32| crate::runtime::GlobalTransform {
            matrix: (Matrix4::new_translation(&Vector3::new(x, 0.0, 0.0))
                * Matrix4::new_scaling(scale))
            .into(),
        };
        // Opaque batch: a GPU body whose stale CPU transform is centered but
        // whose GPU state sits left of the view, and a small centered cube.
        renderables[0].transform = at(0.0, 0.5);
        renderables[1].transform = at(0.0, 0.5);
        // Two small centered blended instances, one draw each.
        renderables[2].transform = at(0.0, 0.25);
        renderables[3].transform = at(0.0, 0.25);
        scene.render_world.gpu_physics =
            vec![crate::runtime::ExtractedGpuPhysicsBody {
                entity: renderables[0].entity,
                physics_id: Default::default(),
                transform: crate::Transform::new([-3.0, 0.0, 0.0]),
                rigid_body: Default::default(),
                solver: Default::default(),
                collider: None,
                custom_shader: None,
                sync: Default::default(),
                rules: Vec::new(),
            }];
        scene.render_world.gpu_physics_revision = 1;
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let commands = scene.renderer.last_draw_commands[0].clone();
            let counts = commands
                .read()
                .unwrap()
                .iter()
                .map(|command| command.instance_count)
                .collect::<Vec<_>>();
            (counts, scene.center_pixel())
        };

        let (counts, center) = frame(&mut scene);
        assert_eq!(scene.renderer.last_culling_path(), CullingPath::Gpu);
        assert!(scene
            .renderer
            .last_frame_passes()
            .contains(&FramePass::Culling));
        // One opaque batch, then one draw per blended instance.
        assert_eq!(
            counts,
            [1, 1, 1],
            "the GPU body is the one instance out of view"
        );
        assert!(center[0] > 200, "the static cube draws: {center:?}");
        let stats = scene.renderer.culling_stats();
        assert_eq!(
            (stats.path, stats.submitted, stats.visible, stats.culled),
            (CullingPath::Gpu, 4, 3, 1),
            "counts read back from the indirect commands"
        );
        assert!(stats.time.is_some(), "cull dispatch is timed");
        // Indirect draws add their read-back triangles to the direct ones.
        let counters = scene.renderer.render_counters();
        let indirect_triangles = scene.renderer.last_draw_commands[0]
            .read()
            .unwrap()
            .iter()
            .map(|command| {
                u64::from(command.index_count / 3)
                    * u64::from(command.instance_count)
            })
            .sum::<u64>();
        assert!(indirect_triangles > 0);
        assert_eq!(
            counters.triangles,
            scene.renderer.counters.triangles + indirect_triangles
        );
        assert_eq!(counters.visible_instances, 3);
        assert!(counters.dispatches >= 1, "{counters:?}");

        // Centered on the GPU body, the same batch keeps only the body, and
        // it draws from its visible-list slot at the GPU position. The
        // blended instances are now off-view.
        scene.render_world.active_camera.as_mut().unwrap().transform =
            crate::runtime::GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(-3.0, 0.0, 5.0))
                    .into(),
            };
        let (counts, center) = frame(&mut scene);
        assert_eq!(counts, [1, 0, 0]);
        assert!(center[0] > 200, "the GPU body draws: {center:?}");
        let stats = scene.renderer.culling_stats();
        assert_eq!((stats.visible, stats.culled), (1, 3));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn occlusion_culls_hidden_objects_and_draws_revealed_ones_the_same_frame() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let unlit = |base_color| MaterialAsset {
            model: MaterialModel::Unlit,
            base_color,
            ..MaterialAsset::default()
        };
        let mut scene = SlabScene::new(&[
            (0.0, unlit([0.0, 1.0, 0.0, 1.0])),
            (-1.0, unlit([1.0, 0.0, 0.0, 1.0])),
        ]);
        scene.render_world.culling = CullingMode::FrustumAndOcclusion;
        // A small red cube right behind the full-screen green wall.
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(0.0, 0.0, -1.0))
                * Matrix4::new_scaling(0.5))
            .into();
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let drawn = scene
                .renderer
                .last_draw_commands
                .iter()
                .map(|commands| {
                    commands
                        .read()
                        .unwrap()
                        .iter()
                        .map(|command| command.instance_count)
                        .sum::<u32>()
                })
                .collect::<Vec<_>>();
            (drawn, scene.center_pixel())
        };

        // No history yet: the late phase draws both. The next frame draws
        // both early, and the late phase finds the cube behind the wall.
        let (drawn, _) = frame(&mut scene);
        assert_eq!(
            scene.renderer.last_culling_path(),
            CullingPath::GpuOcclusion
        );
        assert_eq!(drawn, [0, 2], "[early, late] draws");
        assert_eq!(
            scene.renderer.last_frame_passes(),
            [
                FramePass::Uploads,
                FramePass::Culling,
                FramePass::Shadow,
                FramePass::Scene,
                FramePass::DepthPyramid,
                FramePass::OcclusionCulling,
                FramePass::LateScene,
                FramePass::ToneMap,
            ]
        );
        let (drawn, _) = frame(&mut scene);
        assert_eq!(drawn, [2, 0]);
        let (drawn, [_, g, r, _]) = frame(&mut scene);
        assert_eq!(drawn, [1, 0], "the hidden cube is not drawn");
        assert_eq!((r, g), (0, 255));
        let stats = scene.renderer.culling_stats();
        assert_eq!(
            (stats.path, stats.submitted, stats.visible, stats.culled),
            (CullingPath::GpuOcclusion, 2, 1, 1)
        );
        assert!(stats.time.is_some(), "cull and pyramid are timed");

        // The camera moves past the wall, which keeps the visibility
        // history: the cube shows up in this very frame, drawn by the late
        // phase against a pyramid without the wall.
        scene.render_world.active_camera.as_mut().unwrap().transform =
            crate::runtime::GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(0.0, 0.0, -0.5))
                    .into(),
            };
        let (drawn, [_, g, r, _]) = frame(&mut scene);
        // The wall's loose bounding sphere still reaches the frustum, so it
        // draws early and is clipped away; the cube draws late.
        assert_eq!(drawn, [1, 1]);
        assert_eq!((r, g), (255, 0), "no one-frame pop");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn lod_groups_draw_the_level_for_the_camera_distance_on_every_path() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                model: MaterialModel::Unlit,
                base_color: [0.0, 1.0, 0.0, 1.0],
                ..MaterialAsset::default()
            },
        )]);
        scene.render_world.renderables[0].cast_shadows = true;
        // The coarse level is the slab moved off the view center, so the
        // center pixel tells the levels apart.
        let base = scene.assets.fallback_mesh;
        let mut shifted = scene.assets.meshes.get(base).unwrap().clone();
        for vertex in &mut shifted.vertices {
            vertex.position[0] += 0.6;
        }
        let coarse = scene.assets.meshes.insert(shifted);
        scene.assets.lod_groups.insert(LodGroupAsset {
            metric: LodMetric::Distance,
            levels: vec![
                crate::assets::LodLevel {
                    mesh: base,
                    until: 7.0,
                },
                crate::assets::LodLevel {
                    mesh: coarse,
                    until: 11.0,
                },
            ],
        });
        // Returns the meshes drawn and whether the center pixel is green.
        let frame = |scene: &mut SlabScene, camera_z: f32| {
            scene.render_world.active_camera.as_mut().unwrap().transform =
                crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        0.0, 0.0, camera_z,
                    ))
                    .into(),
                };
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let prepared = scene.renderer.prepared_instances.as_ref().unwrap();
            let counts = if scene.renderer.last_culling_path().on_gpu() {
                scene
                    .renderer
                    .last_draw_commands
                    .iter()
                    .map(|commands| {
                        commands
                            .read()
                            .unwrap()
                            .iter()
                            .map(|command| command.instance_count)
                            .collect::<Vec<_>>()
                    })
                    .reduce(|a, b| {
                        a.iter().zip(b).map(|(a, b)| a + b).collect()
                    })
                    .unwrap()
            } else {
                let visibility = prepared.visibility.as_ref().unwrap();
                visibility.batches.iter().map(|(_, count)| *count).collect()
            };
            let drawn = counts
                .into_iter()
                .enumerate()
                .filter(|(_, count)| *count > 0)
                // Blended draw commands follow the batch draws.
                .filter_map(|(draw, count)| {
                    assert_eq!(count, 1);
                    prepared.draw_meshes.get(draw).copied()
                })
                .collect::<Vec<_>>();
            (drawn, scene.center_pixel()[1] == 255)
        };
        for (mode, path) in [
            (CullingMode::Frustum, CullingPath::Cpu),
            // `Disabled` still picks a level, so it cannot draw directly.
            (CullingMode::Disabled, CullingPath::Cpu),
            (CullingMode::FrustumAndOcclusion, CullingPath::GpuOcclusion),
        ] {
            scene.render_world.culling = mode;
            assert_eq!(
                frame(&mut scene, 5.0),
                (vec![base.key()], true),
                "{mode:?}"
            );
            assert_eq!(scene.renderer.last_culling_path(), path);
            assert_eq!(
                frame(&mut scene, 9.0),
                (vec![coarse.key()], false),
                "{mode:?}"
            );
            assert_eq!(frame(&mut scene, 13.0), (vec![], false), "{mode:?}");
            assert_eq!(
                frame(&mut scene, 5.0),
                (vec![base.key()], true),
                "{mode:?}"
            );
        }
        let stats = scene.renderer.culling_stats();
        assert_eq!(
            (stats.submitted, stats.visible, stats.culled),
            (1, 1, 0),
            "one instance for every level"
        );
        // One batch draws both levels; only the finest casts shadows.
        let prepared = scene.renderer.prepared_instances.as_ref().unwrap();
        let instances = prepared.instances.read().unwrap();
        assert_eq!(prepared.batches.len(), 1);
        assert_eq!(prepared.batches[0].mesh_key, base.key());
        assert_eq!(prepared.draw_meshes, [base.key(), coarse.key()]);
        assert_eq!(instances[0].physics[3] & 1, 1);
        drop(instances);
        // A blended object keeps one instance per level, sorted alone.
        let material = scene.render_world.renderables[0].material;
        scene.assets.materials.get_mut(material).unwrap().alpha_mode =
            AlphaMode::Blend;
        for (mode, camera_z, green) in [
            (CullingMode::Frustum, 5.0, true),
            (CullingMode::Frustum, 9.0, false),
            (CullingMode::FrustumAndOcclusion, 5.0, true),
            (CullingMode::FrustumAndOcclusion, 9.0, false),
            (CullingMode::FrustumAndOcclusion, 13.0, false),
        ] {
            scene.render_world.culling = mode;
            assert_eq!(frame(&mut scene, camera_z).1, green, "{mode:?}");
            let prepared = scene.renderer.prepared_instances.as_ref().unwrap();
            assert_eq!(prepared.blended.len(), 2);
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn depth_pyramid_mips_hold_the_farthest_depth_below_them() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Odd sizes, so the reduction must fold the leftover row and column.
        let extent = [7, 5];
        let mut scene = SlabScene::with_extent(
            &[(
                0.0,
                MaterialAsset {
                    model: MaterialModel::Unlit,
                    ..MaterialAsset::default()
                },
            )],
            extent,
        );
        scene.render_world.culling = CullingMode::FrustumAndOcclusion;
        // Pixels are 0.4 wide over the ±1.4 by ±1 view. The wall leaves a
        // one-pixel border of cleared depth, so a reduction that drops the
        // leftover column and row, or keeps anything but the farthest texel,
        // loses it.
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(2.0, 1.2, 0.1))
                .into();
        // The second frame draws the wall in the early pass.
        for _ in 0..2 {
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
        }
        let image = scene.renderer.depth_pyramid.view.image().clone();
        let sizes = scene.renderer.depth_pyramid.mips.iter().map(|mip| mip.1);
        assert_eq!(sizes.clone().collect::<Vec<_>>(), [[7, 5], [3, 2], [1, 1]]);
        let mut builder = AutoCommandBufferBuilder::primary(
            Arc::new(StandardCommandBufferAllocator::new(
                scene.base.device.clone(),
                Default::default(),
            )),
            scene.base.queue.queue_family_index(),
            CommandBufferUsage::OneTimeSubmit,
        )
        .unwrap();
        let mut buffers = Vec::new();
        for (level, [width, height]) in sizes.enumerate() {
            let buffer = Buffer::new_slice::<f32>(
                scene.memory_allocator.clone(),
                BufferCreateInfo {
                    usage: BufferUsage::TRANSFER_DST,
                    ..Default::default()
                },
                AllocationCreateInfo {
                    memory_type_filter: MemoryTypeFilter::PREFER_HOST
                        | MemoryTypeFilter::HOST_RANDOM_ACCESS,
                    ..Default::default()
                },
                u64::from(width * height),
            )
            .unwrap();
            builder
                .copy_image_to_buffer(vulkano::command_buffer::CopyImageToBufferInfo {
                    regions: [vulkano::command_buffer::BufferImageCopy {
                        image_subresource:
                            vulkano::image::ImageSubresourceLayers {
                                mip_level: level as u32,
                                ..image.subresource_layers()
                            },
                        image_extent: [width, height, 1],
                        ..Default::default()
                    }]
                    .into(),
                    ..vulkano::command_buffer::CopyImageToBufferInfo::image_buffer(
                        image.clone(),
                        buffer.clone(),
                    )
                })
                .unwrap();
            buffers.push(((width, height), buffer));
        }
        scene
            .now()
            .then_execute(scene.base.queue.clone(), builder.build().unwrap())
            .unwrap()
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let mips = buffers
            .iter()
            .map(|(size, buffer)| (*size, buffer.read().unwrap().to_vec()))
            .collect::<Vec<_>>();
        let ((width, height), base) = &mips[0];
        for y in 0..*height {
            for x in 0..*width {
                let depth = base[(y * width + x) as usize];
                if (1..width - 1).contains(&x) && (1..height - 1).contains(&y) {
                    assert!(depth < 0.5, "wall at {x},{y}: {base:?}");
                } else {
                    assert_eq!(depth, 1.0, "cleared at {x},{y}: {base:?}");
                }
            }
        }
        for pair in mips.windows(2) {
            let (
                ((source_width, source_height), source),
                ((width, height), mip),
            ) = (&pair[0], &pair[1]);
            for y in 0..*height {
                for x in 0..*width {
                    let last = |texel: u32, size: u32, source_size: u32| {
                        if texel == size - 1 {
                            source_size - 1
                        } else {
                            texel * 2 + 1
                        }
                    };
                    let mut farthest = 0.0_f32;
                    for sy in y * 2..=last(y, *height, *source_height) {
                        for sx in x * 2..=last(x, *width, *source_width) {
                            farthest = farthest
                                .max(source[(sy * source_width + sx) as usize]);
                        }
                    }
                    assert_eq!(mip[(y * width + x) as usize], farthest);
                }
            }
        }
        assert_eq!(mips[2].1, [1.0], "the top mip is the farthest depth");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn adding_a_gpu_body_keeps_live_state_of_the_others() {
        use crate::runtime::ExtractedGpuPhysicsBody;
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let body = |raw, y| ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(raw).unwrap(),
            physics_id: Default::default(),
            transform: crate::Transform::new([0.0, y, 0.0]),
            rigid_body: Default::default(),
            solver: Default::default(),
            collider: None,
            sync: Default::default(),
            custom_shader: None,
            rules: Vec::new(),
        };
        let mut scene = SlabScene::new(&[]);
        scene.render_world.gpu_physics = vec![body(3000, 5.0)];
        scene.render_world.gpu_physics_revision = 1;
        scene.render_world.physics_enabled = true;
        scene.render_world.fixed_delta_seconds = 1.0 / 60.0;
        scene.render_world.physics_gravity = [0.0, -9.81, 0.0];
        let step = |scene: &mut SlabScene, tick| {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
        };
        for tick in 1..=30 {
            step(&mut scene, tick);
        }
        let states = |scene: &SlabScene| {
            scene
                .renderer
                .prepared_physics
                .as_ref()
                .unwrap()
                .states
                .clone()
        };
        let fallen = states(&scene).read().unwrap()[0].model[3][1];
        assert!(fallen < 4.9, "body fell before the edit: {fallen}");

        // A new body in front shifts the survivor to a new slot.
        scene.render_world.gpu_physics.insert(0, body(3001, 20.0));
        scene.render_world.gpu_physics_revision = 2;
        step(&mut scene, 31);
        let after = states(&scene).read().unwrap()[1].model[3][1];
        assert!(after < fallen, "survivor restarted: {after} vs {fallen}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn resize_defers_old_depth_destruction_until_its_frame_completes() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        let before = scene.now();
        let frame_one = scene.render(before);
        let old_depth = Arc::downgrade(&scene.renderer.depth);
        // A resize replaces the depth target while frame 1 is not submitted.
        scene.renderer.ensure_depth([16, 16], 1).unwrap();
        assert!(
            old_depth.upgrade().is_some(),
            "frame 1 still owns the replaced depth target"
        );
        frame_one
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert!(
            old_depth.upgrade().is_none(),
            "released once frame 1 completed and its fence was dropped"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn missing_assets_draw_visible_fallbacks_and_are_counted() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Unlit, so the center pixel is the sampled base color.
        let unlit = MaterialAsset {
            model: MaterialModel::Unlit,
            ..Default::default()
        };
        let mut scene = SlabScene::new(&[(0.0, unlit)]);
        // Renders one frame and returns the center pixel as [r, g, b].
        let frame = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };
        let magenta = [255, 0, 255];
        let material = scene.render_world.renderables[0].material;
        let removed = scene.assets.textures.insert(TextureAsset {
            size: [1, 1],
            rgba8: vec![0, 255, 0, 255],
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        scene.assets.textures.remove(removed).unwrap();
        scene
            .assets
            .materials
            .get_mut(material)
            .unwrap()
            .base_color_texture = Some(removed);
        assert_eq!(frame(&mut scene), magenta, "removed base color map");
        assert_eq!(scene.renderer.capacity_diagnostics().missing_textures, 1);

        let malformed = scene.assets.textures.insert(TextureAsset {
            size: [2, 2],
            rgba8: vec![0, 255, 0, 255],
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        scene
            .assets
            .materials
            .get_mut(material)
            .unwrap()
            .base_color_texture = Some(malformed);
        assert_eq!(frame(&mut scene), magenta, "malformed base color map");
        assert_eq!(scene.renderer.capacity_diagnostics().missing_textures, 1);

        // A missing normal map shades like no normal map at all. A grazing
        // light and no ambient make the result depend on the normal.
        let lit = scene.assets.materials.get_mut(material).unwrap();
        lit.base_color_texture = None;
        lit.model = MaterialModel::Pbr;
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        let direction = Vector3::new(-1.0, 0.0, -1.0).normalize();
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 50_000.0,
                    shadows: false,
                },
            },
        );
        scene.render_world.lights_revision += 1;
        let flat = frame(&mut scene);
        scene
            .assets
            .materials
            .get_mut(material)
            .unwrap()
            .normal_texture = Some(removed);
        assert_eq!(frame(&mut scene), flat, "missing normal map is flat");

        let removed_material = scene.render_world.renderables[0].material;
        scene.assets.materials.remove(removed_material).unwrap();
        scene.render_world.renderables[0].mesh = {
            let mesh = scene.assets.meshes.insert(MeshAsset::default());
            scene.assets.meshes.remove(mesh).unwrap();
            mesh
        };
        let [r, g, b] = frame(&mut scene);
        assert!(r > 200 && g < 60 && b > 200, "magenta cube: {r} {g} {b}");
        let diagnostics = scene.renderer.capacity_diagnostics();
        assert_eq!(
            (diagnostics.missing_materials, diagnostics.missing_meshes),
            (1, 1)
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn debug_views_show_unshaded_base_color_and_normals() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                base_color: [1.0, 0.0, 0.0, 1.0],
                ..Default::default()
            },
        )]);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        // Renders one frame in `view` and returns the center [r, g, b].
        let frame = |scene: &mut SlabScene, view| {
            scene.debug_view = view;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };
        assert_eq!(frame(&mut scene, SceneDebugView::Lit), [0, 0, 0]);
        assert_eq!(frame(&mut scene, SceneDebugView::Unshaded), [255, 0, 0]);
        // The slab faces +Z: 0.5, 0.5, 1.0 in linear, sRGB-encoded.
        let [r, g, b] = frame(&mut scene, SceneDebugView::Normals);
        assert!(
            r == g && r.abs_diff(188) <= 1 && b == 255,
            "+Z normal: {r} {g} {b}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn every_primitive_shows_its_outside_to_the_camera() {
        use crate::assets::{procedural_primitive_mesh, PrimitiveShape};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        scene.debug_view = SceneDebugView::Normals;
        // The torus has a hole at the center and the plane faces up.
        for shape in PrimitiveShape::ALL.into_iter().filter(|shape| {
            !matches!(shape, PrimitiveShape::Torus | PrimitiveShape::Plane)
        }) {
            let mesh =
                scene.assets.meshes.insert(procedural_primitive_mesh(shape));
            scene.render_world.renderables[0].mesh = mesh;
            scene.render_world.renderables[0].transform =
                crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_scaling(1.5).into(),
                };
            scene.render_world.renderables_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            // Blue is the normal's z: toward the camera when the front faces
            // survive culling, away when the mesh is inside out.
            let [blue, ..] = scene.center_pixel();
            assert!(blue > 200, "{shape:?} shows its inside: blue {blue}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_screen_material_shows_what_its_camera_sees() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Red slab at the back, white screen slab in front of it.
        let mut scene = SlabScene::new(&[
            (
                0.0,
                MaterialAsset {
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..Default::default()
                },
            ),
            (1.0, MaterialAsset::default()),
        ]);
        // The screen camera looks at the red slab from behind.
        let mut camera = scene.render_world.active_camera.unwrap();
        camera.transform = crate::runtime::GlobalTransform {
            matrix: (Matrix4::new_translation(&nalgebra::Vector3::new(
                0.0, 0.0, -5.0,
            )) * Matrix4::from_axis_angle(
                &nalgebra::Vector3::y_axis(),
                std::f32::consts::PI,
            ))
            .into(),
        };
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            let screens = scene
                .renderer
                .render_screens(
                    before,
                    &scene.render_world,
                    &scene.assets,
                    scene.extent,
                )
                .unwrap();
            scene
                .render(screens)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.center_pixel()
        };
        let [blue, green, red, _] = frame(&mut scene);
        assert!(
            blue > 200 && green > 200 && red > 200,
            "no screen: {blue} {green} {red}"
        );
        scene
            .render_world
            .screens
            .push(crate::runtime::ExtractedScreen {
                material: scene.render_world.renderables[1].material,
                camera,
                size: [8, 8],
                mesh: scene.render_world.renderables[1].mesh,
                transform: scene.render_world.renderables[1].transform,
                update_every: 1,
                enabled: true,
                grading: None,
                exposure: 1.0,
            });
        // Specular light on the screen keeps green and blue above 0.
        let [blue, green, red, _] = frame(&mut scene);
        assert!(
            red > 200 && blue < 120 && green < 120,
            "screen: {blue} {green} {red}"
        );
        assert_eq!(scene.renderer.screens_drawn(), 1);
        // Every other frame; skipped frames keep the last image.
        scene.render_world.screens[0].update_every = 2;
        let drawn: Vec<_> = (0..4)
            .map(|_| {
                let [_, _, red, _] = frame(&mut scene);
                assert!(red > 200, "feed lost: {red}");
                scene.renderer.screens_drawn()
            })
            .collect();
        assert_eq!(drawn.iter().sum::<usize>(), 2, "{drawn:?}");
        assert_ne!(drawn[0], drawn[1], "{drawn:?}");
        scene.render_world.screens[0].update_every = 1;
        scene.render_world.screens[0].enabled = false;
        let [_, _, red, _] = frame(&mut scene);
        assert!(red > 200 && scene.renderer.screens_drawn() == 0);
        // A screen mesh outside the view is not drawn.
        scene.render_world.screens[0].enabled = true;
        scene.render_world.screens[0].transform.matrix =
            Matrix4::new_translation(&nalgebra::Vector3::new(1000.0, 0.0, 0.0))
                .into();
        frame(&mut scene);
        assert_eq!(scene.renderer.screens_drawn(), 0);
        // A wall of six monitors: every new feed draws on its first frame,
        // then at update_every 2 they take turns, three per frame.
        let mut wall = scene.render_world.screens[0];
        wall.transform = scene.render_world.renderables[1].transform;
        wall.update_every = 2;
        scene.render_world.screens = (0..6)
            .map(|index| crate::runtime::ExtractedScreen {
                material: scene.assets.materials.insert(MaterialAsset {
                    name: format!("Monitor {index}"),
                    ..Default::default()
                }),
                ..wall
            })
            .collect();
        frame(&mut scene);
        assert_eq!(scene.renderer.screens_drawn(), 6);
        for _ in 0..2 {
            frame(&mut scene);
            assert_eq!(scene.renderer.screens_drawn(), 3);
        }
    }

    /// Compares `scene`'s image with `src/rendering/golden/<name>.png`, or
    /// writes it when `RUSTING_UPDATE_GOLDEN` is set.
    /// `tolerance` is the largest per-channel difference allowed; drivers pick
    /// slightly different mip levels on minified, tilted textures.
    fn assert_golden(
        scene: &SlabScene,
        name: &str,
        tolerance: u8,
    ) -> image::RgbaImage {
        let [width, height] = scene.extent;
        let mut rgba = scene.pixels();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let image = image::RgbaImage::from_raw(width, height, rgba).unwrap();
        let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("src/rendering/golden/{name}.png"));
        if std::env::var_os(crate::scenario::UPDATE_GOLDEN_ENV).is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            image.save(&golden).unwrap();
        }
        crate::rendering::test_support::assert_matches_golden_image(
            &image,
            &golden,
            &std::env::temp_dir().join(format!("rusting-golden-{name}")),
            tolerance,
        );
        image
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_screen_feed_takes_a_crt_look_with_repeatable_grain() {
        use crate::runtime::ColorGrading;
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A screen slab in front of an unlit red and white checkerboard.
        let mut scene = SlabScene::with_extent(
            &[
                (0.0, MaterialAsset::default()),
                (
                    1.0,
                    MaterialAsset {
                        model: MaterialModel::Unlit,
                        ..Default::default()
                    },
                ),
            ],
            [64, 64],
        );
        let checker = scene.assets.textures.insert(TextureAsset {
            size: [8, 8],
            rgba8: (0..64)
                .flat_map(|i| {
                    if (i % 8 + i / 8) % 2 == 0 {
                        [255, 0, 0, 255]
                    } else {
                        [255, 255, 255, 255]
                    }
                })
                .collect(),
            color_space: TextureColorSpace::Linear,
            sampler: TextureSampler {
                mag_filter: TextureFilter::Nearest,
                min_filter: TextureFilter::Nearest,
                mipmap_filter: TextureFilter::Nearest,
                ..TextureSampler::default()
            },
        });
        scene.render_world.renderables[0].material =
            scene.assets.materials.insert(MaterialAsset {
                model: MaterialModel::Unlit,
                base_color_texture: Some(checker),
                ..Default::default()
            });
        scene.render_world.renderables_revision += 1;
        scene.render_world.physics_tick = 30;
        scene.render_world.fixed_delta_seconds = 1.0 / 60.0;
        let mut camera = scene.render_world.active_camera.unwrap();
        camera.transform = crate::runtime::GlobalTransform {
            matrix: (Matrix4::new_translation(&nalgebra::Vector3::new(
                0.0, 0.0, -5.0,
            )) * Matrix4::from_axis_angle(
                &nalgebra::Vector3::y_axis(),
                std::f32::consts::PI,
            ))
            .into(),
        };
        scene
            .render_world
            .screens
            .push(crate::runtime::ExtractedScreen {
                material: scene.render_world.renderables[1].material,
                camera,
                size: [32, 32],
                mesh: scene.render_world.renderables[1].mesh,
                transform: scene.render_world.renderables[1].transform,
                update_every: 1,
                enabled: true,
                grading: None,
                exposure: 1.0,
            });
        let frame = |scene: &mut SlabScene| {
            let before = scene.now();
            let screens = scene
                .renderer
                .render_screens(
                    before,
                    &scene.render_world,
                    &scene.assets,
                    scene.extent,
                )
                .unwrap();
            scene
                .render(screens)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            scene.pixels()
        };
        let plain = frame(&mut scene);
        let off = assert_golden(&scene, "screen_crt_off", 12);
        // A low exposure darkens the feed and brightens nothing.
        scene.render_world.screens[0].exposure = 0.25;
        let dim = frame(&mut scene);
        let darker = plain.iter().zip(&dim).filter(|(a, b)| b < a).count();
        let brighter = plain.iter().zip(&dim).filter(|(a, b)| b > a).count();
        assert!(darker > 64 * 4, "only {darker} channels got darker");
        assert_eq!(brighter, 0);
        scene.render_world.screens[0].exposure = 1.0;
        let crt = ColorGrading {
            grain: 0.5,
            chromatic_aberration: 1.0,
            scanlines: 1.0,
            color_bleed: 0.5,
            vignette: 0.5,
            noise_band: 1.0,
            distortion: 1.0,
            ..ColorGrading::default()
        };
        scene.render_world.screens[0].grading = Some(crt);
        let on_pixels = frame(&mut scene);
        let on = assert_golden(&scene, "screen_crt_on", 12);
        let changed = off
            .pixels()
            .zip(on.pixels())
            .filter(|(a, b)| {
                a.0.iter().zip(b.0).any(|(a, b)| a.abs_diff(b) > 40)
            })
            .count();
        assert!(changed > 64 * 64 / 4, "only {changed} pixels changed");
        // The same tick draws the same grain and band; the next one moves
        // them.
        assert_eq!(frame(&mut scene), on_pixels);
        scene.render_world.physics_tick += 1;
        assert_ne!(frame(&mut scene), on_pixels);
        // The scene's own grading takes the film effects too.
        scene.render_world.screens.clear();
        scene.render_world.color_grading = Some(ColorGrading {
            grain: 0.5,
            chromatic_aberration: 1.0,
            ..ColorGrading::default()
        });
        let graded = frame(&mut scene);
        assert_eq!(frame(&mut scene), graded);
        scene.render_world.color_grading = None;
        assert_ne!(frame(&mut scene), graded);
    }

    #[test]
    #[cfg(feature = "ui")]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn text_reads_on_a_tilted_panel() {
        use crate::text_texture::{text_texture, TextStyle};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::with_extent(
            &[(0.0, MaterialAsset::default())],
            [256, 128],
        );
        let text = scene.assets.textures.insert(text_texture(
            "CAM 3  REC",
            TextStyle {
                size: 48.0,
                color: [255, 240, 200, 255],
                background: [20, 30, 60, 255],
                ..TextStyle::default()
            },
        ));
        let panel = &mut scene.render_world.renderables[0];
        panel.material = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            base_color_texture: Some(text),
            ..Default::default()
        });
        // A sign turned away to the right and leaning back.
        panel.transform = crate::runtime::GlobalTransform {
            matrix: (Matrix4::from_axis_angle(
                &nalgebra::Vector3::y_axis(),
                0.6,
            ) * Matrix4::from_axis_angle(
                &nalgebra::Vector3::x_axis(),
                -0.3,
            ) * Matrix4::new_nonuniform_scaling(
                &nalgebra::Vector3::new(3.0, 0.75, 0.02),
            ))
            .into(),
        };
        scene.render_world.renderables_revision += 1;
        scene
            .render_world
            .active_camera
            .as_mut()
            .unwrap()
            .projection = Projection::Perspective {
            vertical_fov_radians: 0.5,
            near: 0.1,
            far: 100.0,
        };
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let image = assert_golden(&scene, "text_panel_tilted", 48);
        let lit = image.pixels().filter(|pixel| pixel.0[0] > 160).count();
        assert!(lit > 200, "only {lit} text pixels");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn hdr_values_above_one_survive_until_tone_mapping() {
        use crate::runtime::{ToneMapper, ToneMapping};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                base_color: [0.0, 0.0, 0.0, 1.0],
                emissive: [4.0, 0.5, 0.0],
                ..Default::default()
            },
        )]);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        // Renders one frame and returns the center [r, g], sRGB-encoded.
        let frame = |scene: &mut SlabScene, mapper, exposure| {
            scene.render_world.tone_mapping =
                Some(ToneMapping { mapper, exposure });
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [_, g, r, _] = scene.center_pixel();
            [r, g]
        };
        let near = |[r, g]: [u8; 2], [er, eg]: [u8; 2]| {
            r.abs_diff(er) <= 2 && g.abs_diff(eg) <= 2
        };
        // Linear clips at 1 and leaves 0.5 unchanged, like before HDR.
        let linear = frame(&mut scene, ToneMapper::Linear, 1.0);
        assert!(near(linear, [255, 188]), "linear: {linear:?}");
        // Red 4 stored above 1: exposure 0.25 brings it to exactly 1.
        let exposed = frame(&mut scene, ToneMapper::Linear, 0.25);
        assert!(near(exposed, [255, 99]), "exposure 0.25: {exposed:?}");
        // Reinhard: 4 / 5 = 0.8 and 0.5 / 1.5 = 0.33, no clipping.
        let reinhard = frame(&mut scene, ToneMapper::Reinhard, 1.0);
        assert!(near(reinhard, [231, 156]), "reinhard: {reinhard:?}");
        // ACES fit: 4 gives 0.973 and 0.5 gives 0.616.
        let aces = frame(&mut scene, ToneMapper::Aces, 1.0);
        assert!(near(aces, [252, 206]), "aces: {aces:?}");
        // Color grading runs after the curve, on red 1 and green 0.125.
        let grade = |scene: &mut SlabScene, grading| {
            scene.render_world.color_grading = Some(grading);
            frame(scene, ToneMapper::Linear, 0.25)
        };
        let default = crate::runtime::ColorGrading::default();
        let grey = grade(
            &mut scene,
            crate::runtime::ColorGrading {
                saturation: 0.0,
                ..default
            },
        );
        assert!(near(grey, [149, 149]), "saturation 0: {grey:?}");
        let contrast = grade(
            &mut scene,
            crate::runtime::ColorGrading {
                contrast: 2.0,
                ..default
            },
        );
        assert!(near(contrast, [255, 83]), "contrast 2: {contrast:?}");
        let tint = [0.5, 1.0, 1.0];
        let tinted = grade(
            &mut scene,
            crate::runtime::ColorGrading {
                shadows: tint,
                highlights: tint,
                ..default
            },
        );
        assert!(near(tinted, [188, 99]), "tint: {tinted:?}");
        scene.render_world.color_grading = None;
        // Debug views skip the curve.
        scene.debug_view = SceneDebugView::Normals;
        let [r, g] = frame(&mut scene, ToneMapper::Reinhard, 1.0);
        assert!(r == g && r.abs_diff(188) <= 1, "normals: {r} {g}");
    }

    /// A 64x32 equirectangular map, red where `red(column, row)` holds and
    /// blue elsewhere. Column 48 faces +Z; row 0 is straight up.
    fn environment_texture(
        scene: &mut SlabScene,
        red: impl Fn(u32, u32) -> bool,
    ) -> Handle<TextureAsset> {
        let [width, height] = [64, 32];
        let rgba8 = (0..height)
            .flat_map(|row| (0..width).map(move |column| (column, row)))
            .flat_map(|(column, row)| {
                if red(column, row) {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                }
            })
            .collect();
        scene.assets.textures.insert(TextureAsset {
            size: [width, height],
            rgba8,
            color_space: TextureColorSpace::Linear,
            sampler: TextureSampler::default(),
        })
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn hdr_environment_maps_draw_behind_the_scene_brighter_than_white() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Half-float texels: the left half (around -Z) red at `level`, the
        // right half blue at 1.
        let hdr = |scene: &mut SlabScene, level: u16| {
            let [width, height] = [64, 32];
            let rgba8 = (0..height)
                .flat_map(|_| 0..width)
                .flat_map(|column| {
                    if column < 32 {
                        [level, 0, 0, 0x3c00]
                    } else {
                        [0, 0, 0x3c00, 0x3c00]
                    }
                })
                .flat_map(u16::to_le_bytes)
                .collect();
            scene.assets.textures.insert(TextureAsset {
                size: [width, height],
                rgba8,
                color_space: TextureColorSpace::Hdr,
                sampler: TextureSampler::default(),
            })
        };
        // The camera looks down -Z at the empty background.
        let mut scene = SlabScene::new(&[]);
        let dim = hdr(&mut scene, 0x3400); // 0.25
        scene.render_world.environment = Some((dim, 1.0, false));
        assert_eq!(render_pixel(&mut scene, [4, 4]), [0, 0, 0]);
        scene.render_world.environment = Some((dim, 1.0, true));
        let [r, g, b] = render_pixel(&mut scene, [4, 4]);
        assert!(r > 40 && r < 200 && g < 10 && b < 10, "dim: {r} {g} {b}");
        // 4.0 survives the upload: the background clips to full red.
        let bright = hdr(&mut scene, 0x4400);
        scene.render_world.environment = Some((bright, 1.0, true));
        let [r, g, b] = render_pixel(&mut scene, [4, 4]);
        assert!(r > 250 && g < 10 && b < 10, "bright: {r} {g} {b}");
        // With an intensity of 1/16 it lands under the dim one.
        scene.render_world.environment = Some((bright, 1.0 / 16.0, true));
        let [r, ..] = render_pixel(&mut scene, [4, 4]);
        assert!(r > 40 && r < 200, "scaled: {r}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn clear_glass_shows_what_is_behind_it_and_reflects_the_environment() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Clear glass in front of an unlit green slab, facing the camera.
        let mut scene = SlabScene::new(&[
            (
                0.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [0.0, 1.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
            (
                1.0,
                MaterialAsset {
                    alpha_mode: AlphaMode::Blend,
                    base_color: [1.0, 1.0, 1.0, 0.02],
                    roughness: 0.0,
                    ..MaterialAsset::default()
                },
            ),
        ]);
        // The glass reflects +Z, the red side of the map.
        scene.render_world.environment = Some((
            environment_texture(&mut scene, |column, _| column >= 32),
            1.0,
            false,
        ));
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert!(g > 240, "the green slab shows through: {r} {g} {b}");
        // Four percent at normal incidence, not scaled away by a tiny alpha.
        assert!(r > 20 && b < r / 2, "the glass reflects red: {r} {g} {b}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_blend_material_at_opacity_zero_draws_nothing() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[
            (
                0.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [0.0, 1.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
            (
                1.0,
                MaterialAsset {
                    alpha_mode: AlphaMode::Blend,
                    base_color: [1.0, 1.0, 1.0, 0.0],
                    roughness: 0.0,
                    ..MaterialAsset::default()
                },
            ),
        ]);
        scene.render_world.environment = Some((
            environment_texture(&mut scene, |column, _| column >= 32),
            1.0,
            false,
        ));
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert!(
            r < 3 && b < 3 && g > 240,
            "only the green slab shows: {r} {g} {b}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn mirror_floor_reflects_the_scene_in_screen_space() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A mirror floor with an unlit red wall behind the point the camera
        // looks at. No light or environment map: only the traced wall is red.
        let mut scene = SlabScene::new(&[
            (
                0.0,
                MaterialAsset {
                    metallic: 1.0,
                    roughness: 0.0,
                    ..MaterialAsset::default()
                },
            ),
            (
                -1.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
        ]);
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(4.0, 0.1, 4.0))
                .into();
        scene.render_world.ambient_light = None;
        let camera = scene.render_world.active_camera.as_mut().unwrap();
        let eye = Vector3::new(0.0, 2.0, 5.0);
        camera.transform.matrix = (Matrix4::new_translation(&eye)
            * nalgebra::Rotation3::rotation_between(&-Vector3::z(), &-eye)
                .unwrap()
                .to_homogeneous())
        .into();
        camera.projection = Projection::Orthographic {
            vertical_size: 4.0,
            near: 0.1,
            far: 100.0,
        };
        let mut floor = |quality| {
            scene.render_world.quality = quality;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };
        let [r, g, b] = floor(QualityProfile::High);
        assert!(
            r > 150 && g < r / 2 && b < r / 2,
            "floor mirrors the wall: {r} {g} {b}"
        );
        // Eco skips the trace and the floor sees only the gray hemisphere.
        let [r, g, b] = floor(QualityProfile::Eco);
        assert!(
            r.abs_diff(g) < 10 && g.abs_diff(b) < 10,
            "Eco has no traced wall: {r} {g} {b}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn reflection_probe_shows_the_scene_behind_the_camera() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The camera looks straight down at a mirror floor. An unlit red
        // ceiling over the -X half sits behind the camera, so neither the
        // view nor screen-space reflections can see it; only the probe can.
        let mut scene = SlabScene::new(&[
            (
                0.0,
                MaterialAsset {
                    metallic: 1.0,
                    roughness: 0.0,
                    ..MaterialAsset::default()
                },
            ),
            (
                0.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
        ]);
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(10.0, 0.1, 10.0))
                .into();
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(-3.0, 3.0, 0.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    6.0, 0.1, 10.0,
                )))
            .into();
        scene.render_world.ambient_light = None;
        let camera = scene.render_world.active_camera.as_mut().unwrap();
        // Screen right is +X.
        camera.transform = probe_face_transform(
            [0.0, 2.0, 0.0],
            ([0.0, -1.0, 0.0], [0.0, 0.0, -1.0]),
        );
        camera.projection = Projection::Orthographic {
            vertical_size: 4.0,
            near: 0.1,
            far: 100.0,
        };
        let mut floor = |probes: Vec<([f32; 3], ReflectionProbe)>| {
            scene.render_world.reflection_probes = probes;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let pixel = |column: usize| {
                let [b, g, r, _]: [u8; 4] =
                    pixels[(4 * 8 + column) * 4..][..4].try_into().unwrap();
                [r, g, b]
            };
            (pixel(1), pixel(6))
        };
        let (left, right) = floor(Vec::new());
        assert!(
            left[0] < 150 && right[0] < 150,
            "no probe: {left:?} {right:?}"
        );
        let probe = ReflectionProbe {
            extents: [5.0, 2.5, 5.0],
            intensity: 1.0,
        };
        let (left, right) = floor(vec![([0.0, 1.0, 0.0], probe)]);
        assert!(
            left[0] > 150 && left[1] < left[0] / 2 && left[2] < left[0] / 2,
            "the -X floor reflects the red ceiling: {left:?}"
        );
        assert!(right[0] < 150, "the +X floor does not: {right:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn light_probes_light_a_wall_with_the_red_slab_behind_the_camera() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A white wall faces the camera; a wide unlit red slab hangs behind
        // the camera, out of view. Only a probe in front of the wall sees it.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (
                8.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
        ]);
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(0.0, 0.0, 8.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    40.0, 40.0, 0.1,
                )))
            .into();
        let mut wall = |center: [f32; 3]| {
            scene.render_world.light_probe_grid = Some((
                center,
                crate::runtime::LightProbeGrid {
                    extents: [3.0; 3],
                    resolution: [1, 1, 1],
                    intensity: 1.0,
                },
            ));
            render_pixel(&mut scene, [4, 4])
        };
        let outside = wall([0.0, 0.0, 20.0]);
        assert!(
            outside[0] > 40 && outside[0].abs_diff(outside[1]) < 8,
            "gray from the ambient light outside the grid: {outside:?}"
        );
        let inside = wall([0.0, 0.0, 2.0]);
        assert!(
            inside[0] > 40 && inside[1] < inside[0] / 3,
            "red from the probe: {inside:?}"
        );
        scene.render_world.light_probe_grid = None;
        assert_eq!(render_pixel(&mut scene, [4, 4]), outside, "no grid");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn high_quality_relights_light_probes_when_the_red_slab_moves_away() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // The probe test's scene: a probe in front of a white wall sees an
        // unlit red slab behind the camera. Then the slab moves far away
        // without the grid changing.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (
                8.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
        ]);
        scene.render_world.light_probe_grid = Some((
            [0.0, 0.0, 2.0],
            crate::runtime::LightProbeGrid {
                extents: [3.0; 3],
                resolution: [1, 1, 1],
                intensity: 1.0,
            },
        ));
        let slab_at = |x: f32| -> [[f32; 4]; 4] {
            (Matrix4::new_translation(&Vector3::new(x, 0.0, 8.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    40.0, 40.0, 0.1,
                )))
            .into()
        };
        let mut moved = |quality| {
            scene.render_world.quality = quality;
            scene
                .render_world
                .light_probe_grid
                .as_mut()
                .unwrap()
                .1
                .intensity += 0.001;
            scene.render_world.renderables[1].transform.matrix = slab_at(0.0);
            scene.render_world.renderables_revision += 1;
            let red = render_pixel(&mut scene, [4, 4]);
            // Seen edge on from the probe, far off to the side.
            scene.render_world.renderables[1].transform.matrix = slab_at(500.0);
            scene.render_world.renderables_revision += 1;
            (red, render_pixel(&mut scene, [4, 4]))
        };
        let (red, relit) = moved(QualityProfile::High);
        assert!(red[1] < red[0] / 3, "red from the probe: {red:?}");
        assert!(
            relit[0] > 40 && relit[0].abs_diff(relit[1]) < 8,
            "gray once the probe recaptures: {relit:?}"
        );
        let (red, stale) = moved(QualityProfile::Balanced);
        assert!(red[1] < red[0] / 3, "red from the probe: {red:?}");
        assert_eq!(stale, red, "Balanced keeps the baked probes");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn lightmaps_bounce_a_sunlit_red_slab_onto_the_floor() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A white floor faces the camera with no ambient light. Behind the
        // camera a wide red slab is lit from below by a sun shining up +z,
        // which never reaches the floor's top. Only bounce light can.
        let mut scene = SlabScene::new(&[
            (0.0, MaterialAsset::default()),
            (
                8.0,
                MaterialAsset {
                    base_color: [1.0, 0.0, 0.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
        ]);
        scene.render_world.renderables[1].transform.matrix =
            (Matrix4::new_translation(&Vector3::new(0.0, 0.0, 8.0))
                * Matrix4::new_nonuniform_scaling(&Vector3::new(
                    40.0, 40.0, 0.1,
                )))
            .into();
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [1.0; 3],
            intensity: 0.0,
        });
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &Vector3::new(0.0, 0.01, 1.0).normalize(),
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0; 3],
                    illuminance: 2.0
                        * crate::runtime::DirectionalLight::LUX_PER_UNIT,
                    shadows: false,
                },
            },
        );
        let floor = scene.render_world.renderables[0].entity;
        let slab = scene.render_world.renderables[1].entity;
        let mut lit = |lightmapped: &[bevy_ecs::entity::Entity]| {
            scene.render_world.lightmaps = lightmapped
                .iter()
                .map(|&entity| {
                    (entity, crate::runtime::Lightmap { resolution: 4 })
                })
                .collect();
            // In entity order, as extraction sorts them.
            scene
                .render_world
                .lightmaps
                .sort_by_key(|(entity, _)| *entity);
            render_pixel(&mut scene, [4, 4])
        };
        let dark = lit(&[]);
        assert!(dark[0] < 10, "no ambient or direct light: {dark:?}");
        let bounced = lit(&[floor, slab]);
        assert!(
            bounced[0] > 60 && bounced[1] < bounced[0] / 4,
            "red bounce light: {bounced:?}"
        );
        // An object without a lightmap neither receives nor casts bounce.
        assert_eq!(lit(&[floor]), dark, "slab not lightmapped");
        assert_eq!(lit(&[slab]), dark, "floor not lightmapped");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn transmissive_glass_tints_the_refracted_backdrop() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Green-tinted transmissive glass in front of an unlit white slab.
        // Without the scene color copy it would fade to alpha 0 and let the
        // white through untinted.
        let mut scene = SlabScene::new(&[
            (
                0.0,
                MaterialAsset {
                    model: crate::assets::MaterialModel::Unlit,
                    base_color: [1.0, 1.0, 1.0, 1.0],
                    ..MaterialAsset::default()
                },
            ),
            (
                1.0,
                MaterialAsset {
                    base_color: [0.0, 1.0, 0.0, 1.0],
                    roughness: 0.0,
                    transmission: 1.0,
                    thickness: 0.1,
                    ..MaterialAsset::default()
                },
            ),
        ]);
        // Occlusion frames split the late pass instead of the only one.
        for culling in [CullingMode::Frustum, CullingMode::FrustumAndOcclusion]
        {
            scene.render_world.culling = culling;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            assert!(
                g > 200 && r < g / 3 && b < g / 3,
                "{culling:?}: the white backdrop shows through green: {r} {g} {b}"
            );
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn smooth_metal_reflects_the_environment_map() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(
            0.0,
            MaterialAsset {
                metallic: 1.0,
                roughness: 0.0,
                ..MaterialAsset::default()
            },
        )]);
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(4.0, 0.1, 4.0))
                .into();
        scene.render_world.ambient_light = None;
        // Upper half red (sky), lower half blue (ground).
        scene.render_world.environment = Some((
            environment_texture(&mut scene, |_, row| row < 16),
            1.0,
            false,
        ));
        let look_from = |scene: &mut SlabScene, side: Vector3<f32>| {
            let camera = scene.render_world.active_camera.as_mut().unwrap();
            camera.transform.matrix = (Matrix4::new_translation(&(side * 5.0))
                * nalgebra::Rotation3::rotation_between(
                    &-Vector3::z(),
                    &-side,
                )
                .unwrap()
                .to_homogeneous())
            .into();
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };
        let [r, g, b] = look_from(&mut scene, Vector3::y());
        assert!(r > 200 && g == 0 && b < 20, "top reflects sky: {r} {g} {b}");
        let [r, g, b] = look_from(&mut scene, -Vector3::y());
        assert!(
            r < 20 && g == 0 && b > 200,
            "bottom reflects ground: {r} {g} {b}"
        );

        // Fully rough, it sees the blurred average of both halves.
        let material = scene.render_world.renderables[0].material;
        scene.assets.materials.get_mut(material).unwrap().roughness = 1.0;
        scene.render_world.renderables_revision += 1;
        let [r, g, b] = look_from(&mut scene, Vector3::y());
        assert!(r > 40 && b > 40 && g == 0, "rough blurs: {r} {g} {b}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn sky_light_lights_up_faces_with_sky_and_down_faces_with_ground() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        scene.render_world.renderables[0].transform.matrix =
            Matrix4::new_nonuniform_scaling(&Vector3::new(4.0, 0.1, 4.0))
                .into();
        scene.render_world.ambient_light = None;
        scene.render_world.sky_light = Some(crate::runtime::SkyLight {
            sky_color: [1.0, 0.0, 0.0],
            ground_color: [0.0, 0.0, 1.0],
            intensity: 1.0,
        });
        // Places the camera 5 m along `side` looking back at the slab and
        // returns the center pixel as [r, g, b].
        let look_from = |scene: &mut SlabScene, side: Vector3<f32>| {
            let camera = scene.render_world.active_camera.as_mut().unwrap();
            camera.transform.matrix = (Matrix4::new_translation(&(side * 5.0))
                * nalgebra::Rotation3::rotation_between(
                    &-Vector3::z(),
                    &-side,
                )
                .unwrap()
                .to_homogeneous())
            .into();
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };

        let [r, g, b] = look_from(&mut scene, Vector3::y());
        assert!(r > 200 && g == 0 && b == 0, "top sees sky: {r} {g} {b}");
        let [r, g, b] = look_from(&mut scene, -Vector3::y());
        assert!(
            r == 0 && g == 0 && b > 200,
            "bottom sees ground: {r} {g} {b}"
        );

        // A smooth metal has no diffuse; it shows the sky by reflection.
        let material = scene.render_world.renderables[0].material;
        let metal = scene.assets.materials.get_mut(material).unwrap();
        metal.metallic = 1.0;
        metal.roughness = 0.1;
        scene.render_world.renderables_revision += 1;
        let [r, g, b] = look_from(&mut scene, Vector3::y());
        assert!(
            r > 200 && g == 0 && b == 0,
            "metal reflects sky: {r} {g} {b}"
        );

        // No sky and no ambient light falls back to a dim gray.
        scene.render_world.sky_light = None;
        let [r, g, b] = look_from(&mut scene, Vector3::y());
        assert!(r == g && g == b && r > 0, "fallback gray: {r} {g} {b}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn several_point_lights_add_up_and_fade_out_at_their_range() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        let point = |id: u32, position: [f32; 3], color: [f32; 3], range| {
            crate::runtime::ExtractedPointLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(id).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::from(position))
                        .into(),
                },
                light: crate::runtime::PointLight {
                    color,
                    intensity: 500.0,
                    range,
                    shadows: false,
                    radius: 0.0,
                },
            }
        };
        // Returns the center pixel as [r, g, b].
        let frame = |scene: &mut SlabScene| {
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let [b, g, r, _] = scene.center_pixel();
            [r, g, b]
        };

        let red = point(2000, [0.0, 0.0, 2.0], [1.0, 0.0, 0.0], 4.0);
        scene.render_world.point_lights.push(red);
        let [one_red, g, b] = frame(&mut scene);
        assert!(one_red > 20 && g == 0 && b == 0, "{one_red} {g} {b}");

        scene.render_world.point_lights.push(point(
            2001,
            [1.0, 0.0, 2.0],
            [0.0, 1.0, 0.0],
            4.0,
        ));
        // Out of range: the slab is 2.95 m below a light reaching 1 m.
        scene.render_world.point_lights.push(point(
            2002,
            [0.0, 0.0, 3.0],
            [0.0, 0.0, 1.0],
            1.0,
        ));
        let [r, g, b] = frame(&mut scene);
        assert!(r.abs_diff(one_red) <= 1, "red unchanged, got {r}");
        assert!(g > 20, "second light adds green, got {g}");
        assert_eq!(b, 0, "a light past its range adds nothing");

        scene.render_world.point_lights.push(point(
            2003,
            [0.0, 0.0, 2.0],
            [1.0, 0.0, 0.0],
            4.0,
        ));
        let [two_red, _, _] = frame(&mut scene);
        assert!(two_red > one_red + 20, "{two_red} vs {one_red}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn lights_over_capacity_render_first_max_lights_and_report_the_rest() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        let light = crate::runtime::ExtractedPointLight {
            entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
            transform: crate::runtime::GlobalTransform::default(),
            light: crate::runtime::PointLight::default(),
        };
        scene.render_world.quality = QualityProfile::High;
        scene.render_world.point_lights = vec![light; MAX_LIGHTS + 3];
        scene.render_world.lights_revision += 1;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert_eq!(scene.renderer.capacity_diagnostics().dropped_lights, 3);
        assert_eq!(
            scene.renderer.prepared_lights.as_ref().unwrap().count,
            MAX_LIGHTS as u32
        );

        // A profile change alone must rebuild the light list.
        scene.render_world.quality = QualityProfile::Eco;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let eco = light_budget(QualityProfile::Eco);
        assert_eq!(
            scene.renderer.capacity_diagnostics().dropped_lights,
            MAX_LIGHTS + 3 - eco
        );
        assert_eq!(
            scene.renderer.prepared_lights.as_ref().unwrap().count,
            eco as u32
        );

        scene.render_world.point_lights.truncate(1);
        scene.render_world.lights_revision += 1;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert_eq!(scene.renderer.capacity_diagnostics().dropped_lights, 0);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn hot_reloaded_mesh_swaps_next_frame_and_old_buffers_outlive_in_flight_frame(
    ) {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[(0.0, MaterialAsset::default())]);
        let cube = scene.assets.meshes.get(scene.assets.fallback_mesh).cloned();
        let mesh = scene.assets.meshes.insert(cube.unwrap());
        scene.render_world.renderables[0].mesh = mesh;

        // Frame 1 is submitted but deliberately not waited on.
        let before = scene.now();
        #[allow(clippy::arc_with_non_send_sync)]
        let frame_one = Arc::new(
            scene.render(before).then_signal_fence_and_flush().unwrap(),
        );
        let old_vertices = Arc::downgrade(
            scene.renderer.prepared_meshes[&mesh.key()]
                .vertices
                .buffer(),
        );

        // Hot reload between frames: shrink the slab out of the center pixel.
        for vertex in &mut scene.assets.meshes.get_mut(mesh).unwrap().vertices {
            vertex.position = vertex.position.map(|value| value * 0.01);
        }
        let frame_two = scene.render(frame_one.clone().boxed());
        assert!(
            old_vertices.upgrade().is_some(),
            "replaced buffers stay alive while frame 1 may still read them"
        );
        frame_two
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let [b, g, r, _] = scene.center_pixel();
        assert_eq!([b, g, r], [0, 0, 0], "frame 2 draws the reloaded mesh");
        drop(frame_one);
        // The renderer drops completed frame fences on its next frame.
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert!(
            old_vertices.upgrade().is_none(),
            "old buffers are freed once every referencing frame completed"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn alpha_modes_render_opaque_mask_and_sorted_blend() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let material = |base_color, alpha_mode| MaterialAsset {
            model: crate::assets::MaterialModel::Unlit,
            base_color,
            alpha_mode,
            ..MaterialAsset::default()
        };
        let green = material([0.0, 1.0, 0.0, 1.0], AlphaMode::Opaque);
        let mask = AlphaMode::Mask { cutoff: 0.5 };

        let [_, g, r, _] = render_center_pixel(&[
            (0.0, green.clone()),
            (1.0, material([1.0, 0.0, 0.0, 0.2], mask)),
        ]);
        assert_eq!((r, g), (0, 255), "masked-out slab is discarded");

        let [_, g, r, _] = render_center_pixel(&[
            (0.0, green.clone()),
            (1.0, material([1.0, 0.0, 0.0, 0.8], mask)),
        ]);
        assert_eq!((r, g), (255, 0), "kept masked slab is fully opaque");

        let [_, g, r, _] = render_center_pixel(&[
            (0.0, green),
            (1.0, material([1.0, 0.0, 0.0, 0.3], AlphaMode::Opaque)),
        ]);
        assert_eq!((r, g), (255, 0), "opaque ignores base-color alpha");

        // The near slab is inserted first, so draw order is only correct if
        // blended objects are sorted back to front.
        let [b, _, r, _] = render_center_pixel(&[
            (1.0, material([1.0, 0.0, 0.0, 0.5], AlphaMode::Blend)),
            (0.0, material([0.0, 0.0, 1.0, 0.5], AlphaMode::Blend)),
        ]);
        assert!(r > b + 30, "near red must blend over far blue: r={r} b={b}");
        assert!(b > 80, "far blue must still show through: b={b}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn particles_render_with_their_color_and_size() {
        use crate::runtime::{
            ParticleBatch, ParticleBlend, ParticleInstance, ParticleSprite,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let particle = |x: f32, size: f32, color: [f32; 4]| ParticleInstance {
            position: [x, 0.0, 0.0],
            size,
            color,
            stretch: [0.0; 3],
            rotation: 0.0,
        };
        // The orthographic test camera sees 2 units across 32 pixels.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        scene.render_world.particles = vec![
            ParticleBatch {
                entity: None,
                blend: ParticleBlend::Alpha,
                sprite: ParticleSprite::Square,
                instances: vec![
                    particle(-0.5, 0.5, [1.0, 0.0, 0.0, 1.0]),
                    particle(0.5, 0.25, [1.0, 0.0, 0.0, 1.0]),
                ],
            },
            ParticleBatch {
                entity: None,
                blend: ParticleBlend::Additive,
                sprite: ParticleSprite::Square,
                instances: vec![particle(0.5, 0.25, [0.0, 1.0, 0.0, 1.0])],
            },
        ];
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let pixels = scene.pixels();
        let pixel = |x: usize, y: usize| {
            let at = (y * 32 + x) * 4;
            [pixels[at + 2], pixels[at + 1], pixels[at]]
        };
        let red = pixels.chunks(4).filter(|p| p[2] > 128 && p[1] < 64).count();
        // 0.5 units is 8 pixels square.
        assert_eq!(red, 64, "alpha particle covers its size");
        assert_eq!(pixel(8, 16), [255, 0, 0], "alpha particle color");
        let [r, g, b] = pixel(24, 16);
        assert!(
            r > 128 && g > 128 && b < 32,
            "additive adds green: {r} {g} {b}"
        );
        assert_eq!(pixel(16, 2), [0, 0, 0], "background stays clear");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_particles_spawn_move_and_die_on_the_gpu() {
        use crate::runtime::{
            gpu_particle_batch, ColorKey, ParticleEmitter, ParticleSimulation,
            ParticleSprite, ParticleSystem,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // One red square particle starts still at x = -0.5; a strong wind
        // pushes it along +x and it dies after 0.15 s.
        let emitter = ParticleEmitter {
            simulation: ParticleSimulation::Gpu,
            max_particles: 4,
            speed: [0.0, 0.0],
            size: [0.5, 0.5],
            lifetime: [0.15, 0.15],
            wind: [100.0, 0.0, 0.0],
            fade_in: 0.0,
            fade_out: 0.0,
            color_over_life: vec![ColorKey {
                t: 0.0,
                color: [1.0, 0.0, 0.0, 1.0],
            }],
            sprite: ParticleSprite::Square,
            ..ParticleEmitter::default()
        };
        let global = crate::runtime::GlobalTransform {
            matrix: Matrix4::new_translation(&Vector3::new(-0.5, 0.0, 0.0))
                .into(),
        };
        let system = ParticleSystem {
            spawned: 1,
            ..ParticleSystem::default()
        };
        let entity = bevy_ecs::entity::Entity::from_raw_u32(1).unwrap();
        // The orthographic test camera sees 2 units across 32 pixels.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        scene.render_world.gpu_particles = vec![gpu_particle_batch(
            entity,
            &emitter,
            &system,
            Some(&global),
            None,
        )];
        let mut frame = |elapsed: f32| {
            scene.render_world.elapsed_seconds = elapsed;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let red = |x: usize| pixels[(16 * 32 + x) * 4 + 2];
            let count = pixels.chunks(4).filter(|p| p[2] > 128).count();
            (red(8), red(24), count)
        };
        assert_eq!(frame(0.0), (255, 0, 64), "spawned 8 pixels square");
        // 0.1 s of 100 m/s² wind: 10 m/s, so 1 m along +x.
        assert_eq!(frame(0.1), (0, 255, 64), "moved by the wind");
        assert_eq!(frame(0.2), (0, 0, 0), "dead past its lifetime");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_particles_feel_force_fields_and_stop_at_colliders() {
        use crate::runtime::{
            gpu_particle_batch, CollisionLayers, ColorKey, FieldKind,
            ForceField, GpuCollider, GpuForceField, ParticleEmitter,
            ParticleSimulation, ParticleSprite, ParticleSystem,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A still particle at x = -0.5 is pushed along +x by a 100 m/s²
        // field; a thick wall from x = 0.6 stops it.
        let emitter = ParticleEmitter {
            simulation: ParticleSimulation::Gpu,
            collide: true,
            max_particles: 4,
            speed: [0.0, 0.0],
            size: [0.5, 0.5],
            lifetime: [5.0, 5.0],
            fade_in: 0.0,
            fade_out: 0.0,
            color_over_life: vec![ColorKey {
                t: 0.0,
                color: [1.0, 0.0, 0.0, 1.0],
            }],
            sprite: ParticleSprite::Square,
            ..ParticleEmitter::default()
        };
        let global = crate::runtime::GlobalTransform {
            matrix: Matrix4::new_translation(&Vector3::new(-0.5, 0.0, 0.0))
                .into(),
        };
        let system = ParticleSystem {
            spawned: 1,
            ..ParticleSystem::default()
        };
        let entity = bevy_ecs::entity::Entity::from_raw_u32(1).unwrap();
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        scene.render_world.gpu_particles = vec![gpu_particle_batch(
            entity,
            &emitter,
            &system,
            Some(&global),
            None,
        )];
        scene.render_world.gpu_force_fields = vec![GpuForceField {
            model: crate::Transform::new([0.0; 3]).to_matrix(),
            shape: [0.0, 100.0, 100.0, 100.0],
            field: ForceField {
                kind: FieldKind::Directional,
                strength: 100.0,
                direction: [1.0, 0.0, 0.0],
                ..ForceField::default()
            },
        }];
        scene.render_world.gpu_colliders = vec![GpuCollider {
            model: crate::Transform::new([5.6, 0.0, 0.0]).to_matrix(),
            shape: [0.0, 5.0, 5.0, 5.0],
            velocity: [0.0; 3],
            friction: 0.0,
            restitution: 0.0,
            layers: CollisionLayers::default(),
        }];
        let mut frame = |elapsed: f32| {
            scene.render_world.elapsed_seconds = elapsed;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let red = |x: usize| pixels[(16 * 32 + x) * 4 + 2];
            (red(8), red(24))
        };
        assert_eq!(frame(0.0), (255, 0), "spawned at x = -0.5");
        // 0.1 s at 100 m/s²: 10 m/s, so 1 m along +x.
        assert_eq!(frame(0.1), (0, 255), "pushed by the field");
        // Unstopped it would be 2 m further, off screen.
        assert_eq!(frame(0.2), (0, 255), "stopped by the wall");
        assert_eq!(frame(0.3), (0, 255), "still at the wall");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_particle_trails_draw_the_path_behind_each_particle() {
        use crate::runtime::{
            gpu_particle_batch, ColorKey, ParticleEmitter, ParticleSimulation,
            ParticleSprite, ParticleSystem,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A particle flies from x = -0.8 along +x at 10 m/s for 0.1 s,
        // over 6 frames. Returns the red of pixels at x = -0.3 (behind it)
        // and x = 0.2 (where it ends).
        let run = |trail: f32| {
            let emitter = ParticleEmitter {
                simulation: ParticleSimulation::Gpu,
                trail,
                max_particles: 1,
                direction: [1.0, 0.0, 0.0],
                spread: 0.0,
                speed: [10.0, 10.0],
                size: [0.25, 0.25],
                lifetime: [5.0, 5.0],
                fade_in: 0.0,
                fade_out: 0.0,
                color_over_life: vec![ColorKey {
                    t: 0.0,
                    color: [1.0, 0.0, 0.0, 1.0],
                }],
                sprite: ParticleSprite::Square,
                ..ParticleEmitter::default()
            };
            let global = crate::runtime::GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(-0.8, 0.0, 0.0))
                    .into(),
            };
            let system = ParticleSystem {
                spawned: 1,
                ..ParticleSystem::default()
            };
            let entity = bevy_ecs::entity::Entity::from_raw_u32(1).unwrap();
            let mut scene = SlabScene::with_extent(&[], [32, 32]);
            scene.render_world.gpu_particles = vec![gpu_particle_batch(
                entity,
                &emitter,
                &system,
                Some(&global),
                None,
            )];
            for frame in 0..6 {
                scene.render_world.elapsed_seconds = frame as f32 * 0.02;
                let before = scene.now();
                scene
                    .render(before)
                    .then_signal_fence_and_flush()
                    .unwrap()
                    .wait(None)
                    .unwrap();
            }
            let pixels = scene.pixels();
            let red = |x: usize| pixels[(16 * 32 + x) * 4 + 2];
            (red(11), red(19))
        };
        let (behind, head) = run(0.1);
        assert_eq!(head, 255, "the particle itself");
        assert!(behind > 50, "its trail behind it: {behind}");
        assert_eq!(run(0.0), (0, 255), "no trail without `trail`");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn foliage_copies_lean_in_wind_fields_when_drawn() {
        use crate::runtime::{
            Collider, ColliderShape, FieldKind, Foliage, ForceField,
            MeshRenderer, PhysicsBody, RenderExtractPlugin, RigidBody,
            RigidBodyKind,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // One copy of a 0.2 x 1.6 m pillar at the origin, extracted through
        // a real app so the wind comes from the CPU physics fields.
        let extract = |wind: bool| {
            let mut app = crate::App::new();
            app.add_plugin(RenderExtractPlugin).unwrap();
            let server = AssetServer::default();
            let renderer = MeshRenderer {
                mesh: server.fallback_mesh,
                material: server.fallback_material,
                cast_shadows: false,
                receive_shadows: false,
            };
            app.insert_resource(server);
            let pillar = crate::Transform {
                scale: [0.2, 1.6, 0.2],
                ..crate::Transform::default()
            };
            app.spawn((
                pillar,
                renderer,
                Foliage {
                    count: 1,
                    radius: 0.0,
                    scale_min: 1.0,
                    scale_max: 1.0,
                    sway: 0.05,
                    ..Foliage::default()
                },
            ));
            if wind {
                app.spawn((
                    crate::Transform::default(),
                    PhysicsBody::default(),
                    RigidBody {
                        kind: RigidBodyKind::Fixed,
                        ..RigidBody::default()
                    },
                    Collider {
                        shape: ColliderShape::Box {
                            half_extents: [3.0; 3],
                        },
                        sensor: true,
                        ..Collider::default()
                    },
                    ForceField {
                        kind: FieldKind::Wind,
                        strength: 10.0,
                        direction: [1.0, 0.0, 0.0],
                        ..ForceField::default()
                    },
                ));
            }
            for _ in 0..3 {
                app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                    .unwrap();
            }
            app.world().resource::<RenderWorld>().renderables.clone()
        };
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        // Pixels are 1/16 m; row 4 is 0.72 m up, near the pillar's top.
        let lit = |pixel: [u8; 3]| pixel.iter().any(|&channel| channel > 40);
        scene.render_world.renderables = extract(false);
        assert_eq!(scene.render_world.renderables.len(), 1);
        let upright = [16, 4];
        // 0.5 m lean per metre: the top sits 0.36 m to the right.
        let leaned = [21, 4];
        assert!(lit(render_pixel(&mut scene, upright)), "still air: upright");
        assert!(!lit(render_pixel(&mut scene, leaned)));
        scene.render_world.renderables = extract(true);
        scene.render_world.renderables_revision += 1;
        assert!(lit(render_pixel(&mut scene, leaned)), "wind leans the top");
        assert!(!lit(render_pixel(&mut scene, upright)));
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn meshes_rebuilt_in_place_redraw_where_their_vertices_went() {
        use crate::assets::{MeshAsset, MeshVertex};
        use crate::runtime::{ExtractedRenderable, GlobalTransform};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Cloth, soft-body skins and fluid surfaces rewrite their mesh
        // asset each tick and keep the same handle and renderable. A 0.5 m
        // quad facing the camera, first left of center, then moved right
        // by editing its vertices only.
        let quad = |x: f32| MeshAsset {
            vertices: [
                [-0.25, -0.25],
                [0.25, -0.25],
                [0.25, 0.25],
                [-0.25, 0.25],
            ]
            .map(|[dx, dy]| MeshVertex {
                position: [x + dx, dy, 0.0],
                normal: [0.0, 0.0, 1.0],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        };
        for culling in [CullingMode::Frustum, CullingMode::FrustumAndOcclusion]
        {
            let mut scene = SlabScene::with_extent(&[], [32, 32]);
            scene.render_world.culling = culling;
            let mesh = scene.assets.meshes.insert(quad(-0.6));
            scene.render_world.renderables = vec![ExtractedRenderable {
                entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
                transform: GlobalTransform::default(),
                mesh,
                material: scene.assets.fallback_material,
                cast_shadows: false,
                receive_shadows: false,
                bounds: None,
            }];
            scene.render_world.renderables_revision += 1;
            // Pixels are 1/16 m: x = -0.6 m is pixel 6, x = 0.6 m pixel 25.
            let lit =
                |pixel: [u8; 3]| pixel.iter().any(|&channel| channel > 40);
            let (left, right) = ([6, 16], [25, 16]);
            assert!(
                lit(render_pixel(&mut scene, left)),
                "{culling:?}: drawn on the left"
            );
            assert!(!lit(render_pixel(&mut scene, right)));
            *scene.assets.meshes.get_mut(mesh).unwrap() = quad(0.6);
            // Outside the first mesh box: stale bounds would cull it.
            assert!(
                lit(render_pixel(&mut scene, right)),
                "{culling:?}: follows its vertices"
            );
            assert!(
                !lit(render_pixel(&mut scene, left)),
                "{culling:?}: old shape gone"
            );
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn material_overrides_recolor_one_object_inside_the_shared_batch() {
        use crate::assets::{MeshAsset, MeshVertex};
        use crate::runtime::{
            ExtractedRenderable, GlobalTransform, MaterialOverride,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // Two 0.5 m quads at x = -0.5 and 0.5 share one grey unlit
        // material; the right one gets its own color.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let mesh = scene.assets.meshes.insert(MeshAsset {
            vertices: [
                [-0.25, -0.25],
                [0.25, -0.25],
                [0.25, 0.25],
                [-0.25, 0.25],
            ]
            .map(|[x, y]| MeshVertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        });
        let material = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            base_color: [0.5, 0.5, 0.5, 1.0],
            ..MaterialAsset::default()
        });
        let entity = |id| bevy_ecs::entity::Entity::from_raw_u32(id).unwrap();
        scene.render_world.renderables = [(1, -0.5), (2, 0.5)]
            .map(|(id, x)| ExtractedRenderable {
                entity: entity(id),
                transform: GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        x, 0.0, 0.0,
                    ))
                    .into(),
                },
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
                bounds: None,
            })
            .to_vec();
        scene.render_world.renderables_revision += 1;
        // Pixels are 1/16 m: x = -0.5 is pixel 8, x = 0.5 pixel 24.
        let frame = |scene: &mut SlabScene| {
            let left = render_pixel(scene, [8, 16]);
            let right = render_pixel(scene, [24, 16]);
            (left, right, scene.renderer.render_counters().draws)
        };
        let (left, right, shared) = frame(&mut scene);
        assert_eq!(left, right, "both grey");
        let red = |color: [f32; 4]| MaterialOverride {
            base_color: Some(color),
            ..MaterialOverride::default()
        };
        scene.render_world.material_overrides =
            vec![(entity(2), red([1.0, 0.0, 0.0, 1.0]))];
        let (grey, right, draws) = frame(&mut scene);
        assert_eq!(grey, left, "the other object keeps the material");
        assert!(right[0] > 200 && right[1] < 40, "overridden: {right:?}");
        assert_eq!(draws, shared, "still one batch");
        // A changed override redraws without a renderables change.
        scene.render_world.material_overrides =
            vec![(entity(2), red([0.0, 0.0, 1.0, 1.0]))];
        let (_, right, _) = frame(&mut scene);
        assert!(right[2] > 200 && right[0] < 40, "changed: {right:?}");
        scene.render_world.material_overrides.clear();
        assert_eq!(frame(&mut scene).1, left, "removed");
    }

    #[test]
    fn surface_shaders_need_the_versioned_header() {
        let hook = "void surface(SurfaceInput i, inout Surface s) {}";
        let glsl = super::surface_fragment_glsl(
            &format!("\n#rusting surface 1\n{hook}"),
            "",
        )
        .unwrap();
        assert!(glsl.contains("#define RUSTING_CUSTOM_SURFACE"));
        assert!(glsl.contains(hook));
        assert!(!glsl.contains("#include"), "includes are inlined");
        // The hook counts lines from the user's file.
        assert!(glsl.contains("#line 3\n"), "{glsl}");
        for bad in [hook, "#rusting surface 2", "#rusting sky 1"] {
            let error = super::surface_fragment_glsl(bad, "").unwrap_err();
            assert!(error.contains("surface"), "{bad:?}: {error}");
        }
        let post = "void post(PostInput i, inout vec3 color) {}";
        let glsl =
            super::post_fragment_glsl(&format!("#rusting post 1\n{post}"))
                .unwrap();
        assert!(
            glsl.contains("#define RUSTING_CUSTOM_POST") && glsl.contains(post)
        );
        assert!(!glsl.contains("#include"), "includes are inlined");
        let error = super::post_fragment_glsl(hook).unwrap_err();
        assert!(error.contains("post"), "{error}");
        let sky = "void sky(SkyInput i, inout vec3 color) {}";
        let glsl =
            super::sky_fragment_glsl(&format!("#rusting sky 1\n{sky}"), "")
                .unwrap();
        assert!(
            glsl.contains("#define RUSTING_CUSTOM_SKY") && glsl.contains(sky)
        );
        assert!(!glsl.contains("#include"), "includes are inlined");
        let particle = "void particle(ParticleInput i, inout vec4 color) {}";
        let glsl = super::particle_fragment_glsl(&format!(
            "#rusting particle 1\n{particle}"
        ))
        .unwrap();
        assert!(
            glsl.contains("#define RUSTING_CUSTOM_PARTICLE")
                && glsl.contains(particle)
        );
        // The fog hook goes into both the scene and the sky shader, next to
        // their own hooks, and its errors name it.
        let fog = "void fog(FogInput i, inout vec4 haze) {}";
        let fog_source = format!("#rusting fog 1\n{fog}");
        for glsl in [
            super::surface_fragment_glsl("", &fog_source).unwrap(),
            super::sky_fragment_glsl("", &fog_source).unwrap(),
        ] {
            assert!(glsl.contains("#define RUSTING_CUSTOM_FOG"));
            assert!(!glsl.contains("#define RUSTING_CUSTOM_SURFACE"));
            assert!(glsl.contains(fog) && !glsl.contains("#include"));
        }
        let error = super::sky_fragment_glsl("", "#rusting sky 1").unwrap_err();
        assert!(error.contains("fog"), "{error}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_skinning_draws_what_cpu_skinning_draws() {
        use crate::assets::{MeshAsset, MeshVertex, MorphTargets, SkinWeights};
        use crate::runtime::{
            morph_mesh, skin_mesh, ExtractedRenderable, GlobalTransform,
            SkinJob,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let cooked = MeshAsset {
            vertices: [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]
                .map(|[x, y]| MeshVertex {
                    position: [x, y, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    ..MeshVertex::default()
                })
                .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        };
        // The left edge stays on joint 0; the right edge follows joint 1,
        // half and fully, and a blend shape lifts the top left corner and
        // tips every normal.
        let skin = SkinWeights {
            joints: vec![[0; 4], [0, 1, 0, 0], [1, 0, 0, 0], [0; 4]],
            weights: vec![
                [1.0, 0.0, 0.0, 0.0],
                [1.0, 1.0, 0.0, 0.0],
                [2.0, 0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0, 0.0],
            ],
        };
        let morph = MorphTargets {
            positions: vec![vec![
                [0.0; 3],
                [0.0; 3],
                [0.0; 3],
                [0.0, 0.3, 0.0],
            ]],
            normals: vec![vec![[0.6, 0.0, 0.0]; 4]],
        };
        let matrices = [
            Matrix4::identity(),
            Matrix4::new_translation(&Vector3::new(0.3, -0.2, 0.0))
                * Matrix4::from_axis_angle(&Vector3::y_axis(), 0.5),
        ];
        let weights = vec![0.8];
        let cpu = scene.assets.meshes.insert(skin_mesh(
            &morph_mesh(&cooked, &morph, &weights),
            &skin,
            &matrices,
        ));
        let base = scene.assets.meshes.insert(cooked.clone());
        let copy = scene.assets.meshes.insert(cooked);
        scene
            .assets
            .deformers
            .cooked
            .insert(base.key(), Arc::new((Some(skin), morph)));
        let material = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            shader: "#rusting surface 1\n\
                     void surface(SurfaceInput i, inout Surface s) {\n\
                         s.base_color.rgb = s.normal * 0.5 + 0.5;\n\
                     }"
            .into(),
            ..MaterialAsset::default()
        });
        let draw = |scene: &mut SlabScene, mesh| {
            scene.render_world.renderables = vec![ExtractedRenderable {
                entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
                transform: GlobalTransform::default(),
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
                bounds: None,
            }];
            scene.render_world.renderables_revision += 1;
            render_pixel(scene, [0, 0]);
            scene.pixels()
        };
        let undeformed = draw(&mut scene, copy);
        let reference = draw(&mut scene, cpu);
        scene.render_world.skin_jobs = vec![SkinJob {
            mesh: copy,
            base,
            matrices: matrices.map(Into::into).to_vec(),
            morph_weights: weights,
        }];
        let skinned = draw(&mut scene, copy);
        assert!(scene.renderer.shader_errors().0.is_empty());
        let differ = |a: &[u8], b: &[u8]| {
            a.iter().zip(b).filter(|(a, b)| a.abs_diff(**b) > 2).count()
        };
        assert!(differ(&undeformed, &reference) > 100);
        // Edge pixels may round the other way.
        assert!(
            differ(&skinned, &reference) <= 8,
            "{} channels differ",
            differ(&skinned, &reference)
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn surface_shaders_color_their_material_per_pixel() {
        use crate::assets::{MeshAsset, MeshVertex};
        use crate::runtime::{ExtractedRenderable, GlobalTransform};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A grey built-in quad at x = -0.5 and a shaded one at x = 0.5,
        // each 0.5 m wide with u running left to right.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let mesh = scene.assets.meshes.insert(MeshAsset {
            vertices: [
                [-0.25, -0.25],
                [0.25, -0.25],
                [0.25, 0.25],
                [-0.25, 0.25],
            ]
            .map(|[x, y]| MeshVertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [x * 2.0 + 0.5, 0.5 - y * 2.0],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        });
        let shaded = |scene: &mut SlabScene, shader: &str| {
            scene.assets.materials.insert(MaterialAsset {
                model: MaterialModel::Unlit,
                base_color: [0.5, 0.5, 0.5, 1.0],
                shader: shader.to_owned(),
                ..MaterialAsset::default()
            })
        };
        let grey = shaded(&mut scene, "");
        let place = |scene: &mut SlabScene, material| {
            scene.render_world.renderables =
                [(1, -0.5, grey), (2, 0.5, material)]
                    .map(|(id, x, material)| ExtractedRenderable {
                        entity: bevy_ecs::entity::Entity::from_raw_u32(id)
                            .unwrap(),
                        transform: GlobalTransform {
                            matrix: Matrix4::new_translation(&Vector3::new(
                                x, 0.0, 0.0,
                            ))
                            .into(),
                        },
                        mesh,
                        material,
                        cast_shadows: false,
                        receive_shadows: false,
                        bounds: None,
                    })
                    .to_vec();
            scene.render_world.renderables_revision += 1;
        };
        // The shaded quad covers pixels 20 to 27.
        let gradient = shaded(
            &mut scene,
            "#rusting surface 1\n\
             void surface(SurfaceInput i, inout Surface s) {\n\
                 s.base_color.rgb = vec3(i.uv.x, 0.0, 1.0 - i.uv.x);\n\
             }",
        );
        place(&mut scene, gradient);
        let left = render_pixel(&mut scene, [8, 16]);
        let near = render_pixel(&mut scene, [21, 16]);
        let far = render_pixel(&mut scene, [26, 16]);
        assert!(
            scene.renderer.surface_shader_errors().is_empty(),
            "{:?}",
            scene.renderer.surface_shader_errors()
        );
        assert!(left[0] == left[2] && left[1] > 100, "built-in: {left:?}");
        assert!(near[2] > near[0] && far[0] > far[2], "{near:?} {far:?}");
        assert!(near[1] < 20 && far[1] < 20, "{near:?} {far:?}");
        // A shader graph compiles to the same gradient.
        {
            use crate::rendering::shader_graph::{
                ShaderGraph, ShaderInput, ShaderNode, SurfaceOutput,
            };
            let graph = ShaderGraph {
                nodes: vec![
                    ShaderNode::Constant([0.0, 0.0, 1.0, 1.0]),
                    ShaderNode::Constant([1.0, 0.0, 0.0, 1.0]),
                    ShaderNode::Input(ShaderInput::Uv),
                    ShaderNode::Component(2, 0),
                    ShaderNode::Mix(0, 1, 3),
                ],
                outputs: vec![(SurfaceOutput::BaseColor, 4)],
            };
            let graphed = shaded(&mut scene, &graph.compile().unwrap());
            place(&mut scene, graphed);
            assert_eq!(render_pixel(&mut scene, [21, 16]), near);
            assert_eq!(render_pixel(&mut scene, [26, 16]), far);
            assert!(scene.renderer.shader_errors().0.is_empty());
        }
        // `i.time` is the render world's clock.
        let clock = shaded(
            &mut scene,
            "#rusting surface 1\n\
             void surface(SurfaceInput i, inout Surface s) {\n\
                 s.base_color.rgb = vec3(i.time);\n\
             }",
        );
        place(&mut scene, clock);
        scene.render_world.elapsed_seconds = 0.1;
        let early = render_pixel(&mut scene, [24, 16]);
        scene.render_world.elapsed_seconds = 0.9;
        let late = render_pixel(&mut scene, [24, 16]);
        assert!(late[0] > early[0] + 60, "{early:?} {late:?}");
        // Broken shaders draw the built-in surface and say why.
        let future = shaded(&mut scene, "#rusting surface 2\nvoid f() {}");
        place(&mut scene, future);
        assert_eq!(render_pixel(&mut scene, [24, 16]), left);
        let errors = scene.renderer.surface_shader_errors();
        assert!(
            errors.iter().any(|error| error.contains("version")),
            "{errors:?}"
        );
        let typo = shaded(
            &mut scene,
            "#rusting surface 1\n\
             void surface(SurfaceInput i, inout Surface s) {\n\
                 s.base_color = oops;\n\
             }",
        );
        place(&mut scene, typo);
        assert_eq!(render_pixel(&mut scene, [24, 16]), left);
        let errors = scene.renderer.surface_shader_errors();
        assert!(
            errors
                .iter()
                .any(|error| error.contains(":3:") && error.contains("oops")),
            "{errors:?}"
        );
        let errors = scene.renderer.shader_errors().0;
        assert!(
            errors.len() == 2
                && errors.iter().all(|e| e.starts_with("surface: ")),
            "{errors:?}"
        );
        // Fixing the source in place rebuilds the shader and clears its
        // error; the unfixed one keeps reporting.
        scene.assets.materials.get_mut(typo).unwrap().shader =
            scene.assets.materials.get(gradient).unwrap().shader.clone();
        assert_eq!(render_pixel(&mut scene, [26, 16]), far);
        let errors = scene.renderer.shader_errors().0;
        assert!(
            errors.len() == 1 && errors[0].contains("version"),
            "{errors:?}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn sky_shaders_paint_only_the_background() {
        use crate::assets::{MeshAsset, MeshVertex};
        use crate::runtime::{ExtractedRenderable, GlobalTransform};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A grey quad on pixels 4 to 11; the rest is background.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let mesh = scene.assets.meshes.insert(MeshAsset {
            vertices: [
                [-0.25, -0.25],
                [0.25, -0.25],
                [0.25, 0.25],
                [-0.25, 0.25],
            ]
            .map(|[x, y]| MeshVertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        });
        let grey = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            base_color: [0.5, 0.5, 0.5, 1.0],
            ..MaterialAsset::default()
        });
        scene.render_world.renderables = vec![ExtractedRenderable {
            entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
            transform: GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(-0.5, 0.0, 0.0))
                    .into(),
            },
            mesh,
            material: grey,
            cast_shadows: false,
            receive_shadows: false,
            bounds: None,
        }];
        scene.render_world.renderables_revision += 1;
        let plain = [8, 24].map(|x| render_pixel(&mut scene, [x, 16]));
        let sky = |scene: &mut SlabScene, body: &str| {
            scene.render_world.sky_shader = format!(
                "#rusting sky 1\n\
                 void sky(SkyInput i, inout vec3 color) {{\n{body}\n}}"
            );
        };
        // The camera looks along -z, so the background turns blue and the
        // quad stays as it was.
        sky(&mut scene, "color = max(-i.direction, 0.0);");
        let quad = render_pixel(&mut scene, [8, 16]);
        let back = render_pixel(&mut scene, [24, 16]);
        assert_eq!(scene.renderer.sky_shader_error(), None);
        assert_eq!(quad, plain[0], "surfaces cover the sky");
        assert!(back[2] > 200 && back[0] < 20, "{back:?} {plain:?}");
        // `i.time` is the render world's clock.
        sky(&mut scene, "color = vec3(i.time);");
        scene.render_world.elapsed_seconds = 0.1;
        let early = render_pixel(&mut scene, [24, 16]);
        scene.render_world.elapsed_seconds = 0.9;
        let late = render_pixel(&mut scene, [24, 16]);
        assert!(late[0] > early[0] + 60, "{early:?} {late:?}");
        // Broken shaders leave the built-in background and say why.
        scene.render_world.sky_shader = "#rusting sky 2\n".into();
        assert_eq!(render_pixel(&mut scene, [24, 16]), plain[1]);
        let error = scene.renderer.sky_shader_error().unwrap_or_default();
        assert!(error.contains("version"), "{error}");
        sky(&mut scene, "color = oops;");
        assert_eq!(render_pixel(&mut scene, [24, 16]), plain[1]);
        let error = scene.renderer.sky_shader_error().unwrap_or_default();
        assert!(error.contains(":3:") && error.contains("oops"), "{error}");
        scene.render_world.sky_shader.clear();
        assert_eq!(render_pixel(&mut scene, [24, 16]), plain[1]);
        assert_eq!(scene.renderer.sky_shader_error(), None);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn particle_shaders_change_only_their_emitter() {
        use crate::runtime::{
            ParticleBatch, ParticleBlend, ParticleInstance, ParticleSprite,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let entity = |i| bevy_ecs::entity::Entity::from_raw_u32(i).unwrap();
        // Two white square emitters, on pixels 4 to 11 and 20 to 27.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        scene.render_world.particles = [(1, -0.5), (2, 0.5)]
            .map(|(i, x)| ParticleBatch {
                entity: Some(entity(i)),
                blend: ParticleBlend::Alpha,
                sprite: ParticleSprite::Square,
                instances: vec![ParticleInstance {
                    position: [x, 0.0, 0.5],
                    size: 0.5,
                    color: [1.0, 1.0, 1.0, 1.0],
                    ..ParticleInstance::default()
                }],
            })
            .to_vec();
        let plain = render_pixel(&mut scene, [8, 16]);
        let back = render_pixel(&mut scene, [16, 16]);
        assert!(plain[1] > back[1] + 60, "{plain:?} {back:?}");
        let shade = |scene: &mut SlabScene, body: &str| {
            scene.render_world.particle_shaders = HashMap::from([(
                entity(1),
                format!(
                    "#rusting particle 1\n\
                     void particle(ParticleInput i, inout vec4 color) \
                     {{\n{body}\n}}"
                ),
            )]);
        };
        shade(&mut scene, "color.rgb = vec3(1.0, 0.0, 0.0);");
        let red = render_pixel(&mut scene, [8, 16]);
        assert_eq!(scene.renderer.particle_shader_errors(), Vec::<&str>::new());
        assert!(red[0] > 100 && red[1] < 10, "{red:?} {plain:?}");
        assert_eq!(render_pixel(&mut scene, [24, 16]), plain, "other emitter");
        // Alpha 0 draws nothing; `corner` runs -1..1 across the quad.
        shade(&mut scene, "color.a *= step(0.0, i.corner.x);");
        assert_eq!(render_pixel(&mut scene, [5, 16]), back);
        assert_eq!(render_pixel(&mut scene, [10, 16]), plain);
        // `i.time` is the render world's clock.
        shade(&mut scene, "color.rgb = vec3(i.time);");
        scene.render_world.elapsed_seconds = 0.1;
        let early = render_pixel(&mut scene, [8, 16]);
        scene.render_world.elapsed_seconds = 0.9;
        let late = render_pixel(&mut scene, [8, 16]);
        assert!(late[0] > early[0] + 60, "{early:?} {late:?}");
        // Broken shaders leave the built-in look and say why.
        shade(&mut scene, "color = oops;");
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain);
        let errors = scene.renderer.particle_shader_errors();
        assert!(
            errors.len() == 1 && errors[0].contains("oops"),
            "{errors:?}"
        );
        scene.render_world.particle_shaders.clear();
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain);
        assert_eq!(scene.renderer.particle_shader_errors(), Vec::<&str>::new());
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn fog_shaders_cover_surfaces_and_the_background() {
        use crate::assets::{MeshAsset, MeshVertex};
        use crate::runtime::{ExtractedRenderable, GlobalTransform};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A grey quad on pixels 4 to 11; the rest is background.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let mesh = scene.assets.meshes.insert(MeshAsset {
            vertices: [
                [-0.25, -0.25],
                [0.25, -0.25],
                [0.25, 0.25],
                [-0.25, 0.25],
            ]
            .map(|[x, y]| MeshVertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        });
        let grey = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            base_color: [0.5, 0.5, 0.5, 1.0],
            ..MaterialAsset::default()
        });
        scene.render_world.renderables = vec![ExtractedRenderable {
            entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
            transform: GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(-0.5, 0.0, 0.0))
                    .into(),
            },
            mesh,
            material: grey,
            cast_shadows: false,
            receive_shadows: false,
            bounds: None,
        }];
        scene.render_world.renderables_revision += 1;
        let plain = [8, 24].map(|x| render_pixel(&mut scene, [x, 16]));
        let fog = |scene: &mut SlabScene, body: &str| {
            scene.render_world.fog_shader = format!(
                "#rusting fog 1\n\
                 void fog(FogInput i, inout vec4 haze) {{\n{body}\n}}"
            );
        };
        // Without a Fog component, haze starts clear; the shader paints
        // the quad red and the background green.
        fog(
            &mut scene,
            "haze = i.background ? vec4(0.0, 1.0, 0.0, 0.0) \
             : vec4(1.0, 0.0, 0.0, 0.0);",
        );
        let quad = render_pixel(&mut scene, [8, 16]);
        let back = render_pixel(&mut scene, [24, 16]);
        assert_eq!(scene.renderer.sky_shader_error(), None);
        assert_eq!(scene.renderer.surface_shader_errors(), Vec::<&str>::new());
        assert!(quad[0] > 200 && quad[1] < 20, "{quad:?} {plain:?}");
        assert!(back[1] > 200 && back[0] < 20, "{back:?} {plain:?}");
        // Half the surface shows through.
        fog(&mut scene, "haze = vec4(0.0, 0.0, 0.0, 0.5);");
        let dim = render_pixel(&mut scene, [8, 16]);
        assert!(
            dim[0] + 20 < plain[0][0] && dim[0] > 10,
            "{dim:?} {plain:?}"
        );
        // `i.time` is the render world's clock.
        fog(&mut scene, "haze = vec4(vec3(i.time), 0.0);");
        scene.render_world.elapsed_seconds = 0.1;
        let early = render_pixel(&mut scene, [8, 16]);
        scene.render_world.elapsed_seconds = 0.9;
        let late = render_pixel(&mut scene, [8, 16]);
        assert!(late[0] > early[0] + 60, "{early:?} {late:?}");
        // Broken shaders leave the built-in fog and say why.
        fog(&mut scene, "haze = oops;");
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain[0]);
        assert_eq!(render_pixel(&mut scene, [24, 16]), plain[1]);
        let error = scene.renderer.sky_shader_error().unwrap_or_default();
        assert!(error.contains(":3:") && error.contains("oops"), "{error}");
        scene.render_world.fog_shader.clear();
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain[0]);
        assert_eq!(scene.renderer.sky_shader_error(), None);
        assert_eq!(scene.renderer.surface_shader_errors(), Vec::<&str>::new());
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn post_shaders_rework_the_finished_image() {
        use crate::assets::{MeshAsset, MeshVertex};
        use crate::runtime::{ExtractedRenderable, GlobalTransform};
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A grey quad on pixels 4 to 11 of a 32 pixel image.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let mesh = scene.assets.meshes.insert(MeshAsset {
            vertices: [
                [-0.25, -0.25],
                [0.25, -0.25],
                [0.25, 0.25],
                [-0.25, 0.25],
            ]
            .map(|[x, y]| MeshVertex {
                position: [x, y, 0.0],
                normal: [0.0, 0.0, 1.0],
                ..MeshVertex::default()
            })
            .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
        });
        let grey = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            base_color: [0.5, 0.5, 0.5, 1.0],
            ..MaterialAsset::default()
        });
        scene.render_world.renderables = vec![ExtractedRenderable {
            entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
            transform: GlobalTransform {
                matrix: Matrix4::new_translation(&Vector3::new(-0.5, 0.0, 0.0))
                    .into(),
            },
            mesh,
            material: grey,
            cast_shadows: false,
            receive_shadows: false,
            bounds: None,
        }];
        scene.render_world.renderables_revision += 1;
        let plain = [8, 24].map(|x| render_pixel(&mut scene, [x, 16]));
        assert_ne!(plain[0], plain[1], "the quad shows: {plain:?}");
        let post = |scene: &mut SlabScene, body: &str| {
            scene.render_world.post_shader = format!(
                "#rusting post 1\n\
                 void post(PostInput i, inout vec3 color) {{\n{body}\n}}"
            );
        };
        post(&mut scene, "color = vec3(i.uv.x, 0.0, 1.0 - i.uv.x);");
        let left = render_pixel(&mut scene, [4, 16]);
        let right = render_pixel(&mut scene, [28, 16]);
        assert_eq!(scene.renderer.post_shader_error(), None);
        assert!(
            left[2] > left[0] && right[0] > right[2],
            "{left:?} {right:?}"
        );
        // `scene_at` reads other pixels: here the one half the image left.
        post(&mut scene, "color = scene_at(i.uv - vec2(0.5, 0.0));");
        let shifted = render_pixel(&mut scene, [24, 16]);
        assert!(
            shifted
                .iter()
                .zip(plain[1])
                .all(|(a, b)| a.abs_diff(b) > 30),
            "{shifted:?} {plain:?}"
        );
        // `i.time` follows the fixed tick.
        post(&mut scene, "color = vec3(i.time);");
        scene.render_world.fixed_delta_seconds = 1.0 / 60.0;
        scene.render_world.physics_tick = 6;
        let early = render_pixel(&mut scene, [24, 16]);
        scene.render_world.physics_tick = 54;
        let late = render_pixel(&mut scene, [24, 16]);
        assert!(late[0] > early[0] + 60, "{early:?} {late:?}");
        scene.render_world.physics_tick = 0;
        // Broken shaders leave the image as is and say why.
        scene.render_world.post_shader = "#rusting post 2\n".into();
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain[0]);
        let error = scene.renderer.post_shader_error().unwrap_or_default();
        assert!(error.contains("version"), "{error}");
        post(&mut scene, "color = oops;");
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain[0]);
        let error = scene.renderer.post_shader_error().unwrap_or_default();
        assert!(error.contains(":3:") && error.contains("oops"), "{error}");
        scene.render_world.post_shader.clear();
        assert_eq!(render_pixel(&mut scene, [8, 16]), plain[0]);
        assert_eq!(scene.renderer.post_shader_error(), None);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn cloth_textures_land_upright_and_unmirrored() {
        use crate::runtime::cloth::{cloth_mesh, Cloth};
        use crate::runtime::ExtractedRenderable;
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A 1 m sheet facing the camera with a 2 x 2 image: red, green on
        // the top row, blue, white below.
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        let image = scene.assets.textures.insert(TextureAsset {
            size: [2, 2],
            rgba8: [
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [255, 255, 255, 255],
            ]
            .concat(),
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        let material = scene.assets.materials.insert(MaterialAsset {
            model: MaterialModel::Unlit,
            base_color_texture: Some(image),
            ..Default::default()
        });
        let cloth = Cloth::grid(
            [-0.5, -0.5, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [4, 4],
            0.25,
            0.3,
        )
        .unwrap();
        let mesh = scene.assets.meshes.insert(cloth_mesh(&cloth));
        scene.render_world.renderables = vec![ExtractedRenderable {
            entity: bevy_ecs::entity::Entity::from_raw_u32(1).unwrap(),
            transform: crate::runtime::GlobalTransform::default(),
            mesh,
            material,
            cast_shadows: false,
            receive_shadows: false,
            bounds: None,
        }];
        scene.render_world.renderables_revision += 1;
        // Pixels are 1/16 m; the quadrant centers sit 0.25 m off center.
        let quadrants = [
            ([12, 12], [true, false, false]),
            ([20, 12], [false, true, false]),
            ([12, 20], [false, false, true]),
            ([20, 20], [true; 3]),
        ];
        for (pixel, want) in quadrants {
            let got = render_pixel(&mut scene, pixel);
            assert_eq!(got.map(|c| c > 150), want, "pixel {pixel:?}: {got:?}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn terrain_paints_splat_layers_and_drops_detail_with_distance() {
        use crate::runtime::{
            Heightfield, RenderExtractPlugin, Terrain, TerrainLayer,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A flat 2 x 2 m terrain of one chunk, turned to face the camera.
        // Every sample paints the blue layer except column 1 (x = -0.5 m),
        // which paints the orange one; coarse LOD levels skip that column.
        let extract = |lod_distance: f32| {
            let mut app = crate::App::new();
            app.add_plugin(RenderExtractPlugin).unwrap();
            app.insert_resource(AssetServer::default());
            let layer = |color: [f32; 3]| TerrainLayer {
                color,
                ..TerrainLayer::default()
            };
            let splat = (0..5)
                .map(|_| {
                    (0..5)
                        .map(|column| {
                            if column == 1 {
                                [0.0, 1.0, 0.0, 0.0]
                            } else {
                                [1.0, 0.0, 0.0, 0.0]
                            }
                        })
                        .collect()
                })
                .collect();
            app.spawn((
                crate::Transform {
                    rotation: [std::f32::consts::FRAC_PI_2, 0.0, 0.0],
                    ..crate::Transform::default()
                },
                Heightfield {
                    heights: vec![vec![0.0; 5]; 5],
                    spacing: 0.5,
                    ..Heightfield::default()
                },
                Terrain {
                    layers: vec![
                        layer([0.0, 0.0, 1.0]),
                        layer([1.0, 0.5, 0.0]),
                    ],
                    splat,
                    chunk_cells: 4,
                    lod_distance,
                    cast_shadows: false,
                    ..Terrain::default()
                },
            ));
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
            let world = app.world_mut();
            let renderables =
                world.resource::<RenderWorld>().renderables.clone();
            (renderables, world.remove_resource::<AssetServer>().unwrap())
        };
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        // Pixels are 1/16 m: column 1 is pixel 8, column 3 is pixel 24.
        let orange = |[r, g, b]: [u8; 3]| r > 40 && r > g && r > b;
        let blue = |[r, g, b]: [u8; 3]| b > 40 && b > r && b > g;
        // The camera is 5 m away; no LOD distance keeps full detail.
        (scene.render_world.renderables, scene.assets) = extract(0.0);
        assert_eq!(scene.render_world.renderables.len(), 1);
        let painted = render_pixel(&mut scene, [8, 16]);
        assert!(orange(painted), "column 1 paints orange: {painted:?}");
        let painted = render_pixel(&mut scene, [24, 16]);
        assert!(blue(painted), "column 3 paints blue: {painted:?}");
        // Past 2 m the chunk draws its quarter-detail mesh, which only
        // samples columns 0 and 4.
        (scene.render_world.renderables, scene.assets) = extract(1.0);
        scene.render_world.renderables_revision += 1;
        let painted = render_pixel(&mut scene, [8, 16]);
        assert!(blue(painted), "coarse level skips column 1: {painted:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_sub_emitters_spawn_where_parent_particles_die() {
        use crate::runtime::{
            gpu_particle_batch, ColorKey, ParticleEmitter, ParticleSimulation,
            ParticleSprite, ParticleSystem,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let emitter =
            |color: [f32; 4], speed: f32, lifetime: f32| ParticleEmitter {
                simulation: ParticleSimulation::Gpu,
                max_particles: 1,
                direction: [1.0, 0.0, 0.0],
                spread: 0.0,
                speed: [speed, speed],
                size: [0.5, 0.5],
                lifetime: [lifetime, lifetime],
                fade_in: 0.0,
                fade_out: 0.0,
                color_over_life: vec![ColorKey { t: 0.0, color }],
                sprite: ParticleSprite::Square,
                ..ParticleEmitter::default()
            };
        // A still green parent particle at x = -0.5 dies after 0.05 s; its
        // sub-emitter then spawns red particles there that fly along +x at
        // 10 m/s.
        let parent = emitter([0.0, 1.0, 0.0, 1.0], 0.0, 0.05);
        let child = emitter([1.0, 0.0, 0.0, 1.0], 10.0, 5.0);
        let global = crate::runtime::GlobalTransform {
            matrix: Matrix4::new_translation(&Vector3::new(-0.5, 0.0, 0.0))
                .into(),
        };
        // The child emitter itself sits elsewhere: spawns follow the parent
        // particle, not the child's transform.
        let elsewhere = crate::runtime::GlobalTransform {
            matrix: Matrix4::new_translation(&Vector3::new(5.0, 5.0, 0.0))
                .into(),
        };
        let parent_entity = bevy_ecs::entity::Entity::from_raw_u32(1).unwrap();
        let child_entity = bevy_ecs::entity::Entity::from_raw_u32(2).unwrap();
        let mut child_batch = gpu_particle_batch(
            child_entity,
            &child,
            &ParticleSystem::default(),
            Some(&elsewhere),
            None,
        );
        child_batch.parent = Some((parent_entity, 2));
        child_batch.max_particles = 2;
        let mut scene = SlabScene::with_extent(&[], [32, 32]);
        scene.render_world.gpu_particles = vec![
            gpu_particle_batch(
                parent_entity,
                &parent,
                &ParticleSystem {
                    spawned: 1,
                    ..ParticleSystem::default()
                },
                Some(&global),
                None,
            ),
            child_batch,
        ];
        let mut frame = |elapsed: f32| {
            scene.render_world.elapsed_seconds = elapsed;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let red = |x: usize| pixels[(16 * 32 + x) * 4 + 2];
            let green = |x: usize| pixels[(16 * 32 + x) * 4 + 1];
            (green(8), red(8), red(24))
        };
        assert_eq!(frame(0.0), (255, 0, 0), "parent alive");
        assert_eq!(frame(0.1), (0, 255, 0), "parent died, children spawned");
        // 0.1 s at 10 m/s: 1 m along +x.
        assert_eq!(frame(0.2), (0, 0, 255), "children flew on");
        assert_eq!(
            frame(0.3),
            (0, 0, 0),
            "children flew off screen, none respawned"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn clustered_point_lights_each_light_their_own_spot() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A perspective camera looks down at the floor at a slant, so the
        // 49 small lights spread over many screen tiles and depth slices.
        // Each light's spot must be lit and the gaps between them dark.
        let extent = [96, 96];
        let mut scene =
            SlabScene::with_extent(&[(0.0, MaterialAsset::default())], extent);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        let eye = Point3::new(0.0, -2.5, 2.5);
        let camera = scene.render_world.active_camera.as_mut().unwrap();
        camera.transform.matrix =
            Matrix4::look_at_rh(&eye, &Point3::origin(), &Vector3::z())
                .try_inverse()
                .unwrap()
                .into();
        camera.projection = Projection::Perspective {
            vertical_fov_radians: 1.0,
            near: 0.1,
            far: 100.0,
        };
        let camera = scene.render_world.active_camera;
        let spacing = 0.4;
        let spot = |column: i32, row: i32| {
            Vector3::new(column as f32 * spacing, row as f32 * spacing, 0.05)
        };
        for column in -3..=3 {
            for row in -3..=3 {
                let index = scene.render_world.point_lights.len() as u32;
                scene.render_world.point_lights.push(
                    crate::runtime::ExtractedPointLight {
                        entity: bevy_ecs::entity::Entity::from_raw_u32(
                            3000 + index,
                        )
                        .unwrap(),
                        transform: crate::runtime::GlobalTransform {
                            matrix: Matrix4::new_translation(
                                &(spot(column, row) + Vector3::z() * 0.1),
                            )
                            .into(),
                        },
                        light: crate::runtime::PointLight {
                            color: [0.0, 1.0, 0.0],
                            intensity: 3_000.0,
                            range: 0.18,
                            shadows: false,
                            radius: 0.0,
                        },
                    },
                );
            }
        }
        scene.render_world.lights_revision += 1;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        let pixels = scene.pixels();
        let clip = view_projection(camera, extent);
        let green = |point: Vector3<f32>| {
            let clip = clip * point.push(1.0);
            let x = ((clip.x / clip.w * 0.5 + 0.5) * extent[0] as f32) as usize;
            let y = ((clip.y / clip.w * 0.5 + 0.5) * extent[1] as f32) as usize;
            pixels[(y * extent[0] as usize + x) * 4 + 1]
        };
        for column in -3..=3 {
            for row in -3..=3 {
                let lit = green(spot(column, row));
                assert!(
                    lit > 60,
                    "light {column},{row} lights its spot: {lit}"
                );
                let gap =
                    green(spot(column, row) + Vector3::new(0.2, 0.2, 0.0));
                assert!(
                    gap < 5,
                    "gap next to {column},{row} stays dark: {gap}"
                );
            }
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn quality_profiles_compare_on_one_scene() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A floor under a red shadowed sun and 600 small green point lights,
        // more than the Eco and Balanced light budgets.
        let mut scene = SlabScene::with_extent(
            &[(0.0, MaterialAsset::default())],
            [32, 32],
        );
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform::default(),
                light: crate::runtime::DirectionalLight {
                    color: [1.0, 0.0, 0.0],
                    illuminance: 20_000.0,
                    shadows: true,
                },
            },
        );
        for index in 0..600u32 {
            let (column, row) = ((index % 25) as f32, (index / 25) as f32);
            scene.render_world.point_lights.push(
                crate::runtime::ExtractedPointLight {
                    entity: bevy_ecs::entity::Entity::from_raw_u32(
                        2100 + index,
                    )
                    .unwrap(),
                    transform: crate::runtime::GlobalTransform {
                        matrix: Matrix4::new_translation(&Vector3::new(
                            -0.96 + column * 0.08,
                            -0.96 + row * 0.08,
                            0.1,
                        ))
                        .into(),
                    },
                    light: crate::runtime::PointLight {
                        color: [0.0, 1.0, 0.0],
                        intensity: 20.0,
                        range: 0.15,
                        shadows: false,
                        radius: 0.0,
                    },
                },
            );
        }
        let msaa = scene.renderer.capabilities().msaa_samples;
        let mut rows = Vec::new();
        for (quality, lights, shadow_map, samples) in [
            (QualityProfile::Eco, 256, 1024, 1),
            (QualityProfile::Balanced, 512, 2048, msaa),
            (QualityProfile::High, 601, 4096, msaa),
        ] {
            scene.render_world.quality = quality;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let mean = |channel: usize| {
                pixels.chunks(4).map(|p| f32::from(p[channel])).sum::<f32>()
                    / (pixels.len() / 4) as f32
            };
            let (red, green) = (mean(2), mean(1));
            let dropped = scene.renderer.capacity_diagnostics().dropped_lights;
            // The sun counts toward the budget too.
            assert_eq!(dropped, 601 - lights, "{quality:?}");
            assert_eq!(
                scene.renderer.shadow_framebuffer.extent(),
                [shadow_map; 2]
            );
            assert_eq!(scene.renderer.scene_samples, samples);
            eprintln!(
                "{quality:?}: {lights} lights ({dropped} dropped), \
                 {shadow_map}px shadow map, {samples}x MSAA, \
                 mean red {red:.1}, mean green {green:.1}"
            );
            rows.push((red, green));
        }
        // The sun looks the same on every profile; each higher profile keeps
        // more point lights, so more of the floor turns green.
        assert!(rows.iter().all(|(red, _)| (red - rows[0].0).abs() < 2.0));
        assert!(rows[0].0 > 50.0, "sunlit floor: {rows:?}");
        assert!(rows[0].1 < rows[1].1 && rows[1].1 < rows[2].1, "{rows:?}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn vertical_slice_lighting_combines_shadow_point_sky_and_blend() {
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // One frame with every Milestone 7 lighting feature. Each light has
        // its own channel: red directional sun, green point light, blue sky.
        // A white half-transparent pane covers the top rows.
        let pane = MaterialAsset {
            model: MaterialModel::Unlit,
            base_color: [1.0, 1.0, 1.0, 0.5],
            alpha_mode: AlphaMode::Blend,
            ..MaterialAsset::default()
        };
        let mut scene = SlabScene::with_extent(
            &[
                (0.0, MaterialAsset::default()),
                (1.0, MaterialAsset::default()),
                (1.5, pane),
            ],
            [32, 32],
        );
        let slab = |x: f32, y: f32, z: f32, width: f32, height: f32| {
            crate::runtime::GlobalTransform {
                matrix: (Matrix4::new_translation(&Vector3::new(x, y, z))
                    * Matrix4::new_nonuniform_scaling(&Vector3::new(
                        width, height, 0.1,
                    )))
                .into(),
            }
        };
        // The occluder sits outside the view; its shadow covers x > -0.5.
        scene.render_world.renderables[0].receive_shadows = true;
        scene.render_world.renderables[1].cast_shadows = true;
        scene.render_world.renderables[1].transform =
            slab(6.5, 0.0, 1.0, 4.0, 4.0);
        scene.render_world.renderables[2].transform =
            slab(0.0, 0.75, 1.5, 4.0, 0.5);
        scene.render_world.ambient_light = Some(crate::runtime::AmbientLight {
            color: [0.0; 3],
            intensity: 0.0,
        });
        scene.render_world.sky_light = Some(crate::runtime::SkyLight {
            sky_color: [0.0, 0.0, 1.0],
            ground_color: [0.0, 0.0, 1.0],
            intensity: 0.05,
        });
        let direction = Vector3::new(-5.0, 0.0, -1.0).normalize();
        scene.render_world.directional_lights.push(
            crate::runtime::ExtractedDirectionalLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2000).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: nalgebra::Rotation3::rotation_between(
                        &-Vector3::z(),
                        &direction,
                    )
                    .unwrap()
                    .to_homogeneous()
                    .into(),
                },
                light: crate::runtime::DirectionalLight {
                    color: [1.0, 0.0, 0.0],
                    illuminance: 500_000.0,
                    shadows: true,
                },
            },
        );
        scene.render_world.point_lights.push(
            crate::runtime::ExtractedPointLight {
                entity: bevy_ecs::entity::Entity::from_raw_u32(2001).unwrap(),
                transform: crate::runtime::GlobalTransform {
                    matrix: Matrix4::new_translation(&Vector3::new(
                        0.5, -0.5, 1.0,
                    ))
                    .into(),
                },
                light: crate::runtime::PointLight {
                    color: [0.0, 1.0, 0.0],
                    intensity: 500.0,
                    range: 1.5,
                    shadows: false,
                    radius: 0.0,
                },
            },
        );
        // Pixels as [r, g, b]: sunlit floor, shadowed floor near the point
        // light, and sunlit floor behind the pane.
        let frame = |scene: &mut SlabScene| {
            scene.render_world.renderables_revision += 1;
            scene.render_world.lights_revision += 1;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            let pixels = scene.pixels();
            let at = |column: usize, row: usize| {
                let index = (row * 32 + column) * 4;
                [pixels[index + 2], pixels[index + 1], pixels[index]]
            };
            [at(4, 24), at(24, 24), at(4, 4)]
        };

        let [sunlit, shadowed, behind_pane] = frame(&mut scene);
        assert!(sunlit[0] > 150 && sunlit[1] < 5, "sunlit {sunlit:?}");
        assert!(shadowed[0] < 30, "shadowed {shadowed:?}");
        assert!(shadowed[1] > 20, "point light {shadowed:?}");
        assert!(shadowed[2] > 10 && sunlit[2] > 10, "sky {shadowed:?}");
        // The pane adds white; the red floor still shows through it.
        assert!(behind_pane[1] > 60, "pane {behind_pane:?}");
        assert!(behind_pane[0] > behind_pane[1] + 20, "{behind_pane:?}");

        // Turning each feature off removes only its own contribution.
        scene.render_world.directional_lights[0].light.shadows = false;
        let [_, unshadowed, _] = frame(&mut scene);
        assert!(unshadowed[0] > 150, "without shadows {unshadowed:?}");
        scene.render_world.directional_lights[0].light.shadows = true;

        let point = scene.render_world.point_lights.pop().unwrap();
        let [_, dark, _] = frame(&mut scene);
        assert!(dark[1] < 5 && dark[2] > 10, "without point {dark:?}");
        scene.render_world.point_lights.push(point);

        scene.render_world.sky_light = None;
        let [no_sky, no_sky_shadow, _] = frame(&mut scene);
        assert!(
            no_sky[2] < 5 && no_sky_shadow[2] < 5,
            "without sky {no_sky:?}"
        );

        scene.render_world.renderables.pop();
        let [_, _, bare] = frame(&mut scene);
        assert!(bare[1] < 5, "without pane {bare:?}");
    }

    #[test]
    fn hybrid_physics_gpu_layouts_match_shader_structs() {
        use std::mem::{offset_of, size_of};

        type ReflectedBody = super::physics_shader::PhysicsState;
        assert_eq!(size_of::<GpuBodyState>(), size_of::<ReflectedBody>());
        assert_eq!(
            offset_of!(GpuBodyState, metadata),
            offset_of!(ReflectedBody, metadata)
        );

        assert_eq!(
            size_of::<GpuCommandUpload>(),
            size_of::<super::physics_shader::BodyCommand>()
        );
        assert_eq!(
            size_of::<GpuColliderNode>(),
            size_of::<super::physics_shader::ColliderNode>()
        );
        assert_eq!(
            offset_of!(GpuColliderNode, links),
            offset_of!(super::physics_shader::ColliderNode, links)
        );
        assert!(include_str!("../shaders/physics_abi.glsl").contains(
            &format!(
                "#define RUSTING_PHYSICS_ABI_VERSION {}\n",
                crate::runtime::GPU_PHYSICS_ABI_VERSION
            )
        ));
        assert_eq!(
            size_of::<GpuConditionUpload>(),
            size_of::<super::physics_shader::ConditionInstruction>()
        );
        assert_eq!(
            size_of::<GpuRuleState>(),
            size_of::<super::physics_shader::RuleState>()
        );
        assert_eq!(
            size_of::<GpuEventUpload>(),
            size_of::<super::physics_shader::PhysicsEvent>()
        );
        assert_eq!(
            size_of::<PhysicsPushConstants>(),
            size_of::<super::physics_shader::PhysicsPush>()
        );

        // `EventHeader` is a bare buffer block (no named GLSL struct), so
        // there is no generated reflected type to compare against — its
        // hand-computed layout is checked directly instead.
        assert_eq!(size_of::<GpuEventHeader>(), 32);
        assert_eq!(std::mem::offset_of!(GpuEventHeader, contacts), 16);
    }

    #[test]
    fn normal_matrix_ignores_translation_and_handles_scale() {
        let model =
            Matrix4::new_translation(&nalgebra::Vector3::new(2.0, 3.0, 4.0))
                * Matrix4::new_nonuniform_scaling(&nalgebra::Vector3::new(
                    2.0, 4.0, 5.0,
                ));
        assert_eq!(
            normal_columns(model),
            [
                [0.5, 0.0, 0.0, 0.0],
                [0.0, 0.25, 0.0, 0.0],
                [0.0, 0.0, 0.2, 0.0],
            ]
        );
    }

    /// CPU cost of a crowd with a two-level LOD group seen by four cameras
    /// a frame, like forever-bear's CCTV feeds (F49). Prints milliseconds;
    /// the numbers only mean something on real hardware.
    #[test]
    #[ignore = "timing on real hardware: cargo test --release --features gpu-tests lod_crowd_cpu_cost -- --ignored --nocapture"]
    fn lod_crowd_cpu_cost() {
        if vulkano::VulkanLibrary::new().is_err() {
            return;
        }
        let material = MaterialAsset {
            model: MaterialModel::Unlit,
            ..MaterialAsset::default()
        };
        for lod in [false, true] {
            let mut scene = SlabScene::new(&[(0.0, material.clone())]);
            let template = scene.render_world.renderables[0];
            scene.render_world.renderables = (0..2085_u32)
                .map(|index| crate::runtime::ExtractedRenderable {
                    entity: bevy_ecs::entity::Entity::from_raw_u32(index + 1)
                        .unwrap(),
                    transform: crate::runtime::GlobalTransform {
                        matrix: Matrix4::new_translation(&Vector3::new(
                            (index % 50) as f32 - 25.0,
                            0.0,
                            -((index / 50) as f32),
                        ))
                        .into(),
                    },
                    ..template
                })
                .collect();
            if lod {
                let base = scene.assets.fallback_mesh;
                let coarse = scene
                    .assets
                    .meshes
                    .insert(scene.assets.meshes.get(base).unwrap().clone());
                scene.assets.lod_groups.insert(LodGroupAsset {
                    metric: LodMetric::Distance,
                    levels: vec![
                        crate::assets::LodLevel {
                            mesh: base,
                            until: 9.0,
                        },
                        crate::assets::LodLevel {
                            mesh: coarse,
                            until: f32::INFINITY,
                        },
                    ],
                });
            }
            let mut total = Duration::ZERO;
            let frames = 400;
            for frame in 0..frames + 20 {
                scene.render_world.active_camera.as_mut().unwrap().transform =
                    crate::runtime::GlobalTransform {
                        matrix: Matrix4::new_translation(&Vector3::new(
                            (frame % 4) as f32 * 3.0,
                            1.0,
                            5.0,
                        ))
                        .into(),
                    };
                // Every object sways, so instances patch each frame.
                let sway = (frame % 2) as f32 * 0.01;
                for renderable in &mut scene.render_world.renderables {
                    renderable.transform.matrix[3][1] = sway;
                }
                scene.render_world.renderables_revision += 1;
                scene.render_world.renderables_layout_revision = 1;
                let before = scene.now();
                let start = std::time::Instant::now();
                let future = scene.render(before);
                if frame >= 20 {
                    total += start.elapsed();
                }
                future
                    .then_signal_fence_and_flush()
                    .unwrap()
                    .wait(None)
                    .unwrap();
            }
            eprintln!(
                "lod {lod}: {:.3} ms CPU per frame, {} instances",
                total.as_secs_f64() * 1000.0 / f64::from(frames),
                scene.renderer.culling_stats().submitted
            );
        }
    }

    /// The 100K pile: `PhysicsBenchmark::Debris` with 100 000 cubes on the
    /// GPU for 5 s. The score is how far cube corners ended below the
    /// ground; it and the mean wall time of a tick are printed.
    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_100k_pile_lands_on_the_gpu_without_sinking() {
        use crate::runtime::{
            Collider, CollisionLayers, ExtractedGpuPhysicsBody, GpuCollider,
            PhysicsBenchmark, PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        const BODIES: u32 = 100_000;
        const TICKS: u64 = 300;
        let mut slab = SlabScene::new(&[]);
        let world = &mut slab.render_world;
        world.gpu_physics = PhysicsBenchmark::Debris
            .bodies(BODIES as usize)
            .into_iter()
            .zip(0..)
            .map(|(body, slot)| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(5000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: body.transform,
                rigid_body: crate::runtime::RigidBody {
                    linear_velocity: body.linear_velocity,
                    ..Default::default()
                },
                solver: body.solver,
                collider: Some((
                    Collider::default(),
                    CollisionLayers::default(),
                )),
                custom_shader: None,
                rules: Vec::new(),
                sync: Default::default(),
            })
            .collect();
        world.gpu_colliders = vec![GpuCollider {
            model: crate::Transform::new([0.0, -0.5, 0.0]).to_matrix(),
            shape: [0.0, 500.0, 0.5, 500.0],
            velocity: [0.0; 3],
            friction: 0.5,
            restitution: 0.0,
            layers: CollisionLayers::default(),
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let start = std::time::Instant::now();
        for tick in 1..=TICKS {
            let world = &mut slab.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == TICKS;
            world.gpu_physics_commands_serial += u64::from(tick == TICKS);
            let before = slab.now();
            let _in_flight =
                slab.render(before).then_signal_fence_and_flush().unwrap();
            slab.renderer.block_until_physics_readbacks_complete();
        }
        let tick_time = start.elapsed() / TICKS as u32;
        let states = slab.renderer.take_completed_physics_states();
        assert_eq!(states.len(), BODIES as usize);
        // How far each cube's lowest corner went below the ground's top.
        let depths: Vec<f32> = states
            .iter()
            .map(|state| {
                let [x, y, z] = state.transform.rotation;
                let rotation =
                    crate::runtime::sim_math::rotation_from_euler(x, y, z);
                let reach = (0..3)
                    .map(|row| 0.5 * rotation.matrix().row(1)[row].abs())
                    .sum::<f32>();
                reach - state.transform.position[1]
            })
            .collect();
        assert!(depths.iter().all(|depth| depth.is_finite()));
        let deepest = depths.iter().copied().fold(f32::MIN, f32::max);
        let sunk = depths.iter().filter(|&&depth| depth > 0.05).count();
        let diagnostics = slab.renderer.capacity_diagnostics();
        eprintln!(
            "100K pile: {tick_time:?} per tick, deepest {deepest} m, {sunk} \
             deeper than 5 cm, {} grid overflow",
            diagnostics.physics_grid_overflow
        );
        // Sleeping cubes at the bottom are pushed out of the ground too;
        // before that the pile pressed 86 of them up to 0.41 m in.
        assert!(sunk == 0 && deepest < 0.01, "{deepest} {diagnostics:?}");
    }

    /// Twenty unit cubes dropped 1 cm apart, each 2 cm off the one below in
    /// turn along x and z, onto a GPU ground collider must settle into a
    /// standing tower and sleep.
    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_twenty_cube_tower_stands_and_sleeps_on_the_gpu() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            GpuCollider, PhysicsId, PhysicsSolver,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        const LEVELS: u32 = 20;
        const TICKS: u64 = 300;
        let unit_box = Collider {
            shape: ColliderShape::Box {
                half_extents: [0.5; 3],
            },
            ..Collider::default()
        };
        let offset = |level: u32| {
            let side = if level % 4 < 2 { 0.02 } else { -0.02 };
            let (x, z) = if level.is_multiple_of(2) {
                (side, 0.0)
            } else {
                (0.0, side)
            };
            [x, 0.5 + level as f32 * 1.01, z]
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = (0..LEVELS)
            .map(|slot| ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(4000 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new(offset(slot)),
                rigid_body: crate::runtime::RigidBody::default(),
                solver: PhysicsSolver::Full,
                collider: Some((unit_box, CollisionLayers::default())),
                custom_shader: None,
                rules: Vec::new(),
                sync: Default::default(),
            })
            .collect();
        world.gpu_colliders = vec![GpuCollider {
            model: crate::Transform::new([0.0, -0.5, 0.0]).to_matrix(),
            shape: [0.0, 10.0, 0.5, 10.0],
            velocity: [0.0; 3],
            friction: 0.5,
            restitution: 0.0,
            layers: CollisionLayers::default(),
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0, -9.81, 0.0];
        world.fixed_delta_seconds = 1.0 / 60.0;
        for tick in 1..=TICKS {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == TICKS;
            world.gpu_physics_commands_serial += u64::from(tick == TICKS);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
        }
        let mut states = scene.renderer.take_completed_physics_states();
        states.sort_by_key(|state| state.physics_id.slot);
        assert_eq!(states.len(), LEVELS as usize);
        for (level, state) in states.iter().enumerate() {
            let [x, y, z] = state.transform.position;
            let [x0, _, z0] = offset(level as u32);
            let height = 0.5 + level as f32;
            assert!(
                (x - x0).hypot(z - z0) < 0.05 && (y - height).abs() < 0.03,
                "cube {level}: {state:?}"
            );
            assert!(
                state.transform.rotation.iter().all(|a| a.abs() < 0.02),
                "cube {level} tilts: {state:?}"
            );
            assert_eq!(state.linear_velocity, [0.0; 3], "cube {level} sleeps");
        }
    }

    /// Fast GPU bodies meet a 10 cm wall collider on their way, not only
    /// where their step ends.
    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn fast_gpu_bodies_do_not_tunnel_through_thin_colliders() {
        use crate::runtime::{
            Collider, ColliderShape, CollisionLayers, ExtractedGpuPhysicsBody,
            GpuCollider, PhysicsId, PhysicsSolver,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        // A 0.1 m ball at 270 m/s moves 4.5 m a tick, a 0.1 m cube at
        // 100 m/s 1.7 m; the wall's front face is at z = -4.95.
        let bullet = |slot: u32, x: f32, shape: ColliderShape, speed: f32| {
            ExtractedGpuPhysicsBody {
                entity: bevy_ecs::entity::Entity::from_raw_u32(4100 + slot)
                    .unwrap(),
                physics_id: PhysicsId {
                    slot,
                    generation: 0,
                },
                transform: crate::Transform::new([x, 0.0, 0.0]),
                rigid_body: crate::runtime::RigidBody {
                    linear_velocity: [0.0, 0.0, -speed],
                    ..Default::default()
                },
                solver: PhysicsSolver::Full,
                collider: Some((
                    Collider {
                        shape,
                        ..Collider::default()
                    },
                    CollisionLayers::default(),
                )),
                custom_shader: None,
                rules: Vec::new(),
                sync: Default::default(),
            }
        };
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = vec![
            bullet(0, -1.0, ColliderShape::Sphere { radius: 0.1 }, 270.0),
            bullet(
                1,
                1.0,
                ColliderShape::Box {
                    half_extents: [0.1; 3],
                },
                100.0,
            ),
        ];
        world.gpu_colliders = vec![GpuCollider {
            model: crate::Transform::new([0.0, 0.0, -5.0]).to_matrix(),
            shape: [0.0, 4.0, 4.0, 0.05],
            velocity: [0.0; 3],
            friction: 0.5,
            restitution: 0.0,
            layers: CollisionLayers::default(),
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        const TICKS: u64 = 10;
        for tick in 1..=TICKS {
            let world = &mut scene.render_world;
            world.physics_tick = tick;
            world.gpu_physics_read_all = tick == TICKS;
            world.gpu_physics_commands_serial += u64::from(tick == TICKS);
            let before = scene.now();
            let _in_flight =
                scene.render(before).then_signal_fence_and_flush().unwrap();
            scene.renderer.block_until_physics_readbacks_complete();
        }
        let mut states = scene.renderer.take_completed_physics_states();
        states.sort_by_key(|state| state.physics_id.slot);
        assert_eq!(states.len(), 2);
        for state in &states {
            let z = state.transform.position[2];
            // Each stops against the wall's front face.
            assert!((-4.86..=-4.84).contains(&z), "{state:?}");
            assert!(state.linear_velocity[2] > -0.1, "{state:?}");
        }
    }

    /// Each event mode fires on its own ticks for a body crossing a range.
    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn built_in_conditions_fire_by_mode_and_cooldown() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, ExtractedGpuPhysicsRule, GpuCondition,
            GpuEventId, GpuEventMode, GpuEventPayload, GpuStateField,
            PhysicsId,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let x = || GpuCondition::field(GpuStateField::PositionX);
        // The bodies move 0.5 m a tick from x = 0, so x is in 0.9..=2.1 on
        // ticks 2, 3 and 4. The range and the boolean form agree.
        let range = || x().inside(0.9, 2.1);
        let both = || x().greater_than(0.9).and(x().less_than(2.1));
        let rules = [
            (range(), GpuEventMode::OnEnter, 0.0),
            (both(), GpuEventMode::OnExit, 0.0),
            (range(), GpuEventMode::WhileTrue, 0.0),
            (both(), GpuEventMode::Once, 0.0),
            // 1.5 ticks: every other tick while true.
            (range(), GpuEventMode::WhileTrue, 1.5 / 60.0),
        ];
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        world.gpu_physics = rules
            .into_iter()
            .zip(0..)
            .map(|((condition, mode, cooldown_seconds), slot)| {
                ExtractedGpuPhysicsBody {
                    entity: bevy_ecs::entity::Entity::from_raw_u32(3000 + slot)
                        .unwrap(),
                    physics_id: PhysicsId {
                        slot,
                        generation: 0,
                    },
                    transform: crate::Transform::new([
                        0.0,
                        0.0,
                        slot as f32 * 2.0,
                    ]),
                    rigid_body: crate::runtime::RigidBody {
                        linear_velocity: [30.0, 0.0, 0.0],
                        ..Default::default()
                    },
                    solver: Default::default(),
                    collider: None,
                    custom_shader: None,
                    rules: vec![ExtractedGpuPhysicsRule {
                        event_id: GpuEventId(10 + slot),
                        instructions: condition.compile().unwrap(),
                        mode,
                        payload: GpuEventPayload::None,
                        cooldown_seconds,
                    }],
                    sync: Default::default(),
                }
            })
            .collect();
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        let mut fired = vec![Vec::new(); 5];
        for tick in 1..=8 {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            scene
                .render(before)
                .then_signal_fence_and_flush()
                .unwrap()
                .wait(None)
                .unwrap();
            for event in scene.renderer.take_completed_physics_events() {
                assert_eq!(event.event_id, 10 + event.body_slot);
                fired[event.body_slot as usize].push(event.tick_low);
            }
        }
        assert_eq!(
            fired,
            [vec![2], vec![5], vec![2, 3, 4], vec![2], vec![2, 4]]
        );
    }

    /// State read back while frames overlap carries the tick it was taken
    /// on, and the counters report how many frames it waited.
    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn overlapped_state_readback_reports_its_tick_and_frame_age() {
        use crate::runtime::{
            ExtractedGpuPhysicsBody, PhysicsId, PhysicsSyncMode,
        };
        if vulkano::VulkanLibrary::new().is_err() {
            eprintln!("skipping: no Vulkan driver present");
            return;
        }
        let mut scene = SlabScene::new(&[]);
        let world = &mut scene.render_world;
        // 6 m/s is 0.1 m a tick.
        world.gpu_physics = vec![ExtractedGpuPhysicsBody {
            entity: bevy_ecs::entity::Entity::from_raw_u32(3000).unwrap(),
            physics_id: PhysicsId {
                slot: 0,
                generation: 0,
            },
            transform: crate::Transform::default(),
            rigid_body: crate::runtime::RigidBody {
                linear_velocity: [6.0, 0.0, 0.0],
                ..Default::default()
            },
            solver: Default::default(),
            collider: None,
            custom_shader: None,
            rules: Vec::new(),
            sync: PhysicsSyncMode::SelectedState,
        }];
        world.gpu_physics_revision = 1;
        world.physics_enabled = true;
        world.physics_gravity = [0.0; 3];
        world.fixed_delta_seconds = 1.0 / 60.0;
        // Three frames go out before any is waited on.
        let mut in_flight = Vec::new();
        for tick in 1..=3 {
            scene.render_world.physics_tick = tick;
            let before = scene.now();
            in_flight.push(
                scene.render(before).then_signal_fence_and_flush().unwrap(),
            );
        }
        scene.renderer.block_until_physics_readbacks_complete();
        let states = scene.renderer.take_completed_physics_states();
        let mut ticks =
            states.iter().map(|state| state.tick).collect::<Vec<_>>();
        ticks.sort_unstable();
        assert_eq!(ticks, [1, 2, 3]);
        for state in &states {
            let x = state.transform.position[0];
            assert!((x - 0.1 * state.tick as f32).abs() < 1e-5, "{state:?}");
        }
        drop(in_flight);
        // The next frame's counters show the oldest waited two frames
        // past its own.
        scene.render_world.physics_tick = 4;
        let before = scene.now();
        scene
            .render(before)
            .then_signal_fence_and_flush()
            .unwrap()
            .wait(None)
            .unwrap();
        assert_eq!(
            scene
                .renderer
                .render_counters()
                .physics_readback_latency_frames,
            2
        );
    }
}
