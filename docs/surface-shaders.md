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

## Not yet

- Scene files do not save `shader`, so surface shaders come from game code.
- The depth prepass and shadows ignore the shader, so `discard` and alpha
  changes do not cut holes in shadows.
- Sky, particle, fog and post-process shader kinds.
