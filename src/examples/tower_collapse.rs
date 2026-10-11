//! A tower of 50,000 colliding cubes is blown at its base and falls into a
//! pile.
//!
//! ```sh
//! cargo run --release --features window --example tower_collapse
//! ```
//!
//! Add `--portrait` to frame the shot for a 9:16 video. The tower is
//! 25 × 25 cubes wide and 80 layers tall. Every cube is a GPU body with full
//! contacts. A condition shader is the demolition charge: at `BLAST_AT`
//! seconds it kicks the bottom layers outward for one tick, and the rest of
//! the tower comes down on its own. The GPU solver never rotates bodies, so
//! the columns do not tip over; they pour out at the base and spread across
//! the floor. The camera rises at the end to show the whole debris field. It
//! follows game time and the centre of the pile, which creeps sideways as it
//! settles, so a replay films the same shot.

use rusting_engine::prelude::*;
use rusting_engine::runtime::{Collider, DirectionalLight, RigidBodyKind};

const SIDE: usize = 25;
const LAYERS: usize = 80;
/// Gap between neighbouring cubes in a layer, so they start apart.
const GAP: f32 = 0.02;
const BLAST_AT: f32 = 2.5;
/// Layers below this height (metres) get the kick.
const BLAST_HEIGHT: f32 = 6.0;
const BLAST_SPEED: f32 = 10.0;
/// Every `TRACK_EVERY`th cube reports its position, so the camera can follow
/// the pile: the GPU contact pass lets a big heap creep sideways.
const TRACK_EVERY: usize = 250;
const CHARGE: &str = r#"
void condition(inout PhysicsState body) {
    vec4 blast = condition_params.values[0]; // x time, y height, z speed
    if (pc.elapsed < blast.x || pc.elapsed >= blast.x + pc.dt) return;
    vec3 position = body.model[3].xyz;
    if (position.y > blast.y) return;
    vec2 out_dir = normalize(position.xz + vec2(0.01, 0.013));
    float strength = 1.0 - position.y / blast.y;
    body.velocity.xyz += vec3(out_dir.x, 0.25, out_dir.y) * blast.z * strength;
}
"#;

fn portrait() -> bool {
    std::env::args().any(|arg| arg == "--portrait")
}

/// High friction, so the rubble heaps up instead of sliding flat.
fn rough() -> Collider {
    Collider {
        friction: 1.0,
        ..Collider::default()
    }
}

/// ECS rotation whose -Z axis points along `direction`.
fn facing(direction: [f32; 3]) -> [f32; 3] {
    let length = direction.iter().map(|axis| axis * axis).sum::<f32>().sqrt();
    let [x, y, z] = direction.map(|axis| axis / length);
    [y.asin(), (-x).atan2(-z), 0.0]
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.02, 0.025, 0.035, 1.0]);
    scene.world().spawn((
        Transform {
            rotation: facing([-0.5, -1.0, -0.35]),
            ..Transform::default()
        },
        DirectionalLight {
            illuminance: 60_000.0,
            ..DirectionalLight::default()
        },
    ));

    let ground_material = scene.create_material(MaterialAsset {
        base_color: [0.12, 0.12, 0.13, 1.0],
        roughness: 0.9,
        ..MaterialAsset::default()
    });
    let mut ground = Transform::new([0.0, -0.5, 0.0]);
    ground.scale = [400.0, 1.0, 400.0];
    scene.spawn_cube_with_material(
        "Ground",
        ground,
        &CubeSpawn::new().class("tower_ground"),
        ground_material,
    );
    scene.apply_gpu_physics_to_class(
        "tower_ground",
        &GpuBodySettings {
            rigid_body: RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            collider: rough(),
            ..GpuBodySettings::default()
        },
    );

    // Warm at the base, cool at the top, in five bands.
    let bands = [
        [0.9, 0.25, 0.1],
        [0.95, 0.6, 0.12],
        [0.85, 0.85, 0.8],
        [0.2, 0.6, 0.85],
        [0.25, 0.3, 0.8],
    ]
    .map(|color| {
        scene.create_material(MaterialAsset {
            base_color: [color[0], color[1], color[2], 1.0],
            roughness: 0.55,
            ..MaterialAsset::default()
        })
    });
    let shape = CubeSpawn::new().class("tower_cube");
    let tracker = CubeSpawn::new().class("tower_tracker");
    let pitch = 1.0 + GAP;
    let centre = (SIDE - 1) as f32 / 2.0;
    for layer in 0..LAYERS {
        let material = bands[layer * bands.len() / LAYERS];
        for cell in 0..SIDE * SIDE {
            let x = ((cell % SIDE) as f32 - centre) * pitch;
            let z = ((cell / SIDE) as f32 - centre) * pitch;
            let index = layer * SIDE * SIDE + cell;
            scene.spawn_cube_with_material(
                format!("Cube {index}"),
                Transform::new([x, 0.5 + layer as f32, z]),
                if index.is_multiple_of(TRACK_EVERY) {
                    &tracker
                } else {
                    &shape
                },
                material,
            );
        }
    }
    let cubes = GpuBodySettings {
        solver: PhysicsSolver::Full,
        collider: rough(),
        ..GpuBodySettings::default()
    };
    scene.apply_gpu_physics_to_class(
        "tower_tracker",
        &GpuBodySettings {
            sync: PhysicsSyncMode::SelectedState,
            ..cubes.clone()
        },
    );
    scene.apply_gpu_physics_to_class("tower_cube", &cubes);
    println!("tower of {} cubes", SIDE * SIDE * LAYERS);
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_tower_collapse", create);
    scene.set_gpu_condition_shaders(vec![GpuConditionShader {
        events: Vec::new(),
        glsl: CHARGE.into(),
        params: vec![[BLAST_AT, BLAST_HEIGHT, BLAST_SPEED, 0.0]],
        events_per_body: 1,
    }]);

    // Ground-plane centre of the tracked cubes, eased so a straggler does
    // not jerk the shot.
    let mut sum = [0.0_f32; 2];
    let mut count = 0.0_f32;
    for index in (0..SIDE * SIDE * LAYERS).step_by(TRACK_EVERY) {
        if let Some(state) = scene.gpu_state(&format!("Cube {index}")) {
            sum[0] += state.transform.position[0];
            sum[1] += state.transform.position[2];
            count += 1.0;
        }
    }
    let focus = FOCUS.with(|focus| {
        let mut current = focus.get();
        if count > 0.0 {
            let ease = (2.0 * time.delta_seconds()).min(1.0);
            for axis in 0..2 {
                current[axis] += (sum[axis] / count - current[axis]) * ease;
            }
            focus.set(current);
        }
        current
    });

    // A slow turn around the tower. As it comes down the camera aims lower,
    // then rises and pulls back to look down on the debris field.
    let t = time.elapsed_seconds();
    let smooth = |from: f32, to: f32| {
        let k = ((t - from) / (to - from)).clamp(0.0, 1.0);
        k * k * (3.0 - 2.0 * k)
    };
    let fall = smooth(BLAST_AT, BLAST_AT + 8.0);
    let rise = smooth(16.0, 28.0);
    let reach = if portrait() { 1.0 } else { 0.8 };
    let distance = reach * (170.0 - 40.0 * fall + 60.0 * rise);
    let angle = 0.7 + 0.04 * t;
    let height = 55.0 - 15.0 * fall + 110.0 * rise;
    let aim = 38.0 - 32.0 * fall - 6.0 * rise;
    let position = [
        focus[0] + distance * angle.cos(),
        height,
        focus[1] + distance * angle.sin(),
    ];
    scene
        .object("Game Camera")
        .set_position(position)
        .look_at([focus[0], aim, focus[1]]);
}

thread_local! {
    static FOCUS: std::cell::Cell<[f32; 2]> = const { std::cell::Cell::new([0.0; 2]) };
}

rusting_game!("testGame/build/main.rscene.bin", update);
