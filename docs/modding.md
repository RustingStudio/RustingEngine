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

`rusting mod pack better_trees --out better_trees.rmod` checks `mod.json`
and packs the folder into one file. The unpacked folder works too, which is
handy while making the mod.

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

## Conflicts

Two mods that hold the same file conflict: only the later one's file is
read. `rusting mod list` reports each such file under `conflicts`, with the
mods that hold it in load order, and the game prints
`mods: FILE is in A, B; B wins` as it starts. For a `.rscene` file the
report also names each field two mods changed from the game's scene in
different ways: the entity's ID and name, and the field, such as
`transform` or `components/rusting.point_light/intensity`. Two mods that
change different fields of one component, or make the same change, do not
conflict there, but the later mod's scene still replaces the earlier one's
whole, so a mod author who wants both changes merges the scenes into one
mod or makes one mod depend on the other.

## In game code

`rusting_engine::mods::load_mods` reads a mods folder and
`rusting_engine::mods::mount` mounts a list of mods over a game folder, for
a game with its own mod menu; `rusting_engine::mods::conflicts` reports
what they fight over. Everything that reads through
`rusting_engine::assets::read_file`, which is every scene, texture, model
and sound the engine loads, sees the mounted mods.
