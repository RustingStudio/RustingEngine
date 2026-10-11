// Rusting shader language, version 1: the built-ins a `#rusting sky 1`
// shader sees. Fields are only ever added at the end, and a change that
// would break a version 1 shader bumps RUSTING_SHADER_VERSION.
#define RUSTING_SHADER_VERSION 1
// What the engine knows about the background pixel before the shader runs.
struct SkyInput {
    // Unit world-space direction from the camera through the pixel.
    vec3 direction;
    // Seconds since the game started.
    float time;
};
#ifndef RUSTING_CUSTOM_SKY
void sky(SkyInput i, inout vec3 color) {}
#endif
