// Rusting shader language, version 1: the built-ins a `#rusting post 1`
// shader sees. Fields are only ever added at the end, and a change that
// would break a version 1 shader bumps RUSTING_SHADER_VERSION.
#define RUSTING_SHADER_VERSION 1
// What the engine knows about the pixel before the shader runs.
struct PostInput {
    // 0..1 across the image, (0, 0) at the top left.
    vec2 uv;
    // Pixel center in pixels, (0.5, 0.5) at the top left.
    vec2 pixel;
    // One over the image size in pixels: the uv step to the next pixel.
    vec2 texel;
    // Seconds of game time from the fixed tick, the same in a replay.
    float time;
};
// Linear HDR scene color at uv, bloom added, before exposure and tone
// mapping; black outside the image.
vec3 scene_at(vec2 uv) {
    return hdr_at(uv);
}
#ifndef RUSTING_CUSTOM_POST
void post(PostInput i, inout vec3 color) {}
#endif
