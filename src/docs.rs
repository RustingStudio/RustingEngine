//! Offline documentation for the installed engine version, served by
//! `rusting docs`: the manual, tutorials, the agent guides, every command
//! and every diagnostic code, as one list of items an agent can search and
//! read within a token budget.
//!
//! Manual pages are embedded at build time, so the text always matches the
//! binary that prints it.

use serde_json::{json, Value};

use crate::diagnostics::CODES;
use crate::schema::OPERATIONS;

const PAGES: &[(&str, &str, &str)] = &[
    (
        "manual/getting-started",
        "manual",
        include_str!("../docs/getting-started.md"),
    ),
    (
        "manual/concepts",
        "manual",
        include_str!("../docs/concepts.md"),
    ),
    (
        "manual/scripting",
        "manual",
        include_str!("../docs/scripting.md"),
    ),
    (
        "manual/determinism",
        "manual",
        include_str!("../docs/determinism.md"),
    ),
    (
        "manual/physics",
        "manual",
        include_str!("../docs/physics.md"),
    ),
    ("manual/input", "manual", include_str!("../docs/input.md")),
    ("manual/assets", "manual", include_str!("../docs/assets.md")),
    ("manual/2d", "manual", include_str!("../docs/2d.md")),
    (
        "manual/dev-environment",
        "manual",
        include_str!("../docs/dev-environment.md"),
    ),
    (
        "tutorial/hello-cube",
        "tutorial",
        include_str!("../docs/tutorials/01-hello-cube.md"),
    ),
    (
        "tutorial/coin-run-cli",
        "tutorial",
        include_str!("../docs/tutorials/02-coin-run-cli.md"),
    ),
    (
        "tutorial/gameplay-plugin",
        "tutorial",
        include_str!("../docs/tutorials/03-gameplay-plugin.md"),
    ),
    (
        "tutorial/gpu-cube-rain",
        "tutorial",
        include_str!("../docs/tutorials/04-gpu-cube-rain.md"),
    ),
    (
        "tutorial/physics-sandbox",
        "tutorial",
        include_str!("../docs/tutorials/05-physics-sandbox.md"),
    ),
    (
        "tutorial/multiplayer-physics",
        "tutorial",
        include_str!("../docs/tutorials/06-multiplayer-physics.md"),
    ),
    // The agent guides come first, so `rusting docs --brief` lists them.
    (
        "guide/agent-skill",
        "guide",
        include_str!("../skills/rusting-game/SKILL.md"),
    ),
    (
        "guide/project-agents",
        "guide",
        include_str!("project_agents.md"),
    ),
    (
        "guide/gpu-condition-shaders",
        "guide",
        include_str!("../docs/gpu-condition-shaders.md"),
    ),
    ("guide/audio", "guide", include_str!("../docs/audio.md")),
    ("guide/cameras", "guide", include_str!("../docs/cameras.md")),
    (
        "guide/look-and-feel",
        "guide",
        include_str!("../docs/look-and-feel.md"),
    ),
    (
        "guide/menus-and-ui",
        "guide",
        include_str!("../docs/menus-and-ui.md"),
    ),
    ("guide/effects", "guide", include_str!("../docs/effects.md")),
    (
        "guide/animation",
        "guide",
        include_str!("../docs/animation.md"),
    ),
    (
        "guide/lighting",
        "guide",
        include_str!("../docs/lighting.md"),
    ),
    (
        "guide/pitfalls",
        "guide",
        include_str!("../docs/pitfalls.md"),
    ),
    (
        "guide/networking",
        "guide",
        include_str!("../docs/networking.md"),
    ),
    (
        "guide/godot-migration",
        "guide",
        include_str!("../docs/godot-migration.md"),
    ),
    (
        "guide/plugin-authoring",
        "guide",
        include_str!("../docs/plugin-authoring.md"),
    ),
    (
        "guide/surface-shaders",
        "guide",
        include_str!("../docs/surface-shaders.md"),
    ),
    (
        "guide/platforms",
        "guide",
        include_str!("../docs/platforms.md"),
    ),
    (
        "guide/procedural",
        "guide",
        include_str!("../docs/procedural.md"),
    ),
    ("guide/modding", "guide", include_str!("../docs/modding.md")),
];

/// Sample games shipped with the engine: name, README and game code.
/// Read from the copies in `docs/samples/` (made by `scripts/sync_sample_docs.sh`),
/// because each sample is its own Cargo package and is left out of the published crate.
macro_rules! samples {
    ($($name:literal),* $(,)?) => {
        &[$((
            $name,
            include_str!(concat!("../docs/samples/", $name, "/README.md")),
            include_str!(concat!("../docs/samples/", $name, "/main.rs")),
        )),*]
    };
}

const SAMPLES: &[(&str, &str, &str)] = samples![
    "brick_bounce",
    "core_defense",
    "crate_keeper",
    "ember_arena",
    "forever_bear_booth",
    "hammer_run",
    "lantern_grid",
    "night_vault",
    "putt_course",
    "sky_hop",
    "snake_trail",
    "target_range",
    "tower_topple",
];

/// One readable entry.
pub struct DocItem {
    /// `kind/name`, as `rusting docs show` takes it.
    pub id: String,
    /// `manual`, `tutorial`, `guide`, `command`, `code`, `api`,
    /// `component`, `resource` or `asset`.
    pub kind: &'static str,
    pub title: String,
    /// One line for listings and the brief.
    pub summary: String,
    pub text: String,
}

/// Rough token count: four characters to a token, rounded up.
#[must_use]
pub fn tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

fn first_sentence(text: &str) -> String {
    // A stop ends the sentence unless it is part of `..`.
    let end = text
        .match_indices(['.'])
        .map(|(index, _)| index)
        .find(|&index| {
            text[index + 1..].starts_with([' ', '\n'])
                && !text[..index].ends_with('.')
        })
        .map_or(text.len(), |index| index + 1);
    text[..end].trim().replace('\n', " ")
}

fn page(id: &str, kind: &'static str, text: &str) -> DocItem {
    let text = text.trim_start_matches('\u{feff}');
    let title = text
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .unwrap_or(id)
        .trim()
        .to_owned();
    // A guide's front matter carries its own description; otherwise the
    // summary is the first sentence of the first plain paragraph.
    let described = text.strip_prefix("---").and_then(|rest| {
        let front = rest.split("\n---").next()?;
        front
            .lines()
            .find_map(|line| line.strip_prefix("description:"))
            .map(|description| first_sentence(description.trim()))
    });
    let summary = described.unwrap_or_else(|| {
        let paragraph: Vec<&str> = text
            .lines()
            .map(str::trim)
            .skip_while(|line| {
                line.is_empty()
                    || line.starts_with(['#', '`', '-', '|', '>', '<', '!'])
            })
            .take_while(|line| !line.is_empty())
            .collect();
        first_sentence(&paragraph.join(" "))
    });
    DocItem {
        id: id.to_owned(),
        kind,
        title,
        summary,
        text: text.to_owned(),
    }
}

/// Every scenario file field and step kind on one page, from the schema
/// catalog, so it cannot drift from what `rusting test` reads.
fn scenario_reference() -> DocItem {
    let section = crate::schema::catalog_entry("scenario")
        .expect("the catalog has a scenario section");
    let line = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut text = String::from(
        "# Scenario file reference\n\nEvery field of a `rusting test` \
         scenario file and every step kind, generated from \
         `rusting schema scenario`.\n",
    );
    for (key, value) in section.as_object().into_iter().flatten() {
        text += &format!("\n## {key}\n\n");
        match value {
            Value::Object(fields) => {
                for (name, field) in fields {
                    text += &format!("- `{name}`: {}\n", line(field));
                }
            }
            other => text += &format!("{}\n", line(other)),
        }
    }
    page("reference/scenario", "reference", &text)
}

/// One item per entity section, registered component, resource and asset
/// type, generated from the `rusting schema` catalog, so the reference has
/// every reflected field with its unit, range and doc.
fn reflected_items() -> Vec<DocItem> {
    let catalog = crate::schema::catalog();
    let scene = &catalog["scene"];
    let groups = [
        ("component", &scene["entity_sections"]),
        ("component", &scene["components"]),
        ("resource", &catalog["resources"]),
        ("asset", &catalog["asset_types"]),
    ];
    let mut items = Vec::new();
    for (kind, group) in groups {
        for entry in group.as_array().into_iter().flatten() {
            let key = entry["key"].as_str().unwrap_or_default();
            let summary = entry["summary"].as_str().map_or_else(
                || format!("The `{key}` {kind} type and its fields."),
                str::to_owned,
            );
            let mut text = format!("# {key}\n\n{summary}\n");
            if let Some(gpu) = entry["gpu_cost"].as_str() {
                text += &format!("\nGPU: {gpu}\n");
            }
            text += "\nFields:\n\n";
            for field in entry["fields"].as_array().into_iter().flatten() {
                let path = field["path"].as_str().unwrap_or_default();
                text += &format!(
                    "- `{}`",
                    if path.is_empty() { "/" } else { path }
                );
                let hints: Vec<&str> = ["unit", "range"]
                    .iter()
                    .filter_map(|hint| field[hint].as_str())
                    .filter(|hint| !hint.is_empty())
                    .collect();
                if !hints.is_empty() {
                    text += &format!(" ({})", hints.join(", "));
                }
                match field["doc"].as_str() {
                    Some(doc) if !doc.is_empty() => {
                        text += &format!(": {doc}\n")
                    }
                    _ => text.push('\n'),
                }
            }
            for part in ["default", "example"] {
                if !entry[part].is_null() {
                    text += &format!(
                        "\n{}{}: `{}`\n",
                        part[..1].to_uppercase(),
                        &part[1..],
                        entry[part]
                    );
                }
            }
            items.push(DocItem {
                id: format!("{kind}/{key}"),
                kind,
                title: key.to_owned(),
                summary: first_sentence(&summary),
                text,
            });
        }
    }
    items
}

/// Every item, in the order the brief lists them.
#[must_use]
pub fn items() -> Vec<DocItem> {
    let mut items: Vec<DocItem> = PAGES
        .iter()
        .map(|(id, kind, text)| page(id, kind, text))
        .collect();
    for operation in OPERATIONS {
        let mut text = format!(
            "rusting {}\n\n{}\n\nGPU: {}\n",
            operation.usage, operation.summary, operation.gpu
        );
        for (flag, value) in operation.defaults {
            text += &format!("Default {flag}: {value}\n");
        }
        text += &format!("\nExample: rusting {}\n", operation.example);
        items.push(DocItem {
            id: format!("command/{}", operation.name.replace(' ', "-")),
            kind: "command",
            title: format!("rusting {}", operation.usage),
            summary: first_sentence(operation.summary),
            text,
        });
    }
    for info in CODES {
        items.push(DocItem {
            id: format!("code/{}", info.code),
            kind: "code",
            title: info.code.to_owned(),
            summary: info.summary.to_owned(),
            text: format!(
                "{}\n\n{}\n\nFix: {}\n\nExample: {}\n",
                info.code, info.summary, info.fix, info.example
            ),
        });
    }
    items.push(scenario_reference());
    items.extend(reflected_items());
    for (name, source) in crate::cookbook::COOKBOOK {
        // The snippet's `//!` header is the page; the code follows it.
        let header: Vec<&str> = source
            .lines()
            .map_while(|line| line.strip_prefix("//!"))
            .map(str::trim_start)
            .collect();
        let mut item = page(
            &format!("cookbook/{name}"),
            "cookbook",
            &format!("# {}", header.join("\n")),
        );
        item.summary.clone_from(&item.title);
        item.text += &format!("\n\n```rust\n{source}```\n");
        items.push(item);
    }
    for (name, readme, code) in SAMPLES {
        let mut item = page(&format!("sample/{name}"), "sample", readme);
        item.text += &format!("\n## src/main.rs\n\n```rust\n{code}```\n");
        items.push(item);
    }
    let api = api_items();
    for owner in API_TYPES {
        let methods: Vec<&DocItem> = api
            .iter()
            .filter(|item| item.title.split("::").next() == Some(owner))
            .collect();
        // A struct with public fields lists them above its methods.
        let fields = if API_SOURCE.contains(&format!("pub struct {owner} {{")) {
            struct_item(owner, API_SOURCE).text + "\n"
        } else {
            format!("# {owner}\n\n")
        };
        let mut text = format!(
            "{fields}Every public method, {} in all. \
             `rusting docs show api/{owner}::<method>` prints one.\n\n",
            methods.len()
        );
        for item in &methods {
            text += &format!("- {}: {}\n", item.title, item.summary);
        }
        items.push(DocItem {
            id: format!("api/{owner}"),
            kind: "api",
            title: owner.to_owned(),
            summary: format!("Index of every public {owner} method."),
            text,
        });
    }
    items.extend(api);
    for (module, source) in API_MODULES {
        items.extend(module_items(module, source));
    }
    for name in ["PlayerController", "PlayerInput", "PlayerShove"] {
        items.push(struct_item(name, include_str!("runtime/player.rs")));
    }
    items.push(struct_item(
        "GpuStateField",
        include_str!("runtime/hybrid_physics.rs"),
    ));
    items.push(struct_item("RayHit", include_str!("project_runner.rs")));
    items.push(struct_item(
        "GpuBodySettings",
        include_str!("project_runner.rs"),
    ));
    items.push(struct_item(
        "GpuConditionShader",
        include_str!("runtime/hybrid_physics.rs"),
    ));
    items.push(struct_item("MaterialAsset", include_str!("assets/mod.rs")));
    items.push(struct_item("Sound", include_str!("runtime/audio.rs")));
    items.push(struct_item(
        "ParticleEmitter",
        include_str!("runtime/particles.rs"),
    ));
    items.push(struct_item(
        "Animation",
        include_str!("runtime/animation.rs"),
    ));
    items.push(struct_item(
        "AnimationEvent",
        include_str!("runtime/animation.rs"),
    ));
    items.push(struct_item("Skin", include_str!("runtime/skinning.rs")));
    items.push(struct_item("Morph", include_str!("runtime/skinning.rs")));
    items.push(struct_item("Ik", include_str!("runtime/ik.rs")));
    items.push(struct_item("Ragdoll", include_str!("runtime/ragdoll.rs")));
    items.push(struct_item(
        "HudElement",
        include_str!("runtime/game_feel.rs"),
    ));
    items.push(struct_item(
        "HudAnchor",
        include_str!("runtime/game_feel.rs"),
    ));
    items.push(struct_item(
        "PhysicsSyncMode",
        include_str!("runtime/hybrid_physics.rs"),
    ));
    items.push(struct_item("BusEffect", include_str!("runtime/audio.rs")));
    items
}

/// `api/{name}`: the struct's doc comment and every public field with its
/// type and doc, or the enum's and every variant, read from `source`.
fn struct_item(name: &str, source: &str) -> DocItem {
    // The name may be followed by generics, a body, `;` or a tuple.
    let declared = |keyword: &str| {
        let head = format!("pub {keyword} {name}");
        source.match_indices(&head).map(|(at, _)| at).find(|&at| {
            source[at + head.len()..].starts_with([' ', '<', ';', '('])
        })
    };
    let enum_start = declared("enum");
    let start = enum_start
        .or_else(|| declared("struct"))
        .expect("type is in its source file");
    let head = &source[..start];
    let doc: Vec<&str> = head
        .lines()
        .rev()
        // Attributes, possibly over several lines, sit between the doc and
        // the type; an undocumented type stops at the blank line or the end
        // of the previous item rather than taking that item's doc.
        .skip_while(|line| {
            !line.starts_with("///") && !line.is_empty() && *line != "}"
        })
        .take_while(|line| line.starts_with("///"))
        .filter_map(|line| line.strip_prefix("///"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let list = if enum_start.is_some() {
        "Variants"
    } else {
        "Fields"
    };
    let mut text = format!("# {name}\n\n{}\n", doc.join(" "));
    let mut entries = String::new();
    let mut field_doc: Vec<&str> = Vec::new();
    // A unit or tuple struct has no body to list.
    let has_body = source[start..]
        .lines()
        .next()
        .is_some_and(|line| line.trim_end().ends_with('{'));
    let mut lines = source[start..].lines().skip(1).map(str::trim);
    while let Some(line) = lines.next().filter(|_| has_body) {
        if line == "}" {
            break;
        }
        if let Some(doc) = line.strip_prefix("///") {
            field_doc.push(doc.trim());
        } else if let Some(field) = line.strip_prefix("pub ").or(enum_start
            .map(|_| line)
            .filter(|line| !line.starts_with('#')))
        {
            // A variant with named fields spans lines up to its `}`.
            let mut field = field.to_owned();
            if enum_start.is_some() && field.ends_with('{') {
                for inner in lines.by_ref() {
                    if inner.starts_with("///") {
                        continue;
                    }
                    field.push(' ');
                    field.push_str(inner);
                    if inner.starts_with('}') {
                        break;
                    }
                }
                field = field.replace(", }", " }");
            }
            let field = field.trim_end_matches(',');
            entries += &format!("- `{field}`");
            if !field_doc.is_empty() {
                entries += &format!(": {}", field_doc.join(" "));
            }
            entries.push('\n');
            field_doc.clear();
        }
    }
    if !entries.is_empty() {
        text += &format!("\n{list}:\n\n{entries}");
    }
    DocItem {
        id: format!("api/{name}"),
        kind: "api",
        title: name.to_owned(),
        summary: first_sentence(&doc.join(" ")),
        text,
    }
}

/// Types whose public methods make the gameplay API index.
const API_TYPES: [&str; 8] = [
    "GameScene",
    "GameObject",
    "CubeSpawn",
    "SphereSpawn",
    "GpuCondition",
    "GpuFieldCondition",
    "BeatClock",
    "WaypointGraph",
];

/// Source files the API index reads.
const API_SOURCE: &str = concat!(
    include_str!("project_runner.rs"),
    include_str!("runtime/hybrid_physics.rs"),
    include_str!("runtime/audio.rs"),
    include_str!("runtime/waypoints.rs")
);

/// Modules whose public items get `api/` pages: `api/<module>` lists them,
/// each type gets a page with its fields and methods, and each free
/// function, constant and type alias a page of its own.
const API_MODULES: [(&str, &str); 5] = [
    ("net", include_str!("net.rs")),
    ("net::predict", include_str!("net/predict.rs")),
    ("net::input", include_str!("net/input.rs")),
    ("net::interpolate", include_str!("net/interpolate.rs")),
    ("procgen", include_str!("procgen.rs")),
];

/// The `api/` pages of one of [`API_MODULES`]: the module page, read from
/// its `//!` doc, then a page per public item at the top of `source`.
fn module_items(module: &str, source: &str) -> Vec<DocItem> {
    let source = source.split("\n#[cfg(test)]").next().unwrap_or(source);
    let see = format!(
        "\nPart of `rusting_engine::{module}`; \
         `rusting docs show api/{module}` has the module's notes.\n"
    );
    let mut items = Vec::new();
    let mut types = Vec::new();
    let mut docs: Vec<&str> = Vec::new();
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        if let Some(doc) = line.strip_prefix("///") {
            docs.push(doc.strip_prefix(' ').unwrap_or(doc));
            continue;
        }
        // Attributes and plain comments sit between a doc and its item.
        if line.starts_with("#[") || line.starts_with("//") {
            continue;
        }
        let keyword = ["fn", "type", "const", "struct", "enum"]
            .into_iter()
            .find(|keyword| line.starts_with(&format!("pub {keyword} ")));
        match keyword {
            Some("struct" | "enum") => {
                let name = line.split_whitespace().nth(2).unwrap_or_default();
                types.push(name.split(['<', '(', ';', '{']).next().unwrap());
            }
            Some(_) => {
                let mut signature = line.trim().to_owned();
                while !signature.ends_with(['{', ';']) {
                    let Some(next) = lines.next() else { break };
                    signature.push(' ');
                    signature.push_str(next.trim());
                }
                let signature = signature
                    .trim_end_matches(['{', ';'])
                    .trim()
                    .replace("( ", "(")
                    .replace(", )", ")");
                let name = signature.split_whitespace().nth(2).unwrap_or("");
                let name = name.split(['(', '<', ':']).next().unwrap();
                let text = docs.join("\n");
                items.push(DocItem {
                    id: format!("api/{name}"),
                    kind: "api",
                    title: name.to_owned(),
                    summary: first_sentence(text.trim()),
                    text: format!("{signature}\n\n{}\n{see}", text.trim()),
                });
            }
            None => {}
        }
        docs.clear();
    }
    let methods = methods_of(&types, source);
    for name in &types {
        let mut item = struct_item(name, source);
        let own: Vec<&DocItem> = methods
            .iter()
            .filter(|item| item.title.split("::").next() == Some(name))
            .collect();
        if !own.is_empty() {
            item.text += "\nMethods:\n\n";
            for method in own {
                item.text +=
                    &format!("- {}: {}\n", method.title, method.summary);
            }
        }
        item.text += &see;
        items.push(item);
    }
    items.extend(methods.into_iter().map(|mut method| {
        method.text += &see;
        method
    }));
    let doc: Vec<&str> = source
        .lines()
        .map_while(|line| line.strip_prefix("//!"))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect();
    let mut text = format!(
        "# {module}\n\n{}\n\nPublic items; `rusting docs show \
         api/<name>` prints one.\n\n",
        doc.join("\n").trim()
    );
    for item in items.iter().filter(|item| !item.title.contains("::")) {
        text += &format!("- `{}`: {}\n", item.title, item.summary);
    }
    items.insert(
        0,
        DocItem {
            id: format!("api/{module}"),
            kind: "api",
            title: module.to_owned(),
            summary: first_sentence(&doc.join(" ")),
            text,
        },
    );
    items
}

/// The type an `impl` line implements, if it is one of `types`: `impl
/// Name`, `impl Name<'_>` or `impl<T> Name<T>`, but not a trait impl.
fn impl_owner<'a>(line: &str, types: &[&'a str]) -> Option<&'a str> {
    let rest = line.strip_prefix("impl")?;
    let rest = match rest.strip_prefix('<') {
        Some(generics) => {
            let mut depth = 1;
            let end = generics.find(|c| {
                depth += match c {
                    '<' => 1,
                    '>' => -1,
                    _ => 0,
                };
                depth == 0
            })?;
            &generics[end + 1..]
        }
        None => rest,
    };
    let rest = rest.strip_prefix(' ')?;
    types.iter().copied().find(|name| {
        rest.strip_prefix(name)
            .is_some_and(|r| r.starts_with(['<', ' ']))
    })
}

/// One item per public method of [`API_TYPES`].
fn api_items() -> Vec<DocItem> {
    methods_of(&API_TYPES, API_SOURCE)
}

/// One item per public method of `types`, read from the doc comments and
/// signatures in `source`, so the index cannot drift from the code.
fn methods_of(types: &[&str], source: &str) -> Vec<DocItem> {
    let mut items = Vec::new();
    let mut owner = None;
    let mut docs: Vec<&str> = Vec::new();
    let mut signature: Option<String> = None;
    for line in source.lines() {
        if let Some(sig) = signature.as_mut() {
            sig.push(' ');
            sig.push_str(line.trim());
        } else if line.starts_with("impl") {
            owner = impl_owner(line, types);
            docs.clear();
            continue;
        } else if line == "}" {
            owner = None;
            continue;
        } else if let Some(doc) = line.trim().strip_prefix("///") {
            docs.push(doc.strip_prefix(' ').unwrap_or(doc));
            continue;
        } else if line.starts_with("    pub fn ") {
            signature = Some(line.trim().to_owned());
        } else {
            if !line.trim().starts_with('#') {
                docs.clear();
            }
            continue;
        }
        if line.trim_end().ends_with(['{', ';']) {
            let sig = signature.take().unwrap_or_default();
            let sig = sig.trim_end_matches(['{', ';']).trim().to_owned();
            items.extend(api_item(owner, &sig, &docs));
            docs.clear();
        }
    }
    items
}

fn api_item(
    owner: Option<&str>,
    signature: &str,
    docs: &[&str],
) -> Option<DocItem> {
    let owner = owner?;
    let name = signature
        .strip_prefix("pub fn ")?
        .split(['(', '<'])
        .next()?
        .trim();
    let text = docs.join("\n");
    Some(DocItem {
        id: format!("api/{owner}::{name}"),
        kind: "api",
        title: format!("{owner}::{name}"),
        summary: first_sentence(text.trim()),
        text: format!("{signature}\n\n{}\n", text.trim()),
    })
}

/// The item whose id is `id`, or whose name after the `kind/` matches it
/// case-insensitively (`scene-patch`, `scene_conflict`).
#[must_use]
pub fn find(id: &str) -> Option<DocItem> {
    let wanted = id.to_ascii_lowercase();
    let mut matches = items().into_iter().filter(|item| {
        let name = item.id.to_ascii_lowercase();
        name == wanted || name.split_once('/').is_some_and(|(_, n)| n == wanted)
    });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

/// Items holding every word of `query`, best first, cut to `limit`, and the
/// number of items that matched before the cut. A hit in the id or title
/// counts more than hits in the text.
#[must_use]
pub fn search(query: &str, limit: usize) -> (Vec<Value>, usize) {
    let words: Vec<String> =
        query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return (Vec::new(), 0);
    }
    let mut scored: Vec<(usize, DocItem)> = items()
        .into_iter()
        .filter_map(|item| {
            let text = item.text.to_lowercase();
            let head = format!("{} {}", item.id, item.title).to_lowercase();
            let mut score = 0;
            for word in &words {
                let hits = text.matches(word.as_str()).count();
                let in_head = head.contains(word.as_str());
                if hits == 0 && !in_head {
                    return None;
                }
                score += hits.min(20) + if in_head { 25 } else { 0 };
            }
            Some((score, item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let total = scored.len();
    let matches = scored
        .into_iter()
        .take(limit)
        .map(|(score, item)| {
            // The line naming the most query words; the first line repeats
            // the title, so it is used only when no other line matches.
            let found = |line: &str| {
                let line = line.to_lowercase();
                words
                    .iter()
                    .filter(|word| line.contains(word.as_str()))
                    .count()
            };
            let snippet = item
                .text
                .lines()
                .skip(1)
                .enumerate()
                .filter(|(_, line)| found(line) > 0)
                .max_by_key(|(index, line)| {
                    (found(line), std::cmp::Reverse(*index))
                })
                .map(|(_, line)| line)
                .or_else(|| item.text.lines().find(|line| found(line) > 0))
                .map(|line| line.trim().chars().take(160).collect::<String>());
            json!({
                "id": item.id,
                "kind": item.kind,
                "title": item.title,
                "score": score,
                "snippet": snippet,
            })
        })
        .collect();
    (matches, total)
}

/// `text` cut at a line break so it fits `budget` tokens, and whether it was
/// cut. A budget too small for the first line still gets that line whole.
#[must_use]
pub fn within_budget(text: &str, budget: usize) -> (String, bool) {
    if tokens(text) <= budget {
        return (text.to_owned(), false);
    }
    let mut out = String::new();
    for line in text.lines() {
        if !out.is_empty() && tokens(&format!("{out}{line}\n")) > budget {
            break;
        }
        out += line;
        out.push('\n');
    }
    (out, true)
}

/// A compact overview in the `llms.txt` style: what the engine is, how to
/// look things up, then one line per item. Lines are dropped from the end,
/// whole pages and commands first, so the result fits `budget` tokens.
#[must_use]
pub fn brief(budget: usize) -> (String, usize, usize) {
    let head = "# Rusting engine\n\n\
        Rust game engine with a Vulkan renderer, ECS, and a CLI built for \
        agents: every command has `--json`, every error has a code.\n\n\
        Look up: `rusting docs search <words>`, `rusting docs show <id>`, \
        `rusting explain <CODE>`, `rusting schema --json`.\n\n";
    let items = items();
    let mut text = head.to_owned();
    let mut listed = 0;
    let mut kind = "";
    // Keep room for the line that says how many items were left out.
    let room = budget.saturating_sub(FOOTER_TOKENS);
    for item in &items {
        let heading = if item.kind == kind {
            String::new()
        } else {
            format!("\n## {}s\n", item.kind)
        };
        let heading = if listed == 0 {
            heading.trim_start().to_owned()
        } else {
            heading
        };
        let line = format!("{heading}- {}: {}\n", item.id, item.summary);
        if listed > 0 && tokens(&text) + tokens(&line) > room {
            break;
        }
        text += &line;
        kind = item.kind;
        listed += 1;
    }
    if listed < items.len() {
        text += &format!(
            "\n{} more items; `rusting docs` lists them all.\n",
            items.len() - listed
        );
    }
    (text, listed, items.len())
}

const FOOTER_TOKENS: usize = 20;

/// The agent skill's component list: each registered component with the
/// first sentence of its schema summary.
#[must_use]
pub fn component_list() -> String {
    crate::schema::component_summaries()
        .map(|(key, summary)| {
            format!("- `{key}`: {}\n", first_sentence(summary))
        })
        .collect()
}

/// The agent skill's pitfall list: each section of `docs/pitfalls.md` with
/// its entries' bold titles.
#[must_use]
pub fn pitfall_list() -> String {
    let mut list = String::new();
    for section in include_str!("../docs/pitfalls.md").split("\n## ").skip(1) {
        let heading = section.lines().next().unwrap_or_default();
        let titles: Vec<&str> = section
            .split("\n**")
            .skip(1)
            .filter_map(|entry| entry.split_once("**"))
            .map(|(title, _)| title.trim_end_matches('.'))
            .collect();
        list += &format!("- {heading}: {}.\n", titles.join("; "));
    }
    list
}

/// The `GameScene` methods in each row of the agent guides' API table, by
/// need. A method in no row lands in the last row, so a new method still
/// shows; [`api_table`] fails a test if a row names a missing method.
const API_GROUPS: &[(&str, &[&str])] = &[
    (
        "find, move, parent",
        &["object", "try_object", "reparent", "name_of", "world"],
    ),
    ("contacts", &["touching"]),
    (
        "counters",
        &[
            "counter",
            "counter_value",
            "counter_or",
            "set_counter",
            "add_to_counter",
            "spend",
            "counter_complete",
            "counters",
        ],
    ),
    (
        "cooldowns",
        &["cooldown_ready", "start_cooldown", "cooldown_left"],
    ),
    ("state machines", &["set_state", "state", "state_seconds"]),
    (
        "health",
        &[
            "damage",
            "health",
            "same_team",
            "add_status",
            "has_status",
            "status_left",
            "clear_status",
        ],
    ),
    (
        "input",
        &[
            "pressed",
            "held",
            "press_tick",
            "stick",
            "clicked",
            "cursor",
            "keys_pressed",
            "binding",
            "rebind",
            "window_focused",
            "viewport_size",
            "scroll_delta",
        ],
    ),
    (
        "rays",
        &[
            "raycast",
            "raycast_skipping",
            "raycast_only",
            "raycast_visible",
            "aim",
            "camera_ray",
            "pointer_ray",
            "on_screen",
        ],
    ),
    (
        "cameras",
        &[
            "set_active_camera",
            "set_camera",
            "set_camera_fov",
            "camera_fov",
            "basis",
            "set_mouse_look",
        ],
    ),
    (
        "physics",
        &[
            "set_body_kind",
            "set_linear_velocity",
            "linear_velocity",
            "set_angular_velocity",
            "angular_velocity",
            "reset_body",
        ],
    ),
    ("player", &["set_look", "set_player", "player", "dash"]),
    (
        "create, remove",
        &[
            "spawn_cube",
            "spawn_sphere",
            "spawn_cube_with_material",
            "spawn_sphere_with_material",
            "spawn_copy",
            "spawn_copy_at_root",
            "spawn_numbered",
            "spawn_prefab",
            "despawn",
        ],
    ),
    (
        "classes",
        &[
            "in_class",
            "has_class",
            "add_class",
            "remove_class",
            "children",
        ],
    ),
    (
        "sound",
        &[
            "play_sound",
            "play_sound_looped",
            "play_sound_with",
            "play_sound_on",
            "set_sound_rate",
            "set_sound_position",
            "set_sound_volume",
            "pause_sound",
            "resume_sound",
            "pause_sounds",
            "resume_sounds",
            "seek_sound",
            "stop_sound",
            "stop_all_sounds",
            "playing_sounds",
            "sounds_requested",
            "set_master_volume",
            "set_bus_volume",
            "mute_bus",
            "solo_bus",
            "set_bus_effect",
            "set_bus_voice_limit",
            "set_listener",
            "set_captions",
        ],
    ),
    (
        "look",
        &[
            "set_hud",
            "set_visible",
            "color",
            "set_color",
            "set_emissive",
            "create_material",
            "set_material",
            "set_text",
            "set_text_in_font",
            "set_light",
            "set_background_color",
            "set_reflections",
            "set_exposure",
            "set_hard_shadows",
            "set_max_texture_size",
            "set_terrain_look",
            "set_wetness",
            "create_texture",
            "edit_texture",
        ],
    ),
    (
        "game feel",
        &[
            "flash",
            "squash",
            "add_trauma",
            "hit_stop",
            "particles",
            "trigger",
            "tween",
        ],
    ),
    (
        "animation",
        &[
            "play_animation",
            "crossfade",
            "stop_animation",
            "is_playing",
            "set_animation_speed",
            "set_animation_parameter",
            "animation_events",
            "take_root_motion",
            "set_ragdoll",
            "set_ragdoll_muscle",
            "reset_ragdoll",
            "is_limp",
        ],
    ),
    (
        "rounds, levels",
        &[
            "once",
            "restart",
            "load_scene",
            "initial",
            "snapshot",
            "restore",
            "state_hash",
            "seed",
            "set_seed",
            "random",
            "roll_loot",
            "roll_loot_table",
        ],
    ),
    (
        "menus, saves",
        &[
            "ui",
            "set_text_scale",
            "text_scale",
            "set_paused",
            "paused",
            "set_time_scale",
            "time_scale",
            "quit",
            "exiting",
            "save_data",
            "load_data",
            "delete_data",
            "saved_keys",
            "save_counters",
            "save_objects",
            "load_objects",
            "load_counters",
            "load_text",
        ],
    ),
    (
        "video settings",
        &[
            "set_render_scale",
            "render_scale",
            "resolved_quality",
            "set_pixelated",
            "set_upscale_sharpness",
            "set_vsync",
            "set_max_fps",
            "set_fullscreen",
            "fullscreen",
            "set_window_size",
            "set_window_title",
        ],
    ),
    ("translations", &["set_locale", "locale", "tr", "tr_count"]),
    (
        "dialogue",
        &["start_dialogue", "dialogue_line", "advance_dialogue"],
    ),
    ("tiles, fields", &["tile", "set_tile", "set_field"]),
    (
        "GPU bodies",
        &[
            "apply_gpu_physics_to_class",
            "watch_gpu_class",
            "watch_gpu_object",
            "set_gpu_condition_shaders",
            "gpu_events",
            "gpu_state",
            "count_gpu_bodies_in_box",
            "gpu_command",
        ],
    ),
    ("other", &[]),
];

/// `name(arg, ..)` for an API item: the parameter names of its signature,
/// without `self`.
fn api_call(item: &DocItem) -> String {
    let (_, name) = item.title.split_once("::").unwrap_or(("", &item.title));
    let signature = item.text.lines().next().unwrap_or_default();
    let after = &signature[signature.find(name).unwrap_or(0) + name.len()..];
    // Skip any generic parameters, then take the balanced argument list.
    let (mut depth, mut args, mut list) = (0, Vec::new(), String::new());
    let mut previous = ' ';
    for c in after.chars() {
        let arrow = previous == '-';
        previous = c;
        match c {
            '<' | '[' => depth += 1,
            '>' | ']' if !arrow => depth -= 1,
            '(' if depth == 0 && list.is_empty() && args.is_empty() => {
                depth = 100;
                continue;
            }
            '(' => depth += 1,
            ')' if depth == 100 => break,
            ')' => depth -= 1,
            ',' if depth == 100 => {
                args.push(std::mem::take(&mut list));
                continue;
            }
            _ => {}
        }
        if depth >= 100 {
            list.push(c);
        }
    }
    args.push(list);
    let args: Vec<&str> = args
        .iter()
        .filter_map(|arg| arg.split(':').next())
        .map(|arg| {
            arg.trim()
                .trim_start_matches("mut ")
                .trim_start_matches('_')
        })
        .filter(|arg| !arg.is_empty() && !arg.ends_with("self"))
        .collect();
    format!("`{name}({})`", args.join(", "))
}

/// The agent guides' API table, built from the API index: one row per
/// `API_GROUPS` need, then the methods of the object `object(name)`
/// returns.
#[must_use]
pub fn api_table() -> String {
    let api = api_items();
    let scene: Vec<&DocItem> = api
        .iter()
        .filter(|item| item.title.starts_with("GameScene::"))
        .collect();
    let method = |item: &&DocItem| item.title["GameScene::".len()..].to_owned();
    let mut table = String::from("| Need | Call |\n| --- | --- |\n");
    for (need, names) in API_GROUPS {
        let calls: Vec<String> = if names.is_empty() {
            let grouped: Vec<&str> = API_GROUPS
                .iter()
                .flat_map(|(_, n)| n.iter().copied())
                .collect();
            scene
                .iter()
                .filter(|item| !grouped.contains(&method(item).as_str()))
                .map(|item| api_call(item))
                .collect()
        } else {
            names
                .iter()
                .map(|name| {
                    let item = scene
                        .iter()
                        .find(|item| method(item) == *name)
                        .unwrap_or_else(|| {
                            panic!("`{name}` is not a GameScene method")
                        });
                    api_call(item)
                })
                .collect()
        };
        if !calls.is_empty() {
            table += &format!("| {need} | {} |\n", calls.join(", "));
        }
    }
    let object: Vec<String> = api
        .iter()
        .filter(|item| item.title.starts_with("GameObject::"))
        .map(api_call)
        .collect();
    table += &format!("| on `object(name)` | {} |\n", object.join(", "));
    table
}

#[cfg(test)]
mod tests {
    #[test]
    fn sample_doc_copies_match_the_samples() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (name, readme, code) in super::SAMPLES {
            let dir = root.join("samples").join(name);
            if !dir.exists() {
                continue; // published crate: only the copies exist
            }
            let stale = "is out of date; run scripts/sync_sample_docs.sh";
            let real = std::fs::read_to_string(dir.join("README.md")).unwrap();
            assert_eq!(*readme, real, "docs/samples/{name}/README.md {stale}");
            let real =
                std::fs::read_to_string(dir.join("src/main.rs")).unwrap();
            assert_eq!(*code, real, "docs/samples/{name}/main.rs {stale}");
        }
    }

    #[test]
    fn api_index_lists_documented_gameplay_methods() {
        let once = super::find("api/GameScene::once").expect("once indexed");
        assert!(once.text.starts_with("pub fn once("), "{}", once.text);
        assert!(once.summary.contains("once per round"), "{}", once.summary);
        let api: Vec<_> = super::items()
            .into_iter()
            .filter(|i| i.kind == "api")
            .collect();
        assert!(api.len() > 30, "only {} api items", api.len());
        assert!(api.iter().all(|i| !i.summary.is_empty()), "undocumented");
        let lighting = super::find("guide/lighting").expect("lighting");
        for wanted in ["1024", "Eco", "4 GiB", "shadows", "dropped_lights"] {
            assert!(lighting.text.contains(wanted), "{wanted}");
        }
        let graph = super::find("api/WaypointGraph").expect("waypoints");
        for wanted in ["nodes", "nearest", "disconnect", "path", "length"] {
            assert!(graph.text.contains(wanted), "{wanted}: {}", graph.text);
        }
        for (page, field) in [
            ("api/GpuBodySettings", "- `collision_layers:"),
            (
                "api/GpuConditionShader",
                "- `params: Vec<[f32; 4]>`: Values",
            ),
            ("api/MaterialAsset", "- `roughness: f32`: 0 is a mirror"),
        ] {
            let item = super::find(page).expect(page);
            assert!(item.text.contains(field), "{page}: {}", item.text);
        }
        let player = super::find("api/PlayerController").expect("player");
        assert!(
            player.summary.starts_with("Walk, jump"),
            "{}",
            player.summary
        );
        assert!(player.text.contains("PlayerController::step"));
        assert!(super::find("api/PlayerInput").is_some());
        assert!(
            player.text.contains("- `wall: Option<Entity>`: The body"),
            "{}",
            player.text
        );
        assert!(!player.text.contains("`: \n"), "{}", player.text);
        let event = super::find("api/AnimationEvent").expect("animation event");
        for field in [
            "tick: u64",
            "entity: Entity",
            "object: String",
            "clip",
            "name",
        ] {
            assert!(event.text.contains(field), "{}", event.text);
        }
    }

    use super::*;

    #[test]
    fn net_and_procgen_items_have_api_pages() {
        let page = |id: &str| super::find(id).expect(id).text;
        for (id, holds) in [
            ("api/PeerId", "pub type PeerId = u32"),
            (
                "api/fbm2",
                "pub fn fbm2(seed: u64, point: [f32; 2], octaves",
            ),
            ("api/noise2", "between -1 and 1"),
            ("api/procgen", "the same level on every machine"),
            ("api/procgen", "- `poisson_disk`: Points spread"),
            (
                "api/RandomStream",
                "- RandomStream::unit: The next value in",
            ),
            ("api/NetSession", "- NetSession::join: Joins a host"),
            ("api/NetEvent", "`Message { from: PeerId, bytes: Vec<u8> }`"),
            ("api/net", "- `run_relay`: Runs a relay"),
            (
                "api/Prediction",
                "- Prediction::reconcile: Takes the host's",
            ),
            ("api/Predicted", "Marks the one object"),
            ("api/Smoothing::correct", "Duration::ZERO"),
            ("api/InputBuffer::take", "pub fn take(&mut self)"),
            ("api/Interpolation", "- Interpolation::present:"),
        ] {
            assert!(page(id).contains(holds), "{id} lacks {holds:?}");
        }
    }

    #[test]
    fn every_item_has_a_unique_id_a_title_and_a_summary() {
        let items = items();
        let mut ids: Vec<_> = items.iter().map(|item| &item.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), items.len(), "ids are unique");
        for item in &items {
            assert!(!item.title.is_empty(), "{} has no title", item.id);
            assert!(!item.summary.is_empty(), "{} has no summary", item.id);
            assert!(item.id.contains('/'), "{} has no kind", item.id);
        }
        assert_eq!(
            items.iter().filter(|item| item.kind == "command").count(),
            OPERATIONS.len()
        );
        assert_eq!(
            items.iter().filter(|item| item.kind == "code").count(),
            CODES.len()
        );
    }

    #[test]
    fn the_scenario_reference_lists_every_step_kind() {
        let page = find("scenario").unwrap();
        assert_eq!(page.id, "reference/scenario");
        for step in ["expect_pixels", "expect_screen", "capture", "log", "set"]
        {
            assert!(page.text.contains(&format!("- `{step}`:")), "{step}");
        }
        assert!(page.text.contains("## budgets"));
    }

    #[test]
    fn every_reflected_type_has_a_reference_page() {
        let items = items();
        for key in crate::schema::component_summaries().map(|(key, _)| key) {
            let id = format!("component/{key}");
            assert!(items.iter().any(|item| item.id == id), "{id}");
        }
        let fog = find("component/rusting.fog").unwrap();
        assert!(fog.text.contains("- `/density` (1/m, 0..1): extinction"));
        assert!(fog.text.contains("\nDefault: `{"));
        assert!(find("component/transform").is_some());
        assert!(find("resource/rusting.physics_settings").is_some());
        assert!(find("asset/rusting.material").is_some());
    }

    #[test]
    fn find_accepts_a_bare_name_only_when_it_is_unambiguous() {
        assert_eq!(find("scene_conflict").unwrap().id, "code/SCENE_CONFLICT");
        assert_eq!(find("command/scene-patch").unwrap().kind, "command");
        assert!(find("no-such-item").is_none());
    }

    #[test]
    fn search_needs_every_word_and_ranks_head_hits_first() {
        let (found, _) = search("scene patch", 5);
        assert!(!found.is_empty());
        assert_eq!(found[0]["id"], "command/scene-patch");
        assert!(search("zzzznotaword", 5).0.is_empty());
        // The snippet is the line naming the most query words.
        let (found, _) = search("text texture", 50);
        let guide =
            found.iter().find(|item| item["id"] == "guide/menus-and-ui");
        let snippet =
            guide.unwrap()["snippet"].as_str().unwrap().to_lowercase();
        assert!(snippet.contains("text_texture"), "{snippet}");
        assert!(search("  ", 5).0.is_empty());
        let (three, total) = search("scene", 3);
        assert_eq!(three.len(), 3);
        assert!(total > 3, "total counts matches past the limit");
    }

    #[test]
    fn level_of_detail_is_found_by_its_usual_names() {
        for query in ["lod", "level of detail", "rlod"] {
            let (found, _) = search(query, 50);
            assert!(
                found.iter().any(|item| item["id"] == "guide/look-and-feel"),
                "{query}"
            );
        }
    }

    #[test]
    fn the_headless_render_size_is_found_by_its_usual_names() {
        for query in ["resolution", "render size", "1080p"] {
            let (found, _) = search(query, 50);
            assert!(
                found.iter().any(|item| item["id"] == "guide/look-and-feel"),
                "{query}"
            );
        }
    }

    #[test]
    fn the_lighting_guide_explains_brightness_and_direction() {
        for query in ["illuminance intensity", "spot light axis"] {
            let (found, _) = search(query, 50);
            assert!(
                found.iter().any(|item| item["id"] == "guide/lighting"),
                "{query}"
            );
        }
    }

    #[test]
    fn bus_effects_and_press_tick_are_documented() {
        let items = items();
        let page = |id: &str| {
            items
                .iter()
                .find(|item| item.id == id)
                .unwrap_or_else(|| panic!("no {id}"))
                .text
                .clone()
        };
        let bus = page("api/BusEffect");
        assert!(
            bus.contains("LowPass") && bus.contains("cutoff_hz"),
            "{bus}"
        );
        let press = page("api/GameScene::press_tick");
        assert!(press.contains("\"at\""), "{press}");
    }

    #[test]
    fn counter_or_is_documented() {
        let (found, _) = search("counter_or default without a warning", 50);
        assert!(found.iter().any(|item| item["id"] == "guide/menus-and-ui"));
    }

    #[test]
    fn entity_order_effect_on_physics_is_documented() {
        let (found, _) = search("unrelated scene edit toppled", 50);
        assert!(found.iter().any(|item| item["id"] == "manual/determinism"));
    }

    #[test]
    fn when_perf_render_is_filled_is_documented() {
        let (found, _) = search("perf.render cameras null budget", 50);
        assert!(found.iter().any(|item| item["id"] == "manual/concepts"));
        let schema = crate::schema::catalog().to_string();
        assert!(
            schema.contains("no capture step needed"),
            "split-signal F12"
        );
    }

    #[test]
    fn keep_going_and_its_variable_are_documented() {
        let (found, _) = search("RUSTING_KEEP_GOING", 50);
        assert!(found.iter().any(|item| item["id"] == "manual/concepts"));
    }

    #[test]
    fn build_time_budget_and_shared_target_are_documented() {
        let (found, _) = search("CARGO_TARGET_DIR shared build", 50);
        assert!(found.iter().any(|item| item["id"] == "manual/concepts"));
    }

    #[test]
    fn primitive_uvs_and_child_scale_are_explained() {
        for (query, page) in [
            ("cylinder cap texture disc", "guide/look-and-feel"),
            ("child transform relative parent scale", "manual/concepts"),
        ] {
            let (found, _) = search(query, 50);
            assert!(found.iter().any(|item| item["id"] == page), "{query}");
        }
        // Caps map as a disc: the UV is the cap position shifted by 0.5,
        // with the bottom mirrored so it reads upright from below.
        use crate::assets::{procedural_primitive_mesh, PrimitiveShape};
        for shape in [PrimitiveShape::Cylinder, PrimitiveShape::Cone] {
            let mesh = procedural_primitive_mesh(shape);
            let caps: Vec<_> = mesh
                .vertices
                .iter()
                .filter(|v| v.normal[0] == 0.0 && v.normal[2] == 0.0)
                .collect();
            assert!(!caps.is_empty(), "{shape:?}");
            for v in caps {
                let u = 0.5 + v.position[0] * v.normal[1];
                let w = 0.5 + v.position[2];
                assert!((v.uv[0] - u).abs() < 1e-5, "{shape:?} {v:?}");
                assert!((v.uv[1] - w).abs() < 1e-5, "{shape:?} {v:?}");
            }
        }
        // Sides show the whole texture by height, its top row at the top
        // as on the sphere (lantern-keeper F30).
        for (shape, half) in [
            (PrimitiveShape::Cylinder, 0.5),
            (PrimitiveShape::Cone, 0.5),
            (PrimitiveShape::Capsule, 1.0),
        ] {
            let mesh = procedural_primitive_mesh(shape);
            for v in mesh
                .vertices
                .iter()
                .filter(|v| v.normal[0] != 0.0 || v.normal[2] != 0.0)
            {
                let expected = (half - v.position[1]) / (2.0 * half);
                assert!((v.uv[1] - expected).abs() < 1e-4, "{shape:?} {v:?}");
            }
        }
        let plane = procedural_primitive_mesh(PrimitiveShape::Plane);
        let uvs: Vec<_> = plane.vertices.iter().map(|v| v.uv).collect();
        assert_eq!(uvs, [[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]]);
    }

    #[test]
    fn many_body_costs_are_explained() {
        for query in ["subdivisions triangles", "state_hash checkpoint"] {
            let (found, _) = search(query, 50);
            assert!(
                found.iter().any(|item| item["id"] == "manual/concepts"),
                "{query}"
            );
        }
        // The documented count matches the mesh.
        for n in [3, 6, 16] {
            let mesh = crate::assets::procedural_sphere_mesh(n);
            assert_eq!(mesh.indices.len() as u32 / 3, 4 * n * n);
        }
    }

    #[test]
    fn contact_grid_overflow_is_explained() {
        let (found, _) = search("contact grid overflow", 50);
        assert!(
            found.iter().any(|item| item["id"] == "manual/concepts"),
            "{found:?}"
        );
    }

    #[test]
    fn budget_cuts_at_a_line_and_the_brief_fits_it() {
        let (cut, truncated) = within_budget("aaaa\nbbbb\ncccc\n", 3);
        assert!(truncated);
        assert_eq!(cut, "aaaa\nbbbb\n");
        assert_eq!(within_budget("abc", 5), ("abc".to_owned(), false));
        for budget in [300, 1000, 4000] {
            let (text, listed, total) = brief(budget);
            assert!(tokens(&text) <= budget, "{budget}: {}", tokens(&text));
            assert!(listed >= 1 && listed <= total);
            assert_eq!(
                text.contains(" more items;"),
                listed < total,
                "the brief says what it left out"
            );
        }
        let (_, listed, total) = brief(1_000_000);
        assert_eq!(listed, total, "a big budget lists everything");
    }

    #[test]
    fn the_brief_names_only_commands_that_exist() {
        let (text, _, _) = brief(1_000_000);
        for word in ["docs", "explain", "schema"] {
            assert!(
                OPERATIONS.iter().any(|operation| operation.name == word),
                "{word}"
            );
            assert!(text.contains(&format!("`rusting {word}")));
        }
    }

    /// Every `rusting ...` command in a page or the catalog names a real
    /// operation and only flags that operation's usage lists.
    #[test]
    fn documented_commands_name_real_operations_and_flags() {
        let mut commands: Vec<(String, String)> = Vec::new();
        for operation in OPERATIONS {
            commands.push((
                format!("catalog {}", operation.name),
                format!("rusting {}", operation.example),
            ));
        }
        for item in items().into_iter().filter(|item| item.kind != "api") {
            for line in item.text.lines() {
                let mut rest = line;
                while let Some(start) = rest.find("`rusting ") {
                    let after = &rest[start + 1..];
                    let Some(end) = after.find('`') else { break };
                    commands.push((item.id.clone(), after[..end].to_owned()));
                    rest = &after[end..];
                }
                if let Some(command) = line
                    .trim_start()
                    .strip_prefix("$ ")
                    .filter(|c| c.starts_with("rusting "))
                {
                    commands.push((item.id.clone(), command.to_owned()));
                }
            }
        }
        assert!(commands.len() > 40, "only {} commands", commands.len());
        let mut stale = Vec::new();
        for (source, command) in commands {
            let words: Vec<&str> = command
                .split_whitespace()
                .skip(1)
                .take_while(|w| !w.starts_with(['-', '<', '[', '.']))
                .collect();
            let operation = OPERATIONS
                .iter()
                .filter(|op| {
                    let name: Vec<&str> = op.name.split(' ').collect();
                    words.len() >= name.len() && words[..name.len()] == name[..]
                })
                .max_by_key(|op| op.name.len());
            let Some(operation) = operation else {
                // Prose such as "`rusting` itself" has no command word.
                if !words.is_empty() {
                    stale
                        .push(format!("{source}: `{command}` unknown command"));
                }
                continue;
            };
            for flag in command
                .split_whitespace()
                .filter(|w| w.starts_with("--"))
                .map(|w| w.trim_end_matches([',', '.', ';', ')', '`']))
            {
                if !matches!(flag, "--json" | "--help")
                    && !operation.usage.contains(flag)
                {
                    stale.push(format!(
                        "{source}: `{command}` flag {flag} not in `{}`",
                        operation.usage
                    ));
                }
            }
        }
        assert!(stale.is_empty(), "stale examples:\n{}", stale.join("\n"));
    }

    /// API audit (roadmap L2): a public method of a prelude type must not
    /// return a borrow, which blocks the next `GameScene` call, and a
    /// prelude type must not need a lifetime. The entries below predate the
    /// rule; the lists may only shrink.
    #[test]
    fn gameplay_methods_do_not_return_new_borrows() {
        const KNOWN: [&str; 6] = [
            "GameScene::object",
            "GameScene::try_object",
            "GameScene::counter",
            "GameScene::world",
            // Reached through `world`; read-only slices of the resource.
            "PhysicsWorld::contacts",
            "PhysicsWorld::impacts",
        ];
        // Games name `&mut GameScene` with the lifetime elided.
        const KNOWN_LIFETIMES: [&str; 2] = ["GameScene", "GameObject"];
        let lib = include_str!("lib.rs");
        let prelude = &lib[lib.find("pub mod prelude {").unwrap()..];
        let prelude = &prelude[..prelude.find("\n}").unwrap()];
        let types: Vec<&str> = prelude
            .lines()
            .filter(|line| !line.trim().starts_with("//"))
            .flat_map(|line| line.split(|c: char| !c.is_alphanumeric()))
            .filter(|word| word.starts_with(|c: char| c.is_ascii_uppercase()))
            .collect();
        let mut files = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src"
        ))];
        let mut sources = Vec::new();
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(
                    std::fs::read_dir(&path)
                        .unwrap()
                        .map(|entry| entry.unwrap().path()),
                );
            } else if path.extension().is_some_and(|e| e == "rs") {
                sources.push(std::fs::read_to_string(&path).unwrap());
            }
        }
        let mut found = Vec::new();
        let mut lifetimes = Vec::new();
        for source in &sources {
            let mut owner = None;
            let mut signature: Option<String> = None;
            for line in source.lines() {
                for kind in ["pub struct ", "pub enum "] {
                    let Some(rest) = line.strip_prefix(kind) else {
                        continue;
                    };
                    let name = rest.split(['<', ' ', '(', ';']).next();
                    if rest.contains("<'")
                        && name.is_some_and(|name| types.contains(&name))
                    {
                        lifetimes.push(name.unwrap().to_owned());
                    }
                }
                if let Some(rest) = line.strip_prefix("impl") {
                    // `impl<'w> GameScene<'w> {`, not `impl Trait for T`.
                    let rest = match rest.strip_prefix('<') {
                        Some(generic) => {
                            generic.split_once("> ").map_or("", |(_, r)| r)
                        }
                        None => rest.trim_start(),
                    };
                    owner = types.iter().copied().find(|name| {
                        !rest.contains(" for ")
                            && rest
                                .strip_prefix(name)
                                .is_some_and(|r| r.starts_with(['<', ' ']))
                    });
                    continue;
                }
                if line == "}" {
                    owner = None;
                }
                let Some(name) = owner else { continue };
                if let Some(sig) = signature.as_mut() {
                    sig.push(' ');
                    sig.push_str(line.trim());
                } else if line.starts_with("    pub fn ") {
                    signature = Some(line.trim().to_owned());
                } else {
                    continue;
                }
                if !line.trim_end().ends_with(['{', ';']) {
                    continue;
                }
                let sig = signature.take().unwrap();
                let Some((head, returns)) = sig.split_once("->") else {
                    continue;
                };
                // `&mut Self` is a chaining builder, not a borrow that
                // outlives the statement.
                let returns = returns.trim_end_matches(['{', ';']).trim();
                if returns != "&mut Self" && returns.contains(['&', '\'']) {
                    let method = head["pub fn ".len()..]
                        .split(['<', '('])
                        .next()
                        .unwrap();
                    found.push(format!("{name}::{method}"));
                }
            }
        }
        for (found, known, what) in [
            (&found, &KNOWN[..], "returns a borrow"),
            (&lifetimes, &KNOWN_LIFETIMES[..], "needs a lifetime"),
        ] {
            let new: Vec<_> = found
                .iter()
                .filter(|id| !known.contains(&id.as_str()))
                .collect();
            assert!(new.is_empty(), "{what}: {new:?}");
            let gone: Vec<_> = known
                .iter()
                .filter(|id| !found.contains(&id.to_string()))
                .collect();
            assert!(gone.is_empty(), "fixed; remove from the list: {gone:?}");
        }
    }

    /// A `Preferred:` line in a doc comment names the call to use instead;
    /// each name must be in the index.
    #[test]
    fn preferred_calls_exist_in_the_index() {
        let ids: Vec<String> = items()
            .into_iter()
            .filter(|item| item.kind == "api")
            .map(|item| item.id)
            .collect();
        let mut marked = 0;
        for item in items().into_iter().filter(|item| item.kind == "api") {
            let owner = item.id.split("::").next().unwrap_or_default();
            let Some(start) = item.text.find("Preferred:") else {
                continue;
            };
            // The paragraph, which may wrap over lines.
            let paragraph = item.text[start..].split("\n\n").next().unwrap();
            marked += 1;
            for name in paragraph.split("Self::").skip(1) {
                let name: String = name
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                assert!(
                    ids.contains(&format!("{owner}::{name}")),
                    "{}: `{name}` is not in the API index",
                    item.id
                );
            }
        }
        assert!(marked >= 2, "only {marked} calls mark a preferred one");
    }

    #[test]
    fn pitfall_catalog_names_only_real_codes_and_every_entry_has_a_fix() {
        let page = super::find("guide/pitfalls").expect("pitfalls");
        let mut named = 0;
        for line in page.text.lines().filter(|l| l.starts_with("Code: ")) {
            let codes = line.split('`').skip(1).step_by(2);
            for code in codes.filter(|c| !c.contains(' ')) {
                assert!(crate::diagnostics::lookup(code).is_some(), "{code}");
                named += 1;
            }
        }
        assert!(named >= 15, "only {named} codes");
        let entries = page.text.matches("\n**").count();
        assert_eq!(page.text.matches("\nCode: ").count(), entries);
        assert_eq!(page.text.matches("\nFix: ").count(), entries);
    }

    #[test]
    fn agent_guides_cover_every_scene_method_and_name_only_real_ones() {
        let api = super::api_items();
        let methods: Vec<&str> = api
            .iter()
            .filter_map(|item| item.title.split_once("::").map(|(_, m)| m))
            .collect();
        let skill = include_str!("../skills/rusting-game/SKILL.md");
        let agents = include_str!("project_agents.md");
        let guides: String = PAGES.iter().map(|(_, _, text)| *text).collect();
        let named = |text: &str, name: &str| {
            text.match_indices(name).any(|(at, _)| {
                let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
                !text[..at].ends_with(word)
                    && !text[at + name.len()..].starts_with(word)
            })
        };
        let unnamed: Vec<_> = api
            .iter()
            .filter_map(|item| item.title.strip_prefix("GameScene::"))
            .filter(|m| !named(&guides, m) && !named(agents, m))
            .collect();
        assert!(unnamed.is_empty(), "no guide names {unnamed:?}");
        // Calls written as `name(..)` or `scene.name(..)` in the agent
        // guides must be real methods, so a rename cannot leave them stale.
        let std_calls = ["atan2"];
        for text in [skill, agents] {
            for code in text.split('`').skip(1).step_by(2) {
                let call = code.trim_start_matches("scene.");
                let Some((name, _)) = call.split_once('(') else {
                    continue;
                };
                if name.starts_with(|c: char| c.is_ascii_lowercase())
                    && name.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
                    })
                {
                    assert!(
                        methods.contains(&name) || std_calls.contains(&name),
                        "`{code}` names no API method"
                    );
                }
            }
        }
    }

    #[test]
    fn agent_guides_name_only_real_commands_codes_pages_and_schemas() {
        let skill = include_str!("../skills/rusting-game/SKILL.md");
        let agents = include_str!("project_agents.md");
        let mut commands = 0;
        for text in [skill, agents] {
            for code in text.split('`').skip(1).step_by(2) {
                let Some(args) = code.strip_prefix("rusting ") else {
                    continue;
                };
                commands += 1;
                if let Some(problem) = crate::schema::command_problem(args) {
                    panic!("{problem}");
                }
                // `<...>` and `CODE` are placeholders.
                match args.split_whitespace().collect::<Vec<_>>()[..] {
                    ["explain", name, ..] if name.contains('_') => assert!(
                        crate::diagnostics::lookup(name).is_some(),
                        "`{code}`: no such code"
                    ),
                    ["docs", "show", id, ..] if !id.contains('<') => assert!(
                        super::find(id).is_some(),
                        "`{code}`: no such page"
                    ),
                    ["schema", name, ..] if !name.starts_with('-') => {
                        crate::schema::catalog_entry(name)
                            .unwrap_or_else(|e| panic!("`{code}`: {e}"));
                    }
                    _ => {}
                }
            }
        }
        assert!(commands > 60, "only {commands} commands");
    }

    #[test]
    fn agent_guides_hold_their_generated_parts() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let (skill, agents) =
            ("skills/rusting-game/SKILL.md", "src/project_agents.md");
        for (marker, generated, file) in [
            ("api-table", super::api_table(), skill),
            ("api-table", super::api_table(), agents),
            ("components", super::component_list(), skill),
            ("pitfalls", super::pitfall_list(), skill),
        ] {
            let start = format!("<!-- {marker}:start -->\n");
            let end = format!("<!-- {marker}:end -->");
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            let (head, rest) = text.split_once(&start).expect(&start);
            let (old, tail) = rest.split_once(&end).expect(&end);
            if old == generated {
                continue;
            }
            if std::env::var_os(crate::scenario::UPDATE_GOLDEN_ENV).is_some() {
                let text = format!("{head}{start}{generated}{end}{tail}");
                std::fs::write(root.join(file), text).unwrap();
                continue;
            }
            panic!(
                "{file}: the generated {marker} is stale; run with {}=1 to rewrite it",
                crate::scenario::UPDATE_GOLDEN_ENV
            );
        }
    }
}
