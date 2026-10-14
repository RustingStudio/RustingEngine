# Look and feel

A game that passes its tests can still look cheap. Most of the cheap look
comes from a few causes: flat lighting, random colors, plain boxes for
everything, and a HUD laid on top of the scene. This guide is a checklist
for each of them and the polish loop that finds them.

## The polish loop

Do this after the game works and before calling it done.

1. Capture. Add `capture` steps at the moments a player sees most: the
   start, the middle of a round, the win and lose screens. Use
   `"contact_sheet"` to see several frames in one image.
2. Look. Open every image. Describe what you see in one sentence, as a
   player would. If you cannot tell what the player should look at, the
   frame fails.
3. Critique. Go through the checklist below and write down every item the
   frame fails.
4. Fix the worst item, capture again, and repeat. Two or three passes are
   normal.

## Checklist

- **Silhouette.** The player, enemies and pickups read as shapes against
  the background, even in grey. Raise contrast between them and what is
  behind them; a rim of light or an emissive edge helps.
- **Palette.** Use the five colors of your art preset (`rusting preset
  list`): dark, mid and light for the world, two accents for what matters
  (player, goal, danger). Do not give every object its own random color.
  Many random colors in a large crowd read as noise.
- **Value contrast.** The darkest and lightest parts of the frame differ
  clearly. If the whole frame is mid grey, lower the ambient light and
  raise the key light.
- **Warm key, cool fill.** One strong directional or spot light (the key)
  in a warm color, and a weaker sky or ambient light (the fill) in a cool
  color, or the reverse for night. Equal light from every side looks flat.
  One light casts shadows: the directional light, or a spot light with
  `shadows` when there is no shadowed directional light. At most 1024 lights
  draw a frame at `High` quality, 512 at `Balanced` and 256 at `Eco`;
  `rusting docs show guide/lighting` says which one `Auto` picks.
- **Materials.** Not everything has roughness 0.5. Metal is metallic and
  smoother; cloth, wood and stone are rough. Emissive is for lights,
  screens and pickups only.
- **Low-poly look.** Set `flat_shading: true` on a material for faceted
  normals from screen-space derivatives; it works on generated meshes
  such as water. Pair it with `scene.set_hard_shadows(true)` (or Hard
  shadows in the editor's project settings) for crisp one-tap shadow
  edges.
- **Vertex colors.** Each `MeshVertex` has a linear RGBA `color`
  (white by default) that multiplies the material's base color, so a
  generated mesh can fade from shallow to deep water without a gradient
  texture. glTF models keep their `COLOR_0`.
- **Shapes.** Use the rounded primitives (below) or real models for
  anything the player looks at closely. Plain cubes are for floors and
  walls.
- **HUD.** Text stays inside a safe margin (about 3% of the screen from
  each edge) and has a backdrop or outline where it crosses the scene.
  Show only what the player needs now; put long help text behind a key.
  The center of the screen stays clear during play.
- **Motion and feedback.** Every action the player takes gets a response
  within a few frames: a sound, a burst, a flash, a scale pop or a camera
  shake. Idle objects that should feel alive move a little
  (`rusting.tween`). Game code moves an object to a new place, turn or
  size once with `scene.tween(name, TweenProperty::Position, to, seconds,
  Easing::QuadOut)`, starting from where it is now.
- **Air.** An empty sky reads as a stage set. A few slow particles (dust
  motes, falling leaves, snow, fireflies) make the space feel lived in;
  keep them small, sparse and in the palette. `rusting effect apply
  <scene> dust_motes` adds one; `rusting docs show guide/effects`.

## Presets and color grading

`rusting preset apply <scene> <name>` sets the sky, sun, ambient light,
fog, bloom, tone mapping and color grading in one patch. Presets:
`daylight`, `golden_hour`, `night`, `flat_toy`, `dark_interior` (horror,
night shifts) and `bright_stylized` (cartoon). `--only` limits it to some
parts. `rusting preset list --json` prints each preset's values and its
five-color palette.

Light units are on one renderer scale, not physical ones. A white surface
facing the light reflects 1 before exposure for a directional light of
100000 lux, a point or spot light of 1000 up close, or ambient intensity
1. A 3000 lux sun is therefore only 0.03 and almost invisible; moonlight
shafts through windows need 20000-40000 lux. Exposure multiplies
everything.

`rusting.color_grading` runs after tone mapping:

```json
"rusting.color_grading": {"contrast": 1.1, "saturation": 0.9,
  "shadows": [0.92, 0.98, 1.1], "highlights": [1.08, 1.0, 0.9], "vignette": 0.3}
```

- `contrast` above 1 darkens shadows and brightens highlights around mid
  grey.
- `saturation` 0 is greyscale, 1 is unchanged.
- `shadows` and `highlights` tint the dark and bright parts. Cool shadows
  with warm highlights is the classic film look.
- `vignette` 0..1 darkens the corners and leads the eye to the center.

For a look made in a paint or grading program, use `rusting.color_lut`. Its
`texture` is a strip of N squares of N x N pixels, 256x16 or 1024x32: red
goes across a square, green goes down, and blue steps from square to square.
Grade a neutral strip like that in the program, save it, and point the
component at it. `intensity` 0..1 blends from the plain image to the graded
one. The lookup runs after `rusting.color_grading` and does not follow
`rusting.post_volume`:

```json
"rusting.color_lut": {"texture": "looks/warm_film.png", "intensity": 0.8}
```

`rusting.depth_of_field` blurs what is nearer or farther than
`focus_distance` metres, like a camera lens. `blur` is the largest blur
radius as a fraction of the screen height: 0.005 is subtle, 0.02 is strong.
Objects at half the focus distance, or far behind it, get the full blur.
Move `focus_distance` from a script for focus pulls in cutscenes:

```json
"rusting.depth_of_field": {"focus_distance": 4.0, "blur": 0.01}
```

A blurred object in front of a sharp one keeps a hard outline; it does not
spread over the sharp one.

`rusting.motion_blur` smears the image while the camera moves or turns.
`intensity` is the share of the last frame's motion the smear covers, like
a camera shutter: 0.5 is the film look, 1 is heavy. Objects moving on their
own do not blur, and a camera jump of more than a sixth of the screen counts
as a cut, so teleports stay sharp:

```json
"rusting.motion_blur": {"intensity": 0.5}
```

### Film and CRT/VHS effects

The same component carries film and old-screen effects. Each one is 0..1
and off at 0:

```json
"rusting.color_grading": {"grain": 0.15, "chromatic_aberration": 0.2}
```

Grading and fog can change by area. A `rusting.post_volume` on an object
that also has `rusting.fog` or `rusting.color_grading` makes them apply only
while the camera is inside its box, fading over `blend` metres at the edge;
grading and fog without a volume apply everywhere else:

```json
{"name": "Hall Mood", "transform": {"position": [0, 1.5, -12]},
 "components": {"rusting.post_volume": {"extents": [3, 2, 10], "blend": 1.5},
   "rusting.fog": {"color": [0.4, 0.45, 0.3], "density": 0.08},
   "rusting.color_grading": {"saturation": 0.7, "shadows": [0.95, 1.05, 0.85]}}}
```

- `grain` adds film grain. The pattern comes from the fixed tick, so the
  same tick always gives the same grain: replays and golden images stay
  repeatable.
- `chromatic_aberration` splits red and blue toward the edges.
- `scanlines` darkens every other pixel row.
- `color_bleed` smears color to the right while brightness stays sharp,
  like VHS tape.
- `noise_band` rolls a band of static down the image, once every four
  seconds.
- `distortion` bulges the image like a curved tube and sways its rows over
  time. The corners turn black.

`chromatic_aberration`, `color_bleed` and `distortion` read neighboring
pixels, so they add a copy of the HDR image to the frame (about one
full-screen copy; small next to bloom). The others cost nearly nothing.

For CCTV monitors, give each `rusting.camera_screen` its own `grading`
(see [cameras.md](cameras.md#screens-a-camera-on-a-material)) so only the
feeds look like old tape and the room stays clean:

```json
"rusting.camera_screen": {"camera": "Cam 3", "size": [320, 180],
  "grading": {"scanlines": 0.8, "grain": 0.3, "color_bleed": 0.4,
    "vignette": 0.4, "noise_band": 0.3, "distortion": 0.3, "saturation": 0.6}}
```

### Light shafts and fog volumes

With a sun that casts shadows, `light_shafts` on `rusting.fog` (0..1) keeps
the glow toward the sun dark where shadow casters block it, so beams show
between trees and through windows. Set `sun_scatter` above 0 for the glow.

A `rusting.fog_volume` fills a box around its object with uniform fog that
the camera sees from outside too: mist in a valley, smoke in one room. The
box turns and scales with the object; up to eight are drawn:

```json
{"name": "Valley Mist", "transform": {"position": [0, 1, 0]},
 "components": {"rusting.fog_volume": {"color": [0.7, 0.75, 0.8],
   "density": 0.3, "extents": [20, 1, 20]}}}
```

### Procedural sky

A `rusting.procedural_sky` on any object draws a daylight sky behind the
scene: blue overhead, paler at the horizon, red and dim as the sun sets,
with a sun disc `sun_size` degrees across. The sun is the directional light
that casts shadows (else the first one), so turning that light turns the
sky. The sky also lights and reflects in every surface, like an environment
map; a `rusting.environment_map` still wins for lighting when both are set.
`haze` (0..1) greys and brightens it, `ground_color` fills the lower half.

```json
{"name": "Sky", "components": {"rusting.procedural_sky":
  {"intensity": 1.0, "haze": 0.2, "ground_color": [0.3, 0.28, 0.25],
   "sun_size": 1.0}}}
```

For a photographed sky, put an equirectangular (2:1) Radiance `.hdr` image
under `assets/` and use it as a `rusting.environment_map` with
`"background": true`. Unlike PNG or JPEG, `.hdr` keeps the sun and bright
clouds brighter than white, so reflections and lighting keep their punch:

```json
{"name": "Sky", "components": {"rusting.environment_map":
  {"texture": "skies/field.hdr", "intensity": 1.0, "background": true}}}
```

### Decals

A `rusting.decal` paints an image onto whatever lies inside a box around
its object: a poster on a wall, a crack, a stain, road paint. The image is
projected along the object's forward axis (-Z) with its top along +Y, so
place the object against the surface and face it in. Surfaces that face
away from the projection are left alone. `color` multiplies the image and
its alpha scales the coverage; leave `texture` empty to paint plain color.
Later decals paint over earlier ones; up to sixteen are drawn, using at
most four distinct images:

```json
{"name": "Crack", "transform": {"position": [0, 1.5, -4.9]},
 "components": {"rusting.decal": {"texture": "decals/crack.png",
   "color": [1, 1, 1, 1], "extents": [0.5, 0.5, 0.2]}}}
```

Set `roughness` (0 to 1) to paint roughness with the same coverage: a
dark decal with roughness 0.1 is a wet stain or a puddle that catches
highlights. Leave it `null` to keep the surface's. Decals do not change
normals.

Make a decal a child of the object it marks, so it moves with it. When that
object breaks (`rusting.fracture`), the decal moves onto the piece nearest
it and stays on that piece's surface. Set `survive_fracture` to false for a
mark that belongs to the whole object, such as a label, and it is removed
with the object.

## Broken objects

The pieces of a broken object (`rusting.fracture`) use the object's
material. Their outer faces keep the object's normals and texture
coordinates, so on the tick it breaks the pieces look like the object did.
The new cut faces inside get flat normals and the texture laid flat on
them at one repeat per meter. A curved object breaks into flat-sided
pieces, because each piece is convex.

## Cloth and soft blocks

`rusting.cloth` and `rusting.soft_block` take a `color` (linear RGBA) and
a `texture` (asset path), and the color is multiplied into the image. A
cloth sheet shows the image once across the sheet, with its top row along
the top edge. A soft block shows the image once on each side, upright on
the four walls. The image is fixed to the particles, so it stretches,
squashes and tears with the body. Blocks or sheets that leave both unset
share one plain material.

A `rusting.fluid_block` surface draws as translucent water for the
liquid solvers and as opaque sand for `Granular`, so a heap of grains
reads as a heap and not as a puddle.

## Water

`rusting.water` draws one color by default. Three optional fields make
it read as water; each reads the opaque scene behind the surface, so the
frame pays for one copy of the depth buffer (the one screen-space
reflections already make) while any of them is on.

```json
"rusting.water": {
  "size": [40.0, 40.0],
  "color": [0.2, 0.55, 0.6, 0.55],
  "deep_color": [0.02, 0.1, 0.2, 0.95],
  "depth_fade": 3.0,
  "shore_foam": 0.3,
  "ripples": 0.6,
  "ripple_length": 0.8
}
```

- `deep_color` and `depth_fade`: the water starts at `color` and reaches
  `deep_color` once the view crosses `depth_fade` metres of it, so the
  shallows show the ground and open water darkens. `null` keeps one color.
- `shore_foam`: a white band where the ground is closer than this many
  metres under the surface, broken up toward its edge in a slow pattern.
- `ripples` and `ripple_length`: small waves that travel at the
  deep-water speed for their length, lit on top of the mesh. They fade out
  in the distance before they would shimmer.

The depth needs the lit view; debug views draw the plain `color`.

## Text in the world

`rusting.text` puts text on a sign, over a door or above an object. It
uses the same 5 x 7 pixel font as `rusting asset generate <root> font
pixel`, so signs match the chunky placeholder look. `size` is the height
of a capital in meters; 0.3 to 0.5 reads well at room scale. The text is
unlit, so pick a `color` that stands out from the wall behind it. It faces
+Z: turn the entity to aim it. Lines split at `\n` stack downward, each
centered.

## Mesh kit

Built-in primitives (`{"BuiltinPrimitive": "<name>"}`): `Cube`, `Sphere`,
`Cylinder`, `Cone`, `Capsule`, `RoundedCube`, `Torus`, `Plane`, `Quad`,
`Triangle`, `Tetrahedron`, `Octahedron`, `Dodecahedron`, `Icosahedron`,
`Pyramid`. Cylinder, cone, capsule and sphere are smooth shaded.

Every primitive is centered on its origin. At scale 1:

| Primitive | Size in meters (x, y, z) | Shape and axis |
|---|---|---|
| `Cube`, `RoundedCube` | 1 x 1 x 1 | box |
| `Sphere` | 1 x 1 x 1 | diameter 1 |
| `Cylinder` | 1 x 1 x 1 | diameter 1, runs along Y |
| `Cone` | 1 x 1 x 1 | base diameter 1 at y = -0.5, tip at y = +0.5 |
| `Capsule` | 1 x 2 x 1 | diameter 1, runs along Y |
| `Pyramid` | 1 x 1 x 1 | square base at y = -0.5, tip at y = +0.5 |
| `Torus` | 1 x 0.28 x 1 | ring lies flat in XZ around Y; tube radius 0.14 |
| `Plane` | 1 x 0 x 1 | flat in XZ, faces +Y |
| `Quad` | 1 x 1 x 0 | flat in XY, faces +Z |
| `Triangle` | 1 x 1 x 0 | flat in XY, tip at y = +0.5 |
| `Tetrahedron`, `Octahedron` | 1 x 1 x 1 | fits the unit box |
| `Dodecahedron` | 0.93 x 0.93 x 0.93 | |
| `Icosahedron` | 0.85 x 0.85 x 0.85 | |

So a scale of `[0.1, 2.0, 0.1]` on a `Cylinder` is a pole 2 m tall and
10 cm thick. To lay a cylinder on its side (a roller), rotate it by 1.5708
around X or Z.

Texture mapping (UVs): `Cube`, `RoundedCube` and `Plane` show the whole
texture on each face. `Sphere`, `Capsule`, and the sides of `Cylinder` and
`Cone` wrap it once around the Y axis and show its full height, the top
row of the image at the top of the shape, so horizontal bands in the image
become rings. The flat caps of
`Cylinder` and `Cone` show it as a disc, upright when seen from outside with
-Z at the top, so a thin cylinder turned to face the camera is a round
door, coin or clock face.

A child's transform is relative to its parent, scale included. Under a
parent scaled `[0.6, 1.8, 0.6]`, a child sphere at scale 1 is stretched
three times taller than wide; divide the child's scale by the parent's,
or keep the parent at scale 1 and scale only the mesh children, as `Hero`
does below.

- `RoundedCube` is a unit box with edges rounded by 0.1. Edges catch the
  light, so crates, furniture and buttons stop looking like placeholders.
  Large scales stretch the rounding; for a wall, keep a plain `Cube`.
- `Capsule` has diameter 1 and height 2, like a `Capsule` collider with
  radius 0.5 and half height 0.5. Use it for limbs, characters and posts.

A simple character from the kit. Patch it in, then move `Hero` as one
object; the parts follow:

```json
{"operations": [
  {"op": "create", "entity": {"name": "Hero",
    "transform": {"position": [0, 0, 0], "rotation": [0, 0, 0], "scale": [1, 1, 1]}}},
  {"op": "create", "entity": {"name": "Hero Torso", "parent": "Hero",
    "transform": {"position": [0, 1.1, 0], "rotation": [0, 0, 0], "scale": [0.5, 0.6, 0.28]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "RoundedCube"},
      "material": {"Inline": {"base_color": [0.18, 0.32, 0.6, 1], "roughness": 0.7}}}}},
  {"op": "create", "entity": {"name": "Hero Head", "parent": "Hero",
    "transform": {"position": [0, 1.62, 0], "rotation": [0, 0, 0], "scale": [0.34, 0.34, 0.34]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Sphere"},
      "material": {"Inline": {"base_color": [0.9, 0.68, 0.52, 1], "roughness": 0.6}}}}},
  {"op": "create", "entity": {"name": "Hero Arm L", "parent": "Hero",
    "transform": {"position": [-0.34, 1.1, 0], "rotation": [0, 0, 0.08], "scale": [0.14, 0.3, 0.14]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Capsule"},
      "material": {"Inline": {"base_color": [0.18, 0.32, 0.6, 1], "roughness": 0.7}}}}},
  {"op": "create", "entity": {"name": "Hero Arm R", "parent": "Hero",
    "transform": {"position": [0.34, 1.1, 0], "rotation": [0, 0, -0.08], "scale": [0.14, 0.3, 0.14]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Capsule"},
      "material": {"Inline": {"base_color": [0.18, 0.32, 0.6, 1], "roughness": 0.7}}}}},
  {"op": "create", "entity": {"name": "Hero Leg L", "parent": "Hero",
    "transform": {"position": [-0.13, 0.4, 0], "rotation": [0, 0, 0], "scale": [0.18, 0.4, 0.18]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Capsule"},
      "material": {"Inline": {"base_color": [0.15, 0.13, 0.12, 1], "roughness": 0.8}}}}},
  {"op": "create", "entity": {"name": "Hero Leg R", "parent": "Hero",
    "transform": {"position": [0.13, 0.4, 0], "rotation": [0, 0, 0], "scale": [0.18, 0.4, 0.18]},
    "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Capsule"},
      "material": {"Inline": {"base_color": [0.15, 0.13, 0.12, 1], "roughness": 0.8}}}}}
]}
```

The feet are at the origin of `Hero`. Animate it by rotating the arm and
leg parts from game code each fixed tick (swing them with
`sin(tick * speed)`), or key their rotations in a `rusting.animation`
clip (`rusting docs show guide/animation`). A rigged glTF character keeps
its skin and clips through `scene add-model` (see "Skinned meshes" there).
Rename the parts for each copy, or use `spawn_copy` on a hidden template.

## Crowds of one object

Thousands of copies of one mesh and material, like shelves of identical toys,
draw in one instanced batch with GPU culling. No special component is needed:
give every copy the same mesh and the same material handle. Copies with
different materials batch separately, so vary a crowd with a handful of
shared materials, not one per object.

Moving some of them each frame is cheap: change their `Transform` as usual.
Measured with `render_bench` on an RTX 3060 at
1920x1080 (balanced quality, 600 frames, no GPU bodies), mean and 95th
percentile frame time:

| Scene | Mean | p95 |
| --- | --- | --- |
| 5,000 copies | 7.4 ms | 8.2 ms |
| 5,000 copies, 500 moved every frame | 8.8 ms | 9.4 ms |
| 10,000 copies, 500 moved every frame | 12.4 ms | 13.4 ms |
| 5,000 copies, 500 moved, six 320x180 camera screens | 16.5 ms | 19.0 ms |
| the same, screens updating every other frame | 12.4 ms | 13.5 ms |

All of these hold 60 fps except the full scene with screens updating every
frame, which needs `update_every: 2` on the screens. Measure your own scene
with `cargo run --release --example render_bench -- balanced --no-bodies
--bears 5000 --bear-updates 500 --screens 6 --screen-every 2`. There is no
per-instance custom value yet; give a copy that must look different its own
material.

### Level of detail (LOD)

A crowd seen from far away does not need its full mesh. Put a `.rlod` file
beside the `.rmesh` the objects use, with the same name (`bear.mesh-0-0.rlod`
next to `bear.mesh-0-0.rmesh`), and every object drawing that mesh picks one
level per frame. Mesh paths are relative to the `.rlod` file:

```json
{"metric": "Distance", "levels": [
  {"mesh": "bear.mesh-0-0.rmesh", "until": 8},
  {"mesh": "bear_low.mesh-0-0.rmesh", "until": 30},
  {"mesh": "bear_far.mesh-0-0.rmesh"}
]}
```

With `"metric": "Distance"` a level draws while the camera is closer than
its `until`, in metres. The default metric, `"ScreenSize"`, uses the
fraction of the view height the object covers instead, so a level draws while
it covers at least `until` (0.5 is half the screen); it keeps the same look
when the field of view changes. A level without `until` draws at any range;
give the last level an `until` to stop drawing the object past it.

Importing a glTF writes this file for you: each static mesh of 512 triangles
or more gets up to three coarser levels with about a half, a quarter and an
eighth of its triangles (`bear.mesh-0-0.lod1.rmesh` and so on), handing over
at screen sizes 0.3, 0.12 and 0.05. Skinned and morphing meshes are left
alone. The generator merges nearby vertices, so it can round off hard edges;
for hero assets, replace the coarser meshes with your own (from a lower-poly
glTF through `rusting scene add-model`, or your own tool) and edit the
`.rlod` to point at them. The scene does not change: the group loads with the mesh,
and an exported game carries it in `assets/`. Edit the `.rlod` and reload the
scene to retune it.

To measure your own game in its real window, run it with `RUSTING_PERF=1`
(for example `RUSTING_PERF=1 rusting run --release`). Once a second it
prints a `[rusting] perf` line with the frame rate, the p50, p95, p99 and
largest frame time over that second, and where the time went (update,
render, CPU and GPU passes), and shows the frame rate, frame time and p95
in the window's top-right corner. F3 turns the same report on and off
while the game runs; the game also sees the F3 press. The CPU part splits into `fixed` (physics and
fixed systems), `update` (the game's systems), `post_update` (transform
propagation and other engine work) and `extract`; a scenario run reports
the same split per tick as `perf.stages_ms_mean`. In a second with key or
mouse presses, the line ends with `input latency p50 ... max ... ms`: the
time from a press to the present of the first frame that read it, plus
that frame's GPU time. The display's scan-out and the monitor's own delay
come on top and need a camera or photodiode to measure. `rusting test` captures render offscreen and
read every frame back, so their frame times run higher than the window's;
use them as a regression guard, not as the frame budget.
To measure at a set size, put `"window": [1920, 1080]` in `project.json`:
the game asks for that window size when it runs from the project folder.
`"window_title": "My Game"` sets the window title the same way; game code
can change it later with `GameScene::set_window_title`.

For one number to compare runs, use `rusting run --release --bench 1000
--json`. The game opens its window, skips 60 warm-up frames (pipeline
builds and uploads), measures the next 1000, closes itself, and reports
`timings.bench` with `frames`, `mean_ms`, `p50_ms`, `p95_ms`, `p99_ms` and
`max_ms`. It also gives `cpu_p50_ms` (update plus extraction, preparation
and recording, without waits on the GPU), `gpu_p50_ms` (GPU pass time) and
`bound`: `"cpu"` or `"gpu"`, whichever median is larger.
`passes_p50_ms` splits the GPU time by pass: `Shadow`, `DepthPrepass`,
`Scene`, `Bloom`, `ToneMap` and the others, plus parts of a pass:
`Shadow.cascade0` to `Shadow.cascade3`, and `Scene.opaque`,
`Scene.terrain`, `Scene.foliage` and `Scene.sky`. `Scene.sky` includes sky
fog and fog volumes. Fog on objects is shaded with them and counts toward
their part. Post-processing is the `Bloom`, `Exposure` and `ToneMap`
passes. Part times are approximate, because the GPU overlaps work around
the marks. Without `--json` it prints one `bench:` line and the three
costliest passes. Check `bound` before cutting cost: fewer triangles,
mesh LOD and smaller shadow maps only help a `"gpu"` frame. A `"cpu"` frame
needs fewer objects, fewer distinct meshes and materials, or less game code
per frame. An opaque object in a `.rlod` group stays one instance: its batch
picks the level per object and adds one draw per level, so LOD costs little
CPU. Blended objects still become one instance per level. A group has at most
4 levels; the loader refuses more.

Without a window, `rusting test` gives GPU frame times. A scenario with
`"gpu": true` and `"capture_size": [1920, 1080]` draws one offscreen frame
per tick and reports `perf.render.gpu_ms_p50`, `gpu_ms_p95` and
`gpu_ms_max` over those frames (`gpu_frames` counts them). These are GPU
pass times only, without present or CPU work. `perf.render.passes_ms`
splits the last frame's GPU time by pass and part, with the same keys as
`passes_p50_ms` above.

`capture_size` is the render size (resolution) of every headless frame,
captures and `perf.render` included: `[1920, 1080]` for 1080p, `[2560,
1440]` for 1440p. It defaults to `[1280, 720]`. Windowed runs use the
window's size.

## Terrain layers

A `rusting.terrain` paints up to four layers. Each layer keeps its image at
full size up to 2048 px, with mipmaps and anisotropic filtering, so a 1K or
2K ground texture stays sharp near the camera and calm in the distance.
Make layer images seamless: they repeat every `tiling` metres.

```json
{"texture": "textures/grass.png", "color": [1, 1, 1], "tiling": 3.0,
 "normal": "textures/grass_normal.png", "roughness": "textures/grass_rough.png"}
```

- `tiling` on a layer overrides the terrain's `tiling` for that layer only,
  so rock can repeat every 8 m while grass repeats every 3 m.
- `normal` is an OpenGL-style normal map (green up). `roughness` reads the
  red channel. A layer without them stays flat at roughness 0.9.
- Layer images of different sizes are scaled to the largest one.
- The terrain's own `tint` (linear rgb) multiplies every layer and its
  `roughness` scales every layer's roughness. Changing only these two
  updates the material without rebuilding the chunks. `set_terrain_look`
  sets them without a trip through JSON, so a game can step them every
  frame, such as darker and glossier ground while it rains:

  ```rust
  let wet = rain.min(1.0);
  scene.set_terrain_look("Island", [1.0 - 0.3 * wet; 3], 1.0 - 0.4 * wet);
  ```

  `set_field` on `/components/rusting.terrain/tint` also skips the
  rebuild, but it copies the whole terrain, splat weights included,
  through JSON on each call.

### Grass on the terrain

`rusting.foliage` scatters over a flat disc. On a hilly terrain, set
`conform` to drop each copy onto the ground below it, and `layers` to keep
copies on chosen terrain layers only:

```json
"rusting.foliage": {"count": 4000, "radius": 30.0, "conform": true, "layers": [1]}
```

- `layers` holds indices into the terrain's `layers` list. A copy stays
  where those layers make up at least half of the ground's blend, so grass
  on layer 1 skips the sand on layer 0.
- Copies off every heightfield keep the disc height with `conform` alone;
  with `layers`, they are dropped.
- Copies are placed again when the foliage object moves or the heightfield
  or terrain changes, not every frame.

### Trees in the wind

A material with a `sway` block bends every mesh that uses it in the
`rusting.force_field` winds around the mesh's origin, as foliage copies do.
The bend runs in the vertex shader, so a forest of tree crowns costs no CPU
time:

```json
"material": {"Inline": {"base_color": [0.2, 0.45, 0.15, 1.0],
  "sway": {"amplitude": 0.01, "frequency": 0.3, "height": 4.0}}}
```

- `amplitude` is the lean per m/s² of push, in metres per metre of height
  above the origin, the same unit as foliage `sway`. 0 keeps the mesh still.
- `frequency` pulses the lean between half and one and a half times this
  many times a second. Each tree's phase comes from its position, so
  neighbours do not move together.
- `height` keeps the base rooted: the bend grows over this many metres
  above the origin, then runs straight. 0 leans the whole mesh as one.
- Put the origin at the foot of the trunk. Nothing below it bends when
  `height` is above 0.
- Only `rusting.force_field` sensors move it, so add a Wind field around the
  trees. Fields of kind Custom do not bend meshes.

## Free models

A real model beats any pile of primitives. Many good game models are free
under CC0, which allows any use without credit. Sources that work well:

| Source | What | License |
| --- | --- | --- |
| [Kenney](https://kenney.nl/assets) | Large kits in one style: city, dungeon, furniture, vehicles, nature, characters | CC0 |
| [Quaternius](https://quaternius.com) | Low-poly packs: characters, animals, nature, sci-fi, weapons | CC0 |
| [Poly Pizza](https://poly.pizza) | Search across thousands of low-poly models | CC0 or CC-BY per model |
| [Poly Haven](https://polyhaven.com) | Realistic models, PBR textures and HDRIs | CC0 |
| [ambientCG](https://ambientcg.com) | PBR textures (color, normal, roughness maps) | CC0 |

Steps:

1. Download the glTF (`.glb` or `.gltf`) version. Pick models from one
   kit or author so the style matches. Low-poly kits fit best: textures
   are small and draws stay cheap.
2. Check the license on the model's page. CC0 needs no credit. CC-BY needs
   the author's name in your README credits. Skip anything with
   "non-commercial" or no license.
3. Import it into the project and record where it came from:

   ```sh
   rusting asset import . ~/Downloads/barrel.glb --to models \
     --license CC0-1.0 --author "Kenney" --url https://kenney.nl/assets/...
   ```

   The license, author and URL go into the model's `.rmeta` next to it.
   That file is the record; `scene add-model` takes no provenance flags.

4. Place it in a scene:

   ```sh
   rusting scene add-model scenes/main.rscene assets/models/barrel.glb --name "Barrel 1"
   ```

   This creates one object named `Barrel 1` with a child per glTF node
   and keeps the model's materials and textures. It writes the converted
   meshes (`.rmesh`) and textures (`.rtexture`) next to the model; keep
   them with the project. The model's glTF animations become a
   `rusting.animation` on `Barrel 1`; the first clip autoplays. Move,
   scale or add a collider to `Barrel 1` with
   a patch. Run `add-model` again with another `--name` for each copy, or
   `spawn_copy` it from code.
5. Capture and look: imported models are often far larger or smaller than
   the scene (check the scale) and may face +Z instead of -Z (turn the
   parent by π).

Skinned meshes keep their skin and their clips play; see `guide/animation`,
"Skinned meshes". Child object names come from the glTF file, so look them
up under the parent you named rather than by their name alone. A child
name that is already taken in the scene, or repeated inside the file,
gets " 2", " 3" and so on.

A shared Rusting model library on a CDN is planned so agents can fetch a
small set of house models by name. It does not exist yet; use the sources
above until it does.
