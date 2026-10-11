//! Tests for the editor state, dock layout, project tools, and GUI elements.

use std::path::Path;
use std::time::Duration;

use crate::DataAsset;

use super::*;

#[test]
fn restart_waits_for_worker_exit_and_stop_cancels_pending_restart() {
    let mut build = EditorBuildState {
        running: true,
        ..EditorBuildState::default()
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    *build.receiver.get_mut().unwrap() = Some(receiver);
    build.request_restart();
    assert!(!build.take_restart_ready());
    assert!(build.stop.load(std::sync::atomic::Ordering::Relaxed));

    sender
        .send(BuildWorkerMessage::Finished(BuildFinished {
            success: false,
            output: "stopped".into(),
        }))
        .unwrap();
    assert_eq!(build.poll(), Some(false));
    assert!(build.take_restart_ready());
    assert!(!build.take_restart_ready());

    build.running = true;
    build.request_restart();
    build.request_stop();
    assert!(!build.restart_requested);
}

#[test]
fn embedded_preview_restores_authored_scene_after_simulation() {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let world = app.world_mut();
    let entity = world
        .spawn((
            SceneId::default(),
            Name("Preview Object".into()),
            Transform::default(),
        ))
        .id();
    let mut state = EditorState {
        selected: Some(entity),
        selection: vec![entity],
        workspace: EditorWorkspace::Scene,
        active_area: 2,
        ..EditorState::default()
    };
    let mut history = EditorHistory::default();
    view::start_embedded_preview(world, &mut state, &mut history);
    assert_eq!(state.mode, EditorMode::Play);
    assert!(state.play_snapshot.is_some());
    assert!(!world.resource::<crate::runtime::TimeControl>().paused);
    world.get_mut::<Transform>(entity).unwrap().position[0] = 10.0;
    let spawned = world
        .spawn((Name("Runtime".into()), Transform::default()))
        .id();
    history.push_undo(scene_document(world, "Preview Edit").unwrap());

    state.workspace = EditorWorkspace::Game;
    view::stop_embedded_preview(world, &mut state, &mut history);
    assert_eq!(state.mode, EditorMode::Edit);
    assert!(state.play_snapshot.is_none());
    assert!(world.resource::<crate::runtime::TimeControl>().paused);
    assert!(world.get_entity(spawned).is_err());
    assert!(history.undo.is_empty());
    let restored = state.selected.unwrap();
    assert_eq!(world.get::<Transform>(restored).unwrap().position[0], 0.0);
}

#[test]
fn typed_asset_assignment_snapshots_once_and_rejects_unsaved_meshes() {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let world = app.world_mut();
    let (cube, sphere, fallback_material, material, transient) = {
        let mut assets = world.resource_mut::<AssetServer>();
        let cube = assets.fallback_mesh;
        let sphere = assets.builtin_sphere;
        let fallback_material = assets.fallback_material;
        let material = assets.materials.insert(MaterialAsset::default());
        let transient =
            assets.meshes.insert(crate::assets::MeshAsset::default());
        (cube, sphere, fallback_material, material, transient)
    };
    let entity = world
        .spawn((
            SceneId::default(),
            Name("Object".into()),
            Transform::default(),
            MeshRenderer {
                mesh: cube,
                material: fallback_material,
                cast_shadows: true,
                receive_shadows: true,
            },
        ))
        .id();
    let mut history = EditorHistory::default();

    assert!(
        view::assign_mesh_handle(world, &mut history, entity, transient)
            .is_err()
    );
    assert!(history.undo.is_empty());
    assert!(
        view::assign_mesh_handle(world, &mut history, entity, sphere).unwrap()
    );
    assert_eq!(world.get::<MeshRenderer>(entity).unwrap().mesh, sphere);
    assert_eq!(history.undo.len(), 1);
    assert!(
        !view::assign_mesh_handle(world, &mut history, entity, sphere).unwrap()
    );
    assert_eq!(history.undo.len(), 1);

    assert!(view::assign_material_handle(
        world,
        &mut history,
        entity,
        material
    )
    .unwrap());
    assert_eq!(
        world.get::<MeshRenderer>(entity).unwrap().material,
        material
    );
    assert_eq!(history.undo.len(), 2);
}

#[test]
fn editor_play_uses_debug_builds_by_default() {
    let state = EditorState::default();

    assert_eq!(state.game_build_profile, GameBuildProfile::Debug);
    assert!(state.project_root.is_empty());
    assert!(state.scene_path.is_empty());
    assert_eq!(state.code_path, "src/main.rs");
    assert_eq!(GameBuildProfile::Debug.cargo_flag(), None);
    assert_eq!(GameBuildProfile::Release.cargo_flag(), Some("--release"));
}

#[test]
fn editor_plugin_builds_panels_and_preserves_selection() {
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let entity = app.spawn((Name("Cube".into()), Transform::default()));
    app.world_mut().resource_mut::<EditorState>().selected = Some(entity);

    app.update(Duration::from_millis(16)).unwrap();
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });

    assert_eq!(app.world().resource::<EditorState>().selected, Some(entity));
    assert!(!output.shapes.is_empty());
}

#[test]
fn material_edits_copy_shared_materials_and_edit_owned_ones_in_place() {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    let world = app.world_mut();
    let (mesh, fallback) = {
        let assets = world.resource::<AssetServer>();
        (assets.fallback_mesh, assets.fallback_material)
    };
    let renderer = MeshRenderer {
        mesh,
        material: fallback,
        cast_shadows: true,
        receive_shadows: true,
    };
    let edited = world.spawn(renderer).id();
    let other = world.spawn(renderer).id();
    let material_of = |world: &World, entity| {
        world.get::<MeshRenderer>(entity).unwrap().material
    };
    let red = MaterialAsset {
        base_color: [1.0, 0.0, 0.0, 1.0],
        ..MaterialAsset::default()
    };

    // The fallback is shared, so the edit gets its own copy.
    super::view::set_material(world, edited, red.clone());
    let owned = material_of(world, edited);
    assert_ne!(owned, fallback);
    assert_eq!(material_of(world, other), fallback);
    assert_ne!(
        world.resource::<AssetServer>().materials.get(fallback),
        Some(&red)
    );

    // An owned material is edited in place instead of leaking a copy per
    // drag frame.
    let blue = MaterialAsset {
        base_color: [0.0, 0.0, 1.0, 1.0],
        ..red.clone()
    };
    super::view::set_material(world, edited, blue.clone());
    assert_eq!(material_of(world, edited), owned);
    assert_eq!(
        world.resource::<AssetServer>().materials.get(owned),
        Some(&blue)
    );

    // Sharing the owned material again makes the next edit copy it.
    world.get_mut::<MeshRenderer>(other).unwrap().material = owned;
    super::view::set_material(world, edited, red.clone());
    assert_ne!(material_of(world, edited), owned);
    assert_eq!(
        world.resource::<AssetServer>().materials.get(owned),
        Some(&blue)
    );
}

#[test]
fn scene_render_settings_edits_are_undoable_scene_changes() {
    use crate::runtime::{CullingMode, QualityProfile};
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let world = app.world_mut();
    let mut history = EditorHistory::default();
    let mut state = EditorState::default();
    let defaults = (QualityProfile::Auto, CullingMode::Auto);
    let current = |world: &World| {
        let settings = world.resource::<RenderSettings>();
        (settings.quality, settings.culling)
    };

    let mut edited = world.resource::<RenderSettings>().clone();
    edited.quality = QualityProfile::High;
    edited.culling = CullingMode::FrustumAndOcclusion;
    super::view::apply_render_settings(
        world,
        &mut history,
        &mut state,
        edited.clone(),
        defaults,
    );
    let chosen = (QualityProfile::High, CullingMode::FrustumAndOcclusion);
    assert_eq!(current(world), chosen);
    assert!(state.scene_dirty);
    assert_eq!(history.undo.len(), 1);

    // Undo reloads the snapshot taken before the edit.
    let before = history.undo.pop_back().unwrap();
    crate::runtime::load_scene_document(world, &before, SceneLoadMode::Replace)
        .unwrap();
    assert_eq!(current(world), defaults);

    // A panel copy taken before that Undo must not write the edit back.
    super::view::apply_render_settings(
        world,
        &mut history,
        &mut state,
        edited,
        chosen,
    );
    assert_eq!(current(world), defaults);
    assert!(history.undo.is_empty());
}

#[test]
fn scene_view_shading_and_effects_apply_only_in_the_scene_workspace() {
    let mut app = App::new();
    app.add_plugin(EditorPlugin).unwrap();
    assert_eq!(editor_debug_view(app.world()), SceneDebugView::Lit);

    app.world_mut()
        .resource_mut::<EditorGizmoSettings>()
        .shading = SceneDebugView::Normals;
    assert_eq!(editor_debug_view(app.world()), SceneDebugView::Normals);
    let off = crate::rendering::scene_renderer::SceneEffects {
        fog: false,
        bloom: true,
        ambient_occlusion: false,
    };
    app.world_mut()
        .resource_mut::<EditorGizmoSettings>()
        .effects = off;
    assert_eq!(editor_scene_effects(app.world()), off);

    app.world_mut().resource_mut::<EditorState>().workspace =
        EditorWorkspace::Game;
    assert_eq!(editor_debug_view(app.world()), SceneDebugView::Lit);
    assert_eq!(
        editor_scene_effects(app.world()),
        crate::rendering::scene_renderer::SceneEffects::default()
    );
}

#[test]
fn code_editor_keeps_relative_and_absolute_paths_inside_project() {
    assert!(project_source_path("testGame", "../outside.comp").is_err());
    assert!(project_source_path("testGame", "/tmp/outside.comp").is_err());
    assert!(project_source_path("testGame", "shaders/custom.comp").is_ok());
    assert!(
        project_source_path("testGame", "src/shaders/compute/full.comp")
            .is_ok()
    );
    let project = std::path::Path::new("testGame").canonicalize().unwrap();
    assert!(project_source_path(
        project.to_str().unwrap(),
        project.join("src/main.rs").to_str().unwrap()
    )
    .is_ok());
}

#[test]
fn editor_document_paths_always_use_forward_slashes() {
    assert_eq!(
        editor_relative_path(std::path::Path::new(r"src\main.rs")),
        "src/main.rs"
    );
}

#[test]
fn opening_project_replaces_stale_code_with_relative_document_paths() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-open-path-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let project = create_project(&folder, "Fresh Game").unwrap();
    let mut state = EditorState {
        project_root: "/old/project".into(),
        scene_path: "/old/project/scenes/old.rscene".into(),
        code_path: "/old/project/src/main.rs".into(),
        code_source: "old project source".into(),
        code_dirty: true,
        ..EditorState::default()
    };

    set_open_project_paths(&mut state, &project);

    assert_eq!(state.project_root, project.root.display().to_string());
    assert_eq!(state.scene_path, "scenes/main.rscene");
    assert_eq!(state.code_path, "src/main.rs");
    assert!(state.code_source.is_empty());
    assert!(!state.code_dirty);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn project_manager_requires_an_explicit_creation_folder() {
    assert!(ProjectManagerState::default()
        .parent_directory
        .as_os_str()
        .is_empty());
}

#[test]
fn compute_shader_validation_checks_entry_point_and_workgroup() {
    assert!(validate_project_source(
        "testGame",
        "testGame/shaders/custom.comp",
        "#version 450\nlayout(local_size_x = 64) in;\nvoid main() {}",
    )
    .is_ok());
    assert!(validate_project_source(
        "testGame",
        "testGame/shaders/custom.comp",
        "#version 450\nvoid main() {}",
    )
    .is_err());
}

#[test]
fn dock_areas_can_split_change_type_and_close() {
    let mut layout = EditorDockNode::Area {
        id: 1,
        panel: EditorPanel::Scene,
    };
    assert!(layout.split(1, EditorSplitAxis::Columns, 2));
    assert!(layout.set_panel(2, EditorPanel::Code));
    assert!(layout.close(1));
    assert_eq!(
        layout,
        EditorDockNode::Area {
            id: 2,
            panel: EditorPanel::Code,
        }
    );
}

#[test]
fn duplicate_panels_get_different_widget_id_spaces() {
    let mut layout = EditorDockNode::Split {
        axis: EditorSplitAxis::Columns,
        ratio: 0.5,
        first: Box::new(EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Console,
        }),
        second: Box::new(EditorDockNode::Area {
            id: 2,
            panel: EditorPanel::Console,
        }),
    };
    let context = Context::default();
    let mut panel_ids = Vec::new();
    let mut active_area = 1;
    let mut actions = Vec::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        )),
        ..Default::default()
    };

    let _ = context.run(input, |context| {
        CentralPanel::default().show(context, |ui| {
            let rect = ui.available_rect_before_wrap();
            show_dock_node(
                ui,
                &mut layout,
                rect,
                &mut active_area,
                &mut actions,
                &mut |ui, _panel| panel_ids.push(ui.next_auto_id()),
            );
        });
    });

    assert_eq!(panel_ids.len(), 2);
    assert_ne!(panel_ids[0], panel_ids[1]);
}

#[test]
fn dock_header_buttons_have_the_same_size_and_height() {
    let context = Context::default();
    let mut rects = Vec::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(300.0, 100.0),
        )),
        ..Default::default()
    };

    let _ = context.run(input, |context| {
        CentralPanel::default().show(context, |ui| {
            ui.horizontal(|ui| {
                for icon in [
                    EditorIcon::SplitColumns,
                    EditorIcon::SplitRows,
                    EditorIcon::Close,
                ] {
                    rects.push(dock_header_button(ui, icon, "test").rect);
                }
            });
        });
    });

    assert_eq!(rects.len(), 3);
    assert!(rects
        .iter()
        .all(|rect| rect.size() == egui::vec2(22.0, 20.0)));
    assert!(rects.iter().all(|rect| rect.top() == rects[0].top()));
}

#[test]
fn dock_area_clips_panel_content_to_its_own_rectangle() {
    let context = Context::default();
    let mut layout = EditorDockNode::Area {
        id: 1,
        panel: EditorPanel::Hierarchy,
    };
    let mut active_area = 1;
    let mut actions = Vec::new();
    let mut panel_clip = None;
    let mut expected_clip = None;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 300.0),
        )),
        ..Default::default()
    };

    let _ = context.run(input, |context| {
        CentralPanel::default().show(context, |ui| {
            let area_rect = egui::Rect::from_min_size(
                ui.min_rect().min,
                egui::vec2(120.0, 100.0),
            );
            expected_clip =
                Some(ui.clip_rect().intersect(area_rect.shrink(1.0)));
            show_dock_node(
                ui,
                &mut layout,
                area_rect,
                &mut active_area,
                &mut actions,
                &mut |ui, _panel| panel_clip = Some(ui.clip_rect()),
            );
        });
    });

    assert_eq!(panel_clip, expected_clip);
}

#[test]
fn clicking_anywhere_inside_a_dock_selects_it() {
    let mut layout = EditorDockNode::Split {
        axis: EditorSplitAxis::Columns,
        ratio: 0.5,
        first: Box::new(EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Hierarchy,
        }),
        second: Box::new(EditorDockNode::Area {
            id: 2,
            panel: EditorPanel::Inspector,
        }),
    };
    let context = Context::default();
    let mut active_area = 1;
    let mut actions = Vec::new();
    let pointer = egui::pos2(300.0, 80.0);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(400.0, 200.0),
        )),
        events: vec![
            egui::Event::PointerMoved(pointer),
            egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..Default::default()
    };

    let _ = context.run(input, |context| {
        CentralPanel::default().show(context, |ui| {
            let rect = ui.available_rect_before_wrap();
            show_dock_node(
                ui,
                &mut layout,
                rect,
                &mut active_area,
                &mut actions,
                &mut |_ui, _panel| {},
            );
        });
    });

    assert_eq!(active_area, 2);
}

#[test]
fn dock_layout_round_trips_through_project_settings() {
    let file = EditorLayoutFile {
        layout: EditorDockNode::default_layout(),
        active_area: 2,
        next_area_id: 5,
        snap: Some((
            true,
            shortcuts::SnapSteps {
                translate: 0.5,
                ..shortcuts::SnapSteps::default()
            },
        )),
    };
    let json = serde_json::to_string(&file).unwrap();
    let restored: EditorLayoutFile = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.layout, file.layout);
    assert_eq!(restored.active_area, 2);
    assert_eq!(restored.next_area_id, 5);
    assert_eq!(restored.snap, file.snap);
    let old: EditorLayoutFile = serde_json::from_str(&json.replace(
        r#","snap":[true,{"translate":0.5,"rotate_degrees":15.0,"scale":0.1}]"#,
        "",
    ))
    .unwrap();
    assert_eq!(old.snap, None);
}

#[test]
fn imported_project_files_never_overwrite_existing_assets() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-assets-{}", uuid::Uuid::new_v4()));
    let project = folder.join("project");
    let source_folder = folder.join("source");
    std::fs::create_dir_all(project.join("assets")).unwrap();
    std::fs::create_dir_all(&source_folder).unwrap();
    std::fs::write(project.join("assets/cube.glb"), "old").unwrap();
    std::fs::write(source_folder.join("cube.glb"), "new").unwrap();

    let imported = copy_into_project_assets(
        project.to_str().unwrap(),
        &source_folder.join("cube.glb"),
    )
    .unwrap();

    assert_eq!(
        std::fs::read(project.join("assets/cube.glb")).unwrap(),
        b"old"
    );
    assert_eq!(imported.file_name().unwrap(), "cube_2.glb");
    assert_eq!(std::fs::read(imported).unwrap(), b"new");
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn cargo_worker_completion_is_received_without_blocking() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut build = EditorBuildState {
        running: true,
        output: "running".into(),
        console_cursor: 0,
        receiver: std::sync::Mutex::new(Some(receiver)),
        ..Default::default()
    };
    sender
        .send(BuildWorkerMessage::Output(
            "\nruntime panic details\n".into(),
        ))
        .unwrap();

    assert_eq!(build.poll(), None);
    assert!(build.running);
    assert!(build.output.contains("runtime panic details"));

    sender
        .send(BuildWorkerMessage::Finished(BuildFinished {
            success: true,
            output: "task finished".into(),
        }))
        .unwrap();

    assert_eq!(build.poll(), Some(true));
    assert!(!build.running);
    assert!(build.output.contains("runtime panic details"));
    assert!(build.output.contains("task finished"));
    let console_output = build.take_console_output();
    assert!(console_output.contains("runtime panic details"));
    assert!(console_output.contains("task finished"));
    assert!(build.take_console_output().is_empty());
}

#[test]
fn console_cursor_survives_output_truncation() {
    let mut build = EditorBuildState::default();
    // Multi-byte text makes a stale byte offset land inside a character.
    build.append_output(&"é".repeat(200_000));
    build.take_console_output();
    build.append_output(&"ü".repeat(100_000));
    build.append_output("tail");
    let text = build.take_console_output();
    assert_eq!(text, format!("{}tail", "ü".repeat(100_000)));
    assert!(build.take_console_output().is_empty());
}

#[test]
fn export_package_contains_executable_scene_assets_and_readme() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-export-test-{}", uuid::Uuid::new_v4()));
    let project = folder.join("project");
    let exports = folder.join("exports");
    std::fs::create_dir_all(project.join("build")).unwrap();
    std::fs::create_dir_all(project.join("assets")).unwrap();
    std::fs::create_dir_all(&exports).unwrap();
    let executable = project.join("fake_game");
    std::fs::write(&executable, "binary").unwrap();
    std::fs::write(project.join("build/main.rscene.bin"), "scene").unwrap();
    std::fs::write(project.join("assets/texture.png"), "texture").unwrap();
    std::fs::create_dir_all(project.join("scenes")).unwrap();
    std::fs::write(project.join("scenes/level_two.rscene"), "scene").unwrap();
    std::fs::write(project.join("project.json"), "{}").unwrap();

    let exported = package_game_files(
        &project,
        &executable,
        &exports,
        "Fake Game",
        "fake_game",
        std::path::Path::new("build/main.rscene.bin"),
        None,
    )
    .unwrap();

    let executable_name = if cfg!(target_os = "windows") {
        "fake_game.exe"
    } else {
        "fake_game"
    };
    assert!(exported.join(executable_name).is_file());
    assert!(exported.join("build/main.rscene.bin").is_file());
    assert!(exported.join("assets/texture.png").is_file());
    assert!(exported.join("scenes/level_two.rscene").is_file());
    assert!(exported.join("README.txt").is_file());
    // The runtime finds `assets/` from the folder holding `project.json`.
    assert!(exported.join("project.json").is_file());

    // A Windows export from any system gets an .exe in its own folder.
    let windows = package_game_files(
        &project,
        &executable,
        &exports,
        "Fake Game",
        "fake_game",
        std::path::Path::new("build/main.rscene.bin"),
        Some(super::WINDOWS_TARGET),
    )
    .unwrap();
    assert!(windows.ends_with("fake_game_windows_export"));
    assert!(windows.join("fake_game.exe").is_file());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn every_editor_panel_draws_even_when_optional_resources_are_missing() {
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    app.world_mut().remove_resource::<EditorConsole>();
    let context = Context::default();

    for (index, panel) in EditorPanel::ALL.into_iter().enumerate() {
        app.world_mut().resource_mut::<EditorState>().dock_layout =
            EditorDockNode::Area {
                id: index as u64 + 1,
                panel,
            };
        let _ = context.run(egui::RawInput::default(), |context| {
            draw_editor_view(app.world_mut(), context);
        });
    }
}

#[test]
fn idle_editor_stops_continuous_redraw() {
    let mut app = App::new();
    app.add_plugin(EditorPlugin).unwrap();
    assert!(!editor_needs_continuous_redraw(app.world()));

    app.world_mut().resource_mut::<EditorState>().mode = EditorMode::Play;
    assert!(editor_needs_continuous_redraw(app.world()));

    app.world_mut().resource_mut::<EditorState>().mode = EditorMode::Paused;
    app.world_mut().resource_mut::<EditorFlyCamera>().active = true;
    assert!(editor_needs_continuous_redraw(app.world()));
}

#[cfg(unix)]
#[test]
fn streamed_command_sends_lines_early_and_stops_on_request() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_stop = std::sync::Arc::clone(&stop);
    let worker = std::thread::spawn(move || {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "echo first; exec sleep 30"]);
        run_streamed(&mut command, &sender, &worker_stop, None)
    });

    // The first line arrives while the process still runs.
    match receiver.recv_timeout(Duration::from_secs(10)).unwrap() {
        BuildWorkerMessage::Output(text) => assert_eq!(text, "first\n"),
        BuildWorkerMessage::Finished(_) => panic!("finished too early"),
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(worker.join().unwrap().unwrap().is_none());
}

#[cfg(unix)]
#[test]
fn streamed_game_gets_the_save_command_on_standard_input() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let save = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_save = std::sync::Arc::clone(&save);
    let worker = std::thread::spawn(move || {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "echo ready; read line; echo \"got $line\""]);
        let stop = std::sync::atomic::AtomicBool::new(false);
        run_streamed(&mut command, &sender, &stop, Some(&worker_save))
    });
    let next = || match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(BuildWorkerMessage::Output(text)) => text,
        _ => panic!("no output"),
    };
    assert_eq!(next(), "ready\n");
    save.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        next(),
        format!("got {}\n", crate::project::CODE_RELOAD_SAVE_COMMAND)
    );
    assert!(worker.join().unwrap().unwrap().unwrap().success());
}

#[test]
fn code_reload_saves_a_running_game_and_resumes_after_a_failed_build() {
    let mut build = EditorBuildState {
        running: true,
        play_started: Some(std::time::Instant::now()),
        built_after: Some(Duration::from_secs(1)),
        ..EditorBuildState::default()
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    *build.receiver.get_mut().unwrap() = Some(receiver);
    // A running game is asked to save, not killed.
    build.request_code_reload();
    assert!(build.save_state.load(std::sync::atomic::Ordering::Relaxed));
    assert!(!build.stop.load(std::sync::atomic::Ordering::Relaxed));
    sender
        .send(BuildWorkerMessage::Output(format!(
            "{} could not save the scene: no\n",
            crate::project::CODE_RELOAD_CLEAN_MARKER
        )))
        .unwrap();
    sender
        .send(BuildWorkerMessage::Finished(BuildFinished {
            success: true,
            output: "exited".into(),
        }))
        .unwrap();
    assert_eq!(build.poll(), Some(true));
    assert!(build.take_restart_ready());
    assert!(build.code_reload);
    let notices = build.take_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].0, ConsoleLevel::Warning);
    assert!(notices[0].2.contains("could not save the scene"));

    // The rebuilt code fails: the saved scene waits for the next Play.
    build.code_reload = false;
    build.resuming = true;
    build.running = true;
    build.play_started = Some(std::time::Instant::now());
    build.built_after = None;
    let (sender, receiver) = std::sync::mpsc::channel();
    *build.receiver.get_mut().unwrap() = Some(receiver);
    sender
        .send(BuildWorkerMessage::Finished(BuildFinished {
            success: false,
            output: "failed".into(),
        }))
        .unwrap();
    assert_eq!(build.poll(), Some(false));
    assert!(build.code_reload);
    assert!(build.take_notices()[0].2.contains("press Play to continue"));

    // While Cargo still builds, Reload Code restarts the build.
    build.running = true;
    build.request_code_reload();
    assert!(build.stop.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn editor_scene_restores_camera_pose_and_selection_after_reload() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-meta-{}", uuid::Uuid::new_v4()));
    let path = folder.join("main.rscene");
    let mut app = App::new();
    app.insert_resource(AssetServer::default());
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let cube = app.spawn((
        SceneId(uuid::Uuid::new_v4()),
        Name("Cube".into()),
        Transform::default(),
    ));
    let lamp = app.spawn((
        SceneId(uuid::Uuid::new_v4()),
        Name("Lamp".into()),
        Transform::default(),
    ));
    let world = app.world_mut();
    // Spawned like the editor binary does: no `SceneId`, so it is not saved.
    let camera = world
        .spawn((
            Name("Editor Camera".into()),
            Transform::new([1.0, 2.0, 3.0]),
        ))
        .id();
    let mut state = EditorState {
        editor_camera: Some(camera),
        selected: Some(lamp),
        selection: vec![cube, lamp],
        ..Default::default()
    };
    world.resource_mut::<EditorFlyCamera>().orbit_distance = 7.0;
    save_editor_scene(world, &state, &path).unwrap();

    world.get_mut::<Transform>(camera).unwrap().position = [0.0; 3];
    world.resource_mut::<EditorFlyCamera>().orbit_distance = 1.0;
    assert_eq!(load_editor_scene(world, &mut state, &path).unwrap(), 2);

    assert_eq!(
        world.get::<Transform>(camera).unwrap().position,
        [1.0, 2.0, 3.0]
    );
    assert_eq!(world.resource::<EditorFlyCamera>().orbit_distance, 7.0);
    let names: Vec<_> = state
        .selection
        .iter()
        .map(|&entity| world.get::<Name>(entity).unwrap().0.clone())
        .collect();
    assert_eq!(names, ["Lamp", "Cube"], "active object stays first");
    assert_eq!(state.selected, state.selection.first().copied());
    assert!(world.get_entity(cube).is_err(), "reload respawns entities");

    std::fs::remove_file(folder.join("main.rscene.editor.json")).unwrap();
    load_editor_scene(world, &mut state, &path).unwrap();
    assert!(state.selection.is_empty() && state.selected.is_none());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn added_models_keep_their_node_tree_under_one_saved_root() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-model-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    // One triangle without normals, so the import has to make flat ones.
    let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    std::fs::write(folder.join("tri.bin"), &positions).unwrap();
    let path = folder.join("crate.gltf");
    std::fs::write(
        &path,
        r#"{
          "asset": {"version": "2.0"},
          "buffers": [{"uri": "tri.bin", "byteLength": 36}],
          "bufferViews": [{"buffer": 0, "byteLength": 36}],
          "accessors": [{"bufferView": 0, "componentType": 5126,
            "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0]}],
          "cameras": [{"type": "perspective",
            "perspective": {"yfov": 1.0, "znear": 0.1}}],
          "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
          "nodes": [
            {"name": "Body", "mesh": 0, "children": [1],
             "translation": [0, 2, 0]},
            {"name": "Body", "camera": 0}],
          "scenes": [{"nodes": [0]}]
        }"#,
    )
    .unwrap();
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let world = app.world_mut();

    let first = add_model_to_scene(world, &path).unwrap();
    let second = add_model_to_scene(world, &path).unwrap();

    assert_eq!(world.get::<Name>(first).unwrap().0, "crate");
    assert_eq!(world.get::<Name>(second).unwrap().0, "crate 2");
    let mut objects = world.query::<(Entity, &Name, Option<&Parent>)>();
    let objects = objects
        .iter(world)
        .map(|(entity, name, parent)| {
            (entity, name.0.clone(), parent.map(|p| p.0))
        })
        .collect::<Vec<_>>();
    // Two roots, and a body and a camera under each; every name is unique.
    assert_eq!(objects.len(), 6);
    let mut names = objects.iter().map(|(_, name, _)| name).collect::<Vec<_>>();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 6);
    for root in [first, second] {
        let body = objects
            .iter()
            .find(|(_, _, parent)| *parent == Some(root))
            .unwrap()
            .0;
        assert_eq!(
            world.get::<Transform>(body).unwrap().position,
            [0.0, 2.0, 0.0]
        );
        let camera = objects
            .iter()
            .find(|(_, _, parent)| *parent == Some(body))
            .unwrap()
            .0;
        assert!(world.get::<Camera>(camera).is_some());
        let mesh = world.get::<MeshRenderer>(body).unwrap().mesh;
        let assets = world.resource::<AssetServer>();
        for vertex in &assets.meshes.get(mesh).unwrap().vertices {
            assert_eq!(vertex.normal, [0.0, 0.0, 1.0]);
        }
    }
    // Every object saves with the scene.
    let mut ids = world.query::<&SceneId>();
    assert_eq!(ids.iter(world).count(), 6);
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn scenes_instance_as_one_linked_root_but_not_into_themselves() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-instance-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let prefab = folder.join("coin.rscene");
    let mut source = App::new();
    source.add_plugin(crate::AssetPlugin).unwrap();
    source.spawn((SceneId::new(), Name("Coin".into()), Transform::default()));
    crate::runtime::save_scene(source.world_mut(), &prefab, "Coin").unwrap();

    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let world = app.world_mut();
    let open = folder.join("main.rscene");
    let root = instance_scene_in_scene(world, &prefab, Some(&open)).unwrap();
    assert_eq!(world.get::<Name>(root).unwrap().0, "coin");
    assert!(world.get::<crate::runtime::SceneInstance>(root).is_some());
    let mut members =
        world.query::<(&Name, &Parent, &crate::runtime::InstanceMember)>();
    let members = members
        .iter(world)
        .map(|(name, parent, _)| (name.0.clone(), parent.0))
        .collect::<Vec<_>>();
    assert_eq!(members, vec![("Coin".to_owned(), root)]);

    // Hierarchy instance actions reload the scene and keep the object.
    let mut coins = world
        .query_filtered::<&mut Transform, bevy_ecs::query::With<crate::runtime::InstanceMember>>(
        );
    coins.single_mut(world).unwrap().position = [5.0, 0.0, 0.0];
    let root =
        edit_scene_instance(world, root, crate::runtime::InstanceEdit::Revert)
            .unwrap()
            .unwrap();
    let mut coins = world
        .query_filtered::<&Transform, bevy_ecs::query::With<crate::runtime::InstanceMember>>();
    assert_eq!(coins.single(world).unwrap().position, [0.0; 3]);
    let root = edit_scene_instance(
        world,
        root,
        crate::runtime::InstanceEdit::UnpackCompletely,
    )
    .unwrap()
    .unwrap();
    assert!(world.get::<crate::runtime::SceneInstance>(root).is_none());
    let mut members = world.query::<&crate::runtime::InstanceMember>();
    assert_eq!(members.iter(world).count(), 0);

    let error =
        instance_scene_in_scene(world, &prefab, Some(&prefab)).unwrap_err();
    assert!(error.contains("itself"), "{error}");

    // New Variant picks a free name and the variant instances its base.
    let first = new_scene_variant(&prefab).unwrap();
    let second = new_scene_variant(&prefab).unwrap();
    assert_eq!(first, folder.join("coin_variant.rscene"));
    assert_eq!(second, folder.join("coin_variant_2.rscene"));
    let variant = crate::runtime::read_scene_document(&first).unwrap();
    assert_eq!(variant.entities.len(), 1);
    assert_eq!(
        variant.entities[0].components
            [crate::runtime::SCENE_INSTANCE_COMPONENT],
        r#"{"source":"coin.rscene"}"#
    );
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn inspector_shows_gpu_sync_mode_and_readback_age() {
    use crate::runtime::{
        FrameTime, GpuStateMirror, PhysicsBody, PhysicsSyncMode,
        SimulationClass,
    };
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let entity = app.spawn((
        Name("Crate".into()),
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
        PhysicsSyncMode::SelectedState,
        GpuStateMirror {
            tick: 3,
            transform: Transform::default(),
            linear_velocity: [0.0; 3],
            angular_velocity: [0.0; 3],
            custom_values: None,
        },
    ));
    app.world_mut().resource_mut::<FrameTime>().fixed_tick = 5;
    {
        let mut state = app.world_mut().resource_mut::<EditorState>();
        state.selected = Some(entity);
        state.dock_layout = EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Inspector,
        };
    }
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });

    let mut texts = String::new();
    for clipped in &output.shapes {
        if let egui::Shape::Text(text) = &clipped.shape {
            texts.push_str(text.galley.text());
            texts.push('\n');
        }
    }
    assert!(texts.contains("Selected State"), "{texts}");
    assert!(texts.contains("tick 3 (2 ticks old)"), "{texts}");
    // The dedicated row replaces the raw registered-component section.
    assert!(!texts.contains("rusting.physics_sync"), "{texts}");
}

#[test]
fn inspector_shows_the_auto_simulation_decision() {
    use crate::runtime::{
        AllocationDecision, AllocationReason, AutoSimulation, PhysicsBody,
        SimulationClass,
    };
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let entity = app.spawn((
        Name("Crate".into()),
        Transform::default(),
        PhysicsBody {
            simulation: SimulationClass::Gpu,
            ..PhysicsBody::default()
        },
        AutoSimulation {
            decision: Some(AllocationDecision {
                class: SimulationClass::Gpu,
                reason: AllocationReason::ManyBodies,
            }),
        },
    ));
    {
        let mut state = app.world_mut().resource_mut::<EditorState>();
        state.selected = Some(entity);
        state.dock_layout = EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Inspector,
        };
    }
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });

    let mut texts = String::new();
    for clipped in &output.shapes {
        if let egui::Shape::Text(text) = &clipped.shape {
            texts.push_str(text.galley.text());
            texts.push('\n');
        }
    }
    assert!(texts.contains("Auto CPU/GPU"), "{texts}");
    assert!(texts.contains("Gpu (ManyBodies)"), "{texts}");
    assert!(
        texts.contains("On the GPU: at least 256 flexible auto bodies."),
        "{texts}"
    );
    assert!(!texts.contains("rusting.auto_simulation"), "{texts}");
}

#[test]
fn queued_shortcuts_delete_and_undo_like_the_menu() {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let entity = app.spawn((
        Name("Crate".into()),
        Transform::default(),
        crate::runtime::SceneId(uuid::Uuid::new_v4()),
    ));
    {
        let mut state = app.world_mut().resource_mut::<EditorState>();
        state.selected = Some(entity);
        state.selection = vec![entity];
    }
    let count = |app: &mut App| {
        let world = app.world_mut();
        world.query::<&Name>().iter(world).count()
    };
    let frame = |app: &mut App, command| {
        app.world_mut()
            .resource_mut::<EditorCommandQueue>()
            .0
            .push(command);
        let context = Context::default();
        let _ = context.run(egui::RawInput::default(), |context| {
            draw_editor_view(app.world_mut(), context);
        });
    };
    frame(&mut app, EditorAction::DeleteSelection);
    assert_eq!(
        count(&mut app),
        0,
        "{:?}",
        app.world().resource::<EditorState>().scene_message
    );
    assert!(app.world().resource::<EditorCommandQueue>().0.is_empty());
    frame(&mut app, EditorAction::Undo);
    assert_eq!(count(&mut app), 1);

    let entity = {
        let world = app.world_mut();
        world
            .query::<(bevy_ecs::entity::Entity, &Name)>()
            .iter(world)
            .next()
            .unwrap()
            .0
    };
    app.world_mut().resource_mut::<EditorState>().selected = Some(entity);
    frame(&mut app, EditorAction::RenameSelection);
    let state = app.world().resource::<EditorState>();
    assert_eq!(state.rename_target, Some(entity));
    assert_eq!(state.rename_draft, "Crate");
}

#[test]
fn shortcuts_area_lists_every_action_with_its_key() {
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    app.world_mut().resource_mut::<EditorState>().dock_layout =
        EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Shortcuts,
        };
    app.world_mut().resource_mut::<EditorShortcuts>().capturing =
        Some(ShortcutAction::Editor(EditorAction::Redo));
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });
    let mut texts = String::new();
    for clipped in &output.shapes {
        if let egui::Shape::Text(text) = &clipped.shape {
            texts.push_str(text.galley.text());
            texts.push('\n');
        }
    }
    for expected in [
        "Keyboard Shortcuts",
        "Undo",
        "Ctrl+Z",
        "Press a key...",
        // Redo's second key, beside its waiting first one.
        "Ctrl+Y",
        "Fly Camera",
        "Numpad0",
    ] {
        assert!(texts.contains(expected), "{expected} missing from {texts}");
    }
}

#[test]
fn project_area_shows_lod_groups_and_the_resolved_auto_quality() {
    use crate::rendering::scene_renderer::{Capability, RendererCapabilities};
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    app.world_mut().insert_resource(AssetServer::default());
    app.world_mut().insert_resource(RendererCapabilities {
        device_name: String::new(),
        integrated_gpu: true,
        device_local_bytes: 16 << 30,
        multi_draw_indirect: Capability::default(),
        draw_indirect_count: Capability::default(),
        bindless_textures: Capability::default(),
        memory_budget: Capability::default(),
        sampler_anisotropy: Capability::default(),
        timestamp_queries: false,
        msaa_samples: 1,
        msaa_2x: false,
    });
    app.world_mut().resource_mut::<EditorState>().dock_layout =
        EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Project,
        };
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });
    let mut texts = String::new();
    for clipped in &output.shapes {
        if let egui::Shape::Text(text) = &clipped.shape {
            texts.push_str(text.galley.text());
            texts.push('\n');
        }
    }
    // Auto picks Eco on an integrated GPU.
    for expected in ["LOD GROUPS", "Resolved quality", "Eco"] {
        assert!(texts.contains(expected), "{expected} missing from {texts}");
    }
}

#[test]
fn font_scale_multiplies_every_default_text_size() {
    let context = egui::Context::default();
    super::configure_editor_style(&context);
    super::apply_font_scale(&context, 1.5);
    super::apply_font_scale(&context, 1.3);
    let defaults = egui::Style::default().text_styles;
    context
        .style()
        .text_styles
        .iter()
        .for_each(|(style, font)| {
            assert!((font.size - defaults[style].size * 1.3).abs() < 1e-4);
        });
}

#[test]
fn scene_area_draws_the_offscreen_view_image_over_its_viewport() {
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    app.world_mut().resource_mut::<EditorState>().dock_layout =
        EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Scene,
        };
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });
    let image = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(rect)
                if rect.fill_texture_id() == super::SCENE_VIEW_TEXTURE =>
            {
                Some(rect.rect)
            }
            egui::Shape::Mesh(mesh)
                if mesh.texture_id == super::SCENE_VIEW_TEXTURE =>
            {
                Some(egui::Rect::from_points(
                    &mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>(),
                ))
            }
            _ => None,
        })
        .expect("scene view image is drawn");
    let viewport = *app.world().resource::<EditorViewport>();
    assert!(viewport.valid);
    // One point is one pixel here; the viewport rounds outward.
    assert_eq!(
        viewport.offset,
        [image.min.x.floor() as u32, image.min.y.floor() as u32]
    );
    assert_eq!(viewport.extent[0], image.width().ceil() as u32);
}

#[test]
fn reparenting_a_selection_keeps_world_positions_and_nested_children() {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let scene_object = |app: &mut App, name: &str, position| {
        app.spawn((
            Name(name.into()),
            Transform::new(position),
            crate::runtime::SceneId(uuid::Uuid::new_v4()),
        ))
    };
    let target = scene_object(&mut app, "Target", [5.0, 0.0, 0.0]);
    let a = scene_object(&mut app, "A", [1.0, 2.0, 0.0]);
    let b = scene_object(&mut app, "B", [0.0, 0.0, 3.0]);
    let b_child = scene_object(&mut app, "B Child", [0.0, 1.0, 0.0]);
    app.world_mut()
        .entity_mut(b_child)
        .insert(crate::runtime::Parent(b));
    crate::runtime::propagate_transforms(app.world_mut());

    let moved = super::hierarchy::reparent_entities(
        app.world_mut(),
        &[a, b, b_child],
        Some(target),
    )
    .unwrap()
    .unwrap();
    crate::runtime::propagate_transforms(app.world_mut());
    let world = app.world_mut();
    let by_name = |world: &mut World, name: &str| {
        let mut query = world.query::<(Entity, &Name)>();
        query
            .iter(world)
            .find_map(|(entity, found)| (found.0 == name).then_some(entity))
            .unwrap()
    };
    let target = by_name(world, "Target");
    assert_eq!(world.get::<Name>(moved).unwrap().0, "A");
    for (name, parent, position) in [
        ("A", target, [1.0, 2.0, 0.0]),
        ("B", target, [0.0, 0.0, 3.0]),
        ("B Child", by_name(world, "B"), [0.0, 1.0, 3.0]),
    ] {
        let entity = by_name(world, name);
        assert_eq!(
            world.get::<crate::runtime::Parent>(entity).unwrap().0,
            parent
        );
        let global = world
            .get::<crate::runtime::GlobalTransform>(entity)
            .unwrap();
        for (actual, expected) in global.matrix[3].iter().zip(position) {
            assert!((actual - expected).abs() < 1e-5, "{name}");
        }
    }
    let target_child = by_name(world, "B");
    assert!(super::hierarchy::reparent_entities(
        world,
        &[target],
        Some(target_child)
    )
    .is_err());
}

#[test]
fn registered_inspectors_draw_and_edit_their_component() {
    #[derive(
        bevy_ecs::component::Component,
        serde::Serialize,
        serde::Deserialize,
        Default,
        Debug,
        PartialEq,
    )]
    struct Health {
        points: u32,
    }
    crate::reflect! {
        struct Health {
            points: u32,
        }
    }
    fn draw_health(ui: &mut egui::Ui, health: &mut Health) -> bool {
        ui.label(format!("Typed health {}", health.points));
        health.points += 1;
        true
    }
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    app.register_scene_component::<Health>("game.health")
        .unwrap();
    app.world_mut()
        .resource_mut::<super::InspectorRegistry>()
        .register("game.health", draw_health);
    let entity = app.spawn((
        Name("Hero".into()),
        Transform::default(),
        Health { points: 7 },
    ));
    {
        let mut state = app.world_mut().resource_mut::<EditorState>();
        state.selected = Some(entity);
        state.dock_layout = EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Inspector,
        };
    }
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        draw_editor_view(app.world_mut(), context);
    });
    let texts = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(texts.contains("Typed health 7"), "{texts}");
    // The generic JSON editor would show the field name as a row label.
    assert!(!texts.contains("points"), "{texts}");
    assert_eq!(
        app.world().get::<Health>(entity),
        Some(&Health { points: 8 })
    );
}

#[test]
fn dropping_an_image_on_hierarchy_rows_and_texture_slots_assigns_it() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-drop-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let image_path = folder.join("bricks.png");
    image::RgbaImage::from_pixel(4, 4, image::Rgba([200, 20, 20, 255]))
        .save(&image_path)
        .unwrap();

    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let (mesh, material) = {
        let assets = app.world().resource::<AssetServer>();
        (assets.fallback_mesh, assets.fallback_material)
    };
    let entity = app.spawn((
        Name("Crate".into()),
        Transform::default(),
        SceneId(uuid::Uuid::new_v4()),
        MeshRenderer {
            mesh,
            material,
            cast_shadows: true,
            receive_shadows: true,
        },
    ));
    let texture_of = |world: &World, slot: usize| {
        let material = world.get::<MeshRenderer>(entity).unwrap().material;
        let mut material = world
            .resource::<AssetServer>()
            .materials
            .get(material)
            .cloned()
            .unwrap();
        *super::inspector::texture_slots(&mut material)[slot]
    };

    // Releases an image drag over the first text shape reading `target`.
    let drop_on = |app: &mut App, panel: EditorPanel, target: &str| {
        {
            let mut state = app.world_mut().resource_mut::<EditorState>();
            state.selected = Some(entity);
            state.dock_layout = EditorDockNode::Area { id: 1, panel };
        }
        let context = Context::default();
        let output = context.run(egui::RawInput::default(), |context| {
            draw_editor_view(app.world_mut(), context);
        });
        let position = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) if text.galley.text() == target => {
                    Some(text.pos + text.galley.rect.center().to_vec2())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {target} text"));
        egui::DragAndDrop::set_payload(
            &context,
            super::assets_panel::ImageDrag(image_path.clone()),
        );
        let input = egui::RawInput {
            events: vec![
                egui::Event::PointerMoved(position),
                egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                },
            ],
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            draw_editor_view(app.world_mut(), context);
        });
    };

    drop_on(&mut app, EditorPanel::Hierarchy, "Crate");
    assert!(texture_of(app.world(), 0).is_some());
    assert!(texture_of(app.world(), 1).is_none());
    drop_on(&mut app, EditorPanel::Inspector, "Normal Map");
    assert!(texture_of(app.world(), 1).is_some());
    // Each drop is its own Undo step.
    assert_eq!(app.world().resource::<EditorHistory>().undo.len(), 2);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn console_groups_repeats_filters_and_records_status_lines() {
    let mut app = App::new();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    {
        let mut console = app.world_mut().resource_mut::<EditorConsole>();
        console.push_from(ConsoleLevel::Warning, "Assets", "Texture missing");
        console.push_from(ConsoleLevel::Warning, "Assets", "Texture missing");
        console.push_from(ConsoleLevel::Info, "Build", "Build finished");
        assert_eq!(console.entries().len(), 2);
        assert_eq!(console.entries()[0].count, 2);
    }
    {
        let mut state = app.world_mut().resource_mut::<EditorState>();
        state.dock_layout = EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Console,
        };
        state.scene_message = Some("Save As failed: disk full".into());
    }
    let texts = |app: &mut App| {
        let output = Context::default().run(egui::RawInput::default(), |c| {
            draw_editor_view(app.world_mut(), c);
        });
        let mut texts = String::new();
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                texts.push_str(text.galley.text());
                texts.push('\n');
            }
        }
        texts
    };
    // A status line is copied into the Console once, with a guessed level.
    let _ = texts(&mut app);
    let first = texts(&mut app);
    assert!(first.contains("Warnings 2"), "{first}");
    assert!(first.contains("Errors 1"), "{first}");
    assert!(first.contains("x2"), "{first}");
    assert!(first.contains("disk full"), "{first}");

    app.world_mut().resource_mut::<EditorState>().scene_message =
        Some("Undo failed: no snapshot".into());
    let _ = texts(&mut app);
    let last = app
        .world()
        .resource::<EditorConsole>()
        .entries()
        .last()
        .cloned();
    assert_eq!(
        last.map(|entry| (entry.level, entry.source)),
        Some((ConsoleLevel::Error, "Scene"))
    );

    app.world_mut().resource_mut::<EditorConsole>().filter = "build".into();
    let filtered = texts(&mut app);
    assert!(filtered.contains("Build finished"), "{filtered}");
    assert!(!filtered.contains("Texture missing"), "{filtered}");
    let mut console = app.world_mut().resource_mut::<EditorConsole>();
    console.filter.clear();
    console.hidden[ConsoleLevel::Info as usize] = true;
    let hidden = texts(&mut app);
    assert!(!hidden.contains("Build finished"), "{hidden}");
    assert!(hidden.contains("Texture missing"), "{hidden}");
}

#[test]
fn preview_edits_while_paused_drive_the_simulation_until_stop() {
    use crate::runtime::{
        Collider, PhysicsBody, RigidBody, RigidBodyKind, TimeControl,
    };
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let ball = app.world_mut().spawn((
        SceneId::default(),
        Name("Ball".into()),
        Transform::new([0.0, 10.0, 0.0]),
        PhysicsBody::default(),
        RigidBody {
            kind: RigidBodyKind::Dynamic,
            ..RigidBody::default()
        },
        Collider::default(),
    ));
    let ball = ball.id();
    let mut state = EditorState::default();
    let mut history = EditorHistory::default();
    let frames = |app: &mut App, count| {
        for _ in 0..count {
            app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
        }
    };
    let position =
        |app: &App| app.world().get::<Transform>(ball).unwrap().position;

    view::start_embedded_preview(app.world_mut(), &mut state, &mut history);
    frames(&mut app, 30);
    let fallen = position(&app);
    assert!(fallen[1] < 9.0, "the ball falls while playing: {fallen:?}");

    view::set_embedded_preview_paused(app.world_mut(), &mut state, true);
    assert_eq!(state.mode, EditorMode::Paused);
    frames(&mut app, 10);
    assert_eq!(position(&app), fallen, "paused time does not move it");

    // Edit through the undo path while paused, then single-step.
    remember_scene_before_edit(app.world_mut(), &mut history).unwrap();
    app.world_mut().get_mut::<Transform>(ball).unwrap().position =
        [3.0, 20.0, 0.0];
    assert_eq!(history.undo.len(), 1);
    app.world_mut().resource_mut::<TimeControl>().step();
    frames(&mut app, 1);
    let stepped = position(&app);
    assert_eq!(stepped[0], 3.0);
    assert!(stepped[1] < 20.0 && stepped[1] > 19.0, "{stepped:?}");
    frames(&mut app, 5);
    assert_eq!(position(&app), stepped, "one step, then paused again");

    view::set_embedded_preview_paused(app.world_mut(), &mut state, false);
    frames(&mut app, 10);
    assert!(position(&app)[1] < stepped[1], "resumes from the edit");

    view::stop_embedded_preview(app.world_mut(), &mut state, &mut history);
    assert_eq!(state.mode, EditorMode::Edit);
    assert!(history.undo.is_empty(), "preview edits leave no history");
    let world = app.world_mut();
    let mut query = world.query::<(&Name, &Transform)>();
    let (_, authored) = query
        .iter(world)
        .find(|(name, _)| name.0 == "Ball")
        .unwrap();
    assert_eq!(authored.position, [0.0, 10.0, 0.0]);
}

#[test]
fn outside_scene_patch_reloads_under_undo_or_conflicts_with_unsaved_edits() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-patch-{}", uuid::Uuid::new_v4()));
    let path = folder.join("main.rscene");
    let mut app = App::new();
    app.insert_resource(AssetServer::default());
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let id = uuid::Uuid::new_v4();
    let cube =
        app.spawn((SceneId(id), Name("Cube".into()), Transform::default()));
    let world = app.world_mut();
    let mut state = EditorState {
        selected: Some(cube),
        selection: vec![cube],
        ..Default::default()
    };
    let mut history = EditorHistory::default();
    save_editor_scene(world, &state, &path).unwrap();
    let rename = |world: &mut World, name: &str| {
        let patch = serde_json::from_value(serde_json::json!({"operations": [
            {"op": "set", "id": id, "path": "/name", "value": name}]}))
        .unwrap();
        crate::scene_patch::patch_scene_file(&path, &patch, false).unwrap();
        // Coarse file clocks can give two writes one timestamp.
        world.resource_mut::<SceneFileRevision>().modified = None;
    };
    let name = |world: &mut World| {
        let mut query = world.query::<(&SceneId, &Name)>();
        query
            .iter(world)
            .find(|(scene, _)| scene.0 == id)
            .unwrap()
            .1
             .0
            .clone()
    };

    // Clean scene: the outside change loads behind one Undo snapshot.
    rename(world, "Agent");
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(name(world), "Agent");
    assert_eq!(history.undo.len(), 1);
    assert!(!state.scene_dirty);
    assert_eq!(
        state
            .selected
            .and_then(|e| world.get::<SceneId>(e))
            .unwrap()
            .0,
        id
    );
    assert!(state
        .scene_message
        .as_deref()
        .unwrap()
        .contains("0 added, 1 changed, 0 removed"));
    assert_eq!(
        state.selection.len(),
        1,
        "the changed entity is highlighted"
    );
    let journal = world.resource::<agent_panel::AgentJournal>();
    assert_eq!(journal.entries.len(), 1, "the outside write is journaled");
    assert_eq!(journal.entries[0].ids, vec![id]);
    let before = history.undo.back().unwrap().clone();
    crate::runtime::load_scene_document(world, &before, SceneLoadMode::Replace)
        .unwrap();
    assert_eq!(
        name(world),
        "Cube",
        "the snapshot holds the scene before the patch"
    );

    // Unsaved edits: nothing reloads, and Save refuses once.
    save_editor_scene(world, &state, &path).unwrap();
    state.scene_dirty = true;
    rename(world, "Agent Again");
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(name(world), "Cube");
    assert!(state
        .scene_message
        .as_deref()
        .unwrap()
        .contains("unsaved edits"));
    assert!(matches!(
        save_editor_scene(world, &state, &path),
        Err(crate::runtime::SceneIoError::Conflict(_))
    ));
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("Agent Again"));
    save_editor_scene(world, &state, &path).unwrap();
    assert!(!std::fs::read_to_string(&path)
        .unwrap()
        .contains("Agent Again"));

    // Paused agent edits: a clean scene queues the write as pending.
    state.scene_dirty = false;
    world.resource_mut::<agent_panel::AgentJournal>().paused = true;
    rename(world, "Paused");
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(name(world), "Cube", "a paused write does not reload");
    let pending = world
        .resource::<agent_panel::AgentJournal>()
        .pending
        .clone()
        .expect("the write is pending");
    assert_eq!(pending.summary, "0 added, 1 changed, 0 removed");
    assert_eq!(pending.ids, vec![id]);
    // Reject keeps the scene and the file.
    world
        .resource_mut::<agent_panel::AgentJournal>()
        .reject_requested = true;
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(name(world), "Cube");
    assert!(world
        .resource::<agent_panel::AgentJournal>()
        .pending
        .is_none());
    assert!(std::fs::read_to_string(&path).unwrap().contains("Paused"));
    // Accept applies only the reviewed revision: a write that lands after
    // the pending row appeared becomes the new pending entry.
    rename(world, "Reviewed");
    reload_external_scene_change(world, &mut state, &mut history);
    rename(world, "Accepted");
    world
        .resource_mut::<agent_panel::AgentJournal>()
        .accept_requested = true;
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(name(world), "Cube", "an unreviewed write is not applied");
    assert!(world
        .resource::<agent_panel::AgentJournal>()
        .pending
        .is_some());
    // Accept applies the pending write behind one Undo snapshot.
    world
        .resource_mut::<agent_panel::AgentJournal>()
        .accept_requested = true;
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(name(world), "Accepted");
    let before = history.undo.back().unwrap().clone();
    crate::runtime::load_scene_document(world, &before, SceneLoadMode::Replace)
        .unwrap();
    assert_eq!(name(world), "Cube", "Undo reverts the accepted write");
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_pending_outside_write_is_accepted_one_entity_at_a_time() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-accept-{}", uuid::Uuid::new_v4()));
    let path = folder.join("main.rscene");
    let mut app = App::new();
    app.insert_resource(AssetServer::default());
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let (cube, lamp) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    let shelf = uuid::Uuid::new_v4();
    app.spawn((SceneId(cube), Name("Cube".into()), Transform::default()));
    app.spawn((SceneId(shelf), Name("Shelf".into()), Transform::default()));
    let world = app.world_mut();
    let mut state = EditorState::default();
    let mut history = EditorHistory::default();
    save_editor_scene(world, &state, &path).unwrap();
    // An unsaved GUI edit is not part of the agent's pending change.
    let chair = uuid::Uuid::new_v4();
    world.spawn((SceneId(chair), Name("Chair".into()), Transform::default()));
    state.scene_dirty = true;
    world.resource_mut::<agent_panel::AgentJournal>().paused = true;
    let patch = serde_json::from_value(serde_json::json!({"operations": [
        {"op": "set", "id": cube, "path": "/name", "value": "Agent"},
        {"op": "create", "entity": {"id": lamp, "name": "Lamp"}},
        {"op": "delete", "id": shelf}]}))
    .unwrap();
    crate::scene_patch::patch_scene_file(&path, &patch, false).unwrap();
    world.resource_mut::<SceneFileRevision>().modified = None;
    reload_external_scene_change(world, &mut state, &mut history);
    let names = |world: &mut World| {
        let mut query = world.query::<(&SceneId, &Name)>();
        let mut names = query
            .iter(world)
            .map(|(id, name)| (id.0, name.0.clone()))
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let journal = world.resource::<agent_panel::AgentJournal>();
    assert_eq!(
        journal.pending.as_ref().unwrap().summary,
        "1 added, 1 changed, 1 removed"
    );
    assert_eq!(journal.pending_removed, vec![(shelf, "Shelf".into())]);
    assert!(journal
        .pending_fields
        .contains(&(cube, "/name: \"Cube\" -> \"Agent\"".into())));

    // Accept the new lamp only: the cube keeps its name and stays pending.
    world
        .resource_mut::<agent_panel::AgentJournal>()
        .accept_entity = Some(lamp);
    reload_external_scene_change(world, &mut state, &mut history);
    let mut expected = vec![
        (cube, "Cube".to_owned()),
        (lamp, "Lamp".into()),
        (chair, "Chair".into()),
        (shelf, "Shelf".into()),
    ];
    expected.sort();
    assert_eq!(names(world), expected);
    assert!(state.scene_dirty);
    let journal = world.resource::<agent_panel::AgentJournal>();
    assert_eq!(journal.pending.as_ref().unwrap().ids, vec![cube]);
    assert!(journal.pending_fields.iter().all(|(id, _)| *id == cube));
    // Undo's snapshot is the scene before that Accept.
    let before = history.undo.back().unwrap().clone();
    assert_eq!(before.entities.len(), 3);

    // Accepting a removal deletes the entity from the editor scene.
    world
        .resource_mut::<agent_panel::AgentJournal>()
        .accept_entity = Some(shelf);
    reload_external_scene_change(world, &mut state, &mut history);
    assert_eq!(names(world).len(), 3, "the shelf is gone");
    assert!(world
        .resource::<agent_panel::AgentJournal>()
        .pending_removed
        .is_empty());

    // Accepting the last entity clears the pending row.
    world
        .resource_mut::<agent_panel::AgentJournal>()
        .accept_entity = Some(cube);
    reload_external_scene_change(world, &mut state, &mut history);
    let mut expected = vec![
        (cube, "Agent".to_owned()),
        (lamp, "Lamp".into()),
        (chair, "Chair".into()),
    ];
    expected.sort();
    assert_eq!(names(world), expected);
    assert!(world
        .resource::<agent_panel::AgentJournal>()
        .pending
        .is_none());
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn play_reports_first_frame_time_and_per_file_reload_errors() {
    let mut build = EditorBuildState {
        running: true,
        play_started: Some(std::time::Instant::now()),
        ..EditorBuildState::default()
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    *build.receiver.get_mut().unwrap() = Some(receiver);
    for text in [
        format!("\n{BUILD_PASSED} Starting native game...\n"),
        "hot reload failed: failed to load `assets/crate.rtexture`: bad\n"
            .into(),
        format!("{} 250 ms\n", crate::project::FIRST_FRAME_MARKER),
    ] {
        sender.send(BuildWorkerMessage::Output(text)).unwrap();
    }
    assert_eq!(build.poll(), None);
    let notices = build.take_notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0].1, "Assets");
    assert!(notices[0].2.contains("assets/crate.rtexture"));
    assert_eq!(notices[1].1, "Play");
    assert!(notices[1].2.contains("game startup 250 ms"));

    // A failed build reports each error with its file; a failed game after
    // a good build does not repeat the build output as errors.
    build.built_after = None;
    sender
        .send(BuildWorkerMessage::Output(
            "src/main.rs:9:5: error[E0425]: cannot find value `x`\n".into(),
        ))
        .unwrap();
    sender
        .send(BuildWorkerMessage::Finished(BuildFinished {
            success: false,
            output: "\nCargo task failed.\n".into(),
        }))
        .unwrap();
    assert_eq!(build.poll(), Some(false));
    let notices = build.take_notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].1, "Rust");
    assert!(notices[0].2.starts_with("src/main.rs:9: error[E0425]"));
}

#[test]
fn replacing_an_imported_asset_previews_then_writes_on_confirm() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-replace-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(folder.join("assets")).unwrap();
    let png = |name: &str, value: u8| {
        let path = folder.join(name);
        image::RgbaImage::from_pixel(4, 4, image::Rgba([value, 0, 0, 255]))
            .save(&path)
            .unwrap();
        path
    };
    let imported = crate::asset_import::import_asset(
        &folder,
        &png("old.png", 10),
        Path::new(""),
        &crate::asset_import::AssetProvenance::default(),
        &crate::asset_import::ImportSettings::default(),
        false,
    )
    .unwrap();
    let target = folder.join(&imported.path);
    let replacement = png("new.png", 250);
    let red = |path: &Path| image::open(path).unwrap().to_rgba8()[(0, 0)][0];

    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    {
        let world = app.world_mut();
        let mut state = world.resource_mut::<EditorState>();
        state.project_root = folder.display().to_string();
        state.dock_layout = EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Assets,
        };
        world.resource_mut::<EditorAssetState>().replace_target =
            Some(target.clone());
        let chosen = replacement.clone();
        world
            .resource_mut::<FileDialogs>()
            .open(DialogPurpose::ReplaceAsset, async move { vec![chosen] });
    }
    let context = Context::default();
    let frame = |app: &mut App, input: egui::RawInput| {
        context.run(input, |context| draw_editor_view(app.world_mut(), context))
    };
    frame(&mut app, egui::RawInput::default());
    for _ in 0..200 {
        if app
            .world()
            .resource::<EditorAssetState>()
            .replace_preview
            .is_some()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        frame(&mut app, egui::RawInput::default());
    }
    let preview = app
        .world()
        .resource::<EditorAssetState>()
        .replace_preview
        .clone()
        .expect("the chosen file is previewed");
    assert_eq!(preview.report.as_ref().unwrap().id, imported.id);
    assert_eq!(red(&target), 10, "a preview writes nothing");

    // egui lays a new modal out invisibly on its first frame.
    frame(&mut app, egui::RawInput::default());
    let output = frame(&mut app, egui::RawInput::default());
    let button = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == "Replace" => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        })
        .expect("Replace button");
    for pressed in [true, false] {
        frame(
            &mut app,
            egui::RawInput {
                events: vec![
                    egui::Event::PointerMoved(button),
                    egui::Event::PointerButton {
                        pos: button,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                ..Default::default()
            },
        );
    }
    assert_eq!(red(&target), 250);
    let assets = app.world().resource::<EditorAssetState>();
    assert!(assets.replace_preview.is_none());
    assert!(
        assets
            .message
            .as_deref()
            .unwrap()
            .contains(&imported.id.to_string()),
        "{:?}",
        assets.message
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct LootTable {
    gold: u32,
}

crate::reflect! {
    struct LootTable { gold: u32 }
}

impl DataAsset for LootTable {
    const NAME: &'static str = "test.loot_table";
}

#[test]
fn data_assets_are_created_edited_and_reloaded_from_the_editor() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-data-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.register_data_asset::<LootTable>();
    let world = app.world_mut();
    let registered = world
        .resource::<crate::assets::DataAssetTypes>()
        .get(LootTable::NAME)
        .unwrap()
        .clone();

    // New picks a free name from the type and writes the default value.
    let first = new_data_asset(&folder, &registered).unwrap();
    let second = new_data_asset(&folder, &registered).unwrap();
    assert_eq!(first, folder.join("loot_table.rdata"));
    assert_eq!(second, folder.join("loot_table_2.rdata"));

    let handle = world
        .resource_mut::<AssetServer>()
        .load_data::<LootTable>(&first)
        .unwrap();
    let mut inspection = DataInspection::open(first.clone(), None);
    assert_eq!(inspection.type_name, LootTable::NAME);
    assert!(inspection.error.is_none());

    // Saving writes the file and loaded handles see the new value.
    inspection.value["gold"] = serde_json::json!(25);
    save_data_inspection(world, &inspection).unwrap();
    let assets = world.resource::<AssetServer>();
    assert_eq!(assets.data.get(handle).unwrap().gold, 25);

    let broken = folder.join("broken.rdata");
    std::fs::write(&broken, "{").unwrap();
    assert!(DataInspection::open(broken, None).error.is_some());
    let _ = std::fs::remove_dir_all(folder);
}

#[test]
fn edit_mode_builds_tile_maps_without_listing_or_saving_tiles() {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let world = app.world_mut();
    let map = world
        .spawn((
            SceneId::default(),
            Name("Level".into()),
            Transform::default(),
            crate::runtime::TileMap {
                rows: vec!["##".into()],
                tiles: std::collections::BTreeMap::from([(
                    "#".into(),
                    crate::runtime::TileKind::default(),
                )]),
                ..crate::runtime::TileMap::default()
            },
        ))
        .id();
    let tiles = |world: &mut World| {
        world.query::<&crate::runtime::TileOf>().iter(world).count()
    };
    crate::runtime::run_edit_mode_systems(world);
    // Two drawn tiles and one merged collider.
    assert_eq!(tiles(world), 3);
    crate::runtime::run_edit_mode_systems(world);
    assert_eq!(tiles(world), 3, "unchanged maps are not rebuilt");
    let listed = hierarchy::collect_entities(world);
    assert_eq!(
        listed.iter().map(|item| item.entity).collect::<Vec<_>>(),
        vec![map]
    );
    let saved = crate::runtime::scene_document(world, "Level").unwrap();
    assert_eq!(saved.entities.len(), 1);

    world.despawn(map);
    crate::runtime::run_edit_mode_systems(world);
    assert_eq!(tiles(world), 0);
}

#[test]
fn fluid_particles_are_not_listed_in_the_hierarchy() {
    let mut world = World::new();
    let volume = world.spawn(crate::Transform::default()).id();
    world.spawn((
        crate::Transform::default(),
        crate::runtime::FluidParticle(volume),
    ));
    let listed = hierarchy::collect_entities(&mut world);
    assert_eq!(
        listed.iter().map(|item| item.entity).collect::<Vec<_>>(),
        vec![volume]
    );
}

#[test]
fn menu_actions_show_their_shortcut() {
    let context = Context::default();
    let output = context.run(egui::RawInput::default(), |context| {
        egui::CentralPanel::default().show(context, |ui| {
            gui_elements::EditorTheme::menu_shortcut_action(
                ui,
                "Undo",
                Some("Ctrl+Z"),
                true,
            );
        });
    });
    let texts: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_owned()),
            _ => None,
        })
        .collect();
    assert!(texts.contains(&"Undo".into()), "{texts:?}");
    assert!(texts.contains(&"Ctrl+Z".into()), "{texts:?}");
}

/// Editor with one selected emitter and the Inspector filling the window.
fn particle_editor() -> (App, Entity) {
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    app.add_plugin(crate::runtime::RenderExtractPlugin).unwrap();
    app.add_plugin(EditorPlugin).unwrap();
    let entity = app.spawn((
        Name("Sparks".into()),
        Transform::default(),
        SceneId(uuid::Uuid::new_v4()),
    ));
    crate::runtime::set_registered_component(
        app.world_mut(),
        entity,
        crate::runtime::PARTICLE_EMITTER_COMPONENT,
        r#"{"rate": 30.0}"#,
    )
    .unwrap();
    let mut state = app.world_mut().resource_mut::<EditorState>();
    state.selected = Some(entity);
    state.selection = vec![entity];
    state.dock_layout = EditorDockNode::Area {
        id: 1,
        panel: EditorPanel::Inspector,
    };
    (app, entity)
}

/// Draws one editor frame with `events`; returns each text and its center.
fn editor_frame(
    app: &mut App,
    context: &Context,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Pos2)> {
    let input = egui::RawInput {
        events,
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(900.0, 4000.0),
        )),
        ..Default::default()
    };
    let output = context.run(input, |context| {
        draw_editor_view(app.world_mut(), context);
    });
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some((
                text.galley.text().to_owned(),
                text.pos + text.galley.rect.center().to_vec2(),
            )),
            _ => None,
        })
        .collect()
}

fn click_text(app: &mut App, context: &Context, text: &str) {
    let texts = editor_frame(app, context, Vec::new());
    let at = texts
        .iter()
        .find(|(shown, _)| shown == text)
        .unwrap_or_else(|| panic!("no {text}: {texts:?}"))
        .1;
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    editor_frame(app, context, vec![egui::Event::PointerMoved(at)]);
    editor_frame(app, context, vec![button(true)]);
    editor_frame(app, context, vec![button(false)]);
}

#[test]
fn particle_inspector_edits_are_one_undo_step_each() {
    let (mut app, entity) = particle_editor();
    let context = Context::default();
    let texts = editor_frame(&mut app, &context, Vec::new());
    for title in ["Emission", "Shape", "Lifetime", "Velocity", "Rendering"] {
        assert!(texts.iter().any(|(text, _)| text == title), "{title}");
    }
    let bursts = |app: &App| {
        app.world()
            .get::<crate::runtime::ParticleEmitter>(entity)
            .unwrap()
            .bursts
            .len()
    };
    click_text(&mut app, &context, "+ Add Burst");
    assert_eq!(bursts(&app), 1);
    // The edit stopped, so it closes into one Undo step.
    editor_frame(&mut app, &context, Vec::new());
    let history = app.world().resource::<EditorHistory>();
    assert_eq!(history.undo.len(), 1);
    assert!(app.world().resource::<EditorState>().scene_dirty);

    app.world_mut()
        .resource_mut::<EditorCommandQueue>()
        .0
        .push(EditorAction::Undo);
    editor_frame(&mut app, &context, Vec::new());
    let world = app.world_mut();
    let emitters = world
        .query::<&crate::runtime::ParticleEmitter>()
        .iter(world)
        .map(|emitter| (emitter.rate, emitter.bursts.len()))
        .collect::<Vec<_>>();
    assert_eq!(emitters, [(30.0, 0)]);
}

#[test]
fn particles_preview_while_stopped_and_transport_is_not_an_edit() {
    let (mut app, entity) = particle_editor();
    let context = Context::default();
    let alive = |app: &App| {
        app.world()
            .get::<crate::runtime::ParticleSystem>(entity)
            .map_or(0, crate::runtime::ParticleSystem::alive)
    };
    for _ in 0..10 {
        update_edit_preview(app.world_mut(), Duration::from_millis(50));
    }
    assert!(alive(&app) > 0);
    assert!(editor_needs_continuous_redraw(app.world()));

    click_text(&mut app, &context, "Pause");
    editor_frame(&mut app, &context, Vec::new());
    update_edit_preview(app.world_mut(), Duration::from_millis(50));
    let system = app
        .world()
        .get::<crate::runtime::ParticleSystem>(entity)
        .unwrap();
    assert!(system.paused);
    assert!(!editor_needs_continuous_redraw(app.world()));
    assert!(app.world().resource::<EditorHistory>().undo.is_empty());
    assert!(!app.world().resource::<EditorState>().scene_dirty);

    // Play starts the game's emitters fresh, without the preview's state.
    let world = app.world_mut();
    let mut state = world.resource::<EditorState>().clone();
    let mut history = EditorHistory::default();
    view::start_embedded_preview(world, &mut state, &mut history);
    assert!(world
        .get::<crate::runtime::ParticleSystem>(entity)
        .is_none());
    // Nor does the preview step while the game runs.
    world.resource_mut::<EditorState>().mode = state.mode;
    update_edit_preview(world, Duration::from_millis(50));
    assert!(world
        .get::<crate::runtime::ParticleSystem>(entity)
        .is_none());
}

fn timeline_editor() -> (App, Entity) {
    let (mut app, entity) = particle_editor();
    app.world_mut().resource_mut::<EditorState>().dock_layout =
        EditorDockNode::Area {
            id: 1,
            panel: EditorPanel::Timeline,
        };
    (app, entity)
}

fn slide_clip(app: &mut App, entity: Entity) {
    crate::runtime::set_registered_component(
        app.world_mut(),
        entity,
        crate::runtime::ANIMATION_COMPONENT,
        r#"{"clips": [{"name": "slide", "tracks": [{"property": "Position",
            "keys": [{"time": 0.0, "value": [0.0, 0.0, 0.0]},
                     {"time": 1.0, "value": [2.0, 0.0, 0.0]}]}]}]}"#,
    )
    .unwrap();
}

#[test]
fn timeline_adds_animation_and_keys_as_one_undo_step_each() {
    let (mut app, entity) = timeline_editor();
    let context = Context::default();
    click_text(&mut app, &context, "+ Add Animation");
    editor_frame(&mut app, &context, Vec::new());
    assert!(app
        .world()
        .get::<crate::runtime::Animation>(entity)
        .is_some());
    assert_eq!(app.world().resource::<EditorHistory>().undo.len(), 1);

    click_text(&mut app, &context, "Insert Key");
    click_text(&mut app, &context, "All");
    editor_frame(&mut app, &context, Vec::new());
    let animation = app.world().get::<crate::runtime::Animation>(entity);
    assert_eq!(animation.unwrap().clips[0].tracks.len(), 3);
    assert_eq!(app.world().resource::<EditorHistory>().undo.len(), 2);

    app.world_mut()
        .resource_mut::<EditorCommandQueue>()
        .0
        .push(EditorAction::Undo);
    editor_frame(&mut app, &context, Vec::new());
    let world = app.world_mut();
    let tracks = world
        .query::<&crate::runtime::Animation>()
        .iter(world)
        .map(|animation| animation.clips[0].tracks.len())
        .collect::<Vec<_>>();
    assert_eq!(tracks, [0]);
}

#[test]
fn timeline_scrub_poses_but_scene_documents_keep_the_rest_pose() {
    let (mut app, entity) = timeline_editor();
    slide_clip(&mut app, entity);
    let context = Context::default();
    app.world_mut().resource_mut::<EditorState>().timeline.time = 0.5;
    editor_frame(&mut app, &context, Vec::new());
    let x =
        |app: &App| app.world().get::<Transform>(entity).unwrap().position[0];
    assert!((x(&app) - 1.0).abs() < 1e-4, "{}", x(&app));
    let document =
        crate::runtime::scene_document(app.world_mut(), "check").unwrap();
    let saved = document.entities[0].transform.as_ref().unwrap();
    assert_eq!(saved.position[0], 0.0);
    assert!(app.world().resource::<EditorHistory>().undo.is_empty());

    // Leaving the animation puts the rest pose back.
    app.world_mut().resource_mut::<EditorState>().selected = None;
    editor_frame(&mut app, &context, Vec::new());
    assert_eq!(x(&app), 0.0);
}

#[test]
fn timeline_record_mode_keys_transform_edits_at_the_playhead() {
    let (mut app, entity) = timeline_editor();
    slide_clip(&mut app, entity);
    let context = Context::default();
    {
        let mut state = app.world_mut().resource_mut::<EditorState>();
        state.timeline.time = 0.5;
        state.timeline.record = true;
    }
    editor_frame(&mut app, &context, Vec::new());
    // A gizmo or Inspector edit moves the posed object.
    app.world_mut()
        .get_mut::<Transform>(entity)
        .unwrap()
        .position[1] = 3.0;
    editor_frame(&mut app, &context, Vec::new());
    let keys = &app
        .world()
        .get::<crate::runtime::Animation>(entity)
        .unwrap()
        .clips[0]
        .tracks[0]
        .keys;
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[1].time, 0.5);
    assert_eq!(keys[1].value, [1.0, 3.0, 0.0]);
    assert!(app.world().resource::<EditorState>().scene_dirty);
    // Rotation and scale did not change, so they got no track.
    let animation = app.world().get::<crate::runtime::Animation>(entity);
    assert_eq!(animation.unwrap().clips[0].tracks.len(), 1);
}

#[cfg(feature = "gltf")]
#[test]
fn added_model_clips_follow_renamed_nodes() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-windmill-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("windmill.gltf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples/vertical_slice/windmill.gltf"),
        &path,
    )
    .unwrap();
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let world = app.world_mut();
    add_model_to_scene(world, &path).unwrap();
    let second = add_model_to_scene(world, &path).unwrap();
    let animation = world.get::<crate::runtime::Animation>(second).unwrap();
    let rotor = &animation.clips[0].tracks[0].target;
    let target = crate::runtime::find_target(world, second, rotor).unwrap();
    // The second copy's rotor was renamed, and its track follows.
    assert_ne!(rotor, "Tower/Rotor");
    assert!(world.get::<Parent>(target).is_some());
    std::fs::remove_dir_all(folder).unwrap();
}

#[cfg(feature = "gltf")]
#[test]
fn added_model_skin_joints_follow_renamed_nodes() {
    let folder = std::env::temp_dir()
        .join(format!("rusting-editor-bar-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("bending_bar.gltf");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("samples/vertical_slice/bending_bar.gltf"),
        &path,
    )
    .unwrap();
    let mut app = App::new();
    app.add_plugin(crate::AssetPlugin).unwrap();
    let world = app.world_mut();
    add_model_to_scene(world, &path).unwrap();
    let second = add_model_to_scene(world, &path).unwrap();
    let rig = world.get::<crate::runtime::Children>(second).unwrap().0[0];
    let body = world.get::<crate::runtime::Children>(rig).unwrap().0[0];
    let skin = world.get::<crate::runtime::Skin>(body).unwrap().clone();
    // The second copy's joints were renamed, and the skin follows.
    assert_ne!(skin.joints[1], "../Root/Tip");
    for joint in &skin.joints {
        let target = crate::runtime::find_target(world, body, joint).unwrap();
        assert_ne!(target, body);
    }
    // Both skeletons show in the Scene View: Root as a cross, Tip as a
    // cross plus the bone from Root.
    crate::runtime::propagate_transforms(world);
    let bones = crate::editor::overlay::bone_shapes(world);
    assert_eq!(bones.len(), 4);
    let tip =
        crate::runtime::find_target(world, body, &skin.joints[1]).unwrap();
    let (_, lines) = bones.iter().find(|(joint, _)| *joint == tip).unwrap();
    assert_eq!(lines.len(), 4);
    std::fs::remove_dir_all(folder).unwrap();
}
