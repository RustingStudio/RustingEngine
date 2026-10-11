# RustingEngine

<!--[![CI](https://github.com/RustingStudio/RustingEngine/actions/workflows/ci.yml/badge.svg)](https://github.com/RustingStudio/RustingEngine/actions/workflows/ci.yml)-->

**10,000 GPU-simulated cubes at more than 2,000 FPS on an RTX 3060.**

RustingEngine is a game engine written in Rust, made for big physics scenes:
thousands of falling, flying and colliding objects that would make most
engines give up long before your graphics card is even busy.

![10,000 cubes orbiting a planet at 2,191 FPS](docs/images/spaceCubes.jpg)

One planet, 10,000 moving cubes, 2,191 FPS at 1920×1080 on an RTX 3060 and a
Ryzen 5 7600X. The GPU was only 35% busy. This is a stress scene made to show
what the engine can do, not a promise that every game runs this fast.

## The idea

Most engines move every object on the CPU. That is fine for a few hundred
objects and painful for ten thousand. RustingEngine lets you choose: objects
your game needs to touch every frame stay on the CPU, and the crowd of debris,
particles and swarms lives on the GPU. The engine only tells your game what
matters, like "something entered this area", instead of copying every object
back each frame.

## What you get

- **An editor in the style of Blender and Godot.** Split and resize panels,
  drag in models and images, paint tile levels, preview the game without
  building it.
- **Real physics.** Stacks, rolling balls, ragdoll-like chains, hinges,
  springs, joints that break, water you can float in.
- **Good looking scenes.** Shadows, glass, reflections, fog, bloom and soft
  shadows in corners.
- **Plain Rust for game code.** Simple calls like "move this", "is that key
  held", "what did I hit". Use any Rust library you like.
- **Same result every time.** Record a play session and replay it exactly.
  Great for finding bugs that only happen once.
- **Games you can ship.** Export a ready-to-run game for Linux or Windows.

## Made for AI agents too

Every game in `samples/` was built by an AI agent using only the engine's
command-line tool, without opening the editor. The agent creates the project,
edits the scene, writes the code, plays the game in tests and looks at
screenshots to check its own work. New projects come with a guide that
teaches any agent how to do the same.

## Sample games

| Game            | What it is                                     |
| --------------- | ---------------------------------------------- |
| `sky_hop`       | 2D platformer with moving lifts and coins      |
| `brick_bounce`  | Classic brick breaker                          |
| `snake_trail`   | Snake on a tile map                            |
| `crate_keeper`  | Crate-pushing puzzle                           |
| `lantern_grid`  | Lights-out puzzle played with the mouse        |
| `putt_course`   | Mini golf with a rolling ball                  |
| `core_defense`  | Top-down turret shooter                        |
| `target_range`  | First-person shooting gallery                  |
| `tower_topple`  | Knock block towers down with thrown balls      |
| `hammer_run`    | Third-person obstacle course                   |
| `night_vault`   | Stealth game with patrolling guards            |
| `ember_arena`   | Third-person arena survival                    |
| `forever_bear_booth` | Horror night shift with CCTV monitors     |

## Quick start

You need Rust and a graphics card with Vulkan. Linux and Windows are
supported.

```bash
git clone https://github.com/RustingStudio/RustingEngine.git
cd RustingEngine
./scripts/run_editor.sh
```

Play a sample game:

```bash
cargo run --release --bin rusting -- run samples/sky_hop
```

See the 10,000 cubes for yourself:

```bash
cd testGame
cargo run --release
```

New here? Start with [Getting started](docs/getting-started.md) and the
[tutorials](docs/README.md). See the [changelog](CHANGELOG.md) for what is new
and the [roadmap](roadmap.md) for what comes next.

## Contributing

Bug reports, profiling results, documentation fixes, and focused pull requests
are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a PR.
Performance changes should include the scene, hardware, resolution, build mode,
and before/after measurements so results can be reproduced.

## License

RustingEngine uses the [Rusting Engine License 1.0](LICENSE.md). Games and other
created works may be commercial. Redistribution of the engine is subject to the
license terms and attribution requirements.
