// ABI shared by the built-in GPU physics shader and custom condition
// shaders (`GpuConditionShader`). Rust mirrors: `GpuBodyState`,
// `RawGpuPhysicsEvent`, and `PhysicsPushConstants`. Changing a layout here
// is a breaking change for every custom shader: bump the version below and
// `GPU_PHYSICS_ABI_VERSION` in `src/runtime/hybrid_physics.rs` together.
// A custom shader can pin the ABI it was written for with
// `#if RUSTING_PHYSICS_ABI_VERSION != 1` / `#error` / `#endif`.
#define RUSTING_PHYSICS_ABI_VERSION 1

struct PhysicsState {
    mat4 model;
    // w = 1.0 when the body touched a collider or another body in the last
    // step.
    vec4 velocity;
    vec4 angular_velocity;
    // x = mass, y = gravity scale, z = kind (0 fixed, 1 dynamic, 2 kinematic),
    // w = custom solver id (`SOLVER_ID` in a custom solver, else 0).
    vec4 properties;
    vec4 custom_values;
    // x = PhysicsId slot, y = generation, z = rule offset, w = rule count.
    uvec4 metadata;
};
struct PhysicsEvent {
    uvec4 header;
    uvec4 timing;
    vec4 payload;
};
// One queued `GpuBodyCommand` (Rust mirror: `GpuCommandUpload`). header:
// x = body state index, y = kind, z = expected generation. Only the built-in
// shader binds the command buffer; commands apply before a step integrates.
struct BodyCommand {
    uvec4 header;
    vec4 values[4];
};
const uint COMMAND_TELEPORT = 0u;          // values = model matrix columns
const uint COMMAND_SET_VELOCITY = 1u;      // values[0].xyz linear, [1].xyz angular
const uint COMMAND_IMPULSE = 2u;           // values[0].xyz
const uint COMMAND_FORCE = 3u;             // values[0].xyz, for one tick
const uint COMMAND_SET_CUSTOM_VALUES = 4u; // values[0]

layout(set = 0, binding = 0) buffer PhysicsStates { PhysicsState data[]; } bodies;
// `contacts` is written by the built-in body-contact passes, summed over the
// frame's steps: x = bodies whose grid cell was full, y = bodies too big for
// one cell (both go to the fallback list), z = neighbours visited only
// because two cells share a hash, w = pair tests through the fallback list.
layout(set = 0, binding = 3) buffer EventHeader { uint count; uint overflow; uvec2 reserved; uvec4 contacts; } event_header;
layout(set = 0, binding = 4) buffer Events { PhysicsEvent data[]; } events;

layout(push_constant) uniform PhysicsPush {
    float dt;
    float elapsed; // Seconds simulated by the end of this tick: tick * dt.
    uint body_count;
    uint event_capacity;
    uint tick_low;
    uint tick_high;
    float gravity_x;
    float gravity_y;
    float gravity_z;
    uint command_count;
    // Entries in the built-in shader's collider list (binding 7).
    uint collider_count;
    // Edge of one cell in the built-in body-contact grid, in meters.
    float grid_cell_size;
    // Index of this step's first command; see `command_count`.
    uint command_first;
    // Entries in the force-field list (binding 14).
    uint field_count;
    // Bits 0-2: no movement along world x, y, z; bits 3-5: no turning
    // about them (`PhysicsSettings::gpu_locks`).
    uint lock_mask;
    // `GpuSimulationLod`; appended, so shaders written before it still
    // read the fields above unchanged. No observers means no LOD.
    uint observer_count;
    float lod_full_squared;
    float lod_reduced_squared;
    uint lod_stride;
} pc;

// 1.0 for each free axis and 0.0 for each locked one, from bit `first` of
// `pc.lock_mask`: 0 for linear axes, 3 for angular ones.
vec3 axis_keep(uint first) {
    return vec3(equal((uvec3(pc.lock_mask) >> uvec3(first, first + 1u, first + 2u)) & 1u, uvec3(0u)));
}

// Appends one event for `body`. `event_id` is a `GpuEventId` registered on
// the CPU; `payload_kind` and `payload` reach Rust unchanged. Events past the
// frame's capacity are counted, never written out of bounds.
void emit_event(PhysicsState body, uint event_id, uint payload_kind, vec4 payload) {
    uint event_index = atomicAdd(event_header.count, 1u);
    if (event_index >= pc.event_capacity) {
        atomicAdd(event_header.overflow, 1u);
        return;
    }
    events.data[event_index].header = uvec4(body.metadata.x, body.metadata.y, event_id, 0u);
    events.data[event_index].timing = uvec4(pc.tick_low, pc.tick_high, payload_kind, 0u);
    events.data[event_index].payload = payload;
}
