# Pitfalls

Mistakes that games built with this engine have actually made, each with the
diagnostic code that catches it (or "none" when only this page does) and the
fix. Run `rusting check`, `rusting lint` and `rusting test` first: most of
these show up there. `rusting docs show code/<CODE>` explains any code.

## Game code does not compile

**Two `scene` borrows at once.** `let a = scene.object("A"); scene.object("B")...`
fails with E0499 or E0502.
Code: `RUST_ENGINE_HINT`.
Fix: copy values out first, `let p = scene.object("A").position();`, then
write with a second statement.

**A guessed method name.** E0599 on `GameScene` or `GameObject`.
Code: `RUST_ENGINE_HINT`.
Fix: `rusting docs search <word>` lists the real names.

**`f64` or tuples where the scene wants `f32` arrays.** E0308.
Code: `RUST_ENGINE_HINT`.
Fix: write `1.0_f32` or `x as f32`; positions, rotations (radians) and scales
are `[f32; 3]`.

**Importing from engine modules.** E0432 on `rusting_engine::...`.
Code: `RUST_ENGINE_HINT`.
Fix: `use rusting_engine::prelude::*;` and `rusting_engine::egui` for UI.

## Objects do not collide, or are hit where they are not drawn

**A collider with no body.** Raycasts, `aim` and physics skip it silently.
Code: `SCENE_COLLIDER_WITHOUT_BODY`.
Fix: add a `physics_body`, `Static` for scenery, `Cpu` for things that move.

**A collider sized by hand that does not match the mesh.**
Code: `LINT_COLLIDER_MISMATCH`.
Fix: a 1 m cube has half extents 0.5; make trigger zones sensors.

**A scaled player.** The controller is the wrong height and steps fail.
Code: `LINT_PLAYER_SCALE`.
Fix: scale 1 on the player and its parents; size the collider instead.

**Hiding an object with a zero scale.**
Code: `LINT_ZERO_SCALE`.
Fix: set `visible: false`, or `scene.set_visible(name, false)`.

**A pickup or goal placed inside a wall or the floor.**
Code: `LINT_GOAL_INSIDE`.
Fix: move the sensor's centre into open space.

## The view or the HUD is wrong

**A camera inside a wall, pillar or other solid.** The view shows only
the inside of the solid.
Code: `LINT_CAMERA_INSIDE`.
Fix: move the camera out, or parent it to the player.

**A full-screen egui menu hides the game.** A `CentralPanel` fills the
background layer with an opaque `panel_fill`, so shapes painted behind it and
the 3D view vanish.
Code: none.
Fix: give the panel `Frame::NONE` or your own translucent fill; see
`guide/menus-and-ui`.

**HUD text too small to read, anchored off screen, or faint on a button.** A
right-anchored text with a `{counter}` placeholder grows past the edge; dark or
translucent text on a button vanishes into the dark button fill.
Code: `LINT_TEXT_SMALL`, `LINT_TEXT_OFFSCREEN`, `LINT_TEXT_CONTRAST`.
Fix: font size 14 or more; offset text inward from its anchor and leave room
for the longest value; keep button text light and opaque.

**More lights than the renderer uploads.** The extra lights light nothing.
Code: `LINT_LIGHT_BUDGET`.
Fix: hide lights until needed, remove some, or raise `render.quality`.

**A light that can never light anything.** Black, zero range or negative
intensity.
Code: `LINT_LIGHT_OFF`.
Fix: raise intensity and range; intensity 0 is fine for a light code turns on.

**A terrain cut into hundreds of small chunks.** Each chunk is one draw per
view and shadow cascade, so the frame becomes CPU-bound.
Code: `LINT_TERRAIN_CHUNKS` (more than 300 chunks).
Fix: set `chunk_cells` to 0 (the default) to pick the size from the
heightfield, or raise it.

## Assets and scene files

**An asset path in code that is not a file.** `play_sound` or `load_text` with
a wrong path fails only when it runs.
Code: `CODE_MISSING_ASSET`.
Fix: paths are relative to `assets/`; fix the path or add the file.

**A scene that names a missing asset.**
Code: `SCENE_MISSING_ASSET`.
Fix: import the file with `rusting asset import`, or fix the reference.

**A terrain layer image relative to the project root.** Layer `texture`,
`normal` and `roughness` paths are relative to the scene file, like every
scene asset path. A path written relative to the project root still loads,
with a warning. A layer image that fails to load paints the layer's plain
color and logs the path once.
Code: `SCENE_TERRAIN_ROOT_PATH`, or `SCENE_MISSING_ASSET` for a missing file.
Fix: write the path from the scene's folder, such as `../assets/textures/grass.png`.

**A misspelled scene key.** Loading ignores it and the field keeps its default.
Code: `SCENE_UNKNOWN_FIELD`.
Fix: `rusting fix` corrects close misspellings; check `rusting schema --json`.

## Runs do not repeat

**Timing by frames or wall-clock time.** Scenarios pass on one machine and fail
on another.
Code: `DETERMINISM_DIVERGED` (from `rusting determinism`).
Fix: time gameplay with `time.fixed_tick`.

**`rand` or the system clock.**
Code: `DETERMINISM_DIVERGED`.
Fix: `scene.random(stream)`; it repeats for a scenario's `seed`.

**Round state in Rust statics.** `restart` and `load_scene` reset the scene,
but statics keep last round's values.
Code: none.
Fix: keep round state in counters.

**Using objects after `restart` or `load_scene`.** Names looked up earlier in
the frame point at removed entities.
Code: none.
Fix: return from `update` right after the call.

**A score lost on `load_scene`.** Counters live in the scene.
Code: none.
Fix: read the counter before `load_scene` and set it after.

**Gameplay reading GPU bodies.** They reach game code a few frames late and
are not covered by `restart` determinism.
Code: none.
Fix: use `"simulation": "Cpu"` for anything gameplay reads; keep GPU bodies
for decorative piles. To put one GPU body back at a pose at rest, send
`scene.gpu_command(name, GpuBodyCommand::Teleport(transform))` and then
`GpuBodyCommand::SetVelocity` with zero velocities; both apply before the
next GPU tick.

## Tests prove less than they seem

**A scenario that passes without the feature it tests.**
Code: `SCENARIO_TOO_WEAK` (from `rusting test --without`).
Fix: add an `expect` on a value the feature drives.

**A stale CLI or project guide.** Docs, schema and checks miss newer features.
Code: `CLI_OUTDATED`, `AGENTS_OUTDATED`.
Fix: rebuild the CLI from the engine the game uses; run `rusting fix` for
AGENTS.md.

**A recipe applied to a scene with no `Player`.**
Code: `RECIPE_NEEDS_PLAYER`.
Fix: name the player object `Player`, or start from a player template.
