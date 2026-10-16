# Effects

Particles make a scene feel alive: dust in a sunbeam, sparks from a hit,
smoke over a chimney, snow drifting past the camera. This guide covers
`rusting.particle_emitter`, the full particle component.

`rusting.burst_emitter` still works. It spawns one entity per particle and
suits a handful of quick sparks. Use `rusting.particle_emitter` for
anything bigger or longer-lived: its particles are not entities, and each
emitter draws all of them in one instanced draw.

## Presets

Start from a preset and adjust it:

```sh
rusting effect list --json
rusting effect apply scenes/main.rscene fire --at 2,0,-3
rusting effect apply scenes/main.rscene smoke --on Chimney
```

| Preset | Use |
|---|---|
| `dust_motes` | Slow glowing specks in a room or sunbeam. |
| `falling_leaves` | Autumn leaves tumbling over a 10 m square. |
| `snow` | Snowflakes over a 16 m square. |
| `rain` | Rain streaks over a 16 m square. |
| `sparks` | One burst of hot sparks; `scene.trigger` replays it. |
| `smoke` | Grey smoke rising and spreading. |
| `fire` | A campfire flame about a metre tall. |
| `embers` | Glowing embers rising above a fire. |
| `fireflies` | Blinking lights wandering over a 6 m square. |
| `magic_sparkle` | Violet and cyan twinkles around an object. |
| `confetti` | One burst of colored paper; `scene.trigger` replays it. |

Without `--on`, the effect gets a new object at `--at`, or at the
preset's height above the origin (weather presets start high so their
particles fall through the view). Stack presets on nearby objects: fire,
embers and smoke together make a campfire. Weather presets cover a fixed
area; parent the object to the player or camera to keep it around them.

## Add an emitter

Put the component on any object. The object's transform is the emitter's
origin and orientation.

```json
"rusting.particle_emitter": {
  "rate": 40,
  "shape": "Cone",
  "shape_size": [0.2, 0.2, 0.2],
  "lifetime": [0.8, 1.4],
  "speed": [1.0, 2.0],
  "size": [0.15, 0.3],
  "direction": [0, 1, 0],
  "spread": 0.3,
  "drag": 1.0,
  "turbulence": 0.5,
  "size_over_life": [{"t": 0, "value": 0.5}, {"t": 1, "value": 1.5}],
  "color_over_life": [
    {"t": 0, "color": [1.0, 0.7, 0.2, 1.0]},
    {"t": 1, "color": [0.6, 0.1, 0.0, 0.0]}
  ],
  "emissive": 4,
  "blend": "Additive"
}
```

`rusting docs show api/ParticleEmitter` lists every field. Missing fields
take their defaults.

## In the editor

Select the object and open its **Particle Emitter** section in the
Inspector:

- **Preset** replaces every setting with one of the presets above.
- **Play / Pause / Restart / Stop** drive the live preview. Emitters play
  in the Scene view while the game is stopped, so you see each change as
  you make it. The count next to the buttons is the live particle count.
  These buttons do not change the scene.
- **Emission, Shape, Lifetime, Velocity, Size, Color, Rendering** hold the
  settings. Min–max fields sit on one row. **Size Over Life** is a curve:
  click the plot to add a key and drag a dot to move it. **Color Over
  Life** is a gradient: click the bar to add a key with the color shown
  there and drag a marker to move it. The rows under each one set exact
  values or remove keys.

Every setting change is one Undo step. Pressing Play starts the game's
emitters fresh; the preview's particles stay in the editor.

## Settings

- **Emission.** `rate` is particles per second. `bursts` adds
  `{"time": seconds, "count": n}` spawns in each cycle. A cycle lasts
  `duration` seconds; `looping: false` plays one cycle, then stops
  emitting. `max_particles` caps the live count. `prewarm` starts the
  scene as if one cycle had already played, so smoke is already rising on
  the first frame. `autoplay: false` waits for game code.
- **Shape.** `Point`, `Box` (half extents `shape_size`), `Sphere`,
  `Circle` and `Cone` (radius `shape_size[0]`). Shapes follow the object's
  rotation and scale.
- **Random ranges.** `lifetime`, `speed`, `size`, `rotation` and `spin`
  are `[min, max]`. Each particle draws its own value between them.
- **Motion.** `direction` plus `spread` (half angle in radians; `3.14`
  sends particles every way). `gravity` pulls down, `drag` slows, `wind`
  pushes in world space and `turbulence` swirls (`turbulence_frequency`
  sets the swirl size).
- **Over life.** `size_over_life` multiplies the start size and
  `color_over_life` sets linear RGBA, both keyed by life fraction `t` from
  0 to 1. `fade_in` and `fade_out` are fractions of life spent fading.
  `emissive` above 1 glows through bloom; keep it near 1 to 2 so additive
  colors keep their hue instead of clipping to white. `start_colors`
  gives each particle one of several tints (confetti, mixed leaves).
- **Rendering.** `space: "Local"` moves live particles with the object
  (a torch carried by the player); `World` leaves them behind (a trail).
  `facing: "Velocity"` stretches particles along their motion by
  `stretch` seconds of travel (rain, sparks). `blend: "Additive"` adds
  light (fire, sparks, magic); `Alpha` covers what is behind (smoke,
  leaves, snow). `sprite` is `Soft`, `Disc` or `Square`.

Particles are unlit. Their color is what you see, times `emissive`.

## Roofs and ground

With `blockers: true`, an emitter's particles die below
`rusting.heightfield` ground and inside every `rusting.particle_blocker`
box, so rain stops at a cabin's roof instead of falling into it:

```json
{"name": "Cabin roof", "transform": {"position": [0, 3, 0]},
 "components": {"rusting.particle_blocker": {"extents": [3.0, 0.2, 4.0]}}}
```

The box takes the object's rotation and scale. Only CPU simulated
particles are blocked; GPU particles pass through.

## From game code

```rust
scene.particles("Chimney", ParticleCommand::Stop);
scene.particles("Chimney", ParticleCommand::Play);
scene.trigger("Coin"); // also restarts the coin's emitter
```

`Stop` stops emitting and lets live particles finish. `Restart` clears
them and starts a new cycle. `on_collision: true` restarts the emitter
when the object's collider starts touching another one.

## Impacts and settling

Physics contacts can spawn particles where they happen, for sparks off a
metal crate or dust where a rock lands:

```json
"rusting.particle_emitter": {
  "autoplay": false, "rate": 0.0,
  "impact_speed": 2.0, "impact_count": 12, "on_settle": true,
  "speed": [1.0, 3.0], "spread": 0.8, "gravity": 9.81
}
```

Put the emitter on the body that hits things. Each step, every contact of
that body closing at `impact_speed` m/s or faster spawns `impact_count`
particles at the contact point, launched around the contact normal away
from what it hit (`spread` widens the cone; `direction` is ignored).
Resting contacts close slower, so they spawn nothing. `on_settle: true`
spawns `impact_count` particles at the body when it falls asleep, the end
of a pile of debris settling. Impacts fire even while the emitter is
stopped, so `autoplay: false` with `rate: 0` makes an impact-only effect.
Only CPU bodies have contacts here. A GPU emitter spawns impact particles
at the emitter, and Local space particles start at the emitter.

For a burst when a breakable object shatters, make the emitter a child of
the `rusting.fracture` object and set `on_fracture: true`. When the object
breaks, the emitter moves onto the piece nearest it and spawns
`impact_count` particles at the break point, launched from there toward
the emitter. The burst starts one frame after the break.

## Budgets

The quality profile caps the particles drawn per frame, CPU particles and
GPU emitter slots together: 1048576 at High and Auto, 524288 at Balanced,
131072 at Eco. Past the cap every emitter draws the same share of its
particles, its oldest ones, so the kept set does not flicker. Decals are
capped too: 16 at High and Auto, 8 at Balanced, 4 at Eco, later ones in
entity order dropped. Simulation is not thinned, only drawing, so a run
repeats on every profile. `RenderCapacityDiagnostics::dropped_particles`
and `dropped_decals` count what the latest frame left out.

## Determinism

Every random value comes from the scene seed, keyed by the emitter's scene
ID and each particle's spawn number, and indexed by the fixed tick. A run
with the same seed repeats exactly. Particles are visual only: they do not
collide or count toward the physics state hash.
