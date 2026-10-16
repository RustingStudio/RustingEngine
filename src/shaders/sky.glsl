// Procedural daylight sky, shared by the sky bake and the background pass.
//
// Single scattering with one sample per ray: air (Rayleigh, blue) and haze
// (Mie, grey, forward-peaked) scatter the sun toward the eye, and both
// dim the light along the view and sun paths by the relative air mass.

// Kasten-Young relative air mass: 1 at the zenith, about 38 at the horizon.
float sky_air_mass(float up) {
    float c = clamp(up, 0.0, 1.0);
    return 1.0 / (c + 0.50572 * pow(96.07995 - degrees(acos(c)), -1.6364));
}

// Zenith optical depths: air for 8 km of scale height, plus up to 0.4 of
// grey haze.
vec3 sky_air() { return vec3(0.0464, 0.108, 0.265); }
vec3 sky_depth(float haze) {
    return sky_air() + vec3(0.02 + 0.38 * clamp(haze, 0.0, 1.0));
}

// Sunlight left after the trip through the air to the ground, fading out
// as the sun sets.
vec3 sky_sunlight(vec3 sun, vec3 sun_color, float haze) {
    return sun_color * exp(-sky_depth(haze) * sky_air_mass(sun.y))
        * smoothstep(-0.1, 0.05, sun.y);
}

// Light from the sky toward `-direction` for a sun at `sun` (unit vector
// toward it) with color times brightness `sun_color`; `haze` in 0..1 adds
// grey haze, `ground` is the ground's albedo below the horizon. No sun disc.
vec3 sky_radiance(vec3 direction, vec3 sun, vec3 sun_color, float haze,
                  vec3 ground) {
    vec3 air = sky_air();
    vec3 depth = sky_depth(haze);
    vec3 dust = depth - air;
    vec3 sunlight = sky_sunlight(sun, sun_color, haze);
    float mu = dot(direction, sun);
    float g = 0.76;
    float air_phase = 0.0597 * (1.0 + mu * mu);
    float dust_phase = 0.0796 * (1.0 - g * g)
        / pow(1.0 + g * g - 2.0 * g * mu, 1.5);
    // Above the horizon: what the view path scatters in. 4 pi turns the
    // per-steradian phase into radiance near the engine's light units.
    vec3 view_depth = depth * sky_air_mass(direction.y);
    vec3 sky = sunlight * 12.566
        * (air * air_phase + dust * dust_phase) / depth
        * (1.0 - exp(-view_depth));
    // Below it: the sunlit ground, fading in under a thin horizon band.
    vec3 floor_color = ground * sunlight * max(sun.y, 0.0);
    return mix(sky, floor_color, smoothstep(0.0, 0.05, -direction.y));
}

// Clouds: a sheet of value-noise fbm 2 km up, 3 km per noise cell.
float sky_hash(vec2 p) {
    p = fract(p * vec2(127.1, 311.7));
    p += dot(p, p + 34.5);
    return fract(p.x * p.y);
}
float sky_noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(sky_hash(i), sky_hash(i + vec2(1.0, 0.0)), f.x),
               mix(sky_hash(i + vec2(0.0, 1.0)), sky_hash(i + vec2(1.0)), f.x),
               f.y);
}

// How thick the clouds are along `direction`, 0..1, for the sheet drifted
// `drift` metres in x and z and `cover` 0..1 of the sky under clouds.
// Toward the horizon the sheet blurs into its average cover.
float sky_cloud_density(vec3 direction, vec2 drift, float cover) {
    if (cover <= 0.0 || direction.y <= 0.0) {
        return 0.0;
    }
    vec2 p = (direction.xz / max(direction.y, 0.01) * 2000.0 - drift)
        / 3000.0;
    float n = 0.0;
    float amplitude = 0.5;
    for (int octave = 0; octave < 5; octave++) {
        n += amplitude * sky_noise(p);
        p = p * 2.03 + 17.0;
        amplitude *= 0.5;
    }
    float edge = 0.8 * (1.0 - cover);
    float density = smoothstep(edge - 0.1, edge + 0.15, n);
    return mix(cover, density, smoothstep(0.0, 0.15, direction.y));
}

// Cloud color along `direction`: sunlit, glowing at thin edges toward the
// sun, darkened by `darkness` 0..1 where thick, plus light from the sky.
vec3 sky_cloud_color(vec3 direction, vec3 sun, vec3 sun_color, float haze,
                     vec3 ground, float density, float darkness) {
    vec3 sunlight = sky_sunlight(sun, sun_color, haze);
    float glow = pow(max(dot(direction, sun), 0.0), 6.0);
    return sunlight * (0.6 * (1.0 - 0.85 * darkness * density)
            + 2.0 * glow * (1.0 - density))
        + 0.5 * sky_radiance(vec3(0.0, 1.0, 0.0), sun, sun_color, haze, ground);
}
