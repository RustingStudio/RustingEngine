# Menus and UI

The runtime UI is [egui](https://docs.rs/egui/0.31) 0.31, drawn over the game
view. Game code gets the context with `scene.ui()` every frame and uses the
egui version the engine re-exports as `rusting_engine::egui`, so the game
needs no egui dependency of its own.

## A main menu

Keep the current screen in a counter, so scenarios can `set`, `expect` and
`log` it:

```rust
use rusting_engine::egui;
use rusting_engine::prelude::*;

const MAIN: i32 = 0;
const PLAYING: i32 = 1;
const PAUSED: i32 = 2;

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    let ui = scene.ui();
    match scene.counter_value("screen") {
        MAIN => {
            egui::CentralPanel::default().show(&ui, |ui| {
                if ui.button("New Game").clicked() {
                    scene.set_counter("screen", PLAYING);
                }
                if ui.button("Quit").clicked() {
                    scene.quit();
                }
            });
        }
        PLAYING if scene.pressed("pause") => {
            scene.set_paused(true);
            scene.set_counter("screen", PAUSED);
        }
        PAUSED => {
            egui::Window::new("Paused").show(&ui, |ui| {
                if ui.button("Resume").clicked() {
                    scene.set_paused(false);
                    scene.set_counter("screen", PLAYING);
                }
            });
        }
        _ => {}
    }
}
```

- `set_counter` and `add_to_counter` create a missing counter, so game
  state needs no scene object per value. `counter_value` on a counter that
  does not exist reads 0 and warns once, to catch typos; read state that
  is only written later with `scene.counter_or("slot_3_item", 0)`, which
  gives the default without a warning. `scene.counters()` lists them all,
  for a save file.
- `scene.set_paused(true)` stops fixed ticks: physics, tweens, player
  controllers and emitters. The update function keeps running every frame,
  so the pause menu still draws and reads input.
- `scene.quit()` closes the window after the frame. A headless run stops,
  and a scenario ends its run.
- The keyboard works with no extra code. Tab and Shift+Tab move the focus
  between widgets, and Enter or Space clicks the focused one.
- A gamepad works too. The d-pad or left stick focuses the first widget,
  then moves the focus. South (A on an Xbox pad) clicks, and East (B) is
  Escape.
- In a window, egui keeps the clicks and keys it uses, so they do not reach
  `pressed`. A headless run passes every input to both egui and the game.
- `rusting.hud` entities with `button: true` are egui buttons too, and work
  the same way. They take the focus in reading order, top to bottom and
  then left to right, by their anchored point. `scene.clicked()` names the HUD buttons clicked since the
  last frame; `rusting recipe apply <root> pause_menu` writes a pause menu
  built from them.
- egui paints its layers in order: `Order::Background`, `Middle`,
  `Foreground`, `Tooltip`, `Debug`, all over the game view.
  `CentralPanel` and `layer_painter(LayerId::background())` share the one
  background layer, and a `CentralPanel` fills it with the opaque
  `panel_fill` unless you give it `Frame::NONE` or your own fill. Shapes
  painted on that layer before such a panel are hidden under it. For a
  full-screen shade, use a `CentralPanel` with
  `Frame::default().fill(color)`, or paint on a `Foreground` layer.
- egui blends in sRGB space, as its own backends do, but its color
  constructors premultiply in linear light. `Color32::from_white_alpha(8)`
  and `from_rgba_unmultiplied(255, 255, 255, 8)` are both grey 50, a clear
  band over black. For a faint overlay (scanlines, grain) write the
  premultiplied value yourself: `Color32::from_rgba_premultiplied(8, 8, 8, 8)`
  adds 8. `from_black_alpha(128)` darkens the view to half its sRGB value,
  in the window and in scenario screenshots alike.

## Settings, saves and key rebinding

- `scene.save_data(key, text)` writes a file to the game's user data folder,
  and `scene.load_data(key)` reads it back. The key is a relative path, such
  as `settings.json` or `saves/1.json`.
- The folder is `~/.local/share/<game>` on Linux,
  `~/Library/Application Support/<game>` on macOS and `%APPDATA%\<game>` on
  Windows. The `RUSTING_USER_DATA` variable overrides it, and so do
  platform services with their own save folder (below).
- Store values with `rusting_engine::serde_json`.
- `scene.counters()` lists every counter's name and value, so a save can
  hold them all.
- To rebind an action, read `scene.keys_pressed()` while a "press a key"
  prompt shows, then call `scene.rebind_device("serve", &[key])`. It
  replaces only the inputs on the key's device, so a new keyboard key
  keeps the gamepad button. Gamepad buttons appear there too, as
  `PadSouth` and so on. `scene.rebind` replaces every input.
- A rebinding is not saved. Save it yourself, and call `rebind` again when
  the game starts.
- For UI that game code paints itself, `scene.cursor()` gives the cursor in
  pixels and `scene.viewport_size()` gives the view size.

## Achievements, stats and storefronts

```rust
use rusting_engine::platform;

platform::unlock_achievement("night_5"); // unlocking again is fine
platform::set_stat("nights_survived", 5);
if platform::achievement_unlocked("night_5") { /* show the badge */ }
```

- By default the calls keep achievements and stats in `platform.json` in
  the user data folder, so a game with no storefront still remembers them.
- A storefront plugs in by implementing `platform::PlatformServices` over
  its SDK in the game crate (unlock, query, set and read stats, and an
  optional own save folder such as a cloud-synced one). Install it once at
  start with `platform::set_platform_services(Box::new(MyStore::new()))`;
  game code keeps calling the same functions.
- Store content and the workshop go through the same services:
  `platform::owns_content("soundtrack")` says whether the player owns a DLC
  (always true with no storefront), `platform::open_store_page(None)` opens
  the game's store page (or `Some(id)` a DLC's), and
  `platform::workshop_items()` lists the subscribed workshop mods a
  storefront downloaded, which the game loads with its `mods/` folder
  (see [Modding](modding.md)). `platform::publish_workshop_item(archive,
  &manifest)` uploads a mod packed by `rusting mod pack` and returns its
  item ID. Each has a default in the trait, so a store implements only
  what it has.
- The engine ships no storefront SDK yet: Steamworks waits on the owner's
  approval of the `steamworks` crate (see `rusting_engine::steam`).
- Achievements and stats are presentation, like audio. Never let them
  change simulation state, or replays stop matching.

## Video settings

```rust
scene.set_render_scale(0.75); // 3D at 75% of the window, stretched to fit
scene.set_vsync(true);
scene.set_max_fps(Some(60)); // None removes the cap
scene.set_fullscreen(true); // borderless, on the current monitor
scene.set_window_size([1280, 720]);
scene.set_window_title("Rusting Raft"); // or "window_title" in project.json
scene.set_exposure(1.8); // brighter, e.g. a CCTV feed; 1 is neutral
```

- The render scale goes from 0.25 to 2.0. Below 1 the game runs faster and
  looks softer; above 1 it supersamples. UI always draws at full resolution.
  Captures and scenario screenshots use the scale too.
- Fullscreen and window size apply after the frame. The platform can pick
  another window size; read `scene.viewport_size()` on a later frame.
  Headless runs ignore both.
- `set_exposure` writes the scene's `rusting.tone_mapping` exposure and adds
  one when the scene has none.
- None of these are saved. Store them with `save_data` and set them again
  at startup.

## Theme

`assets/ui/theme.json` restyles HUD buttons and the menus game code draws
with `scene.ui()`, the way a Godot theme resource does. Every field is
optional; one left out keeps the default dark look. Colours are RGBA from
0 to 1.

```json
{
  "button_fill": [0.85, 0.75, 0.45, 1.0],
  "button_hover_fill": [0.95, 0.85, 0.55, 1.0],
  "button_pressed_fill": [0.7, 0.6, 0.3, 1.0],
  "panel_fill": [0.08, 0.06, 0.05, 0.95],
  "text_color": [0.1, 0.08, 0.05, 1.0],
  "corner_radius": 6,
  "button_padding": [12, 6]
}
```

- The game reads the file when it starts. A misspelt field or a bad value
  stops the start with the file and the field named.
- A HUD element's `color` still sets its own text colour; `text_color`
  colours menu widgets.
- `rusting lint` judges HUD button text contrast against the theme's
  `button_fill`, so light buttons need dark text.

A HUD element draws in its own font when its `font` names a TTF or OTF
file under `assets/`. `rusting asset generate . font pixel --to fonts`
makes a placeholder pixel font (`pixel` or `bold`) in the chunky style
of the other generated placeholders:

```json
"rusting.hud": {"text": "Score {coins}", "font": "fonts/pixel.ttf", "font_size": 24.0}
```

Characters the font lacks come from the built-in font. A missing or
unreadable file warns once and the text keeps the built-in font. A new
font shows from the second frame, so a capture at tick 0 still shows the
built-in one.

## Scaling the HUD to the window

By default the HUD is drawn at the desktop's DPI scale, so a bigger window
shows more empty screen around the same-sized text. Put
`"ui_base_size": [1280, 720]` in `project.json` to lay the HUD out for a
1280x720 screen instead: it then scales uniformly to fit any window, like
Godot's `canvas_items` stretch mode. A 2560x1440 window draws it at 2x; a
wider window scales by its height, so anchored elements keep to the edges
and nothing stretches. Headless runs scale from the viewport size the same
way, and scenario clicks and `find_text` positions stay in window pixels.

## Testing menus

Scenarios drive egui the same way a player does:

```json
{"name": "menu", "ticks": 120, "capture_size": [640, 360],
 "files": {"saves/1.json": "fixtures/old_save.json"},
 "steps": [
   {"tick": 2, "click": "New Game"},
   {"tick": 4, "expect": {"counter": "screen", "equals": 1}},
   {"tick": 5, "tap": "pause"},
   {"tick": 7, "capture": "shots/paused.png"},
   {"tick": 8, "click": "Resume"},
   {"tick": 20, "restart": true},
   {"tick": 30, "click": "Quit"},
   {"tick": 31, "expect_quit": true}
 ]}
```

- `click` finds the topmost text the last frame drew with that label. It
  moves the cursor there, presses the left mouse button and releases it
  before the next tick, so egui sees one click. It works for egui buttons,
  for HUD buttons, and for text that game code painted. When nothing
  matches, the failure lists the texts on screen.
- `pointer` with `[x, y]` view fractions, plus a press of an action bound to
  `MouseLeft`, does the same thing by position.
- `tap` presses a key action for one tick. egui sees it too, so Tab and
  Enter navigate menus.
- The headless screen is `capture_size` from tick 0, for layout, egui and
  `viewport_size()` alike. Run the same scenario at 640×360 and 480×270 to
  check that small windows fit.
- Inputs on a tick run before the checks on that tick. Put an `expect` one
  tick or more after the `click` it depends on. A failed check names the
  inputs that ran on its tick.
- Each scenario gets an empty user data folder,
  `build/test-userdata/<scenario>/`. `files` copies fixtures into that folder
  before tick 0.
- `restart` reloads the starting scene. Files stay in place, so "save,
  restart, load" needs no hooks in game code.
- `expect_quit` checks that the game called `scene.quit()`. The run ends
  after the tick that quits, and steps at later ticks fail.

- Gamepad presses go through actions: bind `PadSouth` or `PadDpadDown` in a
  `rusting.input_action`, then `tap` the action. `left_stick` and
  `right_stick` steps tilt a stick.

## Text on meshes

Signs, monitor labels and notes on a desk are text that sits in the 3D world
and tilts, lights and fogs with it. From game code, `set_text` draws text
onto an object that already has a mesh, as its base color map:

```rust
use rusting_engine::text_texture::TextStyle;

let style = TextStyle { size: 48.0, monospace: true, ..TextStyle::default() };
let [w, h] = scene.set_text("Clock", &format!("{hour:02}:{minute:02}"), style).unwrap();
// Scale the mesh to w / h so the letters keep their shape.
```

Each distinct string is drawn once and kept, so a clock costs one texture
per string it shows. Give the object an Unlit material for glowing text.
The built-in fonts are egui's. For handwriting or a themed font, put a TTF
or OTF file under `assets/` and call
`scene.set_text_in_font("Tag", "aisle C", style, "fonts/pencil.ttf")`;
glyphs the font lacks fall back to the built-in ones.
`set_material` puts any material from `create_material` on an object, and
`create_texture` registers a texture for one.

For more control, `rusting_engine::text_texture::text_texture`
draws a string into an ordinary texture on the CPU with egui's built-in
fonts (the `ui` feature, on by default). Use the texture as a material's base
color on any mesh, usually a thin box or a plane:

```rust
use rusting_engine::assets::{AssetServer, MaterialAsset, MaterialModel};
use rusting_engine::text_texture::{text_texture, TextStyle};

fn make_sign(assets: &mut AssetServer) {
    let texture = assets.textures.insert(text_texture(
        "CAM 3  REC",
        TextStyle {
            size: 48.0,
            color: [255, 240, 200, 255],
            background: [20, 30, 60, 255],
            ..TextStyle::default()
        },
    ));
    let material = assets.materials.insert(MaterialAsset {
        model: MaterialModel::Unlit,
        base_color_texture: Some(texture),
        ..Default::default()
    });
    // Put `material` on a mesh scaled to the texture's aspect ratio.
}
```

- `size` is the glyph height in texels; `padding` adds empty texels around
  the text; `monospace` picks egui's monospace font. Lines split on `\n` and
  never wrap.
- The texture is exactly as large as the text, so scale the mesh to
  `texture.size[0] / texture.size[1]` to keep letters undistorted.
- To change the text, overwrite the texture through
  `assets.textures.get_mut(handle)`. That bumps its revision, and the renderer
  uploads it again on the next frame. Do this when the text changes, not every
  frame.
- Use `MaterialModel::Unlit` for a glowing screen or sign, and the default
  PBR model for printed paper that should darken in shadow.
- A camera screen with a CRT `grading` (see
  [Cameras](cameras.md#keeping-screens-cheap)) only grades its feed. For an
  overlay such as a timestamp on a monitor, put a text panel just in front of
  the screen mesh.
- There is no separate world-space text component: this helper covers it.
  Rich egui widgets on a mesh (buttons you can click in 3D) are not supported.

The GPU test `text_reads_on_a_tilted_panel` renders this texture on a panel
turned away from the camera and compares it with
`src/rendering/golden/text_panel_tilted.png`.
