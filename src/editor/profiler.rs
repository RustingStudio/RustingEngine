//! Frame history and the dockable editor profiler.

use std::collections::VecDeque;
use std::time::Duration;

use bevy_ecs::prelude::{Resource, World};

use super::gui_elements::EditorTheme;
use super::inspector::widgets;
use crate::rendering::scene_renderer::{
    CullingStats, GpuPassTimes, RenderCapacityDiagnostics, RenderCounters,
};
use crate::runtime::CpuFrameTimings;

const HISTORY_FRAMES: usize = 180;

#[derive(Clone, Debug)]
struct ProfilerSample {
    cpu: CpuFrameTimings,
    gpu: GpuPassTimes,
    counters: RenderCounters,
    culling: CullingStats,
    capacity: RenderCapacityDiagnostics,
}

impl ProfilerSample {
    fn measured_cpu(&self) -> Duration {
        self.cpu.physics
            + self.cpu.extraction
            + self.cpu.preparation
            + self.cpu.recording
            + self.cpu.editor
    }
}

/// Recent editor frames. GPU timestamps describe the latest completed GPU
/// frame and may lag the CPU sample by one or more submissions.
#[derive(Resource, Default)]
pub struct EditorProfiler {
    samples: VecDeque<ProfilerSample>,
    paused: bool,
}

impl EditorProfiler {
    fn record(&mut self, sample: ProfilerSample) {
        if self.paused {
            return;
        }
        if self.samples.len() == HISTORY_FRAMES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }
}

/// Called after the renderer publishes the last frame's diagnostics.
pub fn record_editor_profiler(world: &mut World) {
    let Some(cpu) = world.get_resource::<CpuFrameTimings>().copied() else {
        return;
    };
    let Some(counters) = world.get_resource::<RenderCounters>().copied() else {
        return;
    };
    let sample = ProfilerSample {
        cpu,
        gpu: world
            .get_resource::<GpuPassTimes>()
            .cloned()
            .unwrap_or_default(),
        counters,
        culling: world
            .get_resource::<CullingStats>()
            .copied()
            .unwrap_or_default(),
        capacity: world
            .get_resource::<RenderCapacityDiagnostics>()
            .copied()
            .unwrap_or_default(),
    };
    if let Some(mut profiler) = world.get_resource_mut::<EditorProfiler>() {
        profiler.record(sample);
    }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn draw_history(ui: &mut egui::Ui, profiler: &EditorProfiler) {
    ui.horizontal(|ui| {
        ui.colored_label(EditorTheme::ACCENT_HOVER, "CPU measured");
        ui.colored_label(EditorTheme::ACTIVE_OBJECT, "GPU completed");
    });
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(80.0), 108.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        f32::from(EditorTheme::RADIUS),
        EditorTheme::INPUT,
    );
    if profiler.samples.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Waiting for a rendered frame",
            egui::TextStyle::Body.resolve(ui.style()),
            EditorTheme::TEXT_MUTED,
        );
        return;
    }
    let peak = profiler.samples.iter().fold(16.667_f64, |peak, sample| {
        peak.max(milliseconds(sample.measured_cpu()))
            .max(milliseconds(sample.gpu.total()))
    }) as f32;
    let point = |index: usize, value: f32| {
        egui::pos2(
            rect.left()
                + rect.width() * index as f32 / (HISTORY_FRAMES - 1) as f32,
            rect.bottom() - rect.height() * (value / peak).clamp(0.0, 1.0),
        )
    };
    let target_y = point(0, 16.667).y;
    painter.line_segment(
        [
            egui::pos2(rect.left(), target_y),
            egui::pos2(rect.right(), target_y),
        ],
        egui::Stroke::new(1.0_f32, EditorTheme::BORDER),
    );
    for (color, values) in [
        (
            EditorTheme::ACCENT_HOVER,
            profiler
                .samples
                .iter()
                .map(|sample| Some(milliseconds(sample.measured_cpu()) as f32))
                .collect::<Vec<_>>(),
        ),
        (
            EditorTheme::ACTIVE_OBJECT,
            profiler
                .samples
                .iter()
                .map(|sample| {
                    (!sample.gpu.0.is_empty())
                        .then(|| milliseconds(sample.gpu.total()) as f32)
                })
                .collect::<Vec<_>>(),
        ),
    ] {
        let points = values
            .into_iter()
            .enumerate()
            .filter_map(|(index, value)| value.map(|value| point(index, value)))
            .collect::<Vec<_>>();
        if points.len() > 1 {
            painter.add(egui::Shape::line(
                points,
                egui::Stroke::new(1.5_f32, color),
            ));
        }
    }
    painter.text(
        rect.left_top() + egui::vec2(5.0, 3.0),
        egui::Align2::LEFT_TOP,
        format!("{peak:.1} ms"),
        egui::TextStyle::Small.resolve(ui.style()),
        EditorTheme::TEXT_MUTED,
    );
}

/// Displays CPU spans, the latest completed GPU passes, work counters, and
/// resident GPU allocation size from the renderer's allocator pools.
pub fn draw_profiler_area(ui: &mut egui::Ui, profiler: &mut EditorProfiler) {
    ui.horizontal(|ui| {
        if EditorTheme::toolbar_button(ui, "Pause", profiler.paused, true)
            .clicked()
        {
            profiler.paused = !profiler.paused;
        }
        if EditorTheme::toolbar_button(ui, "Clear", false, true).clicked() {
            profiler.samples.clear();
        }
        ui.colored_label(
            EditorTheme::TEXT_MUTED,
            format!("{} / {HISTORY_FRAMES} frames", profiler.samples.len()),
        );
    });
    egui::ScrollArea::vertical().show(ui, |ui| {
        draw_history(ui, profiler);
        let Some(sample) = profiler.samples.back() else {
            return;
        };
        ui.separator();
        ui.label(egui::RichText::new("CPU spans").strong());
        for (label, duration) in [
            ("Physics", sample.cpu.physics),
            ("Extraction", sample.cpu.extraction),
            ("Preparation", sample.cpu.preparation),
            ("Recording", sample.cpu.recording),
            ("Editor UI", sample.cpu.editor),
        ] {
            widgets::value(
                ui,
                label,
                &format!("{:.3} ms", milliseconds(duration)),
            );
        }
        ui.label(egui::RichText::new("GPU passes (latest completed)").strong());
        if sample.gpu.0.is_empty() {
            widgets::value(ui, "Timestamps", "Unavailable");
        } else {
            for (pass, duration) in &sample.gpu.0 {
                widgets::value(
                    ui,
                    pass.label(),
                    &format!("{:.3} ms", milliseconds(*duration)),
                );
            }
        }
        ui.label(egui::RichText::new("Work and memory").strong());
        for (label, value) in [
            ("Draws", sample.counters.draws.to_string()),
            ("Dispatches", sample.counters.dispatches.to_string()),
            ("Triangles", sample.counters.triangles.to_string()),
            ("Visible", sample.counters.visible_instances.to_string()),
            ("Culled", sample.culling.culled.to_string()),
            (
                "Uploads",
                format!(
                    "{:.2} MiB",
                    sample.counters.upload_bytes as f64 / 1_048_576.0
                ),
            ),
            (
                "GPU allocations",
                format!(
                    "{:.1} MiB",
                    sample.counters.gpu_memory_bytes as f64 / 1_048_576.0
                ),
            ),
            (
                "Grid overflow",
                sample.capacity.physics_grid_overflow.to_string(),
            ),
            (
                "Events lost",
                sample.capacity.physics_events_dropped.to_string(),
            ),
        ] {
            widgets::value(ui, label, &value);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_pause_freezes_the_visible_frame() {
        let mut profiler = EditorProfiler::default();
        for index in 0..HISTORY_FRAMES + 7 {
            profiler.record(ProfilerSample {
                cpu: CpuFrameTimings {
                    physics: Duration::from_micros(index as u64),
                    ..Default::default()
                },
                gpu: GpuPassTimes::default(),
                counters: RenderCounters::default(),
                culling: CullingStats::default(),
                capacity: RenderCapacityDiagnostics::default(),
            });
        }
        assert_eq!(profiler.samples.len(), HISTORY_FRAMES);
        assert_eq!(
            profiler.samples.front().unwrap().cpu.physics,
            Duration::from_micros(7)
        );
        profiler.paused = true;
        let last = profiler.samples.back().unwrap().cpu.physics;
        let sample = profiler.samples.back().unwrap().clone();
        profiler.record(sample);
        assert_eq!(profiler.samples.back().unwrap().cpu.physics, last);
    }

    #[test]
    fn profiler_area_shows_timing_and_memory_sections() {
        let mut profiler = EditorProfiler::default();
        profiler.record(ProfilerSample {
            cpu: CpuFrameTimings {
                physics: Duration::from_millis(2),
                ..Default::default()
            },
            gpu: GpuPassTimes::default(),
            counters: RenderCounters {
                gpu_memory_bytes: 8 * 1_048_576,
                ..Default::default()
            },
            culling: CullingStats::default(),
            capacity: RenderCapacityDiagnostics::default(),
        });
        let context = egui::Context::default();
        let output = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                draw_profiler_area(ui, &mut profiler);
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
            .join("\n");
        assert!(text.contains("CPU spans"), "{text}");
        assert!(text.contains("GPU passes"), "{text}");
        assert!(text.contains("GPU allocations"), "{text}");
        assert!(text.contains("8.0 MiB"), "{text}");
    }
}
