# Procedural content

`rusting_engine::procgen` generates content from a seed: random streams,
gradient noise and evenly spread points. The same seed gives the same
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

## Same on every machine

The generators use integer hashing and the floating-point operations that
round the same way on every supported CPU (`+ - * /`, `floor`, `sqrt`),
and no `sin`, `exp` or `powf`, whose last bits differ between math
libraries. Unit tests pin the exact output bits of each generator, so a
change that would alter saved levels fails the build. They run on the CPU;
do not port them to a shader and expect the same bits.
