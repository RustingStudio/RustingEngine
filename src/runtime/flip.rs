//! Grid fluid solver: FLIP (fluid implicit particle, Zhu and Bridson 2005)
//! on a staggered grid over the container box, for large water volumes.
//!
//! Particles carry the water; each step moves their velocities onto the grid,
//! makes the grid velocity free of divergence with a pressure solve, and
//! gives the particles the change (95 % FLIP, 5 % PIC). It costs one pass
//! over the particles and a few dozen over the cells, with no neighbor
//! search, so it scales to far more particles than the position based solver.
//!
//! Determinism: particles are splatted in index order, the pressure solve
//! sweeps cells in index order, and nothing uses atomics or randomness.

// The per-axis loops index several small arrays at once; iterators read worse.
#![allow(clippy::needless_range_loop)]

use super::fluid::{Fluid, FluidSettings};

/// Share of the grid's velocity change a particle takes on top of its own
/// velocity; the rest of its new velocity is the grid velocity itself (PIC).
/// Lower is calmer and more viscous.
const FLIP_RATIO: f32 = 0.95;

/// How hard a crowded cell pushes its particles apart, as a share of one
/// cell per step at twice the rest count.
const DRIFT: f32 = 0.3;

/// Gauss-Seidel sweeps of the pressure solve per density iteration of the
/// settings.
const SWEEPS_PER_ITERATION: u32 = 15;

/// Most cells a grid holds, whatever the box and spacing ask.
const MAX_CELLS: usize = 1 << 21;

/// The grid over the container: cells of twice the particle spacing, so a
/// settled cell holds about eight particles.
struct Grid {
    min: [f32; 3],
    size: f32,
    cells: [usize; 3],
}

impl Grid {
    fn new(settings: &FluidSettings) -> Self {
        let (min, max) = settings.bounds;
        // Scene data sets the box and the spacing; past MAX_CELLS the cells
        // grow instead, so a huge box cannot allocate without bound.
        let extent: [f64; 3] = std::array::from_fn(|axis| {
            let side = f64::from(max[axis]) - f64::from(min[axis]);
            if side.is_finite() {
                side.max(0.0)
            } else {
                0.0
            }
        });
        let mut size = f64::from(2.0 * settings.spacing).max(1e-3);
        let volume = extent.iter().map(|side| side / size).product::<f64>();
        if volume > MAX_CELLS as f64 {
            size *= (volume / MAX_CELLS as f64).cbrt();
        }
        let cells = std::array::from_fn(|axis| {
            ((extent[axis] / size).ceil() as usize).clamp(1, MAX_CELLS)
        });
        let mut grid = Self {
            min,
            size: size as f32,
            cells,
        };
        // Rounding up each axis can still pass the cap; shrink the largest.
        while grid.cells.iter().product::<usize>() > MAX_CELLS {
            let largest =
                (0..3).max_by_key(|&axis| grid.cells[axis]).unwrap_or(0);
            grid.cells[largest] -= 1;
        }
        grid
    }

    /// Size of the face array holding velocity component `axis`.
    fn faces(&self, axis: usize) -> [usize; 3] {
        let mut dims = self.cells;
        dims[axis] += 1;
        dims
    }

    fn cell_of(&self, p: [f32; 3]) -> usize {
        let at = |axis: usize| {
            let x = ((p[axis] - self.min[axis]) / self.size).floor().max(0.0);
            (x as usize).min(self.cells[axis] - 1)
        };
        index(self.cells, [at(0), at(1), at(2)])
    }

    /// The eight faces of component `axis` around `p` and their trilinear
    /// weights. Faces of `axis` sit on cell walls along it and at cell
    /// centers along the other two axes.
    fn stencil(&self, axis: usize, p: [f32; 3]) -> [(usize, f32); 8] {
        let dims = self.faces(axis);
        let mut base = [0usize; 3];
        let mut frac = [0.0f32; 3];
        for b in 0..3 {
            let shift = if b == axis { 0.0 } else { 0.5 };
            let top = (dims[b] - 1) as f32;
            let x = ((p[b] - self.min[b]) / self.size - shift).clamp(0.0, top);
            let i = (x.floor() as usize).min(dims[b].saturating_sub(2));
            base[b] = i;
            frac[b] = x - i as f32;
        }
        std::array::from_fn(|corner| {
            let mut at = [0usize; 3];
            let mut weight = 1.0;
            for b in 0..3 {
                let up = (corner >> b) & 1 == 1;
                at[b] = (base[b] + usize::from(up)).min(dims[b] - 1);
                weight *= if up { frac[b] } else { 1.0 - frac[b] };
            }
            (index(dims, at), weight)
        })
    }

    fn sample(&self, axis: usize, field: &[f32], p: [f32; 3]) -> f32 {
        self.stencil(axis, p)
            .iter()
            .map(|&(i, w)| field[i] * w)
            .sum()
    }
}

fn index(dims: [usize; 3], at: [usize; 3]) -> usize {
    at[0] + dims[0] * (at[1] + dims[1] * at[2])
}

/// Advances `fluid` by `dt` seconds with the grid solver.
pub(super) fn step(fluid: &mut Fluid, settings: &FluidSettings, dt: f32) {
    let grid = Grid::new(settings);
    for v in &mut fluid.velocities {
        for axis in 0..3 {
            v[axis] += settings.gravity[axis] * dt;
        }
    }
    // Particles to grid, in particle order.
    let mut velocity: [Vec<f32>; 3] = std::array::from_fn(|axis| {
        let dims = grid.faces(axis);
        let mut sum = vec![0.0f32; dims[0] * dims[1] * dims[2]];
        let mut weight = vec![0.0f32; sum.len()];
        for (p, v) in fluid.positions.iter().zip(&fluid.velocities) {
            for (i, w) in grid.stencil(axis, *p) {
                sum[i] += w * v[axis];
                weight[i] += w;
            }
        }
        for (s, w) in sum.iter_mut().zip(&weight) {
            if *w > 0.0 {
                *s /= w;
            }
        }
        // The container walls let nothing through.
        for (i, s) in sum.iter_mut().enumerate() {
            let along =
                (i / [1, dims[0], dims[0] * dims[1]][axis]) % dims[axis];
            if along == 0 || along == dims[axis] - 1 {
                *s = 0.0;
            }
        }
        sum
    });
    let before = velocity.clone();
    let cells = grid.cells;
    let mut crowd = vec![0u32; cells[0] * cells[1] * cells[2]];
    for p in &fluid.positions {
        crowd[grid.cell_of(*p)] += 1;
    }
    let water: Vec<bool> = crowd.iter().map(|&n| n > 0).collect();
    // The grid alone cannot tell a crowded cell from a sparse one, so the
    // particles drift together. A cell holding more than its rest share of
    // eight particles asks for outflow in proportion (drift correction).
    let push = DRIFT * grid.size / dt;
    let target: Vec<f32> = (0..water.len())
        .map(|c| {
            let crowded = crowd[c] as f32 / 8.0 - 1.0;
            push * crowded.max(0.0)
        })
        .collect();
    // Pressure: make every water cell's outflow zero. Air cells hold zero
    // pressure; the walls are closed. `pressure` is in velocity units, so a
    // face loses the difference of its two cells.
    let mut pressure = vec![0.0f32; water.len()];
    let sweeps = settings
        .iterations
        .clamp(1, super::fluid::MAX_FLUID_ITERATIONS)
        * SWEEPS_PER_ITERATION;
    let face = |axis: usize, at: [usize; 3]| index(grid.faces(axis), at);
    for _ in 0..sweeps {
        for k in 0..cells[2] {
            for j in 0..cells[1] {
                for i in 0..cells[0] {
                    let at = [i, j, k];
                    let c = index(cells, at);
                    if !water[c] {
                        continue;
                    }
                    let mut outflow = 0.0;
                    let mut sum = 0.0;
                    let mut open = 0.0;
                    for axis in 0..3 {
                        let mut up = at;
                        up[axis] += 1;
                        outflow += velocity[axis][face(axis, up)]
                            - velocity[axis][face(axis, at)];
                        if at[axis] > 0 {
                            let mut n = at;
                            n[axis] -= 1;
                            sum += pressure[index(cells, n)];
                            open += 1.0;
                        }
                        if at[axis] + 1 < cells[axis] {
                            sum += pressure[index(cells, up)];
                            open += 1.0;
                        }
                    }
                    // Outflow is measured on the unprojected velocity, so the
                    // neighbors' pressure is the only thing that changes.
                    if open > 0.0 {
                        pressure[c] = (sum - outflow + target[c]) / open;
                    }
                }
            }
        }
    }
    project(&grid, &water, &pressure, &mut velocity);
    // Grid to particles, then move.
    let (min, max) = settings.bounds;
    for (p, v) in fluid.positions.iter_mut().zip(&mut fluid.velocities) {
        for axis in 0..3 {
            let pic = grid.sample(axis, &velocity[axis], *p);
            let change = pic - grid.sample(axis, &before[axis], *p);
            v[axis] =
                FLIP_RATIO * (v[axis] + change) + (1.0 - FLIP_RATIO) * pic;
        }
        for axis in 0..3 {
            let moved = p[axis] + v[axis] * dt;
            p[axis] = moved.clamp(min[axis], max[axis]);
            // A wall stops the particle along it.
            if p[axis] != moved {
                v[axis] = 0.0;
            }
        }
    }
}

/// Takes the pressure difference off every inner face next to water.
fn project(
    grid: &Grid,
    water: &[bool],
    pressure: &[f32],
    velocity: &mut [Vec<f32>; 3],
) {
    let cells = grid.cells;
    for k in 0..cells[2] {
        for j in 0..cells[1] {
            for i in 0..cells[0] {
                let at = [i, j, k];
                for axis in 0..3 {
                    if at[axis] == 0 {
                        continue;
                    }
                    let mut below = at;
                    below[axis] -= 1;
                    let (a, b) = (index(cells, below), index(cells, at));
                    if water[a] || water[b] {
                        velocity[axis][index(grid.faces(axis), at)] -=
                            pressure[b] - pressure[a];
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fluid::{Fluid, FluidSettings, FluidSolver};

    fn settings() -> FluidSettings {
        FluidSettings {
            spacing: 0.05,
            bounds: ([-0.5, 0.0, -0.5], [0.5, 2.0, 0.5]),
            solver: FluidSolver::Grid,
            ..FluidSettings::default()
        }
    }

    /// A 0.4 m wide, 1 m tall column on the floor of a 1 m wide box.
    fn column() -> Fluid {
        Fluid::block([-0.2, 0.025, -0.2], [8, 20, 8], 0.05)
    }

    #[test]
    fn a_grid_column_spreads_over_the_floor_and_keeps_its_volume() {
        let settings = settings();
        let mut fluid = column();
        for _ in 0..240 {
            fluid.step(&settings, 1.0 / 60.0);
        }
        let (min, max) = settings.bounds;
        let mut speed = 0.0f32;
        for (p, v) in fluid.positions.iter().zip(&fluid.velocities) {
            for axis in 0..3 {
                assert!(p[axis].is_finite() && v[axis].is_finite());
                assert!(p[axis] >= min[axis] && p[axis] <= max[axis]);
            }
            speed = speed.max((v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt());
        }
        // 1280 particles of 0.05 m fill 0.16 m of the 1 m square floor, so
        // the mean height of a settled pool is about 0.08 m.
        let mean = fluid.positions.iter().map(|p| p[1]).sum::<f32>()
            / fluid.positions.len() as f32;
        assert!((0.06..0.12).contains(&mean), "mean height {mean}");
        assert!(speed < 1.5, "still moving at {speed} m/s");
    }

    #[test]
    fn a_huge_box_gets_a_coarser_grid_not_a_huge_one() {
        for bounds in [
            ([-1e4, -1e4, -1e4], [1e4, 1e4, 1e4]),
            ([f32::NEG_INFINITY, 0.0, 0.0], [f32::INFINITY, 1.0, 1.0]),
        ] {
            let grid = super::Grid::new(&FluidSettings {
                bounds,
                ..settings()
            });
            assert!(grid.cells.iter().product::<usize>() <= super::MAX_CELLS);
            assert!(grid.cells.iter().all(|&n| n >= 1));
        }
        let mut fluid = column();
        let huge = FluidSettings {
            bounds: ([-1e4, 0.0, -1e4], [1e4, 1e4, 1e4]),
            ..settings()
        };
        fluid.step(&huge, 1.0 / 60.0);
        assert!(fluid.positions.iter().flatten().all(|x| x.is_finite()));
    }

    #[test]
    fn the_same_grid_start_gives_the_same_bits() {
        let run = || {
            let mut fluid = column();
            for _ in 0..30 {
                fluid.step(&settings(), 1.0 / 60.0);
            }
            fluid.state_hash()
        };
        assert_eq!(run(), run());
    }
}
