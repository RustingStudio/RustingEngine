#version 450
layout(input_attachment_index = 0, set = 0, binding = 0) uniform subpassInput scene;
// Mip 0 of the bloom chain, half the target size.
layout(set = 0, binding = 1) uniform sampler2D bloom;
// mapper follows ToneMapper: 0 Linear, 1 Reinhard, 2 ACES. bloom is the
// glow's intensity, 0 when the frame has none; inv_extent is one over the
// target size in pixels.
// contrast, saturation, shadows, highlights and vignette follow
// ColorGrading; the defaults leave the color unchanged. shadows.w is
// ColorGrading::lens_droplets.
// A copy of the HDR image in `scene`, for effects that read other pixels;
// up to date only when `sampled` is 1.
layout(set = 0, binding = 2) uniform sampler2D scene_image;
// Factor auto exposure adapted to; read only when auto_exposure is 1.
layout(set = 0, binding = 3) readonly buffer Exposure { float value; } adapted;
// The ColorLut strip, lut_size squares of lut_size x lut_size texels; read
// only when lut > 0, its intensity.
layout(set = 1, binding = 0) uniform sampler2D lut_strip;
// Opaque depth, up to date only when blur or motion strength is above 0.
layout(set = 1, binding = 1) uniform sampler2D scene_depth;
// MotionBlur: reproject takes this frame's clip space to last frame's;
// strength is the intensity, 0 when off.
layout(set = 1, binding = 2) readonly buffer Motion {
    mat4 reproject;
    float strength;
} motion;
// film: grain, chromatic aberration, scanlines, color bleed. tape: noise
// band, distortion, seconds and frame number from the fixed tick. flags bit
// 0 is set when scene_image may be read, bit 1 for auto exposure, bit 2 for
// FXAA. focus and
// blur follow DepthOfField; depth holds the projection entries [2][2],
// [2][3], [3][2] and [3][3] (row, column) that undo stored depth.
layout(push_constant) uniform ToneMap {
    float exposure;
    uint mapper;
    float bloom;
    float contrast;
    vec2 inv_extent;
    float saturation;
    float vignette;
    vec4 shadows;
    vec4 highlights;
    vec4 film;
    vec4 tape;
    uint flags;
    float lut;
    float focus;
    float blur;
    vec4 depth;
} tone;
layout(location = 0) out vec4 f_color;
// Uniform 0..1 from a pixel and a frame number, the same on every GPU.
float noise(uvec2 pixel, uint frame) {
    uint h = pixel.x * 1973u + pixel.y * 9277u + frame * 26699u;
    h = (h ^ 61u) ^ (h >> 16u);
    h *= 9u;
    h ^= h >> 4u;
    h *= 0x27d4eb2du;
    h ^= h >> 15u;
    return float(h) / 4294967295.0;
}
vec3 hdr_at(vec2 uv) {
    if (any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) {
        return vec3(0.0);
    }
    vec3 c = textureLod(scene_image, uv, 0.0).rgb;
    if (tone.bloom > 0.0) {
        c += textureLod(bloom, uv, 0.0).rgb * tone.bloom;
    }
    return c;
}
// Blur radius at uv as a fraction of the screen height.
float blur_at(vec2 uv) {
    float d = textureLod(scene_depth, uv, 0.0).r;
    float z = (tone.depth.y - d * tone.depth.w) / (d * tone.depth.z - tone.depth.x);
    return min(tone.blur * abs(1.0 - tone.focus / max(-z, 1e-4)), tone.blur);
}
// Gathers a disc the size of this pixel's blur. Nearer samples sharper than
// it would reach this pixel stay out, so focused edges do not smear over a
// blurred background.
// ponytail: blurred foreground does not spread over a sharp background;
// a scatter or tiled max-blur pass would add that.
vec3 defocus(vec2 uv) {
    float radius = blur_at(uv);
    float depth = textureLod(scene_depth, uv, 0.0).r;
    vec2 to_uv = vec2(tone.inv_extent.x / tone.inv_extent.y, 1.0);
    vec3 sum = hdr_at(uv);
    float weight = 1.0;
    if (radius / tone.inv_extent.y < 0.5) {
        return sum;
    }
    for (int i = 0; i < 32; i++) {
        // Golden-angle spiral, evenly covering the disc.
        float r = sqrt((float(i) + 0.5) / 32.0) * radius;
        float a = float(i) * 2.39996;
        vec2 at = uv + vec2(cos(a), sin(a)) * r * to_uv;
        float reach = blur_at(at);
        float w = textureLod(scene_depth, at, 0.0).r < depth
            ? clamp((reach - r) / tone.inv_extent.y + 1.0, 0.0, 1.0)
            : 1.0;
        sum += hdr_at(at) * w;
        weight += w;
    }
    return sum / weight;
}
float luma_of(vec3 c) {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}
// Perceived brightness after exposure, roughly as the tone curve shows it,
// so edges between bright areas count as much as dark ones.
float edge_luma(vec2 uv) {
    float l = luma_of(hdr_at(uv)) * tone.exposure;
    return sqrt(l / (1.0 + l));
}
// FXAA in the style of Lottes' console version: four diagonal lumas give
// the edge direction, then two short and two long taps along the edge
// blend it; the long blend is dropped when it leaves the local luma range.
vec3 fxaa(vec2 uv) {
    vec2 px = tone.inv_extent;
    float nw = edge_luma(uv + vec2(-0.5, -0.5) * px);
    float ne = edge_luma(uv + vec2(0.5, -0.5) * px);
    float sw = edge_luma(uv + vec2(-0.5, 0.5) * px);
    float se = edge_luma(uv + vec2(0.5, 0.5) * px);
    vec3 center = hdr_at(uv);
    float m = sqrt(luma_of(center) * tone.exposure / (1.0 + luma_of(center) * tone.exposure));
    float lo = min(m, min(min(nw, ne), min(sw, se)));
    float hi = max(m, max(max(nw, ne), max(sw, se)));
    if (hi - lo < max(0.0625, hi * 0.125)) {
        return center;
    }
    vec2 dir = vec2(-((nw + ne) - (sw + se)), (nw + sw) - (ne + se));
    float reduce = max((nw + ne + sw + se) * 0.03125, 1.0 / 128.0);
    dir = clamp(dir / (min(abs(dir.x), abs(dir.y)) + reduce), -8.0, 8.0) * px;
    vec3 near = 0.5 * (hdr_at(uv + dir * (1.0 / 3.0 - 0.5))
        + hdr_at(uv + dir * (2.0 / 3.0 - 0.5)));
    vec3 far = near * 0.5 + 0.25 * (hdr_at(uv - dir * 0.5) + hdr_at(uv + dir * 0.5));
    float l = luma_of(far) * tone.exposure;
    l = sqrt(l / (1.0 + l));
    return l < lo || l > hi ? near : far;
}
// Rain on the lens: two grids of cells, each holding at most one round drop
// that lives a few seconds at a random spot and shows the image behind it
// flipped, like a small lens, with a darker rim. strength is the share of
// cells with a drop; everything follows seconds, so a tick repeats.
vec3 lens_droplets(vec2 uv, vec3 color, float strength, float seconds) {
    vec2 aspect = vec2(tone.inv_extent.y / tone.inv_extent.x, 1.0);
    for (uint layer = 0u; layer < 2u; layer++) {
        float cells = layer == 0u ? 6.0 : 11.0;
        vec2 p = uv * aspect * cells;
        uvec2 id = uvec2(floor(p)) + uvec2(layer * 977u, 0u);
        float t = seconds / (3.0 + 2.0 * noise(id, 1u)) + noise(id, 2u);
        uint life = uint(t) * 8u;
        if (noise(id, life + 3u) >= strength) {
            continue;
        }
        vec2 center = 0.3 + 0.4 * vec2(noise(id, life + 4u), noise(id, life + 5u));
        float radius = 0.12 + 0.16 * noise(id, life + 6u);
        vec2 d = (fract(p) - center) / radius;
        float r2 = dot(d, d);
        if (r2 >= 1.0) {
            continue;
        }
        float age = fract(t);
        float fade = smoothstep(0.0, 0.05, age) * (1.0 - smoothstep(0.6, 1.0, age));
        vec2 bend = d * radius / (cells * aspect) * 1.6;
        vec3 seen = hdr_at(uv - bend) * (1.0 - 0.5 * smoothstep(0.5, 1.0, r2));
        return mix(color, seen, fade);
    }
    return color;
}
// The post shader hook; see post.glsl.
#include "post.glsl"
void main() {
    vec4 hdr = subpassLoad(scene);
    vec2 uv = gl_FragCoord.xy * tone.inv_extent;
    if ((tone.flags & 1u) != 0u) {
        if (tone.tape.y > 0.0) {
            // Tube bulge, plus rows that sway slowly with time.
            vec2 centered = uv - 0.5;
            uv = 0.5 + centered * (1.0 - tone.tape.y * 0.25
                + tone.tape.y * 0.5 * dot(centered, centered));
            uv.x += tone.tape.y * 0.004
                * sin(uv.y * 60.0 + tone.tape.z * 5.0);
        }
        hdr.rgb = tone.blur > 0.0 ? defocus(uv)
            : (tone.flags & 4u) != 0u ? fxaa(uv) : hdr_at(uv);
        if (tone.shadows.w > 0.0) {
            hdr.rgb = lens_droplets(uv, hdr.rgb, tone.shadows.w, tone.tape.z);
        }
        if (motion.strength > 0.0) {
            // Screen motion since last frame, in uv, centered on the pixel.
            float d = textureLod(scene_depth, uv, 0.0).r;
            vec4 last = motion.reproject * vec4(uv * 2.0 - 1.0, d, 1.0);
            vec2 moved = (uv - (last.xy / last.w * 0.5 + 0.5)) * motion.strength;
            float pixels = length(moved / tone.inv_extent);
            // Longer than a sixth of the height is a cut, not motion.
            if (pixels > 0.5 && pixels * tone.inv_extent.y < 0.17) {
                vec3 sum = vec3(0.0);
                for (int i = 0; i < 16; i++) {
                    sum += hdr_at(uv + moved * (float(i) / 15.0 - 0.5));
                }
                hdr.rgb = sum / 16.0;
            }
        }
        if (tone.film.y > 0.0) {
            vec2 split = (uv - 0.5) * tone.film.y * 0.02;
            hdr.r = hdr_at(uv + split).r;
            hdr.b = hdr_at(uv - split).b;
        }
        if (tone.film.w > 0.0) {
            // Color smears to the right; brightness stays sharp.
            vec2 step_x = vec2(tone.film.w * 8.0 * tone.inv_extent.x, 0.0);
            vec3 smear = (hdr.rgb + hdr_at(uv - step_x) + hdr_at(uv - 2.0 * step_x)
                + hdr_at(uv - 3.0 * step_x)) * 0.25;
            hdr.rgb = max(smear - luma_of(smear) + luma_of(hdr.rgb), 0.0);
        }
    } else if (tone.bloom > 0.0) {
        hdr.rgb += textureLod(bloom, uv, 0.0).rgb * tone.bloom;
    }
    float exposure = tone.exposure;
    if ((tone.flags & 2u) != 0u) {
        exposure *= adapted.value;
    }
    vec3 c = max(hdr.rgb * exposure, 0.0);
    if (tone.mapper == 1u) {
        c = c / (1.0 + c);
    } else if (tone.mapper == 2u) {
        // Narkowicz 2015 fit of the ACES filmic curve.
        c = (c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14);
    }
    c = clamp(c, 0.0, 1.0);
    float luma = dot(c, vec3(0.2126, 0.7152, 0.0722));
    c = max(mix(vec3(luma), c, tone.saturation), 0.0);
    if (tone.contrast != 1.0) {
        // Contrast in log space around linear mid grey.
        c = 0.18 * pow(c / 0.18, vec3(tone.contrast));
    }
    c *= mix(tone.shadows.rgb, tone.highlights.rgb, smoothstep(0.0, 1.0, luma));
    if (tone.vignette > 0.0) {
        vec2 centered = gl_FragCoord.xy * tone.inv_extent - 0.5;
        c *= 1.0 - tone.vignette * smoothstep(0.25, 0.75, length(centered));
    }
    if (tone.lut > 0.0) {
        // Look up in sRGB, as the strip was authored; the texture decodes
        // back to linear. Blue blends two neighboring squares.
        float n = float(textureSize(lut_strip, 0).y);
        vec3 encoded = mix(12.92 * c, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055,
            step(0.0031308, c));
        vec3 at = clamp(encoded, 0.0, 1.0) * (n - 1.0);
        float square = min(floor(at.b), n - 2.0);
        vec2 texel = 1.0 / vec2(n * n, n);
        vec2 uv = (vec2(at.r + square * n, at.g) + 0.5) * texel;
        vec3 graded = mix(
            textureLod(lut_strip, uv, 0.0).rgb,
            textureLod(lut_strip, uv + vec2(n * texel.x, 0.0), 0.0).rgb,
            at.b - square
        );
        c = mix(c, graded, tone.lut);
    }
    uint frame = uint(tone.tape.w);
    if (tone.film.z > 0.0) {
        // Every other pixel row darkens.
        c *= 1.0 - tone.film.z * 0.6 * float(uint(gl_FragCoord.y) & 1u);
    }
    if (tone.tape.x > 0.0) {
        // A band of static an eighth of the height, rolling down once
        // every four seconds.
        float row = gl_FragCoord.y * tone.inv_extent.y;
        float away = abs(fract(row - tone.tape.z * 0.25 + 0.5) - 0.5);
        float band = tone.tape.x * (1.0 - smoothstep(0.0, 0.0625, away));
        float hiss = noise(uvec2(gl_FragCoord.xy) / uvec2(2u, 1u), frame + 7u);
        c = mix(c, vec3(hiss), band * 0.7);
    }
    if (tone.film.x > 0.0) {
        c += tone.film.x * 0.3 * (noise(uvec2(gl_FragCoord.xy), frame) - 0.5);
    }
    post(PostInput(gl_FragCoord.xy * tone.inv_extent, gl_FragCoord.xy,
        tone.inv_extent, tone.tape.z), c);
    f_color = vec4(clamp(c, 0.0, 1.0), hdr.a);
}
