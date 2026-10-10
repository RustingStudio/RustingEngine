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
