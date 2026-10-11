# Surface shaders

A material can carry its own GLSL that changes the surface per pixel before
the engine lights it: scrolling lava, a pulsing shield, a gradient sky dome,
a hologram flicker. Lighting, shadows, fog, decals, reflections and the
alpha modes keep working, because the shader only edits the surface the
engine then shades.

```rust
use rusting_engine::prelude::*;

const LAVA: &str = r#"#rusting surface 1
void surface(SurfaceInput i, inout Surface s) {
    vec2 flow = i.uv + vec2(0.0, i.time * 0.1);
    float heat = 0.5 + 0.5 * sin(flow.y * 20.0 + sin(flow.x * 7.0));
    s.base_color.rgb = mix(vec3(0.2, 0.02, 0.0), vec3(1.0, 0.35, 0.0), heat);
    s.emissive = s.base_color.rgb * heat * 3.0;
    s.roughness = 0.9;
}
"#;

fn build(scene: &mut GameScene) {
    let lava = scene.create_material(MaterialAsset {
        shader: LAVA.into(),
        ..MaterialAsset::default()
    });
    // Use `lava` in a mesh_renderer like any other material.
}
```

## The language

The first non-empty line names the shader kind and the language version:
`#rusting surface 1`. The rest is GLSL 450 that defines

```glsl
void surface(SurfaceInput i, inout Surface s);
```

`s` arrives holding the material's own values with its textures applied;
change only what you need.

| `SurfaceInput i` | |
| --- | --- |
| `vec2 uv` | texture coordinates after `uv_scale` and `uv_offset` |
| `vec3 world_position` | |
| `vec3 view_direction` | unit vector from the surface to the camera |
| `vec4 vertex_color` | vertex color times the material and object color |
| `float time` | seconds since the game started |

| `Surface s` | |
| --- | --- |
| `vec4 base_color` | linear RGB and alpha; alpha drives `Mask` and `Blend` |
| `vec3 normal` | world space; normalized after the shader |
| `float metallic` | clamped to 0..1 after the shader |
| `float roughness` | clamped to 0.04..1 after the shader |
| `vec3 emissive` | glow, linear RGB; above 1 blooms |

The shader may also sample the material's textures: `base_color_texture`,
`normal_texture`, `metallic_roughness_texture`, `occlusion_texture` and
`emissive_texture`. `RUSTING_SHADER_VERSION` is defined as 1. The
definitions live in `src/shaders/surface.glsl`.

Version 1 keeps these names and meanings. Later versions may add fields at
the end of either struct; a change that breaks a version 1 shader gets a new
version number, and the engine rejects a header it does not know.

## Compiling and errors

The renderer compiles each distinct shader once, at runtime, with `glslc`
(Vulkan SDK or shaderc; set `RUSTING_GLSLC` to another path). Line numbers
in messages count lines of your source. A shader that fails draws the
material's built-in surface instead, prints the error once, and
`SceneRenderer::surface_shader_errors` keeps it.

Materials that share a surface shader and textures still share one batch.
`Unlit` materials run the shader too and draw its `base_color`.

## Post shaders

A `PostShader` component runs GLSL on every pixel of the finished image,
after tone mapping, `ColorGrading`, `ColorLut` and the film effects. The
one on the entity with the lowest ID is used, and scene files save it.

```rust
use rusting_engine::runtime::{App, PostShader};

const SEPIA: &str = r#"#rusting post 1
void post(PostInput i, inout vec3 color) {
    float l = dot(color, vec3(0.2126, 0.7152, 0.0722));
    color = mix(color, l * vec3(1.1, 0.95, 0.7), 0.8);
}
"#;

fn build(app: &mut App) {
    app.spawn(PostShader { source: SEPIA.into() });
}
```

The header is `#rusting post 1` and the hook is

```glsl
void post(PostInput i, inout vec3 color);
```

`color` arrives as the display color, about 0..1, and is clamped after the
shader.

| `PostInput i` | |
| --- | --- |
| `vec2 uv` | 0..1 across the image, (0, 0) at the top left |
| `vec2 pixel` | pixel center in pixels |
| `vec2 texel` | one over the image size: the uv step to the next pixel |
| `float time` | game seconds from the fixed tick, the same in a replay |

`vec3 scene_at(vec2 uv)` reads the linear HDR image at any uv, bloom
added, before exposure and tone mapping, and black outside the image; use
it for outlines, blurs or warps. The definitions live in
`src/shaders/post.glsl`. A post shader that fails leaves the image as is,
prints the error once, and `SceneRenderer::post_shader_error` keeps it.

## Sky shaders

A `SkyShader` component colors the background wherever no surface was
drawn. The one on the entity with the lowest ID is used, and scene files
save it.

```rust
use rusting_engine::runtime::{App, SkyShader};

const DUSK: &str = r#"#rusting sky 1
void sky(SkyInput i, inout vec3 color) {
    float up = max(i.direction.y, 0.0);
    color = mix(vec3(1.0, 0.4, 0.2), vec3(0.05, 0.0, 0.2), sqrt(up));
}
"#;

fn build(app: &mut App) {
    app.spawn(SkyShader { source: DUSK.into() });
}
```

The header is `#rusting sky 1` and the hook is

```glsl
void sky(SkyInput i, inout vec3 color);
```

`color` is linear HDR. It arrives as the `ProceduralSky` or the background
`EnvironmentMap`, or black without either, and replaces the background
color. Fog still covers it.

| `SkyInput i` | |
| --- | --- |
| `vec3 direction` | unit world-space direction from the camera through the pixel |
| `float time` | seconds since the game started |

The definitions live in `src/shaders/sky_shader.glsl`. A sky shader that
fails leaves the built-in background, prints the error once, and
`SceneRenderer::sky_shader_error` keeps it.

## Particle shaders

A `ParticleShader` component next to a `ParticleEmitter` changes how that
emitter's particles look. Each emitter has its own; scene files save it.

```rust
use rusting_engine::runtime::{App, ParticleEmitter, ParticleShader};

const RINGS: &str = r#"#rusting particle 1
void particle(ParticleInput i, inout vec4 color) {
    float r = length(i.corner);
    color.a *= step(0.6, r) * step(r, 1.0);
}
"#;

fn build(app: &mut App) {
    app.spawn((
        ParticleEmitter::default(),
        ParticleShader { source: RINGS.into() },
    ));
}
```

The header is `#rusting particle 1` and the hook is

```glsl
void particle(ParticleInput i, inout vec4 color);
```

`color` is linear RGB and alpha. It arrives as the particle's color with
its alpha already shaped by the sprite; alpha 0 draws nothing. Additive
emitters add `color.rgb` times the alpha.

| `ParticleInput i` | |
| --- | --- |
| `vec2 corner` | position on the quad, -1..1 on each axis, (0, 0) at the center |
| `vec4 particle_color` | the particle's RGBA from the emitter's colors and fades |
| `uint sprite` | the emitter's sprite: 0 soft, 1 disc, 2 square |
| `float time` | seconds since the game started |

The definitions live in `src/shaders/particle_shader.glsl`. A particle
shader that fails leaves the built-in look, prints the error once, and
`SceneRenderer::particle_shader_errors` keeps it.

## Fog shaders

A `FogShader` component changes the fog over every surface and the
background. The one on the entity with the lowest ID is used, and scene
files save it.

```rust
use rusting_engine::runtime::{App, FogShader};

const GROUND_MIST: &str = r#"#rusting fog 1
void fog(FogInput i, inout vec4 haze) {
    if (i.background) {
        return;
    }
    vec3 end = i.start + i.direction * i.distance;
    float mist = exp(-0.1 * min(i.distance, 200.0)) + smoothstep(1.0, 0.0, end.y);
    float shows = clamp(1.0 - 0.5 * mist, 0.0, 1.0);
    haze = vec4(haze.rgb * shows + vec3(0.7, 0.75, 0.8) * (1.0 - shows), haze.a * shows);
}
"#;

fn build(app: &mut App) {
    app.spawn(FogShader { source: GROUND_MIST.into() });
}
```

The header is `#rusting fog 1` and the hook is

```glsl
void fog(FogInput i, inout vec4 haze);
```

`haze.rgb` is the linear HDR light the fog adds, and `haze.a` is the
share of the surface or background behind it that still shows. It
arrives as the `Fog` and `FogVolume` result, or `(0, 0, 0, 1)` without
them. The shader also runs where the scene has no fog.

| `FogInput i` | |
| --- | --- |
| `vec3 start` | world position where the view ray leaves the camera |
| `vec3 direction` | unit world-space direction of the ray |
| `float distance` | metres from `start` to the surface; 10000 for the background |
| `bool background` | true for the background, which lies infinitely far away |
| `float time` | seconds since the game started |

The definitions live in `src/shaders/fog_shader.glsl`, and the shader can
call the helpers of `src/shaders/fog.glsl`. While a fog shader is set,
every material draws with a pipeline built for it, so a material's surface
shader and the fog shader compile together. A fog shader that fails leaves
the built-in fog, prints the error once, and `SceneRenderer::sky_shader_error`
and `SceneRenderer::surface_shader_errors` keep it.

## Not yet

- Scene files do not save a material's `shader`, so surface shaders come
  from game code.
- The depth prepass and shadows ignore the surface shader, so `discard`
  and alpha changes do not cut holes in shadows.
- Post shaders cannot read depth.
- Reflections, the environment lighting and reflection probes do not see
  the sky shader.
