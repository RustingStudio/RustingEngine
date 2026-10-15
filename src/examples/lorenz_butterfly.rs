//! The butterfly effect: a million particles start in a cube 0.000001 wide
//! and the Lorenz equations pull them apart over the whole attractor.
//!
//! ```sh
//! cargo run --release --features window --example lorenz_butterfly -- --side 100
//! ```
//!
//! `--side` is the particles along each edge of the starting cube (default
//! 100, so 1,000,000). Every particle is a GPU body with no collisions; a
//! condition shader moves it with RK4 steps of the real Lorenz system
//! (σ = 10, ρ = 28, β = 8/3).
//!
//! A float32 position near x = 20 cannot hold a 0.000001 offset, so each
//! body stores only its offset `d` from a reference path that the CPU works
//! out in f64. The Lorenz equations are quadratic, so the offset's own
//! equation `f(r + d) - f(r)` expands exactly with no cancellation: the
//! particle at `r + d` follows the true Lorenz flow. The shader draws
//! `(r + d - focus) * zoom`, a camera zoom done in Lorenz units, which keeps
//! the tiny starting cube sharp while the view pulls back by a factor of
//! 300,000. The run lasts 30 s at 60 ticks a second and freezes at 24 s.

use rusting_engine::prelude::*;
use rusting_engine::runtime::{
    Bloom, MeshRenderer, NextSpawnOrder, SceneId, SpawnOrder, Visibility,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::sync::OnceLock;

const SIGMA: f64 = 10.0;
const RHO: f64 = 28.0;
const BETA: f64 = 8.0 / 3.0;
/// RK4 step in Lorenz time units.
const H: f64 = 0.005;
/// Edge of the starting cube in Lorenz units.
const CUBE: f64 = 1e-6;
const FPS: f64 = 60.0;
const TICKS: usize = 1800;
/// Tick at which the particles stop.
const FREEZE: usize = 1440;
/// Zoom once the whole attractor is in view: 50 Lorenz units, 2.15 m.
const ZOOM_FULL: f64 = 0.043;
/// Edge of one particle's cube, in metres on screen.
const PARTICLE: f32 = 0.0045;
const HUES: usize = 48;
const CAMERA_DISTANCE: f32 = 4.3;
const CAMERA_FOV: f32 = 0.6;
/// Rows of `params` before the per-tick rows.
const HEAD: usize = 2;

/// GLSL: params[0] = σ, ρ, β, h; params[1] = ticks, first reference row,
/// freeze tick. Then two rows per tick: (view offset, zoom) and (first
/// step, steps); then the reference path at every half step.
const LORENZ: &str = r#"
vec3 to_world(vec3 p) { return vec3(p.x, p.z, -p.y); }
vec3 to_lorenz(vec3 w) { return vec3(w.x, -w.z, w.y); }

// f(r + d) - f(r) for the Lorenz field: exact, no cancellation.
vec3 offset_rate(vec3 r, vec3 d, vec4 k) {
    return vec3(
        k.x * (d.y - d.x),
        d.x * (k.y - r.z) - r.x * d.z - d.x * d.z - d.y,
        r.x * d.y + r.y * d.x + d.x * d.y - k.z * d.z);
}

void condition(inout PhysicsState body) {
    vec4 k = condition_params.values[0];
    vec4 info = condition_params.values[1];
    uint tick = min(uint(pc.elapsed / pc.dt + 0.5), uint(info.x));
    vec4 view = condition_params.values[2u + 2u * tick];
    vec4 steps = condition_params.values[3u + 2u * tick];
    uint reference = uint(info.y);
    vec3 d = body.custom_values.yzw;
    if (d == vec3(0.0)) {
        // First tick: the spawn position is the starting offset, drawn
        // with tick 0's view.
        vec4 start = condition_params.values[2];
        d = (to_lorenz(body.model[3].xyz) - start.xyz) / start.w;
    }
    float h = k.w;
    uint first = uint(steps.x);
    for (uint i = 0u; i < uint(steps.y); i++) {
        uint row = reference + 2u * (first + i);
        vec3 r0 = condition_params.values[row].xyz;
        vec3 r1 = condition_params.values[row + 1u].xyz;
        vec3 r2 = condition_params.values[row + 2u].xyz;
        vec3 k1 = offset_rate(r0, d, k);
        vec3 k2 = offset_rate(r1, d + 0.5 * h * k1, k);
        vec3 k3 = offset_rate(r1, d + 0.5 * h * k2, k);
        vec3 k4 = offset_rate(r2, d + h * k3, k);
        d += h / 6.0 * (k1 + 2.0 * k2 + 2.0 * k3 + k4);
    }
    body.custom_values.yzw = d;
    vec3 world = to_world(view.xyz + view.w * d);
    body.model[3].xyz = world;
    body.velocity.xyz = vec3(0.0);
    body.angular_velocity.w = 0.0;
    // Marked bodies report where they froze, and which wing they are on.
    if (body.properties.x > 1.5 && tick == uint(info.z)) {
        uint now = reference + 2u * uint(steps.x + steps.y);
        float x = condition_params.values[now].x + d.x;
        emit_event(body, EVENTS[0], 1u, vec4(world, x));
    }
}
"#;

type V3 = [f64; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: V3, s: f64) -> V3 {
    a.map(|axis| axis * s)
}

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn lorenz(p: V3) -> V3 {
    [
        SIGMA * (p[1] - p[0]),
        p[0] * (RHO - p[2]) - p[1],
        p[0] * p[1] - BETA * p[2],
    ]
}

fn rk4(p: V3, h: f64) -> V3 {
    let k1 = lorenz(p);
    let k2 = lorenz(add(p, scale(k1, h / 2.0)));
    let k3 = lorenz(add(p, scale(k2, h / 2.0)));
    let k4 = lorenz(add(p, scale(k3, h)));
    let sum = add(add(k1, scale(add(k2, k3), 2.0)), k4);
    add(p, scale(sum, h / 6.0))
}

/// Same as the shader's `offset_rate`.
fn offset_rate(r: V3, d: V3) -> V3 {
    [
        SIGMA * (d[1] - d[0]),
        d[0] * (RHO - r[2]) - r[0] * d[2] - d[0] * d[2] - d[1],
        r[0] * d[1] + r[1] * d[0] + d[0] * d[1] - BETA * d[2],
    ]
}

/// One RK4 step of an offset along reference points `r` at t, t + h/2, t + h.
fn offset_step(r: [V3; 3], d: V3) -> V3 {
    let k1 = offset_rate(r[0], d);
    let k2 = offset_rate(r[1], add(d, scale(k1, H / 2.0)));
    let k3 = offset_rate(r[1], add(d, scale(k2, H / 2.0)));
    let k4 = offset_rate(r[2], add(d, scale(k3, H)));
    let sum = add(add(k1, scale(add(k2, k3), 2.0)), k4);
    add(d, scale(sum, H / 6.0))
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Linear interpolation through `(x, y)` keys sorted by x.
fn keyed(keys: &[(f64, f64)], x: f64) -> f64 {
    let at = keys.iter().position(|key| key.0 > x).unwrap_or(keys.len());
    if at == 0 {
        return keys[0].1;
    }
    if at == keys.len() {
        return keys[at - 1].1;
    }
    let ((x0, y0), (x1, y1)) = (keys[at - 1], keys[at]);
    y0 + (y1 - y0) * (x - x0) / (x1 - x0)
}

/// Lorenz time units per second of video. Slow on the starting dot, fast
/// while the offsets grow from 1e-6 to the size of a wing, slow again on
/// the full attractor, stopped at the freeze.
fn rate(seconds: f64) -> f64 {
    keyed(
        &[
            (0.0, 0.5),
            (2.0, 1.5),
            (3.0, 2.4),
            (7.5, 2.4),
            (9.0, 0.9),
            (16.0, 0.6),
            (24.0, 0.6),
        ],
        seconds,
    )
}

/// A fixed value in [0, 1) for one index and channel.
fn noise(index: usize, channel: u64) -> f64 {
    let mut value = (index as u64) ^ (channel << 56);
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    (value >> 11) as f64 / (1u64 << 53) as f64
}

struct Plan {
    params: Vec<[f32; 4]>,
    /// Per tick: Lorenz time, spread of the sample particles, zoom.
    readout: Vec<[f64; 3]>,
    /// Hue coordinate of an offset: dot with this, then 0.5 ± half.
    hue_axis: V3,
}

fn plan() -> &'static Plan {
    static PLAN: OnceLock<Plan> = OnceLock::new();
    PLAN.get_or_init(|| {
        // Land on the attractor first.
        let mut start = [1.0, 1.0, 1.0];
        for _ in 0..(40.0 / H) as usize {
            start = rk4(start, H);
        }

        // RK4 steps finished by the end of each tick.
        let mut done = vec![0usize; TICKS + 1];
        let mut time = 0.0;
        for (tick, done) in done.iter_mut().enumerate().skip(1) {
            let seconds = (tick as f64 - 0.5) / FPS;
            if tick <= FREEZE {
                time += rate(seconds) / FPS;
            }
            *done = (time / H).round() as usize;
        }
        let total = done[TICKS];

        // The reference path at every half step.
        let mut path = Vec::with_capacity(2 * total + 1);
        let mut point = start;
        path.push(point);
        for _ in 0..2 * total {
            point = rk4(point, H / 2.0);
            path.push(point);
        }

        // Sample offsets: the three half-edges first (they measure the
        // stretch for the colors), then the cube's corners, then random.
        let half = CUBE / 2.0;
        let mut samples: Vec<V3> =
            vec![[half, 0.0, 0.0], [0.0, half, 0.0], [0.0, 0.0, half]];
        for corner in 0..8 {
            samples.push([0, 1, 2].map(|axis| {
                if corner >> axis & 1 == 1 {
                    half
                } else {
                    -half
                }
            }));
        }
        for index in 0..53 {
            samples
                .push([0, 1, 2].map(|axis| (noise(index, axis) - 0.5) * CUBE));
        }

        // Follow the samples; record their centre and spread per tick.
        let mut hue_axis = None;
        let mut centres = Vec::with_capacity(TICKS + 1);
        let mut spreads = Vec::with_capacity(TICKS + 1);
        for tick in 0..=TICKS {
            if tick > 0 {
                for step in done[tick - 1]..done[tick] {
                    let r = [
                        path[2 * step],
                        path[2 * step + 1],
                        path[2 * step + 2],
                    ];
                    for d in &mut samples {
                        *d = offset_step(r, *d);
                    }
                }
            }
            let centre = scale(
                samples.iter().fold([0.0; 3], |sum, d| add(sum, *d)),
                1.0 / samples.len() as f64,
            );
            let spread = samples
                .iter()
                .map(|d| {
                    let o = add(*d, scale(centre, -1.0));
                    dot(o, o).sqrt()
                })
                .fold(0.0, f64::max);
            // Still linear at 1e-3: the stretch direction is clean.
            if hue_axis.is_none() && spread > 1e-3 {
                let longest = samples[..3]
                    .iter()
                    .copied()
                    .max_by(|a, b| dot(*a, *a).total_cmp(&dot(*b, *b)))
                    .unwrap();
                let along = scale(longest, 1.0 / dot(longest, longest).sqrt());
                let axis = [0, 1, 2].map(|a| dot(samples[a], along) / half);
                let range: f64 =
                    axis.iter().map(|v| v.abs()).sum::<f64>() * half;
                hue_axis = Some(scale(axis, 0.5 / range));
            }
            centres.push(centre);
            spreads.push(spread);
        }

        // Zoom: keep the cloud a set size on screen, from a dot to the
        // full attractor. The spread swings tenfold as the cloud folds, so
        // follow its log averaged over about a Lorenz time unit; the
        // swings stay on screen as the thread stretching and folding.
        let times: Vec<f64> =
            done.iter().map(|&steps| steps as f64 * H).collect();
        let trend: Vec<f64> = (0..=TICKS)
            .map(|tick| {
                let (mut sum, mut weight) = (0.0, 0.0);
                for other in 0..=TICKS {
                    let off = (times[other] - times[tick]) / 0.6;
                    let w = (-off * off).exp();
                    sum += spreads[other].ln() * w;
                    weight += w;
                }
                sum / weight
            })
            .collect();
        let screen = |seconds: f64| {
            keyed(
                &[(0.0, 0.012), (1.0, 0.014), (3.0, 0.35), (8.0, 0.8)],
                seconds,
            )
            .ln()
        };
        let raw: Vec<f64> = (0..=TICKS)
            .map(|tick| {
                (screen(tick as f64 / FPS) - trend[tick]).max(ZOOM_FULL.ln())
            })
            .collect();
        let zoom: Vec<f64> = (0..=TICKS)
            .map(|tick| {
                let (mut sum, mut weight) = (0.0, 0.0);
                let first = tick.saturating_sub(40);
                for (back, raw) in
                    raw.iter().enumerate().skip(first).take(tick + 41 - first)
                {
                    let off = (back as f64 - tick as f64) / 20.0;
                    let w = (-off * off).exp();
                    sum += raw * w;
                    weight += w;
                }
                (sum / weight).exp()
            })
            .collect();

        let mut params = vec![
            [SIGMA as f32, RHO as f32, BETA as f32, H as f32],
            [
                TICKS as f32,
                (HEAD + 2 * (TICKS + 1)) as f32,
                FREEZE as f32,
                0.0,
            ],
        ];
        // A little below the fixed points' z = ρ - 1: the camera sits above,
        // so this centres the wings on screen.
        let middle = [0.0, 0.0, RHO - 3.5];
        let mut readout = Vec::with_capacity(TICKS + 1);
        for tick in 0..=TICKS {
            // Look at the cloud while zoomed in, at the attractor's middle
            // once it fills the view.
            let s = zoom[tick];
            let wide = smooth(
                (s.ln() - 1.0_f64.ln())
                    / ((ZOOM_FULL * 1.2).ln() - 1.0_f64.ln()),
            );
            let r = path[2 * done[tick]];
            let to_cloud = scale(centres[tick], -1.0);
            let to_middle = add(r, scale(middle, -1.0));
            let view = scale(
                add(scale(to_cloud, 1.0 - wide), scale(to_middle, wide)),
                s,
            );
            params.push([
                view[0] as f32,
                view[1] as f32,
                view[2] as f32,
                s as f32,
            ]);
            let first = if tick == 0 { 0 } else { done[tick - 1] };
            params.push([first as f32, (done[tick] - first) as f32, 0.0, 0.0]);
            readout.push([times[tick], spreads[tick], s]);
        }
        params.extend(
            path.iter()
                .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32, 0.0]),
        );
        Plan {
            params,
            readout,
            hue_axis: hue_axis.expect("cloud reached 1e-3"),
        }
    })
}

fn side() -> usize {
    let mut arguments = std::env::args();
    arguments
        .find(|arg| arg == "--side")
        .and_then(|_| arguments.next())
        .map_or(100, |value| value.parse().expect("--side"))
}

/// Rainbow from red to violet, as linear RGB.
fn rainbow(x: f64) -> [f32; 3] {
    let hue = 0.83 * x.clamp(0.0, 1.0) * 6.0;
    let c = |n: f64| {
        let k = (n + hue) % 6.0;
        (1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))) as f32
    };
    [c(5.0), c(3.0), c(1.0)].map(|v| v.powf(2.2))
}

thread_local! {
    /// Marked bodies: entity to (i, j, k) in the starting cube.
    static MARKS: RefCell<HashMap<Entity, [usize; 3]>> = RefCell::new(HashMap::new());
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.0, 0.0, 0.0, 1.0]);
    scene.world().spawn(Bloom {
        intensity: 0.5,
        threshold: 1.0,
        spread: 0.6,
    });
    scene.set_camera_fov("Game Camera", CAMERA_FOV);

    let plan = plan();
    let materials: Vec<_> = (0..HUES)
        .map(|hue| {
            let color = rainbow(hue as f64 / (HUES - 1) as f64);
            scene.create_material(MaterialAsset {
                base_color: [0.0, 0.0, 0.0, 1.0],
                roughness: 1.0,
                emissive: color.map(|c| 0.04 + 1.6 * c),
                ..MaterialAsset::default()
            })
        })
        .collect();

    let n = side();
    let start = plan.params[HEAD];
    let mark_rows: Vec<usize> = [0.1, 0.37, 0.63, 0.9]
        .iter()
        .map(|f| ((n - 1) as f64 * f).round() as usize)
        .collect();
    let particle = CubeSpawn::new().class("lorenz");
    let marked = CubeSpawn::new().class("lorenz_mark");
    let mesh = scene.world().resource::<AssetServer>().fallback_mesh;
    let mut next_order = scene
        .world()
        .get_resource_or_insert_with(NextSpawnOrder::default)
        .0;
    let count = n * n * n;
    let offset_of = |index: usize| {
        [index / (n * n), index / n % n, index % n].map(|v| {
            if n == 1 {
                0.0
            } else {
                (v as f64 / (n - 1) as f64 - 0.5) * CUBE
            }
        })
    };
    // Hue by rank along the stretch, so every color gets as many particles.
    let along: Vec<f64> = (0..count)
        .map(|i| dot(offset_of(i), plan.hue_axis))
        .collect();
    let mut ranked: Vec<usize> = (0..count).collect();
    ranked.sort_by(|&a, &b| along[a].total_cmp(&along[b]));
    let mut hues = vec![0; count];
    for (rank, &index) in ranked.iter().enumerate() {
        hues[index] = rank * HUES / count;
    }
    for index in 0..count {
        let ijk = [index / (n * n), index / n % n, index % n];
        let offset = offset_of(index);
        let material = materials[hues[index]];
        let p =
            [0, 1, 2].map(|a| start[a] as f64 + start[3] as f64 * offset[a]);
        let mut transform =
            Transform::new([p[0] as f32, p[2] as f32, -p[1] as f32]);
        transform.scale = [PARTICLE; 3];
        let mark = (ijk[0] == 0 || ijk[0] == n - 1)
            && mark_rows.contains(&ijk[1])
            && mark_rows.contains(&ijk[2]);
        let shape = if mark { &marked } else { &particle };
        // What `spawn_cube_with_material` spawns, minus its unique-name
        // check: a new name misses the name index and scans every entity,
        // which is O(n²) over a million spawns.
        let order = SpawnOrder(next_order);
        next_order += 1;
        let entity = scene
            .world()
            .spawn((
                SceneId::new(),
                order,
                Name(format!("P {index}")),
                transform,
                MeshRenderer {
                    mesh,
                    material,
                    cast_shadows: false,
                    receive_shadows: false,
                },
                Visibility::default(),
                shape.classes.clone(),
            ))
            .id();
        if mark {
            MARKS.with(|marks| marks.borrow_mut().insert(entity, ijk));
        }
    }
    scene.world().resource_mut::<NextSpawnOrder>().0 = next_order;
    for (class, mass) in [("lorenz", 1.0), ("lorenz_mark", 2.0)] {
        scene.apply_gpu_physics_to_class(
            class,
            &GpuBodySettings {
                solver: PhysicsSolver::NoCollision,
                rigid_body: RigidBody {
                    gravity_scale: 0.0,
                    mass,
                    ..RigidBody::default()
                },
                ..GpuBodySettings::default()
            },
        );
    }
    println!("lorenz butterfly with {count} particles");
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_lorenz_butterfly", create);
    let plan = plan();
    scene.set_gpu_condition_shaders(vec![GpuConditionShader {
        events: vec!["lorenz_mark".into()],
        glsl: LORENZ.into(),
        params: plan.params.clone(),
        events_per_body: 1,
    }]);
    for event in scene.gpu_events("lorenz_mark") {
        let ijk =
            MARKS.with(|marks| marks.borrow().get(&event.entity).copied());
        if let Some([i, j, k]) = ijk {
            let [x, y, z, wing] = event.payload;
            println!("lorenz mark {i} {j} {k} {x} {y} {z} {wing}");
        }
    }

    // Front view drifting a little, one slow turn from 16 s to 24 s, then
    // still for the freeze.
    let tick = (time.fixed_tick as usize).min(TICKS);
    let seconds = tick as f64 / FPS;
    let turn = smooth((seconds - 16.0) / 8.0);
    let angle = -0.35 + 0.35 * smooth(seconds / 16.0) + TAU * turn;
    let elevation: f64 = 0.12;
    let d = CAMERA_DISTANCE as f64;
    let eye = [
        d * elevation.cos() * angle.sin(),
        d * elevation.sin(),
        d * elevation.cos() * angle.cos(),
    ]
    .map(|v| v as f32);
    scene
        .object("Game Camera")
        .set_position(eye)
        .look_at([0.0; 3]);
    let [t, spread, zoom] = plan.readout[tick];
    println!(
        "lorenz tick {tick} t {t:.4} spread {spread:.4e} zoom {zoom:.6e} eye {} {} {}",
        eye[0], eye[1], eye[2]
    );
}

rusting_game!("testGame/build/main.rscene.bin", update);
