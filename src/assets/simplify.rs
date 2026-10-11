//! Mesh simplification for generated LOD levels.

use std::collections::{HashMap, HashSet};

use super::{MeshAsset, MeshVertex};

/// Finest grid tried along the mesh's longest side.
const MAX_GRID: u32 = 1024;

/// A coarser copy of `mesh` with at most `target` triangles (and as close to
/// it as the grid allows), made by merging the vertices inside each cell of
/// a uniform grid into one averaged vertex and dropping the triangles that
/// collapse.
// ponytail: vertex clustering ignores curvature and smooths hard edges and
// UV seams; switch to quadric edge collapse if close LODs look wrong.
#[must_use]
pub fn simplify_mesh(mesh: &MeshAsset, target: usize) -> MeshAsset {
    // The triangle count grows with the grid, near enough to search it.
    let (mut low, mut high) = (1, MAX_GRID);
    let mut best = cluster(mesh, 1);
    while low <= high {
        let grid = low + (high - low) / 2;
        let candidate = cluster(mesh, grid);
        if candidate.indices.len() / 3 <= target {
            best = candidate;
            low = grid + 1;
        } else {
            high = grid - 1;
        }
    }
    best
}

#[derive(Default)]
struct Sum {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
    color: [f32; 4],
    count: f32,
}

fn cluster(mesh: &MeshAsset, grid: u32) -> MeshAsset {
    let mut low = [f32::INFINITY; 3];
    let mut high = [f32::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            low[axis] = low[axis].min(vertex.position[axis]);
            high[axis] = high[axis].max(vertex.position[axis]);
        }
    }
    let side = (0..3)
        .map(|axis| high[axis] - low[axis])
        .fold(0.0_f32, f32::max)
        .max(f32::MIN_POSITIVE);
    let cell_size = side / grid as f32;
    let mut cells = HashMap::new();
    let mut sums: Vec<Sum> = Vec::new();
    let remap = mesh
        .vertices
        .iter()
        .map(|vertex| {
            let cell = std::array::from_fn::<u32, 3, _>(|axis| {
                (((vertex.position[axis] - low[axis]) / cell_size) as u32)
                    .min(grid - 1)
            });
            let at = *cells.entry(cell).or_insert_with(|| {
                sums.push(Sum::default());
                sums.len() - 1
            });
            let sum = &mut sums[at];
            for axis in 0..3 {
                sum.position[axis] += vertex.position[axis];
                sum.normal[axis] += vertex.normal[axis];
            }
            for axis in 0..2 {
                sum.uv[axis] += vertex.uv[axis];
            }
            for axis in 0..4 {
                sum.tangent[axis] += vertex.tangent[axis];
                sum.color[axis] += vertex.color[axis];
            }
            sum.count += 1.0;
            at as u32
        })
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut indices = Vec::new();
    for corners in mesh.indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|corner| {
            remap.get(corners[corner] as usize).copied().unwrap_or(0)
        });
        if a == b || b == c || a == c {
            continue;
        }
        // The same triangle turned to start at its smallest corner, so two
        // copies with the same winding count once.
        let turned = if a < b && a < c {
            [a, b, c]
        } else if b < c {
            [b, c, a]
        } else {
            [c, a, b]
        };
        if seen.insert(turned) {
            indices.extend(turned);
        }
    }
    let vertices = sums
        .into_iter()
        .map(|sum| {
            let scale = 1.0 / sum.count;
            let normal = nalgebra::Vector3::from(sum.normal)
                .try_normalize(1e-6)
                .unwrap_or_else(nalgebra::Vector3::y);
            let tangent = nalgebra::Vector3::new(
                sum.tangent[0],
                sum.tangent[1],
                sum.tangent[2],
            )
            .try_normalize(1e-6)
            .unwrap_or_else(nalgebra::Vector3::x);
            MeshVertex {
                position: sum.position.map(|value| value * scale),
                normal: normal.into(),
                uv: sum.uv.map(|value| value * scale),
                tangent: [
                    tangent.x,
                    tangent.y,
                    tangent.z,
                    sum.tangent[3].signum(),
                ],
                color: sum.color.map(|value| value * scale),
            }
        })
        .collect();
    MeshAsset { vertices, indices }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `size` x `size` cell grid on the XZ plane, 1 m per cell.
    fn grid(size: u32) -> MeshAsset {
        let row = size + 1;
        let vertices = (0..row * row)
            .map(|at| MeshVertex {
                position: [(at % row) as f32, 0.0, (at / row) as f32],
                uv: [(at % row) as f32, (at / row) as f32],
                ..MeshVertex::default()
            })
            .collect();
        let mut indices = Vec::new();
        for z in 0..size {
            for x in 0..size {
                let a = z * row + x;
                indices.extend([
                    a,
                    a + row,
                    a + 1,
                    a + 1,
                    a + row,
                    a + row + 1,
                ]);
            }
        }
        MeshAsset { vertices, indices }
    }

    #[test]
    fn simplified_meshes_meet_the_target_and_keep_their_shape() {
        let mesh = grid(32);
        assert_eq!(mesh.indices.len() / 3, 2048);
        let half = simplify_mesh(&mesh, 1024);
        let triangles = half.indices.len() / 3;
        assert!((512..=1024).contains(&triangles), "{triangles}");
        let coarse = simplify_mesh(&mesh, 64);
        let triangles = coarse.indices.len() / 3;
        assert!((16..=64).contains(&triangles), "{triangles}");
        for corners in coarse.indices.chunks_exact(3) {
            assert!(corners[0] != corners[1] && corners[1] != corners[2]);
            assert!(corners
                .iter()
                .all(|&at| (at as usize) < coarse.vertices.len()));
            // Winding is kept: the face still points up.
            let [a, b, c] = [0, 1, 2].map(|corner| {
                nalgebra::Vector3::from(
                    coarse.vertices[corners[corner] as usize].position,
                )
            });
            assert!((b - a).cross(&(c - a)).y > 0.0);
        }
        // Averaged vertices stay inside the original, and the coarse mesh
        // still spans most of it.
        let xs = coarse.vertices.iter().map(|vertex| vertex.position[0]);
        let (min, max) = xs
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), x| {
                (lo.min(x), hi.max(x))
            });
        assert!(min >= 0.0 && max <= 32.0);
        assert!(max - min > 24.0, "{min}..{max}");
    }
}
