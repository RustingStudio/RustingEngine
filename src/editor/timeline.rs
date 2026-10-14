//! Timeline area: a Blender-style dope sheet for the `rusting.animation`
//! clips of the selected object or its nearest animated parent. It picks
//! and edits clips, plays and scrubs them in the Scene view, and adds,
//! moves and deletes keys. Record mode keys every transform edit at the
//! playhead.
//!
//! Clip edits return as `ComponentEdit::Set`, so each one is a snapshot Undo
//! step. Scrubbing poses the real objects; their rest pose is kept in
//! `PreviewRestPose`, which saves and Undo snapshots write instead, and put
//! back when the selection leaves the animation and before Play.

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Resource, World};
use egui::{DragValue, Sense, Ui};

use super::dock::{EditorDockNode, EditorPanel};
use super::gui_elements::EditorTheme;
use super::inspector::ComponentEdit;
use super::{EditorMode, EditorState};
use crate::runtime::{
    apply_preview_pose, find_target, Animation, AnimationClip, AnimationMarker,
    AnimationProperty, AnimationTrack, Children, Interpolation, Keyframe, Name,
    Parent, PreviewRestPose, SceneId, TweenRepeat, Visibility,
    ANIMATION_COMPONENT,
};
use crate::Transform;

/// Keys snap to the fixed tick.
const SNAP: f32 = 1.0 / 60.0;
const NAMES_WIDTH: f32 = 190.0;

/// What the Timeline area shows; lives in [`EditorState`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TimelineState {
    pub clip: usize,
    /// Playhead in clip seconds.
    pub time: f32,
    pub playing: bool,
    /// Transform edits add keys at the playhead.
    pub record: bool,
    /// Selected key as (track, key).
    pub selected: Option<(usize, usize)>,
}

/// What the Timeline last posed. The rest pose lives in
/// [`PreviewRestPose`], where scene documents read it.
#[derive(Resource, Default)]
pub(super) struct TimelinePose {
    root: Option<SceneId>,
    /// Transforms right after the last pose, to spot user edits.
    posed: Vec<(SceneId, Transform)>,
    applied: Option<(usize, f32, AnimationClip)>,
}

/// The selected object, or its nearest parent, that has an [`Animation`].
pub(super) fn animated_root(world: &World, entity: Entity) -> Option<Entity> {
    let mut at = entity;
    loop {
        if world.get::<Animation>(at).is_some() {
            return Some(at);
        }
        at = world.get::<Parent>(at)?.0;
    }
}

/// The child name path from `root` to `entity`, as tracks store it.
fn target_path(world: &World, root: Entity, entity: Entity) -> Option<String> {
    let mut names = Vec::new();
    let mut at = entity;
    while at != root {
        names.push(world.get::<Name>(at)?.0.clone());
        at = world.get::<Parent>(at)?.0;
    }
    names.reverse();
    let path = names.join("/");
    // Two children with one name: the first wins, so keying the second
    // would move the wrong object.
    (find_target(world, root, &path) == Some(entity)).then_some(path)
}

fn subtree(world: &World, root: Entity) -> Vec<Entity> {
    let mut all = vec![root];
    let mut next = 0;
    while let Some(&at) = all.get(next) {
        if let Some(children) = world.get::<Children>(at) {
            all.extend(children.0.iter().copied());
        }
        next += 1;
    }
    all
}

fn by_id(world: &mut World, id: SceneId) -> Option<Entity> {
    let mut query = world.query::<(Entity, &SceneId)>();
    query
        .iter(world)
        .find(|(_, current)| **current == id)
        .map(|(entity, _)| entity)
}

fn posed_transforms(world: &World, root: Entity) -> Vec<(SceneId, Transform)> {
    subtree(world, root)
        .into_iter()
        .filter_map(|e| {
            Some((*world.get::<SceneId>(e)?, *world.get::<Transform>(e)?))
        })
        .collect()
}

/// Puts back the rest pose of every object the Timeline posed. Call before
/// the game starts from the live scene.
pub(super) fn restore_rest_pose(world: &mut World) {
    let mut pose = world.remove_resource::<TimelinePose>().unwrap_or_default();
    restore(world, &mut pose);
    world.insert_resource(pose);
}

fn restore(world: &mut World, pose: &mut TimelinePose) {
    let rest = world
        .remove_resource::<PreviewRestPose>()
        .map(|rest| rest.0)
        .unwrap_or_default();
    for (id, transform, visibility) in rest {
        let Some(entity) = by_id(world, id) else {
            continue;
        };
        let mut entity = world.entity_mut(entity);
        if entity.get::<Transform>() != Some(&transform) {
            entity.insert(transform);
        }
        match visibility {
            Some(visibility)
                if entity.get::<Visibility>() != Some(&visibility) =>
            {
                entity.insert(visibility);
            }
            Some(_) => {}
            None => {
                entity.remove::<Visibility>();
            }
        }
    }
    pose.posed.clear();
    pose.applied = None;
}

/// Adds or replaces the key at `time` on the (target, property) track.
pub(super) fn set_key(
    clip: &mut AnimationClip,
    target: &str,
    property: AnimationProperty,
    time: f32,
    value: Vec<f32>,
) {
    let index = clip
        .tracks
        .iter()
        .position(|t| t.target == target && t.property == property)
        .unwrap_or_else(|| {
            clip.tracks.push(AnimationTrack {
                target: target.into(),
                property,
                interpolation: Interpolation::Linear,
                keys: Vec::new(),
            });
            clip.tracks.len() - 1
        });
    let keys = &mut clip.tracks[index].keys;
    match keys
        .iter_mut()
        .find(|key| (key.time - time).abs() < SNAP * 0.5)
    {
        Some(key) => key.value = value,
        None => {
            keys.push(Keyframe { time, value });
            keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        }
    }
}

fn transform_keys(transform: &Transform) -> [(AnimationProperty, Vec<f32>); 3] {
    [
        (AnimationProperty::Position, transform.position.to_vec()),
        (AnimationProperty::Rotation, transform.rotation.to_vec()),
        (AnimationProperty::Scale, transform.scale.to_vec()),
    ]
}

fn timeline_shown(layout: &EditorDockNode) -> bool {
    match layout {
        EditorDockNode::Area { panel, .. } => *panel == EditorPanel::Timeline,
        EditorDockNode::Split { first, second, .. } => {
            timeline_shown(first) || timeline_shown(second)
        }
    }
}

/// Per editor frame, after edits are applied: keys record-mode edits and
/// poses the animation at the playhead. Only while the Timeline is shown
/// and the scene is stopped.
pub(super) fn update_timeline(world: &mut World, state: &mut EditorState) {
    let mut pose = world.remove_resource::<TimelinePose>().unwrap_or_default();
    let root = state
        .selected
        .filter(|_| {
            state.mode == EditorMode::Edit && timeline_shown(&state.dock_layout)
        })
        .and_then(|entity| animated_root(world, entity));
    let root_id = root.and_then(|root| world.get::<SceneId>(root).copied());
    if pose.root != root_id {
        restore(world, &mut pose);
        // Another animation starts at its first clip; a new selection
        // keeps the playhead.
        if pose.root.is_some() && root_id.is_some() {
            state.timeline = TimelineState {
                record: state.timeline.record,
                ..TimelineState::default()
            };
        }
        pose.root = root_id;
    }
    if let Some(root) = root {
        pose_root(world, state, &mut pose, root);
    }
    world.insert_resource(pose);
}

fn pose_root(
    world: &mut World,
    state: &mut EditorState,
    pose: &mut TimelinePose,
    root: Entity,
) {
    let Some(mut animation) = world.get::<Animation>(root).cloned() else {
        return;
    };
    let timeline = &mut state.timeline;
    timeline.clip = timeline.clip.min(animation.clips.len().saturating_sub(1));
    let Some(clip) = animation.clips.get_mut(timeline.clip) else {
        return;
    };
    let mut keyed = false;
    for (id, posed) in std::mem::take(&mut pose.posed) {
        let Some(entity) = by_id(world, id) else {
            continue;
        };
        let Some(current) = world.get::<Transform>(entity).copied() else {
            continue;
        };
        if current == posed {
            continue;
        }
        if !timeline.record {
            // A plain edit changes the rest pose too, so it survives.
            if let Some(mut rest) = world.get_resource_mut::<PreviewRestPose>()
            {
                if let Some(rest) = rest.0.iter_mut().find(|rest| rest.0 == id)
                {
                    rest.1 = current;
                }
            }
            continue;
        }
        let Some(path) = target_path(world, root, entity) else {
            continue;
        };
        let before = transform_keys(&posed);
        for ((property, value), (_, old)) in
            transform_keys(&current).into_iter().zip(before)
        {
            if value != old {
                set_key(clip, &path, property, timeline.time, value);
                keyed = true;
            }
        }
    }
    let clip = clip.clone();
    if keyed {
        // The transform edit that caused this already took the Undo
        // snapshot, so the key undoes with it.
        if let Some(mut live) = world.get_mut::<Animation>(root) {
            *live = animation.clone();
        }
        state.scene_dirty = true;
    }
    let wanted = (state.timeline.clip, state.timeline.time, clip);
    if pose.applied.as_ref() == Some(&wanted) {
        pose.posed = posed_transforms(world, root);
        return;
    }
    if !world.contains_resource::<PreviewRestPose>() {
        let rest = subtree(world, root)
            .into_iter()
            .filter_map(|e| {
                Some((
                    *world.get::<SceneId>(e)?,
                    *world.get::<Transform>(e)?,
                    world.get::<Visibility>(e).copied(),
                ))
            })
            .collect();
        world.insert_resource(PreviewRestPose(rest));
    }
    apply_preview_pose(world, root, &animation, &wanted.2, wanted.1);
    pose.posed = posed_transforms(world, root);
    pose.applied = Some(wanted);
}

/// The scene was replaced (Undo, Redo) from a document that already holds
/// the rest pose: forget the old pose and pose again from scratch.
pub(super) fn scene_replaced(world: &mut World) {
    world.remove_resource::<PreviewRestPose>();
    if let Some(mut pose) = world.get_resource_mut::<TimelinePose>() {
        pose.posed.clear();
        pose.applied = None;
    }
}

/// Draws the Timeline area. Clip edits push one `ComponentEdit::Set`.
pub(super) fn draw_timeline_area(
    ui: &mut Ui,
    world: &World,
    state: &mut EditorState,
    edits: &mut Vec<ComponentEdit>,
) {
    let Some(selected) = state.selected else {
        ui.colored_label(
            EditorTheme::TEXT_MUTED,
            "Select an object to animate it.",
        );
        return;
    };
    let Some(root) = animated_root(world, selected) else {
        ui.horizontal(|ui| {
            ui.colored_label(
                EditorTheme::TEXT_MUTED,
                "No animation on this object.",
            );
            if EditorTheme::toolbar_button(ui, "+ Add Animation", false, true)
                .clicked()
            {
                edits.push(ComponentEdit::Add {
                    entity: selected,
                    name: ANIMATION_COMPONENT.into(),
                });
            }
        });
        return;
    };
    let Some(before) = world.get::<Animation>(root) else {
        return;
    };
    let mut animation = before.clone();
    let path = target_path(world, root, selected);
    let transform = world.get::<Transform>(selected).copied();
    let names: Vec<String> = animation
        .clips
        .iter()
        .flat_map(|clip| &clip.tracks)
        .map(|track| track_label(world, root, track))
        .collect();
    draw_clip(
        ui,
        &mut animation,
        &mut state.timeline,
        path.as_deref().zip(transform.as_ref()),
        &names,
    );
    if animation != *before {
        if let Ok(value) = serde_json::to_string(&animation) {
            edits.push(ComponentEdit::Set {
                entity: root,
                name: ANIMATION_COMPONENT.into(),
                value,
            });
        }
    }
}

fn track_label(world: &World, root: Entity, track: &AnimationTrack) -> String {
    let object = if track.target.is_empty() {
        world
            .get::<Name>(root)
            .map_or("Self".into(), |n| n.0.clone())
    } else {
        track.target.clone()
    };
    let property = match &track.property {
        AnimationProperty::Field { component, path } => {
            format!("{}{path}", component.trim_start_matches("rusting."))
        }
        other => format!("{other:?}"),
    };
    format!("{object} · {property}")
}

/// Header and dope sheet for one animation. `key_source` is the selected
/// object's track path and transform, for Insert Key.
pub(super) fn draw_clip(
    ui: &mut Ui,
    animation: &mut Animation,
    timeline: &mut TimelineState,
    key_source: Option<(&str, &Transform)>,
    names: &[String],
) {
    if animation.clips.is_empty() {
        animation.clips.push(AnimationClip::default());
    }
    timeline.clip = timeline.clip.min(animation.clips.len() - 1);
    header(ui, animation, timeline, key_source);
    let label_start: usize = animation.clips[..timeline.clip]
        .iter()
        .map(|clip| clip.tracks.len())
        .sum();
    let clip = &mut animation.clips[timeline.clip];
    if timeline.playing {
        let dt = ui.input(|input| input.stable_dt).min(0.1);
        let length = clip.length().max(SNAP);
        timeline.time += dt * animation.speed.max(0.0);
        if timeline.time > length {
            if clip.repeat == TweenRepeat::Once {
                timeline.time = length;
                timeline.playing = false;
            } else {
                timeline.time %= length;
            }
        }
        ui.ctx().request_repaint();
    }
    ui.separator();
    sheet(ui, clip, timeline, &names[label_start.min(names.len())..]);
}

fn header(
    ui: &mut Ui,
    animation: &mut Animation,
    timeline: &mut TimelineState,
    key_source: Option<(&str, &Transform)>,
) {
    ui.horizontal_wrapped(|ui| {
        let current = animation.clips[timeline.clip].name.clone();
        EditorTheme::toolbar_combo_box(ui, "timeline_clip", &current, 120.0, |ui| {
            for (index, clip) in animation.clips.iter().enumerate() {
                if EditorTheme::menu_choice(ui, &clip.name, index == timeline.clip, true)
                    .clicked()
                {
                    *timeline = TimelineState {
                        clip: index,
                        record: timeline.record,
                        ..TimelineState::default()
                    };
                }
            }
        });
        if EditorTheme::toolbar_button(ui, "+", false, true)
            .on_hover_text("New clip")
            .clicked()
        {
            let name = (1..)
                .map(|n| format!("clip {n}"))
                .find(|name| animation.clip(name).is_none())
                .unwrap_or_default();
            animation.clips.push(AnimationClip { name, ..AnimationClip::default() });
            timeline.clip = animation.clips.len() - 1;
            timeline.selected = None;
        }
        if EditorTheme::toolbar_button(ui, "−", false, animation.clips.len() > 1)
            .on_hover_text("Delete clip")
            .clicked()
        {
            animation.clips.remove(timeline.clip);
            timeline.clip = timeline.clip.saturating_sub(1);
            timeline.selected = None;
        }
        let clip = &mut animation.clips[timeline.clip];
        let old_name = clip.name.clone();
        ui.add(egui::TextEdit::singleline(&mut clip.name).desired_width(80.0));
        if clip.name != old_name && animation.autoplay == old_name {
            animation.autoplay = clip.name.clone();
        }
        let clip = &mut animation.clips[timeline.clip];
        let repeat = format!("{:?}", clip.repeat);
        EditorTheme::toolbar_combo_box(ui, "timeline_repeat", &repeat, 80.0, |ui| {
            for mode in [TweenRepeat::Once, TweenRepeat::Loop, TweenRepeat::PingPong] {
                if EditorTheme::menu_choice(ui, &format!("{mode:?}"), clip.repeat == mode, true)
                    .clicked()
                {
                    clip.repeat = mode;
                }
            }
        });
        ui.add(
            DragValue::new(&mut clip.duration)
                .range(0.0..=3600.0)
                .speed(0.05)
                .suffix(" s")
                .custom_formatter(|value, _| {
                    if value <= 0.0 { "auto".into() } else { format!("{value:.2} s") }
                }),
        )
        .on_hover_text("Clip length; auto ends at the last key");
        let mut autoplay = animation.autoplay == clip.name;
        if ui.checkbox(&mut autoplay, "Autoplay").changed() {
            animation.autoplay = if autoplay { clip.name.clone() } else { String::new() };
        }
        ui.separator();
        if EditorTheme::toolbar_button(ui, "⏮", false, true).clicked() {
            timeline.time = 0.0;
        }
        let label = if timeline.playing { "Pause" } else { "Play" };
        if EditorTheme::toolbar_button(ui, label, timeline.playing, true).clicked() {
            timeline.playing = !timeline.playing;
        }
        if EditorTheme::toolbar_button(ui, "Stop", false, true).clicked() {
            timeline.playing = false;
            timeline.time = 0.0;
        }
        ui.monospace(format!("{:6.2} s", timeline.time));
        ui.separator();
        if EditorTheme::toolbar_button(ui, "● Record", timeline.record, true)
            .on_hover_text("Moving, turning or scaling an object adds keys at the playhead")
            .clicked()
        {
            timeline.record = !timeline.record;
        }
        let clip = &mut animation.clips[timeline.clip];
        EditorTheme::toolbar_menu(ui, "timeline_key", "Insert Key", 90.0, 140.0, |ui| {
            let Some((path, transform)) = key_source else {
                ui.colored_label(EditorTheme::TEXT_MUTED, "Name the object first");
                return;
            };
            let keys = transform_keys(transform);
            for (label, which) in [("Location", 0), ("Rotation", 1), ("Scale", 2), ("All", 3)] {
                if EditorTheme::menu_action(ui, label, true).clicked() {
                    for (index, (property, value)) in keys.iter().cloned().enumerate() {
                        if which == 3 || which == index {
                            set_key(clip, path, property, timeline.time, value);
                        }
                    }
                }
            }
        });
        let selected = timeline.selected.filter(|(track, key)| {
            clip.tracks.get(*track).is_some_and(|t| *key < t.keys.len())
        });
        if EditorTheme::toolbar_button(ui, "Delete Key", false, selected.is_some()).clicked() {
            if let Some((track, key)) = selected {
                clip.tracks[track].keys.remove(key);
                timeline.selected = None;
            }
        }
        if EditorTheme::toolbar_button(ui, "+ Event", false, true)
            .on_hover_text("Marker sent to game code; rename it in the Inspector")
            .clicked()
        {
            clip.events.push(AnimationMarker { time: timeline.time, name: "event".into() });
        }
    });
}

fn snap(t: f32) -> f32 {
    (t / SNAP).round() * SNAP
}

fn sheet(
    ui: &mut Ui,
    clip: &mut AnimationClip,
    timeline: &mut TimelineState,
    names: &[String],
) {
    let span = clip.length().max(1.0) * 1.1;
    let row = EditorTheme::ROW_HEIGHT;
    // Ruler.
    let (ruler, lane_width) = ui
        .horizontal(|ui| {
            ui.add_space(NAMES_WIDTH);
            let width = ui.available_width().max(60.0);
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(width, 20.0),
                Sense::click_and_drag(),
            );
            if let Some(pointer) = response.interact_pointer_pos() {
                let t = (pointer.x - rect.left()) / rect.width() * span;
                timeline.time = snap(t.clamp(0.0, span));
                timeline.playing = false;
            }
            (rect, width)
        })
        .inner;
    let x_of = |left: f32, t: f32| left + t / span * lane_width;
    let painter = ui.painter();
    painter.rect_filled(ruler, 0.0, EditorTheme::PANEL_RAISED);
    let step = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0]
        .into_iter()
        .find(|step| step / span * lane_width >= 48.0)
        .unwrap_or(60.0);
    let mut t = 0.0;
    while t <= span {
        let x = x_of(ruler.left(), t);
        painter.line_segment(
            [
                egui::pos2(x, ruler.bottom() - 5.0),
                egui::pos2(x, ruler.bottom()),
            ],
            egui::Stroke::new(1.0_f32, EditorTheme::BORDER),
        );
        painter.text(
            egui::pos2(x + 2.0, ruler.top() + 2.0),
            egui::Align2::LEFT_TOP,
            format!("{t}"),
            egui::FontId::proportional(10.0),
            EditorTheme::TEXT_MUTED,
        );
        t += step;
    }
    for marker in &clip.events {
        let x = x_of(ruler.left(), marker.time);
        let tip = egui::pos2(x, ruler.bottom());
        painter.add(egui::Shape::convex_polygon(
            vec![
                tip,
                tip + egui::vec2(-4.0, -7.0),
                tip + egui::vec2(4.0, -7.0),
            ],
            EditorTheme::WARNING,
            egui::Stroke::NONE,
        ));
    }
    let mut bottom = ruler.bottom();
    let mut remove = None;
    let mut moved = None;
    egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
        if clip.tracks.is_empty() {
            ui.colored_label(
                EditorTheme::TEXT_MUTED,
                "No tracks. Use Insert Key, or turn on Record and move the object.",
            );
        }
        for (index, track) in clip.tracks.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(NAMES_WIDTH, row),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_width(NAMES_WIDTH);
                        if EditorTheme::toolbar_button(ui, "×", false, true)
                            .on_hover_text("Delete track")
                            .clicked()
                        {
                            remove = Some(index);
                        }
                        let mode = match track.interpolation {
                            Interpolation::Step => "Step",
                            Interpolation::Linear => "Lin",
                            Interpolation::Smooth => "Smooth",
                        };
                        if EditorTheme::toolbar_button(ui, mode, false, true)
                            .on_hover_text("Interpolation: click to change")
                            .clicked()
                        {
                            track.interpolation = match track.interpolation {
                                Interpolation::Step => Interpolation::Linear,
                                Interpolation::Linear => Interpolation::Smooth,
                                Interpolation::Smooth => Interpolation::Step,
                            };
                        }
                        let name = names.get(index).map_or("", String::as_str);
                        ui.add(egui::Label::new(name).truncate());
                    },
                );
                let (lane, response) = ui.allocate_exact_size(egui::vec2(lane_width, row), Sense::click());
                bottom = lane.bottom();
                let painter = ui.painter();
                painter.rect_filled(lane, 0.0, EditorTheme::INPUT);
                painter.line_segment(
                    [lane.left_bottom(), lane.right_bottom()],
                    egui::Stroke::new(1.0_f32, EditorTheme::BORDER_SOFT),
                );
                if let Some(pointer) = response.interact_pointer_pos().filter(|_| response.clicked()) {
                    timeline.time = snap(((pointer.x - lane.left()) / lane_width * span).clamp(0.0, span));
                    timeline.selected = None;
                    timeline.playing = false;
                }
                for key in 0..track.keys.len() {
                    let center = egui::pos2(x_of(lane.left(), track.keys[key].time), lane.center().y);
                    let rect = egui::Rect::from_center_size(center, egui::vec2(11.0, 11.0));
                    let response = ui.interact(rect, ui.id().with(("timeline_key", index, key)), Sense::click_and_drag());
                    if response.clicked() || response.drag_started() {
                        timeline.selected = Some((index, key));
                    }
                    if response.dragged() {
                        if let Some(pointer) = response.interact_pointer_pos() {
                            let t = snap(((pointer.x - lane.left()) / lane_width * span).clamp(0.0, span));
                            track.keys[key].time = t;
                            moved = Some((index, key));
                        }
                    }
                    let selected = timeline.selected == Some((index, key));
                    let r = 5.0;
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![
                            center + egui::vec2(0.0, -r),
                            center + egui::vec2(r, 0.0),
                            center + egui::vec2(0.0, r),
                            center + egui::vec2(-r, 0.0),
                        ],
                        if selected { EditorTheme::WARNING } else { EditorTheme::TEXT },
                        egui::Stroke::new(1.0_f32, EditorTheme::BACKGROUND),
                    ));
                }
            });
        }
    });
    if let Some((track, key)) = moved {
        let keys = &mut clip.tracks[track].keys;
        let moving = keys[key].clone();
        keys.sort_by(|a, b| a.time.total_cmp(&b.time));
        let index = keys.iter().position(|k| *k == moving).unwrap_or(key);
        timeline.selected = Some((track, index));
    }
    if let Some(track) = remove {
        clip.tracks.remove(track);
        timeline.selected = None;
    }
    let x = x_of(ruler.left(), timeline.time);
    ui.painter().line_segment(
        [
            egui::pos2(x, ruler.top()),
            egui::pos2(x, bottom.max(ruler.bottom())),
        ],
        egui::Stroke::new(1.5_f32, EditorTheme::ACCENT),
    );
}
