// Exponential height fog, shared by the scene and sky shaders. The includer
// declares the `shadow` block, `shadow_map`, the `camera` push constants and
// the `lights` buffer. Fog is off when `shadow.fog.w`, the density, is 0.
//
// Density at height y is fog.w * exp(-falloff * (y - height)); the optical
// depth along a straight ray is integrated in closed form.

// World position of the point at `ndc` and `depth` in clip space.
vec3 fog_unproject(vec2 ndc, float depth) {
    vec4 point = shadow.inverse_view_projection * vec4(ndc, depth, 1.0);
    return point.xyz / point.w;
}

// The exponent is clamped so a camera far below the fog height does not
// overflow to infinity.
float fog_density_at(float y) {
    return shadow.fog.w
        * exp(min(-shadow.fog_shape.y * (y - shadow.fog_shape.x), 80.0));
}

float fog_optical_depth(vec3 start, vec3 end) {
    float len = length(end - start);
    float climb = shadow.fog_shape.y * (end.y - start.y);
    // Near-level rays and uniform fog: the midpoint density is exact to
    // second order.
    if (abs(climb) < 1e-3) {
        return fog_density_at(0.5 * (start.y + end.y)) * len;
    }
    return (fog_density_at(start.y) - fog_density_at(end.y)) * len / climb;
}

// Share of the sun reaching the fog between `start` and `end`, from one
// hard shadow-map tap at each of 16 points, offset per pixel so the steps
// do not band. 1 without directional shadows or light shafts.
// ponytail: an unweighted average along the ray, 16 taps per shaded
// pixel; march froxels once per frame if overdraw makes this costly.
float fog_sun_visibility(vec3 start, vec3 end) {
    int count = int(shadow.cascade_info.x);
    if (shadow.fog_shafts.x <= 0.0 || count == 0
        || shadow.cascade_info.z > 0.5) {
        return 1.0;
    }
    int tiles = int(shadow.cascade_info.y);
    float offset = fract(52.9829189
        * fract(dot(gl_FragCoord.xy, vec2(0.06711056, 0.00583715))));
    float seen = 0.0;
    for (int i = 0; i < 16; i++) {
        vec3 point = mix(start, end, (float(i) + offset) / 16.0);
        float depth = dot(point, shadow.cascade_forward.xyz)
            - shadow.cascade_forward.w;
        int cascade = 0;
        while (cascade < count - 1 && depth > shadow.cascade_far[cascade]) {
            cascade += 1;
        }
        vec4 clip = shadow.cascades[cascade] * vec4(point, 1.0);
        vec3 coords = clip.xyz / clip.w;
        vec2 uv = coords.xy * 0.5 + 0.5;
        if (depth > shadow.cascade_far[count - 1] || coords.z > 1.0
            || any(lessThan(uv, vec2(0.0))) || any(greaterThan(uv, vec2(1.0)))) {
            seen += 1.0;
            continue;
        }
        vec2 tile = vec2(cascade % tiles, cascade / tiles);
        seen += texture(shadow_map,
            vec3((tile + uv) / float(tiles), coords.z));
    }
    return mix(1.0, seen / 16.0, shadow.fog_shafts.x);
}

// Light the fog sends toward the eye along `direction`: its color, plus the
// sun (the shadowed directional light, else the first one) near its disc,
// times `sunlit`, the share of the ray the sun reaches.
vec3 fog_inscatter(vec3 direction, float sunlit) {
    vec3 color = shadow.fog.rgb;
    uint sun = camera.light_info.y > 0u ? camera.light_info.y - 1u : 0u;
    if (shadow.fog_shape.z > 0.0 && sun < camera.light_info.x
        && lights.data[sun].position_kind.w < 0.5) {
        Light light = lights.data[sun];
        float toward = max(
            dot(direction, -normalize(light.direction_range.xyz)), 0.0);
        color += light.color_intensity.rgb * light.color_intensity.w
            * shadow.fog_shape.z * pow(toward, 8.0) * sunlit;
    }
    return color;
}

// Fog between the camera and `position`: in-scattered light in rgb, the
// share of the surface that still shows through in a.
vec4 fog_at(vec3 position) {
    if (shadow.fog.w <= 0.0) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }
    vec4 clip = camera.view_projection * vec4(position, 1.0);
    vec3 start = fog_unproject(clip.xy / clip.w, 0.0);
    vec3 ray = position - start;
    float transmittance = exp(-fog_optical_depth(start, position));
    return vec4(
        fog_inscatter(ray / max(length(ray), 1e-6),
            fog_sun_visibility(start, position)) * (1.0 - transmittance),
        transmittance
    );
}

// Fog in front of the background at `ndc`, which lies infinitely far away,
// scaled by the fog's sky affect.
vec4 fog_sky(vec2 ndc) {
    vec3 start = fog_unproject(ndc, 0.0);
    vec3 direction = normalize(fog_unproject(ndc, 0.5) - start);
    // Only rays that climb out of thinning fog see finite fog.
    float climb = shadow.fog_shape.y * direction.y;
    float transmittance = climb > 1e-4
        ? exp(-fog_density_at(start.y) / climb)
        : 0.0;
    transmittance = mix(1.0, transmittance, shadow.fog_shape.w);
    return vec4(
        fog_inscatter(direction, fog_sun_visibility(start,
            start + direction * shadow.cascade_far[
                max(int(shadow.cascade_info.x) - 1, 0)]))
            * (1.0 - transmittance),
        transmittance
    );
}
