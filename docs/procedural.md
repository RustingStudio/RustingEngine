# Procedural content

`rusting_engine::procgen` generates content from a seed: random streams,
gradient noise, evenly spread points, and tile maps and 3D grids, and
`rusting scene generate` keeps scenes made by your own generators up to date. The same seed gives the same
result on every machine, so a generated level can be saved as its seed,
shared between players, or checked in a scenario.

## Random streams

A `RandomStream` draws a sequence of values from one seed. Take the seed
from the run's `RandomSeed`, so `rusting run --seed N` and scenario seeds
change the level too:

```rust
use rusting_engine::procgen::RandomStream;
use rusting_engine::runtime::RandomSeed;

fn rooms(seed: RandomSeed) -> Vec<[f32; 2]> {
    // Tick 0: the level is made once, before play starts.
    let mut random = RandomStream::from_seed(seed, 0, "rooms", 0);
    let count = 4 + random.below(5); // 4 to 8 rooms
    (0..count)
        .map(|_| [random.range(0.0, 100.0), random.range(0.0, 60.0)])
        .collect()
}
```

- `unit()` is in `[0, 1)`, `range(min, max)` in `[min, max)`, and
  `below(n)` a whole number in `0..n`.
- `chance(p)` is `true` with probability `p`.
- `weighted(&weights)` picks an index with a chance of its weight over the
  total, and `shuffle(&mut items)` reorders a slice.
- `in_circle()` is a point inside the unit circle.

Give each generator its own stream (its own subsystem name or key). Two
generators sharing a stream change each other's output when either draws
a different number of values. Inside the fixed tick, gameplay code can
keep using `GameScene::random(stream)`; streams are for generators that
draw many values at once.

## Noise

`noise2(seed, [x, y])` and `noise3(seed, [x, y, z])` are gradient (Perlin)
noise: smooth values between -1 and 1 that are 0 at whole-number points.
Features are about one unit across, so scale the point to change their
size. `fbm2` and `fbm3` add `octaves` layers, each twice as fine and half
as strong, for terrain-like detail:

```rust
use rusting_engine::procgen::fbm2;

let height = |x: f32, z: f32| 8.0 * fbm2(seed, [x * 0.02, z * 0.02], 5);
```

## Spreading points

`poisson_disk(seed, [width, depth], spacing)` places points over a
rectangle, no two closer than `spacing` and with no gap where another
would fit. Use it for trees, rocks and pickups that should look scattered
but not clumped.

## Tile and grid generators

The grid generators return one value per cell, `x` fastest, then `y`,
then `z`; `cell_index(size, [x, y, z])` finds a cell. A `size` with depth
1, such as `[40, 30, 1]`, is a 2D map. `tile_rows` turns a 2D result into
the `rows` of a `TileMap`:

```rust
use rusting_engine::procgen::{cellular_caves, tile_rows};

let walls = cellular_caves(seed, [40, 30, 1], 0.45, 4);
tile_map.rows = tile_rows(&walls, 40, |wall| if *wall { '#' } else { '.' });
```

- `cellular_caves(seed, size, fill, steps)` makes caves: cells start as
  wall with chance `fill` (about 0.45), then each step turns a cell to wall
  when most of its neighbours are walls. The edges count as walls, so caves
  close at the border. A 3D size makes caverns using all 26 neighbours.
  Caves can come out as separate pockets; flood-fill from the start and
  keep the largest one when the whole map must connect.
- `rooms_and_corridors(seed, [width, height], rooms, [min, max])` places up
  to `rooms` rectangular rooms that never touch, inside a wall border, and
  joins each one to the one placed before it with an L-shaped corridor, so
  every floor cell connects. It returns the `floor` cells and the `rooms`
  (corner, size and `center()`), for placing the player in the first room
  and the exit in the last.
- `wave_function_collapse(seed, size, &weights, fits)` fills a 2D or 3D
  grid with tiles `0..weights.len()` (at most 64) so that every pair of
  neighbours fits. `fits(a, b, axis)` says whether tile `b` may sit one
  cell after tile `a` along `axis` (0 x, 1 y, 2 z), and `weights` makes
  some tiles more common. It returns `None` when the rules cannot be met,
  after 20 tries from different streams.

```rust
use rusting_engine::procgen::wave_function_collapse;

// Land (0), coast (1) and sea (2): land never touches sea.
let shore = |a: usize, b: usize, _axis: usize| a.abs_diff(b) < 2;
let tiles = wave_function_collapse(seed, [32, 32, 1], &[1.0, 0.5, 1.0], shore)
    .expect("these rules always fit");
```

## Generated scenes

A generator can also be a scene file the project keeps up to date. List it
under `scene_generators` in `project.json`:

```json
"scene_generators": {
  "level": {
    "command": ["cargo", "run", "-q", "--bin", "gen_level"],
    "output": "scenes/level.rscene",
    "seed": 7,
    "inputs": {"size": 24, "rooms": 5},
    "watch": ["src/bin"]
  }
}
```

`rusting scene generate .` runs the command from the project root, without
a shell, with `RUSTING_SEED`, `RUSTING_INPUTS` (the inputs as JSON) and
`RUSTING_OUTPUT`, the path to write the scene to. The program, usually a
binary in the game crate, reads them with `GeneratorRequest::from_env()`,
builds the level with the generators above, and writes a normal scene, for
example with `save_scene`. The engine loads the scene before it replaces the
old one, so a broken generator leaves the last good scene in place, and the
write goes into the operation journal like any edit (`rusting revert`
undoes it).

The result is cached by the command, seed, inputs, and the contents of the
`watch` files and folders: a second run reports `cached` and runs nothing
until one of them changes. A scene edited by hand since it was generated is
refused with `GENERATED_SCENE_EDITED`; `--force` regenerates it anyway, and
also reruns unchanged generators. Instance the generated scene into another
one to use it as a prefab.

## Same on every machine

The generators use integer hashing and the floating-point operations that
round the same way on every supported CPU (`+ - * /`, `floor`, `sqrt`),
and no `sin`, `exp` or `powf`, whose last bits differ between math
libraries. Unit tests pin the exact output bits of each generator, so a
change that would alter saved levels fails the build. They run on the CPU;
do not port them to a shader and expect the same bits.
