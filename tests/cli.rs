use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use uuid::Uuid;

fn run(args: &[&str]) -> (Output, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap();
    (output, value)
}

fn temporary_parent() -> PathBuf {
    let path = std::env::temp_dir()
        .join(format!("rusting-cli-test-{}", Uuid::new_v4()));
    std::fs::create_dir(&path).unwrap();
    path
}

fn generated_project(parent: &Path) -> PathBuf {
    let (output, result) = run(&["new", parent.to_str().unwrap(), "CLI Test"]);
    assert!(output.status.success(), "{result}");
    parent.join("CLI Test")
}

#[test]
fn doctor_reports_capabilities_without_requiring_a_gpu() {
    let (output, result) = run(&["doctor"]);
    assert!(output.status.success(), "{result}");
    assert!(result["data"]["engine_version"].is_string());
    assert!(result["data"]["platform"]["os"].is_string());
    assert!(result["data"]["build_tools"]["cargo"].is_boolean());
    assert!(result["data"]["vulkan"]["available"].is_boolean());
    assert!(result["data"]["probe"].is_null());
    // The probe succeeds or reports why; it never fails the command.
    let (output, probed) = run(&["doctor", "--probe"]);
    assert!(output.status.success(), "{probed}");
    let probe = &probed["data"]["probe"];
    assert!(probe["selected_device"].is_string() || probe["error"].is_string());
}

#[test]
fn repository_project_inspects_validates_and_cooks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("testGame");
    let root = root.to_str().unwrap();
    let (output, inspected) = run(&["project", "inspect", root]);
    assert!(output.status.success(), "{inspected}");
    assert_eq!(inspected["data"]["binary_name"], "project1");
    let (output, scene) =
        run(&["scene", "inspect", &format!("{root}/scenes/main.rscene")]);
    assert!(output.status.success(), "{scene}");
    assert!(scene["data"]["entity_count"].as_u64().unwrap() > 0);
    assert_eq!(
        scene["data"]["entities"].as_array().unwrap().len() as u64,
        scene["data"]["entity_count"].as_u64().unwrap()
    );
    let (_, names) = run(&[
        "scene",
        "inspect",
        &format!("{root}/scenes/main.rscene"),
        "--fields",
        "name",
    ]);
    let first = &names["data"]["entities"][0];
    assert!(
        first["name"].is_string() && first["id"].is_null(),
        "{first}"
    );
    assert_eq!(scene["data"]["scene_version"], 3);
    assert_eq!(
        scene["data"]["loaded_scene_version"],
        rusting_engine::runtime::SCENE_FORMAT_VERSION
    );
    let (output, validated) = run(&["validate", root]);
    assert!(output.status.success(), "{validated}");
}

#[test]
fn generated_project_queries_in_persistent_id_order() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let scene = root.join("scenes/main.rscene");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    document["entities"].as_array_mut().unwrap().reverse();
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    let path = scene.to_str().unwrap();
    let (output, queried) = run(&["scene", "query", path]);
    assert!(output.status.success(), "{queried}");
    let ids: Vec<_> = queried["data"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entity| entity["id"].as_str().unwrap())
        .collect();
    assert!(ids.windows(2).all(|pair| pair[0] <= pair[1]));
    let id = ids[0];
    let (output, filtered) = run(&["scene", "query", path, "--id", id]);
    assert!(output.status.success(), "{filtered}");
    assert_eq!(filtered["data"]["count"], 1);
    assert!(filtered["data"]["entities"][0]["transform"].is_object());
    let (output, validated) = run(&["validate", root.to_str().unwrap()]);
    assert!(output.status.success(), "{validated}");
    let (output, cooked) = run(&["cook", root.to_str().unwrap()]);
    assert!(output.status.success(), "{cooked}");
    assert!(Path::new(cooked["data"]["output"].as_str().unwrap()).is_file());
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn scene_patch_dry_runs_writes_and_reports_revision_conflicts() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let scene = root.join("scenes/main.rscene");
    let path = scene.to_str().unwrap();
    let (_, inspected) = run(&["scene", "inspect", path]);
    let revision = inspected["data"]["revision"].as_str().unwrap().to_owned();
    let (_, queried) = run(&["scene", "query", path]);
    let id = queried["data"]["entities"][0]["id"].as_str().unwrap();
    let patch_path = parent.join("patch.json");
    let patch = serde_json::json!({"expected_revision": revision, "operations": [
        {"op": "set", "id": id, "path": "/name", "value": "Patched"}]});
    std::fs::write(&patch_path, patch.to_string()).unwrap();
    let patch_arg = patch_path.to_str().unwrap();

    let original = std::fs::read(&scene).unwrap();
    let (output, dry) = run(&["scene", "patch", path, patch_arg, "--dry-run"]);
    assert!(output.status.success(), "{dry}");
    assert_eq!(dry["data"]["patch"]["written"], false);
    assert_eq!(dry["data"]["patch"]["changes"][0]["path"], "/name");
    assert_eq!(dry["data"]["patch"]["changes"][0]["after"], "Patched");
    assert_eq!(std::fs::read(&scene).unwrap(), original);

    let (output, applied) = run(&["scene", "patch", path, patch_arg]);
    assert!(output.status.success(), "{applied}");
    let (_, renamed) = run(&["scene", "query", path, "--name", "Patched"]);
    assert_eq!(renamed["data"]["count"], 1);
    assert_eq!(
        renamed["data"]["revision"],
        applied["data"]["patch"]["revision_after"]
    );

    // The same patch now targets a stale revision.
    let (output, conflict) = run(&["scene", "patch", path, patch_arg]);
    assert_eq!(output.status.code(), Some(1), "{conflict}");
    assert_eq!(conflict["diagnostics"][0]["code"], "SCENE_CONFLICT");
    assert_eq!(
        conflict["data"]["revision"],
        applied["data"]["patch"]["revision_after"]
    );
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn read_only_mode_runs_inspection_and_refuses_writes() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let scene = root.join("scenes/main.rscene");
    let path = scene.to_str().unwrap();
    let (_, queried) = run(&["scene", "query", path, "--read-only"]);
    let id = queried["data"]["entities"][0]["id"].as_str().unwrap();
    let patch_path = parent.join("patch.json");
    let patch = serde_json::json!({"operations": [
        {"op": "set", "id": id, "path": "/name", "value": "Patched"}]});
    std::fs::write(&patch_path, patch.to_string()).unwrap();
    let patch_arg = patch_path.to_str().unwrap();
    let original = std::fs::read(&scene).unwrap();
    let code = std::fs::read(root.join("src/main.rs")).unwrap();

    let (output, dry) = run(&[
        "scene",
        "patch",
        path,
        patch_arg,
        "--dry-run",
        "--read-only",
    ]);
    assert!(output.status.success(), "{dry}");
    let (output, linted) =
        run(&["lint", root.to_str().unwrap(), "--read-only"]);
    assert!(output.status.success(), "{linted}");

    let (output, refused) =
        run(&["scene", "patch", path, patch_arg, "--read-only"]);
    assert_eq!(output.status.code(), Some(1), "{refused}");
    assert_eq!(refused["diagnostics"][0]["code"], "READ_ONLY");
    let (output, refused) = run(&[
        "add",
        "system",
        root.to_str().unwrap(),
        "spin",
        "--read-only",
    ]);
    assert_eq!(output.status.code(), Some(1), "{refused}");
    assert_eq!(refused["diagnostics"][0]["code"], "READ_ONLY");

    // `--dry-run` as a flag's value is no dry run.
    let (output, refused) =
        run(&["lease", "claim", path, "--as", "--dry-run", "--read-only"]);
    assert_eq!(output.status.code(), Some(1), "{refused}");
    assert_eq!(refused["diagnostics"][0]["code"], "READ_ONLY");

    // Every command read-only mode allows leaves the project as it was.
    let snapshot = |dir: &Path| {
        let mut files = std::collections::BTreeMap::new();
        let mut stack = vec![dir.to_owned()];
        while let Some(folder) = stack.pop() {
            for entry in std::fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path).unwrap());
                }
            }
        }
        files
    };
    let before = snapshot(&root);
    let dir = root.to_str().unwrap();
    for command in [
        &["version"][..],
        &["doctor"],
        &["project", "inspect", dir],
        &["project", "summary", dir],
        &["impact", dir, "Player"],
        &["lease", "list", dir],
        &["provenance", dir],
        &["log", dir],
        &["systems", dir],
        &["docs"],
        &["docs", "search", "scene"],
        &["explain"],
        &["schema"],
        &["validate", dir],
        &["lint", dir],
        &["diff", path, path],
        &["scene", "inspect", path],
        &["scene", "map", path],
        &["scene", "query", path],
        &["asset", "list", dir],
        &["preset", "list"],
        &["effect", "list"],
        &["recipe", "list"],
        &["scene", "patch", path, patch_arg, "--dry-run"],
        &["fix", dir, "--dry-run"],
    ] {
        let (_, result) = run(&[command, &["--read-only"]].concat());
        assert_ne!(
            result["diagnostics"][0]["code"], "READ_ONLY",
            "{command:?}: {result}"
        );
    }
    assert!(
        snapshot(&root) == before,
        "a read-only command wrote a file"
    );

    // RUSTING_READ_ONLY sets the mode for a session; 0 leaves it off.
    let with_env = |value: &str| {
        Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(["scene", "patch", path, patch_arg, "--json"])
            .env("RUSTING_READ_ONLY", value)
            .output()
            .unwrap()
    };
    assert_eq!(with_env("1").status.code(), Some(1));
    assert_eq!(std::fs::read(&scene).unwrap(), original);
    assert_eq!(std::fs::read(root.join("src/main.rs")).unwrap(), code);
    assert!(with_env("0").status.success());
    assert_ne!(std::fs::read(&scene).unwrap(), original);
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn confine_refuses_paths_outside_the_folder_after_resolving_links() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let dir = root.to_str().unwrap();
    let scene = root.join("scenes/main.rscene");
    let outside = parent.join("patch.json");
    // Runs inside the project, as a confined agent would.
    let inside = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(args)
            .arg("--json")
            .current_dir(&root)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output, value)
    };
    std::fs::write(&outside, r#"{"operations": []}"#).unwrap();

    let (output, linted) = inside(&["lint", dir, "--confine", dir]);
    assert!(output.status.success(), "{linted}");
    // `..` that comes back inside is fine.
    let around = format!("{dir}/../CLI Test/scenes/main.rscene");
    let (output, inspected) =
        inside(&["scene", "inspect", &around, "--confine", dir]);
    assert!(output.status.success(), "{inspected}");

    let (output, refused) = inside(&[
        "scene",
        "patch",
        scene.to_str().unwrap(),
        outside.to_str().unwrap(),
        "--confine",
        dir,
    ]);
    assert_eq!(output.status.code(), Some(1), "{refused}");
    assert_eq!(refused["diagnostics"][0]["code"], "OUTSIDE_CONFINE");
    #[cfg(unix)]
    {
        // A link inside the folder that points out of it is outside.
        std::os::unix::fs::symlink(&parent, root.join("out")).unwrap();
        let (output, refused) = inside(&[
            "scene",
            "inspect",
            &format!("{dir}/out/x.rscene"),
            "--confine",
            dir,
        ]);
        assert_eq!(output.status.code(), Some(1), "{refused}");
        assert_eq!(refused["diagnostics"][0]["code"], "OUTSIDE_CONFINE");
    }

    // A working folder outside is refused, even with only relative words.
    let (output, refused) = run(&["lint", "--confine", dir]);
    assert_eq!(output.status.code(), Some(1), "{refused}");
    assert_eq!(refused["diagnostics"][0]["code"], "OUTSIDE_CONFINE");

    // RUSTING_CONFINE sets it for a session.
    let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .args(["new", parent.to_str().unwrap(), "Other", "--json"])
        .env("RUSTING_CONFINE", dir)
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!parent.join("Other").exists());

    // A flag cannot widen RUSTING_CONFINE to a folder around it.
    let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .args(["new", parent.to_str().unwrap(), "Other", "--json"])
        .args(["--confine", parent.to_str().unwrap()])
        .env("RUSTING_CONFINE", dir)
        .current_dir(&root)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(!parent.join("Other").exists());
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn offline_mode_keeps_cargo_off_the_network() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    // An empty cargo home has no registry cache, so the build needs the
    // network and offline mode must stop it.
    let check = |flag: Option<&str>, env: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(["check", root.to_str().unwrap(), "--json"])
            .args(flag)
            .env("CARGO_HOME", parent.join("cargo-home"))
            .env("CARGO_TARGET_DIR", parent.join("target"))
            .env("RUSTING_OFFLINE", env)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        (output.status.success(), text)
    };
    for (flag, env) in [(Some("--offline"), ""), (None, "1")] {
        let (ok, text) = check(flag, env);
        assert!(!ok, "{text}");
        assert!(text.contains("offline"), "{text}");
    }
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn the_command_log_records_every_command() {
    let parent = temporary_parent();
    let log = parent.join("commands.jsonl");
    for args in [&["explain", "scene_conflict"][..], &["bogus"]] {
        Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(args)
            .env("RUSTING_COMMAND_LOG", &log)
            .output()
            .unwrap();
    }
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[0]["args"],
        serde_json::json!(["explain", "scene_conflict"])
    );
    assert_eq!(lines[0]["ok"], true);
    assert_eq!(lines[1]["ok"], false);
    assert_eq!(lines[1]["codes"], serde_json::json!(["CLI_USAGE"]));
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn malformed_and_missing_inputs_have_json_diagnostics_and_nonzero_exit() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let manifest = root.join("project.json");
    std::fs::write(&manifest, "{").unwrap();
    let (output, result) = run(&["validate", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["diagnostics"][0]["code"], "PROJECT_MANIFEST_JSON");
    std::fs::write(&manifest, serde_json::to_vec(&serde_json::json!({"format_version":1,"name":"CLI Test","main_scene":"scenes/main.rscene","cooked_scene":"build/main.rscene.bin","binary_name":"cli_test"})).unwrap()).unwrap();
    let scene = root.join("scenes/main.rscene");
    std::fs::write(&scene, "{").unwrap();
    let (output, result) = run(&["scene", "inspect", scene.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["diagnostics"][0]["code"], "SCENE_JSON");
    let duplicate = serde_json::json!({"format_version":6,"name":"Broken","entities":[
        {"id":Uuid::nil(),"parent":null,"name":"A","transform":null,"mesh_renderer":null,"camera":null,"visible":null},
        {"id":Uuid::nil(),"parent":null,"name":"B","transform":null,"mesh_renderer":null,"camera":null,"visible":null}
    ]});
    std::fs::write(&scene, serde_json::to_vec(&duplicate).unwrap()).unwrap();
    let (output, result) = run(&["scene", "inspect", scene.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["diagnostics"][0]["code"], "SCENE_STRUCTURE");
    std::fs::remove_file(&scene).unwrap();
    let (output, result) = run(&["validate", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["diagnostics"][0]["code"], "PROJECT_MISSING_FILE");
    let (output, result) =
        run(&["scene", "query", "missing.rscene", "--id", "bad"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(result["diagnostics"][0]["code"], "CLI_USAGE");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn a_collider_without_a_physics_body_is_reported() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let scene = root.join("scenes/main.rscene");
    let (output, result) = run(&["validate", root.to_str().unwrap()]);
    assert!(output.status.success(), "{result}");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    document["entities"][0]["collider"] = serde_json::json!({
        "shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}},
        "friction": 0.5, "restitution": 0.0, "sensor": false
    });
    document["entities"][0]["physics_body"] = Value::Null;
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    let (output, result) = run(&["validate", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{result}");
    let diagnostic = &result["diagnostics"][0];
    assert_eq!(diagnostic["code"], "SCENE_COLLIDER_WITHOUT_BODY");
    assert_eq!(diagnostic["scene_location"], "/entities/0/collider");
    assert_eq!(diagnostic["entity"]["id"], document["entities"][0]["id"]);

    // A player controller's collider is only its own shape.
    document["entities"][0]["components"]["rusting.player_controller"] =
        serde_json::json!("{}");
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    let (output, result) = run(&["validate", root.to_str().unwrap()]);
    assert!(output.status.success(), "{result}");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn missing_asset_reference_fails_validation_with_location() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let scene = root.join("scenes/main.rscene");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    document["entities"][0]["mesh_renderer"]["mesh"] =
        serde_json::json!({"AssetPath":"../assets/missing.rmesh"});
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    let (output, result) = run(&["validate", root.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(result["diagnostics"][0]["code"], "SCENE_MISSING_ASSET");
    assert!(result["diagnostics"][0]["scene_location"]
        .as_str()
        .unwrap()
        .starts_with("/entities/"));
    assert_eq!(
        result["diagnostics"][0]["entity"]["id"],
        document["entities"][0]["id"]
    );
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn docs_lists_searches_shows_and_briefs_within_a_token_budget() {
    let (output, listed) = run(&["docs"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(listed["data"]["items"].as_array().unwrap().len() > 50);

    let (_, found) = run(&["docs", "search", "determinism", "mode"]);
    assert_eq!(found["data"]["matches"][0]["id"], "manual/determinism");

    let (_, shown) =
        run(&["docs", "show", "manual/concepts", "--budget", "50"]);
    assert_eq!(shown["data"]["truncated"], true);
    assert!(shown["data"]["tokens"].as_u64().unwrap() <= 50);
    assert!(shown["data"]["total_tokens"].as_u64().unwrap() > 50);

    let (_, brief) = run(&["docs", "--brief", "--budget", "600"]);
    assert!(brief["data"]["tokens"].as_u64().unwrap() <= 600);
    assert!(brief["data"]["text"]
        .as_str()
        .unwrap()
        .contains("guide/agent-skill"));

    let (output, missing) = run(&["docs", "show", "no-such-page"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(missing["diagnostics"][0]["code"], "CLI_USAGE");
    let (output, _) = run(&["docs", "--budget", "0", "--brief"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn fix_renames_misspelled_scene_keys_in_place_and_keeps_the_rest() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let root_arg = root.to_str().unwrap();
    let scene = root.join("scenes/main.rscene");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    // A required field that does not decode, an optional one that loading
    // drops, and a key close to nothing, which has no certain fix.
    let render = document["render"].as_object_mut().unwrap();
    let quality = render.remove("quality").unwrap();
    render.insert("qualty".into(), quality.clone());
    let cube = document["entities"][0].as_object_mut().unwrap();
    let visible = cube.remove("visible").unwrap_or(Value::Bool(true));
    cube.insert("visble".into(), Value::Bool(!visible.as_bool().unwrap()));
    cube.insert("notes".into(), "keep me".into());
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();

    let (_, result) = run(&["validate", root_arg]);
    assert_eq!(result["diagnostics"][0]["code"], "SCENE_JSON");
    assert_eq!(result["diagnostics"][0]["fix"]["op"], "rename_key");
    assert_eq!(result["diagnostics"][0]["fix"]["path"], "/render/qualty");

    let before = std::fs::read(&scene).unwrap();
    let (_, dry) = run(&["fix", root_arg, "--dry-run"]);
    assert_eq!(dry["data"]["fixed"], 1, "a dry run shows the first round");
    assert_eq!(std::fs::read(&scene).unwrap(), before, "and writes nothing");

    let (output, fixed) = run(&["fix", root_arg]);
    assert_eq!(output.status.code(), Some(1), "`notes` has no certain fix");
    assert_eq!(fixed["data"]["fixed"], 2);
    let left: Vec<_> = fixed["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["scene_location"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(left, ["/entities/0/notes"]);
    let after: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    assert_eq!(after["render"]["quality"], quality);
    assert_eq!(after["entities"][0]["visible"], !visible.as_bool().unwrap());
    assert_eq!(after["entities"][0]["notes"], "keep me");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn scene_and_patch_errors_point_at_the_line_object_and_operation() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let scene = root.join("scenes/main.rscene");
    let path = scene.to_str().unwrap();
    let original = std::fs::read_to_string(&scene).unwrap();
    let mut document: Value = serde_json::from_str(&original).unwrap();

    // An unknown object in the second operation points into the patch file.
    let id = document["entities"][0]["id"].clone();
    let patch_path = parent.join("patch.json");
    std::fs::write(
        &patch_path,
        serde_json::json!({"operations": [
            {"op": "set", "id": id, "path": "/name", "value": "Fine"},
            {"op": "set", "id": Uuid::new_v4(), "path": "/name", "value": "Lost"}]})
        .to_string(),
    )
    .unwrap();
    let (_, result) =
        run(&["scene", "patch", path, patch_path.to_str().unwrap()]);
    let diagnostic = &result["diagnostics"][0];
    assert_eq!(diagnostic["code"], "PATCH_OPERATION");
    assert_eq!(diagnostic["file"], patch_path.to_str().unwrap());
    assert_eq!(diagnostic["scene_location"], "/operations/1");

    // A patch that breaks a scene rule names the object it breaks it at.
    let first = document["entities"][0]["name"].clone();
    let second = document["entities"][1]["id"].clone();
    std::fs::write(
        &patch_path,
        serde_json::json!({"operations": [
            {"op": "set", "id": second, "path": "/name", "value": first}]})
        .to_string(),
    )
    .unwrap();
    let (_, result) =
        run(&["scene", "patch", path, patch_path.to_str().unwrap()]);
    let diagnostic = &result["diagnostics"][0];
    assert_eq!(diagnostic["code"], "PATCH_INVALID");
    assert_eq!(diagnostic["entity"]["id"], second);
    assert_eq!(diagnostic["entity"]["name"], first);

    // A duplicated name names the second object that uses it.
    document["entities"][1]["name"] = first.clone();
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    let (_, result) = run(&["scene", "inspect", path]);
    let diagnostic = &result["diagnostics"][0];
    assert_eq!(diagnostic["code"], "SCENE_STRUCTURE");
    assert_eq!(diagnostic["scene_location"], "/entities/1");
    assert_eq!(diagnostic["entity"]["id"], document["entities"][1]["id"]);
    assert_eq!(diagnostic["entity"]["name"], first);

    // Broken JSON reports its line and column.
    std::fs::write(&scene, original.replacen('{', "{\n  oops", 1)).unwrap();
    let (_, result) = run(&["scene", "inspect", path]);
    let diagnostic = &result["diagnostics"][0];
    assert_eq!(diagnostic["code"], "SCENE_JSON");
    assert_eq!(diagnostic["line"], 2);
    assert!(diagnostic["column"].as_u64().unwrap() > 0);
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn run_and_export_reject_malformed_flags_as_usage_errors() {
    for args in [
        &["run", "project", "--ticks"][..],
        &["run", "project", "--ticks", "many"],
        &["run", "project", "--seed"],
        &["run", "project", "--seed", "-1"],
        &["run", "project", "--fast"],
        &["export", "project"],
        &["export", "project", "out", "--target"],
    ] {
        let (output, result) = run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {result}");
        assert_eq!(result["diagnostics"][0]["code"], "CLI_USAGE");
    }
}

/// Builds a generated project three times with cargo, so it is slow. Run it
/// with `cargo test --test cli -- --ignored`.
#[test]
#[ignore = "runs real cargo builds of a generated project"]
fn generated_project_checks_runs_and_exports_a_verified_game() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let root = root.to_str().unwrap();
    // Share one target folder across runs so later runs reuse the build.
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-games");
    let cli = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(args)
            .arg("--json")
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output, value)
    };

    let (output, checked) = cli(&["check", root]);
    assert!(output.status.success(), "{checked}");

    let (output, ran) = cli(&["run", root, "--ticks", "30"]);
    assert!(output.status.success(), "{ran}");
    assert_eq!(ran["data"]["game"]["exit_code"], 0);
    assert_eq!(ran["data"]["game"]["timed_out"], false);
    let timings = &ran["data"]["timings"];
    assert!(timings["game_first_frame_ms"].is_u64(), "{timings}");
    assert!(
        timings["command_to_first_frame_ms"].as_u64()
            >= timings["game_first_frame_ms"].as_u64()
    );

    let exports = parent.join("exports");
    std::fs::create_dir(&exports).unwrap();
    let (output, exported) = cli(&["export", root, exports.to_str().unwrap()]);
    assert!(output.status.success(), "{exported}");
    assert_eq!(exported["data"]["verified"]["exit_code"], 0);
    let files = exported["data"]["files"].as_array().unwrap();
    assert!(files
        .iter()
        .any(|file| file.as_str().unwrap().ends_with(".rscene.bin")));

    // A Rust error fails the build with the file and line attached.
    let (_, project) = cli(&["project", "inspect", root]);
    let code = project["data"]["code_path"].as_str().unwrap().to_owned();
    let source = std::fs::read_to_string(&code).unwrap();
    std::fs::write(
        &code,
        format!("{source}\nfn broken() {{ missing_value }}\n"),
    )
    .unwrap();
    let (output, failed) = cli(&["run", root, "--ticks", "1"]);
    assert_eq!(output.status.code(), Some(1), "{failed}");
    let rust = failed["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "RUST_BUILD_ERROR")
        .unwrap_or_else(|| panic!("no Rust diagnostic: {failed}"));
    assert!(Path::new(rust["file"].as_str().unwrap()).ends_with("src/main.rs"));
    assert!(rust["message"].as_str().unwrap().contains("missing_value"));
    std::fs::write(&code, source).unwrap();

    // A broken game fails the run with the game's exit reported.
    std::fs::write(Path::new(root).join("scenes/main.rscene"), "not a scene")
        .unwrap();
    let (output, broken) = cli(&["run", root, "--ticks", "30"]);
    assert_eq!(output.status.code(), Some(1), "{broken}");
    let _ = std::fs::remove_dir_all(&parent);
}

/// Runs scenarios inside a real game build. Ignored like the test above.
#[test]
#[ignore = "runs real cargo builds of a generated project"]
fn generated_game_passes_and_fails_scenarios_with_a_tick_trace() {
    let parent = temporary_parent();
    let (scene, cube) = falling_cube_scene(&parent);
    let root = scene.parent().unwrap().parent().unwrap();
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-games");
    let test = |steps: Value| {
        let file = parent.join("scenario.json");
        let scenario = serde_json::json!({
            "name": "falls", "seed": 3, "ticks": 60,
            "capture_size": [64, 36], "steps": steps
        });
        std::fs::write(&file, scenario.to_string()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(["test", root.to_str().unwrap(), file.to_str().unwrap()])
            .arg("--json")
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output, value)
    };

    let (output, passed) = test(serde_json::json!([
        {"tick": 30, "expect": {"entity": cube,
            "path": "/transform/position/1", "less_than": 0.0}},
        {"tick": 30, "capture": "shots/tick30.png"},
    ]));
    assert!(output.status.success(), "{passed}");
    let report = &passed["data"]["scenario"];
    assert_eq!(report["passed"], true);
    assert_eq!(report["ticks_run"], 60);

    let (output, failed) = test(serde_json::json!([
        {"tick": 0, "until": 60, "expect": {"entity": "Cube",
            "path": "/transform/position/1", "greater_than": -0.5}},
    ]));
    assert_eq!(output.status.code(), Some(1), "{failed}");
    assert_eq!(failed["diagnostics"][0]["code"], "SCENARIO_FAILED");
    let tick = failed["data"]["scenario"]["first_failure"]["tick"]
        .as_u64()
        .unwrap();
    assert!(tick > 0 && tick < 60, "{failed}");
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
#[ignore = "runs a real cargo build of a generated project"]
fn rusting_game_registers_a_custom_component_without_extra_crates() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    // The generated Cargo.toml depends on rusting_engine alone.
    std::fs::write(
        root.join("src/main.rs"),
        r#"use rusting_engine::prelude::*;

#[derive(Component, Clone, Default, Serialize, Deserialize)]
#[serde(crate = "rusting_engine::serde")]
struct Night {
    power: i32,
}

rusting_engine::reflect! {
    struct Night {
        power: i32 { doc: "power left" },
    }
}

fn update(scene: &mut GameScene<'_>, _time: &FrameTime) {
    let world = scene.world();
    for mut night in world.query::<&mut Night>().iter_mut(world) {
        night.power += 1;
    }
}

rusting_game!(update, components: [Night => "game.night"]);
"#,
    )
    .unwrap();
    let file = parent.join("night.json");
    let scenario = serde_json::json!({
        "name": "night", "ticks": 10, "steps": [
            {"tick": 1, "set": {"entity": "Cube",
                "path": "/components/game.night", "value": {"power": 100}}},
            {"tick": 9, "expect": {"entity": "Cube",
                "path": "/components/game.night/power", "greater_than": 100}},
        ]
    });
    std::fs::write(&file, scenario.to_string()).unwrap();
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-games");
    let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .args(["test", root.to_str().unwrap(), file.to_str().unwrap()])
        .arg("--json")
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["scenario"]["passed"], true, "{result}");
    let _ = std::fs::remove_dir_all(&parent);
}

/// A generated project whose cube falls under CPU physics.
fn falling_cube_scene(parent: &Path) -> (PathBuf, String) {
    let scene = generated_project(parent).join("scenes/main.rscene");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    let cube = document["entities"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entity| entity["name"] == "Cube")
        .unwrap();
    cube["physics_body"] = serde_json::json!({
        "simulation": "Cpu", "solver": "Full", "custom_shader": null
    });
    cube["rigid_body"] = serde_json::json!({
        "kind": "Dynamic", "mass": 1.0, "linear_velocity": [0.0, 0.0, 0.0],
        "angular_velocity": [0.0, 0.0, 0.0], "gravity_scale": 1.0
    });
    cube["collider"] = serde_json::json!({
        "shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}},
        "friction": 0.5, "restitution": 0.0, "sensor": false
    });
    let id = cube["id"].as_str().unwrap().to_owned();
    std::fs::write(&scene, serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    (scene, id)
}

fn capture(args: &[&str], device: Option<&str>) -> (Output, Value) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rusting"));
    command.arg("capture").args(args).arg("--json");
    if let Some(device) = device {
        command.env("RUSTING_VULKAN_DEVICE", device);
    }
    let output = command.output().unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap();
    (output, value)
}

#[test]
fn capture_without_vulkan_still_reports_camera_and_picks_per_tick() {
    let parent = temporary_parent();
    let (scene, cube) = falling_cube_scene(&parent);
    let document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    let cube_position = document["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entity| entity["name"] == "Cube")
        .unwrap()["transform"]["position"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let scene = scene.to_str().unwrap();
    let png = parent.join("shot.png");
    let png = png.to_str().unwrap();
    let pick = |tick: &str| {
        let (output, result) = capture(
            &[
                scene, png, "--size", "320x180", "--pick", "160,150", "--tick",
                tick,
            ],
            Some("no-such-vulkan-device"),
        );
        assert_eq!(output.status.code(), Some(1), "{result}");
        assert_eq!(result["diagnostics"][0]["code"], "VULKAN_UNAVAILABLE");
        assert_eq!(result["data"]["camera"]["name"], "Game Camera");
        assert!(result["data"]["output"].is_null());
        assert!(result["data"]["picks"][0]["color"].is_null());
        result["data"]["picks"][0]["id"].clone()
    };
    // The cube is under the pixel at the start and has fallen away a
    // second later.
    assert_eq!(pick("0"), cube.as_str());
    assert!(pick("60").is_null());
    // A rectangle over the cube lists it with a share; one over empty sky
    // lists nothing.
    let rect = |rect: &str| {
        let (_, result) = capture(
            &[scene, png, "--size", "320x180", "--pick-rect", rect],
            Some("no-such-vulkan-device"),
        );
        result["data"]["pick_rects"][0].clone()
    };
    let over = rect("120,110,80,70");
    assert_eq!(over["entities"][0]["id"], cube.as_str());
    assert!(over["entities"][0]["share"].as_f64().unwrap() > 0.0);
    assert!(rect("0,0,20,10")["entities"].as_array().unwrap().is_empty());
    assert!(!Path::new(png).exists());

    // A camera placed with --at and aimed with --look-at sees the cube at
    // the center from a side the scene camera never shows.
    let cube_at = |at: &str| {
        let (_, placed) = capture(
            &[
                scene,
                png,
                "--size",
                "320x180",
                "--at",
                at,
                "--look-at",
                &cube_position,
                "--pick",
                "160,90",
            ],
            Some("no-such-vulkan-device"),
        );
        assert_eq!(placed["data"]["camera"]["name"], "Capture Camera");
        placed["data"]["picks"][0]["id"].clone()
    };
    assert_eq!(cube_at("6,1,0"), cube.as_str());
    assert_eq!(cube_at("0,8,0.01"), cube.as_str());
    let (_, away) = capture(
        &[
            scene, png, "--at", "6,1,0", "--look", "0,0", "--pick", "640,360",
        ],
        Some("no-such-vulkan-device"),
    );
    assert!(away["data"]["picks"][0]["id"].is_null(), "{away}");
    let (output, _) = capture(&[scene, png, "--look-at", "0,0,0"], None);
    assert_eq!(output.status.code(), Some(2));

    let (output, missing) = capture(
        &[scene, png, "--camera", "Nobody"],
        Some("no-such-vulkan-device"),
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(missing["diagnostics"][0]["code"], "CAMERA_NOT_FOUND");
    let (output, outside) = capture(&[scene, png, "--pick", "5000,1"], None);
    assert_eq!(output.status.code(), Some(2), "{outside}");
    let _ = std::fs::remove_dir_all(&parent);
}

#[cfg(feature = "gpu-tests")]
#[test]
fn capture_renders_a_png_and_maps_pixels_to_scene_ids() {
    let parent = temporary_parent();
    let (scene, cube) = falling_cube_scene(&parent);
    let scene = scene.to_str().unwrap();
    let png = parent.join("shot.png");
    let (output, result) = capture(
        &[
            scene,
            png.to_str().unwrap(),
            "--size",
            "320x180",
            "--camera",
            "Game Camera",
            "--pick",
            "160,150",
            "--pick",
            "10,10",
        ],
        None,
    );
    assert!(output.status.success(), "{result}");
    let picks = &result["data"]["picks"];
    assert_eq!(picks[0]["id"], cube.as_str());
    assert!(picks[1]["id"].is_null());
    assert_ne!(picks[0]["color"], picks[1]["color"]);
    assert!(result["data"]["render"]["draws"].as_u64().unwrap() > 0);
    let image = image::open(&png).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (320, 180));
    assert_eq!(
        image.get_pixel(160, 150).0.to_vec(),
        picks[0]["color"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_u64().unwrap() as u8)
            .collect::<Vec<_>>()
    );
    // The camera's persistent ID selects the same camera as its name.
    let id = result["data"]["camera"]["id"].as_str().unwrap();
    let (output, by_id) =
        capture(&[scene, png.to_str().unwrap(), "--camera", id], None);
    assert!(output.status.success(), "{by_id}");
    assert_eq!(by_id["data"]["camera"]["name"], "Game Camera");
    // A placed camera renders too, and sees the cube where it aims.
    let (output, placed) = capture(
        &[
            scene,
            png.to_str().unwrap(),
            "--size",
            "320x180",
            "--at",
            "6,1,0",
            "--look-at",
            "0,0.5,0",
            "--pick",
            "160,90",
        ],
        None,
    );
    assert!(output.status.success(), "{placed}");
    assert_eq!(placed["data"]["picks"][0]["id"], cube.as_str());
    assert_ne!(placed["data"]["picks"][0]["color"], picks[1]["color"]);
    // A late tick renders only the last few ticks, not the whole run.
    let (output, late) =
        capture(&[scene, png.to_str().unwrap(), "--tick", "300"], None);
    assert!(output.status.success(), "{late}");
    assert_eq!(late["data"]["render"]["gpu_frames"], 5, "{late}");
    let _ = std::fs::remove_dir_all(&parent);
}

#[test]
fn listing_flags_cut_output_and_every_json_result_reports_its_size() {
    let (output, full) = run(&["explain"]);
    assert!(output.status.success(), "{full}");
    let total = full["data"]["codes"].as_array().unwrap().len();
    let (_, cut) = run(&["explain", "--limit", "2", "--fields", "code"]);
    assert_eq!(cut["data"]["codes"].as_array().unwrap().len(), 2);
    assert_eq!(cut["data"]["codes"][0].as_object().unwrap().len(), 1);
    assert_eq!(cut["data"]["omitted"]["codes"], total - 2);
    assert!(
        cut["size"]["data_bytes"].as_u64().unwrap()
            < full["size"]["data_bytes"].as_u64().unwrap()
    );
    let (_, counted) = run(&["explain", "--summary"]);
    assert_eq!(counted["data"]["codes"]["count"], total);
    let (output, bad) = run(&["explain", "--limit", "x"]);
    assert_eq!(output.status.code(), Some(2), "{bad}");
}

#[test]
fn schema_json_schema_prints_a_schema_per_file_kind() {
    let (output, schema) = run(&["schema", "--json-schema"]);
    assert!(output.status.success(), "{schema}");
    for name in ["scene", "scene_patch", "scenario", "project", "asset_meta"] {
        assert_eq!(
            schema["data"][name]["$schema"],
            "https://json-schema.org/draft/2020-12/schema",
            "{name}"
        );
    }
}

#[test]
fn schema_catalog_examples_parse_and_help_lists_every_operation() {
    let (output, schema) = run(&["schema"]);
    assert!(output.status.success(), "{schema}");
    let operations = schema["data"]["operations"].as_array().unwrap();
    let help = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(help.stdout).unwrap();
    for operation in operations {
        let usage = operation["usage"].as_str().unwrap();
        assert!(help.contains(usage), "help misses `{usage}`");
        // In an empty folder, a valid example fails on the missing project
        // at worst (exit 1); only an argument error exits 2.
        let example = operation["example"].as_str().unwrap();
        // The relay needs no project, so its example would serve forever.
        if operation["name"] == "relay" {
            continue;
        }
        let folder = temporary_parent();
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(example.split_whitespace().skip(1))
            .current_dir(&folder)
            .output()
            .unwrap();
        assert_ne!(output.status.code(), Some(2), "`{example}` is rejected");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

#[test]
fn assets_import_list_and_reimport_with_a_stable_id() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let root = project.to_str().unwrap();
    let art = parent.join("crate.png");
    image::RgbaImage::from_pixel(4, 2, image::Rgba([9, 9, 9, 255]))
        .save(&art)
        .unwrap();

    let (output, imported) = run(&[
        "asset",
        "import",
        root,
        art.to_str().unwrap(),
        "--to",
        "props",
        "--author",
        "Ada",
    ]);
    assert!(output.status.success(), "{imported}");
    let asset = &imported["data"]["asset"];
    assert_eq!(asset["path"], "assets/props/crate.png");
    assert_eq!(asset["size"], serde_json::json!([4, 2]));
    assert_eq!(imported["diagnostics"][0]["code"], "ASSET_NO_LICENSE");
    let id = asset["id"].as_str().unwrap();

    let (output, listed) = run(&["asset", "list", root]);
    assert!(output.status.success(), "{listed}");
    assert_eq!(listed["data"]["assets"][0]["id"], id);

    let (output, replaced) = run(&[
        "asset",
        "reimport",
        root,
        id,
        "--license",
        "CC0-1.0",
        "--max-size",
        "2",
    ]);
    assert!(output.status.success(), "{replaced}");
    assert_eq!(replaced["data"]["asset"]["id"], id);
    assert_eq!(replaced["data"]["asset"]["size"], serde_json::json!([2, 1]));
    assert_eq!(replaced["data"]["asset"]["source"]["author"], "Ada");
    assert!(replaced["diagnostics"].as_array().unwrap().is_empty());

    std::fs::remove_file(project.join("assets/props/crate.png")).unwrap();
    let (output, broken) = run(&["asset", "list", root]);
    assert_eq!(output.status.code(), Some(1), "{broken}");
    assert_eq!(broken["diagnostics"][0]["code"], "ASSET_MISSING");
    let (output, invalid) = run(&["validate", root]);
    assert_eq!(output.status.code(), Some(1), "{invalid}");
    assert_eq!(invalid["diagnostics"][0]["code"], "ASSET_MISSING");
    let (output, usage) = run(&["asset", "import", root, "x.png", "--to"]);
    assert_eq!(output.status.code(), Some(2), "{usage}");
    std::fs::remove_dir_all(parent).unwrap();
}

#[cfg(unix)]
#[test]
fn generator_hooks_preview_and_import_through_the_cli() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let root = project.to_str().unwrap();
    let art = parent.join("made.png");
    image::RgbaImage::new(8, 8).save(&art).unwrap();
    let manifest_path = project.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap())
            .unwrap();
    manifest["generators"] = serde_json::json!({"stub": {
        "command": ["sh", "-c",
            "cp \"$0\" \"$RUSTING_OUTPUT_DIR/icon.png\" && echo '{\"file\": \"icon.png\", \"author\": \"stub\"}'",
            art],
        "license": "CC0-1.0"
    }});
    std::fs::write(&manifest_path, manifest.to_string()).unwrap();

    let (output, preview) = run(&[
        "asset",
        "generate",
        root,
        "stub",
        "a red crate",
        "--to",
        "ui",
        "--dry-run",
    ]);
    assert!(output.status.success(), "{preview}");
    assert_eq!(preview["data"]["asset"]["dry_run"], true);
    assert!(!project.join("assets/ui").exists());

    let (output, made) = run(&[
        "asset",
        "generate",
        root,
        "stub",
        "a red crate",
        "--to",
        "ui",
    ]);
    assert!(output.status.success(), "{made}");
    let asset = &made["data"]["asset"];
    assert_eq!(asset["path"], "assets/ui/icon.png");
    assert_eq!(asset["source"]["original"], "generator:stub");
    assert_eq!(asset["source"]["notes"], "prompt: a red crate");
    assert!(project.join("assets/ui/icon.png.rmeta").is_file());

    let (output, preview) = run(&[
        "asset",
        "reimport",
        root,
        "assets/ui/icon.png",
        "--from",
        art.to_str().unwrap(),
        "--dry-run",
    ]);
    assert!(output.status.success(), "{preview}");
    assert_eq!(preview["data"]["asset"]["id"], asset["id"]);

    let (output, unknown) = run(&["asset", "generate", root, "missing", "x"]);
    assert_eq!(output.status.code(), Some(1), "{unknown}");
    assert_eq!(unknown["diagnostics"][0]["code"], "GENERATOR_UNKNOWN");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn art_presets_list_and_apply_as_scene_patches() {
    let (output, listed) = run(&["preset", "list"]);
    assert!(output.status.success(), "{listed}");
    let names: Vec<_> = listed["data"]["presets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|preset| preset["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"golden_hour".to_owned()), "{names:?}");

    let parent = temporary_parent();
    let project = generated_project(&parent);
    let scene = project.join("scenes/main.rscene");
    let scene = scene.to_str().unwrap();
    let (output, dry) =
        run(&["preset", "apply", scene, "golden_hour", "--dry-run"]);
    assert!(output.status.success(), "{dry}");
    assert_eq!(dry["data"]["patch"]["written"], false);
    assert_eq!(dry["data"]["patch"]["created"].as_array().unwrap().len(), 2);
    let (output, applied) = run(&["preset", "apply", scene, "golden_hour"]);
    assert!(output.status.success(), "{applied}");
    let (_, found) = run(&["scene", "query", scene, "--name", "Sun"]);
    assert_eq!(found["data"]["count"], 1, "{found}");
    let (output, unknown) = run(&["preset", "apply", scene, "neon"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(unknown["diagnostics"][0]["code"], "PRESET_UNKNOWN");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn effect_presets_list_and_apply_as_scene_patches() {
    let (output, listed) = run(&["effect", "list"]);
    assert!(output.status.success(), "{listed}");
    let effects = listed["data"]["effects"].as_array().unwrap();
    assert_eq!(effects.len(), 11);
    assert!(effects.iter().all(|effect| effect["component"].is_object()));

    let parent = temporary_parent();
    let project = generated_project(&parent);
    let scene = project.join("scenes/main.rscene");
    let scene = scene.to_str().unwrap();
    let (output, applied) =
        run(&["effect", "apply", scene, "fire", "--at", "1,0,2"]);
    assert!(output.status.success(), "{applied}");
    let (_, found) = run(&["scene", "query", scene, "--name", "Fire"]);
    assert_eq!(found["data"]["count"], 1, "{found}");
    let (output, smoke) =
        run(&["effect", "apply", scene, "smoke", "--on", "Fire"]);
    assert!(output.status.success(), "{smoke}");
    let document: Value =
        serde_json::from_slice(&std::fs::read(scene).unwrap()).unwrap();
    let fire = document["entities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entity| entity["name"] == "Fire")
        .unwrap();
    assert_eq!(
        fire["transform"]["position"],
        serde_json::json!([1.0, 0.0, 2.0])
    );
    let emitter = fire["components"]["rusting.particle_emitter"]
        .as_str()
        .unwrap();
    assert!(
        emitter.contains("\"Cone\""),
        "smoke replaced fire: {emitter}"
    );
    let (output, unknown) = run(&["effect", "apply", scene, "lava"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(unknown["diagnostics"][0]["code"], "EFFECT_UNKNOWN");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn validate_names_parts_below_the_project_determinism_mode() {
    let parent = temporary_parent();
    let root = generated_project(&parent);
    let manifest_path = root.join("project.json");
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap())
            .unwrap();
    let scene = root.join("scenes/main.rscene");
    let mut document: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    document["entities"][0]["physics_body"] = serde_json::json!(
        {"simulation": "Cpu", "solver": "Simplified", "custom_shader": null}
    );
    std::fs::write(&scene, document.to_string()).unwrap();
    let path = root.to_str().unwrap();
    for (mode, ok) in [("Local", true), ("CrossPlatform", false)] {
        manifest["determinism"] = mode.into();
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        let (output, validated) = run(&["validate", path]);
        assert_eq!(output.status.success(), ok, "{validated}");
        if !ok {
            let diagnostic = &validated["diagnostics"][0];
            assert_eq!(diagnostic["code"], "DETERMINISM_UNSUPPORTED");
            assert!(diagnostic["message"]
                .as_str()
                .unwrap()
                .contains("cpu_physics"));
        }
    }
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn add_scaffolds_a_stub_and_a_failing_scenario() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let root = project.to_str().unwrap();

    let (output, result) = run(&["add", "system", root, "spin_coins"]);
    assert!(output.status.success(), "{result}");
    let code = std::fs::read_to_string(project.join("src/main.rs")).unwrap();
    assert!(code.contains("/// TODO") && code.contains("fn spin_coins("));
    assert!(project.join("tests/spin_coins.json").is_file());

    let (output, result) = run(&["add", "system", root, "spin_coins"]);
    assert!(!output.status.success());
    assert_eq!(result["diagnostics"][0]["code"], "PROJECT_EXISTS");

    let (output, result) = run(&["add", "scenario", root, "Bad Name"]);
    assert!(!output.status.success());
    assert_eq!(result["diagnostics"][0]["code"], "CLI_USAGE");

    let (output, result) = run(&["add", "scenario", root, "door_opens"]);
    assert!(output.status.success(), "{result}");
    let scenario: Value = serde_json::from_slice(
        &std::fs::read(project.join("tests/door_opens.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(scenario["steps"][0]["expect"]["exists"], true);
    std::fs::remove_dir_all(parent).ok();
}

#[test]
fn diff_reports_added_removed_and_changed_entities_by_id() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let scene = project.join("scenes/main.rscene");
    let edited = project.join("scenes/edited.rscene");
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    let (output, result) =
        run(&["diff", scene.to_str().unwrap(), scene.to_str().unwrap()]);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["identical"], true);

    let entities = value["entities"].as_array_mut().unwrap();
    let removed = entities.pop().unwrap();
    entities[0]["name"] = "Renamed".into();
    entities.push(serde_json::json!({"id": Uuid::new_v4(), "name": "Fresh"}));
    std::fs::write(&edited, value.to_string()).unwrap();
    let (_, result) =
        run(&["diff", scene.to_str().unwrap(), edited.to_str().unwrap()]);
    let data = &result["data"];
    assert_eq!(data["identical"], false);
    assert_eq!(data["removed"][0]["id"], removed["id"]);
    assert_eq!(data["added"][0]["name"], "Fresh");
    assert_eq!(data["changed"][0]["changes"][0]["path"], "/name");
    assert_eq!(data["changed"][0]["changes"][0]["after"], "Renamed");
    std::fs::remove_dir_all(parent).ok();
}

#[test]
fn scene_map_draws_tile_rows_and_places_entities_on_them() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let scene = project.join("scenes/main.rscene");
    let mut value: Value =
        serde_json::from_slice(&std::fs::read(&scene).unwrap()).unwrap();
    let tile_map = serde_json::json!({"tile_size": 1.0, "rows": ["#..", "###"],
        "tiles": {"#": {"solid": true}}});
    value["entities"] = serde_json::json!([
        {"id": Uuid::new_v4(), "name": "Ground",
         "transform": {"position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
         "components": {"rusting.tile_map": tile_map.to_string()}},
        {"id": Uuid::new_v4(), "name": "Hero",
         "transform": {"position": [1.5, -0.5, 0.0], "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]}}
    ]);
    std::fs::write(&scene, value.to_string()).unwrap();
    let (output, result) = run(&["scene", "map", scene.to_str().unwrap()]);
    assert!(output.status.success(), "{result}");
    let map = &result["data"]["maps"][0];
    assert_eq!(map["rows"], serde_json::json!(["#A.", "###"]));
    assert_eq!(map["legend"][1]["entity"], "Hero");
    std::fs::remove_dir_all(parent).ok();
}

#[test]
#[ignore = "runs a real cargo build of a generated project"]
fn inspect_tick_reports_entity_state_after_that_tick() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let (output, result) = run(&[
        "inspect",
        project.to_str().unwrap(),
        "--tick",
        "5",
        "--entity",
        "Cube",
    ]);
    assert!(output.status.success(), "{result}");
    assert_eq!(result["data"]["tick"], 5);
    assert_eq!(result["data"]["entities"]["Cube"]["name"], "Cube");
    std::fs::remove_dir_all(parent).ok();
}

#[test]
fn serve_answers_json_rpc_lines_with_the_cli_envelope() {
    use std::io::Write;
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let requests = [
        r#"{"jsonrpc":"2.0","id":1,"method":"doctor"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"docs search","params":["scenario"]}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"serve"}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"doctor","params":[1]}"#,
        "not json",
        r#"{"jsonrpc":"2.0","id":5,"method":"shutdown"}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"doctor"}"#,
    ];
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // The request after `shutdown` is never answered.
    assert_eq!(replies.len(), 6, "{replies:?}");
    let (_, cold) = run(&["doctor"]);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[0]["result"]["ok"], true);
    assert_eq!(
        replies[0]["result"]["data"]["engine_version"],
        cold["data"]["engine_version"]
    );
    assert!(replies[0]["result"]["size"]["data_bytes"].is_number());
    assert_eq!(
        replies[1]["result"]["schema_version"],
        cold["schema_version"]
    );
    assert_eq!(replies[2]["error"]["code"], -32601);
    assert_eq!(replies[3]["error"]["code"], -32602);
    assert_eq!(replies[4]["error"]["code"], -32700);
    assert!(replies[5]["result"].is_null());
}

#[test]
fn cli_serve_and_mcp_give_the_same_envelope_for_each_operation() {
    use std::io::Write;
    use std::process::Stdio;

    let project =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/putt_course");
    // Command words, then the rest of the arguments.
    let operations: [(&str, &[&str]); 9] = [
        ("validate", &[]),
        ("lint", &[]),
        ("project inspect", &["--summary"]),
        ("scene inspect", &["scenes/main.rscene", "--limit", "3"]),
        (
            "scene query",
            &["scenes/main.rscene", "--component", "camera"],
        ),
        ("scene inspect", &["missing.rscene"]),
        ("explain", &["SCENE_IO"]),
        ("schema", &["scene"]),
        ("asset list", &[]),
    ];
    // Sends one request line per operation and reads one reply per line.
    let talk = |command: &[&str], lines: Vec<String>| -> Vec<Value> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(command)
            .current_dir(&project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for line in lines {
            writeln!(stdin, "{line}").unwrap();
        }
        drop(stdin);
        let output = child.wait_with_output().unwrap();
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    };
    let served = talk(
        &["serve"],
        operations
            .iter()
            .enumerate()
            .map(|(id, (method, params))| {
                serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
            })
            .collect(),
    );
    let called = talk(
        &["mcp", "."],
        operations
            .iter()
            .enumerate()
            .map(|(id, (method, params))| {
                serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": method.replace(' ', "_"), "arguments": {"args": params}}}).to_string()
            })
            .collect(),
    );
    assert_eq!(served.len(), operations.len());
    assert_eq!(called.len(), operations.len());
    for (id, (method, params)) in operations.iter().enumerate() {
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(method.split(' '))
            .args(*params)
            .arg("--json")
            .current_dir(&project)
            .output()
            .unwrap();
        let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mcp: Value = serde_json::from_str(
            called[id]["result"]["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(served[id]["result"], cli, "serve differs on {method}");
        assert_eq!(mcp, cli, "mcp differs on {method}");
        assert_eq!(
            called[id]["result"]["isError"],
            !cli["ok"].as_bool().unwrap(),
        );
    }
    assert_eq!(served[5]["result"]["ok"], false);
}

#[test]
#[ignore = "runs a real cargo build of a generated project"]
fn debug_session_steps_and_inspects_a_running_game() {
    use std::io::Write;
    use std::process::Stdio;

    let parent = temporary_parent();
    let project = generated_project(&parent);
    let mut child = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .args(["debug", project.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let get = r#""entity": "Cube", "path": "/transform/position/1""#;
    for request in [
        format!(r#"{{"id": 1, "cmd": "get", {get}}}"#),
        r#"{"id": 2, "cmd": "step", "ticks": 30}"#.to_owned(),
        r#"{"id": 3, "cmd": "set", "entity": "Cube", "path": "/transform/position/1", "value": 3.0}"#.to_owned(),
        format!(r#"{{"id": 4, "cmd": "get", {get}}}"#),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 4, "{replies:?}");
    assert_eq!(replies[0]["ok"], true, "{replies:?}");
    assert_eq!(replies[1]["tick"], 29);
    assert_eq!(replies[3]["result"], 3.0);
    std::fs::remove_dir_all(parent).ok();
}

#[test]
fn friction_fixes_for_docs_new_and_scene_query() {
    // `docs search` reports how many matched past its default cut of 10.
    let (_, found) = run(&["docs", "search", "GameScene::"]);
    assert_eq!(found["data"]["matches"].as_array().unwrap().len(), 10);
    assert!(found["data"]["total"].as_u64().unwrap() > 10, "{found}");
    assert!(found["data"]["omitted"]["matches"].as_u64().unwrap() > 0);
    let (_, more) = run(&["docs", "search", "GameScene::", "--limit", "30"]);
    assert_eq!(more["data"]["matches"].as_array().unwrap().len(), 30);
    // A type index and the sample games are docs items.
    let (output, index) = run(&["docs", "show", "api/GameScene"]);
    assert!(output.status.success(), "{index}");
    assert!(index["data"]["text"]
        .as_str()
        .unwrap()
        .contains("set_counter"));
    let (output, sample) = run(&["docs", "show", "sample/sky_hop"]);
    assert!(output.status.success(), "{sample}");
    assert!(sample["data"]["text"].as_str().unwrap().contains("```rust"));

    // `new` ships the agent skill, and `scene query` prints registered
    // components as JSON objects, not as JSON text.
    let parent = temporary_parent();
    let root = generated_project(&parent);
    assert!(root.join("skills/rusting-game/SKILL.md").is_file());
    let scene = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples/hammer_run/scenes/main.rscene");
    let (_, counters) = run(&[
        "scene",
        "query",
        scene.to_str().unwrap(),
        "--name",
        "Goal Count",
    ]);
    let counter =
        &counters["data"]["entities"][0]["components"]["rusting.counter"];
    assert_eq!(counter["name"], "goal", "{counters}");

    let (output, version) = run(&["--version"]);
    assert!(output.status.success(), "{version}");
    assert_eq!(version["data"]["engine_version"], env!("CARGO_PKG_VERSION"));

    // Presets can be limited to a scope, and an unknown scope is refused.
    let main = root.join("scenes/main.rscene");
    let main = main.to_str().unwrap();
    let (_, lit) = run(&[
        "preset",
        "apply",
        main,
        "night",
        "--only",
        "text",
        "--dry-run",
    ]);
    let changes = lit["data"]["patch"]["changes"].as_array().unwrap();
    assert!(
        changes.iter().all(|change| change["path"]
            .as_str()
            .unwrap()
            .contains("rusting.hud_element")),
        "{lit}"
    );
    let (output, bad) =
        run(&["preset", "apply", main, "night", "--only", "sky"]);
    assert!(!output.status.success());
    assert_eq!(bad["diagnostics"][0]["code"], "PRESET_SCOPE_UNKNOWN");

    // A sound clip written relative to the scene, not to assets/, fails
    // validation. A built-in `sfx:` clip needs no file.
    let patch_path = parent.join("cue.json");
    let patch = serde_json::json!({"operations": [{"op": "create", "entity": {
        "name": "Bell", "components": {"rusting.sound_cue":
            {"clip": "../assets/bell.wav", "volume": 1.0, "on_collision": false}}}},
        {"op": "create", "entity": {"name": "Chime", "components": {"rusting.sound_cue":
            {"clip": "sfx:coin 7", "volume": 1.0, "on_collision": false}}}}]});
    std::fs::write(&patch_path, patch.to_string()).unwrap();
    let (output, patched) =
        run(&["scene", "patch", main, patch_path.to_str().unwrap()]);
    assert!(output.status.success(), "{patched}");
    let (output, validated) = run(&["validate", root.to_str().unwrap()]);
    assert!(!output.status.success());
    assert!(
        validated["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("relative to assets/"),
        "{validated}"
    );
    assert!(!validated.to_string().contains("sfx:coin"), "{validated}");
    std::fs::remove_dir_all(parent).unwrap();
}

#[test]
fn a_closed_stdout_ends_output_without_a_panic() {
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rusting"))
        .args(["docs", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
}

#[test]
fn scene_retarget_saves_a_clip_on_the_other_rig() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let scene = project.join("scenes/main.rscene");
    let scene = scene.to_str().unwrap();
    let ids: Vec<String> = (0..4).map(|_| Uuid::new_v4().to_string()).collect();
    let entity = |id: &str,
                  parent: Option<&str>,
                  name: &str,
                  y: f32,
                  components: Value| {
        serde_json::json!({"op": "create", "entity": {
            "id": id, "parent": parent, "name": name,
            "transform": {"position": [0.0, y, 0.0], "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "components": components,
        }})
    };
    let walk = serde_json::json!({"clips": [{"name": "walk", "tracks": [{
        "target": "mixamorig:Hips",
        "property": "Rotation",
        "keys": [{"time": 0.0, "value": [0.0, 0.5, 0.0]}],
    }]}]});
    let patch = serde_json::json!({"operations": [
        entity(&ids[0], None, "Mixamo", 0.0, serde_json::json!({"rusting.animation": walk})),
        entity(&ids[1], Some(&ids[0]), "mixamorig:Hips", 1.0, serde_json::json!({})),
        entity(&ids[2], None, "Knight", 0.0, serde_json::json!({})),
        entity(&ids[3], Some(&ids[2]), "Hips", 2.0, serde_json::json!({})),
    ]});
    let patch_path = parent.join("rigs.json");
    std::fs::write(&patch_path, patch.to_string()).unwrap();
    let (output, built) =
        run(&["scene", "patch", scene, patch_path.to_str().unwrap()]);
    assert!(output.status.success(), "{built}");
    let (output, dry) = run(&[
        "scene",
        "retarget",
        scene,
        "Mixamo",
        "walk",
        "Knight",
        "--dry-run",
    ]);
    assert!(output.status.success(), "{dry}");
    assert_eq!(dry["data"]["patch"]["written"], false);
    let (output, applied) =
        run(&["scene", "retarget", scene, "Mixamo", "walk", "Knight"]);
    assert!(output.status.success(), "{applied}");
    let document = std::fs::read_to_string(scene).unwrap();
    assert!(document.contains(r#"\"target\":\"Hips\""#), "{document}");
    let (output, missing) =
        run(&["scene", "retarget", scene, "Mixamo", "run", "Knight"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(missing["diagnostics"][0]["code"], "RETARGET_FAILED");
    std::fs::remove_dir_all(parent).unwrap();
}

/// Fenced code blocks of the agent guides as `(language, body)`.
fn guide_blocks() -> Vec<(&'static str, &'static str)> {
    let skill = include_str!("../skills/rusting-game/SKILL.md");
    let agents = include_str!("../src/project_agents.md");
    [skill, agents]
        .iter()
        .flat_map(|text| text.split("```").skip(1).step_by(2))
        .map(|block| block.split_once('\n').unwrap())
        .collect()
}

#[test]
fn agent_guide_examples_run_in_a_new_project() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let cli = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(args)
            .arg("--json")
            .current_dir(&project)
            .output()
            .unwrap();
        (
            output.status.success(),
            String::from_utf8(output.stdout).unwrap(),
        )
    };
    let (mut patches, mut scenarios) = (0, 0);
    for (language, body) in guide_blocks() {
        if language != "json" {
            continue;
        }
        if body.contains("\"operations\"") {
            std::fs::write(project.join("patch.json"), body).unwrap();
            let (ok, out) = cli(&[
                "scene",
                "patch",
                "scenes/main.rscene",
                "patch.json",
                "--dry-run",
            ]);
            assert!(ok, "{body}\n{out}");
            patches += 1;
        } else {
            // Scenarios name a guide's example game, so they only parse.
            serde_json::from_str::<rusting_engine::scenario::Scenario>(body)
                .unwrap_or_else(|e| panic!("{body}\n{e}"));
            scenarios += 1;
        }
    }
    assert!(patches > 0 && scenarios > 0);
    // Commands with no placeholder that write nothing must succeed.
    let skill = include_str!("../skills/rusting-game/SKILL.md");
    let agents = include_str!("../src/project_agents.md");
    let mut ran = 0;
    for code in [skill, agents]
        .iter()
        .flat_map(|text| text.split('`').skip(1).step_by(2))
    {
        let Some(args) = code.strip_prefix("rusting ") else {
            continue;
        };
        let args: Vec<&str> = args.split_whitespace().collect();
        let placeholder = args.iter().any(|arg| {
            arg.contains(['<', '[', ','])
                || arg.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                    && !arg.contains('_')
        });
        let quiet = rusting_engine::cli::READ_ONLY_COMMANDS
            .iter()
            .any(|command| args.starts_with(command))
            || args == ["fix", "--dry-run"];
        if placeholder || !quiet || args.iter().any(|a| a.contains("build/")) {
            continue;
        }
        let (ok, out) = cli(&args);
        assert!(ok, "`{code}` fails: {out}");
        ran += 1;
    }
    assert!(ran > 15, "only {ran} commands ran");
    let _ = std::fs::remove_dir_all(&parent);
}

/// Each whole game in the agent guides builds as a project's `main.rs`.
#[test]
#[ignore = "runs real cargo builds of a generated project"]
fn agent_guide_games_build() {
    let parent = temporary_parent();
    let project = generated_project(&parent);
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/cli-games");
    let mut built = 0;
    for (language, body) in guide_blocks() {
        if language != "rust" || !body.contains("rusting_game!(update)") {
            continue;
        }
        std::fs::write(project.join("src/main.rs"), body).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(["check", ".", "--json"])
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{body}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        built += 1;
    }
    assert!(built >= 2, "only {built} games");
    let _ = std::fs::remove_dir_all(&parent);
}

/// Per sample: a scenario and the component whose removal must fail it.
/// `forever_bear_booth` has none: game code alone drives what its scenario
/// checks, so no scene component can be left out.
const SAMPLE_STRENGTH: [(&str, Option<(&str, &str)>); 13] = [
    ("brick_bounce", Some(("first_hit.json", "collider"))),
    ("core_defense", Some(("aimed_shot.json", "collider"))),
    ("crate_keeper", Some(("solve.json", "rusting.tile_map"))),
    ("ember_arena", Some(("ember_burns.json", "collider"))),
    ("forever_bear_booth", None),
    ("hammer_run", Some(("first_gem.json", "rusting.pickup"))),
    ("lantern_grid", Some(("solve.json", "collider"))),
    ("night_vault", Some(("heist.json", "rusting.pickup"))),
    ("putt_course", Some(("sink.json", "rigid_body"))),
    ("sky_hop", Some(("first_coin.json", "rusting.pickup"))),
    ("snake_trail", Some(("crash.json", "rusting.tile_map"))),
    ("target_range", Some(("first_shot.json", "collider"))),
    ("tower_topple", Some(("topple_pyramid.json", "rigid_body"))),
];

/// Every sample game has a fuzzable `tests/invariants.json` whose
/// invariants name entities of its scene, and a strength check.
#[test]
fn every_sample_has_invariants_to_fuzz() {
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    let mut games: Vec<String> = std::fs::read_dir(&samples)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("tests").is_dir())
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    games.sort();
    let named: Vec<_> = SAMPLE_STRENGTH.iter().map(|row| row.0).collect();
    assert_eq!(games, named);
    for (game, strength) in SAMPLE_STRENGTH {
        let root = samples.join(game);
        if let Some((scenario, _)) = strength {
            assert!(root.join("tests").join(scenario).is_file(), "{game}");
        }
        let text = std::fs::read_to_string(root.join("tests/invariants.json"))
            .unwrap();
        let parsed: rusting_engine::scenario::Scenario =
            serde_json::from_str(&text).unwrap();
        assert!(parsed.fuzz.is_some_and(|fuzz| !fuzz.actions.is_empty()));
        let scene =
            std::fs::read_to_string(root.join("scenes/main.rscene")).unwrap();
        let invariants: Value = serde_json::from_str(&text).unwrap();
        let invariants = invariants["invariants"].as_array().unwrap();
        assert!(invariants.len() >= 2, "{game}");
        for invariant in invariants {
            let entity = invariant["entity"].as_str().unwrap();
            assert!(
                scene.contains(&format!("\"name\": \"{entity}\"")),
                "{game} has no entity {entity}"
            );
        }
    }
}

/// Every sample passes `rusting lint`, all its scenarios, 100 fuzz seeds of
/// its invariants, and fails its strength scenario without the component.
#[test]
#[ignore = "builds and runs every sample game for many minutes"]
fn samples_pass_lint_scenarios_fuzz_and_strength_check() {
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/samples");
    for (game, strength) in SAMPLE_STRENGTH {
        let root = samples.join(game);
        let mut runs = vec![
            vec!["lint".to_owned(), ".".to_owned()],
            vec!["test".to_owned(), ".".to_owned()],
            ["fuzz", ".", "tests/invariants.json", "--seeds", "100"]
                .map(str::to_owned)
                .to_vec(),
        ];
        if let Some((scenario, component)) = strength {
            runs.push(vec![
                "test".to_owned(),
                ".".to_owned(),
                format!("tests/{scenario}"),
                "--without".to_owned(),
                component.to_owned(),
            ]);
        }
        for args in runs {
            let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
                .args(&args)
                .arg("--json")
                .current_dir(&root)
                .env("CARGO_TARGET_DIR", &target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{game} {args:?}\n{}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }
}

#[test]
#[ignore = "builds and runs every template and sample game for many minutes"]
fn every_template_and_sample_builds_and_runs_headless() {
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/samples");
    let parent = std::env::temp_dir()
        .join(format!("rusting-templates-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&parent).unwrap();
    let mut roots = Vec::new();
    for template in rusting_engine::project::ProjectTemplate::ALL {
        let name = format!("Template {}", template.name());
        let (output, created) = run(&[
            "new",
            parent.to_str().unwrap(),
            &name,
            "--template",
            template.name(),
        ]);
        assert!(output.status.success(), "{created}");
        roots.push(parent.join(name));
    }
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    roots.extend(SAMPLE_STRENGTH.map(|row| samples.join(row.0)));
    for root in roots {
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(["run", root.to_str().unwrap(), "--ticks", "120", "--json"])
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        let run: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.status.success(), "{}: {run}", root.display());
        assert_eq!(run["data"]["headless_ticks"], 120, "{run}");
    }
    let _ = std::fs::remove_dir_all(parent);
}

/// Exports a game through one preset per supported platform (Linux and
/// Windows, `docs/getting-started.md`). The Linux export runs its own
/// headless check; the Windows one is run under Wine when it is installed.
#[test]
#[ignore = "runs release builds for two platforms"]
fn every_platform_preset_exports_a_game_that_runs() {
    let parent = temporary_parent();
    let (output, created) = run(&[
        "new",
        parent.to_str().unwrap(),
        "Smoke",
        "--template",
        "starter",
    ]);
    assert!(output.status.success(), "{created}");
    let root = parent.join("Smoke");
    let manifest = root.join("project.json");
    let mut project: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap())
            .unwrap();
    project["export_presets"] = serde_json::json!({
        "linux": {},
        "windows": {"target": "x86_64-pc-windows-gnu"},
    });
    std::fs::write(&manifest, project.to_string()).unwrap();
    let target =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("target/export-smoke");
    let export = |preset: &str| {
        let out = parent.join(preset);
        std::fs::create_dir(&out).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_rusting"))
            .args(["export", root.to_str().unwrap(), out.to_str().unwrap()])
            .args(["--preset", preset, "--json"])
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap();
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(output.status.success(), "{preset}: {value}");
        value
    };

    let linux = export("linux");
    assert_eq!(linux["data"]["verified"]["exit_code"], 0, "{linux}");

    let windows = export("windows");
    assert_eq!(windows["diagnostics"][0]["code"], "EXPORT_NOT_VERIFIED");
    let folder =
        PathBuf::from(windows["data"]["export_path"].as_str().unwrap());
    let exe = folder.join("smoke.exe");
    assert!(std::fs::read(&exe).unwrap().starts_with(b"MZ"), "{windows}");
    match Command::new("wine")
        .arg(&exe)
        .current_dir(&folder)
        .env("RUSTING_HEADLESS_TICKS", "60")
        .env("WINEDEBUG", "-all")
        .output()
    {
        Ok(ran) => assert!(
            ran.status.success(),
            "smoke.exe under Wine: {}",
            String::from_utf8_lossy(&ran.stderr)
        ),
        Err(_) => eprintln!("no Wine: smoke.exe was built but not run"),
    }
    let _ = std::fs::remove_dir_all(&parent);
}
