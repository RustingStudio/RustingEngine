//! Parent-first ordering used by the editor Hierarchy area.

use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::World;

use crate::runtime::{
    scene_document, GlobalTransform, Name, Parent, SceneId, SceneLoadMode,
};
use crate::runtime::{
    Camera, Collider, DirectionalLight, MeshRenderer, PhysicsBody, PointLight,
    RigidBody, SpotLight, Visibility,
};
use crate::Transform;
use nalgebra::Matrix4;
use std::collections::{HashMap, HashSet};

use super::{gui_elements, AssetRequest, EditorState, EntityRequest};

#[derive(Clone, Copy)]
struct HierarchyDrag(Entity);

/// One visible row in the expanded Hierarchy tree.
pub(super) struct HierarchyItem {
    pub(super) entity: Entity,
    pub(super) name: String,
    pub(super) depth: usize,
    pub(super) transform: Option<Transform>,
}

struct RawHierarchyItem {
    entity: Entity,
    name: String,
    parent: Option<Entity>,
    transform: Option<Transform>,
}

fn is_preview_spawned(state: &EditorState, entity: Entity) -> bool {
    state
        .preview_entities
        .as_ref()
        .is_some_and(|before| !before.contains(&entity))
}

/// Builds a parent-first tree where every child directly follows its parent.
pub(super) fn collect_entities(world: &mut World) -> Vec<HierarchyItem> {
    let mut query = world.query_filtered::<(
        Entity,
        Option<&Name>,
        Option<&Parent>,
        Option<&Transform>,
    ), (
        bevy_ecs::query::Without<crate::runtime::TileOf>,
        bevy_ecs::query::Without<crate::runtime::FluidParticle>,
    )>();
    let mut raw = query
        .iter(world)
        .filter(|(_, name, parent, transform)| {
            name.is_some() || parent.is_some() || transform.is_some()
        })
        .map(|(entity, name, parent, transform)| RawHierarchyItem {
            entity,
            name: name.map_or_else(
                || format!("Entity {entity:?}"),
                |name| name.0.clone(),
            ),
            parent: parent.map(|parent| parent.0),
            transform: transform.copied(),
        })
        .collect::<Vec<_>>();
    // Sorting once gives every group of siblings a stable readable order.
    raw.sort_by_cached_key(|item| {
        (item.name.to_ascii_lowercase(), item.entity)
    });

    let known = raw
        .iter()
        .map(|item| item.entity)
        .collect::<std::collections::HashSet<_>>();
    let mut children = std::collections::HashMap::<Entity, Vec<usize>>::new();
    let mut roots = Vec::new();
    for (index, item) in raw.iter().enumerate() {
        if let Some(parent) =
            item.parent.filter(|parent| known.contains(parent))
        {
            children.entry(parent).or_default().push(index);
        } else {
            roots.push(index);
        }
    }

    let mut visited = std::collections::HashSet::new();
    let mut ordered = Vec::with_capacity(raw.len());
    for root in roots {
        append_branch(&raw, &children, root, 0, &mut visited, &mut ordered);
    }
    // Runtime hierarchy validation rejects cycles, but this fallback keeps a
    // damaged external scene visible instead of silently dropping its rows.
    for index in 0..raw.len() {
        if !visited.contains(&raw[index].entity) {
            append_branch(
                &raw,
                &children,
                index,
                0,
                &mut visited,
                &mut ordered,
            );
        }
    }
    ordered
}

fn append_branch(
    raw: &[RawHierarchyItem],
    children: &std::collections::HashMap<Entity, Vec<usize>>,
    index: usize,
    depth: usize,
    visited: &mut std::collections::HashSet<Entity>,
    ordered: &mut Vec<HierarchyItem>,
) {
    let item = &raw[index];
    if !visited.insert(item.entity) {
        return;
    }
    ordered.push(HierarchyItem {
        entity: item.entity,
        name: item.name.clone(),
        depth,
        transform: item.transform,
    });
    if let Some(child_indexes) = children.get(&item.entity) {
        for child in child_indexes {
            append_branch(raw, children, *child, depth + 1, visited, ordered);
        }
    }
}

/// Rows matching `query` (case-insensitive name search) plus their ancestors,
/// so every match is shown in its place in the tree.
fn filter_items<'a>(
    items: &'a [HierarchyItem],
    query: &str,
) -> Vec<&'a HierarchyItem> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return items.iter().collect();
    }
    let mut keep = vec![false; items.len()];
    // Indexes of the current row's ancestors; items are parent-first.
    let mut path = Vec::new();
    for (index, item) in items.iter().enumerate() {
        path.truncate(item.depth);
        if item.name.to_lowercase().contains(&query) {
            keep[index] = true;
            for &ancestor in &path {
                keep[ancestor] = true;
            }
        }
        path.push(index);
    }
    items
        .iter()
        .zip(keep)
        .filter_map(|(item, keep)| keep.then_some(item))
        .collect()
}

/// Applies one row click to the selection and returns the new selection and
/// primary object. Ctrl toggles one row; Shift adds the visible range between
/// the primary object and the clicked row.
fn click_selection(
    selection: &[Entity],
    primary: Option<Entity>,
    visible: &[Entity],
    clicked: Entity,
    toggle: bool,
    range: bool,
) -> (Vec<Entity>, Option<Entity>) {
    let position = |entity| visible.iter().position(|&item| item == entity);
    if range {
        if let (Some(anchor), Some(end)) =
            (primary.and_then(position), position(clicked))
        {
            let mut next = selection.to_vec();
            for &entity in &visible[anchor.min(end)..=anchor.max(end)] {
                if !next.contains(&entity) {
                    next.push(entity);
                }
            }
            return (next, primary);
        }
    }
    if toggle {
        let mut next = selection.to_vec();
        if let Some(index) = next.iter().position(|&item| item == clicked) {
            next.remove(index);
            let primary = if primary == Some(clicked) {
                next.last().copied()
            } else {
                primary
            };
            return (next, primary);
        }
        next.push(clicked);
        return (next, Some(clicked));
    }
    (vec![clicked], Some(clicked))
}

/// Blender's Shift-click in the viewport: add an unselected object and make
/// it active, make a selected one active, or deselect the active one.
pub(super) fn shift_pick_selection(
    selection: &[Entity],
    primary: Option<Entity>,
    picked: Entity,
) -> (Vec<Entity>, Option<Entity>) {
    let mut next = selection.to_vec();
    if primary == Some(picked) {
        next.retain(|&entity| entity != picked);
        let primary = next.last().copied();
        return (next, primary);
    }
    if !next.contains(&picked) {
        next.push(picked);
    }
    (next, Some(picked))
}

/// Icon tint by object kind, so cameras, lights and meshes read apart.
fn icon_color(icon: super::EditorIcon) -> egui::Color32 {
    use super::EditorIcon;
    match icon {
        EditorIcon::Camera => gui_elements::EditorTheme::ICON_CAMERA,
        EditorIcon::Light => gui_elements::EditorTheme::WARNING,
        EditorIcon::Mesh => gui_elements::EditorTheme::LINK,
        _ => gui_elements::EditorTheme::TEXT_MUTED,
    }
}

fn entity_icon(world: &World, entity: Entity) -> super::EditorIcon {
    use super::EditorIcon;
    if world.get::<Camera>(entity).is_some() {
        EditorIcon::Camera
    } else if world.get::<DirectionalLight>(entity).is_some()
        || world.get::<PointLight>(entity).is_some()
        || world.get::<SpotLight>(entity).is_some()
    {
        EditorIcon::Light
    } else if world.get::<MeshRenderer>(entity).is_some() {
        EditorIcon::Mesh
    } else {
        EditorIcon::Empty
    }
}

/// Draws object creation, search, selection, parenting, and the tree rows.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_hierarchy_area(
    ui: &mut egui::Ui,
    world: &World,
    entities: &[HierarchyItem],
    state: &mut EditorState,
    entity_request: &mut Option<EntityRequest>,
    asset_request: &mut Option<AssetRequest>,
    edited_transform: &mut Option<Transform>,
    edited_camera: &mut Option<Camera>,
    edited_physics: &mut Option<PhysicsBody>,
    edited_rigid_body: &mut Option<RigidBody>,
    edited_collider: &mut Option<Collider>,
) {
    use gui_elements::EditorTheme;
    let known = entities
        .iter()
        .map(|item| item.entity)
        .collect::<std::collections::HashSet<_>>();
    if state
        .rename_target
        .is_some_and(|target| !known.contains(&target))
    {
        state.rename_target = None;
    }
    state.selection.retain(|entity| known.contains(entity));
    // Viewport picking only sets the primary object; resync the set here.
    if state
        .selected
        .is_none_or(|primary| !state.selection.contains(&primary))
    {
        state.selection = state.selected.into_iter().collect();
    }

    gui_elements::kit::toolbar(ui, |ui| {
        if EditorTheme::toolbar_icon_button(
            ui,
            "",
            super::EditorIcon::AddObject,
            EditorTheme::ROW_HEIGHT + 8.0,
            true,
        )
        .on_hover_text("Add object")
        .clicked()
        {
            state.add_object_parent = None;
            state.add_object_modal_open = true;
        }
        gui_elements::kit::search_field(
            ui,
            &mut state.hierarchy_filter,
            "Search objects",
        );
    });
    ui.add_space(4.0);

    let visible = filter_items(entities, &state.hierarchy_filter);
    let remote = super::collab_panel::remote_selection(world);
    let visible_entities =
        visible.iter().map(|item| item.entity).collect::<Vec<_>>();
    let (toggle, range) =
        ui.input(|input| (input.modifiers.command, input.modifiers.shift));

    gui_elements::kit::list_well(ui, 20.0, |ui| {
        egui::ScrollArea::both()
            .id_salt("hierarchy_panel_scroll")
            .auto_shrink([false, false])
            .scroll_bar_visibility(
                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
            )
            // Only rows inside the scrolled view are drawn. The inline rename row
            // is a little taller, which shifts later rows by a few pixels.
            .show_rows(ui, EditorTheme::ROW_HEIGHT, visible.len(), |ui, rows| {
                for item in &visible[rows] {
                    let item = *item;
                    if state.rename_target == Some(item.entity) {
                        draw_inline_rename(ui, item, state, entity_request);
                        continue;
                    }
                    let visible_now = world
                        .get::<Visibility>(item.entity)
                        .is_none_or(|visibility| visibility.visible);
                    // Rows hidden by themselves or an ancestor are dimmed, as in
                    // Blender's outliner.
                    let response = ui
                        .scope(|ui| {
                            if !crate::runtime::visible_in_hierarchy(
                                world,
                                item.entity,
                            ) {
                                ui.multiply_opacity(0.45);
                            }
                            let label = if is_preview_spawned(state, item.entity) {
                                format!("{}  [Runtime]", item.name)
                            } else {
                                item.name.clone()
                            };
                            let icon = entity_icon(world, item.entity);
                            EditorTheme::tree_row_tinted(
                                ui,
                                &label,
                                item.depth,
                                state.selection.contains(&item.entity),
                                state.selected == Some(item.entity),
                                Some(icon),
                                Some(icon_color(icon)),
                            )
                        })
                        .inner;

                    // Eye toggle, drawn over the row's right edge. It is
                    // registered after the row, so it wins the click.
                    let eye_rect = egui::Rect::from_center_size(
                        egui::pos2(
                            response.rect.right() - 12.0,
                            response.rect.center().y,
                        ),
                        egui::vec2(18.0, 18.0),
                    );
                    let eye = ui
                        .interact(
                            eye_rect,
                            ui.id().with(("hierarchy_eye", item.entity)),
                            egui::Sense::click(),
                        )
                        .on_hover_text(if visible_now { "Hide" } else { "Show" });
                    super::icons::paint_editor_icon(
                        ui.painter(),
                        if visible_now {
                            super::EditorIcon::Eye
                        } else {
                            super::EditorIcon::EyeClosed
                        },
                        eye_rect,
                        if eye.hovered() {
                            EditorTheme::TEXT
                        } else {
                            EditorTheme::TEXT_MUTED
                        },
                    );
                    // Dots for objects other collaborators have selected.
                    let marks = world
                        .get::<SceneId>(item.entity)
                        .and_then(|id| remote.get(&id.0));
                    for (index, (name, color)) in
                        marks.into_iter().flatten().enumerate()
                    {
                        let center = egui::pos2(
                            eye_rect.left() - 8.0 - index as f32 * 10.0,
                            response.rect.center().y,
                        );
                        ui.painter().circle_filled(center, 3.5, *color);
                        ui.interact(
                            egui::Rect::from_center_size(
                                center,
                                egui::vec2(10.0, 10.0),
                            ),
                            ui.id().with(("hierarchy_peer", item.entity, index)),
                            egui::Sense::hover(),
                        )
                        .on_hover_text(format!("Selected by {name}"));
                    }
                    if eye.clicked() {
                        *entity_request = Some(EntityRequest::SetVisible(
                            item.entity,
                            !visible_now,
                        ));
                    }

                    response.dnd_set_drag_payload(HierarchyDrag(item.entity));
                    if let Some(payload) =
                        response.dnd_hover_payload::<HierarchyDrag>()
                    {
                        let valid = can_reparent(world, payload.0, item.entity);
                        ui.painter().rect_stroke(
                            response.rect,
                            f32::from(EditorTheme::RADIUS),
                            egui::Stroke::new(
                                2.0_f32,
                                if valid {
                                    EditorTheme::ACCENT_HOVER
                                } else {
                                    crate::editor::gui_elements::EditorTheme::ERROR
                                },
                            ),
                            egui::StrokeKind::Inside,
                        );
                    }
                    // An image dropped on a mesh object becomes its base color.
                    if let Some(payload) = response
                        .dnd_release_payload::<super::assets_panel::ImageDrag>()
                    {
                        *asset_request = Some(AssetRequest::SetMaterialTexture {
                            entity: item.entity,
                            slot: 0,
                            path: payload.0.clone(),
                        });
                    }
                    if let Some(payload) =
                        response.dnd_release_payload::<HierarchyDrag>()
                    {
                        // Dragging a selected row moves the whole selection.
                        let moved = if state.selection.contains(&payload.0) {
                            state.selection.clone()
                        } else {
                            vec![payload.0]
                        };
                        let moved: Vec<Entity> = moved
                            .into_iter()
                            .filter(|entity| {
                                can_reparent(world, *entity, item.entity)
                            })
                            .collect();
                        if !moved.is_empty() {
                            *entity_request = Some(EntityRequest::Reparent(
                                moved,
                                Some(item.entity),
                            ));
                        }
                    }

                    // Right-click keeps an existing multi-selection so the
                    // context menu acts on all of it.
                    let right_click_keeps = response.secondary_clicked()
                        && state.selection.contains(&item.entity);
                    if (response.clicked() || response.secondary_clicked())
                        && !right_click_keeps
                    {
                        let (selection, primary) = if response.clicked() {
                            click_selection(
                                &state.selection,
                                state.selected,
                                &visible_entities,
                                item.entity,
                                toggle,
                                range,
                            )
                        } else {
                            (vec![item.entity], Some(item.entity))
                        };
                        state.selection = selection;
                        match primary.and_then(|primary| {
                            entities.iter().find(|item| item.entity == primary)
                        }) {
                            Some(primary) => select_item(
                                world,
                                primary,
                                state,
                                edited_transform,
                                edited_camera,
                                edited_physics,
                                edited_rigid_body,
                                edited_collider,
                            ),
                            None => state.selected = None,
                        }
                    }
                    if response.double_clicked() {
                        start_rename(state, item);
                    }
                    response.context_menu(|ui| {
                        ui.set_min_width(220.0);
                        let count = state.selection.len();
                        EditorTheme::menu_section(ui, "OBJECT");
                        if EditorTheme::menu_action(ui, "Add Child Object...", true)
                            .clicked()
                        {
                            state.add_object_parent = Some(item.entity);
                            state.add_object_modal_open = true;
                        }
                        if EditorTheme::menu_action(ui, "Rename", true).clicked() {
                            start_rename(state, item);
                        }
                        let duplicate = if count > 1 {
                            format!("Duplicate {count} Objects")
                        } else {
                            "Duplicate".to_owned()
                        };
                        if EditorTheme::menu_action(ui, &duplicate, true).clicked()
                        {
                            *entity_request = Some(EntityRequest::Duplicate(
                                state.selection.clone(),
                            ));
                        }
                        if world.get::<Parent>(item.entity).is_some()
                            && EditorTheme::menu_action(
                                ui,
                                "Move to Scene Root",
                                true,
                            )
                            .clicked()
                        {
                            *entity_request = Some(EntityRequest::Reparent(
                                vec![item.entity],
                                None,
                            ));
                        }
                        let member = world
                            .get::<crate::runtime::InstanceMember>(item.entity)
                            .is_some();
                        if member
                            || world
                                .get::<crate::runtime::InstanceExpanded>(
                                    item.entity,
                                )
                                .is_some()
                        {
                            use crate::runtime::InstanceEdit;
                            EditorTheme::menu_section(ui, "INSTANCE");
                            for (label, edit, enabled, hint) in [
                                (
                                    "Revert Object",
                                    InstanceEdit::RevertObject,
                                    member,
                                    "Reset this object to its source scene values",
                                ),
                                (
                                    "Revert Instance",
                                    InstanceEdit::Revert,
                                    true,
                                    "Drop every change made to this instance's \
                                     objects",
                                ),
                                (
                                    "Apply to Source",
                                    InstanceEdit::ApplyToSource,
                                    true,
                                    "Write this instance's objects into its source \
                                     scene file. Undo does not restore the file.",
                                ),
                                (
                                    "Unpack Completely",
                                    InstanceEdit::UnpackCompletely,
                                    true,
                                    "Turn the instance into ordinary objects with \
                                     no link to the source",
                                ),
                            ] {
                                if EditorTheme::menu_action(ui, label, enabled)
                                    .on_hover_text(hint)
                                    .clicked()
                                {
                                    *entity_request = Some(EntityRequest::Instance(
                                        item.entity,
                                        edit,
                                    ));
                                }
                            }
                        }
                        EditorTheme::menu_section(ui, "DANGER");
                        let delete = if count > 1 {
                            format!("Delete {count} Objects")
                        } else {
                            "Delete".to_owned()
                        };
                        if EditorTheme::menu_action(ui, &delete, true).clicked() {
                            *entity_request = Some(EntityRequest::Delete(
                                state.selection.clone(),
                            ));
                        }
                    });
                }
            });
    });
    let selected = state.selection.len();
    gui_elements::kit::footer(
        ui,
        &if selected > 0 {
            format!("{} objects · {selected} selected", entities.len())
        } else {
            format!("{} objects", entities.len())
        },
    );
}

fn start_rename(state: &mut EditorState, item: &HierarchyItem) {
    state.selected = Some(item.entity);
    state.rename_draft = item.name.clone();
    state.rename_target = Some(item.entity);
}

fn can_reparent(world: &World, child: Entity, parent: Entity) -> bool {
    if child == parent
        || world.get_entity(child).is_err()
        || world.get_entity(parent).is_err()
    {
        return false;
    }
    let mut ancestor = Some(parent);
    let mut visited = std::collections::HashSet::new();
    while let Some(entity) = ancestor {
        if entity == child || !visited.insert(entity) {
            return false;
        }
        ancestor = world.get::<Parent>(entity).map(|parent| parent.0);
    }
    true
}

/// Moves `entities` under `parent` (or to the scene root) in one scene
/// reload, keeping their world transforms. A moved entity whose ancestor is
/// also moved keeps its place under that ancestor. Returns the first moved
/// entity after the reload.
pub(super) fn reparent_entities(
    world: &mut World,
    entities: &[Entity],
    parent: Option<Entity>,
) -> Result<Option<Entity>, String> {
    let scene_id = |world: &World, entity: Entity| {
        world
            .get::<SceneId>(entity)
            .map(|id| id.0)
            .ok_or_else(|| "Object is not part of the saved scene".to_owned())
    };
    let parent_id = parent.map(|parent| scene_id(world, parent)).transpose()?;
    let parent_inverse = parent
        .and_then(|parent| world.get::<GlobalTransform>(parent))
        .and_then(|parent| Matrix4::from(parent.matrix).try_inverse());
    let is_moved_descendant = |entity: Entity| {
        let mut ancestor = world.get::<Parent>(entity).map(|parent| parent.0);
        while let Some(current) = ancestor {
            if entities.contains(&current) {
                return true;
            }
            ancestor = world.get::<Parent>(current).map(|parent| parent.0);
        }
        false
    };
    let mut moves = Vec::new();
    for &entity in entities {
        if is_moved_descendant(entity) {
            continue;
        }
        let transform = world
            .get::<GlobalTransform>(entity)
            .map(|child| Matrix4::from(child.matrix))
            .map(|child| {
                parent_inverse.map_or(child, |inverse| inverse * child)
            })
            .map(Transform::from_matrix);
        moves.push((scene_id(world, entity)?, transform));
    }
    let mut document = scene_document(world, "Main Scene")
        .map_err(|error| error.to_string())?;
    let parents: HashMap<_, _> = document
        .entities
        .iter()
        .map(|item| (item.id, item.parent))
        .collect();
    for (child_id, transform) in &moves {
        let mut ancestor = parent_id;
        let mut visited = HashSet::new();
        while let Some(id) = ancestor {
            if id == *child_id || !visited.insert(id) {
                return Err("That parent would create a hierarchy cycle".into());
            }
            ancestor = parents.get(&id).copied().flatten();
        }
        let child = document
            .entities
            .iter_mut()
            .find(|item| item.id == *child_id)
            .ok_or_else(|| "Object was not found in the scene".to_owned())?;
        child.parent = parent_id;
        if let Some(transform) = transform {
            child.transform = Some((*transform).into());
        }
    }
    crate::runtime::load_scene_document(
        world,
        &document,
        SceneLoadMode::Replace,
    )
    .map_err(|error| error.to_string())?;
    let first = moves.first().map(|(id, _)| *id);
    let mut query = world.query::<(Entity, &SceneId)>();
    Ok(query
        .iter(world)
        .find_map(|(entity, id)| (Some(id.0) == first).then_some(entity)))
}

/// Copies one clicked tree row into editor selection and Inspector drafts.
#[allow(clippy::too_many_arguments)]
fn select_item(
    world: &World,
    item: &HierarchyItem,
    state: &mut EditorState,
    edited_transform: &mut Option<Transform>,
    edited_camera: &mut Option<Camera>,
    edited_physics: &mut Option<PhysicsBody>,
    edited_rigid_body: &mut Option<RigidBody>,
    edited_collider: &mut Option<Collider>,
) {
    state.selected = Some(item.entity);
    state.rename_draft = item.name.clone();
    state.rename_target = None;
    *edited_transform = item.transform;
    *edited_camera = world.get::<Camera>(item.entity).copied();
    *edited_physics = world.get::<PhysicsBody>(item.entity).cloned();
    *edited_rigid_body = world.get::<RigidBody>(item.entity).copied();
    *edited_collider = world.get::<Collider>(item.entity).copied();
}

/// Replaces one tree row with a focused text field until Apply or Cancel.
fn draw_inline_rename(
    ui: &mut egui::Ui,
    item: &HierarchyItem,
    state: &mut EditorState,
    entity_request: &mut Option<EntityRequest>,
) {
    const INDENT: f32 = 18.0;
    let mut apply = false;
    let mut cancel = false;
    ui.horizontal(|ui| {
        ui.add_space(item.depth as f32 * INDENT);
        let button_width = 52.0;
        let gap = ui.spacing().item_spacing.x;
        let text_width =
            (ui.available_width() - button_width * 2.0 - gap * 2.0).max(50.0);
        let response = ui.add_sized(
            [text_width, 28.0],
            egui::TextEdit::singleline(&mut state.rename_draft),
        );
        response.request_focus();
        apply =
            gui_elements::EditorTheme::toolbar_button(ui, "Apply", false, true)
                .clicked()
                || (response.has_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter)));
        cancel = gui_elements::EditorTheme::toolbar_button(
            ui, "Cancel", false, true,
        )
        .clicked()
            || ui.input(|input| input.key_pressed(egui::Key::Escape));
    });
    if apply && !state.rename_draft.trim().is_empty() {
        *entity_request = Some(EntityRequest::Rename(
            item.entity,
            state.rename_draft.trim().into(),
        ));
        state.rename_target = None;
    } else if cancel {
        state.rename_draft = item.name.clone();
        state.rename_target = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::gui_elements::EditorTheme;

    #[test]
    fn only_entities_created_during_preview_get_runtime_badges() {
        let mut world = World::new();
        let authored = world.spawn(Name("Authored".into())).id();
        let mut state = EditorState {
            preview_entities: Some([authored].into_iter().collect()),
            ..EditorState::default()
        };
        let spawned = world.spawn(Name("Spawned".into())).id();
        assert!(!is_preview_spawned(&state, authored));
        assert!(is_preview_spawned(&state, spawned));
        state.preview_entities = None;
        assert!(!is_preview_spawned(&state, spawned));
    }

    #[test]
    fn children_are_drawn_immediately_after_their_parent() {
        let mut world = World::new();
        let second_root = world.spawn(Name("Z Root".into())).id();
        let first_root = world.spawn(Name("A Root".into())).id();
        let child =
            world.spawn((Name("Child".into()), Parent(first_root))).id();
        let grandchild =
            world.spawn((Name("Grandchild".into()), Parent(child))).id();

        let items = collect_entities(&mut world);
        let order = items
            .iter()
            .map(|item| (item.entity, item.depth))
            .collect::<Vec<_>>();

        assert_eq!(
            order,
            vec![
                (first_root, 0),
                (child, 1),
                (grandchild, 2),
                (second_root, 0)
            ]
        );
    }

    #[test]
    fn drag_parenting_rejects_self_and_descendant_cycles() {
        let mut world = World::new();
        let root = world.spawn(Name("Root".into())).id();
        let child = world.spawn((Name("Child".into()), Parent(root))).id();
        let grandchild =
            world.spawn((Name("Grandchild".into()), Parent(child))).id();

        assert!(!can_reparent(&world, root, grandchild));
        assert!(!can_reparent(&world, child, child));
        assert!(can_reparent(&world, grandchild, root));
    }

    #[test]
    fn child_rows_are_indented_one_step_from_parent_rows() {
        let context = egui::Context::default();
        let mut rects = None;

        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let parent =
                    EditorTheme::tree_row(ui, "Parent", 0, false, false, None);
                let child =
                    EditorTheme::tree_row(ui, "Child", 1, false, false, None);
                rects = Some((parent.rect, child.rect));
            });
        });

        let (parent, child) = rects.unwrap();
        assert_eq!(child.left() - parent.left(), 18.0);
        assert!(child.top() >= parent.bottom());
    }

    #[test]
    fn search_keeps_matches_and_their_ancestors_only() {
        let mut world = World::new();
        let root = world.spawn(Name("Level".into())).id();
        let arm = world.spawn((Name("Arm".into()), Parent(root))).id();
        let lamp = world.spawn((Name("Desk Lamp".into()), Parent(arm))).id();
        world.spawn((Name("Floor".into()), Parent(root)));
        world.spawn(Name("Sun".into()));
        let items = collect_entities(&mut world);

        let shown = filter_items(&items, "  LAMP ")
            .iter()
            .map(|item| item.entity)
            .collect::<Vec<_>>();
        assert_eq!(shown, vec![root, arm, lamp]);
        assert_eq!(filter_items(&items, "").len(), items.len());
        assert!(filter_items(&items, "missing").is_empty());
    }

    #[test]
    fn clicks_select_toggle_and_extend_with_a_primary_object() {
        let mut world = World::new();
        let [a, b, c, d] = std::array::from_fn(|_| world.spawn_empty().id());
        let visible = [a, b, c, d];

        let (selection, primary) =
            click_selection(&[a, b], Some(a), &visible, c, false, false);
        assert_eq!((selection, primary), (vec![c], Some(c)));

        let (selection, primary) =
            click_selection(&[b], Some(b), &visible, d, true, false);
        assert_eq!((selection.clone(), primary), (vec![b, d], Some(d)));
        let (selection, primary) =
            click_selection(&selection, primary, &visible, d, true, false);
        assert_eq!((selection, primary), (vec![b], Some(b)));

        // Shift extends from the primary object and keeps it primary.
        let (selection, primary) =
            click_selection(&[c], Some(c), &visible, a, false, true);
        assert_eq!((selection, primary), (vec![c, a, b], Some(c)));
    }

    #[test]
    fn viewport_shift_pick_adds_activates_and_deselects() {
        let mut world = World::new();
        let [a, b] = std::array::from_fn(|_| world.spawn_empty().id());

        let (selection, primary) = shift_pick_selection(&[a], Some(a), b);
        assert_eq!((selection.clone(), primary), (vec![a, b], Some(b)));
        let (selection, primary) = shift_pick_selection(&selection, primary, a);
        assert_eq!((selection.clone(), primary), (vec![a, b], Some(a)));
        let (selection, primary) = shift_pick_selection(&selection, primary, a);
        assert_eq!((selection, primary), (vec![b], Some(b)));
    }
}
