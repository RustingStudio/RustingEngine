//! Text standing in the world: signs, labels and numbers over objects,
//! drawn with the built-in 5 x 7 pixel font as a flat mesh, so it needs no
//! font file and no glyph atlas.

use crate::assets::{
    AssetServer, Handle, MaterialAsset, MeshAsset, MeshVertex,
};

/// `rusting.text`: lines of text in the entity's XY plane, centered on it
/// and read from +Z. `size` is the height of a capital in meters. The text
/// draws unlit in `color`, so it reads the same in light and shadow, and
/// redraws whenever the component changes.
#[derive(
    bevy_ecs::prelude::Component,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(default)]
pub struct WorldText {
    pub text: String,
    pub size: f32,
    pub color: [f32; 4],
}

impl Default for WorldText {
    fn default() -> Self {
        Self {
            text: String::new(),
            size: 0.5,
            color: [1.0; 4],
        }
    }
}

/// The mesh a [`WorldText`] owns, rewritten in place when the text changes.
#[derive(bevy_ecs::prelude::Component, Clone, Copy, Debug)]
pub struct WorldTextMesh(pub Handle<MeshAsset>);

/// Pixels from one baseline to the next.
const LINE_PITCH: i16 = 10;
/// Pixels a glyph advances.
const ADVANCE: i16 = 6;
/// Pixel height of a capital.
const CAP: i16 = 7;

/// The mesh of `text` with capitals `size` tall: one quad per run of
/// pixels, each line centered on x = 0 and the lines centered on y = 0.
pub fn text_mesh(text: &str, size: f32) -> MeshAsset {
    let pixel = size / f32::from(CAP);
    let lines: Vec<&str> = text.lines().collect();
    let height = (lines.len() as i16 - 1).max(0) * LINE_PITCH + CAP;
    let mut mesh = MeshAsset::default();
    for (row, line) in lines.iter().enumerate() {
        let baseline =
            f32::from(height) / 2.0 - f32::from(CAP + row as i16 * LINE_PITCH);
        let width = line.chars().count() as f32 * f32::from(ADVANCE) - 1.0;
        let left = -width / 2.0;
        for (column, c) in line.chars().enumerate() {
            let x = left + (column as i16 * ADVANCE) as f32;
            for [l, b, r, t] in crate::fontgen::glyph_pixels(c) {
                let corner = |px: i16, py: i16| MeshVertex {
                    position: [
                        (x + f32::from(px)) * pixel,
                        (baseline + f32::from(py)) * pixel,
                        0.0,
                    ],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0, 0.0],
                    tangent: [1.0, 0.0, 0.0, 1.0],
                    color: [1.0; 4],
                };
                let first = mesh.vertices.len() as u32;
                mesh.vertices.extend([
                    corner(l, b),
                    corner(r, b),
                    corner(r, t),
                    corner(l, t),
                ]);
                mesh.indices
                    .extend([0, 1, 2, 0, 2, 3].map(|index| first + index));
            }
        }
    }
    mesh
}

/// One unlit material per text color.
fn text_material(
    assets: &mut AssetServer,
    color: [f32; 4],
) -> Handle<MaterialAsset> {
    let name = format!("World Text {color:?}");
    if let Some((handle, _)) = assets
        .materials
        .iter()
        .find(|(_, material)| material.name == name)
    {
        return handle;
    }
    assets.materials.insert(MaterialAsset {
        name,
        model: crate::assets::MaterialModel::Unlit,
        base_color: color,
        ..MaterialAsset::default()
    })
}

/// Builds or rebuilds the mesh of every new or changed [`WorldText`].
pub(super) fn sync_world_texts(
    mut commands: bevy_ecs::prelude::Commands,
    mut assets: Option<bevy_ecs::prelude::ResMut<AssetServer>>,
    texts: bevy_ecs::prelude::Query<
        (
            bevy_ecs::prelude::Entity,
            &WorldText,
            Option<&WorldTextMesh>,
        ),
        bevy_ecs::prelude::Changed<WorldText>,
    >,
) {
    let Some(assets) = assets.as_deref_mut() else {
        return;
    };
    for (entity, text, owned) in &texts {
        let built = text_mesh(&text.text, text.size);
        let mesh = match owned.and_then(|owned| {
            assets.meshes.get_mut(owned.0).map(|mesh| (owned.0, mesh))
        }) {
            Some((handle, mesh)) => {
                *mesh = built;
                handle
            }
            None => assets.meshes.insert(built),
        };
        let material = text_material(assets, text.color);
        commands.entity(entity).insert((
            WorldTextMesh(mesh),
            super::MeshRenderer {
                mesh,
                material,
                cast_shadows: false,
                receive_shadows: false,
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_mesh_draws_each_pixel_run_centered() {
        // "I" is a 3-wide top bar, a 1-wide stem of 5 rows and a 3-wide
        // foot: 7 runs.
        let mesh = text_mesh("I", 0.7);
        assert_eq!(mesh.vertices.len(), 7 * 4);
        assert_eq!(mesh.indices.len(), 7 * 6);
        let (min, max) = mesh.vertices.iter().fold(
            ([f32::MAX; 2], [f32::MIN; 2]),
            |(min, max), v| {
                (
                    [min[0].min(v.position[0]), min[1].min(v.position[1])],
                    [max[0].max(v.position[0]), max[1].max(v.position[1])],
                )
            },
        );
        // Capitals are 0.7 m tall; the 5-wide cell's glyph spans columns
        // 1..4, centered on x = 0.
        assert!((max[1] - min[1] - 0.7).abs() < 1e-5, "{min:?} {max:?}");
        assert!((min[0] + max[0]).abs() < 1e-5, "{min:?} {max:?}");
        assert!((min[1] + max[1]).abs() < 1e-5, "{min:?} {max:?}");
        // Two lines stack one pitch apart and a space draws nothing.
        let two = text_mesh("I\nI", 0.7);
        assert_eq!(two.vertices.len(), 2 * 7 * 4);
        assert!(text_mesh(" ", 1.0).vertices.is_empty());
    }

    #[test]
    fn changing_world_text_rewrites_its_own_mesh() {
        let mut app = crate::runtime::App::new();
        app.add_plugin(crate::assets::AssetPlugin).unwrap();
        let sign = app.spawn((
            crate::Transform::default(),
            WorldText {
                text: "I".into(),
                ..WorldText::default()
            },
        ));
        let tick = |app: &mut crate::runtime::App| {
            app.update(std::time::Duration::from_secs_f64(1.0 / 60.0))
                .unwrap();
        };
        tick(&mut app);
        let renderer =
            *app.world().get::<super::super::MeshRenderer>(sign).unwrap();
        app.world_mut().get_mut::<WorldText>(sign).unwrap().text = "II".into();
        tick(&mut app);
        let world = app.world();
        let after = world.get::<super::super::MeshRenderer>(sign).unwrap();
        assert_eq!(after.mesh, renderer.mesh, "the same mesh is rewritten");
        assert_eq!(after.material, renderer.material);
        let assets = world.resource::<AssetServer>();
        assert_eq!(
            assets.meshes.get(after.mesh).unwrap().vertices.len(),
            2 * 7 * 4
        );
        let material = assets.materials.get(after.material).unwrap();
        assert_eq!(material.model, crate::assets::MaterialModel::Unlit);
        assert_eq!(material.base_color, [1.0; 4]);
    }
}
