//! Physics debug draw: what the last CPU step saw, as overlay lines.
//!
//! [`PhysicsWorld::debug_draw`] draws every collider as a wireframe
//! coloured by its state ([`colors`]), each contact as a cross with its
//! normal, each solver contact point's normal impulse (as the push on one
//! of the pair), each joint as a line
//! between its two anchors, and each contact island of two or more bodies
//! as a box around them. GPU bodies show at their read-back pose, since
//! the CPU world keeps them as proxies.
//!
//! The editor draws it in the Scene view while a [`PhysicsDebugDraw`]
//! resource exists; insert one to turn it on.

use std::collections::HashMap;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Resource, World};
use nalgebra::{Rotation3, Vector3};

use super::{Body, Joint, PhysicsWorld, Shape};
use crate::rendering::debug_overlay::RenderDebugOverlay;
use crate::runtime::RigidBodyKind;

/// Which parts of [`PhysicsWorld::debug_draw`] to draw.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicsDebugDraw {
    pub shapes: bool,
    pub contacts: bool,
    pub impulses: bool,
    pub joints: bool,
    pub islands: bool,
}

impl Default for PhysicsDebugDraw {
    fn default() -> Self {
        Self {
            shapes: true,
            contacts: true,
            impulses: true,
            joints: true,
            islands: true,
        }
    }
}

/// Line colours of the debug draw.
pub mod colors {
    pub const DYNAMIC: [f32; 4] = [0.35, 0.9, 0.45, 1.0];
    pub const SLEEPING: [f32; 4] = [0.4, 0.5, 0.75, 1.0];
    pub const FIXED: [f32; 4] = [0.55, 0.55, 0.6, 1.0];
    pub const KINEMATIC: [f32; 4] = [0.95, 0.75, 0.3, 1.0];
    pub const SENSOR: [f32; 4] = [0.3, 0.85, 0.95, 1.0];
    /// Bodies the GPU simulates, and their query proxies.
    pub const GPU: [f32; 4] = [0.85, 0.4, 0.95, 1.0];
    pub const CONTACT: [f32; 4] = [1.0, 0.25, 0.2, 1.0];
    pub const IMPULSE: [f32; 4] = [1.0, 0.9, 0.2, 1.0];
    pub const JOINT: [f32; 4] = [0.3, 0.6, 1.0, 1.0];
    pub const ISLAND: [f32; 4] = [1.0, 1.0, 1.0, 0.5];
}

/// Length of a contact normal, in metres.
const NORMAL_LENGTH: f32 = 0.25;
/// Metres of line per newton second of normal impulse.
// ponytail: one fixed scale; a 1 kg box resting on four points draws
// about 8 cm per point. Make it a setting if heavy scenes need another.
const IMPULSE_SCALE: f32 = 2.0;
const CROSS: f32 = 0.05;
const CIRCLE_SEGMENTS: usize = 16;

impl PhysicsWorld {
    /// Adds the last step's colliders, contacts, impulses, joints and islands, as
    /// chosen by `settings`, to `overlay`. Reads [`Joint`]s from `world`.
    pub fn debug_draw(
        &self,
        world: &World,
        settings: PhysicsDebugDraw,
        overlay: &mut RenderDebugOverlay,
    ) {
        let mut lines = Vec::new();
        if settings.shapes {
            for body in &self.bodies {
                wire(&body.shape, body.position, body.rotation, &mut lines);
                let color = body_color(body);
                for [start, end] in lines.drain(..) {
                    overlay.line(start, end, color);
                }
            }
        }
        if settings.contacts {
            for contact in self.contacts.iter().filter(|c| !c.sensor) {
                let point = Vector3::from(contact.point);
                cross(point, &mut lines);
                let tip = point + Vector3::from(contact.normal) * NORMAL_LENGTH;
                lines.push([contact.point, tip.into()]);
            }
            for [start, end] in lines.drain(..) {
                overlay.line_on_top(start, end, colors::CONTACT, 1.5);
            }
        }
        let bodies: HashMap<Entity, &Body> =
            self.bodies.iter().map(|body| (body.entity, body)).collect();
        if settings.impulses {
            let normals: HashMap<_, _> = self
                .contacts
                .iter()
                .map(|contact| ((contact.a, contact.b), contact.normal))
                .collect();
            for (&(a, b), points) in &self.warm {
                let (Some(body), Some(normal)) = (
                    bodies.get(&a),
                    normals
                        .get(&(a, b))
                        .map(|&normal| Vector3::from(normal))
                        .or_else(|| {
                            normals.get(&(b, a)).map(|&n| -Vector3::from(n))
                        }),
                ) else {
                    continue;
                };
                // The normal points from `a` to `b`; draw the push on `a`,
                // whose frame the point is kept in.
                for (local, impulses) in points {
                    let point = body.position + body.rotation * local;
                    let tip = point - normal * impulses[0] * IMPULSE_SCALE;
                    overlay.line_on_top(
                        point.into(),
                        tip.into(),
                        colors::IMPULSE,
                        2.0,
                    );
                }
            }
        }
        if settings.joints {
            if let Some(mut query) = world.try_query::<(Entity, &Joint)>() {
                for (entity, joint) in query.iter(world) {
                    let Some(body) = bodies.get(&entity) else {
                        continue;
                    };
                    let anchor = body.position
                        + body.rotation * Vector3::from(joint.anchor);
                    let target = Vector3::from(joint.target_anchor);
                    let target = match bodies.get(&joint.target) {
                        Some(other) => other.position + other.rotation * target,
                        None if joint.target == Entity::PLACEHOLDER => target,
                        None => continue,
                    };
                    cross(anchor, &mut lines);
                    cross(target, &mut lines);
                    lines.push([anchor.into(), target.into()]);
                }
            }
            for [start, end] in lines.drain(..) {
                overlay.line_on_top(start, end, colors::JOINT, 2.0);
            }
        }
        if settings.islands {
            for [low, high] in self.island_boxes(world) {
                let center = (low + high) / 2.0;
                wire(
                    &Shape::Box((high - low) / 2.0),
                    center,
                    Rotation3::identity(),
                    &mut lines,
                );
            }
            for [start, end] in lines.drain(..) {
                overlay.line(start, end, colors::ISLAND);
            }
        }
    }

    /// World boxes around each island of two or more dynamic bodies that
    /// touch or are jointed, in the order of their first body. Fixed,
    /// kinematic, sensor and GPU bodies split islands, as in the solver.
    // ponytail: articulation links join an island only through contacts;
    // add their tree edges if articulated islands need to show whole.
    fn island_boxes(&self, world: &World) -> Vec<[Vector3<f32>; 2]> {
        let member = |body: &Body| {
            body.kind == RigidBodyKind::Dynamic && !body.proxy && !body.sensor
        };
        let index: HashMap<Entity, usize> = self
            .bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| member(body))
            .map(|(index, body)| (body.entity, index))
            .collect();
        let mut parent: Vec<usize> = (0..self.bodies.len()).collect();
        fn root(parent: &mut [usize], mut at: usize) -> usize {
            while parent[at] != at {
                parent[at] = parent[parent[at]];
                at = parent[at];
            }
            at
        }
        let mut join = |a: Entity, b: Entity| {
            if let (Some(&a), Some(&b)) = (index.get(&a), index.get(&b)) {
                let (a, b) = (root(&mut parent, a), root(&mut parent, b));
                parent[a.max(b)] = a.min(b);
            }
        };
        for contact in self.contacts.iter().filter(|c| !c.sensor) {
            join(contact.a, contact.b);
        }
        if let Some(mut query) = world.try_query::<(Entity, &Joint)>() {
            for (entity, joint) in query.iter(world) {
                join(entity, joint.target);
            }
        }
        // Each island's box and body count, in the order of its first body.
        let mut boxes: Vec<([Vector3<f32>; 2], usize)> = Vec::new();
        let mut slot = HashMap::new();
        for (at, body) in self.bodies.iter().enumerate() {
            if !member(body) {
                continue;
            }
            let top = root(&mut parent, at);
            let reach = Vector3::repeat(body.bounding_radius());
            let (low, high) = (body.position - reach, body.position + reach);
            let island = *slot.entry(top).or_insert_with(|| {
                boxes.push(([low, high], 0));
                boxes.len() - 1
            });
            let ([min, max], count) = &mut boxes[island];
            *min = min.inf(&low);
            *max = max.sup(&high);
            *count += 1;
        }
        boxes
            .into_iter()
            .filter(|&(_, count)| count > 1)
            .map(|(bounds, _)| bounds)
            .collect()
    }
}

fn body_color(body: &Body) -> [f32; 4] {
    if body.proxy {
        colors::GPU
    } else if body.sensor {
        colors::SENSOR
    } else if body.asleep {
        colors::SLEEPING
    } else {
        match body.kind {
            RigidBodyKind::Fixed => colors::FIXED,
            RigidBodyKind::Kinematic => colors::KINEMATIC,
            RigidBodyKind::Dynamic => colors::DYNAMIC,
        }
    }
}

/// A small 3D cross at `point`.
fn cross(point: Vector3<f32>, out: &mut Vec<[[f32; 3]; 2]>) {
    for axis in 0..3 {
        let mut offset = Vector3::zeros();
        offset[axis] = CROSS;
        out.push([(point - offset).into(), (point + offset).into()]);
    }
}

/// Wireframe of `shape` placed at `position` and `rotation`.
fn wire(
    shape: &Shape,
    position: Vector3<f32>,
    rotation: Rotation3<f32>,
    out: &mut Vec<[[f32; 3]; 2]>,
) {
    let place = |local: Vector3<f32>| -> [f32; 3] {
        (position + rotation * local).into()
    };
    // A circle around local `axis` through `center`.
    let circle =
        |out: &mut Vec<_>, center: Vector3<f32>, axis: usize, radius| {
            let point = |index: usize| {
                let angle = index as f32 * std::f32::consts::TAU
                    / CIRCLE_SEGMENTS as f32;
                let mut local = Vector3::zeros();
                local[(axis + 1) % 3] = radius * angle.cos();
                local[(axis + 2) % 3] = radius * angle.sin();
                place(center + local)
            };
            for index in 0..CIRCLE_SEGMENTS {
                out.push([point(index), point(index + 1)]);
            }
        };
    match shape {
        Shape::Sphere(radius) => {
            for axis in 0..3 {
                circle(out, Vector3::zeros(), axis, *radius);
            }
        }
        Shape::Capsule {
            half_height,
            radius,
        } => {
            for end in [-*half_height, *half_height] {
                let center = Vector3::new(0.0, end, 0.0);
                for axis in 0..3 {
                    circle(out, center, axis, *radius);
                }
            }
            for side in [[1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0]] {
                let x = side[0] * radius;
                let z = side[1] * radius;
                out.push([
                    place(Vector3::new(x, -half_height, z)),
                    place(Vector3::new(x, *half_height, z)),
                ]);
            }
        }
        Shape::Box(half) => {
            let corner = |bits: usize| {
                Vector3::from_fn(|axis, _| {
                    if bits & (1 << axis) == 0 {
                        -half[axis]
                    } else {
                        half[axis]
                    }
                })
            };
            for bits in 0..8 {
                for axis in 0..3 {
                    if bits & (1 << axis) == 0 {
                        out.push([
                            place(corner(bits)),
                            place(corner(bits | 1 << axis)),
                        ]);
                    }
                }
            }
        }
        // ponytail: every triangle edge, shared ones twice, and all of a
        // heightfield; dedupe edges or cull by distance if big meshes lag.
        Shape::Hull(mesh) | Shape::Triangles(mesh) => {
            for [a, b, c] in &mesh.triangles {
                out.push([place(*a), place(*b)]);
                out.push([place(*b), place(*c)]);
                out.push([place(*c), place(*a)]);
            }
        }
        Shape::Compound(parts) => {
            for part in parts.iter() {
                wire(
                    &part.shape,
                    position + rotation * part.offset,
                    rotation * part.rotation,
                    out,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::runtime::{
        Collider, ColliderShape, JointKind, PhysicsBody, RigidBody, Sleeping,
    };
    use crate::{App, Transform};

    fn body(
        app: &mut App,
        y: f32,
        half: [f32; 3],
        kind: RigidBodyKind,
    ) -> Entity {
        app.spawn((
            Transform::new([0.0, y, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                kind,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box { half_extents: half },
                ..Collider::default()
            },
        ))
    }

    fn draw(app: &App) -> Vec<crate::rendering::debug_overlay::DebugLine> {
        let mut overlay = RenderDebugOverlay::default();
        app.world().resource::<PhysicsWorld>().debug_draw(
            app.world(),
            PhysicsDebugDraw::default(),
            &mut overlay,
        );
        overlay.lines
    }

    fn count(
        lines: &[crate::rendering::debug_overlay::DebugLine],
        color: [f32; 4],
    ) -> usize {
        lines.iter().filter(|line| line.color == color).count()
    }

    #[test]
    fn debug_draw_shows_shapes_contacts_impulses_joints_and_sleep() {
        let mut app = App::new();
        body(&mut app, -0.5, [5.0, 0.5, 5.0], RigidBodyKind::Fixed);
        let crate_box = body(&mut app, 0.5, [0.5; 3], RigidBodyKind::Dynamic);
        let hanging = body(&mut app, 3.0, [0.2; 3], RigidBodyKind::Dynamic);
        app.world_mut().entity_mut(hanging).insert(Joint::new(
            JointKind::default(),
            Entity::PLACEHOLDER,
            [0.0, 0.5, 0.0],
            [0.0, 3.5, 0.0],
        ));
        // A fixed GPU body; dynamic ones show once read-back state is fresh.
        let gpu = body(&mut app, 0.5, [0.5; 3], RigidBodyKind::Fixed);
        app.world_mut().get_mut::<Transform>(gpu).unwrap().position[0] = 20.0;
        app.world_mut()
            .get_mut::<PhysicsBody>(gpu)
            .unwrap()
            .simulation = crate::runtime::SimulationClass::Gpu;
        for _ in 0..3 {
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
        }

        let lines = draw(&app);
        // Each box is 12 edges, coloured by kind.
        assert_eq!(count(&lines, colors::FIXED), 12);
        assert_eq!(count(&lines, colors::DYNAMIC), 24);
        assert_eq!(count(&lines, colors::GPU), 12);
        // One contact per touching pair: a cross and its normal.
        assert_eq!(count(&lines, colors::CONTACT), 4);
        let impulses: Vec<_> = lines
            .iter()
            .filter(|line| line.color == colors::IMPULSE)
            .collect();
        assert_eq!(impulses.len(), 4);
        // The crate rests on the ground: the impulses push it up.
        assert!(
            impulses.iter().all(|line| line.end[1] > line.start[1]),
            "{impulses:?}"
        );
        // Two anchor crosses and the line between them, at the anchors.
        let joint: Vec<_> = lines
            .iter()
            .filter(|line| line.color == colors::JOINT)
            .collect();
        assert_eq!(joint.len(), 7);
        let link = joint.last().unwrap();
        assert!((link.end[1] - 3.5).abs() < 1e-5, "{link:?}");
        assert!((link.start[1] - 3.5).abs() < 0.05, "{link:?}");

        // Once the crate falls asleep it turns the sleeping colour.
        for _ in 0..300 {
            if app.world().get::<Sleeping>(crate_box).is_some() {
                break;
            }
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
        }
        assert!(app.world().get::<Sleeping>(crate_box).is_some());
        let lines = draw(&app);
        assert!(count(&lines, colors::SLEEPING) >= 12);
    }

    #[test]
    fn debug_draw_boxes_islands_of_touching_and_jointed_bodies() {
        let mut app = App::new();
        body(&mut app, -0.5, [20.0, 0.5, 20.0], RigidBodyKind::Fixed);
        // Two crates stacked on the ground: one island, since the ground
        // does not join islands.
        body(&mut app, 0.5, [0.5; 3], RigidBodyKind::Dynamic);
        body(&mut app, 1.5, [0.5; 3], RigidBodyKind::Dynamic);
        // A crate alone on the ground: no box of its own.
        let lone = body(&mut app, 0.5, [0.5; 3], RigidBodyKind::Dynamic);
        app.world_mut().get_mut::<Transform>(lone).unwrap().position[0] = 10.0;
        // Two boxes jointed in the air, apart: one island.
        let upper = body(&mut app, 8.0, [0.2; 3], RigidBodyKind::Dynamic);
        let lower = body(&mut app, 6.0, [0.2; 3], RigidBodyKind::Dynamic);
        for (entity, x) in [(upper, -10.0), (lower, -10.0)] {
            app.world_mut()
                .get_mut::<Transform>(entity)
                .unwrap()
                .position[0] = x;
        }
        app.world_mut().entity_mut(lower).insert(Joint::new(
            JointKind::default(),
            upper,
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ));
        app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();

        let mut boxes = app
            .world()
            .resource::<PhysicsWorld>()
            .island_boxes(app.world());
        boxes.sort_by(|a, b| b[0].x.total_cmp(&a[0].x));
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        let [low, high] = boxes[0];
        assert!(low.x < -0.5 && high.x > 0.5 && high.x < 2.0, "{boxes:?}");
        assert!(low.y < 0.0 && high.y > 2.0, "{boxes:?}");
        let [low, high] = boxes[1];
        assert!(low.x < -10.0 && high.x > -10.0, "{boxes:?}");
        assert!(low.y < 6.0 && high.y > 8.0, "{boxes:?}");
        // Each island is drawn as one 12-edge box.
        assert_eq!(count(&draw(&app), colors::ISLAND), 24);
    }
}
