#version 450
layout(location = 0) in vec2 v_ndc;
layout(location = 0) out vec4 f_color;
layout(push_constant) uniform Camera {
    mat4 view_projection;
    vec4 eye;
    vec4 ambient;
    vec4 ground_ambient;
    uvec4 light_info;
} camera;
struct Light {
    vec4 position_kind;
    vec4 direction_range;
    vec4 color_intensity;
    vec4 spot_angles;
    vec4 cookie;
};
layout(set = 0, binding = 2) readonly buffer Lights {
    Light data[];
} lights;
layout(set = 2, binding = 0) uniform sampler2DShadow shadow_map;
layout(set = 2, binding = 1) readonly buffer Shadow {
    vec4 environment;
    vec4 scene_color;
    vec4 viewport;
    vec4 probes[8];
    mat4 inverse_view_projection;
    vec4 fog;
    vec4 fog_shape;
    vec4 ambient_occlusion;
    mat4 cascades[6];
    vec4 cascade_far;
    vec4 cascade_forward;
    vec4 cascade_info;
    vec4 cluster_view;
    vec4 cluster_depth;
    vec4 fog_shafts;
    mat4 fog_volumes[8];
    vec4 fog_volume_colors[8];
    vec4 sky[4];
} shadow;
layout(set = 2, binding = 2) uniform sampler2D environment_map;
#include "fog.glsl"
#include "sky.glsl"
// The sky shader hook; see sky_shader.glsl.
#include "sky_shader.glsl"
void main() {
    vec4 veil = fog_sky(v_ndc);
    vec3 start = fog_unproject(v_ndc, 0.0);
    vec3 direction = normalize(fog_unproject(v_ndc, 0.5) - start);
    // Whether the sky replaces the background under the fog.
    bool replace = shadow.sky[2].w > 0.5;
    vec3 sky_color = vec3(0.0);
    if (shadow.sky[2].w > 1.5) {
        // The environment map, laid out as the scene shader reads it.
        vec2 uv = vec2(
            atan(direction.z, direction.x) / 6.2831853 + 0.5,
            acos(clamp(direction.y, -1.0, 1.0)) / 3.14159265
        );
        sky_color = textureLod(environment_map, uv, 0.0).rgb
            * shadow.environment.y;
    } else if (replace) {
        vec3 sun = shadow.sky[0].xyz;
        vec3 sun_color = shadow.sky[1].rgb;
        float haze = shadow.sky[1].w;
        vec3 ground = shadow.sky[2].rgb;
        sky_color = sky_radiance(direction, sun, sun_color, haze, ground);
        vec4 clouds = shadow.sky[3];
        float density = sky_cloud_density(direction, clouds.xy, clouds.z);
        if (density > 0.0) {
            sky_color = mix(sky_color, sky_cloud_color(direction, sun,
                sun_color, haze, ground, density, clouds.w), density);
        }
        // The sun disc: its sunlight spread over its solid angle, hidden
        // by clouds in front of it.
        float cos_radius = shadow.sky[0].w;
        if (cos_radius < 1.0 && dot(direction, sun) > cos_radius
            && direction.y > 0.0) {
            sky_color += sky_sunlight(sun, sun_color, haze)
                / (6.2831853 * (1.0 - cos_radius)) * (1.0 - density);
        }
    }
#ifdef RUSTING_CUSTOM_SKY
    sky(SkyInput(direction, camera.ambient.w), sky_color);
    replace = true;
#endif
    f_color = replace ? vec4(veil.rgb + veil.a * sky_color, 0.0) : veil;
}
