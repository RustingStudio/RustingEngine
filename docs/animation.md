# Animation

`rusting.animation` plays keyframe clips on an object and its named
children: doors that swing open, platforms that bob, lights that blink,
a camera rig that sweeps through a cutscene. Clips run on the fixed tick,
so the same inputs give the same motion on every run.

## A first clip

```json
"rusting.animation": {
  "clips": [{
    "name": "bob",
    "repeat": "PingPong",
    "tracks": [{
      "property": "Position",
      "interpolation": "Smooth",
      "keys": [
        {"time": 0.0, "value": [0.0, 1.0, 0.0]},
        {"time": 1.5, "value": [0.0, 1.4, 0.0]}
      ]
    }],
    "events": [{"time": 1.5, "name": "top"}]
  }],
  "autoplay": "bob",
  "speed": 1.0
}
```

`autoplay` names the clip that starts with the scene; leave it empty to
wait for game code. `speed` scales playback time; 0 freezes it.

## Clips

| Field | Meaning |
|---|---|
| `name` | What game code calls the clip. |
| `duration` | Length in seconds. 0 uses the last key or event. |
| `repeat` | `Once` stops on the last frame, `Loop` jumps back to the start, `PingPong` plays back and forth. |
| `tracks` | Keyed values, below. |
| `events` | `{time, name}` markers sent to game code when playback passes them. |

## Tracks

A track writes one property of one object. `target` is empty for the
animated object itself, or a path of child names such as `Arm/Hand`.

| `property` | Key `value` |
|---|---|
| `Position` | `[x, y, z]` in metres, local to the parent. |
| `Rotation` | `[x, y, z]` in radians. |
| `Scale` | `[x, y, z]`. |
| `Color` | `[r, g, b, a]`, linear. |
| `Emissive` | `[r, g, b]`, linear; above 1 blooms. |
| `Visible` | `[v]`; shown at 0.5 and above. |
| `Orientation` | `[x, y, z, w]` quaternion; blends the short way round, then writes `Rotation`. glTF clips use it. |
| `{"Field": {"component": "rusting.point_light", "path": "/intensity"}}` | `[v]`, any number field of a registered component. |

`interpolation` is `Step` (hold each key), `Linear`, or `Smooth`
(Catmull-Rom through the keys). Keys are kept in time order.

Color and emissive tracks give the object its own copy of its material the
first time they play, so other objects that shared the material keep
theirs. A `Field` track writes the whole component back each tick, which
resets that component's unsaved runtime state; use it for lights, fog and
similar settings, not for a running particle emitter.

## Imported clips

`rusting scene add-model` and the editor's Add to Scene keep a glTF
model's node animations. Each glTF animation becomes a clip on the new
object, and the first one autoplays. Translation and scale channels
become `Position` and `Scale` tracks, rotation an `Orientation` track,
targeted by node name path. Morph target weight channels become a `Field`
track on `rusting.morph` `/weights`. Cubic spline keys play `Smooth`.
`samples/vertical_slice/windmill.gltf` is a small example.

## Skinned meshes

A glTF mesh with a skin keeps it. The importer cooks the vertex joints and
weights (`JOINTS_0`, `WEIGHTS_0`) into a `.rskin` file next to the
`.rmesh`, and the object that renders the mesh gets `rusting.skin`: one
path per joint, relative to that object (`..` is the parent, so
`../Root/Tip`), and one inverse bind matrix per joint. Joints are the
glTF's nodes, so the model's clips animate them like any other object, and
you can key joints in the Timeline. Each frame a joint moved, the mesh is
bent on the CPU into a copy that is drawn instead; the saved scene keeps
the cooked mesh. `samples/vertical_slice/bending_bar.gltf` is a two-joint
bar that folds over. A node with several primitives spawns the extra ones
as children; they get the same skin with paths one level deeper.

The editor's Scene view draws every skeleton in front of the meshes: a
small cross per joint and a line from its parent joint. Click a bone to
select that joint, then rotate it or key it in the Timeline.

## Blend shapes

A glTF mesh with morph targets keeps them. The importer cooks each
target's position and normal offsets into a `.rmorph` file next to the
`.rmesh`, and the object gets `rusting.morph`, one weight per shape,
starting from the glTF's weights. Extra mesh parts (the children a node's
other primitives spawn) have no `rusting.morph` of their own and use
their parent's weights. Set the weights from a clip
(`{"Field": {"component": "rusting.morph", "path": "/weights"}}`, one value
per shape in each key), the Inspector, or game code. The mesh is
reshaped first and skinned after, on the CPU, each frame a weight
changes. `samples/vertical_slice/squash_box.gltf` squashes and leans a box.

In the editor and in games run from it, skinning and blend shapes run on
the GPU: the copy keeps the cooked vertices, and each frame a compute
shader writes the deformed vertices into the copy's vertex buffer before
anything draws, so shadows and reflections bend too. Headless renders
(`rusting capture`, tests) bend on the CPU, the reference the GPU path
matches.

Limits: four joints per vertex; flat-shaded meshes (no normals in the
file) keep their face normals while morphing; a GPU-bent mesh is never
frustum culled, and clicking it picks against its unbent shape.

## In the editor

Open a **Timeline** area: pick Timeline in any area's editor-type menu. It
works on the selected object, or on its nearest parent with an animation.
Without one it offers **+ Add Animation**.

- The header picks, adds (+), deletes (−) and renames clips, and sets repeat,
  length (auto ends at the last key) and Autoplay.
- Play, Pause, Stop and the ruler scrub the clip. The Scene view shows the
  pose at the playhead. Saves, Undo and the game keep the rest pose; the
  pose is a preview only.
- **Insert Key** keys Location, Rotation, Scale or All of the selected
  object at the playhead. Children are keyed by their name path, so give
  them unique names.
- With **● Record** on, moving, turning or scaling an object in the clip
  adds keys at the playhead. Without it, an edit changes the rest pose.
- Drag a key's diamond to move it; keys snap to the 1/60 s tick. Select one
  and press **Delete Key**. Each row's button cycles Step, Lin and Smooth.
- **+ Event** adds a marker at the playhead; rename it in the Inspector.

Every Timeline edit is one Undo step. Only transform and visibility tracks
preview in the Scene view; color, emissive and field tracks play in the game.

## State machine

`transitions` turn the clips into a state machine, like Godot's
AnimationTree. Each transition names a `from` clip (empty means any clip),
a `to` clip, an optional parameter test, and a crossfade length:

```json
"parameters": { "speed": 0, "jump": 0 },
"transitions": [
  { "from": "", "to": "jump", "parameter": "jump", "compare": "Equal", "value": 1, "fade": 0.1 },
  { "from": "idle", "to": "run", "parameter": "speed", "compare": "Above", "value": 1, "fade": 0.2 },
  { "from": "run", "to": "idle", "parameter": "speed", "compare": "Below", "value": 1, "fade": 0.2 },
  { "from": "jump", "to": "idle", "at_end": true, "fade": 0.1 }
]
```

Each tick without a command, the first transition out of the playing
clip whose test holds starts its `to` clip. `compare` is `Above`, `Below`
or `Equal`; a missing parameter reads 0; an empty `parameter` always
passes. `at_end` also waits until the clip has played its whole length
once. A `Once` clip that has ended can still leave; one stopped by
`stop_animation` stays. Game code sets parameters with
`set_animation_parameter`; a Field track on
`/parameters/<name>` can key them too. Edit transitions in the Inspector.

## Blend spaces

A clip with `blend` points is a 1D blend space, like Godot's
BlendSpace1D. Each point places a clip on the axis of one parameter:

```json
{ "name": "move", "duration": 1.0, "blend_parameter": "speed",
  "blend": [ { "clip": "walk", "at": 0 }, { "clip": "run", "at": 6 } ] }
```

At speed 3 the pose is half walk, half run. Every point clip is stretched
to the blend clip's length (1 s when `duration` is 0), so their cycles stay
in step and feet do not slide between strides. Below the first point and
above the last, that clip plays alone. The blend clip's own tracks and
markers still play, on top of the mix. Play it, crossfade to it, or use
it as a state like any clip; a blend point that is itself a blend space
plays only its own tracks. The Timeline previews the mix at the current
parameter values.

Set `blend_parameter_y` for a 2D blend space, like Godot's BlendSpace2D:
walk and strafe directions on two axes. Each point also gets `at_y`:

```json
{ "name": "locomotion", "blend_parameter": "move_x", "blend_parameter_y": "move_z",
  "blend": [ { "clip": "idle", "at": 0, "at_y": 0 },
             { "clip": "forward", "at": 0, "at_y": 1 },
             { "clip": "back", "at": 0, "at_y": -1 },
             { "clip": "left", "at": -1, "at_y": 0 },
             { "clip": "right", "at": 1, "at_y": 0 } ] }
```

Every point is weighted by gradient bands: on a point's spot it plays
alone, between points their weights fall off with distance along each
pair, and past the outer points the nearest edge clips play. Points need
not form a grid.

## Layers

`layers` play clips on top of the state machine, each on its own clock
from scene start, in list order:

```json
"layers": [
  { "clip": "wave", "weight": 1.0, "mask": ["Body/Arm"] },
  { "clip": "breathe", "additive": true, "weight_parameter": "tired" }
]
```

- An override layer replaces the tracks it keys by `weight` (0 to 1).
  Tracks the state does not key are written at full value.
- An `additive` layer adds how far its clip has moved from its own first
  frame: positions and Euler rotations add, scales multiply, `Orientation`
  turns in the joint's own frame. Key the first frame as the rest pose.
  It only changes tracks the state pose keys.
- `mask` lists target paths the layer may write, each with its children:
  `Body/Arm` covers `Body/Arm/Hand`; `""` is the object itself. Empty
  writes everything, so an upper-body wave over a walk masks the spine.
- `weight_parameter` reads the weight from a parameter, so game code fades
  a layer with `set_animation_parameter`.

Layer markers do not fire events, and the Timeline preview shows only the
clip it edits.

## Root motion

A walk clip often moves the hips forward each stride. `root_motion` turns
that into motion of the whole character instead:

```json
"root_motion": "InPlace", "root_bone": "Armature/Hips"
```

`root_bone` is the path of the bone whose `Position` track carries the
motion (empty: the animated object's own track). With any policy but
`Off`, that track stays at its first-frame x and z (its height still
plays, so the hips bob), and the horizontal distance it would have moved
is collected each tick, across loop wraps and crossfades:

| `root_motion` | Effect |
|---|---|
| `Off` | The clip moves the bone as keyed. |
| `InPlace` | Nothing moves. Game code calls `scene.take_root_motion("Hero")` and feeds the distance into its own controller, such as `move_character`, so walls and floors still stop it. |
| `Transform` | The object's position moves, turned by its rotation and scale. Only for objects without a physics body. |
| `Velocity` | Sets the object's GPU body velocity (angular to 0) through the physics command bridge. |

Root motion never touches physics except through `Velocity`'s commands,
so a simulation stays deterministic. Turning in place (yaw) and vertical
motion are not extracted.

## Retargeting

`rusting scene retarget` copies a clip from one character to another whose
skeleton is built differently, such as a Mixamo walk onto your own rig:

```sh
rusting scene retarget scenes/main.rscene Mixamo walk Knight --dry-run
```

Bones pair up by name, ignoring case, separators and rig prefixes
(`mixamorig:LeftArm` matches `Left_Arm`). When names differ, give either
rig a `humanoid` map of standard names to its bone paths:

```json
"humanoid": [{"bone": "Hips", "path": "Pelvis"}, {"bone": "LeftArm", "path": "Pelvis/ArmL"}]
```

Each rotation key keeps its turn away from the source's rest pose, so a
target bone that points along a different axis still swings the same way.
Hips position keys are scaled by the ratio of the two hip heights; other
position and scale keys are dropped, as are bones the target lacks. Rest
poses are the scene's transforms, so retarget while the scene is stopped.
The clip lands on the target's `rusting.animation`, replacing a clip of the
same name. Blend spaces are retargeted one point clip at a time.

## Inverse kinematics

`rusting.ik` on the end of a joint chain makes it reach for or look at
another object, after the clip pose is written each fixed tick:

```json
"rusting.ik": { "kind": "TwoBone", "target": 12, "pole": 13, "weight": 1.0 }
```

- `LookAt` turns the object so its local `forward` axis (default
  `[0, 0, -1]`) points at `target`: heads, turrets, eyes.
- `TwoBone` bends the object's parent and grandparent (upper arm and
  forearm, thigh and shin) so the object's origin lands on `target`. A
  target out of reach straightens the chain toward it. The middle joint
  bends toward `pole` if set, else keeps the bend plane it has.
- `Foot` is `TwoBone` toward the ground instead of a target, for feet on
  slopes and stairs. A ray from `reach` metres (default 0.5) above the
  character's floor finds the physics collider under the foot, and the
  foot keeps its animated height above that ground: a planted foot lands
  on the step, a lifted one clears it by the same amount. The character
  is the nearest parent with `rusting.animation`; its own colliders are
  skipped. Set `pole` to a point in front of the knee. The foot turns to
  the slope under it. When the ground under a foot is lower than the leg
  can reach, the hips (the parent of the thigh) drop until it reaches,
  as far as the lowest foot needs.
- `Chain` bends the `joints` (default 3) joints above the object, for
  tails, spines, tentacles and cranes, so the object reaches `target`
  (FABRIK). The top joint stays where it is; a target out of reach
  straightens the chain toward it. It keeps the bends the pose already
  has as its starting guess, so key a curl to choose which way it bends.
- `weight` blends from the clip pose (0, no solve) to the full solve (1).
  Each tick starts from the clip pose, or from the pose before last
  tick's IK on joints no clip drives, so results never pile up.

Pick the target and pole in the Inspector. Move the target from game code
or a clip to drive the chain. IK changes joint rotations and, for `Foot`,
the hips' position; bone lengths stay. The Timeline preview does not run IK.

## Ragdolls

`rusting.ragdoll` on a character lets its bones go limp under CPU physics
and blend back to the animation afterwards:

```json
"rusting.ragdoll": {
  "bones": [
    { "path": "Armature/Hips", "length": 0.25, "radius": 0.12, "mass": 10.0 },
    { "path": "Armature/Hips/Spine", "length": 0.4, "radius": 0.12, "mass": 12.0 }
  ],
  "hit_speed": 6.0, "recover_after": 2.0, "blend_time": 0.5
}
```

- Each bone gets a capsule body `length` metres long along its local +Y,
  starting at the bone. It starts with the speed the animation gave it.
- A bone's body joins the nearest ancestor bone's body with `joint`
  (default a `ConeTwist` of 0.7 rad swing and ±0.4 rad twist). `frame`
  turns the joint axes; the default puts X along the bone.
- The character goes limp when game code calls `set_ragdoll(name, true)`,
  or when a CPU body hits its colliders at `hit_speed` m/s or faster (0:
  only on command). Its own collider is off while limp.
- After `recover_after` seconds (0: wait for `set_ragdoll(name, false)`)
  the character moves to where its hips lie, the bodies go away, and the
  bones blend back to the animation over `blend_time` seconds.
- Bones without an entry keep their animated local pose.

`scene.generate_ragdoll("Hero", 70.0)` fills `bones` from the joints of
the `Skin`s under the character, sharing 70 kg by volume, and keeps the
other settings. Each bone runs to its farthest child joint along its +Y
(an end bone gets half its parent's length), its radius is a quarter of
its length, and bones under 5 cm (fingers, toes) get no body. Treat the
result as a start: widen the hips and chest by hand.

### Active ragdolls

Set `muscle` (Hz) above 0 and the character is physical all the time: its
bodies exist from the first tick, and muscles turn each body toward its
bone's animated pose, relative to its parent body. The top body is held to
the animated hips, so the character stands and walks with its clips.
Where no clip keys a bone's position, rotation or scale, that part is held
to the bone's pose from when the muscles took over, so a clip that keys
only the hips' position still keeps them upright.

- A push or a light hit bends the bodies, and the muscles pull them back.
- A hit at `hit_speed` or `set_ragdoll(name, true)` drops the muscles. After
  recovery the character moves under its hips and the muscles regain
  strength over `blend_time`, so it is pulled back up.
- 5 Hz is loose and floppy, 15 Hz is stiff. A bone held away from upright
  sags a little under gravity; stiffer muscles sag less.
- `set_ragdoll_muscle(name, 0.0)` lets it go limp and then return to plain
  animation; `set_ragdoll_muscle(name, 10.0)` makes it active again.
- `set_position` moves the character at once, but the bodies spring after
  it at the muscle's rate. For a teleport, call
  `scene.reset_ragdoll(name)` right after: the bodies respawn at rest on
  the bones next tick. On a limp character it stands it up at once.
- Rays hit the bodies. A hit reports the bone's name, and
  `raycast_skipping` skips the bodies by the bone's classes: give every
  bone the character's class and its own sight rays pass through its limbs.

The character's own collider stays off while it is active; the bodies
collide instead.

Getting up keeps the character's height and facing. List bones from the
hips out; give every limb its own entry, or it hangs stiff.

## Game code

```rust
scene.play_animation("Door", "open");          // from the start
scene.crossfade("Hero", "run", 0.25);          // blend over 0.25 s
scene.stop_animation("Door");                  // hold the current pose
scene.set_animation_speed("Fan", 2.0);
scene.set_animation_parameter("Hero", "speed", 3.5);  // state machine input
let step = scene.take_root_motion("Hero");     // local metres since last call
scene.generate_ragdoll("Hero", 70.0);          // bones from the skeleton
scene.set_ragdoll("Hero", true);               // go limp; false gets up
scene.set_ragdoll_muscle("Hero", 10.0);        // active ragdoll; 0 is passive
if scene.is_limp("Hero") { /* knocked down */ }
if !scene.is_playing("Door", "open") { /* finished */ }
for event in scene.animation_events() {
    if event.name == "footstep" {
        scene.play_sound("sfx/step.wav", 0.6);
    }
}
```

Commands apply on the next fixed tick. `animation_events()` returns the
markers passed since the last frame, ordered by tick, then by object, then
by time. Each `AnimationEvent` has these fields: `tick` (the fixed tick the
marker was passed on), `entity`, `object` (the object's name), `clip` and
`name` (the marker's name). `rusting docs show api/AnimationEvent` prints
them.

## Rules

- A clip writes its tracks every tick it plays, after `rusting.tween`. Do
  not animate the same property with both.
- A crossfade blends matching tracks of the old and new clip. Tracks only
  the old clip has hold their last value until the fade ends.
- Stopping holds the pose. Playing the clip again starts it over.
