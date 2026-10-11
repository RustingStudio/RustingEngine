# Assets: import and export

A project keeps its files under `assets/`: images, glTF models, sounds and
data assets. Import copies a file into `assets/`, checks that the engine
can load it, and records where it came from. Export builds a release and
packages the game with its assets into one folder that runs without the
engine checkout.

For typed data files (`.rdata`), see "Data assets" in
[Core concepts](concepts.md). For where to find free models and
textures, see [Look and feel](look-and-feel.md).

## Import a file

```bash
rusting asset import my_game ~/Downloads/crate.png --to props --license CC0-1.0 --author "Kenney"
```

```text
Asset: assets/props/crate.png (5cd9ac84-b54f-4ed7-88ad-c8e73cb76421, license CC0-1.0)
```

| Kind | Files |
| --- | --- |
| Image | `.png`, `.jpg`, `.jpeg`, `.bmp`, `.tga` |
| Model | `.gltf` (with its external buffers and images), `.glb` |
| Sound | `.wav`, `.ogg` |
| Font | `.ttf`, `.otf` |

Import loads the file as the game would, then copies it to
`assets/<folder>` (`--to props` and `--to assets/props` mean the same
folder). Any other type fails with `ASSET_UNSUPPORTED`, a file the engine
cannot read fails with `ASSET_INVALID`, and a file that is already there
fails with `ASSET_EXISTS`: replace it with `asset reimport` instead.

Flags:

- `--author`, `--license`, `--url`, `--generator` and `--notes` record
  where the file came from. Use an SPDX name for the license, such as
  `CC0-1.0` or `CC-BY-4.0`. Without a license, import warns
  `ASSET_NO_LICENSE`.
- `--max-size 512` scales an image down so its longest edge is 512 pixels,
  keeping its shape.
- `--dry-run` runs every check on a staged copy and writes nothing.

### The `.rmeta` file

Import writes `<file>.rmeta` next to the asset:

```json
{
  "format_version": 1,
  "id": "5cd9ac84-b54f-4ed7-88ad-c8e73cb76421",
  "kind": "image",
  "settings": {"max_size": null},
  "dependencies": [],
  "content_hash": "a5c162ecbe3adeea",
  "source": {
    "original": "crate.png",
    "author": "Kenney",
    "license": "CC0-1.0",
    "url": null,
    "generator": null,
    "notes": null
  }
}
```

- `id` is stable. It survives a reimport or a replacement, so tools can
  follow an asset across edits. Scenes still refer to the file by path.
- `dependencies` lists the files a glTF loads, such as its `.bin` buffers
  and textures.
- `content_hash` spots edits made to the file outside import.
- `source` is the license record. Keep the `.rmeta` with the asset in
  version control.

## Use an asset in a scene

Scenes refer to a file by its path relative to the scene file. The
`reference` field of `asset import --json` gives that path. A texture goes
into a material slot:

```json
{
  "operations": [
    {"op": "set", "name": "Cube", "path": "/mesh_renderer/material/Inline/base_color_texture", "value": "../assets/textures/bricks_3.png"}
  ]
}
```

The other slots are `normal_texture`, `metallic_roughness_texture`,
`occlusion_texture` and `emissive_texture`; `uv_scale` repeats the
texture across a face. Run `rusting docs show component/mesh_renderer`
for every field.

A model goes in as one object with a child for each glTF node:

```bash
rusting scene add-model my_game/scenes/main.rscene my_game/assets/models/barrel.glb --name "Barrel 1"
```

`add-model` keeps the model's materials, textures, animations and skin.
It writes the converted meshes (`.rmesh`) and textures (`.rtexture`) next
to the model; keep them with the project. Run it again with another
`--name` for each copy, or `spawn_copy` the object in code.

Sounds are the exception: `rusting.sound_cue.clip` and `play_sound` take a
path relative to `assets/`, such as `sounds/hit.wav`. See
[Audio](audio.md).

## Replace an asset

```bash
rusting asset reimport my_game assets/props/crate.png --from art/crate_v2.png
```

Reimport replaces the file and keeps its ID. The asset is named by its
path or its ID. The new file comes from `--from`, else from the recorded
original if it still exists; otherwise reimport only checks the asset in
place. It takes the same provenance flags and `--max-size` as import, and
each given flag replaces the recorded value. A glTF also cooks its
`.rmesh` files again.

Reimport reports the scenes that use the asset. A running game or the
editor reloads the file, so the change shows without a restart.

A file put under `assets/` by hand, or by `scene add-model`, has no
`.rmeta`. Reimport it to give it an ID and register it.

## List and check assets

```bash
rusting asset list my_game
```

```text
Asset: assets/models/barrel_4.glb (e01073d3-883d-4f5a-a711-8836b88e0aeb, license CC0-1.0)
Asset: assets/props/crate.png (5cd9ac84-b54f-4ed7-88ad-c8e73cb76421, license none)
warning [ASSET_NO_LICENSE]: no license recorded (.../assets/props/crate.png)
```

With `--json`, each asset also lists its dependencies, settings,
provenance and the scenes that refer to it (`referenced_by`). The command
fails on a missing file or dependency and on invalid or duplicate
metadata. It warns on a file changed since import, a file without
`.rmeta`, and an asset without a license. Run it before you ship.

`rusting validate` checks that every asset a scene refers to exists.

## Generate assets

`asset generate` makes a file from a prompt and imports it. Five
generators are built in, in one shared palette and chunky style. Each
gives the same CC0 file for the same prompt, and the optional number
picks a variation:

| Hook | Prompt | Makes |
| --- | --- | --- |
| `sfx` | `jump`, `coin`, `hit`, `explosion`, `laser`, `powerup` or `blip`, then a seed: `coin 7` | a WAV |
| `texture` | `grid`, `checker`, `bricks`, `planks`, `tiles` or `noise`: `bricks 3` | a 256 px tileable PNG |
| `sprite` | `circle`, `square`, `triangle`, `diamond`, `star` or `heart`: `star 2` | a 128 px outlined PNG on a clear background |
| `mesh` | `crate`, `barrel`, `rock`, `tree` or `gem`: `barrel 4` | a flat-shaded low-poly `.glb` standing on its origin |
| `font` | `pixel` or `bold`, with no number | a TrueType pixel font of printable ASCII, for `rusting.hud` `font` and `set_text_in_font` |

```bash
rusting asset generate my_game texture "bricks 3" --to textures
```

```text
Asset: assets/textures/bricks_3.png (2e0d5d19-64f4-48e8-9a46-3872c71afc82, license CC0-1.0)
```

`--replace <asset>` reimports an existing asset from the output instead,
keeping its ID.

### Your own generator

A project can add hooks that run any tool or model service. List them in
`project.json`:

```json
"generators": {
  "icons": {"command": ["python3", "tools/make_icon.py"], "license": "CC0-1.0", "settings": {"max_size": 256}}
}
```

The engine runs the command with the prompt in `RUSTING_PROMPT` and an
empty folder in `RUSTING_OUTPUT_DIR`. The command writes one file there
and prints, as its last line, JSON naming it:

```json
{"file": "icon.png", "license": "CC0-1.0", "author": "...", "generator": "my-model v2"}
```

Only `file` is required. The file then goes through `asset import`, so
its type, size and metadata are checked like any other file. Output with
no license, from the hook or its config, is refused. A hook named `sfx`,
`texture`, `sprite`, `mesh` or `font` replaces the built-in one.

## Export the game

```bash
rusting export my_game ~/exports
```

```text
Exported: /home/you/exports/my_game_export
Game exit code: 0, timed out: false
```

Export validates and cooks the main scene, builds a release, and copies
into a new folder:

- the executable;
- the cooked main scene (`build/`);
- `project.json`, `assets/` and the source scenes in `scenes/`;
- the engine license, as `RUSTING_ENGINE_LICENSE.md`;
- a `README.txt` that says where settings and saves live.

It then copies the folder to a fresh temporary folder and runs the game
there for 60 headless ticks. A game that reads a file from outside its
folder fails this check with `EXPORT_VERIFY_FAILED`. An existing export
is never replaced; the next one is named `my_game_export_2`.

`--target x86_64-pc-windows-gnu` builds for another system, with that
Rust target installed. The folder is named after the system, such as
`my_game_windows_export`. A cross-target export cannot run here, so it is
not checked and export warns `EXPORT_NOT_VERIFIED`.

Before you ship, run `rusting asset list` and add the credits that
`CC-BY` assets need to your README.
