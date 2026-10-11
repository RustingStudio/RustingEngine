//! A black hole with a glowing accretion disk and planets falling in.
//!
//! ```sh
//! cargo run --release --features window --example black_hole -- 60000
//! ```
//!
//! The argument is the number of disk particles (default 60000); add
//! `--no-planets` for the disk alone. Every body
//! is a GPU body with gravity off; one condition shader pulls them all toward
//! the hole with Newtonian gravity, so the disk turns faster on the inside
//! than on the outside. Planets also feel a little drag, spiral in, and
//! reappear far out once they cross the horizon, so the scene loops forever.
//! The camera follows game time, so a replay films the same shot.

use rusting_engine::prelude::*;
use rusting_engine::runtime::{Bloom, PointLight};
use std::f32::consts::TAU;

/// G times the hole's mass, in m³/s².
const GM: f32 = 4000.0;
/// Event horizon radius: planets that cross it are swallowed.
const HORIZON: f32 = 6.0;
/// The black disk seen around the hole: light that passes closer than about
/// 2.6 horizon radii falls in, so the shadow is wider than the horizon.
/// ponytail: drawn as a black sphere, no lensing; a real lensed image needs
/// a post-process pass.
const SHADOW: f32 = 9.0;
/// Light orbiting just outside the shadow, seen as a thin bright ring.
const PHOTON_RING: f32 = 9.2;
/// Inner edge of the disk: the innermost stable orbit, three horizon radii.
const DISK_INNER: f32 = 18.0;
const DISK_OUTER: f32 = 85.0;
/// Bodies at least this heavy are planets.
const PLANET_MASS: f32 = 10.0;

const GRAVITY: &str = r#"
float hash01(uint value) {
    value ^= value >> 16u; value *= 0x7feb352du;
    value ^= value >> 15u; value *= 0x846ca68bu;
    value ^= value >> 16u;
    return float(value) / 4294967295.0;
}

void condition(inout PhysicsState body) {
    vec4 hole = condition_params.values[0]; // x GM, y horizon, z planet drag, w respawn radius
    vec3 position = body.model[3].xyz;
    float distance = max(length(position), 0.5);
    body.velocity.xyz -= position * (hole.x / (distance * distance * distance)) * pc.dt;
    if (body.properties.x < 5.0) return;
    // Planets lose orbital energy, so they spiral in.
    body.velocity.xyz *= 1.0 - hole.z * pc.dt;
    if (distance > hole.y) return;
    // Swallowed: come back far out on a fresh, eccentric orbit.
    float angle = 6.2831853 * hash01(body.metadata.x * 7919u + pc.tick_low);
    float tilt = (hash01(body.metadata.x + pc.tick_low * 31u) - 0.5) * 0.3;
    vec3 outward = vec3(cos(angle), tilt, sin(angle));
    vec3 along = normalize(vec3(-sin(angle), 0.0, cos(angle)));
    body.model[3].xyz = outward * hole.w;
    body.velocity.xyz = along * sqrt(hole.x / hole.w) * 0.8;
    body.angular_velocity.w = 0.0;
    emit_event(body, EVENTS[0], 1u, vec4(position, 0.0));
}
"#;

fn particle_count() -> usize {
    std::env::args()
        .nth(1)
        .filter(|arg| !arg.starts_with("--"))
        .map_or(60_000, |count| count.parse().expect("particle count"))
}

fn with_planets() -> bool {
    !std::env::args().any(|arg| arg == "--no-planets")
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

fn circular_velocity(position: [f32; 3]) -> [f32; 3] {
    let radius = position[0].hypot(position[2]);
    let speed = (GM / radius).sqrt();
    [
        -position[2] / radius * speed,
        0.0,
        position[0] / radius * speed,
    ]
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

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.0, 0.0, 0.0, 1.0]);
    scene.world().spawn(Bloom {
        intensity: 0.45,
        threshold: 1.2,
        spread: 0.55,
    });
    // The disk lights the planets from the middle.
    scene.world().spawn((
        Transform::new([0.0, 0.0, 0.0]),
        PointLight {
            color: [1.0, 0.9, 0.8],
            intensity: 2500.0,
            range: 400.0,
            ..PointLight::default()
        },
    ));

    let horizon = glow(scene, [0.0; 3]);
    scene.spawn_sphere_with_material(
        "Shadow",
        Transform {
            scale: [2.0 * SHADOW; 3],
            ..Transform::default()
        },
        &SphereSpawn::new().subdivisions(96),
        horizon,
    );
    // The photon ring looks the same from every side, so it is a flat ring
    // that turns to face the camera each frame (see `update`). Its parent is
    // a unit cube hidden inside the shadow.
    scene.spawn_cube("Photon Ring", Transform::default(), &CubeSpawn::new());
    for (ring, radius, glow_color, size, count) in [
        (0, PHOTON_RING, [7.0, 5.6, 4.2], 0.3, 900),
        (1, PHOTON_RING + 0.35, [2.4, 1.3, 0.5], 0.6, 700),
    ] {
        let material = glow(scene, glow_color);
        for index in 0..count {
            let angle = TAU * index as f32 / count as f32;
            let name = format!("Ring {ring} {index}");
            let mut transform = Transform::new([
                radius * angle.cos(),
                radius * angle.sin(),
                0.0,
            ])
            .with_rotation(0.0, 0.0, angle);
            transform.scale = [size; 3];
            scene.spawn_cube_with_material(
                name.clone(),
                transform,
                &CubeSpawn::new(),
                material,
            );
            scene.reparent(&name, Some("Photon Ring"));
        }
    }

    // Far stars, not simulated.
    let star = glow(scene, [1.5, 1.6, 2.0]);
    let stars = CubeSpawn::new();
    for index in 0..3000 {
        let (u, v) = (noise(index, 10), noise(index, 11));
        let (theta, z) = (TAU * u, 2.0 * v - 1.0);
        let ring = (1.0 - z * z).sqrt();
        let mut transform = Transform::new(
            [ring * theta.cos(), z, ring * theta.sin()]
                .map(|axis| axis * 900.0),
        );
        transform.scale = [0.6 + 1.2 * noise(index, 12); 3];
        scene.spawn_cube_with_material(
            format!("Star {index}"),
            transform,
            &stars,
            star,
        );
    }

    // Hotter, brighter bands toward the inner edge.
    let bands = [
        (23.0, [2.4, 2.1, 1.8]),
        (31.0, [2.2, 1.1, 0.35]),
        (42.0, [1.8, 0.55, 0.1]),
        (57.0, [1.0, 0.22, 0.04]),
        (f32::MAX, [0.45, 0.08, 0.02]),
    ]
    .map(|(edge, color)| (edge, glow(scene, color)));
    let disk = CubeSpawn::new().class("hole_body");
    let count = particle_count();
    let mut velocities = Vec::with_capacity(count);
    for index in 0..count {
        // Squared noise packs more particles toward the inner edge.
        let radius =
            DISK_INNER + (DISK_OUTER - DISK_INNER) * noise(index, 0).powf(1.6);
        let angle = TAU * noise(index, 1);
        let height = (noise(index, 2) - 0.5) * 0.06 * radius;
        let position = [radius * angle.cos(), height, radius * angle.sin()];
        let mut transform = Transform::new(position).with_rotation(
            TAU * noise(index, 3),
            TAU * noise(index, 4),
            0.0,
        );
        transform.scale = [0.2 + 0.3 * noise(index, 5); 3];
        // Jittered so the bands blend instead of making hard rings.
        let heat = radius * (0.85 + 0.3 * noise(index, 6));
        let material = bands.iter().find(|(edge, _)| heat < *edge).unwrap().1;
        scene.spawn_cube_with_material(
            format!("Disk {index}"),
            transform,
            &disk,
            material,
        );
        velocities.push(circular_velocity(position));
    }

    let planets = if with_planets() {
        [
            // Color, radius, distance, angle, share of the circular speed: the
            // slower ones plunge first.
            ([0.15, 0.35, 0.9], 4.5, 90.0, 0.0_f32, 0.35),
            ([0.85, 0.3, 0.1], 3.5, 100.0, 1.3, 0.5),
            ([0.1, 0.7, 0.6], 2.5, 80.0, 2.4, 0.6),
            ([0.95, 0.8, 0.45], 5.5, 115.0, 3.6, 0.45),
            ([0.5, 0.2, 0.8], 2.0, 95.0, 4.9, 0.7),
        ]
        .to_vec()
    } else {
        Vec::new()
    };
    let planet = SphereSpawn::new().subdivisions(64).class("hole_planet");
    for (index, (color, size, radius, angle, _)) in
        planets.iter().copied().enumerate()
    {
        let material = scene.create_material(MaterialAsset {
            base_color: [color[0], color[1], color[2], 1.0],
            roughness: 0.8,
            ..MaterialAsset::default()
        });
        let position = [
            radius * angle.cos(),
            4.0 - 2.0 * index as f32,
            radius * angle.sin(),
        ];
        let mut transform = Transform::new(position);
        transform.scale = [2.0 * size; 3];
        scene.spawn_sphere_with_material(
            format!("Planet {index}"),
            transform,
            &planet,
            material,
        );
    }

    for (class, mass) in [("hole_body", 1.0), ("hole_planet", PLANET_MASS)] {
        scene.apply_gpu_physics_to_class(
            class,
            &GpuBodySettings {
                // No contacts: the disk is gas, and planets pass through it.
                solver: PhysicsSolver::NoCollision,
                rigid_body: RigidBody {
                    mass,
                    gravity_scale: 0.0,
                    ..RigidBody::default()
                },
                ..GpuBodySettings::default()
            },
        );
    }
    for (index, velocity) in velocities.into_iter().enumerate() {
        scene.set_linear_velocity(&format!("Disk {index}"), velocity);
    }
    for (index, (.., share)) in planets.into_iter().enumerate() {
        let position = scene.object(&format!("Planet {index}")).position();
        let velocity = circular_velocity(position).map(|axis| axis * share);
        scene.set_linear_velocity(&format!("Planet {index}"), velocity);
    }
    println!("black hole with {count} disk particles");
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_black_hole", create);
    scene.set_gpu_condition_shaders(vec![GpuConditionShader {
        events: vec!["swallowed".into()],
        glsl: GRAVITY.into(),
        params: vec![[GM, HORIZON, 0.1, 130.0]],
        events_per_body: 1,
    }]);
    for event in scene.gpu_events("swallowed") {
        println!("planet swallowed at tick {}", event.tick);
    }

    // Slow orbit that dives from high above toward the disk plane and back.
    let t = time.elapsed_seconds();
    let angle = 0.6 + 0.05 * t;
    let dive = 0.5 - 0.5 * (t * TAU / 50.0).cos();
    let distance = 135.0 - 60.0 * dive;
    let elevation = 0.75 - 0.5 * dive;
    let position = [
        distance * elevation.cos() * angle.cos(),
        distance * elevation.sin(),
        distance * elevation.cos() * angle.sin(),
    ];
    scene
        .object("Game Camera")
        .set_position(position)
        .look_at([0.0; 3]);
    scene
        .object("Photon Ring")
        .set_position([0.0; 3])
        .look_at(position);
}

rusting_game!("testGame/build/main.rscene.bin", update);
