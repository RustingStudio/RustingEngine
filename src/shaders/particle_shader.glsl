// Rusting shader language, version 1: the built-ins a `#rusting particle 1`
// shader sees. Fields are only ever added at the end, and a change that
// would break a version 1 shader bumps RUSTING_SHADER_VERSION.
#define RUSTING_SHADER_VERSION 1
// What the engine knows about the particle pixel before the shader runs.
struct ParticleInput {
    // Position on the quad, -1..1 on each axis, (0, 0) at the center.
    vec2 corner;
    // The particle's linear RGBA from the emitter's colors and fades.
    vec4 particle_color;
    // The emitter's sprite: 0 soft, 1 disc, 2 square.
    uint sprite;
    // Seconds since the game started.
    float time;
};
#ifndef RUSTING_CUSTOM_PARTICLE
void particle(ParticleInput i, inout vec4 color) {}
#endif
