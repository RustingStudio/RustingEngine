# Modding

Players can change a shipped game by putting mods in a `mods/` folder next
to the game. A mod can replace any file the game reads from `assets/` or
`scenes/`, such as a texture, a sound, a model or a level, and can add new
ones. Mods cannot run code yet; scripted mods come with WASM scripting.

## Making a mod

A mod is a folder with a `mod.json` and the same `assets/` and `scenes/`
layout as the game:

```text
better_trees/
  mod.json
  assets/models/tree.glb
  scenes/level_2.rscene
```

```json
{
  "format_version": 1,
  "name": "better_trees",
  "version": "1.0.0",
  "description": "Taller trees with more leaves.",
  "depends": { "hd_textures": "1.2" }
}
```

- `name` is letters, digits, `_`, `-` and `.`, and unique among the game's
  mods. Other mods depend on it by this name.
- `version` is `major.minor.patch`.
- `depends` maps a mod name to the lowest version this mod needs. `"1.2"`
  accepts any version from 1.2.0 up to, but not including, 2.0.0. For a
  `0.x` version the minor number must match too: `"0.2"` accepts 0.2.0 up
  to 0.3.0.

`rusting mod pack better_trees` checks `mod.json` and packs the folder into
one `better_trees.rmod` file; `--out FILE` picks another name. The
unpacked folder works too, which is handy while making the mod.

## Installing and ordering mods

Copy the `.rmod` file or the mod folder into the game's `mods/` folder:
beside `build/` in a project, or beside the executable in an exported game.
The game mounts every mod as it starts and prints `mods: loaded NAME
VERSION` for each one on stderr.

A file a mod holds is read in place of the game's own. When two mods hold
the same file, the one later in the load order wins. The load order is:

- the order in `mods/load_order.json`, a list of mod names, when that file
  exists. Only the mods it names load, so it also turns mods off;
- otherwise every mod, by name;
- and in either case every mod comes after the mods it depends on.

A mod does not load when its `mod.json` is wrong (`MOD_MANIFEST_INVALID`),
when a mod it needs is missing, turned off or at a version it does not
accept (`MOD_DEPENDENCY_UNMET`, which also leaves out every mod that
depends on it), or when it is in a dependency loop (`MOD_DEPENDENCY_CYCLE`).
The game still starts without it.

`rusting mod list` shows what a game would load, in order, with an error for
each mod left out:

```sh
rusting mod list my_game --json
```

## In game code

`rusting_engine::mods::load_mods` reads a mods folder and
`rusting_engine::mods::mount` mounts a list of mods over a game folder, for
a game with its own mod menu. Everything that reads through
`rusting_engine::assets::read_file`, which is every scene, texture, model
and sound the engine loads, sees the mounted mods.
