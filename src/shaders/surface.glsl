// Rusting shader language, version 1: the built-ins a `#rusting surface 1`
// shader sees. Fields are only ever added at the end, and a change that
// would break a version 1 shader bumps RUSTING_SHADER_VERSION.
#define RUSTING_SHADER_VERSION 1
// What the engine knows about the pixel before the shader runs.
struct SurfaceInput {
    // Texture coordinates after the material's uv_scale and uv_offset.
    vec2 uv;
    vec3 world_position;
    // Unit vector from the surface toward the camera.
    vec3 view_direction;
    // Vertex color times the material and per-object color.
    vec4 vertex_color;
    // Seconds since the game started.
    float time;
};
// The surface the engine lights. It starts as the material's own values,
// textures applied; the shader may change any of them.
struct Surface {
    // Linear RGB and alpha; alpha drives Mask and Blend.
    vec4 base_color;
    // World-space normal; the engine normalizes it after the shader.
    vec3 normal;
    float metallic;
    float roughness;
    // Light given off, linear RGB.
    vec3 emissive;
};
#ifndef RUSTING_CUSTOM_SURFACE
void surface(SurfaceInput i, inout Surface s) {}
#endif
