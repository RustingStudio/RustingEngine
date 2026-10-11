//! Agent-friendly window-free engine command line.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusting_engine::cli::{self, CliResult, SceneFilter};
use rusting_engine::project::ProjectTemplate;
use rusting_engine::schema::OPERATIONS;
use uuid::Uuid;

fn help() -> String {
    let mut text = String::from(
        "rusting — RustingEngine project and scene tools\n\nUsage:\n",
    );
    for operation in OPERATIONS {
        text += &format!("  rusting {}\n", operation.usage);
    }
    text + "\nExit codes: 0 success, 1 validation or operation error, 2 usage error.\nJSON output has schema_version, ok, data, and diagnostics.\n"
}

fn help_for(command: &[String]) -> String {
    let topic = command
        .iter()
        .filter(|arg| !arg.starts_with('-'))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let topic = match topic.as_str() {
        "project" => "project inspect",
        "scene" => "scene inspect",
        "asset" => "asset import",
        "add" => "add scenario",
        "preset" => "preset apply",
        "effect" => "effect apply",
        "recipe" => "recipe apply",
        topic => topic,
    };
    match OPERATIONS.iter().find(|operation| operation.name == topic) {
        Some(operation) => format!("{}\n{}\n", help(), operation.summary),
        None => help(),
    }
}

/// Matches `rusting docs search` shows when `--limit` is left out.
const DOCS_SEARCH_LIMIT: usize = 10;

/// Token budget `rusting docs` and `rusting project summary` use when
/// `--budget` is left out.
const DOCS_BUDGET: usize = 2000;
/// How long `rusting lease claim` holds a path without `--for`.
const LEASE_SECONDS: u64 = 1800;

fn docs_command(args: &[&str]) -> CliResult {
    let mut words = Vec::new();
    let mut budget = None;
    let mut brief = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match *arg {
            "--brief" => brief = true,
            "--budget" => match args.next().and_then(|n| n.parse().ok()) {
                Some(tokens) if tokens > 0 => budget = Some(tokens),
                _ => return usage("--budget takes a token count above 0"),
            },
            flag if flag.starts_with("--") => {
                return usage(format!("unknown docs flag `{flag}`"))
            }
            word => words.push(word),
        }
    }
    let budget_or = |default| budget.unwrap_or(default);
    match words.as_slice() {
        [] if brief => {
            cli::docs(cli::DocsAction::Brief(budget_or(DOCS_BUDGET)))
        }
        [] if budget.is_some() => {
            usage("--budget needs `--brief` or `show <id>`")
        }
        [] => cli::docs(cli::DocsAction::List),
        ["search", query @ ..] if !query.is_empty() && !brief => {
            cli::docs(cli::DocsAction::Search(&query.join(" "), usize::MAX))
        }
        ["show", id] if !brief => {
            // Whole page unless `--budget` asks for less.
            cli::docs(cli::DocsAction::Show(id, budget_or(usize::MAX)))
        }
        _ => usage("docs takes `--brief`, `search <words>` or `show <id>`"),
    }
}

fn usage(message: impl Into<String>) -> CliResult {
    CliResult::failure("CLI_USAGE", message, None)
}

/// Inserts `.` for a left-out project root, so `rusting check` works inside
/// a project. `extra` is how many positional arguments follow the root.
fn default_root(mut args: Vec<&str>) -> Vec<&str> {
    const COMMANDS: &[(&[&str], usize)] = &[
        (&["check"], 0),
        (&["inspect"], 0),
        (&["validate"], 0),
        (&["lint"], 0),
        (&["cook"], 0),
        (&["fix"], 0),
        (&["run"], 0),
        (&["determinism"], 0),
        (&["project", "inspect"], 0),
        (&["project", "summary"], 0),
        (&["systems"], 0),
        (&["impact"], 1),
        (&["lease", "list"], 0),
        (&["provenance"], 0),
        (&["log"], 0),
        (&["revert"], 1),
        (&["asset", "list"], 0),
        (&["test"], 1),
        (&["fuzz"], 1),
        (&["add", "scenario"], 1),
        (&["add", "system"], 1),
    ];
    for (command, extra) in COMMANDS {
        if !args.starts_with(command) {
            continue;
        }
        let given = args[command.len()..]
            .iter()
            .take_while(|arg| !arg.starts_with("--"))
            .count();
        let project_only = *command == ["test"]
            && given == 1
            && Path::new(args[1]).join("project.json").is_file();
        if project_only {
            // `rusting test <project>` runs that project's tests folder.
            args.insert(2, "tests");
        } else if given == *extra {
            args.insert(command.len(), ".");
        } else if *command == ["test"] && given == 0 {
            args.splice(1..1, [".", "tests"]);
        }
        break;
    }
    args
}

fn execute(args: &[String]) -> CliResult {
    let started = std::time::Instant::now();
    let result = run_command(args);
    if let Some(log) = std::env::var_os(cli::COMMAND_LOG_ENV) {
        use std::io::Write;
        let line = serde_json::json!({
            "args": args,
            "ok": result.ok,
            "codes": result.diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
            "ms": started.elapsed().as_millis() as u64,
        });
        // A log that cannot be written must not fail the command.
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .and_then(|mut file| writeln!(file, "{line}"));
    }
    result
}

fn run_command(args: &[String]) -> CliResult {
    // Files written by the command are one operation in the journal.
    let _operation = rusting_engine::runtime::journal::begin(&args.join(" "));
    // The environment and every flag each limit commands; a flag can
    // narrow RUSTING_CONFINE but never widen it.
    let mut confines: Vec<String> = std::env::var(cli::CONFINE_ENV)
        .ok()
        .filter(|dir| !dir.is_empty())
        .into_iter()
        .collect();
    let mut words = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" | "--read-only" | "--offline" => {}
            "--confine" => match rest.next() {
                Some(dir) => confines.push(dir.clone()),
                None => return usage("--confine takes a folder"),
            },
            word => words.push(word),
        }
    }
    let positional = default_root(words);
    let read_only = args.iter().any(|arg| arg == "--read-only")
        || std::env::var(cli::READ_ONLY_ENV)
            .is_ok_and(|value| !value.is_empty() && value != "0");
    if read_only {
        if let Some(refusal) = cli::read_only_refusal(&positional) {
            return refusal;
        }
    }
    let mut confine: Option<PathBuf> = None;
    for dir in confines {
        let dir = match Path::new(&dir).canonicalize() {
            Ok(dir) => dir,
            Err(error) => {
                return usage(format!(
                    "--confine needs an existing folder: {error}"
                ))
            }
        };
        confine = match confine {
            Some(outer) if dir.starts_with(&outer) => Some(dir),
            Some(inner) if inner.starts_with(&dir) => Some(inner),
            Some(other) => {
                return usage(format!(
                    "--confine {} is not inside {}, the folder already confining commands",
                    dir.display(),
                    other.display()
                ))
            }
            None => Some(dir),
        };
    }
    if let Some(refusal) = confine
        .as_deref()
        .and_then(|dir| cli::confine_refusal(&positional, dir))
    {
        return refusal;
    }
    rusting_engine::runtime::lease::set_scope(read_only, confine);
    // The engine opens no connection itself; this keeps cargo off the
    // network too. Generator hooks are the project's own commands and may.
    let truthy = |value: String| !value.is_empty() && value != "0";
    rusting_engine::runtime::lease::set_offline(
        args.iter().any(|arg| arg == "--offline")
            || std::env::var(cli::OFFLINE_ENV).is_ok_and(truthy),
    );
    match positional.as_slice() {
        ["--version" | "-V" | "version"] => cli::CliResult::success(
            serde_json::json!({"engine_version": env!("CARGO_PKG_VERSION")}),
        ),
        ["doctor"] => cli::doctor(false),
        ["doctor", "--probe"] => cli::doctor(true),
        ["new", parent, name] => {
            cli::new_project(Path::new(parent), name, ProjectTemplate::Basic3d)
        }
        ["new", parent, name, "--template", template] => {
            match ProjectTemplate::parse(template) {
                Some(template) => {
                    cli::new_project(Path::new(parent), name, template)
                }
                None => usage(
                    "--template takes 3d, first-person, third-person, sandbox, 2d, starter, puzzle, top-down, racing, twin-stick, tower-defense, roguelike, card-game, rhythm, or empty",
                ),
            }
        }
        ["project", "inspect", root] => cli::inspect_project(Path::new(root)),
        ["impact", root, target] => cli::impact(Path::new(root), target),
        ["lease", "list", root] => cli::lease_list(Path::new(root)),
        ["provenance", root] => cli::provenance(Path::new(root)),
        ["log", root] => cli::journal_log(Path::new(root)),
        ["revert", root, op] => cli::revert(Path::new(root), op),
        ["lease", verb @ ("claim" | "release"), path, flags @ ..] => {
            let (mut holder, mut seconds) = (None, Some(LEASE_SECONDS));
            for pair in flags.chunks(2) {
                match pair {
                    ["--as", name] => holder = Some(*name),
                    ["--for", value] if *verb == "claim" => {
                        seconds = value.parse().ok().filter(|s| *s > 0);
                    }
                    _ => return usage("lease takes --as NAME and --for SECONDS"),
                }
            }
            let action = match (*verb, seconds) {
                ("release", _) => cli::LeaseAction::Release,
                (_, Some(seconds)) => cli::LeaseAction::Claim(
                    std::time::Duration::from_secs(seconds),
                ),
                (_, None) => return usage("--for takes seconds above 0"),
            };
            cli::lease(Path::new(path), holder, action)
        }
        ["merge", base, ours, theirs] => cli::merge_scene(
            Path::new(base),
            Path::new(ours),
            Path::new(theirs),
            Path::new(ours),
        ),
        ["merge", base, ours, theirs, "--output", output] => cli::merge_scene(
            Path::new(base),
            Path::new(ours),
            Path::new(theirs),
            Path::new(output),
        ),
        ["systems", root] => cli::system_access(Path::new(root), None, None),
        ["systems", root, "--reads", name] => {
            cli::system_access(Path::new(root), Some(name), None)
        }
        ["systems", root, "--writes", name] => {
            cli::system_access(Path::new(root), None, Some(name))
        }
        ["project", "summary", root] => {
            cli::project_summary(Path::new(root), DOCS_BUDGET)
        }
        ["project", "summary", root, "--budget", tokens] => {
            match tokens.parse() {
                Ok(tokens) if tokens > 0 => {
                    cli::project_summary(Path::new(root), tokens)
                }
                _ => usage("--budget takes a token count above 0"),
            }
        }
        ["scene", "inspect", scene] => cli::inspect_scene(Path::new(scene)),
        ["scene", "map", scene] => cli::map_scene(Path::new(scene)),
        ["scene", "split", scene] => cli::convert_scene(Path::new(scene), true),
        ["scene", "join", scene] => cli::convert_scene(Path::new(scene), false),
        ["scene", "query", scene] => {
            cli::query_scene(Path::new(scene), &SceneFilter::All)
        }
        ["scene", "query", scene, flag, value] => {
            let filter = match *flag {
                "--id" => match Uuid::parse_str(value) {
                    Ok(id) => SceneFilter::Id(id),
                    Err(_) => return usage("--id requires a valid UUID"),
                },
                "--name" => SceneFilter::Name((*value).to_owned()),
                "--class" => SceneFilter::Class((*value).to_owned()),
                "--component" => SceneFilter::Component((*value).to_owned()),
                _ => {
                    return usage(format!(
                        "unknown scene query filter `{flag}`"
                    ))
                }
            };
            cli::query_scene(Path::new(scene), &filter)
        }
        ["scene", "patch", scene, patch] => {
            cli::patch_scene(Path::new(scene), Path::new(patch), false)
        }
        ["scene", "patch", scene, patch, "--dry-run"]
        | ["scene", "patch", scene, "--dry-run", patch] => {
            cli::patch_scene(Path::new(scene), Path::new(patch), true)
        }
        ["scene", "retarget", scene, from, clip, to, flags @ ..] => {
            let dry_run = match flags {
                [] => false,
                ["--dry-run"] => true,
                _ => return usage("`scene retarget` takes only --dry-run"),
            };
            cli::retarget_clip(Path::new(scene), from, clip, to, dry_run)
        }
        ["scene", "add-model", scene, model, flags @ ..] => {
            let mut name = Path::new(model)
                .file_stem()
                .map_or_else(|| "Model".to_owned(), |stem| stem.to_string_lossy().into_owned());
            let mut dry_run = false;
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                match (*flag, flags.clone().next()) {
                    ("--dry-run", _) => dry_run = true,
                    ("--name", Some(value)) => {
                        name = (*value).to_owned();
                        flags.next();
                    }
                    _ => return usage(format!("unknown `scene add-model` flag `{flag}`")),
                }
            }
            #[cfg(feature = "gltf")]
            {
                cli::add_model(Path::new(scene), Path::new(model), &name, dry_run)
            }
            #[cfg(not(feature = "gltf"))]
            {
                let _ = (scene, name, dry_run);
                usage("this build has no glTF support (feature `gltf`)")
            }
        }
        ["determinism", root] => {
            cli::check_game_determinism(Path::new(root), 600)
        }
        ["determinism", root, "--gpu", scenario] => {
            cli::check_scenario_determinism(
                Path::new(root),
                Path::new(scenario),
                true,
            )
        }
        ["determinism", root, "--scenario", scenario] => {
            cli::check_scenario_determinism(
                Path::new(root),
                Path::new(scenario),
                false,
            )
        }
        ["determinism", root, "--ticks", ticks] => match ticks.parse() {
            Ok(ticks) => cli::check_game_determinism(Path::new(root), ticks),
            Err(_) => usage("--ticks requires a tick count"),
        },
        ["validate", root] => cli::validate_project(Path::new(root)),
        ["lint", root] => cli::lint_project(Path::new(root)),
        ["cook", root] => cli::cook_project(Path::new(root)),
        ["add", "scenario", root, name] => {
            cli::add_scenario(Path::new(root), name)
        }
        ["add", "system", root, name] => cli::add_system(Path::new(root), name),
        ["diff", before, after] => {
            cli::diff_scenes(Path::new(before), Path::new(after))
        }
        ["fix", root] => cli::fix_project(Path::new(root), false),
        ["fix", root, "--dry-run"] | ["fix", "--dry-run", root] => {
            cli::fix_project(Path::new(root), true)
        }
        ["docs", rest @ ..] => docs_command(rest),
        ["explain"] => cli::explain(None),
        ["explain", code] => cli::explain(Some(code)),
        ["schema", "--json-schema"] => {
            CliResult::success(rusting_engine::schema::json_schemas())
        }
        ["schema"] => CliResult::success(rusting_engine::schema::catalog()),
        ["schema", name] if !name.starts_with("--") => {
            match rusting_engine::schema::catalog_entry(name) {
                Ok(entry) => CliResult::success(entry),
                Err(error) => usage(error),
            }
        }
        ["inspect", root, flags @ ..] => {
            let (mut tick, mut entities) = (None, Vec::new());
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                match (*flag, flags.next()) {
                    ("--tick", Some(value)) => tick = value.parse().ok(),
                    ("--entity", Some(value)) => entities.push(value.to_string()),
                    _ => return usage("inspect takes --tick N and --entity NAME"),
                }
            }
            match tick {
                Some(tick) => cli::inspect_tick(Path::new(root), tick, &entities),
                None => usage("inspect requires --tick N"),
            }
        }
        ["check", root] => cli::check_project(Path::new(root)),
        ["run", root, flags @ ..] | ["test", root, _, flags @ ..] => {
            let mut options = cli::RunOptions::default();
            let mut full = false;
            if let ["test", _, scenario, ..] = positional.as_slice() {
                // A scenario path is looked up in the project root first.
                let in_root = Path::new(root).join(scenario);
                options.scenario = Some(if in_root.exists() {
                    in_root
                } else {
                    Path::new(scenario).to_path_buf()
                });
            }
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                match *flag {
                    "--release" => options.release = true,
                    "--update-golden" => options.update_golden = true,
                    "--keep-going" => options.keep_going = true,
                    "--full" => full = true,
                    "--stderr" => options.echo_stderr = true,
                    "--server" => options.server = true,
                    "--ticks" => match flags.next().map(|v| v.parse()) {
                        Some(Ok(ticks)) => options.headless_ticks = Some(ticks),
                        _ => return usage("--ticks requires a tick count"),
                    },
                    "--seed" => match flags.next().map(|v| v.parse()) {
                        Some(Ok(seed)) => options.seed = Some(seed),
                        _ => return usage("--seed requires a whole number"),
                    },
                    "--bench" => match flags.next().map(|v| v.parse()) {
                        Some(Ok(frames)) => options.bench = Some(frames),
                        _ => return usage("--bench requires a frame count"),
                    },
                    "--timeout" => match flags.next().map(|v| v.parse()) {
                        Some(Ok(seconds)) => {
                            options.timeout =
                                Some(Duration::from_secs_f64(seconds));
                        }
                        _ => return usage("--timeout requires seconds"),
                    },
                    "--record" => match flags.next() {
                        Some(path) => options.record = Some(path.into()),
                        _ => return usage("--record requires a file"),
                    },
                    "--replay" => match flags.next() {
                        Some(path) => options.replay = Some(path.into()),
                        _ => return usage("--replay requires a file"),
                    },
                    "--without" => match flags.next() {
                        Some(name) => options.without.push(name.to_string()),
                        _ => return usage("--without requires a component name"),
                    },
                    "--exe" => match flags.next() {
                        Some(path) => options.exe = Some(path.into()),
                        _ => return usage("--exe requires a game executable"),
                    },
                    _ => return usage(format!("unknown run flag `{flag}`")),
                }
            }
            let mut result = match &options.scenario {
                Some(folder) if folder.is_dir() => {
                    let folder = folder.clone();
                    cli::test_game_folder(Path::new(root), &folder, options)
                }
                _ => cli::run_game_project(Path::new(root), options),
            };
            if !full {
                drop_state_hashes(&mut result.data);
            }
            result
        }
        ["bisect", first, second, flags @ ..] => {
            let ticks = match flags {
                [] => 600,
                ["--ticks", ticks] => match ticks.parse() {
                    Ok(ticks) => ticks,
                    Err(_) => return usage("--ticks requires a tick count"),
                },
                _ => return usage("bisect flags: --ticks N"),
            };
            cli::bisect_game_projects(Path::new(first), Path::new(second), ticks)
        }
        ["fuzz", root, scenario, flags @ ..] => {
            let (mut first, mut count) = (0_u64, 20_u64);
            let mut actions = Vec::new();
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                let value = flags.next();
                let number = value.and_then(|v| v.parse().ok());
                match (*flag, value, number) {
                    ("--seeds", _, Some(n)) => count = n,
                    ("--first-seed", _, Some(n)) => first = n,
                    ("--action", Some(name), _) => actions.push(name.to_string()),
                    _ => {
                        return usage(
                            "fuzz flags: --seeds N, --first-seed N, --action NAME",
                        )
                    }
                }
            }
            let in_root = Path::new(root).join(scenario);
            let scenario =
                if in_root.exists() { in_root } else { scenario.into() };
            cli::fuzz_game_project(
                Path::new(root),
                &scenario,
                first..first + count,
                &actions,
            )
        }
        ["export", root, parent] => cli::export_game_project(
            Path::new(root),
            Path::new(parent),
            None,
            None,
        ),
        ["export", root, parent, "--target", target] => {
            cli::export_game_project(
                Path::new(root),
                Path::new(parent),
                Some(target),
                None,
            )
        }
        ["export", root, parent, "--preset", preset] => {
            cli::export_game_project(
                Path::new(root),
                Path::new(parent),
                None,
                Some(preset),
            )
        }
        ["preset", "list"] => cli::list_presets(),
        ["preset", "apply", scene, name, flags @ ..] => {
            let mut only = Vec::new();
            let mut dry_run = false;
            let mut flags = flags.iter();
            let mut bad = None;
            while let Some(flag) = flags.next() {
                match (*flag, flags.clone().next()) {
                    ("--dry-run", _) => dry_run = true,
                    ("--only", Some(scopes)) => {
                        only.extend(scopes.split(','));
                        flags.next();
                    }
                    _ => bad = Some(*flag),
                }
            }
            match bad {
                Some(flag) => usage(format!("unknown `preset apply` flag `{flag}`")),
                None => cli::apply_preset(Path::new(scene), name, &only, dry_run),
            }
        }
        ["effect", "list"] => cli::list_effects(),
        ["recipe", "list"] => cli::list_recipes(),
        ["recipe", "apply", root, name, flags @ ..] => match flags {
            [] => cli::apply_recipe(Path::new(root), name, false),
            ["--dry-run"] => cli::apply_recipe(Path::new(root), name, true),
            _ => usage("`recipe apply` takes only --dry-run"),
        },
        ["effect", "apply", scene, name, flags @ ..] => {
            let (mut on, mut object, mut at, mut dry_run) = (None, None, None, false);
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                match (*flag, flags.clone().next()) {
                    ("--dry-run", _) => dry_run = true,
                    ("--on", Some(value)) => {
                        on = Some(*value);
                        flags.next();
                    }
                    ("--name", Some(value)) => {
                        object = Some(*value);
                        flags.next();
                    }
                    ("--at", Some(value)) => {
                        let parts: Vec<f32> =
                            value.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                        let Ok(position) = <[f32; 3]>::try_from(parts) else {
                            return usage("--at takes X,Y,Z");
                        };
                        at = Some(position);
                        flags.next();
                    }
                    _ => return usage(format!("unknown `effect apply` flag `{flag}`")),
                }
            }
            let target = match (on, object, at) {
                (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                    return usage("--on puts the effect on an existing object; leave out --name and --at")
                }
                (Some(on), ..) => cli::EffectTarget::On(on),
                (None, name, at) => cli::EffectTarget::New { name, at },
            };
            cli::apply_effect(Path::new(scene), name, target, dry_run)
        }
        ["asset", "list", root] => cli::list_assets(Path::new(root)),
        ["asset", action @ ("import" | "reimport"), root, target, flags @ ..] =>
        {
            let mut options = cli::AssetImportFlags::default();
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                if *flag == "--dry-run" {
                    options.dry_run = true;
                    continue;
                }
                let Some(value) = flags.next().map(|value| (*value).to_owned())
                else {
                    return usage(format!("`{flag}` needs a value"));
                };
                let source = &mut options.provenance;
                match *flag {
                    "--to" if *action == "import" => {
                        options.folder = value.into()
                    }
                    "--from" if *action == "reimport" => {
                        options.from = Some(value.into());
                    }
                    "--author" => source.author = Some(value),
                    "--license" => source.license = Some(value),
                    "--url" => source.url = Some(value),
                    "--generator" => source.generator = Some(value),
                    "--notes" => source.notes = Some(value),
                    "--max-size" => match value.parse() {
                        Ok(size) => options.max_size = Some(size),
                        Err(_) => return usage("--max-size requires pixels"),
                    },
                    _ => {
                        return usage(format!(
                            "unknown asset {action} flag `{flag}`"
                        ))
                    }
                }
            }
            if *action == "import" {
                cli::import_asset(Path::new(root), Path::new(target), &options)
            } else {
                cli::reimport_asset(Path::new(root), target, &options)
            }
        }
        ["asset", "generate", root, hook, prompt, flags @ ..] => {
            let mut options = cli::AssetImportFlags::default();
            let mut replace = None;
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                match (*flag, flags.clone().next()) {
                    ("--dry-run", _) => {
                        options.dry_run = true;
                        continue;
                    }
                    ("--to", Some(folder)) => {
                        options.folder = folder.into();
                    }
                    ("--replace", Some(asset)) => replace = Some(*asset),
                    _ => {
                        return usage(format!(
                            "unknown or incomplete asset generate flag `{flag}`"
                        ))
                    }
                }
                flags.next();
            }
            cli::generate_asset(
                Path::new(root),
                hook,
                prompt,
                replace,
                &options,
            )
        }
        ["capture", scene, output, flags @ ..] => {
            let mut options = cli::CaptureOptions {
                camera: None,
                tick: 0,
                extent: [1280, 720],
                output: (*output).into(),
                picks: Vec::new(),
                pick_rects: Vec::new(),
                hud: true,
                at: None,
                look_at: None,
                look: None,
            };
            let floats = |value: &str| {
                value
                    .split(',')
                    .map(|v| v.parse::<f32>().ok())
                    .collect::<Option<Vec<_>>>()
                    .unwrap_or_default()
            };
            let pair = |value: &str, separator| {
                let (a, b) = value.split_once(separator)?;
                Some([a.parse().ok()?, b.parse().ok()?])
            };
            let mut game = false;
            let mut flags = flags.iter();
            while let Some(flag) = flags.next() {
                if *flag == "--no-hud" {
                    options.hud = false;
                    continue;
                }
                if *flag == "--game" {
                    game = true;
                    continue;
                }
                let value = flags.next().copied();
                match (*flag, value) {
                    ("--camera", Some(camera)) => {
                        options.camera = Some(camera.to_owned());
                    }
                    ("--tick", Some(tick)) => match tick.parse() {
                        Ok(tick) => options.tick = tick,
                        Err(_) => {
                            return usage("--tick requires a tick number")
                        }
                    },
                    ("--size", Some(size)) => match pair(size, 'x') {
                        Some(extent) => options.extent = extent,
                        None => return usage("--size requires WIDTHxHEIGHT"),
                    },
                    ("--pick", Some(pixel)) => match pair(pixel, ',') {
                        Some(pixel) => options.picks.push(pixel),
                        None => return usage("--pick requires X,Y"),
                    },
                    ("--at", Some(value)) => match floats(value)[..] {
                        [x, y, z] => options.at = Some([x, y, z]),
                        _ => return usage("--at requires X,Y,Z"),
                    },
                    ("--look-at", Some(value)) => match floats(value)[..] {
                        [x, y, z] => options.look_at = Some([x, y, z]),
                        _ => return usage("--look-at requires X,Y,Z"),
                    },
                    ("--look", Some(value)) => match floats(value)[..] {
                        [yaw, pitch] => options.look = Some([yaw, pitch]),
                        _ => return usage("--look requires YAW,PITCH"),
                    },
                    ("--pick-rect", Some(rect)) => {
                        let parts: Vec<u32> =
                            rect.split(',').filter_map(|v| v.parse().ok()).collect();
                        match parts[..] {
                            [x, y, w, h] if rect.split(',').count() == 4 => {
                                options.pick_rects.push([x, y, w, h]);
                            }
                            _ => return usage("--pick-rect requires X,Y,W,H"),
                        }
                    }
                    _ => {
                        return usage(format!(
                            "unknown or incomplete capture flag `{flag}`"
                        ))
                    }
                }
            }
            if options.at.is_none()
                && (options.look_at.is_some() || options.look.is_some())
            {
                return usage("--look-at and --look need --at X,Y,Z");
            }
            if game {
                if !options.picks.is_empty()
                    || !options.pick_rects.is_empty()
                    || options.at.is_some()
                {
                    return usage("--game takes no --pick, --pick-rect or --at");
                }
                return cli::capture_game(Path::new(scene), &options);
            }
            cli::capture_scene(Path::new(scene), &options)
        }
        _ => usage(bad_arguments(&positional)),
    }
}

/// Names a flag the command's usage does not list, and shows that usage.
fn bad_arguments(args: &[&str]) -> String {
    let words = args.iter().take_while(|arg| !arg.starts_with('-'));
    let operation = OPERATIONS
        .iter()
        .filter(|operation| {
            let name: Vec<_> = operation.name.split(' ').collect();
            words.clone().take(name.len()).eq(name.iter())
        })
        .max_by_key(|operation| operation.name.len());
    let Some(operation) = operation else {
        return "invalid command or arguments; run `rusting --help`".into();
    };
    let listed = |flag: &str| {
        operation
            .usage
            .split(|c: char| !(c.is_alphanumeric() || c == '-'))
            .any(|word| word == flag)
    };
    match args
        .iter()
        .find(|arg| arg.starts_with("--") && !listed(arg))
    {
        Some(&"--release") if operation.name == "determinism" => {
            "unknown flag `--release` for `determinism`: it always builds \
             and compares debug and release"
                .into()
        }
        Some(flag) => format!(
            "unknown flag `{flag}` for `{}`; usage: rusting {}",
            operation.name, operation.usage
        ),
        None => format!(
            "invalid arguments for `{}`; usage: rusting {}",
            operation.name, operation.usage
        ),
    }
}

fn render_human(result: &CliResult) -> String {
    let mut lines = Vec::new();
    for run in result
        .data
        .get("scenarios")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        lines.push(format!(
            "{} {}: {}",
            if run["ok"] == true { "PASS" } else { "FAIL" },
            run["file"].as_str().unwrap_or("?"),
            run["message"].as_str().unwrap_or("")
        ));
    }
    // Scenario `log` steps and step warnings, one line each, pass or fail.
    for step in result.data["scenario"]["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            result.data["scenarios"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|run| run["logs"].as_array())
                .flatten(),
        )
    {
        let message = step["message"].as_str().unwrap_or("");
        if let Some(log) = message.strip_prefix("log: ") {
            lines.push(format!("tick {}: {log}", step["tick"]));
        } else if message.contains("; warning: ") {
            lines.push(format!("tick {}: {message}", step["tick"]));
        }
    }
    if result.ok {
        let data = &result.data;
        if let Some(version) = data["engine_version"].as_str() {
            lines.push(format!("RustingEngine {version}"));
        }
        let bench = &data["timings"]["bench"];
        if bench.is_object() {
            let ms = |key: &str| bench[key].as_f64().unwrap_or(0.0);
            lines.push(format!(
                "bench: {} frames, mean {:.2} ms, p50 {:.2}, p95 {:.2}, p99 {:.2}, max {:.2}",
                bench["frames"],
                ms("mean_ms"),
                ms("p50_ms"),
                ms("p95_ms"),
                ms("p99_ms"),
                ms("max_ms")
            ));
            if let Some(bound) = bench["bound"].as_str() {
                lines.push(format!(
                    "{bound}-bound: CPU p50 {:.2} ms, GPU p50 {:.2} ms",
                    ms("cpu_p50_ms"),
                    ms("gpu_p50_ms")
                ));
            }
        }
        if let Some(root) = data.get("root").and_then(|v| v.as_str()) {
            lines.push(format!("Project: {root}"));
        }
        if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
            lines.push(format!("Scene: {path}"));
        }
        for (key, label) in [
            ("binary_name", "Binary"),
            ("manifest_path", "Manifest"),
            ("main_scene", "Main scene"),
            ("cargo_manifest", "Cargo manifest"),
            ("code_path", "Code"),
        ] {
            if let Some(value) = data.get(key).and_then(|v| v.as_str()) {
                lines.push(format!("{label}: {value}"));
            }
        }
        let join = |list: &serde_json::Value| {
            let items: Vec<_> = list
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            items.join(", ")
        };
        for scene in data["scenes"].as_array().into_iter().flatten() {
            if scene["entities"].is_array() {
                lines.push(format!(
                    "Scene {}: {} entities: {}",
                    scene["path"].as_str().unwrap_or("?"),
                    scene["entity_count"],
                    join(&scene["entities"])
                ));
                continue;
            }
            lines.push(format!(
                "Scene {}: {} entities; components {}; assets {}",
                scene["path"].as_str().unwrap_or("?"),
                scene["entities"],
                join(&scene["components"]),
                join(&scene["assets"])
            ));
        }
        for system in data["systems"].as_array().into_iter().flatten() {
            if let Some(stage) = system["stage"].as_str() {
                lines.push(if system["all"] == true {
                    format!(
                        "{stage} {}: whole World",
                        system["name"].as_str().unwrap_or("?")
                    )
                } else {
                    format!(
                        "{stage} {}: reads {}; writes {}",
                        system["name"].as_str().unwrap_or("?"),
                        join(&system["reads"]),
                        join(&system["writes"])
                    )
                });
                if let Some(file) = system["file"].as_str() {
                    if let Some(last) = lines.last_mut() {
                        last.push_str(&format!(" ({file}:{})", system["line"]));
                    }
                }
                continue;
            }
            lines.push(format!(
                "Code {}: {}; assets {}",
                system["file"].as_str().unwrap_or("?"),
                join(&system["functions"]),
                join(&system["assets"])
            ));
        }
        for code in data["code"].as_array().into_iter().flatten() {
            lines.push(format!(
                "Code {}:{}",
                code["file"].as_str().unwrap_or("?"),
                code["line"]
            ));
        }
        for path in data["scenario_files"].as_array().into_iter().flatten() {
            lines.push(format!("Scenario {}", path.as_str().unwrap_or("?")));
        }
        for krate in data["crates"].as_array().into_iter().flatten() {
            lines.push(format!(
                "Crate {} {} {}",
                krate["name"].as_str().unwrap_or("?"),
                krate["version"].as_str().unwrap_or("?"),
                krate["source"].as_str().unwrap_or("(path)")
            ));
        }
        if let Some(hash) = data["code_hash"].as_str() {
            lines.push(format!(
                "Code {hash} ({} files); {} assets, {} scenes, {} scenarios; --json lists each hash",
                data["code_files"],
                data["assets"].as_array().map_or(0, Vec::len),
                data["scene_hashes"].as_array().map_or(0, Vec::len),
                data["scenario_hashes"].as_array().map_or(0, Vec::len)
            ));
        }
        for op in data["journal"].as_array().into_iter().flatten() {
            let files: Vec<_> = op["files"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|file| file["file"].as_str())
                .collect();
            lines.push(format!(
                "{} {} {}: rusting {} ({})",
                op["op"].as_str().unwrap_or("?"),
                op["time"],
                op["tool"].as_str().unwrap_or("?"),
                op["command"].as_str().unwrap_or("?"),
                files.join(", ")
            ));
        }
        let one_lease = data.get("lease").into_iter();
        for lease in
            one_lease.chain(data["leases"].as_array().into_iter().flatten())
        {
            lines.push(format!(
                "Lease {} held by {} until {}",
                lease["path"].as_str().unwrap_or("?"),
                lease["holder"].as_str().unwrap_or("?"),
                lease["expires"]
            ));
        }
        if let Some(omitted) = data["omitted"].as_u64().filter(|n| *n > 0) {
            lines.push(format!(
                "{omitted} list items cut to fit --budget; --json shows what is left"
            ));
        }
        if let Some(version) = data.get("scene_version") {
            lines.push(format!("Scene version: {version}"));
        }
        if let Some(count) = data.get("entity_count") {
            lines.push(format!("Entities: {count}"));
        }
        if let Some(count) = data.get("count") {
            lines.push(format!("Matches: {count}"));
        }
        if let Some(entities) = data.get("entities").and_then(|v| v.as_array())
        {
            for entity in entities {
                lines.push(format!(
                    "{} {}",
                    entity["id"].as_str().unwrap_or("?"),
                    entity["name"].as_str().unwrap_or("(unnamed)")
                ));
            }
        }
        if let Some(fixes) = data.get("fixes").and_then(|v| v.as_array()) {
            let verb = if data["dry_run"] == true {
                "Would fix"
            } else {
                "Fixed"
            };
            if fixes.is_empty() {
                lines.push("No fixes needed.".to_owned());
            }
            for fix in fixes {
                lines.push(format!(
                    "{verb}: {}",
                    fix["message"].as_str().unwrap_or("?")
                ));
            }
        }
        if let Some(revision) = data.get("revision").and_then(|v| v.as_str()) {
            lines.push(format!("Revision: {revision}"));
        }
        if let Some(patch) = data.get("patch") {
            lines.push(format!(
                "{} {} -> {}: {} created, {} deleted, {} field changes",
                if patch["written"] == true {
                    "Patched"
                } else {
                    "Dry run"
                },
                patch["revision_before"].as_str().unwrap_or(""),
                patch["revision_after"].as_str().unwrap_or(""),
                patch["created"].as_array().map_or(0, Vec::len),
                patch["deleted"].as_array().map_or(0, Vec::len),
                patch["changes"].as_array().map_or(0, Vec::len),
            ));
            for change in patch["changes"].as_array().into_iter().flatten() {
                lines.push(format!(
                    "  {}{}: {} -> {}",
                    match (change["name"].as_str(), change["id"].as_str()) {
                        (Some(name), _) => name,
                        (None, Some(id)) if id == Uuid::nil().to_string() => {
                            "(scene)"
                        }
                        (None, id) => id.unwrap_or("?"),
                    },
                    change["path"].as_str().unwrap_or(""),
                    change["before"],
                    change["after"]
                ));
            }
        }
        if let Some(path) = data.get("export_path").and_then(|v| v.as_str()) {
            lines.push(format!("Exported: {path}"));
        }
        if let Some(game) = data.get("game").or(data.get("verified")) {
            if !game.is_null() {
                lines.push(format!(
                    "Game exit code: {}, timed out: {}",
                    game["exit_code"], game["timed_out"]
                ));
            }
        }
        if let Some(path) = data.get("final_scene").and_then(|v| v.as_str()) {
            lines.push(format!(
                "Final state: {path} (inspect with `rusting scene query`)"
            ));
        }
        if let Some(operations) =
            data.get("operations").and_then(|v| v.as_array())
        {
            lines.push(format!(
                "Schema catalog {}: {} operations; use --json for the full catalog",
                data["catalog_version"],
                operations.len()
            ));
            for operation in operations {
                lines.push(format!(
                    "  {}  [GPU: {}]",
                    operation["usage"].as_str().unwrap_or(""),
                    operation["gpu_cost"].as_str().unwrap_or("")
                ));
            }
        }
        if let Some(scenario) =
            data.get("scenario").filter(|v| v.get("passed").is_some())
        {
            lines.push(format!(
                "Scenario {}: {} after {} ticks, seed {}",
                scenario["name"],
                if scenario["passed"] == true {
                    "passed"
                } else {
                    "failed"
                },
                scenario["ticks_run"],
                scenario["seed"]
            ));
        }
        if let Some(picks) = data.get("picks").and_then(|v| v.as_array()) {
            for pick in picks {
                lines.push(format!(
                    "Pick {}: {} {}",
                    pick["pixel"],
                    pick["id"].as_str().unwrap_or("(nothing)"),
                    pick["name"].as_str().unwrap_or("")
                ));
            }
        }
        if let Some(output) = data.get("output").and_then(|v| v.as_str()) {
            lines.push(format!("Output: {output}"));
        }
        if let Some(verdict) = data.get("verdict").and_then(|v| v.as_str()) {
            lines.push(verdict.to_owned());
        }
        if let Some(next) = data.get("next").and_then(|v| v.as_str()) {
            lines.push(format!("Next: {next}"));
        }
        if let Some(vulkan) = data.get("vulkan") {
            lines.push(format!("Vulkan available: {}", vulkan["available"]));
            if let Some(devices) = vulkan["devices"].as_array() {
                for device in devices {
                    lines.push(format!(
                        "Device: {}",
                        device.as_str().unwrap_or("?")
                    ));
                }
            }
        }
        if let Some(probe) = data.get("probe").filter(|probe| !probe.is_null())
        {
            lines.push(match probe["selected_device"].as_str() {
                Some(device) if probe["ok"] == true => {
                    format!("Probe: ok, a test buffer round-trips on {device}")
                }
                _ => format!(
                    "Probe: FAILED: {}",
                    probe["error"].as_str().unwrap_or("unknown error")
                ),
            });
        }
        if let Some(tools) = data.get("build_tools").and_then(|v| v.as_object())
        {
            for (name, available) in tools {
                lines.push(format!("{name}: {available}"));
            }
        }
        for (key, label) in
            [("classes", "Classes"), ("referenced_assets", "Assets")]
        {
            if let Some(items) = data.get(key).and_then(|v| v.as_array()) {
                lines.push(format!(
                    "{label}: {}",
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        for asset in data
            .get("assets")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .chain(data.get("asset"))
        {
            // WAV only: sample rate, channels and length.
            let sound = asset["channels"].as_u64().map_or(String::new(), |n| {
                let channels = match n {
                    1 => "mono".to_owned(),
                    2 => "stereo".to_owned(),
                    n => format!("{n} channels"),
                };
                format!(
                    ", {} Hz, {channels}, {} ms",
                    asset["size"][0], asset["size"][1]
                )
            });
            lines.push(format!(
                "Asset: {} ({}, license {}{sound}){}",
                asset["path"].as_str().unwrap_or("?"),
                asset["id"].as_str().unwrap_or("?"),
                asset["source"]["license"].as_str().unwrap_or("none"),
                if asset["dry_run"] == true {
                    " [preview, nothing written]"
                } else {
                    ""
                }
            ));
        }
        if let Some(cameras) = data.get("cameras").and_then(|v| v.as_array()) {
            for camera in cameras {
                lines.push(format!(
                    "Camera: {} ({})",
                    camera["name"].as_str().unwrap_or("unnamed"),
                    camera["id"].as_str().unwrap_or("?")
                ));
            }
        }
        if let Some(code) = data.get("code").and_then(|v| v.as_str()) {
            lines.push(format!(
                "{code}: {}",
                data["summary"].as_str().unwrap_or("")
            ));
            lines.push(format!("Fix: {}", data["fix"].as_str().unwrap_or("")));
            lines.push(format!(
                "Example: {}",
                data["example"].as_str().unwrap_or("")
            ));
        }
        if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
            lines.push(text.trim_end().to_owned());
        }
        for item in data
            .get("items")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "{}  {}",
                item["id"].as_str().unwrap_or(""),
                item["title"].as_str().unwrap_or("")
            ));
        }
        for hit in data
            .get("matches")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "{}  {}",
                hit["id"].as_str().unwrap_or(""),
                hit["snippet"].as_str().unwrap_or("")
            ));
        }
        if let Some(more) = data["omitted"]["matches"].as_u64() {
            lines.push(format!("... {more} more; add `--limit N` to see them"));
        }
        for info in data
            .get("codes")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            lines.push(format!(
                "{}  {}",
                info["code"].as_str().unwrap_or(""),
                info["summary"].as_str().unwrap_or("")
            ));
        }
        if lines.is_empty() {
            lines.push("OK".to_owned());
        }
    }
    for diagnostic in &result.diagnostics {
        let mut place = diagnostic
            .file
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        for number in [diagnostic.line, diagnostic.column].into_iter().flatten()
        {
            place = format!("{place}:{number}");
        }
        if let Some(pointer) = &diagnostic.scene_location {
            place = format!("{place} at {pointer}");
        }
        if let Some(entity) = &diagnostic.entity {
            place = match &entity.name {
                Some(name) => format!("{place}, object `{name}` {}", entity.id),
                None => format!("{place}, object {}", entity.id),
            };
        }
        let place = place.trim_start_matches([',', ' ']);
        lines.push(format!(
            "{} [{}]: {}{}",
            diagnostic.severity,
            diagnostic.code,
            diagnostic.message,
            if place.is_empty() {
                String::new()
            } else {
                format!(" ({place})")
            }
        ));
    }
    let fixable = result
        .diagnostics
        .iter()
        .filter(|d| d.fix.is_some())
        .count();
    if fixable > 0 {
        lines.push(format!(
            "Run `rusting fix --dry-run` to preview {fixable} certain fix(es), then `rusting fix` to apply them."
        ));
    }
    if let Some(diagnostic) = result
        .diagnostics
        .iter()
        .find(|d| d.severity == "error" && d.code != "CLI_USAGE")
    {
        lines.push(format!(
            "Run `rusting explain {}` for the cause and a fix.",
            diagnostic.code
        ));
    }
    lines.join("\n")
}

/// Removes `--limit N`, `--fields a,b` and `--summary` from the arguments.
fn take_shape(args: Vec<String>) -> Result<(Vec<String>, cli::Shape), String> {
    let mut shape = cli::Shape::default();
    let mut rest = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--limit" => match args.next().and_then(|n| n.parse().ok()) {
                Some(limit) => shape.limit = Some(limit),
                None => return Err("--limit takes an item count".into()),
            },
            "--fields" => match args.next() {
                Some(fields) => {
                    shape.fields =
                        Some(fields.split(',').map(str::to_owned).collect());
                }
                None => return Err("--fields takes a comma list".into()),
            },
            "--summary" => shape.summary = true,
            _ => rest.push(arg),
        }
    }
    // Search finds every match; show the best ten unless `--limit` says.
    if rest.len() >= 2 && rest[0] == "docs" && rest[1] == "search" {
        shape.limit.get_or_insert(DOCS_SEARCH_LIMIT);
    }
    Ok((rest, shape))
}

/// The `--json` form of a result: the envelope plus the size of `data`, so
/// a caller can decide whether to ask for more.
fn envelope(result: &CliResult, args: &[String]) -> serde_json::Value {
    let mut value =
        serde_json::to_value(result).expect("serializable CLI result");
    let bytes = value["data"].to_string().len();
    value["touched"] = rusting_engine::schema::touched(args, result.ok);
    value["size"] = serde_json::json!({
        "data_bytes": bytes,
        "data_tokens": bytes.div_ceil(4),
    });
    value
}

/// One JSON-RPC request line: `method` is the command words and `params`
/// (an array, or `{"args": [...]}`) the rest of its arguments, as on the
/// command line. The result is the same envelope as `--json`.
fn serve_request(line: &str) -> (serde_json::Value, bool) {
    use serde_json::{json, Value};
    let error = |id: Value, code: i32, message: &str| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return (error(Value::Null, -32700, "parse error"), false);
    };
    let id = request["id"].clone();
    let Some(method) = request["method"].as_str() else {
        return (
            error(id, -32600, "invalid request: method is required"),
            false,
        );
    };
    if method == "shutdown" {
        return (json!({"jsonrpc": "2.0", "id": id, "result": null}), true);
    }
    let params = match &request["params"] {
        Value::Null => Some(&[][..]),
        Value::Array(items) => Some(items.as_slice()),
        other => other["args"].as_array().map(Vec::as_slice),
    };
    let strings: Option<Vec<String>> = params.and_then(|items| {
        items
            .iter()
            .map(|v| v.as_str().map(str::to_owned))
            .collect()
    });
    let Some(strings) = strings else {
        return (
            error(id, -32602, "params must be an array of strings"),
            false,
        );
    };
    let mut args: Vec<String> =
        method.split_whitespace().map(str::to_owned).collect();
    args.extend(strings);
    if args.first().is_none_or(|first| first == "serve") {
        return (error(id, -32601, "method not found"), false);
    }
    let outcome = std::panic::catch_unwind(|| {
        let (args, shape) = take_shape(args)?;
        let mut result = execute(&args);
        shape.apply(&mut result.data);
        Ok::<_, String>(envelope(&result, &args))
    });
    let reply = match outcome {
        Ok(Ok(result)) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Ok(Err(message)) => error(id, -32602, &message),
        Err(_) => error(id, -32603, "the operation panicked"),
    };
    (reply, false)
}

/// `rusting serve`: reads one JSON-RPC request per line from stdin and
/// writes one response per line to stdout until `shutdown` or end of input.
fn serve() {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let (reply, stop) = serve_request(&line);
        if writeln!(stdout, "{reply}")
            .and_then(|()| stdout.flush())
            .is_err()
            || stop
        {
            break;
        }
    }
}

/// One MCP request line. Notifications (no `id`) get no reply. `tools/call`
/// runs through `serve_request`, so a tool gives the CLI's envelope.
fn mcp_request(line: &str) -> Option<serde_json::Value> {
    use serde_json::{json, Value};
    let reply = |id: &Value, result: Value| {
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    };
    let fail = |id: &Value, code: i32, message: &str| {
        Some(
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        )
    };
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return fail(&Value::Null, -32700, "parse error");
    };
    let id = &request["id"];
    if id.is_null() {
        return None;
    }
    match request["method"].as_str() {
        Some("initialize") => reply(
            id,
            json!({
                "protocolVersion": request["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "rusting", "version": env!("CARGO_PKG_VERSION")},
            }),
        ),
        Some("ping") => reply(id, json!({})),
        Some("tools/list") => {
            let tools: Vec<Value> = OPERATIONS
                .iter()
                .filter(|op| !matches!(op.name, "serve" | "debug" | "mcp" | "relay"))
                .map(|op| {
                    json!({
                        "name": op.name.replace(' ', "_"),
                        "description": format!("rusting {}\n{}", op.usage, op.summary),
                        "inputSchema": {
                            "type": "object",
                            "properties": {"args": {
                                "type": "array",
                                "items": {"type": "string"},
                                "description": "Arguments after the command words, as on the command line. Paths are relative to the project root.",
                            }},
                        },
                        "annotations": {
                            "readOnlyHint": rusting_engine::schema::READ_ONLY.contains(&op.name),
                        },
                    })
                })
                .collect();
            reply(id, json!({"tools": tools}))
        }
        Some("tools/call") => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or_default();
            let Some(op) = OPERATIONS
                .iter()
                .find(|op| op.name.replace(' ', "_") == name)
                .filter(|op| {
                    !matches!(op.name, "serve" | "debug" | "mcp" | "relay")
                })
            else {
                return fail(id, -32602, "unknown tool");
            };
            let args = params["arguments"]["args"].clone();
            let escapes = args.as_array().is_some_and(|items| {
                items.iter().filter_map(Value::as_str).any(|arg| {
                    let path = Path::new(arg);
                    path.is_absolute()
                        || path.components().any(|c| {
                            matches!(c, std::path::Component::ParentDir)
                        })
                })
            });
            if escapes {
                return fail(
                    id,
                    -32602,
                    "paths must stay inside the project root",
                );
            }
            let call = json!({"id": 1, "method": op.name, "params": if args.is_null() { json!([]) } else { args }});
            let (inner, _) = serve_request(&call.to_string());
            match inner.get("result") {
                Some(result) => reply(
                    id,
                    json!({
                        "content": [{"type": "text", "text": result.to_string()}],
                        "isError": !result["ok"].as_bool().unwrap_or(false),
                    }),
                ),
                None => Some(
                    json!({"jsonrpc": "2.0", "id": id, "error": inner["error"]}),
                ),
            }
        }
        _ => fail(id, -32601, "method not found"),
    }
}

/// `rusting mcp [root]`: Model Context Protocol over stdio, scoped to the
/// project root (the working directory becomes `root`).
fn mcp(root: &str) {
    use std::io::{BufRead, Write};
    if std::env::set_current_dir(root).is_err() {
        eprintln!("cannot enter {root}");
        std::process::exit(1);
    }
    let mut stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = mcp_request(&line) {
            if writeln!(stdout, "{reply}")
                .and_then(|()| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
}

/// Writes a line to standard output. A closed pipe (`rusting ... | head`)
/// ends the output quietly instead of panicking like `println!`.
fn print_line(text: impl std::fmt::Display) {
    use std::io::Write;
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "mcp")
        && !args.iter().any(|arg| arg == "--help" || arg == "-h")
    {
        mcp(args.get(1).map_or(".", String::as_str));
        return;
    }
    if args == ["serve"] {
        serve();
        return;
    }
    if args.first().is_some_and(|arg| arg == "relay")
        && args.len() <= 2
        && !args.iter().any(|arg| arg == "--help" || arg == "-h")
    {
        let address = args.get(1).map_or("0.0.0.0:7777", String::as_str);
        let result =
            std::net::TcpListener::bind(address).and_then(|listener| {
                eprintln!("relay listening on {}", listener.local_addr()?);
                rusting_engine::net::run_relay(
                    listener,
                    &std::env::var("RUSTING_RELAY_TOKEN").unwrap_or_default(),
                )
            });
        if let Err(error) = result {
            eprintln!("relay on {address}: {error}");
            std::process::exit(1);
        }
        return;
    }
    if args.first().is_some_and(|arg| arg == "debug")
        && !args.iter().any(|arg| arg == "--help" || arg == "-h")
    {
        // Standard output belongs to the game's line protocol.
        let root = args.get(1).map_or(".", String::as_str);
        let result = cli::debug_game_project(Path::new(root));
        if !result.ok {
            eprintln!("{}", envelope(&result, &args));
        }
        std::process::exit(result.exit_code());
    }
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h")
    {
        print_line(help_for(&args).trim_end());
        return;
    }
    let json = args.iter().any(|arg| arg == "--json");
    let (args, shape) = match take_shape(args) {
        Ok(taken) => taken,
        Err(message) => {
            let result = usage(message);
            if json {
                print_line(
                    serde_json::to_string(&result).expect("serializable"),
                );
            } else {
                eprintln!("{}", render_human(&result));
            }
            std::process::exit(result.exit_code());
        }
    };
    let mut result = execute(&args);
    shape.apply(&mut result.data);
    if json {
        print_line(envelope(&result, &args));
    } else if result.ok {
        print_line(render_human(&result));
    } else {
        eprintln!("{}", render_human(&result));
    }
    std::process::exit(result.exit_code());
}

/// Every tick's hashes dwarf the rest of a `test` result; they stay in
/// build/scenario-report.json and come back with `--full`.
fn drop_state_hashes(data: &mut serde_json::Value) {
    if let Some(scenario) =
        data.get_mut("scenario").and_then(|s| s.as_object_mut())
    {
        scenario.remove("state_hashes");
        scenario.remove("gpu_state_hashes");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_flag_is_named_with_the_command_usage() {
        let result = execute(&["determinism".into(), "--release".into()]);
        assert_eq!(result.exit_code(), 2);
        let message = &result.diagnostics[0].message;
        assert!(message.contains("always builds and compares"), "{message}");
        let result = execute(&["check".into(), "--fast".into()]);
        let message = &result.diagnostics[0].message;
        assert!(
            message.starts_with(
                "unknown flag `--fast` for `check`; usage: rusting check ["
            ),
            "{message}"
        );
        let result = execute(&["determinism".into(), "--ticks".into()]);
        let message = &result.diagnostics[0].message;
        assert!(
            message.starts_with("invalid arguments for `determinism`"),
            "{message}"
        );
        let result = execute(&["nonsense".into()]);
        assert!(result.diagnostics[0].message.starts_with("invalid command"));
        // Accepted: the run fails later, on the missing project.
        let result = execute(&[
            "test".into(),
            "/no/such/project".into(),
            "a.json".into(),
            "--keep-going".into(),
        ]);
        assert_ne!(result.diagnostics[0].code, "CLI_USAGE");
    }

    #[test]
    fn test_results_leave_the_state_hashes_out() {
        use serde_json::json;
        let mut data = json!({"scenario": {"passed": true,
            "state_hashes": [[0, 1]], "gpu_state_hashes": [], "trace": []}});
        drop_state_hashes(&mut data);
        assert_eq!(data, json!({"scenario": {"passed": true, "trace": []}}));
        let mut data = json!(null);
        drop_state_hashes(&mut data);
        assert_eq!(data, json!(null));
    }

    #[test]
    fn mcp_lists_tools_and_calls_them_like_the_cli() {
        use serde_json::{json, Value};
        let init = mcp_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "rusting");
        assert!(mcp_request(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
        let list =
            mcp_request(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#)
                .unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        let tool =
            |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
        assert_eq!(tool("scene_query")["annotations"]["readOnlyHint"], true);
        assert_eq!(tool("scene_patch")["annotations"]["readOnlyHint"], false);
        assert!(tools.iter().all(|t| !matches!(
            t["name"].as_str(),
            Some("serve" | "debug" | "mcp" | "relay")
        )));
        let call = |args: Value| {
            let line = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"schema","arguments":{"args":args}}});
            mcp_request(&line.to_string()).unwrap()
        };
        let ok = call(json!(["--json"]));
        let text: Value = serde_json::from_str(
            ok["result"]["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(text["ok"], true);
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(call(json!(["../x"]))["error"]["code"], -32602);
        assert_eq!(call(json!(["/etc"]))["error"]["code"], -32602);
    }

    #[test]
    fn usage_has_json_error_and_exit_code() {
        let result = execute(&[
            "scene".into(),
            "query".into(),
            "missing.rscene".into(),
            "--id".into(),
            "bad".into(),
            "--json".into(),
        ]);
        assert_eq!(result.exit_code(), 2);
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value["ok"], false);
        assert_eq!(value["diagnostics"][0]["code"], "CLI_USAGE");
    }

    #[test]
    fn explain_describes_a_code_and_suggests_similar_ones_for_a_typo() {
        let args = |list: &[&str]| -> Vec<String> {
            list.iter().map(|arg| (*arg).to_owned()).collect()
        };
        let known = execute(&args(&["explain", "scene_conflict"]));
        assert!(known.ok);
        assert_eq!(known.data["code"], "SCENE_CONFLICT");
        assert!(render_human(&known).contains("Fix: "));
        let listed = execute(&args(&["explain"]));
        assert_eq!(
            listed.data["codes"].as_array().unwrap().len(),
            rusting_engine::diagnostics::CODES.len()
        );
        let typo = execute(&args(&["explain", "scene_conflicts"]));
        assert_eq!(typo.exit_code(), 2);
        assert!(typo.diagnostics[0].message.contains("SCENE_CONFLICT,"));
        // A failed command points at `explain` for its first error.
        let failed = CliResult::failure("SCENE_IO", "missing", None);
        assert!(render_human(&failed).ends_with(
            "Run `rusting explain SCENE_IO` for the cause and a fix."
        ));
    }

    #[test]
    fn plain_output_lists_scenario_log_values_even_on_failure() {
        let mut failed = CliResult::failure("SCENARIO_FAILED", "tick 9", None);
        failed.data = serde_json::json!({"scenario": {"steps": [
            {"tick": 3, "message": "log: Player /transform is [1, 2, 3]"},
            {"tick": 4, "message": "pressed jump"},
        ]}});
        let text = render_human(&failed);
        assert!(text.starts_with("tick 3: Player /transform is [1, 2, 3]\n"));
        assert!(!text.contains("pressed jump"), "{text}");
    }

    #[test]
    fn project_root_defaults_to_the_current_folder() {
        assert_eq!(default_root(vec!["check"]), ["check", "."]);
        assert_eq!(
            default_root(vec!["run", "--ticks", "5"]),
            ["run", ".", "--ticks", "5"]
        );
        assert_eq!(default_root(vec!["run", "game"]), ["run", "game"]);
        assert_eq!(
            default_root(vec!["test", "a.json"]),
            ["test", ".", "a.json"]
        );
        assert_eq!(
            default_root(vec!["test", "game", "a.json"]),
            ["test", "game", "a.json"]
        );
        assert_eq!(
            default_root(vec!["test", "samples/putt_course"]),
            ["test", "samples/putt_course", "tests"]
        );
        assert_eq!(
            default_root(vec!["test", "--release"]),
            ["test", ".", "tests", "--release"]
        );
        assert_eq!(
            default_root(vec!["project", "inspect"]),
            ["project", "inspect", "."]
        );
    }
}
