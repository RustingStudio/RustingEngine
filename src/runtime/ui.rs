//! Runtime UI: immediate-mode egui drawn over the game view.
//!
//! Every `App` built with the `ui` feature has a [`RuntimeUi`] resource.
//! `App::update` opens one egui pass around the `Update` and `PostUpdate`
//! schedules, so any system there can draw a HUD or menu:
//!
//! ```ignore
//! fn hud(ui: Res<RuntimeUi>, time: Res<FrameTime>) {
//!     egui::Area::new("hud".into()).show(ui.context(), |ui| {
//!         ui.label(format!("{:.0} FPS", 1.0 / time.delta.as_secs_f32()));
//!     });
//! }
//! ```
//!
//! The window runner feeds window input in and paints the finished pass over
//! the rendered scene. Clicks and keys that egui uses do not reach
//! `RuntimeInput`. Without a window (scenarios, debug sessions, headless
//! runs) each pass reads `RuntimeInput` instead: the cursor, mouse buttons
//! and keys, on a screen of `RuntimeInput::viewport_size`. So a scenario
//! `pointer` step plus a mouse press clicks an egui button, and a `click`
//! step finds one by its text.

use bevy_ecs::prelude::Resource;
use rusting_core::input::{KeyCode, MouseButton, PadButton, RuntimeInput};
use serde::Deserialize;

#[derive(Resource)]
pub struct RuntimeUi {
    context: egui::Context,
    /// Window input for the next pass; taken when the pass begins. None
    /// builds it from `RuntimeInput`.
    input: Option<egui::RawInput>,
    /// Text drawn by the last pass, topmost last, with its screen rect.
    texts: Vec<(String, egui::Rect)>,
    /// Result of the last finished pass, until the runner takes it.
    output: Option<egui::FullOutput>,
    /// Screen size in pixels the HUD is laid out for; see
    /// [`Self::set_base_size`].
    base_size: Option<[f32; 2]>,
}

impl Default for RuntimeUi {
    fn default() -> Self {
        let context = egui::Context::default();
        // Dark always: following the desktop theme would turn button fills
        // light under white HUD text, and `rusting lint` checks button
        // contrast against the dark fill.
        context.set_theme(egui::Theme::Dark);
        Self {
            context,
            input: None,
            texts: Vec::new(),
            output: None,
            base_size: None,
        }
    }
}

impl RuntimeUi {
    /// Context to draw with during `Update` and `PostUpdate`.
    #[must_use]
    pub fn context(&self) -> &egui::Context {
        &self.context
    }

    /// Lays the UI out for a `[width, height]` screen in pixels and scales
    /// it uniformly to fit the real one, keeping the aspect ratio, like
    /// Godot's `canvas_items` stretch mode. `None` (the default) draws at
    /// the desktop's DPI scale instead.
    pub fn set_base_size(&mut self, size: Option<[f32; 2]>) {
        self.base_size =
            size.filter(|[width, height]| *width > 0.0 && *height > 0.0);
    }

    /// Factor the window's `native` DPI scale must be multiplied by so a
    /// `physical` pixel screen fits the base size.
    fn zoom(&self, physical: [f32; 2], native: f32) -> f32 {
        match self.base_size {
            Some([width, height]) if physical[0] > 0.0 && physical[1] > 0.0 => {
                (physical[0] / width).min(physical[1] / height) / native
            }
            _ => 1.0,
        }
    }

    /// Scales the UI for a window of `physical` pixels at the `native` DPI
    /// scale. The runner calls it before reading window input, which egui
    /// then converts with the new scale.
    pub fn fit_window(&mut self, physical: [f32; 2], native: f32) {
        self.context.set_zoom_factor(self.zoom(physical, native));
    }

    /// Restyles buttons and menus from egui's dark look.
    pub fn set_theme(&mut self, theme: &UiTheme) {
        let mut style = (*self.context.style()).clone();
        style.visuals = egui::Visuals::dark();
        theme.apply(&mut style);
        self.context.set_style(style);
    }

    /// Sets the input the next pass reads.
    pub fn set_input(&mut self, input: egui::RawInput) {
        self.input = Some(input);
    }

    /// Center of the topmost visible text the last pass drew that equals
    /// `label` (ignoring surrounding spaces), for clicking a widget by name.
    ///
    /// # Errors
    /// Returns the texts on screen when none matches.
    pub fn find_text(&self, label: &str) -> Result<[f32; 2], String> {
        self.find_text_where(label, false, None)
    }

    /// Like [`Self::find_text`], but `prefix` matches texts that start with
    /// `label`, and `index` picks the nth match (0-based) in reading order:
    /// top to bottom, then left to right. Copies of a text drawn within
    /// 4 px of each other, such as a drop shadow, count as one place.
    ///
    /// # Errors
    /// Returns the texts on screen when no match has that index.
    pub fn find_text_where(
        &self,
        label: &str,
        prefix: bool,
        index: Option<usize>,
    ) -> Result<[f32; 2], String> {
        let label = label.trim();
        let mut found: Vec<egui::Rect> = self
            .texts
            .iter()
            .filter(|(text, _)| {
                let text = text.trim();
                if prefix {
                    text.starts_with(label)
                } else {
                    text == label
                }
            })
            .map(|(_, rect)| *rect)
            .collect();
        let rect = match index {
            None => found.last().copied(),
            Some(index) => {
                // A later copy is drawn on top, so it replaces the earlier.
                let mut places: Vec<egui::Rect> = Vec::new();
                for rect in found {
                    match places.iter_mut().find(|place| {
                        place.center().distance(rect.center()) <= 4.0
                    }) {
                        Some(place) => *place = rect,
                        None => places.push(rect),
                    }
                }
                found = places;
                found.sort_by(|a, b| {
                    (a.center().y, a.center().x)
                        .partial_cmp(&(b.center().y, b.center().x))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                found.get(index).copied()
            }
        };
        if let Some(rect) = rect {
            return Ok([rect.center().x, rect.center().y]);
        }
        let label = match index {
            Some(index) => {
                format!("{label}` #{index} (of {} matches)", found.len())
            }
            None => format!("{label}`"),
        };
        let shown: Vec<_> = self
            .texts
            .iter()
            .map(|(text, _)| format!("`{}`", text.trim()))
            .take(40)
            .collect();
        Err(format!(
            "no UI text `{label} on screen; the last frame drew: {}",
            if shown.is_empty() {
                "nothing".into()
            } else {
                shown.join(", ")
            }
        ))
    }

    /// Takes the last finished pass: shapes, texture changes, and platform
    /// output such as the cursor icon.
    pub fn take_output(&mut self) -> Option<egui::FullOutput> {
        self.output.take()
    }

    pub(super) fn begin_pass(&mut self, input: &RuntimeInput) {
        let mut raw = self.input.take().unwrap_or_else(|| {
            let zoom = self.zoom(input.viewport_size(), 1.0);
            self.context.set_zoom_factor(zoom);
            raw_input(input, zoom)
        });
        let focused = self.context.memory(|memory| memory.focused().is_some());
        raw.events.extend(pad_keys(input, focused));
        self.context.begin_pass(raw);
    }

    pub(super) fn end_pass(&mut self) {
        let mut output = self.context.end_pass();
        self.texts.clear();
        for clipped in &output.shapes {
            collect_texts(&clipped.shape, clipped.clip_rect, &mut self.texts);
        }
        // In pixels, where clicks land, when a base size scales the UI.
        let zoom = self.context.zoom_factor();
        for (_, rect) in &mut self.texts {
            *rect = egui::Rect::from_min_max(
                (rect.min.to_vec2() * zoom).to_pos2(),
                (rect.max.to_vec2() * zoom).to_pos2(),
            );
        }
        // Texture changes are cumulative; keep ones the runner has not
        // uploaded yet (a headless run never takes them).
        if let Some(previous) = self.output.take() {
            let mut textures = previous.textures_delta;
            textures.append(output.textures_delta);
            output.textures_delta = textures;
        }
        self.output = Some(output);
    }
}

/// The look of HUD buttons and game menus, read from
/// `assets/ui/theme.json` when the game starts. Colours are RGBA from 0 to
/// 1; a field left out keeps egui's dark look.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiTheme {
    pub button_fill: Option<[f32; 4]>,
    pub button_hover_fill: Option<[f32; 4]>,
    pub button_pressed_fill: Option<[f32; 4]>,
    /// Behind panels and windows that game menus open.
    pub panel_fill: Option<[f32; 4]>,
    /// Text of menu widgets; a HUD element's `color` still wins.
    pub text_color: Option<[f32; 4]>,
    /// Corner rounding of buttons and panels, in points.
    pub corner_radius: Option<f32>,
    /// Space between a button's text and its edge, `[x, y]` in points.
    pub button_padding: Option<[f32; 2]>,
}

impl UiTheme {
    pub fn apply(&self, style: &mut egui::Style) {
        let color = |c: [f32; 4]| {
            let [r, g, b, a] = c.map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8);
            egui::Color32::from_rgba_unmultiplied(r, g, b, a)
        };
        let visuals = &mut style.visuals;
        for (fill, widget) in [
            (self.button_fill, &mut visuals.widgets.inactive),
            (self.button_hover_fill, &mut visuals.widgets.hovered),
            (self.button_pressed_fill, &mut visuals.widgets.active),
        ] {
            if let Some(fill) = fill {
                widget.bg_fill = color(fill);
                widget.weak_bg_fill = color(fill);
            }
        }
        if let Some(fill) = self.panel_fill {
            visuals.panel_fill = color(fill);
            visuals.window_fill = color(fill);
        }
        visuals.override_text_color = self.text_color.map(color);
        if let Some(radius) = self.corner_radius {
            let radius =
                egui::CornerRadius::same(radius.clamp(0.0, 255.0) as u8);
            visuals.window_corner_radius = radius;
            for widget in [
                &mut visuals.widgets.noninteractive,
                &mut visuals.widgets.inactive,
                &mut visuals.widgets.hovered,
                &mut visuals.widgets.active,
                &mut visuals.widgets.open,
            ] {
                widget.corner_radius = radius;
            }
        }
        if let Some([x, y]) = self.button_padding {
            style.spacing.button_padding = egui::vec2(x, y);
        }
    }
}

fn collect_texts(
    shape: &egui::Shape,
    clip: egui::Rect,
    texts: &mut Vec<(String, egui::Rect)>,
) {
    match shape {
        egui::Shape::Vec(shapes) => {
            for shape in shapes {
                collect_texts(shape, clip, texts);
            }
        }
        egui::Shape::Text(text) => {
            let rect = text.galley.rect.translate(text.pos.to_vec2());
            if !text.galley.text().trim().is_empty() && clip.intersects(rect) {
                texts.push((text.galley.text().to_owned(), rect));
            }
        }
        _ => {}
    }
}

/// egui input for a pass with no window, read from `RuntimeInput`, in
/// points of `zoom` pixels.
fn raw_input(input: &RuntimeInput, zoom: f32) -> egui::RawInput {
    let [width, height] = input.viewport_size().map(|side| side / zoom);
    let mut raw = egui::RawInput {
        screen_rect: (width > 0.0 && height > 0.0).then(|| {
            egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )
        }),
        ..egui::RawInput::default()
    };
    let cursor = input
        .cursor_position()
        .map(|[x, y]| egui::pos2(x / zoom, y / zoom));
    if let Some(pos) = cursor {
        raw.events.push(egui::Event::PointerMoved(pos));
        let [x, y] = input.scroll();
        if x != 0.0 || y != 0.0 {
            raw.events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(x, y),
                modifiers: egui::Modifiers::default(),
            });
        }
        for (button, egui_button) in [
            (MouseButton::Left, egui::PointerButton::Primary),
            (MouseButton::Right, egui::PointerButton::Secondary),
            (MouseButton::Middle, egui::PointerButton::Middle),
        ] {
            for (pressed, edge) in [
                (true, input.mouse_just_pressed(button)),
                (false, input.mouse_just_released(button)),
            ] {
                if edge {
                    raw.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui_button,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    });
                }
            }
        }
    }
    let keys = input
        .keys_pressed()
        .map(|key| (key, true))
        .chain(input.keys_released().map(|key| (key, false)));
    for (key, pressed) in keys {
        if let Some(key) = egui_key(key) {
            raw.events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            });
        }
    }
    raw
}

/// Gamepad menu navigation as egui keys: the d-pad and left stick move the
/// focus (the first press focuses the first widget), South clicks, East is
/// Escape.
fn pad_keys(
    input: &RuntimeInput,
    focused: bool,
) -> impl Iterator<Item = egui::Event> + '_ {
    use egui::Key;
    use PadButton::*;
    input.pads_pressed().filter_map(move |button| {
        let key = match button {
            DpadUp | LeftStickUp if focused => Key::ArrowUp,
            DpadDown | LeftStickDown if focused => Key::ArrowDown,
            DpadLeft | LeftStickLeft if focused => Key::ArrowLeft,
            DpadRight | LeftStickRight if focused => Key::ArrowRight,
            DpadUp | DpadDown | DpadLeft | DpadRight | LeftStickUp
            | LeftStickDown | LeftStickLeft | LeftStickRight => Key::Tab,
            South => Key::Enter,
            East => Key::Escape,
            _ => return None,
        };
        Some(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        })
    })
}

/// `KeyA` is egui's `A`, `Digit1` its `1`; most other names match.
fn egui_key(key: KeyCode) -> Option<egui::Key> {
    let name = format!("{key:?}");
    let name = name
        .strip_prefix("Key")
        .or_else(|| name.strip_prefix("Digit"))
        .unwrap_or(&name);
    egui::Key::from_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_text_where_picks_by_prefix_and_reading_order() {
        let mut ui = RuntimeUi::default();
        let mut input = RuntimeInput::default();
        input.record_viewport_size([640.0, 360.0]);
        ui.begin_pass(&input);
        egui::CentralPanel::default().show(ui.context(), |panel| {
            let mut last = None;
            for _ in 0..3 {
                last = Some(panel.button("-").rect);
            }
            // A shadow copy 1 px below the last `-` adds no place.
            panel.painter().text(
                last.unwrap().center() + egui::vec2(0.0, 1.0),
                egui::Align2::CENTER_CENTER,
                "-",
                egui::FontId::default(),
                egui::Color32::BLACK,
            );
            let _ = panel.button("Slot 1: Night 1, $30.00");
        });
        ui.end_pass();
        let first = ui.find_text_where("-", false, Some(0)).unwrap();
        let third = ui.find_text_where("-", false, Some(2)).unwrap();
        assert!(third[1] > first[1], "{first:?} {third:?}");
        assert!(ui.find_text_where("Slot 1", true, None).is_ok());
        assert!(ui.find_text("Slot 1").is_err(), "exact by default");
        let error = ui.find_text_where("-", false, Some(5)).unwrap_err();
        assert!(error.contains("#5 (of 3 matches)"), "{error}");
    }

    #[test]
    fn a_base_size_scales_the_ui_to_fit_the_screen() {
        let label_at = |base: Option<[f32; 2]>, screen: [f32; 2]| {
            let mut ui = RuntimeUi::default();
            ui.set_base_size(base);
            let mut input = RuntimeInput::default();
            input.record_viewport_size(screen);
            ui.begin_pass(&input);
            ui.context()
                .layer_painter(egui::LayerId::background())
                .text(
                    egui::pos2(100.0, 50.0),
                    egui::Align2::LEFT_TOP,
                    "Score",
                    egui::FontId::default(),
                    egui::Color32::WHITE,
                );
            ui.end_pass();
            ui.find_text("Score").unwrap()
        };
        let small = label_at(Some([640.0, 360.0]), [640.0, 360.0]);
        // Twice the base size: twice as far from the corner.
        let large = label_at(Some([640.0, 360.0]), [1280.0, 720.0]);
        for axis in 0..2 {
            assert!(
                (large[axis] - 2.0 * small[axis]).abs() < 1.0,
                "{small:?} {large:?}"
            );
        }
        // A wider screen scales by its height and keeps the aspect.
        let wide = label_at(Some([640.0, 360.0]), [1920.0, 720.0]);
        assert!((wide[0] - large[0]).abs() < 1.0, "{wide:?} {large:?}");
        // No base size: points are pixels at any size.
        assert_eq!(label_at(None, [1280.0, 720.0]), small);
    }

    #[test]
    fn a_theme_restyles_buttons_and_panels() {
        let theme: UiTheme = serde_json::from_str(
            r#"{"button_fill": [1, 0, 0, 1], "panel_fill": [0, 0, 1, 1],
                "text_color": [0, 1, 0, 1], "corner_radius": 8,
                "button_padding": [12, 6]}"#,
        )
        .unwrap();
        let mut ui = RuntimeUi::default();
        ui.set_theme(&theme);
        let style = ui.context().style();
        let visuals = &style.visuals;
        assert_eq!(visuals.widgets.inactive.weak_bg_fill, egui::Color32::RED);
        assert_eq!(visuals.panel_fill, egui::Color32::BLUE);
        assert_eq!(visuals.override_text_color, Some(egui::Color32::GREEN));
        assert_eq!(visuals.widgets.hovered.corner_radius.nw, 8);
        assert_eq!(style.spacing.button_padding, egui::vec2(12.0, 6.0));
        // Unset fields keep the dark look.
        assert_eq!(
            visuals.widgets.hovered.weak_bg_fill,
            egui::Visuals::dark().widgets.hovered.weak_bg_fill
        );
        assert!(serde_json::from_str::<UiTheme>(
            r#"{"buton_fill": [1, 1, 1, 1]}"#
        )
        .is_err());
    }

    #[test]
    fn the_hud_stays_dark_on_a_light_desktop() {
        let mut ui = RuntimeUi::default();
        ui.set_input(egui::RawInput {
            system_theme: Some(egui::Theme::Light),
            ..Default::default()
        });
        ui.begin_pass(&RuntimeInput::default());
        let dark = ui.context().style().visuals.dark_mode;
        ui.end_pass();
        assert!(dark);
    }

    #[test]
    fn a_headless_pass_clicks_a_button_from_runtime_input() {
        let mut ui = RuntimeUi::default();
        let mut input = RuntimeInput::default();
        input.record_viewport_size([640.0, 360.0]);
        let clicks = std::cell::Cell::new(0);
        let frame = |ui: &mut RuntimeUi, input: &mut RuntimeInput| {
            ui.begin_pass(input);
            egui::CentralPanel::default().show(ui.context(), |panel| {
                clicks.set(
                    clicks.get()
                        + usize::from(panel.button("New Game").clicked()),
                );
            });
            ui.end_pass();
            input.clear_frame_edges();
        };
        frame(&mut ui, &mut input);
        assert_eq!(ui.context().screen_rect().width(), 640.0);
        let at = ui.find_text("New Game").unwrap();
        assert!(ui.find_text("Quit").unwrap_err().contains("`New Game`"));
        input.record_cursor_position(at);
        input.record_mouse_button(MouseButton::Left, true);
        frame(&mut ui, &mut input);
        input.record_mouse_button(MouseButton::Left, false);
        frame(&mut ui, &mut input);
        assert_eq!(clicks.get(), 1);
        // Keyboard only: Tab focuses the button, Enter clicks it.
        for key in [KeyCode::Tab, KeyCode::Enter] {
            input.record_key(key, true);
            frame(&mut ui, &mut input);
            input.record_key(key, false);
            frame(&mut ui, &mut input);
        }
        assert_eq!(clicks.get(), 2);
        // Gamepad only, from no focus: the d-pad focuses, South clicks.
        let mut ui = RuntimeUi::default();
        frame(&mut ui, &mut input);
        for button in [PadButton::DpadDown, PadButton::South] {
            input.record_pad_button(button, true);
            frame(&mut ui, &mut input);
            input.record_pad_button(button, false);
            frame(&mut ui, &mut input);
        }
        assert_eq!(clicks.get(), 3);
        assert_eq!(egui_key(KeyCode::KeyA), Some(egui::Key::A));
        assert_eq!(egui_key(KeyCode::Digit1), Some(egui::Key::Num1));
        assert_eq!(egui_key(KeyCode::Enter), Some(egui::Key::Enter));
    }
}
