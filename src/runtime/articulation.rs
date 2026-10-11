//! Reduced-coordinate articulations: joint trees solved in joint space, so
//! their joints never drift apart and their limits hold exactly.
//!
//! Put [`Articulation`] on the root body. Every `Fixed`, `Hinge`, `Slider`,
//! or `BallSocket` [`Joint`] whose target is the root, or a body already in
//! the tree, adds its body to the tree as a link; the tree is walked
//! breadth-first in joint entity order. A dynamic root floats with six
//! degrees of freedom; a fixed or kinematic root is the tree's base. Link
//! bodies must be dynamic CPU bodies.
//!
//! Each step the joint coordinates are read back from the link poses and
//! velocities, then every link is placed from its parent through its joint
//! (forward kinematics), which removes any drift. The joint-space mass
//! matrix `H` is the sum of each link's mass and inertia through its
//! Jacobian, and gravity and the velocity-product (Coriolis and
//! centrifugal) forces from a recursive pass over the tree give the free
//! motion. Contacts and other joints on a link push the tree through `H⁻¹`,
//! so the whole chain answers a push at one link. Hinge and slider limits,
//! springs, and motors are joint-space rows; after integration each
//! coordinate is clamped to its limit.
//!
//! Joints of other kinds, and joints that would close a loop, stay regular
//! joints. Reduced joints do not break.
// ponytail: ball sockets have no cone limit, reduced joints never break,
// and articulations never sleep; add each when a scene needs it.

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, World};
use nalgebra::{DMatrix, Matrix3, Rotation3, UnitQuaternion, Vector3};
use serde::{Deserialize, Serialize};

use super::joints::{swing_twist, JointLink};
use super::{Body, Joint, JointKind, JointMotor, JointSpring};
use crate::runtime::{sim_math, RigidBody, RigidBodyKind};

/// Makes this body the root of a reduced-coordinate joint tree. See the
/// module docs.
#[derive(
    Component, Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize,
)]
pub struct Articulation {}

/// Small joint-space inertia added to every coordinate so `H` stays
/// invertible when a link's inertia vanishes along an axis.
const ARMATURE: f32 = 1e-4;
/// Share of a hinge or slider's limit error removed per step.
const LIMIT_BIAS: f32 = 0.2;

#[derive(Clone, Copy, PartialEq)]
enum Dof {
    Root,
    Fixed,
    Revolute,
    Prismatic,
    Spherical,
}

impl Dof {
    fn count(self) -> usize {
        match self {
            Self::Revolute | Self::Prismatic => 1,
            Self::Spherical => 3,
            Self::Root | Self::Fixed => 0,
        }
    }
}

struct Link {
    body: usize,
    /// Parent link index; unused for the root.
    parent: usize,
    joint: Joint,
    kind: Dof,
    /// First coordinate of this link's joint.
    dof: usize,
    /// Hinge angle or slider offset.
    q: f32,
    /// Ball socket rotation of this link's frame in the parent's.
    rel: UnitQuaternion<f32>,
    /// Joint coordinates this link moves with, root first, each with its
    /// angular and linear velocity per unit coordinate speed.
    columns: Vec<(usize, Vector3<f32>, Vector3<f32>)>,
    gravity_scale: f32,
}

/// One coordinate's motion: a rotation about `axis` through `point`, or a
/// translation along `axis`.
#[derive(Clone, Copy)]
struct Axis {
    rotation: bool,
    axis: Vector3<f32>,
    point: Vector3<f32>,
}

/// Joint-space limit, spring, or motor row on one coordinate.
struct Row {
    dof: usize,
    mass: f32,
    bias: f32,
    softness: f32,
    lower: f32,
    upper: f32,
    impulse: f32,
}

struct Tree {
    /// Parents before children; `links[0]` is the root.
    links: Vec<Link>,
    floating: bool,
    axes: Vec<Axis>,
    velocity: Vec<f32>,
    inverse: DMatrix<f32>,
    rows: Vec<Row>,
}

/// Every articulation of this step and which link each body is.
#[derive(Default)]
pub(super) struct Articulations {
    trees: Vec<Tree>,
    of_body: Vec<Option<(usize, usize)>>,
}

fn euler(angles: [f32; 3]) -> Rotation3<f32> {
    sim_math::rotation_from_euler(angles[0], angles[1], angles[2])
}

/// Parent-side joint frame and point of `joint` on a parent at `body`.
fn parent_frame(body: &Body, joint: &Joint) -> (Rotation3<f32>, Vector3<f32>) {
    (
        body.rotation * euler(joint.target_frame),
        body.position + body.rotation * Vector3::from(joint.target_anchor),
    )
}

fn child_frame(body: &Body, joint: &Joint) -> (Rotation3<f32>, Vector3<f32>) {
    (
        body.rotation * euler(joint.frame),
        body.position + body.rotation * Vector3::from(joint.anchor),
    )
}

/// Builds the articulations and marks which joint links they took over.
pub(super) fn build(
    world: &mut World,
    bodies: &mut [Body],
    links: &[JointLink],
) -> (Articulations, Vec<bool>) {
    let mut taken = vec![false; links.len()];
    let mut articulations = Articulations {
        trees: Vec::new(),
        of_body: vec![None; bodies.len()],
    };
    let index: std::collections::HashMap<Entity, usize> = bodies
        .iter()
        .enumerate()
        .map(|(index, body)| (body.entity, index))
        .collect();
    let find = |entity: Entity| index.get(&entity).copied();
    let mut roots: Vec<_> = world
        .query::<(Entity, &Articulation)>()
        .iter(world)
        .filter_map(|(entity, _)| find(entity))
        .collect();
    roots.sort_unstable();
    let mut children = vec![Vec::new(); bodies.len()];
    for (index, link) in links.iter().enumerate() {
        if let Some(a) = link.a {
            children[a].push(index);
        }
    }
    let mut used = vec![false; bodies.len()];
    for &root in &roots {
        used[root] = true;
    }
    for root in roots {
        let floating = bodies[root].kind == RigidBodyKind::Dynamic
            && bodies[root].inverse_mass > 0.0;
        let mut tree = Tree {
            links: vec![Link {
                body: root,
                parent: 0,
                joint: Joint::default(),
                kind: Dof::Root,
                dof: 0,
                q: 0.0,
                rel: UnitQuaternion::identity(),
                columns: Vec::new(),
                gravity_scale: 1.0,
            }],
            floating,
            axes: Vec::new(),
            velocity: Vec::new(),
            inverse: DMatrix::zeros(0, 0),
            rows: Vec::new(),
        };
        let mut dofs = if floating { 6 } else { 0 };
        let mut next = 0;
        while next < tree.links.len() {
            let parent = tree.links[next].body;
            for &index in &children[parent] {
                let link = &links[index];
                let kind = match link.joint.kind {
                    JointKind::Fixed => Dof::Fixed,
                    JointKind::Hinge { .. } => Dof::Revolute,
                    JointKind::Slider { .. } => Dof::Prismatic,
                    JointKind::BallSocket => Dof::Spherical,
                    _ => continue,
                };
                let body = &bodies[link.b];
                if used[link.b]
                    || body.kind != RigidBodyKind::Dynamic
                    || body.inverse_mass == 0.0
                {
                    continue;
                }
                used[link.b] = true;
                taken[index] = true;
                tree.links.push(Link {
                    body: link.b,
                    parent: next,
                    joint: link.joint,
                    kind,
                    dof: dofs,
                    q: 0.0,
                    rel: UnitQuaternion::identity(),
                    columns: Vec::new(),
                    gravity_scale: 1.0,
                });
                dofs += match kind {
                    Dof::Revolute | Dof::Prismatic => 1,
                    Dof::Spherical => 3,
                    _ => 0,
                };
            }
            next += 1;
        }
        if tree.links.len() == 1 {
            continue;
        }
        tree.velocity = vec![0.0; dofs];
        tree.axes = vec![
            Axis {
                rotation: false,
                axis: Vector3::zeros(),
                point: Vector3::zeros(),
            };
            dofs
        ];
        let number = articulations.trees.len();
        for (index, link) in tree.links.iter_mut().enumerate() {
            let body = &mut bodies[link.body];
            if index > 0 || floating {
                articulations.of_body[link.body] = Some((number, index));
                body.articulated = true;
                body.asleep = false;
            }
            link.gravity_scale = world
                .get::<RigidBody>(body.entity)
                .map_or(1.0, |rigid| rigid.gravity_scale);
        }
        tree.read(bodies);
        let given: Vec<_> = tree
            .links
            .iter()
            .map(|link| {
                let body = &bodies[link.body];
                (body.angular_velocity, body.velocity)
            })
            .collect();
        tree.place(bodies);
        tree.absorb(bodies, &given);
        articulations.trees.push(tree);
    }
    (articulations, taken)
}

impl Tree {
    /// Joint coordinates and speeds from the link poses and velocities.
    fn read(&mut self, bodies: &[Body]) {
        if self.floating {
            let root = &bodies[self.links[0].body];
            self.velocity[..3]
                .copy_from_slice(root.angular_velocity.as_slice());
            self.velocity[3..6].copy_from_slice(root.velocity.as_slice());
        }
        for index in 1..self.links.len() {
            let link = &self.links[index];
            let (parent, child) =
                (&bodies[self.links[link.parent].body], &bodies[link.body]);
            let (frame_a, point_a) = parent_frame(parent, &link.joint);
            let (frame_b, point_b) = child_frame(child, &link.joint);
            let relative = UnitQuaternion::from_rotation_matrix(
                &(frame_a.inverse() * frame_b),
            );
            let axis = frame_a * Vector3::x();
            let spin = child.angular_velocity - parent.angular_velocity;
            let (kind, dof) = (link.kind, link.dof);
            let link = &mut self.links[index];
            match kind {
                Dof::Revolute => {
                    link.q = swing_twist(&relative).0;
                    self.velocity[dof] = axis.dot(&spin);
                }
                Dof::Prismatic => {
                    link.q = axis.dot(&(point_b - point_a));
                    let carried = parent.velocity
                        + parent
                            .angular_velocity
                            .cross(&(child.position - parent.position));
                    self.velocity[dof] = axis.dot(&(child.velocity - carried));
                }
                Dof::Spherical => {
                    link.rel = relative;
                    let local = frame_a.inverse() * spin;
                    self.velocity[dof..dof + 3]
                        .copy_from_slice(local.as_slice());
                }
                Dof::Root | Dof::Fixed => {}
            }
        }
    }

    /// Places every link from its parent through its joint, then rebuilds
    /// the joint axes, Jacobian columns, and link velocities.
    fn place(&mut self, bodies: &mut [Body]) {
        let root = &bodies[self.links[0].body];
        let origin = root.position;
        if self.floating {
            for k in 0..3 {
                let axis = Vector3::ith(k, 1.0);
                self.axes[k] = Axis {
                    rotation: true,
                    axis,
                    point: origin,
                };
                self.axes[k + 3] = Axis {
                    rotation: false,
                    axis,
                    point: origin,
                };
            }
        }
        for index in 1..self.links.len() {
            let link = &self.links[index];
            let joint = link.joint;
            let (frame_a, point_a) =
                parent_frame(&bodies[self.links[link.parent].body], &joint);
            let axis = frame_a * Vector3::x();
            let (relative, point_b) = match link.kind {
                Dof::Revolute => (
                    Rotation3::from_axis_angle(&Vector3::x_axis(), link.q),
                    point_a,
                ),
                Dof::Prismatic => {
                    (Rotation3::identity(), point_a + axis * link.q)
                }
                Dof::Spherical => (link.rel.to_rotation_matrix(), point_a),
                Dof::Root | Dof::Fixed => (Rotation3::identity(), point_a),
            };
            let rotation = frame_a * relative * euler(joint.frame).inverse();
            let body = &mut bodies[link.body];
            body.rotation = rotation;
            body.position = point_b - rotation * Vector3::from(joint.anchor);
            let dof = link.dof;
            match link.kind {
                Dof::Revolute | Dof::Prismatic => {
                    self.axes[dof] = Axis {
                        rotation: link.kind == Dof::Revolute,
                        axis,
                        point: point_a,
                    };
                }
                Dof::Spherical => {
                    for k in 0..3 {
                        self.axes[dof + k] = Axis {
                            rotation: true,
                            axis: frame_a * Vector3::ith(k, 1.0),
                            point: point_a,
                        };
                    }
                }
                Dof::Root | Dof::Fixed => {}
            }
        }
        // Columns: every coordinate from the root down to each link.
        for index in 0..self.links.len() {
            let mut dofs: Vec<usize> = Vec::new();
            let mut at = index;
            while at != 0 {
                let link = &self.links[at];
                let count = match link.kind {
                    Dof::Revolute | Dof::Prismatic => 1,
                    Dof::Spherical => 3,
                    _ => 0,
                };
                dofs.extend((link.dof..link.dof + count).rev());
                at = link.parent;
            }
            if self.floating {
                dofs.extend((0..6).rev());
            }
            dofs.reverse();
            let position = bodies[self.links[index].body].position;
            self.links[index].columns = dofs
                .into_iter()
                .map(|k| {
                    let axis = self.axes[k];
                    if axis.rotation {
                        (
                            k,
                            axis.axis,
                            axis.axis.cross(&(position - axis.point)),
                        )
                    } else {
                        (k, Vector3::zeros(), axis.axis)
                    }
                })
                .collect();
        }
        self.refresh(bodies);
    }

    /// Link velocities from the joint speeds.
    fn refresh(&self, bodies: &mut [Body]) {
        let root = &bodies[self.links[0].body];
        let (spin, velocity, origin) = if self.floating {
            (Vector3::zeros(), Vector3::zeros(), root.position)
        } else {
            (root.angular_velocity, root.velocity, root.position)
        };
        let start = usize::from(!self.floating);
        for link in &self.links[start..] {
            let body = &mut bodies[link.body];
            let mut angular = spin;
            let mut linear = velocity + spin.cross(&(body.position - origin));
            for (k, column_angular, column_linear) in &link.columns {
                angular += column_angular * self.velocity[*k];
                linear += column_linear * self.velocity[*k];
            }
            body.angular_velocity = angular;
            body.velocity = linear;
        }
    }

    /// Mass and world inertia of a link body.
    fn inertia(body: &Body) -> (f32, Matrix3<f32>) {
        let rotation = body.rotation.matrix();
        let moments = body.inverse_inertia.map(|inverse| {
            if inverse > 0.0 {
                1.0 / inverse
            } else {
                0.0
            }
        });
        (
            1.0 / body.inverse_mass,
            rotation * Matrix3::from_diagonal(&moments) * rotation.transpose(),
        )
    }

    /// Generalized force of a torque and force on link `index`.
    fn generalized(
        &self,
        index: usize,
        torque: &Vector3<f32>,
        force: &Vector3<f32>,
        into: &mut [f32],
    ) {
        for (k, angular, linear) in &self.links[index].columns {
            into[*k] += angular.dot(torque) + linear.dot(force);
        }
    }

    /// Generalized gravity and velocity-product forces at the current link
    /// velocities.
    fn forces(&self, bodies: &[Body], gravity: Vector3<f32>) -> Vec<f32> {
        let mut force = vec![0.0; self.velocity.len()];
        // Link accelerations at zero joint acceleration, parents first.
        let mut accelerations =
            vec![
                (Vector3::<f32>::zeros(), Vector3::<f32>::zeros());
                self.links.len()
            ];
        for index in 1..self.links.len() {
            let link = &self.links[index];
            let (parent, child) =
                (&bodies[self.links[link.parent].body], &bodies[link.body]);
            let (parent_angular, parent_linear) = accelerations[link.parent];
            let (mut spin, mut slide) = (Vector3::zeros(), Vector3::zeros());
            let mut point = child.position;
            for k in link.dof..link.dof + link.kind.count() {
                let axis = self.axes[k];
                if axis.rotation {
                    spin += axis.axis * self.velocity[k];
                    point = axis.point;
                } else {
                    slide += axis.axis * self.velocity[k];
                }
            }
            let w = parent.angular_velocity;
            let point_velocity =
                parent.velocity + w.cross(&(point - parent.position));
            let angular = parent_angular + w.cross(&spin);
            let linear = parent_linear
                + parent_angular.cross(&(child.position - parent.position))
                + w.cross(&(child.velocity - parent.velocity))
                + w.cross(&slide)
                + w.cross(&spin).cross(&(child.position - point))
                + spin.cross(&(child.velocity - point_velocity));
            accelerations[index] = (angular, linear);
        }
        for index in usize::from(!self.floating)..self.links.len() {
            let body = &bodies[self.links[index].body];
            let (link_mass, inertia) = Self::inertia(body);
            let (angular, linear) = accelerations[index];
            let spin = body.angular_velocity;
            let torque = -(inertia * angular + spin.cross(&(inertia * spin)));
            let push = (gravity * self.links[index].gravity_scale - linear)
                * link_mass;
            self.generalized(index, &torque, &push, &mut force);
        }
        force
    }

    /// The joint-space mass matrix `H`, with [`ARMATURE`] on its diagonal.
    fn mass_matrix(&self, bodies: &[Body]) -> DMatrix<f32> {
        let dofs = self.velocity.len();
        let mut mass = DMatrix::<f32>::identity(dofs, dofs) * ARMATURE;
        for link in &self.links[usize::from(!self.floating)..] {
            let (link_mass, inertia) = Self::inertia(&bodies[link.body]);
            for (row, (k, angular_k, linear_k)) in
                link.columns.iter().enumerate()
            {
                for (l, angular_l, linear_l) in &link.columns[row..] {
                    let value = angular_k.dot(&(inertia * angular_l))
                        + link_mass * linear_k.dot(linear_l);
                    mass[(*k, *l)] += value;
                    if k != l {
                        mass[(*l, *k)] += value;
                    }
                }
            }
        }
        mass
    }

    /// Takes link velocities that something outside the tree changed
    /// (fluids and deformables push link bodies between steps) into the
    /// joint speeds. `given` holds each link's (angular, linear) velocity
    /// before [`Self::place`] rebuilt them from the joint speeds that
    /// [`Self::read`] found, which see only the links' spin. The difference
    /// is an impulse; `H⁻¹ Jᵀ` of its momentum keeps the tree's momentum
    /// and leaves velocities the tree made itself unchanged.
    fn absorb(
        &mut self,
        bodies: &mut [Body],
        given: &[(Vector3<f32>, Vector3<f32>)],
    ) {
        let mut impulse = vec![0.0; self.velocity.len()];
        let mut any = false;
        for (link, &(angular, linear)) in self.links.iter().zip(given) {
            let body = &bodies[link.body];
            let (spin, push) =
                (angular - body.angular_velocity, linear - body.velocity);
            if spin.norm() + push.norm() < 1e-6 {
                continue;
            }
            any = true;
            let (link_mass, inertia) = Self::inertia(body);
            let (spin, push) = (inertia * spin, push * link_mass);
            for (k, column_angular, column_linear) in &link.columns {
                impulse[*k] +=
                    column_angular.dot(&spin) + column_linear.dot(&push);
            }
        }
        if !any {
            return;
        }
        let Some(cholesky) = self.mass_matrix(bodies).cholesky() else {
            return;
        };
        let change = cholesky.solve(&nalgebra::DVector::from_vec(impulse));
        for (speed, change) in self.velocity.iter_mut().zip(change.iter()) {
            *speed += change;
        }
        self.refresh(bodies);
    }

    /// Inverts `H` and moves the joint speeds by gravity and the
    /// velocity-product forces over `dt`, taken at the midpoint speeds so
    /// fast chains do not gain energy.
    // ponytail: dense H and its inverse, O(n²) per row and O(n³) per step;
    // move to the articulated-body algorithm when trees pass ~30 links.
    fn free_motion(
        &mut self,
        bodies: &mut [Body],
        gravity: Vector3<f32>,
        dt: f32,
    ) {
        let dofs = self.velocity.len();
        self.inverse = self.mass_matrix(bodies).cholesky().map_or_else(
            || DMatrix::zeros(dofs, dofs),
            |cholesky| cholesky.inverse(),
        );
        let start = self.velocity.clone();
        for share in [0.5, 1.0] {
            let force = self.forces(bodies, gravity);
            self.velocity.copy_from_slice(&start);
            self.change_speed(&force, share * dt);
            self.refresh(bodies);
        }
    }

    /// Limit, spring, and motor rows of every hinge and slider.
    fn build_rows(&mut self, dt: f32) {
        self.rows.clear();
        for link in &self.links {
            let (limit, spring, motor): (
                Option<[f32; 2]>,
                Option<JointSpring>,
                Option<JointMotor>,
            ) = match link.joint.kind {
                JointKind::Hinge {
                    limit,
                    spring,
                    motor,
                }
                | JointKind::Slider {
                    limit,
                    spring,
                    motor,
                } if matches!(link.kind, Dof::Revolute | Dof::Prismatic) => {
                    (limit, spring, motor)
                }
                _ => continue,
            };
            let (dof, value) = (link.dof, link.q);
            let diagonal = self.inverse[(dof, dof)];
            let mut push =
                |bias: f32, softness: f32, lower: f32, upper: f32| {
                    if diagonal + softness > 0.0 {
                        self.rows.push(Row {
                            dof,
                            mass: 1.0 / (diagonal + softness),
                            bias,
                            softness,
                            lower,
                            upper,
                            impulse: 0.0,
                        });
                    }
                };
            if let Some([min, max]) = limit {
                // Speculative: a limit not reached yet lets the joint close
                // the gap in one step. Rows on -q push the upper side.
                for (side, bound) in [(1.0_f32, min), (-1.0, max)] {
                    let gap = side * (value - bound);
                    let bias = if gap > 0.0 {
                        gap / dt
                    } else {
                        LIMIT_BIAS / dt * gap
                    };
                    let (lower, upper) = if side > 0.0 {
                        (0.0, f32::INFINITY)
                    } else {
                        (f32::NEG_INFINITY, 0.0)
                    };
                    push(side * bias, 0.0, lower, upper);
                }
            }
            if let Some(JointSpring {
                target,
                stiffness,
                damping,
            }) = spring
            {
                let denominator = dt * (damping + dt * stiffness);
                if denominator > 0.0 {
                    let factor = dt * stiffness / (damping + dt * stiffness);
                    push(
                        factor / dt * (value - target),
                        1.0 / denominator,
                        f32::NEG_INFINITY,
                        f32::INFINITY,
                    );
                }
            }
            if let Some(motor) = motor {
                let limit = motor.max_force.max(0.0) * dt;
                push(-motor.speed, 0.0, -limit, limit);
            }
        }
    }

    fn change_speed(&mut self, generalized: &[f32], impulse: f32) {
        let dofs = self.velocity.len();
        for k in 0..dofs {
            let change: f32 = (0..dofs)
                .map(|l| self.inverse[(k, l)] * generalized[l])
                .sum();
            self.velocity[k] += change * impulse;
        }
    }

    fn solve_rows(&mut self, bodies: &mut [Body]) {
        if self.rows.is_empty() {
            return;
        }
        for index in 0..self.rows.len() {
            let row = &self.rows[index];
            let speed = self.velocity[row.dof];
            let change =
                -row.mass * (speed + row.bias + row.softness * row.impulse);
            let total = (row.impulse + change).clamp(row.lower, row.upper);
            let applied = total - row.impulse;
            let dof = row.dof;
            self.rows[index].impulse = total;
            let mut unit = vec![0.0; self.velocity.len()];
            unit[dof] = 1.0;
            self.change_speed(&unit, applied);
        }
        self.refresh(bodies);
    }

    /// Moves the coordinates by the joint speeds, clamps hinge and slider
    /// limits, and places the links.
    fn integrate(&mut self, bodies: &mut [Body], dt: f32) {
        if self.floating {
            let spin = Vector3::from_column_slice(&self.velocity[..3]);
            let root = &mut bodies[self.links[0].body];
            root.position +=
                Vector3::from_column_slice(&self.velocity[3..6]) * dt;
            if spin != Vector3::zeros() {
                root.rotation = sim_math::rotation_from_scaled_axis(spin * dt)
                    * root.rotation;
            }
        }
        for link in &mut self.links[1..] {
            let dof = link.dof;
            match link.kind {
                Dof::Revolute | Dof::Prismatic => {
                    link.q += self.velocity[dof] * dt;
                    let limit = match link.joint.kind {
                        JointKind::Hinge { limit, .. }
                        | JointKind::Slider { limit, .. } => limit,
                        _ => None,
                    };
                    if let Some([min, max]) = limit {
                        if link.q < min {
                            link.q = min;
                            self.velocity[dof] = self.velocity[dof].max(0.0);
                        } else if link.q > max {
                            link.q = max;
                            self.velocity[dof] = self.velocity[dof].min(0.0);
                        }
                    }
                }
                Dof::Spherical => {
                    let spin = Vector3::from_column_slice(
                        &self.velocity[dof..dof + 3],
                    );
                    link.rel =
                        UnitQuaternion::from_scaled_axis(spin * dt) * link.rel;
                }
                Dof::Root | Dof::Fixed => {}
            }
        }
        self.place(bodies);
    }
}

/// One side of a velocity row: body index, and the angular and linear
/// impulse it takes per unit of the row's impulse.
pub(super) type Side = (usize, Vector3<f32>, Vector3<f32>);

impl Articulations {
    pub(super) fn contains(&self, body: usize) -> bool {
        self.of_body.get(body).is_some_and(Option::is_some)
    }

    pub(super) fn free_motion(
        &mut self,
        bodies: &mut [Body],
        gravity: Vector3<f32>,
        dt: f32,
    ) {
        for tree in &mut self.trees {
            tree.free_motion(bodies, gravity, dt);
            tree.build_rows(dt);
        }
    }

    pub(super) fn solve_rows(&mut self, bodies: &mut [Body]) {
        for tree in &mut self.trees {
            tree.solve_rows(bodies);
        }
    }

    pub(super) fn integrate(&mut self, bodies: &mut [Body], dt: f32) {
        for tree in &mut self.trees {
            tree.integrate(bodies, dt);
        }
    }

    /// Generalized force per tree of the articulated sides of a row.
    fn gather(&self, sides: &[Side]) -> Vec<(usize, Vec<f32>)> {
        let mut forces: Vec<(usize, Vec<f32>)> = Vec::new();
        for (body, angular, linear) in sides {
            let Some((tree, link)) = self.of_body[*body] else {
                continue;
            };
            let at = forces.iter().position(|(other, _)| *other == tree);
            let at = at.unwrap_or_else(|| {
                forces.push((tree, vec![0.0; self.trees[tree].velocity.len()]));
                forces.len() - 1
            });
            self.trees[tree].generalized(
                link,
                angular,
                linear,
                &mut forces[at].1,
            );
        }
        forces
    }

    /// Inverse effective mass of a row. Rigid sides add their own mass and
    /// inertia; the articulated sides of one tree add `Gᵀ H⁻¹ G` together,
    /// so a row between two links of one tree is exact.
    pub(super) fn inverse_mass(
        &self,
        bodies: &[Body],
        inertia: &[Matrix3<f32>],
        sides: &[Side],
    ) -> f32 {
        let mut k = 0.0;
        for (body, angular, linear) in sides {
            if !self.contains(*body) {
                k += bodies[*body].inverse_mass * linear.norm_squared()
                    + angular.dot(&(inertia[*body] * angular));
            }
        }
        for (tree, force) in self.gather(sides) {
            let inverse = &self.trees[tree].inverse;
            for (k_index, value) in force.iter().enumerate() {
                let row: f32 = force
                    .iter()
                    .enumerate()
                    .map(|(l, other)| inverse[(k_index, l)] * other)
                    .sum();
                k += value * row;
            }
        }
        k
    }

    /// Applies `impulse` along a row to its sides.
    pub(super) fn apply(
        &mut self,
        bodies: &mut [Body],
        inertia: &[Matrix3<f32>],
        sides: &[Side],
        impulse: f32,
    ) {
        for (body, angular, linear) in sides {
            if !self.contains(*body) {
                let rigid = &mut bodies[*body];
                rigid.velocity += linear * (rigid.inverse_mass * impulse);
                rigid.angular_velocity += inertia[*body] * angular * impulse;
            }
        }
        for (tree, force) in self.gather(sides) {
            let tree = &mut self.trees[tree];
            tree.change_speed(&force, impulse);
            tree.refresh(bodies);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::{
        App, Articulation, Collider, ColliderShape, CollisionLayers, Joint,
        JointKind, PhysicsBody, RigidBody, RigidBodyKind,
    };
    use crate::Transform;

    #[test]
    fn a_push_from_outside_moves_a_hanging_link_and_keeps_its_joint() {
        let mut app = App::new();
        let world = app.world_mut();
        let body = |world: &mut bevy_ecs::world::World, at, kind| {
            world
                .spawn((
                    Transform::new(at),
                    PhysicsBody::default(),
                    RigidBody {
                        kind,
                        ..RigidBody::default()
                    },
                    Collider {
                        shape: ColliderShape::Sphere { radius: 0.1 },
                        ..Collider::default()
                    },
                    CollisionLayers {
                        memberships: 0,
                        filters: 0,
                    },
                ))
                .id()
        };
        let base = body(world, [0.0, 3.0, 0.0], RigidBodyKind::Fixed);
        world.entity_mut(base).insert(Articulation {});
        // Hangs straight down, hinged about z.
        let bob = body(world, [0.0, 2.5, 0.0], RigidBodyKind::Dynamic);
        world.entity_mut(bob).insert(Joint {
            frame: [0.0, -std::f32::consts::FRAC_PI_2, 0.0],
            target_frame: [0.0, -std::f32::consts::FRAC_PI_2, 0.0],
            ..Joint::new(
                JointKind::Hinge {
                    limit: None,
                    spring: None,
                    motor: None,
                },
                base,
                [0.0, 0.5, 0.0],
                [0.0; 3],
            )
        });
        let step = |app: &mut App| {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        };
        step(&mut app);
        // What a fluid or a soft body does between steps: a sideways push
        // on the link body alone, and one along the rod the joint takes.
        app.world_mut()
            .get_mut::<RigidBody>(bob)
            .unwrap()
            .linear_velocity = [1.0, 5.0, 0.0];
        let mut highest = f32::MIN;
        for _ in 0..30 {
            step(&mut app);
            let at = app.world().get::<Transform>(bob).unwrap().position;
            let rod = ((at[0]).powi(2) + (at[1] - 3.0).powi(2)).sqrt();
            assert!((rod - 0.5).abs() < 1e-3, "rod stretched to {rod}");
            highest = highest.max(at[1]);
        }
        // 1 m/s swings it up about v²/2g = 5 cm; the rod takes the rest.
        assert!(highest > 2.53, "rose to {highest}");
        assert!(highest < 2.6, "rose to {highest}");
    }
}
