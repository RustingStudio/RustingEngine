//! Particle emitter Inspector: a preset picker, Play/Pause/Restart/Stop for
//! the live preview, and Godot-style sections with range, gradient, and
//! curve widgets. Edits return as a new value and save through the scene
//! registry, so each one is a snapshot Undo step.

use egui::{DragValue, Sense, Ui};

use super::widgets;
use crate::editor::gui_elements::EditorTheme;
use crate::runtime::{
    effect_preset, ColorKey, CurveKey, EmitterShape, ParticleBlend,
    ParticleBurst, ParticleCommand, ParticleEmitter, ParticleFacing,
    ParticleSimulation, ParticleSpace, ParticleSprite, ParticleSystem,
    EFFECT_PRESETS, MAX_GPU_PARTICLES,
};

const SHAPES: [(EmitterShape, &str); 5] = [
    (EmitterShape::Point, "Point"),
    (EmitterShape::Box, "Box"),
    (EmitterShape::Sphere, "Sphere"),
    (EmitterShape::Cone, "Cone"),
    (EmitterShape::Circle, "Circle"),
];
const SPACES: [(ParticleSpace, &str); 2] = [
    (ParticleSpace::World, "World"),
    (ParticleSpace::Local, "Local"),
];
const FACINGS: [(ParticleFacing, &str); 2] = [
    (ParticleFacing::Billboard, "Billboard"),
    (ParticleFacing::Velocity, "Velocity"),
];
const BLENDS: [(ParticleBlend, &str); 2] = [
    (ParticleBlend::Alpha, "Alpha"),
    (ParticleBlend::Additive, "Additive"),
];
const SIMULATIONS: [(ParticleSimulation, &str); 2] = [
    (ParticleSimulation::Cpu, "CPU"),
    (ParticleSimulation::Gpu, "GPU"),
];
const SPRITES: [(ParticleSprite, &str); 3] = [
    (ParticleSprite::Soft, "Soft"),
    (ParticleSprite::Disc, "Disc"),
    (ParticleSprite::Square, "Square"),
];

/// Draws `emitter` and returns true when a setting changed. A transport
/// button sets `command`; `system` is the live preview, when there is one.
pub(super) fn draw_particle_emitter(
    ui: &mut Ui,
    emitter: &mut ParticleEmitter,
    system: Option<&ParticleSystem>,
    command: &mut Option<ParticleCommand>,
) -> bool {
    let mut changed = false;
    widgets::property_row(ui, "Preset", |ui| {
        egui::ComboBox::from_id_salt("particle_preset")
            .width(ui.available_width())
            .selected_text("Apply preset…")
            .show_ui(ui, |ui| {
                for preset in EFFECT_PRESETS {
                    if ui
                        .selectable_label(false, preset.name)
                        .on_hover_text(preset.summary)
                        .clicked()
                    {
                        changed |= apply_preset(emitter, preset.name);
                    }
                }
            });
    });
    transport(ui, system, command);

    widgets::section(ui, "Emission", false, |ui| {
        changed |= widgets::checkbox(ui, "Autoplay", &mut emitter.autoplay);
        changed |= widgets::drag(
            ui,
            "Rate",
            DragValue::new(&mut emitter.rate)
                .range(0.0..=f32::MAX)
                .speed(0.5)
                .suffix(" /s"),
        );
        changed |= widgets::choice(
            ui,
            "Simulation",
            &mut emitter.simulation,
            &SIMULATIONS,
        );
        if emitter.simulation == ParticleSimulation::Gpu {
            widgets::describe_next_row(ui, "bounce off physics colliders");
            changed |= widgets::checkbox(ui, "Collide", &mut emitter.collide);
            changed |= widgets::drag(
                ui,
                "Trail",
                DragValue::new(&mut emitter.trail)
                    .range(0.0..=10.0)
                    .speed(0.01)
                    .suffix(" s"),
            );
            widgets::describe_next_row(ui, "spawns where parent particles die");
            changed |= widgets::drag(
                ui,
                "On Parent Death",
                DragValue::new(&mut emitter.on_parent_death).range(0..=64),
            );
        }
        let most = match emitter.simulation {
            ParticleSimulation::Cpu => 100_000,
            ParticleSimulation::Gpu => MAX_GPU_PARTICLES,
        };
        changed |= widgets::drag(
            ui,
            "Max Particles",
            DragValue::new(&mut emitter.max_particles).range(1..=most),
        );
        changed |= widgets::drag(
            ui,
            "Duration",
            DragValue::new(&mut emitter.duration)
                .range(0.001..=f32::MAX)
                .speed(0.05)
                .suffix(" s"),
        );
        changed |= widgets::checkbox(ui, "Looping", &mut emitter.looping);
        changed |= widgets::checkbox(ui, "Prewarm", &mut emitter.prewarm);
        widgets::describe_next_row(ui, "restart on contact; CPU colliders");
        changed |=
            widgets::checkbox(ui, "On Collision", &mut emitter.on_collision);
        changed |= bursts(ui, &mut emitter.bursts);
    });
    widgets::section(ui, "Shape", false, |ui| {
        changed |= widgets::choice(ui, "Shape", &mut emitter.shape, &SHAPES);
        widgets::describe_next_row(
            ui,
            "box half extents; x is the radius of round shapes",
        );
        changed |= widgets::vec3(ui, "Size", &mut emitter.shape_size, 0.02);
        changed |= widgets::vec3(ui, "Direction", &mut emitter.direction, 0.02);
        changed |=
            widgets::angle(ui, "Spread", &mut emitter.spread, 0.0..=180.0);
    });
    widgets::section(ui, "Lifetime", false, |ui| {
        changed |= range(ui, "Lifetime", &mut emitter.lifetime, 0.05, " s");
        changed |= fraction(ui, "Fade In", &mut emitter.fade_in);
        changed |= fraction(ui, "Fade Out", &mut emitter.fade_out);
    });
    widgets::section(ui, "Velocity", false, |ui| {
        changed |= range(ui, "Speed", &mut emitter.speed, 0.05, " m/s");
        changed |= float(ui, "Gravity", &mut emitter.gravity, " m/s²");
        changed |= float(ui, "Drag", &mut emitter.drag, " /s");
        changed |= widgets::vec3(ui, "Wind", &mut emitter.wind, 0.05);
        changed |= float(ui, "Turbulence", &mut emitter.turbulence, " m/s²");
        changed |= float(
            ui,
            "Turb. Frequency",
            &mut emitter.turbulence_frequency,
            " /m",
        );
        changed |= degrees_range(ui, "Rotation", &mut emitter.rotation, "°");
        changed |= degrees_range(ui, "Spin", &mut emitter.spin, "°/s");
    });
    widgets::section(ui, "Size", false, |ui| {
        changed |= range(ui, "Start Size", &mut emitter.size, 0.01, " m");
        changed |= size_curve(ui, emitter);
    });
    widgets::section(ui, "Color", false, |ui| {
        changed |= gradient(ui, emitter);
        changed |= start_colors(ui, &mut emitter.start_colors);
        widgets::describe_next_row(ui, "above 1 blooms");
        changed |= float(ui, "Emissive", &mut emitter.emissive, "×");
    });
    widgets::section(ui, "Rendering", false, |ui| {
        changed |= widgets::choice(ui, "Blend", &mut emitter.blend, &BLENDS);
        changed |= widgets::choice(ui, "Sprite", &mut emitter.sprite, &SPRITES);
        changed |= widgets::choice(ui, "Facing", &mut emitter.facing, &FACINGS);
        widgets::describe_next_row(ui, "Velocity facing: length per m/s");
        changed |= float(ui, "Stretch", &mut emitter.stretch, " s");
        changed |= widgets::choice(ui, "Space", &mut emitter.space, &SPACES);
    });
    changed
}

/// Replaces every setting with the preset's; false for an unknown name.
pub(super) fn apply_preset(emitter: &mut ParticleEmitter, name: &str) -> bool {
    let Some(preset) = effect_preset(name) else {
        return false;
    };
    *emitter = preset.emitter();
    true
}

fn transport(
    ui: &mut Ui,
    system: Option<&ParticleSystem>,
    command: &mut Option<ParticleCommand>,
) {
    ui.horizontal(|ui| {
        let playing = system.is_some_and(|s| s.playing && !s.paused);
        for (label, action, selected) in [
            ("Play", ParticleCommand::Play, playing),
            (
                "Pause",
                ParticleCommand::Pause,
                system.is_some_and(|s| s.paused),
            ),
            ("Restart", ParticleCommand::Restart, false),
            ("Stop", ParticleCommand::Stop, false),
        ] {
            if EditorTheme::toolbar_button(ui, label, selected, true).clicked()
            {
                *command = Some(action);
            }
        }
        let alive = system.map_or(0, ParticleSystem::alive);
        ui.colored_label(EditorTheme::TEXT_MUTED, format!("{alive} alive"));
    });
}

fn float(ui: &mut Ui, label: &str, value: &mut f32, suffix: &str) -> bool {
    widgets::drag(ui, label, DragValue::new(value).speed(0.02).suffix(suffix))
}

fn fraction(ui: &mut Ui, label: &str, value: &mut f32) -> bool {
    widgets::drag(
        ui,
        label,
        DragValue::new(value)
            .range(0.0..=1.0)
            .speed(0.01)
            .suffix(" life"),
    )
}

/// Min–max pair on one row; dragging one end past the other moves both.
fn range(
    ui: &mut Ui,
    label: &str,
    pair: &mut [f32; 2],
    speed: f64,
    suffix: &str,
) -> bool {
    widgets::describe_next_row(ui, "each particle picks a value in min..max");
    let changed = widgets::property_row(ui, label, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let width = ((ui.available_width() - 14.0) / 2.0).max(28.0);
        let [min, max] = pair;
        let low = ui
            .add_sized(
                [width, EditorTheme::ROW_HEIGHT],
                DragValue::new(min).speed(speed).suffix(suffix),
            )
            .changed();
        ui.colored_label(EditorTheme::TEXT_MUTED, "–");
        let high = ui
            .add_sized(
                [width, EditorTheme::ROW_HEIGHT],
                DragValue::new(max).speed(speed).suffix(suffix),
            )
            .changed();
        if low && *min > *max {
            *max = *min;
        }
        if high && *max < *min {
            *min = *max;
        }
        low || high
    });
    changed
}

/// Radian min–max pair shown in degrees.
fn degrees_range(
    ui: &mut Ui,
    label: &str,
    pair: &mut [f32; 2],
    suffix: &str,
) -> bool {
    let mut shown = pair.map(f32::to_degrees);
    let changed = range(ui, label, &mut shown, 1.0, suffix);
    if changed {
        *pair = shown.map(f32::to_radians);
    }
    changed
}

fn bursts(ui: &mut Ui, bursts: &mut Vec<ParticleBurst>) -> bool {
    let mut changed = false;
    let mut remove = None;
    for (index, burst) in bursts.iter_mut().enumerate() {
        ui.push_id(("burst", index), |ui| {
            widgets::property_row(ui, &format!("Burst {}", index + 1), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let width = ((ui.available_width() - 26.0) / 2.0).max(28.0);
                changed |= ui
                    .add_sized(
                        [width, EditorTheme::ROW_HEIGHT],
                        DragValue::new(&mut burst.time)
                            .range(0.0..=f32::MAX)
                            .speed(0.02)
                            .prefix("at ")
                            .suffix(" s"),
                    )
                    .changed();
                changed |= ui
                    .add_sized(
                        [width, EditorTheme::ROW_HEIGHT],
                        DragValue::new(&mut burst.count).suffix(" particles"),
                    )
                    .changed();
                if ui.small_button("×").on_hover_text("Remove burst").clicked()
                {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        bursts.remove(index);
        changed = true;
    }
    if add_button(ui, "Add Burst") {
        bursts.push(ParticleBurst::default());
        changed = true;
    }
    changed
}

fn start_colors(ui: &mut Ui, colors: &mut Vec<[f32; 4]>) -> bool {
    let mut changed = false;
    let mut remove = None;
    for (index, color) in colors.iter_mut().enumerate() {
        ui.push_id(("start_color", index), |ui| {
            widgets::property_row(ui, &format!("Tint {}", index + 1), |ui| {
                ui.spacing_mut().interact_size.x = ui.available_width() - 24.0;
                changed |=
                    ui.color_edit_button_rgba_unmultiplied(color).changed();
                if ui.small_button("×").on_hover_text("Remove tint").clicked()
                {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        colors.remove(index);
        changed = true;
    }
    if add_button(ui, "Add Start Tint") {
        colors.push([1.0; 4]);
        changed = true;
    }
    changed
}

fn add_button(ui: &mut Ui, label: &str) -> bool {
    widgets::property_row(ui, "", |ui| {
        EditorTheme::toolbar_button(ui, &format!("+ {label}"), false, true)
            .clicked()
    })
}

fn color32(rgba: [f32; 4]) -> egui::Color32 {
    egui::Rgba::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
        .into()
}

/// Gradient bar of `color_over_life` with draggable key markers below it;
/// a click on the bar adds a key with the color shown there. Rows under the
/// bar edit each key's color and remove it.
fn gradient(ui: &mut Ui, emitter: &mut ParticleEmitter) -> bool {
    let mut changed = false;
    widgets::describe_next_row(
        ui,
        "click the bar to add a key; drag a marker to move it",
    );
    widgets::property_row(ui, "Color Over Life", |ui| {
        let (bar, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), EditorTheme::ROW_HEIGHT),
            Sense::click(),
        );
        paint_checker(ui, bar);
        const SLICES: usize = 48;
        for slice in 0..SLICES {
            let t0 = slice as f32 / SLICES as f32;
            let t1 = (slice + 1) as f32 / SLICES as f32;
            let rect = egui::Rect::from_x_y_ranges(
                bar.left() + bar.width() * t0..=bar.left() + bar.width() * t1,
                bar.y_range(),
            );
            let color = emitter.color_at((t0 + t1) * 0.5);
            ui.painter().rect_filled(rect, 0.0, color32(color));
        }
        ui.painter().rect_stroke(
            bar,
            EditorTheme::RADIUS,
            egui::Stroke::new(1.0_f32, EditorTheme::BORDER),
            egui::StrokeKind::Inside,
        );
        if let Some(pointer) =
            response.clicked().then(|| response.interact_pointer_pos())
        {
            let t = pointer.map_or(0.5, |p| (p.x - bar.left()) / bar.width());
            add_color_key(&mut emitter.color_over_life, t);
            changed = true;
        }
        let keys = emitter.color_over_life.len();
        for index in 0..keys {
            let t = emitter.color_over_life[index].t;
            let x = bar.left() + bar.width() * t.clamp(0.0, 1.0);
            let handle = egui::Rect::from_center_size(
                egui::pos2(x, bar.bottom() - 3.0),
                egui::vec2(9.0, 9.0),
            );
            let drag = ui.interact(
                handle,
                ui.id().with(("color_key", index)),
                Sense::drag(),
            );
            if drag.dragged() {
                let moved = t + drag.drag_delta().x / bar.width();
                let (low, high) =
                    neighbours(&emitter.color_over_life, index, |k| k.t);
                emitter.color_over_life[index].t = moved.clamp(low, high);
                changed = true;
            }
            marker(
                ui,
                handle,
                emitter.color_over_life[index].color,
                drag.hovered(),
            );
        }
    });
    let mut remove = None;
    for (index, key) in emitter.color_over_life.iter_mut().enumerate() {
        ui.push_id(("color_row", index), |ui| {
            widgets::property_row(ui, &format!("Key {}", index + 1), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                changed |= ui
                    .add_sized(
                        [52.0, EditorTheme::ROW_HEIGHT],
                        DragValue::new(&mut key.t).range(0.0..=1.0).speed(0.01),
                    )
                    .changed();
                ui.spacing_mut().interact_size.x = ui.available_width() - 24.0;
                changed |= ui
                    .color_edit_button_rgba_unmultiplied(&mut key.color)
                    .changed();
                if ui.small_button("×").on_hover_text("Remove key").clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        emitter.color_over_life.remove(index);
        changed = true;
    }
    if changed {
        sort_keys(&mut emitter.color_over_life, |k| k.t);
    }
    changed
}

/// Size factor over life as a small plot. Click the plot to add a key, drag
/// a dot to move it; rows below edit or remove keys exactly.
fn size_curve(ui: &mut Ui, emitter: &mut ParticleEmitter) -> bool {
    let mut changed = false;
    // The plot's top is the largest key, at least 1.
    let top = emitter
        .size_over_life
        .iter()
        .fold(1.0_f32, |top, key| top.max(key.value))
        * 1.1;
    widgets::describe_next_row(
        ui,
        "size factor by life; click to add a key, drag a dot to move it",
    );
    widgets::property_row(ui, "Size Over Life", |ui| {
        let (plot, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 56.0),
            Sense::click(),
        );
        let painter = ui.painter_at(plot);
        painter.rect_filled(plot, EditorTheme::RADIUS, EditorTheme::INPUT);
        let to_screen = |t: f32, value: f32| {
            egui::pos2(
                plot.left() + plot.width() * t,
                plot.bottom() - plot.height() * (value / top),
            )
        };
        let points = (0..=32)
            .map(|step| {
                let t = step as f32 / 32.0;
                to_screen(t, emitter.size_at(t))
            })
            .collect();
        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5_f32, EditorTheme::ACCENT_HOVER),
        ));
        if response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let t = (pointer.x - plot.left()) / plot.width();
                add_size_key(&mut emitter.size_over_life, t);
                changed = true;
            }
        }
        for index in 0..emitter.size_over_life.len() {
            let key = emitter.size_over_life[index];
            let handle = egui::Rect::from_center_size(
                to_screen(key.t, key.value),
                egui::vec2(9.0, 9.0),
            );
            let drag = ui.interact(
                handle,
                ui.id().with(("size_key", index)),
                Sense::drag(),
            );
            if drag.dragged() {
                let delta = drag.drag_delta();
                let (low, high) =
                    neighbours(&emitter.size_over_life, index, |k| k.t);
                let key = &mut emitter.size_over_life[index];
                key.t = (key.t + delta.x / plot.width()).clamp(low, high);
                key.value =
                    (key.value - delta.y / plot.height() * top).max(0.0);
                changed = true;
            }
            painter.circle(
                handle.center(),
                if drag.hovered() { 4.5 } else { 3.5 },
                EditorTheme::TEXT_STRONG,
                egui::Stroke::new(1.0_f32, EditorTheme::BACKGROUND),
            );
        }
    });
    let mut remove = None;
    for (index, key) in emitter.size_over_life.iter_mut().enumerate() {
        ui.push_id(("size_row", index), |ui| {
            widgets::property_row(ui, &format!("Key {}", index + 1), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let width = ((ui.available_width() - 26.0) / 2.0).max(28.0);
                changed |= ui
                    .add_sized(
                        [width, EditorTheme::ROW_HEIGHT],
                        DragValue::new(&mut key.t)
                            .range(0.0..=1.0)
                            .speed(0.01)
                            .prefix("t "),
                    )
                    .changed();
                changed |= ui
                    .add_sized(
                        [width, EditorTheme::ROW_HEIGHT],
                        DragValue::new(&mut key.value)
                            .range(0.0..=f32::MAX)
                            .speed(0.01)
                            .prefix("× "),
                    )
                    .changed();
                if ui.small_button("×").on_hover_text("Remove key").clicked() {
                    remove = Some(index);
                }
            });
        });
    }
    if let Some(index) = remove {
        emitter.size_over_life.remove(index);
        changed = true;
    }
    if changed {
        sort_keys(&mut emitter.size_over_life, |k| k.t);
    }
    changed
}

fn paint_checker(ui: &Ui, rect: egui::Rect) {
    let cell = rect.height() / 2.0;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, EditorTheme::RADIUS, EditorTheme::BUTTON);
    let columns = (rect.width() / cell).ceil() as usize;
    for column in 0..columns {
        for row in 0..2 {
            if (column + row) % 2 == 0 {
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        rect.min
                            + egui::vec2(
                                column as f32 * cell,
                                row as f32 * cell,
                            ),
                        egui::vec2(cell, cell),
                    ),
                    0.0,
                    EditorTheme::BUTTON_HOVER,
                );
            }
        }
    }
}

fn marker(ui: &Ui, rect: egui::Rect, color: [f32; 4], hovered: bool) {
    let mut opaque = color;
    opaque[3] = 1.0;
    ui.painter().rect(
        rect,
        2.0,
        color32(opaque),
        egui::Stroke::new(
            if hovered { 2.0_f32 } else { 1.0 },
            EditorTheme::TEXT_STRONG,
        ),
        egui::StrokeKind::Middle,
    );
}

/// Range a key may move in without passing its neighbours.
fn neighbours<K>(
    keys: &[K],
    index: usize,
    t: impl Fn(&K) -> f32,
) -> (f32, f32) {
    let low = index.checked_sub(1).map_or(0.0, |i| t(&keys[i]));
    let high = keys.get(index + 1).map_or(1.0, &t);
    (low, high)
}

fn sort_keys<K>(keys: &mut [K], t: impl Fn(&K) -> f32) {
    keys.sort_by(|a, b| t(a).total_cmp(&t(b)));
}

/// Inserts a key at `t` holding the color the gradient shows there, so the
/// gradient looks the same until the key is edited.
pub(super) fn add_color_key(keys: &mut Vec<ColorKey>, t: f32) {
    let t = t.clamp(0.0, 1.0);
    let probe = ParticleEmitter {
        color_over_life: keys.clone(),
        fade_in: 0.0,
        fade_out: 0.0,
        ..ParticleEmitter::default()
    };
    keys.push(ColorKey {
        t,
        color: probe.color_at(t),
    });
    sort_keys(keys, |k| k.t);
}

/// Inserts a size key at `t` on the current curve.
pub(super) fn add_size_key(keys: &mut Vec<CurveKey>, t: f32) {
    let t = t.clamp(0.0, 1.0);
    let probe = ParticleEmitter {
        size_over_life: keys.clone(),
        ..ParticleEmitter::default()
    };
    keys.push(CurveKey {
        t,
        value: probe.size_at(t),
    });
    sort_keys(keys, |k| k.t);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_keys_keep_the_curve_and_stay_sorted() {
        let mut colors = vec![
            ColorKey {
                t: 0.0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
            ColorKey {
                t: 1.0,
                color: [0.0, 0.0, 1.0, 0.0],
            },
        ];
        add_color_key(&mut colors, 0.5);
        assert_eq!(
            colors.iter().map(|k| k.t).collect::<Vec<_>>(),
            [0.0, 0.5, 1.0]
        );
        assert_eq!(colors[1].color, [0.5, 0.0, 0.5, 0.5]);

        let mut sizes = vec![CurveKey { t: 0.0, value: 2.0 }];
        add_size_key(&mut sizes, 2.0);
        assert_eq!(sizes[1], CurveKey { t: 1.0, value: 2.0 });
        assert_eq!(neighbours(&sizes, 0, |k| k.t), (0.0, 1.0));
        assert_eq!(neighbours(&sizes, 1, |k| k.t), (0.0, 1.0));
    }

    #[test]
    fn presets_replace_every_setting() {
        let mut emitter = ParticleEmitter::default();
        assert!(!apply_preset(&mut emitter, "no such effect"));
        assert_eq!(emitter, ParticleEmitter::default());
        assert!(apply_preset(&mut emitter, "snow"));
        assert_eq!(emitter, effect_preset("snow").unwrap().emitter());
    }

    #[test]
    fn every_section_draws_and_transport_reads_the_live_system() {
        let context = egui::Context::default();
        let mut emitter = effect_preset("fire").unwrap().emitter();
        let system = ParticleSystem {
            playing: true,
            ..ParticleSystem::default()
        };
        let mut command = None;
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                assert!(!draw_particle_emitter(
                    ui,
                    &mut emitter,
                    Some(&system),
                    &mut command,
                ));
            });
        });
        let texts = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for title in [
            "Preset",
            "Play",
            "Pause",
            "Restart",
            "Stop",
            "0 alive",
            "Emission",
            "Shape",
            "Lifetime",
            "Velocity",
            "Size",
            "Color",
            "Rendering",
            "Color Over Life",
            "Size Over Life",
        ] {
            assert!(texts.iter().any(|t| t == title), "{title}: {texts:?}");
        }
        assert_eq!(command, None);
    }
}
