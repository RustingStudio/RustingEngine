//! Dockable renderer and physics status views.

use egui::Ui;

use super::gui_elements::EditorTheme;
use super::inspector::widgets::{checkbox, choice, drag, section, value};
use crate::rendering::scene_renderer::{
    resolve_quality, CullingStats, RenderCapacityDiagnostics, RenderCounters,
    RendererCapabilities, ShaderErrors,
};
use crate::runtime::{
    Antialiasing, PhysicsBackendStatus, PhysicsSettings, RenderSettings,
    RenderWorld, ShadowQuality,
};

pub(super) fn draw_render_settings_area(
    ui: &mut Ui,
    settings: &mut RenderSettings,
    capabilities: Option<&RendererCapabilities>,
    culling: Option<&CullingStats>,
    capacity: Option<&RenderCapacityDiagnostics>,
    shader_errors: Option<&ShaderErrors>,
) {
    egui::ScrollArea::vertical()
        .id_salt("render_settings_area")
        .show(ui, |ui| {
            super::view::draw_scene_render_settings(ui, settings);
            section(ui, "Display", false, |ui| {
                checkbox(ui, "VSync", &mut settings.vsync);
                checkbox(ui, "Limit FPS", &mut settings.limit_fps);
                ui.add_enabled_ui(settings.limit_fps, |ui| {
                    drag(
                        ui,
                        "Maximum FPS",
                        egui::DragValue::new(&mut settings.max_fps)
                            .range(1..=1_000),
                    );
                });
            });
            // Session options like VSync: the scene file does not keep them.
            section(ui, "Quality", false, |ui| {
                choice(
                    ui,
                    "Anti-aliasing",
                    &mut settings.antialiasing,
                    &[
                        (Antialiasing::Auto, "Auto (by quality)"),
                        (Antialiasing::Off, "Off"),
                        (Antialiasing::Msaa2, "MSAA 2x"),
                        (Antialiasing::Msaa4, "MSAA 4x"),
                        (Antialiasing::Fxaa, "FXAA"),
                        (Antialiasing::Taa, "TAA"),
                    ],
                );
                choice(
                    ui,
                    "Shadows",
                    &mut settings.shadows,
                    &[
                        (ShadowQuality::Auto, "Auto (by quality)"),
                        (ShadowQuality::Low, "Low (1024)"),
                        (ShadowQuality::Medium, "Medium (2048)"),
                        (ShadowQuality::High, "High (4096)"),
                    ],
                );
                checkbox(ui, "Reflections", &mut settings.reflections);
                if let Some(caps) = capabilities {
                    value(
                        ui,
                        "MSAA in use",
                        &format!(
                            "{}x",
                            crate::rendering::scene_renderer::scene_sample_count(
                                settings.antialiasing,
                                resolve_quality(settings.quality, caps),
                                caps,
                            )
                        ),
                    );
                }
            });
            section(ui, "Device", false, |ui| {
                if let Some(caps) = capabilities {
                    value(ui, "GPU", &caps.device_name);
                    value(
                        ui,
                        "Resolved quality",
                        &format!(
                            "{:?}",
                            resolve_quality(settings.quality, caps)
                        ),
                    );
                    value(
                        ui,
                        "Max MSAA",
                        &format!("{}x", caps.msaa_samples),
                    );
                    value(
                        ui,
                        "GPU timestamps",
                        if caps.timestamp_queries {
                            "Available"
                        } else {
                            "Unavailable"
                        },
                    );
                } else {
                    ui.colored_label(
                        EditorTheme::TEXT_MUTED,
                        "GPU device information unavailable",
                    );
                }
            });
            section(ui, "Rendering diagnostics", false, |ui| {
                if let Some(stats) = culling {
                    value(ui, "Culling path", &format!("{:?}", stats.path));
                    value(ui, "Submitted", &stats.submitted.to_string());
                    value(ui, "Visible", &stats.visible.to_string());
                    value(ui, "Culled", &stats.culled.to_string());
                }
                if let Some(capacity) = capacity {
                    value(
                        ui,
                        "Dropped lights",
                        &capacity.dropped_lights.to_string(),
                    );
                    value(
                        ui,
                        "Missing meshes",
                        &capacity.missing_meshes.to_string(),
                    );
                    value(
                        ui,
                        "Missing materials",
                        &capacity.missing_materials.to_string(),
                    );
                    value(
                        ui,
                        "Missing textures",
                        &capacity.missing_textures.to_string(),
                    );
                }
            });
            // Shaders rebuild when their source changes, so this list
            // follows edits made in the Inspector.
            if let Some(ShaderErrors(errors)) =
                shader_errors.filter(|errors| !errors.0.is_empty())
            {
                section(ui, "Shader errors", false, |ui| {
                    for error in errors {
                        ui.colored_label(EditorTheme::ERROR, error);
                    }
                });
            }
        });
}

pub(super) fn draw_physics_diagnostics_area(
    ui: &mut Ui,
    backends: PhysicsBackendStatus,
    settings: Option<&PhysicsSettings>,
    render_world: Option<&RenderWorld>,
    counters: Option<&RenderCounters>,
    capacity: Option<&RenderCapacityDiagnostics>,
) {
    egui::ScrollArea::vertical()
        .id_salt("physics_diagnostics_area")
        .show(ui, |ui| {
            section(ui, "Simulation", false, |ui| {
                value(
                    ui,
                    "Gameplay backend",
                    if backends.gameplay_available {
                        "Available"
                    } else {
                        "Unavailable"
                    },
                );
                value(
                    ui,
                    "GPU dynamic backend",
                    if backends.gpu_dynamic_available {
                        "Available"
                    } else {
                        "Unavailable"
                    },
                );
                if let Some(settings) = settings {
                    value(
                        ui,
                        "Enabled",
                        if settings.enabled { "Yes" } else { "No" },
                    );
                    value(
                        ui,
                        "Gravity",
                        &format!(
                            "{:.2}, {:.2}, {:.2}",
                            settings.gravity[0],
                            settings.gravity[1],
                            settings.gravity[2]
                        ),
                    );
                }
                if let Some(render_world) = render_world {
                    value(
                        ui,
                        "Fixed tick",
                        &render_world.physics_tick.to_string(),
                    );
                    value(
                        ui,
                        "GPU bodies",
                        &render_world.gpu_physics.len().to_string(),
                    );
                    value(
                        ui,
                        "CPU/static colliders",
                        &render_world.gpu_colliders.len().to_string(),
                    );
                }
            });
            section(ui, "GPU work and readback", false, |ui| {
                if let Some(counters) = counters {
                    value(
                        ui,
                        "Physics dispatches",
                        &counters.physics_dispatches.to_string(),
                    );
                    value(
                        ui,
                        "Commands",
                        &counters.physics_commands.to_string(),
                    );
                    value(
                        ui,
                        "Command bytes",
                        &counters.physics_command_bytes.to_string(),
                    );
                    value(
                        ui,
                        "Event bytes",
                        &counters.physics_event_bytes.to_string(),
                    );
                    value(
                        ui,
                        "State bytes",
                        &counters.physics_state_bytes.to_string(),
                    );
                    value(
                        ui,
                        "Readback latency (frames)",
                        &counters.physics_readback_latency_frames.to_string(),
                    );
                }
            });
            section(ui, "Capacity and fallback", false, |ui| {
                if let Some(capacity) = capacity {
                    value(
                        ui,
                        "Events dropped (lifetime)",
                        &capacity.physics_events_dropped.to_string(),
                    );
                    value(
                        ui,
                        "Commands rejected (lifetime)",
                        &capacity.physics_commands_rejected.to_string(),
                    );
                    value(
                        ui,
                        "Grid overflow",
                        &capacity.physics_grid_overflow.to_string(),
                    );
                    value(
                        ui,
                        "Oversized bodies",
                        &capacity.physics_oversized_bodies.to_string(),
                    );
                    value(
                        ui,
                        "Grid hash collisions",
                        &capacity.physics_grid_hash_collisions.to_string(),
                    );
                    value(
                        ui,
                        "Fallback pair tests",
                        &capacity.physics_fallback_tests.to_string(),
                    );
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_expose_missing_assets_and_physics_overflow() {
        let context = egui::Context::default();
        let capacity = RenderCapacityDiagnostics {
            missing_meshes: 2,
            missing_materials: 3,
            missing_textures: 4,
            physics_grid_overflow: 5,
            physics_events_dropped: 6,
            ..Default::default()
        };
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                draw_render_settings_area(
                    ui,
                    &mut RenderSettings::default(),
                    None,
                    None,
                    Some(&capacity),
                    Some(&ShaderErrors(vec![
                        "surface: 3: error: 'oops' : undeclared".into(),
                    ])),
                );
                draw_physics_diagnostics_area(
                    ui,
                    PhysicsBackendStatus::default(),
                    Some(&PhysicsSettings::default()),
                    Some(&RenderWorld::default()),
                    Some(&RenderCounters::default()),
                    Some(&capacity),
                );
            });
        });
        let text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Missing meshes"), "{text}");
        assert!(text.contains("Grid overflow"), "{text}");
        assert!(text.contains("Events dropped"), "{text}");
        assert!(text.contains("Shader errors"), "{text}");
        assert!(text.contains("'oops' : undeclared"), "{text}");
    }
}
