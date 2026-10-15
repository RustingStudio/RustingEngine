//! 100,000 balls fall through a Galton board and pile up into a bell curve.
//!
//! ```sh
//! cargo run --release --features window --example galton_board
//! ```
//!
//! `--restitution`, `--friction` and `--static-friction` change the
//! surfaces, `--peg-radius` and `--terminal` (m/s) the board, `--rate`
//! (balls a second) and `--in-flight` the pour, `--measure-at` (s) when
//! the pile is measured, and `--balls 20000` the count. The board is 2D:
//! every GPU body is locked to the XY plane. 40 rows of static round pegs
//! sit above 41 bins. Each ball is a real GPU body with full contacts
//! against the pegs, the walls and the other balls.
//!
//! The balls start parked far to the side with gravity off. A condition
//! shader drops them one by one in spawn order, in a thin column inside the
//! funnel neck, and adds air drag while they fall, so they reach the pile no
//! faster than `--terminal`. A ball that drops below the bin tops sends a
//! `landed` event, and at `--measure-at` every landed ball sends its
//! position. From those the example prints the spread of the pile in bins
//! next to the theory, √10, and draws the ideal normal curve over the pile,
//! scaled by the measured packing. The camera follows game time, so a
//! replay films the same shot.

use rusting_engine::prelude::*;
use rusting_engine::runtime::{
    AxisLock, Bloom, Collider, ColliderShape, PhysicsBody, SimulationClass,
};
use std::cell::RefCell;
use std::f32::consts::PI;

/// Peg spacing in a row, and the width of a bin, in ball diameters.
const SPACING: f32 = 20.0;
const ROWS: usize = 40;
const BINS: usize = ROWS + 1;
/// Height of the bin walls; the floor is at y = 0.
const BIN_HEIGHT: f32 = 950.0;
/// Gap from the bin tops up to the centre of the last peg row.
const LAST_ROW_GAP: f32 = 15.0;
const WALL: f32 = 0.6;
/// Parked balls: a grid off to the side of the board.
const PARK_X: f32 = 1500.0;
const PARK_SPACING: f32 = 1.2;
const PARK_COLUMNS: usize = 320;
/// The shot: game seconds where the video starts.
const SHOT_START: f32 = 175.0;
/// Seconds after the measurement that the curve draws itself over.
const CURVE_FROM: f32 = 6.0;
const CURVE_SECONDS: f32 = 4.0;
const CURVE_SEGMENTS: usize = 240;
const COLORS: usize = 32;

const POUR: &str = r#"
void condition(inout PhysicsState body) {
    vec4 pour = condition_params.values[0];    // x rate, y limit, z bin top, w measure time
    vec4 park = condition_params.values[1];    // xy origin, z spacing, w columns
    vec4 release = condition_params.values[2]; // x y, y half width, z speed, w drag
    vec3 position = body.model[3].xyz;
    if (body.custom_values.y == 0.0) {
        vec2 cell = round((position.xy - park.xy) / park.z);
        float index = cell.y * park.w + cell.x;
        if (index >= min(pour.x * pc.elapsed, pour.y)) return;
        // Golden-ratio lanes: balls dropped close in time land far
        // apart across the stream, so none starts inside another.
        float lane = fract(index * 0.618034);
        body.model[3].xy = vec2((2.0 * lane - 1.0) * release.y, release.x);
        body.velocity.xyz = vec3(0.0, -release.z, 0.0);
        body.properties.y = 1.0;
        body.custom_values.y = 1.0;
        body.custom_values.z = index;
        body.custom_values.w = 0.0;
        body.angular_velocity.w = 0.0;
        return;
    }
    // Drag on every dropped ball, so none hits the pile faster than the
    // contact solver can take.
    body.velocity.xyz *= exp(-release.w * pc.dt);
    if (body.custom_values.y == 1.0) {
        // A falling ball never sleeps: a slow one near a peg's top would
        // stay perched there.
        body.angular_velocity.w = 0.0;
        if (position.y < pour.z) {
            body.custom_values.y = 2.0;
            // A small sideways push, so a ball does not balance exactly on
            // top of a fixed one below it and build a one-ball tower.
            body.velocity.x += (2.0 * fract(body.custom_values.z * 0.754878) - 1.0) * 0.5;
            emit_event(body, EVENTS[0], 1u, vec4(position, body.custom_values.z));
        }
        return;
    }
    // Landed: the bin is decided. Deep stacks jitter in the contact solver
    // and throw balls upward, so upward motion is damped hard, and a ball
    // that has stayed slow for a second is fixed in place.
    if (body.custom_values.y == 2.0) {
        body.velocity.y = min(body.velocity.y, body.velocity.y * exp(-20.0 * pc.dt));
        body.custom_values.w = length(body.velocity.xyz) < 0.5 ? body.custom_values.w + 1.0 : 0.0;
        if (body.custom_values.w >= 60.0) {
            body.properties.z = 0.0;
            body.velocity.xyz = vec3(0.0);
            body.custom_values.y = 3.0;
        }
    }
    if (pc.elapsed >= pour.w && pc.elapsed < pour.w + pc.dt) {
        emit_event(body, EVENTS[1], 1u, vec4(position, body.custom_values.z));
    }
}
"#;

fn flag(name: &str) -> Option<String> {
    let mut arguments = std::env::args();
    arguments.find(|arg| arg == name)?;
    arguments.next()
}

fn number(name: &str, default: f32) -> f32 {
    flag(name).map_or(default, |value| value.parse().expect(name))
}

fn ball_count() -> usize {
    flag("--balls").map_or(100_000, |value| value.parse().expect("--balls"))
}

fn row_height() -> f32 {
    SPACING * 3.0_f32.sqrt() / 2.0
}

fn row_y(row: usize) -> f32 {
    BIN_HEIGHT + LAST_ROW_GAP + (ROWS - 1 - row) as f32 * row_height()
}

/// Height of the funnel above its neck. Tall enough to hold every ball
/// waiting to go through, so none spills over the rim.
const FUNNEL_HEIGHT: f32 = 160.0;
/// Half the funnel's width at its rim.
const FUNNEL_RIM: f32 = 130.0;

fn neck_y() -> f32 {
    row_y(0) + 25.0
}

fn half_width() -> f32 {
    BINS as f32 * SPACING / 2.0
}

/// Friction of the pegs and walls. The solver never spins a ball, so with
/// friction one could rest on a peg's top where a real ball rolls off.
fn static_friction() -> f32 {
    number("--static-friction", 0.0)
}

/// Peg radius. Rows sit `row_height()` (17.3) apart along the lattice's
/// 60 degree lines, so pegs over 8.2 close those lines to a ball: none can
/// roll down a straight lane, every row turns it left or right.
fn peg_radius() -> f32 {
    number("--peg-radius", 8.5)
}

/// Game seconds when the pile is measured.
fn measure_at() -> f32 {
    number(
        "--measure-at",
        ball_count() as f32 / number("--rate", 15.0) + 400.0,
    )
}

fn hue(hue: f32) -> [f32; 3] {
    let channel = |offset: f32| {
        let k = (offset + hue * 6.0) % 6.0;
        1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))
    };
    [channel(5.0), channel(3.0), channel(1.0)]
}

/// A black material that only emits: `Unlit` ignores `emissive`.
fn glow(scene: &mut GameScene<'_>, emissive: [f32; 3]) -> Handle {
    scene.create_material(MaterialAsset {
        base_color: [0.0, 0.0, 0.0, 1.0],
        roughness: 1.0,
        emissive,
        ..MaterialAsset::default()
    })
}

type Handle = rusting_engine::assets::Handle<MaterialAsset>;

/// Spawns a static collider drawn with `material`.
fn spawn_static(
    scene: &mut GameScene<'_>,
    name: String,
    transform: Transform,
    shape: ColliderShape,
    friction: f32,
    material: Handle,
) {
    let entity = if matches!(shape, ColliderShape::Sphere { .. }) {
        scene.spawn_sphere_with_material(
            name,
            transform,
            &SphereSpawn::new().subdivisions(16),
            material,
        )
    } else {
        scene.spawn_cube_with_material(
            name,
            transform,
            &CubeSpawn::new(),
            material,
        )
    };
    scene.world().entity_mut(entity).insert((
        Collider {
            shape,
            friction,
            restitution: number("--restitution", 0.3),
            ..Collider::default()
        },
        PhysicsBody {
            simulation: SimulationClass::Static,
            ..PhysicsBody::default()
        },
    ));
}

/// A wall from `from` to `to` in the XY plane.
fn spawn_wall(
    scene: &mut GameScene<'_>,
    name: String,
    from: [f32; 2],
    to: [f32; 2],
    material: Handle,
) {
    let (dx, dy) = (to[0] - from[0], to[1] - from[1]);
    let mut transform =
        Transform::new([(from[0] + to[0]) / 2.0, (from[1] + to[1]) / 2.0, 0.0])
            .with_rotation(0.0, 0.0, dy.atan2(dx));
    transform.scale = [(dx * dx + dy * dy).sqrt(), WALL, 4.0];
    let box_shape = ColliderShape::Box {
        half_extents: [0.5; 3],
    };
    spawn_static(
        scene,
        name,
        transform,
        box_shape,
        static_friction(),
        material,
    );
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.006, 0.008, 0.012, 1.0]);
    scene.world().spawn(Bloom {
        intensity: 0.35,
        threshold: 1.0,
        spread: 0.5,
    });
    scene.set_gpu_axis_lock(AxisLock::PLANE_XY);

    let peg = glow(scene, [0.32, 0.34, 0.4]);
    let wall = glow(scene, [0.22, 0.24, 0.3]);
    let sphere = ColliderShape::Sphere { radius: 0.5 };
    for row in 0..ROWS {
        let shift = if row % 2 == 1 { 0.5 } else { 0.0 };
        for column in 0..=BINS {
            let x = (column as f32 - (BINS / 2) as f32 + shift) * SPACING;
            if x.abs() >= half_width() - peg_radius() {
                continue;
            }
            let mut transform = Transform::new([x, row_y(row), 0.0]);
            transform.scale = [2.0 * peg_radius(); 3];
            spawn_static(
                scene,
                format!("Peg {row} {column}"),
                transform,
                sphere,
                static_friction(),
                peg,
            );
        }
    }
    // Bin walls under the last row's pegs; the outer two run up the board.
    let edge = half_width();
    for wall_index in 0..=BINS {
        let x = (wall_index as f32 - BINS as f32 / 2.0) * SPACING;
        let top = if wall_index == 0 || wall_index == BINS {
            neck_y() + FUNNEL_HEIGHT + 40.0
        } else {
            BIN_HEIGHT
        };
        spawn_wall(
            scene,
            format!("Bin Wall {wall_index}"),
            [x, 0.0],
            [x, top],
            wall,
        );
    }
    spawn_wall(
        scene,
        "Floor".into(),
        [-edge, -WALL / 2.0],
        [edge, -WALL / 2.0],
        wall,
    );
    // The funnel: two slopes that end at a neck one bin wide.
    let neck = neck_y();
    for side in [-1.0_f32, 1.0] {
        spawn_wall(
            scene,
            format!("Funnel {side}"),
            [side * SPACING / 2.0, neck],
            [side * FUNNEL_RIM, neck + FUNNEL_HEIGHT],
            wall,
        );
    }

    // Rainbow by spawn order: red first, violet last.
    let colors: Vec<_> = (0..COLORS)
        .map(|index| {
            let [r, g, b] = hue(0.8 * index as f32 / (COLORS - 1) as f32);
            glow(scene, [1.6 * r, 1.6 * g, 1.6 * b])
        })
        .collect();
    let count = ball_count();
    let shape = SphereSpawn::new().subdivisions(6).class("galton_ball");
    for index in 0..count {
        let x = PARK_X + (index % PARK_COLUMNS) as f32 * PARK_SPACING;
        let y = (index / PARK_COLUMNS) as f32 * PARK_SPACING;
        scene.spawn_sphere_with_material(
            format!("Ball {index}"),
            Transform::new([x, y, 0.0]),
            &shape,
            colors[index * COLORS / count],
        );
    }
    scene.apply_gpu_physics_to_class(
        "galton_ball",
        &GpuBodySettings {
            solver: PhysicsSolver::Full,
            rigid_body: RigidBody {
                gravity_scale: 0.0,
                ..RigidBody::default()
            },
            collider: Collider {
                shape: sphere,
                friction: number("--friction", 0.05),
                restitution: number("--restitution", 0.3),
                ..Collider::default()
            },
            ..GpuBodySettings::default()
        },
    );

    // The ideal curve, hidden until the pile is measured.
    let white = glow(scene, [2.2, 2.2, 2.2]);
    for index in 0..CURVE_SEGMENTS {
        let mut transform = Transform::new([0.0, 0.0, 3.0]);
        transform.scale = [0.0; 3];
        scene.spawn_cube_with_material(
            format!("Curve {index}"),
            transform,
            &CubeSpawn::new(),
            white,
        );
    }
    println!(
        "galton board: {count} balls, {} pegs of radius {}, restitution {}, \
         friction {}, terminal speed {} m/s, measured at {} s",
        ROWS * BINS - ROWS / 2,
        peg_radius(),
        number("--restitution", 0.3),
        number("--friction", 0.05),
        number("--terminal", 8.0),
        measure_at()
    );
}

#[derive(Default)]
struct Tally {
    total: usize,
    /// Points of the ideal curve once the pile is measured.
    curve: Option<Vec<[f32; 2]>>,
}

thread_local! {
    static TALLY: RefCell<Tally> = RefCell::new(Tally::default());
}

/// Prints the pile's spread and returns the ideal curve over it.
fn measure(pile: &[[f32; 4]], landed: usize) -> Vec<[f32; 2]> {
    let half = (BINS / 2) as f32;
    let mut heights = vec![Vec::new(); BINS];
    for ball in pile {
        let bin = (ball[0] / SPACING + half).round().clamp(0.0, half * 2.0);
        heights[bin as usize].push(ball[1]);
    }
    let counts = heights.iter().map(Vec::len).collect::<Vec<_>>();
    // The 97th percentile ball, scaled up: a stray ball on top does not
    // count as the whole column.
    let tops = heights
        .iter_mut()
        .map(|heights| {
            heights.sort_by(f32::total_cmp);
            heights
                .get(heights.len() * 97 / 100)
                .map_or(0.0, |y| (y + 0.5) / 0.97)
        })
        .collect::<Vec<_>>();
    let total = pile.len() as f32;
    let mean = pile.iter().map(|ball| ball[0] / SPACING).sum::<f32>() / total;
    let variance = pile
        .iter()
        .map(|ball| (ball[0] / SPACING - mean).powi(2))
        .sum::<f32>()
        / total;
    let binned = counts
        .iter()
        .enumerate()
        .map(|(bin, &count)| count as f32 * (bin as f32 - half - mean).powi(2))
        .sum::<f32>()
        / total;
    // Packing from the full bins: balls per square metre of pile.
    let inside = SPACING - WALL;
    let (balls, area) = counts
        .iter()
        .zip(&tops)
        .filter(|(count, _)| **count > 500)
        .fold((0.0, 0.0), |(balls, area), (&count, top)| {
            (balls + count as f32, area + top * inside)
        });
    let packing = balls / area;
    let sigma = (ROWS as f32).sqrt() / 2.0;
    println!(
        "galton measure: {} balls in the pile ({landed} landed events), \
         mean {mean:.3} bins, spread {:.3} bins (binned {:.3}), theory \
         {sigma:.3}, ratio {:.3}, packing {packing:.3} balls/m²",
        pile.len(),
        variance.sqrt(),
        binned.sqrt(),
        variance.sqrt() / sigma
    );
    let outside = pile
        .iter()
        .filter(|ball| ball[0].abs() > half_width() || ball[1] > BIN_HEIGHT)
        .count();
    println!("galton bins: {counts:?}, {outside} outside the bins");
    let strays = pile
        .iter()
        .filter(|ball| ball[0].abs() > half_width() || ball[1] > BIN_HEIGHT)
        .take(6)
        .map(|ball| [ball[0].round(), ball[1].round()])
        .collect::<Vec<_>>();
    println!("galton strays: {strays:?}");
    println!(
        "galton tops: {:?}",
        tops.iter()
            .map(|top| (top * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    // Expected count per bin from the normal, as a pile height.
    let height = |x: f32| {
        let z = x / (sigma * SPACING);
        total * (-z * z / 2.0).exp()
            / (sigma * (2.0 * PI).sqrt())
            / (packing * inside)
    };
    (0..=CURVE_SEGMENTS)
        .map(|index| {
            let x = (index as f32 / CURVE_SEGMENTS as f32 * 2.0 - 1.0)
                * half_width();
            [x, height(x)]
        })
        .collect()
}

fn smooth(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_galton_board", create);
    let landed = scene.gpu_events("landed");
    let pile = scene.gpu_events("pile");
    let total = TALLY.with(|tally| {
        let mut tally = tally.borrow_mut();
        tally.total += landed.len();
        tally.total
    });
    let limit = (total as f32 + number("--in-flight", 8_000.0))
        .min(ball_count() as f32);
    scene.set_gpu_condition_shaders(vec![GpuConditionShader {
        events: vec!["landed".into(), "pile".into()],
        glsl: POUR.into(),
        params: vec![
            // A sparse stream: at 30 balls a second and up, crowded balls
            // push each other through the board's walls.
            [number("--rate", 15.0), limit, BIN_HEIGHT, measure_at()],
            [PARK_X, 0.0, PARK_SPACING, PARK_COLUMNS as f32],
            [
                neck_y() + FUNNEL_HEIGHT + 5.0,
                // A stream narrower than the neck, so no ball hits the
                // funnel's slopes: a hard hit there throws balls out of the
                // board.
                SPACING / 2.0 - 2.0,
                number("--terminal", 8.0),
                9.81 / number("--terminal", 8.0),
            ],
        ],
        events_per_body: 1,
    }]);

    let t = time.elapsed_seconds();
    let curve = TALLY.with(|tally| {
        let mut tally = tally.borrow_mut();
        if !pile.is_empty() && tally.curve.is_none() {
            let points: Vec<_> =
                pile.iter().map(|event| event.payload).collect();
            tally.curve = Some(measure(&points, tally.total));
        }
        println!("galton tick {} landed {}", time.fixed_tick, tally.total);
        tally.curve.clone()
    });

    // The curve draws itself from left to right.
    if let Some(points) = curve {
        let reveal = smooth((t - measure_at() - CURVE_FROM) / CURVE_SECONDS);
        let shown = reveal * CURVE_SEGMENTS as f32;
        for index in 0..CURVE_SEGMENTS {
            let [a, b] = [points[index], points[index + 1]];
            let part = (shown - index as f32).clamp(0.0, 1.0);
            let end =
                [a[0] + (b[0] - a[0]) * part, a[1] + (b[1] - a[1]) * part];
            let (dx, dy) = (end[0] - a[0], end[1] - a[1]);
            let length = (dx * dx + dy * dy).sqrt();
            let mut segment = scene.object(&format!("Curve {index}"));
            segment
                .set_position([
                    (a[0] + end[0]) / 2.0,
                    (a[1] + end[1]) / 2.0,
                    3.0,
                ])
                .set_rotation([0.0, 0.0, dy.atan2(dx)]);
            if part > 0.0 {
                segment.set_scale([length + 2.5, 2.5, 2.5]);
            } else {
                segment.set_scale([0.0; 3]);
            }
        }
    }

    // Tight on the funnel, then down and back to the whole board.
    let fov = 60.0_f32.to_radians();
    let pull = smooth((t - number("--shot-start", SHOT_START) - 2.0) / 6.0);
    let tight = [0.0, neck_y() - 35.0, 150.0];
    let top = neck_y() + FUNNEL_HEIGHT;
    let full = [0.0, (top + 20.0) / 2.0 - 10.0, top + 80.0];
    let view = |axis: usize| tight[axis] + (full[axis] - tight[axis]) * pull;
    let distance = view(2) / 2.0 / (fov / 2.0).tan();
    scene
        .object("Game Camera")
        .set_position([0.0, view(1), distance])
        .look_at([0.0, view(1), 0.0]);
}

rusting_game!("testGame/build/main.rscene.bin", update);
