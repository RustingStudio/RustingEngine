# Input

Game code reads **actions**, such as `jump` or `throw`, not keys. A scene or
the game binds each action to keys, mouse buttons and gamepad buttons. Any
bound input triggers the action. Scenario tests press actions by name, so
a game that reads actions can be tested without a keyboard.

## Bind actions in the scene

A `rusting.input_action` component binds one action. Put it on any object,
for example an empty "Controls" object:

```json
{"rusting.input_action": {"action": "throw", "inputs": ["Space", "MouseLeft", "PadSouth"]}}
```

Input names:

- Keys use winit names: `KeyA` to `KeyZ`, `Digit0` to `Digit9`, `Space`,
  `Enter`, `Escape`, `Tab`, `ShiftLeft`, `ControlLeft`, `ArrowUp`, `F1`.
- Mouse buttons: `MouseLeft`, `MouseRight`, `MouseMiddle`.
- Gamepad buttons, in the layout of an Xbox pad: `PadSouth` (A), `PadEast`
  (B), `PadWest` (X), `PadNorth` (Y), `PadLeftBumper`, `PadRightBumper`,
  `PadLeftTrigger`, `PadRightTrigger`, `PadSelect`, `PadStart`,
  `PadLeftStick` and `PadRightStick` (stick clicks), and `PadDpadUp`,
  `PadDpadDown`, `PadDpadLeft`, `PadDpadRight`.
- Stick directions act as buttons that press past half tilt:
  `PadLeftStickUp`, `PadLeftStickDown`, `PadLeftStickLeft`,
  `PadLeftStickRight`, and the same for `PadRightStick`.

An unknown name fails when the scene loads, and the error lists the valid
forms. Every connected gamepad feeds the same state.

The engine binds the player controller's actions itself:

| Action | Keyboard | Gamepad |
| --- | --- | --- |
| `player.forward`, `player.back`, `player.left`, `player.right` | WASD and the arrow keys | left stick and D-pad |
| `player.jump` | Space | `PadSouth` |
| `player.sprint` | Shift | `PadLeftStick` |
| `player.crouch` | C or left Ctrl | `PadEast` |

## Read actions in code

| Call | True or returns |
| --- | --- |
| `scene.pressed("jump")` | on the frame (or tick) the action went down |
| `scene.held("jump")` | while any bound input is down |
| `scene.press_tick("jump")` | when it was pressed, in fractional fixed ticks (`121.4`) |
| `scene.stick(Stick::Left)` | the stick's tilt, each axis -1..1, `[0, 1]` fully up |
| `scene.cursor()` | the cursor in pixels from the top-left of the view |
| `scene.pointer_ray()` | the world ray under the cursor, for aiming and picking |
| `scene.clicked()` | the HUD buttons clicked since the last frame |
| `scene.window_focused()` | false while the window has lost focus |

```rust
use rusting_engine::prelude::*;

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    if scene.pressed("throw") {
        // one throw per press, not one per frame while held
    }
    let [x, y] = scene.stick(Stick::Left);
    if x.abs() > 0.2 || y.abs() > 0.2 {
        // analog movement
    }
}

rusting_game!(update);
```

### Frames and ticks

`update` runs once per rendered frame, and a frame can run no fixed tick
or several. `pressed` in `update` is true for one frame. Game code that
changes the simulation should read input in a tick function instead:

```rust
fn tick(scene: &mut GameScene<'_>, _time: &FrameTime) {
    if scene.pressed("jump") {
        // runs on the tick the key went down, even when one frame ran
        // several ticks or a frame ran none
    }
}

rusting_game!(update, tick: tick);
```

Inside `tick`, `pressed` is true on the tick the press happened in, by the
time the window recorded for the key event. `press_tick` gives that time
to a fraction of a tick, so a rhythm game can judge timing finer than a
tick.

### When the window loses focus

Alt-tab releases every held key and button, and each reports as just
released, so a held key does not stick. `scene.window_focused()` turns
false; pause the game there if it should not run unattended.

## Let players rebind

`scene.keys_pressed()` names the keys and gamepad buttons pressed this
frame, for a "press a key" prompt. Pass one to `rebind`:

```rust
if let Some(key) = scene.keys_pressed().first().cloned() {
    let _ = scene.rebind_device("jump", &[key.as_str()]);
}
```

- `scene.rebind(action, inputs)` binds the action to exactly `inputs`.
- `scene.rebind_device(action, inputs)` replaces only the inputs on the
  same device: rebinding `jump` from `Space` to `KeyJ` keeps `PadSouth`.
- `scene.binding(action)` returns the current inputs, for a settings menu.

Both edit the scene's `rusting.input_action`, so an unknown name returns
an error and changes nothing. Rebinding is not saved: store the inputs
with `scene.save_data` and rebind again at start.

## Raw input and bindings in Rust

For what actions do not cover, the `RuntimeInput` resource holds this
frame's raw state: `key_held`, `key_just_pressed`, `mouse_held`,
`mouse_motion` (raw movement, which keeps counting while the cursor is
captured), `pad_held` and `stick`. `set_cursor_captured(true)` hides and
locks the cursor for mouse look; the player controller does this on a left
click and releases it on Escape.

```rust
use rusting_engine::runtime::RuntimeInput;

let motion = scene.world().resource::<RuntimeInput>().mouse_motion();
```

The `ActionMap` resource holds every binding. `bind(action, binding)` and
`unbind(action, binding)` change it from Rust. Bindings made in code stay
when the scene's `rusting.input_action` components change.

A click on a rendered object sends a `ClickEvent` with the entity, the
button and the world point hit; read it in a system with `Res<EventQueue<ClickEvent>>`.

## Test input without a keyboard

Scenario steps press actions by name:

| Step | Does |
| --- | --- |
| `{"tick": 5, "press": "player.jump"}` | presses every input bound to the action |
| `{"tick": 6, "release": "player.jump"}` | releases them |
| `{"tick": 5, "tap": "pause"}` | presses and releases before the next tick |
| `{"tick": 10, "pointer": [0.5, 0.5]}` | moves the cursor, as fractions of the view |
| `{"tick": 2, "click": "New Game"}` | clicks the text with that label |

`"at": 0.4` on a `press` or `tap` lands it 40% into the tick, so
`press_tick` returns `5.4`. Run them with `rusting test <project>
<scenario>`; `rusting docs show reference/scenario` lists every step.
