//! Standard physics quality scenes, run headless on the CPU solver.
//!
//! Each [`SuiteScene`] builds the same bodies every run, steps them at
//! 60 Hz and returns a [`SuiteScore`]: the mean wall time of a step and one
//! error number that a correct solver keeps near zero. The
//! `physics_suite` example prints them; the tests hold the error bounds.

use std::time::{Duration, Instant};

use bevy_ecs::entity::Entity;

use super::{
    Collider, ColliderShape, Joint, JointKind, PhysicsBody, RigidBody,
    RigidBodyKind,
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
    pub const ALL: [Self; 3] =
        [Self::StackingStability, Self::JointChain, Self::CcdBullets];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::StackingStability => "stacking_stability",
            Self::JointChain => "joint_chain",
            Self::CcdBullets => "ccd_bullets",
        }
    }

    /// What [`SuiteScore::error`] counts.
    #[must_use]
    pub fn error_unit(self) -> &'static str {
        match self {
            Self::StackingStability => "max drift (m)",
            Self::JointChain => "max joint gap (m)",
            Self::CcdBullets => "tunnelled share",
        }
    }

    /// Builds the scene with `size` bodies and runs it for `steps` steps.
    #[must_use]
    pub fn run(self, size: usize, steps: u32) -> SuiteScore {
        let mut app = App::new();
        let mut elapsed = Duration::ZERO;
        let mut step = |app: &mut App| {
            let start = Instant::now();
            app.update(Duration::from_secs_f64(DT)).unwrap();
            elapsed += start.elapsed();
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
                    step(&mut app);
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
                    step(&mut app);
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
                    step(&mut app);
                }
                let through = bullets
                    .iter()
                    .filter(|&&bullet| position(&app, bullet)[2] < -0.05)
                    .count();
                through as f32 / size.max(1) as f32
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
    }
}
