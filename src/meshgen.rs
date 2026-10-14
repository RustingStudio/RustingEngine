//! Deterministic placeholder meshes: a preset name and a seed give the same
//! low-poly binary glTF on every machine, in the texture palette, so
//! `rusting asset generate <root> mesh "barrel 3"` puts a prop in a scene
//! that is not a grey cube. Meshes carry no normals; the importer shades
//! them flat, which is the style.

/// Preset names `synth` accepts.
pub const PRESETS: [&str; 5] = ["crate", "barrel", "rock", "tree", "gem"];

/// One coloured part of a mesh: indexed triangles, counter-clockwise seen
/// from outside, y up, metres.
struct Part {
    color: [f32; 3],
    positions: Vec<[f32; 3]>,
    indices: Vec<u32>,
}

/// A closed surface of revolution: `profile` runs bottom to top as
/// `(radius, height)`, both ends on the axis. `sides` vertices per ring,
/// the first at `turn` of a full circle; `radius_at` may move each vertex
/// in or out (the rock's lumps).
fn lathe(
    profile: &[(f32, f32)],
    sides: u32,
    turn: f32,
    radius_at: impl Fn(u32, u32) -> f32,
) -> (Vec<[f32; 3]>, Vec<u32>) {
    let last = profile.len() as u32 - 1;
    let mut positions = vec![[0.0, profile[0].1, 0.0]];
    for (ring, &(radius, height)) in profile.iter().enumerate().skip(1) {
        let ring = ring as u32;
        if ring == last {
            break;
        }
        for side in 0..sides {
            let angle =
                std::f32::consts::TAU * (side as f32 / sides as f32 + turn);
            let radius = radius * radius_at(ring, side);
            positions.push([
                radius * angle.cos(),
                height,
                -radius * angle.sin(),
            ]);
        }
    }
    positions.push([0.0, profile[last as usize].1, 0.0]);
    let top = positions.len() as u32 - 1;
    let at = |ring: u32, side: u32| 1 + (ring - 1) * sides + side % sides;
    let mut indices = Vec::new();
    for side in 0..sides {
        indices.extend([0, at(1, side + 1), at(1, side)]);
        for ring in 1..last - 1 {
            let (a, b) = (at(ring, side), at(ring, side + 1));
            let (c, d) = (at(ring + 1, side), at(ring + 1, side + 1));
            indices.extend([a, b, d, a, d, c]);
        }
        indices.extend([at(last - 1, side), at(last - 1, side + 1), top]);
    }
    (positions, indices)
}

/// Seeded splitmix64 value in 0..1; never the global RNG.
fn hash(seed: u64, a: u32, b: u32) -> f32 {
    let mut z = seed
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u64::from(a) << 32 | u64::from(b));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 24) as f32
}

fn parts(preset: &str, seed: u64) -> Option<Vec<Part>> {
    let color = crate::texgen::palette(seed);
    let part = |color, (positions, indices)| Part {
        color,
        positions,
        indices,
    };
    let round = |_, _| 1.0;
    let wood = [0.42, 0.29, 0.18];
    Some(match preset {
        // 1 m box on the ground.
        "crate" => {
            let r = std::f32::consts::FRAC_1_SQRT_2;
            let profile = [(0.0, 0.0), (r, 0.0), (r, 1.0), (0.0, 1.0)];
            vec![part(color, lathe(&profile, 4, 0.125, round))]
        }
        // Bulging twelve-sided barrel, 1.1 m tall.
        "barrel" => {
            let profile = [
                (0.0, 0.0),
                (0.36, 0.0),
                (0.42, 0.3),
                (0.44, 0.55),
                (0.42, 0.8),
                (0.36, 1.1),
                (0.0, 1.1),
            ];
            vec![part(color, lathe(&profile, 12, 0.0, round))]
        }
        // Lumpy boulder about 1 m across; the seed moves every lump.
        "rock" => {
            let profile = [
                (0.0, 0.0),
                (0.45, 0.05),
                (0.55, 0.3),
                (0.5, 0.55),
                (0.3, 0.75),
                (0.0, 0.8),
            ];
            let lump = |ring, side| 0.75 + 0.5 * hash(seed, ring, side);
            vec![part([0.5, 0.5, 0.52], lathe(&profile, 7, 0.0, lump))]
        }
        // Wooden trunk under two stacked cones, 3 m tall.
        "tree" => {
            let trunk = [(0.0, 0.0), (0.15, 0.0), (0.12, 1.0), (0.0, 1.0)];
            let lower = [(0.0, 0.8), (0.9, 0.8), (0.0, 2.2)];
            let upper = [(0.0, 1.7), (0.65, 1.7), (0.0, 3.0)];
            vec![
                part(wood, lathe(&trunk, 6, 0.0, round)),
                part(color, lathe(&lower, 7, 0.0, round)),
                part(color, lathe(&upper, 7, 0.5, round)),
            ]
        }
        // Six-sided pickup gem, 0.6 m tall, its base on the ground.
        "gem" => {
            let profile = [(0.0, 0.0), (0.25, 0.25), (0.15, 0.6), (0.0, 0.6)];
            vec![part(color, lathe(&profile, 6, 0.0, round))]
        }
        _ => return None,
    })
}

/// The binary glTF (`.glb`) for `preset`: one mesh with a primitive and an
/// unlit-looking rough material per part. `None` for an unknown preset.
#[must_use]
pub fn synth(preset: &str, seed: u64) -> Option<Vec<u8>> {
    Some(glb(preset, &parts(preset, seed)?))
}

fn glb(name: &str, parts: &[Part]) -> Vec<u8> {
    let mut bin = Vec::new();
    let (mut views, mut accessors, mut materials, mut primitives) =
        (vec![], vec![], vec![], vec![]);
    for (index, part) in parts.iter().enumerate() {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        let start = bin.len();
        for position in &part.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
                bin.extend(position[axis].to_le_bytes());
            }
        }
        views.push(serde_json::json!({"buffer": 0, "byteOffset": start,
            "byteLength": bin.len() - start, "target": 34962}));
        let indices_start = bin.len();
        for index in &part.indices {
            bin.extend(index.to_le_bytes());
        }
        views
            .push(serde_json::json!({"buffer": 0, "byteOffset": indices_start,
            "byteLength": bin.len() - indices_start, "target": 34963}));
        accessors.push(serde_json::json!({"bufferView": 2 * index,
            "componentType": 5126, "count": part.positions.len(),
            "type": "VEC3", "min": min, "max": max}));
        accessors.push(serde_json::json!({"bufferView": 2 * index + 1,
            "componentType": 5125, "count": part.indices.len(),
            "type": "SCALAR"}));
        // glTF colour factors are linear; the palette is sRGB.
        let linear = part.color.map(|channel| channel.powf(2.2));
        materials.push(serde_json::json!({"pbrMetallicRoughness": {
            "baseColorFactor": [linear[0], linear[1], linear[2], 1.0],
            "metallicFactor": 0.0, "roughnessFactor": 0.85}}));
        primitives.push(serde_json::json!({
            "attributes": {"POSITION": 2 * index},
            "indices": 2 * index + 1, "material": index}));
    }
    let json = serde_json::json!({
        "asset": {"version": "2.0", "generator": "rusting meshgen"},
        "scene": 0, "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0, "name": name}],
        "meshes": [{"name": name, "primitives": primitives}],
        "materials": materials, "accessors": accessors,
        "bufferViews": views, "buffers": [{"byteLength": bin.len()}],
    });
    glb_container(&json, bin)
}

/// Wraps a glTF JSON document and its binary buffer in a GLB file.
pub(crate) fn glb_container(json: &serde_json::Value, bin: Vec<u8>) -> Vec<u8> {
    let mut json = serde_json::to_vec(json).unwrap_or_default();
    // Chunks are 4-byte aligned: JSON pads with spaces, BIN with zeros.
    json.resize(json.len().next_multiple_of(4), b' ');
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend(b"glTF");
    glb.extend(2u32.to_le_bytes());
    glb.extend((total as u32).to_le_bytes());
    glb.extend((json.len() as u32).to_le_bytes());
    glb.extend(b"JSON");
    glb.extend(json);
    glb.extend((bin.len() as u32).to_le_bytes());
    glb.extend(b"BIN\0");
    glb.extend(bin);
    glb
}

type Vector = nalgebra::Vector3<f32>;

/// A mesh built by [`extrude`], [`loft`] or the solid operations:
/// indexed triangles, counter-clockwise seen from outside, metres.
///
/// Save it with [`ProcMesh::glb`] and point a scene's `mesh_renderer` at
/// the file, or insert [`ProcMesh::asset`] into the `AssetServer` at run
/// time. Either way a `TriangleMesh` collider on the same entity collides
/// with its exact surface (static level geometry), and a `ConvexMesh`
/// collider with its convex hull (a body that moves).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcMesh {
    pub positions: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl ProcMesh {
    /// A binary glTF with one primitive in `color` (sRGB), named `name`.
    #[must_use]
    pub fn glb(&self, name: &str, color: [f32; 3]) -> Vec<u8> {
        glb(
            name,
            &[Part {
                color,
                positions: self.positions.clone(),
                indices: self.indices.clone(),
            }],
        )
    }

    /// The mesh as an engine asset, flat shaded: each triangle gets its
    /// own three vertices with the face normal.
    #[must_use]
    pub fn asset(&self) -> crate::assets::MeshAsset {
        let mut vertices = Vec::with_capacity(self.indices.len());
        for corner in self.indices.as_chunks::<3>().0 {
            let [a, b, c] = [0, 1, 2]
                .map(|k| Vector::from(self.positions[corner[k] as usize]));
            let normal = (b - a)
                .cross(&(c - a))
                .try_normalize(0.0)
                .unwrap_or_default();
            for position in [a, b, c] {
                vertices.push(crate::assets::MeshVertex {
                    position: position.into(),
                    normal: normal.into(),
                    ..crate::assets::MeshVertex::default()
                });
            }
        }
        let indices = (0..vertices.len() as u32).collect();
        crate::assets::MeshAsset { vertices, indices }
    }

    /// Volume enclosed by the surface; negative when it is inside out.
    #[must_use]
    pub fn volume(&self) -> f32 {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|corner| {
                let [a, b, c] = [0, 1, 2]
                    .map(|k| Vector::from(self.positions[corner[k] as usize]));
                a.dot(&b.cross(&c)) / 6.0
            })
            .sum()
    }

    /// Everything inside either mesh. Both must be closed.
    #[must_use]
    pub fn union(&self, other: &Self) -> Self {
        let (mut a, mut b) =
            (Bsp::new(self.polygons()), Bsp::new(other.polygons()));
        a.clip_to(&b);
        b.clip_to(&a);
        b.invert();
        b.clip_to(&a);
        b.invert();
        a.add(b.polygons());
        Self::from_polygons(a.polygons())
    }

    /// This mesh with `other` cut out of it. Both must be closed.
    #[must_use]
    pub fn subtract(&self, other: &Self) -> Self {
        let (mut a, mut b) =
            (Bsp::new(self.polygons()), Bsp::new(other.polygons()));
        a.invert();
        a.clip_to(&b);
        b.clip_to(&a);
        b.invert();
        b.clip_to(&a);
        b.invert();
        a.add(b.polygons());
        a.invert();
        Self::from_polygons(a.polygons())
    }

    /// Only what is inside both meshes. Both must be closed.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        let (mut a, mut b) =
            (Bsp::new(self.polygons()), Bsp::new(other.polygons()));
        a.invert();
        b.clip_to(&a);
        b.invert();
        a.clip_to(&b);
        b.clip_to(&a);
        a.add(b.polygons());
        a.invert();
        Self::from_polygons(a.polygons())
    }

    fn polygons(&self) -> Vec<Polygon> {
        self.indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter_map(|corner| {
                Polygon::new(
                    corner
                        .iter()
                        .map(|&index| {
                            Vector::from(self.positions[index as usize])
                        })
                        .collect(),
                )
            })
            .collect()
    }

    /// Fans each convex polygon into triangles, sharing equal vertices.
    fn from_polygons(polygons: Vec<Polygon>) -> Self {
        let mut mesh = Self::default();
        let mut seen = std::collections::HashMap::new();
        for polygon in polygons {
            let indices: Vec<u32> = polygon
                .vertices
                .iter()
                .map(|vertex| {
                    let key = vertex.map(f32::to_bits);
                    *seen.entry((key.x, key.y, key.z)).or_insert_with(|| {
                        mesh.positions.push((*vertex).into());
                        mesh.positions.len() as u32 - 1
                    })
                })
                .collect();
            for k in 1..indices.len() - 1 {
                mesh.indices
                    .extend([indices[0], indices[k], indices[k + 1]]);
            }
        }
        mesh
    }
}

/// Points along a Catmull-Rom spline through `controls`, `steps` per
/// segment, from the first control point to the last. The ends repeat
/// their neighbour, so the curve starts and stops on them.
#[must_use]
pub fn spline(controls: &[[f32; 3]], steps: usize) -> Vec<[f32; 3]> {
    let Some(&last) = controls.last() else {
        return Vec::new();
    };
    let steps = steps.max(1);
    let at = |index: isize| {
        Vector::from(
            controls[index.clamp(0, controls.len() as isize - 1) as usize],
        )
    };
    let mut points = Vec::new();
    for segment in 0..controls.len() as isize - 1 {
        let [p0, p1, p2, p3] = [-1, 0, 1, 2].map(|k| at(segment + k));
        for step in 0..steps {
            let t = step as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let point = (p1 * 2.0
                + (p2 - p0) * t
                + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * t2
                + (p1 * 3.0 - p0 - p2 * 3.0 + p3) * t3)
                * 0.5;
            points.push(point.into());
        }
    }
    points.push(last);
    points
}

/// Sweeps the closed 2D `profile` along `path` (such as [`spline`]
/// points): at each point the profile's `x` runs sideways and `y` up,
/// turning with the path without twisting. The ends are capped, so the
/// mesh is closed; the profile must be star-shaped around its centre (any
/// convex shape is). A road, a rail or a pipe.
#[must_use]
pub fn extrude(profile: &[[f32; 2]], path: &[[f32; 3]]) -> ProcMesh {
    let points: Vec<Vector> =
        path.iter().map(|&point| Vector::from(point)).collect();
    let count = points.len();
    let mut sections = Vec::with_capacity(count);
    let mut up = Vector::y();
    for index in 0..count {
        let ahead = points[(index + 1).min(count - 1)];
        let behind = points[index.saturating_sub(1)];
        let tangent = (ahead - behind)
            .try_normalize(0.0)
            .unwrap_or_else(Vector::z);
        // Parallel transport: the last up, with its tangent part removed.
        let mut next = up - tangent * up.dot(&tangent);
        if next.norm_squared() < 1e-6 {
            next = Vector::x() - tangent * tangent.x;
        }
        up = next.normalize();
        let side = up.cross(&tangent);
        sections.push(
            profile
                .iter()
                .map(|&[x, y]| (points[index] + side * x + up * y).into())
                .collect(),
        );
    }
    loft(&sections)
}

/// Skins rings of points into a closed mesh: each ring is one cross
/// section with the same number of points, in the same turning order, and
/// the first and last are capped. A vase from circles of changing radius,
/// or a shape that morphs from a square to a star.
#[must_use]
pub fn loft(sections: &[Vec<[f32; 3]>]) -> ProcMesh {
    let ring = sections.first().map_or(0, Vec::len);
    if sections.len() < 2
        || ring < 3
        || sections.iter().any(|section| section.len() != ring)
    {
        return ProcMesh::default();
    }
    let mut mesh = ProcMesh {
        positions: sections.concat(),
        indices: Vec::new(),
    };
    let at = |section: usize, k: usize| (section * ring + k % ring) as u32;
    for section in 0..sections.len() - 1 {
        for k in 0..ring {
            let (a, b) = (at(section, k), at(section, k + 1));
            let (c, d) = (at(section + 1, k), at(section + 1, k + 1));
            mesh.indices.extend([a, b, d, a, d, c]);
        }
    }
    for (section, flip) in [(0, false), (sections.len() - 1, true)] {
        let centre = sections[section]
            .iter()
            .fold(Vector::zeros(), |sum, &point| sum + Vector::from(point))
            / ring as f32;
        mesh.positions.push(centre.into());
        let middle = mesh.positions.len() as u32 - 1;
        for k in 0..ring {
            let (a, b) = (at(section, k), at(section, k + 1));
            mesh.indices.extend(if flip {
                [middle, a, b]
            } else {
                [middle, b, a]
            });
        }
    }
    // Which way the rings turn decides the winding; make it face out.
    if mesh.volume() < 0.0 {
        for corner in mesh.indices.as_chunks_mut::<3>().0 {
            corner.swap(1, 2);
        }
    }
    mesh
}

/// Distance under which a point counts as on a plane.
const PLANE_EPSILON: f32 = 1e-5;

/// A convex polygon of a [`Bsp`], with its plane.
#[derive(Clone)]
struct Polygon {
    vertices: Vec<Vector>,
    normal: Vector,
    offset: f32,
}

impl Polygon {
    /// `None` for a triangle with no area.
    fn new(vertices: Vec<Vector>) -> Option<Self> {
        let normal = (vertices[1] - vertices[0])
            .cross(&(vertices[2] - vertices[0]))
            .try_normalize(1e-12)?;
        Some(Self {
            offset: normal.dot(&vertices[0]),
            normal,
            vertices,
        })
    }

    fn flip(&mut self) {
        self.vertices.reverse();
        self.normal = -self.normal;
        self.offset = -self.offset;
    }
}

/// Where a polygon lies against a plane.
#[derive(Default)]
struct Split {
    coplanar_front: Vec<Polygon>,
    coplanar_back: Vec<Polygon>,
    front: Vec<Polygon>,
    back: Vec<Polygon>,
}

/// Cuts `polygon` by the plane of `by` into `split`.
fn split(by: &Polygon, polygon: Polygon, split: &mut Split) {
    const FRONT: u8 = 1;
    const BACK: u8 = 2;
    let sides: Vec<u8> = polygon
        .vertices
        .iter()
        .map(|vertex| {
            let distance = by.normal.dot(vertex) - by.offset;
            if distance < -PLANE_EPSILON {
                BACK
            } else if distance > PLANE_EPSILON {
                FRONT
            } else {
                0
            }
        })
        .collect();
    match sides.iter().fold(0, |all, side| all | side) {
        0 if by.normal.dot(&polygon.normal) > 0.0 => {
            split.coplanar_front.push(polygon)
        }
        0 => split.coplanar_back.push(polygon),
        FRONT => split.front.push(polygon),
        BACK => split.back.push(polygon),
        _ => {
            let (mut front, mut back) = (Vec::new(), Vec::new());
            let count = polygon.vertices.len();
            for i in 0..count {
                let j = (i + 1) % count;
                let (a, b) = (polygon.vertices[i], polygon.vertices[j]);
                if sides[i] != BACK {
                    front.push(a);
                }
                if sides[i] != FRONT {
                    back.push(a);
                }
                if sides[i] | sides[j] == FRONT | BACK {
                    let t = (by.offset - by.normal.dot(&a))
                        / by.normal.dot(&(b - a));
                    let cut = a + (b - a) * t;
                    front.push(cut);
                    back.push(cut);
                }
            }
            for (part, into) in
                [(front, &mut split.front), (back, &mut split.back)]
            {
                if part.len() >= 3 {
                    into.push(Polygon {
                        vertices: part,
                        ..polygon.clone()
                    });
                }
            }
        }
    }
}

/// A binary space partition of a closed mesh's polygons, after the
/// classic csg.js: each node keeps the polygons on its plane, with the
/// ones in front and behind in its children.
#[derive(Default)]
struct Bsp {
    plane: Option<Polygon>,
    polygons: Vec<Polygon>,
    front: Option<Box<Bsp>>,
    back: Option<Box<Bsp>>,
}

impl Bsp {
    fn new(polygons: Vec<Polygon>) -> Self {
        let mut node = Self::default();
        node.add(polygons);
        node
    }

    /// Turns solid into empty space and back.
    fn invert(&mut self) {
        for polygon in &mut self.polygons {
            polygon.flip();
        }
        if let Some(plane) = &mut self.plane {
            plane.flip();
        }
        if let Some(front) = &mut self.front {
            front.invert();
        }
        if let Some(back) = &mut self.back {
            back.invert();
        }
        std::mem::swap(&mut self.front, &mut self.back);
    }

    /// The parts of `polygons` outside this solid.
    fn clip(&self, polygons: Vec<Polygon>) -> Vec<Polygon> {
        let Some(plane) = &self.plane else {
            return polygons;
        };
        let mut parts = Split::default();
        for polygon in polygons {
            split(plane, polygon, &mut parts);
        }
        let mut front = parts.front;
        front.append(&mut parts.coplanar_front);
        let mut back = parts.back;
        back.append(&mut parts.coplanar_back);
        let mut front = match &self.front {
            Some(node) => node.clip(front),
            None => front,
        };
        // Behind a leaf is inside the solid: dropped.
        if let Some(node) = &self.back {
            front.append(&mut node.clip(back));
        }
        front
    }

    /// Removes the parts of this tree's polygons inside `other`.
    fn clip_to(&mut self, other: &Self) {
        self.polygons = other.clip(std::mem::take(&mut self.polygons));
        if let Some(front) = &mut self.front {
            front.clip_to(other);
        }
        if let Some(back) = &mut self.back {
            back.clip_to(other);
        }
    }

    fn polygons(&self) -> Vec<Polygon> {
        let mut all = self.polygons.clone();
        for child in [&self.front, &self.back].into_iter().flatten() {
            all.append(&mut child.polygons());
        }
        all
    }

    fn add(&mut self, polygons: Vec<Polygon>) {
        if polygons.is_empty() {
            return;
        }
        let plane = self
            .plane
            .get_or_insert_with(|| polygons[0].clone())
            .clone();
        let mut parts = Split::default();
        for polygon in polygons {
            split(&plane, polygon, &mut parts);
        }
        self.polygons.append(&mut parts.coplanar_front);
        self.polygons.append(&mut parts.coplanar_back);
        for (part, child) in
            [(parts.front, &mut self.front), (parts.back, &mut self.back)]
        {
            if !part.is_empty() {
                child.get_or_insert_with(Box::default).add(part);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_closed_outward_seeded_meshes() {
        for preset in PRESETS {
            for part in parts(preset, 1).unwrap() {
                // Closed: every edge is used once in each direction.
                let mut edges = std::collections::HashMap::new();
                let mut volume = 0.0;
                for corner in part.indices.chunks(3) {
                    for k in 0..3 {
                        let edge = (corner[k], corner[(k + 1) % 3]);
                        *edges.entry(edge).or_insert(0) += 1;
                    }
                    let [a, b, c] = [0, 1, 2].map(|k| {
                        nalgebra::Vector3::from(
                            part.positions[corner[k] as usize],
                        )
                    });
                    volume += a.dot(&b.cross(&c)) / 6.0;
                }
                for (&(a, b), &count) in &edges {
                    assert_eq!(count, 1, "{preset}: edge {a}-{b} repeats");
                    assert!(edges.contains_key(&(b, a)), "{preset}: hole");
                }
                // Outward winding gives a positive volume.
                assert!(volume > 0.001, "{preset} is inside out: {volume}");
            }
            let one = synth(preset, 1).unwrap();
            assert_eq!(one, synth(preset, 1).unwrap(), "{preset} repeats");
            assert_ne!(one, synth(preset, 2).unwrap(), "{preset} seeds differ");
            assert_eq!(one.len() % 4, 0);
        }
        assert!(synth("teapot", 1).is_none());
    }

    /// Every edge used once each way, so the mesh is closed.
    fn assert_closed(mesh: &ProcMesh) {
        let mut edges = std::collections::HashMap::new();
        for corner in mesh.indices.chunks(3) {
            for k in 0..3 {
                *edges.entry((corner[k], corner[(k + 1) % 3])).or_insert(0) +=
                    1;
            }
        }
        for (&(a, b), &count) in &edges {
            assert_eq!(count, 1, "edge {a}-{b} repeats");
            assert!(edges.contains_key(&(b, a)), "hole at {a}-{b}");
        }
    }

    /// A box from `min` to `max`, as a square swept along z.
    fn cuboid(min: [f32; 3], max: [f32; 3]) -> ProcMesh {
        let [hx, hy] = [(max[0] - min[0]) / 2.0, (max[1] - min[1]) / 2.0];
        let [cx, cy] = [(max[0] + min[0]) / 2.0, (max[1] + min[1]) / 2.0];
        extrude(
            &[[-hx, -hy], [hx, -hy], [hx, hy], [-hx, hy]],
            &[[cx, cy, min[2]], [cx, cy, max[2]]],
        )
    }

    #[test]
    fn extrusion_and_lofting_make_closed_outward_meshes() {
        let close =
            |a: f32, b: f32| assert!((a - b).abs() < 1e-3, "{a} != {b}");
        let slab = cuboid([0.0; 3], [2.0, 1.0, 3.0]);
        assert_closed(&slab);
        close(slab.volume(), 6.0);

        // A pipe along a curve keeps its cross-section: volume is area
        // times length, and both point orders of the profile face out.
        let curve = spline(&[[0.0; 3], [4.0, 1.0, 0.0], [8.0, 0.0, 4.0]], 16);
        assert_eq!(curve.len(), 33);
        assert_eq!(curve[0], [0.0; 3]);
        assert_eq!(curve[16], [4.0, 1.0, 0.0]);
        let square = [[-0.1, -0.1], [0.1, -0.1], [0.1, 0.1], [-0.1, 0.1]];
        let pipe = extrude(&square, &curve);
        assert_closed(&pipe);
        let length: f32 = curve
            .windows(2)
            .map(|pair| (Vector::from(pair[1]) - Vector::from(pair[0])).norm())
            .sum();
        assert!((pipe.volume() / (0.04 * length) - 1.0).abs() < 0.02);
        let mut reversed = square;
        reversed.reverse();
        close(extrude(&reversed, &curve).volume(), pipe.volume());

        // A cone lofted from a ring to a point-sized ring.
        let ring = |radius: f32, height: f32| -> Vec<[f32; 3]> {
            (0..64)
                .map(|k| {
                    let angle = std::f32::consts::TAU * k as f32 / 64.0;
                    [radius * angle.cos(), height, radius * angle.sin()]
                })
                .collect()
        };
        let vase = loft(&[ring(1.0, 0.0), ring(1.0, 1.0), ring(0.5, 2.0)]);
        assert_closed(&vase);
        let area = |r: f32| 32.0 * r * r * (std::f32::consts::TAU / 64.0).sin();
        let frustum =
            (area(1.0) + area(0.5) + (area(1.0) * area(0.5)).sqrt()) / 3.0;
        close(vase.volume(), area(1.0) + frustum);
        assert_eq!(loft(&[ring(1.0, 0.0)]), ProcMesh::default());
        assert_eq!(slab, cuboid([0.0; 3], [2.0, 1.0, 3.0]), "repeats");
    }

    #[test]
    fn solid_operations_cut_and_join_closed_meshes() {
        let close =
            |a: f32, b: f32| assert!((a - b).abs() < 1e-3, "{a} != {b}");
        let a = cuboid([0.0; 3], [2.0; 3]);
        let b = cuboid([1.0; 3], [3.0; 3]);
        close(a.union(&b).volume(), 15.0);
        close(a.subtract(&b).volume(), 7.0);
        close(a.intersect(&b).volume(), 1.0);
        // A hole through a slab, and a box apart from it.
        let slab = cuboid([0.0; 3], [4.0, 1.0, 4.0]);
        let hole = cuboid([1.0, -1.0, 1.0], [2.0, 2.0, 2.0]);
        close(slab.subtract(&hole).volume(), 15.0);
        let apart = cuboid([10.0; 3], [11.0; 3]);
        close(slab.union(&apart).volume(), 17.0);
        close(slab.intersect(&apart).volume(), 0.0);
    }

    #[test]
    fn generated_meshes_collide_through_mesh_colliders() {
        use crate::runtime::{
            App, Collider, ColliderShape, MeshRenderer, PhysicsBody, RigidBody,
            RigidBodyKind,
        };
        let mut app = App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        // A 10 m floor, top at y = 0, with a 2 m pit cut out of the middle.
        let floor = cuboid([-5.0, -1.0, -5.0], [5.0, 0.0, 5.0])
            .subtract(&cuboid([-1.0, -0.5, -1.0], [1.0, 1.0, 1.0]));
        let (mesh, material) = {
            let mut assets =
                app.world_mut().resource_mut::<crate::assets::AssetServer>();
            (
                assets.meshes.insert(floor.asset()),
                assets.fallback_material,
            )
        };
        let world = app.world_mut();
        world.spawn((
            crate::Transform::new([0.0; 3]),
            PhysicsBody::default(),
            RigidBody {
                kind: RigidBodyKind::Fixed,
                ..RigidBody::default()
            },
            Collider {
                shape: ColliderShape::TriangleMesh,
                ..Collider::default()
            },
            MeshRenderer {
                mesh,
                material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ));
        let ball = |world: &mut bevy_ecs::world::World, x: f32| {
            world
                .spawn((
                    crate::Transform::new([x, 2.0, 0.0]),
                    PhysicsBody::default(),
                    RigidBody::default(),
                    Collider {
                        shape: ColliderShape::Sphere { radius: 0.25 },
                        ..Collider::default()
                    },
                ))
                .id()
        };
        let (on_floor, in_pit) = (ball(world, 3.0), ball(world, 0.0));
        for _ in 0..240 {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        }
        let height = |entity| {
            app.world()
                .get::<crate::Transform>(entity)
                .unwrap()
                .position[1]
        };
        assert!(
            (height(on_floor) - 0.25).abs() < 0.05,
            "{}",
            height(on_floor)
        );
        assert!((height(in_pit) + 0.25).abs() < 0.05, "{}", height(in_pit));
    }

    #[cfg(feature = "gltf")]
    #[test]
    fn generated_meshes_load_as_gltf() {
        let ramp = cuboid([0.0; 3], [1.0; 3]).glb("ramp", [0.5; 3]);
        let presets = PRESETS.map(|preset| synth(preset, 3).unwrap());
        for glb in presets.iter().chain([&ramp]) {
            let (document, buffers, _) = gltf::import_slice(glb).unwrap();
            let mesh = document.meshes().next().unwrap();
            for primitive in mesh.primitives() {
                let reader =
                    primitive.reader(|buffer| Some(&buffers[buffer.index()]));
                let count = reader.read_positions().unwrap().count();
                assert!(reader
                    .read_indices()
                    .unwrap()
                    .into_u32()
                    .all(|index| (index as usize) < count));
            }
        }
    }
}
