//! The shared GPU broad phase: `src/shaders/compute/broad_phase.comp`, run
//! by [`crate::rendering::gpu_broad_phase::GpuBroadPhase`], finds every
//! overlapping pair among axis-aligned boxes from any solver, and
//! [`pairs_on_cpu`] is its Rust reference.
//!
//! A solver turns what it simulates into [`BroadProxy`] boxes: a rigid
//! body's bounds, a cloth or soft-body particle grown by its radius, a fluid
//! or granular particle. `layer` and `mask` choose which proxies may pair,
//! as for [`super::CollisionLayers`], so one pass serves rigid, deformable,
//! particle and fluid proxies together and each solver reads back the pairs
//! it cares about.
//!
//! The shader hashes uniform grid cells (the cell is the largest box's
//! extent, so one huge box coarsens the grid for all; give such bodies
//! their own pass) into buckets with atomic counts, then each proxy gathers the
//! partners with a higher index into its own row, kept ascending. Atomics
//! only decide the order buckets fill in; a row is the same set in the
//! same order whatever the scheduling, so the output is deterministic.
//! A row holds `capacity` partners; past that it keeps the lowest indices
//! and reports the proxy in [`BroadPairs::overflowed`].

/// An axis-aligned box in the shared broad phase.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BroadProxy {
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Bits this proxy is on.
    pub layer: u32,
    /// Bits of the proxies this one pairs with; a pair needs each side's
    /// layer in the other's mask.
    pub mask: u32,
}

/// Overlapping proxy pairs `[i, j]` with `i < j`, sorted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BroadPairs {
    pub pairs: Vec<[u32; 2]>,
    /// Proxies whose row had more than `capacity` partners, ascending;
    /// their rows kept the lowest partner indices.
    pub overflowed: Vec<u32>,
}

/// Words before the proxies; see the header comment in `broad_phase.comp`.
const HEADER_WORDS: usize = 10;
const PROXY_WORDS: usize = 8;
/// The shader's `local_size_x`; the pass runs in one workgroup.
pub const GPU_BROAD_PHASE_LANES: u32 = 256;
/// Most proxies one pass takes.
pub const MAX_BROAD_PROXIES: usize = 1 << 16;
/// Cell coordinates stay inside this, so the shader's float to int
/// conversion is defined.
const MAX_CELL: f32 = (1 << 24) as f32;

/// Cell of `p` with `inverse` the inverse cell size; the shader's
/// `cell_of`.
fn cell_of(p: [f32; 3], inverse: f32) -> [i32; 3] {
    p.map(|x| (x * inverse).floor() as i32)
}

/// The shader's `bucket_of`.
fn bucket_of(cell: [i32; 3], buckets: u32) -> u32 {
    let [x, y, z] = cell.map(|c| c as u32);
    (x.wrapping_mul(73_856_093)
        ^ y.wrapping_mul(19_349_663)
        ^ z.wrapping_mul(83_492_791))
        & (buckets - 1)
}

/// Buckets a proxy spanning cells `lo..=hi` enters, once each, in the
/// shader's visiting order (x fastest).
fn buckets_of(lo: [i32; 3], hi: [i32; 3], buckets: u32) -> Vec<u32> {
    let mut seen = Vec::new();
    for z in lo[2]..=hi[2] {
        for y in lo[1]..=hi[1] {
            for x in lo[0]..=hi[0] {
                let bucket = bucket_of([x, y, z], buckets);
                if !seen.contains(&bucket) {
                    seen.push(bucket);
                }
            }
        }
    }
    seen
}

/// Packs `proxies` into the word buffer `broad_phase.comp` reads and
/// writes, with rows of `capacity` partners. `None` for more than
/// [`MAX_BROAD_PROXIES`] proxies, a box with a min above its max, or
/// coordinates that are not finite or too far out for the grid.
#[must_use]
pub fn pack(proxies: &[BroadProxy], capacity: u32) -> Option<Vec<u32>> {
    if proxies.len() > MAX_BROAD_PROXIES {
        return None;
    }
    let mut extent = 0.0_f32;
    for proxy in proxies {
        for axis in 0..3 {
            let (lo, hi) = (proxy.min[axis], proxy.max[axis]);
            if !lo.is_finite() || !hi.is_finite() || lo > hi {
                return None;
            }
            extent = extent.max(hi - lo);
        }
    }
    let cell = if extent > 0.0 { extent } else { 1.0 };
    let inverse = 1.0 / cell;
    let count = proxies.len();
    let buckets = (count * 2).next_power_of_two().clamp(64, 1 << 16) as u32;
    let mut items = 0;
    for proxy in proxies {
        let lo = cell_of(proxy.min, inverse);
        let hi = cell_of(proxy.max, inverse);
        let far = |c: [i32; 3]| c.iter().any(|&c| (c as f32).abs() >= MAX_CELL);
        if far(lo) || far(hi) {
            return None;
        }
        items += buckets_of(lo, hi, buckets).len();
    }
    let proxy_base = HEADER_WORDS;
    let starts = proxy_base + count * PROXY_WORDS;
    let cursors = starts + buckets as usize;
    let item_base = cursors + buckets as usize;
    let counts = item_base + items;
    let rows = counts + count;
    let mut words = vec![0; rows + count * capacity as usize];
    words[..HEADER_WORDS].copy_from_slice(&[
        count as u32,
        buckets,
        capacity,
        inverse.to_bits(),
        proxy_base as u32,
        starts as u32,
        cursors as u32,
        item_base as u32,
        counts as u32,
        rows as u32,
    ]);
    for (i, proxy) in proxies.iter().enumerate() {
        let at = proxy_base + i * PROXY_WORDS;
        let [a, b, c] = proxy.min.map(f32::to_bits);
        let [d, e, f] = proxy.max.map(f32::to_bits);
        words[at..at + PROXY_WORDS].copy_from_slice(&[
            a,
            b,
            c,
            d,
            e,
            f,
            proxy.layer,
            proxy.mask,
        ]);
    }
    Some(words)
}

/// The pairs in words a pass finished.
#[must_use]
pub fn unpack(words: &[u32]) -> BroadPairs {
    let [count, capacity, counts, rows] =
        [0, 2, 8, 9].map(|index| words[index] as usize);
    let mut out = BroadPairs::default();
    for i in 0..count {
        let found = words[counts + i] as usize;
        if found > capacity {
            out.overflowed.push(i as u32);
        }
        let row = rows + i * capacity;
        out.pairs.extend(
            words[row..row + found.min(capacity)]
                .iter()
                .map(|&j| [i as u32, j]),
        );
    }
    out
}

/// What the pass gives for `proxies`, by testing every pair.
#[must_use]
pub fn pairs_on_cpu(proxies: &[BroadProxy], capacity: u32) -> BroadPairs {
    let mut out = BroadPairs::default();
    for (i, a) in proxies.iter().enumerate() {
        let mut found = 0;
        for (j, b) in proxies.iter().enumerate().skip(i + 1) {
            let overlap = (0..3).all(|axis| {
                a.min[axis] <= b.max[axis] && b.min[axis] <= a.max[axis]
            });
            if overlap && a.layer & b.mask != 0 && b.layer & a.mask != 0 {
                if found < capacity {
                    out.pairs.push([i as u32, j as u32]);
                }
                found += 1;
            }
        }
        if found > capacity {
            out.overflowed.push(i as u32);
        }
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `count` boxes from a fixed generator: specks, particles, cloth-sized
    /// boxes and a few large rigid bodies, on both sides of the origin.
    pub(crate) fn scattered(count: usize) -> Vec<BroadProxy> {
        let mut state = 0x9E37_79B9_u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as f32 / u32::MAX as f32
        };
        (0..count)
            .map(|i| {
                let size = match i % 7 {
                    0 => 1.5,
                    1 | 2 => 0.02,
                    3 => 0.0,
                    _ => 0.3,
                } * (0.5 + next());
                let centre = [0, 1, 2].map(|_| next() * 12.0 - 6.0);
                BroadProxy {
                    min: centre.map(|c| c - size),
                    max: centre.map(|c| c + size),
                    layer: 1 << (i % 3),
                    mask: if i % 5 == 0 { 0b011 } else { u32::MAX },
                }
            })
            .collect()
    }

    #[test]
    fn the_reference_pairs_overlapping_boxes_whose_layers_meet() {
        let at = |x: f32, layer, mask| BroadProxy {
            min: [x, 0.0, 0.0],
            max: [x + 1.0, 1.0, 1.0],
            layer,
            mask,
        };
        let proxies = [
            at(0.0, 1, u32::MAX),
            at(0.5, 1, u32::MAX),
            // Touching faces count; this one also meets the next.
            at(1.5, 2, u32::MAX),
            at(2.5, 1, 1),
            at(9.0, 1, u32::MAX),
        ];
        let out = pairs_on_cpu(&proxies, 8);
        // [2, 3] fails: 3's mask leaves out 2's layer.
        assert_eq!(out.pairs, vec![[0, 1], [1, 2]]);
        assert!(out.overflowed.is_empty());
        let capped = pairs_on_cpu(&proxies, 0);
        assert!(capped.pairs.is_empty());
        assert_eq!(capped.overflowed, vec![0, 1]);
    }

    #[test]
    fn packing_refuses_bad_boxes_and_sizes_its_buffers() {
        let good = scattered(100);
        let words = pack(&good, 4).unwrap();
        assert_eq!(words[0], 100);
        assert!(words[1].is_power_of_two());
        // Nothing ran yet, so every row is empty.
        assert_eq!(unpack(&words), BroadPairs::default());
        let mut inverted = good.clone();
        inverted[3].min[1] = inverted[3].max[1] + 1.0;
        assert!(pack(&inverted, 4).is_none());
        let mut nan = good.clone();
        nan[7].max[0] = f32::NAN;
        assert!(pack(&nan, 4).is_none());
        let mut far = good;
        // A speck far out: the cell stays small, so its cell is too far.
        far[0].min = [1e30; 3];
        far[0].max = [1e30; 3];
        assert!(pack(&far, 4).is_none());
    }

    #[test]
    fn a_proxy_enters_each_bucket_once() {
        let buckets = buckets_of([-1, -1, -1], [1, 1, 1], 64);
        let mut sorted = buckets.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), buckets.len());
    }
}
