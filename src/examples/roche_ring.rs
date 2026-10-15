//! A moon that crossed its planet's Roche limit is torn apart by tides and
//! settles into a thin ring.
//!
//! ```sh
//! cargo run --release --features window --example roche_ring
//! ```
//!
//! `--rocks 80000` sets the rock count and `--portrait` pulls the camera
//! back for a 9:16 frame. The run is simulated first on the CPU (about
//! 12 minutes for 80,000 rocks on 12 threads; the window shows the newest
//! simulated frame meanwhile), then replays from the start at 60 fps on a
//! fixed 32 s timeline: start the screen recording when the replay begins
//! ("baked in ... s; playing" prints). Sim time
//! runs slowly through the breakup and speeds up to a few orbits a second
//! while the ring flattens.
//!
//! Physics, in one fixed-step leapfrog (kick-drift-kick) integrator:
//! - The planet is a point mass. A rock that reaches its surface is removed.
//! - Self-gravity between rocks: a Barnes-Hut octree (θ = 0.7) with Plummer
//!   softening of one rock diameter.
//! - Contacts: soft spheres with a spring and a damper along the line of
//!   centres, tuned to a restitution of 0.3, found through a spatial hash.
//!   Every collision takes some up-and-down motion out of the debris, while
//!   the orbital angular momentum stays, so the ring ends up flat.
//!
//! The moon starts at 0.8 times the Roche distance, taken here as the
//! distance where a tidally locked rubble pile just fills its Hill sphere:
//! d = R (3M/m)^(1/3). It turns once per orbit, like a tidally locked moon.
//! At the end the example prints the ring's size and thickness.

use rayon::prelude::*;
use rusting_engine::prelude::*;
use rusting_engine::runtime::{Bloom, DirectionalLight, MaterialOverride};
use std::cell::RefCell;
use std::f32::consts::{PI, TAU};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const PLANET_RADIUS: f32 = 10.0;
const MOON_RADIUS: f32 = 2.5;
/// Planet mass over moon mass.
const MASS_RATIO: f32 = 333.0;
/// Start distance as a share of the Roche distance.
const START_SHARE: f32 = 0.8;
const RESTITUTION: f32 = 0.3;
/// Barnes-Hut opening angle.
const THETA: f32 = 0.7;
/// Largest number of rocks in an octree leaf.
const LEAF: usize = 8;
/// Contact overlap, as a share of a rock radius, under the weight at the
/// middle of the starting moon. Sets the contact stiffness.
const OVERLAP: f32 = 0.15;
/// Steps per contact, so the spring is resolved.
const STEPS_PER_CONTACT: f32 = 8.0;
/// Self-gravity changes slowly next to the contacts, so it is recomputed
/// only every this many steps.
const GRAVITY_EVERY: usize = 4;
/// Sim time before the first frame, in orbits: the lattice the moon starts
/// as settles, and the stretching has begun when the video opens.
const PREROLL: f32 = 0.04;
const FPS: f32 = 60.0;
const VIDEO_SECONDS: f32 = 32.0;
/// Recorded positions are i16 steps of this many units: ±65 units.
const QUANTUM: f32 = 0.002;
/// Sim speed over the video: (video second, orbits per video second),
/// linear in between.
const SPEED: [(f32, f32); 6] = [
    (0.0, 0.05),
    (7.0, 0.05),
    (14.0, 0.35),
    (19.0, 1.2),
    (25.0, 1.2),
    (29.0, 0.4),
];
/// Shown rocks are this much bigger than their contact spheres, so they
/// still read as dots in the wide shots.
const VISUAL_SCALE: f32 = 1.2;
const DASHES: usize = 72;

const PLANET_SHADER: &str = r#"#rusting surface 1
void surface(SurfaceInput i, inout Surface s) {
    // Latitude bands, wobbling a little with longitude.
    vec3 n = normalize(i.world_position);
    float longitude = atan(n.z, n.x);
    float lat = n.y + 0.012 * sin(longitude * 3.0 + n.y * 20.0);
    float broad = 0.5 + 0.5 * sin(lat * 23.0 + 1.7 * sin(lat * 7.0));
    float fine = 0.5 + 0.5 * sin(lat * 95.0 + 2.0 * sin(lat * 13.0));
    vec3 cream = vec3(0.86, 0.74, 0.52);
    vec3 tan_band = vec3(0.62, 0.43, 0.26);
    vec3 pale = vec3(0.95, 0.9, 0.78);
    vec3 color = mix(tan_band, cream, broad);
    color = mix(color, pale, 0.25 * fine);
    // Darker, bluer poles.
    color = mix(color, vec3(0.35, 0.38, 0.45), smoothstep(0.75, 0.98, abs(n.y)));
    s.base_color.rgb = color;
    s.roughness = 0.95;
    s.metallic = 0.0;
}
"#;

fn flag_value(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1).cloned())
}

fn rock_count() -> usize {
    flag_value("--rocks")
        .map_or(80_000, |count| count.parse().expect("--rocks"))
}

fn portrait() -> bool {
    std::env::args().any(|arg| arg == "--portrait")
}

fn roche_distance() -> f32 {
    MOON_RADIUS * (3.0 * MASS_RATIO).cbrt()
}

fn start_distance() -> f32 {
    START_SHARE * roche_distance()
}

/// Video frames in the timeline.
fn frame_count() -> usize {
    (VIDEO_SECONDS * FPS) as usize
}

fn orbits_per_second(t: f32) -> f32 {
    let (last_t, last_speed) = SPEED[SPEED.len() - 1];
    if t >= last_t {
        return last_speed;
    }
    SPEED
        .windows(2)
        .find(|pair| t < pair[1].0)
        .map_or(SPEED[0].1, |pair| {
            let [(t0, s0), (t1, s1)] = [pair[0], pair[1]];
            s0 + (s1 - s0) * ((t - t0) / (t1 - t0)).max(0.0)
        })
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// A fixed value in [0, 1) for one index and channel.
fn noise(index: usize, channel: u64) -> f32 {
    let mut value = (index as u64) ^ (channel << 56);
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    (value >> 40) as f32 / (1u64 << 24) as f32
}

fn smooth(k: f32) -> f32 {
    let k = k.clamp(0.0, 1.0);
    k * k * (3.0 - 2.0 * k)
}

/// The moon as rocks on a cubic lattice: offsets from its centre, closest
/// first, and the lattice spacing.
fn moon_lattice(count: usize) -> (Vec<[f32; 3]>, f32) {
    let spacing = MOON_RADIUS * (4.0 * PI / (3.0 * count as f32)).cbrt();
    let reach = (1.15 * MOON_RADIUS / spacing).ceil() as i32;
    let mut points = Vec::new();
    for x in -reach..=reach {
        for y in -reach..=reach {
            for z in -reach..=reach {
                points.push([x, y, z].map(|axis| axis as f32 * spacing));
            }
        }
    }
    points.sort_by(|a, b| {
        dot(*a, *a)
            .total_cmp(&dot(*b, *b))
            .then(a[0].total_cmp(&b[0]))
            .then(a[1].total_cmp(&b[1]))
            .then(a[2].total_cmp(&b[2]))
    });
    assert!(points.len() >= count, "lattice too small");
    points.truncate(count);
    (points, spacing)
}

const NONE: u32 = u32::MAX;

#[derive(Clone, Copy)]
struct Node {
    centre_of_mass: [f32; 3],
    mass: f32,
    /// Squared edge length of the node's cube.
    size2: f32,
    start: u32,
    end: u32,
    children: [u32; 8],
}

/// Barnes-Hut octree over some of the rocks.
struct Octree {
    nodes: Vec<Node>,
    order: Vec<u32>,
}

impl Octree {
    fn new(x: &[[f32; 3]], live: &[u32]) -> Self {
        let mut low = [f32::MAX; 3];
        let mut high = [f32::MIN; 3];
        for &i in live {
            for axis in 0..3 {
                low[axis] = low[axis].min(x[i as usize][axis]);
                high[axis] = high[axis].max(x[i as usize][axis]);
            }
        }
        let centre = std::array::from_fn(|axis| 0.5 * (low[axis] + high[axis]));
        let half = (0..3)
            .map(|axis| 0.5 * (high[axis] - low[axis]))
            .fold(0.0_f32, f32::max)
            * 1.001
            + 1e-3;
        let mut tree = Self {
            nodes: Vec::with_capacity(live.len() / 2),
            order: live.to_vec(),
        };
        let mut order = std::mem::take(&mut tree.order);
        tree.build(x, &mut order, 0, centre, half);
        tree.order = order;
        tree
    }

    fn build(
        &mut self,
        x: &[[f32; 3]],
        order: &mut [u32],
        offset: usize,
        centre: [f32; 3],
        half: f32,
    ) -> u32 {
        let mut sum = [0.0_f64; 3];
        for &i in order.iter() {
            for axis in 0..3 {
                sum[axis] += f64::from(x[i as usize][axis]);
            }
        }
        let mass = order.len() as f32;
        let index = self.nodes.len();
        self.nodes.push(Node {
            centre_of_mass: sum.map(|s| (s / order.len() as f64) as f32),
            mass,
            size2: 4.0 * half * half,
            start: offset as u32,
            end: (offset + order.len()) as u32,
            children: [NONE; 8],
        });
        if order.len() <= LEAF || half < 1e-3 {
            return index as u32;
        }
        let octant = |i: u32| {
            let p = x[i as usize];
            usize::from(p[0] >= centre[0])
                | usize::from(p[1] >= centre[1]) << 1
                | usize::from(p[2] >= centre[2]) << 2
        };
        // A stable counting partition into the eight octants.
        let mut counts = [0usize; 8];
        for &i in order.iter() {
            counts[octant(i)] += 1;
        }
        let mut starts = [0usize; 9];
        for oct in 0..8 {
            starts[oct + 1] = starts[oct] + counts[oct];
        }
        let mut next = starts;
        let copy = order.to_vec();
        for i in copy {
            let oct = octant(i);
            order[next[oct]] = i;
            next[oct] += 1;
        }
        let quarter = 0.5 * half;
        for oct in 0..8 {
            let (from, to) = (starts[oct], starts[oct + 1]);
            if from == to {
                continue;
            }
            let child_centre = std::array::from_fn(|axis| {
                centre[axis]
                    + if oct >> axis & 1 == 1 {
                        quarter
                    } else {
                        -quarter
                    }
            });
            let child = self.build(
                x,
                &mut order[from..to],
                offset + from,
                child_centre,
                quarter,
            );
            self.nodes[index].children[oct] = child;
        }
        index as u32
    }

    /// Gravitational pull on rock `i` per unit of G times rock mass.
    fn pull(&self, x: &[[f32; 3]], i: usize, softening2: f32) -> [f32; 3] {
        let p = x[i];
        let theta2 = THETA * THETA;
        let mut acceleration = [0.0_f32; 3];
        let mut add = |offset: [f32; 3], mass: f32| {
            let d2 = dot(offset, offset) + softening2;
            let scale = mass / (d2 * d2.sqrt());
            for axis in 0..3 {
                acceleration[axis] += offset[axis] * scale;
            }
        };
        let mut stack = [0u32; 512];
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            let offset = sub(node.centre_of_mass, p);
            if node.children == [NONE; 8] {
                for &j in &self.order[node.start as usize..node.end as usize] {
                    if j as usize != i {
                        add(sub(x[j as usize], p), 1.0);
                    }
                }
            } else if node.size2 < theta2 * dot(offset, offset) {
                add(offset, node.mass);
            } else {
                for &child in &node.children {
                    if child != NONE {
                        stack[top] = child;
                        top += 1;
                    }
                }
            }
        }
        acceleration
    }
}

/// Spatial hash of the rocks with cells one rock diameter wide.
struct Grid {
    cell: f32,
    mask: u32,
    starts: Vec<u32>,
    entries: Vec<u32>,
}

impl Grid {
    fn cell_of(&self, p: [f32; 3]) -> [i32; 3] {
        p.map(|axis| (axis / self.cell).floor() as i32)
    }

    fn bucket(&self, cell: [i32; 3]) -> u32 {
        ((cell[0] as u32).wrapping_mul(73_856_093)
            ^ (cell[1] as u32).wrapping_mul(19_349_663)
            ^ (cell[2] as u32).wrapping_mul(83_492_791))
            & self.mask
    }

    fn new(x: &[[f32; 3]], live: &[u32], cell: f32) -> Self {
        let buckets = (2 * live.len()).next_power_of_two().max(2);
        let mut grid = Self {
            cell,
            mask: buckets as u32 - 1,
            starts: vec![0; buckets + 1],
            entries: Vec::new(),
        };
        let mut keys: Vec<(u32, u32)> = live
            .par_iter()
            .map(|&i| (grid.bucket(grid.cell_of(x[i as usize])), i))
            .collect();
        keys.par_sort_unstable();
        for &(bucket, _) in &keys {
            grid.starts[bucket as usize + 1] += 1;
        }
        for bucket in 0..buckets {
            grid.starts[bucket + 1] += grid.starts[bucket];
        }
        grid.entries = keys.into_iter().map(|(_, i)| i).collect();
        grid
    }

    /// Calls `visit` with every rock in the 27 cells around `p`, each once.
    fn near(&self, p: [f32; 3], mut visit: impl FnMut(usize)) {
        let centre = self.cell_of(p);
        let mut buckets = [0u32; 27];
        let mut count = 0;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    buckets[count] = self.bucket([
                        centre[0] + dx,
                        centre[1] + dy,
                        centre[2] + dz,
                    ]);
                    count += 1;
                }
            }
        }
        buckets.sort_unstable();
        let mut previous = NONE;
        for bucket in buckets {
            if bucket == previous {
                continue;
            }
            previous = bucket;
            let range = self.starts[bucket as usize] as usize
                ..self.starts[bucket as usize + 1] as usize;
            for &j in &self.entries[range] {
                visit(j as usize);
            }
        }
    }
}

struct Sim {
    x: Vec<[f32; 3]>,
    v: Vec<[f32; 3]>,
    /// Acceleration at the current positions, for the next half kick.
    a: Vec<[f32; 3]>,
    /// Self-gravity from the last octree.
    gravity: Vec<[f32; 3]>,
    alive: Vec<bool>,
    live: Vec<u32>,
    steps: usize,
    dt: f32,
    /// G times the planet's mass.
    gm: f32,
    /// G times one rock's mass.
    gm_rock: f32,
    softening2: f32,
    radius: f32,
    stiffness: f32,
    damping: f32,
}

impl Sim {
    fn new(count: usize) -> Self {
        let (lattice, spacing) = moon_lattice(count);
        let distance = start_distance();
        // One orbit per sim second at the start distance.
        let spin = TAU;
        let gm = spin * spin * distance.powi(3);
        let gm_moon = gm / MASS_RATIO;
        let radius = 0.5 * spacing;
        // Weight on one contact in the middle of the moon: the central
        // pressure of a uniform ball, ρ g R / 2, over one lattice cell face.
        let surface_gravity = gm_moon / (MOON_RADIUS * MOON_RADIUS);
        let load = surface_gravity * MOON_RADIUS / (2.0 * spacing);
        let stiffness = load / (OVERLAP * radius);
        // Spring-dashpot between two unit masses (reduced mass 1/2) with
        // the chosen restitution.
        let reduced = 0.5;
        let log_e = RESTITUTION.ln();
        let zeta = -log_e / (PI * PI + log_e * log_e).sqrt();
        let natural = (stiffness / reduced).sqrt();
        let damping = 2.0 * zeta * (stiffness * reduced).sqrt();
        let contact_time = PI / (natural * (1.0 - zeta * zeta).sqrt());
        let x: Vec<[f32; 3]> = lattice
            .iter()
            .map(|offset| [distance + offset[0], offset[1], offset[2]])
            .collect();
        // Turning as one body with the orbit: tidally locked.
        let v: Vec<[f32; 3]> =
            x.iter().map(|p| [spin * p[2], 0.0, -spin * p[0]]).collect();
        let mut sim = Self {
            a: vec![[0.0; 3]; count],
            gravity: vec![[0.0; 3]; count],
            alive: vec![true; count],
            live: (0..count as u32).collect(),
            x,
            v,
            steps: 0,
            dt: contact_time / STEPS_PER_CONTACT,
            gm,
            gm_rock: gm_moon / count as f32,
            softening2: spacing * spacing,
            radius,
            stiffness,
            damping,
        };
        sim.gravity = sim.self_gravity();
        sim.a = sim.accelerations();
        sim
    }

    fn self_gravity(&self) -> Vec<[f32; 3]> {
        let tree = Octree::new(&self.x, &self.live);
        (0..self.x.len())
            .into_par_iter()
            .map(|i| {
                if !self.alive[i] {
                    return [0.0; 3];
                }
                tree.pull(&self.x, i, self.softening2)
                    .map(|axis| axis * self.gm_rock)
            })
            .collect()
    }

    fn accelerations(&self) -> Vec<[f32; 3]> {
        let diameter = 2.0 * self.radius;
        let grid = Grid::new(&self.x, &self.live, diameter);
        (0..self.x.len())
            .into_par_iter()
            .map(|i| {
                if !self.alive[i] {
                    return [0.0; 3];
                }
                let p = self.x[i];
                let r2 = dot(p, p);
                let planet = self.gm / (r2 * r2.sqrt());
                let mut a = std::array::from_fn(|axis| {
                    self.gravity[i][axis] - p[axis] * planet
                });
                grid.near(p, |j| {
                    if j == i {
                        return;
                    }
                    let offset = sub(p, self.x[j]);
                    let d2 = dot(offset, offset);
                    if d2 >= diameter * diameter || d2 == 0.0 {
                        return;
                    }
                    let d = d2.sqrt();
                    let normal = offset.map(|axis| axis / d);
                    let closing = dot(sub(self.v[i], self.v[j]), normal);
                    let push = self.stiffness * (diameter - d)
                        - self.damping * closing;
                    if push > 0.0 {
                        for axis in 0..3 {
                            a[axis] += normal[axis] * push;
                        }
                    }
                });
                a
            })
            .collect()
    }

    fn kick(&mut self, dt: f32) {
        self.v
            .par_iter_mut()
            .zip(self.a.par_iter())
            .for_each(|(v, a)| {
                for axis in 0..3 {
                    v[axis] += a[axis] * dt;
                }
            });
    }

    fn step(&mut self) {
        let dt = self.dt;
        self.kick(0.5 * dt);
        self.x
            .par_iter_mut()
            .zip(self.v.par_iter())
            .for_each(|(x, v)| {
                for axis in 0..3 {
                    x[axis] += v[axis] * dt;
                }
            });
        // Rocks that hit the planet, or fly past the recorded range, go.
        let floor = PLANET_RADIUS + self.radius;
        let range = 0.95 * QUANTUM * f32::from(i16::MAX);
        let mut removed = false;
        for &i in &self.live {
            let p = self.x[i as usize];
            if dot(p, p) < floor * floor
                || p.iter().any(|axis| axis.abs() > range)
            {
                self.alive[i as usize] = false;
                removed = true;
            }
        }
        if removed {
            let alive = &self.alive;
            self.live.retain(|&i| alive[i as usize]);
        }
        self.steps += 1;
        if self.steps.is_multiple_of(GRAVITY_EVERY) {
            self.gravity = self.self_gravity();
        }
        self.a = self.accelerations();
        self.kick(0.5 * dt);
    }

    fn frame(&self) -> Vec<[i16; 3]> {
        self.x
            .iter()
            .zip(&self.alive)
            .map(|(p, &alive)| {
                if alive {
                    p.map(|axis| (axis / QUANTUM).round() as i16)
                } else {
                    [i16::MIN; 3]
                }
            })
            .collect()
    }

    fn centre(&self) -> [f32; 3] {
        let mut sum = [0.0_f64; 3];
        for &i in &self.live {
            for (total, axis) in sum.iter_mut().zip(self.x[i as usize]) {
                *total += f64::from(axis);
            }
        }
        sum.map(|s| (s / self.live.len().max(1) as f64) as f32)
    }

    /// Prints where the rocks are: the spread around their centre, and the
    /// ring's radii and thickness, in rock diameters.
    fn report(&self, label: &str) {
        let centre = self.centre();
        let count = self.live.len().max(1) as f32;
        let spread = (self
            .live
            .iter()
            .map(|&i| {
                let d = sub(self.x[i as usize], centre);
                dot(d, d)
            })
            .sum::<f32>()
            / count)
            .sqrt();
        let mut radii: Vec<f32> = self
            .live
            .iter()
            .map(|&i| {
                let p = self.x[i as usize];
                (p[0] * p[0] + p[2] * p[2]).sqrt()
            })
            .collect();
        radii.sort_by(f32::total_cmp);
        let at =
            |share: f32| radii[((radii.len() - 1) as f32 * share) as usize];
        let thickness = (self
            .live
            .iter()
            .map(|&i| self.x[i as usize][1].powi(2))
            .sum::<f32>()
            / count)
            .sqrt();
        println!(
            "{label}: {} rocks, spread {spread:.2}, ring radius {:.1} to {:.1} \
             (median {:.1}), rms thickness {thickness:.3} = {:.1} rock diameters",
            self.live.len(),
            at(0.05),
            at(0.95),
            at(0.5),
            thickness / (2.0 * self.radius),
        );
    }
}

#[derive(Default)]
struct Recording {
    frames: Vec<Vec<[i16; 3]>>,
    centres: Vec<[f32; 3]>,
    done: bool,
}

fn recording() -> &'static Mutex<Recording> {
    static RECORDING: OnceLock<Mutex<Recording>> = OnceLock::new();
    RECORDING.get_or_init(Mutex::default)
}

fn bake(count: usize) {
    let started = Instant::now();
    let mut sim = Sim::new(count);
    println!(
        "roche ring: {count} rocks, radius {:.4}, dt {:.2e} s ({:.0} steps an orbit), \
         Roche distance {:.1}, start {:.1}",
        sim.radius,
        sim.dt,
        1.0 / sim.dt,
        roche_distance(),
        start_distance(),
    );
    let mut owed = PREROLL;
    let mut orbits = 0.0;
    let frames = frame_count();
    for frame in 0..frames {
        while owed >= sim.dt {
            sim.step();
            owed -= sim.dt;
            orbits += sim.dt;
        }
        {
            let mut recording = recording().lock().unwrap();
            recording.frames.push(sim.frame());
            recording.centres.push(sim.centre());
        }
        let t = frame as f32 / FPS;
        owed += orbits_per_second(t) / FPS;
        if frame % 60 == 0 {
            println!(
                "baked {frame}/{frames} frames, {orbits:.2} orbits, {} steps, {:.0} s",
                sim.steps,
                started.elapsed().as_secs_f32()
            );
            sim.report(&format!("  video {t:.0} s"));
        }
    }
    sim.report("ring");
    println!("baked in {:.0} s; playing", started.elapsed().as_secs_f32());
    recording().lock().unwrap().done = true;
}

/// Linear RGB of a rock from its starting depth, 0 at the centre and 1 at
/// the surface, and its glow.
fn rock_color(depth: f32, index: usize) -> ([f32; 4], [f32; 3]) {
    let core = [1.0, 0.32, 0.07];
    let mantle = [0.45, 0.24, 0.13];
    let shade = 0.85 + 0.3 * noise(index, 1);
    let crust = [0.42 * shade, 0.42 * shade, 0.44 * shade];
    let mix = |a: [f32; 3], b: [f32; 3], k: f32| {
        std::array::from_fn(|axis| a[axis] + (b[axis] - a[axis]) * k)
    };
    let color = mix(
        mix(core, mantle, smooth((depth - 0.35) / 0.25)),
        crust,
        smooth((depth - 0.6) / 0.15),
    );
    let heat = 1.0 - smooth((depth - 0.3) / 0.3);
    (
        [color[0], color[1], color[2], 1.0],
        [2.4 * heat, 0.6 * heat, 0.1 * heat],
    )
}

#[derive(Default)]
struct Playback {
    rocks: Vec<Entity>,
    dashes: Vec<Entity>,
    visual_radius: f32,
    start: Option<f32>,
    shown: Option<usize>,
}

thread_local! {
    static PLAYBACK: RefCell<Playback> = RefCell::new(Playback::default());
}

/// ECS rotation whose -Z axis points along `direction`.
fn facing(direction: [f32; 3]) -> [f32; 3] {
    let length = dot(direction, direction).sqrt();
    let [x, y, z] = direction.map(|axis| axis / length);
    [y.asin(), (-x).atan2(-z), 0.0]
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.004, 0.005, 0.009, 1.0]);
    scene.world().spawn(Bloom {
        intensity: 0.4,
        threshold: 1.1,
        spread: 0.5,
    });
    scene.world().spawn((
        Transform {
            rotation: facing([-0.8, -0.25, 0.55]),
            ..Transform::default()
        },
        DirectionalLight {
            illuminance: 90_000.0,
            ..DirectionalLight::default()
        },
    ));

    // Far stars, not simulated.
    let far = scene.create_material(MaterialAsset {
        base_color: [0.0, 0.0, 0.0, 1.0],
        emissive: [1.1, 1.2, 1.4],
        ..MaterialAsset::default()
    });
    let far_shape = CubeSpawn::new();
    for index in 0..2500 {
        let (theta, z) = (TAU * noise(index, 20), 2.0 * noise(index, 21) - 1.0);
        let ring = (1.0 - z * z).sqrt();
        let mut transform = Transform::new(
            [ring * theta.cos(), z, ring * theta.sin()]
                .map(|axis| axis * 600.0),
        );
        transform.scale = [0.4 + 0.9 * noise(index, 22); 3];
        scene.spawn_cube_with_material(
            format!("Far Star {index}"),
            transform,
            &far_shape,
            far,
        );
    }

    let planet = scene.create_material(MaterialAsset {
        shader: PLANET_SHADER.into(),
        ..MaterialAsset::default()
    });
    let mut transform = Transform::new([0.0; 3]);
    transform.scale = [2.0 * PLANET_RADIUS; 3];
    scene.spawn_sphere_with_material(
        "Planet",
        transform,
        &SphereSpawn::new().subdivisions(64),
        planet,
    );

    // The Roche limit: a dashed red circle in the orbit plane.
    let red = scene.create_material(MaterialAsset {
        model: MaterialModel::Unlit,
        base_color: [1.0, 0.08, 0.05, 1.0],
        emissive: [3.0, 0.2, 0.1],
        ..MaterialAsset::default()
    });
    let roche = roche_distance();
    let dash_shape = CubeSpawn::new();
    let mut dashes = Vec::with_capacity(DASHES);
    for index in 0..DASHES {
        let angle = TAU * (index as f32 + 0.5) / DASHES as f32;
        let mut transform =
            Transform::new([roche * angle.cos(), 0.0, roche * angle.sin()])
                .with_rotation(0.0, -angle - PI / 2.0, 0.0);
        transform.scale = [0.55 * TAU * roche / DASHES as f32, 0.12, 0.12];
        dashes.push(scene.spawn_cube_with_material(
            format!("Roche Dash {index}"),
            transform,
            &dash_shape,
            red,
        ));
    }

    // The moon, coloured by how deep each rock starts.
    let count = rock_count();
    let (lattice, spacing) = moon_lattice(count);
    let visual_radius = 0.5 * spacing * VISUAL_SCALE;
    let rock = scene.create_material(MaterialAsset {
        roughness: 0.9,
        ..MaterialAsset::default()
    });
    let shape = SphereSpawn::new().subdivisions(3);
    let distance = start_distance();
    let mut rocks = Vec::with_capacity(count);
    for (index, offset) in lattice.iter().enumerate() {
        let mut transform =
            Transform::new([distance + offset[0], offset[1], offset[2]]);
        transform.scale = [2.0 * visual_radius; 3];
        let entity = scene.spawn_sphere_with_material(
            format!("Rock {index}"),
            transform,
            &shape,
            rock,
        );
        let (base_color, emissive) =
            rock_color(dot(*offset, *offset).sqrt() / MOON_RADIUS, index);
        scene.world().entity_mut(entity).insert(MaterialOverride {
            base_color: Some(base_color),
            emissive: Some(emissive),
            ..MaterialOverride::default()
        });
        rocks.push(entity);
    }
    PLAYBACK.with(|playback| {
        *playback.borrow_mut() = Playback {
            rocks,
            dashes,
            visual_radius,
            ..Playback::default()
        };
    });
    // A scenario run (the video export) bakes before its first frame, so
    // every captured tick is one video frame.
    if std::env::var_os(rusting_engine::scenario::TEST_SCENARIO_ENV).is_some() {
        bake(count);
    } else {
        std::thread::spawn(move || bake(count));
    }
}

/// Camera position and aim at video time `t`, with `centre` the rocks'
/// centre of mass then and `wide_angle` the azimuth the wide shot starts
/// from.
fn camera(t: f32, centre: [f32; 3], wide_angle: f32) -> ([f32; 3], [f32; 3]) {
    let reach = if portrait() { 1.3 } else { 1.0 };
    // Close view of the moon from outside its orbit and below the orbit
    // plane, looking up: the planet, farther away, sits under the moon.
    // A slow push-in, then a pull-back that keeps the stream in frame.
    let angle = centre[2].atan2(centre[0]);
    let out = [angle.cos(), 0.0, angle.sin()];
    let ahead = [angle.sin(), 0.0, -angle.cos()];
    let push =
        1.0 - 0.1 * smooth((t - 1.0) / 2.5) + 1.6 * smooth((t - 3.5) / 4.5);
    let close_reach = if portrait() { 1.2 } else { 1.0 };
    let offset = [7.0, -3.5, -2.5].map(|k| k * push * close_reach);
    let near: [f32; 3] = std::array::from_fn(|axis| {
        centre[axis]
            + out[axis] * offset[0]
            + ahead[axis] * offset[2]
            + if axis == 1 { offset[1] } else { 0.0 }
    });
    let near_aim: [f32; 3] = std::array::from_fn(|axis| centre[axis] * 0.8);
    // Wide shot of the planet: above the plane, then tilting down to almost
    // edge-on so the ring's thinness reads, then a slow orbit.
    let tilt = smooth((t - 15.0) / 11.0);
    let elevation = (34.0 - 30.0 * tilt).to_radians();
    let azimuth = wide_angle + 0.05 * (t - 7.0);
    let distance = reach * (78.0 - 10.0 * tilt);
    let far = [
        distance * elevation.cos() * azimuth.cos(),
        distance * elevation.sin(),
        distance * elevation.cos() * azimuth.sin(),
    ];
    let blend = smooth((t - 7.0) / 6.0);
    let position = std::array::from_fn(|axis| {
        near[axis] + (far[axis] - near[axis]) * blend
    });
    let aim = near_aim.map(|axis| axis * (1.0 - blend));
    (position, aim)
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_roche_ring", create);
    let elapsed = time.elapsed_seconds();
    let (done, frames) = {
        let recording = recording().lock().unwrap();
        (recording.done, recording.frames.len())
    };
    if frames == 0 {
        return;
    }
    // While baking, show the newest frame; then replay from the start.
    let t = if done {
        let start = PLAYBACK.with(|playback| {
            *playback.borrow_mut().start.get_or_insert(elapsed)
        });
        elapsed - start
    } else {
        (frames - 1) as f32 / FPS
    };
    let frame = ((t * FPS).round() as usize).min(frames - 1);
    let recording = recording().lock().unwrap();
    let centre = recording.centres[frame];
    // The wide shot starts on the side the close shot looks from at 7 s.
    let at_seven = recording.centres[((7.0 * FPS) as usize).min(frames - 1)];
    let (near_at_seven, _) = camera(0.0, at_seven, 0.0);
    let wide_angle = near_at_seven[2].atan2(near_at_seven[0]);
    let (position, aim) = camera(t, centre, wide_angle);
    PLAYBACK.with(|playback| {
        let mut playback = playback.borrow_mut();
        if playback.shown != Some(frame) {
            playback.shown = Some(frame);
            let world = scene.world();
            let size = 2.0 * playback.visual_radius;
            for (&entity, q) in
                playback.rocks.iter().zip(&recording.frames[frame])
            {
                if let Some(mut transform) = world.get_mut::<Transform>(entity)
                {
                    if q[0] == i16::MIN {
                        transform.scale = [0.0; 3];
                    } else {
                        transform.position =
                            q.map(|axis| f32::from(axis) * QUANTUM);
                        transform.scale = [size; 3];
                    }
                }
            }
        }
        // The Roche circle thins away once the moon is coming apart. Dashes
        // right by the camera would fill the frame, so they hide.
        let thickness = 0.12 * (1.0 - smooth((t - 5.0) / 2.5));
        let world = scene.world();
        for &dash in &playback.dashes {
            if let Some(mut transform) = world.get_mut::<Transform>(dash) {
                let offset = sub(transform.position, position);
                let shown = if dot(offset, offset) < 144.0 {
                    0.0
                } else {
                    thickness
                };
                transform.scale[1] = shown;
                transform.scale[2] = shown;
            }
        }
    });
    drop(recording);
    scene
        .object("Game Camera")
        .set_position(position)
        .look_at(aim);
}

rusting_game!("testGame/build/main.rscene.bin", update);
