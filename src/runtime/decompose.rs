//! Approximate convex decomposition (V-HACD-class) of a closed mesh, so a
//! concave imported mesh can collide as a few convex hulls instead of a
//! static triangle mesh: add [`ConvexDecomposition`] next to a `ConvexMesh`
//! collider.
//!
//! The mesh is voxelized solid, then its box is split by axis-aligned
//! planes. A region's concavity is the volume of the convex hull of the
//! mesh surface clipped to it, minus the solid volume inside it. The most
//! concave region splits next, at the voxel plane that leaves the least
//! hull volume, until every region is convex enough or `max_hulls` is
//! reached. Each region's hull is built from the clipped surface, so the
//! hulls follow the mesh, not the voxels. Everything runs in a fixed
//! order: the same mesh gives the same hulls.

use std::collections::{BTreeMap, HashSet};

use bevy_ecs::prelude::Component;
use nalgebra::Vector3;

use crate::assets::{MeshAsset, MeshVertex};

type V = Vector3<f32>;

/// How finely [`decompose`] looks and how far it goes. As a component next
/// to a `ConvexMesh` collider, the CPU solver collides the object as the
/// hulls of its `MeshRenderer` mesh (scaled by its transform) instead of
/// the one hull around it, so a concave prop can be dynamic.
#[derive(
    Component,
    Clone,
    Copy,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct ConvexDecomposition {
    /// Voxels along the mesh's longest side, at most [`MAX_RESOLUTION`].
    pub resolution: u32,
    /// Most hulls returned, at most [`MAX_HULLS`].
    pub max_hulls: u32,
    /// A region stops splitting once its hull exceeds the solid inside it
    /// by less than this share of the whole mesh's volume.
    pub concavity: f32,
    /// Split positions tried per axis, spread evenly over the region; at
    /// `resolution` or more every voxel plane is tried.
    pub candidates: u32,
}

impl Default for ConvexDecomposition {
    fn default() -> Self {
        Self {
            resolution: 32,
            max_hulls: 16,
            concavity: 0.01,
            candidates: 32,
        }
    }
}

/// Largest [`ConvexDecomposition::resolution`] used: scene files set it,
/// and the voxel grid grows with its cube.
pub const MAX_RESOLUTION: u32 = 128;
/// Largest [`ConvexDecomposition::max_hulls`] used.
pub const MAX_HULLS: u32 = 256;

/// Convex hulls covering `mesh`, as meshes in its frame. A mesh that is not
/// closed, or too flat to fill a voxel, comes back as its one hull; an
/// empty or flat mesh gives none.
#[must_use]
pub fn decompose(
    mesh: &MeshAsset,
    settings: ConvexDecomposition,
) -> Vec<MeshAsset> {
    let triangles: Vec<[V; 3]> = mesh
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|t| {
            let corner = |k: usize| {
                mesh.vertices
                    .get(t[k] as usize)
                    .map(|vertex| V::from(vertex.position))
            };
            Some([corner(0)?, corner(1)?, corner(2)?])
        })
        .collect();
    let points: Vec<V> = triangles.iter().flatten().copied().collect();
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let low = points.iter().fold(first, |m, p| m.inf(p));
    let high = points.iter().fold(first, |m, p| m.sup(p));
    let whole = || hull_mesh(&points).into_iter().collect();
    let resolution = settings.resolution.clamp(1, MAX_RESOLUTION);
    let max_hulls = settings.max_hulls.min(MAX_HULLS);
    let size = (high - low).max() / resolution as f32;
    if size <= 0.0 || max_hulls <= 1 {
        return whole();
    }
    let grid = Grid::new(&triangles, low, high, size);
    if grid.solid_total == 0 {
        return whole();
    }
    let total = grid.solid_total as f32 * size.powi(3);
    let mut regions = vec![grid.region([0; 3], grid.cells, &triangles)];
    while regions.len() < max_hulls as usize {
        // The most concave region still worth splitting, lowest index on ties.
        let Some((index, _)) = regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.concavity > settings.concavity * total)
            .filter(|(_, r)| (0..3).any(|a| r.high[a] - r.low[a] > 1))
            .fold(None::<(usize, f32)>, |best, (i, r)| match best {
                Some((_, c)) if c >= r.concavity => best,
                _ => Some((i, r.concavity)),
            })
        else {
            break;
        };
        let region = &regions[index];
        let mut best: Option<(f32, Region, Region)> = None;
        for axis in 0..3 {
            let span = region.high[axis] - region.low[axis];
            let tries = settings.candidates.max(1).min(span.saturating_sub(1));
            for k in 1..=tries {
                let at = region.low[axis] + span * k / (tries + 1);
                let mut split = region.high;
                split[axis] = at;
                let left = grid.region(region.low, split, &triangles);
                let mut start = region.low;
                start[axis] = at;
                let right = grid.region(start, region.high, &triangles);
                let cost = left.volume + right.volume;
                if best.as_ref().is_none_or(|(c, ..)| cost < *c) {
                    best = Some((cost, left, right));
                }
            }
        }
        let Some((_, left, right)) = best else {
            break;
        };
        regions.splice(index..=index, [left, right]);
    }
    regions.into_iter().filter_map(|r| r.hull).collect()
}

/// One box of voxels `[low, high)` and what [`decompose`] knows of it.
struct Region {
    low: [u32; 3],
    high: [u32; 3],
    hull: Option<MeshAsset>,
    volume: f32,
    concavity: f32,
}

/// The solid voxels of a mesh.
struct Grid {
    origin: V,
    size: f32,
    cells: [u32; 3],
    solid: Vec<bool>,
    solid_total: usize,
}

impl Grid {
    /// Fills each voxel whose centre a vertical ray from below finds inside
    /// the mesh (an odd number of crossings under it).
    fn new(triangles: &[[V; 3]], low: V, high: V, size: f32) -> Self {
        let cells = [0, 1, 2]
            .map(|a| (((high[a] - low[a]) / size).ceil() as u32).max(1));
        let [nx, ny, nz] = cells.map(|c| c as usize);
        let mut solid = vec![false; nx * ny * nz];
        let eps = 1e-5 * size;
        for z in 0..nz {
            for x in 0..nx {
                let px = low.x + (x as f32 + 0.5) * size;
                let pz = low.z + (z as f32 + 0.5) * size;
                let mut hits: Vec<f32> = triangles
                    .iter()
                    .filter_map(|t| vertical_hit(t, px, pz))
                    .collect();
                hits.sort_by(f32::total_cmp);
                // A ray through a shared edge hits both triangles.
                hits.dedup_by(|a, b| (*a - *b).abs() <= eps);
                for pair in hits.as_chunks::<2>().0 {
                    for y in 0..ny {
                        let py = low.y + (y as f32 + 0.5) * size;
                        if py > pair[0] && py < pair[1] {
                            solid[(z * ny + y) * nx + x] = true;
                        }
                    }
                }
            }
        }
        let solid_total = solid.iter().filter(|&&s| s).count();
        Self {
            origin: low,
            size,
            cells,
            solid,
            solid_total,
        }
    }

    fn region(
        &self,
        low: [u32; 3],
        high: [u32; 3],
        triangles: &[[V; 3]],
    ) -> Region {
        let [nx, ny, _] = self.cells.map(|c| c as usize);
        let mut inside = 0usize;
        // Where the box cuts through solid the hull must reach the cut,
        // which the clipped surface alone does not show. Per cut plane and
        // grid line on it: the first and last solid voxel corner.
        let mut cuts: BTreeMap<(usize, u32, u32), (u32, u32)> = BTreeMap::new();
        for z in low[2]..high[2] {
            for y in low[1]..high[1] {
                for x in low[0]..high[0] {
                    if !self.solid
                        [(z as usize * ny + y as usize) * nx + x as usize]
                    {
                        continue;
                    }
                    inside += 1;
                    let cell = [x, y, z];
                    for axis in 0..3 {
                        let plane = if cell[axis] == low[axis] && low[axis] > 0
                        {
                            low[axis]
                        } else if cell[axis] + 1 == high[axis]
                            && high[axis] < self.cells[axis]
                        {
                            high[axis]
                        } else {
                            continue;
                        };
                        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
                        for row in [cell[b], cell[b] + 1] {
                            let span = cuts
                                .entry((axis, plane, row))
                                .or_insert((cell[c], cell[c] + 1));
                            span.0 = span.0.min(cell[c]);
                            span.1 = span.1.max(cell[c] + 1);
                        }
                    }
                }
            }
        }
        // Region edges on the mesh's own bounds stay open, so rounding the
        // grid up never trims the mesh.
        let corner = |cell: [u32; 3], open: [bool; 3], sign: f32| {
            V::from([0, 1, 2].map(|a| {
                if open[a] {
                    sign * f32::INFINITY
                } else {
                    self.origin[a] + cell[a] as f32 * self.size
                }
            }))
        };
        let box_low = corner(low, low.map(|c| c == 0), -1.0);
        let box_high =
            corner(high, [0, 1, 2].map(|a| high[a] == self.cells[a]), 1.0);
        let mut points: Vec<V> = triangles
            .iter()
            .flat_map(|t| clip(t, box_low, box_high))
            .collect();
        for (&(axis, plane, row), &(first, last)) in &cuts {
            for end in [first, last] {
                let mut cell = [0; 3];
                cell[axis] = plane;
                cell[(axis + 1) % 3] = row;
                cell[(axis + 2) % 3] = end;
                points.push(
                    self.origin + V::from(cell.map(|c| c as f32)) * self.size,
                );
            }
        }
        let hull = hull_mesh(&points);
        let volume = hull.as_ref().map_or(0.0, mesh_volume);
        let solid = inside as f32 * self.size.powi(3);
        Region {
            low,
            high,
            hull,
            volume,
            concavity: (volume - solid).max(0.0),
        }
    }
}

/// Height where the vertical line at `(x, z)` crosses triangle `t`.
fn vertical_hit(t: &[V; 3], x: f32, z: f32) -> Option<f32> {
    let [a, b, c] = t;
    let det = (b.x - a.x) * (c.z - a.z) - (c.x - a.x) * (b.z - a.z);
    if det.abs() < f32::EPSILON * 1e-3 {
        return None;
    }
    let u = ((x - a.x) * (c.z - a.z) - (c.x - a.x) * (z - a.z)) / det;
    let v = ((b.x - a.x) * (z - a.z) - (x - a.x) * (b.z - a.z)) / det;
    (u >= 0.0 && v >= 0.0 && u + v <= 1.0)
        .then(|| a.y + u * (b.y - a.y) + v * (c.y - a.y))
}

/// The part of triangle `t` inside the box `[low, high]`.
fn clip(t: &[V; 3], low: V, high: V) -> Vec<V> {
    let normal = (t[1] - t[0]).cross(&(t[2] - t[0]));
    let mut polygon = t.to_vec();
    for axis in 0..3 {
        for (bound, keep_below) in [(high[axis], true), (low[axis], false)] {
            if !bound.is_finite() || polygon.is_empty() {
                continue;
            }
            let inside =
                |p: &V| (p[axis] <= bound) == keep_below || p[axis] == bound;
            let mut next = Vec::with_capacity(polygon.len() + 2);
            for (i, p) in polygon.iter().enumerate() {
                let q = polygon[(i + 1) % polygon.len()];
                if inside(p) {
                    next.push(*p);
                }
                if inside(p) != inside(&q) {
                    let s = (bound - p[axis]) / (q[axis] - p[axis]);
                    next.push(p + (q - p) * s);
                }
            }
            // What is left on the bound plane itself only belongs to the box
            // when it is a face looking out of it: a face looking in is the
            // surface of a touching neighbour, and an edge touching the
            // plane adds no area.
            let facing_out = if keep_below {
                normal[axis] > 0.0
            } else {
                normal[axis] < 0.0
            };
            if !facing_out && next.iter().all(|p| p[axis] == bound) {
                return Vec::new();
            }
            polygon = next;
        }
    }
    polygon
}

/// Signed volume of a closed, outward-wound mesh.
fn mesh_volume(mesh: &MeshAsset) -> f32 {
    mesh.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let [a, b, c] = [0, 1, 2]
                .map(|k| V::from(mesh.vertices[t[k] as usize].position));
            a.dot(&b.cross(&c)) / 6.0
        })
        .sum()
}

/// The convex hull of `points` as a flat-shaded mesh, or `None` when they
/// span no volume.
fn hull_mesh(points: &[V]) -> Option<MeshAsset> {
    let (corners, faces) = hull(points)?;
    let mut mesh = MeshAsset::default();
    for [a, b, c] in faces {
        let [pa, pb, pc] = [corners[a], corners[b], corners[c]];
        let normal = (pb - pa).cross(&(pc - pa)).normalize();
        let u = (pb - pa).normalize();
        let first = mesh.vertices.len() as u32;
        mesh.vertices.extend([pa, pb, pc].map(|p| MeshVertex {
            position: p.into(),
            normal: normal.into(),
            uv: [p.dot(&u), p.dot(&normal.cross(&u))],
            tangent: [u.x, u.y, u.z, 1.0],
            color: [1.0; 4],
        }));
        mesh.indices.extend([first, first + 1, first + 2]);
    }
    Some(mesh)
}

/// Incremental 3D convex hull: the hull's points and its outward
/// counterclockwise triangles indexing them. `None` for flat input.
// ponytail: O(points * faces); switch to quickhull's conflict lists if
// decomposition of big meshes gets slow.
fn hull(points: &[V]) -> Option<(Vec<V>, Vec<[usize; 3]>)> {
    let first = *points.first()?;
    let low = points.iter().fold(first, |m, p| m.inf(p));
    let high = points.iter().fold(first, |m, p| m.sup(p));
    let eps = 1e-6 * (high - low).max().max(f32::MIN_POSITIVE);
    // A starting tetrahedron of well-spread points.
    let a = first;
    let b = *points
        .iter()
        .max_by(|p, q| (*p - a).norm().total_cmp(&(*q - a).norm()))?;
    let line = |p: &V| (p - a).cross(&(b - a)).norm();
    let c = *points.iter().max_by(|p, q| line(p).total_cmp(&line(q)))?;
    let normal = (b - a).cross(&(c - a));
    if normal.norm() <= eps * eps {
        return None;
    }
    let plane = |p: &V| (p - a).dot(&normal);
    let d = *points
        .iter()
        .max_by(|p, q| plane(p).abs().total_cmp(&plane(q).abs()))?;
    if plane(&d).abs() <= eps * normal.norm() * 1e-2 {
        return None;
    }
    let mut corners = vec![a, b, c, d];
    let mut faces = if plane(&d) > 0.0 {
        vec![[0, 2, 1], [0, 1, 3], [1, 2, 3], [2, 0, 3]]
    } else {
        vec![[0, 1, 2], [0, 3, 1], [1, 3, 2], [2, 3, 0]]
    };
    let outside = |corners: &[V], f: &[usize; 3], p: &V| {
        let [pa, pb, pc] = f.map(|i| corners[i]);
        let n = (pb - pa).cross(&(pc - pa));
        (p - pa).dot(&n) > eps * n.norm()
    };
    for p in points {
        let visible: Vec<bool> =
            faces.iter().map(|f| outside(&corners, f, p)).collect();
        if !visible.contains(&true) {
            continue;
        }
        let edges: HashSet<(usize, usize)> = faces
            .iter()
            .zip(&visible)
            .filter(|(_, &v)| v)
            .flat_map(|(f, _)| [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])])
            .collect();
        let index = corners.len();
        corners.push(*p);
        let mut next = Vec::with_capacity(faces.len() + 4);
        for (f, &v) in faces.iter().zip(&visible) {
            if !v {
                next.push(*f);
                continue;
            }
            // Horizon edges: their other face stays.
            for (s, e) in [(f[0], f[1]), (f[1], f[2]), (f[2], f[0])] {
                if !edges.contains(&(e, s)) {
                    next.push([s, e, index]);
                }
            }
        }
        faces = next;
    }
    Some((corners, faces))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Closed box mesh `[low, high]`, outward wound.
    fn cuboid(low: V, high: V, mesh: &mut MeshAsset) {
        let p = |x: usize, y: usize, z: usize| {
            V::new([low.x, high.x][x], [low.y, high.y][y], [low.z, high.z][z])
        };
        let quads = [
            [p(0, 0, 0), p(0, 0, 1), p(0, 1, 1), p(0, 1, 0)],
            [p(1, 0, 0), p(1, 1, 0), p(1, 1, 1), p(1, 0, 1)],
            [p(0, 0, 0), p(1, 0, 0), p(1, 0, 1), p(0, 0, 1)],
            [p(0, 1, 0), p(0, 1, 1), p(1, 1, 1), p(1, 1, 0)],
            [p(0, 0, 0), p(0, 1, 0), p(1, 1, 0), p(1, 0, 0)],
            [p(0, 0, 1), p(1, 0, 1), p(1, 1, 1), p(0, 1, 1)],
        ];
        for quad in quads {
            let first = mesh.vertices.len() as u32;
            mesh.vertices.extend(quad.map(|p| MeshVertex {
                position: p.into(),
                ..MeshVertex::default()
            }));
            mesh.indices.extend([
                first,
                first + 1,
                first + 2,
                first,
                first + 2,
                first + 3,
            ]);
        }
    }

    fn bounds(mesh: &MeshAsset) -> [[f32; 3]; 2] {
        let points = mesh.vertices.iter().map(|v| V::from(v.position));
        let low = points
            .clone()
            .fold(V::repeat(f32::INFINITY), |m, p| m.inf(&p));
        let high = points.fold(V::repeat(f32::NEG_INFINITY), |m, p| m.sup(&p));
        [low.into(), high.into()]
    }

    #[test]
    fn a_convex_mesh_stays_one_hull_and_a_hull_is_closed() {
        let mut mesh = MeshAsset::default();
        cuboid(V::new(-1.0, -0.5, -0.25), V::new(1.0, 0.5, 0.25), &mut mesh);
        let hulls = decompose(&mesh, ConvexDecomposition::default());
        assert_eq!(hulls.len(), 1);
        assert!((mesh_volume(&hulls[0]) - 1.0).abs() < 1e-4);
        assert_eq!(bounds(&hulls[0]), [[-1.0, -0.5, -0.25], [1.0, 0.5, 0.25]]);
    }

    #[test]
    fn an_l_shape_splits_into_two_hulls_that_follow_the_mesh() {
        // Two closed boxes meeting at x 0: an L, 1 + 0.5 = 1.5 m³. Its hull
        // would be 1.75 m³.
        let mut mesh = MeshAsset::default();
        cuboid(V::new(-1.0, 0.0, 0.0), V::new(0.0, 1.0, 1.0), &mut mesh);
        cuboid(V::new(0.0, 0.0, 0.0), V::new(1.0, 0.5, 1.0), &mut mesh);
        let hulls = decompose(&mesh, ConvexDecomposition::default());
        assert_eq!(
            hulls.len(),
            2,
            "{:?}",
            hulls.iter().map(bounds).collect::<Vec<_>>()
        );
        let volume: f32 = hulls.iter().map(mesh_volume).sum();
        assert!((volume - 1.5).abs() < 0.02, "{volume}");
        let mut boxes: Vec<_> = hulls.iter().map(bounds).collect();
        boxes.sort_by(|a, b| a[0][0].total_cmp(&b[0][0]));
        assert_eq!(boxes[0][1][1], 1.0);
        assert_eq!(boxes[1][1][1], 0.5);
        // The same mesh decomposes the same way.
        assert_eq!(decompose(&mesh, ConvexDecomposition::default()), hulls);
    }

    #[test]
    fn a_cup_needs_several_hulls_and_max_hulls_caps_them() {
        // A square cup: a floor and four walls, as closed boxes.
        let mut mesh = MeshAsset::default();
        cuboid(V::new(-1.0, 0.0, -1.0), V::new(1.0, 0.2, 1.0), &mut mesh);
        cuboid(V::new(-1.0, 0.2, -1.0), V::new(-0.8, 1.0, 1.0), &mut mesh);
        cuboid(V::new(0.8, 0.2, -1.0), V::new(1.0, 1.0, 1.0), &mut mesh);
        cuboid(V::new(-0.8, 0.2, -1.0), V::new(0.8, 1.0, -0.8), &mut mesh);
        cuboid(V::new(-0.8, 0.2, 0.8), V::new(0.8, 1.0, 1.0), &mut mesh);
        let solid = 0.8 + 2.0 * 0.32 + 2.0 * 0.256;
        let hulls = decompose(&mesh, ConvexDecomposition::default());
        assert!(hulls.len() >= 5, "{}", hulls.len());
        let volume: f32 = hulls.iter().map(mesh_volume).sum();
        // The cup's own hull would be 4 m³.
        assert!(volume < 1.15 * solid, "{volume} against {solid}");
        let capped = decompose(
            &mesh,
            ConvexDecomposition {
                max_hulls: 3,
                ..ConvexDecomposition::default()
            },
        );
        assert_eq!(capped.len(), 3);
    }

    #[test]
    fn an_open_or_empty_mesh_gives_its_hull_or_nothing() {
        assert!(decompose(
            &MeshAsset::default(),
            ConvexDecomposition::default()
        )
        .is_empty());
        let mut mesh = MeshAsset::default();
        cuboid(V::new(0.0, 0.0, 0.0), V::new(1.0, 1.0, 1.0), &mut mesh);
        // Drop the top face: no voxel is inside, so one hull of all points.
        mesh.indices.drain(18..24);
        let hulls = decompose(&mesh, ConvexDecomposition::default());
        assert_eq!(hulls.len(), 1);
        assert!((mesh_volume(&hulls[0]) - 1.0).abs() < 1e-4);
    }

    /// Where a small box dropped into a dynamic cup on the ground comes to
    /// rest, with and without the cup decomposed.
    fn drop_into_cup(decomposed: bool) -> f32 {
        use crate::assets::AssetServer;
        use crate::runtime::cpu_physics::next_spawn_order;
        use crate::runtime::{
            App, Collider, ColliderShape, MeshRenderer, PhysicsBody, RigidBody,
            RigidBodyKind,
        };
        use crate::Transform;
        let mut app = App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        let world = app.world_mut();
        world.spawn((
            Transform::new([0.0, -0.5, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::Box {
                    half_extents: [5.0, 0.5, 5.0],
                },
                ..Collider::default()
            },
        ));
        let mut cup = MeshAsset::default();
        cuboid(V::new(-1.0, 0.0, -1.0), V::new(1.0, 0.2, 1.0), &mut cup);
        cuboid(V::new(-1.0, 0.2, -1.0), V::new(-0.8, 1.0, 1.0), &mut cup);
        cuboid(V::new(0.8, 0.2, -1.0), V::new(1.0, 1.0, 1.0), &mut cup);
        cuboid(V::new(-0.8, 0.2, -1.0), V::new(0.8, 1.0, -0.8), &mut cup);
        cuboid(V::new(-0.8, 0.2, 0.8), V::new(0.8, 1.0, 1.0), &mut cup);
        let (mesh, material) = {
            let mut assets = world.resource_mut::<AssetServer>();
            let material = assets.fallback_material;
            (assets.meshes.insert(cup), material)
        };
        let order = next_spawn_order(world);
        let mut body = world.spawn((
            Transform::new([0.0, 0.0, 0.0]),
            PhysicsBody::default(),
            RigidBody {
                mass: 20.0,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::ConvexMesh,
                ..Collider::default()
            },
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
            order,
        ));
        if decomposed {
            body.insert(ConvexDecomposition::default());
        }
        let order = next_spawn_order(world);
        let falling = world
            .spawn((
                Transform::new([0.0, 2.0, 0.0]),
                PhysicsBody::default(),
                RigidBody::default(),
                Collider {
                    shape: ColliderShape::Box {
                        half_extents: [0.2; 3],
                    },
                    ..Collider::default()
                },
                order,
            ))
            .id();
        for _ in 0..120 {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        }
        app.world().get::<Transform>(falling).unwrap().position[1]
    }

    #[test]
    fn a_decomposed_dynamic_cup_holds_what_falls_into_it() {
        // The cup's floor is 0.2 m thick, so the box rests at 0.4 inside it;
        // on the one hull around the cup it would rest on top, near 1.2.
        let inside = drop_into_cup(true);
        assert!((inside - 0.4).abs() < 0.05, "{inside}");
        let on_top = drop_into_cup(false);
        assert!((on_top - 1.2).abs() < 0.05, "{on_top}");
    }

    #[test]
    fn huge_settings_from_a_scene_file_are_clamped() {
        let mut mesh = MeshAsset::default();
        cuboid(V::new(0.0, 0.0, 0.0), V::new(1.0, 1.0, 1.0), &mut mesh);
        let hulls = decompose(
            &mesh,
            ConvexDecomposition {
                resolution: u32::MAX,
                max_hulls: u32::MAX,
                candidates: 1,
                ..ConvexDecomposition::default()
            },
        );
        assert_eq!(hulls.len(), 1);
    }
}
