// Collider shapes shared by the built-in physics shader and the GPU
// body-contact passes. Not part of the custom-shader ABI.

#include <sim_math.glsl>

// Shape encoding: x = kind (0 box, 1 sphere, 2 capsule along local Y,
// 3 none); box yzw = half extents; sphere y = radius; capsule y = half
// height, z = radius. Material x = friction, y = restitution. Layers x =
// memberships, y = filters.
struct BodyShape {
    vec4 shape;
    vec4 material;
    uvec4 layers;
};
layout(set = 0, binding = 6) readonly buffer BodyShapes { BodyShape data[]; } body_shapes;
// `SimulationObserver` positions (xyz), `pc.observer_count` of them.
layout(set = 0, binding = 15) readonly buffer Observers { vec4 data[]; } observers;

// Ticks of time a dynamic body at `position` steps over this tick under
// `GpuSimulationLod`: 1 at full rate, `pc.lod_stride` on its reduced ticks,
// 0 while it waits or is frozen. `slot` (its PhysicsId slot) spreads
// reduced bodies over the stride. Mirrors `GpuSimulationLod::steps`.
uint lod_steps(vec3 position, uint slot) {
    if (pc.observer_count == 0u) return 1u;
    float nearest = 3.4e38;
    // Min is exact and order-free, so observer order does not matter.
    for (uint index = 0u; index < pc.observer_count; index++) {
        precise vec3 offset = position - observers.data[index].xyz;
        nearest = min(nearest, sim_dot(offset, offset));
    }
    if (nearest <= pc.lod_full_squared) return 1u;
    if (nearest > pc.lod_reduced_squared) return 0u;
    return (pc.tick_low + slot) % pc.lod_stride == 0u ? pc.lod_stride : 0u;
}

// How far a shape reaches from its center along world direction `n`.
float support(vec4 shape, mat3 rotation, vec3 n) {
    vec3 local = sim_mul_transposed(rotation, n);
    if (shape.x < 0.5) return sim_dot(abs(local), shape.yzw);
    if (shape.x < 1.5) return shape.y;
    precise float reach = shape.z + abs(local.y) * shape.y;
    return reach;
}

// Radius of the sphere around the center that holds the whole shape.
float bounding_radius(vec4 shape) {
    if (shape.x < 0.5) return sim_length(shape.yzw);
    if (shape.x < 1.5) return shape.y;
    precise float radius = shape.y + shape.z;
    return radius;
}

// Rotation of a model matrix with its scale removed.
mat3 rotation_of(mat4 model) {
    return mat3(sim_normalize(model[0].xyz), sim_normalize(model[1].xyz), sim_normalize(model[2].xyz));
}

// Direction from the surface of `shape` (centered at `origin`, turned by
// `rotation`) to `point`, and the signed distance (negative inside).
void shape_distance(vec4 shape, mat3 rotation, vec3 origin, vec3 point, out vec3 normal, out float distance) {
    precise vec3 offset = point - origin;
    vec3 local = sim_mul_transposed(rotation, offset);
    vec3 local_normal = vec3(0.0, 1.0, 0.0);
    if (shape.x < 0.5) {
        vec3 half_extents = shape.yzw;
        precise vec3 outside = abs(local) - half_extents;
        float largest = max(outside.x, max(outside.y, outside.z));
        if (largest > 0.0) {
            precise vec3 delta = local - clamp(local, -half_extents, half_extents);
            distance = sim_length(delta);
            local_normal = sim_div(delta, distance);
        } else {
            // Inside: leave through the nearest face.
            distance = largest;
            if (outside.x == largest) local_normal = vec3(local.x < 0.0 ? -1.0 : 1.0, 0.0, 0.0);
            else if (outside.y == largest) local_normal = vec3(0.0, local.y < 0.0 ? -1.0 : 1.0, 0.0);
            else local_normal = vec3(0.0, 0.0, local.z < 0.0 ? -1.0 : 1.0);
        }
    } else {
        // A sphere is a capsule with no height.
        float half_height = shape.x < 1.5 ? 0.0 : shape.y;
        float radius = shape.x < 1.5 ? shape.y : shape.z;
        precise vec3 delta = local - vec3(0.0, clamp(local.y, -half_height, half_height), 0.0);
        float length_delta = sim_length(delta);
        if (length_delta > 0.000001) local_normal = sim_div(delta, length_delta);
        precise float surface = length_delta - radius;
        distance = surface;
    }
    normal = sim_mul(rotation, local_normal);
}
