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

/// Index of the cell at `[x, y, z]` in a grid of `size`, as the grid
/// generators lay cells out: `x` fastest, then `y`, then `z`.
#[must_use]
pub fn cell_index(size: [usize; 3], cell: [usize; 3]) -> usize {
    cell[0] + size[0] * (cell[1] + size[1] * cell[2])
}

/// The rows of a [`crate::runtime::TileMap`] for a 2D grid `width` cells
/// wide, with `tile` choosing each cell's character.
#[must_use]
pub fn tile_rows<T>(
    cells: &[T],
    width: usize,
    tile: impl Fn(&T) -> char,
) -> Vec<String> {
    cells
        .chunks(width.max(1))
        .map(|row| row.iter().map(&tile).collect())
        .collect()
}

/// Caves by cellular automaton: cells start as wall with chance `fill`
/// (about 0.45 makes open caves), then each of `steps` passes turns a cell
/// to wall when most of its neighbours are walls. Returns `true` for wall,
/// laid out as [`cell_index`] says. A `size` with depth 1 is a 2D map
/// with 8 neighbours per cell; deeper grids use all 26. Cells outside the
/// grid count as walls, so the caves close at the edges.
#[must_use]
pub fn cellular_caves(
    seed: u64,
    size: [usize; 3],
    fill: f32,
    steps: u32,
) -> Vec<bool> {
    let mut random = RandomStream::new(seed);
    let count = size[0] * size[1] * size[2];
    let mut walls: Vec<bool> =
        (0..count).map(|_| random.chance(fill)).collect();
    let depth = if size[2] > 1 { 1 } else { 0 };
    let neighbours = if depth == 1 { 26 } else { 8 };
    // 2D: wall with 5 or more wall neighbours, or 4 if already a wall (the
    // classic 4-5 rule); 3D scales it to 14 and 13 of 26.
    let (born, stays) = (neighbours / 2 + 1, neighbours / 2);
    for _ in 0..steps {
        let mut next = walls.clone();
        for z in 0..size[2] {
            for y in 0..size[1] {
                for x in 0..size[0] {
                    let mut around = 0;
                    for dz in -depth..=depth {
                        for dy in -1..=1_i64 {
                            for dx in -1..=1_i64 {
                                if (dx, dy, dz) == (0, 0, 0) {
                                    continue;
                                }
                                let at = [
                                    x as i64 + dx,
                                    y as i64 + dy,
                                    z as i64 + dz,
                                ];
                                let inside = (0..3).all(|axis| {
                                    (0..size[axis] as i64).contains(&at[axis])
                                });
                                around += usize::from(
                                    !inside
                                        || walls[cell_index(
                                            size,
                                            at.map(|value| value as usize),
                                        )],
                                );
                            }
                        }
                    }
                    let index = cell_index(size, [x, y, z]);
                    next[index] =
                        around >= born || (walls[index] && around >= stays);
                }
            }
        }
        walls = next;
    }
    walls
}

/// A room of a [`RoomLayout`]: its corner cell and its size in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Room {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Room {
    /// The room's middle cell.
    #[must_use]
    pub fn center(&self) -> [usize; 2] {
        [self.x + self.width / 2, self.y + self.height / 2]
    }
}

/// A dungeon from [`rooms_and_corridors`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomLayout {
    /// `true` for floor, `width` cells to a row.
    pub floor: Vec<bool>,
    /// The rooms, in the order corridors join them: each one to the one
    /// before.
    pub rooms: Vec<Room>,
}

/// A dungeon of up to `rooms` rectangular rooms, each `room_size[0]` to
/// `room_size[1]` cells on a side, placed without touching inside a
/// `size[0]` by `size[1]` map with a wall border, and joined by L-shaped
/// corridors so every floor cell can reach every other. Fewer rooms are
/// placed when the map runs out of space.
#[must_use]
pub fn rooms_and_corridors(
    seed: u64,
    size: [usize; 2],
    rooms: usize,
    room_size: [usize; 2],
) -> RoomLayout {
    const TRIES: usize = 50;
    let mut random = RandomStream::new(seed);
    let [width, height] = size;
    let mut floor = vec![false; width * height];
    let mut placed: Vec<Room> = Vec::new();
    let side = |random: &mut RandomStream| {
        let span = room_size[1].saturating_sub(room_size[0]) as u64 + 1;
        room_size[0].max(1) + random.below(span) as usize
    };
    for _ in 0..rooms * TRIES {
        if placed.len() == rooms {
            break;
        }
        let (room_width, room_height) = (side(&mut random), side(&mut random));
        // Keep a one-cell wall border around the map.
        if room_width + 2 > width || room_height + 2 > height {
            continue;
        }
        let room = Room {
            x: 1 + random.below((width - room_width - 1) as u64) as usize,
            y: 1 + random.below((height - room_height - 1) as u64) as usize,
            width: room_width,
            height: room_height,
        };
        // Rooms keep at least one wall cell between them.
        let touches = placed.iter().any(|other| {
            room.x <= other.x + other.width
                && other.x <= room.x + room.width
                && room.y <= other.y + other.height
                && other.y <= room.y + room.height
        });
        if touches {
            continue;
        }
        for y in room.y..room.y + room.height {
            floor[y * width + room.x..y * width + room.x + room.width]
                .fill(true);
        }
        if let Some(last) = placed.last() {
            let ([x0, y0], [x1, y1]) = (last.center(), room.center());
            let corner = if random.chance(0.5) {
                [x1, y0]
            } else {
                [x0, y1]
            };
            for (from, to) in [([x0, y0], corner), (corner, [x1, y1])] {
                for y in from[1].min(to[1])..=from[1].max(to[1]) {
                    for x in from[0].min(to[0])..=from[0].max(to[0]) {
                        floor[y * width + x] = true;
                    }
                }
            }
        }
        placed.push(room);
    }
    RoomLayout {
        floor,
        rooms: placed,
    }
}

/// Fills a grid of `size` with tiles by wave function collapse: each cell
/// takes one of the tiles `0..weights.len()`, picked with a chance of its
/// weight, so that every pair of neighbours fits. `fits(a, b, axis)` says
/// whether tile `b` may sit one cell after tile `a` along `axis` (0 is x,
/// 1 is y, 2 is z). The result is laid out as [`cell_index`] says.
///
/// `None` when the rules cannot be met: after a contradiction the search
/// starts over from the next random stream, up to 20 times. At most 64
/// tiles; more give `None`.
#[must_use]
pub fn wave_function_collapse(
    seed: u64,
    size: [usize; 3],
    weights: &[f32],
    fits: impl Fn(usize, usize, usize) -> bool,
) -> Option<Vec<usize>> {
    const ATTEMPTS: u64 = 20;
    let tiles = weights.len();
    if tiles == 0 || tiles > 64 {
        return None;
    }
    let all = if tiles == 64 {
        u64::MAX
    } else {
        (1 << tiles) - 1
    };
    // after[axis][a]: tiles that may follow `a` along +axis; before: -axis.
    let mask = |fits_pair: &dyn Fn(usize) -> bool| {
        (0..tiles)
            .filter(|&tile| fits_pair(tile))
            .fold(0_u64, |mask, tile| mask | 1 << tile)
    };
    let after: [[u64; 64]; 3] = std::array::from_fn(|axis| {
        std::array::from_fn(|a| mask(&|b| a < tiles && fits(a, b, axis)))
    });
    let before: [[u64; 64]; 3] = std::array::from_fn(|axis| {
        std::array::from_fn(|b| mask(&|a| b < tiles && fits(a, b, axis)))
    });
    let count = size[0] * size[1] * size[2];
    let neighbour = |index: usize, axis: usize, forward: bool| {
        let cell = [
            index % size[0],
            index / size[0] % size[1],
            index / (size[0] * size[1]),
        ];
        if forward {
            (cell[axis] + 1 < size[axis])
                .then(|| index + [1, size[0], size[0] * size[1]][axis])
        } else {
            (cell[axis] > 0)
                .then(|| index - [1, size[0], size[0] * size[1]][axis])
        }
    };
    'attempt: for attempt in 0..ATTEMPTS {
        let mut random = RandomStream::new(RandomSeed(seed).value(attempt, 0));
        let mut options = vec![all; count];
        // The open cell with the fewest options, lowest index first.
        while let Some(cell) = (0..count)
            .filter(|&cell| options[cell].count_ones() > 1)
            .min_by_key(|&cell| options[cell].count_ones())
        {
            let mut weights_here = [0.0; 64];
            for tile in 0..tiles {
                if options[cell] & 1 << tile != 0 {
                    weights_here[tile] = weights[tile];
                }
            }
            let tile = random
                .weighted(&weights_here[..tiles])
                .unwrap_or(options[cell].trailing_zeros() as usize);
            options[cell] = 1 << tile;
            // Narrow the neighbours until nothing changes.
            let mut changed = vec![cell];
            while let Some(index) = changed.pop() {
                for axis in 0..3 {
                    for forward in [true, false] {
                        let Some(other) = neighbour(index, axis, forward)
                        else {
                            continue;
                        };
                        let table = if forward { &after } else { &before };
                        let allowed = (0..tiles)
                            .filter(|tile| options[index] & 1 << tile != 0)
                            .fold(0, |mask, tile| mask | table[axis][tile]);
                        let narrowed = options[other] & allowed;
                        if narrowed == 0 {
                            continue 'attempt;
                        }
                        if narrowed != options[other] {
                            options[other] = narrowed;
                            changed.push(other);
                        }
                    }
                }
            }
        }
        // A cell can start with one option; check it against its
        // neighbours once, since nothing propagated from it.
        let valid = (0..count).all(|index| {
            options[index] != 0
                && (0..3).all(|axis| {
                    neighbour(index, axis, true).is_none_or(|other| {
                        let a = options[index].trailing_zeros() as usize;
                        let b = options[other].trailing_zeros() as usize;
                        fits(a, b, axis)
                    })
                })
        });
        if valid {
            return Some(
                options
                    .iter()
                    .map(|option| option.trailing_zeros() as usize)
                    .collect(),
            );
        }
    }
    None
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

    #[test]
    fn caves_smooth_into_open_areas_in_2d_and_3d() {
        let size = [50, 30, 1];
        let walls = cellular_caves(4, size, 0.45, 4);
        assert_eq!(walls, cellular_caves(4, size, 0.45, 4));
        assert_ne!(walls, cellular_caves(5, size, 0.45, 4));
        let share = walls.iter().filter(|wall| **wall).count() as f32 / 1500.0;
        assert!((0.3..0.75).contains(&share), "{share}");
        // Smoothing leaves no lone wall cell in open floor.
        for y in 1..29 {
            for x in 1..49 {
                if walls[cell_index(size, [x, y, 0])] {
                    let around = (y - 1..=y + 1)
                        .flat_map(|ny| (x - 1..=x + 1).map(move |nx| (nx, ny)))
                        .filter(|&(nx, ny)| {
                            walls[cell_index(size, [nx, ny, 0])]
                        })
                        .count();
                    assert!(around > 1, "lone wall at {x},{y}");
                }
            }
        }
        assert!(cellular_caves(1, size, 1.0, 3).iter().all(|wall| *wall));
        let rows = tile_rows(&walls, 50, |wall| if *wall { '#' } else { '.' });
        assert_eq!(rows.len(), 30);
        assert!(rows.iter().all(|row| row.len() == 50));

        let solid = cellular_caves(4, [12, 12, 12], 0.45, 3);
        assert_eq!(solid.len(), 1728);
        let share = solid.iter().filter(|wall| **wall).count() as f32 / 1728.0;
        assert!((0.2..0.9).contains(&share), "{share}");
        assert_eq!(
            digest(walls.iter().map(|wall| f32::from(u8::from(*wall)))),
            16_149_778_793_422_775_509
        );
    }

    #[test]
    fn rooms_do_not_touch_and_corridors_join_every_floor_cell() {
        let layout = rooms_and_corridors(8, [60, 40], 8, [4, 9]);
        assert_eq!(layout, rooms_and_corridors(8, [60, 40], 8, [4, 9]));
        assert_eq!(layout.rooms.len(), 8);
        for (index, room) in layout.rooms.iter().enumerate() {
            assert!(
                (4..=9).contains(&room.width) && (4..=9).contains(&room.height)
            );
            for y in room.y..room.y + room.height {
                for x in room.x..room.x + room.width {
                    assert!(layout.floor[y * 60 + x]);
                }
            }
            for other in &layout.rooms[index + 1..] {
                assert!(
                    room.x > other.x + other.width
                        || other.x > room.x + room.width
                        || room.y > other.y + other.height
                        || other.y > room.y + room.height,
                    "{room:?} touches {other:?}"
                );
            }
        }
        for x in 0..60 {
            assert!(!layout.floor[x] && !layout.floor[39 * 60 + x]);
        }
        // Flood fill from the first room reaches every floor cell.
        let start = layout.rooms[0].center();
        let mut seen = vec![false; 60 * 40];
        let mut stack = vec![start[1] * 60 + start[0]];
        seen[stack[0]] = true;
        while let Some(cell) = stack.pop() {
            for next in [cell - 1, cell + 1, cell - 60, cell + 60] {
                if layout.floor[next] && !seen[next] {
                    seen[next] = true;
                    stack.push(next);
                }
            }
        }
        assert_eq!(seen, layout.floor);
        // A map too small for any room places none.
        assert!(rooms_and_corridors(1, [5, 5], 3, [4, 4]).rooms.is_empty());
    }

    #[test]
    fn collapse_meets_the_rules_in_2d_and_3d() {
        // Along x tiles alternate, along y they repeat: vertical stripes.
        let stripes = |a: usize, b: usize, axis: usize| match axis {
            0 => a != b,
            1 => a == b,
            _ => true,
        };
        let size = [12, 8, 1];
        let cells =
            wave_function_collapse(3, size, &[1.0, 1.0], stripes).unwrap();
        assert_eq!(
            cells,
            wave_function_collapse(3, size, &[1.0, 1.0], stripes).unwrap()
        );
        for y in 0..8 {
            for x in 0..12 {
                let tile = cells[cell_index(size, [x, y, 0])];
                assert_eq!(tile, (cells[0] + x) % 2, "{x},{y}");
            }
        }

        // Land (0), coast (1) and sea (2): land never touches sea.
        let shore = |a: usize, b: usize, _: usize| a.abs_diff(b) < 2;
        let size = [10, 10, 4];
        let map =
            wave_function_collapse(9, size, &[1.0, 0.5, 1.0], shore).unwrap();
        for z in 0..4 {
            for y in 0..10 {
                for x in 0..10 {
                    let tile = map[cell_index(size, [x, y, z])];
                    for (axis, next) in
                        [[x + 1, y, z], [x, y + 1, z], [x, y, z + 1]]
                            .into_iter()
                            .enumerate()
                    {
                        if next[axis] < size[axis] {
                            assert!(shore(
                                tile,
                                map[cell_index(size, next)],
                                axis
                            ));
                        }
                    }
                }
            }
        }
        assert!((0..3).all(|tile| map.contains(&tile)), "{map:?}");

        let never = |_: usize, _: usize, _: usize| false;
        assert_eq!(wave_function_collapse(1, [2, 1, 1], &[1.0], never), None);
        assert_eq!(
            wave_function_collapse(1, [1, 1, 1], &[1.0; 65], |_, _, _| true),
            None
        );
        assert_eq!(
            digest(map.iter().map(|tile| *tile as f32)),
            3_834_443_859_693_355_621
        );
    }
}
