#version 450
layout(location = 0) in vec4 v_color;
layout(location = 1) in vec2 v_corner;
layout(location = 2) flat in uint v_sprite;
layout(location = 3) flat in float v_time;
layout(location = 0) out vec4 f_color;
#include "particle_shader.glsl"
void main() {
    float r = length(v_corner);
    float shape;
    if (v_sprite == 0u) {
        // Soft: bright core, smooth fall to the edge.
        float edge = clamp(1.0 - r, 0.0, 1.0);
        shape = edge * edge * (3.0 - 2.0 * edge);
    } else if (v_sprite == 1u) {
        float width = max(fwidth(r), 1e-4);
        shape = 1.0 - smoothstep(1.0 - width, 1.0, r);
    } else {
        shape = 1.0;
    }
    vec4 color = vec4(v_color.rgb, clamp(v_color.a, 0.0, 1.0) * shape);
    particle(ParticleInput(v_corner, v_color, v_sprite, v_time), color);
    float alpha = clamp(color.a, 0.0, 1.0);
    if (alpha <= 0.0) {
        discard;
    }
    // Premultiplied. Additive pipelines ignore the alpha output.
    f_color = vec4(color.rgb * alpha, alpha);
}
