use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Component, With, World};

use crate::rendering::debug_overlay::RenderDebugOverlay;
use crate::runtime::{
    Camera, DirectionalLight, Fog, GlobalTransform, PointLight, Projection,
    ReflectionProbe, RenderBounds, SpotLight,
};

/// One world-space line segment.
pub type Segment = [[f32; 3]; 2];

/// Blender-style wire shapes for cameras, lights and reflection probes, which have no mesh to
/// see or click. Each entry is the object and its world-space segments; the
/// Scene View draws them and picks objects by clicking near them. `skip` is
/// the editor camera. Hidden objects have no shape, as in Blender.
pub fn object_shapes(
    world: &World,
    skip: Option<Entity>,
) -> Vec<(Entity, Vec<Segment>)> {
    // One query per kind: a filter over a component type no object has
    // used yet (never registered) would hide every other kind too.
    fn objects<T: Component>(world: &World) -> Vec<(Entity, GlobalTransform)> {
        world
            .try_query_filtered::<(Entity, &GlobalTransform), With<T>>()
            .map(|mut query| {
                query
                    .iter(world)
                    .map(|(entity, transform)| (entity, *transform))
                    .collect()
            })
            .unwrap_or_default()
    }
    let mut found = objects::<Camera>(world);
    found.extend(objects::<DirectionalLight>(world));
    found.extend(objects::<PointLight>(world));
    found.extend(objects::<SpotLight>(world));
    found.extend(objects::<ReflectionProbe>(world));
    found.sort_by_key(|(entity, _)| *entity);
    found.dedup_by_key(|(entity, _)| *entity);
    // The editor camera's right and up axes, for shapes that face the view.
    let view_axes = skip
        .and_then(|camera| world.get::<GlobalTransform>(camera))
        .map(|camera| {
            let axis = |column: usize| {
                let [x, y, z, _] = camera.matrix[column];
                let length = (x * x + y * y + z * z).sqrt().max(f32::EPSILON);
                [x / length, y / length, z / length]
            };
            (axis(0), axis(1))
        });
    found
        .into_iter()
        .filter(|(entity, _)| {
            Some(*entity) != skip
                && crate::runtime::visible_in_hierarchy(world, *entity)
        })
        .map(|(entity, transform)| {
            let mut lines: Vec<Segment> =
                local_shape(world, entity, view_axes.is_some())
                    .into_iter()
                    .map(|line| {
                        line.map(|point| {
                            transform_point(transform.matrix, point)
                        })
                    })
                    .collect();
            // The probe box is world-aligned and ignores rotation and scale,
            // like the renderer.
            if let Some(probe) = world.get::<ReflectionProbe>(entity) {
                let center = transform_point(transform.matrix, [0.0; 3]);
                let corner = |sign: f32| {
                    std::array::from_fn(|axis| {
                        center[axis] + sign * probe.extents[axis]
                    })
                };
                lines.extend(box_segments(corner(-1.0), corner(1.0)));
            }
            // A point light is a circle facing the view, as in Blender.
            if let (Some(_), Some((right, up))) =
                (world.get::<PointLight>(entity), view_axes)
            {
                let center = transform_point(transform.matrix, [0.0; 3]);
                circle_between(&mut lines, center, 0.15, right, up);
                circle_between(&mut lines, center, 0.05, right, up);
            }
            (entity, lines)
        })
        .collect()
}

/// Joints of every visible skin, Blender-armature style: a line from the
/// parent joint plus a small cross, both owned by the joint so clicking a
/// bone selects it. Joints shared by several skins appear once. The Scene
/// View draws them in front of meshes and picks them before meshes.
pub fn bone_shapes(world: &World) -> Vec<(Entity, Vec<Segment>)> {
    let Some(mut query) = world.try_query::<(Entity, &crate::runtime::Skin)>()
    else {
        return Vec::new();
    };
    let mut joints: Vec<Entity> = query
        .iter(world)
        .filter(|(entity, _)| {
            crate::runtime::visible_in_hierarchy(world, *entity)
        })
        .flat_map(|(entity, skin)| {
            skin.joints.iter().filter_map(move |path| {
                crate::runtime::find_target(world, entity, path)
            })
        })
        .collect();
    joints.sort();
    joints.dedup();
    let origin = |entity| {
        world
            .get::<GlobalTransform>(entity)
            .map(|transform| transform_point(transform.matrix, [0.0; 3]))
    };
    joints
        .iter()
        .filter_map(|&joint| {
            let at = origin(joint)?;
            let mut lines: Vec<Segment> = (0..3)
                .map(|axis| {
                    let (mut start, mut end) = (at, at);
                    start[axis] -= 0.04;
                    end[axis] += 0.04;
                    [start, end]
                })
                .collect();
            if let Some(parent) = world
                .get::<crate::runtime::Parent>(joint)
                .filter(|parent| joints.binary_search(&parent.0).is_ok())
                .and_then(|parent| origin(parent.0))
            {
                lines.push([parent, at]);
            }
            Some((joint, lines))
        })
        .collect()
}

/// A camera frustum outline of vertical field of view `fov`, in local space.
fn camera_pyramid(lines: &mut Vec<Segment>, fov: f32) {
    let depth = 0.8;
    let half_height = depth * (fov * 0.5).tan().clamp(0.1, 2.0);
    let half_width = half_height * 16.0 / 9.0;
    let corners = [
        [-half_width, -half_height, -depth],
        [half_width, -half_height, -depth],
        [half_width, half_height, -depth],
        [-half_width, half_height, -depth],
    ];
    for index in 0..4 {
        lines.push([[0.0; 3], corners[index]]);
        lines.push([corners[index], corners[(index + 1) % 4]]);
    }
    // The filled triangle in Blender marks the camera's up side.
    let base = half_height * 1.1;
    let tip = [0.0, base + half_height * 0.6, -depth];
    let left = [-half_width * 0.6, base, -depth];
    let right = [half_width * 0.6, base, -depth];
    lines.extend([[left, right], [right, tip], [tip, left]]);
}

/// Another person's editor camera at world `matrix`, as world segments.
pub(super) fn camera_marker(matrix: [[f32; 4]; 4]) -> Vec<Segment> {
    let mut lines = Vec::new();
    camera_pyramid(&mut lines, 60f32.to_radians());
    lines
        .into_iter()
        .map(|[a, b]| [transform_point(matrix, a), transform_point(matrix, b)])
        .collect()
}

/// Local-space shape: forward is -Z and up is +Y, like the renderer.
/// `faces_view` leaves out shapes that `object_shapes` draws facing the
/// editor camera.
fn local_shape(
    world: &World,
    entity: Entity,
    faces_view: bool,
) -> Vec<Segment> {
    let mut lines = Vec::new();
    if let Some(camera) = world.get::<Camera>(entity) {
        // ponytail: 16:9 frame and a fixed 50° pyramid for orthographic
        // cameras; use the game resolution once the project stores one.
        let fov = match camera.projection {
            Projection::Perspective {
                vertical_fov_radians,
                ..
            } => vertical_fov_radians,
            Projection::Orthographic { .. } => 50f32.to_radians(),
        };
        camera_pyramid(&mut lines, fov);
    }
    if world.get::<DirectionalLight>(entity).is_some() {
        circle(&mut lines, [0.0; 3], 0.2, [0, 1]);
        for step in 0..8 {
            let angle = step as f32 / 8.0 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            lines.push([
                [cos * 0.3, sin * 0.3, 0.0],
                [cos * 0.45, sin * 0.45, 0.0],
            ]);
        }
        lines.push([[0.0; 3], [0.0, 0.0, -1.5]]);
    }
    // Without a view to face, a point light is three circles.
    if !faces_view && world.get::<PointLight>(entity).is_some() {
        for axes in [[0, 1], [1, 2], [2, 0]] {
            circle(&mut lines, [0.0; 3], 0.15, axes);
        }
    }
    if let Some(spot) = world.get::<SpotLight>(entity) {
        let length = 1.0;
        let radius = length * spot.outer_angle.clamp(0.01, 1.5).tan();
        circle(&mut lines, [0.0; 3], 0.1, [0, 1]);
        circle(&mut lines, [0.0, 0.0, -length], radius, [0, 1]);
        for step in 0..4 {
            let angle = step as f32 / 4.0 * std::f32::consts::TAU;
            let (sin, cos) = angle.sin_cos();
            lines.push([[0.0; 3], [cos * radius, sin * radius, -length]]);
        }
    }
    lines
}

fn circle(
    lines: &mut Vec<Segment>,
    center: [f32; 3],
    radius: f32,
    [first, second]: [usize; 2],
) {
    let unit =
        |axis: usize| std::array::from_fn(|index| f32::from(index == axis));
    circle_between(lines, center, radius, unit(first), unit(second));
}

/// A circle in the plane of two unit axes.
fn circle_between(
    lines: &mut Vec<Segment>,
    center: [f32; 3],
    radius: f32,
    first: [f32; 3],
    second: [f32; 3],
) {
    const SEGMENTS: usize = 16;
    let point = |step: usize| {
        let angle = step as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        std::array::from_fn(|axis| {
            center[axis] + radius * (cos * first[axis] + sin * second[axis])
        })
    };
    for step in 0..SEGMENTS {
        lines.push([point(step), point(step + 1)]);
    }
}

/// Adds one axis with an arrow head scaled proportionally to its shaft.
pub fn add_axis(
    overlay: &mut RenderDebugOverlay,
    origin: [f32; 3],
    direction: [f32; 3],
    length: f32,
    color: [f32; 4],
) {
    let length = length.max(0.01);
    let end = [
        origin[0] + direction[0] * length,
        origin[1] + direction[1] * length,
        origin[2] + direction[2] * length,
    ];
    overlay.line_on_top(origin, end, color, 4.0);

    // Choose a perpendicular that remains valid for axes pointing vertically.
    let reference = if direction[1].abs() < 0.9 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let side = normalize(cross(direction, reference));
    let head_length = length * 0.16;
    let head_width = length * 0.08;
    for sign in [-1.0, 1.0] {
        overlay.line_on_top(
            end,
            [
                end[0] - direction[0] * head_length
                    + side[0] * head_width * sign,
                end[1] - direction[1] * head_length
                    + side[1] * head_width * sign,
                end[2] - direction[2] * head_length
                    + side[2] * head_width * sign,
            ],
            color,
            4.0,
        );
    }
}

fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

fn normalize(value: [f32; 3]) -> [f32; 3] {
    let length =
        (value[0] * value[0] + value[1] * value[1] + value[2] * value[2])
            .sqrt();
    if length > f32::EPSILON {
        [value[0] / length, value[1] / length, value[2] / length]
    } else {
        [1.0, 0.0, 0.0]
    }
}

/// Finds the farthest transformed corner of a local mesh box from the
/// object's origin. The selected axes use this to extend beyond large or
/// heavily scaled meshes.
pub fn mesh_world_radius_from_origin(
    (minimum, maximum): ([f32; 3], [f32; 3]),
    matrix: [[f32; 4]; 4],
) -> f32 {
    let origin = [matrix[3][0], matrix[3][1], matrix[3][2]];
    let mut radius: f32 = 0.0;
    for x in [minimum[0], maximum[0]] {
        for y in [minimum[1], maximum[1]] {
            for z in [minimum[2], maximum[2]] {
                let corner = transform_point(matrix, [x, y, z]);
                let offset = [
                    corner[0] - origin[0],
                    corner[1] - origin[1],
                    corner[2] - origin[2],
                ];
                radius = radius.max(
                    (offset[0] * offset[0]
                        + offset[1] * offset[1]
                        + offset[2] * offset[2])
                        .sqrt(),
                );
            }
        }
    }
    radius
}

/// Adds a box around the selected mesh.
///
/// `minimum` and `maximum` are local mesh positions. `matrix` converts every
/// corner into world space, so the box follows object position, rotation,
/// hierarchy, and scale exactly like the rendered mesh.
pub fn add_bound_box(
    overlay: &mut RenderDebugOverlay,
    matrix: [[f32; 4]; 4],
    minimum: [f32; 3],
    maximum: [f32; 3],
    color: [f32; 4],
) {
    for [start, end] in box_segments(minimum, maximum) {
        overlay.line(
            transform_point(matrix, start),
            transform_point(matrix, end),
            color,
        );
    }
}

/// The twelve edges of the box from `minimum` to `maximum`.
fn box_segments(minimum: [f32; 3], maximum: [f32; 3]) -> Vec<Segment> {
    let corners = [
        [minimum[0], minimum[1], minimum[2]],
        [maximum[0], minimum[1], minimum[2]],
        [maximum[0], maximum[1], minimum[2]],
        [minimum[0], maximum[1], minimum[2]],
        [minimum[0], minimum[1], maximum[2]],
        [maximum[0], minimum[1], maximum[2]],
        [maximum[0], maximum[1], maximum[2]],
        [minimum[0], maximum[1], maximum[2]],
    ];
    // Four bottom edges, four top edges, then four upright edges.
    [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ]
    .map(|(start, end)| [corners[start], corners[end]])
    .to_vec()
}

/// Keeps the selected object's wire outline visible through foreground
/// geometry. Only lines added since `first_line` are affected, so grid and
/// unselected helpers still respect scene depth.
pub fn make_selection_outline_visible(
    overlay: &mut RenderDebugOverlay,
    first_line: usize,
) {
    for line in &mut overlay.lines[first_line..] {
        line.on_top = true;
        line.thickness = 2.0;
    }
}

/// Adds the world-space volume that frustum culling tests for a
/// `RenderBounds` override, so the outline matches what the renderer uses:
/// a box becomes the world axis-aligned box around its transformed corners,
/// and a sphere grows by the largest axis scale.
pub fn add_render_bounds(
    overlay: &mut RenderDebugOverlay,
    bounds: RenderBounds,
    matrix: [[f32; 4]; 4],
    color: [f32; 4],
) {
    match bounds.transformed(&matrix) {
        RenderBounds::Aabb { min, max } => add_bound_box(
            overlay,
            nalgebra::Matrix4::<f32>::identity().into(),
            min,
            max,
            color,
        ),
        RenderBounds::Sphere { center, radius } => {
            const SEGMENTS: usize = 32;
            let point = |axis: usize, step: usize| {
                let angle =
                    step as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let mut point = center;
                point[(axis + 1) % 3] += radius * angle.cos();
                point[(axis + 2) % 3] += radius * angle.sin();
                point
            };
            // One great circle around each world axis.
            for axis in 0..3 {
                for step in 0..SEGMENTS {
                    overlay.line(
                        point(axis, step),
                        point(axis, step + 1),
                        color,
                    );
                }
            }
        }
    }
}

/// Squares that show where fog sits: a solid one at `height`, where the
/// fog has its full density, and a quieter one where height falloff has
/// thinned it to 1/e (about 37%). Uniform fog has only the first. Both span
/// the editor grid.
pub fn fog_height_lines(fog: &Fog) -> Vec<(Segment, bool)> {
    const HALF: f32 = 20.0;
    let square = |y: f32| {
        let corners = [
            [-HALF, y, -HALF],
            [HALF, y, -HALF],
            [HALF, y, HALF],
            [-HALF, y, HALF],
        ];
        (0..4).map(move |index| [corners[index], corners[(index + 1) % 4]])
    };
    let mut lines: Vec<_> =
        square(fog.height).map(|line| (line, true)).collect();
    if fog.height_falloff > 0.0 {
        lines.extend(
            square(fog.height + 1.0 / fog.height_falloff)
                .map(|line| (line, false)),
        );
    }
    lines
}

/// The outline of the tile map cells between two corner cells, on the
/// map's plane at `origin`.
pub fn tile_rect_lines(
    tile_size: f32,
    origin: [f32; 3],
    from: (usize, usize),
    to: (usize, usize),
) -> [Segment; 4] {
    let x = |column: usize| origin[0] + column as f32 * tile_size;
    let y = |row: usize| origin[1] - row as f32 * tile_size;
    let (left, right) = (x(from.0.min(to.0)), x(from.0.max(to.0) + 1));
    let (top, bottom) = (y(from.1.min(to.1)), y(from.1.max(to.1) + 1));
    let corners = [
        [left, top, origin[2]],
        [right, top, origin[2]],
        [right, bottom, origin[2]],
        [left, bottom, origin[2]],
    ];
    std::array::from_fn(|index| [corners[index], corners[(index + 1) % 4]])
}

/// The center of each face of a reflection probe's world-aligned box,
/// with the face's axis. The Scene View drags these to resize the box.
pub fn probe_face_centers(
    center: [f32; 3],
    extents: [f32; 3],
) -> [(usize, [f32; 3]); 6] {
    std::array::from_fn(|index| {
        let axis = index / 2;
        let sign = if index.is_multiple_of(2) { -1.0 } else { 1.0 };
        let mut face = center;
        face[axis] += sign * extents[axis];
        (axis, face)
    })
}

/// Local-space drag handles of a `RenderBounds` override. Handle `index`
/// sits on local axis `index / 2`, on the negative side when even: a box
/// face center, or a point on the sphere.
pub fn render_bounds_handles(bounds: RenderBounds) -> [[f32; 3]; 6] {
    let (center, reach) = match bounds {
        RenderBounds::Aabb { min, max } => (
            std::array::from_fn(|axis| (min[axis] + max[axis]) * 0.5),
            std::array::from_fn(|axis| (max[axis] - min[axis]) * 0.5),
        ),
        RenderBounds::Sphere { center, radius } => (center, [radius; 3]),
    };
    std::array::from_fn(|index| {
        let axis = index / 2;
        let sign = if index.is_multiple_of(2) { -1.0 } else { 1.0 };
        let mut point: [f32; 3] = center;
        point[axis] += sign * reach[axis];
        point
    })
}

/// Moves `RenderBounds` handle `index` to local coordinate `value` on its
/// axis. A box face stops at the opposite face; a sphere takes the distance
/// from its center as the radius.
pub fn move_render_bounds_handle(
    bounds: &mut RenderBounds,
    index: usize,
    value: f32,
) {
    let axis = index / 2;
    match bounds {
        RenderBounds::Aabb { min, max } if index.is_multiple_of(2) => {
            min[axis] = value.min(max[axis]);
        }
        RenderBounds::Aabb { min, max } => {
            max[axis] = value.max(min[axis]);
        }
        RenderBounds::Sphere { center, radius } => {
            *radius = (value - center[axis]).abs();
        }
    }
}

/// Multiplies a local point by the column-major transform array used by ECS.
pub(crate) fn transform_point(
    matrix: [[f32; 4]; 4],
    point: [f32; 3],
) -> [f32; 3] {
    [
        matrix[0][0] * point[0]
            + matrix[1][0] * point[1]
            + matrix[2][0] * point[2]
            + matrix[3][0],
        matrix[0][1] * point[0]
            + matrix[1][1] * point[1]
            + matrix[2][1] * point[2]
            + matrix[3][1],
        matrix[0][2] * point[0]
            + matrix[1][2] * point[1]
            + matrix[2][2] * point[2]
            + matrix[3][2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_rect_lines_outline_the_cells_between_two_corners() {
        let lines = tile_rect_lines(0.5, [1.0, 2.0, 3.0], (3, 2), (2, 0));
        assert_eq!(lines[0], [[2.0, 2.0, 3.0], [3.0, 2.0, 3.0]]);
        assert_eq!(lines[2], [[3.0, 0.5, 3.0], [2.0, 0.5, 3.0]]);
    }

    #[test]
    fn point_lights_face_the_editor_camera() {
        let mut world = World::new();
        // The editor camera looks straight down.
        let matrix = (nalgebra::Matrix4::new_translation(
            &nalgebra::Vector3::new(0.0, 10.0, 0.0),
        ) * nalgebra::Matrix4::from_euler_angles(
            -std::f32::consts::FRAC_PI_2,
            0.0,
            0.0,
        ))
        .into();
        let camera = world.spawn(GlobalTransform { matrix }).id();
        let light = world
            .spawn((
                PointLight::default(),
                GlobalTransform {
                    matrix: nalgebra::Matrix4::new_translation(
                        &nalgebra::Vector3::new(1.0, 2.0, 3.0),
                    )
                    .into(),
                },
            ))
            .id();
        let shapes = object_shapes(&world, Some(camera));
        assert_eq!(shapes[0].0, light);
        // Two circles flat on the ground plane, the view's plane.
        assert_eq!(shapes[0].1.len(), 32);
        for point in shapes[0].1.iter().flatten() {
            assert!((point[1] - 2.0).abs() < 1e-5, "{point:?}");
            let radius = (point[0] - 1.0).hypot(point[2] - 3.0);
            assert!(
                (radius - 0.15).abs() < 1e-5 || (radius - 0.05).abs() < 1e-5
            );
        }
        // With no view to face, it is three circles.
        assert_eq!(object_shapes(&world, None)[0].1.len(), 48);
    }

    #[test]
    fn reflection_probe_box_is_world_aligned_around_the_object() {
        let mut world = World::new();
        // Rotated and scaled: the box must ignore both.
        let matrix = (nalgebra::Matrix4::new_translation(
            &nalgebra::Vector3::new(1.0, 2.0, 3.0),
        ) * nalgebra::Matrix4::from_euler_angles(0.0, 0.7, 0.0)
            * nalgebra::Matrix4::new_scaling(2.0))
        .into();
        let entity = world
            .spawn((
                GlobalTransform { matrix },
                ReflectionProbe {
                    extents: [1.0, 2.0, 3.0],
                    intensity: 1.0,
                },
            ))
            .id();
        let shapes = object_shapes(&world, None);
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].0, entity);
        let points = shapes[0].1.iter().flatten();
        let fold = |pick: fn(f32, f32) -> f32, start: f32| {
            points.clone().fold([start; 3], |acc, point| {
                std::array::from_fn(|axis| pick(acc[axis], point[axis]))
            })
        };
        let near = |a: [f32; 3], b: [f32; 3]| {
            a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-5)
        };
        assert_eq!(shapes[0].1.len(), 12);
        assert!(near(fold(f32::min, f32::MAX), [0.0, 0.0, 0.0]));
        assert!(near(fold(f32::max, f32::MIN), [2.0, 4.0, 6.0]));

        // A hidden probe has no shape to see or click.
        world
            .entity_mut(entity)
            .insert(crate::runtime::Visibility { visible: false });
        assert!(object_shapes(&world, None).is_empty());
    }

    #[test]
    fn fog_height_squares_mark_full_density_and_the_thinned_height() {
        let fog = Fog {
            height: 2.0,
            height_falloff: 0.5,
            ..Fog::default()
        };
        let lines = fog_height_lines(&fog);
        assert_eq!(lines.len(), 8);
        assert!(lines[..4]
            .iter()
            .all(|([a, b], main)| *main && a[1] == 2.0 && b[1] == 2.0));
        assert!(lines[4..].iter().all(|([a, _], main)| !main && a[1] == 4.0));
        let uniform = Fog {
            height_falloff: 0.0,
            ..fog
        };
        assert_eq!(fog_height_lines(&uniform).len(), 4);
    }

    #[test]
    fn axis_uses_requested_world_length() {
        let mut overlay = RenderDebugOverlay::default();
        add_axis(
            &mut overlay,
            [2.0, 3.0, 4.0],
            [1.0, 0.0, 0.0],
            25.0,
            [1.0; 4],
        );

        assert_eq!(overlay.lines.len(), 3);
        assert_eq!(overlay.lines[0].end, [27.0, 3.0, 4.0]);
    }

    #[test]
    fn mesh_radius_includes_object_scale() {
        let matrix = crate::Transform::default()
            .with_scale(10.0, 10.0, 10.0)
            .to_matrix();

        let radius =
            mesh_world_radius_from_origin(([-1.0; 3], [1.0; 3]), matrix);
        assert!((radius - 300.0_f32.sqrt()).abs() < 0.001);
    }

    #[test]
    fn bound_box_has_twelve_edges() {
        let mut overlay = RenderDebugOverlay::default();
        add_bound_box(
            &mut overlay,
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            [-1.0, -1.0, -1.0],
            [1.0, 1.0, 1.0],
            [1.0, 1.0, 0.0, 1.0],
        );
        assert_eq!(overlay.lines.len(), 12);
    }

    #[test]
    fn render_bounds_outline_uses_the_culled_world_volume() {
        let matrix = crate::Transform::default()
            .with_position(10.0, 0.0, 0.0)
            .with_scale(2.0, 1.0, 1.0)
            .to_matrix();
        let mut overlay = RenderDebugOverlay::default();
        add_render_bounds(
            &mut overlay,
            RenderBounds::Aabb {
                min: [-1.0; 3],
                max: [1.0; 3],
            },
            matrix,
            [1.0; 4],
        );
        assert_eq!(overlay.lines.len(), 12);
        assert_eq!(overlay.lines[0].start, [8.0, -1.0, -1.0]);
        assert_eq!(overlay.lines[6].start, [12.0, 1.0, 1.0]);

        let mut overlay = RenderDebugOverlay::default();
        add_render_bounds(
            &mut overlay,
            RenderBounds::Sphere {
                center: [0.0; 3],
                radius: 1.0,
            },
            matrix,
            [1.0; 4],
        );
        assert_eq!(overlay.lines.len(), 96);
        // Every point sits on the radius-2 world sphere at x = 10.
        for line in &overlay.lines {
            let [x, y, z] = line.start;
            let distance = ((x - 10.0).powi(2) + y * y + z * z).sqrt();
            assert!((distance - 2.0).abs() < 1e-4, "{distance}");
        }
    }

    #[test]
    fn selection_outline_ignores_depth_without_changing_other_helpers() {
        let mut overlay = RenderDebugOverlay::default();
        overlay.line([0.0; 3], [1.0; 3], [0.5; 4]);
        let first_line = overlay.lines.len();
        add_bound_box(
            &mut overlay,
            nalgebra::Matrix4::<f32>::identity().into(),
            [-1.0; 3],
            [1.0; 3],
            [1.0; 4],
        );
        make_selection_outline_visible(&mut overlay, first_line);
        assert!(!overlay.lines[0].on_top);
        assert_eq!(overlay.lines[0].thickness, 1.0);
        assert_eq!(overlay.lines.len(), 13);
        assert!(overlay.lines[1..]
            .iter()
            .all(|line| line.on_top && line.thickness == 2.0));
    }
}
