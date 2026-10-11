// Rusting shader language, version 1: the built-ins a `#rusting fog 1`
// shader sees. Fields are only ever added at the end, and a change that
// would break a version 1 shader bumps RUSTING_SHADER_VERSION.
#define RUSTING_SHADER_VERSION 1
// The view ray the fog covers.
struct FogInput {
    // World position where the ray leaves the camera.
    vec3 start;
    // Unit world-space direction of the ray.
    vec3 direction;
    // Metres from start to the surface; 10000 for the background.
    float distance;
    // True for the background, which lies infinitely far away.
    bool background;
    // Seconds since the game started.
    float time;
};
#ifndef RUSTING_CUSTOM_FOG
void fog(FogInput i, inout vec4 haze) {}
#endif
