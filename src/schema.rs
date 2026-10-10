//! Machine-readable catalog of CLI operations, scene sections, and scene
//! components, printed by `rusting schema`.
//!
//! Defaults are not written by hand: they are the scene form of each
//! component's `Default`, read back through the same code that saves scenes.
//! Tests check that every documented field exists, every saved field is
//! documented, and every example loads through the scene parser and the
//! component registry.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::reflect::{EnumInfo, Hints, TypeInfo, TypeRegistry, VariantFields};
use crate::runtime::{
    add_registered_component, registered_component_names,
    registered_component_values, scene_document, Camera, Collider,
    CollisionLayers, DirectionalLight, GpuPhysicsWatch, Name, PhysicsBody,
    PhysicsSyncMode, PointLight, RigidBody, SceneComponentRegistry, SceneId,
    SpotLight, SCENE_FORMAT_VERSION,
};
use crate::{App, AssetPlugin, Transform};

/// Version of the catalog layout. Raise it when a key changes meaning.
/// Version 2 derives component fields from reflection, with `*` for map
/// keys, and adds `resources` and `asset_types`.
pub const SCHEMA_CATALOG_VERSION: u32 = 2;

/// One `rusting` command.
pub struct Operation {
    /// Words that select the command, as `rusting --help <topic>` takes them.
    pub name: &'static str,
    /// Arguments after `rusting`.
    pub usage: &'static str,
    pub summary: &'static str,
    /// Whether the command needs Vulkan, and what it reads back.
    pub gpu: &'static str,
    /// Flag defaults as `(flag, value)`.
    pub defaults: &'static [(&'static str, &'static str)],
    /// Arguments after `rusting`; relative paths name a project `my_game`
    /// in the current folder.
    pub example: &'static str,
}

const NO_GPU: &str = "none";

/// Operations that change nothing on disk; every other one is marked
/// mutating for tool clients (`rusting mcp`).
pub const READ_ONLY: &[&str] = &[
    "doctor",
    "project inspect",
    "scene inspect",
    "scene map",
    "scene query",
    "validate",
    "lint",
    "check",
    "inspect",
    "determinism",
    "asset list",
    "diff",
    "preset list",
    "effect list",
    "recipe list",
    "docs",
    "explain",
    "schema",
];

/// Operations that write project files (scenes, assets, sources, the
/// manifest). Build outputs and captures do not count.
pub const WRITES_PROJECT: &[&str] = &[
    "new",
    "scene patch",
    "fix",
    "asset import",
    "asset reimport",
    "asset generate",
    "add scenario",
    "add system",
    "preset apply",
    "effect apply",
    "recipe apply",
    "cook",
];

/// What a finished command changed: `disk` for project files, `editor` and
/// `game` for a live editor buffer or running game. A command line tool
/// never writes either of those itself: an open editor picks up a disk
/// change on its next scene poll, and `rusting debug` reports `game`.
pub fn touched(args: &[String], ok: bool) -> serde_json::Value {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|arg| !arg.starts_with("--"))
        .collect();
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let writes = WRITES_PROJECT.iter().any(|name| {
        let name: Vec<&str> = name.split(' ').collect();
        words.starts_with(&name)
    });
    serde_json::json!({"disk": ok && writes && !dry_run, "editor": false, "game": false})
}

pub const OPERATIONS: &[Operation] = &[
    Operation {
        name: "doctor",
        usage: "doctor [--probe] [--json]",
        summary: "Report engine, platform, build tools, and Vulkan device capability. No GPU is a valid result. --probe also opens the device a run would pick, runs a tiny GPU job on it and reports probe.selected_device, or probe.ok false with the error (15 s limit).",
        gpu: "optional: lists devices when a driver is present",
        defaults: &[],
        example: "doctor --json",
    },
    Operation {
        name: "new",
        usage: "new <parent-directory> <project-name> [--template 3d|first-person|third-person|sandbox|2d|starter|puzzle|top-down|racing|twin-stick|tower-defense|roguelike|card-game|rhythm|empty] [--json]",
        summary: "Create a project from a template: 3d is the editor's lit cube; first-person and third-person a walkable room with a player controller and a passing tests/walk.json; sandbox a pile of dynamic bodies that fall and settle, with a passing tests/drop.json; 2d a playable side-view level with a passing tests/run.json; starter the complete Coin Run game (collect five coins, then touch the flag); puzzle Box Push, a grid puzzle with game code and a passing tests/solve.json; top-down Arena, a top-down action game with code and a passing tests/fight.json; racing Circuit, three laps of a track with code and a passing tests/lap.json; twin-stick Swarm, a twin-stick shooter with code and a passing tests/wave.json; tower-defense Outpost, towers on pads holding a path against a wave with code and a passing tests/defend.json; roguelike Crypt, three seeded floors of turn-based fighting with code and a passing tests/descend.json; card-game Duel, a card battle from a seeded deck with code and a passing tests/duel.json; rhythm Beat, notes in three lanes scored by timing with code and a passing tests/song.json; empty only a camera, so game code can spawn any name. The parent must exist and the project folder must not.",
        gpu: NO_GPU,
        defaults: &[("--template", "3d")],
        example: "new . my_game --json",
    },
    Operation {
        name: "project inspect",
        usage: "project inspect [project-root] [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Inspect and validate project.json, Cargo.toml, the main scene path, and src/main.rs.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "project inspect my_game --json",
    },
    Operation {
        name: "project summary",
        usage: "project summary [project-root] [--budget N] [--json]",
        summary: "A budgeted overview of a project: every scene and prefab (.rscene under scenes/ and assets/) with its entity count, components and referenced assets; every game code file with its functions that take GameScene and the asset paths it names; and how many entities use each component. Over budget, the longest lists are halved and data.omitted counts the cut items.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder"), ("--budget", "2000 tokens")],
        example: "project summary my_game --budget 1000 --json",
    },
    Operation {
        name: "systems",
        usage: "systems [root] [--reads TYPE | --writes TYPE] [--json]",
        summary: "The engine's ECS systems by stage with the components and resources each reads and writes (short type names, such as Transform). --reads or --writes keeps the systems that touch that type; systems with `all: true` take the whole World, may touch anything, and are always kept. Inside a project (root defaults to `.`) it adds the game code functions with stage `Game`, `file` and `line`: bevy system parameters (`Query<&mut Health>` writes Health, `Res<T>` reads T) and the `GameScene` calls each makes (`set_position` writes Transform, `set_counter` writes Counter, `set_field` with `/components/game.health/..` writes game.health). The game scan reads source text, so calls through helpers or macros are missed.",
        gpu: NO_GPU,
        defaults: &[],
        example: "systems --writes Transform --json",
    },
    Operation {
        name: "impact",
        usage: "impact <file-or-component> [project-root] [--json]",
        summary: "What a change would touch, before you make it. A file (relative to the project or to assets/: a texture, mesh, sound, data file or prefab) lists the scenes and entities that reference it and the code lines that name it; anything else is a component name (collider, rusting.sound_cue, game.health) and lists the entities that have it and the code lines that mention it. Scenarios run the main scene and the game code, so a hit in either lists every scenario in tests/; otherwise only scenarios that name the target. Each scene shows its first 10 entity names and entity_count.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "impact textures/crate.png --json",
    },
    Operation {
        name: "merge",
        usage: "merge <base> <ours> <theirs> [--output PATH] [--json]",
        summary: "Three-way merge of a scene file by entity ID and field, for git. Each side is read and migrated; a change on one side wins; each components entry merges on its own; an entity one side deleted and the other changed is kept. A field both sides changed differently keeps ours and is a SCENE_MERGE_CONFLICT error naming the entity and field, so git marks the file conflicted. Writes the result to <ours> (git's %A) unless --output is given. Set it up with `*.rscene merge=rusting-scene` in .gitattributes and `git config merge.rusting-scene.driver \"rusting merge %O %A %B\"`.",
        gpu: NO_GPU,
        defaults: &[("output", "<ours>")],
        example: "merge base.rscene scenes/main.rscene theirs.rscene --output merged.rscene --json",
    },
    Operation {
        name: "lease claim",
        usage: "lease claim <path> [--as NAME] [--for SECONDS] [--json]",
        summary: "Claim a file or folder of the project for one agent, so parallel agents do not overwrite each other. Until the lease is released or expires, every scene or prefab write by the CLI, daemon or editor to that path from any other holder fails with LEASE_HELD naming the holder; a claim that overlaps another agent's lease fails the same way. The holder is --as or RUSTING_AGENT; writers name themselves through RUSTING_AGENT. Claiming again renews the lease. Leases live in .rusting/leases.json. Source files are not written by rusting, so a lease on one is a claim other agents see in `lease list`, not a lock.",
        gpu: NO_GPU,
        defaults: &[("as", "$RUSTING_AGENT"), ("for", "1800")],
        example: "lease claim scenes/main.rscene --as builder --for 600 --json",
    },
    Operation {
        name: "lease release",
        usage: "lease release <path> [--as NAME] [--json]",
        summary: "Release your lease on a path (the same path you claimed). Releasing a lease that is not there succeeds; another agent's lease fails with LEASE_HELD.",
        gpu: NO_GPU,
        defaults: &[("as", "$RUSTING_AGENT")],
        example: "lease release scenes/main.rscene --as builder --json",
    },
    Operation {
        name: "lease list",
        usage: "lease list [project-root] [--json]",
        summary: "Every live lease of the project: path, holder and expiry (Unix seconds).",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "lease list --json",
    },
    Operation {
        name: "provenance",
        usage: "provenance [project-root] [--json]",
        summary: "A portable record of what the project is made of, to attach to a build or compare between two: the tool version, every `rusting*` crate in Cargo.lock with its version and source (a git source carries the commit), one hash over the game code in src/, every imported asset with its hash, ID and .rmeta provenance (original, author, license, url, generator), the generator hooks in project.json, each scene's hash, and each scenario's seed, ticks and hash. Hashes are FNV-1a 64, as in .rmeta; a missing file hashes to null.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "provenance --json",
    },
    Operation {
        name: "log",
        usage: "log [project-root] [--json]",
        summary: "The operation journal, newest first. Every command that writes a project file through rusting (scene and prefab edits, merges, fixes, recipes, add scenario, add system, AGENTS.md refresh) is one operation: an 8-character ID, the Unix time, the tool (RUSTING_AGENT, or rusting), the command line, and each file with its content hash before and after (null before means created, null after means deleted). Contents are kept in .rusting/blobs/<hash>, so `diff .rusting/blobs/<before> .rusting/blobs/<after>` shows a change. Imported asset files and editor saves are not journaled.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "log --json",
    },
    Operation {
        name: "revert",
        usage: "revert [project-root] <op> [--json]",
        summary: "Undo one journaled operation by putting back every file it changed (deleting files it created). Refused with REVERT_CONFLICT, writing nothing, when one of those files changed since; the message names the later operation to revert first. The revert is itself an operation, so it can be reverted.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "revert 3fa2c1d9 --json",
    },
    Operation {
        name: "scene inspect",
        usage: "scene inspect <scene-path> [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Inspect a migrated scene: every entity's id and name, cameras, classes, assets, and reference warnings. `--fields id` or `--limit N` trims the lists; `scene query` gives full entities.",
        gpu: NO_GPU,
        defaults: &[],
        example: "scene inspect my_game/scenes/main.rscene --json",
    },
    Operation {
        name: "scene map",
        usage: "scene map <scene-path> [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Draw every tile map as rows of characters with a legend. Entities that sit over a map's cells appear as letters, with their name, ID, column and row. A text view of a 2D layout for a model without vision. A scene without a tile map gives an empty `maps` list; list its entities with `scene inspect`.",
        gpu: NO_GPU,
        defaults: &[],
        example: "scene map my_game/scenes/main.rscene --json",
    },
    Operation {
        name: "scene query",
        usage: "scene query <scene-path> [--id ID | --name NAME | --class CLASS | --component COMPONENT] [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Find entities by one filter. Results are ordered by persistent UUID and include full component data.",
        gpu: NO_GPU,
        defaults: &[("filter", "all entities")],
        example: "scene query my_game/scenes/main.rscene --name Cube --json",
    },
    Operation {
        name: "scene patch",
        usage: "scene patch <scene-path> <patch.json> [--dry-run] [--json]",
        summary: "Apply a patch batch atomically: every operation succeeds and the scene validates, or nothing is written. Reports the revision and a per-field diff. expected_revision and per-field expected values report SCENE_CONFLICT instead of overwriting newer edits.",
        gpu: NO_GPU,
        defaults: &[("dry-run", "false")],
        example: "scene patch my_game/scenes/main.rscene patch.json --dry-run --json",
    },
    Operation {
        name: "scene split",
        usage: "scene split <scene-path> [--json]",
        summary: "Turn a scene file into folder form: a `.rscene` directory holding `scene.json` (everything but the entities, which it lists by ID in order) and `entities/<id>.json` per entity. Every command, the editor and the runtime read and write both forms the same way, and a patch rewrites only the entity files it changed, so parallel edits to different objects do not conflict in git. A scene already in folder form is left alone.",
        gpu: NO_GPU,
        defaults: &[],
        example: "scene split my_game/scenes/main.rscene --json",
    },
    Operation {
        name: "scene join",
        usage: "scene join <scene-path> [--json]",
        summary: "Turn a folder-form scene (see `scene split`) back into one file. A scene that is already one file is left alone.",
        gpu: NO_GPU,
        defaults: &[],
        example: "scene join my_game/scenes/main.rscene --json",
    },
    Operation {
        name: "scene add-model",
        usage: "scene add-model <scene-path> <model.glb|gltf> [--name NAME] [--dry-run] [--json]",
        summary: "Place a glTF or GLB model in a scene as one object (named after the file, or NAME) with a child per node and primitive, keeping its materials and textures. Import the model under assets/ first with `asset import`, which records its license and author in the `.rmeta`; add-model takes no provenance flags. Move the object with a patch afterwards.",
        gpu: NO_GPU,
        defaults: &[("dry-run", "false")],
        example: "scene add-model my_game/scenes/main.rscene my_game/assets/models/tree.glb --name Tree --json",
    },
    Operation {
        name: "scene retarget",
        usage: "scene retarget <scene-path> <from-object> <clip> <to-object> [--dry-run] [--json]",
        summary: "Copy a clip from one character's skeleton to another's and save it on the target's rusting.animation (replacing a clip of the same name). Bones pair up by the objects' `humanoid` maps, or by bone name with rig prefixes such as mixamorig: dropped. Rotations keep their turn from the source's rest pose; only the hips keep position keys, scaled by hip height. Rest poses are the scene's transforms.",
        gpu: NO_GPU,
        defaults: &[("dry-run", "false")],
        example: "scene retarget my_game/scenes/main.rscene Mixamo walk Knight --dry-run --json",
    },
    Operation {
        name: "validate",
        usage: "validate [project-root] [--json]",
        summary: "Check project files, scene structure, referenced assets, and that every physics solver and custom shader in the main scene supports project.json `determinism` (Off | Local | CrossPlatform), without Vulkan or a window.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "validate my_game --json",
    },
    Operation {
        name: "lint",
        usage: "lint [project-root] [--json]",
        summary: "Presentation and design checks on the main scene, as warnings with codes: a player body outside 0.5 to 3 m tall (LINT_PLAYER_SCALE), a zero scale axis (LINT_ZERO_SCALE), a camera that starts inside another entity's box, sphere or capsule collider other than a player body (LINT_CAMERA_INSIDE), a sensor (pickup, goal or trigger) whose centre is inside another entity's solid box, sphere or capsule collider (LINT_GOAL_INSIDE), a sensor at the player's height that no walk or jump from the player's start reaches past fixed walls (LINT_GOAL_UNREACHABLE; walls named as strings in game code count as doors), HUD text below 14 px (LINT_TEXT_SMALL) anchored outside a 1280 x 720 view (LINT_TEXT_OFFSCREEN) or measured to run past its edge (LINT_TEXT_OVERFLOW, with the ui feature), HUD button text below WCAG contrast against the button fill (egui's dark one, or `button_fill` in assets/ui/theme.json), 4.5:1 or 3:1 from 24 px (LINT_TEXT_CONTRAST, with the ui feature), a solid collider more than twice or less than half the size of its entity's built-in cube, sphere, cylinder or capsule mesh on some axis (LINT_COLLIDER_MISMATCH), lights that can never light anything: negative intensity, zero range or black color (LINT_LIGHT_OFF), and visible lights past the quality profile's light budget (Eco 256, Balanced 512, High and Auto 1024), which the renderer drops (LINT_LIGHT_BUDGET), and counters that can never end a round: none has a target and game code never calls counter_complete, load_scene or quit (LINT_NO_ENDING; a scene with no counters is not judged), and a literal object name in game code, such as `scene.flash(\"Stairz\")`, that no scene or prefab has, with close names (LINT_MISSING_OBJECT; names built at run time and names a spawn call creates are skipped), and a literal action in `pressed`, `held`, `press_tick` or `binding` that no `rusting.input_action`, `rebind` call or built-in `player.*` action defines (LINT_MISSING_ACTION), and a literal class in `in_class` that no entity has and no other string in game code names (LINT_MISSING_CLASS); a literal `gpu_events` name that no rule in game code or a scene names (LINT_MISSING_EVENT); a literal scene path, prefab path or sound clip with no file there (LINT_MISSING_FILE); and a HUD element whose colour game code changes with `set_hud` while its text never changes and holds no counter readout (LINT_COLOR_ONLY_STATUS); and a counter with a target that nothing reacts to: no HUD element requires or shows it, no pickup requires it, and game code never names it (LINT_SILENT_GOAL); and a literal `tr(\"key\")` or `tr_count(\"key\", ..)` key in game code, or a `{tr:key}` in the main scene's HUD text, that a locale file in `assets/locales/` lacks; `tr_count` needs `key.one` and `key.other` (LINT_MISSING_TRANSLATION). Fails when any check warns.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "lint my_game --json",
    },
    Operation {
        name: "cook",
        usage: "cook [project-root] [--json]",
        summary: "Validate and cook the manifest's main scene to its configured cooked_scene path.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "cook my_game --json",
    },
    Operation {
        name: "fix",
        usage: "fix [project-root] [--dry-run] [--json]",
        summary: "Apply every certain fix `validate` finds to the main scene: a misspelled key is renamed in place, so nothing else in the file changes. --dry-run lists the fixes without writing. Problems without a certain fix stay as diagnostics.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "fix my_game --dry-run --json",
    },
    Operation {
        name: "check",
        usage: "check [project-root] [--json]",
        summary: "Validate the project, then compile its Rust code with a debug `cargo build`, the same build `run` and `test` reuse. The first build of a new project compiles the engine and takes minutes; later ones take seconds.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "check my_game --json",
    },
    Operation {
        name: "run",
        usage: "run [project-root] [--release] [--ticks N | --bench FRAMES | --server] [--timeout SECONDS] [--record FILE | --replay FILE] [--seed N] [--stderr] [--json]",
        summary: "Cook, build, and run the game from the project folder. --ticks N runs N fixed ticks without a window, saves the end state to build/final.rscene for `scene query`, and exits. --server runs it as a dedicated server: no window or renderer, one fixed tick per fixed step of real time, until game code quits (`scene.quit()`) or --timeout stops it; game code sees the `net::DedicatedServer` resource and should host; an exported game does the same with RUSTING_SERVER=1. --timeout stops a game still running; reaching it is not a failure. --record FILE saves the windowed session's input, frame times and per-tick state hashes to FILE when the window closes (a FILE ending in .scenario.json gets a scenario instead: press, release and tap steps for the named actions the player held, so a human can hand a bug to an agent as a test); --replay FILE plays such a file back without a window and fails at the first tick whose state differs, so a recorded run is a regression test. --bench FRAMES opens the window, skips 60 warm-up frames, measures FRAMES frames, closes, and reports their mean, p50, p95, p99 and max in timings.bench, with the median CPU and GPU time per frame and `bound` (\"cpu\" or \"gpu\"), the side that limits the frame. --seed N starts the game's random streams (`scene.random`, particles) from N instead of 0; a scenario's `seed` and a replay's recorded seed still win.",
        gpu: "required for a window; none with --ticks or --server",
        defaults: &[("--release", "off (debug build)"), ("--ticks", "off (opens a window)"), ("--bench", "off"), ("--timeout", "none"), ("project-root", "the current folder")],
        example: "run my_game --ticks 120 --json",
    },
    Operation {
        name: "test",
        usage: "test [project-root] [scenario.json | folder] [--release] [--timeout SECONDS] [--update-golden] [--keep-going] [--full] [--stderr] [--without COMPONENT]... [--exe GAME_EXECUTABLE] [--json]",
        summary: "Cook and build the game, then run a scenario file in it without a window: named actions at fixed ticks, checks on reflected scene state and events, and optional captures. Given a folder, runs every .json in it in name order and lists each result under `scenarios`; with neither argument, runs tests/. Fails with SCENARIO_FAILED and the first failing tick and step. --update-golden rewrites capture `golden` images instead of comparing them. --keep-going runs every step after a failed check and lists every failure, like `\"keep_going\": true` in the scenario. The result leaves out the per-tick `state_hashes` and `gpu_state_hashes`; --full keeps them, and build/scenario-report.json always has the whole report of the last scenario run. --stderr prints the game's stderr (`eprintln!`) as it runs instead of only in the result's `game.stderr` at the end. --without COMPONENT (repeatable) leaves that component out of every scene the game loads (`collider`, `rigid_body`, a light, or a registered component name) and inverts the result: the run passes when the scenario fails, with the failure under `without`, and fails with SCENARIO_TOO_WEAK when it still passes. --exe runs an already built game, such as an exported one, from its own folder instead of cooking and building the project: the scenario, report and test data stay in the project. The scenario format is under `scenario` in `rusting schema`.",
        gpu: "optional: only capture, expect_pixels and render-budget runs render; captures are skipped without Vulkan; one frame readback per capture or pixel check",
        defaults: &[("--release", "off (debug build)"), ("--timeout", "none"), ("project-root", "the current folder"), ("scenario", "every file in tests/")],
        example: "test my_game my_game/tests/falls.json --json",
    },
    Operation {
        name: "bisect",
        usage: "bisect <project-root> <other-root> [--ticks N] [--json]",
        summary: "Build two copies of a game, such as two git worktrees holding two builds or two revisions of the scenes, run each headless for N ticks, and compare every tick's world-state hash. Fails with DETERMINISM_DIVERGED naming the first tick whose state differs and the first entity that differs there (entities pair by scene ID, so added or removed entities still line up); passes when every tick matches. Hash reports go to build/bisect/ in each root.",
        gpu: NO_GPU,
        defaults: &[("--ticks", "600")],
        example: "bisect my_game ../my_game_main --ticks 300 --json",
    },
    Operation {
        name: "fuzz",
        usage: "fuzz [project-root] scenario.json [--seeds N] [--first-seed N] [--action NAME]... [--json]",
        summary: "Run a scenario once per seed with seeded random presses and releases of named actions (each action flips with chance 0.1 per tick), on top of the scenario's own steps and invariants. Stops at the first seed that fails a step or an invariant, or crashes the game, and writes it to build/fuzz/seed-N.json as a ready scenario: the presses as ordinary steps up to the failing tick, so `rusting test` replays the failure. Without --action, presses every action the scene binds; name actions that game code binds with --action. Passes, with `fuzz.tried`, when every seed passes.",
        gpu: "as for test",
        defaults: &[("--seeds", "20"), ("--first-seed", "0"), ("--action", "every action the scene binds"), ("project-root", "the current folder")],
        example: "fuzz my_game tests/invariants.json --seeds 50 --json",
    },
    Operation {
        name: "debug",
        usage: "debug [project-root]",
        summary: "Cook and build the game, then run it without a window as a debug session: the game's standard input and output carry one JSON request and one JSON reply per line. The game is paused between commands. Commands: `{\"cmd\": \"step\", \"ticks\": N}` (the only way time passes), `get` (`entity`, `path`), `set` (`entity`, `path`, `value`), `press` / `release` (`action`), `capture` (`path`, optional `size` [w, h]; needs Vulkan), `tick`, `quit`. A reply is `{\"id\", \"ok\", \"tick\", \"result\" or \"error\"}`. Build failures go to standard error as the usual result envelope.",
        gpu: "capture needs Vulkan; every other command does not",
        defaults: &[("project-root", "the current folder")],
        example: "debug my_game",
    },
    Operation {
        name: "serve",
        usage: "serve",
        summary: "Run as a local daemon: read one JSON-RPC 2.0 request per line from stdin and write one response per line to stdout, until `shutdown` or end of input. `method` is the command words (`scene query`), `params` the remaining arguments as an array of strings (or `{\"args\": [...]}`), and `result` the same envelope as `--json`. Every command is available except `serve`. Errors use the JSON-RPC codes -32700 (parse), -32600 (invalid request), -32601 (unknown method), -32602 (bad params) and -32603 (the operation panicked). Each request runs in the daemon process, so it costs no process start; builds, shaders and loaded scenes are not cached between requests yet.",
        gpu: NO_GPU,
        defaults: &[],
        example: "serve",
    },
    Operation {
        name: "relay",
        usage: "relay [address]",
        summary: "Run a multiplayer relay until stopped. A game hosts through it with `NetSession::host_room(relay, token)`, which hands out a six-character room code, and players join with `NetSession::join_room(relay, code, token)`; the relay forwards host messages to clients and client messages to the host, so neither side needs an open port. Messages are reliable and in order (TCP). Set `RUSTING_RELAY_TOKEN` to require that token from hosts and clients before exposing the relay on the internet; unset, anyone can use it. Each IP address may hold 16 connections and open 60 a minute, the relay holds 1024 in all, and each connection is read at up to 1 MiB/s; `net::run_relay_with` takes other `RelayLimits`. See `guide/networking`.",
        gpu: NO_GPU,
        defaults: &[("address", "0.0.0.0:7777")],
        example: "relay 0.0.0.0:7777",
    },
    Operation {
        name: "mcp",
        usage: "mcp [root]",
        summary: "Run as a Model Context Protocol server over stdio (newline-delimited JSON-RPC), scoped to the project `root` (default: the current folder). Every command except `serve`, `debug` and `mcp` is a tool named with underscores (`scene_query`) taking `{\"args\": [...]}`, the arguments after the command words. Tools that change nothing carry `readOnlyHint: true`; the rest are mutating. A result is the `--json` envelope as text, with `isError` set when `ok` is false, so it matches the CLI. Arguments with an absolute path or `..` are refused.",
        gpu: NO_GPU,
        defaults: &[],
        example: "mcp my_game",
    },
    Operation {
        name: "inspect",
        usage: "inspect [project-root] --tick N [--entity NAME]... [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Cook and build the game, run it without a window to tick N, and report the scene form of each named entity after that tick's update: transform, components and counters. With no `--entity`, reports every named entity of the main scene. Entities that do not exist at that tick are null. A player or platformer controller also gets `feel`: top and sprint speed, time to top speed, stopping distance, air control, jump apex height and time, air time and jump distance in metres and seconds, the genre ranges they are compared with, and `notes` for numbers outside them.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder"), ("--entity", "every named entity")],
        example: "inspect my_game --tick 120 --entity Player --json",
    },
    Operation {
        name: "determinism",
        usage: "determinism [project-root] [--ticks N | --scenario scenario.json | --gpu scenario.json] [--json]",
        summary: "Build the game in debug and release, run each headless for N ticks (release also pinned to one CPU when `taskset` exists), and compare every tick's world-state hash. Fails with DETERMINISM_DIVERGED naming the first divergent tick and entity. Hash reports go to build/determinism/<configuration>-<ticks>.json. With --scenario, it replays a scenario (its scripted input) in a debug and then a release build and compares the CPU world-state hash of every tick, and the GPU body hash of every tick both runs delivered (DETERMINISM_DIVERGED names the tick and data.divergence.state says CPU or GPU). GPU physics bodies do not simulate headless; for them use --gpu, which is --scenario that also fails with DETERMINISM_NO_GPU_STATE when the scenario hashed no GPU bodies (set \"gpu\": true in it). A failing expect step in the scenario does not stop the comparison; data.scenario_passed reports it. It catches order-dependent GPU code, not vendor differences. It always builds both profiles, so it takes no --release.",
        gpu: NO_GPU,
        defaults: &[("--ticks", "600"), ("project-root", "the current folder")],
        example: "determinism my_game --ticks 300 --json",
    },
    Operation {
        name: "export",
        usage: "export <project-root> <parent-directory> [--target TRIPLE] [--json]",
        summary: "Cook, build a release, and package the game into a new folder under the parent. A native export is verified by running a copy for 60 headless ticks from a fresh temporary folder.",
        gpu: NO_GPU,
        defaults: &[("--target", "host")],
        example: "export my_game exports --json",
    },
    Operation {
        name: "capture",
        usage: "capture <scene-path> <output.png> [--camera ID|NAME] [--tick N] [--size WxH] [--pick X,Y]... [--pick-rect X,Y,W,H]... [--at X,Y,Z [--look-at X,Y,Z | --look YAW,PITCH]] [--no-hud] [--game] [--json]",
        summary: "Render one camera of a scene offscreen to a PNG after N fixed ticks. --no-hud leaves the HUD out. --at renders from a camera placed at a point instead of a scene camera, facing a point (--look-at) or a yaw and pitch in radians (--look; default faces -Z); it takes its lens from --camera or the active camera. Each --pick maps a pixel to the persistent ID of the object under it. Each --pick-rect lists every object covering a rectangle with its share of the rectangle (sampled, at most 64x64 points). Game code is not run, unless --game: then the scene path only names the project, which is cooked and built, and its game runs from its own main scene with game code to tick N (at least 1) and is captured as `rusting test` would (--camera, --size and --no-hud apply; --pick, --pick-rect and --at do not). Without Vulkan, camera data and picks are still reported with a VULKAN_UNAVAILABLE error.",
        gpu: "required for the PNG: renders the last few ticks before N (every tick when the scene has GPU bodies) and reads back one frame (width x height x 4 bytes)",
        defaults: &[("--camera", "the scene's active camera"), ("--tick", "0"), ("--size", "1280x720")],
        example: "capture my_game/scenes/main.rscene shot.png --tick 60 --pick 640,360 --json",
    },
    Operation {
        name: "asset import",
        usage: "asset import <project-root> <source-file> [--to FOLDER] [--author A] [--license L] [--url U] [--generator G] [--notes N] [--max-size PIXELS] [--dry-run] [--json]",
        summary: "Copy a png, jpeg, bmp, tga, gltf, glb, wav or ogg file (and a glTF's external buffers and images) into assets/FOLDER (`sounds` and `assets/sounds` both mean assets/sounds) after loading it as the runtime would, and write <file>.rmeta with a new stable ID, settings, dependencies, content hash, and source/license provenance. Scenes reference it by the returned `reference` path (relative to the scene); sound clips (`rusting.sound_cue.clip`, `play_sound`) are relative to assets/ instead, e.g. `sounds/hit.wav`. Warns when no license is given. --dry-run runs every check on a staged copy and writes nothing (`dry_run: true` in the report).",
        gpu: NO_GPU,
        defaults: &[("--to", "assets/ itself"), ("--max-size", "none (images keep their size)")],
        example: "asset import my_game art/crate.png --to props --license CC0-1.0 --json",
    },
    Operation {
        name: "asset reimport",
        usage: "asset reimport <project-root> <asset-id-or-path> [--from FILE] [--author A] [--license L] [--url U] [--generator G] [--notes N] [--max-size PIXELS] [--dry-run] [--json]",
        summary: "Replace an imported asset and keep its ID: from --from, else the recorded original file if it still exists, else revalidate the asset in place. A file under assets/ with no .rmeta yet, such as a model added by `scene add-model`, is given a new ID and registered; a glTF also re-cooks its .rmesh files. Given provenance flags and settings replace the recorded ones. Reports the scenes that use it; a running game or editor hot-reloads the file. --dry-run previews the replacement without writing.",
        gpu: NO_GPU,
        defaults: &[("--from", "the recorded original, else the asset itself")],
        example: "asset reimport my_game assets/props/crate.png --from art/crate_v2.png --json",
    },
    Operation {
        name: "asset generate",
        usage: "asset generate <project-root> <hook> <prompt> [--to FOLDER | --replace ASSET-ID-OR-PATH] [--dry-run] [--json]",
        summary: "Run the optional generator hook `generators.<hook>` from project.json ({\"command\": [program, args...], \"license\": L, \"settings\": {\"max_size\": N}}) with RUSTING_PROMPT and an empty RUSTING_OUTPUT_DIR. The hook writes a file there and prints a last line of JSON {\"file\": NAME, \"license\"?, \"author\"?, \"url\"?, \"generator\"?, \"notes\"?}. The file then goes through `asset import` (or `asset reimport` with --replace), so type, size, dependencies, and metadata are checked the same way; output without a license from the hook or its config is rejected. Projects work without any hook. The hook name `sfx` is built in unless project.json defines it: the prompt is a preset (jump, coin, hit, explosion, laser, powerup, blip) and an optional whole-number seed, such as `coin 7`, and the same prompt always gives the same CC0 WAV. The hook name `texture` is built in the same way: a preset (grid, checker, bricks, planks, tiles, noise) and an optional seed that picks the colour from one shared palette, such as `bricks 3`, give the same 256 px tileable CC0 PNG. The hook name `sprite` is built in too: a shape (circle, square, triangle, diamond, star, heart) and an optional seed give a 128 px outlined CC0 PNG on a clear background, such as `star 2`. So is `mesh`: a preset (crate, barrel, rock, tree, gem) and an optional seed give a flat-shaded low-poly CC0 `.glb` standing on its origin, such as `barrel 4`.",
        gpu: NO_GPU,
        defaults: &[("--to", "assets/ itself"), ("--replace", "none (imports a new asset)")],
        example: "asset generate my_game icons red_crate --to props --dry-run --json",
    },
    Operation {
        name: "asset list",
        usage: "asset list [project-root] [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "List imported assets with ID, dependencies, provenance, settings, and referencing scenes. Fails on missing files or dependencies, invalid or duplicate metadata; warns on files changed since import, files without .rmeta, and assets without a license.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "asset list my_game --json",
    },
    Operation {
        name: "add scenario",
        usage: "add scenario [project-root] <name> [--json]",
        summary: "Write tests/<name>.json with a check that fails until you name a real entity and value. Red scenario first, then make it pass.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "add scenario my_game door_opens --json",
    },
    Operation {
        name: "add system",
        usage: "add system [project-root] <name> [--json]",
        summary: "Append a documented stub `fn <name>(_scene, _time)` to src/main.rs and write a failing scenario <name>.json. Call the function from `update`, replace the `todo!`, then make the scenario pass.",
        gpu: NO_GPU,
        defaults: &[("project-root", "the current folder")],
        example: "add system my_game spin_coins --json",
    },
    Operation {
        name: "diff",
        usage: "diff <scene-a> <scene-b> [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Compare two scene files by entity ID: entities added, removed and changed, with the JSON path, old value and new value of every changed field, plus changes to scene-level fields.",
        gpu: NO_GPU,
        defaults: &[],
        example: "diff old.rscene my_game/scenes/main.rscene --json",
    },
    Operation {
        name: "preset list",
        usage: "preset list [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "List the starter art-direction presets and every value each one writes.",
        gpu: NO_GPU,
        defaults: &[],
        example: "preset list --json",
    },
    Operation {
        name: "preset apply",
        usage: "preset apply <scene-path> <preset> [--only lighting,environment,camera,text] [--dry-run] [--json]",
        summary: "Apply an art-direction preset as one scene patch: sun, ambient and sky light, tone mapping, background color (scope `lighting`; `environment` is the same without the sun), perspective camera field of view (`camera`), and HUD text size and color (`text`). `--only` limits it to the named scopes; the result lists every changed field in `patch.changes`, so `--dry-run` previews them. Creates a Sun entity when the scene has no directional light and an Environment entity for ambient, sky, tone mapping, grading and background when no entity has them. The values stay ordinary, editable scene data; reapplying edits the same entities.",
        gpu: NO_GPU,
        defaults: &[("--dry-run", "false"), ("--only", "every scope")],
        example: "preset apply my_game/scenes/main.rscene night --only lighting --dry-run --json",
    },
    Operation {
        name: "effect list",
        usage: "effect list [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "List the particle effect presets (dust_motes, falling_leaves, snow, rain, sparks, smoke, fire, embers, fireflies, magic_sparkle, confetti) with a summary, a suggested height and the full `rusting.particle_emitter` each one writes.",
        gpu: NO_GPU,
        defaults: &[],
        example: "effect list --json",
    },
    Operation {
        name: "effect apply",
        usage: "effect apply <scene-path> <effect> [--on OBJECT | --name NAME --at X,Y,Z] [--dry-run] [--json]",
        summary: "Add a particle effect preset to a scene as one scene patch. `--on` sets the `rusting.particle_emitter` of an existing object (a torch, a chimney); otherwise a new object named after the effect is created at `--at`, or at the preset's suggested height above the origin. Every value stays ordinary, editable scene data.",
        gpu: NO_GPU,
        defaults: &[("--at", "0, the preset's height, 0"), ("--name", "the effect's name in title case, made unique"), ("--dry-run", "false")],
        example: "effect apply my_game/scenes/main.rscene fire --at 2,0,-3 --json",
    },
    Operation {
        name: "recipe list",
        usage: "recipe list [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "List the gameplay recipes (checkpoints, health, double_jump, inventory, day_timer, pause_menu, wave_spawner, turret, dash, alarm) with a summary and the call `update` makes.",
        gpu: NO_GPU,
        defaults: &[],
        example: "recipe list --json",
    },
    Operation {
        name: "recipe apply",
        usage: "recipe apply <project-root> <recipe> [--dry-run] [--json]",
        summary: "Write a gameplay recipe into a project: its source as `src/<recipe>.rs`, its objects into the main scene as one patch (placed from the object named `Player`), and a passing scenario as `tests/<recipe>.json`. Never overwrites a file. `src/main.rs` is left alone: `next` names the `mod` line and the call to add to `update`. A recipe that is only a scene change (`double_jump`) writes no source.",
        gpu: NO_GPU,
        defaults: &[("--dry-run", "false")],
        example: "recipe apply my_game checkpoints --json",
    },
    Operation {
        name: "docs",
        usage: "docs [--brief] [--budget TOKENS] | docs search <words> [--limit N] | docs show <id> [--budget TOKENS] [--json]",
        summary: "Read the manual, tutorials, agent guides, every command and every diagnostic code offline, from the installed version. With no arguments, list the items. `--brief` prints a compact overview sized to `--budget` tokens. `search` needs every word, ranks title hits first, shows the best 10 (`--limit N` for more) and reports `total` and `omitted`. `api/GameScene` (and `api/GameObject`, ...) lists every method of a type; `sample/<name>` prints a sample game's README and code. `show` prints one item, cut at a line to fit `--budget`, and reports whether it was cut.",
        gpu: NO_GPU,
        defaults: &[("--budget", "2000 tokens (about four characters each)"), ("--limit", "10 search matches")],
        example: "docs search scene patch --json",
    },
    Operation {
        name: "explain",
        usage: "explain [CODE] [--limit N] [--fields a,b] [--summary] [--json]",
        summary: "Explain a diagnostic code: what it means, how to fix it, and an example. With no code, list every code the tools can emit.",
        gpu: NO_GPU,
        defaults: &[],
        example: "explain SCENE_CONFLICT --json",
    },
    Operation {
        name: "schema",
        usage: "schema [NAME] [--json-schema] [--json]",
        summary: "Print this catalog: operations, scene sections and components with defaults, examples, units, ranges, and GPU cost. With --json-schema, print JSON Schema (draft 2020-12) for scene, scene_patch, scenario, project and asset_meta files instead. With NAME, print only that part: a component, resource or asset type (`player_controller` or `rusting.player_controller`), an operation, or a top-level section such as `scenario`; an unknown NAME lists the known ones.",
        gpu: NO_GPU,
        defaults: &[],
        example: "schema camera_screen --json",
    },
];

/// Documentation for one saved field. `path` is a JSON pointer relative to
/// the section; enum variants appear as keys, as they do in scene files.
struct Field {
    path: &'static str,
    unit: &'static str,
    range: &'static str,
    doc: &'static str,
}

const fn field(
    path: &'static str,
    unit: &'static str,
    range: &'static str,
    doc: &'static str,
) -> Field {
    Field {
        path,
        unit,
        range,
        doc,
    }
}

const RGB: &str = "linear RGB";
const METRES: &str = "m";
const RADIANS: &str = "rad";

/// A top-level entity key or a registered component.
struct Section {
    key: &'static str,
    summary: &'static str,
    gpu: &'static str,
    /// Scene form of an authored, non-default value.
    example: fn() -> Value,
    fields: &'static [Field],
}

/// Each registered component's key and summary, in catalog order.
pub(crate) fn component_summaries(
) -> impl Iterator<Item = (&'static str, &'static str)> {
    COMPONENT_SECTIONS
        .iter()
        .map(|section| (section.key, section.summary))
}

/// A registered component. Its fields come from its reflected type.
struct ComponentSection {
    key: &'static str,
    summary: &'static str,
    gpu: &'static str,
    /// Scene form of an authored, non-default value.
    example: fn() -> Value,
}

const ENTITY_SECTIONS: &[Section] = &[
    Section {
        key: "id",
        summary: "Persistent object ID. Unique in the scene; keep it when editing so references and patches stay valid.",
        gpu: NO_GPU,
        example: || json!("6f1c1d6e-9a53-4a6d-8f59-2c3e1c0c5a10"),
        fields: &[field("", "UUID", "unique", "")],
    },
    Section {
        key: "parent",
        summary: "Parent object ID; the transform is relative to the parent.",
        gpu: NO_GPU,
        example: || Value::Null,
        fields: &[field("", "UUID", "an existing object; no cycles", "null for a root object")],
    },
    Section {
        key: "name",
        summary: "Object name. Unique in the scene, so scenarios and queries can find it.",
        gpu: NO_GPU,
        example: || json!("Crate"),
        fields: &[field("", "", "unique", "")],
    },
    Section {
        key: "classes",
        summary: "Reusable class names. One class selects any number of objects, for queries and GPU physics rules.",
        gpu: NO_GPU,
        example: || json!(["crates", "breakable"]),
        fields: &[field("", "", "non-empty, no repeats", "")],
    },
    Section {
        key: "visible",
        summary: "Whether the object and its children render.",
        gpu: NO_GPU,
        example: || json!(true),
        fields: &[field("", "", "", "null means visible")],
    },
    Section {
        key: "transform",
        summary: "Position, rotation, and scale relative to the parent.",
        gpu: NO_GPU,
        example: || json!({"position": [2.0, 0.5, -1.0], "rotation": [0.0, 0.785, 0.0], "scale": [1.0, 1.0, 1.0]}),
        fields: &[
            field("/position", METRES, "", "[x, y, z]; +Y is up and -Z is forward"),
            field("/rotation", RADIANS, "", "Euler angles around X, Y, and Z, applied X first, then Y, then Z (R = Rz·Ry·Rx); forward is -Z: (-sin y·cos x, sin x, -cos y·cos x) for z = 0"),
            field("/scale", "factor", "> 0 on each axis", ""),
        ],
    },
    Section {
        key: "mesh_renderer",
        summary: "Mesh and material to draw.",
        gpu: "one draw per mesh and material batch; mesh and texture memory",
        example: || json!({
            "mesh": {"BuiltinPrimitive": "Torus"},
            "material": {"Inline": {
                "name": "", "model": "Pbr", "alpha_mode": "Opaque",
                "base_color": [0.8, 0.5, 0.2, 1.0], "emissive": [0.0, 0.0, 0.0],
                "metallic": 0.0, "roughness": 0.6,
                "transmission": 0.0, "ior": 1.5, "thickness": 0.0,
                "uv_scale": [1.0, 1.0], "uv_offset": [0.0, 0.0],
                "flat_shading": false,
                "base_color_texture": null, "normal_texture": null,
                "metallic_roughness_texture": null, "occlusion_texture": null,
                "emissive_texture": null
            }},
            "cast_shadows": true, "receive_shadows": true
        }),
        fields: &[
            field("/mesh/BuiltinPrimitive", "", "Cube, Sphere, Triangle, Plane, Tetrahedron, Octahedron, Dodecahedron, Icosahedron, Pyramid, Cylinder, Cone, Torus, Quad, RoundedCube, Capsule", "centered on the origin; at scale 1 Cube, RoundedCube, Sphere, Cylinder, Cone and Pyramid fill a 1 m box; Cylinder, Cone, Capsule and Pyramid run along Y (cone and pyramid tip at +Y); Capsule is 1 x 2 x 1; Torus lies in XZ, 1 x 0.28 x 1; Plane is flat in XZ facing +Y; Quad and Triangle are flat in XY facing +Z; table in docs/look-and-feel.md \"Mesh kit\". Other variants: \"BuiltinCube\", \"BuiltinSphere\", {\"AssetPath\": \"assets/model.gltf\"}"),
            field("/material/Inline/name", "", "text", "label shown in the editor; may be empty"),
            field("/material/Inline/model", "", "Pbr | Unlit", "other variant: \"BuiltinError\""),
            field("/material/Inline/alpha_mode", "", "Opaque | {\"Mask\": {\"cutoff\": 0..1}} | Blend", ""),
            field("/material/Inline/base_color", "linear RGBA", "0..1", ""),
            field("/material/Inline/emissive", RGB, ">= 0", "HDR; values above 1 glow"),
            field("/material/Inline/metallic", "", "0..1", ""),
            field("/material/Inline/roughness", "", "0..1", ""),
            field("/material/Inline/transmission", "", "0..1", "optional, default 0; light passing through, tinted by base_color (glass)"),
            field("/material/Inline/ior", "", "1..3", "optional, default 1.5; index of refraction"),
            field("/material/Inline/thickness", "m", ">= 0", "optional, default 0; how far the refracted ray travels"),
            field("/material/Inline/uv_scale", "", "[u, v]", "optional, default [1, 1]; texture repeats per face, [8, 4] tiles a long floor"),
            field("/material/Inline/uv_offset", "", "[u, v]", "optional, default [0, 0]; texture shift in whole-texture units"),
            field("/material/Inline/flat_shading", "", "", "optional, default false; light each triangle with its face normal for a faceted low-poly look"),
            field("/material/Inline/base_color_texture", "path relative to the scene file", "", "a plain string such as \"../assets/textures/crate.png\", not {\"$asset\": ...}; null for none; same for the other texture slots"),
            field("/material/Inline/normal_texture", "path relative to the scene file", "", ""),
            field("/material/Inline/metallic_roughness_texture", "path relative to the scene file", "", "glTF layout: roughness in G, metallic in B"),
            field("/material/Inline/occlusion_texture", "path relative to the scene file", "", ""),
            field("/material/Inline/emissive_texture", "path relative to the scene file", "", ""),
            field("/cast_shadows", "", "", ""),
            field("/receive_shadows", "", "", ""),
        ],
    },
    Section {
        key: "camera",
        summary: "A view. Among active cameras without a viewport the highest priority fills the window; on a tie the one spawned first wins. Every active camera with a viewport then draws into its part of the window, lower priority first, so split screen is two active cameras with viewports. Game code switches with `scene.set_active_camera(name)`, which makes that camera the only active one, or `scene.set_camera(name, active, viewport)`; `scene.set_camera_fov(name, radians)` and `scene.camera_fov(name)` zoom a perspective camera.",
        gpu: "one scene render per drawn camera; inactive cameras cost nothing",
        example: || json!({"projection": {"Perspective": {"vertical_fov_radians": 1.0, "near": 0.1, "far": 500.0}}, "active": true, "priority": 1, "viewport": [0.0, 0.0, 0.5, 1.0]}),
        fields: &[
            field("/projection/Perspective/vertical_fov_radians", RADIANS, "0 < fov < 3.14", "other variant: {\"Orthographic\": {\"vertical_size\" (m), \"near\", \"far\"}}"),
            field("/projection/Perspective/near", METRES, "> 0", ""),
            field("/projection/Perspective/far", METRES, "> near", ""),
            field("/active", "", "", ""),
            field("/priority", "", "i32", "higher wins among active cameras"),
            field("/viewport", "fractions", "[x, y, width, height], each 0..1", "optional; part of the window from the top-left corner, e.g. [0, 0, 0.5, 1] is the left half"),
        ],
    },
    Section {
        key: "physics_body",
        summary: "Where the body simulates. Needs a collider to collide.",
        gpu: "Gpu: compute per fixed tick; nothing is read back unless rusting.physics_sync asks for state",
        example: || json!({"simulation": "Gpu", "solver": "Simplified", "custom_shader": null}),
        fields: &[
            field("/simulation", "", "None | Static | Cpu | Gpu", "Cpu bodies are readable by gameplay every tick"),
            field("/solver", "", "Full | Simplified | NoCollision | Custom | Space", "GPU compute profile; Custom needs custom_shader"),
            field("/custom_shader", "project path", "", "GLSL defining `void solve(inout PhysicsState body)`; null unless solver is Custom. See `rusting docs show guide/gpu-condition-shaders`, which also covers game-code condition shaders"),
        ],
    },
    Section {
        key: "rigid_body",
        summary: "Mass and motion of a simulated body.",
        gpu: NO_GPU,
        example: || json!({"kind": "Dynamic", "mass": 5.0, "linear_velocity": [0.0, 2.0, 0.0], "angular_velocity": [0.0, 0.0, 0.0], "gravity_scale": 1.0}),
        fields: &[
            field("/kind", "", "Dynamic | Kinematic | Fixed", ""),
            field("/mass", "kg", "> 0", ""),
            field("/linear_velocity", "m/s", "", "initial velocity"),
            field("/angular_velocity", "rad/s", "", "initial spin"),
            field("/gravity_scale", "factor", "", "0 disables gravity"),
        ],
    },
    Section {
        key: "collider",
        summary: "Collision shape in the body's local space.",
        gpu: NO_GPU,
        example: || json!({"shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}}, "friction": 0.6, "restitution": 0.2, "sensor": false}),
        fields: &[
            field("/shape/Box/half_extents", METRES, "> 0", "other variants: {\"Sphere\": {\"radius\"}}, {\"Capsule\": {\"half_height\", \"radius\"}}, \"ConvexMesh\", \"TriangleMesh\" (static only), \"Heightfield\" (static only, reads rusting.heightfield), \"Polygon\" (convex hull of rusting.polygon), \"Chain\" (static only, segments of rusting.polygon)"),
            field("/friction", "", ">= 0", ""),
            field("/restitution", "", "0..1", "bounciness"),
            field("/sensor", "", "", "reports overlaps without pushing"),
        ],
    },
    Section {
        key: "collision_layers",
        summary: "Which bodies may collide: both must list each other's membership in their filters.",
        gpu: NO_GPU,
        example: || json!({"memberships": 2, "filters": 1}),
        fields: &[
            field("/memberships", "bitmask", "u32", ""),
            field("/filters", "bitmask", "u32", ""),
        ],
    },
    Section {
        key: "gpu_physics_watch",
        summary: "GPU conditions that raise named gameplay events for this body.",
        gpu: "evaluated in GPU physics; only matching events are read back",
        example: || json!({"rules": []}),
        fields: &[field("/rules", "", "", "see GpuPhysicsRule: event, condition, mode, payload, cooldown_seconds")],
    },
    Section {
        key: "directional_light",
        summary: "Sun-like light from the object's forward direction.",
        gpu: "one shadow pass when shadows is true",
        example: || json!({"color": [1.0, 0.95, 0.9], "illuminance": 50000.0, "shadows": true}),
        fields: &[
            field("/color", RGB, "0..1", ""),
            field("/illuminance", "lux, renderer scale", ">= 0", "100000 lights a white surface facing it as brightly as a point_light of 1000 up close, or ambient intensity 1, before exposure. Below about 10000 it barely shows over dark ambient. Daylight: 70000-100000; moonlight shafts through windows: 20000-40000"),
            field("/shadows", "", "", ""),
        ],
    },
    Section {
        key: "point_light",
        summary: "Light radiating from the object's position. `shadows` gives it six shadow map faces (a lamp in a room); one light per frame casts shadows, and a shadowed directional or spot light wins.",
        gpu: "shading cost per lit pixel; each pixel shades only the point and spot lights whose range reaches its screen cluster; up to 1024 lights per frame (512 at Balanced, 256 at Eco), the rest are dropped; six shadow views when shadows is true, each drawing every caster (guide/lighting)",
        example: || json!({"color": [1.0, 0.8, 0.6], "intensity": 800.0, "range": 8.0}),
        fields: &[
            field("/color", RGB, "0..1", ""),
            field("/intensity", "renderer units, not lumens", ">= 0", "1000 lights a white surface facing it, up close, as brightly as the 100000 lux sun; brightness falls off as (1 - distance/range)^2. A desk lamp in a dark room: 400-1500; a bright party light: 3000-8000"),
            field("/range", METRES, "> 0", "light reaches zero here"),
            field("/shadows", "", "", "default false"),
            field("/radius", METRES, ">= 0", "size of the glowing sphere; wider, softer highlights on shiny surfaces. Default 0, a point. A bulb: 0.03; a paper lantern: 0.2"),
        ],
    },
    Section {
        key: "spot_light",
        summary: "Cone of light along the object's forward direction. `shadows` gives it a shadow map (a flashlight stops at walls); one light per frame casts shadows, and a shadowed directional light wins. A rusting.light_cookie on the same object projects an image through it.",
        gpu: "as point_light; one shadow pass when shadows is true",
        example: || json!({"color": [1.0, 1.0, 1.0], "intensity": 1200.0, "range": 12.0, "inner_angle": 0.3, "outer_angle": 0.5}),
        fields: &[
            field("/color", RGB, "0..1", ""),
            field("/intensity", "renderer units, not lumens", ">= 0", "as point_light: 1000 matches the 100000 lux sun up close"),
            field("/range", METRES, "> 0", "light reaches zero here"),
            field("/inner_angle", RADIANS, "0..outer_angle", "fully lit cone"),
            field("/outer_angle", RADIANS, "inner_angle..1.57", "light reaches zero here"),
            field("/shadows", "", "", "default false"),
            field("/radius", METRES, ">= 0", "as point_light"),
        ],
    },
];

const COMPONENT_SECTIONS: &[ComponentSection] = &[
    ComponentSection {
        key: "rusting.ambient_light",
        summary: "Flat light added everywhere.",
        gpu: NO_GPU,
        example: || json!({"color": [0.8, 0.85, 1.0], "intensity": 0.2}),
    },
    ComponentSection {
        key: "rusting.sky_light",
        summary: "Hemisphere light: up-facing surfaces see sky_color, down-facing ones ground_color.",
        gpu: NO_GPU,
        example: || json!({"sky_color": [0.5, 0.7, 1.0], "ground_color": [0.3, 0.25, 0.2], "intensity": 0.4}),
    },
    ComponentSection {
        key: "rusting.tone_mapping",
        summary: "Exposure and the curve that maps HDR color to the display. The first one found is used.",
        gpu: NO_GPU,
        example: || json!({"mapper": "Aces", "exposure": 1.2}),
    },
    ComponentSection {
        key: "rusting.color_grading",
        summary: "Runs after tone mapping: contrast around mid grey, saturation (0 grey, 1 unchanged), shadows and highlights color tints, vignette (0..1) darkening the corners, and film/CRT effects (0..1, off at 0): grain, chromatic_aberration, scanlines, color_bleed, noise_band, distortion. Grain and the noise band follow the fixed tick. The first one found is used; one on an object with rusting.post_volume applies only in that area.",
        gpu: "a few instructions per pixel; chromatic_aberration, color_bleed or distortion add one full-screen copy",
        example: || json!({"contrast": 1.1, "saturation": 0.9, "shadows": [0.92, 0.98, 1.1], "highlights": [1.08, 1.0, 0.9], "vignette": 0.3, "grain": 0.1, "chromatic_aberration": 0.2, "scanlines": 0.0, "color_bleed": 0.0, "noise_band": 0.0, "distortion": 0.0}),
    },
    ComponentSection {
        key: "rusting.camera_screen",
        summary: "On an object with a mesh: shows what the camera entity named camera sees, at size [w, h] pixels, in place of the material's base color and emissive maps. The camera may be inactive. Give each screen its own material. update_every draws the feed every N frames; enabled false, or a screen out of view, keeps the last image. grading is the feed's own color grading (e.g. scanlines and grain). exposure multiplies the scene's exposure for the feed only (4 makes a dark room readable on a monitor).",
        gpu: "renders the scene once more per screen each time its feed draws",
        example: || json!({"camera": "Cam B", "size": [320, 180], "update_every": 2, "enabled": true, "exposure": 2.0, "grading": {"contrast": 1.2, "saturation": 0.6, "shadows": [1.0, 1.0, 1.0], "highlights": [1.0, 1.0, 1.0], "vignette": 0.4, "grain": 0.3, "chromatic_aberration": 0.4, "scanlines": 0.6, "color_bleed": 0.3, "noise_band": 0.5, "distortion": 0.3}}),
    },
    ComponentSection {
        key: "rusting.environment_map",
        summary: "Equirectangular (2:1) sky image under assets/ that surfaces reflect and are lit by, replacing the sky_light hemisphere. Rough surfaces see it blurred. The first one found is used.",
        gpu: NO_GPU,
        example: || json!({"texture": "sky.png", "intensity": 1.0}),
    },
    ComponentSection {
        key: "rusting.color_lut",
        summary: "Color lookup table under assets/, applied after tone mapping and rusting.color_grading: an image strip of N squares of N x N pixels side by side (256 x 16 or 1024 x 32). Red runs across each square, green down it, blue from square to square; an identity strip changes nothing, so paste one into a screenshot, grade it in any image editor and save the strip. intensity 0..1 blends it in. The first one found is used.",
        gpu: "one texture sample per pixel in tone mapping",
        example: || json!({"texture": "grades/teal_orange.png", "intensity": 1.0}),
    },
    ComponentSection {
        key: "rusting.light_cookie",
        summary: "On an object with a spot_light: projects an image under assets/ through the light's outer cone, like a slide in a projector (window frames, leaves, a logo). The light's color is multiplied by the image; its top points along the light's up (+Y). At most four cookie spot lights per frame, the first in entity order; others light without their image.",
        gpu: "one texture sample per lit pixel for each cookie light that reaches it",
        example: || json!({"texture": "window_cookie.png"}),
    },
    ComponentSection {
        key: "rusting.reflection_probe",
        summary: "Box of half size extents around the object's position. Surfaces inside reflect the scene as seen from that position, projected onto the box walls, in place of the environment map. Captured when added or changed; up to four are used.",
        gpu: "each probe renders the scene six times when captured",
        example: || json!({"extents": [5.0, 3.0, 5.0], "intensity": 1.0}),
    },
    ComponentSection {
        key: "rusting.fog",
        summary: "Exponential height fog: the scene fades into color with distance, thinning above height by height_falloff per metre. Brighter toward the sun by sun_scatter; sky_affect fades the background too. The first one found is used; one on an object with rusting.post_volume applies only in that area.",
        gpu: "a few instructions per pixel, plus one full-screen sky pass when sky_affect > 0",
        example: || json!({"color": [0.55, 0.65, 0.75], "density": 0.02, "height": 0.0, "height_falloff": 0.1, "sun_scatter": 0.3, "sky_affect": 1.0}),
    },
    ComponentSection {
        key: "rusting.post_volume",
        summary: "Makes the rusting.fog and rusting.color_grading on the same object apply only near it: fully while the active camera is inside the box of half size extents (world axes, metres), fading out over blend metres outside. Higher priority wins where volumes overlap. Fog and grading without a volume apply everywhere else; a hall with fog and a sickly grade and an office with a warm grade each get one.",
        gpu: NO_GPU,
        example: || json!({"extents": [4.0, 2.0, 10.0], "blend": 1.5, "priority": 0}),
    },
    ComponentSection {
        key: "rusting.bloom",
        summary: "Glow around pixels brighter than threshold (linear, before exposure), spread over the screen before tone mapping. The first one found is used.",
        gpu: "a half-resolution blur chain of about 12 compute passes",
        example: || json!({"intensity": 0.5, "threshold": 1.0, "spread": 0.7}),
    },
    ComponentSection {
        key: "rusting.auto_exposure",
        summary: "Eye adaptation: every frame the renderer measures the average brightness of the lit image and moves exposure toward key / average, clamped to min_exposure..max_exposure, at speed per second (the first frame adapts at once). It multiplies the rusting.tone_mapping exposure. The first one found is used.",
        gpu: "one single-workgroup compute pass reading 4096 samples",
        example: || json!({"key": 0.18, "min_exposure": 0.25, "max_exposure": 4.0, "speed": 1.5}),
    },
    ComponentSection {
        key: "rusting.depth_of_field",
        summary: "Lens blur: objects nearer or farther than focus_distance metres blur, up to blur (a fraction of the screen height) far behind the focus or at half its distance. Reads the opaque depth, so transparent objects blur with what is behind them. The first one found is used.",
        gpu: "a copy of the HDR image and depth, and 32 taps per pixel in tone mapping",
        example: || json!({"focus_distance": 10.0, "blur": 0.01}),
    },
    ComponentSection {
        key: "rusting.motion_blur",
        summary: "Camera motion blur: each pixel smears along its screen motion since the last frame, scaled by intensity (0.5 is a 180-degree shutter). Only camera movement blurs; a jump of more than a sixth of the screen counts as a cut. The first one found is used.",
        gpu: "a copy of the HDR image and depth, and 16 taps per moving pixel in tone mapping",
        example: || json!({"intensity": 0.5}),
    },
    ComponentSection {
        key: "rusting.ambient_occlusion",
        summary: "Screen-space ambient occlusion: darkens ambient, sky and environment light in creases within radius metres. Off on the Eco quality profile. The first one found is used.",
        gpu: "a depth prepass plus two full-screen compute passes",
        example: || json!({"radius": 1.0, "intensity": 1.0}),
    },
    ComponentSection {
        key: "rusting.background",
        summary: "Clear color behind the scene. The one on the entity with the lowest ID is used; without one the game keeps its render settings.",
        gpu: NO_GPU,
        example: || json!({"color": [0.5, 0.7, 0.9, 1.0]}),
    },
    ComponentSection {
        key: "rusting.render_bounds",
        summary: "Render-only visibility bounds in local space, separate from the collider.",
        gpu: "smaller bounds let culling skip more draws",
        example: || json!({"Sphere": {"center": [0.0, 0.5, 0.0], "radius": 1.0}}),
    },
    ComponentSection {
        key: "rusting.physics_sync",
        summary: "What a GPU body sends back to the CPU.",
        gpu: "SelectedState and FullState read back 144 bytes per body per fixed tick; Events reads back only fired events",
        example: || json!("SelectedState"),
    },
    ComponentSection {
        key: "rusting.auto_simulation",
        summary: "Lets the engine choose CPU or GPU simulation for the body.",
        gpu: NO_GPU,
        example: || json!({}),
    },
    ComponentSection {
        key: "rusting.player_controller",
        summary: "First- or third-person walking body. Reads the player.* actions; parent a camera to it at eye height. camera_distance above 0 orbits that camera behind the body. Ground steeper than max_slope is a wall; ledges up to max_step_height are stepped onto; push_bodies false keeps it from moving dynamic bodies. turn_speed (rad/s) turns its non-camera children toward the walking direction. pitch_limits and yaw_limits ([low, high] radians; yaw_limits null turns freely) bound the view, for a seated or turret camera. crouch_height above 0 lets player.crouch (C, left Ctrl, pad East) shrink the body to that height from the top, at crouch_multiplier speed; it stands again only where there is room. air_jumps allows that many extra jumps before landing (1 is a double jump). Game code's scene.dash(name, velocity, seconds) moves it at that velocity with no gravity for that long. swim_speed above 0 makes it swim in rusting.water: below the surface it moves at swim_speed, floats with its center float_depth under the waves, rises while player.jump is held, dives while player.crouch is held, drifts with the current, and jumps out from the surface. It rides moving and turning platforms, turning with them.",
        gpu: NO_GPU,
        example: || json!({"walk_speed": 5.0, "sprint_multiplier": 1.5, "jump_speed": 6.0, "gravity": 12.0, "look_sensitivity": 0.003, "mouse_look": true, "collision_mask": 1, "yaw": 1.57, "pitch": 0.0, "pitch_limits": [-1.55, 1.55], "yaw_limits": null, "camera_distance": 4.0, "camera_height": 0.6, "camera_offset": [0.0, 0.0, 0.0], "max_slope": 0.78, "max_step_height": 0.3, "push_bodies": true, "turn_speed": 0.0, "crouch_height": 1.0, "crouch_multiplier": 0.5, "air_jumps": 0, "swim_speed": 3.0, "float_depth": 0.5}),
    },
    ComponentSection {
        key: "rusting.tween",
        summary: "Animates one Transform property from `from` to `to` on the fixed step.",
        gpu: NO_GPU,
        example: || json!({"property": "Scale", "from": [1.0, 1.0, 1.0], "to": [1.2, 1.2, 1.2], "duration": 0.4, "delay": 0.0, "easing": "BackOut", "repeat": "PingPong"}),
    },
    ComponentSection {
        key: "rusting.camera_shake",
        summary: "Trauma shake on a camera. Game code calls add_trauma(name, amount) on a hit or explosion; the shake strength is trauma squared, trauma falls by decay per second, and only the drawn view moves (the camera's Transform keeps its pose).",
        gpu: NO_GPU,
        example: || json!({"trauma": 0.0, "decay": 1.5, "max_offset": [0.3, 0.3, 0.0], "max_roll": 0.05, "frequency": 12.0}),
    },
    ComponentSection {
        key: "rusting.spawn_grid",
        summary: "Bulk spawn. When the game starts or a scene loads, the object is copied with its children onto a grid of count cells (X, Y, Z, original included) spacing meters apart along positive local axes; copies are named \"<name>#<n>\" from 1. A 40 by 25 by 40 grid makes a 40,000-ball pit from one authored ball. Runs in a running game only, not in the editor, and the component is removed once used.",
        gpu: NO_GPU,
        example: || json!({"count": [10, 1, 10], "spacing": [0.5, 0.5, 0.5]}),
    },
    ComponentSection {
        key: "rusting.squash",
        summary: "Squash and stretch spring. Game code calls squash(name, amount) on a landing or hit (positive flattens, negative stretches, -0.8 to 0.8); the object's Transform scale wobbles back to rest keeping its volume. Colliders scale too, so put it on a visible child, not the physics body.",
        gpu: NO_GPU,
        example: || json!({"stiffness": 400.0, "damping": 14.0}),
    },
    ComponentSection {
        key: "rusting.flash",
        summary: "Hit flash. Game code calls flash(name) on a hit or pickup; the object and its children draw tinted toward color (linear RGB, also used as glow), fading back over duration seconds. Only the drawn colors change, not the shared material, so other objects using it stay as they are.",
        gpu: NO_GPU,
        example: || json!({"color": [1.0, 1.0, 1.0], "duration": 0.12}),
    },
    ComponentSection {
        key: "rusting.sound_cue",
        summary: "Sends a SoundEvent when the body starts touching something or game code calls trigger(). The windowed game plays the clip (path relative to assets/, not the scene-relative asset `reference`); headless runs and scenarios count it (scenario entity `audio:`). A clip `sfx:<preset> [seed]` (jump, coin, hit, explosion, laser, powerup, blip; seed default 1) plays a built-in synthesized sound with no file. A non-empty `with_material` plays only on touches of a collider whose physics_material has that name. A non-empty `caption` such as `[glass breaks]` shows for 2 s each time it fires.",
        gpu: NO_GPU,
        example: || json!({"clip": "sounds/hit.ogg", "volume": 0.8, "on_collision": true, "caption": "[thud]", "full_volume_speed": 6.0, "with_material": "metal", "on_gpu_event": "hit"}),
    },
    ComponentSection {
        key: "rusting.slide_sound",
        summary: "Loops clip on this body while it slides or rolls against another collider, louder the faster it moves across the contact (full volume at full_volume_speed m/s, silent under min_speed). CPU bodies only. The loop follows the body like an attached sound.",
        gpu: NO_GPU,
        example: || json!({"clip": "sounds/scrape.ogg", "volume": 0.8, "full_volume_speed": 4.0, "min_speed": 0.2, "bus": "world"}),
    },
    ComponentSection {
        key: "rusting.reverb_zone",
        summary: "Room reverb on a bus while the listener (the active camera, or set_listener) is inside this entity's collider; give it a sensor collider. Leaving fades the reverb back out over fade seconds. Overlapping zones: the first in entity order wins. A zone owns the reverb on its bus, so do not also set_bus_effect a reverb there.",
        gpu: NO_GPU,
        example: || json!({"bus": "world", "room": 0.8, "damping": 0.5, "mix": 0.4, "fade": 0.5}),
    },
    ComponentSection {
        key: "rusting.burst_emitter",
        summary: "Spawns particles that fly out, fall, and shrink, when the body starts touching something or game code calls trigger(). rate above 0 emits that many particles per second with no trigger, starting anywhere in the area box (half extents); stretch makes particles taller, for rain. Particles copy the emitter's mesh.",
        gpu: NO_GPU,
        example: || json!({"count": 20, "speed": 4.0, "lifetime": 0.8, "particle_scale": 0.1, "gravity": 9.81, "on_collision": false, "rate": 0.0, "area": [0.0, 0.0, 0.0], "stretch": 1.0}),
    },
    ComponentSection {
        key: "rusting.particle_emitter",
        summary: "Particle effects: rate and bursts per cycle, an emission shape (Point, Box, Sphere, Cone, Circle), random [min, max] ranges for lifetime, speed, size, rotation and spin, gravity, drag, wind and turbulence, size and color keys over life, fades, World or Local space, Billboard or Velocity facing, Alpha or Additive blend. Particles are not entities; each emitter is one instanced draw. Random values come from the scene seed, the emitter's SceneId and the fixed tick. Game code calls play(), pause(), stop() or restart(). `rusting effect list` shows ready presets.",
        gpu: "one instanced draw of up to max_particles quads per emitter, rebuilt every frame",
        example: || json!({"autoplay": true, "rate": 20.0, "bursts": [], "max_particles": 500, "prewarm": false, "duration": 5.0, "looping": true, "on_collision": false, "shape": "Sphere", "shape_size": [0.5, 0.5, 0.5], "lifetime": [1.0, 2.0], "speed": [0.5, 1.5], "size": [0.05, 0.1], "rotation": [0.0, 0.0], "spin": [0.0, 0.0], "direction": [0.0, 1.0, 0.0], "spread": 0.5, "gravity": 0.0, "drag": 0.5, "wind": [0.0, 0.0, 0.0], "turbulence": 0.5, "turbulence_frequency": 1.0, "size_over_life": [{"t": 0.0, "value": 1.0}, {"t": 1.0, "value": 0.2}], "color_over_life": [{"t": 0.0, "color": [1.0, 0.8, 0.3, 1.0]}, {"t": 1.0, "color": [1.0, 0.2, 0.1, 1.0]}], "start_colors": [], "emissive": 3.0, "fade_in": 0.1, "fade_out": 0.4, "space": "World", "facing": "Billboard", "stretch": 0.1, "blend": "Additive", "sprite": "Soft"}),
    },
    ComponentSection {
        key: "rusting.animation",
        summary: "Keyframe animation: named clips of tracks keyed over time. A track writes Position, Rotation (radians), Scale, Color (RGBA), Emissive (RGB), Visible, Orientation (quaternion x,y,z,w, used by imported glTF clips), or a numeric Field of another registered component, on this entity or a child path such as Arm/Hand. Interpolation is Step, Linear or Smooth (Catmull-Rom); repeat is Once, Loop or PingPong; speed scales time. Markers in events reach game code as animation events. A clip with `blend` points is a 1D blend space: its clips are stretched to its length (1 s when 0) and the two around the `blend_parameter` value are mixed. With `blend_parameter_y` set it is a 2D blend space: each point also has `at_y`, and every point is weighted by gradient bands (a point plays alone on its spot). Transitions make a state machine: when the playing clip is `from` (empty: any) and the parameter test holds (Above, Below or Equal; at_end also waits for the clip to finish), it crossfades to `to`; the first match in list order wins. Layers play clips on top of the state on their own clocks: an override layer replaces, an additive one adds the clip's motion from its first frame, `weight` (or the `weight_parameter` value) scales it, and `mask` limits it to target paths and their children. `root_motion` (Off, InPlace, Transform, Velocity) keeps the `root_bone` Position track at its first-frame x and z and collects its horizontal motion: InPlace for game code (take_root_motion), Transform moves the object, Velocity sets its GPU body velocity through the physics command bridge. `humanoid` maps standard bone names (Hips, Spine, LeftUpperArm...) to this rig's bone paths for `rusting scene retarget`; empty matches bones by name without rig prefixes. Runs on the fixed tick, so playback is deterministic. Game code calls play_animation(), crossfade(), stop_animation(), is_playing() and set_animation_parameter().",
        gpu: NO_GPU,
        example: || json!({"clips": [{"name": "bob", "duration": 0.0, "repeat": "PingPong", "tracks": [{"target": "", "property": "Position", "interpolation": "Smooth", "keys": [{"time": 0.0, "value": [0.0, 0.0, 0.0]}, {"time": 1.0, "value": [0.0, 0.5, 0.0]}]}], "events": [{"time": 1.0, "name": "top"}], "blend": [], "blend_parameter": "", "blend_parameter_y": ""}], "autoplay": "bob", "speed": 1.0, "parameters": {}, "transitions": [{"from": "bob", "to": "run", "parameter": "speed", "compare": "Above", "value": 1.0, "at_end": false, "fade": 0.2}], "layers": [{"clip": "wave", "weight": 1.0, "weight_parameter": "", "additive": false, "mask": ["Body/Arm"]}], "root_motion": "Off", "root_bone": "", "humanoid": [{"bone": "Hips", "path": "Armature/Hips"}]}),
    },
    ComponentSection {
        key: "rusting.skin",
        summary: "Skinned mesh: joint paths from this object (child names joined by /, .. for the parent) and one column-major inverse bind matrix per joint. Vertex joints and weights come from the .rskin file the glTF importer cooks next to the mesh. glTF import and `scene add-model` fill it in; animate the joints with rusting.animation. The mesh is bent on the CPU each frame a joint moves.",
        gpu: "the bent mesh is uploaded again each frame a joint moves",
        example: || json!({"joints": ["../Armature/Root", "../Armature/Root/Tip"], "inverse_bind": [[[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, 0.0, 0.0, 1.0]], [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0], [0.0, -1.0, 0.0, 1.0]]]}),
    },
    ComponentSection {
        key: "rusting.ik",
        summary: "Inverse kinematics on the end of a joint chain, solved each fixed step after the animation pose. kind LookAt turns this object so its local forward axis points at target. kind TwoBone bends this object's parent and grandparent (upper arm and forearm, thigh and shin) so this object's origin reaches target; the middle joint bends toward pole if set. kind Foot is TwoBone toward the ground under this object (no target): a ray from reach metres above the character's floor (nearest ancestor with rusting.animation) finds the CPU collider below, and the foot keeps its animated height above it, turns to the slope, and lowers the hips (the thigh's parent) when the ground is out of reach. weight 0 keeps the animated pose, 1 solves fully. A null target turns off LookAt and TwoBone. kind Chain bends the `joints` ancestors above this object (FABRIK, for tails, spines and tentacles) so it reaches target. Each tick starts from the clip pose, never last tick's IK.",
        gpu: NO_GPU,
        example: || json!({"kind": "TwoBone", "target": null, "pole": null, "weight": 1.0, "forward": [0.0, 0.0, -1.0], "reach": 0.5, "joints": 3}),
    },
    ComponentSection {
        key: "rusting.rope",
        summary: "A rope of `segments` (1 to 256) small CPU sphere bodies (radius m, mass kg each) from this object's body at anchor (local m) to target's body at target_anchor (target null: a world point). Adjacent beads are joined by distance joints, so the rope goes slack but never stretches past length metres, and pulls on both ends. The beads spawn on the straight line between the ends the first fixed step of play; game code can read their transforms from RopeState to draw the rope. This object's body gets the joint to the last bead, so it cannot carry a rusting.joint of its own.",
        gpu: NO_GPU,
        example: || json!({"target": null, "anchor": [0.0, 0.5, 0.0], "target_anchor": [0.0, 5.0, 0.0], "length": 4.0, "segments": 8, "radius": 0.0625, "mass": 0.125}),
    },
    ComponentSection {
        key: "rusting.ragdoll",
        summary: "Hands a character's bones from animation to CPU physics and back. Going limp (game code `set_ragdoll(name, true)`, or a CPU body closing on the character's colliders at hit_speed m/s or faster; 0 only on command) spawns a capsule body per entry of bones (path from this object, length and radius in metres along the bone's local +Y, mass in kg) moving as the animation moved it, jointed to the nearest ancestor bone's body with `joint` (a rusting joint kind; X axis along the bone by default, set by frame). The character's own collider is taken off while limp. After recover_after seconds (0 waits for `set_ragdoll(name, false)`) the character moves under its hips, the bodies are removed, and bones blend back to the animation over blend_time seconds. Bones without an entry keep their animated local pose. muscle (Hz, 0 = passive) makes it an active ragdoll: the bodies exist from the start, each is turned toward its bone's animated pose relative to its parent body by a critically damped spring at that frequency, and the top body is held to the animated hips; hits push it and it springs back, a hit at hit_speed drops the muscles until recovery, after which they regain strength over blend_time. Set muscle to 0 to go limp and return to plain animation.",
        gpu: NO_GPU,
        example: || json!({"bones": [{"path": "Armature/Hips", "length": 0.25, "radius": 0.12, "mass": 10.0, "joint": {"ConeTwist": {"swing": 0.7, "twist": [-0.4, 0.4]}}, "frame": [0.0, 0.0, 1.5707964]}, {"path": "Armature/Hips/Spine", "length": 0.4, "radius": 0.12, "mass": 12.0, "joint": {"ConeTwist": {"swing": 0.5, "twist": [-0.3, 0.3]}}, "frame": [0.0, 0.0, 1.5707964]}], "hit_speed": 6.0, "recover_after": 2.0, "blend_time": 0.5, "muscle": 0.0}),
    },
    ComponentSection {
        key: "rusting.morph",
        summary: "Blend shape (morph target) weights for the object's mesh, one per shape, usually 0 to 1. The shape offsets come from the .rmorph file the glTF importer cooks next to the mesh. glTF import and `scene add-model` fill it in, and imported clips animate it with a Field track on /weights. Extra mesh parts of the object (its children without their own rusting.morph) use these weights too. The mesh is reshaped on the CPU each frame a weight changes.",
        gpu: "the reshaped mesh is uploaded again each frame a weight changes",
        example: || json!({"weights": [0.0, 1.0]}),
    },
    ComponentSection {
        key: "rusting.fluid_block",
        summary: "Particle fluid: a block of count_x by count_y by count_z particles resting on the floor of a box centered on the entity. It runs every fixed tick, floats and sinks dynamic bodies with sphere colliders, and draws a smooth water surface when visible (show_particles adds a sphere per particle). solver Particles (position based) suits small splashy volumes of a few thousand particles; Grid runs FLIP on a grid over the box and carries large water volumes (softer, slowly loses volume); Granular makes grains with friction (sand, gravel, rubble) that pile into heaps, steeper as friction grows. CPU only, deterministic.",
        gpu: NO_GPU,
        example: || json!({"spacing": 0.1, "count_x": 6, "count_y": 6, "count_z": 6, "container_half_extents": [0.5, 0.5, 0.5], "iterations": 4, "viscosity": 0.01, "solver": "Particles", "friction": 0.6, "visible": true, "show_particles": false}),
    },
    ComponentSection {
        key: "rusting.soft_block",
        summary: "XPBD soft body: a block of count_x by count_y by count_z cubes of spacing metres centered on the entity, each cut into six tetrahedra that keep their edge lengths and volumes. softness (edge compliance) 0 is rigid and 1e-3 a wobbly jelly; squash lets the volume change; tear_strain above 0 tears edges stretched past rest times 1 + tear_strain. It falls under gravity every fixed tick, keeps out of sphere, box and capsule colliders and pushes dynamic bodies back. visible draws its outer faces (and the cuts once torn). CPU only, deterministic; at most 4096 particles.",
        gpu: NO_GPU,
        example: || json!({"spacing": 0.1, "count_x": 4, "count_y": 4, "count_z": 4, "density": 1000.0, "softness": 0.0001, "squash": 0.0, "damping": 1.0, "tear_strain": 0.0, "substeps": 10, "visible": true}),
    },
    ComponentSection {
        key: "rusting.cloth",
        summary: "XPBD cloth: a sheet of count_x by count_y squares of spacing metres in the entity's XY plane, centered on it, each cut into two triangles. stretch (edge compliance) 0 does not stretch; bend sets how easily it folds. wind blows on it with drag; self_collision keeps folds thickness apart. pin_top holds the top edge on holder (a body that carries it and feels its pull) or in place when holder is null. tear_strain above 0 tears edges stretched past rest times 1 + tear_strain. It drapes over sphere, box and capsule colliders and pushes dynamic bodies back. visible draws both sides. CPU only, deterministic; at most 4096 particles.",
        gpu: NO_GPU,
        // Numbers exact in f32: components with entity fields save through
        // JSON values, which widen 0.1 to 0.10000000149011612.
        example: || json!({"spacing": 0.125, "count_x": 8, "count_y": 8, "density": 0.25, "stretch": 0.0, "bend": 0.0, "damping": 0.5, "wind": [0.0, 0.0, 2.0], "drag": 1.0, "thickness": 0.03125, "self_collision": true, "tear_strain": 0.0, "substeps": 10, "pin_top": true, "holder": null, "visible": true}),
    },
    ComponentSection {
        key: "rusting.gravity_volume",
        summary: "Replaces the scene gravity for dynamic CPU bodies overlapping this object's sensor collider (it needs a CPU physics body, usually Fixed, with sensor true): gravity [0, 0, 0] is a zero-g room, a sideways vector a wind tunnel, and toward_center above 0 pulls toward the object's centre at that many m/s² for a small planet (gravity is then ignored). Where volumes overlap the highest priority wins. gravity_scale still applies. GPU bodies and the player controller are not affected.",
        gpu: NO_GPU,
        example: || json!({"gravity": [0.0, 0.0, 0.0], "toward_center": 0.0, "priority": 0}),
    },
    ComponentSection {
        key: "rusting.mesh_surfaces",
        summary: "Per-triangle surfaces for a collider with shape \"TriangleMesh\" on the same object. triangles[i] picks an entry of surfaces for the mesh's triangle i (in index buffer order), a patch such as ice or mud with its own friction, restitution and material name (sound_cue with_material matches it); missing entries and indices past the end use the collider's own values and physics_material. A glTF file's primitives already import as separate meshes, each with its own collider; use this when one mesh needs several surfaces.",
        gpu: NO_GPU,
        example: || json!({"triangles": [0, 0, 255, 255], "surfaces": [{"name": "ice", "friction": 0.02, "restitution": 0.0}]}),
    },
    ComponentSection {
        key: "rusting.polygon",
        summary: "A 2D outline for a collider with shape \"Polygon\" or \"Chain\" on the same object. points are in the object's XY plane in metres, extruded depth metres along Z, centred on the object and scaled by its transform. Polygon uses the convex hull of the points (dynamic bodies may use it; list the points counterclockwise). Chain makes each segment a thin static wall, closed joins the last point back to the first. Pair dynamic bodies with rusting.axis_lock for 2D. Game code can draw the same shape with Polygon2d::mesh().",
        gpu: NO_GPU,
        example: || json!({"points": [[-1.0, 0.0], [1.0, 0.0], [0.0, 1.0]], "depth": 1.0, "closed": true}),
    },
    ComponentSection {
        key: "rusting.heightfield",
        summary: "A grid of ground heights for a collider with shape \"Heightfield\" on the same object (static only, like TriangleMesh). heights[row][column] in metres; rows run along +Z and columns along +X, spacing metres apart, centred on the object and scaled by its transform. Game code can draw the same surface with Heightfield::mesh(). cells[row][column] (one row and column fewer than heights) picks an entry of surfaces for that cell, a patch such as ice or mud with its own friction, restitution and material name (sound_cue with_material matches it); missing cells and indices past the end use the collider's own values and physics_material.",
        gpu: NO_GPU,
        example: || json!({"heights": [[0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.5, 0.0]], "spacing": 2.0, "cells": [[0, 255], [255, 255]], "surfaces": [{"name": "ice", "friction": 0.02, "restitution": 0.0}]}),
    },
    ComponentSection {
        key: "rusting.axis_lock",
        summary: "Freezes a dynamic CPU body's movement along world axes (linear) and its turning around them (angular), like Godot's axis locks. A 2D game in the XY plane locks linear z and angular x and y: Sphere bodies act as circles, Box as rectangles and Capsule as 2D capsules, as long as everything they touch shares the plane. Contacts, joints, gravity and force fields never move a locked axis.",
        gpu: NO_GPU,
        example: || json!({"linear": [false, false, true], "angular": [true, true, false]}),
    },
    ComponentSection {
        key: "rusting.force_field",
        summary: "Pushes dynamic CPU bodies overlapping this object's sensor collider (it needs a CPU physics body, usually Fixed, with sensor true) with an acceleration of strength m/s², the same for any mass: Directional along direction, Radial out from the object's centre (negative strength pulls in), Vortex around the direction axis through the centre, Wind along direction in gusts of turbulence times strength, Custom with the Rust function the game registered under the name in function (App::add_force_field_function; CPU bodies only), scaled by strength. falloff_distance above 0 fades the push to nothing that far from the centre. Fields add up where they overlap and add to gravity. GPU bodies whose centre comes within their collider's bounding radius of the sensor are pushed the same way; the player controller is not affected.",
        gpu: NO_GPU,
        example: || json!({"kind": "Wind", "strength": 6.0, "direction": [1.0, 0.0, 0.0], "falloff_distance": 0.0, "turbulence": 0.5, "function": ""}),
    },
    ComponentSection {
        key: "rusting.physics_material",
        summary: "Names the surface of this object's CPU collider and sets how its collider friction and restitution combine with the other side's in a contact: Default (friction geometric mean, restitution the larger), Average, Min, Multiply or Max. Where the two sides' modes differ, the later one in that list wins, so Min on ice keeps it slippery against anything without Multiply or Max. A sound_cue with with_material set to this name plays only on touches of this surface. GPU bodies ignore it.",
        gpu: NO_GPU,
        example: || json!({"name": "ice", "friction_combine": "Min", "restitution_combine": "Default"}),
    },
    ComponentSection {
        key: "rusting.vehicle",
        summary: "A raycast car on this object's dynamic CPU rigid body (box collider, mass around 1200). Each wheel casts a ray down the body's up axis from its local position (forward is -Z); a hit within suspension + radius pushes the body up with a spring (stiffness, damping), and the tire grips along the ground by a slip curve up to grip times the load. Driven wheels share engine_torque times the gear ratio and final_drive equally (open differential); the gearbox shifts by rpm. Set throttle (-1..1, below 0 reverses), brake (0..1) and steer (-1..1, above 0 turns right) from game code, or player_input true to drive with the player forward/back/left/right actions and jump to brake. Keep the collider box clear of the ground at rest. gear, rpm and each wheel's spin, compression and grounded are state.",
        gpu: NO_GPU,
        example: || {
            // Through text, so f32 fields stay short like the other examples.
            let text = serde_json::to_string(&crate::runtime::Vehicle {
                player_input: true,
                ..crate::runtime::Vehicle::default()
            })
            .expect("a vehicle serializes");
            serde_json::from_str(&text).expect("it parses back")
        },
    },
    ComponentSection {
        key: "rusting.water",
        summary: "Water for seas, lakes and rivers: a size by size rectangle of animated waves centered on the entity (axis aligned, rotation and scale ignored). Dynamic bodies with sphere, box or capsule colliders float in it, and flow_speed carries them along flow_direction, which also turns the waves. Deterministic and cheap; a long thin rectangle with a flow_speed is a river. flat_shading gives faceted low-poly waves. A player_controller with swim_speed swims in it. Put it on an empty object.",
        gpu: "one mesh of about resolution squared vertices, rewritten every fixed tick",
        example: || json!({"size": [20.0, 20.0], "resolution": 64, "wave_height": 0.25, "wave_length": 4.0, "wave_speed": 1.0, "flow_direction": 0.0, "flow_speed": 0.0, "color": [0.1, 0.4, 0.7, 0.7], "flat_shading": false}),
    },
    ComponentSection {
        key: "rusting.hud",
        summary: "Text label or button drawn over the game view. offset points inward from the anchor, in logical pixels, and {tr:key} shows the current locale's text for key (scene.set_locale reads assets/locales/<locale>.json) and may itself hold {counter} placeholders; the text is measured after {counter} values are filled in, so right and bottom anchors keep their margin. {state:Name} shows that object's rusting.state, translated when the locale has it as a key, and {dialogue:Name} and {dialogue:Name/n} show a rusting.dialogue's line and choices. A button whose whole text is {binding:action} shows that rusting.input_action's inputs and, when clicked, rebinds the action to the next key pressed, keeping its inputs on the other device (keyboard and mouse, or gamepad) (Escape cancels; Press a key, or the binding.waiting translation, shows meanwhile). An element whose text fills to nothing is not drawn. A clicked button sends HudButtonPressed. follow names an object: the element then sits on that object's place on screen plus follow_offset (world metres, [0, 2, 0] is above a head), aligned by anchor (Bottom puts the text's bottom edge there) with offset in pixels, +y down, and hides while the object is behind the camera: name tags, health bars, waypoint markers.",
        gpu: "a few egui triangles",
        example: || json!({"text": "Score: 0", "anchor": "TopRight", "offset": [24.0, 24.0], "font_size": 24.0, "color": [1.0, 0.9, 0.4, 1.0], "button": false, "requires": null, "camera": null, "follow": null, "follow_offset": [0.0, 0.0, 0.0]}),
    },
    ComponentSection {
        key: "rusting.input_action",
        summary: "Binds a named action to keys and mouse buttons, for game code (GameScene::pressed, held) and scenario press steps. Bindings add to the ActionMap; the player.* actions are already bound.",
        gpu: NO_GPU,
        example: || json!({"action": "fire", "inputs": ["MouseLeft", "KeyF"]}),
    },
    ComponentSection {
        key: "rusting.counter",
        summary: "Named integer. HUD text shows it as {name}; pickups add to it.",
        gpu: NO_GPU,
        example: || json!({"name": "coins", "value": 0, "target": 5}),
    },
    ComponentSection {
        key: "rusting.health",
        summary: "Hit points. Game code's scene.damage(name, amount) lowers value (a negative amount heals up to max) and returns what is left; objects with the same non-empty team are allies (scene.same_team).",
        gpu: NO_GPU,
        example: || json!({"value": 3, "max": 3, "team": "enemies"}),
    },
    ComponentSection {
        key: "rusting.dialogue",
        summary: "A branching conversation on a named object, such as a shopkeeper or quest giver. Game code's scene.start_dialogue(name) shows the first line, scene.dialogue_line(name) gives the shown line with speaker, text and choice texts translated by the locale (keys the locale lacks are shown as they are), and scene.advance_dialogue(name, choice) moves to the line's next, or to the picked choice's next after adding add to its counter (empty for none); an empty or unknown next ends it. current is the shown line's id, empty when not running, so snapshots, saves and scenarios see where the talk is. With no game code, a rusting.hud text {dialogue:Name} shows the line as speaker: text, and a button whose whole text is {dialogue:Name/n} shows choice n (from 1; Continue, or the dialogue.continue translation, for /1 on a line with no choices) and picks it when clicked; elements whose text fills to nothing are not drawn, so unused choice buttons hide.",
        gpu: NO_GPU,
        example: || json!({"current": "", "lines": [
            {"id": "hello", "speaker": "npc.smith", "text": "smith.hello", "next": "", "choices": [
                {"text": "smith.buy", "next": "buy", "counter": "", "add": 0},
                {"text": "smith.quest", "next": "", "counter": "quest.smith", "add": 1}]},
            {"id": "buy", "speaker": "npc.smith", "text": "smith.buy_reply", "next": "", "choices": []}]}),
    },
    ComponentSection {
        key: "rusting.loot_table",
        summary: "A weighted loot table on a named object, such as a chest or an enemy's drops. Game code's scene.roll_loot_table(name, stream) picks one entry's item with a chance of its weight over the total, from the run's seed, so the same seed rolls the same loot; weight 0 never drops.",
        gpu: NO_GPU,
        example: || json!({"entries": [{"item": "coin", "weight": 3}, {"item": "gem", "weight": 1}]}),
    },
    ComponentSection {
        key: "rusting.state",
        summary: "The current state of an object's state machine, such as an enemy's patrol or chase. Game code changes it with scene.set_state(name, state), which records since_tick, the fixed tick it was entered on; set the starting state here. transitions run it with no code: each fixed tick the first edge whose from matches (empty matches any), whose after_seconds have passed in the state, and whose counter (empty for none) is at least at_least, whose held input action (empty for none) is held, and whose touching object (empty for none) touches it, moves it to `to` and adds then_add to the counter then_counter (empty for none). actions add to a counter when their state is entered, or left when exit is true, by a transition or set_state. A quest is an object named Quest whose states are its stages (keys such as quest.find_hammer), whose transitions wait on counters that pickups or rusting.dialogue choices add to, and whose stage a HUD shows with {state:Quest}.",
        gpu: NO_GPU,
        example: || {
            json!({
                "state": "patrol",
                "since_tick": 0,
                "transitions": [{
                    "from": "patrol",
                    "to": "chase",
                    "after_seconds": 0.0,
                    "counter": "alarm",
                    "at_least": 1,
                    "held": "",
                    "touching": "",
                    "then_counter": "alerts",
                    "then_add": 1,
                }],
                "actions": [{"state": "chase", "exit": false, "counter": "chasers", "add": 1}],
            })
        },
    },
    ComponentSection {
        key: "rusting.pickup",
        summary: "Collected once when a platformer or player body touches its collider (make it a sensor): adds value to the counter, hides the entity, and triggers its sound cue and burst emitter.",
        gpu: NO_GPU,
        example: || json!({"counter": "coins", "value": 1, "requires": null, "collected": false}),
    },
    ComponentSection {
        key: "rusting.tile_map",
        summary: "Grid of square tiles written as text rows, top row first; the Transform position is the top-left corner. Tiles spawn as unsaved Quad entities, and each run of solid tiles in a row gets one fixed box collider.",
        gpu: "one instanced Quad draw per tile kind",
        example: || json!({"tile_size": 0.5, "rows": ["....", "#..#", "####"], "tiles": {"#": {"color": [0.3, 0.6, 0.3, 1.0], "texture": null, "solid": true}}}),
    },
    ComponentSection {
        key: "rusting.platformer_controller",
        summary: "Side-view run and jump in the XY plane. Reads player.left, player.right and player.jump; parent an orthographic camera to follow. air_jumps allows that many extra jumps before landing (1 is a double jump). Game code's scene.dash(name, velocity, seconds) moves it at that velocity with no gravity for that long.",
        gpu: NO_GPU,
        example: || json!({"run_speed": 6.0, "jump_speed": 9.0, "gravity": 25.0, "collision_mask": 1, "air_jumps": 1}),
    },
    ComponentSection {
        key: "rusting.scene_instance",
        summary: "Places another scene under this entity when the scene loads, as a prefab. The placed entities get stable IDs; the text scene stores only the link and each changed field (rusting.instance_overrides), and cooking includes them, so a game ships without the source scene. Nested instances work; a scene that contains itself fails to load.",
        gpu: NO_GPU,
        example: || json!({"source": "prefabs/coin.rscene"}),
    },
    ComponentSection {
        key: "rusting.connections",
        summary: "Signal connections of the object that emits them: each runs a Rust handler the game registered with App::add_signal_handler, with target as the receiver, when this object sends the handler's event or gains or loses its component. A game fails to load a scene that names an unregistered handler or a missing object.",
        gpu: NO_GPU,
        example: || json!({"list": [{"handler": "add_score", "target": "8d1f6f0e-3c43-4b0a-9a57-2f64a1b0c7d2"}]}),
    },
    ComponentSection {
        key: "rusting.joint",
        summary: "Joins this CPU body to target's (null: the world) at anchor and target_anchor. kind is Fixed, Hinge, Slider, BallSocket, ConeTwist, Distance, Spring, or Generic (per-axis Locked, Free, or Limited, each with an optional spring and motor). The frame's X axis is the hinge, slider, and twist axis. A break_force or break_torque above 0 removes the joint and sends JointBroken when a step's load passes it.",
        gpu: NO_GPU,
        example: || json!({"target": null, "kind": {"Hinge": {"limit": [-1.0, 1.0], "spring": {"target": 0.0, "stiffness": 20.0, "damping": 2.0}, "motor": {"speed": 2.0, "max_force": 50.0}}}, "anchor": [0.0, 1.0, 0.0], "frame": [0.0, 0.0, 1.5], "target_anchor": [0.0, 3.0, 0.0], "target_frame": [0.0, 0.0, 1.5], "collide_connected": false, "break_force": 500.0, "break_torque": 0.0}),
    },
    ComponentSection {
        key: "rusting.articulation",
        summary: "Makes this body the root of a reduced-coordinate joint tree. Fixed, Hinge, Slider, and BallSocket joints hanging from it, directly or through other links, are solved in joint space: they never drift apart and hinge and slider limits hold exactly. A dynamic root floats; a fixed or kinematic root is the base. Reduced joints do not break.",
        gpu: NO_GPU,
        example: || json!({}),
    },
];

/// The scene form of an entity holding every built-in section and every
/// registered component at its default value.
pub(crate) fn defaults() -> &'static (Map<String, Value>, Map<String, Value>) {
    static DEFAULTS: std::sync::OnceLock<(
        Map<String, Value>,
        Map<String, Value>,
    )> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(build_defaults)
}

fn build_defaults() -> (Map<String, Value>, Map<String, Value>) {
    let mut app = App::new();
    app.add_plugin(AssetPlugin)
        .expect("a new app accepts the asset plugin");
    let world = app.world_mut();
    // Built from the asset defaults, so the scene form cannot drift from them.
    let renderer = {
        let mut assets = world.resource_mut::<crate::assets::AssetServer>();
        let material = assets.materials.insert(Default::default());
        crate::runtime::MeshRenderer {
            mesh: assets.builtin_primitives
                [&crate::assets::PrimitiveShape::Cube],
            material,
            cast_shadows: true,
            receive_shadows: true,
        }
    };
    let entity = world
        .spawn((
            renderer,
            SceneId(Uuid::nil()),
            Name("Default".into()),
            Transform::default(),
            Camera::default(),
            PhysicsBody::default(),
            RigidBody::default(),
            Collider::default(),
            CollisionLayers::default(),
            GpuPhysicsWatch::default(),
            DirectionalLight::default(),
            PointLight::default(),
            SpotLight::default(),
        ))
        .id();
    for name in registered_component_names(world) {
        add_registered_component(world, entity, &name)
            .expect("registered components have defaults");
    }
    let components = registered_component_values(world, entity)
        .expect("defaults serialize")
        .into_iter()
        .map(|(name, text)| {
            (
                name,
                serde_json::from_str(&text).expect("registry writes JSON"),
            )
        })
        .collect();
    let document = scene_document(world, "defaults").expect("defaults save");
    let Ok(Value::Object(mut entity)) =
        serde_json::to_value(&document.entities[0])
    else {
        unreachable!("an entity saves as an object");
    };
    entity.remove("components");
    // The document writes a cube as "BuiltinCube"; the docs list the
    // primitive form.
    entity["mesh_renderer"]["mesh"] = json!({"BuiltinPrimitive": "Cube"});
    (entity, components)
}

fn section_value(section: &Section, default: Option<&Value>) -> Value {
    json!({
        "key": section.key,
        "summary": section.summary,
        "gpu_cost": section.gpu,
        "default": default,
        "example": (section.example)(),
        "fields": section.fields.iter().map(|field| json!({
            "path": field.path,
            "unit": field.unit,
            "range": field.range,
            "doc": field.doc,
        })).collect::<Vec<_>>(),
    })
}

/// Field documentation of a reflected type, in the layout of [`Field`].
/// Map keys show as `*`, enum payloads under their variant name, and
/// containers appear only when they carry a doc.
fn reflected_fields(info: &TypeInfo) -> Vec<Value> {
    let mut fields = Vec::new();
    describe(info, &Hints::default(), String::new(), &mut fields);
    fields
}

fn describe(
    info: &TypeInfo,
    hints: &Hints,
    path: String,
    out: &mut Vec<Value>,
) {
    let entry = |range: String| json!({"path": path, "unit": hints.unit, "range": range, "doc": hints.doc});
    match info {
        TypeInfo::Option(inner) => describe(inner, hints, path, out),
        TypeInfo::Struct(info) => {
            if !hints.doc.is_empty() {
                out.push(entry(String::new()));
            }
            for field in &info.fields {
                describe(
                    &field.ty,
                    &field.hints,
                    format!("{path}/{}", field.name),
                    out,
                );
            }
        }
        TypeInfo::Map(item) if matches!(**item, TypeInfo::Struct(_)) => {
            if !hints.doc.is_empty() {
                out.push(entry(String::new()));
            }
            describe(item, &Hints::default(), format!("{path}/*"), out);
        }
        TypeInfo::Enum(info) if !info.is_unit_only() => {
            out.push(entry(variant_names(info)));
            for variant in &info.variants {
                let at = format!("{path}/{}", variant.name);
                match &variant.fields {
                    VariantFields::Unit => {}
                    VariantFields::Tuple(items) if items.len() == 1 => {
                        describe(&items[0], &Hints::default(), at, out);
                    }
                    VariantFields::Tuple(items) => {
                        for (index, item) in items.iter().enumerate() {
                            describe(
                                item,
                                &Hints::default(),
                                format!("{at}/{index}"),
                                out,
                            );
                        }
                    }
                    VariantFields::Struct(fields) => {
                        for field in fields {
                            describe(
                                &field.ty,
                                &field.hints,
                                format!("{at}/{}", field.name),
                                out,
                            );
                        }
                    }
                }
            }
        }
        leaf => out.push(entry(range_text(leaf, hints))),
    }
}

fn variant_names(info: &EnumInfo) -> String {
    info.variants
        .iter()
        .map(|variant| variant.name)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn range_text(info: &TypeInfo, hints: &Hints) -> String {
    match (hints.min, hints.max) {
        (Some(min), Some(max)) => return format!("{min}..{max}"),
        (Some(min), None) => return format!(">= {min}"),
        (None, Some(max)) => return format!("<= {max}"),
        (None, None) => {}
    }
    match info {
        TypeInfo::Int { name, .. } => (*name).to_owned(),
        TypeInfo::Enum(info) => variant_names(info),
        TypeInfo::Entity | TypeInfo::Handle(_) => info.kind_name().to_owned(),
        TypeInfo::Array(item, _)
        | TypeInfo::List(item)
        | TypeInfo::Map(item) => range_text(item, &Hints::default()),
        _ => String::new(),
    }
}

fn component_value(
    section: &ComponentSection,
    info: &TypeInfo,
    default: Option<&Value>,
) -> Value {
    json!({
        "key": section.key,
        "summary": section.summary,
        "gpu_cost": section.gpu,
        "default": default,
        "example": (section.example)(),
        "fields": reflected_fields(info),
    })
}

fn reflected_types<'a>(
    types: impl Iterator<Item = (&'a str, &'a TypeInfo)>,
) -> Vec<Value> {
    types
        .map(
            |(key, info)| json!({"key": key, "fields": reflected_fields(info)}),
        )
        .collect()
}

/// JSON Schema (draft 2020-12) for the files the engine reads, keyed by
/// `scene`, `scene_patch`, `scenario`, `project` and `asset_meta`, so generic
/// editors and validators can check them. Entity sections and registered
/// components come from the catalog tables; their values are not described
/// further (the catalog lists the fields).
#[must_use]
pub fn json_schemas() -> Value {
    let registry = SceneComponentRegistry::default();
    let mut entity: Map<String, Value> = ENTITY_SECTIONS
        .iter()
        .map(|section| {
            (
                section.key.to_owned(),
                json!({"description": section.summary}),
            )
        })
        .collect();
    let components: Map<String, Value> = COMPONENT_SECTIONS
        .iter()
        .filter_map(|section| {
            registry.info(section.key).map(|_| {
                (
                    section.key.to_owned(),
                    json!({"type": "string", "description": "component value as a JSON string"}),
                )
            })
        })
        .collect();
    entity.insert(
        "components".into(),
        json!({"type": "object", "properties": components}),
    );
    let entity_ref =
        json!({"type": "string", "description": "entity UUID or unique name"});
    let operation = |name: &str, required: &[&str], props: Value| {
        let mut properties = props;
        properties["op"] = json!({"const": name});
        let mut required = required.to_vec();
        required.push("op");
        json!({"type": "object", "properties": properties,
               "required": required, "additionalProperties": false})
    };
    let value = json!({"description": "any JSON value"});
    let path = json!({"type": "string", "description": "JSON pointer"});
    json!({
        "scene": {
            "$schema": JSON_SCHEMA_DRAFT,
            "title": "RustingEngine scene",
            "type": "object",
            "properties": {
                "format_version": {"type": "integer"},
                "name": {"type": "string"},
                "entities": {"type": "array", "items": {
                    "type": "object", "properties": entity}},
                "render": {"type": "object"},
                "simulation": {"type": "object"},
            },
            "required": ["name", "entities"],
        },
        "scene_patch": {
            "$schema": JSON_SCHEMA_DRAFT,
            "title": "RustingEngine scene patch",
            "type": "object",
            "properties": {
                "expected_revision": {"type": ["string", "null"]},
                "operations": {"type": "array", "items": {"oneOf": [
                    operation("create", &["entity"], json!({"entity": {"type": "object"}})),
                    operation("upsert", &["entity"], json!({"entity": {"type": "object"}, "merge": {"type": "boolean"}})),
                    operation("set", &["path", "value"], json!({"id": entity_ref, "name": entity_ref, "path": path, "value": value, "expected": value})),
                    operation("remove", &["path"], json!({"id": entity_ref, "name": entity_ref, "path": path, "expected": value})),
                    operation("reparent", &["parent"], json!({"id": entity_ref, "name": entity_ref, "parent": {"type": ["string", "null"]}})),
                    operation("duplicate", &["id"], json!({"id": entity_ref, "new_id": {"type": "string"}, "name": {"type": "string"}})),
                    operation("delete", &[], json!({"id": entity_ref, "name": entity_ref, "missing_ok": {"type": "boolean"}})),
                    operation("set_scene", &["path", "value"], json!({"path": path, "value": value, "expected": value})),
                ]}},
            },
            "required": ["operations"],
            "additionalProperties": false,
        },
        "scenario": {
            "$schema": JSON_SCHEMA_DRAFT,
            "title": "RustingEngine scenario",
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "seed": {"type": "integer", "minimum": 0},
                "ticks": {"type": "integer", "minimum": 0},
                "capture_size": {"type": "array", "items": {"type": "integer", "minimum": 1},
                                 "minItems": 2, "maxItems": 2},
                "steps": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"tick": {"type": "integer", "minimum": 0}},
                    "required": ["tick"]}},
                "keep_going": {"type": "boolean"},
                "annotate": {"type": "boolean"},
                "contact_sheet": {"type": "string"},
                "audio_out": {"type": "string"},
                "audio_reference": {"type": "object", "properties": {
                    "path": {"type": "string"}, "tolerance": {"type": "number"}},
                    "required": ["path"]},
                "files": {"type": "object", "additionalProperties": {"type": "string"}},
                "fuzz": {"type": "object", "properties": {
                    "seed": {"type": "integer", "minimum": 0},
                    "actions": {"type": "array", "items": {"type": "string"}},
                    "rate": {"type": "number", "minimum": 0, "maximum": 1}}},
                "explore": {"type": "object", "properties": {
                    "goals": {"type": "array", "items": {"oneOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3}]}},
                    "ticks_per_goal": {"type": "integer", "minimum": 1}}},
                "gpu": {"type": "boolean"},
                "invariants": {"type": "array", "items": {
                    "type": "object",
                    "properties": {"entity": {"type": "string"}, "counter": {"type": "string"}}}},
                "budgets": {"type": "object", "properties": {
                    "max_tick_ms": {"type": "number"},
                    "mean_tick_ms": {"type": "number"},
                    "p95_tick_ms": {"type": "number"},
                    "mean_cpu_ms": {"type": "number"},
                    "max_draws": {"type": "integer", "minimum": 0},
                    "max_triangles": {"type": "integer", "minimum": 0},
                    "max_entities": {"type": "integer", "minimum": 0},
                    "max_lights": {"type": "integer", "minimum": 0}}},
            },
            "required": ["name", "ticks"],
        },
        "project": {
            "$schema": JSON_SCHEMA_DRAFT,
            "title": "RustingEngine project.json",
            "type": "object",
            "properties": {
                "format_version": {"type": "integer"},
                "name": {"type": "string"},
                "main_scene": {"type": "string"},
                "cooked_scene": {"type": "string"},
                "binary_name": {"type": "string"},
                "generators": {"type": "object"},
                "determinism": {"type": "string"},
                "window": {"type": "array", "items": {"type": "integer",
                    "minimum": 1}, "minItems": 2, "maxItems": 2},
                "ui_base_size": {"type": "array", "items": {"type": "integer",
                    "minimum": 1}, "minItems": 2, "maxItems": 2,
                    "description": "[width, height] in pixels the HUD is laid out for; it scales uniformly to fit the window, keeping its aspect ratio. Unset: drawn at the desktop's DPI scale."},
                "window_title": {"type": "string",
                    "description": "Window title when the game opens from the project folder."},
            },
            "required": ["name", "main_scene", "cooked_scene"],
        },
        "asset_meta": {
            "$schema": JSON_SCHEMA_DRAFT,
            "title": "RustingEngine .rmeta asset sidecar",
            "type": "object",
            "properties": {
                "format_version": {"type": "integer"},
                "id": {"type": "string"},
                "kind": {"type": ["string", "object"]},
                "settings": {"type": "object"},
                "dependencies": {"type": "array", "items": {"type": "string"}},
                "content_hash": {"type": "string"},
                "source": {"type": "object"},
            },
            "required": ["format_version", "id", "kind", "content_hash"],
        },
    })
}

const JSON_SCHEMA_DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// The parts of [`catalog`] named `name`: a top-level section such as
/// `scenario`, or every component, resource, asset type or operation whose
/// `key` or `name` is `name` (the `rusting.` prefix is optional). The error
/// lists the names that exist.
pub fn catalog_entry(name: &str) -> Result<Value, String> {
    let catalog = catalog();
    if let Some(section) = catalog.get(name) {
        return Ok(section.clone());
    }
    let full = format!("rusting.{name}");
    let mut found = Vec::new();
    let mut names = Vec::new();
    fn walk(
        value: &Value,
        wanted: [&str; 2],
        found: &mut Vec<Value>,
        names: &mut Vec<String>,
    ) {
        match value {
            Value::Object(fields) => {
                let id = fields
                    .get("key")
                    .or_else(|| fields.get("name"))
                    .and_then(Value::as_str);
                if let Some(id) = id {
                    if wanted.contains(&id) {
                        found.push(value.clone());
                    }
                    names.push(id.to_owned());
                }
                fields
                    .values()
                    .for_each(|field| walk(field, wanted, found, names));
            }
            Value::Array(items) => items
                .iter()
                .for_each(|item| walk(item, wanted, found, names)),
            _ => {}
        }
    }
    walk(&catalog, [name, &full], &mut found, &mut names);
    match found.len() {
        0 => {
            let mut sections: Vec<String> = catalog
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, _)| key.clone())
                .collect();
            sections.extend(names);
            sections.dedup();
            Err(format!(
                "no catalog entry `{name}`; known: {}",
                sections.join(", ")
            ))
        }
        1 => Ok(found.remove(0)),
        _ => Ok(Value::Array(found)),
    }
}

/// The full catalog as JSON.
#[must_use]
pub fn catalog() -> Value {
    let (entity, components) = defaults();
    let registry = SceneComponentRegistry::default();
    let types = TypeRegistry::default();
    json!({
        "catalog_version": SCHEMA_CATALOG_VERSION,
        "engine_version": env!("CARGO_PKG_VERSION"),
        "scene_format_version": SCENE_FORMAT_VERSION,
        "confine": "Any command takes --confine DIR, and RUSTING_CONFINE=DIR sets it for a whole session: the working folder, or an argument that names a path, outside DIR (after links and `..` resolve) fails with OUTSIDE_CONFINE before the command starts, and so does a project file write outside it.",
        "command_log": "RUSTING_COMMAND_LOG=<file> makes every command append one JSON line to that file: args, ok, diagnostic codes and ms. The agent benchmark runner (benchmarks/agent/run.py) uses it to count an agent's commands and corrective builds.",
        "offline": "Any command takes --offline, and RUSTING_OFFLINE=1 sets it for a whole session: every cargo run (check, run, test, export) gets --offline, so a build uses only crates already downloaded and fails instead of reaching the network. The engine opens no connection itself; generator hooks are the project's own commands and may.",
        "read_only": {
            "summary": "Any command takes --read-only, and RUSTING_READ_ONLY=1 sets it for a whole session. Only these commands run then, and `scene patch` or `fix` with --dry-run; others fail with READ_ONLY before they start, and a file write fails the same way.",
            "commands": crate::cli::READ_ONLY_COMMANDS.iter()
                .map(|words| words.join(" ")).collect::<Vec<_>>(),
        },
        "operations": OPERATIONS.iter().map(|operation| json!({
            "name": operation.name,
            "usage": format!("rusting {}", operation.usage),
            "summary": operation.summary,
            "gpu_cost": operation.gpu,
            "defaults": operation.defaults.iter()
                .map(|(flag, value)| (flag.to_string(), json!(value)))
                .collect::<Map<_, _>>(),
            "example": format!("rusting {}", operation.example),
        })).collect::<Vec<_>>(),
        "scene": {
            "file": "JSON object with format_version, name, entities, and render {quality: Auto|Eco|Balanced|High, culling: Auto|Disabled|Frustum|FrustumAndOcclusion}",
            "entity_sections": ENTITY_SECTIONS.iter()
                .map(|section| section_value(section, entity.get(section.key)))
                .collect::<Vec<_>>(),
            "components_note": "Registered components are stored under `components` as JSON strings keyed by name. Scenario `expect` paths and this catalog show them parsed. Fields come from each type's reflection: `*` stands for any map key, enum payloads sit under the variant name, an entity reference saves as the target's object ID, an asset handle as {\"$asset\": path}, and a component with migrations saves \"$version\". Unknown fields fail the load; register a rename or remove migration instead.",
            "components": COMPONENT_SECTIONS.iter()
                .map(|section| {
                    let info = registry.info(section.key)
                        .expect("catalog components are registered");
                    component_value(section, info, components.get(section.key))
                })
                .collect::<Vec<_>>(),
        },
        "resources_note": "Resources are runtime state that game code sets (for example `scene.set_render_scale`). A scene file cannot set them: scene settings are `render` and `simulation` (set_scene `/render/...`).",
        "resources": reflected_types(types.resources()),
        "asset_types_note": "The fields of asset types as a registered component holds them through an asset handle. A texture slot there is {\"$asset\": path}; a material written inline in a scene's mesh_renderer takes a plain path string instead (scene.entity_sections).",
        "asset_types": reflected_types(types.assets()),
        "physics_sync_readback_bytes": PhysicsSyncMode::STATE_READBACK_BYTES,
        "scene_patch": {
            "file": "JSON object: expected_revision (optional, from scene inspect), operations",
            "ids": "id, parent and create's entity.parent take a UUID or the unique name of an entity",
            "paths": "JSON pointers into the entity's scene form with registered components parsed, as in scenario expect paths; /id cannot change",
            "operations": {
                "create": "{\"op\": \"create\", \"entity\": {\"name\": \"Crate\", \"parent\": null}}: a missing id gets a new UUID; fields left out of built-in sections take their defaults",
                "upsert": "{\"op\": \"upsert\", \"entity\": {\"name\": \"Crate\"}}: create, or replace the entity with the same id (else name) and keep its id; a patch of upserts can run again; \"merge\": true keeps fields left out (null removes one)",
                "set": "{\"op\": \"set\", \"id\": UUID, \"path\": \"/transform/position/1\", \"value\": 2.0, \"expected\": 1.0}: expected is optional",
                "remove": "{\"op\": \"remove\", \"id\": UUID, \"path\": \"/collider\"}",
                "reparent": "{\"op\": \"reparent\", \"id\": UUID, \"parent\": UUID or null}",
                "duplicate": "{\"op\": \"duplicate\", \"id\": UUID, \"new_id\": UUID, \"name\": \"Copy\"}: one entity, no children; unnamed unless name is given",
                "delete": "{\"op\": \"delete\", \"id\": UUID, \"missing_ok\": true}: also deletes descendants; missing_ok skips a missing entity. In set, remove, reparent and delete, id takes a UUID or unique name and may be spelled name: {\"op\": \"delete\", \"name\": \"Crate 1\"}",
                "set_scene": "{\"op\": \"set_scene\", \"path\": \"/render/quality\", \"value\": \"High\"}: scene fields (name, render, simulation); expected is optional",
            },
            "create_entity": "An entity is {\"name\", \"parent\" (UUID, unique name or null), \"id\" (optional)}, plus built-in sections such as transform, mesh_renderer, collider, physics_body, camera and lights at its top level, plus \"components\": {\"rusting.<name>\": {...}} for registered components. Sections and components may be partial.",
            "create_example": serde_json::from_str::<Value>(crate::scene_patch::CREATE_EXAMPLE)
                .expect("the create example is JSON"),
            "errors": "SCENE_CONFLICT (revision or expected value differs), PATCH_OPERATION, PATCH_INVALID",
        },
        "scenario": {
            "file": "JSON object: name, seed (u64, default 0), ticks (last tick run), capture_size ([w, h], default [1280, 720]; the headless screen size from tick 0), files, steps",
            "files": "{\"files\": {\"saves/1.json\": \"fixtures/save.json\"}}: top-level; copies fixtures (relative to the scenario file) into the run's empty user data folder, build/test-userdata/<scenario>/, before tick 0",
            "tick_order": "Scenario tick N is the game's fixed tick N: game code sees time.fixed_tick == N in that tick's update. Within one tick: set, press, release and pointer steps apply first; then the game's update and the fixed physics step run; then expect, expect_events, expect_screen, log and capture steps and the invariants see the result. So an expect on the same tick as a set sees the value after that tick's update, not the value before the set; check the old value one tick earlier.",
            "class": "\"entity\": \"class:ball\" in expect, log and invariants reads {\"count\": n, \"gpu\": {\"count\": n, \"min\": [x, y, z], \"max\": [x, y, z]}}: the members of an object class, and the bounds of those whose GPU pose has arrived. \"class:ball in -5,0,-5 5,10,5\" adds gpu/inside, the bodies inside that box. Reading it requests a GPU snapshot that lands 1 to 3 ticks later, so use within or until: {\"tick\": 60, \"until\": 600, \"expect\": {\"entity\": \"class:ball in -5,0,-5 5,10,5\", \"path\": \"/gpu/inside\", \"greater_than\": 39000}}.",
            "audio": "\"entity\": \"audio:\" in expect, log and invariants reads {\"requested\": n, \"clips\": {\"<clip path>\": n}}: every sound game code and sound cues asked for, played or not (headless runs have no device). Escape `/` in a clip path as `~1`: /clips/sfx~1hit.wav; a clip that never played reads as 0 plays. From an offline mix of those sounds it also reads level [l, r] (RMS over the last tick), peak [l, r] (largest sample that tick; 1.0 is full scale), clipped (samples at or over full scale since tick 0; expect it to equal 0 to catch clipping) and playing.",
            "counter": "In expect, set, log and invariants, {\"counter\": \"score\"} replaces entity and path: it names the entity holding the `rusting.counter` called `score` and defaults the path to /components/rusting.counter/value. Counters made by game code (`set_counter` or `add_to_counter` on a new name) work too. A counter nobody has created yet reads as 0, as in game code, with `missing: true` in the logged form.",
            "steps": {
                "press": "{\"tick\": 5, \"press\": \"player.jump\"}: press every input bound to the action before the tick's update; \"at\": 0.4 (press and tap) lands the press 40% into the tick, so scene.press_tick gives 5.4",
                "release": "{\"tick\": 6, \"release\": \"player.jump\"}",
                "tap": "{\"tick\": 5, \"tap\": \"pause\"}: presses the action and releases it before the next tick; egui sees the keys too, so Tab and Enter navigate menus",
                "left_stick": "{\"tick\": 5, \"left_stick\": [0.0, 1.0]}: tilts the gamepad's left stick (each axis -1..1, [0, 1] fully up) until another step moves it; past half tilt it also presses PadLeftStickUp and the other directions. right_stick works the same",
                "click": "{\"tick\": 2, \"click\": \"New Game\"}: moves the cursor to the topmost text the last frame drew with this exact label (egui widgets, HUD buttons, painted text) and clicks the left mouse button; fails listing the texts on screen when none matches. Object form {\"click\": {\"text\": \"-\", \"index\": 2}} or {\"click\": {\"starts_with\": \"Slot 1\"}}: starts_with matches a prefix; index (0-based) picks the nth match in reading order, top to bottom then left to right; copies drawn within 4 px of each other, such as a text shadow, count once",
                "restart": "{\"tick\": 20, \"restart\": true}: reloads the starting scene; the user data folder keeps its files",
                "expect_quit": "{\"tick\": 31, \"expect_quit\": true}: the game called scene.quit() by this tick; the run ends after the tick that quits and steps at later ticks fail",
                "expect_file": "{\"tick\": 41, \"expect_file\": {\"path\": \"settings.cfg\", \"contains\": \"volume=60\"}}: a file in the user data folder; exists (default true) false checks it is absent; unlike other checks it also runs after the game quits",
                "focus": "{\"tick\": 10, \"focus\": false}: takes the window focus away before the tick's update, as alt-tab does: held inputs are released and scene.window_focused() is false until {\"focus\": true}",
                "pointer": "{\"tick\": 10, \"pointer\": [0.5, 0.5]}: moves the mouse cursor to this point of the view, as fractions of its width and height from the top-left corner",
                "expect": "{\"tick\": 30, \"until\": 60, \"expect\": {\"entity\": \"Cube\", \"path\": \"/transform/position/1\", \"less_than\": 0.0}}: JSON pointer into the entity's scene form; equals (with tolerance), not_equals (fails when equal within tolerance), greater_than, less_than (an array such as audio:/playing compares its length), or exists (false once an entity is despawned; empty path means the whole entity); until: an absolute tick; the check must hold every tick from `tick` through it; within: an absolute tick; passes on the first tick from `tick` through it that holds",
                "expect_events": "{\"tick\": 60, \"expect_events\": {\"kind\": \"collision\", \"entity\": \"Cube\", \"at_least\": 1}}: events gameplay saw from tick 0",
                "expect_screen": "{\"tick\": 30, \"expect_screen\": {\"entity\": \"Coin\", \"on_screen\": true, \"inside\": [0.0, 0.0, 0.5, 1.0], \"min_share\": 0.01}}: where a mesh entity is in the active camera's frame (capture_size): on_screen, occluded (all of its box is behind other objects), inside (its box within the fractions [left, top, right, bottom]), min_share (share of the frame it is the nearest hit for); bounds picking; until and within work too. \"camera\": \"Cam 2\" projects through that camera instead, and inside and min_share are then fractions of its viewport",
                "expect_pixels": "{\"tick\": 30, \"expect_pixels\": {\"region\": [0.5, 0.0, 1.0, 1.0], \"mean_min\": [0, 0, 80], \"mean_max\": [60, 60, 255], \"stddev_min\": 2.0}}: mean sRGB [r, g, b] (0..255) and brightness standard deviation of a region of the frame (HUD included); region is [left, top, right, bottom] fractions, the whole frame by default; \"camera\" makes region relative to that camera's viewport; \"differs_from\": \"shots/a.png\" with difference_min and/or difference_max compares the region with an earlier capture by the largest of the R, G and B mean differences (0..255), for example difference_min 2.0 to check two moments look different; until and within work too; fails without Vulkan",
                "capture": "{\"tick\": 30, \"capture\": \"shots/tick30.png\"} or {\"capture\": {\"path\": \"shots/a.png\", \"camera\": \"Cam 2\", \"hud\": false, \"golden\": \"golden/a.png\", \"tolerance\": 1.0}}: path relative to the scenario file; camera renders that camera over the whole frame without the HUD; golden compares with that image: it fails when the largest of the R, G and B mean differences (0..255) is over tolerance (default 1.0), and `rusting test --update-golden` rewrites it; skipped without Vulkan",
                "log": "{\"tick\": 10, \"until\": 40, \"log\": {\"counter\": \"score\"}}: records the value (entity and path, or counter) in the report every tick through until; never fails",
                "set": "{\"tick\": 0, \"set\": {\"entity\": \"Player\", \"path\": \"/transform/position\", \"value\": [0.0, 1.0, -12.0]}}: writes before the tick runs; path under /transform, /visible, /rigid_body, /collider, /physics_body, /collision_layers, /point_light, /spot_light, /directional_light (the entity must already have it) or /components/<name>, for example /components/rusting.counter/value or /collider/friction. To turn a player's view before a click (clicks aim along the view centre), set /components/rusting.player_controller/yaw and /pitch in radians; the controller applies them, within its limits, that tick",
            },
            "gpu": "{\"gpu\": true}: top-level; opens the headless Vulkan device so GPU bodies simulate and GPU events arrive in `rusting test`. A scenario with a `capture` step does this too, but without either the GPU bodies stay where they spawned and GPU rules never fire. Fails with `gpu: no Vulkan device` when none opens; software Vulkan (lavapipe) works but is slow",
            "explore": "{\"explore\": {\"goals\": [\"Coin\"], \"ticks_per_goal\": 600}}: top-level; an explorer bot walks the PlayerController toward each goal, an entity name or an [x, y, z] point, so a list of points walks a route (empty: every sensor collider, nearest first) with the player.* actions, jumps when it stops getting closer, and fails naming the goals it did not reach within ticks_per_goal; the report lists `explore.goals` (name, reached tick or null) and `explore.stuck`",
            "fuzz": "{\"fuzz\": {\"seed\": 3, \"actions\": [\"jump\"], \"rate\": 0.1}}: top-level; before each tick from 1, each action (empty: every action the scene binds) flips between pressed and released with chance `rate`, seeded by `seed`; the report lists the presses as `fuzz_steps`. `rusting fuzz` sets it per seed",
            "invariants": "{\"invariants\": [{\"entity\": \"Player\", \"path\": \"/transform/position\", \"finite\": true}, {\"entity\": \"Player\", \"path\": \"/transform/position/1\", \"greater_than\": -5.0}]}: top-level; same fields as `expect` plus `finite` (no NaN or infinite number). Checked after every tick; the first tick one fails is reported as `invariant N:`. A missing entity or path passes unless `exists` is set",
            "budgets": "{\"budgets\": {\"p95_tick_ms\": 8.0, \"max_draws\": 200}}: top-level; each limit exceeded fails the run with a `budget:` step. Timing is wall-clock and machine-dependent: set headroom and read `perf.environment` (engine version, OS, arch, debug or release, device, `cpus` and the one-minute `load_average` on Linux); a timing failure on a machine with load above half its CPUs says so, because other builds and tests slow every tick 2-3x in `rusting test --json`. `mean_cpu_ms` limits `perf.cpu_ms_mean`, the CPU time of all game threads per tick over the whole run (Linux only, 10 ms steps over the run, so give it a few hundred ticks; not checked elsewhere): other processes slow it far less than wall time, so it suits a shared machine. Draw and triangle limits draw every tick through headless Vulkan, no capture step needed, and fill `perf.render` (draws, triangles, gpu_ms, and `cameras` with each camera's gpu_ms, draws and triangles). `max_lights` limits `perf.render.lights`, the directional, point and spot lights the last frame uploaded (at most 64; `dropped_lights` counts the rest), and draws every tick like `max_draws`. `max_entities` limits `perf.entities_max`, the most live entities (scene objects, spawned copies, counters; not engine resources) after any tick. `perf.render` is null only in a scenario with no render budget, no `\"gpu\": true` and no capture or expect_pixels step",
            "tick_length_seconds": 1.0 / 60.0,
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn touched_reports_project_writes_but_not_reads_dry_runs_or_failures() {
        let words = |line: &str| -> Vec<String> {
            line.split(' ').map(str::to_owned).collect()
        };
        let disk =
            |line: &str, ok| super::touched(&words(line), ok)["disk"].as_bool();
        assert_eq!(disk("scene patch a.json p.json", true), Some(true));
        assert_eq!(
            disk("scene patch a.json --dry-run p.json", true),
            Some(false)
        );
        assert_eq!(disk("scene patch a.json p.json", false), Some(false));
        assert_eq!(disk("scene query a.json", true), Some(false));
        assert_eq!(disk("fix . --json", true), Some(true));
        let all = super::touched(&words("test ."), true);
        assert_eq!(
            (all["editor"].as_bool(), all["game"].as_bool()),
            (Some(false), Some(false))
        );
    }

    use std::collections::BTreeSet;

    use super::*;
    use crate::runtime::{
        load_scene_document, set_registered_component, SceneDocument,
        SceneLoadMode, CONNECTIONS_COMPONENT, SCENE_INSTANCE_COMPONENT,
    };

    fn schema_keys(schema: &Value) -> BTreeSet<String> {
        schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    fn written_keys(value: &impl serde::Serialize) -> BTreeSet<String> {
        serde_json::to_value(value)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn json_schemas_describe_every_key_the_engine_writes() {
        let schemas = json_schemas();
        // Every schema key must be known to serde; every written key must be
        // in the schema.
        let scene = SceneDocument {
            format_version: 0,
            name: String::new(),
            entities: Vec::new(),
            render: Default::default(),
            simulation: Default::default(),
        };
        assert_eq!(written_keys(&scene), schema_keys(&schemas["scene"]));
        let scenario: crate::scenario::Scenario =
            serde_json::from_value(json!({"name": "n", "ticks": 1})).unwrap();
        assert_eq!(written_keys(&scenario), schema_keys(&schemas["scenario"]));
        let patch = crate::scene_patch::ScenePatch::default();
        assert_eq!(written_keys(&patch), schema_keys(&schemas["scene_patch"]));
        let project = crate::project::ProjectManifest {
            format_version: 1,
            name: "n".into(),
            main_scene: "a".into(),
            cooked_scene: "b".into(),
            binary_name: String::new(),
            generators: Default::default(),
            determinism: Default::default(),
            window: None,
            ui_base_size: None,
            window_title: None,
        };
        let mut keys = written_keys(&project);
        keys.extend(
            [
                "generators",
                "determinism",
                "window",
                "ui_base_size",
                "window_title",
            ]
            .map(String::from),
        );
        assert_eq!(keys, schema_keys(&schemas["project"]));
        for name in
            ["scene", "scene_patch", "scenario", "project", "asset_meta"]
        {
            assert_eq!(schemas[name]["$schema"], JSON_SCHEMA_DRAFT, "{name}");
        }
    }

    #[test]
    fn json_schema_patch_operations_match_the_parser() {
        let schemas = json_schemas();
        let ops = schemas["scene_patch"]["properties"]["operations"]["items"]
            ["oneOf"]
            .as_array()
            .unwrap();
        for op in ops {
            let name = op["properties"]["op"]["const"].as_str().unwrap();
            // A patch naming only `op` must fail for a missing field, not for
            // an unknown operation.
            let error = serde_json::from_value::<
                crate::scene_patch::PatchOperation,
            >(json!({"op": name}))
            .unwrap_err()
            .to_string();
            assert!(!error.contains("unknown variant"), "{name}: {error}");
        }
        assert_eq!(ops.len(), 8);
    }

    #[test]
    fn json_schema_entity_lists_every_section_and_component() {
        let schemas = json_schemas();
        let entity = &schemas["scene"]["properties"]["entities"]["items"];
        let keys = schema_keys(entity);
        for section in ENTITY_SECTIONS {
            assert!(keys.contains(section.key), "{}", section.key);
        }
        let components = schema_keys(&entity["properties"]["components"]);
        for section in COMPONENT_SECTIONS {
            assert!(components.contains(section.key), "{}", section.key);
        }
    }

    /// JSON pointers of every leaf. Arrays are leaves.
    fn leaves(value: &Value, prefix: String, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) if !map.is_empty() => {
                for (key, value) in map {
                    leaves(value, format!("{prefix}/{key}"), out);
                }
            }
            _ => {
                out.insert(prefix);
            }
        }
    }

    /// Whether `path` is `pattern`, or under it when `prefix` is set, with
    /// `*` in the pattern matching any one segment.
    fn matches(pattern: &str, path: &str, prefix: bool) -> bool {
        let mut pattern = pattern.split('/');
        let mut path = path.split('/');
        loop {
            match (pattern.next(), path.next()) {
                (None, None) => return true,
                (None, Some(_)) => return prefix,
                (Some(_), None) => return false,
                (Some(wanted), Some(segment)) => {
                    if wanted != "*" && wanted != segment {
                        return false;
                    }
                }
            }
        }
    }

    /// Every saved leaf is documented, and every documented path has a
    /// saved leaf at or under it in the example or the default.
    fn check_fields(
        key: &str,
        documented: &[String],
        example: &Value,
        default: Option<&Value>,
    ) {
        let mut saved = BTreeSet::new();
        leaves(example, String::new(), &mut saved);
        if let Some(default) = default.filter(|value| !value.is_null()) {
            leaves(default, String::new(), &mut saved);
        }
        saved.remove("");
        for path in &saved {
            assert!(
                documented
                    .iter()
                    .any(|pattern| matches(pattern, path, false)),
                "{key}{path} is saved but not documented"
            );
        }
        // Fields of an enum variant the example does not use have no
        // saved parent, so only fields under a saved parent must be saved.
        let parent_saved = |pattern: &String| {
            pattern.rsplit_once('/').is_none_or(|(parent, _)| {
                parent.is_empty()
                    || saved.iter().any(|path| matches(parent, path, true))
            })
        };
        for pattern in documented
            .iter()
            .filter(|path| !path.is_empty() && parent_saved(path))
        {
            assert!(
                saved.iter().any(|path| matches(pattern, path, true)),
                "{key}{pattern} is documented but not saved"
            );
        }
    }

    fn documented(section: &Section) -> Vec<String> {
        section
            .fields
            .iter()
            .map(|field| field.path.to_owned())
            .collect()
    }

    #[test]
    fn every_saved_field_is_documented_and_every_section_is_listed() {
        let (entity, components) = defaults();
        let listed: BTreeSet<_> =
            ENTITY_SECTIONS.iter().map(|section| section.key).collect();
        let saved: BTreeSet<_> = entity.keys().map(String::as_str).collect();
        assert_eq!(listed, saved, "entity sections and saved keys differ");
        for section in ENTITY_SECTIONS {
            check_fields(
                section.key,
                &documented(section),
                &(section.example)(),
                entity.get(section.key),
            );
        }
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        let registered = registered_component_names(app.world());
        let mut listed: Vec<_> = COMPONENT_SECTIONS
            .iter()
            .map(|section| section.key)
            .collect();
        listed.sort_unstable();
        assert_eq!(listed, registered, "catalog and component registry differ");
        let registry = SceneComponentRegistry::default();
        for section in COMPONENT_SECTIONS {
            let fields = reflected_fields(registry.info(section.key).unwrap());
            let paths: Vec<String> = fields
                .iter()
                .map(|field| field["path"].as_str().unwrap().to_owned())
                .collect();
            check_fields(
                section.key,
                &paths,
                &(section.example)(),
                components.get(section.key),
            );
        }
    }

    #[test]
    fn examples_load_through_the_scene_parser_and_registry_unchanged() {
        let mut entity: Map<String, Value> = ENTITY_SECTIONS
            .iter()
            .map(|section| (section.key.to_owned(), (section.example)()))
            .collect();
        // The instance example names a file that does not exist here, and
        // the connections example a handler and an object.
        let components: Map<String, Value> = COMPONENT_SECTIONS
            .iter()
            .filter(|section| {
                ![SCENE_INSTANCE_COMPONENT, CONNECTIONS_COMPONENT]
                    .contains(&section.key)
            })
            .map(|section| {
                let text = (section.example)().to_string();
                (section.key.to_owned(), Value::String(text))
            })
            .collect();
        entity.insert("components".into(), Value::Object(components));
        let document: SceneDocument = serde_json::from_value(json!({
            "format_version": SCENE_FORMAT_VERSION,
            "name": "examples",
            "entities": [entity],
        }))
        .expect("examples parse as a scene");

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        let world = app.world_mut();
        load_scene_document(world, &document, SceneLoadMode::Replace)
            .expect("examples load");
        let saved = scene_document(world, "examples").unwrap();
        let mut saved = serde_json::to_value(&saved.entities[0]).unwrap();
        let mut expected = serde_json::to_value(&document.entities[0]).unwrap();
        // Compare components as values: field order may differ.
        for value in [&mut saved, &mut expected] {
            for component in
                value["components"].as_object_mut().unwrap().values_mut()
            {
                *component =
                    serde_json::from_str(component.as_str().unwrap()).unwrap();
            }
        }
        assert_eq!(saved, expected);

        // Each component example also restores alone through the registry.
        let entity = world.spawn(SceneId(Uuid::new_v4())).id();
        for section in COMPONENT_SECTIONS {
            set_registered_component(
                world,
                entity,
                section.key,
                &(section.example)().to_string(),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", section.key));
        }
    }

    #[test]
    fn catalog_entry_finds_one_component_or_section() {
        let player = catalog_entry("player_controller").unwrap();
        assert_eq!(player["key"], "rusting.player_controller");
        assert!(catalog_entry("rusting.player_controller").is_ok());
        assert!(catalog_entry("scenario").unwrap().get("audio").is_some());
        assert_eq!(catalog_entry("doctor").unwrap()["name"], "doctor");
        let error = catalog_entry("nope").unwrap_err();
        assert!(error.contains("rusting.camera_screen"), "{error}");
    }

    #[test]
    fn catalog_lists_every_operation_with_defaults_and_cost() {
        let catalog = catalog();
        let operations = catalog["operations"].as_array().unwrap();
        assert_eq!(operations.len(), OPERATIONS.len());
        for operation in operations {
            assert!(operation["gpu_cost"]
                .as_str()
                .is_some_and(|c| !c.is_empty()));
            assert!(operation["example"].as_str().unwrap().starts_with(
                &format!("rusting {}", operation["name"].as_str().unwrap())
            ));
        }
        let camera = &catalog["scene"]["entity_sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|section| section["key"] == "camera")
            .unwrap()["default"];
        assert_eq!(camera["active"], false);
        let player = catalog["scene"]["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|section| section["key"] == "rusting.player_controller")
            .unwrap();
        assert_eq!(player["default"]["walk_speed"], 4.0);
        let pitch = player["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["path"] == "/pitch")
            .unwrap();
        assert_eq!(pitch["range"], "-1.55..1.55");
        assert_eq!(pitch["unit"], "rad");
        let tiles = catalog["scene"]["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|section| section["key"] == "rusting.tile_map")
            .unwrap();
        assert!(tiles["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["path"] == "/tiles/*/solid"));
        let resources = catalog["resources"].as_array().unwrap();
        assert!(resources
            .iter()
            .any(|resource| resource["key"] == "rusting.render_settings"));
        let material = &catalog["asset_types"][0];
        assert_eq!(material["key"], "rusting.material");
        assert!(material["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["path"] == "/alpha_mode/Mask/cutoff"));
    }
}
