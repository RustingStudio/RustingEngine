// Force fields shared by the built-in physics shader and GPU particles.
// Needs `physics_shapes.glsl` first. Not part of the custom-shader ABI.

struct ForceField {
    mat4 model;
    vec4 shape;
    vec4 direction; // w = strength
    vec4 params;    // x = kind, y = falloff distance, z = turbulence
};
layout(set = 0, binding = 14) readonly buffer ForceFields { ForceField data[]; } force_fields;

// Summed acceleration from every force field whose collider comes within
// `radius` of `center`, in upload order; mirrors `ForceField::push`.
vec3 field_push(vec3 center, float radius, uint field_count, float elapsed) {
    precise vec3 total = vec3(0.0);
    for (uint index = 0u; index < field_count; index++) {
        ForceField field = force_fields.data[index];
        vec3 origin = field.model[3].xyz;
        vec3 normal;
        float distance;
        shape_distance(field.shape, mat3(field.model), origin, center, normal, distance);
        if (distance > radius) continue;
        precise vec3 offset = center - origin;
        vec3 direction = sim_normalize(field.direction.xyz);
        float fade = 1.0;
        if (field.params.y > 0.0) {
            precise float faded = 1.0 - sim_div(sim_length(offset), field.params.y);
            fade = max(faded, 0.0);
        }
        precise vec3 toward = direction;
        if (field.params.x > 2.5) {
            // Wind: the same two sine gusts as on the CPU.
            precise float a_angle = 1.7 * elapsed + 0.31 * offset.x;
            precise float b_angle = 0.6 * elapsed + 0.23 * offset.z + 1.3;
            precise float gust = 1.0 + field.params.z * 0.5 * (sim_sin_cos(a_angle).x + sim_sin_cos(b_angle).x);
            toward = direction * gust;
        } else if (field.params.x > 1.5) {
            precise vec3 around = vec3(
                direction.y * offset.z - direction.z * offset.y,
                direction.z * offset.x - direction.x * offset.z,
                direction.x * offset.y - direction.y * offset.x);
            toward = sim_normalize(around);
        } else if (field.params.x > 0.5) {
            toward = sim_normalize(offset);
        }
        total = total + toward * field.direction.w * fade;
    }
    return total;
}
