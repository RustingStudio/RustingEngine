# RustingEngine documentation

Start with **Getting started**, then work through the tutorials in order.
Each tutorial builds a small, working game. The commands and code in them
were run against this version of the engine.

## Start here

- [Getting started](getting-started.md): install the tools, open the editor,
  create a project, and press Play.
- [Coming from Godot](godot-migration.md): Godot nodes, scenes, signals,
  resources and GDScript callbacks mapped to RustingEngine.
- [Core concepts](concepts.md): projects, scenes, cooking, frames and fixed
  ticks, CPU and GPU physics, and where game code runs.

## Tutorials

1. [Hello cube](tutorials/01-hello-cube.md): move and spin a scene object
   from Rust with the short `rusting_game!` API. (Editor or CLI, 10 minutes.)
2. [Coin Run from the command line](tutorials/02-coin-run-cli.md): create a
   complete 2D game from the starter template, change it with scene
   patches, test it with a scenario, capture a screenshot, and export it.
3. [Gameplay plugins](tutorials/03-gameplay-plugin.md): write ECS systems,
   read input through named actions, and save your own components in
   scenes.
4. [GPU cube rain](tutorials/04-gpu-cube-rain.md): simulate thousands of
   bodies on the GPU and get only the events you ask for back in Rust.
5. [Physics sandbox](tutorials/05-physics-sandbox.md): make a ball bouncy,
   throw balls from the camera with an input action, test it first, and
   check that debug and release builds simulate the same.
6. [Multiplayer physics](tutorials/06-multiplayer-physics.md): turn the
   sandbox into a networked game with a dedicated server, and test a client
   against it.

## Guides and reference

- [Editor guide](../editor_gui.md): areas, Scene View navigation and the
  transform gizmo, Hierarchy, Inspector, assets, Preview and Play, export,
  the Profiler, and keyboard shortcuts.
- [README: headless CLI](../README.md#headless-project-cli): every `rusting`
  command, scene patches, and the scenario format.
- `rusting schema`: the full catalog of commands and scene components, with
  each field's default, unit, and valid range. It is generated from the
  engine itself, so it is always current.
- API reference: `rusting docs show component/<name>` (also `resource/` and
  `asset/`) prints one reflected type's fields, generated from the same
  catalog, and `rusting docs show api/GameScene` indexes the gameplay
  methods. For the Rust crates, `cargo doc --workspace --no-deps --open`
  builds the full reference; CI fails on a broken link in it.
- [Assets](assets.md): importing images, models and sounds with their
  license record, generating assets, and exporting the game.
- [2D games](2d.md): orthographic cameras, sprites, tile maps, platformer
  movement and bodies kept in the plane.
- [Input](input.md): actions bound to keys, mouse and gamepad, reading
  them per frame or per tick, rebinding, and pressing them in tests.
- [Physics guide](physics.md): CPU or GPU ownership, how state and events
  travel between them and how late, determinism, and which solver fits
  which job.
- [Determinism](determinism.md): what makes a simulation reproducible, state
  hashes, `rusting determinism`, and replays.
- [Architecture](../architecture.md): how the runtime, renderer, and editor
  fit together.
- [Animation](animation.md): keyframe clips, skinned models, state
  machines, blend spaces, IK, and ragdolls.
- [Effects](effects.md): particle emitters and ready-made presets.
- [Look and feel](look-and-feel.md): art presets, color grading, and a
  polish checklist.
- [Cameras](cameras.md), [Audio](audio.md), and
  [Menus and UI](menus-and-ui.md).
- [GPU condition shaders](gpu-condition-shaders.md): custom checks and
  solvers for GPU physics.
- [WebAssembly scripts](scripting.md): the optional sandboxed scripting
  host for modding and designer logic, its host functions, and its limits.
- [Writing a plugin](plugin-authoring.md): reusable plugin crates, naming,
  determinism rules, and testing a plugin without a window.
- [Development environment](dev-environment.md): iteration speed targets, running GPU tests without a
  GPU (lavapipe).
- [Changelog](../CHANGELOG.md): what changed in each release.
- [Roadmap](../roadmap.md): what is done and what comes next.
