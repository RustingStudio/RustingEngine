//! `rusting asset bake-foliage`: turns a scanned or modelled tree crown,
//! where every leaf or needle is its own triangles, into a few alpha-tested
//! cards a game can draw, plus an optional crossed-billboard impostor for
//! the farthest level of detail.
//!
//! The foliage is every triangle whose material is not opaque (`MASK` or
//! `BLEND`). Its triangles are grouped into `cards` clusters by k-means on
//! their centres. Each cluster's texture samples are splatted onto two
//! crossed quads: one spanning the cluster's two widest axes and one
//! crossing it along the widest. The colours land in one RGBA atlas whose
//! alpha is the covered texels. Everything is deterministic: the same model
//! and options write the same bytes.

use std::path::{Path, PathBuf};

use nalgebra::{Matrix3, Matrix4, Point3, SymmetricEigen, Vector3};
use rayon::prelude::*;

use crate::assets::{AlphaMode, AssetServer};

/// Settings for [`bake_foliage`].
#[derive(Clone, Debug, PartialEq)]
pub struct FoliageBakeOptions {
    /// Number of leaf clusters; each becomes two crossed quads (4 triangles).
    pub cards: usize,
    /// Also write `<out>-impostor.glb`: two crossed quads showing the whole
    /// tree, bark included, for the last LOD.
    pub impostor: bool,
    /// sRGB colour the card atlas's mean colour is scaled to. `None` keeps
    /// the source textures' colours.
    pub tint: Option<[u8; 3]>,
    /// Width and height of the card atlas in pixels.
    pub atlas: u32,
    /// Bake only this node and its children, at the node's own origin.
    /// Files that hold several variants side by side need it. `None`
    /// bakes the whole scene as placed.
    pub node: Option<String>,
}

impl Default for FoliageBakeOptions {
    fn default() -> Self {
        Self {
            cards: 1500,
            impostor: false,
            tint: None,
            atlas: 2048,
            node: None,
        }
    }
}

/// What [`bake_foliage`] wrote.
#[derive(Clone, Debug, PartialEq)]
pub struct FoliageBake {
    /// Triangles of the source model, foliage and bark.
    pub source_triangles: usize,
    /// Of those, the triangles that were baked into cards.
    pub foliage_triangles: usize,
    /// Cards written (a cluster with too few samples is dropped).
    pub cards: usize,
    /// Triangles of the cards file: 4 per card.
    pub card_triangles: usize,
    /// Share of the card atlas covered by foliage, 0 to 1.
    pub coverage: f32,
    /// The written files: the cards, then the impostor when asked for.
    pub files: Vec<PathBuf>,
}

/// One texture sample on the source surface: world position and sRGB colour.
#[derive(Clone, Copy)]
struct Sample {
    position: Vector3<f32>,
    color: [f32; 3],
}

/// Barycentric points sampled per triangle: corners, edge midpoints, centre.
const WEIGHTS: [[f32; 3]; 7] = [
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.5, 0.5, 0.0],
    [0.0, 0.5, 0.5],
    [0.5, 0.0, 0.5],
    [1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0],
];

/// The model's triangles in model space, split into foliage and bark.
struct Surface {
    /// Centre of each foliage triangle.
    centres: Vec<Vector3<f32>>,
    /// `WEIGHTS.len()` samples per foliage triangle; a sample the texture
    /// cuts away is `None`.
    foliage: Vec<Option<Sample>>,
    /// Samples of every triangle, for the impostor.
    all: Vec<Sample>,
    triangles: usize,
}

/// Bakes `model` (glTF or GLB) into `<out>-cards.glb`, and with
/// `options.impostor` into `<out>-impostor.glb` too. `out` is a path
/// without extension, such as `assets/trees/pine`.
pub fn bake_foliage(
    model: &Path,
    out: &Path,
    options: &FoliageBakeOptions,
) -> Result<FoliageBake, String> {
    if options.cards == 0 {
        return Err("--cards must be at least 1".into());
    }
    let grid = ((2 * options.cards) as f32).sqrt().ceil() as u32;
    if options.atlas / grid < 8 {
        return Err(format!(
            "a {} px atlas is too small for {} cards; raise --atlas or lower --cards",
            options.atlas, options.cards
        ));
    }
    let surface =
        read_surface(model, options.node.as_deref(), options.impostor)?;
    if surface.centres.is_empty() {
        return Err(format!(
            "{} has no foliage: no triangle uses a MASK or BLEND material",
            model.display()
        ));
    }
    let name = out
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("foliage")
        .to_owned();
    let with_suffix = |suffix: &str| {
        let mut path = out.as_os_str().to_owned();
        path.push(suffix);
        PathBuf::from(path)
    };
    let cards = bake_cards(&surface, options);
    let cards_path = with_suffix("-cards.glb");
    write(&cards_path, &cards.glb(&format!("{name}_cards")))?;
    let mut files = vec![cards_path];
    if options.impostor {
        let impostor = bake_impostor(&surface.all, options.atlas);
        let path = with_suffix("-impostor.glb");
        write(&path, &impostor.glb(&format!("{name}_impostor")))?;
        files.push(path);
    }
    Ok(FoliageBake {
        source_triangles: surface.triangles,
        foliage_triangles: surface.centres.len(),
        cards: cards.indices.len() / 12,
        card_triangles: cards.indices.len() / 3,
        coverage: cards.coverage,
        files,
    })
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(path, bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn read_surface(
    model: &Path,
    only: Option<&str>,
    keep_bark: bool,
) -> Result<Surface, String> {
    let mut server = AssetServer::default();
    let nodes = server
        .import_gltf_scene(model)
        .map_err(|error| error.to_string())?;
    let local: Vec<Matrix4<f32>> = nodes
        .iter()
        .map(|node| Matrix4::from(node.transform.to_matrix()))
        .collect();
    let root = match only {
        Some(name) => {
            Some(nodes.iter().position(|node| node.name == name).ok_or_else(
                || {
                    let names: Vec<&str> =
                        nodes.iter().map(|node| node.name.as_str()).collect();
                    format!(
                        "{} has no node {name:?}; its nodes are {names:?}",
                        model.display()
                    )
                },
            )?)
        }
        None => None,
    };
    // The transform from a node to the model, or to the chosen node's
    // origin; `None` when the node is outside the chosen one.
    let world = |mut index: usize| {
        let mut matrix = Matrix4::identity();
        loop {
            if Some(index) == root {
                return Some(matrix);
            }
            matrix = local[index] * matrix;
            match nodes[index].parent {
                Some(parent) => index = parent,
                None => return root.is_none().then_some(matrix),
            }
        }
    };
    let mut surface = Surface {
        centres: Vec::new(),
        foliage: Vec::new(),
        all: Vec::new(),
        triangles: 0,
    };
    for (index, node) in nodes.iter().enumerate() {
        let Some(matrix) = world(index) else {
            continue;
        };
        for primitive in &node.primitives {
            let (Some(mesh), Some(material)) = (
                server.meshes.get(primitive.mesh),
                server.materials.get(primitive.material),
            ) else {
                continue;
            };
            let texture = material
                .base_color_texture
                .and_then(|texture| server.textures.get(texture));
            let cutoff = match material.alpha_mode {
                AlphaMode::Opaque => None,
                AlphaMode::Mask { cutoff } => Some(cutoff),
                AlphaMode::Blend => Some(0.5),
            };
            // Texture bytes are sRGB; the colour factor is linear.
            let [r, g, b, a] = material.base_color;
            let factor =
                [r.powf(1.0 / 2.2), g.powf(1.0 / 2.2), b.powf(1.0 / 2.2), a];
            let color_at = |uv: [f32; 2]| match texture {
                Some(texture) => {
                    let [w, h] = texture.size;
                    let x =
                        ((uv[0].rem_euclid(1.0) * w as f32) as u32).min(w - 1);
                    let y =
                        ((uv[1].rem_euclid(1.0) * h as f32) as u32).min(h - 1);
                    let at = 4 * (y * w + x) as usize;
                    let texel = &texture.rgba8[at..at + 4];
                    [0, 1, 2, 3].map(|c| texel[c] as f32 * factor[c])
                }
                None => factor.map(|c| c * 255.0),
            };
            for triangle in mesh.indices.as_chunks::<3>().0 {
                surface.triangles += 1;
                let vertices = [triangle[0], triangle[1], triangle[2]]
                    .map(|index| mesh.vertices[index as usize]);
                let positions = vertices.map(|vertex| {
                    matrix
                        .transform_point(&Point3::from(vertex.position))
                        .coords
                });
                if cutoff.is_some() {
                    surface.centres.push(
                        (positions[0] + positions[1] + positions[2]) / 3.0,
                    );
                } else if !keep_bark {
                    continue;
                }
                for weights in WEIGHTS {
                    let position = positions[0] * weights[0]
                        + positions[1] * weights[1]
                        + positions[2] * weights[2];
                    let uv = [0, 1].map(|axis| {
                        (0..3)
                            .map(|corner| {
                                vertices[corner].uv[axis] * weights[corner]
                            })
                            .sum::<f32>()
                    });
                    let [r, g, b, a] = color_at(uv);
                    let sample = (cutoff
                        .is_none_or(|cutoff| a >= cutoff * 255.0))
                    .then_some(Sample {
                        position,
                        color: [r, g, b],
                    });
                    if cutoff.is_some() {
                        surface.foliage.push(sample);
                    }
                    if keep_bark {
                        surface.all.extend(sample);
                    }
                }
            }
        }
    }
    Ok(surface)
}

/// Lloyd's k-means on `points`, `iterations` rounds, started from evenly
/// spaced points so it needs no random numbers. Returns each point's
/// cluster.
// ponytail: brute force, O(points x clusters) per round; a grid or k-d tree
// over the centres if multi-million-triangle crowns bake too slowly.
fn kmeans(points: &[Vector3<f32>], k: usize, iterations: usize) -> Vec<u32> {
    let k = k.min(points.len());
    let mut centres: Vec<Vector3<f32>> =
        (0..k).map(|i| points[i * points.len() / k]).collect();
    let mut labels = vec![0u32; points.len()];
    for _ in 0..iterations {
        labels
            .par_iter_mut()
            .zip(points)
            .for_each(|(label, point)| {
                let mut best = (f32::MAX, 0);
                for (index, centre) in centres.iter().enumerate() {
                    let distance = (point - centre).norm_squared();
                    if distance < best.0 {
                        best = (distance, index);
                    }
                }
                *label = best.1 as u32;
            });
        let mut sums = vec![(Vector3::zeros(), 0usize); k];
        for (label, point) in labels.iter().zip(points) {
            let sum = &mut sums[*label as usize];
            sum.0 += point;
            sum.1 += 1;
        }
        for (centre, (sum, count)) in centres.iter_mut().zip(sums) {
            if count > 0 {
                *centre = sum / count as f32;
            }
        }
    }
    labels
}

/// The 1st and 99th percentiles of `values`, so stray samples do not
/// stretch a card.
fn spread(mut values: Vec<f32>) -> (f32, f32) {
    values.sort_by(f32::total_cmp);
    let at = |share: f32| {
        values[((values.len() - 1) as f32 * share).round() as usize]
    };
    (at(0.01), at(0.99))
}

/// Splatted colours on a `width` x `height` pixel atlas.
struct Atlas {
    width: u32,
    height: u32,
    sum: Vec<[f32; 3]>,
    count: Vec<u32>,
}

impl Atlas {
    fn new(width: u32, height: u32) -> Self {
        let texels = (width * height) as usize;
        Self {
            width,
            height,
            sum: vec![[0.0; 3]; texels],
            count: vec![0; texels],
        }
    }

    fn splat(&mut self, x: u32, y: u32, color: [f32; 3]) {
        let at = (y * self.width + x) as usize;
        for (sum, c) in self.sum[at].iter_mut().zip(color) {
            *sum += c;
        }
        self.count[at] += 1;
    }

    /// RGBA8: the mean colour of each covered texel, alpha 255 on the
    /// covered texels grown by `grow` pixels so thin leaves survive
    /// mipmapping, and each cell's mean colour under the empty texels so
    /// mipmaps do not bleed black. Returns the pixels and the covered share.
    fn finish(
        &self,
        cells: &[[u32; 4]],
        grow: u32,
        tint: Option<[u8; 3]>,
    ) -> (Vec<u8>, f32) {
        let (width, height) = (self.width as usize, self.height as usize);
        let covered: Vec<bool> = self.count.iter().map(|&c| c > 0).collect();
        let mut color: Vec<[f32; 3]> = self
            .sum
            .iter()
            .zip(&self.count)
            .map(|(sum, &count)| sum.map(|c| c / count.max(1) as f32))
            .collect();
        let hits = covered.iter().filter(|&&c| c).count();
        if let (Some(tint), true) = (tint, hits > 0) {
            let mut mean = [0.0f32; 3];
            for (c, on) in color.iter().zip(&covered) {
                if *on {
                    for axis in 0..3 {
                        mean[axis] += c[axis] / hits as f32;
                    }
                }
            }
            let scale =
                [0, 1, 2].map(|axis| tint[axis] as f32 / mean[axis].max(1.0));
            for c in &mut color {
                for axis in 0..3 {
                    c[axis] *= scale[axis];
                }
            }
        }
        let mut alpha = covered.clone();
        for _ in 0..grow {
            let before = alpha.clone();
            for y in 0..height {
                for x in 0..width {
                    let at = y * width + x;
                    alpha[at] = before[at]
                        || (x > 0 && before[at - 1])
                        || (x + 1 < width && before[at + 1])
                        || (y > 0 && before[at - width])
                        || (y + 1 < height && before[at + width]);
                }
            }
        }
        for &[x0, y0, x1, y1] in cells {
            let texels = || {
                (y0..y1).flat_map(move |y| {
                    (x0..x1).map(move |x| (y as usize) * width + x as usize)
                })
            };
            let inside: Vec<usize> = texels().filter(|&t| covered[t]).collect();
            if inside.is_empty() {
                continue;
            }
            let mut mean = [0.0f32; 3];
            for &t in &inside {
                for axis in 0..3 {
                    mean[axis] += color[t][axis] / inside.len() as f32;
                }
            }
            for t in texels().filter(|&t| !covered[t]) {
                color[t] = mean;
            }
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for (c, on) in color.iter().zip(&alpha) {
            rgba.extend(c.map(|c| c.round().clamp(0.0, 255.0) as u8));
            rgba.push(if *on { 255 } else { 0 });
        }
        (rgba, hits as f32 / (width * height) as f32)
    }
}

/// A textured, alpha-tested, double-sided mesh ready to write as GLB.
struct Baked {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
    size: [u32; 2],
    rgba: Vec<u8>,
    coverage: f32,
}

impl Baked {
    fn empty() -> Self {
        Self {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            size: [0; 2],
            rgba: Vec::new(),
            coverage: 0.0,
        }
    }

    /// Adds one quad with `corners` clockwise from the top left, mapped to
    /// the atlas rectangle `uv` (`[u0, v0, u1, v1]`). Normals lean out from
    /// the vertical axis through `centre` and up, so the crown shades round.
    fn quad(
        &mut self,
        corners: [Vector3<f32>; 4],
        uv: [f32; 4],
        centre: Vector3<f32>,
        radius: f32,
    ) {
        let base = self.positions.len() as u32;
        let [u0, v0, u1, v1] = uv;
        for (corner, uv) in
            corners.iter().zip([[u0, v0], [u1, v0], [u1, v1], [u0, v1]])
        {
            let out = Vector3::new(
                corner.x - centre.x,
                0.5 * radius,
                corner.z - centre.z,
            );
            let normal = out.try_normalize(1e-6).unwrap_or(Vector3::y());
            self.positions.push((*corner).into());
            self.normals.push(normal.into());
            self.uvs.push(uv);
        }
        self.indices.extend([
            base,
            base + 1,
            base + 2,
            base,
            base + 2,
            base + 3,
        ]);
    }

    fn glb(&self, name: &str) -> Vec<u8> {
        let mut png = Vec::new();
        if let Some(image) = image::RgbaImage::from_raw(
            self.size[0],
            self.size[1],
            self.rgba.clone(),
        ) {
            // Encoding an in-memory buffer of the right size cannot fail.
            let _ = image.write_to(
                &mut std::io::Cursor::new(&mut png),
                image::ImageFormat::Png,
            );
        }
        let mut bin = Vec::new();
        let mut views = Vec::new();
        let mut add = |bytes: &[u8], target: Option<u32>| {
            let mut view = serde_json::json!({"buffer": 0,
                "byteOffset": bin.len(), "byteLength": bytes.len()});
            if let Some(target) = target {
                view["target"] = target.into();
            }
            bin.extend(bytes);
            bin.resize(bin.len().next_multiple_of(4), 0);
            views.push(view);
            views.len() - 1
        };
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for position in &self.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }
        let positions = add(bytemuck::cast_slice(&self.positions), Some(34962));
        let normals = add(bytemuck::cast_slice(&self.normals), Some(34962));
        let uvs = add(bytemuck::cast_slice(&self.uvs), Some(34962));
        let indices = add(bytemuck::cast_slice(&self.indices), Some(34963));
        let image = add(&png, None);
        let count = self.positions.len();
        let json = serde_json::json!({
            "asset": {"version": "2.0", "generator": "rusting asset bake-foliage"},
            "scene": 0, "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "name": name}],
            "meshes": [{"name": name, "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                "indices": 3, "material": 0}]}],
            "materials": [{"name": name, "alphaMode": "MASK",
                "alphaCutoff": 0.5, "doubleSided": true,
                "pbrMetallicRoughness": {"baseColorTexture": {"index": 0},
                    "metallicFactor": 0.0, "roughnessFactor": 0.9}}],
            "textures": [{"source": 0}],
            "images": [{"bufferView": image, "mimeType": "image/png"}],
            "accessors": [
                {"bufferView": positions, "componentType": 5126,
                    "count": count, "type": "VEC3", "min": min, "max": max},
                {"bufferView": normals, "componentType": 5126,
                    "count": count, "type": "VEC3"},
                {"bufferView": uvs, "componentType": 5126,
                    "count": count, "type": "VEC2"},
                {"bufferView": indices, "componentType": 5125,
                    "count": self.indices.len(), "type": "SCALAR"},
            ],
            "bufferViews": views, "buffers": [{"byteLength": bin.len()}],
        });
        crate::meshgen::glb_container(&json, bin)
    }
}

/// Horizontal centre and radius of `points`.
fn crown(points: impl Iterator<Item = Vector3<f32>>) -> (Vector3<f32>, f32) {
    let mut min = Vector3::repeat(f32::MAX);
    let mut max = Vector3::repeat(f32::MIN);
    for point in points {
        min = min.inf(&point);
        max = max.sup(&point);
    }
    let centre = (min + max) / 2.0;
    (centre, (max.x - min.x).max(max.z - min.z) / 2.0)
}

fn bake_cards(surface: &Surface, options: &FoliageBakeOptions) -> Baked {
    let labels = kmeans(&surface.centres, options.cards, 10);
    let clusters = labels.iter().max().map_or(0, |&l| l as usize + 1);
    let mut members = vec![Vec::new(); clusters];
    for (triangle, &label) in labels.iter().enumerate() {
        members[label as usize].push(triangle);
    }
    let grid = ((2 * clusters) as f32).sqrt().ceil() as u32;
    let cell = options.atlas / grid;
    let mut atlas = Atlas::new(options.atlas, options.atlas);
    let mut cells = Vec::new();
    let mut baked = Baked::empty();
    let (centre, radius) = crown(surface.centres.iter().copied());
    for triangles in members {
        let samples: Vec<Sample> = triangles
            .iter()
            .flat_map(|&t| {
                surface.foliage[t * WEIGHTS.len()..(t + 1) * WEIGHTS.len()]
                    .iter()
                    .flatten()
                    .copied()
            })
            .collect();
        if samples.len() < WEIGHTS.len() {
            continue;
        }
        let mean = samples.iter().map(|s| s.position).sum::<Vector3<f32>>()
            / samples.len() as f32;
        let covariance = samples.iter().fold(Matrix3::zeros(), |sum, s| {
            let d = s.position - mean;
            sum + d * d.transpose()
        });
        let eigen = SymmetricEigen::new(covariance);
        let mut axes = [0, 1, 2];
        axes.sort_by(|&a, &b| {
            eigen.eigenvalues[b].total_cmp(&eigen.eigenvalues[a])
        });
        let axis = |i: usize| eigen.eigenvectors.column(axes[i]).into_owned();
        let widest = axis(0);
        // Card A spans the two widest axes; card B crosses it along the
        // widest.
        for across in [axis(1), axis(2)] {
            let slot = cells.len() as u32;
            let (gx, gy) = (slot % grid * cell, slot / grid * cell);
            let along = |axis: &Vector3<f32>| {
                spread(
                    samples
                        .iter()
                        .map(|p| (p.position - mean).dot(axis))
                        .collect(),
                )
            };
            let (s0, s1) = along(&widest);
            let (mut t0, mut t1) = along(&across);
            // Keep the crossing card from going flat.
            t0 = t0.min(-0.15 * (s1 - s0));
            t1 = t1.max(0.15 * (s1 - s0));
            // A 2 px margin keeps the grown alpha inside the cell.
            let span = (cell - 5) as f32;
            for sample in &samples {
                let d = sample.position - mean;
                let u = (d.dot(&widest) - s0) / (s1 - s0 + 1e-6);
                let v = (t1 - d.dot(&across)) / (t1 - t0 + 1e-6);
                let x = ((u * span) as i64 + 2).clamp(2, cell as i64 - 3);
                let y = ((v * span) as i64 + 2).clamp(2, cell as i64 - 3);
                atlas.splat(gx + x as u32, gy + y as u32, sample.color);
            }
            let size = options.atlas as f32;
            baked.quad(
                [
                    mean + widest * s0 + across * t1,
                    mean + widest * s1 + across * t1,
                    mean + widest * s1 + across * t0,
                    mean + widest * s0 + across * t0,
                ],
                [
                    gx as f32 / size,
                    gy as f32 / size,
                    (gx + cell) as f32 / size,
                    (gy + cell) as f32 / size,
                ],
                centre,
                radius,
            );
            cells.push([gx, gy, gx + cell, gy + cell]);
        }
    }
    let (rgba, coverage) = atlas.finish(&cells, 2, options.tint);
    baked.size = [options.atlas; 2];
    baked.rgba = rgba;
    baked.coverage = coverage;
    baked
}

/// Two crossed quads through the model's vertical axis, one facing +Z and
/// one facing +X, each showing every sample projected along its normal.
fn bake_impostor(samples: &[Sample], atlas_size: u32) -> Baked {
    let mut min = Vector3::repeat(f32::MAX);
    let mut max = Vector3::repeat(f32::MIN);
    for sample in samples {
        min = min.inf(&sample.position);
        max = max.sup(&sample.position);
    }
    let centre = (min + max) / 2.0;
    let half = (max.x - min.x).max(max.z - min.z) / 2.0;
    // Two square cells side by side: width is the wider of the X and Z
    // extents, height the Y extent.
    let size = atlas_size / 2;
    let mut atlas = Atlas::new(atlas_size, size);
    let mut baked = Baked::empty();
    let mut cells = Vec::new();
    for (slot, across) in [Vector3::x(), Vector3::z()].into_iter().enumerate() {
        let x0 = slot as u32 * size;
        // A 1 px margin keeps the grown alpha inside the cell.
        let span = (size - 3) as f32;
        for sample in samples {
            let d = sample.position - centre;
            let u = (d.dot(&across) + half) / (2.0 * half).max(1e-6);
            let v = (max.y - sample.position.y) / (max.y - min.y).max(1e-6);
            atlas.splat(
                x0 + 1 + (u * span) as u32,
                1 + (v * span) as u32,
                sample.color,
            );
        }
        let (top, bottom) = (max.y, min.y);
        let at = |side: f32, y: f32| {
            Vector3::new(centre.x, y, centre.z) + across * side
        };
        let full = atlas_size as f32;
        baked.quad(
            [
                at(-half, top),
                at(half, top),
                at(half, bottom),
                at(-half, bottom),
            ],
            [x0 as f32 / full, 0.0, (x0 + size) as f32 / full, 1.0],
            centre,
            half,
        );
        cells.push([x0, 0, x0 + size, size]);
    }
    let (rgba, coverage) = atlas.finish(&cells, 1, None);
    baked.size = [atlas_size, size];
    baked.rgba = rgba;
    baked.coverage = coverage;
    baked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A crown of `blobs` leaf clumps, each `per_blob` small triangles,
    /// with one MASK material and a 2x2 texture whose right half is cut
    /// away, under a node that lifts it 2 m.
    fn crown_model(dir: &Path, blobs: usize, per_blob: usize) -> PathBuf {
        let mut leaves = Baked::empty();
        for blob in 0..blobs {
            let angle = blob as f32 * std::f32::consts::TAU / blobs as f32;
            let at = Vector3::new(3.0 * angle.cos(), 0.0, 3.0 * angle.sin());
            for leaf in 0..per_blob {
                let offset = Vector3::new(
                    (leaf % 5) as f32 * 0.1,
                    (leaf / 5) as f32 * 0.1,
                    (leaf % 3) as f32 * 0.05,
                );
                let base = leaves.positions.len() as u32;
                for (corner, uv) in [
                    (Vector3::zeros(), [0.0, 0.0]),
                    (Vector3::new(0.05, 0.0, 0.0), [0.5, 0.0]),
                    (Vector3::new(0.0, 0.05, 0.0), [0.0, 0.5]),
                ] {
                    leaves.positions.push((at + offset + corner).into());
                    leaves.normals.push([0.0, 0.0, 1.0]);
                    leaves.uvs.push(uv);
                }
                leaves.indices.extend([base, base + 1, base + 2]);
            }
        }
        leaves.size = [2, 2];
        leaves.rgba = vec![
            40, 200, 60, 255, 0, 0, 0, 0, //
            40, 200, 60, 255, 0, 0, 0, 0,
        ];
        let glb = leaves.glb("crown");
        // Lift the node so the bake must apply node transforms.
        let path = dir.join("tree.glb");
        let json_len =
            u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let mut json: serde_json::Value =
            serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
        json["nodes"][0]["translation"] = serde_json::json!([0.0, 2.0, 0.0]);
        let bin_start = 20 + json_len + 8;
        let bin = glb[bin_start..].to_vec();
        std::fs::write(&path, crate::meshgen::glb_container(&json, bin))
            .unwrap();
        path
    }

    #[test]
    fn bake_foliage_writes_crossed_cards_and_an_impostor() {
        let dir = std::env::temp_dir()
            .join(format!("rusting-foliage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model = crown_model(&dir, 4, 25);
        let options = FoliageBakeOptions {
            cards: 4,
            impostor: true,
            atlas: 64,
            ..FoliageBakeOptions::default()
        };
        let bake = bake_foliage(&model, &dir.join("out/pine"), &options)
            .expect("bake");
        assert_eq!(bake.source_triangles, 100);
        assert_eq!(bake.foliage_triangles, 100);
        assert_eq!(bake.cards, 4);
        assert_eq!(bake.card_triangles, 16);
        assert!(bake.coverage > 0.0 && bake.coverage < 1.0, "{bake:?}");
        assert_eq!(
            bake.files,
            [
                dir.join("out/pine-cards.glb"),
                dir.join("out/pine-impostor.glb")
            ]
        );

        let mut server = AssetServer::default();
        let cards = server.import_gltf(&bake.files[0]).expect("cards load");
        let mesh = server.meshes.get(cards[0].mesh).unwrap();
        assert_eq!(mesh.indices.len(), 48);
        // The node's 2 m lift is baked in: every card sits at the crown.
        assert!(mesh.vertices.iter().all(|v| v.position[1] > 1.5));
        let material = server.materials.get(cards[0].material).unwrap();
        assert!(matches!(material.alpha_mode, AlphaMode::Mask { .. }));
        let atlas = server
            .textures
            .get(material.base_color_texture.unwrap())
            .unwrap();
        // Only the texture's opaque, green half was sampled.
        let covered: Vec<_> = atlas
            .rgba8
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|texel| texel[3] == 255)
            .collect();
        assert!(!covered.is_empty());
        assert!(covered.iter().all(|texel| texel[1] > texel[0]));

        let impostor =
            server.import_gltf(&bake.files[1]).expect("impostor loads");
        let mesh = server.meshes.get(impostor[0].mesh).unwrap();
        assert_eq!(mesh.indices.len(), 12);

        // Same model and options, same bytes.
        let first = std::fs::read(&bake.files[0]).unwrap();
        bake_foliage(&model, &dir.join("out/pine"), &options).unwrap();
        assert_eq!(first, std::fs::read(&bake.files[0]).unwrap());

        // One named node bakes at its own origin, without its 2 m lift.
        let node = FoliageBakeOptions {
            node: Some("crown".into()),
            ..options.clone()
        };
        let bake = bake_foliage(&model, &dir.join("out/one"), &node).unwrap();
        let cards = server.import_gltf(&bake.files[0]).unwrap();
        let mesh = server.meshes.get(cards[0].mesh).unwrap();
        assert!(mesh.vertices.iter().all(|v| v.position[1] < 1.5));
        let missing = FoliageBakeOptions {
            node: Some("trunk".into()),
            ..options
        };
        let error =
            bake_foliage(&model, &dir.join("out/none"), &missing).unwrap_err();
        assert!(error.contains("no node \"trunk\""), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bake_foliage_refuses_a_model_without_foliage() {
        let dir = std::env::temp_dir()
            .join(format!("rusting-foliage-opaque-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let model = dir.join("tree.glb");
        std::fs::write(&model, crate::meshgen::synth("tree", 1).unwrap())
            .unwrap();
        let error = bake_foliage(
            &model,
            &dir.join("tree"),
            &FoliageBakeOptions::default(),
        )
        .unwrap_err();
        assert!(error.contains("no foliage"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kmeans_separates_distant_clumps() {
        let points: Vec<Vector3<f32>> = (0..60)
            .map(|i| {
                let clump = (i % 3) as f32 * 10.0;
                Vector3::new(clump + (i / 3) as f32 * 0.01, 0.0, 0.0)
            })
            .collect();
        let labels = kmeans(&points, 3, 10);
        for clump in 0..3 {
            let first = labels[clump];
            assert!((clump..60).step_by(3).all(|i| labels[i] == first));
        }
        assert_ne!(labels[0], labels[1]);
        assert_ne!(labels[1], labels[2]);
        assert_ne!(labels[0], labels[2]);
    }
}
