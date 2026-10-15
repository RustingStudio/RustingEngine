# Cameras

A scene can hold any number of `rusting.camera` entities. Only active
cameras render; an inactive camera costs nothing.

## Which camera fills the window

Among active cameras **without** a viewport, the highest `priority` fills
the window. On a tie, the one spawned first wins. That camera also aims
pointer clicks, `scene.pointer_ray()`, `expect_screen` and the
3D sound listener.

## Split screen and picture-in-picture

`viewport` is the part of the window a camera draws into, as fractions
`[x, y, width, height]` from the top-left corner. Every active camera with a
viewport draws after the full-window camera, lower priority first, so a
higher priority view lands on top.

Two players side by side:

```json
{"name": "P1 Camera", "components": {"rusting.camera": {
  "projection": {"Perspective": {"vertical_fov_radians": 1.0, "near": 0.1, "far": 200.0}},
  "active": true, "priority": 0, "viewport": [0.0, 0.0, 0.5, 1.0]}}}
{"name": "P2 Camera", "components": {"rusting.camera": {
  "projection": {"Perspective": {"vertical_fov_radians": 1.0, "near": 0.1, "far": 200.0}},
  "active": true, "priority": 1, "viewport": [0.5, 0.0, 0.5, 1.0]}}}
```

A minimap is one full-window camera plus a small viewport camera with a
higher priority, for example `[0.75, 0.0, 0.25, 0.25]`.

When every active camera has a viewport, the uncovered parts of the window
are black.

From game code:

```rust
scene.set_camera("P2 Camera", true, Some([0.5, 0.0, 0.5, 1.0])); // on, right half
scene.set_camera("P2 Camera", false, None);                       // off
scene.set_active_camera("Cutscene Camera"); // the only active camera
```

`set_camera` leaves other cameras alone. `set_active_camera` turns every
other camera off.

`set_camera_fov` changes a perspective camera's vertical field of view in
radians, for a zoom; `camera_fov` reads it. Ease toward the target every
tick for a smooth zoom:

```rust
let fov = scene.camera_fov("Seat Camera").unwrap_or(1.0);
let target = if focused { 0.45 } else { 1.0 };
scene.set_camera_fov("Seat Camera", fov + (target - fov) * (8.0 * time.fixed_delta).min(1.0));
```

Both return false or `None` for an orthographic camera or a name that is
not a camera.

## Aiming a camera

Rotation is Euler angles in radians. X applies first, then Y, then Z.
Forward is -Z. `scene.basis(name)` returns the world `[right, up, forward]`
unit vectors of any object, parents included:

```rust
if let Some([_, _, forward]) = scene.basis("P1 Camera") {
    let eye = scene.object("P1 Camera").position();
    let hit = scene.raycast(eye, forward, 50.0);
}
```

`raycast_skipping(eye, dir, 50.0, &["glass"])` passes through objects in
those classes. `raycast_only(eye, dir, 50.0, &["island"])` is the reverse:
it hits only objects in those classes and passes through everything else. `raycast_visible(eye, dir, 50.0)` passes through hidden
objects and their children, such as templates kept for `spawn_copy`, so it
answers "can the player see this" where plain `raycast` still hits invisible
walls.

`on_screen(point)` gives where a world point shows in the active camera's
view, as fractions from the top-left corner, or `None` when the point is
behind the camera or outside the view. It ignores walls in front: follow it
with `raycast_visible` from `camera_ray`'s eye toward the point.

Raycasts and `aim` only hit colliders that have a `physics_body`. Give a
collider that never moves `{"simulation": "Static", "solver": "Full"}`.
`rusting check` warns with `SCENE_COLLIDER_WITHOUT_BODY` when a collider has
no body.

A `rusting.player_controller` owns the transform of its direct camera
children. It sets their pitch, and it sets their orbit offset when
`camera_distance > 0`. `camera_offset` moves the camera off the body, for an
over-the-shoulder view.

Where the eye sits:

- First person (`camera_distance` 0): the camera child's own local
  position, measured from the body centre. `camera_height` is not used. A
  non-zero `camera_offset` replaces the child's position. For a seated eye
  at 1.2 m with the body centre at 0.9 m, put the camera child at
  `[0, 0.3, 0]`.
- Third person: the orbit centre is `camera_height` above the body centre,
  plus `camera_offset`. The child's own position is overwritten.

`mouse_look: false` stops the controller from
capturing the mouse, so a second player can use it.

With `mouse_look` on, the first left click captures the cursor and Escape
releases it. A captured cursor has no screen position, so `pointer_ray()`
is not useful while looking. To click things while looking, use
`scene.aim(distance)`, the ray through the centre of the view, on a left
click, and draw a crosshair or aim dot at the centre of the screen.
`pointer_ray()` is for views where the cursor stays free: set
`mouse_look: false` for those. From game code,
`scene.set_mouse_look("Player", false)` frees the cursor for a menu or an
in-world screen, and `true` hands the view back and captures the cursor at
once while the window has focus. Losing focus (alt-tab, the Windows key)
frees the cursor until the next left click. Esc frees it too; a game that
uses an Esc itself (cancel placing a part) calls
`scene.set_cursor_captured(true)` on that frame to keep the view turning.

A scenario turns the view with `set` on the controller's `yaw` and `pitch`,
then clicks what the centre now faces:

```json
{"tick": 10, "set": {"entity": "Player",
  "path": "/components/rusting.player_controller/yaw", "value": 0.6}},
{"tick": 10, "set": {"entity": "Player",
  "path": "/components/rusting.player_controller/pitch", "value": -0.4}},
{"tick": 11, "press": "click"}
```

`pitch_limits` and `yaw_limits` bound the view, in radians, for a seated or
turret camera. A booth seat that turns ±100° and looks ±40° up and down:

```json
"rusting.player_controller": {"walk_speed": 0.0, "jump_speed": 0.0,
  "pitch_limits": [-0.7, 0.7], "yaw_limits": [-1.75, 1.75]}
```

The limits apply after mouse, stick and `set_look` changes in the same
frame, so the view never shows past them. `yaw_limits` is absolute yaw;
`null` (the default) turns freely.

## HUD per camera

`"camera": "P2 Camera"` on a `rusting.hud` element anchors the element to
that camera's viewport. The element shows only while that camera is
active.

`"follow": "Enemy"` pins the element over the object named `Enemy`, for
name tags, health bars and "press E" prompts. `follow_offset` lifts the
point in world units (`[0, 2, 0]` sits above the head), and `offset` then
moves it in pixels. The element hides while the object is behind the
camera or missing. It projects through `camera` when set, else the active
camera.

## Testing cameras in scenarios

- `expect_screen` with `"camera": "P2 Camera"` projects through that
  camera, active or not. `inside` and `min_share` are then fractions of its
  viewport.
- `capture` with `{"path": "p2.png", "camera": "P2 Camera"}` renders only
  that camera, over the whole frame and without the HUD.
- `expect_pixels` with `"camera"` checks a region of that camera's
  viewport in the real frame.
- With viewport cameras, `perf.render.cameras` lists each drawn camera
  with its `gpu_ms`, `draws` and `triangles`. Camera screens drawn in that
  frame follow, marked `"screen": true`, so a wall of monitors shows what
  each feed costs. A screen skipped by `update_every` that frame is not
  listed.

## Screens: a camera on a material

`rusting.camera_screen` on an object with a mesh shows what another
camera sees on that mesh, like a CCTV monitor, a mirror-like portal or a
rear-view screen:

```json
"components": {"rusting.camera_screen": {"camera": "Cam B", "size": [320, 180]}}
```

- `camera` is the name of a camera entity. It may be inactive
  (`"active": false`), so it draws only into the screen.
- `size` is the image size in pixels. Small sizes are cheaper and give a
  low-resolution monitor look.
- The image replaces the material's base color and emissive maps. Give
  the material some `emissive` so the screen glows, and give each screen
  its own material; screens sharing one material show one feed.
- Each screen that draws renders the whole scene once more. Keep sizes
  small, and use the options below to skip work.

### Keeping screens cheap

```json
"rusting.camera_screen": {"camera": "Cam B", "size": [320, 180], "update_every": 2, "enabled": true}
```

- `update_every` draws the feed every this many frames (default 1). At 2
  the feed costs half as much and looks like a low-frame-rate CCTV camera.
  Screens with the same value take turns, so a wall of six screens at 2
  draws three per frame.
- `enabled: false` stops drawing the feed and keeps its last image. Swap
  the material too if the monitor should go dark.
- A screen whose mesh is outside the view of every camera drawing to the
  window keeps its last image and does not draw. Turning around to face
  it draws it again on that frame.
- A new feed always draws once, even when disabled or off-screen, so it
  never shows an empty image.
- `grading` gives the feed its own color grading, such as scanlines,
  grain and color bleed for a VHS security-camera look. Without it the
  feed uses the scene's `rusting.color_grading`. The effects are listed in
  [look-and-feel.md](look-and-feel.md#film-and-crtvhs-effects).
- `exposure` multiplies the scene's tone mapping exposure for the feed
  only (default 1). At 4 a dark aisle reads on a security monitor while
  the player still sees it dark, with no extra lights.
- Feeds reuse the main view's GPU physics instead of simulating again, so
  GPU bodies in a feed lag the main view by one frame.

Measured with `render_bench` on an RTX 3060 at 1920x1080 (balanced
quality, 600 frames), six 320x180 screens over the benchmark scene with
its 1,000 GPU bodies: 134.9 ms per frame before these changes (every feed
re-simulated physics), 24.9 ms after, against 22.2 ms with no screens.
Without the GPU bodies, no screens take 3.1 ms, six screens 5.2 ms.
Run `cargo run --release --example render_bench -- balanced --screens 6`
to measure your own scene size.


- Pointer clicks, `pointer_ray()` and the sound listener use the
  full-window camera, even over another camera's viewport.
- Occlusion culling (`FrustumAndOcclusion`) keeps one depth history for all
  cameras. Use `Frustum` culling with split screen.
- Post effects (bloom, fog, tone mapping, render scale) apply to every
  camera alike. Per-camera effects are not supported.
- A camera screen's image cannot go into an egui image, and screens do not
  show other screens' feeds (they show the material without the feed).
  The editor viewport shows screens without their feeds; the game window
  and captures show them.
