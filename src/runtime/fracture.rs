//! Pre-fractured meshes: a [`Fracture`] on an object says how its mesh
//! breaks, by Voronoi cells around seeded random points, by slicing planes,
//! or both, and [`Fracture::fracture`] turns the mesh into convex pieces
//! plus the connectivity graph of which pieces touch.
//!
//! The source mesh is treated as convex: pieces fill its convex shape, so
//! a non-convex mesh loses its dents. Every piece is convex, so it can use
//! a `ConvexMesh` collider as it is.
//!
//! Determinism: seeds come from [`RandomSeed`] streams of the fracture's own
//! seed, cells are cut in index order, and nothing reads a global RNG, so
//! the same fracture and mesh always give the same bits.

use bevy_ecs::prelude::Component;
use nalgebra::Vector3;

use crate::assets::{MeshAsset, MeshVertex};
use crate::runtime::RandomSeed;

/// Most pieces one fracture makes, Voronoi cells and slice halves
/// together. Fracture settings come from scene files, and cutting costs
/// grow with the square of the piece count.
pub const MAX_FRACTURE_PIECES: usize = 256;

/// How an object's mesh breaks into pieces.
#[derive(
    Component, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(default)]
pub struct Fracture {
    /// Voronoi cells to cut the mesh into; 0 or 1 makes no Voronoi cuts.
    pub pieces: u32,
    /// Picks where the Voronoi cells lie.
    pub seed: u64,
    /// Planes `[x, y, z, d]` in the object's local frame, each cutting every
    /// piece along the points `p` with `[x, y, z] · p = d`.
    pub slices: Vec<[f32; 4]>,
}

impl Default for Fracture {
    fn default() -> Self {
        Self {
            pieces: 8,
            seed: 1,
            slices: Vec::new(),
        }
    }
}

/// The pieces of a fractured mesh, in the source mesh's frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FracturedMesh {
    pub pieces: Vec<MeshAsset>,
    /// Pairs of pieces (lower index first, sorted) that share a face of
    /// some area; touching only at an edge or a corner is no link.
    pub links: Vec<[u32; 2]>,
}

type V = Vector3<f32>;

/// The cut a face came from; the source surface is no cut.
const SURFACE: u32 = u32::MAX;
/// Cut numbers of slicing planes start here, above the Voronoi pairs.
const SLICE_CUTS: u32 = 1 << 20;

#[derive(Clone, Debug)]
struct Face {
    /// Counterclockwise seen from outside the cell.
    points: Vec<V>,
    cut: u32,
}

type Cell = Vec<Face>;

impl Fracture {
    /// Cuts `mesh` into convex pieces and finds which ones touch.
    pub fn fracture(&self, mesh: &MeshAsset) -> FracturedMesh {
        let points: Vec<V> =
            mesh.vertices.iter().map(|v| V::from(v.position)).collect();
        let Some(base) = hull_cell(mesh, &points) else {
            return FracturedMesh::default();
        };
        let low = points.iter().fold(points[0], |m, p| m.inf(p));
        let high = points.iter().fold(points[0], |m, p| m.sup(p));
        let size = (high - low).norm();
        let eps = 1e-5 * size;

        let count = (self.pieces as usize).min(MAX_FRACTURE_PIECES);
        let mut cells = if count > 1 {
            let seeds = seeds(&base, low, high, count, self.seed);
            voronoi(&base, &seeds, eps)
        } else {
            vec![base]
        };
        for (k, slice) in self.slices.iter().enumerate() {
            let normal = V::new(slice[0], slice[1], slice[2]);
            let Some(length) = Some(normal.norm()).filter(|l| *l > 1e-12)
            else {
                continue;
            };
            let (normal, offset) = (normal / length, slice[3] / length);
            let cut = SLICE_CUTS + k as u32;
            let mut halves = Vec::new();
            for (i, cell) in cells.iter().enumerate() {
                if halves.len() + cells.len() - i >= MAX_FRACTURE_PIECES {
                    halves.push(cell.clone());
                    continue;
                }
                halves.extend(
                    [(normal, offset), (-normal, -offset)]
                        .into_iter()
                        .map(|(n, d)| clip(cell, n, d, cut, eps))
                        .filter(|half| !half.is_empty()),
                );
            }
            cells = halves;
        }
        FracturedMesh {
            links: links(&cells, size * size * 1e-6),
            pieces: cells.iter().map(piece_mesh).collect(),
        }
    }
}

/// The convex shape of `mesh`: its box cut by every triangle's plane.
fn hull_cell(mesh: &MeshAsset, points: &[V]) -> Option<Cell> {
    let first = *points.first()?;
    let low = points.iter().fold(first, |m, p| m.inf(p));
    let high = points.iter().fold(first, |m, p| m.sup(p));
    let eps = 1e-5 * (high - low).norm();
    let center = points.iter().sum::<V>() / points.len() as f32;
    let indices: Vec<usize> = if mesh.indices.is_empty() {
        (0..points.len()).collect()
    } else {
        mesh.indices.iter().map(|&i| i as usize).collect()
    };
    let mut cell = box_cell(low, high);
    for triangle in indices.chunks_exact(3) {
        let Some([a, b, c]) = triangle
            .iter()
            .map(|&i| points.get(i).copied())
            .collect::<Option<Vec<V>>>()
            .and_then(|corners| <[V; 3]>::try_from(corners).ok())
        else {
            continue;
        };
        let Some(mut normal) = (b - a).cross(&(c - a)).try_normalize(1e-12)
        else {
            continue;
        };
        if normal.dot(&(a - center)) < 0.0 {
            normal = -normal;
        }
        cell = clip(&cell, normal, normal.dot(&a), SURFACE, eps);
    }
    (cell.len() >= 4).then_some(cell)
}

/// The box from `low` to `high`.
fn box_cell(low: V, high: V) -> Cell {
    let mut cell = Vec::new();
    for axis in 0..3 {
        let (b, c) = ((axis + 1) % 3, (axis + 2) % 3);
        for (at, outward) in [(high[axis], true), (low[axis], false)] {
            let corner = |u: f32, v: f32| {
                let mut p = V::zeros();
                p[axis] = at;
                p[b] = u;
                p[c] = v;
                p
            };
            // Counterclockwise around +axis in (b, c) order.
            let mut points = vec![
                corner(low[b], low[c]),
                corner(high[b], low[c]),
                corner(high[b], high[c]),
                corner(low[b], high[c]),
            ];
            if !outward {
                points.reverse();
            }
            cell.push(Face {
                points,
                cut: SURFACE,
            });
        }
    }
    cell
}

/// `count` points inside `cell`, drawn from the seed's stream.
fn seeds(cell: &Cell, low: V, high: V, count: usize, seed: u64) -> Vec<V> {
    let stream = RandomSeed(seed);
    let inside = |p: &V| {
        cell.iter().all(|face| {
            let normal = face_normal(&face.points);
            normal.dot(&(p - face.points[0])) < 0.0
        })
    };
    let mut seeds = Vec::with_capacity(count);
    let mut draw = 0u64;
    // A thin shape in a big box rejects most draws; give up rather than
    // loop forever, and make fewer pieces.
    while seeds.len() < count && draw < 64 * count as u64 {
        let p = V::from_fn(|axis, _| {
            let t = stream.unit(0, draw * 3 + axis as u64);
            low[axis] + (high[axis] - low[axis]) * t
        });
        draw += 1;
        if inside(&p) {
            seeds.push(p);
        }
    }
    seeds
}

/// The Voronoi cell of each seed within `base`.
fn voronoi(base: &Cell, seeds: &[V], eps: f32) -> Vec<Cell> {
    let n = seeds.len();
    let mut cells = Vec::with_capacity(n);
    for i in 0..n {
        let mut cell = base.clone();
        for j in 0..n {
            let Some(normal) = (seeds[j] - seeds[i]).try_normalize(1e-12)
            else {
                continue;
            };
            if cell.is_empty() {
                break;
            }
            let middle = 0.5 * (seeds[i] + seeds[j]);
            let cut = (i.min(j) * n + i.max(j)) as u32;
            cell = clip(&cell, normal, normal.dot(&middle), cut, eps);
        }
        if !cell.is_empty() {
            cells.push(cell);
        }
    }
    cells
}

/// The part of `points` (a convex polygon) with `normal · p <= offset`.
fn clip_polygon(points: &[V], normal: V, offset: f32) -> Vec<V> {
    let mut out = Vec::with_capacity(points.len() + 1);
    for (k, &a) in points.iter().enumerate() {
        let b = points[(k + 1) % points.len()];
        let (da, db) = (normal.dot(&a) - offset, normal.dot(&b) - offset);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            out.push(a + (b - a) * (da / (da - db)));
        }
    }
    out
}

/// The part of `cell` with `normal · p <= offset`, closed by a face of
/// `cut`. Empty when nothing of the cell is on that side.
fn clip(cell: &Cell, normal: V, offset: f32, cut: u32, eps: f32) -> Cell {
    let side = |p: &V| normal.dot(p) - offset;
    let all = || cell.iter().flat_map(|face| &face.points);
    if all().all(|p| side(p) <= eps) {
        // Nothing (or only a sliver within rounding) is cut off.
        return cell.clone();
    }
    if all().all(|p| side(p) >= -eps) {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(cell.len() + 1);
    let mut rim: Vec<V> = Vec::new();
    for face in cell {
        let points = clip_polygon(&face.points, normal, offset);
        for p in &points {
            if side(p).abs() <= eps
                && !rim.iter().any(|q| (q - p).norm() <= eps)
            {
                rim.push(*p);
            }
        }
        if points.len() >= 3 {
            out.push(Face {
                points,
                cut: face.cut,
            });
        }
    }
    if rim.len() >= 3 {
        let center = rim.iter().sum::<V>() / rim.len() as f32;
        let u = (rim[0] - center).normalize();
        let v = normal.cross(&u);
        // Counterclockwise around `normal`, which points out of the kept
        // side. A pseudo-angle keeps the order free of platform trig.
        let angle = |p: &V| {
            let (x, y) = ((p - center).dot(&u), (p - center).dot(&v));
            let t = y / (x.abs() + y.abs()).max(f32::MIN_POSITIVE);
            if x < 0.0 {
                2.0 - t
            } else if y < 0.0 {
                4.0 + t
            } else {
                t
            }
        };
        rim.sort_by(|a, b| angle(a).total_cmp(&angle(b)));
        out.push(Face { points: rim, cut });
    }
    if out.len() < 4 {
        return Vec::new();
    }
    out
}

/// Newell's normal of a polygon, unit length (zero when degenerate).
fn face_normal(points: &[V]) -> V {
    let mut normal = V::zeros();
    for (k, a) in points.iter().enumerate() {
        normal += a.cross(&points[(k + 1) % points.len()]);
    }
    normal.try_normalize(1e-20).unwrap_or_default()
}

fn area(points: &[V]) -> f32 {
    let mut sum = V::zeros();
    for (k, a) in points.iter().enumerate() {
        sum += a.cross(&points[(k + 1) % points.len()]);
    }
    0.5 * sum.norm()
}

/// Pairs of cells with faces of the same cut that overlap by more than
/// `min_area`.
fn links(cells: &[Cell], min_area: f32) -> Vec<[u32; 2]> {
    let mut links = Vec::new();
    for a in 0..cells.len() {
        for b in a + 1..cells.len() {
            let touching = cells[a].iter().any(|fa| {
                fa.cut != SURFACE
                    && cells[b].iter().any(|fb| {
                        fb.cut == fa.cut
                            && overlap(&fa.points, &fb.points) > min_area
                    })
            });
            if touching {
                links.push([a as u32, b as u32]);
            }
        }
    }
    links
}

/// Area two coplanar convex polygons share.
fn overlap(a: &[V], b: &[V]) -> f32 {
    let normal = face_normal(b);
    let mut shared = a.to_vec();
    for (k, p) in b.iter().enumerate() {
        if shared.len() < 3 {
            return 0.0;
        }
        let edge = b[(k + 1) % b.len()] - p;
        // Inside is left of each edge seen along the outward normal.
        let out = edge.cross(&normal);
        shared = clip_polygon(&shared, out, out.dot(p));
    }
    if shared.len() < 3 {
        return 0.0;
    }
    area(&shared)
}

/// Flat-shaded triangles of a cell's faces.
fn piece_mesh(cell: &Cell) -> MeshAsset {
    let mut mesh = MeshAsset::default();
    for face in cell {
        let normal = face_normal(&face.points);
        let u = (face.points[1] - face.points[0])
            .try_normalize(1e-12)
            .unwrap_or_else(V::x);
        let v = normal.cross(&u);
        let first = mesh.vertices.len() as u32;
        mesh.vertices.extend(face.points.iter().map(|p| MeshVertex {
            position: (*p).into(),
            normal: normal.into(),
            uv: [p.dot(&u), p.dot(&v)],
            tangent: [u.x, u.y, u.z, 1.0],
            color: [1.0; 4],
        }));
        for k in 1..face.points.len() as u32 - 1 {
            mesh.indices.extend([first, first + k, first + k + 1]);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_mesh(half: [f32; 3]) -> MeshAsset {
        piece_mesh(&box_cell(-V::from(half), V::from(half)))
    }

    fn volume(mesh: &MeshAsset) -> f32 {
        mesh.indices
            .chunks_exact(3)
            .map(|t| {
                let [a, b, c] = [0, 1, 2]
                    .map(|k| V::from(mesh.vertices[t[k] as usize].position));
                a.dot(&b.cross(&c)) / 6.0
            })
            .sum()
    }

    fn connected(count: usize, links: &[[u32; 2]]) -> bool {
        let mut reached = vec![false; count];
        let mut stack = vec![0];
        reached[0] = true;
        while let Some(at) = stack.pop() {
            for &[a, b] in links {
                let (a, b) = (a as usize, b as usize);
                for (from, to) in [(a, b), (b, a)] {
                    if from == at && !reached[to] {
                        reached[to] = true;
                        stack.push(to);
                    }
                }
            }
        }
        reached.iter().all(|&r| r)
    }

    #[test]
    fn two_slices_quarter_a_box_and_link_only_face_neighbours() {
        let fracture = Fracture {
            pieces: 0,
            slices: vec![[1.0, 0.0, 0.0, 0.0], [0.0, 2.0, 0.0, 0.0]],
            ..Fracture::default()
        };
        let broken = fracture.fracture(&box_mesh([1.0; 3]));
        assert_eq!(broken.pieces.len(), 4);
        for piece in &broken.pieces {
            assert!((volume(piece) - 2.0).abs() < 1e-4, "{}", volume(piece));
        }
        // Order: x halves first, each split in y. Diagonal quarters only
        // share an edge.
        assert_eq!(broken.links, vec![[0, 1], [0, 2], [1, 3], [2, 3]]);
    }

    #[test]
    fn voronoi_cells_fill_a_prism_and_form_one_connected_graph() {
        // A triangular prism, so the hull clipping matters.
        let prism = crate::runtime::Polygon2d::default().mesh();
        let fracture = Fracture {
            pieces: 12,
            seed: 7,
            slices: vec![[0.0, 0.0, 1.0, 0.1]],
        };
        let broken = fracture.fracture(&prism);
        assert!(broken.pieces.len() > 12, "{}", broken.pieces.len());
        let total: f32 = broken.pieces.iter().map(volume).sum();
        assert!((total - 0.5).abs() < 1e-3, "pieces fill {total} m³ of 0.5");
        for piece in &broken.pieces {
            assert!(volume(piece) > 0.0);
        }
        assert!(connected(broken.pieces.len(), &broken.links));
        // A cell touches a handful of neighbours, not every other cell.
        assert!(broken.links.len() < 6 * broken.pieces.len());
    }

    #[test]
    fn the_seed_alone_picks_the_cells() {
        let mesh = box_mesh([1.0, 0.5, 0.25]);
        let at = |seed| {
            Fracture {
                pieces: 10,
                seed,
                ..Fracture::default()
            }
            .fracture(&mesh)
        };
        assert_eq!(at(3), at(3));
        assert_ne!(at(3), at(4));
    }

    #[test]
    fn a_huge_piece_count_is_capped() {
        let broken = Fracture {
            pieces: u32::MAX,
            slices: vec![[1.0, 0.0, 0.0, 0.0]; 20],
            ..Fracture::default()
        }
        .fracture(&box_mesh([1.0; 3]));
        assert!(broken.pieces.len() <= MAX_FRACTURE_PIECES);
        assert!(Fracture::default()
            .fracture(&MeshAsset::default())
            .pieces
            .is_empty());
    }
}
