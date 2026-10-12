//! Registry of every diagnostic code the `rusting` tools emit, printed by
//! `rusting explain`.
//!
//! A test scans the source tree for code literals, so a new code without an
//! entry here, or an entry for a code nothing emits, fails the build.

use serde_json::{json, Value};

/// One diagnostic code: what it means, how to fix it, and an example.
pub struct CodeInfo {
    pub code: &'static str,
    pub summary: &'static str,
    pub fix: &'static str,
    pub example: &'static str,
}

const fn code(
    code: &'static str,
    summary: &'static str,
    fix: &'static str,
    example: &'static str,
) -> CodeInfo {
    CodeInfo {
        code,
        summary,
        fix,
        example,
    }
}

/// Every code, sorted by name.
pub const CODES: &[CodeInfo] = &[
    code(
        "AGENTS_OUTDATED",
        "The project's AGENTS.md is an engine copy that differs from this engine's, so it may miss newer features.",
        "Run `rusting fix`; it replaces AGENTS.md and keeps the old file as AGENTS.md.old. Copy your own notes back.",
        "rusting fix",
    ),
    code(
        "ASSET_CHANGED",
        "An imported file no longer matches the content hash in its .rmeta.",
        "Record the new content with `rusting asset reimport`, or restore the original file.",
        "rusting asset reimport . assets/textures/crate.png",
    ),
    code(
        "ASSET_DEPENDENCY_MISSING",
        "An imported asset needs a file (a glTF buffer or image) that is not in the project.",
        "Copy the missing file next to the asset, or reimport the asset from a source folder that has it.",
        "rusting asset reimport . assets/models/tree.gltf --from ~/art/tree.gltf",
    ),
    code(
        "ASSET_DUPLICATE_ID",
        "Two .rmeta files carry the same asset ID, usually because an asset folder was copied by hand.",
        "Delete the copied .rmeta and import the copy again so it gets its own ID.",
        "rm assets/crate_copy.png.rmeta && rusting asset import . assets/crate_copy.png",
    ),
    code(
        "ASSET_EXISTS",
        "The import target already exists in the project.",
        "Use `rusting asset reimport` to replace it, or `--to` to import into another folder.",
        "rusting asset import . ~/art/crate.png --to textures/v2",
    ),
    code(
        "ASSET_INVALID",
        "The file could not be loaded as the runtime would load it (bad image, bad glTF, or a URI that leaves its folder).",
        "Open the file in its authoring tool and export it again; keep glTF buffers and images inside the model's folder.",
        "rusting asset import . ~/art/level.glb",
    ),
    code(
        "ASSET_IO",
        "Reading or writing an asset file failed.",
        "Check that the path exists and is writable, then run the command again.",
        "rusting asset list --json",
    ),
    code(
        "ASSET_META_INVALID",
        "An .rmeta file is not valid JSON or has missing fields.",
        "Restore it from version control, or delete it and import the asset again (this gives it a new ID).",
        "git checkout -- assets/crate.png.rmeta",
    ),
    code(
        "ASSET_MISSING",
        "An .rmeta file has no imported file next to it.",
        "Restore the file, or delete the .rmeta if the asset is no longer used.",
        "rusting asset list --json",
    ),
    code(
        "ASSET_NOT_FOUND",
        "No asset has the given ID or path.",
        "List the project's assets and use an ID or path from the list.",
        "rusting asset list --json",
    ),
    code(
        "ASSET_NOT_IMPORTED",
        "A file under assets/ has no .rmeta, so it has no stable ID or license.",
        "Import it so the engine records an ID, settings, and provenance.",
        "rusting asset import . assets/sounds/jump.wav --license CC0-1.0",
    ),
    code(
        "ASSET_NO_LICENSE",
        "An asset has no license in its provenance record.",
        "Reimport it with `--license` (and `--author`, `--url` where known).",
        "rusting asset reimport . assets/crate.png --license CC0-1.0 --author \"Kenney\"",
    ),
    code(
        "ASSET_RELOAD_FAILED",
        "An asset failed to hot reload while the game was running; the old version stays loaded.",
        "Fix the file named in the message; the game picks it up on the next save.",
        "rusting asset list --json",
    ),
    code(
        "ASSET_SOURCE_MISSING",
        "The source file given to an import does not exist.",
        "Check the path; relative paths are read from the current folder.",
        "rusting asset import . ./art/crate.png",
    ),
    code(
        "ASSET_UNSUPPORTED",
        "The file type cannot be imported.",
        "Convert it to png, jpeg, bmp, tga, gltf, or glb.",
        "rusting asset import . ~/art/crate.png",
    ),
    code(
        "BUILD_FAILED",
        "Cargo failed to build the game; the message holds the end of its output.",
        "Fix the first compiler error in the message, then run `rusting check` again.",
        "rusting check --json",
    ),
    code(
        "BUILD_TOOL_MISSING",
        "Cargo could not be started.",
        "Install Rust through rustup and make sure `cargo` is on PATH; `rusting doctor` reports what it finds.",
        "rusting doctor",
    ),
    code(
        "CAMERA_NOT_FOUND",
        "No camera in the scene has the ID or name given to `--camera`, or the scene has no camera.",
        "Query the scene for cameras and pass one of their IDs or names, or add a camera with a patch.",
        "rusting scene query scenes/main.rscene --component camera --json",
    ),
    code(
        "CAPTURE_FAILED",
        "The scene could not be prepared or rendered for a capture.",
        "Fix the scene error in the message; run `rusting validate` to see every problem.",
        "rusting validate --json",
    ),
    code(
        "CAPTURE_TEXT_CONTRAST",
        "Plain HUD text in the captured frame is hard to read: its median contrast against the scene pixels behind it is below WCAG AA (4.5:1, or 3:1 from 24 px).",
        "Change the HUD element's `color` so it stands out from what is behind it, or move it with `anchor` and `offset`.",
        "rusting capture scenes/main.rscene build/frame.png --json",
    ),
    code(
        "CLI_OUTDATED",
        "The `rusting` CLI is older than the engine the game builds against, so its docs, schema and checks may miss new features. A version mismatch is reported on every check; engine source edited after the CLI was built is reported once a day per project (the marker is build/cli-outdated-shown).",
        "Reinstall the CLI from the engine the game uses, then run the command again.",
        "cargo install --path <engine folder> --locked",
    ),
    code(
        "CLI_USAGE",
        "The command or its arguments are not valid. Exit code 2.",
        "Read the usage for the command and run it again.",
        "rusting --help scene patch",
    ),
    code(
        "CODE_MISSING_ASSET",
        "Game code names an asset path in `load_text`, `play_sound*` or `spawn_prefab` that is not a file under `assets/`.",
        "Fix the path in the code (it is relative to `assets/`) or add the file.",
        "rusting validate --json",
    ),
    code(
        "DETERMINISM_DIVERGED",
        "Two runs with the same inputs produced different state hashes; the message names the first tick that differs.",
        "Remove wall-clock time, unseeded randomness, and order-dependent loops from game code; use `GameScene::random`.",
        "rusting determinism --ticks 600 --json",
    ),
    code(
        "DETERMINISM_NO_GPU_STATE",
        "`rusting determinism --gpu` ran the scenario, but no GPU body state was hashed.",
        "Set \"gpu\": true in the scenario and make sure the scene has GPU physics bodies.",
        "rusting determinism --gpu tests/seed.json --json",
    ),
    code(
        "DETERMINISM_UNSUPPORTED",
        "project.json asks for a determinism mode that a scene entity does not support (for example a GPU-owned body).",
        "Lower the mode in project.json, or change the entity to a body kind the mode supports.",
        "rusting scene query scenes/main.rscene --component physics_body --json",
    ),
    code(
        "EFFECT_UNKNOWN",
        "No particle effect preset has the given name.",
        "List the effects and use one of their names.",
        "rusting effect list",
    ),
    code(
        "EXPORT_FAILED",
        "The export build or copy step failed.",
        "Fix the error in the message; `rusting check` must pass before an export.",
        "rusting check && rusting export . ../dist",
    ),
    code(
        "EXPORT_NOT_VERIFIED",
        "A cross-target export was written but not run, since it cannot run on this machine.",
        "Run the exported game on the target platform to verify it.",
        "rusting export . ../dist --target x86_64-pc-windows-gnu",
    ),
    code(
        "EXPORT_PRESET_UNKNOWN",
        "project.json has no export preset with the name given to --preset.",
        "Use one of the names the message lists, or add the preset under export_presets in project.json.",
        "rusting export . ../dist --preset demo --json",
    ),
    code(
        "EXPORT_SIGN_FAILED",
        "The export preset's sign command could not run or exited with an error.",
        "Run the sign command by hand on the exported executable, fix it, then export again.",
        "rusting export . ../dist --preset windows --json",
    ),
    code(
        "EXPORT_VERIFY_FAILED",
        "The exported game did not start cleanly from its own folder, so a file it needs was not packaged.",
        "Make sure every file the game loads is under assets/ or scenes/, then export again.",
        "rusting export . ../dist --json",
    ),
    code(
        "FILE_NOT_FOUND",
        "A scenario file or folder given to `rusting test` does not exist.",
        "Scenario paths are looked up in the project first; check the name, or run `rusting test` for every file in tests/.",
        "rusting test tests/win.json",
    ),
    code(
        "GAME_FAILED",
        "The game process could not start, crashed, or ended without its report.",
        "Read `game.stderr` in the JSON result for the panic message, fix it, and run again.",
        "rusting run --ticks 120 --json",
    ),
    code(
        "GENERATED_SCENE_EDITED",
        "A scene made by a `scene_generators` entry was edited by hand since it was generated, so `rusting scene generate` did not replace it.",
        "Keep the edit by moving it into the generator or its inputs, or pass --force to regenerate over it; `rusting revert` brings back the replaced scene.",
        "rusting scene generate . level --force --json",
    ),
    code(
        "GENERATOR_FAILED",
        "An asset generator hook failed or printed no valid JSON result on its last output line.",
        "Run the hook's command by hand with RUSTING_PROMPT and RUSTING_OUTPUT_DIR set and fix its output.",
        "rusting asset generate . icons \"red gem\" --dry-run",
    ),
    code(
        "GENERATOR_UNKNOWN",
        "project.json has no generator hook with the given name.",
        "Add the hook under `generators` in project.json, or use a name that is there.",
        "rusting project inspect --json",
    ),
    code(
        "IO_ERROR",
        "A file or folder the command needs could not be created or written.",
        "Check permissions and free space for the path in the message.",
        "rusting determinism --json",
    ),
    code(
        "JOURNAL_UNKNOWN_OP",
        "`rusting revert` was given an operation ID the project's journal does not have.",
        "Run `rusting log` and copy the 8-character `op` of the operation to undo.",
        "rusting revert 3fa2c1d9 --json",
    ),
    code(
        "LEASE_HELD",
        "Another agent leases this file or folder, so the claim or write was refused. The message names the holder and how long the lease has left.",
        "Wait for the holder to finish, or work on another file. The holder releases with `rusting lease release <path>`; an abandoned lease expires on its own.",
        "rusting lease claim scenes/main.rscene --as builder --json",
    ),
    code(
        "LEASE_IO",
        "A lease claim or release could not read or write `.rusting/leases.json`, or the path is not inside a project.",
        "Run it on a path inside a folder with `project.json`; if `.rusting/leases.lock` is left over from a killed process, delete it.",
        "rusting lease claim scenes/main.rscene --as builder --json",
    ),
    code(
        "LINT_CAMERA_INSIDE",
        "A camera starts inside another entity's box, sphere or capsule collider (player bodies aside), so the first frame shows the inside of that geometry.",
        "Move the camera or its parent out of the collider, or make the collider a sensor if it is not solid.",
        "rusting lint --json",
    ),
    code(
        "LINT_COLLIDER_MISMATCH",
        "A solid collider is more than twice as big or less than half as big as its entity's built-in mesh on some axis, so the object is hit where it is not drawn, or not hit where it is.",
        "Size the collider to the mesh (a 1 m cube has half extents 0.5), or make it a sensor if it is a trigger zone.",
        "rusting lint --json",
    ),
    code(
        "LINT_COLOR_ONLY_STATUS",
        "Game code changes a HUD element's `color` through `set_hud` but never its `text`, and its scene text has no `{counter}` readout, so the state it shows reaches only players who tell those colours apart.",
        "Change the text along with the colour (a word such as `READY`, or a symbol), or show the state somewhere else as well.",
        "rusting lint --json",
    ),
    code(
        "LINT_GOAL_INSIDE",
        "A sensor collider (a pickup, goal or trigger) has its centre inside another entity's solid box, sphere or capsule collider, so a player walking up to it is stopped before it is reached.",
        "Move the sensor out of the solid, or make that collider a sensor too if it is not meant to block.",
        "rusting lint --json",
    ),
    code(
        "LINT_GOAL_UNREACHABLE",
        "A sensor collider (a pickup, goal or trigger) at the player's height cannot be reached on foot from the player's start: fixed walls, or ledges higher than the player can step or jump, close off every route on that floor.",
        "Open a gap in the walls, lower a ledge, or move the sensor. Walls whose names appear as strings in game code count as doors and do not block; a door reached another way (a parent's name, a name built at run time) still does.",
        "rusting lint --json",
    ),
    code(
        "LINT_LIGHT_BUDGET",
        "More lights are visible than the quality profile uploads (Eco 256, Balanced 512, High and Auto 1024). The renderer takes visible directional, then point, then spot lights in scene order and drops the rest without lighting anything.",
        "Hide lights that are not needed yet with `visible: false` (hidden lights are not uploaded), remove lights, or raise `render.quality`.",
        "rusting lint --json",
    ),
    code(
        "LINT_LIGHT_OFF",
        "A light can never light anything: negative intensity, a zero range, or a black color. Intensity 0 is allowed, for lights game code switches on.",
        "Raise the intensity (illuminance for a directional light) and range, or remove the light.",
        "rusting lint --json",
    ),
    code(
        "LINT_MISSING_ACTION",
        "Game code reads an input action, as in `scene.pressed(\"jmup\")`, that no `rusting.input_action` in any scene or prefab, `rebind` call or built-in `player.*` action defines, so it is never pressed.",
        "Fix the name (the message lists close action names), or add a `rusting.input_action` for it.",
        "rusting lint --json",
    ),
    code(
        "LINT_MISSING_CLASS",
        "Game code loops over a class, as in `scene.in_class(\"enemys\")`, that no entity in any scene or prefab has and no other string in game code names (an `add_class` call, a spawn helper's class argument), so the loop always sees no objects.",
        "Fix the name (the message lists close class names), or put objects in the class with `classes` in the scene or `add_class`.",
        "rusting lint --json",
    ),
    code(
        "LINT_MISSING_EVENT",
        "Game code reads GPU physics events by a literal name, as in `scene.gpu_events(\"landd\")`, that no `GpuPhysicsRule` in game code and no scene names, so the read always returns no events.",
        "Fix the name (the message lists close names from game code), or add a rule that raises the event with `watch_gpu_object` or `watch_gpu_class`.",
        "rusting lint --json",
    ),
    code(
        "LINT_MISSING_FILE",
        "Game code loads a scene, places a prefab or plays a clip by a literal path, as in `scene.load_scene(\"scenes/levl_2.rscene\")`, and no file is there, so the load fails or the sound never plays.",
        "Fix the path (scenes are relative to the project, prefabs and clips to `assets/`; the message lists close names in the same folder), or add the file.",
        "rusting lint --json",
    ),
    code(
        "LINT_MISSING_OBJECT",
        "Game code names an object, as in `scene.object(\"Playr\")`, that no entity in any scene or prefab has, so the call panics or does nothing at run time.",
        "Fix the name (the message lists close scene names), or add the object to the scene.",
        "rusting lint --json",
    ),
    code(
        "LINT_MISSING_TRANSLATION",
        "Game code calls `tr(\"key\")` or `tr_count(\"key\", ..)`, or a HUD text holds `{tr:key}`, but a locale file in `assets/locales/` has no text for that key, so players in that locale see the bare key.",
        "Add the key to that locale's JSON file; `tr_count` needs `key.one` and `key.other`.",
        "rusting lint --json",
    ),
    code(
        "LINT_NO_ENDING",
        "The scene keeps counters, but no counter has a target and the game code never calls counter_complete, load_scene or quit, so a round can never be won or lost.",
        "Give the counter that decides the round a target and show a win or lose `rusting.hud` with `requires` set to it, or end the round from game code.",
        "rusting lint --json",
    ),
    code(
        "LINT_PLAYER_SCALE",
        "A player body is outside 0.5 to 3 m tall, usually a scale left on it or a parent.",
        "Set the scale on the player and its parents to 1 and size the collider instead.",
        "rusting lint --json",
    ),
    code(
        "LINT_SILENT_GOAL",
        "A counter has a target, but nothing reacts when it is reached: no HUD element `requires` it or shows it as `{name}`, no pickup `requires` it, and game code never names it. The player reaches the goal and nothing shows or sounds.",
        "Show the result: a HUD element with `requires` set to the counter (a win screen), a pickup or door that `requires` it, or game code that checks `counter_complete`.",
        "rusting lint --json",
    ),
    code(
        "LINT_TEXT_CONTRAST",
        "HUD button text has a WCAG contrast ratio against the dark button fill below 4.5:1 (3:1 from 24 px), so it is hard to read. Plain HUD text is not judged: the scene behind it is only known from a rendered frame.",
        "Use light text on buttons (the default white gives about 11:1), or raise the color's alpha to 1.",
        "rusting lint --json",
    ),
    code(
        "LINT_TEXT_OFFSCREEN",
        "HUD text's anchored corner lands outside a 1280 x 720 view: its offset points away from the screen or is bigger than the screen, so the text is not seen.",
        "Offsets point from the anchor toward the screen centre; use a positive offset smaller than the view, or pick the anchor nearest where the text belongs.",
        "rusting lint --json",
    ),
    code(
        "LINT_TEXT_OVERFLOW",
        "HUD text, measured in the HUD's font at its font_size with counters at their starting values, runs past the edge of a 1280 x 720 view, so part of it is cut off.",
        "Shorten the text, split it with line breaks, lower font_size, or anchor it nearer the side it grows from (TopRight text grows left).",
        "rusting lint --json",
    ),
    code(
        "LINT_TEXT_SMALL",
        "HUD text has a font_size below 14 logical pixels, which is hard to read, more so on a TV or a high-DPI laptop.",
        "Raise font_size to 18 or more for body text (the default) and 14 at the least.",
        "rusting lint --json",
    ),
    code(
        "LINT_ZERO_SCALE",
        "An entity has a zero scale on some axis, so its mesh and collider vanish.",
        "Set every scale axis above zero; hide the entity with `visible: false` instead.",
        "rusting lint --json",
    ),
    code(
        "MODEL_IMPORT",
        "`scene add-model` could not import the glTF model.",
        "Check that the file is a valid .gltf or .glb and that its buffers and images sit next to it.",
        "rusting scene add-model scenes/main.rscene assets/models/barrel.glb --dry-run",
    ),
    code(
        "OUTSIDE_CONFINE",
        "--confine (or RUSTING_CONFINE) limits commands to one folder, and the working folder, an argument or a file write was outside it. Links and `..` are resolved first.",
        "Use paths inside the confined folder, or run without --confine and with RUSTING_CONFINE unset.",
        "rusting lint my_game --confine my_game --json",
    ),
    code(
        "PATCH_INVALID",
        "Every operation applied, but the patched scene fails validation, so nothing was written.",
        "Fix the operation that causes the problem in the message; use --dry-run to check before writing.",
        "rusting scene patch scenes/main.rscene patch.json --dry-run",
    ),
    code(
        "PATCH_JSON",
        "The patch file is not valid JSON or does not match the patch format.",
        "Read the patch format under scene_patch in `rusting schema --json` and fix the file. The example is a full patch with `create` operations.",
        crate::scene_patch::CREATE_EXAMPLE,
    ),
    code(
        "PATCH_OPERATION",
        "One patch operation could not be applied (unknown entity, bad path, or wrong value type); nothing was written.",
        "Query the scene for the entity's current form and fix the operation the message numbers. For `create`, built-in sections sit at the top level of the entity and `rusting.*` components under `components`; `rusting explain PATCH_JSON` shows a full example.",
        "rusting scene query scenes/main.rscene --name Player --json",
    ),
    code(
        "PATCH_UNVALIDATED_COMPONENT",
        "A patch writes a component the tools do not know; the game validates it when it loads.",
        "Check the component name and fields against the game's registration, then run `rusting check`.",
        "rusting check --json",
    ),
    code(
        "PRESET_SCOPE_UNKNOWN",
        "`preset apply --only` named a scope that does not exist.",
        "Use `lighting`, `camera` or `text`, comma separated.",
        "rusting preset apply scenes/main.rscene night --only lighting --dry-run",
    ),
    code(
        "PRESET_UNKNOWN",
        "No art-direction preset has the given name.",
        "List the presets and use one of their names.",
        "rusting preset list",
    ),
    code(
        "PROJECT_EXISTS",
        "The project folder already exists.",
        "Choose another name or parent folder.",
        "rusting new . my_game_2",
    ),
    code(
        "PROJECT_INVALID",
        "The project or binary name is not valid.",
        "Use only letters, numbers, spaces, - and _ in the project name.",
        "rusting new . coin_run",
    ),
    code(
        "PROJECT_IO",
        "Reading or writing a project file failed.",
        "Check that the folder exists and is writable.",
        "rusting project inspect --json",
    ),
    code(
        "PROJECT_MANIFEST_JSON",
        "project.json is not valid JSON or has missing fields.",
        "Fix the file; `rusting project inspect` reports the line.",
        "rusting project inspect --json",
    ),
    code(
        "PROJECT_MISSING_FILE",
        "The folder is not a project (no project.json), or a file the manifest names is missing.",
        "Run the command from the project folder, or pass the project root.",
        "rusting check path/to/my_game",
    ),
    code(
        "PROJECT_VERSION",
        "project.json has a format version this engine does not read.",
        "Use the engine version that made the project, or upgrade the manifest.",
        "rusting doctor",
    ),
    code(
        "READ_ONLY",
        "Read-only mode (--read-only or RUSTING_READ_ONLY) refused a command or a file write. Only commands that write no file run: inspection, lint, validate, docs, schema, diff, log, provenance, list commands, and `scene patch` or `fix` with --dry-run.",
        "Run the command without --read-only and with RUSTING_READ_ONLY unset, or use its --dry-run form to see the change.",
        "rusting lint --read-only --json",
    ),
    code(
        "RECIPE_NEEDS_CONTROLLER",
        "The recipe changes the player's controller, and `Player` has no `rusting.player_controller` or `rusting.platformer_controller` it can use.",
        "Give `Player` one of those controllers with a jump speed and gravity above 0, or start from a player template.",
        "rusting scene query scenes/main.rscene --name Player --json",
    ),
    code(
        "RECIPE_NEEDS_PLAYER",
        "The recipe places its objects from the object named `Player`, and the main scene has none.",
        "Name the player object `Player`, or start from a player template.",
        "rusting scene query scenes/main.rscene --name Player --json",
    ),
    code(
        "RECIPE_UNKNOWN",
        "No gameplay recipe has the given name.",
        "List the recipes and use one of their names.",
        "rusting recipe list",
    ),
    code(
        "RETARGET_FAILED",
        "`scene retarget` could not find an object or clip, or the clip is a blend space.",
        "Check both object names and the clip name; retarget the point clips of a blend space one by one.",
        "rusting scene retarget scenes/main.rscene Mixamo walk Knight --dry-run",
    ),
    code(
        "REVERT_CONFLICT",
        "A file the operation changed was changed again later, so reverting it would lose that later change. Nothing was written.",
        "Revert the later operation the message names first, or edit the file by hand.",
        "rusting revert 3fa2c1d9 --json",
    ),
    code(
        "RUST_BUILD_ERROR",
        "Game code failed to compile during a code reload; the running game keeps the old code.",
        "Fix the compiler error in the message; the game reloads on the next successful build.",
        "rusting check --json",
    ),
    code(
        "RUST_ENGINE_HINT",
        "A rustc error in game code matches a known engine pattern; the hint says how to write it.",
        "Apply the hint to the error it points at, then run `rusting check` again.",
        "rusting check --json",
    ),
    code(
        "SCENARIO_FAILED",
        "A scenario check failed; the message names the first failing tick, entity, and path.",
        "Inspect the state at that tick with a `log` step or `rusting run --ticks N`, then fix the game or the check.",
        "rusting test tests/win.json --json",
    ),
    code(
        "SCENARIO_TOO_WEAK",
        "A scenario still passed with `--without` components left out, so it does not prove the game depends on them.",
        "Add a check that fails when those components are missing, such as an `expect` on a value they drive.",
        "rusting test tests/win.json --without rusting.player_controller --json",
    ),
    code(
        "SCENE_COLLIDER_WITHOUT_BODY",
        "An entity has a collider but no `physics_body`, so physics, `raycast` and `aim` skip it.",
        "Add a `physics_body`: `Static` for a collider that never moves, `Cpu` for one that does. Player and platformer controllers need none.",
        "{\"operations\": [{\"op\": \"set\", \"id\": \"Monitor 1\", \"path\": \"/physics_body\", \"value\": {\"simulation\": \"Static\", \"solver\": \"Full\"}}]}",
    ),
    code(
        "SCENE_COMPONENT_FIELD",
        "A scene component has a field with the wrong name or type.",
        "Compare the component with its entry in `rusting schema --json`.",
        "rusting schema --json",
    ),
    code(
        "SCENE_CONFLICT",
        "The scene changed since the patch was built (revision or `expected` value mismatch); nothing was written.",
        "Query the scene again, rebuild the patch from the current values, and apply it.",
        "rusting scene inspect scenes/main.rscene --json",
    ),
    code(
        "SCENE_INVALID",
        "The scene failed validation.",
        "Fix the problem in the message; `rusting validate` lists every problem in the project.",
        "rusting validate --json",
    ),
    code(
        "SCENE_IO",
        "The scene file could not be read or written.",
        "Check the path; scene paths are relative to the current folder.",
        "rusting scene inspect scenes/main.rscene",
    ),
    code(
        "SCENE_JSON",
        "The scene file is not valid JSON or does not match the scene format.",
        "When the diagnostic carries a fix (a misspelled required field), run `rusting fix`. Otherwise go to the line and column it gives; edit scenes with `rusting scene patch` instead of by hand, or restore the file from version control.",
        "git checkout -- scenes/main.rscene",
    ),
    code(
        "SCENE_MERGE_CONFLICT",
        "`rusting merge` found a field (or a whole entity) that both sides changed differently; the merged scene keeps our value.",
        "Open the merged scene, set the field to the value you want, and finish the merge with `git add`.",
        "rusting merge base.rscene ours.rscene theirs.rscene --json",
    ),
    code(
        "SCENE_MISSING_ASSET",
        "The scene references an asset file that does not exist.",
        "Import the asset, or patch the reference to an existing path; paths are relative to the scene file.",
        "rusting asset list --json",
    ),
    code(
        "SCENE_STRUCTURE",
        "The scene has a duplicate ID, a duplicate unique name, a missing parent, or a parent cycle.",
        "Patch the entity the message names; `reparent` and `set` on /name fix most cases.",
        "{\"operations\": [{\"op\": \"reparent\", \"id\": \"Lamp\", \"parent\": null}]}",
    ),
    code(
        "SCENE_UNKNOWN_FIELD",
        "The scene file has a key the scene format does not have, so loading ignores it and the field keeps its default.",
        "Run `rusting fix` when the diagnostic carries a fix (a close misspelling); otherwise rename the key to one the message lists, checking `rusting schema --json`.",
        "rusting fix --dry-run",
    ),
    code(
        "SCENE_VERSION",
        "The scene has a format version this engine does not read.",
        "Open it with the engine version that wrote it, or upgrade the engine.",
        "rusting doctor",
    ),
    code(
        "SHADER_BUILD_ERROR",
        "A shader failed to compile during a reload; the old shader stays in use.",
        "Fix the shader error in the message.",
        "rusting check --json",
    ),
    code(
        "VULKAN_UNAVAILABLE",
        "No Vulkan device could render; captures fall back to CPU-only validation.",
        "Install a Vulkan driver, or Mesa lavapipe for a software device (see docs/dev-environment.md).",
        "rusting doctor",
    ),
];

#[must_use]
pub fn lookup(code: &str) -> Option<&'static CodeInfo> {
    CODES.iter().find(|info| info.code == code)
}

#[must_use]
pub fn to_json(info: &CodeInfo) -> Value {
    json!({
        "code": info.code,
        "summary": info.summary,
        "fix": info.fix,
        "example": info.example,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;

    /// Upper-case string literals that are not diagnostic codes.
    fn not_a_code(literal: &str) -> bool {
        ["RUSTING_", "RUST_LOG", "CARGO_", "XDG_"]
            .iter()
            .any(|prefix| {
                literal.starts_with(prefix) && literal != "RUST_BUILD_ERROR"
            })
            || ["GRID_PASS", "OVERSIZED_PASS", "D32_SFLOAT"].contains(&literal)
    }

    fn scan(folder: &Path, found: &mut BTreeSet<String>) {
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !path.ends_with("diagnostics.rs")
            {
                // A quote char or an escaped quote would shift which
                // pieces are inside string literals.
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("\\\"", "")
                    .replace("'\"'", "");
                for literal in text.split('"').skip(1).step_by(2) {
                    let shaped = literal.contains('_')
                        && literal
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_uppercase())
                        && literal.chars().all(|c| {
                            c.is_ascii_uppercase()
                                || c.is_ascii_digit()
                                || c == '_'
                        });
                    if shaped && !not_a_code(literal) {
                        found.insert(literal.to_owned());
                    }
                }
            }
        }
    }

    #[test]
    fn every_emitted_code_is_registered_and_every_registered_code_is_emitted() {
        let mut emitted = BTreeSet::new();
        scan(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut emitted,
        );
        let registered: BTreeSet<String> =
            CODES.iter().map(|info| info.code.to_owned()).collect();
        let missing: Vec<_> = emitted.difference(&registered).collect();
        let unused: Vec<_> = registered.difference(&emitted).collect();
        assert!(missing.is_empty(), "codes without an entry: {missing:?}");
        assert!(unused.is_empty(), "entries nothing emits: {unused:?}");
    }

    #[test]
    fn codes_are_sorted_unique_and_fully_documented() {
        for pair in CODES.windows(2) {
            assert!(
                pair[0].code < pair[1].code,
                "{} out of order",
                pair[1].code
            );
        }
        for info in CODES {
            assert!(
                !info.summary.is_empty()
                    && !info.fix.is_empty()
                    && !info.example.is_empty()
            );
        }
    }

    #[test]
    fn json_patch_examples_parse_as_patches() {
        for info in CODES.iter().filter(|info| info.example.starts_with('{')) {
            serde_json::from_str::<crate::scene_patch::ScenePatch>(
                info.example,
            )
            .unwrap_or_else(|error| panic!("{}: {error}", info.code));
        }
    }

    #[test]
    fn command_examples_name_a_real_command_and_its_flags() {
        let mut commands = 0;
        for info in CODES {
            for command in info.example.split("&&").map(str::trim) {
                if let Some(args) = command.strip_prefix("rusting ") {
                    commands += 1;
                    if let Some(problem) = crate::schema::command_problem(args)
                    {
                        panic!("{}: {problem}", info.code);
                    }
                }
            }
        }
        assert!(commands > 80, "only {commands} command examples");
    }
}
