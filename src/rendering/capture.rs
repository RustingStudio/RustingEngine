//! Offscreen frames for tools: `rusting capture` and scenario tests.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use vulkano::command_buffer::allocator::StandardCommandBufferAllocator;
use vulkano::image::view::ImageView;
use vulkano::image::{Image, ImageCreateInfo, ImageUsage};
use vulkano::memory::allocator::{
    AllocationCreateInfo, StandardMemoryAllocator,
};
use vulkano::sync::GpuFuture;

use super::render_scale::render_game;
use super::scene_renderer::{resolve_quality, SceneRenderer};
use super::swapchain::OFFSCREEN_COLOR_FORMAT;
use super::HeadlessVulkanBase;
use crate::runtime::{
    apply_gpu_state_samples, record_gpu_state_hashes, route_gpu_physics_events,
    EventQueue, GpuPhysicsEventsLost, RenderSettings, RenderWorld,
};
use crate::{App, AssetServer};

/// Renders every app update into one offscreen image, as the game window
/// would, so GPU physics and its readback advance the same way. With the
/// `ui` feature the runtime UI (HUD) is painted over the frame at one pixel
/// per point.
pub struct HeadlessCapture {
    base: HeadlessVulkanBase,
    allocator: Arc<StandardMemoryAllocator>,
    renderer: SceneRenderer,
    image: Arc<Image>,
    target: Arc<ImageView>,
    extent: [u32; 2],
    scaled: super::render_scale::ScaledTarget,
    /// Each drawn camera of the last frame with a split view: its work
    /// counters and GPU time. Empty with one full-window camera.
    views: Vec<(
        Option<bevy_ecs::entity::Entity>,
        super::scene_renderer::RenderCounters,
        Duration,
    )>,
    /// Second target for [`Self::view_rgba`], made on first use.
    alt: Option<(Arc<Image>, Arc<ImageView>)>,
    /// GPU milliseconds of every frame [`Self::frame`] drew.
    gpu_frames: Vec<f64>,
    /// Steps `GpuCloth` volumes; made on first use.
    cloth: Option<super::gpu_cloth::GpuClothRunner>,
    #[cfg(feature = "ui")]
    ui: Option<super::egui_painter::EguiPainter>,
    /// A UNORM view of [`Self::image`]: the UI blends in gamma space, as
    /// over the window's UNORM swapchain.
    #[cfg(feature = "ui")]
    ui_target: Option<Arc<ImageView>>,
    /// Text the UI painted over the last frame.
    #[cfg(feature = "ui")]
    texts: Vec<TextSpot>,
}

/// A run of UI text painted over a frame: its box in pixels as `[min x,
/// min y, max x, max y]`, its sRGB colour with alpha, its font size, and
/// whether a filled shape painted before it (a button, a caption box) lies
/// under its centre.
#[cfg(feature = "ui")]
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpot {
    pub text: String,
    pub rect: [f32; 4],
    pub color: [f32; 4],
    pub size: f32,
    pub backed: bool,
}

/// Every text shape in `shapes`, in paint order, scaled to pixels.
#[cfg(feature = "ui")]
fn text_spots(
    shapes: &[egui::epaint::ClippedShape],
    pixels_per_point: f32,
) -> Vec<TextSpot> {
    fn walk(
        shape: &egui::Shape,
        scale: f32,
        fills: &mut Vec<egui::Rect>,
        spots: &mut Vec<TextSpot>,
    ) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    walk(shape, scale, fills, spots);
                }
            }
            egui::Shape::Rect(rect) if rect.fill.a() > 0 => {
                fills.push(rect.rect);
            }
            egui::Shape::Text(text)
                if !text.galley.job.text.trim().is_empty() =>
            {
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                let format =
                    text.galley.job.sections.first().map(|s| &s.format);
                let color = text
                    .override_text_color
                    .or(format
                        .map(|format| format.color)
                        .filter(|color| *color != egui::Color32::PLACEHOLDER))
                    .unwrap_or(text.fallback_color)
                    .to_srgba_unmultiplied()
                    .map(|c| f32::from(c) / 255.0);
                spots.push(TextSpot {
                    text: text.galley.job.text.clone(),
                    rect: [
                        rect.min.x * scale,
                        rect.min.y * scale,
                        rect.max.x * scale,
                        rect.max.y * scale,
                    ],
                    color: [
                        color[0],
                        color[1],
                        color[2],
                        color[3] * text.opacity_factor,
                    ],
                    size: format.map_or(0.0, |format| format.font_id.size)
                        * scale,
                    backed: fills
                        .iter()
                        .any(|fill| fill.contains(rect.center())),
                });
            }
            _ => {}
        }
    }
    let (mut fills, mut spots) = (Vec::new(), Vec::new());
    for clipped in shapes {
        walk(&clipped.shape, pixels_per_point, &mut fills, &mut spots);
    }
    spots
}

impl HeadlessCapture {
    /// Opens a headless Vulkan device. Fails without printing when there is
    /// no usable device.
    pub fn new(extent: [u32; 2]) -> Result<Self, String> {
        let base = super::try_init_vulkan_headless()?;
        let allocator =
            Arc::new(StandardMemoryAllocator::new_default(base.device.clone()));
        let renderer = SceneRenderer::new(
            base.queue.clone(),
            allocator.clone(),
            OFFSCREEN_COLOR_FORMAT,
            extent,
        )
        .map_err(|error| format!("scene renderer: {error}"))?;
        let (image, target) = color_target(&allocator, extent)?;
        let scaled = super::render_scale::ScaledTarget::new(allocator.clone());
        Ok(Self {
            base,
            allocator,
            renderer,
            image,
            target,
            extent,
            scaled,
            views: Vec::new(),
            alt: None,
            gpu_frames: Vec::new(),
            cloth: None,
            #[cfg(feature = "ui")]
            ui: None,
            #[cfg(feature = "ui")]
            ui_target: None,
            #[cfg(feature = "ui")]
            texts: Vec::new(),
        })
    }

    /// Renders the last update again into a second image, without the UI:
    /// through `camera` over the whole frame, or as the game view when
    /// `None`. The frame [`Self::rgba`] reads stays as it was.
    pub fn view_rgba(
        &mut self,
        world: &bevy_ecs::world::World,
        camera: Option<bevy_ecs::entity::Entity>,
    ) -> Result<Vec<u8>, String> {
        if self.alt.is_none() {
            self.alt = Some(color_target(&self.allocator, self.extent)?);
        }
        let (image, view) = self.alt.clone().unwrap();
        let render_world = world.resource::<RenderWorld>();
        let assets = world.resource::<AssetServer>();
        let before = vulkano::sync::now(self.base.device.clone()).boxed();
        let frame = match camera {
            Some(entity) => {
                let camera = world
                    .get::<crate::runtime::Camera>(entity)
                    .ok_or("not a camera")?;
                let transform = world
                    .get::<crate::runtime::GlobalTransform>(entity)
                    .ok_or("camera has no transform")?;
                let options = super::scene_renderer::SceneRenderOptions {
                    camera: Some(crate::runtime::ExtractedCamera {
                        entity,
                        transform: *transform,
                        projection: camera.projection,
                        priority: camera.priority,
                    }),
                    ..super::scene_renderer::SceneRenderOptions::game(
                        self.extent,
                    )
                };
                self.renderer
                    .render(
                        before,
                        view,
                        self.extent,
                        options,
                        render_world,
                        assets,
                    )
                    .map_err(|error| format!("render: {error}"))?
            }
            None => render_game(
                &mut self.renderer,
                Some(&mut self.scaled),
                before,
                view,
                world.resource::<RenderSettings>(),
                render_world,
                assets,
                |_, future, _| Ok(future),
            )?,
        };
        frame
            .then_signal_fence_and_flush()
            .map_err(|error| format!("submit: {error}"))?
            .wait(None)
            .map_err(|error| format!("wait: {error}"))?;
        Ok(self.read(&image))
    }

    /// Updates `app` by `delta` without rendering, for ticks no image step
    /// looks at. GPU physics does not advance on such a tick.
    pub fn update(
        &mut self,
        app: &mut App,
        delta: Duration,
    ) -> Result<(), String> {
        // The UI pass and pointer picking read the view size; a headless
        // view is the capture.
        let mut input = app
            .world_mut()
            .resource_mut::<crate::runtime::RuntimeInput>();
        if input.viewport_size().contains(&0.0) {
            input.record_viewport_size(self.extent.map(|side| side as f32));
        }
        app.update(delta)
            .map(drop)
            .map_err(|error| format!("update: {error}"))
    }

    /// Like [`Self::frame`], but only advances GPU physics: nothing is
    /// drawn and the tick does not count in `gpu_frames`. GPU state hashes
    /// match the ones drawn frames give.
    pub fn step_physics(
        &mut self,
        app: &mut App,
        delta: Duration,
    ) -> Result<(), String> {
        self.deliver_physics(app);
        self.update(app, delta)?;
        let world = app.world();
        self.renderer
            .step_physics(
                vulkano::sync::now(self.base.device.clone()).boxed(),
                self.target.clone(),
                self.extent,
                world.resource::<RenderWorld>(),
                world.resource::<AssetServer>(),
            )
            .map_err(|error| format!("physics: {error}"))?
            .then_signal_fence_and_flush()
            .map_err(|error| format!("submit: {error}"))?
            .wait(None)
            .map_err(|error| format!("wait: {error}"))
    }

    /// Delivers completed GPU physics readback, updates `app` by `delta`,
    /// and renders the frame, waiting for it to finish.
    pub fn frame(
        &mut self,
        app: &mut App,
        delta: Duration,
    ) -> Result<(), String> {
        self.deliver_physics(app);
        self.update(app, delta)?;
        self.draw(app)
    }

    /// Hands completed GPU physics readback to the world.
    fn deliver_physics(&mut self, app: &mut App) {
        let events = self.renderer.take_completed_physics_events();
        let states = self.renderer.take_completed_physics_states();
        let hashes = self.renderer.take_completed_physics_state_hashes();
        let lost = self.renderer.take_physics_events_lost();
        let world = app.world_mut();
        world.insert_resource(self.renderer.capacity_diagnostics());
        if lost > 0 {
            world
                .resource_mut::<EventQueue<GpuPhysicsEventsLost>>()
                .send(GpuPhysicsEventsLost { count: lost });
        }
        if !events.is_empty() {
            route_gpu_physics_events(world, &events);
        }
        if !states.is_empty() {
            apply_gpu_state_samples(world, &states);
        }
        record_gpu_state_hashes(world, &hashes);
        if self.cloth.is_none() {
            self.cloth = super::gpu_cloth::GpuClothRunner::new(
                self.base.device.clone(),
                self.base.queue.clone(),
            )
            .map_err(|error| eprintln!("GPU cloth: {error}"))
            .ok();
        }
        if let Some(runner) = &mut self.cloth {
            if let Err(error) =
                super::gpu_cloth::service_gpu_cloths(world, runner)
            {
                eprintln!("GPU cloth: {error}");
            }
        }
    }

    fn draw(&mut self, app: &mut App) -> Result<(), String> {
        let world = app.world();
        let render_world = world.resource::<RenderWorld>();
        let split = !render_world.views.is_empty();
        let device = self.base.device.clone();
        self.views.clear();
        let views = &mut self.views;
        let frame = render_game(
            &mut self.renderer,
            Some(&mut self.scaled),
            vulkano::sync::now(device.clone()).boxed(),
            self.target.clone(),
            world.resource::<RenderSettings>(),
            render_world,
            world.resource::<AssetServer>(),
            |renderer, future, camera| {
                if !split {
                    return Ok(future);
                }
                // Each camera's own numbers need its work finished.
                future
                    .then_signal_fence_and_flush()
                    .map_err(|error| format!("submit: {error}"))?
                    .wait(None)
                    .map_err(|error| format!("wait: {error}"))?;
                let counters = renderer.render_counters();
                let gpu = renderer.gpu_pass_times().total();
                views.push((camera, counters, gpu));
                Ok(vulkano::sync::now(device.clone()).boxed())
            },
        )?;
        #[cfg(feature = "ui")]
        let frame = self.paint_ui(app, frame)?;
        frame
            .then_signal_fence_and_flush()
            .map_err(|error| format!("submit: {error}"))?
            .wait(None)
            .map_err(|error| format!("wait: {error}"))?;
        let gpu = self.gpu_time().as_secs_f64() * 1000.0;
        self.gpu_frames.push(gpu);
        Ok(())
    }

    /// GPU time of the last frame, summed over split-view cameras.
    fn gpu_time(&mut self) -> Duration {
        match self.views.is_empty() {
            true => self.renderer.gpu_pass_times().total(),
            false => self.views.iter().map(|(_, _, gpu)| *gpu).sum(),
        }
    }

    /// Paints the runtime UI output of the last update over the frame.
    #[cfg(feature = "ui")]
    fn paint_ui(
        &mut self,
        app: &mut App,
        frame: Box<dyn GpuFuture>,
    ) -> Result<Box<dyn GpuFuture>, String> {
        self.texts.clear();
        let Some(output) = app
            .world_mut()
            .get_resource_mut::<crate::runtime::RuntimeUi>()
            .and_then(|mut ui| ui.take_output())
        else {
            return Ok(frame);
        };
        self.texts = text_spots(&output.shapes, output.pixels_per_point);
        let primitives = app
            .world()
            .resource::<crate::runtime::RuntimeUi>()
            .context()
            .tessellate(output.shapes, output.pixels_per_point);
        let gamma = vulkano::format::Format::B8G8R8A8_UNORM;
        let target = match &self.ui_target {
            Some(view) => view.clone(),
            None => self
                .ui_target
                .insert(
                    ImageView::new(
                        self.image.clone(),
                        vulkano::image::view::ImageViewCreateInfo {
                            format: gamma,
                            ..vulkano::image::view::ImageViewCreateInfo::from_image(
                                &self.image,
                            )
                        },
                    )
                    .map_err(|error| format!("UI target view: {error}"))?,
                )
                .clone(),
        };
        let painter = match &mut self.ui {
            Some(painter) => painter,
            None => self.ui.insert(
                super::egui_painter::EguiPainter::new(
                    self.base.queue.clone(),
                    self.allocator.clone(),
                    gamma,
                )
                .map_err(|error| format!("UI painter: {error}"))?,
            ),
        };
        painter
            .paint(
                frame,
                target,
                output.pixels_per_point,
                &primitives,
                &output.textures_delta,
            )
            .map_err(|error| format!("UI paint: {error}"))
    }

    /// Text the runtime UI painted over the last frame.
    #[cfg(feature = "ui")]
    #[must_use]
    pub fn texts(&self) -> &[TextSpot] {
        &self.texts
    }

    /// The last rendered frame as tightly packed RGBA8 sRGB rows.
    #[must_use]
    pub fn rgba(&self) -> Vec<u8> {
        self.read(&self.image)
    }

    fn read(&self, image: &Arc<Image>) -> Vec<u8> {
        let command_allocator = Arc::new(StandardCommandBufferAllocator::new(
            self.base.device.clone(),
            Default::default(),
        ));
        let mut pixels = super::readback::read_back_image(
            &self.base.device,
            &self.base.queue,
            &self.allocator,
            &command_allocator,
            image,
        );
        // The offscreen format is BGRA.
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        pixels
    }

    /// Device, work counters and GPU time of the last frame, also per
    /// camera when viewport cameras split it and per camera screen drawn.
    #[must_use]
    pub fn metadata(&mut self, app: &App) -> Value {
        let gpu = self.gpu_time();
        let properties = self.base.device.physical_device().properties();
        let counters = self.renderer.render_counters();
        let capacity = self.renderer.capacity_diagnostics();
        let requested = app.world().resource::<RenderSettings>().quality;
        let cameras: Vec<_> = self
            .views
            .iter()
            .map(|(camera, counters, gpu)| {
                let name = camera
                    .and_then(|camera| {
                        app.world().get::<crate::runtime::Name>(camera)
                    })
                    .map(|name| name.0.clone());
                json!({
                    "name": name,
                    "gpu_ms": gpu.as_secs_f64() * 1000.0,
                    "draws": counters.draws,
                    "triangles": counters.triangles,
                })
            })
            .collect();
        let mut cameras = cameras;
        let name = |camera| {
            app.world()
                .get::<crate::runtime::Name>(camera)
                .map(|name| name.0.clone())
        };
        for (camera, counters, gpu) in self.renderer.screen_stats() {
            cameras.push(json!({
                "name": name(camera),
                "screen": true,
                "gpu_ms": gpu.as_secs_f64() * 1000.0,
                "draws": counters.draws,
                "triangles": counters.triangles,
            }));
        }
        let mut frames = self.gpu_frames.clone();
        frames.sort_by(f64::total_cmp);
        let rank = |p: f64| {
            let index = (p * frames.len() as f64).ceil() as usize;
            frames.get(index.saturating_sub(1)).copied()
        };
        json!({
            "gpu_ms": gpu.as_secs_f64() * 1000.0,
            "gpu_frames": frames.len(),
            "gpu_ms_p50": rank(0.5),
            "gpu_ms_p95": rank(0.95),
            "gpu_ms_max": rank(1.0),
            "cameras": cameras,
            "device": properties.device_name,
            "driver": properties.driver_info,
            "quality": resolve_quality(requested, self.renderer.capabilities()),
            "requested_quality": requested,
            "draws": counters.draws,
            "triangles": counters.triangles,
            "visible_instances": counters.visible_instances,
            "dropped_lights": capacity.dropped_lights,
            "lights": capacity.lights,
            "physics_grid_overflow": capacity.physics_grid_overflow,
            "physics_oversized_bodies": capacity.physics_oversized_bodies,
            "physics_fallback_tests": capacity.physics_fallback_tests,
        })
    }

    /// Writes the last rendered frame as an image; the extension picks the
    /// format, normally `.png`.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        save_rgba(path, &self.rgba(), self.extent)
    }

    #[must_use]
    pub fn extent(&self) -> [u32; 2] {
        self.extent
    }
}

/// Writes RGBA8 rows as a PNG, making its folder.
pub fn save_rgba(
    path: &Path,
    rgba: &[u8],
    extent: [u32; 2],
) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    image::save_buffer(
        path,
        rgba,
        extent[0],
        extent[1],
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn color_target(
    allocator: &Arc<StandardMemoryAllocator>,
    extent: [u32; 2],
) -> Result<(Arc<Image>, Arc<ImageView>), String> {
    let image = Image::new(
        allocator.clone(),
        ImageCreateInfo {
            // The UI paints through a UNORM view of the same image.
            flags: vulkano::image::ImageCreateFlags::MUTABLE_FORMAT,
            format: OFFSCREEN_COLOR_FORMAT,
            extent: [extent[0], extent[1], 1],
            usage: ImageUsage::COLOR_ATTACHMENT
                | ImageUsage::TRANSFER_SRC
                | ImageUsage::TRANSFER_DST,
            ..Default::default()
        },
        AllocationCreateInfo::default(),
    )
    .map_err(|error| format!("capture target: {error}"))?;
    let view = ImageView::new_default(image.clone())
        .map_err(|error| format!("capture target view: {error}"))?;
    Ok((image, view))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::HeadlessCapture;
    use crate::runtime::{
        Camera, MeshRenderer, Projection, RenderExtractPlugin, RenderSettings,
    };
    use crate::{App, AssetPlugin, AssetServer, Transform};

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_half_render_scale_still_fills_the_whole_target() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        app.spawn((
            Transform::new([0.0, 0.0, 3.0]),
            Camera {
                projection: Projection::Perspective {
                    vertical_fov_radians: 1.0,
                    near: 0.1,
                    far: 100.0,
                },
                active: true,
                priority: 0,
                viewport: None,
            },
        ));
        app.spawn((
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
            },
        ));
        let mut capture = HeadlessCapture::new([64, 64]).unwrap();
        let mut shoot = |app: &mut App, scale: f32| {
            app.world_mut()
                .resource_mut::<RenderSettings>()
                .render_scale = scale;
            capture.frame(app, Duration::from_millis(16)).unwrap();
            capture.rgba()
        };
        let half = shoot(&mut app, 0.5);
        let full = shoot(&mut app, 1.0);
        assert_eq!(half.len(), full.len());
        let pixel = |rgba: &[u8], x: usize, y: usize| {
            let at = (y * 64 + x) * 4;
            rgba[at..at + 3].to_vec()
        };
        // The centre shows the mesh and a corner the background, as at full
        // scale: the blit stretched the half-size frame over the target.
        for (x, y) in [(32, 32), (1, 1), (62, 62)] {
            let (a, b) = (pixel(&full, x, y), pixel(&half, x, y));
            let close = a.iter().zip(&b).all(|(a, b)| a.abs_diff(*b) < 24);
            assert!(close, "({x}, {y}): {a:?} vs {b:?}");
        }
        assert_ne!(pixel(&half, 32, 32), pixel(&half, 1, 1));
        // Pixelated: a quarter-size frame shows 4 x 4 blocks of one color.
        app.world_mut().resource_mut::<RenderSettings>().pixelated = true;
        let blocky = shoot(&mut app, 0.25);
        for y in 0..64 {
            for x in 0..64 {
                let corner = pixel(&blocky, x - x % 4, y - y % 4);
                assert_eq!(pixel(&blocky, x, y), corner, "({x}, {y})");
            }
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn sharp_upscale_keeps_edges_steeper_than_the_linear_stretch() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        app.spawn((
            Transform::new([0.0, 0.0, 3.0]),
            Camera {
                active: true,
                ..Camera::default()
            },
        ));
        app.spawn((
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
            },
        ));
        let mut capture = HeadlessCapture::new([64, 64]).unwrap();
        let mut shoot = |app: &mut App, sharpness: f32| {
            let mut settings = app.world_mut().resource_mut::<RenderSettings>();
            settings.render_scale = 0.5;
            settings.upscale_sharpness = sharpness;
            capture.frame(app, Duration::from_millis(16)).unwrap();
            capture.rgba()
        };
        let linear = shoot(&mut app, 0.0);
        let sharp = shoot(&mut app, 1.0);
        let green = |rgba: &[u8], x: usize, y: usize| {
            i64::from(rgba[(y * 64 + x) * 4 + 1])
        };
        // Squared steps between neighbors: steeper edges add up to more.
        let steepness = |rgba: &[u8]| {
            let mut sum = 0;
            for y in 0..64 {
                for x in 0..63 {
                    sum += (green(rgba, x + 1, y) - green(rgba, x, y)).pow(2);
                    sum += (green(rgba, y, x + 1) - green(rgba, y, x)).pow(2);
                }
            }
            sum
        };
        let (soft, crisp) = (steepness(&linear), steepness(&sharp));
        assert!(crisp * 2 > soft * 3, "linear {soft}, sharp {crisp}");
        // Flat areas look the same either way.
        for (x, y) in [(32, 32), (1, 1), (62, 62)] {
            let (a, b) = (green(&linear, x, y), green(&sharp, x, y));
            assert!(a.abs_diff(b) < 8, "({x}, {y}): {a} vs {b}");
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_gpu_cloth_follows_the_fixed_tick_with_the_cpu_reference_bits() {
        use crate::runtime::gpu_cloth::{pack, step_on_cpu, unpack, GpuCloth};
        use crate::runtime::{Cloth, ClothSettings, ClothVolume, FrameTime};

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let mut cloth = Cloth::grid(
            [0.0, 2.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [6, 6],
            0.125,
            0.3,
        )
        .unwrap();
        for i in 0..7 {
            cloth.pin(i);
        }
        let volume = ClothVolume {
            settings: ClothSettings {
                substeps: 4,
                ..ClothSettings::default()
            },
            cloth,
            attachments: Vec::new(),
            skin: None,
        };
        let start = volume.clone();
        let entity = app.spawn((volume, GpuCloth::default()));
        let mut capture = HeadlessCapture::new([16, 16]).unwrap();
        for _ in 0..12 {
            capture.frame(&mut app, Duration::from_millis(16)).unwrap();
        }
        let gpu = *app.world().get::<GpuCloth>(entity).unwrap();
        let reached = gpu.tick.unwrap();
        let now = app.world().resource::<FrameTime>().fixed_tick;
        // The cloth trails the fixed tick by at most a few frames.
        assert!(reached >= 3 && now - reached <= 4, "{reached} of {now}");
        // Several uneven GPU runs give the bits of one run from the start.
        let dt = app.world().resource::<FrameTime>().fixed_delta;
        let mut expected = start.cloth.clone();
        let mut words =
            pack(&expected, &start.settings, dt.as_secs_f32(), reached as u32)
                .unwrap();
        step_on_cpu(&mut words);
        unpack(&words, &mut expected);
        let shown = &app.world().get::<ClothVolume>(entity).unwrap().cloth;
        assert_eq!(shown.positions, expected.positions);
        assert_eq!(shown.velocities, expected.velocities);
        // The free edge swings down from the flat start.
        assert!(shown.positions[45][1] < 1.98, "{:?}", shown.positions[45]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_gpu_soft_body_follows_the_fixed_tick_with_the_cpu_reference_bits() {
        use crate::runtime::gpu_cloth::{
            pack_soft_body, step_on_cpu, unpack_soft_body, GpuSoftBody,
        };
        use crate::runtime::{
            FrameTime, SoftBody, SoftBodySettings, SoftBodyVolume,
        };

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let body = SoftBody::block([-0.5, 0.3, -0.5], [3, 3, 3], 0.25, 1000.0)
            .unwrap();
        let start = SoftBodyVolume::new(SoftBodySettings::default(), body);
        let entity = app.spawn((start.clone(), GpuSoftBody::default()));
        let mut capture = HeadlessCapture::new([16, 16]).unwrap();
        for _ in 0..12 {
            capture.frame(&mut app, Duration::from_millis(16)).unwrap();
        }
        let reached = app.world().get::<GpuSoftBody>(entity).unwrap().tick;
        let reached = reached.unwrap();
        let now = app.world().resource::<FrameTime>().fixed_tick;
        assert!(reached >= 3 && now - reached <= 4, "{reached} of {now}");
        let dt = app.world().resource::<FrameTime>().fixed_delta;
        let mut expected = start.body.clone();
        let mut words = pack_soft_body(
            &expected,
            &start.settings,
            dt.as_secs_f32(),
            reached as u32,
        )
        .unwrap();
        step_on_cpu(&mut words);
        unpack_soft_body(&words, &mut expected);
        let shown = &app.world().get::<SoftBodyVolume>(entity).unwrap().body;
        assert_eq!(shown.positions, expected.positions);
        assert_eq!(shown.velocities, expected.velocities);
        assert!(shown.positions[0][1] < 0.3, "{:?}", shown.positions[0]);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn a_flash_tints_the_drawn_object_and_fades_back() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        app.spawn((
            Transform::new([0.0, 0.0, 3.0]),
            Camera {
                active: true,
                ..Camera::default()
            },
        ));
        let cube = app.spawn((
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
            },
            crate::runtime::Flash {
                color: [0.0, 1.0, 0.0],
                duration: 0.3,
                ..Default::default()
            },
        ));
        let mut capture = HeadlessCapture::new([32, 32]).unwrap();
        let mut centre = |app: &mut App| {
            capture.frame(app, Duration::from_millis(16)).unwrap();
            let at = (16 * 32 + 16) * 4;
            capture.rgba()[at..at + 3].to_vec()
        };
        let before = centre(&mut app);
        app.world_mut()
            .get_mut::<crate::runtime::Flash>(cube)
            .unwrap()
            .flash();
        let flashed = centre(&mut app);
        assert!(
            flashed[1] > before[1].saturating_add(80)
                && flashed[0] < before[0].max(1),
            "green flash: {before:?} then {flashed:?}"
        );
        for _ in 0..30 {
            centre(&mut app);
        }
        assert_eq!(centre(&mut app), before, "back to the material color");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn viewport_cameras_split_the_frame() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        let (mesh, material) = {
            let assets = app.world().resource::<AssetServer>();
            (assets.fallback_mesh, assets.fallback_material)
        };
        let camera = |priority, viewport| Camera {
            active: true,
            priority,
            viewport: Some(viewport),
            ..Camera::default()
        };
        // The left camera sees the mesh, the right one empty space.
        app.spawn((
            crate::runtime::Name("Left".into()),
            Transform::new([0.0, 0.0, 3.0]),
            camera(0, [0.0, 0.0, 0.5, 1.0]),
        ));
        app.spawn((
            crate::runtime::Name("Right".into()),
            Transform::new([100.0, 0.0, 3.0]),
            camera(1, [0.5, 0.0, 0.5, 1.0]),
        ));
        // An inactive camera shown on the mesh as a screen.
        app.spawn((
            crate::runtime::Name("Feed".into()),
            Transform::new([0.0, 0.0, 5.0]),
            Camera {
                active: false,
                ..Camera::default()
            },
        ));
        app.spawn((
            Transform::default(),
            MeshRenderer {
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
            },
            crate::runtime::CameraScreen {
                camera: "Feed".into(),
                size: [8, 8],
                ..Default::default()
            },
        ));
        let mut capture = HeadlessCapture::new([64, 32]).unwrap();
        capture.frame(&mut app, Duration::from_millis(16)).unwrap();
        let rgba = capture.rgba();
        let pixel = |x: usize, y: usize| {
            let at = (y * 64 + x) * 4;
            rgba[at..at + 3].to_vec()
        };
        // The mesh is centred in the left half, and the right half matches
        // the left half's background.
        assert_ne!(pixel(16, 16), pixel(1, 1));
        assert_eq!(pixel(48, 16), pixel(62, 1));
        assert_eq!(pixel(1, 1), pixel(62, 1));
        let metadata = capture.metadata(&app);
        let names: Vec<_> = metadata["cameras"]
            .as_array()
            .unwrap()
            .iter()
            .map(|camera| camera["name"].clone())
            .collect();
        // Screen feeds come after the views, each with its own numbers.
        assert_eq!(names, ["Left", "Right", "Feed"]);
        assert_eq!(metadata["cameras"][2]["screen"], true);
        assert!(metadata["cameras"][2]["draws"].as_u64().unwrap() > 0);
    }

    #[test]
    #[cfg(feature = "ui")]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn hud_text_is_on_the_first_captured_frame() {
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.world_mut().spawn(crate::runtime::HudElement {
            text: "Score".into(),
            ..Default::default()
        });
        let mut capture = HeadlessCapture::new([64, 64]).unwrap();
        capture.frame(&mut app, Duration::from_millis(16)).unwrap();
        let texts = capture.texts();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(!texts[0].backed);
    }

    #[test]
    #[cfg(feature = "ui")]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn the_ui_blends_in_gamma_space_as_over_the_window() {
        use crate::runtime::{RuntimeUi, ScheduleStage};
        use bevy_ecs::prelude::Res;
        fn hud(ui: Res<RuntimeUi>) {
            let painter =
                ui.context().layer_painter(egui::LayerId::background());
            let all = egui::Rect::from_min_max(
                egui::pos2(0.0, 0.0),
                egui::pos2(8.0, 8.0),
            );
            painter.rect_filled(all, 0.0, egui::Color32::from_gray(200));
            let shade = egui::Color32::from_rgba_premultiplied(0, 0, 0, 128);
            painter.rect_filled(all, 0.0, shade);
        }
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        app.add_system(ScheduleStage::Update, hud);
        app.world_mut()
            .resource_mut::<RuntimeUi>()
            .set_input(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_max(
                    egui::pos2(0.0, 0.0),
                    egui::pos2(8.0, 8.0),
                )),
                ..Default::default()
            });
        let mut capture = HeadlessCapture::new([8, 8]).unwrap();
        capture.frame(&mut app, Duration::from_millis(16)).unwrap();
        // Half black over grey 200 is 100 when blended in gamma space, as
        // egui backends and the UNORM window do; linear light gives 146.
        let red = capture.rgba()[(4 * 8 + 4) * 4];
        assert!(red.abs_diff(100) <= 2, "{red}");
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_events_past_the_buffer_are_counted_and_reported_to_the_world() {
        use crate::rendering::scene_renderer::RenderCapacityDiagnostics;
        use crate::runtime::{
            EventQueue, GpuConditionShader, GpuConditionShaders,
            GpuPhysicsEvent, GpuPhysicsEventsLost, HybridPhysicsPlugin,
            PhysicsBody, SimulationClass,
        };

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        const BODIES: usize = 100;
        for index in 0..BODIES {
            app.spawn((
                Transform::new([index as f32 * 2.0, 0.0, 0.0]),
                PhysicsBody {
                    simulation: SimulationClass::Gpu,
                    ..PhysicsBody::default()
                },
            ));
        }
        // The shader promises one event per body and tick but sends two,
        // so the buffer holds half of them.
        app.world_mut().resource_mut::<GpuConditionShaders>().0 =
            vec![GpuConditionShader {
                events: vec!["ping".into()],
                glsl: "void condition(inout PhysicsState body) {\n\
                       emit_event(body, EVENTS[0], 0u, vec4(0.0));\n\
                       emit_event(body, EVENTS[0], 0u, vec4(0.0));\n\
                       }"
                .into(),
                events_per_body: 1,
                ..Default::default()
            }];
        let mut capture = HeadlessCapture::new([16, 16]).unwrap();
        let (mut delivered, mut lost) = (0, 0);
        for _ in 0..8 {
            capture.frame(&mut app, Duration::from_millis(16)).unwrap();
            let world = app.world();
            delivered += world.resource::<EventQueue<GpuPhysicsEvent>>().len();
            lost += world
                .resource::<EventQueue<GpuPhysicsEventsLost>>()
                .iter()
                .map(|event| event.count)
                .sum::<u64>();
        }
        assert!(delivered >= 2 * BODIES, "{delivered}");
        assert_eq!(lost, delivered as u64);
        // The diagnostics keep the running total for the profiler.
        let dropped = app
            .world()
            .resource::<RenderCapacityDiagnostics>()
            .physics_events_dropped;
        assert_eq!(dropped, lost);
    }

    #[test]
    #[cfg_attr(
        not(feature = "gpu-tests"),
        ignore = "run with `--features gpu-tests` on a machine with a Vulkan driver"
    )]
    fn gpu_condition_events_reach_their_entity_with_id_and_payload() {
        use crate::runtime::{
            EventQueue, GpuCondition, GpuEventPayload, GpuEventRegistry,
            GpuPhysicsEvent, GpuPhysicsRule, GpuPhysicsWatch,
            HybridPhysicsPlugin, PhysicsBody, SimulationClass,
        };

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(HybridPhysicsPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        // A despawned entity and an unwatched body keep entity, slot and
        // spawn order apart.
        let gone = app.spawn(Transform::default());
        app.world_mut().despawn(gone);
        let gpu = || PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        };
        app.spawn((Transform::new([-4.0, 2.0, 0.0]), gpu()));
        let watched = |rule| GpuPhysicsWatch { rules: vec![rule] };
        // Both fall from rest: past 3 m/s after 0.31 s, below 1 m after
        // 0.45 s.
        let fast = app.spawn((
            Transform::new([3.0, 2.0, 0.0]),
            gpu(),
            watched(
                GpuPhysicsRule::new(
                    "fast",
                    GpuCondition::velocity_y().less_than(-3.0),
                )
                .payload(GpuEventPayload::Velocity),
            ),
        ));
        let low = app.spawn((
            Transform::new([5.0, 2.0, -1.0]),
            gpu(),
            watched(
                GpuPhysicsRule::new(
                    "low",
                    GpuCondition::position_y().less_than(1.0),
                )
                .payload(GpuEventPayload::Position),
            ),
        ));
        let mut capture = HeadlessCapture::new([16, 16]).unwrap();
        let mut events = Vec::new();
        for _ in 0..45 {
            capture.frame(&mut app, Duration::from_millis(16)).unwrap();
            let queue = app.world().resource::<EventQueue<GpuPhysicsEvent>>();
            events.extend(queue.iter().copied());
        }
        let registry = app.world().resource::<GpuEventRegistry>();
        let [first, second] = events[..] else {
            panic!("{events:?}");
        };
        assert_eq!(first.entity, fast);
        assert_eq!(first.event_id, registry.id("fast").unwrap());
        assert!(first.tick < second.tick, "{events:?}");
        let [_, vy, _, _] = first.payload;
        assert!((-3.2..-3.0).contains(&vy), "{first:?}");
        assert_eq!(second.entity, low);
        assert_eq!(second.event_id, registry.id("low").unwrap());
        let [x, y, z, _] = second.payload;
        assert_eq!([x, z], [5.0, -1.0]);
        assert!((0.9..1.0).contains(&y), "{second:?}");
    }
}
