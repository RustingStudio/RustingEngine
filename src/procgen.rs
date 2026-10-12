//! Seeded randomness for generated content: random streams, gradient noise
//! and Poisson-disk sampling.
//!
//! Every result depends only on the seed and the inputs. The code uses
//! integer hashing and the IEEE operations `+ - * /` and `sqrt` on `f32`,
//! which round the same way everywhere, and no `sin`, `exp` or `powf`, whose
//! last bits differ between platform math libraries. So the same seed gives
//! the same level on every machine.

use crate::runtime::RandomSeed;

/// A sequence of random values from one seed, for generating content.
///
/// Take the seed from the run's [`RandomSeed`] with [`Self::from_seed`], so
/// a level made at a given fixed tick repeats for the same game seed. Use
/// one stream per generator: two generators sharing a stream change each
/// other's results when either draws a different number of values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RandomStream {
    seed: u64,
    index: u64,
}

impl RandomStream {
    /// A stream starting from `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { seed, index: 0 }
    }

    /// The stream for `key` of the subsystem called `subsystem` at fixed
    /// `tick`, under the Milestone 8 stream rules: the same game seed, tick,
    /// subsystem and key always give the same values.
    #[must_use]
    pub fn from_seed(
        seed: RandomSeed,
        tick: u64,
        subsystem: &str,
        key: u64,
    ) -> Self {
        Self::new(seed.value(tick, RandomSeed::stream(subsystem, key)))
    }

    /// The next uniform 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let value = RandomSeed(self.seed).value(self.index, 0);
        self.index += 1;
        value
    }

    /// The next value in `[0, 1)`, with 24 random bits.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1_u64 << 24) as f32
    }

    /// The next value in `[min, max)`.
    pub fn range(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.unit()
    }

    /// The next whole number in `0..count`; 0 when `count` is 0.
    pub fn below(&mut self, count: u64) -> u64 {
        // ponytail: multiply-shift has a bias under count / 2^64, far
        // below anything a game can see; reject the low part if it matters.
        ((u128::from(self.next_u64()) * u128::from(count)) >> 64) as u64
    }

    /// `true` with probability `chance` (0 never, 1 always).
    pub fn chance(&mut self, chance: f32) -> bool {
        self.unit() < chance
    }

    /// An index into `weights`, each picked with a chance of its weight over
    /// the total. `None` when no weight is above 0; negative weights count
    /// as 0.
    pub fn weighted(&mut self, weights: &[f32]) -> Option<usize> {
        let total: f32 = weights.iter().map(|weight| weight.max(0.0)).sum();
        if total <= 0.0 {
            return None;
        }
        let mut left = self.unit() * total;
        let last = weights.iter().rposition(|weight| *weight > 0.0)?;
        for (index, weight) in weights.iter().enumerate().take(last) {
            left -= weight.max(0.0);
            if left < 0.0 {
                return Some(index);
            }
        }
        Some(last)
    }

    /// Puts `items` in a random order.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for last in (1..items.len()).rev() {
            let other = self.below(last as u64 + 1) as usize;
            items.swap(last, other);
        }
    }

    /// A point inside the unit circle, uniform over its area.
    pub fn in_circle(&mut self) -> [f32; 2] {
        loop {
            let point = [self.range(-1.0, 1.0), self.range(-1.0, 1.0)];
            if point[0] * point[0] + point[1] * point[1] < 1.0 {
                return point;
            }
        }
    }
}

/// Lattice hash of a noise corner, as a value in `0..256`.
fn corner(seed: u64, cell: [i32; 3]) -> u64 {
    let xy = u64::from(cell[0] as u32) | u64::from(cell[1] as u32) << 32;
    RandomSeed(seed).value(xy, u64::from(cell[2] as u32)) >> 56
}

/// Perlin's fade curve, `6t^5 - 15t^4 + 10t^3`.
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Gradient noise in 2D at `point`, between -1 and 1, 0 at whole-number
/// points, and smooth between them: features are about one unit across, so
/// scale `point` to make them larger or smaller.
#[must_use]
pub fn noise2(seed: u64, point: [f32; 2]) -> f32 {
    let base = [point[0].floor(), point[1].floor()];
    let cell = [base[0] as i32, base[1] as i32];
    let local = [point[0] - base[0], point[1] - base[1]];
    let dot = |dx: i32, dy: i32| {
        let (x, y) = (local[0] - dx as f32, local[1] - dy as f32);
        match corner(seed, [cell[0] + dx, cell[1] + dy, 0]) & 7 {
            0 => x + y,
            1 => x - y,
            2 => -x + y,
            3 => -x - y,
            4 => x,
            5 => -x,
            6 => y,
            _ => -y,
        }
    };
    let (u, v) = (fade(local[0]), fade(local[1]));
    let value = lerp(
        lerp(dot(0, 0), dot(1, 0), u),
        lerp(dot(0, 1), dot(1, 1), u),
        v,
    );
    value.clamp(-1.0, 1.0)
}

/// Gradient noise in 3D at `point`, as [`noise2`] is in 2D.
#[must_use]
pub fn noise3(seed: u64, point: [f32; 3]) -> f32 {
    let base = point.map(f32::floor);
    let cell = base.map(|value| value as i32);
    let local = [point[0] - base[0], point[1] - base[1], point[2] - base[2]];
    let dot = |dx: i32, dy: i32, dz: i32| {
        let (x, y, z) = (
            local[0] - dx as f32,
            local[1] - dy as f32,
            local[2] - dz as f32,
        );
        // Perlin's twelve cube-edge gradients; 12-15 repeat four of them.
        match corner(seed, [cell[0] + dx, cell[1] + dy, cell[2] + dz]) & 15 {
            0 | 12 => x + y,
            1 | 13 => -x + y,
            2 => x - y,
            3 => -x - y,
            4 => x + z,
            5 => -x + z,
            6 => x - z,
            7 => -x - z,
            8 => y + z,
            9 | 14 => -y + z,
            10 => y - z,
            _ => -y - z,
        }
    };
    let [u, v, w] = local.map(fade);
    let face = |dz: i32| {
        lerp(
            lerp(dot(0, 0, dz), dot(1, 0, dz), u),
            lerp(dot(0, 1, dz), dot(1, 1, dz), u),
            v,
        )
    };
    lerp(face(0), face(1), w).clamp(-1.0, 1.0)
}

/// Fractal noise: `octaves` layers of [`noise2`], each at twice the
/// frequency and half the strength of the one before, scaled back to
/// between -1 and 1. More octaves add finer detail.
#[must_use]
pub fn fbm2(seed: u64, point: [f32; 2], octaves: u32) -> f32 {
    fractal(octaves, |octave, scale| {
        noise2(seed.wrapping_add(octave), point.map(|value| value * scale))
    })
}

/// Fractal noise of [`noise3`], as [`fbm2`] is of [`noise2`].
#[must_use]
pub fn fbm3(seed: u64, point: [f32; 3], octaves: u32) -> f32 {
    fractal(octaves, |octave, scale| {
        noise3(seed.wrapping_add(octave), point.map(|value| value * scale))
    })
}

fn fractal(octaves: u32, mut layer: impl FnMut(u64, f32) -> f32) -> f32 {
    let (mut sum, mut total, mut strength, mut scale) = (0.0, 0.0, 1.0, 1.0);
    for octave in 0..octaves {
        sum += layer(u64::from(octave), scale) * strength;
        total += strength;
        strength *= 0.5;
        scale *= 2.0;
    }
    if total > 0.0 {
        sum / total
    } else {
        0.0
    }
}

/// Points spread over the rectangle from `[0, 0]` to `size`, no two closer
/// than `spacing` and with no gap where another would fit, in a natural,
/// unclumped pattern (Bridson's Poisson-disk sampling). Use it to place
/// trees, rocks or spawn points. Empty when `spacing` or the size is not
/// above 0.
#[must_use]
pub fn poisson_disk(seed: u64, size: [f32; 2], spacing: f32) -> Vec<[f32; 2]> {
    const TRIES: u32 = 30;
    if !(spacing > 0.0 && size[0] > 0.0 && size[1] > 0.0) {
        return Vec::new();
    }
    let mut random = RandomStream::new(seed);
    // A cell's diagonal is `spacing`, so each cell holds at most one point.
    let cell = spacing / std::f32::consts::SQRT_2;
    let columns = (size[0] / cell).ceil() as usize;
    let rows = (size[1] / cell).ceil() as usize;
    let mut grid = vec![usize::MAX; columns * rows];
    let slot = |point: [f32; 2]| {
        let column = ((point[0] / cell) as usize).min(columns - 1);
        let row = ((point[1] / cell) as usize).min(rows - 1);
        (column, row)
    };
    let first = [random.range(0.0, size[0]), random.range(0.0, size[1])];
    let mut points = vec![first];
    let (column, row) = slot(first);
    grid[row * columns + column] = 0;
    let mut active = vec![0];
    let spacing_squared = spacing * spacing;
    while !active.is_empty() {
        let pick = random.below(active.len() as u64) as usize;
        let center = points[active[pick]];
        let mut placed = false;
        for _ in 0..TRIES {
            // A point in the ring from `spacing` to twice it, by rejection.
            let offset = [
                random.range(-2.0, 2.0) * spacing,
                random.range(-2.0, 2.0) * spacing,
            ];
            let distance = offset[0] * offset[0] + offset[1] * offset[1];
            if distance < spacing_squared || distance > 4.0 * spacing_squared {
                continue;
            }
            let point = [center[0] + offset[0], center[1] + offset[1]];
            if !(0.0..size[0]).contains(&point[0])
                || !(0.0..size[1]).contains(&point[1])
            {
                continue;
            }
            let (column, row) = slot(point);
            let near = (row.saturating_sub(2)..(row + 3).min(rows)).any(|r| {
                (column.saturating_sub(2)..(column + 3).min(columns)).any(|c| {
                    let other = grid[r * columns + c];
                    other != usize::MAX && {
                        let dx = points[other][0] - point[0];
                        let dy = points[other][1] - point[1];
                        dx * dx + dy * dy < spacing_squared
                    }
                })
            });
            if !near {
                grid[row * columns + column] = points.len();
                active.push(points.len());
                points.push(point);
                placed = true;
                break;
            }
        }
        if !placed {
            active.swap_remove(pick);
        }
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FNV-1a over the bit patterns of `values`, to pin outputs exactly.
    fn digest(values: impl IntoIterator<Item = f32>) -> u64 {
        values
            .into_iter()
            .fold(0xCBF2_9CE4_8422_2325, |hash, value| {
                (hash ^ u64::from(value.to_bits()))
                    .wrapping_mul(0x0100_0000_01B3)
            })
    }

    #[test]
    fn a_stream_repeats_for_its_seed_and_stays_in_range() {
        let mut one = RandomStream::from_seed(RandomSeed(7), 3, "level", 1);
        let mut two = RandomStream::from_seed(RandomSeed(7), 3, "level", 1);
        let first: Vec<u64> = (0..8).map(|_| one.next_u64()).collect();
        assert_eq!(first, (0..8).map(|_| two.next_u64()).collect::<Vec<_>>());
        let mut other = RandomStream::from_seed(RandomSeed(7), 4, "level", 1);
        assert_ne!(first[0], other.next_u64(), "the tick changes the stream");

        let mut random = RandomStream::new(1);
        let mut counts = [0_u32; 6];
        for _ in 0..60_000 {
            let unit = random.unit();
            assert!((0.0..1.0).contains(&unit));
            let value = random.range(-3.0, 5.0);
            assert!((-3.0..5.0).contains(&value));
            counts[random.below(6) as usize] += 1;
        }
        assert!(counts.iter().all(|count| (9_500..10_500).contains(count)));
        assert_eq!(random.below(0), 0);
        assert!(!random.chance(0.0));
        assert!(random.chance(1.0));

        let mut picks = [0_u32; 3];
        for _ in 0..3_000 {
            picks[random.weighted(&[1.0, 0.0, 2.0]).unwrap()] += 1;
        }
        assert_eq!(picks[1], 0);
        assert!((850..1_150).contains(&picks[0]), "{picks:?}");
        assert_eq!(random.weighted(&[0.0, -1.0]), None);
        assert_eq!(random.weighted(&[]), None);

        let mut deck: Vec<u32> = (0..52).collect();
        random.shuffle(&mut deck);
        assert_ne!(deck, (0..52).collect::<Vec<_>>());
        deck.sort_unstable();
        assert_eq!(deck, (0..52).collect::<Vec<_>>());
        let [x, y] = random.in_circle();
        assert!(x * x + y * y < 1.0);

        // Pinned so a change to the generator, or a platform that rounds
        // differently, fails here instead of changing saved levels.
        let mut pinned = RandomStream::new(42);
        assert_eq!(
            digest((0..64).map(|_| pinned.unit())),
            6_222_191_704_279_304_085
        );
    }

    #[test]
    fn noise_is_smooth_bounded_and_zero_on_the_lattice() {
        assert_eq!(noise2(5, [3.0, -2.0]), 0.0);
        assert_eq!(noise3(5, [3.0, -2.0, 7.0]), 0.0);
        let mut low = f32::MAX;
        let mut high = f32::MIN;
        for i in 0..4_000 {
            let point = [i as f32 * 0.0731, i as f32 * 0.0419 - 50.0];
            let value = noise2(9, point);
            let nearby = noise2(9, [point[0] + 0.001, point[1]]);
            assert!((value - nearby).abs() < 0.01, "{value} {nearby}");
            low = low.min(value);
            high = high.max(value);
            let value3 = noise3(9, [point[0], point[1], i as f32 * 0.013]);
            assert!((-1.0..=1.0).contains(&value3));
            assert!((-1.0..=1.0).contains(&fbm3(9, [point[0], 1.0, 2.0], 4)));
            assert!((-1.0..=1.0).contains(&fbm2(9, point, 5)));
        }
        assert!(low < -0.4 && high > 0.4, "{low} {high}");
        assert_ne!(noise2(1, [0.5, 0.5]), noise2(2, [0.5, 0.5]));
        assert_eq!(fbm2(1, [0.3, 0.3], 0), 0.0);

        let samples = (0..256).map(|i| i as f32 * 0.137);
        assert_eq!(
            digest(samples.clone().map(|t| noise2(3, [t, -t]))),
            17_524_606_513_887_477_177
        );
        assert_eq!(
            digest(samples.clone().map(|t| noise3(3, [t, t, -t]))),
            9_590_332_238_438_072_754
        );
        assert_eq!(
            digest(samples.map(|t| fbm2(3, [t, 1.0], 6))),
            10_407_434_067_790_441_808
        );
    }

    #[test]
    fn poisson_points_keep_their_spacing_and_fill_the_area() {
        let points = poisson_disk(11, [40.0, 25.0], 2.0);
        assert_eq!(points, poisson_disk(11, [40.0, 25.0], 2.0));
        assert_ne!(points, poisson_disk(12, [40.0, 25.0], 2.0));
        for (index, a) in points.iter().enumerate() {
            assert!((0.0..40.0).contains(&a[0]) && (0.0..25.0).contains(&a[1]));
            for b in &points[index + 1..] {
                let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
                assert!(dx * dx + dy * dy >= 4.0, "{a:?} {b:?}");
            }
        }
        // The densest packing at this spacing fits about 289 points; a
        // finished Poisson-disk set holds roughly half of that.
        assert!(points.len() > 120, "{}", points.len());
        // No hole wide enough for another point: every spot on a grid lies
        // within twice the spacing of some point.
        for x in 0..40 {
            for y in 0..25 {
                let spot = [x as f32, y as f32];
                assert!(points.iter().any(|p| {
                    let (dx, dy) = (p[0] - spot[0], p[1] - spot[1]);
                    dx * dx + dy * dy < 16.0
                }));
            }
        }
        assert!(poisson_disk(1, [10.0, 10.0], 0.0).is_empty());
        assert!(poisson_disk(1, [0.0, 10.0], 1.0).is_empty());
        assert_eq!(
            digest(points.iter().flatten().copied()),
            16_011_008_218_353_132_648
        );
    }
}
