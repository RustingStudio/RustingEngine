//! Deterministic XPBD cloth from triangle meshes on the CPU.
//!
//! Each triangle edge is a stretch constraint and each pair of triangles
//! sharing an edge holds its two far corners apart with a bend constraint.
//! Wind pushes every triangle along its normal by how fast the air crosses
//! it. With self-collision on, particles closer than twice the thickness
//! are pushed apart, found through a grid walked in ascending particle
//! order. It shares the substep loop, rigid-body anchors and collider
//! obstacles of [`super::soft_body`], so the same determinism holds: no
//! atomics, no randomness, constraints in index order. Overstretched edges
//! tear by removing the triangles around them. [`ClothSheet`] is the scene
//! form and [`ClothVolume`] the runtime state on an entity.

// The particle loops index several parallel arrays at once.
#![allow(clippy::needless_range_loop)]

use std::collections::BTreeMap;

use nalgebra::Vector3;

use bevy_ecs::prelude::{Commands, Component, Entity, Query, ResMut, Without};

use super::soft_body::{
    advance_bodies, body_rotation, finish_substep, named_material, predict,
    solve_distances, solve_holds, vector, Anchor, AnchorBody, Obstacle,
    SoftAttachment, SoftSkin, MAX_SOFT_BODY_SUBSTEPS,
};

/// Solver settings. Lengths are meters and times seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClothSettings {
    pub gravity: [f32; 3],
    /// Substeps per step, up to [`MAX_SOFT_BODY_SUBSTEPS`].
    pub substeps: u32,
    /// Edge compliance (inverse stiffness). 0 keeps edges at rest length.
    pub stretch_compliance: f32,
    /// Bend compliance. Larger folds more easily.
    pub bend_compliance: f32,
    /// Fraction of velocity lost per second.
    pub damping: f32,
    /// Air velocity, in meters per second.
    pub wind: [f32; 3],
    /// Air drag per square meter: force is
    /// `drag * area * (relative air velocity · normal)` along the normal.
    pub drag: f32,
    /// Half the closest two particles get with self-collision on.
    pub thickness: f32,
    pub self_collision: bool,
    /// Height of a ground plane, if any (see
    /// [`super::SoftBodySettings::floor`]).
    pub floor: Option<f32>,
    /// Strain past which an edge tears, removing the triangles that hold
    /// it. `None` never tears.
    pub tear_strain: Option<f32>,
}

impl Default for ClothSettings {
    fn default() -> Self {
        Self {
            gravity: [0.0, -9.81, 0.0],
            substeps: 10,
            stretch_compliance: 0.0,
            bend_compliance: 1e-3,
            damping: 0.5,
            wind: [0.0; 3],
            drag: 1.0,
            thickness: 0.01,
            self_collision: true,
            floor: Some(0.0),
            tear_strain: None,
        }
    }
}

/// A piece of cloth: particles joined by triangles.
#[derive(Clone, Debug, PartialEq)]
pub struct Cloth {
    pub positions: Vec<[f32; 3]>,
    pub velocities: Vec<[f32; 3]>,
    /// 0 pins a particle.
    pub inverse_masses: Vec<f32>,
    pub triangles: Vec<[u32; 3]>,
    /// Where each particle sits at rest; rest lengths come from here.
    rest_positions: Vec<[f32; 3]>,
    /// Sorted triangle edges, then the far corners of each pair of
    /// triangles sharing an edge; all solved as distance constraints.
    edges: Vec<[u32; 2]>,
    bends: Vec<[u32; 2]>,
    edge_rests: Vec<f32>,
    bend_rests: Vec<f32>,
}

impl Cloth {
    /// Shortest rest edge, 0 with no edges: the grid spacing of a sheet.
    #[must_use]
    pub fn min_rest_length(&self) -> f32 {
        self.edge_rests
            .iter()
            .copied()
            .reduce(f32::min)
            .unwrap_or(0.0)
    }

    /// Cloth over `positions` (its rest shape) with `density` in kilograms
    /// per square meter. Each triangle gives a third of its mass to each
    /// corner.
    pub fn new(
        positions: Vec<[f32; 3]>,
        triangles: Vec<[u32; 3]>,
        density: f32,
    ) -> Result<Self, String> {
        if density.is_nan() || density <= 0.0 {
            return Err(format!("density must be positive, got {density}"));
        }
        let mut masses = vec![0.0f32; positions.len()];
        for &triangle in &triangles {
            if triangle.iter().any(|&i| i as usize >= positions.len()) {
                return Err(format!(
                    "triangle {triangle:?} uses a particle past {}",
                    positions.len()
                ));
            }
            let area = triangle_area(&positions, triangle);
            if area <= 1e-12 {
                return Err(format!("triangle {triangle:?} has no area"));
            }
            for i in triangle {
                masses[i as usize] += density * area / 3.0;
            }
        }
        let mut cloth = Self {
            velocities: vec![[0.0; 3]; positions.len()],
            inverse_masses: masses
                .iter()
                .map(|&mass| if mass > 0.0 { 1.0 / mass } else { 0.0 })
                .collect(),
            rest_positions: positions.clone(),
            positions,
            triangles,
            edges: Vec::new(),
            bends: Vec::new(),
            edge_rests: Vec::new(),
            bend_rests: Vec::new(),
        };
        cloth.link();
        Ok(cloth)
    }

    /// A sheet of `cells` squares of side `spacing`, from `origin` along
    /// the directions `u` and `v`, each square cut into two triangles with
    /// diagonals alternating like a checkerboard so it folds evenly.
    pub fn grid(
        origin: [f32; 3],
        u: [f32; 3],
        v: [f32; 3],
        cells: [u32; 2],
        spacing: f32,
        density: f32,
    ) -> Result<Self, String> {
        let (u, v) = (vector(u).normalize(), vector(v).normalize());
        let [nu, nv] = cells.map(|count| count + 1);
        let mut positions = Vec::with_capacity((nu * nv) as usize);
        for y in 0..nv {
            for x in 0..nu {
                let point = vector(origin)
                    + u * (x as f32 * spacing)
                    + v * (y as f32 * spacing);
                positions.push(point.into());
            }
        }
        let index = |x: u32, y: u32| x + nu * y;
        let mut triangles = Vec::new();
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                let [a, b, c, d] = [
                    index(x, y),
                    index(x + 1, y),
                    index(x + 1, y + 1),
                    index(x, y + 1),
                ];
                if (x + y) % 2 == 0 {
                    triangles.extend([[a, b, c], [a, c, d]]);
                } else {
                    triangles.extend([[a, b, d], [b, c, d]]);
                }
            }
        }
        Self::new(positions, triangles, density)
    }

    /// Rebuilds edges and bends from the triangles.
    fn link(&mut self) {
        let mut opposite: BTreeMap<[u32; 2], Vec<u32>> = BTreeMap::new();
        for &[a, b, c] in &self.triangles {
            for (edge, far) in [([a, b], c), ([b, c], a), ([c, a], b)] {
                opposite.entry(sorted(edge)).or_default().push(far);
            }
        }
        self.edges = opposite.keys().copied().collect();
        self.bends = opposite
            .values()
            .filter(|far| far.len() == 2 && far[0] != far[1])
            .map(|far| sorted([far[0], far[1]]))
            .collect();
        let rest = |pair: &[u32; 2]| {
            (vector(self.rest_positions[pair[0] as usize])
                - vector(self.rest_positions[pair[1] as usize]))
            .norm()
        };
        self.edge_rests = self.edges.iter().map(rest).collect();
        self.bend_rests = self.bends.iter().map(rest).collect();
    }

    /// Pins a particle where it is.
    pub fn pin(&mut self, particle: usize) {
        self.inverse_masses[particle] = 0.0;
        self.velocities[particle] = [0.0; 3];
    }

    /// Every distance constraint as its particles, rest length and
    /// compliance: the edges, then the bends.
    pub(super) fn distance_constraints(
        &self,
        settings: &ClothSettings,
    ) -> Vec<([u32; 2], f32, f32)> {
        let edges = self
            .edges
            .iter()
            .zip(&self.edge_rests)
            .map(|(&pair, &rest)| (pair, rest, settings.stretch_compliance));
        let bends = self
            .bends
            .iter()
            .zip(&self.bend_rests)
            .map(|(&pair, &rest)| (pair, rest, settings.bend_compliance));
        edges.chain(bends).collect()
    }

    /// Triangle edges, sorted, each as its two particles.
    pub fn edges(&self) -> &[[u32; 2]] {
        &self.edges
    }

    /// Removes every triangle holding an edge stretched past
    /// `rest * (1 + strain)` and relinks the rest. Returns how many
    /// triangles were removed.
    pub fn tear(&mut self, strain: f32) -> usize {
        let torn: Vec<[u32; 2]> = self
            .edges
            .iter()
            .zip(&self.edge_rests)
            .filter(|&(&[a, b], &rest)| {
                let length = (vector(self.positions[a as usize])
                    - vector(self.positions[b as usize]))
                .norm();
                length > rest * (1.0 + strain)
            })
            .map(|(&edge, _)| edge)
            .collect();
        if torn.is_empty() {
            return 0;
        }
        let before = self.triangles.len();
        self.triangles.retain(|&[a, b, c]| {
            [[a, b], [b, c], [c, a]]
                .iter()
                .all(|&edge| torn.binary_search(&sorted(edge)).is_err())
        });
        self.link();
        before - self.triangles.len()
    }

    /// Advances the cloth by `dt` seconds.
    pub fn step(&mut self, settings: &ClothSettings, dt: f32) {
        self.step_coupled(settings, dt, &mut [], &[], &[]);
    }

    /// Advances the cloth by `dt` seconds, held on and pushed out of rigid
    /// bodies as in [`super::SoftBody::step_coupled`].
    pub fn step_coupled(
        &mut self,
        settings: &ClothSettings,
        dt: f32,
        bodies: &mut [AnchorBody],
        anchors: &[Anchor],
        obstacles: &[Obstacle],
    ) {
        let substeps = settings.substeps.clamp(1, MAX_SOFT_BODY_SUBSTEPS);
        let h = dt / substeps as f32;
        let mut previous = self.positions.clone();
        for _ in 0..substeps {
            previous.copy_from_slice(&self.positions);
            self.blow(settings, h);
            predict(
                &mut self.positions,
                &mut self.velocities,
                &self.inverse_masses,
                settings.gravity,
                settings.damping,
                h,
            );
            advance_bodies(bodies, h);
            solve_distances(
                &mut self.positions,
                &self.inverse_masses,
                &self.edges,
                &self.edge_rests,
                settings.stretch_compliance / (h * h),
            );
            solve_distances(
                &mut self.positions,
                &self.inverse_masses,
                &self.bends,
                &self.bend_rests,
                settings.bend_compliance / (h * h),
            );
            if settings.self_collision {
                self.collide_self(settings.thickness);
            }
            solve_holds(
                &mut self.positions,
                &self.inverse_masses,
                bodies,
                anchors,
                obstacles,
                h,
            );
            finish_substep(
                &mut self.positions,
                &mut self.velocities,
                &self.inverse_masses,
                &previous,
                settings.floor,
                h,
            );
        }
        if let Some(strain) = settings.tear_strain {
            self.tear(strain);
        }
    }

    /// Adds the wind's push on each triangle to its corners' velocities.
    fn blow(&mut self, settings: &ClothSettings, h: f32) {
        if settings.drag == 0.0 {
            return;
        }
        for &triangle in &self.triangles {
            let [a, b, c] =
                triangle.map(|i| vector(self.positions[i as usize]));
            let cross = (b - a).cross(&(c - a));
            let twice_area = cross.norm();
            if twice_area < 1e-12 {
                continue;
            }
            let normal = cross / twice_area;
            let air = triangle.iter().fold(Vector3::zeros(), |sum, &i| {
                sum + vector(self.velocities[i as usize])
            }) / 3.0;
            let crossing = (vector(settings.wind) - air).dot(&normal);
            let force = normal * (settings.drag * crossing * twice_area / 2.0);
            for i in triangle.map(|i| i as usize) {
                self.velocities[i] = (vector(self.velocities[i])
                    + force * (self.inverse_masses[i] * h / 3.0))
                    .into();
            }
        }
    }

    /// Pushes apart every pair of particles closer than `2 * thickness`
    /// that are farther apart at rest, in ascending particle order.
    fn collide_self(&mut self, thickness: f32) {
        let reach = 2.0 * thickness;
        if reach <= 0.0 {
            return;
        }
        let cell = |p: [f32; 3]| p.map(|x| (x / reach).floor() as i32);
        let mut grid: BTreeMap<[i32; 3], Vec<u32>> = BTreeMap::new();
        for (i, &p) in self.positions.iter().enumerate() {
            grid.entry(cell(p)).or_default().push(i as u32);
        }
        for i in 0..self.positions.len() {
            let [cx, cy, cz] = cell(self.positions[i]);
            for dz in -1..=1 {
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let Some(others) =
                            grid.get(&[cx + dx, cy + dy, cz + dz])
                        else {
                            continue;
                        };
                        for &j in others {
                            let j = j as usize;
                            if j > i {
                                self.push_apart(i, j, reach);
                            }
                        }
                    }
                }
            }
        }
    }

    fn push_apart(&mut self, i: usize, j: usize, reach: f32) {
        let weight = self.inverse_masses[i] + self.inverse_masses[j];
        let rest = (vector(self.rest_positions[i])
            - vector(self.rest_positions[j]))
        .norm();
        let delta = vector(self.positions[i]) - vector(self.positions[j]);
        let length = delta.norm();
        if weight == 0.0 || rest < reach || length >= reach || length < 1e-9 {
            return;
        }
        let normal = delta / length;
        let push = (reach - length) / weight;
        self.positions[i] = (vector(self.positions[i])
            + normal * (push * self.inverse_masses[i]))
            .into();
        self.positions[j] = (vector(self.positions[j])
            - normal * (push * self.inverse_masses[j]))
            .into();
    }

    /// FNV-1a over every position and velocity bit, for determinism checks.
    pub fn state_hash(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for value in self.positions.iter().chain(&self.velocities).flatten() {
            hash = (hash ^ u64::from(value.to_bits()))
                .wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

fn sorted([a, b]: [u32; 2]) -> [u32; 2] {
    [a.min(b), a.max(b)]
}

fn triangle_area(positions: &[[f32; 3]], triangle: [u32; 3]) -> f32 {
    let [a, b, c] = triangle.map(|i| vector(positions[i as usize]));
    (b - a).cross(&(c - a)).norm() / 2.0
}

/// Cloth on an entity, stepped once per fixed tick after rigid physics by
/// `step_soft_bodies`. Runtime state, kept in snapshots
/// but not in scene files; a scene saves a [`ClothSheet`].
#[derive(Component, Clone, Debug)]
pub struct ClothVolume {
    pub settings: ClothSettings,
    pub cloth: Cloth,
    pub attachments: Vec<SoftAttachment>,
    /// Draws both sides of the triangles; `None` draws nothing.
    pub skin: Option<SoftSkin>,
}

impl ClothVolume {
    pub fn new(settings: ClothSettings, cloth: Cloth) -> Self {
        Self {
            settings,
            cloth,
            attachments: Vec::new(),
            skin: None,
        }
    }
}

/// Most particles one [`ClothSheet`] spawns.
pub const MAX_CLOTH_PARTICLES: u64 = 4_096;

/// A scene's cloth: a sheet of `count_x` by `count_y` squares of `spacing`
/// meters in the entity's XY plane, centered on it. With `pin_top` its top
/// edge hangs on `holder` (a body, carried along and pulled on) or, when
/// `holder` is null, stays where it starts. The first fixed tick gives the
/// entity its [`ClothVolume`].
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct ClothSheet {
    pub spacing: f32,
    pub count_x: u32,
    pub count_y: u32,
    /// kg/m².
    pub density: f32,
    /// Edge compliance; 0 does not stretch.
    pub stretch: f32,
    /// Bend compliance; larger folds more easily.
    pub bend: f32,
    pub damping: f32,
    pub wind: [f32; 3],
    pub drag: f32,
    pub thickness: f32,
    pub self_collision: bool,
    /// Strain past which edges tear; 0 never tears.
    pub tear_strain: f32,
    pub substeps: u32,
    pub pin_top: bool,
    pub holder: Entity,
    /// Draw both sides of the sheet.
    pub visible: bool,
    /// Linear RGBA of the sheet, multiplied into `texture`.
    pub color: [f32; 4],
    /// Image stretched once over the sheet, top row at the top edge;
    /// empty for plain `color`.
    pub texture: std::path::PathBuf,
}

impl Default for ClothSheet {
    fn default() -> Self {
        let settings = ClothSettings::default();
        Self {
            spacing: 0.1,
            count_x: 10,
            count_y: 10,
            density: 0.3,
            stretch: settings.stretch_compliance,
            bend: settings.bend_compliance,
            damping: settings.damping,
            wind: settings.wind,
            drag: settings.drag,
            thickness: settings.thickness,
            self_collision: settings.self_collision,
            tear_strain: 0.0,
            substeps: settings.substeps,
            pin_top: true,
            holder: Entity::PLACEHOLDER,
            visible: true,
            color: CLOTH_COLOR,
            texture: std::path::PathBuf::new(),
        }
    }
}

impl ClothSheet {
    /// The cloth this sheet starts as on `transform`, and how many
    /// particles its top row (the first ones) has. Pins are not applied yet
    /// (see `spawn_cloths`).
    pub fn volume(
        &self,
        transform: &crate::Transform,
    ) -> Result<(ClothVolume, usize), String> {
        let mut counts = [self.count_x, self.count_y].map(|count| count.max(1));
        while counts.iter().map(|&c| u64::from(c) + 1).product::<u64>()
            > MAX_CLOTH_PARTICLES
        {
            let axis = usize::from(counts[1] > counts[0]);
            counts[axis] = (counts[axis] / 2).max(1);
        }
        let spacing = self.spacing.max(0.01);
        let rotation = body_rotation(transform);
        let right = rotation * Vector3::x();
        let down = rotation * -Vector3::y();
        let origin = vector(transform.position)
            - right * (counts[0] as f32 * spacing / 2.0)
            - down * (counts[1] as f32 * spacing / 2.0);
        let cloth = Cloth::grid(
            origin.into(),
            right.into(),
            down.into(),
            counts,
            spacing,
            self.density.max(1e-3),
        )?;
        let settings = ClothSettings {
            substeps: self.substeps,
            stretch_compliance: self.stretch.max(0.0),
            bend_compliance: self.bend.max(0.0),
            damping: self.damping.max(0.0),
            wind: self.wind,
            drag: self.drag.max(0.0),
            thickness: self.thickness.max(0.0),
            self_collision: self.self_collision,
            floor: None,
            tear_strain: (self.tear_strain > 0.0).then_some(self.tear_strain),
            ..ClothSettings::default()
        };
        Ok((ClothVolume::new(settings, cloth), counts[0] as usize + 1))
    }
}

/// Per fixed step, before [`super::soft_body::step_soft_bodies`]: gives
/// every [`ClothSheet`] without a volume its [`ClothVolume`].
pub(super) fn spawn_cloths(
    mut commands: Commands,
    mut assets: Option<ResMut<crate::assets::AssetServer>>,
    sheets: Query<
        (Entity, &ClothSheet, &crate::Transform),
        Without<ClothVolume>,
    >,
    holders: Query<&crate::Transform>,
) {
    for (entity, sheet, transform) in &sheets {
        let (mut volume, top) = match sheet.volume(transform) {
            Ok((volume, top)) => (volume, if sheet.pin_top { top } else { 0 }),
            Err(error) => {
                eprintln!("cloth {entity}: {error}");
                continue;
            }
        };
        match holders.get(sheet.holder) {
            Ok(holder) => {
                let rotation = body_rotation(holder);
                for particle in 0..top {
                    let offset = vector(volume.cloth.positions[particle])
                        - vector(holder.position);
                    volume.attachments.push(SoftAttachment {
                        particle,
                        body: sheet.holder,
                        local: (rotation.inverse() * offset).into(),
                    });
                }
            }
            Err(_) => (0..top).for_each(|particle| volume.cloth.pin(particle)),
        }
        if let Some(assets) = assets.as_deref_mut().filter(|_| sheet.visible) {
            volume.skin = Some(SoftSkin::new(sheet_material(sheet, assets)));
        }
        commands.entity(entity).insert(volume);
    }
}

/// Base color of a sheet with no `color` set.
const CLOTH_COLOR: [f32; 4] = [0.75, 0.2, 0.2, 1.0];

/// The shared plain cloth material, or one of the sheet's own for another
/// color or a texture. A texture that fails to load leaves the color.
fn sheet_material(
    sheet: &ClothSheet,
    assets: &mut crate::assets::AssetServer,
) -> crate::assets::Handle<crate::assets::MaterialAsset> {
    if sheet.color == CLOTH_COLOR && sheet.texture.as_os_str().is_empty() {
        return named_material(assets, "Cloth", CLOTH_COLOR, 0.8);
    }
    super::soft_body::textured_material(
        assets,
        "Cloth",
        sheet.color,
        &sheet.texture,
        0.8,
    )
}

/// Both sides of a cloth's triangles, each side with its own smooth
/// normals: the first half of the vertices faces the way the triangles
/// wind, the second half the other way. UVs stay pinned to the rest shape,
/// so a texture moves with the cloth; see [`rest_uvs`].
pub fn cloth_mesh(cloth: &Cloth) -> crate::assets::MeshAsset {
    let count = cloth.positions.len();
    let uvs = rest_uvs(cloth);
    let mut normals = vec![Vector3::zeros(); count];
    for &triangle in &cloth.triangles {
        let [a, b, c] = triangle.map(|i| vector(cloth.positions[i as usize]));
        let face = (b - a).cross(&(c - a));
        for i in triangle {
            normals[i as usize] += face;
        }
    }
    let mut vertices = Vec::with_capacity(2 * count);
    for side in [1.0, -1.0] {
        for ((&position, normal), &uv) in
            cloth.positions.iter().zip(&normals).zip(&uvs)
        {
            vertices.push(crate::assets::MeshVertex {
                position,
                uv,
                normal: (normal.try_normalize(1e-12).unwrap_or(Vector3::z())
                    * side)
                    .into(),
                ..crate::assets::MeshVertex::default()
            });
        }
    }
    let mut indices = Vec::with_capacity(6 * cloth.triangles.len());
    for &[a, b, c] in &cloth.triangles {
        indices.extend([a, b, c]);
    }
    let back = count as u32;
    for &[a, b, c] in &cloth.triangles {
        indices.extend([a + back, c + back, b + back]);
    }
    crate::assets::MeshAsset { vertices, indices }
}

/// Each particle's rest position projected on the plane of the first
/// triangle (u along its first edge), stretched to fill 0..1 on both axes
/// with v = 0 at the far end of the second axis, the top of an image. A
/// `Cloth::grid` sheet gets its grid as UVs: u along `u`, v down `v`.
// ponytail: one plane for the whole cloth; a cloth that is curved at rest
// (a sleeve) would need its own authored UVs.
fn rest_uvs(cloth: &Cloth) -> Vec<[f32; 2]> {
    let rest = &cloth.rest_positions;
    let axes = cloth.triangles.first().and_then(|&[a, b, c]| {
        let [a, b, c] = [a, b, c].map(|i| vector(rest[i as usize]));
        let u = (b - a).try_normalize(1e-12)?;
        let normal = u.cross(&(c - a)).try_normalize(1e-12)?;
        Some((u, normal.cross(&u)))
    });
    let Some((u, v)) = axes else {
        return vec![[0.0; 2]; rest.len()];
    };
    let projected: Vec<[f32; 2]> = rest
        .iter()
        .map(|&point| [vector(point).dot(&u), vector(point).dot(&v)])
        .collect();
    let range = |axis: usize| {
        projected
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| {
                (lo.min(p[axis]), hi.max(p[axis]))
            })
    };
    let ((u_low, u_high), (v_low, v_high)) = (range(0), range(1));
    let span = |low: f32, high: f32| (high - low).max(1e-12);
    projected
        .iter()
        .map(|&[pu, pv]| {
            [
                (pu - u_low) / span(u_low, u_high),
                (v_high - pv) / span(v_low, v_high),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ObstacleShape;

    /// A 1 m sheet in the XY plane hanging from its two top corners.
    fn hanging(cells: u32) -> Cloth {
        let spacing = 1.0 / cells as f32;
        let mut cloth = Cloth::grid(
            [0.0, 1.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [cells, cells],
            spacing,
            0.2,
        )
        .unwrap();
        let top = (cells + 1) * cells;
        cloth.pin(top as usize);
        cloth.pin((top + cells) as usize);
        cloth
    }

    #[test]
    fn cloth_uvs_follow_the_rest_grid_on_both_sides() {
        let mut cloth = hanging(4);
        // Moving the particles keeps their UVs: the texture moves along.
        for position in &mut cloth.positions {
            position[2] += position[1] * 0.5;
        }
        let mesh = cloth_mesh(&cloth);
        let count = cloth.positions.len();
        assert_eq!(mesh.vertices.len(), 2 * count);
        for y in 0..5 {
            for x in 0..5 {
                let at = (x + 5 * y) as usize;
                let want = [x as f32 / 4.0, 1.0 - y as f32 / 4.0];
                for side in [at, at + count] {
                    let uv = mesh.vertices[side].uv;
                    assert!(
                        (uv[0] - want[0]).abs() < 1e-5
                            && (uv[1] - want[1]).abs() < 1e-5,
                        "particle ({x}, {y}): {uv:?}, want {want:?}"
                    );
                }
            }
        }
    }

    fn settings() -> ClothSettings {
        ClothSettings {
            floor: None,
            ..ClothSettings::default()
        }
    }

    fn worst_strain(cloth: &Cloth) -> f32 {
        cloth
            .edges
            .iter()
            .zip(&cloth.edge_rests)
            .map(|(&[a, b], &rest)| {
                let length = (vector(cloth.positions[a as usize])
                    - vector(cloth.positions[b as usize]))
                .norm();
                (length / rest - 1.0).abs()
            })
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_grid_has_edges_bends_and_its_mass() {
        let cloth = hanging(2);
        assert_eq!(cloth.triangles.len(), 8);
        // 12 sides along the grid plus 4 diagonals.
        assert_eq!(cloth.edges.len(), 16);
        // One bend per edge shared by two triangles: 16 - 8 border edges.
        assert_eq!(cloth.bends.len(), 8);
        let mass: f32 = cloth
            .inverse_masses
            .iter()
            .filter(|&&w| w > 0.0)
            .map(|w| 1.0 / w)
            .sum();
        // Two pinned corners of 0.2 kg/m², each carrying a sixth or a third
        // of a cell.
        assert!(mass < 0.2 && mass > 0.15, "{mass}");
    }

    #[test]
    fn a_sheet_hangs_from_its_pins_without_stretching_and_repeats() {
        let run = || {
            let mut cloth = hanging(8);
            for _ in 0..120 {
                cloth.step(&settings(), 1.0 / 60.0);
            }
            cloth
        };
        let cloth = run();
        assert_eq!(cloth.positions[72], [0.0, 2.0, 0.0]);
        assert!(worst_strain(&cloth) < 0.05, "{}", worst_strain(&cloth));
        let lowest = cloth.positions.iter().map(|p| p[1]).fold(9.0, f32::min);
        assert!(lowest > 0.9 && lowest < 1.1, "{lowest}");
        assert_eq!(cloth.state_hash(), run().state_hash());
    }

    #[test]
    fn wind_blows_a_hanging_sheet_downwind() {
        let blown = |wind: [f32; 3]| {
            let mut cloth = hanging(6);
            let settings = ClothSettings { wind, ..settings() };
            for _ in 0..120 {
                cloth.step(&settings, 1.0 / 60.0);
            }
            cloth.positions.iter().map(|p| p[2]).sum::<f32>()
                / cloth.positions.len() as f32
        };
        assert!(blown([0.0; 3]).abs() < 1e-3);
        assert!(blown([0.0, 0.0, 5.0]) > 0.1, "{}", blown([0.0, 0.0, 5.0]));
        assert!(blown([0.0, 0.0, -5.0]) < -0.1);
    }

    #[test]
    fn self_collision_keeps_two_layers_apart() {
        // Two loose squares 5 cm apart, the lower one rising through where
        // the upper one is.
        let square = |y: f32| {
            [[0.0, y, 0.0], [0.1, y, 0.0], [0.1, y, 0.1], [0.0, y, 0.1]]
        };
        let mut positions = square(1.0).to_vec();
        positions.extend(square(1.05));
        let triangles = vec![[0, 1, 2], [0, 2, 3], [4, 5, 6], [4, 6, 7]];
        let gap = |self_collision: bool| {
            let mut cloth =
                Cloth::new(positions.clone(), triangles.clone(), 0.2).unwrap();
            for i in 0..4 {
                cloth.velocities[i] = [0.0, 1.0, 0.0];
            }
            let settings = ClothSettings {
                gravity: [0.0; 3],
                self_collision,
                thickness: 0.01,
                ..settings()
            };
            for _ in 0..10 {
                cloth.step(&settings, 1.0 / 60.0);
            }
            cloth.positions[4][1] - cloth.positions[0][1]
        };
        assert!(gap(true) > 0.019, "{}", gap(true));
        assert!(gap(false) < 0.0, "{}", gap(false));
    }

    #[test]
    fn an_overstretched_edge_tears_the_triangles_around_it() {
        let mut cloth = hanging(2);
        let mut stiff = cloth.clone();
        // Drag the middle particle far out of the sheet.
        cloth.positions[4] = [1.0, 1.5, 2.0];
        stiff.positions[4] = cloth.positions[4];
        assert_eq!(stiff.tear(10.0), 0);
        assert_eq!(cloth.tear(0.5), 8);
        assert!(cloth.triangles.is_empty());
        assert!(cloth.edges.is_empty());

        // In a step: a heavy pull on one corner of a sheet tears it.
        let mut cloth = hanging(4);
        cloth.velocities[0] = [0.0, -200.0, 0.0];
        cloth.inverse_masses[0] = 1e-3;
        let settings = ClothSettings {
            tear_strain: Some(0.5),
            ..settings()
        };
        let before = cloth.triangles.len();
        for _ in 0..30 {
            cloth.step(&settings, 1.0 / 60.0);
        }
        assert!(cloth.triangles.len() < before);
    }

    #[test]
    fn a_sheet_drapes_over_a_fixed_ball() {
        let mut cloth = Cloth::grid(
            [-0.5, 0.6, -0.5],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [10, 10],
            0.1,
            0.2,
        )
        .unwrap();
        let mut bodies = [AnchorBody {
            position: [0.0; 3],
            rotation: nalgebra::Rotation3::identity(),
            velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            inverse_mass: 0.0,
            inverse_inertia: nalgebra::Matrix3::zeros(),
        }];
        let obstacles = [Obstacle {
            body: 0,
            shape: ObstacleShape::Sphere(0.5),
        }];
        let settings = ClothSettings {
            floor: Some(-1.0),
            ..settings()
        };
        // Without friction it slides off in the end; check it while it
        // drapes.
        for _ in 0..45 {
            cloth.step_coupled(
                &settings,
                1.0 / 60.0,
                &mut bodies,
                &[],
                &obstacles,
            );
            for p in &cloth.positions {
                assert!(vector(*p).norm() > 0.49, "{p:?}");
            }
        }
        // The middle rests on top of the ball, the corners hang below it.
        let middle = cloth.positions[60];
        assert!((middle[1] - 0.5).abs() < 0.03, "{middle:?}");
        assert!(
            cloth.positions[0][1] < middle[1] - 0.1,
            "{:?}",
            cloth.positions[0]
        );
    }
}
