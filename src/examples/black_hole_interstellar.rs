//! Gargantua: a spinning black hole in the style of the film Interstellar.
//!
//! ```sh
//! cargo run --release --features window --example black_hole_interstellar
//! ```
//!
//! Like `black_hole_cinematic`, the scene has no mesh: a fog shader traces
//! every background pixel's ray backwards through spacetime. This one is a
//! Kerr hole with spin a = 0.6 M, the spin the film's disk shots used. The
//! rays follow Hamilton's equations in Kerr-Schild coordinates, where the
//! inverse metric is `η - f l l`, integrated with RK4. Frame dragging
//! makes the shadow lopsided and lets the disk reach down to the prograde
//! innermost stable orbit at 3.83 M.
//!
//! The disk is a thin glowing slab, not a plane: each ray step that comes
//! near it takes eight samples of emission and absorption, so seen edge-on
//! it is a bright line, and above it shows fine ring bands. As in the film,
//! the Doppler shift is mostly left out (`DOPPLER`) and the gravitational
//! redshift only tints it (`REDSHIFT`).
//!
//! A post shader adds a faint horizontal lens glow and the fades between
//! the four shots of the fixed 60 s path, so
//! `scripts/black_hole_interstellar_video.sh` films the same cut each time.

use rusting_engine::prelude::*;
use rusting_engine::runtime::{
    Bloom, Camera, ColorGrading, FogShader, PostShader, Projection,
};

/// Length of the whole cut; the post shader's `SECONDS` must match.
const SECONDS: f32 = 60.0;

const SHADER: &str = r#"#rusting fog 1
// Units: the hole's mass M is 1. Kerr-Schild coordinates with the spin
// along z, which is world y; the disk lies in z = 0.
const float A = 0.6;
const float HORIZON = 1.8;
// Prograde innermost stable circular orbit for a = 0.6.
const float DISK_IN = 3.83;
const float DISK_OUT = 42.0;
// Share of the Doppler shift and of the gravitational redshift kept.
const float DOPPLER = 0.2;
const float REDSHIFT = 0.35;
const float SPIN = 6.0;
// Gas scale height over radius.
const float THICKNESS = 0.012;

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

// Boyer-Lindquist radius of a Kerr-Schild point.
float kerr_r(vec3 x) {
    float b = dot(x, x) - A * A;
    return sqrt(0.5 * (b + sqrt(b * b + 4.0 * A * A * x.z * x.z)));
}

// The metric's f and l at x: g^{uv} = eta^{uv} - f l^u l^v.
void kerr(vec3 x, out float f, out vec3 l) {
    float r = kerr_r(x);
    float r2 = r * r;
    f = 2.0 * r2 * r / (r2 * r2 + A * A * x.z * x.z);
    l = vec3((r * x.x + A * x.y) / (r2 + A * A),
             (r * x.y - A * x.x) / (r2 + A * A),
             x.z / r);
}

// f (l.p)^2, the part of the Hamiltonian that depends on position.
float bend(vec3 x, vec3 p, float e) {
    float f;
    vec3 l;
    kerr(x, f, l);
    float s = e + dot(l, p);
    return f * s * s;
}

// Hamilton's equations for H = (|p|^2 - e^2 - f (e + l.p)^2) / 2.
void derivs(vec3 x, vec3 p, float e, out vec3 dx, out vec3 dp) {
    float f;
    vec3 l;
    kerr(x, f, l);
    dx = p - f * (e + dot(l, p)) * l;
    float h = 0.01 * max(length(x), 1.0);
    dp = 0.25 / h * vec3(
        bend(x + vec3(h, 0, 0), p, e) - bend(x - vec3(h, 0, 0), p, e),
        bend(x + vec3(0, h, 0), p, e) - bend(x - vec3(0, h, 0), p, e),
        bend(x + vec3(0, 0, h), p, e) - bend(x - vec3(0, 0, h), p, e));
}

// Emission (rgb) and absorption per unit length (a) of the disk gas at q,
// seen along `ray`.
vec4 gas(vec3 q, vec3 ray, float time) {
    float rc = length(q.xy);
    if (rc < DISK_IN || rc > DISK_OUT) {
        return vec4(0.0);
    }
    float height = THICKNESS * rc;
    float vertical = exp(-0.5 * q.z * q.z / (height * height));
    if (vertical < 1e-3) {
        return vec4(0.0);
    }
    float rs = sqrt(rc);
    // Prograde circular orbit; the gas turns against the traced spin
    // because the rays run backwards in time.
    float omega = 1.0 / (rc * rs + A);
    float ut = (rc * rs + A) / (pow(rc, 0.75) * sqrt(rc * rs - 3.0 * rs + 2.0 * A));
    vec3 flow = normalize(vec3(q.y, -q.x, 0.0));
    float toward = dot(flow, -ray);
    float beta = min(1.0 / rs, 0.7);
    float g = mix(1.0, 1.0 / ut, REDSHIFT)
        * mix(1.0, 1.0 / (1.0 - beta * toward), DOPPLER);

    float phi = atan(q.y, q.x) + omega * time * SPIN;
    vec3 at = vec3(cos(phi) * 4.0, sin(phi) * 4.0, rc * 1.3 + q.z / height * 0.25);
    float cloud = fbm(at);
    float rings = 0.6 * noise3(vec3(rc * 4.5, 0.5, 2.5))
        + 0.4 * noise3(vec3(rc * 16.0, 7.5, 1.5 + phi * 0.3));
    float density = clamp(2.0 * cloud - 0.45, 0.0, 1.5)
        * (0.3 + 1.4 * rings * rings) * vertical
        * smoothstep(DISK_IN, DISK_IN * 1.06, rc)
        * (1.0 - smoothstep(DISK_OUT * 0.3, DISK_OUT, rc));

    float x = rc / DISK_IN;
    float heat = pow(x, -0.75) * pow(1.0 - 0.9 * inversesqrt(x), 0.25) / 0.57;
    float kelvin = 4600.0 * pow(heat, 0.5) * g;
    float glow = 2.6 * pow(heat, 3.0) * pow(g, 4.0);
    float absorb = 0.35 * density / height;
    return vec4(blackbody(kelvin) * glow * absorb, absorb);
}

vec3 stars(vec3 direction) {
    vec3 color = vec3(0.0);
    for (int layer = 0; layer < 2; layer++) {
        float scale = layer == 0 ? 420.0 : 160.0;
        vec3 p = direction * scale;
        vec3 cell = floor(p - 0.5);
        for (int k = 0; k < 8; k++) {
            vec3 c = cell + vec3(k & 1, (k >> 1) & 1, (k >> 2) & 1);
            vec3 h = hash33(c + float(layer) * 91.7);
            if (h.x > (layer == 0 ? 0.08 : 0.03)) {
                continue;
            }
            vec3 star = normalize(c + h) * scale;
            float d = length(p - star);
            float size = layer == 0 ? 0.16 : 0.2;
            float bright = layer == 0 ? 1.3 * pow(h.y, 8.0) : 22.0 * pow(h.y, 10.0);
            vec3 tint = blackbody(3200.0 + 8000.0 * h.z);
            color += tint * bright * exp(-d * d / (size * size));
        }
    }
    vec3 axis = normalize(vec3(0.3, 0.85, -0.42));
    float band = exp(-pow(dot(direction, axis) * 4.0, 2.0));
    float dust = fbm(direction * 6.0);
    color += band * vec3(0.05, 0.045, 0.06) * dust * dust * 0.5;
    return color;
}

void fog(FogInput i, inout vec4 haze) {
    if (!i.background) {
        return;
    }
    float time = i.time;
    vec3 x = i.start.xzy;
    vec3 p = i.direction.xzy;
    // Energy that makes the momentum null, future-directed.
    float f;
    vec3 l;
    kerr(x, f, l);
    float s = dot(l, p);
    float e = (-f * s + sqrt(1.0 + f - f * s * s)) / (1.0 + f);

    vec3 color = vec3(0.0);
    float shows = 1.0;
    bool captured = false;
    float far = max(length(x) * 1.1, 80.0);
    vec3 dx = p;
    for (int step = 0; step < 1400; step++) {
        float r = kerr_r(x);
        if (r < HORIZON + 0.03) {
            captured = true;
            break;
        }
        vec3 dx1, dp1;
        derivs(x, p, e, dx1, dp1);
        dx = dx1;
        if (r > far && dot(x, dx1) > 0.0) {
            break;
        }
        float dt = clamp(0.06 * (r - HORIZON), 0.01, 3.0);
        vec3 dx2, dp2, dx3, dp3, dx4, dp4;
        derivs(x + 0.5 * dt * dx1, p + 0.5 * dt * dp1, e, dx2, dp2);
        derivs(x + 0.5 * dt * dx2, p + 0.5 * dt * dp2, e, dx3, dp3);
        derivs(x + dt * dx3, p + dt * dp3, e, dx4, dp4);
        vec3 next = x + dt / 6.0 * (dx1 + 2.0 * dx2 + 2.0 * dx3 + dx4);
        vec3 next_p = p + dt / 6.0 * (dp1 + 2.0 * dp2 + 2.0 * dp3 + dp4);

        // The disk slab: eight samples of the segment when it comes near.
        float reach = 4.0 * THICKNESS * max(length(x.xy), DISK_IN);
        if (x.z * next.z < 0.0 || min(abs(x.z), abs(next.z)) < reach) {
            vec3 seg = next - x;
            float ds = length(seg) / 8.0;
            vec3 ray = seg / max(length(seg), 1e-6);
            for (int k = 0; k < 8; k++) {
                vec4 g = gas(x + seg * ((float(k) + 0.5) / 8.0), ray, time);
                if (g.a > 0.0) {
                    float through = exp(-g.a * ds);
                    color += shows * g.rgb / g.a * (1.0 - through);
                    shows *= through;
                }
            }
        }
        x = next;
        p = next_p;
        if (shows < 0.004) {
            break;
        }
    }
    if (!captured && shows >= 0.004) {
        color += shows * stars(normalize(dx).xzy);
    }
    haze = vec4(color, 0.0);
}
"#;

/// Fades between shots and a faint horizontal glow from the brightest
/// light, like a lens.
const POST: &str = r#"#rusting post 1
const float SECONDS = 60.0;

float dip(float t, float cut) {
    return smoothstep(0.0, 0.7, abs(t - cut));
}

void post(PostInput i, inout vec3 color) {
    vec3 streak = vec3(0.0);
    for (int k = 1; k <= 24; k++) {
        float reach = float(k * k) * 1.5 * i.texel.x * 1920.0 / 1920.0;
        float w = exp(-float(k) * 0.16);
        vec3 l = scene_at(i.uv + vec2(reach * 0.75, 0.0))
            + scene_at(i.uv - vec2(reach * 0.75, 0.0));
        streak += w * max(l - vec3(2.5), vec3(0.0));
    }
    color += streak * vec3(1.0, 0.75, 0.45) * 0.0035;
    float t = mod(i.time, SECONDS);
    float fade = smoothstep(0.0, 3.0, t) * (1.0 - smoothstep(SECONDS - 3.5, SECONDS, t))
        * dip(t, 14.0) * dip(t, 28.0) * dip(t, 44.0);
    color *= fade;
}
"#;

fn smooth(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// One frame of the cut: camera position, roll and vertical field of view
/// in degrees. Distances are in hole masses; the camera looks at the hole.
fn shot(t: f32) -> ([f32; 3], f32, f32) {
    let (distance, elevation, azimuth, roll, fov) = if t < 14.0 {
        // The film's view: almost edge-on, far, drifting in.
        let u = smooth(t / 14.0);
        (
            72.0 - 22.0 * u,
            2.2 - 0.9 * u,
            0.35 - 0.012 * t,
            9.0 - 3.0 * u,
            32.0,
        )
    } else if t < 28.0 {
        // High above: the far side of the disk wraps over the hole.
        let u = smooth((t - 14.0) / 14.0);
        (34.0 - 4.0 * u, 58.0 - 20.0 * u, 1.2 + 0.5 * u, 0.0, 46.0)
    } else if t < 44.0 {
        // Close and low: the disk fills the frame below a huge arc.
        let u = smooth((t - 28.0) / 16.0);
        (
            17.0 - 6.0 * u,
            11.0 - 7.0 * u,
            2.6 + 0.35 * u,
            -6.0 + 4.0 * u,
            58.0,
        )
    } else {
        // Pull back, nearly edge-on, and hold.
        let u = smooth((t - 44.0) / 16.0);
        (
            26.0 + 34.0 * u,
            2.5,
            4.2 + 0.01 * (t - 44.0),
            4.0 * (1.0 - u),
            30.0,
        )
    };
    let elevation = elevation.to_radians();
    let position = [
        distance * elevation.cos() * azimuth.sin(),
        distance * elevation.sin(),
        distance * elevation.cos() * azimuth.cos(),
    ];
    (position, roll, fov)
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|axis| axis / length)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Euler angles (the engine's X, then Y, then Z order) that look from
/// `position` at the origin, turned `roll` degrees about the view axis.
fn look_with_roll(position: [f32; 3], roll: f32) -> [f32; 3] {
    let forward = normalize(position.map(|axis| -axis));
    let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);
    let (sin, cos) = roll.to_radians().sin_cos();
    let right_rolled: [f32; 3] =
        std::array::from_fn(|i| right[i] * cos + up[i] * sin);
    let up_rolled: [f32; 3] =
        std::array::from_fn(|i| up[i] * cos - right[i] * sin);
    // Columns of the rotation: right, up and back (the camera looks down -Z).
    let back = forward.map(|axis| -axis);
    [
        up_rolled[2].atan2(back[2]),
        -right_rolled[2].asin(),
        right_rolled[1].atan2(right_rolled[0]),
    ]
}

fn create(scene: &mut GameScene<'_>) {
    scene.set_background_color([0.0, 0.0, 0.0, 1.0]);
    let world = scene.world();
    world.spawn(FogShader {
        source: SHADER.into(),
    });
    world.spawn(PostShader {
        source: POST.into(),
    });
    world.spawn(Bloom {
        intensity: 0.6,
        threshold: 1.0,
        spread: 0.8,
    });
    world.spawn(ColorGrading {
        contrast: 1.1,
        saturation: 1.3,
        shadows: [0.96, 0.96, 1.06],
        highlights: [1.1, 1.0, 0.85],
        vignette: 0.4,
        grain: 0.05,
        ..ColorGrading::DEFAULT
    });
}

fn update(scene: &mut GameScene<'_>, time: &FrameTime) {
    scene.once("create_black_hole_interstellar", create);
    let t = time.elapsed_seconds() % SECONDS;
    let (position, roll, fov) = shot(t);
    let rotation = look_with_roll(position, roll);
    let mut camera = scene.object("Game Camera");
    camera.set_position(position).set_rotation(rotation);
    let entity = camera.entity();
    if let Some(mut camera) = scene.world().get_mut::<Camera>(entity) {
        camera.projection = Projection::Perspective {
            vertical_fov_radians: fov.to_radians(),
            near: 0.05,
            far: 1000.0,
        };
    }
}

rusting_game!("testGame/build/main.rscene.bin", update);
