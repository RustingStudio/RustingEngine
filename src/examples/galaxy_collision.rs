//! Two spiral galaxies fall together and merge.
//!
//! ```sh
//! cargo run --release --features window --example galaxy_collision -- 100000
//! ```
//!
//! The argument is the number of stars (default 100000); add `--portrait`
//! to pull the camera back for a 9:16 frame. Every star is a GPU body with
//! gravity off. Stars do not pull on each other: each one feels the two
//! galaxy cores, the classic restricted model of Toomre & Toomre (1972).
//! The cores' path is worked out on the CPU at startup, with a drag term
//! standing in for dynamical friction so they merge after two passes. The
//! path goes to the condition shader as a table of positions, so the shader
//! knows where both cores are at any tick. The camera follows game time, so
//! a replay films the same shot.

use rusting_engine::prelude::*;
use rusting_engine::runtime::Bloom;
use std::f32::consts::{PI, TAU};
use std::sync::OnceLock;

/// G times one core's mass, in m³/s². Both cores weigh the same.
const GM: f32 = 3000.0;
/// Plummer softening of a core's pull on a star: the core is a soft ball
/// of mass, not a point, so the middle of the disk turns smoothly.
const STAR_SOFTENING: f32 = 4.0;
/// Softening between the two cores, so the merger settles instead of
/// slinging them apart.
const CORE_SOFTENING: f32 = 6.0;
/// Dynamical friction: drag on the cores' relative speed, strongest when
/// they overlap.
const FRICTION: f32 = 0.2;
const FRICTION_RANGE: f32 = 25.0;
/// Starting distance between the cores and the closest approach that a
/// pure two-body orbit would reach.
const START_DISTANCE: f32 = 110.0;
const PERICENTER: f32 = 25.0;
/// Share of escape speed at the start: under one, so the pair is bound.
const START_SPEED: f32 = 0.9;
/// The core path table: one sample per tick for this many seconds, then
/// the cores stay where the table ends.
const SAMPLE_DT: f32 = 1.0 / 60.0;
const PATH_SECONDS: f32 = 60.0;
const DISK_INNER: f32 = 3.0;
const DISK_OUTER: f32 = 40.0;
/// Share of stars in the round bulge at the middle of each galaxy.
const BULGE: f32 = 0.12;

const GRAVITY: &str = r#"
vec3 pull(vec3 offset, float gm, float softening2) {
    float d2 = dot(offset, offset) + softening2;
    return offset * (gm * inversesqrt(d2 * d2 * d2));
}

void condition(inout PhysicsState body) {
    vec4 setup = condition_params.values[0]; // x GM, y softening², z sample dt, w samples
    float sample_at = clamp(pc.elapsed / setup.z, 0.0, setup.w - 1.001);
    uint index = uint(sample_at);
    // The cores sit at +core and -core: equal masses, still centre of mass.
    vec3 core = mix(condition_params.values[1u + index].xyz,
                    condition_params.values[2u + index].xyz, fract(sample_at));
    vec3 position = body.model[3].xyz;
    vec3 acceleration = pull(position - core, setup.x, setup.y)
                      + pull(position + core, setup.x, setup.y);
    body.velocity.xyz -= acceleration * pc.dt;
}
"#;

fn star_count() -> usize {
    std::env::args()
        .nth(1)
        .filter(|arg| !arg.starts_with("--"))
        .map_or(100_000, |count| count.parse().expect("star count"))
}

fn portrait() -> bool {
    std::env::args().any(|arg| arg == "--portrait")
}

/// A fixed value in [0, 1) for one index and channel.
fn noise(index: usize, channel: u64) -> f32 {
    let mut value = (index as u64) ^ (channel << 56);
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    (value >> 40) as f32 / (1u64 << 24) as f32
}

/// Core A's position at each sample (core B mirrors it through the
/// origin), and core A's starting velocity.
struct CorePath {
    samples: Vec<[f32; 4]>,
    start_velocity: [f32; 3],
}

fn core_path() -> &'static CorePath {
    static PATH: OnceLock<CorePath> = OnceLock::new();
    PATH.get_or_init(|| {
        // Relative orbit of A around B in the x-z plane.
        let mu = 2.0 * GM;
        let speed = START_SPEED * (2.0 * mu / START_DISTANCE).sqrt();
        let across = (2.0 * mu * PERICENTER).sqrt() / START_DISTANCE;
        let inward = (speed * speed - across * across).sqrt();
        let (mut x, mut z) = (START_DISTANCE, 0.0_f32);
        let (mut vx, mut vz) = (-inward, across);
        let start_velocity = [vx / 2.0, 0.0, vz / 2.0];
        let substeps = 10;
        let dt = SAMPLE_DT / substeps as f32;
        let count = (PATH_SECONDS / SAMPLE_DT) as usize;
        let mut samples = Vec::with_capacity(count);
        for _ in 0..count {
            samples.push([x / 2.0, 0.0, z / 2.0, 0.0]);
            for _ in 0..substeps {
                let r2 = x * x + z * z;
                let d2 = r2 + CORE_SOFTENING * CORE_SOFTENING;
                let pull = mu / (d2 * d2.sqrt());
                let drag =
                    FRICTION * (-r2 / (FRICTION_RANGE * FRICTION_RANGE)).exp();
                vx -= (pull * x + drag * vx) * dt;
                vz -= (pull * z + drag * vz) * dt;
                x += vx * dt;
                z += vz * dt;
            }
        }
        CorePath {
            samples,
            start_velocity,
        }
    })
}

/// Speed of a circular orbit at `radius` around one softened core.
fn circular_speed(radius: f32) -> f32 {
    let d2 = radius * radius + STAR_SOFTENING * STAR_SOFTENING;
    (GM * radius * radius / (d2 * d2.sqrt())).sqrt()
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    a.map(|axis| axis * s)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    scale(a, 1.0 / (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt())
}

/// A black material that only emits: `Unlit` ignores `emissive`.
fn glow(scene: &mut GameScene<'_>, emissive: [f32; 3]) -> Handle {
    scene.create_material(MaterialAsset {
        base_color: [0.0, 0.0, 0.0, 1.0],
        roughness: 1.0,
        emissive,
        ..MaterialAsset::default()
    })
}

type Handle = rusting_engine::assets::Handle<MaterialAsset>;

struct Galaxy {
    centre: [f32; 3],
    velocity: [f32; 3],
    /// Spin axis of the disk.
    normal: [f32; 3],
    bulge: Handle,
    inner: Handle,
    arms: Handle,
    nebula: Handle,
}

/// Spawns one star of `galaxy` and returns its velocity.
fn spawn_star(
    scene: &mut GameScene<'_>,
    galaxy: &Galaxy,
    index: usize,
    shape: &CubeSpawn,
) -> [f32; 3] {
    let n = galaxy.normal;
    let u = normalize(cross(n, [0.3, 0.1, 1.0]));
    let w = cross(n, u);
    let (offset, axis, material) = if noise(index, 0) < BULGE {
        // A ball of stars on circular orbits tipped every which way.
        let radius = 0.5 + 6.0 * noise(index, 1).powi(2);
        let (theta, z) = (TAU * noise(index, 2), 2.0 * noise(index, 3) - 1.0);
        let ring = (1.0 - z * z).sqrt();
        let offset = scale([ring * theta.cos(), z, ring * theta.sin()], radius);
        let tip = [noise(index, 4) - 0.5, 1.0, noise(index, 5) - 0.5];
        (offset, normalize(add(n, scale(tip, 0.8))), galaxy.bulge)
    } else {
        // Two log-spiral arms, with a third of the stars between them.
        let radius =
            DISK_INNER + (DISK_OUTER - DISK_INNER) * noise(index, 1).powf(1.3);
        let in_arm = noise(index, 6) < 0.7;
        let angle = if in_arm {
            let arm = if noise(index, 7) < 0.5 { 0.0 } else { PI };
            let jitter = (noise(index, 2) + noise(index, 8) - 1.0) * 0.6;
            arm + (radius / DISK_INNER).ln() / 0.45 + jitter
        } else {
            TAU * noise(index, 2)
        };
        let height = (noise(index, 3) - 0.5) * 1.2;
        let offset = add(
            add(
                scale(u, radius * angle.cos()),
                scale(w, radius * angle.sin()),
            ),
            scale(n, height),
        );
        let material = if radius < 9.0 {
            galaxy.inner
        } else if in_arm && noise(index, 9) < 0.05 {
            galaxy.nebula
        } else {
            galaxy.arms
        };
        (offset, n, material)
    };
    let radius =
        (offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2])
            .sqrt();
    let along = normalize(cross(axis, offset));
    let mut transform = Transform::new(add(galaxy.centre, offset))
        .with_rotation(TAU * noise(index, 10), TAU * noise(index, 11), 0.0);
    transform.scale = [0.25 + 0.3 * noise(index, 12); 3];
    scene.spawn_cube_with_material(
        format!("Star {index}"),
        transform,
        shape,
        material,
    );
    add(galaxy.velocity, scale(along, circular_speed(radius)))
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.0, 0.0, 0.0, 1.0]);
    scene.world().spawn(Bloom {
        intensity: 0.45,
        threshold: 1.2,
        spread: 0.55,
    });

    // Far stars, not simulated.
    let far = glow(scene, [1.2, 1.3, 1.6]);
    let far_shape = CubeSpawn::new();
    for index in 0..3000 {
        let (u, v) = (noise(index, 20), noise(index, 21));
        let (theta, z) = (TAU * u, 2.0 * v - 1.0);
        let ring = (1.0 - z * z).sqrt();
        let mut transform = Transform::new(
            [ring * theta.cos(), z, ring * theta.sin()]
                .map(|axis| axis * 900.0),
        );
        transform.scale = [0.6 + 1.2 * noise(index, 22); 3];
        scene.spawn_cube_with_material(
            format!("Far Star {index}"),
            transform,
            &far_shape,
            far,
        );
    }

    let path = core_path();
    let start = path.samples[0];
    let start = [start[0], start[1], start[2]];
    let velocity = path.start_velocity;
    let galaxies = [
        // Turns the same way as the orbit (about -y): long tidal tails.
        Galaxy {
            centre: start,
            velocity,
            normal: [0.0, -1.0, 0.0],
            bulge: glow(scene, [1.5, 1.15, 0.7]),
            inner: glow(scene, [1.1, 0.85, 0.55]),
            arms: glow(scene, [0.3, 0.5, 1.4]),
            nebula: glow(scene, [1.6, 0.3, 0.6]),
        },
        // Tipped steeply, so its debris makes a broad fan instead.
        Galaxy {
            centre: scale(start, -1.0),
            velocity: scale(velocity, -1.0),
            normal: normalize([0.4, -0.5, 0.75]),
            bulge: glow(scene, [1.5, 1.05, 0.8]),
            inner: glow(scene, [1.1, 0.75, 0.6]),
            arms: glow(scene, [0.75, 0.35, 1.4]),
            nebula: glow(scene, [1.6, 0.35, 0.35]),
        },
    ];

    let shape = CubeSpawn::new().class("galaxy_star");
    let count = star_count();
    let velocities: Vec<_> = (0..count)
        .map(|index| spawn_star(scene, &galaxies[index % 2], index, &shape))
        .collect();
    scene.apply_gpu_physics_to_class(
        "galaxy_star",
        &GpuBodySettings {
            // Stars are far apart: galaxies pass through each other.
            solver: PhysicsSolver::NoCollision,
            rigid_body: RigidBody {
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
            ..GpuBodySettings::default()
        },
    );
    for (index, velocity) in velocities.into_iter().enumerate() {
        scene.set_linear_velocity(&format!("Star {index}"), velocity);
    }
    println!("galaxy collision with {count} stars");
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_galaxy_collision", create);
    let path = core_path();
    let mut params = Vec::with_capacity(path.samples.len() + 1);
    params.push([
        GM,
        STAR_SOFTENING * STAR_SOFTENING,
        SAMPLE_DT,
        path.samples.len() as f32,
    ]);
    params.extend_from_slice(&path.samples);
    scene.set_gpu_condition_shaders(vec![GpuConditionShader {
        events: Vec::new(),
        glsl: GRAVITY.into(),
        params,
        events_per_body: 1,
    }]);

    // A slow turn that sinks from above toward the orbit plane and closes
    // in as the cores merge.
    let t = time.elapsed_seconds();
    let close = (t / 30.0).clamp(0.0, 1.0);
    let ease = close * close * (3.0 - 2.0 * close);
    let reach = if portrait() { 1.9 } else { 1.0 };
    let distance = reach * (195.0 - 20.0 * ease);
    let angle = -1.9 + 0.035 * t;
    let elevation = 1.0 - 0.45 * ease;
    let position = [
        distance * elevation.cos() * angle.cos(),
        distance * elevation.sin(),
        distance * elevation.cos() * angle.sin(),
    ];
    scene
        .object("Game Camera")
        .set_position(position)
        .look_at([0.0; 3]);
}

rusting_game!("testGame/build/main.rscene.bin", update);
