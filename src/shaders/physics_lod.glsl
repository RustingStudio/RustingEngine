// `GpuSimulationLod` stepping shared by the built-in physics shader and the
// GPU body-contact passes. Needs `physics_abi.glsl` (for `pc`) and
// `sim_math.glsl` first. Not part of the custom-shader ABI.

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
