#version 450
layout(location = 0) in vec3 v_normal;
layout(location = 1) in vec4 v_color;
layout(location = 2) in vec3 v_world_position;
layout(location = 3) flat in uvec3 v_alpha;
layout(location = 4) in vec2 v_uv;
layout(location = 5) in vec4 v_tangent;
layout(location = 6) flat in vec4 v_emissive;
layout(location = 7) flat in vec4 v_surface;
layout(location = 8) flat in vec4 v_transmission;
// Repeats of each terrain layer per unit of v_uv.
layout(location = 9) flat in vec4 v_splat_scale;
// Terrain: the material color, multiplied into the blended layers.
layout(location = 10) flat in vec3 v_tint;
// Water: x depth fade, y shore foam width, z ripple strength, w ripple
// length; v_splat_scale holds the deep color.
layout(location = 11) flat in vec4 v_water;
layout(location = 0) out vec4 f_color;
layout(set = 1, binding = 0) uniform sampler2D base_color_texture;
layout(set = 1, binding = 1) uniform sampler2D normal_texture;
layout(set = 1, binding = 2) uniform sampler2D metallic_roughness_texture;
layout(set = 1, binding = 3) uniform sampler2D occlusion_texture;
layout(set = 1, binding = 4) uniform sampler2D emissive_texture;
// Terrain layers: base color, and normal (rgb) with roughness (alpha), one
// array layer each. Bound for splat materials only.
layout(set = 1, binding = 5) uniform sampler2DArray terrain_colors;
layout(set = 1, binding = 6) uniform sampler2DArray terrain_normals;
const float PI = 3.14159265;
layout(push_constant) uniform Camera {
    mat4 view_projection;
    vec4 eye;
    vec4 ambient;
    vec4 ground_ambient;
    uvec4 light_info;
} camera;
struct Light {
    vec4 position_kind;
    vec4 direction_range;
    vec4 color_intensity;
    vec4 spot_angles;
    vec4 cookie;
};
layout(set = 0, binding = 2) readonly buffer Lights {
    Light data[];
} lights;
layout(set = 2, binding = 0) uniform sampler2DShadow shadow_map;
layout(set = 2, binding = 1) readonly buffer Shadow {
    vec4 environment;
    vec4 scene_color;
    vec4 viewport;
    vec4 probes[8];
    mat4 inverse_view_projection;
    vec4 fog;
    vec4 fog_shape;
    vec4 ambient_occlusion;
    mat4 cascades[6];
    vec4 cascade_far;
    vec4 cascade_forward;
    vec4 cascade_info;
    vec4 cluster_view;
    vec4 cluster_depth;
    vec4 fog_shafts;
    mat4 fog_volumes[8];
    vec4 fog_volume_colors[8];
    vec4 sky[4];
    vec4 decal_info;
    mat4 decals[16];
    vec4 decal_axes[16];
    vec4 decal_colors[16];
    // Roughness in x, painted when y is 1.
    vec4 decal_surfaces[16];
} shadow;
layout(set = 2, binding = 2) uniform sampler2D environment_map;
layout(set = 2, binding = 3) uniform sampler2D scene_color;
// Mip 0 of the depth pyramid: the opaque depth the reflection march tests.
layout(set = 2, binding = 4) uniform sampler2D scene_depth;
// Six faces per reflection probe; see PROBE_FACES.
layout(set = 2, binding = 5) uniform sampler2DArray probe_maps;
// Screen-space ambient occlusion per target pixel when
// shadow.ambient_occlusion.x is 1.
layout(set = 2, binding = 6) uniform sampler2D ambient_occlusion;
// Per light cluster an (offset, count) pair, then the light indices; see
// `build_light_clusters`.
layout(set = 2, binding = 7) readonly buffer Clusters {
    uint data[];
} clusters;
// Spot light cookie images; Light.cookie.w picks one. Matches
// MAX_LIGHT_COOKIES.
layout(set = 2, binding = 8) uniform sampler2D cookies[4];
// The cookie image in `slot` at `uv`. Constant indices only: the slot is
// not uniform across a draw.
vec3 cookie_at(int slot, vec2 uv) {
    if (slot == 0) return texture(cookies[0], uv).rgb;
    if (slot == 1) return texture(cookies[1], uv).rgb;
    if (slot == 2) return texture(cookies[2], uv).rgb;
    return texture(cookies[3], uv).rgb;
}
// Light bounced off nearby surfaces per target pixel when
// shadow.ambient_occlusion.z is 1.
layout(set = 2, binding = 10) uniform sampler2D indirect_light;
// Light probe grid: center and probe count (0 for none), half size and
// intensity, probes per axis, then six rows of irradiance over pi per probe
// (+X, -X, +Y, -Y, +Z, -Z), x fastest, then y, then z.
layout(set = 2, binding = 11) readonly buffer LightProbes {
    vec4 header[3];
    vec4 cubes[];
} light_probes;
// Baked lightmaps, laid out as `lightmap.rs` describes.
layout(set = 2, binding = 12) readonly buffer Lightmaps {
    vec4 data[];
} lightmaps;
// Decal images; decal_axes[i].w picks one. Matches MAX_DECAL_IMAGES.
layout(set = 2, binding = 9) uniform sampler2D decal_images[4];
vec4 decal_at(int slot, vec2 uv) {
    if (slot < 0) return vec4(1.0);
    if (slot == 0) return texture(decal_images[0], uv);
    if (slot == 1) return texture(decal_images[1], uv);
    if (slot == 2) return texture(decal_images[2], uv);
    return texture(decal_images[3], uv);
}
// `color` (and `roughness`, for decals that set one) with every decal whose
// box holds `position` painted over it in order, faded out on surfaces
// turned away from the projection.
vec3 paint_decals(vec3 color, inout float roughness, vec3 position,
    vec3 normal) {
    for (int i = 0; i < int(shadow.decal_info.x); ++i) {
        vec3 p = (shadow.decals[i] * vec4(position, 1.0)).xyz;
        if (any(greaterThan(abs(p), vec3(1.0)))) {
            continue;
        }
        float facing = dot(normal, shadow.decal_axes[i].xyz);
        // Image top along +Y; texture rows run down.
        vec4 paint = decal_at(int(floor(shadow.decal_axes[i].w + 0.5)),
            vec2(p.x, -p.y) * 0.5 + 0.5) * shadow.decal_colors[i];
        float coverage =
            clamp(paint.a, 0.0, 1.0) * smoothstep(0.1, 0.3, facing);
        color = mix(color, paint.rgb, coverage);
        roughness = mix(roughness, shadow.decal_surfaces[i].x,
            coverage * shadow.decal_surfaces[i].y);
    }
    return color;
}
#include "fog.glsl"
#include "surface.glsl"
// 0 opaque, 1 blended, 2 screen-space reflection overlay on opaque surfaces.
layout(constant_id = 0) const uint PASS = 0u;
// Matches SSR_MAX_ROUGHNESS.
const float SSR_MAX_ROUGHNESS = 0.5;
// Light arriving from `direction`: the equirectangular environment map at
// mip `lod` (image top is +Y), else the ambient hemisphere.
vec3 environment(vec3 direction, float lod) {
    if (shadow.environment.x > 0.5) {
        vec2 uv = vec2(
            atan(direction.z, direction.x) / (2.0 * PI) + 0.5,
            acos(clamp(direction.y, -1.0, 1.0)) / PI
        );
        // Keep the poles off the opposite edge a repeating sampler wraps to,
        // measured on the coarser of the two mips a trilinear lookup blends.
        float half_texel =
            0.5 / float(textureSize(environment_map, int(ceil(lod))).y);
        uv.y = clamp(uv.y, half_texel, 1.0 - half_texel);
        return textureLod(environment_map, uv, lod).rgb * shadow.environment.y;
    }
    return mix(
        camera.ground_ambient.rgb,
        camera.ambient.rgb,
        direction.y * 0.5 + 0.5
    );
}
const vec3 PROBE_FORWARD[6] = vec3[](
    vec3(1.0, 0.0, 0.0), vec3(-1.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0),
    vec3(0.0, -1.0, 0.0), vec3(0.0, 0.0, 1.0), vec3(0.0, 0.0, -1.0)
);
const vec3 PROBE_UP[6] = vec3[](
    vec3(0.0, 1.0, 0.0), vec3(0.0, 1.0, 0.0), vec3(0.0, 0.0, 1.0),
    vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 0.0), vec3(0.0, 1.0, 0.0)
);
// Light arriving at `position` from `direction`: the environment at mip
// `lod`, overlaid by the reflection probes whose box holds the position.
// Each probe traces `direction` to its box wall and looks up the captured
// face toward that point; `rough` in 0..1 picks the probe mip.
vec3 surroundings(vec3 position, vec3 direction, float lod, float rough) {
    vec3 sky = environment(direction, lod);
    vec4 sum = vec4(0.0);
    for (int i = 0; i < int(shadow.environment.w); ++i) {
        vec3 center = shadow.probes[2 * i].xyz;
        vec3 extents = shadow.probes[2 * i + 1].xyz;
        vec3 local = position - center;
        vec3 inside = extents - abs(local);
        float margin = min(inside.x, min(inside.y, inside.z));
        if (margin <= 0.0) {
            continue;
        }
        vec3 toward = step(0.0, direction) * 2.0 - 1.0;
        vec3 wall = (extents - local * toward)
            / max(abs(direction), vec3(1e-5));
        vec3 d = local + direction * min(wall.x, min(wall.y, wall.z));
        vec3 a = abs(d);
        int face = a.x >= a.y && a.x >= a.z ? (d.x > 0.0 ? 0 : 1)
            : a.y >= a.z ? (d.y > 0.0 ? 2 : 3)
            : (d.z > 0.0 ? 4 : 5);
        vec3 forward = PROBE_FORWARD[face];
        vec3 up = PROBE_UP[face];
        vec2 uv = vec2(dot(d, cross(forward, up)), -dot(d, up))
            / dot(d, forward) * 0.5 + 0.5;
        vec3 light = textureLod(
            probe_maps,
            vec3(uv, float(6 * i + face)),
            rough * shadow.probes[2 * i + 1].w
        ).rgb * shadow.probes[2 * i].w;
        // Fades in over the outer tenth of the box.
        float weight = clamp(
            margin / (0.1 * min(extents.x, min(extents.y, extents.z))),
            0.0,
            1.0
        );
        sum += vec4(light * weight, weight);
    }
    return sum.w > 0.0 ? mix(sky, sum.rgb / sum.w, min(sum.w, 1.0)) : sky;
}
// Diffuse light at `position` toward `normal` from the light probe grid,
// blended trilinearly between the eight nearest probes, with its weight in
// w: 0 outside the grid, fading in over the outer tenth of the box.
vec4 probe_light(vec3 position, vec3 normal) {
    if (light_probes.header[0].w < 0.5) {
        return vec4(0.0);
    }
    vec3 extents = max(light_probes.header[1].xyz, vec3(1e-4));
    vec3 local = position - light_probes.header[0].xyz;
    vec3 inside = extents - abs(local);
    float margin = min(inside.x, min(inside.y, inside.z));
    if (margin <= 0.0) {
        return vec4(0.0);
    }
    ivec3 count = ivec3(light_probes.header[2].xyz);
    vec3 cell = (local / extents * 0.5 + 0.5) * vec3(count - 1);
    ivec3 low = clamp(ivec3(floor(cell)), ivec3(0), max(count - 2, ivec3(0)));
    vec3 t = clamp(cell - vec3(low), 0.0, 1.0);
    vec3 n2 = normal * normal;
    ivec3 face = ivec3(normal.x < 0.0 ? 1 : 0, normal.y < 0.0 ? 3 : 2,
        normal.z < 0.0 ? 5 : 4);
    vec3 sum = vec3(0.0);
    for (int corner = 0; corner < 8; ++corner) {
        ivec3 step_to = ivec3(corner & 1, (corner >> 1) & 1, (corner >> 2) & 1);
        ivec3 probe = min(low + step_to, count - 1);
        vec3 w = mix(1.0 - t, t, vec3(step_to));
        int base = 6 * ((probe.z * count.y + probe.y) * count.x + probe.x);
        sum += w.x * w.y * w.z * (n2.x * light_probes.cubes[base + face.x].rgb
            + n2.y * light_probes.cubes[base + face.y].rgb
            + n2.z * light_probes.cubes[base + face.z].rgb);
    }
    float weight = clamp(
        margin / (0.1 * min(extents.x, min(extents.y, extents.z))), 0.0, 1.0);
    return vec4(sum * light_probes.header[1].w, weight);
}
// Diffuse light baked into lightmap `slot` (plus one, 0 for none) at
// `position` toward `normal`, with its weight in w. Each axis reads the
// box face the normal points to, bilinearly, weighted by the squared
// normal in the object's axes.
vec4 lightmap_light(vec3 position, vec3 normal, uint slot) {
    if (slot == 0u || float(slot) > lightmaps.data[0].x) {
        return vec4(0.0);
    }
    uint header = 4u * slot - 3u;
    vec4 info = lightmaps.data[header + 3u];
    int res = int(info.x);
    if (res == 0) {
        return vec4(0.0);
    }
    vec3 local;
    vec3 axes;
    for (int a = 0; a < 3; ++a) {
        vec4 row = lightmaps.data[header + uint(a)];
        local[a] = dot(row, vec4(position, 1.0));
        axes[a] = dot(normal, normalize(row.xyz));
    }
    vec3 sum = vec3(0.0);
    for (int a = 0; a < 3; ++a) {
        int face = 2 * a + (axes[a] >= 0.0 ? 0 : 1);
        vec2 st = clamp(vec2(local[(a + 1) % 3], local[(a + 2) % 3]), 0.0,
            1.0) * float(res) - 0.5;
        ivec2 low = ivec2(floor(st));
        vec2 f = st - vec2(low);
        ivec2 a0 = clamp(low, 0, res - 1);
        ivec2 a1 = clamp(low + 1, 0, res - 1);
        int first = int(info.y) + face * res * res;
        vec3 texel = mix(
            mix(lightmaps.data[first + a0.y * res + a0.x].rgb,
                lightmaps.data[first + a0.y * res + a1.x].rgb, f.x),
            mix(lightmaps.data[first + a1.y * res + a0.x].rgb,
                lightmaps.data[first + a1.y * res + a1.x].rgb, f.x),
            f.y);
        sum += axes[a] * axes[a] * texel;
    }
    return vec4(sum / max(dot(axes, axes), 1e-4), 1.0);
}
// Target pixel of a point in normalized device coordinates.
vec2 screen_pixel(vec2 ndc) {
    return shadow.viewport.xy + (ndc * 0.5 + 0.5) * shadow.viewport.zw;
}
float scene_depth_at(vec2 ndc) {
    return texelFetch(scene_depth, ivec2(screen_pixel(ndc)), 0).r;
}
// Screen-space reflection: marches `direction` from the fragment through the
// opaque depth copy. Returns the opaque color copy where the ray crosses the
// depth surface, weighted down near the screen edge, at the end of the
// march, and toward SSR_MAX_ROUGHNESS; weight 0 on a miss.
// ponytail: linear march with growing steps and a binary refine; march the
// depth pyramid (Hi-Z) if long rays skip thin objects.
vec4 trace_reflection(vec3 direction, float roughness) {
    const int STEPS = 64;
    vec3 position = v_world_position;
    vec4 start = camera.view_projection * vec4(position, 1.0);
    float previous = start.z / start.w;
    float step_length = 0.02 * max(start.w, 1.0);
    for (int i = 0; i < STEPS; ++i) {
        vec3 next = position + direction * step_length;
        vec4 clip = camera.view_projection * vec4(next, 1.0);
        if (clip.w <= 0.0) {
            break;
        }
        vec3 ndc = clip.xyz / clip.w;
        if (any(greaterThan(abs(ndc.xy), vec2(1.0))) || ndc.z < 0.0
            || ndc.z > 1.0) {
            break;
        }
        float scene = scene_depth_at(ndc.xy);
        // A hit crosses the depth surface within this step; a ray that is
        // far behind it passed behind a foreground object and marches on.
        if (i > 0 && ndc.z > scene
            && ndc.z - scene <= 2.0 * (ndc.z - previous) + 1e-5) {
            vec3 low = position;
            vec3 high = next;
            for (int j = 0; j < 5; ++j) {
                vec3 middle = (low + high) * 0.5;
                vec4 c = camera.view_projection * vec4(middle, 1.0);
                if (c.z / c.w > scene_depth_at(c.xy / c.w)) {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            vec4 c = camera.view_projection * vec4(high, 1.0);
            vec2 hit = clamp(c.xy / c.w, -1.0, 1.0);
            vec2 edge = 1.0 - smoothstep(0.8, 1.0, abs(hit));
            float weight = edge.x * edge.y
                * (1.0 - smoothstep(0.75, 1.0, float(i) / float(STEPS)))
                * (1.0 - smoothstep(0.5 * SSR_MAX_ROUGHNESS, SSR_MAX_ROUGHNESS,
                    roughness));
            vec3 color = textureLod(
                scene_color,
                screen_pixel(hit) / vec2(textureSize(scene_color, 0)),
                roughness * shadow.scene_color.y
            ).rgb;
            return vec4(color, weight);
        }
        previous = ndc.z;
        position = next;
        step_length *= 1.08;
    }
    return vec4(0.0);
}
float shadow_factor(vec3 surface_normal) {
    // Directional lights pick the first cascade that reaches this depth,
    // point lights the cube face along the major axis away from the light;
    // each view is one tile of the atlas.
    int count = int(shadow.cascade_info.x);
    int tiles = int(shadow.cascade_info.y);
    int cascade = 0;
    if (count == 0) {
        return 1.0;
    }
    if (shadow.cascade_info.z > 0.5) {
        vec3 away = v_world_position - shadow.cascade_forward.xyz;
        vec3 size = abs(away);
        if (size.x >= size.y && size.x >= size.z) {
            cascade = away.x > 0.0 ? 0 : 1;
        } else if (size.y >= size.z) {
            cascade = away.y > 0.0 ? 2 : 3;
        } else {
            cascade = away.z > 0.0 ? 4 : 5;
        }
    } else {
        float depth = dot(v_world_position, shadow.cascade_forward.xyz)
            - shadow.cascade_forward.w;
        while (cascade < count - 1 && depth > shadow.cascade_far[cascade]) {
            cascade += 1;
        }
        if (depth > shadow.cascade_far[count - 1]) {
            return 1.0;
        }
    }
    // One texel in the cascade's own 0..1 coordinates.
    vec2 texel = float(tiles) / vec2(textureSize(shadow_map, 0));
    // Normal offset: sample from two shadow texels off the surface. The PCF
    // taps reach one texel sideways, where a sloped surface's own depth is
    // nearer the light; without the offset it shadows itself in stripes.
    // Row 0 of the light matrix scales world units to clip x; dividing by
    // w (1 for the orthographic sun, the distance for a spot light) gives
    // the texel size at this surface.
    mat4 light = shadow.cascades[cascade];
    vec4 surface_clip = light * vec4(v_world_position, 1.0);
    if (surface_clip.w <= 0.0) {
        return 1.0;
    }
    float texel_world = 2.0 * texel.x * surface_clip.w
        / length(vec3(light[0][0], light[1][0], light[2][0]));
    vec3 position = v_world_position + surface_normal * 2.0 * texel_world;
    vec4 clip = light * vec4(position, 1.0);
    vec3 coords = clip.xyz / clip.w;
    if (coords.z > 1.0) {
        return 1.0;
    }
    vec2 uv = coords.xy * 0.5 + 0.5;
    vec2 tile = vec2(cascade % tiles, cascade / tiles);
    // Taps stay half a texel inside the tile, off its neighbours.
    #define SHADOW_TAP(offset) texture(shadow_map, vec3( \
        (tile + clamp(uv + (offset) * texel, texel * 0.5, 1.0 - texel * 0.5)) \
            / float(tiles), coords.z))
    if (shadow.ambient_occlusion.y > 0.5) {
        return SHADOW_TAP(vec2(0.0));
    }
    float lit = 0.0;
    for (int x = -1; x <= 1; ++x) {
        for (int y = -1; y <= 1; ++y) {
            lit += SHADOW_TAP(vec2(x, y));
        }
    }
    return lit / 9.0;
}
void main() {
    vec4 base_color = v_color * texture(base_color_texture, v_uv);
    // Terrain normal (rgb) and roughness (alpha), blended like the colors.
    vec4 terrain_bump = vec4(0.5, 0.5, 1.0, 1.0);
    // transmission.w is 1 for a terrain splat material, 2 for water.
    bool terrain = abs(v_transmission.w - 1.0) < 0.5;
    if (terrain) {
        // Terrain: blend the layer arrays by the vertex color's weights.
        // Each layer repeats with the sampler's wrap, so mips and
        // anisotropic filtering stay seamless; gradients are taken here,
        // outside the branch on the weight.
        vec4 weights = max(v_color, vec4(0.0));
        weights /= max(dot(weights, vec4(1.0)), 1e-4);
        vec2 dx = dFdx(v_uv);
        vec2 dy = dFdy(v_uv);
        base_color = vec4(0.0);
        if (v_surface.z > 0.5) {
            terrain_bump = vec4(0.0);
        }
        for (int layer = 0; layer < 4; ++layer) {
            if (weights[layer] <= 0.0) {
                continue;
            }
            float scale = v_splat_scale[layer];
            vec3 at = vec3(v_uv * scale, float(layer));
            base_color += weights[layer]
                * textureGrad(terrain_colors, at, dx * scale, dy * scale);
            if (v_surface.z > 0.5) {
                terrain_bump += weights[layer]
                    * textureGrad(terrain_normals, at, dx * scale, dy * scale);
            }
        }
        base_color = vec4(base_color.rgb * v_tint, 1.0);
    }
    // Flat shading's face normal, taken before any discard or branch:
    // derivatives are undefined once a pixel of the 2x2 quad has left, and
    // some drivers then return garbage.
    vec3 face = cross(dFdx(v_world_position), dFdy(v_world_position));
    vec3 normal = normalize(v_normal);
    // Flat shading: the triangle's face normal, turned to the side the
    // vertex normal points to. A sliver too thin to give one keeps the
    // vertex normal instead of normalizing zero into NaN.
    if (v_surface.w > 0.5 && dot(face, face) > 1e-20) {
        face = normalize(face);
        normal = dot(face, normal) < 0.0 ? -face : face;
    }
    if (v_surface.z > 0.5) {
        vec3 tangent = normalize(
            v_tangent.xyz - normal * dot(normal, v_tangent.xyz)
        );
        vec3 bitangent = cross(normal, tangent) * v_tangent.w;
        vec3 sampled = (terrain
            ? terrain_bump.xyz : texture(normal_texture, v_uv).xyz) * 2.0 - 1.0;
        normal = normalize(mat3(tangent, bitangent, normal) * sampled);
    }
    vec3 view_dir = camera.eye.w > 0.5
        ? normalize(camera.eye.xyz - v_world_position)
        : normalize(camera.eye.xyz);
    // Water: small scrolling waves on the normal, then color and shore foam
    // by the water between the surface and the opaque scene behind it.
    if (v_transmission.w > 1.5) {
        float time = camera.ambient.w;
        if (v_water.z > 0.0) {
            // Three crossing waves at the deep-water speed sqrt(g / k). They
            // fade out once a pixel spans half a wave, before they alias.
            float k = 6.2831853 / max(v_water.w, 0.01);
            vec2 slope = vec2(0.0);
            for (int i = 0; i < 3; ++i) {
                float angle = 0.4 + 2.1 * float(i);
                vec2 dir = vec2(cos(angle), sin(angle));
                float wave_k = k * (1.0 + 0.37 * float(i));
                float phase = wave_k * dot(dir, v_world_position.xz)
                    - sqrt(9.81 * wave_k) * time;
                slope += dir * cos(phase);
            }
            vec2 footprint = fwidth(v_world_position.xz);
            float fade = clamp(
                1.0 - 2.0 * max(footprint.x, footprint.y) / max(v_water.w, 0.01),
                0.0, 1.0);
            slope *= 0.15 * v_water.z * fade;
            normal = normalize(normal - vec3(slope.x, 0.0, slope.y));
        }
        // The opaque depth is copied before blended draws when some water
        // reads it; scene_color.w says it was.
        if (PASS == 1u && shadow.scene_color.w > 0.5
            && (v_water.x > 0.0 || v_water.y > 0.0)) {
            vec4 clip = camera.view_projection * vec4(v_world_position, 1.0);
            vec2 ndc = clip.xy / clip.w;
            vec4 behind = shadow.inverse_view_projection
                * vec4(ndc, scene_depth_at(ndc), 1.0);
            behind.xyz /= behind.w;
            if (v_water.x > 0.0) {
                // Light crosses more water the longer the view runs under
                // the surface; open water over the far plane is deep.
                float through = length(behind.xyz - v_world_position);
                base_color = mix(base_color, v_splat_scale,
                    clamp(through / v_water.x, 0.0, 1.0));
            }
            if (v_water.y > 0.0) {
                // Foam fills the band where the ground is close under the
                // surface and breaks up toward its edge in a slow pattern.
                float depth = max(v_world_position.y - behind.y, 0.0);
                vec2 p = v_world_position.xz * 3.0;
                float pattern = 0.25 * (sin(p.x + time * 0.7)
                    * sin(p.y * 1.3 - time * 0.5)
                    + sin((p.x + p.y) * 1.7 + time));
                float edge = 1.0 - depth / v_water.y;
                float foam = smoothstep(0.3, 0.6, edge + 0.4 * pattern);
                base_color = mix(base_color, vec4(1.0), foam);
            }
        }
    }
    // glTF packs roughness in green and metallic in blue.
    vec4 packed = texture(metallic_roughness_texture, v_uv);
    Surface s = Surface(base_color, normal, v_surface.x * packed.b,
        v_surface.y * packed.g * terrain_bump.a,
        v_emissive.rgb * texture(emissive_texture, v_uv).rgb);
    // camera.ambient.w carries the time.
    surface(SurfaceInput(v_uv, v_world_position, view_dir, v_color,
        camera.ambient.w), s);
    base_color = s.base_color;
    normal = normalize(s.normal);
    if (v_alpha.x == 1u && base_color.a < uintBitsToFloat(v_alpha.y)) {
        discard;
    }
    float alpha = v_alpha.x == 2u ? base_color.a : 1.0;
    base_color.rgb = paint_decals(base_color.rgb, s.roughness,
        v_world_position, normalize(v_normal));
    // Opacity 0 draws nothing: reflections are not scaled by alpha, so a
    // fully clear surface would otherwise still show a faint mirror.
    if (v_alpha.x == 2u && alpha <= 0.0 && v_transmission.x <= 0.0) {
        discard;
    }
    // Transmitted light comes from the opaque scene copy when the frame has
    // one; without it, transmission only makes the surface see-through.
    float transmission = v_transmission.x;
    bool refracts = transmission > 0.0 && shadow.scene_color.x > 0.5;
    alpha = refracts ? 1.0 : alpha * (1.0 - transmission);
    // light_info.z is the SceneDebugView: 1 unshaded, 2 normals.
    bool unlit = v_emissive.w > 0.5 && camera.light_info.z != 2u;
    if (unlit || camera.light_info.z == 1u) {
        // Fog is off in debug views.
        vec4 haze = fog_at(v_world_position);
        f_color = vec4(base_color.rgb * alpha * haze.a + haze.rgb * alpha, alpha);
        return;
    }
    if (camera.light_info.z == 2u) {
        f_color = vec4((normal * 0.5 + 0.5) * alpha, alpha);
        return;
    }
    float metallic = clamp(s.metallic, 0.0, 1.0);
    float roughness = clamp(s.roughness, 0.04, 1.0);
    float occlusion = texture(occlusion_texture, v_uv).r;
    // Blended surfaces are not in the depth the occlusion was traced from.
    if (PASS != 1u && shadow.ambient_occlusion.x > 0.5) {
        occlusion *= texelFetch(ambient_occlusion, ivec2(gl_FragCoord.xy), 0).r;
    }
    float n_dot_v = max(dot(normal, view_dir), 0.0001);
    vec3 f0 = mix(vec3(0.04), base_color.rgb, metallic);
    // Premultiplied by alpha; specular is not, see the blend pipeline.
    vec3 diffuse_color = base_color.rgb * (1.0 - metallic) * alpha
        * (refracts ? 1.0 - transmission : 1.0);
    float a2 = roughness * roughness * roughness * roughness;
    float k = (roughness + 1.0) * (roughness + 1.0) / 8.0;
    // Hemisphere environment: diffuse from the normal, specular from the
    // reflection direction with roughness-aware Fresnel.
    // ponytail: without an environment map, a two-color hemisphere. With
    // one, mip levels stand in for prefiltered radiance and irradiance.
    vec3 reflected = reflect(-view_dir, normal);
    vec3 env_fresnel = f0 + (max(vec3(1.0 - roughness), f0) - f0)
        * pow(1.0 - n_dot_v, 5.0);
    float last_mip = shadow.environment.z;
    vec3 reflection = surroundings(
        v_world_position, reflected, roughness * last_mip, roughness);
    // Surfaces drawn after the opaque copy see the scene in their
    // reflections. The overlay adds the difference over the opaque pass.
    vec4 traced = vec4(0.0);
    if (PASS != 0u && shadow.scene_color.z > 0.5
        && roughness < SSR_MAX_ROUGHNESS) {
        traced = trace_reflection(reflected, roughness);
    }
    // The overlay's difference is seen through the same fog as the
    // opaque surface under it.
    vec4 haze = fog_at(v_world_position);
    if (PASS == 2u) {
        f_color = vec4(
            (traced.rgb - reflection) * traced.a * env_fresnel * occlusion
                * haze.a,
            0.0
        );
        return;
    }
    reflection = mix(reflection, traced.rgb, traced.a);
    vec3 ambient = surroundings(
        v_world_position, normal, max(last_mip - 1.0, 0.0), 1.0);
    vec4 probed = probe_light(v_world_position, normal);
    ambient = mix(ambient, probed.rgb, probed.w);
    vec4 baked = lightmap_light(v_world_position, normal, v_alpha.z >> 8u);
    ambient = mix(ambient, baked.rgb, baked.w);
    vec3 result = (ambient * diffuse_color * (1.0 - env_fresnel)
        + reflection * env_fresnel)
        * occlusion;
    if (PASS != 1u && shadow.ambient_occlusion.z > 0.5) {
        result += texelFetch(indirect_light, ivec2(gl_FragCoord.xy), 0).rgb
            * diffuse_color * (1.0 - env_fresnel);
    }
    // Directional lights (the first light_info.w) light every pixel; point
    // and spot lights come from this pixel's cluster.
    vec2 cell = (gl_FragCoord.xy - shadow.viewport.xy) / shadow.viewport.zw;
    uvec2 tile = min(uvec2(max(cell, 0.0) * vec2(16.0, 9.0)), uvec2(15u, 8u));
    float view_depth = dot(v_world_position, shadow.cluster_view.xyz)
        - shadow.cluster_view.w;
    uint slice = view_depth <= shadow.cluster_depth.x ? 0u : min(
        uint(log(view_depth / shadow.cluster_depth.x)
            / shadow.cluster_depth.y * 24.0),
        23u
    );
    uint cluster = (slice * 9u + tile.y) * 16u + tile.x;
    uint cluster_first = clusters.data[2u * cluster];
    uint directional = camera.light_info.w;
    uint light_count = directional + clusters.data[2u * cluster + 1u];
    for (uint step = 0; step < light_count; ++step) {
        uint index = step < directional
            ? step
            : clusters.data[cluster_first + step - directional];
        Light light = lights.data[index];
        float kind = light.position_kind.w;
        vec3 to_light;
        float attenuation = 1.0;
        vec3 tint = vec3(1.0);
        // Sphere lights light specular from the point of the sphere
        // nearest the reflection ray, with the lobe widened to keep its
        // energy (Karis 2013, representative point).
        vec3 specular_dir;
        float lobe_energy = 1.0;
        if (kind < 0.5) {
            to_light = normalize(-light.direction_range.xyz);
            specular_dir = to_light;
        } else {
            vec3 delta = light.position_kind.xyz - v_world_position;
            float distance_to_light = length(delta);
            to_light = distance_to_light > 0.0001
                ? delta / distance_to_light
                : vec3(0.0, 1.0, 0.0);
            float range_fade = clamp(
                1.0 - distance_to_light / light.direction_range.w,
                0.0,
                1.0
            );
            attenuation = range_fade * range_fade;
            float source_radius = light.spot_angles.z;
            specular_dir = to_light;
            if (source_radius > 0.0) {
                vec3 to_ray = dot(delta, reflected) * reflected - delta;
                vec3 nearest = delta + to_ray
                    * clamp(source_radius / max(length(to_ray), 0.0001),
                        0.0, 1.0);
                specular_dir = normalize(nearest);
                float lobe = roughness * roughness;
                float widened = clamp(
                    lobe + source_radius / (2.0 * max(distance_to_light, 0.0001)),
                    0.0, 1.0);
                lobe_energy = (lobe / widened) * (lobe / widened);
            }
            if (kind > 1.5) {
                vec3 axis = normalize(light.direction_range.xyz);
                float cone = dot(-to_light, axis);
                attenuation *= smoothstep(
                    light.spot_angles.y,
                    light.spot_angles.x,
                    cone
                );
                if (light.cookie.w > -0.5 && cone > 0.0) {
                    // Projected like a slide: the outer cone spans the
                    // image, its top along the light's up.
                    vec3 right = light.cookie.xyz;
                    vec3 up = cross(right, axis);
                    vec2 spread = vec2(dot(-to_light, right),
                        dot(-to_light, up)) / (cone * light.spot_angles.w);
                    tint = cookie_at(int(light.cookie.w + 0.5),
                        vec2(0.5 + 0.5 * spread.x, 0.5 - 0.5 * spread.y));
                }
            }
        }
        if (index + 1u == camera.light_info.y && (v_alpha.z & 2u) != 0u) {
            attenuation *= shadow_factor(normalize(v_normal));
        }
        float n_dot_l = max(dot(normal, to_light), 0.0);
        if (n_dot_l <= 0.0) {
            continue;
        }
        // Cook-Torrance: GGX distribution, Smith-Schlick geometry,
        // Schlick Fresnel.
        vec3 half_dir = normalize(specular_dir + view_dir);
        float n_dot_h = max(dot(normal, half_dir), 0.0);
        float v_dot_h = max(dot(view_dir, half_dir), 0.0);
        float d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
        float distribution = a2 / (PI * d * d) * lobe_energy;
        float geometry = n_dot_v / (n_dot_v * (1.0 - k) + k)
            * n_dot_l / (n_dot_l * (1.0 - k) + k);
        vec3 fresnel = f0 + (1.0 - f0) * pow(1.0 - v_dot_h, 5.0);
        vec3 specular = distribution * geometry * fresnel
            / (4.0 * n_dot_v * n_dot_l + 0.0001);
        vec3 radiance = light.color_intensity.rgb
            * light.color_intensity.w * attenuation * tint;
        // Light intensity is scaled so a white Lambert surface facing a
        // unit light reflects 1, hence the PI on the specular lobe.
        result += ((1.0 - fresnel) * diffuse_color + specular * PI)
            * radiance * n_dot_l;
    }
    if (refracts) {
        // What is behind, seen along the ray bent at the surface after it
        // crosses `thickness`, tinted by the base color and blurred by
        // roughness.
        // ponytail: one refraction at the entry face, no exit face, and
        // blended objects behind are missing from the copy.
        vec3 refracted = refract(-view_dir, normal, 1.0 / v_transmission.y);
        vec4 clip = camera.view_projection
            * vec4(v_world_position + refracted * v_transmission.z, 1.0);
        vec2 pixel = shadow.viewport.xy
            + (clip.xy / clip.w * 0.5 + 0.5) * shadow.viewport.zw;
        vec3 behind = textureLod(
            scene_color,
            pixel / vec2(textureSize(scene_color, 0)),
            roughness * shadow.scene_color.y
        ).rgb;
        result += behind * base_color.rgb * transmission * (1.0 - env_fresnel);
    }
    result += s.emissive * alpha;
    // Premultiplied like the rest: blended surfaces add fog by coverage.
    // ponytail: refracted and screen-space reflected light comes from the
    // fogged opaque copy, so it is fogged twice over the stretch in front of
    // this surface; trace the fog per sample if dense fog shows it.
    f_color = vec4(result * haze.a + haze.rgb * alpha, alpha);
}
