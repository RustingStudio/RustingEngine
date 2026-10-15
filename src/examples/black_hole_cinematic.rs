//! A black hole filmed up close: light bent by real Schwarzschild gravity.
//!
//! ```sh
//! cargo run --release --features window --example black_hole_cinematic
//! ```
//!
//! There is no mesh in the scene. A fog shader runs on every background
//! pixel, where it knows the camera position and the view ray, and traces
//! that ray backwards through curved spacetime: RK4 on the null geodesic
//! equation `a = -3/2 h² x / r⁵` (units of the Schwarzschild radius). Each
//! time the ray crosses the disk plane it picks up the disk's light, so the
//! far side of the disk shows above and below the hole, and rays that
//! circle the hole once or more make the thin photon ring. Rays that fall
//! inside the horizon stay black; the rest end on a procedural star field,
//! lensed like everything else.
//!
//! The disk runs from the innermost stable orbit (3 radii) outward, hotter
//! inside (Novikov-Thorne temperature), turning with Kepler's speed. The
//! side coming toward the camera is blue-shifted and brighter, the receding
//! side redder and dimmer, plus the gravitational redshift near the hole.
//! The camera flies a fixed 40 s path, so `scripts/black_hole_video.sh`
//! films the same shot every time.

use rusting_engine::prelude::*;
use rusting_engine::runtime::{
    Bloom, Camera, ColorGrading, FogShader, Projection,
};

/// Length of the camera path; the shader fades in and out over it, so its
/// `SECONDS` must match.
const SECONDS: f32 = 40.0;

const SHADER: &str = r#"#rusting fog 1
// Units: the Schwarzschild radius is 1. The hole sits at the origin and
// the disk lies in the y = 0 plane.
const float SECONDS = 40.0;
const float DISK_IN = 3.0;
const float DISK_OUT = 15.0;
// 1 keeps the full Doppler shift and beaming, 0 removes it.
const float DOPPLER = 0.65;
const float SPIN = 5.0;

float hash13(vec3 p) {
    p = fract(p * 0.1031);
    p += dot(p, p.zyx + 31.32);
    return fract((p.x + p.y) * p.z);
}

vec3 hash33(vec3 p) {
    p = fract(p * vec3(0.1031, 0.1030, 0.0973));
    p += dot(p, p.yxz + 33.33);
    return fract((p.xxy + p.yxx) * p.zyx);
}

float noise3(vec3 p) {
    vec3 i = floor(p);
    vec3 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(
        mix(mix(hash13(i), hash13(i + vec3(1, 0, 0)), f.x),
            mix(hash13(i + vec3(0, 1, 0)), hash13(i + vec3(1, 1, 0)), f.x), f.y),
        mix(mix(hash13(i + vec3(0, 0, 1)), hash13(i + vec3(1, 0, 1)), f.x),
            mix(hash13(i + vec3(0, 1, 1)), hash13(i + vec3(1, 1, 1)), f.x), f.y),
        f.z);
}

float fbm(vec3 p) {
    float sum = 0.0;
    float weight = 0.5;
    for (int octave = 0; octave < 5; octave++) {
        sum += weight * noise3(p);
        p = p * 2.03 + vec3(1.7, 9.2, 4.1);
        weight *= 0.5;
    }
    return sum;
}

// Color of a black body at `kelvin`, linear sRGB scaled to a peak of 1
// (the Planckian locus fit by Krystek).
vec3 blackbody(float kelvin) {
    float t = clamp(kelvin, 1000.0, 40000.0);
    float u = (0.860117757 + 1.54118254e-4 * t + 1.28641212e-7 * t * t)
        / (1.0 + 8.42420235e-4 * t + 7.08145163e-7 * t * t);
    float v = (0.317398726 + 4.22806245e-5 * t + 4.20481691e-8 * t * t)
        / (1.0 - 2.89741816e-5 * t + 1.61456053e-7 * t * t);
    float x = 3.0 * u / (2.0 * u - 8.0 * v + 4.0);
    float y = 2.0 * v / (2.0 * u - 8.0 * v + 4.0);
    vec3 xyz = vec3(x / y, 1.0, (1.0 - x - y) / y);
    vec3 rgb = mat3(3.2404542, -0.9692660, 0.0556434,
                    -1.5371385, 1.8760108, -0.2040259,
                    -0.4985314, 0.0415560, 1.0572252) * xyz;
    rgb = max(rgb, vec3(0.0));
    return rgb / max(max(rgb.r, rgb.g), rgb.b);
}

// Light from the disk where the ray crosses it at `hit`, travelling along
// `ray` (from the camera), and how much the disk covers there.
vec4 disk(vec3 hit, vec3 ray, float time) {
    float r = length(hit.xz);
    if (r < DISK_IN || r > DISK_OUT) {
        return vec4(0.0);
    }
    // Kepler: angular speed sqrt(M / r^3) with M = 1/2; speed seen by a
    // static observer sqrt(M / (r - 2M)).
    float omega = sqrt(0.5 / (r * r * r));
    float beta = sqrt(0.5 / (r - 1.0));
    vec3 flow = normalize(vec3(-hit.z, 0.0, hit.x));
    // The photon travels toward the camera, against the traced ray.
    float toward = dot(flow, -normalize(ray));
    float g = sqrt(1.0 - 1.5 / r) / (1.0 - beta * toward);
    g = mix(sqrt(1.0 - 1.5 / r), g, DOPPLER);

    // Gas streaks: noise on the turning disk, sheared by the faster inside.
    float phi = atan(hit.z, hit.x) - omega * time * SPIN;
    vec3 at = vec3(cos(phi) * 5.0, sin(phi) * 5.0, r * 2.2);
    float gas = fbm(at);
    float fine = noise3(vec3(cos(phi) * 22.0, sin(phi) * 22.0, r * 9.0));
    float density = clamp(1.35 * gas + 0.35 * fine - 0.2, 0.0, 1.5);

    float x = r / DISK_IN;
    // Novikov-Thorne profile, kept bright right up to the inner edge.
    float heat = pow(x, -0.75) * pow(1.0 - 0.93 * inversesqrt(x), 0.25) / 0.52;
    float kelvin = 4300.0 * pow(heat, 1.3) * g;
    float edge = smoothstep(DISK_IN, DISK_IN + 0.25, r)
        * (1.0 - smoothstep(DISK_OUT * 0.55, DISK_OUT, r));
    float brightness = 2.4 * pow(heat, 3.4) * pow(g, 3.5) * density * edge;
    float cover = clamp(density * edge * 1.3, 0.0, 0.985);
    return vec4(blackbody(kelvin) * brightness, cover);
}

vec3 stars(vec3 direction) {
    vec3 color = vec3(0.0);
    // Two layers: many faint stars and a few bright ones.
    for (int layer = 0; layer < 2; layer++) {
        float scale = layer == 0 ? 420.0 : 160.0;
        vec3 p = direction * scale;
        vec3 cell = floor(p);
        for (int k = 0; k < 8; k++) {
            vec3 c = cell + vec3(k & 1, (k >> 1) & 1, (k >> 2) & 1) - 0.5;
            c = floor(c + 0.5);
            vec3 h = hash33(c + float(layer) * 91.7);
            if (h.x > (layer == 0 ? 0.07 : 0.025)) {
                continue;
            }
            vec3 star = normalize(c + h) * scale;
            float d = length(p - star);
            float size = layer == 0 ? 0.16 : 0.2;
            float bright = layer == 0 ? 1.2 * pow(h.y, 8.0) : 20.0 * pow(h.y, 10.0);
            vec3 tint = blackbody(3000.0 + 9000.0 * h.z);
            color += tint * bright * exp(-d * d / (size * size));
        }
    }
    // A faint band of the galaxy behind.
    vec3 axis = normalize(vec3(0.3, 0.85, -0.42));
    float band = exp(-pow(dot(direction, axis) * 4.0, 2.0));
    float dust = fbm(direction * 6.0);
    color += band * vec3(0.05, 0.045, 0.06) * dust * dust * 0.6;
    return color;
}

void fog(FogInput i, inout vec4 haze) {
    if (!i.background) {
        return;
    }
    float time = mod(i.time, SECONDS);
    vec3 pos = i.start;
    vec3 vel = i.direction;
    vec3 h = cross(pos, vel);
    float h2 = dot(h, h);
    vec3 color = vec3(0.0);
    float shows = 1.0;
    bool captured = false;
    float far = max(length(pos) * 1.2, 40.0);
    for (int step = 0; step < 700; step++) {
        float r = length(pos);
        if (r < 1.0) {
            captured = true;
            break;
        }
        if (r > far && dot(pos, vel) > 0.0) {
            break;
        }
        float dt = clamp(0.06 * (r - 0.9), 0.008, 1.5);
        // RK4 on the null geodesic in Cartesian form.
        vec3 p1 = pos;
        vec3 v1 = vel;
        vec3 a1 = -1.5 * h2 * p1 / pow(dot(p1, p1), 2.5);
        vec3 p2 = pos + 0.5 * dt * v1;
        vec3 v2 = vel + 0.5 * dt * a1;
        vec3 a2 = -1.5 * h2 * p2 / pow(dot(p2, p2), 2.5);
        vec3 p3 = pos + 0.5 * dt * v2;
        vec3 v3 = vel + 0.5 * dt * a2;
        vec3 a3 = -1.5 * h2 * p3 / pow(dot(p3, p3), 2.5);
        vec3 p4 = pos + dt * v3;
        vec3 v4 = vel + dt * a3;
        vec3 a4 = -1.5 * h2 * p4 / pow(dot(p4, p4), 2.5);
        vec3 next = pos + dt / 6.0 * (v1 + 2.0 * v2 + 2.0 * v3 + v4);
        vec3 next_vel = vel + dt / 6.0 * (a1 + 2.0 * a2 + 2.0 * a3 + a4);
        if (pos.y * next.y < 0.0) {
            float t = pos.y / (pos.y - next.y);
            vec4 light = disk(mix(pos, next, t), mix(vel, next_vel, t), time);
            color += shows * light.rgb * light.a;
            shows *= 1.0 - light.a;
        }
        pos = next;
        vel = next_vel;
        if (shows < 0.01) {
            break;
        }
    }
    if (!captured && shows >= 0.01) {
        color += shows * stars(normalize(vel));
    }
    float fade = smoothstep(0.0, 2.5, time) * (1.0 - smoothstep(SECONDS - 2.5, SECONDS, time));
    haze = vec4(color * fade, 0.0);
}
"#;

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Camera position at `t` seconds, in Schwarzschild radii, looking at the
/// hole. Three moves: a slow push in close to the disk plane, a climb
/// around the hole that shows the disk from above, and a dive back down to
/// the edge-on view.
fn camera(t: f32) -> [f32; 3] {
    let distance = 38.0
        - 16.0 * smooth(t / 13.0)
        - 4.0 * smooth((t - 13.0) / 13.0)
        - 7.0 * smooth((t - 26.0) / 12.0);
    let elevation =
        1.5 + 2.5 * smooth(t / 12.0) + 24.0 * smooth((t - 12.0) / 14.0)
            - 25.0 * smooth((t - 26.0) / 12.0);
    let azimuth = 0.02 * t + 1.3 * smooth((t - 10.0) / 18.0);
    let (elevation, azimuth) = (elevation.to_radians(), azimuth);
    [
        distance * elevation.cos() * azimuth.sin(),
        distance * elevation.sin(),
        distance * elevation.cos() * azimuth.cos(),
    ]
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.0, 0.0, 0.0, 1.0]);
    let world = scene.world();
    world.spawn(FogShader {
        source: SHADER.into(),
    });
    world.spawn(Bloom {
        intensity: 0.55,
        threshold: 1.0,
        spread: 0.75,
    });
    world.spawn(ColorGrading {
        contrast: 1.08,
        saturation: 1.05,
        shadows: [0.95, 0.97, 1.06],
        highlights: [1.06, 1.0, 0.92],
        vignette: 0.35,
        grain: 0.04,
        ..ColorGrading::DEFAULT
    });
    let camera = scene.object("Game Camera").entity();
    if let Some(mut camera) = scene.world().get_mut::<Camera>(camera) {
        camera.projection = Projection::Perspective {
            vertical_fov_radians: 40f32.to_radians(),
            near: 0.05,
            far: 1000.0,
        };
    }
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_black_hole_cinematic", create);
    let t = time.elapsed_seconds() % SECONDS;
    scene
        .object("Game Camera")
        .set_position(camera(t))
        .look_at([0.0, 0.0, 0.0]);
}

rusting_game!("testGame/build/main.rscene.bin", update);
