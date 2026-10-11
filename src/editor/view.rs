//! Main editor frame and panel contents.
//!
//! Keeping the complete frame in this file makes the smaller editor modules
//! easier to read while this view is gradually divided into panel modules.

use egui::Pos2;
use nalgebra::{Matrix4, Rotation3, Vector3, Vector4};

use super::picking::{
    drag_scene_handle, edit_tile_map, grab_scene_handle, pick_entity,
    point_handles, project_world_to_screen, scene_ray, tile_cell_under,
};
use super::*;
use crate::editor::overlay::{add_axis, add_bound_box};
use crate::runtime::{CullingMode, QualityProfile};

/// Starting point for a `PhysicsSolver::Custom` hook file. The engine
/// prepends the physics ABI (`src/shaders/physics_abi.glsl`) and calls
/// `solve` once per fixed tick for each body that selected this file.
const CUSTOM_SOLVER_TEMPLATE: &str = "\
// Custom GPU solver. `solve` runs once per fixed tick for every body that
// selected this file, after queued commands. Watch rules see the body as it
// was before `solve`. `body` follows `PhysicsState`; `pc.dt` is the step.
void solve(inout PhysicsState body) {
    if (body.properties.z != 1.0) return; // dynamic bodies only
    body.velocity.xyz += vec3(pc.gravity_x, pc.gravity_y, pc.gravity_z)
        * body.properties.y * pc.dt;
    body.model[3].xyz += body.velocity.xyz * pc.dt;
}

";

/// Draws the interactive editor into an already-open egui frame.
///
/// Window runners use this entry point after forwarding winit input to egui.
///
/// # Arguments
/// * `world` - ECS world containing scene objects and editor resources.
/// * `context` - Current egui frame used to draw all controls.
pub fn draw_editor_view(world: &mut World, context: &Context) {
    // Navigation owns the keyboard; a text field must not keep focus and
    // take the keys back when it ends.
    if editor_navigation_active(world) {
        context.memory_mut(egui::Memory::stop_text_input);
    }
    // The window image is not cleared under the UI, so every pixel starts
    // with the workspace background.
    context
        .layer_painter(egui::LayerId::background())
        .rect_filled(
            context.screen_rect(),
            0.0,
            gui_elements::EditorTheme::BACKGROUND,
        );
    // Move editor values that egui will change during this frame out of the
    // world. They are inserted back at the end; cloning them every frame
    // copied the whole Undo history.
    let mut state = take_resource::<EditorState>(world);
    let mut project_manager = take_resource::<ProjectManagerState>(world);
    let mut history = take_resource::<EditorHistory>(world);
    let mut pending_action = take_resource::<PendingDestructiveAction>(world);
    let mut editor_assets = take_resource::<EditorAssetState>(world);
    let mut dialogs = take_resource::<FileDialogs>(world);
    super::reload_external_scene_change(world, &mut state, &mut history);
    let (build_running, build_finished, build_console_output, restart_ready) = {
        let mut build = world.resource_mut::<EditorBuildState>();
        let finished = build.poll();
        let console_output = build.take_console_output();
        let restart_ready = build.take_restart_ready();
        let notices = build.take_notices();
        if let Some(mut console) = world.get_resource_mut::<EditorConsole>() {
            for (level, source, text) in notices {
                console.push_from(level, source, text);
            }
        }
        let build = world.resource::<EditorBuildState>();
        (build.running, finished, console_output, restart_ready)
    };
    let (reloaded, reload_failures) = world
        .get_resource_mut::<AssetServer>()
        .map(|mut assets| {
            (assets.take_reloaded(), assets.take_reload_failures())
        })
        .unwrap_or_default();
    for path in reloaded {
        world.resource_mut::<EditorConsole>().push_from(
            ConsoleLevel::Info,
            "Assets",
            format!("Hot reloaded {}", path.display()),
        );
    }
    for failure in reload_failures {
        world.resource_mut::<EditorConsole>().push_from(
            ConsoleLevel::Error,
            "Assets",
            format!("Hot reload failed, keeping the last version: {failure}"),
        );
    }
    if !build_console_output.is_empty() {
        world.resource_mut::<EditorConsole>().push_from(
            ConsoleLevel::Info,
            "Build",
            format!("Cargo Output\n{build_console_output}"),
        );
    }
    if let Some(success) = build_finished {
        world.resource_mut::<EditorConsole>().push_from(
            if success {
                ConsoleLevel::Info
            } else {
                ConsoleLevel::Error
            },
            "Build",
            if success {
                "Build or game task finished successfully"
            } else {
                "Build or game failed; open Code Editor for output"
            },
        );
    }
    let entities = collect_entities(world);
    editor_assets.refresh_files(&state.project_root);
    let asset_files = std::mem::take(&mut editor_assets.files);
    let mut render_settings = world.resource::<RenderSettings>().clone();
    let original_scene_render =
        (render_settings.quality, render_settings.culling);
    let mut gizmo_settings = *world.resource::<EditorGizmoSettings>();
    let mut gizmo_drag = take_resource::<EditorGizmoDrag>(world);
    let mut transform_mode = *world.resource::<EditorTransformMode>();
    let editor_shortcuts = world.resource::<EditorShortcuts>().clone();
    let shortcut_label = |action| {
        editor_shortcuts
            .get(ShortcutAction::Editor(action))
            .map(KeyBinding::label)
    };
    let physics_backends = *world.resource::<PhysicsBackendStatus>();
    let asset_counts = world.get_resource::<AssetServer>().map(|assets| {
        (
            assets.meshes.len(),
            assets.materials.len(),
            assets.textures.len(),
            assets.scenes.len(),
            assets.lod_groups.len(),
        )
    });
    let render_report = world
        .get_resource::<RenderWorld>()
        .map(|render_world| render_world.report);
    let culling_stats = world
        .get_resource::<crate::rendering::scene_renderer::CullingStats>()
        .copied();
    let gpu_pass_times = world
        .get_resource::<crate::rendering::scene_renderer::GpuPassTimes>()
        .filter(|times| !times.0.is_empty())
        .map(gpu_pass_times_label);
    let render_counters = world
        .get_resource::<crate::rendering::scene_renderer::RenderCounters>()
        .copied();
    let render_capacity = world
        .get_resource::<crate::rendering::scene_renderer::RenderCapacityDiagnostics>()
        .copied();
    let renderer_capabilities = world
        .get_resource::<crate::rendering::scene_renderer::RendererCapabilities>(
        )
        .cloned();
    let physics_settings = world
        .get_resource::<crate::runtime::PhysicsSettings>()
        .cloned();
    let physics_label = render_counters.zip(
        world
            .get_resource::<crate::rendering::scene_renderer::RenderCapacityDiagnostics>()
            .copied(),
    )
    .map(|(counters, capacity)| physics_counters_label(&counters, &capacity));
    let cpu_timings = world
        .get_resource::<crate::runtime::CpuFrameTimings>()
        .copied();
    let mut edited_transform = state
        .selected
        .and_then(|entity| world.get::<Transform>(entity).copied());
    let mut edited_camera = state
        .selected
        .and_then(|entity| world.get::<Camera>(entity).copied());
    let mut edited_directional_light = state
        .selected
        .and_then(|entity| world.get::<DirectionalLight>(entity).copied());
    let mut edited_point_light = state
        .selected
        .and_then(|entity| world.get::<PointLight>(entity).copied());
    let mut edited_spot_light = state
        .selected
        .and_then(|entity| world.get::<SpotLight>(entity).copied());
    let mut edited_classes = state.selected.map(|entity| {
        world
            .get::<ObjectClasses>(entity)
            .cloned()
            .unwrap_or_default()
    });
    let mut edited_physics = state
        .selected
        .and_then(|entity| world.get::<PhysicsBody>(entity).cloned());
    let mut edited_rigid_body = state
        .selected
        .and_then(|entity| world.get::<RigidBody>(entity).copied());
    let mut edited_collider = state
        .selected
        .and_then(|entity| world.get::<Collider>(entity).copied());
    let mut edited_render_bounds = state
        .selected
        .and_then(|entity| world.get::<RenderBounds>(entity).copied());
    let mut edited_material = state.selected.and_then(|entity| {
        let renderer = world.get::<MeshRenderer>(entity)?;
        world
            .get_resource::<AssetServer>()?
            .materials
            .get(renderer.material)
            .cloned()
    });
    let original_material = edited_material.clone();
    let original_selected = state.selected;
    let original_render_bounds = edited_render_bounds;
    let original_transform = edited_transform;
    let original_camera = edited_camera;
    let original_directional_light = edited_directional_light;
    let original_point_light = edited_point_light;
    let original_spot_light = edited_spot_light;
    let original_classes = edited_classes.clone();
    let original_physics = edited_physics.clone();
    let original_rigid_body = edited_rigid_body;
    let original_collider = edited_collider;
    let registered_names = registered_component_names(world);
    let custom_values = state
        .selected
        .map(|entity| registered_component_values(world, entity))
        .transpose()
        .unwrap_or_else(|error| {
            state.scene_message = Some(format!("Inspector failed: {error}"));
            None
        })
        .unwrap_or_default();
    // Buttons set these small requests while drawing. We apply them later,
    // after egui no longer borrows temporary values.
    let mut viewport_rect = None;
    let mut scene_click_position: Option<Pos2> = None;
    let mut scene_drag_right_started: Option<Pos2> = None;
    let mut scene_drag_right_position: Option<Pos2> = None;
    let mut scene_drag_right_stopped = false;
    let mut scene_drag_left_started: Option<Pos2> = None;
    let mut scene_drag_left_position: Option<Pos2> = None;
    let mut scene_drag_left_stopped = false;
    let mut scene_hover_position: Option<Pos2> = None;
    let mut scene_right_clicked = false;
    let mut scene_hovered = false;
    let left_pressed = context.input(|input| {
        input.pointer.button_pressed(egui::PointerButton::Primary)
    });
    let right_pressed = context.input(|input| {
        input.pointer.button_pressed(egui::PointerButton::Secondary)
    });
    let escape_pressed =
        context.input(|input| input.key_pressed(egui::Key::Escape));
    let mut save_clicked = false;
    let mut load_clicked = false;
    let mut component_edits = Vec::new();
    let mut add_physics = false;
    let mut remove_physics = false;
    let mut edit_custom_shader = false;
    let mut load_code = false;
    let mut save_code = false;
    let mut validate_code = false;
    let mut run_project = restart_ready;
    let mut stop_build = false;
    let mut restart_build = false;
    let mut reload_code = false;
    let mut preview_start = false;
    let mut preview_pause = false;
    let mut preview_resume = false;
    let mut preview_step = false;
    let mut preview_stop = false;
    let mut add_area = false;
    let mut reset_layout = false;
    let mut save_layout = false;
    let mut load_layout = false;
    let mut open_projects = false;
    let mut new_project = false;
    let mut save_project = false;
    let mut undo_clicked = false;
    let mut redo_clicked = false;
    let mut new_scene = false;
    let mut open_scene = false;
    let mut save_scene_as = false;
    let mut entity_request = None;
    let mut asset_request = None;
    let mut build_request = None;
    let data_types: Vec<&'static str> = world
        .get_resource::<crate::assets::DataAssetTypes>()
        .map(|types| types.iter().map(|registered| registered.name).collect())
        .unwrap_or_default();
    // Picking another object takes the Inspector back from a data asset.
    if editor_assets
        .inspected
        .as_ref()
        .is_some_and(|inspection| inspection.selection != state.selected)
    {
        editor_assets.inspected = None;
    }
    let mut export_destination = None;
    // Keyboard shortcuts queued by the window-event handler run like the
    // matching menu entries.
    let commands = world
        .get_resource_mut::<EditorCommandQueue>()
        .map(|mut queue| std::mem::take(&mut queue.0))
        .unwrap_or_default();
    for command in commands {
        match command {
            EditorAction::Undo => undo_clicked = true,
            EditorAction::Redo => redo_clicked = true,
            EditorAction::SaveScene => save_clicked = true,
            EditorAction::DeleteSelection if !state.selection.is_empty() => {
                entity_request =
                    Some(EntityRequest::Delete(state.selection.clone()));
            }
            EditorAction::RenameSelection => {
                if let Some(entity) = state.selected {
                    state.rename_draft = world
                        .get::<Name>(entity)
                        .map(|name| name.0.clone())
                        .unwrap_or_default();
                    state.rename_target = Some(entity);
                }
            }
            EditorAction::DeleteSelection => {}
        }
    }
    let has_open_project = !state.project_root.is_empty()
        && std::path::Path::new(&state.project_root)
            .join("project.json")
            .is_file();
    // Route a finished native file dialog to the action that opened it.
    let mut dialog_project_request = None;
    let mut dialog_scene = None;
    let mut save_before_continue_to = None;
    let mut save_as_path = None;
    if let Some((purpose, paths)) = dialogs.poll() {
        let first = paths.first().cloned();
        match purpose {
            DialogPurpose::OpenProject => {
                dialog_project_request = first.map(ProjectRequest::Open);
            }
            DialogPurpose::ProjectParentFolder => {
                if let Some(path) = first {
                    project_manager.parent_directory = path;
                }
            }
            DialogPurpose::CreateProjectIn => {
                if let Some(path) = first {
                    project_manager.parent_directory = path.clone();
                    dialog_project_request = Some(ProjectRequest::Create {
                        parent: path,
                        name: project_manager.project_name.clone(),
                        template: project_manager.template,
                    });
                }
            }
            DialogPurpose::OpenScene => dialog_scene = first,
            // A cancel still answers the Unsaved Scene prompt.
            DialogPurpose::SaveSceneBeforeContinue => {
                save_before_continue_to = Some(first);
            }
            DialogPurpose::SaveSceneAs => save_as_path = first,
            DialogPurpose::ExportGame { target } => {
                export_destination = first.map(|parent| (parent, target));
            }
            DialogPurpose::ImportFiles if !paths.is_empty() => {
                asset_request = Some(AssetRequest::ImportFiles(paths));
            }
            DialogPurpose::ImportFiles => {}
            DialogPurpose::ReplaceAsset => match first {
                Some(path) => {
                    asset_request =
                        Some(AssetRequest::PreviewReplacement(path));
                }
                None => editor_assets.replace_target = None,
            },
            DialogPurpose::MaterialTexture { entity, slot } => {
                asset_request = first.map(|path| {
                    AssetRequest::SetMaterialTexture { entity, slot, path }
                });
            }
        }
    }
    if dialogs.is_open() {
        // The modal keeps panel clicks from changing what the dialog's
        // answer will apply to.
        egui::Modal::new(egui::Id::new("file_dialog_wait")).show(
            context,
            |ui| {
                ui.label("Waiting for the file dialog...");
            },
        );
        context.request_repaint_after(std::time::Duration::from_millis(50));
    }

    // The top toolbar stays visible even when every area below it changes.
    TopBottomPanel::top("visible_editor_toolbar")
        .frame(
            egui::Frame::NONE
                .fill(gui_elements::EditorTheme::PANEL)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    gui_elements::EditorTheme::BORDER_SOFT,
                )),
        )
        .show(context, |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("editor_toolbar_scroll")
                .scroll_bar_visibility(
                    egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                )
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("RustingEngine");
                        if state.scene_dirty {
                            ui.colored_label(
                                gui_elements::EditorTheme::WARNING,
                                "Unsaved scene",
                            );
                        }
                        ui.separator();
                        gui_elements::EditorTheme::toolbar_menu(
                            ui,
                            "toolbar_file_menu",
                            "File",
                            66.0,
                            280.0,
                            |ui| {
                                gui_elements::EditorTheme::menu_section(
                                    ui, "PROJECT",
                                );
                                open_projects |= gui_elements::EditorTheme::menu_action(
                                    ui,
                                    "Open Project...",
                                    true,
                                )
                                .clicked();
                                new_project |= gui_elements::EditorTheme::menu_action(
                                    ui,
                                    "New Project...",
                                    true,
                                )
                                .clicked();
                                save_project |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "Save Project",
                                        has_open_project,
                                    )
                                    .on_hover_text(
                                        "Save the open source file and scene",
                                    )
                                    .clicked();
                                gui_elements::EditorTheme::menu_section(
                                    ui, "SCENE",
                                );
                                new_scene |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "New Scene",
                                        has_open_project,
                                    )
                                    .clicked();
                                open_scene |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "Open Scene...",
                                        has_open_project,
                                    )
                                    .clicked();
                                load_clicked |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "Reload Scene",
                                        has_open_project,
                                    )
                                    .clicked();
                                save_clicked |=
                                    gui_elements::EditorTheme::menu_shortcut_action(
                                        ui,
                                        "Save Scene",
                                        shortcut_label(EditorAction::SaveScene)
                                            .as_deref(),
                                        has_open_project,
                                    )
                                    .clicked();
                                save_scene_as |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "Save Scene As...",
                                        has_open_project,
                                    )
                                    .clicked();
                            },
                        );
                        gui_elements::EditorTheme::toolbar_menu(
                            ui,
                            "toolbar_edit_menu",
                            "Edit",
                            66.0,
                            220.0,
                            |ui| {
                                gui_elements::EditorTheme::menu_section(
                                    ui, "HISTORY",
                                );
                                undo_clicked |=
                                    gui_elements::EditorTheme::menu_shortcut_action(
                                        ui,
                                        "Undo",
                                        shortcut_label(EditorAction::Undo)
                                            .as_deref(),
                                        !history.undo.is_empty()
                                            || history
                                                .pending_inspector
                                                .is_some(),
                                    )
                                    .clicked();
                                redo_clicked |=
                                    gui_elements::EditorTheme::menu_shortcut_action(
                                        ui,
                                        "Redo",
                                        shortcut_label(EditorAction::Redo)
                                            .as_deref(),
                                        !history.redo.is_empty(),
                                    )
                                    .clicked();
                            },
                        );
                        gui_elements::EditorTheme::toolbar_menu(
                            ui,
                            "toolbar_view_menu",
                            "View",
                            70.0,
                            250.0,
                            |ui| {
                                gui_elements::EditorTheme::menu_section(
                                    ui, "WORKSPACE",
                                );
                                add_area |= gui_elements::EditorTheme::menu_action(
                                    ui,
                                    "Add Area",
                                    true,
                                )
                                .on_hover_text(
                                    "Split the selected area left and right",
                                )
                                .clicked();
                                reset_layout |= gui_elements::EditorTheme::menu_action(
                                    ui,
                                    "Reset Layout",
                                    true,
                                )
                                .clicked();
                                gui_elements::EditorTheme::menu_section(
                                    ui, "LAYOUT FILE",
                                );
                                save_layout |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "Save Layout",
                                        has_open_project,
                                    )
                                    .clicked();
                                load_layout |=
                                    gui_elements::EditorTheme::menu_action(
                                        ui,
                                        "Load Layout",
                                        has_open_project,
                                    )
                                    .clicked();
                                gui_elements::EditorTheme::menu_section(
                                    ui, "UI SCALE",
                                );
                                let zoom = ui.ctx().zoom_factor();
                                for scale in EditorPreferences::UI_SCALES {
                                    if gui_elements::EditorTheme::menu_choice(
                                        ui,
                                        &format!("{:.0}%", scale * 100.0),
                                        (zoom - scale).abs() < 0.01,
                                        true,
                                    )
                                    .clicked()
                                    {
                                        ui.ctx().set_zoom_factor(scale);
                                        let mut preferences =
                                            EditorPreferences::load();
                                        preferences.ui_scale = scale;
                                        if let Err(error) = preferences.save() {
                                            state.scene_message = Some(format!(
                                                "Could not save UI scale: {error}"
                                            ));
                                        }
                                    }
                                }
                                gui_elements::EditorTheme::menu_section(
                                    ui, "TEXT SIZE",
                                );
                                let body = ui
                                    .style()
                                    .text_styles
                                    .get(&egui::TextStyle::Body)
                                    .map_or(0.0, |font| font.size);
                                let default_body = egui::Style::default()
                                    .text_styles
                                    .get(&egui::TextStyle::Body)
                                    .map_or(1.0, |font| font.size);
                                for scale in EditorPreferences::FONT_SCALES {
                                    if gui_elements::EditorTheme::menu_choice(
                                        ui,
                                        &format!("{:.0}%", scale * 100.0),
                                        (body - default_body * scale).abs()
                                            < 0.01,
                                        true,
                                    )
                                    .clicked()
                                    {
                                        super::apply_font_scale(ui.ctx(), scale);
                                        let mut preferences =
                                            EditorPreferences::load();
                                        preferences.font_scale = scale;
                                        if let Err(error) = preferences.save() {
                                            state.scene_message = Some(format!(
                                                "Could not save text size: {error}"
                                            ));
                                        }
                                    }
                                }
                            },
                        );
                        ui.separator();
                        if build_running {
                            stop_build =
                                gui_elements::EditorTheme::toolbar_button(
                                    ui, "Stop", false, true,
                                )
                                .on_hover_text(
                                    "Stop the running Cargo task or native game",
                                )
                                .clicked();
                            restart_build =
                                gui_elements::EditorTheme::toolbar_button(
                                    ui, "Restart", false, true,
                                )
                                .on_hover_text(
                                    "Stop the current process, then save, rebuild, and run again",
                                )
                                .clicked();
                            reload_code =
                                gui_elements::EditorTheme::toolbar_button(
                                    ui, "Reload Code", false, true,
                                )
                                .on_hover_text(
                                    "Keep the running game's objects, rebuild the Rust code, and continue with them",
                                )
                                .clicked();
                        } else {
                            run_project =
                                gui_elements::EditorTheme::toolbar_button(
                                    ui,
                                    "Play",
                                    false,
                                    has_open_project,
                                )
                                .on_hover_text(
                                    "Save and cook the scene, compile the Rust game, then run it",
                                )
                                .clicked();
                        }
                        ui.separator();
                        match state.mode {
                            EditorMode::Edit => {
                                preview_start = gui_elements::EditorTheme::toolbar_button(
                                    ui, "Preview", false, !build_running,
                                ).on_hover_text("Run the scene inside the editor without compiling the project").clicked();
                            }
                            EditorMode::Play => {
                                preview_pause = ui.button("Pause").clicked();
                                preview_stop = ui.button("Stop Preview").clicked();
                            }
                            EditorMode::Paused => {
                                preview_resume = ui.button("Resume").clicked();
                                preview_step = ui.button("Step").clicked();
                                preview_stop = ui.button("Stop Preview").clicked();
                            }
                        }
                        gui_elements::EditorTheme::toolbar_combo_box_with_popup(
                            ui,
                            "toolbar_game_build_profile",
                            state.game_build_profile.label(),
                            96.0,
                            250.0,
                            |ui| {
                                gui_elements::EditorTheme::menu_section(
                                    ui,
                                    "BUILD PROFILE",
                                );
                                if gui_elements::EditorTheme::menu_choice(
                                    ui,
                                    "Debug | Fast compile",
                                    state.game_build_profile
                                        == GameBuildProfile::Debug,
                                    true,
                                )
                                .on_hover_text(
                                    "Fast compile for normal game development",
                                )
                                .clicked()
                                {
                                    state.game_build_profile =
                                        GameBuildProfile::Debug;
                                }
                                if gui_elements::EditorTheme::menu_choice(
                                    ui,
                                    "Release | Full optimization",
                                    state.game_build_profile
                                        == GameBuildProfile::Release,
                                    true,
                                )
                                .on_hover_text(
                                    "Slow compile with full optimizations",
                                )
                                .clicked()
                                {
                                    state.game_build_profile =
                                        GameBuildProfile::Release;
                                }
                            },
                        );
                        ui.separator();
                        ui.label(if build_running {
                            "GAME TASK RUNNING"
                        } else {
                            "EDITING"
                        });
                    });
                });
        });

    if open_projects {
        project_manager.open = true;
    }
    if new_project {
        project_manager.open = true;
        project_manager.parent_directory = PathBuf::new();
        project_manager.message = Some(
            "Choose the parent folder where the new Cargo project will be created"
                .into(),
        );
        dialogs.pick_folder(
            DialogPurpose::ProjectParentFolder,
            rfd::AsyncFileDialog::new()
                .set_title("Choose Where to Create the New Project"),
        );
    }
    if save_clicked && state.scene_path.is_empty() {
        save_clicked = false;
        save_scene_as = true;
    }
    let requested_project =
        draw_project_manager(context, &mut project_manager, &mut dialogs)
            .or(dialog_project_request);
    if open_scene {
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Open RustingEngine Scene")
            .add_filter("RustingEngine Scene", &["rscene"]);
        if has_open_project {
            dialog = dialog.set_directory(
                std::path::Path::new(&state.project_root).join("scenes"),
            );
        }
        dialogs.pick_file(DialogPurpose::OpenScene, dialog);
    }
    let requested_action = requested_project
        .map(DestructiveRequest::OpenProject)
        .or_else(|| dialog_scene.map(DestructiveRequest::OpenScene))
        .or_else(|| new_scene.then_some(DestructiveRequest::NewScene))
        .or_else(|| load_clicked.then_some(DestructiveRequest::ReloadScene));
    let mut action_to_run = None;
    if let Some(request) = requested_action {
        if state.scene_dirty {
            pending_action.request = Some(request);
        } else {
            action_to_run = Some(request);
        }
    }
    if pending_action.request.is_some() {
        let mut save_and_continue = false;
        let mut discard_and_continue = false;
        let mut cancel = false;
        egui::Window::new("Unsaved Scene")
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(context, |ui| {
                ui.label("The current scene has unsaved changes.");
                ui.label("Save it before continuing?");
                ui.horizontal(|ui| {
                    save_and_continue =
                        ui.button("Save and Continue").clicked();
                    discard_and_continue =
                        ui.button("Discard Changes").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        let chosen_path = if save_before_continue_to.is_some() {
            save_before_continue_to
        } else if save_and_continue && state.scene_path.is_empty() {
            let mut dialog = rfd::AsyncFileDialog::new()
                .set_title("Save RustingEngine Scene")
                .set_file_name("main.rscene")
                .add_filter("RustingEngine Scene", &["rscene"]);
            if has_open_project {
                dialog = dialog.set_directory(
                    std::path::Path::new(&state.project_root).join("scenes"),
                );
            }
            dialogs.save_file(DialogPurpose::SaveSceneBeforeContinue, dialog);
            None
        } else if save_and_continue {
            Some(
                project_content_path(&state.project_root, &state.scene_path)
                    .ok(),
            )
        } else {
            None
        };
        if let Some(chosen_path) = chosen_path {
            let result = chosen_path
                .ok_or_else(|| "Save was cancelled".to_owned())
                .and_then(|path| {
                    let relative =
                        project_relative_path(&state.project_root, &path)?;
                    save_editor_scene(world, &state, &path)
                        .map(|()| relative)
                        .map_err(|error| error.to_string())
                });
            match result {
                Ok(relative) => {
                    state.scene_path = relative;
                    state.scene_dirty = false;
                    action_to_run = pending_action.request.take();
                }
                Err(error) => {
                    state.scene_message = Some(format!(
                        "Could not save before continuing: {error}"
                    ));
                }
            }
        } else if discard_and_continue {
            action_to_run = pending_action.request.take();
        } else if cancel {
            pending_action.request = None;
        }
    }
    // Convert the confirmed request back into the small flags used below.
    new_scene = matches!(action_to_run, Some(DestructiveRequest::NewScene));
    load_clicked =
        matches!(action_to_run, Some(DestructiveRequest::ReloadScene));
    let open_scene_path = match &action_to_run {
        Some(DestructiveRequest::OpenScene(path)) => Some(path.clone()),
        _ => None,
    };
    let project_request = match action_to_run {
        Some(DestructiveRequest::OpenProject(request)) => Some(request),
        _ => None,
    };

    if reset_layout {
        state.dock_layout = EditorDockNode::default_layout();
        state.active_area = 2;
        state.next_area_id = 5;
    }
    if add_area {
        let new_id = state.next_area_id;
        state.next_area_id = state.next_area_id.saturating_add(1);
        if state.dock_layout.split(
            state.active_area,
            EditorSplitAxis::Columns,
            new_id,
        ) {
            state.active_area = new_id;
        }
    }

    // Move the layout out of EditorState for a moment. Panel code needs to edit
    // the rest of EditorState while the layout tree is also borrowed.
    let mut dock_layout = std::mem::replace(
        &mut state.dock_layout,
        EditorDockNode::Area {
            id: 0,
            panel: EditorPanel::Scene,
        },
    );
    let mut dock_actions = Vec::new();
    let mut rendered_workspace = None;
    let mut active_area = state.active_area;
    let mut focus_requested = false;
    let mut orthographic = world
        .get_resource::<shortcuts::EditorFlyCamera>()
        .is_some_and(|fly| fly.perspective_fov.is_some());
    let was_orthographic = orthographic;
    // Every movable area lives inside this one transparent central panel.
    CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::TRANSPARENT))
        .show(context, |ui| {
            let rect = ui.available_rect_before_wrap();
            show_dock_node(
                ui,
                &mut dock_layout,
                rect,
                &mut active_area,
                &mut dock_actions,
                &mut |ui, panel| match panel {
                    EditorPanel::Hierarchy => {
                        draw_hierarchy_area(
                            ui,
                            world,
                            &entities,
                            &mut state,
                            &mut entity_request,
                            &mut asset_request,
                            &mut edited_transform,
                            &mut edited_camera,
                            &mut edited_physics,
                            &mut edited_rigid_body,
                            &mut edited_collider,
                        );
                    }
                    EditorPanel::Inspector
                        if editor_assets.inspected.is_some() =>
                    {
                        let inspection = editor_assets
                            .inspected
                            .as_mut()
                            .expect("checked by the match guard");
                        match super::inspector::data_asset::draw_data_asset(
                            ui, world, inspection,
                        ) {
                            super::inspector::data_asset::DataAssetAction::None => {}
                            super::inspector::data_asset::DataAssetAction::Save => {
                                asset_request =
                                    Some(AssetRequest::SaveDataAsset);
                            }
                            super::inspector::data_asset::DataAssetAction::Close => {
                                editor_assets.inspected = None;
                            }
                        }
                    }
                    EditorPanel::Inspector => draw_inspector_area(
                        ui,
                        world,
                        &mut state,
                        physics_backends,
                        &mut edited_transform,
                        &mut edited_camera,
                        &mut edited_directional_light,
                        &mut edited_point_light,
                        &mut edited_spot_light,
                        &mut edited_classes,
                        &mut edited_physics,
                        &mut edited_rigid_body,
                        &mut edited_collider,
                        &mut edited_render_bounds,
                        &mut edited_material,
                        &mut asset_request,
                        &registered_names,
                        &custom_values,
                        &mut component_edits,
                        &mut add_physics,
                        &mut remove_physics,
                        &mut edit_custom_shader,
                    ),
                    EditorPanel::Scene | EditorPanel::Game => {
                        let workspace = if panel == EditorPanel::Scene {
                            EditorWorkspace::Scene
                        } else {
                            EditorWorkspace::Game
                        };
                        if workspace == EditorWorkspace::Game {
                            ui.small("Active game camera | runtime preview");
                            ui.separator();
                        }
                        if viewport_rect.is_none() {
                            let rect = ui.available_rect_before_wrap();
                            ui.painter().image(
                                super::SCENE_VIEW_TEXTURE,
                                rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                            if workspace == EditorWorkspace::Scene {
                                let response = ui.interact(
                                    rect,
                                    ui.id().with("scene_view_pick_area"),
                                    egui::Sense::click_and_drag(),
                                );
                                scene_hover_position = response.hover_pos();
                                scene_hovered = response.contains_pointer();
                                // ponytail: dropped models land at the
                                // origin; place them under the cursor once
                                // picking returns surface hit points.
                                if response
                                    .dnd_hover_payload::<assets_panel::ModelDrag>()
                                    .is_some()
                                {
                                    ui.painter().rect_stroke(
                                        rect.shrink(1.0),
                                        0.0,
                                        egui::Stroke::new(
                                            2.0_f32,
                                            gui_elements::EditorTheme::ACCENT_HOVER,
                                        ),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                if let Some(model) = response
                                    .dnd_release_payload::<assets_panel::ModelDrag>()
                                {
                                    asset_request = Some(AssetRequest::AddModel(
                                        model.0.clone(),
                                    ));
                                }
                                if response.clicked() {
                                    scene_click_position =
                                        response.interact_pointer_pos();
                                }
                                ui.input(|input| {
                                    if input.pointer.button_pressed(
                                        egui::PointerButton::Primary,
                                    ) {
                                        scene_drag_left_started = input
                                            .pointer
                                            .press_origin()
                                            .or_else(|| input.pointer.latest_pos());
                                    }
                                    if input.pointer.button_down(
                                        egui::PointerButton::Primary,
                                    ) {
                                        scene_drag_left_position =
                                            input.pointer.latest_pos();
                                    }
                                    if input.pointer.button_released(
                                        egui::PointerButton::Primary,
                                    ) {
                                        scene_drag_left_stopped = true;
                                    }
                                });
                                if response.drag_started_by(egui::PointerButton::Secondary) {
                                    scene_drag_right_started =
                                        response.interact_pointer_pos();
                                }
                                if response.dragged_by(egui::PointerButton::Secondary) {
                                    scene_drag_right_position =
                                        response.interact_pointer_pos();
                                }
                                if response.drag_stopped_by(egui::PointerButton::Secondary) {
                                    scene_drag_right_stopped = true;
                                }
                                scene_right_clicked = response.clicked_by(egui::PointerButton::Secondary);
                                let (transform_toolbar_hovered, focus_clicked) =
                                    draw_viewport_transform_toolbar(
                                        ui,
                                        rect,
                                        &mut transform_mode,
                                        &editor_shortcuts,
                                        gizmo_drag.is_active(),
                                        state.selected.is_some(),
                                    );
                                focus_requested |= focus_clicked;
                                let settings_hovered =
                                    draw_viewport_render_settings(
                                        ui,
                                        rect,
                                        &mut gizmo_settings,
                                        &mut orthographic,
                                    );
                                if transform_toolbar_hovered || settings_hovered {
                                    // The toolbar floats over the viewport, so its
                                    // clicks must not also reach picking or a gizmo.
                                    scene_hover_position = None;
                                    scene_click_position = None;
                                    scene_drag_left_started = None;
                                    scene_drag_left_position = None;
                                    scene_drag_left_stopped = false;
                                    scene_drag_right_started = None;
                                    scene_drag_right_position = None;
                                    scene_drag_right_stopped = false;
                                    scene_right_clicked = false;
                                    scene_hovered = false;
                                }
                            }
                            viewport_rect = Some(rect);
                            rendered_workspace = Some(workspace);
                        } else {
                            ui.centered_and_justified(|ui| {
                                ui.label("Only one live 3D viewport is currently supported.");
                            });
                        }
                    }
                    EditorPanel::Code => {
                        egui::ScrollArea::both()
                            .id_salt("code_panel_scroll")
                            .auto_shrink([false, false])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                            )
                            .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            load_code = ui
                                .add_enabled(
                                    has_open_project,
                                    egui::Button::new("Open"),
                                )
                                .clicked();
                            save_code = ui
                                .add_enabled(
                                    has_open_project,
                                    egui::Button::new("Save"),
                                )
                                .clicked();
                            validate_code = ui
                                .add_enabled(
                                    !build_running && has_open_project,
                                    egui::Button::new("Check"),
                                )
                                .clicked();
                            run_project |= ui
                                .add_enabled(
                                    !build_running && has_open_project,
                                    egui::Button::new(format!(
                                        "Build & Run ({})",
                                        state.game_build_profile.label()
                                    )),
                                )
                                .on_hover_text("Save, cook, and run native Rust")
                                .clicked();
                            if state.code_dirty {
                                ui.colored_label(
                                    gui_elements::EditorTheme::WARNING,
                                    "Unsaved",
                                );
                            }
                        });
                        ui.small("Project-relative source path");
                        ui.text_edit_singleline(&mut state.code_path);
                        if let Some(message) = &state.code_message {
                            ui.small(message);
                        }
                        if build_running {
                            ui.horizontal(|ui| {
                                ui.colored_label(
                                    gui_elements::EditorTheme::WARNING,
                                    "Build or native game is running...",
                                );
                                stop_build |= ui.button("Stop").clicked();
                                restart_build |= ui.button("Restart").clicked();
                                reload_code |=
                                    ui.button("Reload Code").clicked();
                            });
                        }
                        // Build logs live in Console, so the source editor
                        // always receives the complete remaining panel height.
                        let source_height = ui.available_height().max(160.0);
                        let editor = egui::TextEdit::multiline(&mut state.code_source)
                            .code_editor()
                            .desired_width(f32::INFINITY)
                            .lock_focus(true);
                        if ui
                            .add_sized(
                                egui::vec2(ui.available_width(), source_height),
                                editor,
                            )
                            .changed()
                        {
                            state.code_dirty = true;
                        }
                            });
                    }
                    EditorPanel::Project => {
                        let physics_text = physics_label.as_deref();
                        let export = project_settings::draw(
                            ui,
                            project_settings::ProjectSettingsView {
                                project_root: &state.project_root,
                                scene_path: &mut state.scene_path,
                                scene_message: state.scene_message.as_deref(),
                                settings: &mut render_settings,
                                build_running,
                                asset_counts,
                                capabilities: renderer_capabilities.as_ref(),
                                extraction: render_report,
                                culling: culling_stats,
                                counters: render_counters,
                                cpu: cpu_timings,
                                gpu_passes: gpu_pass_times.as_deref(),
                                physics: physics_text,
                                windows_target: WINDOWS_TARGET,
                            },
                        );
                        if let Some(export) = export {
                            dialogs.pick_folder(
                                DialogPurpose::ExportGame {
                                    target: match export {
                                        project_settings::ExportRequest::Native => None,
                                        project_settings::ExportRequest::Windows => {
                                            Some(WINDOWS_TARGET)
                                        }
                                    },
                                },
                                rfd::AsyncFileDialog::new()
                                    .set_title("Choose Export Parent Folder"),
                            );
                        }
                    }
                    EditorPanel::Shortcuts => {
                        let mut shortcuts =
                            world.resource_mut::<EditorShortcuts>();
                        if shortcuts::draw_shortcuts_area(ui, &mut shortcuts) {
                            if let Err(error) = shortcuts.save() {
                                state.scene_message = Some(format!(
                                    "Could not save shortcuts: {error}"
                                ));
                            }
                        }
                    }
                    EditorPanel::Profiler => {
                        if let Some(mut profiler) =
                            world.get_resource_mut::<profiler::EditorProfiler>()
                        {
                            profiler::draw_profiler_area(ui, &mut profiler);
                        }
                    }
                    EditorPanel::RenderSettings => {
                        diagnostics::draw_render_settings_area(
                            ui,
                            &mut render_settings,
                            renderer_capabilities.as_ref(),
                            culling_stats.as_ref(),
                            render_capacity.as_ref(),
                        );
                    }
                    EditorPanel::PhysicsDiagnostics => {
                        diagnostics::draw_physics_diagnostics_area(
                            ui,
                            physics_backends,
                            physics_settings.as_ref(),
                            world.get_resource::<RenderWorld>(),
                            render_counters.as_ref(),
                            render_capacity.as_ref(),
                        );
                    }
                    EditorPanel::Timeline => timeline::draw_timeline_area(
                        ui,
                        world,
                        &mut state,
                        &mut component_edits,
                    ),
                    EditorPanel::Agent => {
                        let clicked = world
                            .get_resource_mut::<agent_panel::AgentJournal>()
                            .and_then(|mut journal| {
                                agent_panel::draw_agent_area(
                                    ui,
                                    &mut journal,
                                    &state.project_root,
                                )
                            });
                        if let Some(ids) = clicked {
                            highlight_scene_ids(world, &mut state, &ids);
                        }
                    }
                    EditorPanel::Console => {
                        if let Some(mut console) =
                            world.get_resource_mut::<EditorConsole>()
                        {
                            draw_console_area(ui, &mut console);
                        } else {
                            ui.colored_label(
                                gui_elements::EditorTheme::ERROR,
                                "EditorConsole is unavailable. Install EditorPlugin.",
                            );
                        }
                    }
                    EditorPanel::Assets => {
                        assets_panel::draw_assets_area(
                            ui,
                            &mut editor_assets,
                            &state.project_root,
                            &asset_files,
                            state.selected,
                            &data_types,
                            &mut dialogs,
                            &mut asset_request,
                        );
                    }
                },
            );
        });
    state.dock_layout = dock_layout;
    state.active_area = active_area;
    apply_dock_actions(&mut state, dock_actions);
    let add_object_parent = state.add_object_parent;
    draw_add_object_modal(
        context,
        &mut state.add_object_modal_open,
        add_object_parent,
        world,
        &mut entity_request,
    );
    if !state.add_object_modal_open {
        state.add_object_parent = None;
    }
    let layout_path =
        std::path::Path::new(&state.project_root).join("editor_layout.json");
    if save_layout {
        let file = EditorLayoutFile {
            layout: state.dock_layout.clone(),
            active_area: state.active_area,
            next_area_id: state.next_area_id,
            snap: Some((
                transform_mode.snap_enabled,
                transform_mode.snap_steps,
            )),
        };
        state.scene_message = Some(
            serde_json::to_vec_pretty(&file)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    crate::runtime::write_atomic(&layout_path, &bytes)
                        .map_err(|error| error.to_string())
                })
                .map_or_else(
                    |error| format!("Could not save layout: {error}"),
                    |()| format!("Saved layout to {}", layout_path.display()),
                ),
        );
    }
    if load_layout {
        state.scene_message = Some(
            std::fs::read(&layout_path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    serde_json::from_slice::<EditorLayoutFile>(&bytes)
                        .map_err(|error| error.to_string())
                })
                .map_or_else(
                    |error| format!("Could not load layout: {error}"),
                    |file| {
                        state.dock_layout = file.layout;
                        state.active_area = file.active_area;
                        state.next_area_id = file.next_area_id;
                        if let Some((enabled, steps)) = file.snap {
                            transform_mode.snap_enabled = enabled;
                            transform_mode.snap_steps = steps;
                        }
                        format!("Loaded layout from {}", layout_path.display())
                    },
                ),
        );
    }
    if let Some(workspace) = rendered_workspace {
        state.workspace = workspace;
    }

    // Dragging a handle of the selected object edits one field instead of
    // picking: a fog square sets `height` (main square) or
    // `height_falloff`, and a reflection probe face sets its `extents`.
    if let (Some(entity), Some(viewport), Some(camera), true) = (
        state.selected,
        viewport_rect,
        state.editor_camera,
        state.mode == EditorMode::Edit
            && state.workspace == EditorWorkspace::Scene,
    ) {
        let grabbed = scene_drag_left_started
            .filter(|&point| viewport.contains(point))
            .and_then(|point| {
                grab_scene_handle(world, camera, entity, point, viewport)
            });
        if let Some(handle) = grabbed {
            // ponytail: a drag that ends where it began still adds an Undo
            // step, like a tile stroke.
            if let Err(error) = remember_scene_before_edit(world, &mut history)
            {
                state.scene_message = Some(error);
            }
            state.scene_handle = Some((entity, handle));
        }
        if let Some((_, handle)) =
            state.scene_handle.filter(|handle| handle.0 == entity)
        {
            if let Some(point) = scene_drag_left_position {
                state.scene_dirty |= drag_scene_handle(
                    world, camera, entity, handle, point, viewport,
                );
            }
            scene_click_position = None;
            scene_drag_left_started = None;
            scene_drag_left_position = None;
            scene_drag_left_stopped = false;
        }
    }
    if !context.input(|input| input.pointer.primary_down()) {
        state.scene_handle = None;
    }

    // With a tile brush on a selected tile map, left clicks and drags in
    // the Scene View paint instead of reaching the gizmo and picking.
    let tile_map = state.selected.filter(|&entity| {
        world.get::<crate::runtime::TileMap>(entity).is_some()
    });
    let mut tile_preview = None;
    if let (Some(brush), Some(map), true) = (
        state.tile_brush,
        tile_map,
        state.mode == EditorMode::Edit
            && state.workspace == EditorWorkspace::Scene,
    ) {
        let inside = |point: Pos2| {
            viewport_rect.is_some_and(|rect| rect.contains(point))
        };
        let camera = state.editor_camera;
        let cell_at = |world: &World, point: Option<Pos2>| {
            tile_cell_under(
                world,
                camera?,
                map,
                point.filter(|&point| inside(point))?,
                viewport_rect?,
            )
        };
        let hover =
            cell_at(world, scene_drag_left_position.or(scene_hover_position));
        let pressed = cell_at(world, scene_drag_left_started);
        if scene_drag_left_started.is_some_and(inside) {
            state.tile_stroke_undo =
                match scene_document(world, "Undo Snapshot") {
                    Ok(document) => Some(document),
                    Err(error) => {
                        state.scene_message = Some(format!(
                            "Could not create undo snapshot: {error}"
                        ));
                        None
                    }
                };
            state.tile_rect_start = pressed;
        }
        // Only strokes that began in the viewport took an Undo snapshot.
        let stroke = context
            .input(|input| input.pointer.press_origin())
            .is_some_and(inside);
        let changed = match state.tile_tool {
            TileTool::Paint => {
                stroke
                    && cell_at(
                        world,
                        scene_drag_left_position.or(scene_drag_left_started),
                    )
                    .is_some_and(|(column, row)| {
                        edit_tile_map(world, map, |tiles| {
                            tiles.set_cell(column, row, brush)
                        })
                    })
            }
            TileTool::Fill => pressed.is_some_and(|(column, row)| {
                edit_tile_map(world, map, |tiles| {
                    tiles.fill(column, row, brush)
                })
            }),
            TileTool::Rectangle | TileTool::Line if scene_drag_left_stopped => {
                let tool = state.tile_tool;
                state.tile_rect_start.take().zip(hover).is_some_and(
                    |(from, to)| {
                        edit_tile_map(world, map, |tiles| {
                            if tool == TileTool::Line {
                                tiles.fill_line(from, to, brush)
                            } else {
                                tiles.fill_rect(from, to, brush)
                            }
                        })
                    },
                )
            }
            TileTool::Rectangle | TileTool::Line => false,
        };
        if changed {
            state.scene_dirty = true;
            if let Some(document) = state.tile_stroke_undo.take() {
                if let Some(pending) = history.pending_inspector.take() {
                    history.push_undo(pending);
                }
                history.push_undo(document);
            }
        }
        if !stroke {
            state.tile_stroke_undo = None;
            state.tile_rect_start = None;
        }
        // The cell under the pointer, or the rectangle or line being
        // dragged, as rectangles to outline.
        tile_preview = hover.map(|to| {
            let from = state.tile_rect_start.unwrap_or(to);
            let rects = match state.tile_tool {
                TileTool::Rectangle => vec![(from, to)],
                TileTool::Line => crate::runtime::TileMap::line_cells(from, to)
                    .into_iter()
                    .map(|cell| (cell, cell))
                    .collect(),
                TileTool::Paint | TileTool::Fill => vec![(to, to)],
            };
            (map, rects)
        });
        scene_click_position = None;
        scene_drag_left_started = None;
        scene_drag_left_position = None;
        scene_drag_left_stopped = false;
    }

    let (hovered_gizmo, gizmo_consumed) =
        if state.workspace == EditorWorkspace::Scene {
            update_transform_gizmo(
                world,
                &mut state,
                viewport_rect,
                scene_hover_position,
                scene_drag_left_started,
                scene_drag_left_position,
                scene_drag_left_stopped,
                &mut gizmo_drag,
                &mut edited_transform,
                &mut history,
                scene_right_clicked || right_pressed,
                left_pressed,
                escape_pressed,
                &mut transform_mode,
            )
        } else {
            (None, false)
        };

    // A Scene View click selects the closest renderable mesh under the editor
    // camera ray. Game View clicks never change editor selection.
    if let (Some(click), Some(viewport), Some(camera)) =
        (scene_click_position, viewport_rect, state.editor_camera)
    {
        if !gizmo_consumed {
            let picked = pick_entity(world, camera, click, viewport);
            let extend = context.input(|input| input.modifiers.shift);
            match (extend, picked) {
                (true, Some(picked)) => {
                    (state.selection, state.selected) =
                        super::hierarchy::shift_pick_selection(
                            &state.selection,
                            state.selected,
                            picked,
                        );
                }
                // Shift-clicking empty space keeps the selection.
                (true, None) => {}
                (false, picked) => {
                    state.selected = picked;
                    state.selection = picked.into_iter().collect();
                }
            }
            state.rename_draft = state
                .selected
                .and_then(|entity| world.get::<Name>(entity))
                .map_or_else(String::new, |name| name.0.clone());
        }
    }

    // Convert egui points into physical pixels used by the Vulkan viewport.
    if let Some(rect) = viewport_rect {
        let pixels_per_point = context.pixels_per_point();
        let min = rect.min * pixels_per_point;
        let max = rect.max * pixels_per_point;
        let offset =
            [min.x.max(0.0).floor() as u32, min.y.max(0.0).floor() as u32];
        let end = [
            max.x.max(min.x).ceil() as u32,
            max.y.max(min.y).ceil() as u32,
        ];
        let viewport = EditorViewport {
            offset,
            extent: [
                end[0].saturating_sub(offset[0]),
                end[1].saturating_sub(offset[1]),
            ],
            valid: true,
            hovered: scene_hovered,
        };
        if let Some(mut current) = world.get_resource_mut::<EditorViewport>() {
            *current = viewport;
        } else {
            world.insert_resource(viewport);
        }
    } else if let Some(mut current) = world.get_resource_mut::<EditorViewport>()
    {
        current.valid = false;
    }

    // Scene file buttons use the operating system picker, so game projects
    // can keep every editable file inside their selected project folder.
    if let Some(path) = open_scene_path {
        let result = project_relative_path(&state.project_root, &path)
            .and_then(|relative| {
                load_editor_scene(world, &mut state, &path)
                    .map(|count| (count, relative))
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok((entity_count, relative)) => {
                state.scene_path = relative;
                state.scene_dirty = false;
                history.undo.clear();
                history.redo.clear();
                history.pending_inspector = None;
                state.scene_message = Some(format!(
                    "Opened {entity_count} objects from {}",
                    path.display()
                ));
            }
            Err(error) => {
                state.scene_message =
                    Some(format!("Could not open scene: {error}"));
            }
        }
    }
    if save_scene_as {
        let suggested_name = std::path::Path::new(&state.scene_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("main.rscene");
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Save RustingEngine Scene")
            .set_file_name(suggested_name)
            .add_filter("RustingEngine Scene", &["rscene"]);
        if has_open_project {
            dialog = dialog.set_directory(
                std::path::Path::new(&state.project_root).join("scenes"),
            );
        }
        dialogs.save_file(DialogPurpose::SaveSceneAs, dialog);
    }
    if let Some(path) = save_as_path {
        let result = project_relative_path(&state.project_root, &path)
            .and_then(|relative| {
                save_editor_scene(world, &state, &path)
                    .map(|()| relative)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(relative) => {
                state.scene_path = relative;
                state.scene_dirty = false;
                state.scene_message =
                    Some(format!("Saved {}", state.scene_path));
            }
            Err(error) => {
                state.scene_message = Some(format!("Save As failed: {error}"));
            }
        }
    }
    if new_scene {
        let empty = SceneDocument {
            format_version: crate::runtime::SCENE_FORMAT_VERSION,
            name: "New Scene".into(),
            entities: Vec::new(),
            render: Default::default(),
            simulation: Default::default(),
        };
        match crate::runtime::load_scene_document(
            world,
            &empty,
            SceneLoadMode::Replace,
        ) {
            Ok(_) => {
                state.selected = None;
                state.scene_path.clear();
                state.scene_dirty = true;
                history.undo.clear();
                history.redo.clear();
                history.pending_inspector = None;
                state.scene_message = Some(
                    "Created a new scene. Use Save As to choose its file."
                        .into(),
                );
            }
            Err(error) => {
                state.scene_message =
                    Some(format!("Could not create scene: {error}"));
            }
        }
    }
    if undo_clicked {
        if let Some(document) = history.pending_inspector.take() {
            history.push_undo(document);
        }
        if let Some(previous) = history.undo.pop_back() {
            match scene_document(world, "Redo Snapshot") {
                Ok(current) => {
                    match reload_keeping_selection(world, &mut state, |world| {
                        crate::runtime::load_scene_document(
                            world,
                            &previous,
                            SceneLoadMode::Replace,
                        )
                    }) {
                        Ok(()) => {
                            history.redo.push(current);
                            gizmo_drag = EditorGizmoDrag::default();
                            finish_transform_mode(&mut transform_mode);
                            state.scene_dirty = true;
                            state.scene_message = Some("Undo applied".into());
                            timeline::scene_replaced(world);
                        }
                        Err(error) => {
                            history.undo.push_back(previous);
                            state.scene_message =
                                Some(format!("Undo failed: {error}"));
                        }
                    }
                }
                Err(error) => {
                    history.undo.push_back(previous);
                    state.scene_message = Some(format!("Undo failed: {error}"));
                }
            }
        }
    }
    if redo_clicked {
        if let Some(next) = history.redo.pop() {
            match scene_document(world, "Undo Snapshot") {
                Ok(current) => {
                    match reload_keeping_selection(world, &mut state, |world| {
                        crate::runtime::load_scene_document(
                            world,
                            &next,
                            SceneLoadMode::Replace,
                        )
                    }) {
                        Ok(()) => {
                            history.undo.push_back(current);
                            gizmo_drag = EditorGizmoDrag::default();
                            finish_transform_mode(&mut transform_mode);
                            state.scene_dirty = true;
                            state.scene_message = Some("Redo applied".into());
                            timeline::scene_replaced(world);
                        }
                        Err(error) => {
                            history.redo.push(next);
                            state.scene_message =
                                Some(format!("Redo failed: {error}"));
                        }
                    }
                }
                Err(error) => {
                    history.redo.push(next);
                    state.scene_message = Some(format!("Redo failed: {error}"));
                }
            }
        }
    }

    let inspector_changed = !gizmo_consumed
        && state.selected == original_selected
        && (edited_transform != original_transform
            || edited_camera != original_camera
            || edited_directional_light != original_directional_light
            || edited_point_light != original_point_light
            || edited_spot_light != original_spot_light
            || edited_classes != original_classes
            || edited_physics != original_physics
            || edited_rigid_body != original_rigid_body
            || edited_collider != original_collider
            || edited_render_bounds != original_render_bounds
            || edited_material != original_material
            || add_physics
            || remove_physics
            // Custom component field edits are live; drags coalesce into
            // one Undo step like the built-in fields.
            || component_edits
                .iter()
                .any(|edit| matches!(edit, ComponentEdit::Set { .. })));
    if inspector_changed {
        if history.pending_inspector.is_none() {
            match scene_document(world, "Inspector Undo Snapshot") {
                Ok(document) => history.pending_inspector = Some(document),
                Err(error) => {
                    state.scene_message = Some(format!(
                        "Could not create undo snapshot: {error}"
                    ));
                }
            }
        }
        state.scene_dirty = true;
    } else if let Some(document) = history.pending_inspector.take() {
        // The field stopped changing, so all drag frames become one Undo step.
        history.push_undo(document);
    }

    // A Hierarchy click changes selection after these temporary values were
    // copied. Never write the previous object's values into the new object.
    let unchanged_selection = state.selected == original_selected;

    // Write temporary Inspector values back to the selected ECS object. Only
    // changed values are inserted, so ECS change detection stays meaningful.
    if let (true, Some(entity), Some(transform)) = (
        unchanged_selection && edited_transform != original_transform,
        state.selected,
        edited_transform,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(transform);
        }
    }
    if let (true, Some(entity), Some(camera)) = (
        unchanged_selection && edited_camera != original_camera,
        state.selected,
        edited_camera,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(camera);
        }
    }
    if let (true, Some(entity), Some(light)) = (
        unchanged_selection
            && edited_directional_light != original_directional_light,
        state.selected,
        edited_directional_light,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(light);
        }
    }
    if let (true, Some(entity), Some(light)) = (
        unchanged_selection && edited_point_light != original_point_light,
        state.selected,
        edited_point_light,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(light);
        }
    }
    if let (true, Some(entity), Some(light)) = (
        unchanged_selection && edited_spot_light != original_spot_light,
        state.selected,
        edited_spot_light,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(light);
        }
    }
    if let (true, Some(entity), Some(classes)) = (
        unchanged_selection && edited_classes != original_classes,
        state.selected,
        edited_classes,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            if classes.names.is_empty() {
                entity.remove::<ObjectClasses>();
            } else {
                entity.insert(classes);
            }
        }
    }
    if let (true, Some(entity)) = (
        unchanged_selection && edited_render_bounds != original_render_bounds,
        state.selected,
    ) {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            match edited_render_bounds {
                Some(bounds) => entity.insert(bounds),
                None => entity.remove::<RenderBounds>(),
            };
        }
    }
    if let (true, Some(entity), Some(material)) = (
        unchanged_selection && edited_material != original_material,
        state.selected,
        edited_material,
    ) {
        set_material(world, entity, material);
    }
    let physics_changed = add_physics
        || remove_physics
        || edited_physics != original_physics
        || edited_rigid_body != original_rigid_body
        || edited_collider != original_collider;
    if let (true, Some(entity)) =
        (unchanged_selection && physics_changed, state.selected)
    {
        if let Ok(mut entity_mut) = world.get_entity_mut(entity) {
            if remove_physics {
                entity_mut.remove::<(
                    PhysicsBody,
                    RigidBody,
                    Collider,
                    CollisionLayers,
                    crate::runtime::PhysicsSyncMode,
                    crate::runtime::GpuStateMirror,
                )>();
            } else if let Some(physics) = edited_physics {
                entity_mut.insert(physics);
                if let Some(rigid_body) = edited_rigid_body {
                    entity_mut.insert(rigid_body);
                }
                if let Some(collider) = edited_collider {
                    entity_mut.insert(collider);
                }
                if add_physics && !entity_mut.contains::<CollisionLayers>() {
                    entity_mut.insert(CollisionLayers::default());
                }
            }
        }
    }
    if edit_custom_shader {
        if let Some(path) = state
            .selected
            .and_then(|entity| world.get::<PhysicsBody>(entity))
            .and_then(|physics| physics.custom_shader.clone())
        {
            let exists = std::path::Path::new(&path).exists();
            state.code_path = path;
            state.workspace = EditorWorkspace::Code;
            state
                .dock_layout
                .set_panel(state.active_area, EditorPanel::Code);
            if exists {
                load_code = true;
            } else {
                // A new solver starts from a template, saved on Save.
                state.code_source = CUSTOM_SOLVER_TEMPLATE.into();
                state.code_dirty = true;
                state.code_message =
                    Some(format!("New custom solver {}", state.code_path));
            }
        }
    }
    if load_code {
        let result = project_source_path(&state.project_root, &state.code_path)
            .and_then(|path| {
                let source =
                    std::fs::read_to_string(&path).map_err(|error| {
                        format!("Could not open {}: {error}", path.display())
                    })?;
                Ok((path, source))
            });
        match result {
            Ok((path, source)) => {
                state.code_path =
                    project_relative_path(&state.project_root, &path)
                        .unwrap_or_else(|_| state.code_path.clone());
                state.code_source = source;
                state.code_dirty = false;
                state.code_message =
                    Some(format!("Opened {}", state.code_path));
            }
            Err(error) => state.code_message = Some(error),
        }
    }
    if stop_build {
        world.resource_mut::<EditorBuildState>().request_stop();
    } else if restart_build {
        world.resource_mut::<EditorBuildState>().request_restart();
    } else if reload_code {
        world
            .resource_mut::<EditorBuildState>()
            .request_code_reload();
    }
    if preview_start {
        start_embedded_preview(world, &mut state, &mut history);
    }
    if preview_pause {
        set_embedded_preview_paused(world, &mut state, true);
    }
    if preview_resume {
        set_embedded_preview_paused(world, &mut state, false);
    }
    if preview_step {
        world.resource_mut::<crate::runtime::TimeControl>().step();
    }
    if preview_stop {
        stop_embedded_preview(world, &mut state, &mut history);
    }
    if validate_code {
        let is_rust = std::path::Path::new(&state.code_path)
            .extension()
            .is_some_and(|extension| extension == "rs");
        let result = if is_rust {
            save_project_source(
                &state.project_root,
                &state.code_path,
                &state.code_source,
            )
        } else {
            validate_project_source(
                &state.project_root,
                &state.code_path,
                &state.code_source,
            )
        };
        if result.is_ok() && is_rust {
            state.code_dirty = false;
            build_request = Some(BuildRequest::Check);
        }
        state.code_message = Some(result.map_or_else(
            |error| error,
            |()| {
                if is_rust {
                    "Saved Rust source and started cargo check".into()
                } else {
                    "Shader validation passed".into()
                }
            },
        ));
    }
    if save_project {
        let result = (|| -> Result<(), String> {
            // Opening validates Cargo.toml, project.json, and the standard
            // project files before any editable document is written.
            open_project(std::path::Path::new(&state.project_root))
                .map_err(|error| error.to_string())?;
            let scene_path =
                project_content_path(&state.project_root, &state.scene_path)?;
            save_project_source(
                &state.project_root,
                &state.code_path,
                &state.code_source,
            )?;
            save_editor_scene(world, &state, &scene_path)
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                state.code_dirty = false;
                state.scene_dirty = false;
                state.scene_message =
                    Some(format!("Saved project {}", state.project_root));
                state.code_message = Some(format!("Saved {}", state.code_path));
            }
            Err(error) => {
                state.scene_message =
                    Some(format!("Could not save project: {error}"));
            }
        }
    }
    if save_code {
        state.code_message = Some(
            match save_project_source(
                &state.project_root,
                &state.code_path,
                &state.code_source,
            ) {
                Ok(()) => {
                    state.code_dirty = false;
                    if let Ok(path) = project_relative_path(
                        &state.project_root,
                        std::path::Path::new(&state.code_path),
                    ) {
                        state.code_path = path;
                    }
                    format!("Saved {}", state.code_path)
                }
                Err(error) => error,
            },
        );
    }
    if run_project {
        let cooked = std::path::Path::new(&state.project_root)
            .join("build/main.rscene.bin");
        let result =
            project_content_path(&state.project_root, &state.scene_path)
                .and_then(|scene_path| {
                    if state.code_dirty {
                        save_project_source(
                            &state.project_root,
                            &state.code_path,
                            &state.code_source,
                        )
                    } else {
                        Ok(())
                    }
                    .and_then(|()| {
                        save_editor_scene(world, &state, &scene_path)
                            .map_err(|error| error.to_string())
                    })
                    .and_then(|()| {
                        cook_scene(&scene_path, &cooked)
                            .map_err(|error| error.to_string())
                    })
                });
        if result.is_ok() {
            state.code_dirty = false;
            state.scene_dirty = false;
            build_request = Some(BuildRequest::BuildAndRun {
                profile: state.game_build_profile,
            });
        }
        state.code_message = Some(result.map_or_else(
            |error| format!("Could not run project: {error}"),
            |()| {
                format!(
                    "Saved and cooked; {} build started",
                    state.game_build_profile.label()
                )
            },
        ));
    }
    if let Some((parent, target)) = export_destination {
        let result = open_project(std::path::Path::new(&state.project_root))
            .map_err(|error| error.to_string())
            .and_then(|project| {
                if state.code_dirty {
                    save_project_source(
                        &state.project_root,
                        &state.code_path,
                        &state.code_source,
                    )?;
                }
                let scene_path = project_content_path(
                    &state.project_root,
                    &state.scene_path,
                )?;
                save_editor_scene(world, &state, &scene_path)
                    .map_err(|error| error.to_string())?;
                let cooked = project.root.join(&project.manifest.cooked_scene);
                cook_scene(&scene_path, &cooked)
                    .map_err(|error| error.to_string())?;
                Ok(BuildRequest::Export {
                    parent,
                    project_name: project.manifest.name,
                    binary_name: project.manifest.binary_name,
                    cooked_scene: project.manifest.cooked_scene,
                    target,
                })
            });
        match result {
            Ok(request) => {
                state.code_dirty = false;
                state.scene_dirty = false;
                build_request = Some(request);
                state.code_message =
                    Some("Saved and cooked; export build started".into());
            }
            Err(error) => {
                state.code_message =
                    Some(format!("Could not export game: {error}"));
            }
        }
    }
    if save_clicked {
        state.scene_message = Some(
            match project_content_path(&state.project_root, &state.scene_path)
                .and_then(|path| {
                    save_editor_scene(world, &state, &path)
                        .map_err(|error| error.to_string())
                }) {
                Ok(()) => {
                    state.scene_dirty = false;
                    format!("Saved {}", state.scene_path)
                }
                Err(error) => format!("Save failed: {error}"),
            },
        );
    }
    if load_clicked {
        state.scene_message = Some(
            match project_content_path(&state.project_root, &state.scene_path)
                .and_then(|path| {
                    load_editor_scene(world, &mut state, &path)
                        .map_err(|error| error.to_string())
                }) {
                Ok(entity_count) => {
                    state.scene_dirty = false;
                    history.undo.clear();
                    history.redo.clear();
                    history.pending_inspector = None;
                    format!(
                        "Loaded {entity_count} entities from {}",
                        state.scene_path
                    )
                }
                Err(error) => format!("Load failed: {error}"),
            },
        );
    }
    if let Some(request) = entity_request {
        let keeps_selection = matches!(request, EntityRequest::SetVisible(..));
        if let Err(error) = remember_scene_before_edit(world, &mut history) {
            state.scene_message = Some(error);
        } else {
            let result = (|| -> Result<Option<Entity>, String> {
                match request {
                    EntityRequest::CreateEmpty(parent) => {
                        let name = unique_object_name(world, "Empty");
                        let entity = world
                            .spawn((
                                SceneId::new(),
                                Name(name),
                                Transform::default(),
                            ))
                            .id();
                        if let Some(parent) = parent {
                            world.entity_mut(entity).insert(Parent(parent));
                        }
                        Ok(Some(entity))
                    }
                    EntityRequest::CreatePrimitive(shape, parent) => {
                        let name = unique_object_name(world, shape.label());
                        let (mesh, material) = {
                            let assets = world.resource::<AssetServer>();
                            (
                                assets.builtin_primitive(shape),
                                assets.fallback_material,
                            )
                        };
                        let entity = world
                            .spawn((
                                SceneId::new(),
                                Name(name),
                                Transform::default(),
                                MeshRenderer {
                                    mesh,
                                    material,
                                    cast_shadows: true,
                                    receive_shadows: true,
                                },
                                Visibility::default(),
                            ))
                            .id();
                        if let Some(parent) = parent {
                            world.entity_mut(entity).insert(Parent(parent));
                        }
                        Ok(Some(entity))
                    }
                    EntityRequest::CreateWith {
                        name,
                        components,
                        parent,
                    } => {
                        let name = unique_object_name(world, name);
                        let entity = world
                            .spawn((
                                SceneId::new(),
                                Name(name),
                                Transform::default(),
                            ))
                            .id();
                        if let Some(parent) = parent {
                            world.entity_mut(entity).insert(Parent(parent));
                        }
                        for component in components {
                            crate::runtime::add_registered_component(
                                world, entity, component,
                            )
                            .map_err(|error| error.to_string())?;
                        }
                        Ok(Some(entity))
                    }
                    EntityRequest::CreateCamera(parent) => {
                        let name = unique_object_name(world, "Camera");
                        let entity = world
                            .spawn((
                                SceneId::new(),
                                Name(name),
                                Transform::default(),
                                Camera {
                                    active: false,
                                    ..Camera::default()
                                },
                            ))
                            .id();
                        if let Some(parent) = parent {
                            world.entity_mut(entity).insert(Parent(parent));
                        }
                        Ok(Some(entity))
                    }
                    EntityRequest::CreateLight(kind, parent) => {
                        let name = unique_object_name(world, kind.label());
                        let mut transform = Transform::default();
                        match kind {
                            EditorLightType::Directional => {
                                transform.rotation = [
                                    -45.0_f32.to_radians(),
                                    -30.0_f32.to_radians(),
                                    0.0,
                                ];
                            }
                            EditorLightType::Point => {
                                transform.position = [0.0, 3.0, 0.0];
                            }
                            EditorLightType::Spot => {
                                transform.position = [0.0, 3.0, 0.0];
                                transform.rotation =
                                    [-90.0_f32.to_radians(), 0.0, 0.0];
                            }
                        }
                        let mut entity = world.spawn((
                            SceneId::new(),
                            Name(name),
                            transform,
                        ));
                        match kind {
                            EditorLightType::Directional => {
                                entity.insert(DirectionalLight::default());
                            }
                            EditorLightType::Point => {
                                entity.insert(PointLight::default());
                            }
                            EditorLightType::Spot => {
                                entity.insert(SpotLight::default());
                            }
                        }
                        let entity = entity.id();
                        if let Some(parent) = parent {
                            world.entity_mut(entity).insert(Parent(parent));
                        }
                        Ok(Some(entity))
                    }
                    EntityRequest::Rename(entity, name) => {
                        let duplicate = {
                            let mut names = world.query::<(Entity, &Name)>();
                            names.iter(world).any(|(other, current)| {
                                other != entity && current.0 == name
                            })
                        };
                        if duplicate {
                            return Err(format!(
                                "Another object is already named `{name}`"
                            ));
                        }
                        let mut entity =
                            world.get_entity_mut(entity).map_err(|_| {
                                "Selected object no longer exists".to_owned()
                            })?;
                        entity.insert(Name(name));
                        Ok(Some(entity.id()))
                    }
                    EntityRequest::Instance(entity, edit) => {
                        edit_scene_instance(world, entity, edit)
                    }
                    EntityRequest::Reparent(entities, parent) => {
                        super::hierarchy::reparent_entities(
                            world, &entities, parent,
                        )
                    }
                    EntityRequest::Duplicate(entities) => {
                        let mut document = scene_document(world, "Main Scene")
                            .map_err(|error| error.to_string())?;
                        let mut new_id = None;
                        for entity in entities {
                            let source_id = world
                                .get::<SceneId>(entity)
                                .copied()
                                .ok_or_else(|| {
                                "Selected object is not part of the saved scene"
                                    .to_owned()
                            })?;
                            let mut copy = document
                                .entities
                                .iter()
                                .find(|item| item.id == source_id.0)
                                .cloned()
                                .ok_or_else(|| {
                                    "Selected object was not found in the scene"
                                        .to_owned()
                                })?;
                            copy.id = uuid::Uuid::new_v4();
                            copy.name = Some(format!(
                                "{} Copy",
                                copy.name.as_deref().unwrap_or("Object")
                            ));
                            new_id = Some(copy.id);
                            document.entities.push(copy);
                        }
                        crate::runtime::load_scene_document(
                            world,
                            &document,
                            SceneLoadMode::Replace,
                        )
                        .map_err(|error| error.to_string())?;
                        let mut query = world.query::<(Entity, &SceneId)>();
                        Ok(query.iter(world).find_map(|(entity, id)| {
                            (Some(id.0) == new_id).then_some(entity)
                        }))
                    }
                    EntityRequest::Delete(entities) => {
                        let mut removed = std::collections::HashSet::new();
                        for entity in entities {
                            let source_id = world
                                .get::<SceneId>(entity)
                                .copied()
                                .ok_or_else(|| {
                                "Selected object is not part of the saved scene"
                                    .to_owned()
                            })?;
                            removed.insert(source_id.0);
                        }
                        let mut document = scene_document(world, "Main Scene")
                            .map_err(|error| error.to_string())?;
                        loop {
                            let old_count = removed.len();
                            for item in &document.entities {
                                if item
                                    .parent
                                    .is_some_and(|id| removed.contains(&id))
                                {
                                    removed.insert(item.id);
                                }
                            }
                            if removed.len() == old_count {
                                break;
                            }
                        }
                        document
                            .entities
                            .retain(|item| !removed.contains(&item.id));
                        crate::runtime::load_scene_document(
                            world,
                            &document,
                            SceneLoadMode::Replace,
                        )
                        .map_err(|error| error.to_string())?;
                        Ok(None)
                    }
                    EntityRequest::SetVisible(entity, visible) => {
                        world
                            .get_entity_mut(entity)
                            .map_err(|_| {
                                "Selected object no longer exists".to_owned()
                            })?
                            .insert(crate::runtime::Visibility { visible });
                        Ok(state.selected)
                    }
                }
            })();
            match result {
                Ok(selected) => {
                    // Scene reloads give entities new ids, so a
                    // multi-selection only survives edits that keep them.
                    if !keeps_selection {
                        state.selection = selected.into_iter().collect();
                    }
                    state.selected = selected;
                    state.rename_draft = selected
                        .and_then(|entity| world.get::<Name>(entity))
                        .map(|name| name.0.clone())
                        .unwrap_or_default();
                    state.scene_dirty = true;
                    state.scene_message = Some("Scene object changed".into());
                }
                Err(error) => {
                    // The edit did not happen, so its unused snapshot is removed.
                    history.undo.pop_back();
                    state.scene_message =
                        Some(format!("Could not change scene object: {error}"));
                }
            }
        }
    }
    editor_assets.files = asset_files;
    if let Some(request) = asset_request {
        // Imports copy files into `assets`; show them on the next frame.
        editor_assets.files_scanned = None;
        let result: Result<String, String> = match request {
            AssetRequest::ImportFiles(paths) => {
                (|| -> Result<String, String> {
                    let mut imported_paths = Vec::new();
                    let mut prepared_count = 0;
                    let mut snapshot_taken = false;
                    for source in paths {
                        let path = copy_into_project_assets(
                            &state.project_root,
                            &source,
                        )?;
                        imported_paths.push(path);
                    }
                    // Prepare only after every selected dependency was copied.
                    for path in &imported_paths {
                        match path
                            .extension()
                            .and_then(|value| value.to_str())
                            .map(str::to_ascii_lowercase)
                            .as_deref()
                        {
                            Some("png" | "jpg" | "jpeg" | "bmp" | "tga") => {
                                world
                                    .resource_mut::<AssetServer>()
                                    .load_texture(path)
                                    .map_err(|error| error.to_string())?;
                                prepared_count += 1;
                            }
                            Some("gltf" | "glb") => {
                                if !snapshot_taken {
                                    remember_scene_before_edit(
                                        world,
                                        &mut history,
                                    )?;
                                    snapshot_taken = true;
                                }
                                let root = add_model_to_scene(world, path)?;
                                select_added_model(&mut state, world, root);
                                prepared_count += 1;
                            }
                            _ => {}
                        }
                    }
                    let imported_count = imported_paths.len();
                    Ok(format!(
                        "Imported {imported_count} file(s) and prepared {prepared_count} asset(s)"
                    ))
                })()
            }
            AssetRequest::PreviewReplacement(source) => {
                match editor_assets.replace_target.take() {
                    Some(target) => {
                        let report = replace_asset(
                            &state.project_root,
                            &target,
                            &source,
                            true,
                        );
                        let message = report
                            .as_ref()
                            .map(|_| "Previewed asset replacement".to_owned())
                            .map_err(Clone::clone);
                        editor_assets.replace_preview = Some(ReplacePreview {
                            target,
                            source,
                            report,
                        });
                        message
                    }
                    None => Err("No asset was chosen to replace".into()),
                }
            }
            AssetRequest::ConfirmReplacement => {
                match editor_assets.replace_preview.take() {
                    Some(preview) => replace_asset(
                        &state.project_root,
                        &preview.target,
                        &preview.source,
                        false,
                    )
                    .map(|report| {
                        // The asset server hot-reloads the file itself.
                        editor_assets.thumbnails.remove(&preview.target);
                        format!(
                            "Replaced {} and kept ID {}",
                            report.path, report.id
                        )
                    }),
                    None => Err("No replacement to confirm".into()),
                }
            }
            AssetRequest::LoadTexture(path) => world
                .resource_mut::<AssetServer>()
                .load_texture(&path)
                .map(|_| format!("Loaded texture {}", path.display()))
                .map_err(|error| error.to_string()),
            AssetRequest::AddModel(path) => (|| -> Result<String, String> {
                remember_scene_before_edit(world, &mut history)?;
                let root =
                    add_model_to_scene(world, &path).inspect_err(|_| {
                        // Nothing was added, so the snapshot is not an Undo step.
                        history.undo.pop_back();
                    })?;
                select_added_model(&mut state, world, root);
                Ok(format!("Added {} to the scene", path.display()))
            })(),
            AssetRequest::InstanceScene(path) => {
                (|| -> Result<String, String> {
                    let open_scene = project_content_path(
                        &state.project_root,
                        &state.scene_path,
                    )
                    .ok();
                    remember_scene_before_edit(world, &mut history)?;
                    let root = instance_scene_in_scene(
                        world,
                        &path,
                        open_scene.as_deref(),
                    )
                    .inspect_err(|_| {
                        history.undo.pop_back();
                    })?;
                    select_added_model(&mut state, world, root);
                    Ok(format!("Instanced {} in the scene", path.display()))
                })()
            }
            AssetRequest::NewDataAsset { folder, type_name } => world
                .resource::<crate::assets::DataAssetTypes>()
                .get(type_name)
                .cloned()
                .ok_or_else(|| format!("`{type_name}` is not registered"))
                .and_then(|registered| new_data_asset(&folder, &registered))
                .map(|path| {
                    let message = format!("Created {}", path.display());
                    editor_assets.files_scanned = None;
                    editor_assets.selected = Some(path.clone());
                    editor_assets.inspected =
                        Some(DataInspection::open(path, state.selected));
                    message
                }),
            AssetRequest::SaveDataAsset => {
                match editor_assets.inspected.as_mut() {
                    None => Err("No data asset is open".to_owned()),
                    Some(inspection) => {
                        inspection.dirty = false;
                        let saved = save_data_inspection(world, inspection);
                        inspection.error = saved.as_ref().err().cloned();
                        saved
                    }
                }
            }
            AssetRequest::NewSceneVariant(path) => new_scene_variant(&path)
                .map(|variant| {
                    let message =
                        format!("Created variant {}", variant.display());
                    editor_assets.selected = Some(variant);
                    message
                }),
            AssetRequest::AssignTexture(path) => state
                .selected
                .filter(|entity| world.get::<MeshRenderer>(*entity).is_some())
                .ok_or_else(|| "Select a mesh object first".to_owned())
                .and_then(|entity| {
                    let texture = world
                        .resource_mut::<AssetServer>()
                        .load_texture(&path)
                        .map_err(|error| error.to_string())?;
                    remember_scene_before_edit(world, &mut history)?;
                    let mut material = {
                        let assets = world.resource::<AssetServer>();
                        let renderer = world.get::<MeshRenderer>(entity);
                        renderer
                            .and_then(|renderer| {
                                assets.materials.get(renderer.material)
                            })
                            .cloned()
                            .unwrap_or_default()
                    };
                    material.base_color_texture = Some(texture);
                    set_material(world, entity, material);
                    state.scene_dirty = true;
                    Ok(format!("Used {} as base color", path.display()))
                }),
            AssetRequest::NewMaterial => state
                .selected
                .filter(|entity| world.get::<MeshRenderer>(*entity).is_some())
                .ok_or_else(|| "Select a mesh object first".to_owned())
                .and_then(|entity| {
                    remember_scene_before_edit(world, &mut history)?;
                    let material = world
                        .resource_mut::<AssetServer>()
                        .materials
                        .insert(MaterialAsset::default());
                    world
                        .get_mut::<MeshRenderer>(entity)
                        .expect("checked above")
                        .material = material;
                    state.scene_dirty = true;
                    Ok("Created a new material for the selected object".into())
                }),
            AssetRequest::AssignMesh { entity, handle } => {
                (|| -> Result<String, String> {
                    let assigned = assign_mesh_handle(
                        world,
                        &mut history,
                        entity,
                        handle,
                    )?;
                    state.scene_dirty |= assigned;
                    Ok(if assigned {
                        "Assigned mesh to selected object"
                    } else {
                        "Mesh is already assigned"
                    }
                    .into())
                })()
            }
            AssetRequest::AssignMaterial { entity, handle } => {
                (|| -> Result<String, String> {
                    let assigned = assign_material_handle(
                        world,
                        &mut history,
                        entity,
                        handle,
                    )?;
                    state.scene_dirty |= assigned;
                    Ok(if assigned {
                        "Assigned material to selected object"
                    } else {
                        "Material is already assigned"
                    }
                    .into())
                })()
            }
            AssetRequest::LoadMaterialTexture(slot) => state
                .selected
                .filter(|entity| world.get::<MeshRenderer>(*entity).is_some())
                .ok_or_else(|| "Select a mesh object first".to_owned())
                .map(|entity| {
                    dialogs.pick_file(
                        DialogPurpose::MaterialTexture { entity, slot },
                        rfd::AsyncFileDialog::new()
                            .set_title("Choose Texture")
                            .add_filter(
                                "Image",
                                &["png", "jpg", "jpeg", "bmp", "tga"],
                            ),
                    );
                    "Choosing a texture".to_owned()
                }),
            AssetRequest::SetMaterialTexture { entity, slot, path } => (|| {
                let mut material = world
                    .get::<MeshRenderer>(entity)
                    .and_then(|renderer| {
                        world
                            .resource::<AssetServer>()
                            .materials
                            .get(renderer.material)
                            .cloned()
                    })
                    .ok_or_else(|| {
                        "The object no longer has a Mesh Renderer".to_owned()
                    })?;
                // Project textures are copied in so the saved scene points
                // inside the project and the exported game finds them.
                let path = if has_open_project {
                    copy_into_project_assets(&state.project_root, &path)?
                } else {
                    path
                };
                let texture = world
                    .resource_mut::<AssetServer>()
                    .load_texture(&path)
                    .map_err(|error| error.to_string())?;
                remember_scene_before_edit(world, &mut history)?;
                *crate::editor::inspector::texture_slots(&mut material)[slot] =
                    Some(texture);
                set_material(world, entity, material);
                state.scene_dirty = true;
                Ok(format!("Loaded texture {}", path.display()))
            })(
            ),
        };
        if let Some(mut console) = world.get_resource_mut::<EditorConsole>() {
            match &result {
                Ok(message) => {
                    console.push_from(ConsoleLevel::Info, "Assets", message)
                }
                Err(error) => {
                    console.push_from(ConsoleLevel::Error, "Assets", error)
                }
            }
        }
        editor_assets.message = Some(result.unwrap_or_else(|error| error));
    }
    if component_edits
        .iter()
        .any(|edit| !matches!(edit, ComponentEdit::Set { .. }))
    {
        match remember_scene_before_edit(world, &mut history) {
            Ok(()) => state.scene_dirty = true,
            Err(error) => state.scene_message = Some(error),
        }
    }
    for edit in component_edits {
        let result = match edit {
            ComponentEdit::Set {
                entity,
                name,
                value,
            } => set_registered_component(world, entity, &name, &value),
            ComponentEdit::Add { entity, name } => {
                add_registered_component(world, entity, &name)
            }
            ComponentEdit::Remove { entity, name } => {
                remove_registered_component(world, entity, &name)
            }
        };
        if let Err(error) = result {
            state.scene_message =
                Some(format!("Component edit failed: {error}"));
        }
    }
    if let Some((entity, command)) = state.particle_command.take() {
        if let Some(mut emitter) =
            world.get_mut::<crate::runtime::ParticleEmitter>(entity)
        {
            emitter.command = command;
        }
    }
    timeline::update_timeline(world, &mut state);
    if let Some(request) = project_request {
        let opened = match request {
            ProjectRequest::Create {
                parent,
                name,
                template,
            } => create_project_from(&parent, &name, template),
            ProjectRequest::Open(path) => open_project(&path),
        };
        match opened {
            Ok(project) => {
                match load_scene(
                    world,
                    &project.scene_path,
                    SceneLoadMode::Replace,
                ) {
                    Ok(entity_count) => {
                        super::SceneFileRevision::record(
                            world,
                            &project.scene_path,
                        );
                        state.selected = None;
                        set_open_project_paths(&mut state, &project);
                        match std::fs::read_to_string(&project.code_path) {
                            Ok(source) => {
                                state.code_source = source;
                                state.code_dirty = false;
                                state.code_message = Some(format!(
                                    "Opened {} from {}",
                                    state.code_path, project.manifest.name
                                ));
                            }
                            Err(error) => {
                                state.code_message = Some(format!(
                                    "Could not open Rust code: {error}"
                                ));
                            }
                        }
                        state.scene_message = Some(format!(
                            "Opened {} with {entity_count} scene objects",
                            project.manifest.name
                        ));
                        state.scene_dirty = false;
                        history.undo.clear();
                        history.redo.clear();
                        history.pending_inspector = None;
                        project_manager.open = false;
                        if let Err(error) = project::remember_project(
                            &mut project_manager.recent_projects,
                            &project,
                        ) {
                            project_manager.message = Some(format!(
                                "Project opened, but recent projects could not be saved: {error}"
                            ));
                        } else {
                            project_manager.message = None;
                        }
                    }
                    Err(error) => {
                        project_manager.message = Some(format!(
                            "Could not load project scene: {error}"
                        ));
                    }
                }
            }
            Err(error) => {
                project_manager.message =
                    Some(format!("Could not open project: {error}"));
            }
        }
    }
    world.resource_mut::<RenderCameraOverride>().entity = (state.workspace
        == EditorWorkspace::Scene)
        .then_some(state.editor_camera)
        .flatten();
    render_settings.max_fps = render_settings.max_fps.max(1);
    apply_render_settings(
        world,
        &mut history,
        &mut state,
        render_settings,
        original_scene_render,
    );
    if let Some(request) = build_request {
        if let Err(error) = world
            .resource_mut::<EditorBuildState>()
            .start(PathBuf::from(&state.project_root), request)
        {
            state.code_message =
                Some(format!("Could not start Cargo: {error}"));
        }
    }
    if state.mode == EditorMode::Edit {
        crate::runtime::run_edit_mode_systems(world);
    }
    crate::runtime::propagate_transforms(world);
    let mut overlay = if state.workspace == EditorWorkspace::Scene {
        build_scene_debug_overlay(
            world,
            state.editor_camera,
            state.selected,
            &state.selection,
            gizmo_settings,
            hovered_gizmo,
            &gizmo_drag,
            transform_mode.active_mode,
            transform_mode.global_axes,
        )
    } else {
        RenderDebugOverlay::default()
    };
    if let (Some(settings), Some(physics)) = (
        world.get_resource::<crate::runtime::PhysicsDebugDraw>(),
        world.get_resource::<crate::runtime::PhysicsWorld>(),
    ) {
        physics.debug_draw(world, *settings, &mut overlay);
    }
    if let Some((map, rects)) = tile_preview {
        if let (Some(tiles), Some(transform)) = (
            world.get::<crate::runtime::TileMap>(map),
            world.get::<crate::Transform>(map),
        ) {
            for (from, to) in rects {
                for [start, end] in crate::editor::overlay::tile_rect_lines(
                    tiles.tile_size,
                    transform.position,
                    from,
                    to,
                ) {
                    overlay.line_on_top(
                        start,
                        end,
                        [1.0, 0.78, 0.12, 1.0],
                        2.0,
                    );
                }
            }
        }
    }
    world.resource_mut::<EditorDebugOverlay>().0 = overlay;
    *world.resource_mut::<EditorGizmoSettings>() = gizmo_settings;
    *world.resource_mut::<EditorTransformMode>() = transform_mode;
    world.insert_resource(gizmo_drag);
    if let Some(mut console) = world.get_resource_mut::<EditorConsole>() {
        for (index, source, now) in [
            (0, "Scene", &state.scene_message),
            (1, "Code", &state.code_message),
        ] {
            if *now != console.status_seen[index] {
                console.status_seen[index].clone_from(now);
                if let Some(message) = now {
                    console.push_from(status_level(message), source, message);
                }
            }
        }
    }
    world.insert_resource(state);
    if focus_requested {
        shortcuts::camera_to_object(world);
    }
    if orthographic != was_orthographic {
        shortcuts::toggle_orthographic(world);
    }
    world.insert_resource(history);
    world.insert_resource(pending_action);
    world.insert_resource(editor_assets);
    world.insert_resource(project_manager);
    world.insert_resource(dialogs);
}

/// Console area: level toggles with counts, a text filter, and the
/// messages, newest at the bottom.
fn draw_console_area(ui: &mut egui::Ui, console: &mut EditorConsole) {
    use gui_elements::EditorTheme;
    let levels = [
        (ConsoleLevel::Info, "Info", EditorTheme::TEXT),
        (ConsoleLevel::Warning, "Warnings", EditorTheme::WARNING),
        (ConsoleLevel::Error, "Errors", EditorTheme::ERROR),
    ];
    let mut counts = [0_u32; 3];
    for entry in &console.entries {
        counts[entry.level as usize] += entry.count;
    }
    egui::Frame::new()
        .fill(EditorTheme::PANEL_RAISED)
        .stroke(egui::Stroke::new(1.0, EditorTheme::BORDER_SOFT))
        .corner_radius(EditorTheme::RADIUS)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for ((level, label, color), count) in levels.iter().zip(counts)
                {
                    let shown = &mut console.hidden[*level as usize];
                    let text = egui::RichText::new(format!("{label} {count}"))
                        .color(if *shown {
                            EditorTheme::TEXT_MUTED
                        } else {
                            *color
                        });
                    if ui.selectable_label(!*shown, text).clicked() {
                        *shown = !*shown;
                    }
                }
                if ui.button("Clear").clicked() {
                    console.entries.clear();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut console.filter)
                        .hint_text("Filter messages")
                        .desired_width(f32::INFINITY),
                );
            });
        });
    ui.add_space(4.0);
    let filter = console.filter.to_lowercase();
    let visible: Vec<&ConsoleEntry> = console
        .entries
        .iter()
        .filter(|entry| !console.hidden[entry.level as usize])
        .filter(|entry| {
            filter.is_empty()
                || entry.message.to_lowercase().contains(&filter)
                || entry.source.to_lowercase().contains(&filter)
        })
        .collect();
    egui::Frame::new()
        .fill(EditorTheme::BACKGROUND)
        .stroke(egui::Stroke::new(1.0, EditorTheme::BORDER_SOFT))
        .corner_radius(EditorTheme::RADIUS)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("console_panel_scroll")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if visible.is_empty() {
                        ui.add_space(8.0);
                        ui.vertical_centered(|ui| {
                            ui.colored_label(
                                EditorTheme::TEXT_MUTED,
                                if console.entries.is_empty() {
                                    "No console messages."
                                } else {
                                    "No messages match the filter."
                                },
                            );
                        });
                    }
                    for (index, entry) in visible.into_iter().enumerate() {
                        let color = levels[entry.level as usize].2;
                        let fill = if index % 2 == 0 {
                            EditorTheme::BACKGROUND
                        } else {
                            EditorTheme::INPUT
                        };
                        let row = egui::Frame::new()
                            .fill(fill)
                            .inner_margin(egui::Margin {
                                left: 10,
                                right: 6,
                                top: 3,
                                bottom: 3,
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(entry.source)
                                            .small()
                                            .color(EditorTheme::ACCENT_HOVER)
                                            .background_color(
                                                EditorTheme::PANEL,
                                            ),
                                    );
                                    ui.label(
                                        egui::RichText::new(&entry.message)
                                            .monospace()
                                            .color(
                                                if entry.level
                                                    == ConsoleLevel::Info
                                                {
                                                    EditorTheme::TEXT
                                                } else {
                                                    color
                                                },
                                            ),
                                    );
                                    if entry.count > 1 {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "x{}",
                                                entry.count
                                            ))
                                            .small()
                                            .color(EditorTheme::TEXT_STRONG)
                                            .background_color(
                                                EditorTheme::ACCENT,
                                            ),
                                        );
                                    }
                                });
                            });
                        // Level stripe on the row's left edge.
                        let rect = row.response.rect;
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                rect.min,
                                egui::vec2(3.0, rect.height()),
                            ),
                            0.0,
                            if entry.level == ConsoleLevel::Info {
                                EditorTheme::ACCENT
                            } else {
                                color
                            },
                        );
                    }
                });
        });
}

/// Console level of a status line the editor wrote as plain text.
// ponytail: keyword guess, because status lines are strings. Give the
// status fields a level if a message is misfiled.
fn status_level(message: &str) -> ConsoleLevel {
    let lower = message.to_lowercase();
    if ["fail", "could not", "cannot", "error", "invalid"]
        .iter()
        .any(|word| lower.contains(word))
    {
        ConsoleLevel::Error
    } else if ["missing", "skipped", "not found", "warning"]
        .iter()
        .any(|word| lower.contains(word))
    {
        ConsoleLevel::Warning
    } else {
        ConsoleLevel::Info
    }
}

fn take_resource<T: Resource>(world: &mut World) -> T {
    world
        .remove_resource::<T>()
        .unwrap_or_else(|| panic!("{} is missing", std::any::type_name::<T>()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GizmoHandle {
    mode: TransformModes,
    axis: GizmoAxis,
}

#[allow(clippy::too_many_arguments)]
fn update_transform_gizmo(
    world: &mut World,
    state: &mut EditorState,
    viewport: Option<egui::Rect>,
    hover: Option<Pos2>,
    drag_started: Option<Pos2>,
    drag_position: Option<Pos2>,
    drag_stopped: bool,
    drag: &mut EditorGizmoDrag,
    edited_transform: &mut Option<Transform>,
    history: &mut EditorHistory,
    right_clicked: bool,
    left_pressed: bool,
    escape_pressed: bool,
    transform_mode: &mut EditorTransformMode,
) -> (Option<GizmoHandle>, bool) {
    // A drag belongs to the entity it started on. When the selection moves
    // away, restore that entity instead of previewing onto the new selection.
    if drag.is_active() && drag.entity != state.selected {
        cancel_gizmo_drag(world, drag);
        finish_transform_mode(transform_mode);
    }
    let (Some(entity), Some(camera), Some(viewport)) =
        (state.selected, state.editor_camera, viewport)
    else {
        *drag = EditorGizmoDrag::default();
        transform_mode.start_requested = false;
        transform_mode.active_mode = TransformModes::Combo;
        return (None, false);
    };
    let global_axes = transform_mode.global_axes;
    let Some(geometry) = gizmo_geometry(world, entity, global_axes) else {
        if drag.is_active() {
            *edited_transform = drag.original_transform;
            *drag = EditorGizmoDrag::default();
            finish_transform_mode(transform_mode);
        }
        return (None, false);
    };
    // Pressing G/S/R during an unfinished operation changes operation type by
    // cancelling the preview and beginning again from the authored transform.
    if transform_mode.start_requested && drag.is_active() {
        *edited_transform = drag.original_transform;
        *drag = EditorGizmoDrag::default();
    }
    if transform_mode.start_requested {
        if let Some(pointer) = hover {
            begin_gizmo_drag(
                world,
                entity,
                pointer,
                transform_mode.active_mode,
                true,
                transform_mode.axis_mask,
                transform_mode.snap_enabled,
                global_axes,
                &geometry,
                camera,
                viewport,
                drag,
                *edited_transform,
            );
            transform_mode.start_requested = false;
        }
    }

    let hovered =
        if let (Some(mode), Some(axis)) = (drag.mode, drag.active_axis) {
            Some(GizmoHandle { mode, axis })
        } else {
            hover
                .and_then(|pointer| {
                    hit_test_gizmo(world, camera, viewport, pointer, &geometry)
                })
                .map(|hit| hit.1)
        };
    let mut consumed = drag.is_active();
    if !drag.is_active() {
        if let Some(pointer) = drag_started {
            if let Some((_, handle)) =
                hit_test_gizmo(world, camera, viewport, pointer, &geometry)
            {
                let mut mask = [false; 3];
                mask[gizmo_axis_index(handle.axis)] = true;
                begin_gizmo_drag(
                    world,
                    entity,
                    pointer,
                    handle.mode,
                    false,
                    mask,
                    transform_mode.snap_enabled,
                    global_axes,
                    &geometry,
                    camera,
                    viewport,
                    drag,
                    *edited_transform,
                );
                transform_mode.active_mode = handle.mode;
                transform_mode.axis_mask = mask;
                consumed = true;
            }
        }
    }

    if drag.is_active() {
        drag.axis_mask = transform_mode.axis_mask;
        drag.snap_enabled = transform_mode.snap_enabled;
        drag.snap_steps = transform_mode.snap_steps;
        drag.active_axis = single_enabled_axis(drag.axis_mask);
        let pointer = if drag.modal { hover } else { drag_position };
        if let Some(pointer) = pointer {
            let move_parameter = if drag.mode == Some(TransformModes::Move)
                && single_enabled_axis(drag.axis_mask).is_some()
            {
                world
                    .get::<Camera>(camera)
                    .copied()
                    .zip(world.get::<GlobalTransform>(camera).copied())
                    .and_then(|(camera_component, camera_transform)| {
                        scene_ray(
                            pointer,
                            viewport,
                            camera_component,
                            camera_transform,
                        )
                    })
                    .and_then(|ray| {
                        move_axis_parameter(
                            ray,
                            drag.move_axis_origin,
                            drag.move_axis_direction,
                            drag.move_drag_plane_normal,
                        )
                    })
            } else {
                None
            };
            if let Some(transform) =
                transformed_from_pointer(drag, pointer, move_parameter)
            {
                *edited_transform = Some(transform);
                state.scene_dirty = true;
            }
        }
    }

    if (right_clicked || escape_pressed) && drag.is_active() {
        *edited_transform = drag.original_transform;
        *drag = EditorGizmoDrag::default();
        finish_transform_mode(transform_mode);
        return (hovered, true);
    }

    let confirmed = drag.is_active()
        && if drag.modal {
            left_pressed && hover.is_some()
        } else {
            drag_stopped
        };
    if confirmed {
        if let Some(snapshot) = drag.undo_snapshot.take() {
            if drag.original_transform != *edited_transform {
                history.push_undo(snapshot);
            }
        }

        *drag = EditorGizmoDrag::default();
        finish_transform_mode(transform_mode);
        return (hovered, true);
    }
    (hovered, consumed)
}

/// Replaces the scene with `document` and maps the selection back through
/// `SceneId`, because the reload respawns every entity with a new `Entity`.
pub(super) fn start_embedded_preview(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
) {
    match scene_document(world, "Preview Snapshot") {
        Ok(snapshot) => {
            state.preview_scene_dirty = Some(state.scene_dirty);
            state.preview_entities =
                Some(world.iter_entities().map(|entity| entity.id()).collect());
            history.preview_before = Some(Box::new(EditorHistory {
                undo: history.undo.clone(),
                redo: history.redo.clone(),
                pending_inspector: history.pending_inspector.clone(),
                preview_before: None,
            }));
            state.play_snapshot = Some(snapshot);
            // Preview particles stay in the editor; the game starts fresh.
            crate::runtime::reset_particles(world);
            timeline::restore_rest_pose(world);
            state.timeline.playing = false;
            state.mode = EditorMode::Play;
            world.resource_mut::<crate::runtime::TimeControl>().resume();
            if state.workspace == EditorWorkspace::Scene {
                state
                    .dock_layout
                    .set_panel(state.active_area, EditorPanel::Game);
            }
            state.scene_message = Some("Embedded preview started".into());
        }
        Err(error) => {
            state.scene_message =
                Some(format!("Could not preview scene: {error}"));
        }
    }
}

/// Pauses or resumes fixed time while the embedded preview runs. Scene
/// edits made while paused are kept when the preview resumes.
pub(super) fn set_embedded_preview_paused(
    world: &mut World,
    state: &mut EditorState,
    paused: bool,
) {
    let mut time = world.resource_mut::<crate::runtime::TimeControl>();
    if paused {
        state.mode = EditorMode::Paused;
        time.pause();
    } else {
        state.mode = EditorMode::Play;
        time.resume();
    }
}

pub(super) fn stop_embedded_preview(
    world: &mut World,
    state: &mut EditorState,
    history: &mut EditorHistory,
) {
    world.resource_mut::<crate::runtime::TimeControl>().pause();
    let Some(snapshot) = state.play_snapshot.take() else {
        state.mode = EditorMode::Edit;
        return;
    };
    if let Some(before) = &state.preview_entities {
        // Resources and observers are entities too; systems that first ran
        // during the preview create them, and they must survive Stop.
        let spawned = world
            .iter_entities()
            .filter(|entity| {
                !entity.contains::<bevy_ecs::resource::IsResource>()
                    && !entity.contains::<bevy_ecs::observer::Observer>()
            })
            .map(|entity| entity.id())
            .filter(|entity| !before.contains(entity))
            .collect::<Vec<_>>();
        for entity in spawned {
            world.despawn(entity);
        }
    }
    match reload_keeping_selection(world, state, |world| {
        crate::runtime::load_scene_document(
            world,
            &snapshot,
            SceneLoadMode::Replace,
        )
    }) {
        Ok(()) => {
            state.scene_dirty = state
                .preview_scene_dirty
                .take()
                .unwrap_or(state.scene_dirty);
            state.preview_entities = None;
            if let Some(before) = history.preview_before.take() {
                *history = *before;
            }
            state.mode = EditorMode::Edit;
            if state.workspace == EditorWorkspace::Game {
                state
                    .dock_layout
                    .set_panel(state.active_area, EditorPanel::Scene);
            }
            state.scene_message = Some(
                "Embedded preview stopped; authored scene restored".into(),
            );
        }
        Err(error) => {
            state.mode = EditorMode::Paused;
            state.play_snapshot = Some(snapshot);
            state.scene_message =
                Some(format!("Could not restore scene: {error}"));
        }
    }
}

pub(super) fn reload_keeping_selection(
    world: &mut World,
    state: &mut EditorState,
    load: impl FnOnce(&mut World) -> Result<usize, crate::runtime::SceneIoError>,
) -> Result<(), crate::runtime::SceneIoError> {
    let scene_id =
        |world: &World, entity| world.get::<SceneId>(entity).map(|id| id.0);
    let selected = state.selected.and_then(|entity| scene_id(world, entity));
    let selection: Vec<_> = state
        .selection
        .iter()
        .filter_map(|&entity| scene_id(world, entity))
        .collect();
    load(world)?;
    let mut query = world.query::<(Entity, &SceneId)>();
    let entities: std::collections::HashMap<_, _> = query
        .iter(world)
        .map(|(entity, id)| (id.0, entity))
        .collect();
    state.selected = selected.and_then(|id| entities.get(&id).copied());
    state.selection = selection
        .iter()
        .filter_map(|id| entities.get(id).copied())
        .collect();
    Ok(())
}

/// Writes the authored transform back to the drag's own entity and ends the
/// drag without an Undo step.
fn cancel_gizmo_drag(world: &mut World, drag: &mut EditorGizmoDrag) {
    if let (Some(entity), Some(transform)) =
        (drag.entity, drag.original_transform)
    {
        if let Ok(mut entity) = world.get_entity_mut(entity) {
            entity.insert(transform);
        }
    }
    *drag = EditorGizmoDrag::default();
}

fn finish_transform_mode(transform_mode: &mut EditorTransformMode) {
    transform_mode.active_mode = TransformModes::Combo;
    transform_mode.axis_mask = [true; 3];
    transform_mode.start_requested = false;
}

fn draw_add_object_modal(
    context: &egui::Context,
    open: &mut bool,
    parent: Option<Entity>,
    world: &World,
    request: &mut Option<EntityRequest>,
) {
    if !*open {
        return;
    }

    let screen = context.screen_rect();
    let modal_size = egui::vec2(screen.width() * 0.6, screen.height() * 0.7);
    let parent_name = parent
        .and_then(|entity| world.get::<Name>(entity))
        .map(|name| name.0.as_str());
    let mut close_requested = false;
    let response = egui::Modal::new(egui::Id::new("add_object_modal"))
        .frame(
            egui::Frame::popup(&context.style())
                .fill(gui_elements::EditorTheme::PANEL)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    gui_elements::EditorTheme::BORDER,
                ))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::same(14)),
        )
        .show(context, |ui| {
            ui.set_min_size(modal_size);
            ui.set_max_size(modal_size);
            ui.horizontal(|ui| {
                ui.heading(parent_name.map_or_else(
                    || "Add Object".to_owned(),
                    |name| format!("Add Child to {name}"),
                ));
                ui.add_space((ui.available_width() - 28.0).max(0.0));
                if gui_elements::icon_button_sized(
                    ui,
                    EditorIcon::Close,
                    false,
                    true,
                    egui::vec2(26.0, 24.0),
                )
                .on_hover_text("Close")
                .clicked()
                {
                    close_requested = true;
                }
            });
            ui.label(
                egui::RichText::new(
                    "Choose a scene object. Categories stay compact until you open them.",
                )
                .color(gui_elements::EditorTheme::TEXT_MUTED),
            );
            ui.separator();

            let content_height = (ui.available_height() - 8.0).max(80.0);
            egui::ScrollArea::vertical()
                .max_height(content_height)
                .show(ui, |ui| {
                    egui::CollapsingHeader::new("Meshes")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for shape in PrimitiveShape::ALL {
                                    if ui
                                        .add_sized(
                                            egui::vec2(142.0, 38.0),
                                            egui::Button::new(shape.label()),
                                        )
                                        .clicked()
                                    {
                                        *request = Some(
                                            EntityRequest::CreatePrimitive(
                                                shape, parent,
                                            ),
                                        );
                                        close_requested = true;
                                    }
                                }
                            });
                        });
                    egui::CollapsingHeader::new("Scene Objects")
                        .default_open(false)
                        .show(ui, |ui| {
                            if ui
                                .add_sized(
                                    egui::vec2(142.0, 38.0),
                                    egui::Button::new("Empty Object"),
                                )
                                .clicked()
                            {
                                *request =
                                    Some(EntityRequest::CreateEmpty(parent));
                                close_requested = true;
                            }
                        });
                    egui::CollapsingHeader::new("Environment and UI")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let has_environment = super::inspector::placement::scene_has_environment(world);
                                let presets: [(&'static str, &'static [&'static str], bool, &str); 4] = [
                                    (
                                        "World Environment",
                                        &super::inspector::placement::ENVIRONMENT_COMPONENTS,
                                        !has_environment,
                                        "The scene already has a Sky Light",
                                    ),
                                    (
                                        "HUD Element",
                                        &[crate::runtime::HUD_ELEMENT_COMPONENT],
                                        true,
                                        "",
                                    ),
                                    (
                                        "Reflection Probe",
                                        &[crate::runtime::REFLECTION_PROBE_COMPONENT],
                                        true,
                                        "",
                                    ),
                                    (
                                        "Water",
                                        &[crate::runtime::WATER_COMPONENT],
                                        true,
                                        "",
                                    ),
                                ];
                                for (name, components, enabled, reason) in presets {
                                    let response = ui.add_enabled(
                                        enabled,
                                        egui::Button::new(name)
                                            .min_size(egui::vec2(142.0, 38.0)),
                                    );
                                    if response
                                        .on_disabled_hover_text(reason)
                                        .clicked()
                                    {
                                        *request = Some(EntityRequest::CreateWith {
                                            name,
                                            components,
                                            parent,
                                        });
                                        close_requested = true;
                                    }
                                }
                            });
                        });
                    egui::CollapsingHeader::new("Cameras")
                        .default_open(false)
                        .show(ui, |ui| {
                            if ui
                                .add_sized(
                                    egui::vec2(142.0, 38.0),
                                    egui::Button::new("Perspective Camera"),
                                )
                                .clicked()
                            {
                                *request =
                                    Some(EntityRequest::CreateCamera(parent));
                                close_requested = true;
                            }
                        });
                    egui::CollapsingHeader::new("Lights")
                        .default_open(false)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                for light in EditorLightType::ALL {
                                    if ui
                                        .add_sized(
                                            egui::vec2(142.0, 38.0),
                                            egui::Button::new(light.label()),
                                        )
                                        .clicked()
                                    {
                                        *request = Some(
                                            EntityRequest::CreateLight(
                                                light, parent,
                                            ),
                                        );
                                        close_requested = true;
                                    }
                                }
                            });
                        });
                });
        });

    if close_requested || response.should_close() {
        *open = false;
    }
}

fn draw_viewport_transform_toolbar(
    ui: &mut egui::Ui,
    viewport: egui::Rect,
    transform: &mut EditorTransformMode,
    shortcuts: &EditorShortcuts,
    drag_active: bool,
    has_selection: bool,
) -> (bool, bool) {
    let transform_active = drag_active || transform.start_requested;
    let toolbar_size =
        egui::vec2(if transform_active { 410.0 } else { 301.0 }, 36.0);
    let toolbar_rect = egui::Rect::from_min_size(
        viewport.min + egui::vec2(8.0, 8.0),
        toolbar_size,
    )
    .intersect(viewport);

    let mut focus_clicked = false;
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt("viewport_transform_toolbar")
            .max_rect(toolbar_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.set_clip_rect(ui.clip_rect().intersect(viewport));
            egui::Frame::NONE
                .fill(gui_elements::EditorTheme::PANEL)
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    gui_elements::EditorTheme::BORDER,
                ))
                .corner_radius(6.0)
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (mode, icon) in [
                        (TransformModes::Move, EditorIcon::Move),
                        (TransformModes::Rotate, EditorIcon::Rotate),
                        (TransformModes::Scale, EditorIcon::Scale),
                    ] {
                        let shortcut =
                            transform_shortcut_label(shortcuts, mode);
                        if gui_elements::icon_button(
                            ui,
                            icon,
                            transform.active_mode == mode,
                            has_selection,
                        )
                        .on_hover_text(format!(
                            "{} ({shortcut})",
                            mode_name(mode)
                        ))
                        .clicked()
                        {
                            transform.active_mode = mode;
                            transform.axis_mask = [true; 3];
                            transform.start_requested = true;
                        }
                    }

                    ui.separator();
                    if ui
                        .selectable_label(transform.snap_enabled, "Snap")
                        .on_hover_text("Snap move, rotation and scale to the increments set in the menu beside it")
                        .clicked()
                    {
                        transform.snap_enabled = !transform.snap_enabled;
                    }
                    ui.menu_button("⏷", |ui| {
                        let steps = &mut transform.snap_steps;
                        for (label, value, speed, suffix) in [
                            ("Move", &mut steps.translate, 0.05, " m"),
                            ("Rotate", &mut steps.rotate_degrees, 1.0, "°"),
                            ("Scale", &mut steps.scale, 0.01, ""),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(label);
                                ui.add(
                                    egui::DragValue::new(value)
                                        .speed(speed)
                                        .range(0.001..=360.0)
                                        .suffix(suffix),
                                );
                            });
                        }
                    })
                    .response
                    .on_hover_text("Snap increments");
                    if ui
                        .add_enabled(
                            !drag_active,
                            egui::Button::new(if transform.global_axes {
                                "Global"
                            } else {
                                "Local"
                            }),
                        )
                        .on_hover_text("Use world axes for Move, Rotate, and Scale")
                        .clicked()
                    {
                        transform.global_axes = !transform.global_axes;
                    }
                    if ui
                        .add_enabled(has_selection && !drag_active, egui::Button::new("Frame"))
                        .on_hover_text("Frame the selected object (F)")
                        .clicked()
                    {
                        focus_clicked = true;
                    }

                    if transform_active {
                        ui.separator();
                        for (axis, label) in [
                            (GizmoAxis::X, "X"),
                            (GizmoAxis::Y, "Y"),
                            (GizmoAxis::Z, "Z"),
                        ] {
                            let active =
                                transform.axis_mask[gizmo_axis_index(axis)];
                            let text = egui::RichText::new(label)
                                .strong()
                                .color(gui_elements::EditorTheme::TEXT_STRONG);
                            let mut button = egui::Button::new(text)
                                .min_size(egui::vec2(24.0, 24.0));
                            button = if active {
                                button
                                    .fill(
                                        gui_elements::EditorTheme::ACCENT_HOVER,
                                    )
                                    .stroke(egui::Stroke::new(
                                        1.0_f32,
                                        gui_elements::EditorTheme::LINK,
                                    ))
                            } else {
                                button.frame(false)
                            };
                            if ui
                                .add(button)
                                .on_hover_text(format!(
                                    "Toggle {label} direction ({label})"
                                ))
                                .clicked()
                            {
                                transform.select_only_or_toggle(axis);
                            }
                        }
                    }
                });
        },
    );

    let hovered = ui.input(|input| {
        input
            .pointer
            .latest_pos()
            .is_some_and(|position| toolbar_rect.contains(position))
    });
    (hovered, focus_clicked)
}

/// Writes the panel's render settings back. Quality and culling are scene
/// data, so an edit takes an undo snapshot and marks the scene dirty.
/// Without an edit the world keeps its own values, which an Undo this frame
/// may have just restored.
/// Writes an edited material to `entity`'s renderer. A material other
/// renderers share (or the built-in fallback) is cloned first so the edit
/// stays on this object.
// ponytail: the share check scans every renderer each edit frame; keep a
// material use count if scenes grow large enough for this to show.
pub(super) fn assign_mesh_handle(
    world: &mut World,
    history: &mut EditorHistory,
    entity: Entity,
    handle: crate::assets::Handle<crate::assets::MeshAsset>,
) -> Result<bool, String> {
    let assets = world.resource::<AssetServer>();
    if !assets.meshes.contains(handle) {
        return Err("The selected mesh is no longer loaded".into());
    }
    if assets.primitive_for_handle(handle).is_none()
        && assets.meshes.path(handle).is_none()
    {
        return Err("The selected mesh cannot be saved in a scene".into());
    }
    let current = world
        .get::<MeshRenderer>(entity)
        .ok_or_else(|| "The object no longer has a Mesh Renderer".to_owned())?
        .mesh;
    if current == handle {
        return Ok(false);
    }
    remember_scene_before_edit(world, history)?;
    world
        .get_mut::<MeshRenderer>(entity)
        .expect("checked above")
        .mesh = handle;
    Ok(true)
}

pub(super) fn assign_material_handle(
    world: &mut World,
    history: &mut EditorHistory,
    entity: Entity,
    handle: crate::assets::Handle<MaterialAsset>,
) -> Result<bool, String> {
    if !world.resource::<AssetServer>().materials.contains(handle) {
        return Err("The selected material is no longer loaded".into());
    }
    let current = world
        .get::<MeshRenderer>(entity)
        .ok_or_else(|| "The object no longer has a Mesh Renderer".to_owned())?
        .material;
    if current == handle {
        return Ok(false);
    }
    remember_scene_before_edit(world, history)?;
    world
        .get_mut::<MeshRenderer>(entity)
        .expect("checked above")
        .material = handle;
    Ok(true)
}

pub(super) fn set_material(
    world: &mut World,
    entity: Entity,
    material: MaterialAsset,
) {
    let Some(handle) = world.get::<MeshRenderer>(entity).map(|r| r.material)
    else {
        return;
    };
    let shared = handle == world.resource::<AssetServer>().fallback_material
        || world.query::<(Entity, &MeshRenderer)>().iter(world).any(
            |(other, renderer)| other != entity && renderer.material == handle,
        );
    let mut assets = world.resource_mut::<AssetServer>();
    match assets.materials.get_mut(handle) {
        Some(slot) if !shared => *slot = material,
        _ => {
            let handle = assets.materials.insert(material);
            world
                .get_mut::<MeshRenderer>(entity)
                .expect("checked above")
                .material = handle;
        }
    }
}

pub(super) fn apply_render_settings(
    world: &mut World,
    history: &mut EditorHistory,
    state: &mut EditorState,
    mut settings: RenderSettings,
    original: (QualityProfile, CullingMode),
) {
    if (settings.quality, settings.culling) != original {
        if let Err(error) = remember_scene_before_edit(world, history) {
            state.scene_message = Some(error);
        }
        state.scene_dirty = true;
    } else {
        let scene = world.resource::<RenderSettings>();
        settings.quality = scene.quality;
        settings.culling = scene.culling;
    }
    *world.resource_mut::<RenderSettings>() = settings;
}

/// Scene-saved renderer options. The exported game loads them with the
/// scene.
pub(super) fn draw_scene_render_settings(
    ui: &mut egui::Ui,
    settings: &mut RenderSettings,
) {
    use super::inspector::widgets::{choice, section};
    section(ui, "Scene Rendering", false, |ui| {
        choice(
            ui,
            "Quality",
            &mut settings.quality,
            &[
                (QualityProfile::Auto, "Auto (by GPU)"),
                (QualityProfile::Eco, "Eco"),
                (QualityProfile::Balanced, "Balanced"),
                (QualityProfile::High, "High"),
            ],
        );
        choice(
            ui,
            "Culling",
            &mut settings.culling,
            &[
                (CullingMode::Auto, "Auto"),
                (CullingMode::Disabled, "Off"),
                (CullingMode::Frustum, "Frustum"),
                (CullingMode::FrustumAndOcclusion, "Frustum + Occlusion"),
            ],
        );
    });
}

fn draw_viewport_render_settings(
    ui: &mut egui::Ui,
    viewport: egui::Rect,
    settings: &mut EditorGizmoSettings,
    orthographic: &mut bool,
) -> bool {
    let toolbar_rect = egui::Rect::from_min_size(
        egui::pos2(viewport.right() - 44.0, viewport.top() + 8.0),
        egui::vec2(36.0, 36.0),
    )
    .intersect(viewport);
    let popup_id = ui.make_persistent_id("viewport_render_settings_popup");
    let open = ui.memory(|memory| memory.is_popup_open(popup_id));

    let (button_rect, popup_rect) = ui
        .scope_builder(
            egui::UiBuilder::new()
                .id_salt("viewport_render_settings")
                .max_rect(toolbar_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
            |ui| {
                ui.set_clip_rect(ui.clip_rect().intersect(viewport));
                let button_response = egui::Frame::NONE
                    .fill(gui_elements::EditorTheme::PANEL)
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        gui_elements::EditorTheme::BORDER,
                    ))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(4))
                    .show(ui, |ui| {
                        let response = gui_elements::icon_button(
                            ui,
                            EditorIcon::ViewOptions,
                            open,
                            true,
                        )
                        .on_hover_text("Viewport render settings");
                        if response.clicked() {
                            ui.memory_mut(|memory| {
                                memory.toggle_popup(popup_id)
                            });
                        }
                        response
                    })
                    .inner;

                let popup_rect = egui::popup::popup_below_widget(
                    ui,
                    popup_id,
                    &button_response,
                    egui::popup::PopupCloseBehavior::CloseOnClickOutside,
                    |ui| {
                        ui.set_min_width(190.0);
                        gui_elements::EditorTheme::menu_section(ui, "VIEW");
                        ui.checkbox(orthographic, "Orthographic")
                            .on_hover_text(
                                "No perspective: parallel lines stay \
                                 parallel (Numpad 5)",
                            );
                        gui_elements::EditorTheme::menu_section(
                            ui,
                            "VIEWPORT OVERLAYS",
                        );
                        ui.checkbox(&mut settings.show_grid, "Grid")
                            .on_hover_text(
                                "Show the editor-only XZ ground grid",
                            );
                        ui.checkbox(&mut settings.show_selected_axes, "Axes")
                            .on_hover_text(
                                "Show X/Y/Z arrows on the selected object",
                            );
                        ui.checkbox(
                            &mut settings.show_selected_bounds,
                            "Bounds",
                        )
                        .on_hover_text(
                            "Show a yellow box around the selected mesh",
                        );
                        gui_elements::EditorTheme::menu_section(
                            ui,
                            "VIEWPORT SHADING",
                        );
                        for view in SceneDebugView::ALL {
                            ui.radio_value(
                                &mut settings.shading,
                                view,
                                view.label(),
                            );
                        }
                        gui_elements::EditorTheme::menu_section(
                            ui,
                            "SCENE EFFECTS",
                        );
                        let effects = &mut settings.effects;
                        ui.checkbox(&mut effects.fog, "Fog").on_hover_text(
                            "Draw the scene's fog here; Play always does",
                        );
                        ui.checkbox(&mut effects.bloom, "Bloom").on_hover_text(
                            "Draw the scene's bloom here; Play always does",
                        );
                        ui.checkbox(
                            &mut effects.ambient_occlusion,
                            "Ambient Occlusion",
                        )
                        .on_hover_text(
                            "Draw the scene's ambient occlusion here; Play \
                             always does",
                        );
                        ui.min_rect()
                    },
                );
                (button_response.rect, popup_rect)
            },
        )
        .inner;

    ui.input(|input| {
        input.pointer.latest_pos().is_some_and(|position| {
            button_rect.contains(position)
                || popup_rect.is_some_and(|rect| rect.contains(position))
        })
    })
}

const fn mode_name(mode: TransformModes) -> &'static str {
    match mode {
        TransformModes::Move => "Move",
        TransformModes::Rotate => "Rotate",
        TransformModes::Scale => "Scale",
        TransformModes::Combo => "Combined Transform",
    }
}

fn transform_shortcut_label(
    shortcuts: &EditorShortcuts,
    mode: TransformModes,
) -> String {
    shortcuts
        .get(ShortcutAction::SceneView(SceneViewAction::TransformModes(
            mode,
        )))
        .map_or_else(
            || "Unbound".into(),
            |binding| {
                let debug = format!("{:?}", binding.key);
                debug.strip_prefix("Key").unwrap_or(&debug).to_owned()
            },
        )
}

#[allow(clippy::too_many_arguments)]
fn begin_gizmo_drag(
    world: &mut World,
    entity: Entity,
    pointer: Pos2,
    mode: TransformModes,
    modal: bool,
    axis_mask: [bool; 3],
    snap_enabled: bool,
    global_axes: bool,
    geometry: &GizmoGeometry,
    camera: Entity,
    viewport: egui::Rect,
    drag: &mut EditorGizmoDrag,
    original_transform: Option<Transform>,
) {
    let Some(origin_screen) =
        project_world_to_screen(world, camera, geometry.origin, viewport)
    else {
        return;
    };
    let world_axes = geometry.axes.map(|(_, direction)| direction);
    let global_scale_weights = world.get::<GlobalTransform>(entity).map_or(
        [[0.0; 3]; 3],
        |transform| {
            let local_axes: [[f32; 3]; 3] = std::array::from_fn(|index| {
                let column = transform.matrix[index];
                normalized_axis([column[0], column[1], column[2]])
            });
            std::array::from_fn(|world_axis| {
                std::array::from_fn(|local_axis| {
                    local_axes[local_axis][world_axis].powi(2)
                })
            })
        },
    );
    let parent_inverse = world
        .get::<Parent>(entity)
        .and_then(|parent| world.get::<GlobalTransform>(parent.0))
        .and_then(|parent| Matrix4::from(parent.matrix).try_inverse());
    let local_delta_axes = world_axes.map(|axis| {
        parent_inverse.map_or(axis, |inverse| {
            let local = inverse * Vector4::new(axis[0], axis[1], axis[2], 0.0);
            [local.x, local.y, local.z]
        })
    });
    let screen_vectors = world_axes.map(|axis| {
        let point = [
            geometry.origin[0] + axis[0],
            geometry.origin[1] + axis[1],
            geometry.origin[2] + axis[2],
        ];
        project_world_to_screen(world, camera, point, viewport)
            .map_or([0.0; 2], |screen| {
                [screen.x - origin_screen.x, screen.y - origin_screen.y]
            })
    });
    let rotation_screen_signs = std::array::from_fn(|normal_axis| {
        let first = screen_vectors[(normal_axis + 1) % 3];
        let second = screen_vectors[(normal_axis + 2) % 3];
        let determinant = first[0] * second[1] - first[1] * second[0];
        if determinant.abs() > 0.001 {
            determinant.signum()
        } else {
            1.0
        }
    });
    let camera_forward = world.get::<GlobalTransform>(camera).map_or(
        [0.0, 0.0, -1.0],
        |camera_transform| {
            let matrix = camera_transform.matrix;
            normalized_axis([-matrix[2][0], -matrix[2][1], -matrix[2][2]])
        },
    );
    let view_rotation_axis = parent_inverse.map_or(camera_forward, |inverse| {
        let local = inverse
            * Vector4::new(
                camera_forward[0],
                camera_forward[1],
                camera_forward[2],
                0.0,
            );
        normalized_axis([local.x, local.y, local.z])
    });
    let move_axis = single_enabled_axis(axis_mask)
        .map(gizmo_axis_index)
        .unwrap_or(0);
    let move_axis_direction = world_axes[move_axis];
    let camera_position =
        world
            .get::<GlobalTransform>(camera)
            .map_or([0.0; 3], |transform| {
                [
                    transform.matrix[3][0],
                    transform.matrix[3][1],
                    transform.matrix[3][2],
                ]
            });
    let to_camera =
        Vector3::from(camera_position) - Vector3::from(geometry.origin);
    let axis_vector = Vector3::from(move_axis_direction);
    let plane_normal = to_camera - axis_vector * to_camera.dot(&axis_vector);
    let move_drag_plane_normal = plane_normal
        .try_normalize(0.0001)
        .map_or(camera_forward, Into::into);
    let move_start_axis_parameter = if mode == TransformModes::Move
        && single_enabled_axis(axis_mask).is_some()
    {
        let camera_component = world.get::<Camera>(camera).copied();
        let camera_transform = world.get::<GlobalTransform>(camera).copied();
        camera_component
            .zip(camera_transform)
            .and_then(|(camera_component, camera_transform)| {
                scene_ray(pointer, viewport, camera_component, camera_transform)
            })
            .and_then(|ray| {
                move_axis_parameter(
                    ray,
                    geometry.origin,
                    move_axis_direction,
                    move_drag_plane_normal,
                )
            })
    } else {
        None
    };
    let active_axis = single_enabled_axis(axis_mask);
    *drag = EditorGizmoDrag {
        mode: Some(mode),
        modal,
        axis_mask,
        snap_enabled,
        global_axes,
        global_scale_weights,
        active_axis,
        entity: Some(entity),
        start_pointer: Some(pointer),
        original_transform,
        origin_screen: Some(origin_screen),
        world_axes,
        local_delta_axes,
        screen_vectors,
        rotation_screen_signs,
        view_rotation_axis,
        move_axis_origin: geometry.origin,
        move_axis_direction,
        move_drag_plane_normal,
        move_start_axis_parameter,
        gizmo_axis_length: geometry.axis_length,
        undo_snapshot: scene_document(world, "Gizmo Undo Snapshot").ok(),
        ..EditorGizmoDrag::default()
    };
}

fn transformed_from_pointer(
    drag: &EditorGizmoDrag,
    pointer: Pos2,
    move_axis_parameter: Option<f32>,
) -> Option<Transform> {
    let start = drag.start_pointer?;
    let original = drag.original_transform?;
    let delta = pointer - start;
    let mut transform = original;
    match drag.mode? {
        TransformModes::Move => {
            if let (
                Some(start_parameter),
                Some(current_parameter),
                Some(axis),
            ) = (
                drag.move_start_axis_parameter,
                move_axis_parameter,
                single_enabled_axis(drag.axis_mask),
            ) {
                let distance = current_parameter - start_parameter;
                let distance = snap_value(
                    distance,
                    drag.snap_steps.translate,
                    drag.snap_enabled,
                );
                let axis = gizmo_axis_index(axis);
                for component in 0..3 {
                    transform.position[component] +=
                        drag.local_delta_axes[axis][component] * distance;
                }
                return Some(transform);
            }
            let coefficients = screen_axis_coefficients(
                drag.screen_vectors,
                drag.axis_mask,
                [delta.x, delta.y],
            );
            for (axis, coefficient) in coefficients.into_iter().enumerate() {
                let coefficient = snap_value(
                    coefficient,
                    drag.snap_steps.translate,
                    drag.snap_enabled,
                );
                for component in 0..3 {
                    transform.position[component] +=
                        drag.local_delta_axes[axis][component] * coefficient;
                }
            }
        }
        TransformModes::Scale => {
            let factor = if drag.modal {
                ((delta.x - delta.y) * 0.01).exp()
            } else {
                let axis = drag.active_axis?;
                let vector = drag.screen_vectors[gizmo_axis_index(axis)];
                let handle = [
                    vector[0] * drag.gizmo_axis_length * 0.68,
                    vector[1] * drag.gizmo_axis_length * 0.68,
                ];
                let length_squared =
                    (handle[0] * handle[0] + handle[1] * handle[1]).max(16.0);
                (1.0 + (delta.x * handle[0] + delta.y * handle[1])
                    / length_squared)
                    .max(0.01)
            };
            let factor = 1.0
                + snap_value(
                    factor - 1.0,
                    drag.snap_steps.scale,
                    drag.snap_enabled,
                );
            for local_axis in 0..3 {
                let weight = if drag.global_axes {
                    (0..3)
                        .filter(|&world_axis| drag.axis_mask[world_axis])
                        .map(|world_axis| {
                            drag.global_scale_weights[world_axis][local_axis]
                        })
                        .sum::<f32>()
                        .clamp(0.0, 1.0)
                } else if drag.axis_mask[local_axis] {
                    1.0
                } else {
                    0.0
                };
                transform.scale[local_axis] = (original.scale[local_axis]
                    * (1.0 + (factor - 1.0) * weight))
                    .max(0.001);
            }
        }
        TransformModes::Rotate => {
            if drag.modal && drag.axis_mask == [true; 3] {
                return Some(rotate_transform_in_parent_space(
                    original,
                    drag.view_rotation_axis,
                    snap_angle(
                        drag,
                        raw_screen_rotation_angle(drag, pointer),
                        drag.snap_enabled,
                    ),
                ));
            }
            let mut angles = [0.0; 3];
            if let Some(axis) = single_enabled_axis(drag.axis_mask) {
                let index = gizmo_axis_index(axis);
                angles[index] = snap_angle(
                    drag,
                    screen_rotation_angle(drag, pointer, index),
                    drag.snap_enabled,
                );
            } else if drag.modal {
                // Free rotation behaves like a small virtual trackball. Mouse
                // vertical rotates around local X, horizontal around local Y,
                // and diagonal motion contributes local Z when it is enabled.
                angles = [
                    -delta.y * 0.01,
                    delta.x * 0.01,
                    (delta.x - delta.y) * 0.005,
                ];
            } else {
                let index = gizmo_axis_index(drag.active_axis?);
                angles[index] = snap_angle(
                    drag,
                    screen_rotation_angle(drag, pointer, index),
                    drag.snap_enabled,
                );
            }
            for angle in &mut angles {
                *angle = snap_angle(drag, *angle, drag.snap_enabled);
            }
            transform = if drag.global_axes {
                angles.into_iter().enumerate().fold(
                    original,
                    |current, (axis, angle)| {
                        if drag.axis_mask[axis] && angle != 0.0 {
                            rotate_transform_in_parent_space(
                                current,
                                drag.local_delta_axes[axis],
                                angle,
                            )
                        } else {
                            current
                        }
                    },
                )
            } else {
                rotate_transform_locally(original, angles, drag.axis_mask)
            };
        }
        TransformModes::Combo => return None,
    }
    Some(transform)
}

fn snap_value(value: f32, step: f32, enabled: bool) -> f32 {
    if enabled && step > 0.0 {
        (value / step).round() * step
    } else {
        value
    }
}

fn snap_angle(drag: &EditorGizmoDrag, angle: f32, enabled: bool) -> f32 {
    snap_value(angle, drag.snap_steps.rotate_degrees.to_radians(), enabled)
}

fn move_axis_parameter(
    ray: super::picking::Ray,
    axis_origin: [f32; 3],
    axis_direction: [f32; 3],
    plane_normal: [f32; 3],
) -> Option<f32> {
    let origin = Vector3::from(axis_origin);
    let axis = Vector3::from(axis_direction);
    let normal = Vector3::from(plane_normal);
    let denominator = ray.direction.dot(&normal);
    if denominator.abs() <= 0.0001 {
        return None;
    }
    let ray_distance = (origin - ray.origin).dot(&normal) / denominator;
    let point = ray.origin + ray.direction * ray_distance;
    Some((point - origin).dot(&axis))
}

fn screen_rotation_angle(
    drag: &EditorGizmoDrag,
    pointer: Pos2,
    axis: usize,
) -> f32 {
    raw_screen_rotation_angle(drag, pointer) * drag.rotation_screen_signs[axis]
}

fn raw_screen_rotation_angle(drag: &EditorGizmoDrag, pointer: Pos2) -> f32 {
    let Some(origin) = drag.origin_screen else {
        return 0.0;
    };
    let Some(start) = drag.start_pointer else {
        return 0.0;
    };
    let from = start - origin;
    let to = pointer - origin;
    if from.length_sq() <= 4.0 || to.length_sq() <= 4.0 {
        let delta = pointer - start;
        return delta.x * 0.01;
    }
    (from.x * to.y - from.y * to.x).atan2(from.dot(to))
}

fn rotate_transform_locally(
    original: Transform,
    angles: [f32; 3],
    mask: [bool; 3],
) -> Transform {
    let current = Rotation3::from_euler_angles(
        original.rotation[0],
        original.rotation[1],
        original.rotation[2],
    );
    let mut local_delta = Rotation3::identity();
    if mask[0] {
        local_delta *=
            Rotation3::from_axis_angle(&Vector3::x_axis(), angles[0]);
    }
    if mask[1] {
        local_delta *=
            Rotation3::from_axis_angle(&Vector3::y_axis(), angles[1]);
    }
    if mask[2] {
        local_delta *=
            Rotation3::from_axis_angle(&Vector3::z_axis(), angles[2]);
    }
    let rotation = (current * local_delta).euler_angles();
    Transform {
        rotation: [rotation.0, rotation.1, rotation.2],
        ..original
    }
}

fn rotate_transform_in_parent_space(
    original: Transform,
    axis: [f32; 3],
    angle: f32,
) -> Transform {
    let current = Rotation3::from_euler_angles(
        original.rotation[0],
        original.rotation[1],
        original.rotation[2],
    );
    let axis = nalgebra::Unit::new_normalize(Vector3::from(axis));
    let rotation =
        (Rotation3::from_axis_angle(&axis, angle) * current).euler_angles();
    Transform {
        rotation: [rotation.0, rotation.1, rotation.2],
        ..original
    }
}

fn screen_axis_coefficients(
    vectors: [[f32; 2]; 3],
    mask: [bool; 3],
    delta: [f32; 2],
) -> [f32; 3] {
    // Minimum-norm solution of A*c=delta. It works for one, two, or all three
    // projected axes and naturally gives free movement in the camera plane.
    let mut xx = 0.0;
    let mut xy = 0.0;
    let mut yy = 0.0;
    for axis in 0..3 {
        if mask[axis] {
            xx += vectors[axis][0] * vectors[axis][0];
            xy += vectors[axis][0] * vectors[axis][1];
            yy += vectors[axis][1] * vectors[axis][1];
        }
    }
    let determinant = xx * yy - xy * xy;
    let mut result = [0.0; 3];
    if determinant.abs() > 0.001 {
        let solved = [
            (yy * delta[0] - xy * delta[1]) / determinant,
            (-xy * delta[0] + xx * delta[1]) / determinant,
        ];
        for axis in 0..3 {
            if mask[axis] {
                result[axis] =
                    vectors[axis][0] * solved[0] + vectors[axis][1] * solved[1];
            }
        }
    } else {
        for axis in 0..3 {
            if mask[axis] {
                let length_squared = vectors[axis][0] * vectors[axis][0]
                    + vectors[axis][1] * vectors[axis][1];
                if length_squared > 0.001 {
                    result[axis] = (vectors[axis][0] * delta[0]
                        + vectors[axis][1] * delta[1])
                        / length_squared;
                }
            }
        }
    }
    result
}

fn single_enabled_axis(mask: [bool; 3]) -> Option<GizmoAxis> {
    if mask.iter().filter(|enabled| **enabled).count() != 1 {
        return None;
    }
    mask.into_iter()
        .position(|enabled| enabled)
        .map(gizmo_axis_from_index)
}

const fn gizmo_axis_index(axis: GizmoAxis) -> usize {
    match axis {
        GizmoAxis::X => 0,
        GizmoAxis::Y => 1,
        GizmoAxis::Z => 2,
    }
}

const fn gizmo_axis_from_index(index: usize) -> GizmoAxis {
    match index {
        0 => GizmoAxis::X,
        1 => GizmoAxis::Y,
        _ => GizmoAxis::Z,
    }
}

struct GizmoGeometry {
    origin: [f32; 3],
    axes: [(GizmoAxis, [f32; 3]); 3],
    axis_length: f32,
}

fn gizmo_geometry(
    world: &World,
    entity: Entity,
    global_axes: bool,
) -> Option<GizmoGeometry> {
    let matrix = world.get::<GlobalTransform>(entity)?.matrix;
    let origin = [matrix[3][0], matrix[3][1], matrix[3][2]];
    let mut axes = [
        (
            GizmoAxis::X,
            normalized_axis([matrix[0][0], matrix[0][1], matrix[0][2]]),
        ),
        (
            GizmoAxis::Y,
            normalized_axis([matrix[1][0], matrix[1][1], matrix[1][2]]),
        ),
        (
            GizmoAxis::Z,
            normalized_axis([matrix[2][0], matrix[2][1], matrix[2][2]]),
        ),
    ];
    if global_axes {
        axes = [
            (GizmoAxis::X, [1.0, 0.0, 0.0]),
            (GizmoAxis::Y, [0.0, 1.0, 0.0]),
            (GizmoAxis::Z, [0.0, 0.0, 1.0]),
        ];
    }
    let axis_length = world
        .get::<MeshRenderer>(entity)
        .and_then(|renderer| {
            world.resource::<AssetServer>().mesh_bounds(renderer.mesh)
        })
        .map(|bounds| {
            crate::editor::overlay::mesh_world_radius_from_origin(
                bounds, matrix,
            )
        })
        .map_or(1.0, |radius| (radius * 1.2).max(1.0));
    Some(GizmoGeometry {
        origin,
        axes,
        axis_length,
    })
}

fn hit_test_gizmo(
    world: &World,
    camera: Entity,
    viewport: egui::Rect,
    pointer: Pos2,
    geometry: &GizmoGeometry,
) -> Option<(f32, GizmoHandle)> {
    let origin_screen =
        project_world_to_screen(world, camera, geometry.origin, viewport)?;
    let mut hits = Vec::new();
    for (index, (axis, direction)) in geometry.axes.iter().copied().enumerate()
    {
        let project_at = |distance: f32| {
            project_world_to_screen(
                world,
                camera,
                [
                    geometry.origin[0] + direction[0] * distance,
                    geometry.origin[1] + direction[1] * distance,
                    geometry.origin[2] + direction[2] * distance,
                ],
                viewport,
            )
        };

        if let (Some(start), Some(end)) = (
            project_at(geometry.axis_length * 0.82),
            project_at(geometry.axis_length * 1.25),
        ) {
            let distance = point_segment_distance(pointer, start, end);
            if (end - start).length() >= 12.0 && distance <= 8.0 {
                hits.push((
                    distance,
                    GizmoHandle {
                        mode: TransformModes::Move,
                        axis,
                    },
                ));
            }
        }

        if let Some(handle) = project_at(geometry.axis_length * 0.68) {
            let distance = pointer.distance(handle);
            if handle.distance(origin_screen) >= 14.0 && distance <= 10.0 {
                hits.push((
                    distance,
                    GizmoHandle {
                        mode: TransformModes::Scale,
                        axis,
                    },
                ));
            }
        }

        let radius = geometry.axis_length * 0.52;
        let mut previous = None;
        let mut ring_distance = f32::INFINITY;
        for step in 0..=48 {
            let angle = std::f32::consts::TAU * step as f32 / 48.0;
            let world_point = gizmo_ring_point(geometry, index, radius, angle);
            let Some(screen) =
                project_world_to_screen(world, camera, world_point, viewport)
            else {
                previous = None;
                continue;
            };
            if let Some(start) = previous {
                ring_distance = ring_distance
                    .min(point_segment_distance(pointer, start, screen));
            }
            previous = Some(screen);
        }
        if ring_distance <= 7.0 {
            hits.push((
                ring_distance,
                GizmoHandle {
                    mode: TransformModes::Rotate,
                    axis,
                },
            ));
        }
    }
    hits.into_iter()
        .min_by(|left, right| left.0.total_cmp(&right.0))
}

fn point_segment_distance(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let along = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    point.distance(start + segment * along)
}

fn gizmo_ring_point(
    geometry: &GizmoGeometry,
    normal_axis: usize,
    radius: f32,
    angle: f32,
) -> [f32; 3] {
    let first = geometry.axes[(normal_axis + 1) % 3].1;
    let second = geometry.axes[(normal_axis + 2) % 3].1;
    let cosine = angle.cos() * radius;
    let sine = angle.sin() * radius;
    [
        geometry.origin[0] + first[0] * cosine + second[0] * sine,
        geometry.origin[1] + first[1] * cosine + second[1] * sine,
        geometry.origin[2] + first[2] * cosine + second[2] * sine,
    ]
}

fn gizmo_axis_color(
    mode: TransformModes,
    axis: GizmoAxis,
    hovered: Option<GizmoHandle>,
    drag: &EditorGizmoDrag,
    normal: [f32; 4],
) -> [f32; 4] {
    let handle = GizmoHandle { mode, axis };
    if drag.mode == Some(mode) && drag.axis_mask[gizmo_axis_index(axis)] {
        [1.0, 0.9, 0.2, 1.0]
    } else if hovered == Some(handle) {
        [1.0, 1.0, 0.65, 1.0]
    } else {
        normal
    }
}

/// Creates the first editor helpers: a quiet XZ grid and selected-object axes.
/// More helpers such as bounds and collider shapes can append lines here later.
#[allow(clippy::too_many_arguments)]
fn build_scene_debug_overlay(
    world: &World,
    editor_camera: Option<Entity>,
    selected: Option<Entity>,
    selection: &[Entity],
    settings: EditorGizmoSettings,
    hovered: Option<GizmoHandle>,
    drag: &EditorGizmoDrag,
    selected_mode: TransformModes,
    global_axes: bool,
) -> RenderDebugOverlay {
    let mut overlay = RenderDebugOverlay::default();
    if settings.show_grid {
        // A future procedural grid can follow the editor camera
        // without building CPU vertices every frame.
        for index in -20..=20 {
            let value = index as f32;
            let color = if index == 0 {
                [0.34, 0.38, 0.46, 0.9]
            } else {
                [0.16, 0.18, 0.23, 0.72]
            };
            overlay.line([value, 0.0, -20.0], [value, 0.0, 20.0], color);
            overlay.line([-20.0, 0.0, value], [20.0, 0.0, value], color);
        }
    }
    // Cameras and lights have no mesh; draw Blender-style wire shapes in
    // the selection colors so they can be seen and clicked.
    for (entity, lines) in
        crate::editor::overlay::object_shapes(world, editor_camera)
    {
        let color = if Some(entity) == selected {
            [1.0, 0.78, 0.12, 1.0]
        } else if selection.contains(&entity) {
            [0.85, 0.42, 0.08, 1.0]
        } else {
            [0.72, 0.74, 0.78, 1.0]
        };
        for [start, end] in lines {
            if Some(entity) == selected || selection.contains(&entity) {
                overlay.line_on_top(start, end, color, 2.0);
            } else {
                overlay.line(start, end, color);
            }
        }
    }
    // Skeletons, in front of the meshes they bend.
    for (entity, lines) in crate::editor::overlay::bone_shapes(world) {
        let (color, width) = if Some(entity) == selected {
            ([1.0, 0.78, 0.12, 1.0], 2.5)
        } else if selection.contains(&entity) {
            ([0.85, 0.42, 0.08, 1.0], 2.0)
        } else {
            ([0.35, 0.75, 0.85, 0.9], 1.5)
        };
        for [start, end] in lines {
            overlay.line_on_top(start, end, color, width);
        }
    }
    // The selected environment's fog height, like a light's range.
    if let Some(fog) =
        selected.and_then(|entity| world.get::<crate::runtime::Fog>(entity))
    {
        for ([start, end], main) in
            crate::editor::overlay::fog_height_lines(fog)
        {
            let alpha = if main { 0.9 } else { 0.45 };
            overlay.line_on_top(start, end, [0.45, 0.7, 1.0, alpha], 1.5);
        }
    }
    // Point handles of the selection: reflection probe and render bounds
    // faces, dragged to resize them. Each is a small 3D cross.
    for (_, position) in
        selected.map_or_else(Vec::new, |entity| point_handles(world, entity))
    {
        for axis in 0..3 {
            let (mut start, mut end) = (position, position);
            start[axis] -= 0.12;
            end[axis] += 0.12;
            overlay.line_on_top(start, end, [1.0, 0.78, 0.12, 1.0], 2.5);
        }
    }
    if settings.show_selected_bounds {
        // Blender style: the rest of a multi-selection in darker orange.
        for &entity in
            selection.iter().filter(|&&entity| Some(entity) != selected)
        {
            if let Some(transform) = world.get::<GlobalTransform>(entity) {
                add_selected_bounds(
                    &mut overlay,
                    world,
                    entity,
                    transform.matrix,
                    [0.85, 0.42, 0.08, 1.0],
                );
            }
        }
    }
    if settings.show_selected_axes {
        if let Some(entity) = selected {
            if let Some(transform) = world.get::<GlobalTransform>(entity) {
                let matrix = transform.matrix;
                let origin = [matrix[3][0], matrix[3][1], matrix[3][2]];
                let axis_length = world
                    .get::<MeshRenderer>(entity)
                    .and_then(|renderer| {
                        world
                            .resource::<AssetServer>()
                            .mesh_bounds(renderer.mesh)
                    })
                    .map(|bounds| {
                        crate::editor::overlay::mesh_world_radius_from_origin(
                            bounds, matrix,
                        )
                    })
                    .map_or(1.0, |radius| (radius * 1.2).max(1.0));
                let mut geometry = GizmoGeometry {
                    origin,
                    axes: [
                        (
                            GizmoAxis::X,
                            normalized_axis([
                                matrix[0][0],
                                matrix[0][1],
                                matrix[0][2],
                            ]),
                        ),
                        (
                            GizmoAxis::Y,
                            normalized_axis([
                                matrix[1][0],
                                matrix[1][1],
                                matrix[1][2],
                            ]),
                        ),
                        (
                            GizmoAxis::Z,
                            normalized_axis([
                                matrix[2][0],
                                matrix[2][1],
                                matrix[2][2],
                            ]),
                        ),
                    ],
                    axis_length,
                };
                let display_mode = drag.mode.unwrap_or(selected_mode);
                if global_axes {
                    geometry.axes = [
                        (GizmoAxis::X, [1.0, 0.0, 0.0]),
                        (GizmoAxis::Y, [0.0, 1.0, 0.0]),
                        (GizmoAxis::Z, [0.0, 0.0, 1.0]),
                    ];
                }
                if matches!(
                    display_mode,
                    TransformModes::Combo | TransformModes::Move
                ) {
                    for (axis, direction) in geometry.axes {
                        add_axis(
                            &mut overlay,
                            origin,
                            direction,
                            axis_length * 1.25,
                            gizmo_axis_color(
                                TransformModes::Move,
                                axis,
                                hovered,
                                drag,
                                gizmo_base_color(axis),
                            ),
                        );
                    }
                }
                if matches!(
                    display_mode,
                    TransformModes::Combo | TransformModes::Scale
                ) {
                    for (axis, direction) in geometry.axes {
                        add_scale_gizmo_handle(
                            &mut overlay,
                            origin,
                            direction,
                            axis_length * 0.68,
                            gizmo_axis_color(
                                TransformModes::Scale,
                                axis,
                                hovered,
                                drag,
                                gizmo_base_color(axis),
                            ),
                        );
                    }
                }
                if matches!(
                    display_mode,
                    TransformModes::Combo | TransformModes::Rotate
                ) {
                    for index in 0..3 {
                        let axis = geometry.axes[index].0;
                        add_rotation_gizmo_ring(
                            &mut overlay,
                            &geometry,
                            index,
                            axis_length * 0.52,
                            gizmo_axis_color(
                                TransformModes::Rotate,
                                axis,
                                hovered,
                                drag,
                                gizmo_base_color(axis),
                            ),
                        );
                    }
                }
                if settings.show_selected_bounds {
                    add_selected_bounds(
                        &mut overlay,
                        world,
                        entity,
                        matrix,
                        [1.0, 0.78, 0.12, 1.0],
                    );
                }
            }
        }
    }
    overlay
}

// Red leans pink and green leans yellow-light so X and Y stay apart for
// red-green color-blind users; see `axis_colors_survive_color_blindness`.
const fn gizmo_base_color(axis: GizmoAxis) -> [f32; 4] {
    match axis {
        GizmoAxis::X => [0.9, 0.18, 0.28, 1.0],
        GizmoAxis::Y => [0.55, 0.95, 0.2, 1.0],
        GizmoAxis::Z => [0.25, 0.52, 1.0, 1.0],
    }
}

fn add_scale_gizmo_handle(
    overlay: &mut RenderDebugOverlay,
    origin: [f32; 3],
    direction: [f32; 3],
    length: f32,
    color: [f32; 4],
) {
    let end = add3(origin, scale3(direction, length));
    overlay.line_on_top(origin, end, color, 2.0);
    let reference = if direction[1].abs() < 0.9 {
        [0.0, 1.0, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let side = normalized_axis(cross3(direction, reference));
    let other = normalized_axis(cross3(direction, side));
    let size = length * 0.075;
    overlay.line_on_top(
        add3(end, scale3(side, -size)),
        add3(end, scale3(side, size)),
        color,
        5.0,
    );
    overlay.line_on_top(
        add3(end, scale3(other, -size)),
        add3(end, scale3(other, size)),
        color,
        5.0,
    );
}

fn add_rotation_gizmo_ring(
    overlay: &mut RenderDebugOverlay,
    geometry: &GizmoGeometry,
    normal_axis: usize,
    radius: f32,
    color: [f32; 4],
) {
    let mut previous = gizmo_ring_point(geometry, normal_axis, radius, 0.0);
    for step in 1..=48 {
        let angle = std::f32::consts::TAU * step as f32 / 48.0;
        let current = gizmo_ring_point(geometry, normal_axis, radius, angle);
        overlay.line_on_top(previous, current, color, 2.0);
        previous = current;
    }
}

fn add3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn scale3(value: [f32; 3], scale: f32) -> [f32; 3] {
    [value[0] * scale, value[1] * scale, value[2] * scale]
}

fn cross3(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
    [
        left[1] * right[2] - left[2] * right[1],
        left[2] * right[0] - left[0] * right[2],
        left[0] * right[1] - left[1] * right[0],
    ]
}

/// One profiler line: total GPU frame time, then each pass's time.
fn gpu_pass_times_label(
    times: &crate::rendering::scene_renderer::GpuPassTimes,
) -> String {
    let ms = |time: std::time::Duration| time.as_secs_f64() * 1000.0;
    let mut label = format!("GPU {:.3} ms", ms(times.total()));
    for (pass, time) in &times.0 {
        label += &format!(" | {} {:.3}", pass.label(), ms(*time));
    }
    label
}

/// One profiler line: GPU physics work, traffic and contact-grid fallbacks.
/// CPU and GPU physics time are on the timing lines above it.
fn physics_counters_label(
    counters: &crate::rendering::scene_renderer::RenderCounters,
    capacity: &crate::rendering::scene_renderer::RenderCapacityDiagnostics,
) -> String {
    let kib = |bytes: u64| bytes as f64 / 1024.0;
    format!(
        "GPU Physics: Dispatches {} | Commands {} ({:.1} KiB) | Events {:.1} KiB | States {:.1} KiB | Latency {} frames | Grid overflow {} | Oversized {} | Hash collisions {} | Fallback tests {} | Events lost {}",
        counters.physics_dispatches,
        counters.physics_commands,
        kib(counters.physics_command_bytes),
        kib(counters.physics_event_bytes),
        kib(counters.physics_state_bytes),
        counters.physics_readback_latency_frames,
        capacity.physics_grid_overflow,
        capacity.physics_oversized_bodies,
        capacity.physics_grid_hash_collisions,
        capacity.physics_fallback_tests,
        capacity.physics_events_dropped
    )
}

fn add_selected_bounds(
    overlay: &mut RenderDebugOverlay,
    world: &World,
    entity: Entity,
    matrix: [[f32; 4]; 4],
    color: [f32; 4],
) {
    let Some(renderer) = world.get::<MeshRenderer>(entity).copied() else {
        return;
    };
    let first_line = overlay.lines.len();
    if let Some(&bounds) = world.get::<RenderBounds>(entity) {
        crate::editor::overlay::add_render_bounds(
            overlay, bounds, matrix, color,
        );
        crate::editor::overlay::make_selection_outline_visible(
            overlay, first_line,
        );
        return;
    }
    let Some((minimum, maximum)) =
        world.resource::<AssetServer>().mesh_bounds(renderer.mesh)
    else {
        return;
    };
    add_bound_box(overlay, matrix, minimum, maximum, color);
    crate::editor::overlay::make_selection_outline_visible(overlay, first_line);
}

/// Removes object scale from a transform column so gizmos stay a useful size.
fn normalized_axis(axis: [f32; 3]) -> [f32; 3] {
    let length =
        (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    if length > f32::EPSILON {
        [axis[0] / length, axis[1] / length, axis[2] / length]
    } else {
        [1.0, 0.0, 0.0]
    }
}

/// Selects a model root that `add_model_to_scene` just added.
fn select_added_model(state: &mut EditorState, world: &World, root: Entity) {
    state.selected = Some(root);
    state.selection = vec![root];
    state.rename_draft = world
        .get::<Name>(root)
        .map(|name| name.0.clone())
        .unwrap_or_default();
    state.scene_dirty = true;
}

#[cfg(test)]
mod gizmo_tests {
    use super::*;

    /// CIELAB of an sRGB colour as a protanope (`1`) or deuteranope
    /// (`2`) sees it, by Machado et al. 2009 at full severity, or as
    /// typical vision does (`0`).
    fn seen_lab(srgb: [f32; 3], vision: usize) -> [f32; 3] {
        const MATRICES: [[[f32; 3]; 3]; 3] = [
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [
                [0.152_286, 1.052_583, -0.204_868],
                [0.114_503, 0.786_281, 0.099_216],
                [-0.003_882, -0.048_116, 1.051_998],
            ],
            [
                [0.367_322, 0.860_646, -0.227_968],
                [0.280_085, 0.672_501, 0.047_413],
                [-0.011_82, 0.042_94, 0.968_881],
            ],
        ];
        let linear = srgb.map(|c| {
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        });
        let [r, g, b] = MATRICES[vision].map(|row| {
            (row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2])
                .clamp(0.0, 1.0)
        });
        let f = |t: f32| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let x = f((0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.950_47);
        let y = f(0.2126 * r + 0.7152 * g + 0.0722 * b);
        let z = f((0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.088_83);
        [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
    }

    /// Every pair of `colors` differs by at least CIE76 delta E 25 under
    /// typical, protan and deutan vision.
    fn assert_distinct_for_color_blindness(colors: &[[f32; 3]], what: &str) {
        for vision in 0..3 {
            for (i, a) in colors.iter().enumerate() {
                for b in &colors[i + 1..] {
                    let (a, b) = (seen_lab(*a, vision), seen_lab(*b, vision));
                    let distance = (0..3)
                        .map(|k| (a[k] - b[k]).powi(2))
                        .sum::<f32>()
                        .sqrt();
                    assert!(
                        distance >= 25.0,
                        "{what}: {distance} apart in vision {vision}"
                    );
                }
            }
        }
    }

    #[test]
    fn axis_colors_survive_color_blindness() {
        let rgb = |color: [f32; 4]| [color[0], color[1], color[2]];
        let theme = |color: egui::Color32| {
            [color.r(), color.g(), color.b()].map(|c| f32::from(c) / 255.0)
        };
        assert_distinct_for_color_blindness(
            &[GizmoAxis::X, GizmoAxis::Y, GizmoAxis::Z]
                .map(|axis| rgb(gizmo_base_color(axis))),
            "gizmo axes",
        );
        assert_distinct_for_color_blindness(
            &gui_elements::EditorTheme::AXIS.map(theme),
            "theme axes",
        );
        assert_distinct_for_color_blindness(
            &[
                gui_elements::EditorTheme::WARNING,
                gui_elements::EditorTheme::ERROR,
            ]
            .map(theme),
            "warning and error",
        );
        assert_distinct_for_color_blindness(
            &[[1.0, 0.78, 0.12], [0.85, 0.42, 0.08], [0.72, 0.74, 0.78]],
            "active, selected and plain wireframes",
        );
    }

    #[test]
    fn gpu_pass_times_label_shows_total_then_each_pass() {
        use crate::rendering::frame_passes::FramePass;
        use crate::rendering::scene_renderer::GpuPassTimes;
        use std::time::Duration;
        let times = GpuPassTimes(vec![
            (FramePass::Shadow, Duration::from_micros(100)),
            (FramePass::Scene, Duration::from_micros(1250)),
        ]);
        assert_eq!(
            gpu_pass_times_label(&times),
            "GPU 1.350 ms | Shadow 0.100 | Scene 1.250"
        );
    }

    #[test]
    fn physics_counters_label_shows_traffic_and_fallbacks() {
        use crate::rendering::scene_renderer::{
            RenderCapacityDiagnostics, RenderCounters,
        };
        let counters = RenderCounters {
            physics_dispatches: 8,
            physics_commands: 2,
            physics_command_bytes: 160,
            physics_event_bytes: 2048,
            physics_state_bytes: 512,
            physics_readback_latency_frames: 2,
            ..RenderCounters::default()
        };
        let capacity = RenderCapacityDiagnostics {
            physics_grid_overflow: 3,
            physics_oversized_bodies: 1,
            physics_grid_hash_collisions: 4,
            physics_fallback_tests: 40,
            physics_events_dropped: 5,
            ..RenderCapacityDiagnostics::default()
        };
        assert_eq!(
            physics_counters_label(&counters, &capacity),
            "GPU Physics: Dispatches 8 | Commands 2 (0.2 KiB) | Events 2.0 KiB | States 0.5 KiB | Latency 2 frames | Grid overflow 3 | Oversized 1 | Hash collisions 4 | Fallback tests 40 | Events lost 5"
        );
    }

    #[test]
    fn selected_outline_shows_the_render_bounds_override() {
        let mut world = World::new();
        let assets = AssetServer::default();
        let renderer = MeshRenderer {
            mesh: assets.fallback_mesh,
            material: assets.fallback_material,
            cast_shadows: true,
            receive_shadows: true,
        };
        world.insert_resource(assets);
        let entity = world.spawn(renderer).id();
        let matrix = Transform::default().to_matrix();

        let mut overlay = RenderDebugOverlay::default();
        add_selected_bounds(&mut overlay, &world, entity, matrix, [1.0; 4]);
        assert_eq!(overlay.lines.len(), 12, "mesh box without an override");

        world.entity_mut(entity).insert(RenderBounds::Sphere {
            center: [0.0; 3],
            radius: 3.0,
        });
        let mut overlay = RenderDebugOverlay::default();
        add_selected_bounds(&mut overlay, &world, entity, matrix, [1.0; 4]);
        assert_eq!(overlay.lines.len(), 96, "override sphere circles");
        let [x, y, z] = overlay.lines[0].start;
        assert!(((x * x + y * y + z * z).sqrt() - 3.0).abs() < 1e-4);
    }

    #[test]
    fn selection_change_cancels_drag_on_its_own_entity() {
        let mut world = World::new();
        let original = Transform::default();
        let preview = Transform::default().with_position(4.0, 0.0, 0.0);
        let first = world.spawn(preview).id();
        let second_transform =
            Transform::default().with_position(0.0, 9.0, 0.0);
        let second = world.spawn(second_transform).id();
        let mut state = EditorState {
            selected: Some(second),
            ..EditorState::default()
        };
        let mut drag = EditorGizmoDrag {
            mode: Some(TransformModes::Move),
            modal: true,
            entity: Some(first),
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(original),
            ..EditorGizmoDrag::default()
        };
        let mut edited = Some(second_transform);

        update_transform_gizmo(
            &mut world,
            &mut state,
            None,
            Some(Pos2::new(50.0, 0.0)),
            None,
            None,
            false,
            &mut drag,
            &mut edited,
            &mut EditorHistory::default(),
            false,
            false,
            false,
            &mut EditorTransformMode::default(),
        );

        assert!(!drag.is_active());
        assert_eq!(world.get::<Transform>(first), Some(&original));
        assert_eq!(edited, Some(second_transform));
    }

    #[test]
    fn projected_two_axis_move_recovers_both_axis_distances() {
        let coefficients = screen_axis_coefficients(
            [[10.0, 0.0], [0.0, 20.0], [5.0, 5.0]],
            [true, true, false],
            [30.0, 40.0],
        );

        assert!((coefficients[0] - 3.0).abs() < 0.001);
        assert!((coefficients[1] - 2.0).abs() < 0.001);
        assert_eq!(coefficients[2], 0.0);
    }

    #[test]
    fn move_line_uses_world_axis_parameter_without_screen_projection_drift() {
        let ray = super::super::picking::Ray {
            origin: Vector3::new(5.0, 0.0, 10.0),
            direction: Vector3::new(0.0, 0.0, -1.0),
        };
        let parameter = move_axis_parameter(
            ray,
            [0.0; 3],
            [1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
        )
        .unwrap();
        let drag = EditorGizmoDrag {
            mode: Some(TransformModes::Move),
            axis_mask: [true, false, false],
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(Transform::default()),
            local_delta_axes: [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            move_start_axis_parameter: Some(2.0),
            ..EditorGizmoDrag::default()
        };

        let transformed =
            transformed_from_pointer(&drag, Pos2::ZERO, Some(parameter))
                .unwrap();

        assert!((transformed.position[0] - 3.0).abs() < 0.001);
        assert_eq!(transformed.position[1], 0.0);
        assert_eq!(transformed.position[2], 0.0);
    }

    #[test]
    fn modal_scale_only_changes_enabled_axes() {
        let drag = EditorGizmoDrag {
            mode: Some(TransformModes::Scale),
            modal: true,
            axis_mask: [true, false, true],
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(
                Transform::default().with_scale(1.0, 2.0, 3.0),
            ),
            ..EditorGizmoDrag::default()
        };
        let pointer = Pos2::new(std::f32::consts::LN_2 / 0.01, 0.0);

        let transformed =
            transformed_from_pointer(&drag, pointer, None).unwrap();

        assert!((transformed.scale[0] - 2.0).abs() < 0.001);
        assert_eq!(transformed.scale[1], 2.0);
        assert!((transformed.scale[2] - 6.0).abs() < 0.001);
    }

    #[test]
    fn direct_scale_sensitivity_uses_visible_handle_length() {
        let make_drag = |axis_length| EditorGizmoDrag {
            mode: Some(TransformModes::Scale),
            modal: false,
            axis_mask: [true, false, false],
            active_axis: Some(GizmoAxis::X),
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(Transform::default()),
            screen_vectors: [[5.0, 0.0], [0.0; 2], [0.0; 2]],
            gizmo_axis_length: axis_length,
            ..EditorGizmoDrag::default()
        };
        let short = transformed_from_pointer(
            &make_drag(10.0),
            Pos2::new(34.0, 0.0),
            None,
        )
        .unwrap();
        let long = transformed_from_pointer(
            &make_drag(20.0),
            Pos2::new(68.0, 0.0),
            None,
        )
        .unwrap();

        assert!((short.scale[0] - 2.0).abs() < 0.001);
        assert!((long.scale[0] - 2.0).abs() < 0.001);
    }

    #[test]
    fn gizmo_snap_quantizes_move_scale_and_rotation() {
        let move_drag = EditorGizmoDrag {
            mode: Some(TransformModes::Move),
            snap_enabled: true,
            axis_mask: [true, false, false],
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(Transform::default()),
            screen_vectors: [[10.0, 0.0], [0.0, 10.0], [0.0; 2]],
            local_delta_axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0; 3]],
            ..EditorGizmoDrag::default()
        };
        let moved =
            transformed_from_pointer(&move_drag, Pos2::new(14.0, 0.0), None)
                .unwrap();
        assert_eq!(moved.position, [1.0, 0.0, 0.0]);

        let scale_drag = EditorGizmoDrag {
            mode: Some(TransformModes::Scale),
            modal: true,
            snap_enabled: true,
            axis_mask: [true; 3],
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(Transform::default()),
            ..EditorGizmoDrag::default()
        };
        let scaled =
            transformed_from_pointer(&scale_drag, Pos2::new(14.0, 0.0), None)
                .unwrap();
        assert_eq!(scaled.scale, [1.2; 3]);
        let drag = &EditorGizmoDrag::default();
        assert!(
            (snap_angle(drag, 0.3, true) - 15.0_f32.to_radians()).abs()
                < 0.0001
        );
        assert_eq!(snap_angle(drag, 0.3, false), 0.3);

        let fine_drag = EditorGizmoDrag {
            snap_steps: super::super::shortcuts::SnapSteps {
                translate: 0.25,
                rotate_degrees: 10.0,
                scale: 0.5,
            },
            ..move_drag
        };
        let moved =
            transformed_from_pointer(&fine_drag, Pos2::new(14.0, 0.0), None)
                .unwrap();
        assert_eq!(moved.position, [1.5, 0.0, 0.0]);
        assert!(
            (snap_angle(&fine_drag, 0.3, true) - 20.0_f32.to_radians()).abs()
                < 0.0001
        );
    }

    #[test]
    fn global_gizmo_axes_ignore_object_rotation() {
        let mut world = World::new();
        let rotated = Transform::default().with_rotation(
            0.0,
            0.0,
            std::f32::consts::FRAC_PI_2,
        );
        let entity = world
            .spawn(GlobalTransform {
                matrix: rotated.to_matrix(),
            })
            .id();
        let local = gizmo_geometry(&world, entity, false).unwrap();
        let global = gizmo_geometry(&world, entity, true).unwrap();
        assert!(local.axes[0].1[1] > 0.99);
        assert_eq!(global.axes[0].1, [1.0, 0.0, 0.0]);
        assert_eq!(global.axes[1].1, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn global_rotation_uses_parent_space_axis() {
        let original = Transform::default().with_rotation(0.4, 0.7, -0.2);
        let drag = EditorGizmoDrag {
            mode: Some(TransformModes::Rotate),
            global_axes: true,
            axis_mask: [true, false, false],
            start_pointer: Some(Pos2::new(10.0, 0.0)),
            origin_screen: Some(Pos2::ZERO),
            original_transform: Some(original),
            local_delta_axes: [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            rotation_screen_signs: [1.0; 3],
            ..EditorGizmoDrag::default()
        };
        let actual =
            transformed_from_pointer(&drag, Pos2::new(0.0, 10.0), None)
                .unwrap();
        let expected = rotate_transform_in_parent_space(
            original,
            [1.0, 0.0, 0.0],
            std::f32::consts::FRAC_PI_2,
        );
        let actual_matrix = Matrix4::from(actual.to_matrix());
        let expected_matrix = Matrix4::from(expected.to_matrix());
        assert!((actual_matrix - expected_matrix).norm() < 0.001);
    }

    #[test]
    fn global_scale_on_rotated_object_uses_matching_local_axis() {
        let drag = EditorGizmoDrag {
            mode: Some(TransformModes::Scale),
            modal: true,
            global_axes: true,
            axis_mask: [true, false, false],
            start_pointer: Some(Pos2::ZERO),
            original_transform: Some(
                Transform::default().with_scale(2.0, 3.0, 4.0),
            ),
            global_scale_weights: [
                [0.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            ..EditorGizmoDrag::default()
        };
        let scaled = transformed_from_pointer(
            &drag,
            Pos2::new(std::f32::consts::LN_2 / 0.01, 0.0),
            None,
        )
        .unwrap();
        assert_eq!(scaled.scale[0], 2.0);
        assert!((scaled.scale[1] - 6.0).abs() < 0.001);
        assert_eq!(scaled.scale[2], 4.0);
    }

    #[test]
    fn projected_ring_direction_is_converted_to_positive_local_rotation() {
        let drag = EditorGizmoDrag {
            start_pointer: Some(Pos2::new(10.0, 0.0)),
            origin_screen: Some(Pos2::ZERO),
            rotation_screen_signs: [1.0, 1.0, -1.0],
            ..EditorGizmoDrag::default()
        };

        let angle = screen_rotation_angle(&drag, Pos2::new(0.0, -10.0), 2);

        assert!((angle - std::f32::consts::FRAC_PI_2).abs() < 0.001);
    }

    #[test]
    fn rotation_composes_around_the_objects_local_axis() {
        let original = Transform::default().with_rotation(0.4, 0.7, -0.2);
        let angle = 0.3;
        let transformed = rotate_transform_locally(
            original,
            [0.0, 0.0, angle],
            [false, false, true],
        );
        let actual = Matrix4::from(transformed.to_matrix());
        let expected = Matrix4::from(original.to_matrix())
            * Rotation3::from_axis_angle(&Vector3::z_axis(), angle)
                .to_homogeneous();

        for row in 0..3 {
            for column in 0..3 {
                assert!(
                    (actual[(row, column)] - expected[(row, column)]).abs()
                        < 0.001
                );
            }
        }
    }

    #[test]
    fn free_rotation_composes_around_the_camera_facing_axis() {
        let original = Transform::default().with_rotation(0.4, 0.7, -0.2);
        let angle = 0.3;
        let transformed =
            rotate_transform_in_parent_space(original, [0.0, 0.0, -1.0], angle);
        let actual = Matrix4::from(transformed.to_matrix());
        let expected = Rotation3::from_axis_angle(&Vector3::z_axis(), -angle)
            .to_homogeneous()
            * Matrix4::from(original.to_matrix());

        for row in 0..3 {
            for column in 0..3 {
                assert!(
                    (actual[(row, column)] - expected[(row, column)]).abs()
                        < 0.001
                );
            }
        }
    }
}
