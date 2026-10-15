//! Runtime-side input capture and named action mapping.
//!
//! `RuntimeInput` holds raw keyboard and mouse state for gameplay systems.
//! Window integrations record raw events into it and clear edge state once per
//! rendered frame after gameplay systems have had a chance to read it.

use std::collections::{BTreeSet, HashMap};

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
pub use winit::event::MouseButton;
pub use winit::keyboard::KeyCode;

/// Fired the frame a mouse button is pressed over a renderable entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClickEvent {
    pub entity: Entity,
    pub button: MouseButton,
    pub world_position: [f32; 3],
}

/// Held keys, mouse buttons, and cursor position for the current frame.
///
/// `just_pressed`/`just_released` sets are edge-triggered: they hold exactly
/// the keys or buttons that changed state since the last call to
/// [`RuntimeInput::clear_frame_edges`]. Runtime integrations call that method
/// once per rendered frame after gameplay systems have had a chance to read
/// them.
///
/// It serializes whole, so replays can record what each frame's systems saw.
#[derive(
    Resource, Clone, Debug, Default, PartialEq, Serialize, Deserialize,
)]
pub struct RuntimeInput {
    keys_held: BTreeSet<KeyCode>,
    keys_just_pressed: BTreeSet<KeyCode>,
    keys_just_released: BTreeSet<KeyCode>,
    mouse_held: BTreeSet<MouseButton>,
    mouse_just_pressed: BTreeSet<MouseButton>,
    mouse_just_released: BTreeSet<MouseButton>,
    cursor_position: Option<[f32; 2]>,
    viewport_size: [f32; 2],
    mouse_motion: [f32; 2],
    cursor_captured: bool,
    #[serde(default)]
    pad_held: BTreeSet<PadButton>,
    #[serde(default)]
    pad_just_pressed: BTreeSet<PadButton>,
    #[serde(default)]
    pad_just_released: BTreeSet<PadButton>,
    #[serde(default)]
    sticks: [[f32; 2]; 2],
    /// When this frame's presses happened, in fractional fixed ticks.
    #[serde(default)]
    press_ticks: Vec<(InputBinding, f64)>,
    /// The window lost focus (alt-tab) and has not got it back.
    #[serde(default)]
    unfocused: bool,
    /// Mouse wheel turned this frame, in lines.
    #[serde(default)]
    scroll: [f32; 2],
}

/// A gamepad button, in the layout of an Xbox pad: `South` is A, `East` B.
/// Every connected pad feeds the same state. Each stick also acts as four
/// buttons (`LeftStickUp`, ...) that press past half tilt, so menus and
/// digital actions can bind a stick.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
pub enum PadButton {
    South,
    East,
    West,
    North,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    LeftStick,
    RightStick,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
    LeftStickUp,
    LeftStickDown,
    LeftStickLeft,
    LeftStickRight,
    RightStickUp,
    RightStickDown,
    RightStickLeft,
    RightStickRight,
}

/// Which stick [`RuntimeInput::stick`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stick {
    Left,
    Right,
}

fn record_edge<T: Ord + Copy>(
    held: &mut BTreeSet<T>,
    pressed_now: &mut BTreeSet<T>,
    released_now: &mut BTreeSet<T>,
    item: T,
    pressed: bool,
) {
    if pressed {
        if held.insert(item) {
            pressed_now.insert(item);
        }
    } else if held.remove(&item) {
        released_now.insert(item);
    }
}

impl RuntimeInput {
    #[must_use]
    pub fn key_held(&self, key: KeyCode) -> bool {
        self.keys_held.contains(&key)
    }
    #[must_use]
    pub fn key_just_pressed(&self, key: KeyCode) -> bool {
        self.keys_just_pressed.contains(&key)
    }
    #[must_use]
    pub fn key_just_released(&self, key: KeyCode) -> bool {
        self.keys_just_released.contains(&key)
    }
    /// Keys pressed since the last [`Self::clear_frame_edges`], for a
    /// "press any key" prompt.
    pub fn keys_pressed(&self) -> impl Iterator<Item = KeyCode> + '_ {
        self.keys_just_pressed.iter().copied()
    }
    /// Keys released since the last [`Self::clear_frame_edges`].
    pub fn keys_released(&self) -> impl Iterator<Item = KeyCode> + '_ {
        self.keys_just_released.iter().copied()
    }
    #[must_use]
    pub fn mouse_held(&self, button: MouseButton) -> bool {
        self.mouse_held.contains(&button)
    }
    #[must_use]
    pub fn mouse_just_pressed(&self, button: MouseButton) -> bool {
        self.mouse_just_pressed.contains(&button)
    }
    #[must_use]
    pub fn mouse_just_released(&self, button: MouseButton) -> bool {
        self.mouse_just_released.contains(&button)
    }
    #[must_use]
    pub fn cursor_position(&self) -> Option<[f32; 2]> {
        self.cursor_position
    }

    /// Raw mouse movement since the last [`Self::clear_frame_edges`]. It keeps
    /// counting while the cursor is captured and cannot move.
    #[must_use]
    pub fn mouse_motion(&self) -> [f32; 2] {
        self.mouse_motion
    }

    /// Mouse wheel turned since the last [`Self::clear_frame_edges`], in
    /// lines: positive `y` turns the wheel away from the player, which
    /// shows content above.
    #[must_use]
    pub fn scroll(&self) -> [f32; 2] {
        self.scroll
    }

    /// True when gameplay asked the window to hide and lock the cursor.
    #[must_use]
    pub fn cursor_captured(&self) -> bool {
        self.cursor_captured
    }

    /// Asks the window to hide and lock the cursor, for mouse look. The
    /// window runner applies the request after the frame's systems ran.
    pub fn set_cursor_captured(&mut self, captured: bool) {
        self.cursor_captured = captured;
    }

    /// Current window/render-viewport size in pixels, used to convert cursor
    /// positions into normalized device coordinates for gameplay picking.
    #[must_use]
    pub fn viewport_size(&self) -> [f32; 2] {
        self.viewport_size
    }

    /// Records one keyboard `WindowEvent`. Ignores OS auto-repeat: repeated
    /// presses do not re-fire `just_pressed`.
    pub fn record_key(&mut self, key: KeyCode, pressed: bool) {
        if pressed {
            if self.keys_held.insert(key) {
                self.keys_just_pressed.insert(key);
            }
        } else if self.keys_held.remove(&key) {
            self.keys_just_released.insert(key);
        }
    }

    /// Records one mouse button `WindowEvent`.
    pub fn record_mouse_button(&mut self, button: MouseButton, pressed: bool) {
        if pressed {
            if self.mouse_held.insert(button) {
                self.mouse_just_pressed.insert(button);
            }
        } else if self.mouse_held.remove(&button) {
            self.mouse_just_released.insert(button);
        }
    }

    /// Releases every held key and button, reporting each as just released.
    /// Call it when the window loses focus, because the matching release
    /// events then go to another window.
    pub fn release_all(&mut self) {
        self.keys_just_released.append(&mut self.keys_held);
        self.mouse_just_released.append(&mut self.mouse_held);
        self.pad_just_released.append(&mut self.pad_held);
        self.sticks = [[0.0; 2]; 2];
    }

    /// Whether the game window has keyboard focus. True until a
    /// [`Self::record_focus`] says otherwise.
    #[must_use]
    pub fn focused(&self) -> bool {
        !self.unfocused
    }

    /// Records a `WindowEvent::Focused`. Losing focus also releases every
    /// held key and button, as [`Self::release_all`] does.
    pub fn record_focus(&mut self, focused: bool) {
        self.unfocused = !focused;
        if !focused {
            self.release_all();
        }
    }

    /// Records the latest cursor position from a `CursorMoved` event.
    pub fn record_cursor_position(&mut self, position: [f32; 2]) {
        self.cursor_position = Some(position);
    }
    /// Adds one raw `DeviceEvent::MouseMotion` delta.
    pub fn record_mouse_motion(&mut self, delta: [f32; 2]) {
        self.mouse_motion[0] += delta[0];
        self.mouse_motion[1] += delta[1];
    }
    /// Adds one `MouseWheel` event, in lines.
    pub fn record_scroll(&mut self, lines: [f32; 2]) {
        self.scroll[0] += lines[0];
        self.scroll[1] += lines[1];
    }
    /// Records the current window/render-viewport size in pixels.
    pub fn record_viewport_size(&mut self, size: [f32; 2]) {
        self.viewport_size = size;
    }

    #[must_use]
    pub fn pad_held(&self, button: PadButton) -> bool {
        self.pad_held.contains(&button)
    }
    #[must_use]
    pub fn pad_just_pressed(&self, button: PadButton) -> bool {
        self.pad_just_pressed.contains(&button)
    }
    #[must_use]
    pub fn pad_just_released(&self, button: PadButton) -> bool {
        self.pad_just_released.contains(&button)
    }
    /// Pad buttons pressed since the last [`Self::clear_frame_edges`].
    pub fn pads_pressed(&self) -> impl Iterator<Item = PadButton> + '_ {
        self.pad_just_pressed.iter().copied()
    }
    /// Stick tilt, each axis -1..1; `[0, 1]` is pushed fully up.
    #[must_use]
    pub fn stick(&self, stick: Stick) -> [f32; 2] {
        self.sticks[stick as usize]
    }

    /// Records one gamepad button press or release.
    pub fn record_pad_button(&mut self, button: PadButton, pressed: bool) {
        record_edge(
            &mut self.pad_held,
            &mut self.pad_just_pressed,
            &mut self.pad_just_released,
            button,
            pressed,
        );
    }

    /// Records a stick's tilt and presses or releases its four direction
    /// buttons: pressed past 0.5, released below 0.3, so a stick resting
    /// near the threshold does not flicker.
    pub fn record_stick(&mut self, stick: Stick, tilt: [f32; 2]) {
        use PadButton::*;
        self.sticks[stick as usize] = tilt;
        let [up, down, left, right] = match stick {
            Stick::Left => {
                [LeftStickUp, LeftStickDown, LeftStickLeft, LeftStickRight]
            }
            Stick::Right => [
                RightStickUp,
                RightStickDown,
                RightStickLeft,
                RightStickRight,
            ],
        };
        for (button, amount) in [
            (up, tilt[1]),
            (down, -tilt[1]),
            (left, -tilt[0]),
            (right, tilt[0]),
        ] {
            if amount > 0.5 {
                self.record_pad_button(button, true);
            } else if amount < 0.3 {
                self.record_pad_button(button, false);
            }
        }
    }

    /// Records when a press happened, in fractional fixed ticks. The
    /// windowed runner calls it as key and mouse events arrive; the first
    /// press of a binding in a frame wins.
    pub fn record_press_tick(&mut self, binding: InputBinding, tick: f64) {
        if self.press_tick(binding).is_none() {
            self.press_ticks.push((binding, tick));
        }
    }

    /// When `binding` was pressed this frame, if the runner recorded it.
    #[must_use]
    pub fn press_tick(&self, binding: InputBinding) -> Option<f64> {
        self.press_ticks
            .iter()
            .find(|(bound, _)| *bound == binding)
            .map(|(_, tick)| *tick)
    }

    /// Moves the presses recorded at or after `end`, in fractional fixed
    /// ticks, out of this input and returns them as an input holding only
    /// those presses, so a frame that runs several ticks gives each press to
    /// the tick it happened in. Untimed presses and releases stay.
    pub fn take_presses_after(&mut self, end: f64) -> Self {
        let mut later = Self::default();
        let (taken, kept) = std::mem::take(&mut self.press_ticks)
            .into_iter()
            .partition(|&(_, tick)| tick >= end);
        self.press_ticks = kept;
        later.press_ticks = taken;
        for &(binding, _) in &later.press_ticks {
            // An earlier press of the same input stays with this tick too.
            let stays = self.press_tick(binding).is_some();
            match binding {
                InputBinding::Key(key) => {
                    if !stays {
                        self.keys_just_pressed.remove(&key);
                    }
                    later.keys_just_pressed.insert(key);
                }
                InputBinding::Mouse(button) => {
                    if !stays {
                        self.mouse_just_pressed.remove(&button);
                    }
                    later.mouse_just_pressed.insert(button);
                }
                InputBinding::Pad(button) => {
                    if !stays {
                        self.pad_just_pressed.remove(&button);
                    }
                    later.pad_just_pressed.insert(button);
                }
            }
        }
        later
    }

    /// Clears this frame's edge sets. Runtime integrations call this once per
    /// rendered frame after gameplay systems have read them, so the next frame
    /// starts empty.
    /// Adds the edge sets, press times, mouse motion and scroll of `earlier`, an
    /// earlier frame, to this one, so presses from frames that ran no fixed
    /// tick reach the next tick.
    pub fn merge_frame_edges(&mut self, earlier: &Self) {
        self.keys_just_pressed.extend(&earlier.keys_just_pressed);
        self.keys_just_released.extend(&earlier.keys_just_released);
        self.mouse_just_pressed.extend(&earlier.mouse_just_pressed);
        self.mouse_just_released
            .extend(&earlier.mouse_just_released);
        self.pad_just_pressed.extend(&earlier.pad_just_pressed);
        self.pad_just_released.extend(&earlier.pad_just_released);
        let later = std::mem::replace(
            &mut self.press_ticks,
            earlier.press_ticks.clone(),
        );
        self.press_ticks.extend(later);
        self.mouse_motion[0] += earlier.mouse_motion[0];
        self.mouse_motion[1] += earlier.mouse_motion[1];
        self.record_scroll(earlier.scroll);
    }

    pub fn clear_frame_edges(&mut self) {
        self.keys_just_pressed.clear();
        self.keys_just_released.clear();
        self.mouse_just_pressed.clear();
        self.mouse_just_released.clear();
        self.pad_just_pressed.clear();
        self.pad_just_released.clear();
        self.press_ticks.clear();
        self.mouse_motion = [0.0; 2];
        self.scroll = [0.0; 2];
    }
}

/// A raw input source which can satisfy a named gameplay action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputBinding {
    Key(KeyCode),
    Mouse(MouseButton),
    Pad(PadButton),
}

/// Maps action names to one or more raw input bindings (any bound input
/// satisfies the action).
#[derive(Resource, Clone, Default)]
pub struct ActionMap {
    bindings: HashMap<String, Vec<InputBinding>>,
}

impl ActionMap {
    pub fn bind(
        &mut self,
        action: impl Into<String>,
        binding: InputBinding,
    ) -> &mut Self {
        self.bindings
            .entry(action.into())
            .or_default()
            .push(binding);
        self
    }
    /// Every action with a binding, in name order.
    #[must_use]
    pub fn actions(&self) -> Vec<&str> {
        let mut names: Vec<&str> =
            self.bindings.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
    /// Removes one binding of `action`; other bindings stay.
    pub fn unbind(&mut self, action: &str, binding: InputBinding) {
        if let Some(list) = self.bindings.get_mut(action) {
            list.retain(|bound| *bound != binding);
            if list.is_empty() {
                self.bindings.remove(action);
            }
        }
    }
    #[must_use]
    pub fn held(&self, input: &RuntimeInput, action: &str) -> bool {
        self.bindings_for(action).any(|binding| match binding {
            InputBinding::Key(key) => input.key_held(*key),
            InputBinding::Mouse(button) => input.mouse_held(*button),
            InputBinding::Pad(button) => input.pad_held(*button),
        })
    }
    #[must_use]
    pub fn just_pressed(&self, input: &RuntimeInput, action: &str) -> bool {
        self.bindings_for(action).any(|binding| match binding {
            InputBinding::Key(key) => input.key_just_pressed(*key),
            InputBinding::Mouse(button) => input.mouse_just_pressed(*button),
            InputBinding::Pad(button) => input.pad_just_pressed(*button),
        })
    }
    #[must_use]
    pub fn just_released(&self, input: &RuntimeInput, action: &str) -> bool {
        self.bindings_for(action).any(|binding| match binding {
            InputBinding::Key(key) => input.key_just_released(*key),
            InputBinding::Mouse(button) => input.mouse_just_released(*button),
            InputBinding::Pad(button) => input.pad_just_released(*button),
        })
    }
    /// The earliest recorded time, in fractional fixed ticks, of a binding of
    /// `action` pressed this frame.
    #[must_use]
    pub fn press_tick(
        &self,
        input: &RuntimeInput,
        action: &str,
    ) -> Option<f64> {
        self.bindings_for(action)
            .filter_map(|binding| input.press_tick(*binding))
            .reduce(f64::min)
    }
    /// Inputs bound to `action`, in binding order; empty when unbound.
    #[must_use]
    pub fn bindings(&self, action: &str) -> &[InputBinding] {
        self.bindings.get(action).map_or(&[], Vec::as_slice)
    }
    fn bindings_for(
        &self,
        action: &str,
    ) -> impl Iterator<Item = &InputBinding> {
        self.bindings.get(action).into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::prelude::Resource;

    #[test]
    fn mouse_motion_adds_up_until_the_frame_ends() {
        let mut input = RuntimeInput::default();
        input.record_mouse_motion([1.0, -2.0]);
        input.record_mouse_motion([0.5, 1.0]);
        assert_eq!(input.mouse_motion(), [1.5, -1.0]);
        input.clear_frame_edges();
        assert_eq!(input.mouse_motion(), [0.0, 0.0]);
    }

    #[test]
    fn release_all_clears_held_input_as_releases() {
        let mut input = RuntimeInput::default();
        input.record_key(KeyCode::KeyW, true);
        input.record_mouse_button(MouseButton::Right, true);
        input.clear_frame_edges();
        input.release_all();
        assert!(!input.key_held(KeyCode::KeyW));
        assert!(input.key_just_released(KeyCode::KeyW));
        assert!(input.mouse_just_released(MouseButton::Right));
    }

    #[test]
    fn pad_buttons_and_stick_directions_drive_actions() {
        let mut map = ActionMap::default();
        map.bind("jump", InputBinding::Pad(PadButton::South))
            .bind("up", InputBinding::Pad(PadButton::LeftStickUp));
        let mut input = RuntimeInput::default();
        input.record_pad_button(PadButton::South, true);
        assert!(map.just_pressed(&input, "jump"));
        input.record_stick(Stick::Left, [0.0, 0.6]);
        assert!(map.just_pressed(&input, "up"));
        assert_eq!(input.stick(Stick::Left), [0.0, 0.6]);
        input.clear_frame_edges();
        // Between the thresholds the direction stays held.
        input.record_stick(Stick::Left, [0.0, 0.4]);
        assert!(map.held(&input, "up"));
        input.record_stick(Stick::Left, [0.0, 0.1]);
        assert!(map.just_released(&input, "up"));
        input.release_all();
        assert!(map.just_released(&input, "jump"));
    }

    #[test]
    fn runtime_input_and_action_map_are_ecs_resources() {
        fn assert_resource<T: Resource>() {}
        assert_resource::<RuntimeInput>();
        assert_resource::<ActionMap>();
    }
    #[test]
    fn keyboard_edges_and_auto_repeat_are_tracked() {
        let mut input = RuntimeInput::default();
        input.record_key(KeyCode::Space, true);
        assert!(input.key_held(KeyCode::Space));
        assert!(input.key_just_pressed(KeyCode::Space));
        input.record_key(KeyCode::Space, true);
        input.clear_frame_edges();
        assert!(input.key_held(KeyCode::Space));
        assert!(!input.key_just_pressed(KeyCode::Space));
        input.record_key(KeyCode::Space, false);
        assert!(input.key_just_released(KeyCode::Space));
    }
    #[test]
    fn mouse_cursor_and_viewport_are_tracked() {
        let mut input = RuntimeInput::default();
        input.record_mouse_button(MouseButton::Left, true);
        input.record_cursor_position([12.0, 34.0]);
        input.record_viewport_size([800.0, 600.0]);
        assert!(input.mouse_held(MouseButton::Left));
        assert!(input.mouse_just_pressed(MouseButton::Left));
        assert_eq!(input.cursor_position(), Some([12.0, 34.0]));
        assert_eq!(input.viewport_size(), [800.0, 600.0]);
        input.clear_frame_edges();
        input.record_mouse_button(MouseButton::Left, false);
        assert!(input.mouse_just_released(MouseButton::Left));
        assert!(!input.mouse_held(MouseButton::Left));
    }
    #[test]
    fn actions_cover_keyboard_mouse_and_edge_clearing() {
        let mut map = ActionMap::default();
        map.bind("jump", InputBinding::Key(KeyCode::Space))
            .bind("jump", InputBinding::Mouse(MouseButton::Left));
        let mut input = RuntimeInput::default();
        input.record_key(KeyCode::Space, true);
        assert!(map.just_pressed(&input, "jump"));
        assert!(map.held(&input, "jump"));
        input.clear_frame_edges();
        assert!(!map.just_pressed(&input, "jump"));
        assert!(map.held(&input, "jump"));
        input.record_key(KeyCode::Space, false);
        assert!(map.just_released(&input, "jump"));
        input.clear_frame_edges();
        input.record_mouse_button(MouseButton::Left, true);
        assert!(map.just_pressed(&input, "jump"));
        assert!(map.held(&input, "jump"));
        assert!(!map.held(&input, "unbound_action"));
    }
}
