//! Standard physics quality scenes, run headless on the CPU solvers.
//!
//! Each [`SuiteScene`] builds the same bodies every run, steps them at
//! 60 Hz and returns a [`SuiteScore`]: the mean wall time of a step and one
//! error number that a correct solver keeps near zero. The
//! `physics_suite` example prints them; the tests hold the error bounds.

use std::time::{Duration, Instant};

use bevy_ecs::entity::Entity;

use super::{
    Cloth, ClothSettings, Collider, ColliderShape, Debris, Fluid,
    FluidSettings, Fracture, Joint, JointKind, MeshRenderer, Name, PhysicsBody,
    Ragdoll, RagdollBone, RagdollState, RigidBody, RigidBodyKind, Vehicle,
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
    pub const ALL: [Self; 8] = [
        Self::StackingStability,
        Self::JointChain,
        Self::CcdBullets,
        Self::ClothHang,
        Self::FluidDrop,
        Self::VehicleStress,
        Self::FractureDrop,
        Self::RagdollPile,
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
            Self::JointChain => {
                const HALF: f32 = 0.25;
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
}
