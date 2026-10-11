//! Heightmap terrain: draws the entity's [`super::Heightfield`] as square
//! chunks, each with coarser LOD meshes for distant views, painted with up
//! to four texture layers blended by splat weights.
//!
//! Chunk meshes, LOD groups, the layer atlas and the splat material are
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
/// Texels along each side of an atlas quadrant with no layer texture.
const PLAIN_TILE: u32 = 4;
/// Largest atlas quadrant side; bigger layer textures are downsampled.
const MAX_TILE: u32 = 512;

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
    /// Height cells along each side of a chunk.
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
            chunk_cells: 32,
            lod_distance: 50.0,
            cast_shadows: true,
        }
    }
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
}

impl Default for TerrainLayer {
    fn default() -> Self {
        Self {
            texture: PathBuf::new(),
            color: [1.0; 3],
            min_height: 0.0,
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
    pub atlas: Handle<TextureAsset>,
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
    let _ = assets.textures.remove(chunks.atlas);
}

fn build(
    terrain: &Terrain,
    field: &Heightfield,
    assets: &mut AssetServer,
    built: Tick,
) -> TerrainChunks {
    let atlas = layer_atlas(terrain, assets);
    let atlas = assets.textures.insert(atlas);
    let material = assets.materials.insert(MaterialAsset {
        name: "terrain".into(),
        roughness: 0.9,
        splat: true,
        base_color_texture: Some(atlas),
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
        atlas,
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
    let size = terrain.chunk_cells.max(1) as usize;
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

/// A 2x2 atlas: layer `i` fills quadrant (`i % 2`, `i / 2`) with its
/// texture tinted by its color, or with the plain color.
fn layer_atlas(terrain: &Terrain, assets: &mut AssetServer) -> TextureAsset {
    let textures = terrain
        .layers
        .iter()
        .take(MAX_TERRAIN_LAYERS)
        .map(|layer| {
            if layer.texture.as_os_str().is_empty() {
                return None;
            }
            let handle = match assets.textures.handle_for_path(&layer.texture) {
                Some(handle) => handle,
                None => assets.load_texture(&layer.texture).ok()?,
            };
            assets
                .textures
                .get(handle)
                .filter(|texture| texture.color_space != TextureColorSpace::Hdr)
                .cloned()
        })
        .collect::<Vec<_>>();
    let tile = textures
        .iter()
        .flatten()
        .map(|texture| texture.size[0].max(texture.size[1]))
        .max()
        .unwrap_or(PLAIN_TILE)
        .min(MAX_TILE) as usize;
    let side = 2 * tile;
    let mut rgba8 = vec![255; side * side * 4];
    for layer in 0..MAX_TERRAIN_LAYERS {
        let color = terrain
            .layers
            .get(layer)
            .map_or([1.0; 3], |layer| layer.color);
        let texture = textures.get(layer).and_then(Option::as_ref);
        let [left, top] = [(layer % 2) * tile, (layer / 2) * tile];
        for y in 0..tile {
            for x in 0..tile {
                let texel = texture.map_or([255; 4], |texture| {
                    let [w, h] = texture.size.map(|side| side.max(1) as usize);
                    let at = 4 * ((y * h / tile) * w + x * w / tile);
                    let mut texel = [255; 4];
                    for (out, &byte) in texel
                        .iter_mut()
                        .zip(texture.rgba8.get(at..).unwrap_or(&[]))
                    {
                        *out = byte;
                    }
                    texel
                });
                let out = 4 * ((top + y) * side + left + x);
                for channel in 0..3 {
                    // The tint scales linear light, so it applies to the
                    // decoded value, then re-encodes as sRGB.
                    let linear = (f32::from(texel[channel]) / 255.0).powf(2.2)
                        * color[channel].max(0.0);
                    rgba8[out + channel] =
                        (linear.min(1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
                }
            }
        }
    }
    TextureAsset {
        size: [side as u32; 2],
        rgba8,
        color_space: TextureColorSpace::Srgb,
        sampler: TextureSampler::default(),
    }
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
    fn terrain_atlas_survives_a_texture_shorter_than_its_size() {
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
        let atlas = layer_atlas(&terrain, &mut assets);
        assert_eq!(atlas.size, [8, 8]);
        // The first texel exists; missing ones read as white.
        assert_eq!(atlas.rgba8[..3], [10; 3]);
        assert_eq!(atlas.rgba8[4..8], [255; 4]);
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
        let atlas = assets.textures.get(chunks.atlas).unwrap();
        // Plain layers fill 4 x 4 quadrants; layer 0 is red, the unused
        // layer 1 stays white.
        assert_eq!(atlas.size, [8, 8]);
        assert_eq!(atlas.rgba8[..4], [255, 0, 0, 255]);
        assert_eq!(atlas.rgba8[4 * 4..4 * 5], [255, 255, 255, 255]);
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
