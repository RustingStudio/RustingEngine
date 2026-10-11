//! Granular solver: sand, gravel and rubble as position based grains
//! (Macklin et al. 2014, "Unified particle physics").
//!
//! Each grain is a sphere of diameter `spacing`. Every iteration pushes
//! overlapping grains apart and cancels their sliding by Coulomb friction:
//! sliding shorter than `friction` times the overlap sticks, longer sliding
//! is slowed by that much. The container floor and walls hold grains the
//! same way. Friction is what lets a heap keep its slope instead of
//! spreading flat; the slope it keeps (the angle of repose) grows with
//! `friction`.
//!
//! Determinism: neighbor lists are sorted, pairs are solved in index order,
//! and nothing uses atomics or randomness.

// The per-axis loops index several small arrays at once; iterators read worse.
#![allow(clippy::needless_range_loop)]

use super::fluid::{Fluid, FluidSettings, MAX_FLUID_ITERATIONS};

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|axis| a[axis] - b[axis])
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Takes from `slide` (a displacement across the contact) what Coulomb
/// friction removes for a contact `overlap` deep.
fn friction(slide: [f32; 3], overlap: f32, friction: f32) -> [f32; 3] {
    let length = dot(slide, slide).sqrt();
    let limit = friction * overlap;
    if length <= limit {
        return slide.map(|x| -x);
    }
    slide.map(|x| -x * limit / length)
}

/// Advances `fluid` by `dt` seconds as grains.
pub(super) fn step(fluid: &mut Fluid, settings: &FluidSettings, dt: f32) {
    let count = fluid.positions.len();
    let diameter = settings.spacing;
    let (low, high) = settings.bounds;
    let mut predicted: Vec<[f32; 3]> = (0..count)
        .map(|i| {
            let v = &mut fluid.velocities[i];
            for axis in 0..3 {
                v[axis] += settings.gravity[axis] * dt;
            }
            std::array::from_fn(|axis| fluid.positions[i][axis] + v[axis] * dt)
        })
        .collect();
    // A margin, so grains that close in during the iterations still meet.
    let neighbors = super::fluid::neighbors(&predicted, 1.5 * diameter);
    for _ in 0..settings.iterations.clamp(1, MAX_FLUID_ITERATIONS) {
        // Gauss-Seidel over the pairs in index order: each pair sees the
        // corrections made before it, which holds a heap far better than
        // averaged (Jacobi) corrections do.
        for i in 0..count {
            for &j in &neighbors[i] {
                let j = j as usize;
                if j < i {
                    continue;
                }
                let offset = sub(predicted[i], predicted[j]);
                let distance = dot(offset, offset).sqrt();
                let overlap = diameter - distance;
                if overlap <= 0.0 || distance < 1e-6 {
                    continue;
                }
                let normal = offset.map(|x| x / distance);
                // Each grain takes half: apart along the normal, and half
                // of their relative slide this step cancelled.
                let moved = sub(
                    sub(predicted[i], fluid.positions[i]),
                    sub(predicted[j], fluid.positions[j]),
                );
                let along = dot(moved, normal);
                let slide: [f32; 3] = std::array::from_fn(|axis| {
                    moved[axis] - along * normal[axis]
                });
                let stop = friction(slide, overlap, settings.friction);
                for axis in 0..3 {
                    let push = 0.5 * (overlap * normal[axis] + stop[axis]);
                    predicted[i][axis] += push;
                    predicted[j][axis] -= push;
                }
            }
            // The container: a grain pushed back in through a wall also has
            // its slide along that wall cut by friction.
            let grain = &mut predicted[i];
            for axis in 0..3 {
                let wall = if grain[axis] < low[axis] {
                    low[axis]
                } else if grain[axis] > high[axis] {
                    high[axis]
                } else {
                    continue;
                };
                let overlap = (wall - grain[axis]).abs();
                grain[axis] = wall;
                let mut slide = sub(*grain, fluid.positions[i]);
                slide[axis] = 0.0;
                let stop = friction(slide, overlap, settings.friction);
                for other in 0..3 {
                    if other != axis {
                        grain[other] += stop[other];
                    }
                }
            }
        }
    }
    for i in 0..count {
        for axis in 0..3 {
            fluid.velocities[i][axis] =
                (predicted[i][axis] - fluid.positions[i][axis]) / dt;
        }
    }
    fluid.positions = predicted;
}

#[cfg(test)]
mod tests {
    use super::super::fluid::{Fluid, FluidSettings, FluidSolver};

    fn settings(friction: f32) -> FluidSettings {
        FluidSettings {
            spacing: 0.05,
            iterations: 8,
            bounds: ([-1.0, 0.0, -1.0], [1.0, 2.0, 1.0]),
            solver: FluidSolver::Granular,
            friction,
            ..FluidSettings::default()
        }
    }

    /// A 0.35 m wide, 1 m tall column of 980 grains on the floor.
    fn column() -> Fluid {
        Fluid::block([-0.15, 0.025, -0.15], [7, 20, 7], 0.05)
    }

    /// The height under 95 % of the grains over the reach of 95 % of them
    /// from the middle
    /// after the column collapses for 6 s, and its mean speed then.
    fn heap(friction: f32) -> (f32, f32) {
        let settings = settings(friction);
        let mut grains = column();
        for _ in 0..360 {
            grains.step(&settings, 1.0 / 60.0);
        }
        let (low, high) = settings.bounds;
        let mut top = Vec::new();
        let mut reach = Vec::new();
        let mut speed = 0.0;
        for (p, v) in grains.positions.iter().zip(&grains.velocities) {
            for axis in 0..3 {
                assert!(p[axis].is_finite() && v[axis].is_finite());
                assert!(p[axis] >= low[axis] && p[axis] <= high[axis]);
            }
            top.push(p[1]);
            reach.push(p[0].hypot(p[2]));
            speed += (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        }
        top.sort_by(f32::total_cmp);
        reach.sort_by(f32::total_cmp);
        let count = grains.positions.len();
        let high = count * 95 / 100;
        (top[high] / reach[high], speed / count as f32)
    }

    #[test]
    fn grains_pile_into_a_heap_and_friction_steepens_it() {
        let (sand, speed) = heap(0.6);
        let (slick, _) = heap(0.0);
        // Measured 0.33 with friction and 0.17 without; a flat layer of
        // grains would be about 0.05.
        assert!(sand > 0.25, "sand heap slope {sand}");
        assert!(slick < 0.7 * sand, "frictionless slope {slick}");
        assert!(speed < 0.1, "grains still move at {speed} m/s on average");
    }

    #[test]
    fn the_same_grains_give_the_same_bits() {
        let run = || {
            let mut grains = column();
            for _ in 0..30 {
                grains.step(&settings(0.6), 1.0 / 60.0);
            }
            grains.state_hash()
        };
        assert_eq!(run(), run());
    }
}
