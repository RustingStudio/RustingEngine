//! First slice of the 2D path: tile maps and a side-view platformer
//! controller. Sprites are `MeshRenderer`s with the `Quad` primitive and an
//! unlit material; a 2D camera is a `Camera` with an orthographic
//! projection looking down -Z; screen-space UI is `HudElement`.
//!
//! 2D scenes live in the XY plane: +X is right, +Y is up, and the camera
//! sits at +Z. CPU physics stays 3D; 2D colliders are boxes and capsules
//! centered on z = 0.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{
    Commands, Component, DetectChanges, Query, Ref, RemovedComponents, Res,
    ResMut, Without,
};
use serde::{Deserialize, Serialize};

use super::{
    ActionMap, CharacterMove, Collider, ColliderShape, FrameTime, MeshRenderer,
    PhysicsBody, PhysicsWorld, RigidBody, RigidBodyKind, RuntimeInput,
    PLAYER_JUMP, PLAYER_LEFT, PLAYER_RIGHT,
};
use crate::assets::{
    AlphaMode, AssetServer, Handle, MaterialAsset, MaterialModel,
    PrimitiveShape,
};
use crate::Transform;

/// How one tile character draws and collides.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TileKind {
    /// Linear RGBA; multiplies the texture.
    pub color: [f32; 4],
    /// Image path relative to the project's `assets` folder. Transparent
    /// pixels (alpha below 0.5) are cut out.
    pub texture: Option<PathBuf>,
    /// Blocks bodies with a CPU collider.
    pub solid: bool,
}

impl Default for TileKind {
    fn default() -> Self {
        Self {
            color: [1.0; 4],
            texture: None,
            solid: true,
        }
    }
}

/// A flat image in the XY plane, centered on the object and facing +Z,
/// drawn by the 2D sprite pass in canvas order: lower `layer` first, then
/// lower world z, then entity order. Consecutive sprites that share an
/// image draw as one batch.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sprite {
    /// Image path relative to the project's `assets` folder; empty draws
    /// plain `color`.
    pub texture: PathBuf,
    /// Linear RGBA multiplied with the image.
    pub color: [f32; 4],
    /// Width and height in world units, before the object's scale.
    pub size: [f32; 2],
    /// Part of the image to draw, `[u0, v0, u1, v1]` with v down: one frame
    /// of a sprite sheet.
    pub region: [f32; 4],
    pub flip_x: bool,
    pub flip_y: bool,
    /// Canvas layer; higher layers draw over lower ones whatever their z.
    pub layer: i32,
    /// The loaded `texture`, set by the engine.
    #[serde(skip)]
    pub handle: Option<Handle<crate::assets::TextureAsset>>,
    /// Tangent-space normal map (green up) for [`Light2d`]s, read as linear
    /// data; empty lights the sprite as a flat card.
    pub normal_map: PathBuf,
    /// The loaded `normal_map`, set by the engine.
    #[serde(skip)]
    pub normal_handle: Option<Handle<crate::assets::TextureAsset>>,
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            texture: PathBuf::new(),
            color: [1.0; 4],
            size: [1.0, 1.0],
            region: [0.0, 0.0, 1.0, 1.0],
            flip_x: false,
            flip_y: false,
            layer: 0,
            handle: None,
            normal_map: PathBuf::new(),
            normal_handle: None,
        }
    }
}

/// A round light for [`Sprite`]s, centered on the object. It brightens
/// sprites within `radius` world units in the XY plane, fading to nothing at
/// the edge, and adds over [`Ambient2d`].
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Light2d {
    /// Linear RGB.
    pub color: [f32; 3],
    pub intensity: f32,
    pub radius: f32,
    /// How far the light sits above the sprites, in world units: only
    /// normal-mapped sprites see it, lower lights graze their bumps.
    pub height: f32,
}

impl Default for Light2d {
    fn default() -> Self {
        Self {
            color: [1.0; 3],
            intensity: 1.0,
            radius: 5.0,
            height: 0.5,
        }
    }
}

/// The light every [`Sprite`] gets before any [`Light2d`]: dark for a
/// night scene that only lights reveal. Without one, sprites keep their full
/// color and lights brighten them. The first one in entity order is used.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ambient2d {
    /// Linear RGB multiplied with each sprite.
    pub color: [f32; 3],
}

impl Default for Ambient2d {
    fn default() -> Self {
        Self { color: [0.2; 3] }
    }
}

/// Draws the [`super::ParticleEmitter`] on the same object in the 2D pass
/// with the [`Sprite`]s, in canvas order at `layer` and its world z, instead
/// of in the 3D scene.
#[derive(
    Component, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
#[serde(default)]
pub struct Particles2d {
    /// Canvas layer; higher layers draw over lower ones whatever their z.
    pub layer: i32,
}

/// A polygon that blocks every [`Light2d`]: sprite pixels whose straight
/// line to a light crosses one of its edges get none of that light. The
/// renderer uses the first 256 edges in entity order.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LightOccluder2d {
    /// Closed outline in the object's XY plane.
    pub points: Vec<[f32; 2]>,
}

impl Default for LightOccluder2d {
    fn default() -> Self {
        Self {
            points: vec![[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]],
        }
    }
}

/// Plays frames of a sprite sheet on the [`Sprite`] of the same object by
/// writing its `region`. The sheet is a grid of `columns` x `rows` equal
/// frames numbered row by row from the top left; the animation shows
/// `count` of them starting at `first`, `fps` per second of fixed time.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpriteAnimation {
    pub columns: u32,
    pub rows: u32,
    pub first: u32,
    pub count: u32,
    pub fps: f32,
    /// Start over after the last frame; otherwise hold it.
    pub looping: bool,
    /// Seconds of fixed time played so far.
    #[serde(skip)]
    pub elapsed: f64,
}

impl Default for SpriteAnimation {
    fn default() -> Self {
        Self {
            columns: 1,
            rows: 1,
            first: 0,
            count: 1,
            fps: 10.0,
            looping: true,
            elapsed: 0.0,
        }
    }
}

impl SpriteAnimation {
    /// The sheet frame showing now.
    #[must_use]
    pub fn frame(&self) -> u32 {
        let count = self.count.max(1);
        // Whole frames only, so a frame shows for exactly 1 / fps seconds.
        // The tolerance keeps whole-nanosecond fixed steps (six 1/60 s steps
        // at 10 fps sum to 0.99999996 frames) from showing a frame a tick
        // late. ponytail: the shortfall grows about 1e-3 frames per 40
        // minutes of play at 60 Hz; count ticks instead if longer runs drift.
        let played =
            (self.elapsed * f64::from(self.fps.max(0.0)) + 1e-3).floor() as u32;
        let step = if self.looping {
            played % count
        } else {
            played.min(count - 1)
        };
        self.first + step
    }

    /// `[u0, v0, u1, v1]` of `frame` in the sheet, v down.
    #[must_use]
    pub fn region(&self, frame: u32) -> [f32; 4] {
        let (columns, rows) = (self.columns.max(1), self.rows.max(1));
        let (column, row) = (frame % columns, (frame / columns).min(rows - 1));
        let (width, height) = (1.0 / columns as f32, 1.0 / rows as f32);
        [
            column as f32 * width,
            row as f32 * height,
            (column + 1) as f32 * width,
            (row + 1) as f32 * height,
        ]
    }
}

/// Per fixed step: advances every sprite animation and shows its frame.
pub(super) fn animate_sprites(
    time: Res<FrameTime>,
    mut sprites: Query<(&mut SpriteAnimation, &mut Sprite)>,
) {
    let dt = time.fixed_delta.as_secs_f64();
    for (mut animation, mut sprite) in &mut sprites {
        animation.elapsed += dt;
        let region = animation.region(animation.frame());
        if sprite.region != region {
            sprite.region = region;
        }
    }
}

/// Farthest column or row `set_tile` may grow a map to; a stray coordinate
/// must not allocate gigabytes.
pub const MAX_TILE_CELLS: usize = 4096;

/// A grid of square tiles written as text rows, top row first. Each
/// character looks up its [`TileKind`] in `tiles`; characters without one
/// (such as space or `.`) are empty. The entity's `Transform` position is
/// the top-left corner of the grid; rotation and scale are not applied.
///
/// Tiles are spawned when the map is added or changed, as entities without
/// a `SceneId`, so they are never saved. Each run of solid tiles in a row
/// gets one fixed box collider.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TileMap {
    /// Tile edge length in metres.
    pub tile_size: f32,
    pub rows: Vec<String>,
    /// Keyed by a one-character string.
    pub tiles: BTreeMap<String, TileKind>,
}

impl Default for TileMap {
    fn default() -> Self {
        Self {
            tile_size: 1.0,
            rows: vec!["#...#".into(), "#####".into()],
            tiles: BTreeMap::from([("#".into(), TileKind::default())]),
        }
    }
}

impl TileMap {
    /// Center of the cell at `column`, `row` relative to the map origin.
    #[must_use]
    pub fn cell_center(&self, column: usize, row: usize) -> [f32; 2] {
        [
            (column as f32 + 0.5) * self.tile_size,
            -(row as f32 + 0.5) * self.tile_size,
        ]
    }

    /// The cell under a point relative to the map origin, if the point is
    /// right of and below the origin and within [`MAX_TILE_CELLS`] of it. The
    /// cell may lie past the last row or column.
    #[must_use]
    pub fn cell_at(&self, local: [f32; 2]) -> Option<(usize, usize)> {
        let column = (local[0] / self.tile_size).floor();
        let row = (-local[1] / self.tile_size).floor();
        let range = 0.0..MAX_TILE_CELLS as f32;
        (range.contains(&column) && range.contains(&row))
            .then_some((column as usize, row as usize))
    }

    /// The character in a cell; `.` past the end of the grid.
    #[must_use]
    pub fn cell(&self, column: usize, row: usize) -> char {
        self.rows
            .get(row)
            .and_then(|text| text.chars().nth(column))
            .unwrap_or('.')
    }

    /// Writes `character` into a cell, padding with `.` to reach it.
    /// Returns false when the cell already held it.
    pub fn set_cell(
        &mut self,
        column: usize,
        row: usize,
        character: char,
    ) -> bool {
        if self.cell(column, row) == character {
            return false;
        }
        if self.rows.len() <= row {
            self.rows.resize(row + 1, String::new());
        }
        let mut cells: Vec<char> = self.rows[row].chars().collect();
        if cells.len() <= column {
            cells.resize(column + 1, '.');
        }
        cells[column] = character;
        self.rows[row] = cells.into_iter().collect();
        true
    }

    /// Writes `character` into every cell of the rectangle between two
    /// corner cells. Returns true when a cell changed.
    pub fn fill_rect(
        &mut self,
        from: (usize, usize),
        to: (usize, usize),
        character: char,
    ) -> bool {
        let columns = from.0.min(to.0)..=from.0.max(to.0);
        let mut changed = false;
        // One pass per row: `set_cell` per cell rebuilds the row each time.
        for row in from.1.min(to.1)..=from.1.max(to.1) {
            if columns
                .clone()
                .all(|column| self.cell(column, row) == character)
            {
                continue;
            }
            if self.rows.len() <= row {
                self.rows.resize(row + 1, String::new());
            }
            let mut cells: Vec<char> = self.rows[row].chars().collect();
            if cells.len() <= *columns.end() {
                cells.resize(columns.end() + 1, '.');
            }
            cells[columns.clone()].fill(character);
            self.rows[row] = cells.into_iter().collect();
            changed = true;
        }
        changed
    }

    /// Writes `character` into the cells of a straight line between two
    /// cells. Returns true when a cell changed.
    pub fn fill_line(
        &mut self,
        from: (usize, usize),
        to: (usize, usize),
        character: char,
    ) -> bool {
        let mut changed = false;
        for (column, row) in Self::line_cells(from, to) {
            changed |= self.set_cell(column, row, character);
        }
        changed
    }

    /// The cells of a straight line from `from` to `to`, both included,
    /// joined edge or corner to corner (Bresenham's line).
    #[must_use]
    pub fn line_cells(
        from: (usize, usize),
        to: (usize, usize),
    ) -> Vec<(usize, usize)> {
        let (mut x, mut y) = (from.0 as i64, from.1 as i64);
        let (end_x, end_y) = (to.0 as i64, to.1 as i64);
        let (dx, dy) = ((end_x - x).abs(), -(end_y - y).abs());
        let (step_x, step_y) = ((end_x - x).signum(), (end_y - y).signum());
        let mut error = dx + dy;
        let mut cells = vec![(from.0, from.1)];
        while (x, y) != (end_x, end_y) {
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += step_x;
            }
            if doubled <= dx {
                error += dx;
                y += step_y;
            }
            cells.push((x as usize, y as usize));
        }
        cells
    }

    /// Flood fills the cells joined edge to edge with the cell at `column`,
    /// `row` that hold the same character, inside the grid's used area. A
    /// cell outside that area fills nothing. Returns true when a cell
    /// changed.
    pub fn fill(&mut self, column: usize, row: usize, character: char) -> bool {
        let target = self.cell(column, row);
        let width = self
            .rows
            .iter()
            .map(|text| text.chars().count())
            .max()
            .unwrap_or(0);
        let height = self.rows.len();
        if target == character || column >= width || row >= height {
            return false;
        }
        let mut open = vec![(column, row)];
        let mut changed = false;
        while let Some((column, row)) = open.pop() {
            if column >= width
                || row >= height
                || self.cell(column, row) != target
            {
                continue;
            }
            changed |= self.set_cell(column, row, character);
            open.extend([(column + 1, row), (column, row + 1)]);
            open.extend(column.checked_sub(1).map(|left| (left, row)));
            open.extend(row.checked_sub(1).map(|up| (column, up)));
        }
        changed
    }

    /// The kind at `column`, `row`, if the cell is not empty.
    #[must_use]
    pub fn kind_at(&self, column: usize, row: usize) -> Option<&TileKind> {
        let character = self.rows.get(row)?.chars().nth(column)?;
        self.tiles.get(character.encode_utf8(&mut [0; 4]) as &str)
    }
}

/// Marks an entity spawned for the [`TileMap`] on the given entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileOf(pub Entity);

/// Per frame: respawns the tiles of every added or changed map and removes
/// the tiles of removed maps.
pub(super) fn build_tile_maps(
    mut commands: Commands,
    assets: Option<ResMut<AssetServer>>,
    maps: Query<(Entity, Ref<TileMap>, Ref<Transform>)>,
    mut removed: RemovedComponents<TileMap>,
    tiles: Query<(Entity, &TileOf)>,
) {
    let mut stale: BTreeSet<Entity> = removed.read().collect();
    let changed: Vec<_> = maps
        .iter()
        .filter(|(_, map, transform)| {
            map.is_changed() || transform.is_changed()
        })
        .collect();
    stale.extend(changed.iter().map(|(entity, ..)| *entity));
    // Owners gone without a removal event (a scene reload in the editor).
    stale.extend(
        tiles
            .iter()
            .map(|(_, owner)| owner.0)
            .filter(|owner| !maps.contains(*owner)),
    );
    if stale.is_empty() {
        return;
    }
    for (tile, owner) in &tiles {
        if stale.contains(&owner.0) {
            commands.entity(tile).despawn();
        }
    }
    let Some(mut assets) = assets else {
        return;
    };
    let quad = assets.builtin_primitive(PrimitiveShape::Quad);
    for (entity, map, transform) in changed {
        let origin = transform.position;
        let size = map.tile_size;
        let mut materials = BTreeMap::new();
        for (row, text) in map.rows.iter().enumerate() {
            let mut run: Option<usize> = None;
            let columns = text.chars().count();
            for column in 0..=columns {
                let kind = map.kind_at(column, row);
                if let Some(kind) = kind {
                    let key = text.chars().nth(column).unwrap_or(' ');
                    let material = *materials
                        .entry(key)
                        .or_insert_with(|| tile_material(&mut assets, kind));
                    let [x, y] = map.cell_center(column, row);
                    commands.spawn((
                        Transform {
                            position: [origin[0] + x, origin[1] + y, origin[2]],
                            scale: [size, size, 1.0],
                            ..Transform::default()
                        },
                        MeshRenderer {
                            mesh: quad,
                            material,
                            cast_shadows: false,
                            receive_shadows: false,
                        },
                        TileOf(entity),
                    ));
                }
                let solid = kind.is_some_and(|kind| kind.solid);
                match (solid, run) {
                    (true, None) => run = Some(column),
                    (false, Some(start)) => {
                        run = None;
                        let width = (column - start) as f32 * size;
                        let [x, y] = map.cell_center(start, row);
                        commands.spawn((
                            Transform::new([
                                origin[0] + x - size / 2.0 + width / 2.0,
                                origin[1] + y,
                                origin[2],
                            ]),
                            PhysicsBody::default(),
                            RigidBody {
                                kind: RigidBodyKind::Fixed,
                                ..RigidBody::default()
                            },
                            Collider {
                                shape: ColliderShape::Box {
                                    half_extents: [
                                        width / 2.0,
                                        size / 2.0,
                                        size / 2.0,
                                    ],
                                },
                                ..Collider::default()
                            },
                            TileOf(entity),
                        ));
                    }
                    _ => {}
                }
            }
        }
    }
}

fn tile_material(
    assets: &mut AssetServer,
    kind: &TileKind,
) -> Handle<MaterialAsset> {
    let texture = kind.texture.as_ref().and_then(|path| {
        assets
            .textures
            .handle_for_path(path)
            .map(Ok)
            .unwrap_or_else(|| assets.load_texture(path))
            .map_err(|error| eprintln!("tile texture: {error}"))
            .ok()
    });
    let material = MaterialAsset {
        model: MaterialModel::Unlit,
        alpha_mode: if texture.is_some() {
            AlphaMode::Mask { cutoff: 0.5 }
        } else {
            AlphaMode::Opaque
        },
        base_color: kind.color,
        base_color_texture: texture,
        ..MaterialAsset::default()
    };
    let existing = assets.materials.iter().find_map(|(handle, existing)| {
        (*existing == material).then_some(handle)
    });
    existing.unwrap_or_else(|| assets.materials.insert(material))
}

/// Default body of a [`PlatformerController`] without a `Collider`: 1 m
/// tall and 0.6 m wide.
pub const DEFAULT_PLATFORMER_SHAPE: ColliderShape = ColliderShape::Capsule {
    half_height: 0.2,
    radius: 0.3,
};

/// Side-view run and jump in the XY plane. Reads `player.left`,
/// `player.right`, and `player.jump` from the [`ActionMap`]; speeds are in
/// metres per second and `gravity` in metres per second squared. Parent an
/// orthographic camera to the body to follow it.
#[derive(Component, Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlatformerController {
    pub run_speed: f32,
    pub jump_speed: f32,
    pub gravity: f32,
    /// Collision layers the body stops against.
    pub collision_mask: u32,
    /// Extra jumps allowed in the air before landing: 1 is a double jump.
    pub air_jumps: u32,
    /// Air jumps taken since the body last stood on ground.
    #[serde(skip)]
    pub air_jumps_used: u32,
    #[serde(skip)]
    pub vertical_speed: f32,
    #[serde(skip)]
    pub grounded: bool,
    /// Seconds a jump press stays pending; set to [`JUMP_GRACE`] on press.
    #[serde(skip)]
    pub jump_buffer: f32,
    /// Seconds since the body was last grounded.
    #[serde(skip)]
    pub air_time: f32,
    /// The floor body under the controller after the last step and where
    /// it was, so a moving platform carries the controller.
    #[serde(skip)]
    pub floor: Option<(Entity, [f32; 3])>,
    /// Velocity of a dash set by `GameScene::dash`, in metres per second;
    /// while `dash_left` lasts it replaces walking, jumping and gravity.
    #[serde(skip)]
    pub dash_velocity: [f32; 3],
    /// Seconds of the dash still to go.
    #[serde(skip)]
    pub dash_left: f32,
}

/// A jump pressed up to this long before landing, or this long after
/// running off a ledge, still jumps.
pub const JUMP_GRACE: f32 = 0.1;

impl Default for PlatformerController {
    fn default() -> Self {
        Self {
            run_speed: 5.0,
            jump_speed: 8.0,
            gravity: 20.0,
            collision_mask: u32::MAX,
            air_jumps: 0,
            air_jumps_used: 0,
            vertical_speed: 0.0,
            grounded: false,
            jump_buffer: 0.0,
            air_time: 0.0,
            floor: None,
            dash_velocity: [0.0; 3],
            dash_left: 0.0,
        }
    }
}

/// Per rendered frame: remembers jump presses for the next fixed step.
pub(super) fn platformer_jump(
    input: Res<RuntimeInput>,
    actions: Res<ActionMap>,
    mut players: Query<&mut PlatformerController>,
) {
    if actions.just_pressed(&input, PLAYER_JUMP) {
        for mut player in &mut players {
            player.jump_buffer = JUMP_GRACE;
        }
    }
}

/// Per fixed step: running, gravity, jumping, and collision sliding.
#[allow(clippy::type_complexity)]
pub(super) fn platformer_move(
    time: Res<FrameTime>,
    input: Res<RuntimeInput>,
    actions: Res<ActionMap>,
    physics: Res<PhysicsWorld>,
    mut players: Query<(
        Entity,
        &mut PlatformerController,
        &mut Transform,
        Option<&Collider>,
    )>,
    floors: Query<
        (
            &Transform,
            Option<&super::Parent>,
            Option<&super::GlobalTransform>,
        ),
        Without<PlatformerController>,
    >,
) {
    let floor_at = |floor: Entity| {
        let (t, parent, global) = floors.get(floor).ok()?;
        Some(super::cpu_physics::world_position(t, parent, global))
    };
    let dt = time.fixed_delta.as_secs_f32();
    let run = f32::from(u8::from(actions.held(&input, PLAYER_RIGHT)))
        - f32::from(u8::from(actions.held(&input, PLAYER_LEFT)));
    for (entity, mut player, mut transform, collider) in &mut players {
        if player.jump_buffer > 0.0 && player.air_time <= JUMP_GRACE {
            player.vertical_speed = player.jump_speed;
            player.jump_buffer = 0.0;
            // No second jump from the same grace window.
            player.air_time = f32::INFINITY;
        } else if player.jump_buffer > 0.0
            && player.air_jumps_used < player.air_jumps
        {
            player.air_jumps_used += 1;
            player.vertical_speed = player.jump_speed;
            player.jump_buffer = 0.0;
        }
        player.jump_buffer = (player.jump_buffer - dt).max(0.0);
        player.vertical_speed -= player.gravity * dt;
        let shape = collider
            .map_or(DEFAULT_PLATFORMER_SHAPE, |collider| collider.shape);
        // ponytail: translation only; a platform turning around Z does
        // not swing a 2D rider.
        let (start, _) = CharacterMove::ride(
            &physics,
            shape,
            transform.position,
            player.floor.map(|(floor, at)| {
                (floor, at, super::cpu_physics::NO_ROTATION)
            }),
            player.collision_mask,
            entity,
            |floor| Some((floor_at(floor)?, super::cpu_physics::NO_ROTATION)),
        );
        let mut motion =
            [run * player.run_speed * dt, player.vertical_speed * dt, 0.0];
        if player.dash_left > 0.0 {
            motion = [
                player.dash_velocity[0] * dt,
                player.dash_velocity[1] * dt,
                0.0,
            ];
            player.vertical_speed = 0.0;
            player.dash_left = (player.dash_left - dt).max(0.0);
        }
        let moved = physics.move_character(
            shape,
            start,
            motion,
            player.collision_mask,
            Some(entity),
        );
        player.grounded = moved.grounded;
        if moved.grounded && player.vertical_speed < 0.0 {
            player.vertical_speed = 0.0;
            player.air_time = 0.0;
            player.air_jumps_used = 0;
        } else {
            player.air_time += dt;
        }
        if moved.ceiling && player.vertical_speed > 0.0 {
            player.vertical_speed = 0.0;
        }
        player.floor = moved
            .floor
            .and_then(|floor| Some((floor, floor_at(floor)?)));
        transform.position = moved.position;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_animations_step_through_sheet_frames_on_the_fixed_tick() {
        let mut app = super::super::App::new();
        let step = app.world().resource::<FrameTime>().fixed_delta;
        // A 4 x 2 sheet; the walk is frames 2..5, wrapping onto row two.
        let walk = app.spawn((
            Sprite::default(),
            SpriteAnimation {
                columns: 4,
                rows: 2,
                first: 2,
                count: 4,
                fps: 1.0 / step.as_secs_f32(),
                ..SpriteAnimation::default()
            },
        ));
        let once = app.spawn((
            Sprite::default(),
            SpriteAnimation {
                columns: 4,
                rows: 2,
                count: 2,
                fps: 1.0 / step.as_secs_f32(),
                looping: false,
                ..SpriteAnimation::default()
            },
        ));
        // Ten frames a second on the 60 Hz step: a new frame every six
        // steps, though six whole-nanosecond steps sum just under 0.1 s.
        let slow = app.spawn((
            Sprite::default(),
            SpriteAnimation {
                count: 8,
                columns: 8,
                fps: 10.0,
                ..SpriteAnimation::default()
            },
        ));
        let mut frames = Vec::new();
        let mut slow_frames = Vec::new();
        for _ in 0..6 {
            app.update(step).unwrap();
            let animation =
                |entity| app.world().get::<SpriteAnimation>(entity).unwrap();
            frames.push((animation(walk).frame(), animation(once).frame()));
            slow_frames.push(animation(slow).frame());
        }
        let region = |entity| app.world().get::<Sprite>(entity).unwrap().region;
        assert_eq!(region(walk), [0.0, 0.5, 0.25, 1.0], "frame 4 is row two");
        assert_eq!(
            frames,
            [(3, 1), (4, 1), (5, 1), (2, 1), (3, 1), (4, 1)],
            "one frame per fixed step; the walk loops, the other holds"
        );
        assert_eq!(slow_frames, [0, 0, 0, 0, 0, 1]);
    }
}
