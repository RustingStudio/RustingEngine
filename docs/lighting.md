# Lighting

Which lights the renderer has, how many it draws, and which ones cast
shadows. `rusting schema point_light` (and `spot_light`,
`directional_light`) gives each field with its units and typical values.

## Light types

| Light | Scene key | Shadows |
| --- | --- | --- |
| Sun | `directional_light` | Yes, with `"shadows": true`. Only the first shadowed directional light casts them. |
| Lamp | `point_light` | Yes, with `"shadows": true`, when no directional or spot light casts them. |
| Cone | `spot_light` | Yes, with `"shadows": true`, when no directional light casts them. |
| Flat fill | `rusting.ambient_light` | No; it lights everything equally. |
| Sky and ground fill | `rusting.sky_light` | No; up-facing surfaces get `sky_color`, down-facing ones `ground_color`. |

Unshadowed point lights pass through walls: a lamp lights the room behind a
wall as far as its `range` reaches. Keep each `range` inside its room and put
lamps near room centres. A spot or point light with `"shadows": true` stops
at walls, so a flashlight does not light the far side of a door. One light
per frame casts shadows: the first directional light with `shadows`, or,
when there is none, the first spot light with `shadows`, then the first
point light with `shadows`. The sun's shadow uses cascades, sharp near the
camera: 2 at `ShadowQuality::Low`, 3 at `Medium` and 4 at `High`; a point light's shadow renders six cube faces. A mesh's `cast_shadows` and
`receive_shadows` apply to that one shadow.

Point and spot lights have a `radius`: the size of the glowing bulb, in
metres. At 0 a shiny floor reflects the lamp as a pinpoint; a paper
lantern at 0.2 reflects as a soft disc. Lighting of matte surfaces does
not change.

A `rusting.light_cookie` on a spot light's object projects an image
through the cone, like a slide in a projector: a window frame, leaves, a
logo. White lets the light through, black blocks it, colors tint it. The
image's top points along the light's +Y. Four cookie lights draw per
frame; further ones light without their image.

## Brightness and direction

A directional light's `illuminance` is in lux: 100000 is the midday sun and
lights a white surface facing it at full brightness before exposure. Point
and spot light `intensity` is in renderer units on the same scale: 1000
lights a surface up close as brightly as that sun, fading to nothing at
`range` as `(1 - distance/range)^2`. The built-in art presets use:

| Preset | Sun `illuminance` | Ambient `intensity` | Sky `intensity` |
| --- | --- | --- | --- |
| `daylight` | 100000 | 0.1 | 0.35 |
| `golden_hour` (dusk) | 70000 | 0.08 | 0.3 |
| `night` (blue moonlight) | 20000 | 0.06 | 0.2 |
| `dark_interior` | 30000 | 0.02 | 0.05 |

Lamps on top of them: a desk lamp in a dark room is 400 to 1500, a bright
party light 3000 to 8000.

`rusting preset list --json` prints every preset's full values, and
`rusting preset apply <scene> <name>` writes one into a scene. Tone-mapping
`exposure` scales everything at once.

Directional and spot lights shine along their object's -Z axis, the same
way a camera looks. An unrotated spot light points at the horizon toward
-Z; to aim it straight down, rotate it -90 degrees (`-1.5708` radians)
about X. Point lights shine in every direction, so their rotation does not
matter.

## How many lights draw

The renderer draws at most 1024 lights a frame at `High` quality, 512 at
`Balanced` and 256 at `Eco` (`/render/quality` in the scene). `Auto` picks
`Eco` on an integrated GPU, `Balanced` on a discrete GPU with less than
4 GiB of video memory, and `High` otherwise (1024 on an RTX 3060). Each
pixel shades every directional light but only the point and spot lights
whose `range` reaches its screen cluster, so many small lamps cost little;
a few lamps with huge ranges cost as much as before. Budget for
the lowest quality the game supports. Ambient and sky light do not count.

Past the cap, lights are dropped in this order: directional lights are kept
first, then point lights, then spot lights; within each kind, lights are
taken in entity order, which is the order objects were created in. So a
flashlight spot light is the first thing to go in a building with many
lamps. Hidden objects (`visible: false`, or under a hidden parent) do not
count, so hiding lamps in rooms the player is not in keeps the count down.

`rusting capture --json` (`data.render`) and `rusting test --json`
(`perf.render` of each scenario that renders) give the dropped count as
`dropped_lights`; zero means every light drew.
