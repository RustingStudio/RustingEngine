//! Heightmap terrain: draws the entity's [`super::Heightfield`] as square
//! chunks, each with coarser LOD meshes for distant views, painted with up
//! to four texture layers blended by splat weights.
//!
//! Chunk meshes, LOD groups, the layer texture arrays and the splat
//! material are
//! built into the [`AssetServer`] at extraction time and tracked by
//! [`TerrainChunks`], which is not saved: a loaded scene rebuilds it.

use std::path::PathBuf;

use bevy_ecs::change_detection::{DetectChanges, Tick};
use bevy_ecs::prelude::{Component, Entity, Ref, With, Without, World};
use nalgebra::Vector3;
use serde::{Deserialize, Serialize};

use super::Heightfield;
use crate::assets::{
    AssetServer, Handle, LodGroupAsset, LodLevel, LodMetric, MaterialAsset,
    MeshAsset, MeshVertex, TextureAsset, TextureColorSpace, TextureSampler,
};

/// Layers the splat material blends; later ones are ignored.
pub const MAX_TERRAIN_LAYERS: usize = 4;
/// LOD meshes per chunk: every sample, every second, every fourth.
const TERRAIN_LOD_LEVELS: u32 = 3;
/// Texels along each side of a layer array with no layer image.
const PLAIN_TILE: u32 = 4;
/// Largest layer side; bigger layer images are downsampled.
const MAX_TILE: u32 = 2048;
/// Roughness of a layer without a `roughness` map.
const PLAIN_ROUGHNESS: f32 = 0.9;
/// Most chunks an automatic `chunk_cells` makes: each chunk is one draw
/// per view and shadow cascade, so many small chunks make the frame
/// CPU-bound (WILDROOT F11).
const AUTO_MAX_CHUNKS: usize = 150;
/// Smallest automatic chunk side, the old fixed default.
const AUTO_MIN_CELLS: usize = 32;

/// Draws the [`Heightfield`] on the same entity as terrain. Each layer
/// paints the ground with its texture tinted by its color. Without
/// `splat` weights, layers cover height bands: each starts at its
/// `min_height` (list them low to high) and fades in over one metre.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Terrain {
    pub layers: Vec<TerrainLayer>,
    /// `splat[row][column]`: weight of each layer at that height sample,
    /// normalized when drawn. Missing samples fall back to height bands.
    pub splat: Vec<Vec<[f32; 4]>>,
    /// Metres the layer textures span before they repeat.
    pub tiling: f32,
    /// Height cells along each side of a chunk; 0 picks it from the
    /// heightfield size so the terrain makes at most about 150 chunks.
    pub chunk_cells: u32,
    /// Distance where a chunk switches to its half-detail mesh; the
    /// quarter-detail mesh starts at twice it. 0 keeps full detail.
    pub lod_distance: f32,
    pub cast_shadows: bool,
}

impl Default for Terrain {
    fn default() -> Self {
        Self {
            layers: Vec::new(),
            splat: Vec::new(),
            tiling: 4.0,
            chunk_cells: 0,
            lod_distance: 50.0,
            cast_shadows: true,
        }
    }
}

impl Terrain {
    /// Height cells along each side of a chunk: `chunk_cells`, or when it
    /// is 0 the smallest side from 32 up that keeps the terrain at most
    /// [`AUTO_MAX_CHUNKS`] chunks.
    pub fn chunk_size(&self, field: &Heightfield) -> usize {
        if self.chunk_cells > 0 {
            return self.chunk_cells as usize;
        }
        let [rows, columns] = field_cells(field);
        (AUTO_MIN_CELLS..)
            .find(|&size| {
                rows.div_ceil(size) * columns.div_ceil(size) <= AUTO_MAX_CHUNKS
            })
            .unwrap_or(AUTO_MIN_CELLS)
    }

    /// Chunks this terrain draws over `field`.
    pub fn chunk_count(&self, field: &Heightfield) -> usize {
        let [rows, columns] = field_cells(field);
        let size = self.chunk_size(field);
        rows.div_ceil(size) * columns.div_ceil(size)
    }
}

/// Cells along each axis of `field`: one fewer than its samples.
fn field_cells(field: &Heightfield) -> [usize; 2] {
    let columns = field.heights.iter().map(Vec::len).max().unwrap_or(0);
    [
        field.heights.len().saturating_sub(1),
        columns.saturating_sub(1),
    ]
}

/// One [`Terrain`] paint layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerrainLayer {
    /// Image file; empty paints the plain color.
    pub texture: PathBuf,
    /// Linear tint multiplied into the texture.
    pub color: [f32; 3],
    /// Height band start used when there are no splat weights.
    pub min_height: f32,
    /// Tangent-space normal map (OpenGL style, green up); empty keeps the
    /// ground's own normal.
    pub normal: PathBuf,
    /// Roughness map, read from its red channel; empty is 0.9.
    pub roughness: PathBuf,
    /// Metres this layer's images span before they repeat; 0 uses the
    /// terrain's `tiling`.
    pub tiling: f32,
}

impl Default for TerrainLayer {
    fn default() -> Self {
        Self {
            texture: PathBuf::new(),
            color: [1.0; 3],
            min_height: 0.0,
            normal: PathBuf::new(),
            roughness: PathBuf::new(),
            tiling: 0.0,
        }
    }
}

/// Render assets built for a [`Terrain`]; rebuilt when it or its
/// [`Heightfield`] changes.
#[derive(Component, Clone, Debug, PartialEq, Eq, Hash)]
pub struct TerrainChunks {
    /// Finest mesh of each chunk; each starts one of `groups`.
    pub meshes: Vec<Handle<MeshAsset>>,
    pub groups: Vec<Handle<LodGroupAsset>>,
    pub material: Handle<MaterialAsset>,
    /// Base color of each layer, one array layer each.
    pub colors: Handle<TextureAsset>,
    /// Normal (rgb) and roughness (alpha) of each layer; `None` when no
    /// layer has a normal or roughness map.
    pub normals: Option<Handle<TextureAsset>>,
    pub cast_shadows: bool,
    built: Tick,
}

/// Builds terrain assets for new or changed terrains and frees those of
/// removed ones. Runs at the start of extraction, so the editor shows
/// terrain without running game systems.
// ponytail: any change rebuilds every chunk; rebuild only touched chunks
// when sculpting large terrains in the editor gets slow. A despawned
// terrain leaks its assets; free them on despawn if scenes churn terrain.
pub(super) fn build_terrains(world: &mut World) {
    if !world.contains_resource::<AssetServer>() {
        return;
    }
    let this_run = world.increment_change_tick();
    let mut orphans = world
        .query_filtered::<Entity, (With<TerrainChunks>, Without<Terrain>)>();
    let mut bare = world
        .query_filtered::<Entity, (With<TerrainChunks>, Without<Heightfield>)>(
        );
    let gone = orphans
        .iter(world)
        .chain(bare.iter(world))
        .collect::<Vec<_>>();
    for entity in gone {
        if let Some(chunks) = world.entity_mut(entity).take::<TerrainChunks>() {
            free(&chunks, &mut world.resource_mut::<AssetServer>());
        }
    }
    let mut query = world.query::<(
        Entity,
        Ref<Terrain>,
        Ref<Heightfield>,
        Option<&TerrainChunks>,
    )>();
    let stale = query
        .iter(world)
        .filter(|(_, terrain, field, chunks)| {
            chunks.is_none_or(|chunks| {
                terrain.last_changed().is_newer_than(chunks.built, this_run)
                    || field
                        .last_changed()
                        .is_newer_than(chunks.built, this_run)
            })
        })
        .map(|(entity, terrain, field, chunks)| {
            (
                entity,
                Terrain::clone(&terrain),
                Heightfield::clone(&field),
                chunks.cloned(),
            )
        })
        .collect::<Vec<_>>();
    for (entity, terrain, field, old) in stale {
        let mut assets = world.resource_mut::<AssetServer>();
        if let Some(old) = old {
            free(&old, &mut assets);
        }
        let chunks = build(&terrain, &field, &mut assets, this_run);
        world.entity_mut(entity).insert(chunks);
    }
}

fn free(chunks: &TerrainChunks, assets: &mut AssetServer) {
    for &group in &chunks.groups {
        if let Ok(group) = assets.lod_groups.remove(group) {
            for level in group.levels {
                let _ = assets.meshes.remove(level.mesh);
            }
        }
    }
    let _ = assets.materials.remove(chunks.material);
    let _ = assets.textures.remove(chunks.colors);
    if let Some(normals) = chunks.normals {
        let _ = assets.textures.remove(normals);
    }
}

fn build(
    terrain: &Terrain,
    field: &Heightfield,
    assets: &mut AssetServer,
    built: Tick,
) -> TerrainChunks {
    let colors = layer_colors(terrain, assets);
    let colors = assets.textures.insert(colors);
    let normals = layer_normals(terrain, assets);
    let normals = normals.map(|normals| assets.textures.insert(normals));
    let tiling = terrain.tiling.max(1e-3);
    let mut splat_scale = [1.0; MAX_TERRAIN_LAYERS];
    for (scale, layer) in splat_scale.iter_mut().zip(&terrain.layers) {
        if layer.tiling > 0.0 {
            *scale = tiling / layer.tiling;
        }
    }
    let material = assets.materials.insert(MaterialAsset {
        name: "terrain".into(),
        // Layer roughness lives in the normal array's alpha.
        roughness: if normals.is_some() {
            1.0
        } else {
            PLAIN_ROUGHNESS
        },
        splat: true,
        splat_scale,
        base_color_texture: Some(colors),
        normal_texture: normals,
        ..MaterialAsset::default()
    });
    let mut meshes = Vec::new();
    let mut groups = Vec::new();
    for levels in chunk_meshes(terrain, field) {
        let count = levels.len();
        let levels = levels
            .into_iter()
            .enumerate()
            .map(|(level, mesh)| LodLevel {
                mesh: assets.meshes.insert(mesh),
                until: if level + 1 == count {
                    f32::INFINITY
                } else {
                    terrain.lod_distance * (1 << level) as f32
                },
            })
            .collect::<Vec<_>>();
        meshes.push(levels[0].mesh);
        groups.push(assets.lod_groups.insert(LodGroupAsset {
            metric: LodMetric::Distance,
            levels,
        }));
    }
    TerrainChunks {
        meshes,
        groups,
        material,
        colors,
        normals,
        cast_shadows: terrain.cast_shadows,
        built,
    }
}

/// Each chunk's meshes in the entity's space, finest first. Every mesh
/// hangs a skirt below its border to hide cracks between chunks drawn at
/// different levels.
pub(super) fn chunk_meshes(
    terrain: &Terrain,
    field: &Heightfield,
) -> Vec<Vec<MeshAsset>> {
    let rows = field.heights.len();
    let columns = field.heights.iter().map(Vec::len).max().unwrap_or(0);
    if rows < 2 || columns < 2 {
        return Vec::new();
    }
    let size = terrain.chunk_size(field);
    let levels = if terrain.lod_distance > 0.0 {
        TERRAIN_LOD_LEVELS
    } else {
        1
    };
    let mut chunks = Vec::new();
    for row in (0..rows - 1).step_by(size) {
        for column in (0..columns - 1).step_by(size) {
            let span = [row, (row + size).min(rows - 1)];
            let across = [column, (column + size).min(columns - 1)];
            chunks.push(
                (0..levels)
                    .map(|level| {
                        chunk_mesh(terrain, field, span, across, 1 << level)
                    })
                    .collect(),
            );
        }
    }
    chunks
}

/// Samples from `range[0]` to `range[1]` every `stride`, ending exactly on
/// `range[1]`.
fn samples(range: [usize; 2], stride: usize) -> Vec<usize> {
    let mut samples = (range[0]..range[1]).step_by(stride).collect::<Vec<_>>();
    samples.push(range[1]);
    samples
}

fn chunk_mesh(
    terrain: &Terrain,
    field: &Heightfield,
    span: [usize; 2],
    across: [usize; 2],
    stride: usize,
) -> MeshAsset {
    let rows = field.heights.len();
    let columns = field.heights.iter().map(Vec::len).max().unwrap_or(0);
    let height = |row: usize, column: usize| {
        field.heights[row.min(rows - 1)]
            .get(column.min(columns - 1))
            .copied()
            .unwrap_or(0.0)
    };
    let spacing = field.spacing;
    let origin = [
        -0.5 * (columns - 1) as f32 * spacing,
        -0.5 * (rows - 1) as f32 * spacing,
    ];
    let tiling = terrain.tiling.max(1e-3);
    let vertex = |row: usize, column: usize, drop: f32| {
        let dx =
            height(row, column + 1) - height(row, column.saturating_sub(1));
        let dz =
            height(row + 1, column) - height(row.saturating_sub(1), column);
        let x = origin[0] + column as f32 * spacing;
        let z = origin[1] + row as f32 * spacing;
        MeshVertex {
            position: [x, height(row, column) - drop, z],
            normal: Vector3::new(-dx, 2.0 * spacing, -dz).normalize().into(),
            uv: [x / tiling, z / tiling],
            tangent: [1.0, 0.0, 0.0, 1.0],
            color: weights(terrain, row, column, height(row, column)),
        }
    };
    let row_samples = samples(span, stride);
    let column_samples = samples(across, stride);
    let width = column_samples.len();
    let mut mesh = MeshAsset::default();
    for &row in &row_samples {
        for &column in &column_samples {
            mesh.vertices.push(vertex(row, column, 0.0));
        }
    }
    for r in 0..row_samples.len() - 1 {
        for c in 0..width - 1 {
            let at = |r: usize, c: usize| (r * width + c) as u32;
            let [a, b, c, d] =
                [at(r, c), at(r, c + 1), at(r + 1, c), at(r + 1, c + 1)];
            // Counterclockwise seen from above, like `Heightfield::mesh`.
            mesh.indices.extend([a, c, b, b, c, d]);
        }
    }
    // The border walked once around, each corner once.
    let last_row = row_samples.len() - 1;
    let mut border = (0..width).map(|c| (0, c)).collect::<Vec<_>>();
    border.extend((1..=last_row).map(|r| (r, width - 1)));
    border.extend((0..width - 1).rev().map(|c| (last_row, c)));
    border.extend((1..last_row).rev().map(|r| (r, 0)));
    border.push((0, 0));
    let drop = stride as f32 * spacing;
    for pair in border.windows(2) {
        let [(r0, c0), (r1, c1)] = [pair[0], pair[1]];
        let top = [(r0 * width + c0) as u32, (r1 * width + c1) as u32];
        let first = mesh.vertices.len() as u32;
        mesh.vertices
            .push(vertex(row_samples[r0], column_samples[c0], drop));
        mesh.vertices
            .push(vertex(row_samples[r1], column_samples[c1], drop));
        let bottom = [first, first + 1];
        // Both windings, so the skirt shows from either side.
        mesh.indices
            .extend([top[0], bottom[0], top[1], top[1], bottom[0], bottom[1]]);
        mesh.indices
            .extend([top[0], top[1], bottom[0], top[1], bottom[1], bottom[0]]);
    }
    mesh
}

/// Layer weights at one height sample, from `splat` or height bands.
pub(super) fn weights(
    terrain: &Terrain,
    row: usize,
    column: usize,
    height: f32,
) -> [f32; 4] {
    if let Some(splat) = terrain.splat.get(row).and_then(|row| row.get(column))
    {
        return *splat;
    }
    let layers = terrain.layers.len().clamp(1, MAX_TERRAIN_LAYERS);
    // How far each layer has faded in; the first always covers.
    let start = |layer: usize| match terrain.layers.get(layer) {
        _ if layer == 0 => 1.0,
        Some(found) if layer < layers => {
            (height - found.min_height + 0.5).clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    std::array::from_fn(|layer| {
        if layer < layers {
            start(layer) * (1.0 - start(layer + 1))
        } else {
            0.0
        }
    })
}

/// Loads one layer image, or `None` for an empty path, a file that fails
/// to load, or an HDR image.
fn layer_image(
    path: &std::path::Path,
    assets: &mut AssetServer,
) -> Option<TextureAsset> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let texture = match assets.textures.handle_for_path(path) {
        Some(handle) => Ok(handle),
        None => assets.load_texture(path).map_err(|error| error.to_string()),
    }
    .and_then(|handle| {
        assets
            .textures
            .get(handle)
            .ok_or_else(|| "not loaded".to_owned())
    })
    .and_then(|texture| match texture.color_space {
        TextureColorSpace::Hdr => Err("HDR images cannot paint terrain".into()),
        _ => Ok(texture.clone()),
    });
    texture
        .map_err(|error| {
            // Terrains rebuild on every edit; say it once per path.
            static WARNED: std::sync::Mutex<Vec<std::path::PathBuf>> =
                std::sync::Mutex::new(Vec::new());
            let mut warned = WARNED.lock().unwrap_or_else(|e| e.into_inner());
            if !warned.iter().any(|warned| warned == path) {
                warned.push(path.to_path_buf());
                eprintln!(
                    "terrain layer `{}`: {error}; the layer paints its plain color",
                    path.display()
                );
            }
        })
        .ok()
}

/// Side of a layer array: the largest image's, at most [`MAX_TILE`].
fn layer_side<'a>(images: impl Iterator<Item = &'a TextureAsset>) -> usize {
    images
        .map(|texture| texture.size[0].max(texture.size[1]))
        .max()
        .unwrap_or(PLAIN_TILE)
        .min(MAX_TILE) as usize
}

/// Texel (`x`, `y`) of `texture` stretched to `side` x `side`; texels past
/// a short image's data read as white.
// ponytail: nearest resampling; a box filter if downsampled 4K layers alias.
fn resample(
    texture: &TextureAsset,
    side: usize,
    x: usize,
    y: usize,
) -> [u8; 4] {
    let [w, h] = texture.size.map(|side| side.max(1) as usize);
    let at = 4 * ((y * h / side) * w + x * w / side);
    let mut texel = [255; 4];
    for (out, &byte) in
        texel.iter_mut().zip(texture.rgba8.get(at..).unwrap_or(&[]))
    {
        *out = byte;
    }
    texel
}

/// A texture array of [`MAX_TERRAIN_LAYERS`] layers of `side` x `side`,
/// back to back; `texel(layer, x, y)` fills each texel.
fn layer_array(
    side: usize,
    color_space: TextureColorSpace,
    texel: impl Fn(usize, usize, usize) -> [u8; 4],
) -> TextureAsset {
    let mut rgba8 = Vec::with_capacity(MAX_TERRAIN_LAYERS * side * side * 4);
    for layer in 0..MAX_TERRAIN_LAYERS {
        for y in 0..side {
            for x in 0..side {
                rgba8.extend(texel(layer, x, y));
            }
        }
    }
    TextureAsset {
        size: [side as u32; 2],
        rgba8,
        color_space,
        sampler: TextureSampler::default(),
    }
}

/// Base color array: each layer's image tinted by its color, or the plain
/// color.
fn layer_colors(terrain: &Terrain, assets: &mut AssetServer) -> TextureAsset {
    let images = terrain
        .layers
        .iter()
        .take(MAX_TERRAIN_LAYERS)
        .map(|layer| layer_image(&layer.texture, assets))
        .collect::<Vec<_>>();
    let side = layer_side(images.iter().flatten());
    layer_array(side, TextureColorSpace::Srgb, |layer, x, y| {
        let color = terrain
            .layers
            .get(layer)
            .map_or([1.0; 3], |layer| layer.color);
        let texel = images
            .get(layer)
            .and_then(Option::as_ref)
            .map_or([255; 4], |image| resample(image, side, x, y));
        let mut out = [255; 4];
        for channel in 0..3 {
            // The tint scales linear light, so it applies to the decoded
            // value, then re-encodes as sRGB.
            let linear = (f32::from(texel[channel]) / 255.0).powf(2.2)
                * color[channel].max(0.0);
            out[channel] =
                (linear.min(1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
        }
        out
    })
}

/// Normal and roughness array: rgb from each layer's normal map (flat
/// without one), alpha from its roughness map's red channel (0.9 without
/// one). `None` when no layer has either map, so plain terrain keeps its
/// vertex normals.
fn layer_normals(
    terrain: &Terrain,
    assets: &mut AssetServer,
) -> Option<TextureAsset> {
    let layers =
        &terrain.layers[..terrain.layers.len().min(MAX_TERRAIN_LAYERS)];
    if layers.iter().all(|layer| {
        layer.normal.as_os_str().is_empty()
            && layer.roughness.as_os_str().is_empty()
    }) {
        return None;
    }
    let normals = layers
        .iter()
        .map(|layer| layer_image(&layer.normal, assets))
        .collect::<Vec<_>>();
    let roughness = layers
        .iter()
        .map(|layer| layer_image(&layer.roughness, assets))
        .collect::<Vec<_>>();
    let side = layer_side(normals.iter().chain(&roughness).flatten());
    let plain = (PLAIN_ROUGHNESS * 255.0).round() as u8;
    Some(layer_array(
        side,
        TextureColorSpace::Linear,
        |layer, x, y| {
            let [r, g, b, _] = normals
                .get(layer)
                .and_then(Option::as_ref)
                .map_or([128, 128, 255, 0], |image| {
                    resample(image, side, x, y)
                });
            let a = roughness
                .get(layer)
                .and_then(Option::as_ref)
                .map_or(plain, |image| resample(image, side, x, y)[0]);
            [r, g, b, a]
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(rows: usize, columns: usize) -> Heightfield {
        Heightfield {
            heights: (0..rows)
                .map(|row| {
                    (0..columns).map(|column| (row + column) as f32).collect()
                })
                .collect(),
            spacing: 2.0,
            ..Heightfield::default()
        }
    }

    #[test]
    fn terrain_splits_into_chunks_with_coarser_lod_meshes() {
        let terrain = Terrain {
            chunk_cells: 4,
            ..Terrain::default()
        };
        // 9 x 5 samples: 8 x 4 cells make two chunks of 4 x 4 cells.
        let chunks = chunk_meshes(&terrain, &field(5, 9));
        assert_eq!(chunks.len(), 2);
        let grid = |mesh: &MeshAsset| mesh.vertices.len();
        let levels = &chunks[0];
        assert_eq!(levels.len(), 3);
        // 5 x 5 samples, 16 border edges with skirts; then 3 x 3 and 8;
        // then 2 x 2 and 4.
        assert_eq!(grid(&levels[0]), 25 + 2 * 16);
        assert_eq!(grid(&levels[1]), 9 + 2 * 8);
        assert_eq!(grid(&levels[2]), 4 + 2 * 4);
        assert_eq!(levels[0].indices.len(), 16 * 6 + 16 * 12);
        assert_eq!(levels[2].indices.len(), 6 + 4 * 12);
        // Centred on the object: the second chunk's far corner is the
        // grid's +X/+Z corner, and skirts hang one stride below it.
        let corner = levels[0].vertices[24];
        assert_eq!(corner.position, [0.0, 8.0, 4.0]);
        let far = &chunks[1][1].vertices[8];
        assert_eq!(far.position, [8.0, 12.0, 4.0]);
        let lowest = chunks[1][1]
            .vertices
            .iter()
            .map(|vertex| vertex.position[1])
            .fold(f32::INFINITY, f32::min);
        assert_eq!(lowest, 4.0 - 2.0 * 2.0);
        // Texture coordinates tile every `tiling` metres of position.
        assert_eq!(far.uv, [2.0, 1.0]);
        // No LOD distance keeps a single level.
        let flat = Terrain {
            lod_distance: 0.0,
            ..terrain
        };
        assert_eq!(chunk_meshes(&flat, &field(5, 9))[0].len(), 1);
    }

    #[test]
    fn automatic_chunk_size_keeps_large_terrains_near_150_chunks() {
        let auto = Terrain::default();
        // Small terrains keep the old 32-cell chunks.
        assert_eq!(auto.chunk_size(&field(65, 129)), 32);
        assert_eq!(auto.chunk_count(&field(65, 129)), 8);
        // WILDROOT's 1501 x 1501 island: 12 x 12 chunks, not 24 x 24.
        let island = field(1501, 1501);
        assert_eq!(auto.chunk_size(&island), 125);
        assert_eq!(auto.chunk_count(&island), 144);
        // An explicit size still wins.
        let small = Terrain {
            chunk_cells: 64,
            ..Terrain::default()
        };
        assert_eq!(small.chunk_count(&island), 576);
    }

    #[test]
    fn terrain_layers_blend_by_height_bands_or_splat_weights() {
        let layer = |min_height: f32| TerrainLayer {
            min_height,
            ..TerrainLayer::default()
        };
        let mut terrain = Terrain {
            layers: vec![layer(0.0), layer(10.0), layer(20.0)],
            ..Terrain::default()
        };
        assert_eq!(weights(&terrain, 0, 0, 0.0), [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(weights(&terrain, 0, 0, 10.0), [0.5, 0.5, 0.0, 0.0]);
        assert_eq!(weights(&terrain, 0, 0, 15.0), [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(weights(&terrain, 0, 0, 50.0), [0.0, 0.0, 1.0, 0.0]);
        terrain.splat = vec![vec![[0.0, 0.0, 0.0, 2.0]]];
        assert_eq!(weights(&terrain, 0, 0, 0.0), [0.0, 0.0, 0.0, 2.0]);
        // Samples the splat map leaves out use the bands.
        assert_eq!(weights(&terrain, 0, 1, 15.0), [0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn terrain_layers_survive_a_texture_shorter_than_its_size() {
        let mut assets = AssetServer::default();
        assets
            .textures
            .insert_with_path(
                "short.png",
                TextureAsset {
                    size: [4, 4],
                    rgba8: vec![10; 4],
                    color_space: TextureColorSpace::Srgb,
                    sampler: TextureSampler::default(),
                },
            )
            .unwrap();
        let terrain = Terrain {
            layers: vec![TerrainLayer {
                texture: "short.png".into(),
                ..TerrainLayer::default()
            }],
            ..Terrain::default()
        };
        let colors = layer_colors(&terrain, &mut assets);
        // Four layers of the image's full size, back to back.
        assert_eq!(colors.size, [4, 4]);
        assert_eq!(colors.rgba8.len(), 4 * 4 * 4 * 4);
        // The first texel exists; missing ones read as white.
        assert_eq!(colors.rgba8[..3], [10; 3]);
        assert_eq!(colors.rgba8[4..8], [255; 4]);
    }

    #[test]
    fn terrain_layers_carry_normal_roughness_and_tiling() {
        let mut assets = AssetServer::default();
        let image = |rgba: [u8; 4]| TextureAsset {
            size: [2, 2],
            rgba8: rgba.repeat(4),
            color_space: TextureColorSpace::Linear,
            sampler: TextureSampler::default(),
        };
        assets
            .textures
            .insert_with_path("bump.png", image([200, 100, 250, 255]))
            .unwrap();
        assets
            .textures
            .insert_with_path("rough.png", image([51, 0, 0, 255]))
            .unwrap();
        let terrain = Terrain {
            layers: vec![
                TerrainLayer {
                    normal: "bump.png".into(),
                    tiling: 8.0,
                    ..TerrainLayer::default()
                },
                TerrainLayer {
                    roughness: "rough.png".into(),
                    ..TerrainLayer::default()
                },
            ],
            ..Terrain::default()
        };
        let normals = layer_normals(&terrain, &mut assets).unwrap();
        assert_eq!(normals.size, [2, 2]);
        // Layer 0: its normal map with the plain 0.9 roughness. Layer 1: a
        // flat normal with its roughness map's red channel.
        assert_eq!(normals.rgba8[..4], [200, 100, 250, 230]);
        assert_eq!(normals.rgba8[16..20], [128, 128, 255, 51]);
        // Without maps there is no array.
        assert!(layer_normals(&Terrain::default(), &mut assets).is_none());

        let mut world = World::new();
        world.insert_resource(assets);
        let entity = world.spawn((terrain, field(5, 9))).id();
        build_terrains(&mut world);
        let chunks = world.get::<TerrainChunks>(entity).unwrap().clone();
        let assets = world.resource::<AssetServer>();
        let material = assets.materials.get(chunks.material).unwrap();
        assert_eq!(material.normal_texture, chunks.normals);
        // Roughness comes from the array alone.
        assert_eq!(material.roughness, 1.0);
        // Layer 0 spans 8 m against the terrain's 4 m: half the repeats.
        assert_eq!(material.splat_scale, [0.5, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn terrain_assets_rebuild_on_change_and_free_on_removal() {
        let mut world = World::new();
        world.insert_resource(AssetServer::default());
        let red = TerrainLayer {
            color: [1.0, 0.0, 0.0],
            ..TerrainLayer::default()
        };
        let entity = world
            .spawn((
                Terrain {
                    layers: vec![red],
                    chunk_cells: 4,
                    ..Terrain::default()
                },
                field(5, 9),
            ))
            .id();
        let baseline = world.resource::<AssetServer>().meshes.iter().count();
        build_terrains(&mut world);
        let chunks = world.get::<TerrainChunks>(entity).unwrap().clone();
        assert_eq!(chunks.meshes.len(), 2);
        let assets = world.resource::<AssetServer>();
        assert_eq!(assets.meshes.iter().count(), baseline + 6);
        let material = assets.materials.get(chunks.material).unwrap();
        assert!(material.splat);
        let colors = assets.textures.get(chunks.colors).unwrap();
        // Plain layers are 4 x 4; layer 0 is red, the unused layer 1 stays
        // white.
        assert_eq!(colors.size, [4, 4]);
        assert_eq!(colors.rgba8[..4], [255, 0, 0, 255]);
        assert_eq!(colors.rgba8[16 * 4..16 * 4 + 4], [255, 255, 255, 255]);
        // No normal or roughness maps: no normal array, the old roughness.
        assert_eq!(chunks.normals, None);
        assert_eq!(material.roughness, 0.9);
        let group = assets.lod_groups.get(chunks.groups[0]).unwrap();
        assert_eq!(group.levels[0].mesh, chunks.meshes[0]);
        assert_eq!(group.levels[1].until, 100.0);
        assert_eq!(group.levels[2].until, f32::INFINITY);

        // Nothing changed: the same assets stay.
        build_terrains(&mut world);
        assert_eq!(world.get::<TerrainChunks>(entity), Some(&chunks));

        // A taller heightfield rebuilds and frees the old assets.
        world.get_mut::<Heightfield>(entity).unwrap().heights[0][0] = 3.0;
        build_terrains(&mut world);
        let rebuilt = world.get::<TerrainChunks>(entity).unwrap().clone();
        assert_ne!(rebuilt.meshes, chunks.meshes);
        let assets = world.resource::<AssetServer>();
        assert!(assets.meshes.get(chunks.meshes[0]).is_none());
        assert_eq!(assets.meshes.iter().count(), baseline + 6);

        world.entity_mut(entity).remove::<Terrain>();
        build_terrains(&mut world);
        assert!(world.get::<TerrainChunks>(entity).is_none());
        let assets = world.resource::<AssetServer>();
        assert_eq!(assets.meshes.iter().count(), baseline);
        assert!(assets.materials.get(rebuilt.material).is_none());
    }
}
