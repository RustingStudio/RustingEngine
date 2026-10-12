//! Standard physics quality scenes, run headless on the CPU solvers.
//!
//! Each [`SuiteScene`] builds the same bodies every run, steps them at
//! 60 Hz and returns a [`SuiteScore`]: the mean wall time of a step and one
//! error number that a correct solver keeps near zero. The
//! `physics_suite` example prints them; the tests hold the error bounds.

use std::time::{Duration, Instant};

use bevy_ecs::entity::Entity;

use super::{
    AxisLock, Cloth, ClothSettings, ClothVolume, Collider, ColliderShape,
    Debris, Fluid, FluidSettings, Fracture, Joint, JointKind, MeshRenderer,
    Name, PhysicsBody, Polygon2d, Ragdoll, RagdollBone, RagdollState,
    RigidBody, RigidBodyKind, SoftBody, SoftBodySettings, SoftBodyVolume,
    Vehicle,
};
use crate::{App, Transform};

/// One standard scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuiteScene {
    /// A tower of `size` touching unit cubes; the error is how far the
    /// cube that moved most drifted from its start, in metres.
    StackingStability,
    /// `size` links on ball sockets, hung level from the world and left to
    /// swing; the error is the widest gap between two joined anchors seen
    /// in any step, in metres.
    JointChain,
    /// `size` 5 cm spheres shot at 300 m/s into a 10 cm wall; the error is
    /// the share that came out the far side.
    CcdBullets,
    /// A `size` by `size` cell sheet, 5 cm cells, hung by its two top
    /// corners; the error is the most any edge stretched past its rest
    /// length in any step, as a fraction of it.
    ClothHang,
    /// A cube of about `size` fluid particles dropped 1 m into a column
    /// that fits it and left to settle; the error is how much lower its
    /// mean particle height ends than at rest spacing, as a fraction of
    /// that height (compression; negative is expansion).
    FluidDrop,
    /// `size` raycast cars on a flat floor, 12 m apart, at full throttle
    /// and full right lock; the error is the most any car's up axis
    /// tilted from vertical in any step, in radians.
    VehicleStress,
    /// `size` 10 kg crates that break into 6 pieces above 100 J, dropped
    /// 3 m onto a floor; the error is the share of the `6 * size` pieces
    /// not resting above the floor's top at the end (sunk, or never
    /// broken off).
    FractureDrop,
    /// `size` seven-bone ragdolls, 2 m apart, dropped limp from standing
    /// onto a floor; the error is the widest gap between joined anchors
    /// in any step, in metres.
    RagdollPile,
    /// The [`JointChain`](Self::JointChain) hung from a fixed anchor that
    /// carries an `Articulation`, so its joints are reduced coordinates;
    /// the error is the widest gap between two joined anchors in any
    /// step, in metres.
    ArticulatedChain,
    /// A player with the default `PlayerController` walks for `steps`
    /// steps (4 m/s) up a stair of `size` 0.2 m steps, each 1 m deep and
    /// the top one 20 m deep, starting 1 m before the first; the error is
    /// how far below the top step's surface its feet end, in metres.
    CharacterStairs,
    /// Rigid with fluid: `size` 0.15 m balls, each dropped into its own
    /// water pool, alternating 7 kg (half as dense as water) and 50 kg;
    /// the error is the share that ended on the wrong side of 0.2 m (a
    /// light ball not floating or a heavy one not on the floor).
    BuoyancySort,
    /// Rigid with deformable: `size` balls of 1, 2, 3, ... kg dropped 1 m
    /// onto their own 1 m cloth sheets pinned at the edges; the error is
    /// the share that ended under the sheet or off it.
    SheetCatch,
    /// Deformable with fluid: `size` 0.3 m jelly cubes, each dropped into
    /// its own pool, alternating 300 and 5000 kg/m³; the error is the
    /// share on the wrong side (a light jelly's bottom under 0.12 m or a
    /// heavy one's over 0.1 m).
    JellyRaft,
}

/// What one run of a [`SuiteScene`] measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SuiteScore {
    pub scene: SuiteScene,
    pub size: usize,
    pub steps: u32,
    pub mean_step: Duration,
    pub error: f32,
}

const DT: f64 = 1.0 / 60.0;

impl SuiteScene {
    pub const ALL: [Self; 13] = [
        Self::StackingStability,
        Self::JointChain,
        Self::CcdBullets,
        Self::ClothHang,
        Self::FluidDrop,
        Self::VehicleStress,
        Self::FractureDrop,
        Self::RagdollPile,
        Self::ArticulatedChain,
        Self::CharacterStairs,
        Self::BuoyancySort,
        Self::SheetCatch,
        Self::JellyRaft,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::StackingStability => "stacking_stability",
            Self::JointChain => "joint_chain",
            Self::CcdBullets => "ccd_bullets",
            Self::ClothHang => "cloth_hang",
            Self::FluidDrop => "fluid_drop",
            Self::VehicleStress => "vehicle_stress",
            Self::FractureDrop => "fracture_drop",
            Self::RagdollPile => "ragdoll_pile",
            Self::ArticulatedChain => "articulated_chain",
            Self::CharacterStairs => "character_stairs",
            Self::BuoyancySort => "buoyancy_sort",
            Self::SheetCatch => "sheet_catch",
            Self::JellyRaft => "jelly_raft",
        }
    }

    /// What [`SuiteScore::error`] counts.
    #[must_use]
    pub fn error_unit(self) -> &'static str {
        match self {
            Self::StackingStability => "max drift (m)",
            Self::JointChain => "max joint gap (m)",
            Self::CcdBullets => "tunnelled share",
            Self::ClothHang => "max edge strain",
            Self::FluidDrop => "height loss",
            Self::VehicleStress => "max tilt (rad)",
            Self::FractureDrop => "missing share",
            Self::RagdollPile => "max joint gap (m)",
            Self::ArticulatedChain => "max joint gap (m)",
            Self::CharacterStairs => "height short (m)",
            Self::BuoyancySort | Self::SheetCatch | Self::JellyRaft => {
                "wrong share"
            }
        }
    }

    /// Builds the scene with `size` bodies and runs it for `steps` steps.
    #[must_use]
    pub fn run(self, size: usize, steps: u32) -> SuiteScore {
        let mut app = App::new();
        let mut elapsed = Duration::ZERO;
        let step = |app: &mut App, elapsed: &mut Duration| {
            let start = Instant::now();
            app.update(Duration::from_secs_f64(DT)).unwrap();
            *elapsed += start.elapsed();
        };
        let error = match self {
            Self::StackingStability => {
                body(
                    &mut app,
                    [0.0, -0.5, 0.0],
                    ColliderShape::Box {
                        half_extents: [5.0, 0.5, 5.0],
                    },
                    RigidBodyKind::Fixed,
                );
                let cubes: Vec<_> = (0..size)
                    .map(|level| {
                        let start = [0.0, 0.5 + level as f32, 0.0];
                        let cube = body(
                            &mut app,
                            start,
                            ColliderShape::Box {
                                half_extents: [0.5; 3],
                            },
                            RigidBodyKind::Dynamic,
                        );
                        (cube, start)
                    })
                    .collect();
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                cubes
                    .iter()
                    .map(|&(cube, start)| distance(position(&app, cube), start))
                    .fold(0.0, f32::max)
            }
            Self::JointChain | Self::ArticulatedChain => {
                const HALF: f32 = 0.25;
                let anchor = (self == Self::ArticulatedChain).then(|| {
                    let anchor = body(
                        &mut app,
                        [0.0, 10.0, 0.0],
                        ColliderShape::Sphere { radius: 0.05 },
                        RigidBodyKind::Fixed,
                    );
                    app.world_mut()
                        .entity_mut(anchor)
                        .insert(super::Articulation::default());
                    anchor
                });
                let mut links: Vec<(Entity, Option<Entity>)> = Vec::new();
                for index in 0..size {
                    let x = HALF * (2 * index + 1) as f32;
                    let link = body(
                        &mut app,
                        [x, 10.0, 0.0],
                        ColliderShape::Box {
                            half_extents: [HALF, 0.05, 0.05],
                        },
                        RigidBodyKind::Dynamic,
                    );
                    let previous = links.last().map(|&(link, _)| link);
                    let joint = Joint::new(
                        JointKind::BallSocket,
                        previous.or(anchor).unwrap_or(Entity::PLACEHOLDER),
                        [-HALF, 0.0, 0.0],
                        match (previous, anchor) {
                            (Some(_), _) => [HALF, 0.0, 0.0],
                            (None, Some(_)) => [0.0; 3],
                            (None, None) => [0.0, 10.0, 0.0],
                        },
                    );
                    app.world_mut().entity_mut(link).insert(joint);
                    links.push((link, previous));
                }
                let mut widest = 0.0_f32;
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                    for &(link, previous) in &links {
                        let anchor = point(&app, link, [-HALF, 0.0, 0.0]);
                        let target = previous.map_or([0.0, 10.0, 0.0], |p| {
                            point(&app, p, [HALF, 0.0, 0.0])
                        });
                        widest = widest.max(distance(anchor, target));
                    }
                }
                widest
            }
            Self::CharacterStairs => {
                const RISE: f32 = 0.2;
                body(
                    &mut app,
                    [0.0, -0.5, 0.0],
                    ColliderShape::Box {
                        half_extents: [3.0, 0.5, size as f32 + 20.0],
                    },
                    RigidBodyKind::Fixed,
                );
                // Step `k` (from 1) is a block from the floor to `k * RISE`,
                // 1 m deep, toward -Z; the last one runs on for 20 m.
                for k in 1..=size {
                    let height = k as f32 * RISE;
                    let depth = if k == size { 20.0 } else { 1.0 };
                    body(
                        &mut app,
                        [0.0, height / 2.0, -(k as f32) - depth / 2.0 + 1.0],
                        ColliderShape::Box {
                            half_extents: [3.0, height / 2.0, depth / 2.0],
                        },
                        RigidBodyKind::Fixed,
                    );
                }
                let player = body(
                    &mut app,
                    [0.0, 0.95, 1.0],
                    super::DEFAULT_PLAYER_SHAPE,
                    RigidBodyKind::Kinematic,
                );
                app.world_mut()
                    .entity_mut(player)
                    .insert(super::PlayerController::default());
                app.world_mut()
                    .resource_mut::<super::RuntimeInput>()
                    .record_key(super::KeyCode::KeyW, true);
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                // The default capsule is 1.8 m tall around its centre.
                size as f32 * RISE - (position(&app, player)[1] - 0.9)
            }
            Self::BuoyancySort => {
                body(
                    &mut app,
                    [0.0, -0.5, 0.0],
                    ColliderShape::Box {
                        half_extents: [size as f32 + 1.0, 0.5, 1.0],
                    },
                    RigidBodyKind::Fixed,
                );
                let balls: Vec<_> = (0..size)
                    .map(|index| {
                        let x = 2.0 * index as f32;
                        pool(&mut app, x);
                        let ball = body(
                            &mut app,
                            [x, 0.6, 0.0],
                            ColliderShape::Sphere { radius: 0.15 },
                            RigidBodyKind::Dynamic,
                        );
                        let light = index % 2 == 0;
                        app.world_mut()
                            .get_mut::<RigidBody>(ball)
                            .unwrap()
                            .mass = if light { 7.0 } else { 50.0 };
                        (ball, light)
                    })
                    .collect();
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                let wrong = balls
                    .iter()
                    .filter(|&&(ball, light)| {
                        (position(&app, ball)[1] > 0.2) != light
                    })
                    .count();
                wrong as f32 / size.max(1) as f32
            }
            Self::SheetCatch => {
                let balls: Vec<_> = (0..size)
                    .map(|index| {
                        let x = 2.0 * index as f32;
                        let mut sheet = Cloth::grid(
                            [x - 0.5, 1.0, -0.5],
                            [1.0, 0.0, 0.0],
                            [0.0, 0.0, 1.0],
                            [10, 10],
                            0.1,
                            10.0,
                        )
                        .unwrap();
                        for i in 0..121 {
                            let (u, v) = (i % 11, i / 11);
                            if u == 0 || u == 10 || v == 0 || v == 10 {
                                sheet.pin(i);
                            }
                        }
                        app.spawn(ClothVolume::new(
                            ClothSettings::default(),
                            sheet,
                        ));
                        let ball = body(
                            &mut app,
                            [x, 2.0, 0.0],
                            ColliderShape::Sphere { radius: 0.15 },
                            RigidBodyKind::Dynamic,
                        );
                        app.world_mut()
                            .get_mut::<RigidBody>(ball)
                            .unwrap()
                            .mass = 1.0 + index as f32;
                        (ball, x)
                    })
                    .collect();
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                let wrong = balls
                    .iter()
                    .filter(|&&(ball, x)| {
                        let [bx, by, bz] = position(&app, ball);
                        // On the sheet: above where it sags to, inside it.
                        !(by > 0.5 && (bx - x).abs() < 0.5 && bz.abs() < 0.5)
                    })
                    .count();
                wrong as f32 / size.max(1) as f32
            }
            Self::JellyRaft => {
                let jellies: Vec<_> = (0..size)
                    .map(|index| {
                        let x = 2.0 * index as f32;
                        pool(&mut app, x);
                        let light = index % 2 == 0;
                        let jelly = SoftBody::block(
                            [x - 0.15, 0.6, -0.15],
                            [3; 3],
                            0.1,
                            if light { 300.0 } else { 5000.0 },
                        )
                        .unwrap();
                        let jelly = app.spawn(SoftBodyVolume::new(
                            SoftBodySettings::default(),
                            jelly,
                        ));
                        (jelly, light)
                    })
                    .collect();
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                let wrong = jellies
                    .iter()
                    .filter(|&&(jelly, light)| {
                        let bottom = app
                            .world()
                            .get::<SoftBodyVolume>(jelly)
                            .unwrap()
                            .body
                            .positions
                            .iter()
                            .map(|p| p[1])
                            .fold(f32::INFINITY, f32::min);
                        if light {
                            bottom < 0.12
                        } else {
                            bottom > 0.1
                        }
                    })
                    .count();
                wrong as f32 / size.max(1) as f32
            }
            Self::CcdBullets => {
                body(
                    &mut app,
                    [0.0, 2.0, 0.0],
                    ColliderShape::Box {
                        half_extents: [4.0, 2.0, 0.05],
                    },
                    RigidBodyKind::Fixed,
                );
                let side = (size as f32).sqrt().ceil() as usize;
                let bullets: Vec<_> = (0..size)
                    .map(|index| {
                        let spread = 3.0 / side.max(1) as f32;
                        let x = -1.5 + spread * (index % side) as f32;
                        let y = 0.5 + spread * (index / side) as f32;
                        let bullet = body(
                            &mut app,
                            [x, y, 5.0],
                            ColliderShape::Sphere { radius: 0.05 },
                            RigidBodyKind::Dynamic,
                        );
                        app.world_mut()
                            .get_mut::<RigidBody>(bullet)
                            .unwrap()
                            .linear_velocity = [0.0, 0.0, -300.0];
                        bullet
                    })
                    .collect();
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                let through = bullets
                    .iter()
                    .filter(|&&bullet| position(&app, bullet)[2] < -0.05)
                    .count();
                through as f32 / size.max(1) as f32
            }
            Self::ClothHang => {
                const SPACING: f32 = 0.05;
                let cells = size.max(1) as u32;
                let mut cloth = Cloth::grid(
                    [0.0, 2.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0],
                    [cells; 2],
                    SPACING,
                    0.2,
                )
                .unwrap();
                cloth.pin(0);
                cloth.pin(cells as usize);
                let edges = cloth.edges().to_vec();
                let rests: Vec<f32> = edges
                    .iter()
                    .map(|&[a, b]| edge_length(&cloth.positions, a, b))
                    .collect();
                let settings = ClothSettings {
                    floor: None,
                    ..ClothSettings::default()
                };
                let mut worst = 0.0_f32;
                for _ in 0..steps {
                    let start = Instant::now();
                    cloth.step(&settings, DT as f32);
                    elapsed += start.elapsed();
                    for (&[a, b], rest) in edges.iter().zip(&rests) {
                        let length = edge_length(&cloth.positions, a, b);
                        worst = worst.max(length / rest - 1.0);
                    }
                }
                worst
            }
            Self::FluidDrop => {
                let side = (size as f32).cbrt().ceil().max(2.0) as u32;
                let spacing = FluidSettings::default().spacing;
                let width = (side - 1) as f32 * spacing;
                let settings = FluidSettings {
                    bounds: ([0.0; 3], [width, 4.0 * width + 1.0, width]),
                    ..FluidSettings::default()
                };
                let mut fluid =
                    Fluid::block([0.0, 1.0, 0.0], [side; 3], spacing);
                for _ in 0..steps {
                    let start = Instant::now();
                    fluid.step(&settings, DT as f32);
                    elapsed += start.elapsed();
                }
                // The column fits the block, so settled at rest spacing its
                // particles average half the block's height.
                let mean_height =
                    fluid.positions.iter().map(|p| p[1]).sum::<f32>()
                        / fluid.positions.len() as f32;
                1.0 - mean_height / (width / 2.0)
            }
            Self::VehicleStress => {
                let side = (size as f32).sqrt().ceil().max(1.0) as usize;
                let half = 6.0 * side as f32 + 50.0;
                body(
                    &mut app,
                    [0.0, -0.5, 0.0],
                    ColliderShape::Box {
                        half_extents: [half, 0.5, half],
                    },
                    RigidBodyKind::Fixed,
                );
                let cars: Vec<_> = (0..size)
                    .map(|index| {
                        let x = 12.0 * (index % side) as f32;
                        let z = 12.0 * (index / side) as f32;
                        let car = body(
                            &mut app,
                            [x - 6.0 * side as f32, 1.0, z - 6.0 * side as f32],
                            ColliderShape::Box {
                                half_extents: [0.9, 0.3, 2.0],
                            },
                            RigidBodyKind::Dynamic,
                        );
                        let world = app.world_mut();
                        world.get_mut::<RigidBody>(car).unwrap().mass = 1200.0;
                        world.entity_mut(car).insert(Vehicle {
                            throttle: 1.0,
                            steer: 1.0,
                            ..Vehicle::default()
                        });
                        car
                    })
                    .collect();
                let mut worst = 0.0_f32;
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                    for &car in &cars {
                        let up = point(&app, car, [0.0, 1.0, 0.0])[1]
                            - position(&app, car)[1];
                        worst = worst.max(up.clamp(-1.0, 1.0).acos());
                    }
                }
                worst
            }
            Self::FractureDrop => {
                app.add_plugin(crate::assets::AssetPlugin).unwrap();
                let half = 1.5 * size as f32 + 5.0;
                body(
                    &mut app,
                    [0.0, -0.5, 0.0],
                    ColliderShape::Box {
                        half_extents: [half, 0.5, 5.0],
                    },
                    RigidBodyKind::Fixed,
                );
                for index in 0..size {
                    let world = app.world_mut();
                    let order = super::cpu_physics::next_spawn_order(world);
                    let mut assets =
                        world.resource_mut::<crate::assets::AssetServer>();
                    let renderer = MeshRenderer {
                        mesh: assets.meshes.insert(
                            crate::assets::procedural_primitive_mesh(
                                crate::assets::PrimitiveShape::Cube,
                            ),
                        ),
                        material: assets.fallback_material,
                        cast_shadows: true,
                        receive_shadows: true,
                    };
                    world.spawn((
                        Transform::new([
                            3.0 * index as f32 - 1.5 * size as f32,
                            3.5,
                            0.0,
                        ]),
                        PhysicsBody::default(),
                        RigidBody {
                            mass: 10.0,
                            ..RigidBody::default()
                        },
                        Collider {
                            shape: ColliderShape::ConvexMesh,
                            ..Collider::default()
                        },
                        renderer,
                        Fracture {
                            pieces: 6,
                            break_energy: 100.0,
                            ..Fracture::default()
                        },
                        order,
                    ));
                }
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                }
                let world = app.world_mut();
                let above = world
                    .query::<(&Transform, &Debris)>()
                    .iter(world)
                    .filter(|(transform, _)| transform.position[1] > 0.0)
                    .count();
                1.0 - above as f32 / (6 * size).max(1) as f32
            }
            Self::RagdollPile => {
                let side = (size as f32).sqrt().ceil().max(1.0) as usize;
                let half = side as f32 + 5.0;
                body(
                    &mut app,
                    [0.0, -0.5, 0.0],
                    ColliderShape::Box {
                        half_extents: [half, 0.5, half],
                    },
                    RigidBodyKind::Fixed,
                );
                let heroes: Vec<_> = (0..size)
                    .map(|index| {
                        let x = 2.0 * (index % side) as f32 - side as f32;
                        let z = 2.0 * (index / side) as f32 - side as f32;
                        ragdoll(&mut app, [x, 0.9, z])
                    })
                    .collect();
                let mut widest = 0.0_f32;
                for _ in 0..steps {
                    step(&mut app, &mut elapsed);
                    let mut joints = 0;
                    for &hero in &heroes {
                        let world = app.world();
                        let parts =
                            &world.get::<RagdollState>(hero).unwrap().parts;
                        for &part in parts {
                            let Some(joint) = world.get::<Joint>(part) else {
                                continue;
                            };
                            joints += 1;
                            let at = point(&app, part, joint.anchor);
                            let target =
                                point(&app, joint.target, joint.target_anchor);
                            widest = widest.max(distance(at, target));
                        }
                    }
                    // Every bone but the hips hangs from a joint; a missing
                    // one is a ragdoll that came apart or never went limp.
                    if joints != 6 * size {
                        widest = f32::INFINITY;
                    }
                }
                widest
            }
        };
        SuiteScore {
            scene: self,
            size,
            steps,
            mean_step: elapsed / steps.max(1),
            error,
        }
    }

    /// The scenes [`Self::run_2d`] has a 2D form of.
    pub const ALL_2D: [Self; 3] =
        [Self::StackingStability, Self::JointChain, Self::CcdBullets];

    /// Runs the 2D mirror of this scene, or `None` for a scene not in
    /// [`Self::ALL_2D`]. Bodies are `Polygon` outlines (circles for the
    /// bullets) kept in the XY plane by `AxisLock::PLANE_XY`, on a `Chain`
    /// floor or wall; the error means the same as in 3D. A body that leaves
    /// its plane makes the error infinite.
    #[must_use]
    pub fn run_2d(self, size: usize, steps: u32) -> Option<SuiteScore> {
        let mut app = App::new();
        let mut elapsed = Duration::ZERO;
        let mut bodies = Vec::new();
        let error = match self {
            Self::StackingStability => {
                chain(&mut app, [[-5.0, 0.0], [5.0, 0.0]]);
                let boxes: Vec<_> = (0..size)
                    .map(|level| {
                        let start = [0.0, 0.5 + level as f32, 0.0];
                        (body_2d(&mut app, start, 0.5, 0.5), start)
                    })
                    .collect();
                bodies.extend(boxes.iter().map(|&(entity, _)| entity));
                run_steps(&mut app, steps, &mut elapsed);
                boxes
                    .iter()
                    .map(|&(entity, start)| {
                        distance(position(&app, entity), start)
                    })
                    .fold(0.0, f32::max)
            }
            Self::JointChain => {
                const HALF: f32 = 0.25;
                let mut links: Vec<(Entity, Option<Entity>)> = Vec::new();
                for index in 0..size {
                    let x = HALF * (2 * index + 1) as f32;
                    let link = body_2d(&mut app, [x, 10.0, 0.0], HALF, 0.05);
                    let previous = links.last().map(|&(link, _)| link);
                    // A ball socket on a plane-locked body is a 2D pin.
                    let joint = Joint::new(
                        JointKind::BallSocket,
                        previous.unwrap_or(Entity::PLACEHOLDER),
                        [-HALF, 0.0, 0.0],
                        if previous.is_some() {
                            [HALF, 0.0, 0.0]
                        } else {
                            [0.0, 10.0, 0.0]
                        },
                    );
                    app.world_mut().entity_mut(link).insert(joint);
                    links.push((link, previous));
                }
                bodies.extend(links.iter().map(|&(link, _)| link));
                let mut widest = 0.0_f32;
                for _ in 0..steps {
                    run_steps(&mut app, 1, &mut elapsed);
                    for &(link, previous) in &links {
                        let anchor = point(&app, link, [-HALF, 0.0, 0.0]);
                        let target = previous.map_or([0.0, 10.0, 0.0], |p| {
                            point(&app, p, [HALF, 0.0, 0.0])
                        });
                        widest = widest.max(distance(anchor, target));
                    }
                }
                widest
            }
            Self::CcdBullets => {
                // A vertical wall at x 0, shot from x 5 at 300 m/s. Each
                // bullet has its own lane up the wall, as each has its own
                // spot on the 3D wall.
                let top = 1.0 + 0.15 * size as f32;
                chain(&mut app, [[0.0, 0.0], [0.0, top]]);
                let bullets: Vec<_> = (0..size)
                    .map(|index| {
                        let y = 0.5 + 0.15 * index as f32;
                        let bullet = body(
                            &mut app,
                            [5.0, y, 0.0],
                            ColliderShape::Sphere { radius: 0.05 },
                            RigidBodyKind::Dynamic,
                        );
                        let world = app.world_mut();
                        world.entity_mut(bullet).insert(AxisLock::PLANE_XY);
                        world
                            .get_mut::<RigidBody>(bullet)
                            .unwrap()
                            .linear_velocity = [-300.0, 0.0, 0.0];
                        bullet
                    })
                    .collect();
                bodies.extend(&bullets);
                run_steps(&mut app, steps, &mut elapsed);
                let through = bullets
                    .iter()
                    .filter(|&&bullet| position(&app, bullet)[0] < -0.05)
                    .count();
                through as f32 / size.max(1) as f32
            }
            _ => return None,
        };
        let in_plane = bodies.iter().all(|&entity| {
            let transform = app.world().get::<Transform>(entity).unwrap();
            let [x, y, _] = transform.rotation;
            transform.position[2] == 0.0 && x == 0.0 && y == 0.0
        });
        Some(SuiteScore {
            scene: self,
            size,
            steps,
            mean_step: elapsed / steps.max(1),
            error: if in_plane { error } else { f32::INFINITY },
        })
    }
}

fn run_steps(app: &mut App, steps: u32, elapsed: &mut Duration) {
    for _ in 0..steps {
        let start = Instant::now();
        app.update(Duration::from_secs_f64(DT)).unwrap();
        *elapsed += start.elapsed();
    }
}

/// A dynamic `half_x` by `half_y` polygon box locked to the XY plane.
fn body_2d(
    app: &mut App,
    position: [f32; 3],
    half_x: f32,
    half_y: f32,
) -> Entity {
    let entity = body(
        app,
        position,
        ColliderShape::Polygon,
        RigidBodyKind::Dynamic,
    );
    app.world_mut().entity_mut(entity).insert((
        Polygon2d {
            points: vec![
                [-half_x, -half_y],
                [half_x, -half_y],
                [half_x, half_y],
                [-half_x, half_y],
            ],
            ..Polygon2d::default()
        },
        AxisLock::PLANE_XY,
    ));
    entity
}

/// A fixed open `Chain` segment.
fn chain(app: &mut App, points: [[f32; 2]; 2]) {
    let entity =
        body(app, [0.0; 3], ColliderShape::Chain, RigidBodyKind::Fixed);
    app.world_mut().entity_mut(entity).insert(Polygon2d {
        points: points.to_vec(),
        closed: false,
        ..Polygon2d::default()
    });
}

fn body(
    app: &mut App,
    position: [f32; 3],
    shape: ColliderShape,
    kind: RigidBodyKind,
) -> Entity {
    app.spawn((
        Transform::new(position),
        PhysicsBody::default(),
        RigidBody {
            kind,
            ..RigidBody::default()
        },
        Collider {
            shape,
            ..Collider::default()
        },
    ))
}

/// A standing character with a limp seven-bone ragdoll (hips, spine,
/// head, two arms, two legs) whose hips are at `at`.
fn ragdoll(app: &mut App, at: [f32; 3]) -> Entity {
    use std::f32::consts::{FRAC_PI_2, PI};
    // (path, local position, roll about Z, length, radius, mass)
    let bones = [
        ("Hips", [0.0, 0.0, 0.0], 0.0, 0.2, 0.12, 10.0),
        ("Hips/Spine", [0.0, 0.2, 0.0], 0.0, 0.4, 0.12, 12.0),
        ("Hips/Spine/Head", [0.0, 0.4, 0.0], 0.0, 0.25, 0.1, 4.0),
        (
            "Hips/Spine/ArmL",
            [0.15, 0.35, 0.0],
            -FRAC_PI_2,
            0.5,
            0.05,
            3.0,
        ),
        (
            "Hips/Spine/ArmR",
            [-0.15, 0.35, 0.0],
            FRAC_PI_2,
            0.5,
            0.05,
            3.0,
        ),
        ("Hips/LegL", [0.1, 0.0, 0.0], PI, 0.8, 0.07, 8.0),
        ("Hips/LegR", [-0.1, 0.0, 0.0], PI, 0.8, 0.07, 8.0),
    ];
    let hero = app.spawn((
        Transform::new(at),
        Ragdoll {
            bones: bones
                .iter()
                .map(|&(path, _, _, length, radius, mass)| RagdollBone {
                    path: path.into(),
                    length,
                    radius,
                    mass,
                    ..RagdollBone::default()
                })
                .collect(),
            command: Some(true),
            ..Ragdoll::default()
        },
    ));
    for (path, position, roll, ..) in bones {
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let mut transform = Transform::new(position);
        transform.rotation = [0.0, 0.0, roll];
        let bone = app.spawn((Name(name.into()), transform));
        let parent = if parent_path.is_empty() {
            hero
        } else {
            super::find_target(app.world(), hero, parent_path).unwrap()
        };
        app.set_parent(bone, parent).unwrap();
    }
    hero
}

/// A 0.8 m square water pool centred on `x`, 0.4 m deep, 1 kg drops.
fn pool(app: &mut App, x: f32) {
    let settings = FluidSettings {
        bounds: ([x - 0.4, 0.0, -0.4], [x + 0.4, 2.0, 0.4]),
        particle_mass: 1.0,
        ..FluidSettings::default()
    };
    let fluid = Fluid::block([x - 0.35, 0.0, -0.35], [8, 4, 8], 0.1);
    app.spawn(super::FluidVolume::new(settings, fluid));
}

fn position(app: &App, entity: Entity) -> [f32; 3] {
    app.world().get::<Transform>(entity).unwrap().position
}

/// `local` on `entity`, in world space.
fn point(app: &App, entity: Entity, local: [f32; 3]) -> [f32; 3] {
    let transform = app.world().get::<Transform>(entity).unwrap();
    let [x, y, z] = transform.rotation;
    let rotated = super::sim_math::rotation_from_euler(x, y, z)
        * nalgebra::Vector3::from(local);
    (nalgebra::Vector3::from(transform.position) + rotated).into()
}

fn edge_length(points: &[[f32; 3]], a: u32, b: u32) -> f32 {
    distance(points[a as usize], points[b as usize])
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    (nalgebra::Vector3::from(a) - nalgebra::Vector3::from(b)).norm()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_scenes_stay_within_their_error_bounds() {
        let stack = SuiteScene::StackingStability.run(10, 300);
        // Ten cubes drift 3.5 cm; 15 drift 16 cm and 20 fall over, which
        // the example shows.
        assert!(stack.error < 0.05, "{stack:?}");
        let chain = SuiteScene::JointChain.run(20, 300);
        // Baseline, not a goal: a swinging 20-link chain stretches about
        // 0.43 m at its worst (Milestone 10 follow-up in the roadmap).
        assert!(chain.error < 0.5, "{chain:?}");
        let bullets = SuiteScene::CcdBullets.run(25, 30);
        assert_eq!(bullets.error, 0.0, "{bullets:?}");
        let cloth = SuiteScene::ClothHang.run(10, 120);
        assert!(cloth.error < 0.05, "{cloth:?}");
        let fluid = SuiteScene::FluidDrop.run(216, 120);
        // Baseline, not a goal: four density iterations let a 6-particle
        // column sink about a third (1000 particles: a fifth).
        assert!(fluid.error.abs() < 0.4, "{fluid:?}");
        let cars = SuiteScene::VehicleStress.run(4, 240);
        assert!(cars.error < 0.2, "{cars:?}");
        let crates = SuiteScene::FractureDrop.run(4, 90);
        assert_eq!(crates.error, 0.0, "{crates:?}");
        let ragdolls = SuiteScene::RagdollPile.run(4, 120);
        assert!(ragdolls.error < 0.05, "{ragdolls:?}");
    }

    #[test]
    fn regression_scenes_repeat_their_recorded_results() {
        // Recorded on 2026-10-11 with the CPU solver (x86_64). Each run
        // is bit-identical to the one before it; the tolerance only
        // allows for another libm. A change past it is a solver change:
        // check the scene in the example, then record the new value.
        for (scene, size, steps, recorded) in [
            (SuiteScene::JointChain, 8, 240, 0.207_075_94),
            (SuiteScene::ArticulatedChain, 8, 240, 1.907_348_6e-6),
            (SuiteScene::CharacterStairs, 4, 180, -4.085_898_4e-4),
            (SuiteScene::VehicleStress, 1, 240, 0.068_611_324),
            (SuiteScene::RagdollPile, 1, 120, 0.015_326_262),
            (SuiteScene::BuoyancySort, 4, 240, 0.0),
            (SuiteScene::SheetCatch, 4, 180, 0.0),
            (SuiteScene::JellyRaft, 4, 240, 0.0),
        ] {
            let first = scene.run(size, steps);
            let second = scene.run(size, steps);
            assert_eq!(
                first.error.to_bits(),
                second.error.to_bits(),
                "{first:?} {second:?}"
            );
            assert!(
                (first.error - recorded).abs() <= 1e-4,
                "{} recorded {recorded}, got {first:?}",
                scene.name()
            );
        }
    }

    #[test]
    fn the_2d_scenes_stay_in_their_plane_and_within_the_3d_bounds() {
        let stack = SuiteScene::StackingStability.run_2d(10, 300).unwrap();
        assert!(stack.error < 0.05, "{stack:?}");
        let chain = SuiteScene::JointChain.run_2d(20, 300).unwrap();
        assert!(chain.error < 0.5, "{chain:?}");
        let bullets = SuiteScene::CcdBullets.run_2d(25, 30).unwrap();
        assert_eq!(bullets.error, 0.0, "{bullets:?}");
        assert!(SuiteScene::ClothHang.run_2d(4, 10).is_none());
        let again = SuiteScene::StackingStability.run_2d(10, 300).unwrap();
        assert_eq!(stack.error.to_bits(), again.error.to_bits());
    }
}
