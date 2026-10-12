//! Project creation, validation, opening, and recent-project storage.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::runtime::{
    SceneAlphaMode, SceneCamera, SceneDocument, SceneEntity, SceneMaterial,
    SceneMaterialData, SceneMaterialModel, SceneMesh, SceneMeshRenderer,
    SceneProjection, SceneTransform, SCENE_FORMAT_VERSION,
};

/// Current version of `project.json` written by the editor.
pub const PROJECT_FORMAT_VERSION: u32 = 1;

/// Environment variable that makes a game run this many ticks headless and
/// exit, for smoke tests of built and exported games.
pub const HEADLESS_TICKS_ENV: &str = "RUSTING_HEADLESS_TICKS";
/// Environment variable that makes a game run as a dedicated server: no
/// window or renderer, ticking in real time until game code quits.
pub const SERVER_ENV: &str = "RUSTING_SERVER";
/// Where `rusting test` saves its newest folder run, relative to the
/// project root; the editor's Agent panel reads it.
pub const TEST_RESULTS_FILE: &str = "build/test-results.json";

/// Environment variable naming the folder where game code keeps settings
/// and saves (`GameScene::save_data`). `rusting test` points it at an empty
/// `build/test-userdata/<scenario>/` folder for each scenario.
pub const USER_DATA_ENV: &str = "RUSTING_USER_DATA";

/// Folder for a game's settings and saves: [`USER_DATA_ENV`] when set,
/// else the platform's own save folder
/// ([`crate::platform::platform_save_folder`]), else
/// [`default_user_data_folder`].
#[must_use]
pub fn user_data_folder() -> PathBuf {
    if let Some(folder) = std::env::var_os(USER_DATA_ENV) {
        return PathBuf::from(folder);
    }
    crate::platform::platform_save_folder()
        .unwrap_or_else(default_user_data_folder)
}

/// [`USER_DATA_ENV`] when set, else a folder named after the game's
/// executable in the user's data folder (`~/.local/share/<game>` on Linux,
/// `~/Library/Application Support/<game>` on macOS, `%APPDATA%\<game>` on
/// Windows), ignoring the platform services.
#[must_use]
pub fn default_user_data_folder() -> PathBuf {
    if let Some(folder) = std::env::var_os(USER_DATA_ENV) {
        return PathBuf::from(folder);
    }
    let game = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.file_stem().map(ToOwned::to_owned))
        .unwrap_or_else(|| "rusting-game".into());
    let env = |name| std::env::var_os(name).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|home| home.join("Library/Application Support"))
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|home| home.join(".local/share")))
    };
    base.unwrap_or_else(|| PathBuf::from("userdata")).join(game)
}

/// Environment variable naming a file where a headless run writes its
/// `StateHashReport` as JSON, for determinism checks across processes.
pub const STATE_HASH_OUT_ENV: &str = "RUSTING_STATE_HASH_OUT";

/// Environment variable naming a file where a headless run saves the scene
/// as it stands after the last tick, so tools can query the end state.
pub const FINAL_SCENE_OUT_ENV: &str = "RUSTING_FINAL_SCENE_OUT";

/// Environment variable naming a file where a windowed game writes a
/// `Replay` of the session as JSON when it exits.
pub const REPLAY_OUT_ENV: &str = "RUSTING_REPLAY_OUT";

/// Environment variable with the [`crate::runtime::RandomSeed`] a game run
/// starts with, from `rusting run --seed`. A scenario's `seed` and a
/// replay's recorded seed still win.
pub const SEED_ENV: &str = "RUSTING_SEED";

/// Milliseconds after which a windowed game closes itself as if its window
/// closed, so `rusting run --record --timeout` still saves the replay.
pub const QUIT_AFTER_MS_ENV: &str = "RUSTING_QUIT_AFTER_MS";

/// Environment variable naming a `Replay` JSON file that a game plays back
/// headless instead of opening a window, failing if any tick's hash differs.
pub const REPLAY_PLAY_ENV: &str = "RUSTING_REPLAY_PLAY";

/// Environment variable the editor sets on Play: a file that carries the
/// game's state across a code reload. The game writes its scene there and
/// exits when it reads [`CODE_RELOAD_SAVE_COMMAND`] on standard input, and
/// starts from that file, then deletes it, when the file exists.
pub const CODE_RELOAD_STATE_ENV: &str = "RUSTING_CODE_RELOAD_STATE";

/// Standard input line that asks a game started with
/// [`CODE_RELOAD_STATE_ENV`] to save its state and exit.
pub const CODE_RELOAD_SAVE_COMMAND: &str = "save-state";

/// Line prefix a game prints to standard error when a code reload kept the
/// scene.
pub const CODE_RELOAD_KEPT_MARKER: &str =
    "[rusting] code reload kept the scene";

/// Line prefix a game prints to standard error when a code reload could not
/// keep the scene, followed by the reason.
pub const CODE_RELOAD_CLEAN_MARKER: &str =
    "[rusting] code reload restarted clean:";

/// Line prefix a game prints to standard error once its first playable frame
/// is done, followed by the milliseconds since the game started.
pub const FIRST_FRAME_MARKER: &str = "[rusting] first playable frame after";

/// Milliseconds the game reported with [`FIRST_FRAME_MARKER`].
#[must_use]
pub fn first_frame_ms(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        line.strip_prefix(FIRST_FRAME_MARKER)?
            .trim()
            .strip_suffix("ms")?
            .trim()
            .parse()
            .ok()
    })
}

/// Printed by a headless run with the mean wall time of one fixed tick.
pub const TICK_TIME_MARKER: &str = "[rusting] headless ms per tick";

/// Frames a windowed game measures before it closes itself and prints
/// [`BENCH_MARKER`]; set by `rusting run --bench FRAMES`.
pub const BENCH_FRAMES_ENV: &str = "RUSTING_BENCH_FRAMES";

/// Comma-separated scene component names (`collider`, `rigid_body`, or a
/// registered component) that every scene load leaves out, set by
/// `rusting test --without`.
pub const WITHOUT_ENV: &str = "RUSTING_WITHOUT";

/// Frames a bench run skips before measuring: the first frames build
/// pipelines and upload assets.
pub const BENCH_WARMUP_FRAMES: u32 = 60;

/// Prefix of the line holding a bench run's frame times as JSON.
pub const BENCH_MARKER: &str = "[rusting] bench";

/// A bench line for frame lengths in milliseconds: frame count, mean, p50,
/// p95, p99 and max (nearest rank). `work` holds each measured frame's CPU
/// and GPU milliseconds; their medians give `cpu_p50_ms`, `gpu_p50_ms` and
/// `bound`, the side that limits the frame (`null` without GPU times).
/// Sorts `lengths`.
#[must_use]
pub fn bench_line(lengths: &mut [f64], work: &[[f64; 2]]) -> String {
    fn rank(sorted: &[f64], p: f64) -> f64 {
        let index = (p * sorted.len() as f64).ceil() as usize;
        sorted.get(index.saturating_sub(1)).copied().unwrap_or(0.0)
    }
    lengths.sort_by(f64::total_cmp);
    let mean = lengths.iter().sum::<f64>() / lengths.len().max(1) as f64;
    let [cpu, gpu] = [0, 1].map(|side| {
        let mut times: Vec<f64> =
            work.iter().map(|frame| frame[side]).collect();
        times.sort_by(f64::total_cmp);
        rank(&times, 0.5)
    });
    // No timestamp queries means no GPU times, not an idle GPU.
    let bound = (gpu > 0.0).then_some(if gpu > cpu { "gpu" } else { "cpu" });
    let summary = serde_json::json!({
        "frames": lengths.len(),
        "mean_ms": mean,
        "p50_ms": rank(lengths, 0.5),
        "p95_ms": rank(lengths, 0.95),
        "p99_ms": rank(lengths, 0.99),
        "max_ms": rank(lengths, 1.0),
        "cpu_p50_ms": cpu,
        "gpu_p50_ms": gpu,
        "bound": bound,
    });
    format!("{BENCH_MARKER} {summary}")
}

/// The frame times a game printed with [`BENCH_MARKER`].
#[must_use]
pub fn bench_result(output: &str) -> Option<serde_json::Value> {
    output.lines().find_map(|line| {
        serde_json::from_str(line.strip_prefix(BENCH_MARKER)?.trim()).ok()
    })
}

/// Milliseconds per tick the game reported with [`TICK_TIME_MARKER`].
#[must_use]
pub fn tick_time_ms(output: &str) -> Option<f64> {
    output.lines().find_map(|line| {
        line.strip_prefix(TICK_TIME_MARKER)?.trim().parse().ok()
    })
}

/// One failed Rust, shader, or asset reload, attached to its file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReloadDiagnostic {
    /// `rust`, `shader`, or `asset`.
    pub kind: &'static str,
    pub file: Option<PathBuf>,
    pub line: Option<u32>,
    pub message: String,
}

/// Collects errors from `cargo --message-format short` output and asset hot
/// reload failures printed by a running game. Shader errors are reported by
/// `vulkano_shaders` at build time, so they arrive as Rust build errors that
/// name a shader file.
#[must_use]
pub fn reload_diagnostics(output: &str) -> Vec<ReloadDiagnostic> {
    const SHADERS: [&str; 5] = [".comp", ".vert", ".frag", ".glsl", ".geom"];
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(failure) = line.strip_prefix("hot reload failed: ") {
                let file = failure.split('`').nth(1).map(PathBuf::from);
                return Some(ReloadDiagnostic {
                    kind: "asset",
                    file,
                    line: None,
                    message: failure.to_owned(),
                });
            }
            let (location, message) = match line.find(": error") {
                Some(index) => (&line[..index], &line[index + 2..]),
                None if line.starts_with("error") => ("", line),
                None => return None,
            };
            if message.starts_with("error: could not compile")
                || message.starts_with("error: aborting")
            {
                return None;
            }
            let mut parts = location.rsplitn(3, ':');
            let (column, number, file) =
                (parts.next(), parts.next(), parts.next());
            let (file, number) = match (file, number, column) {
                (Some(file), Some(number), Some(_)) => {
                    (Some(PathBuf::from(file)), number.parse().ok())
                }
                _ => (None, None),
            };
            let shader = SHADERS.iter().any(|extension| {
                message.contains(&format!("{extension}:"))
                    || file.as_ref().is_some_and(|file| {
                        file.to_string_lossy().ends_with(extension)
                    })
            });
            Some(ReloadDiagnostic {
                kind: if shader { "shader" } else { "rust" },
                file,
                line: number,
                message: message.to_owned(),
            })
        })
        .collect()
}

/// Small file that tells the editor how a game project is arranged.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectManifest {
    /// Version used to reject project files that are too new.
    #[serde(default)]
    pub format_version: u32,
    /// Human-readable name shown in the Project Manager.
    pub name: String,
    /// Scene opened when this project starts.
    pub main_scene: PathBuf,
    /// Compact scene file loaded by the built game.
    pub cooked_scene: PathBuf,
    /// Cargo binary copied into exported game folders.
    #[serde(default)]
    pub binary_name: String,
    /// Optional asset generator hooks by name; see
    /// [`crate::asset_import::GeneratorHook`]. Projects need none.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub generators:
        std::collections::BTreeMap<String, crate::asset_import::GeneratorHook>,
    /// Optional scene generators by name; see
    /// [`crate::asset_import::SceneGenerator`] and `rusting scene generate`.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub scene_generators:
        std::collections::BTreeMap<String, crate::asset_import::SceneGenerator>,
    /// How strictly the simulation must reproduce itself; see
    /// [`crate::runtime::DeterminismMode`]. `cook` copies it into the
    /// cooked scene.
    #[serde(default, skip_serializing_if = "is_default")]
    pub determinism: crate::runtime::DeterminismMode,
    /// Window size in pixels, `[width, height]`, asked for when the game
    /// opens from the project folder; `None` keeps the default window.
    /// Game code can still change it with `GameScene::set_window_size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<[u32; 2]>,
    /// Window title shown when the game opens from the project folder;
    /// `None` keeps the runner's title. Game code can change it with
    /// `GameScene::set_window_title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    /// Screen size in pixels, `[width, height]`, the HUD is laid out for.
    /// The HUD then scales uniformly to fit any window, keeping its aspect
    /// ratio; `None` draws it at the desktop's DPI scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_base_size: Option<[u32; 2]>,
    /// Window icon image, relative to the project folder; put it under
    /// `assets/` so exports carry it. `None` keeps the system's icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<PathBuf>,
    /// Named export settings, one per platform or build kind; `rusting
    /// export <project> <folder> --preset <name>` uses one.
    #[serde(
        default,
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub export_presets: std::collections::BTreeMap<String, ExportPreset>,
}

/// One named way to export a game, kept in `project.json` under
/// `export_presets`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportPreset {
    /// Rust target triple to build for, such as `x86_64-pc-windows-gnu`;
    /// `None` builds for this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Cargo features of the game crate to turn on for this export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// Files under `assets/` and `scenes/` to leave out, as paths relative
    /// to the project folder: `*` matches within one folder name, `**` any
    /// number of folders, as in `assets/source/**` or `**/*.psd`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
    /// Code-signing command run on the exported executable before the
    /// export is verified: the program, then its arguments, where `{exe}`
    /// becomes the executable's path. It runs in the project folder, and a
    /// non-zero exit fails the export.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sign: Vec<String>,
    /// Pack the exported `assets/` and `scenes/` folders into
    /// `assets.rpak` and `scenes.rpak` after excluded files are removed.
    /// The game reads a file missing on disk from the pack beside its
    /// folder, so asset paths do not change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pack: bool,
    /// Zip the finished export folder into `<folder>.zip` beside it, once
    /// it is signed and verified, ready to upload.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zip: bool,
}

/// Whether `path` (relative, `/`-separated) matches `pattern`, where `*`
/// matches any part of one path segment and a `**` segment any number of
/// segments.
#[must_use]
pub fn path_matches(pattern: &str, path: &str) -> bool {
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => {
                (0..=path.len()).any(|skip| segments(rest, &path[skip..]))
            }
            Some((first, rest)) => {
                path.split_first().is_some_and(|(name, others)| {
                    segment(first.as_bytes(), name.as_bytes())
                        && segments(rest, others)
                })
            }
        }
    }
    fn segment(pattern: &[u8], name: &[u8]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some((b'*', rest)) => {
                (0..=name.len()).any(|skip| segment(rest, &name[skip..]))
            }
            Some((first, rest)) => name
                .split_first()
                .is_some_and(|(c, others)| c == first && segment(rest, others)),
        }
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

/// Deletes the files under `assets/` and `scenes/` of an export folder
/// that match any of `patterns` (see [`ExportPreset::exclude`]), and
/// returns their paths relative to the folder, sorted. Nothing else in the
/// folder is touched.
///
/// # Errors
/// Returns the I/O error when a matching file cannot be deleted.
pub fn remove_excluded(
    export: &std::path::Path,
    patterns: &[String],
) -> std::io::Result<Vec<String>> {
    fn walk(folder: &std::path::Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&export.join("assets"), &mut files);
    walk(&export.join("scenes"), &mut files);
    let mut removed = Vec::new();
    for file in files {
        let Ok(relative) = file.strip_prefix(export) else {
            continue;
        };
        let relative = relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if patterns
            .iter()
            .any(|pattern| path_matches(pattern, &relative))
        {
            std::fs::remove_file(&file)?;
            removed.push(relative);
        }
    }
    removed.sort();
    Ok(removed)
}

/// Packs the `assets/` and `scenes/` folders of an export into
/// `<folder>.rpak` beside them and removes the loose folders; returns the
/// pack names written.
pub fn pack_export(export: &std::path::Path) -> std::io::Result<Vec<String>> {
    let mut packed = Vec::new();
    for name in ["assets", "scenes"] {
        let folder = export.join(name);
        if !folder.is_dir() {
            continue;
        }
        let pack = format!("{name}.rpak");
        crate::assets::pack_folder(&folder, &export.join(&pack))?;
        std::fs::remove_dir_all(&folder)?;
        packed.push(pack);
    }
    Ok(packed)
}

/// Writes `folder` into a zip file at `out`, with every file under the
/// folder's own name. Files are stored without compression (compressing
/// needs a dependency); Unix file modes are kept, so the executable stays
/// executable after unzipping, and the same folder gives the same bytes.
/// Fails on more than 65,535 files or 4 GiB, which needs Zip64.
pub fn zip_folder(
    folder: &std::path::Path,
    out: &std::path::Path,
) -> std::io::Result<()> {
    let written = write_zip(folder, out);
    if written.is_err() {
        let _ = std::fs::remove_file(out);
    }
    written
}

fn write_zip(
    folder: &std::path::Path,
    out: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::{Error, Read, Write};
    let top = folder
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::other("the folder has no UTF-8 name"))?;
    let mut files = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    let too_big =
        || Error::other("the export needs Zip64 (over 65,535 files or 4 GiB)");
    let count = u16::try_from(files.len()).map_err(|_| too_big())?;
    let mut zip = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut central = Vec::new();
    let mut offset = 0u64;
    for file in &files {
        let relative = file.strip_prefix(folder).map_err(Error::other)?;
        let relative = relative.to_str().ok_or_else(|| {
            Error::other(format!("{} is not UTF-8", file.display()))
        })?;
        let name = format!("{top}/{}", relative.replace('\\', "/"));
        let meta = std::fs::metadata(file)?;
        #[cfg(unix)]
        let mode = std::os::unix::fs::PermissionsExt::mode(&meta.permissions());
        #[cfg(not(unix))]
        let mode = 0o100_644;
        let size = u32::try_from(meta.len()).map_err(|_| too_big())?;
        let mut crc = !0u32;
        let mut reader = std::fs::File::open(file)?;
        let mut buffer = vec![0; 1 << 16];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            crc = crc32_update(crc, &buffer[..read]);
        }
        let crc = !crc;
        let start = u32::try_from(offset).map_err(|_| too_big())?;
        // Version 2.0, UTF-8 names, stored, DOS date 1980-01-01 00:00.
        let shared = |bytes: &mut Vec<u8>| {
            for half in [20u16, 0x0800, 0, 0, 0x21] {
                bytes.extend(half.to_le_bytes());
            }
            for word in [crc, size, size] {
                bytes.extend(word.to_le_bytes());
            }
            bytes.extend((name.len() as u16).to_le_bytes());
            bytes.extend(0u16.to_le_bytes());
        };
        let mut local = 0x0403_4b50u32.to_le_bytes().to_vec();
        shared(&mut local);
        local.extend(name.as_bytes());
        zip.write_all(&local)?;
        let copied = std::io::copy(
            &mut std::fs::File::open(file)?.take(size.into()),
            &mut zip,
        )?;
        if copied != u64::from(size) {
            return Err(Error::other(format!(
                "{} changed while zipping",
                file.display()
            )));
        }
        offset += local.len() as u64 + copied;
        central.extend(0x0201_4b50u32.to_le_bytes());
        // Made by Unix, so the external attributes hold the file mode.
        central.extend((3u16 << 8 | 20).to_le_bytes());
        shared(&mut central);
        central.extend([0; 6]); // Comment length, disk, internal attributes.
        central.extend((mode << 16).to_le_bytes());
        central.extend(start.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let central_start = u32::try_from(offset).map_err(|_| too_big())?;
    let central_len = u32::try_from(central.len()).map_err(|_| too_big())?;
    zip.write_all(&central)?;
    zip.write_all(&0x0605_4b50u32.to_le_bytes())?;
    zip.write_all(&[0; 4])?;
    zip.write_all(&count.to_le_bytes())?;
    zip.write_all(&count.to_le_bytes())?;
    zip.write_all(&central_len.to_le_bytes())?;
    zip.write_all(&central_start.to_le_bytes())?;
    zip.write_all(&[0; 2])?;
    zip.flush()
}

/// Folds `bytes` into a running CRC-32 (the zip and PNG polynomial).
fn crc32_update(mut crc: u32, bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        std::array::from_fn(|index| {
            (0..8).fold(index as u32, |c, _| {
                if c & 1 == 1 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                }
            })
        })
    });
    for &byte in bytes {
        crc = table[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// Valid project paths returned after create or open succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenProject {
    /// Absolute folder containing `project.json` and `Cargo.toml`.
    pub root: PathBuf,
    /// Checked project settings loaded from `project.json`.
    pub manifest: ProjectManifest,
    /// Absolute path to the main editable scene.
    pub scene_path: PathBuf,
    /// Absolute path opened by Code Editor first.
    pub code_path: PathBuf,
}

/// One project displayed in the recent-project list.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecentProject {
    /// Name last read from the project manifest.
    pub name: String,
    /// Absolute path to the project folder.
    pub path: PathBuf,
}

/// Project Manager values that survive between GUI frames.
#[derive(Resource, Clone, Debug)]
pub struct ProjectManagerState {
    /// True while the Project Manager covers the main editor.
    pub open: bool,
    /// Name typed into the Create Project form.
    pub project_name: String,
    /// Parent folder selected for the new project.
    pub parent_directory: PathBuf,
    /// Starting content of the new project.
    pub template: ProjectTemplate,
    /// Projects loaded from the editor settings file.
    pub recent_projects: Vec<RecentProject>,
    /// Last create, open, or validation message.
    pub message: Option<String>,
}

impl Default for ProjectManagerState {
    fn default() -> Self {
        Self {
            open: true,
            project_name: "MyGame".into(),
            // An empty path forces the user to choose where the project lives.
            parent_directory: PathBuf::new(),
            template: ProjectTemplate::default(),
            recent_projects: load_recent_projects().unwrap_or_default(),
            message: None,
        }
    }
}

/// Errors shown by the Project Manager instead of crashing the editor.
#[derive(Debug)]
pub enum ProjectError {
    /// Project name is empty or cannot be used as a folder name.
    InvalidName,
    /// Cargo executable name is unsafe or invalid.
    InvalidBinaryName,
    /// Selected parent folder does not exist.
    MissingParent(PathBuf),
    /// Target project folder already exists, so nothing was overwritten.
    AlreadyExists(PathBuf),
    /// Required project file is missing.
    MissingFile(PathBuf),
    /// Project file was created by a newer editor.
    UnsupportedVersion(u32),
    /// File-system operation failed.
    Io(std::io::Error),
    /// JSON project or scene data could not be read.
    Json(serde_json::Error),
}

impl Display for ProjectError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName => formatter.write_str(
                "Project name must contain letters, numbers, spaces, - or _",
            ),
            Self::InvalidBinaryName => formatter.write_str(
                "Project binary name must contain only letters, numbers, - or _",
            ),
            Self::MissingParent(path) => write!(
                formatter,
                "Parent directory `{}` does not exist",
                path.display()
            ),
            Self::AlreadyExists(path) => write!(
                formatter,
                "Project folder `{}` already exists; no files were changed",
                path.display()
            ),
            Self::MissingFile(path) => {
                write!(formatter, "Required file `{}` is missing", path.display())
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "Project format {version} is newer than supported format {PROJECT_FORMAT_VERSION}"
            ),
            Self::Io(error) => Display::fmt(error, formatter),
            Self::Json(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ProjectError {}

impl From<std::io::Error> for ProjectError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for ProjectError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Creates a complete game project without overwriting an existing folder.
///
/// # Arguments
/// * `parent` - Existing folder that will contain the project.
/// * `name` - Project and folder name selected by the user.
pub fn create_project(
    parent: &Path,
    name: &str,
) -> Result<OpenProject, ProjectError> {
    create_project_from(parent, name, ProjectTemplate::Basic3d)
}

/// Starting content of a new project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProjectTemplate {
    /// A lit cube and a perspective camera.
    #[default]
    Basic3d,
    /// A playable side-view level: tile map, platformer player with a
    /// following orthographic camera, goal burst, and HUD text.
    Platformer2d,
    /// Coin Run, a complete small game on the 2D template: collect every
    /// coin, then touch the flag to win.
    Starter,
    /// A walkable 3D room with crates and a first-person player.
    FirstPerson3d,
    /// The first-person room with a visible player body and an orbiting
    /// camera behind it.
    ThirdPerson3d,
    /// A pile of dynamic boxes and balls that fall onto a floor and settle.
    PhysicsSandbox,
    /// The 3D template without its cube: only a camera, so game
    /// code can spawn any name.
    Empty,
    /// Box Push, a grid puzzle with game code: step on a grid and push
    /// every box onto a goal.
    Puzzle,
    /// Arena, a top-down action game with code: move, attack the
    /// enemies that chase the player, and win before taking three hits.
    TopDown,
    /// Circuit, a racing game with code: drive three laps through the
    /// checkpoints of a rectangular track.
    Racing,
    /// Swarm, a twin-stick shooter with code: move with one stick, aim and
    /// fire with the other, and defeat a wave of twelve enemies.
    TwinStick,
    /// Outpost, a tower defense game with code: build towers on pads
    /// beside a path and stop a wave of enemies reaching the base.
    TowerDefense,
    /// Crypt, a roguelike with code: step through three floors made from
    /// the run's seed, defeating enemies on the way to the stairs.
    Roguelike,
    /// Duel, a card battle with code: play Strike, Guard and Heal cards from
    /// a seeded deck against a foe whose next attack shows in advance.
    CardGame,
    /// Beat, a rhythm game with code: hit notes in three lanes as they
    /// cross the line, scored by timing.
    Rhythm,
}

impl ProjectTemplate {
    /// Every template, in the order pickers list them.
    pub const ALL: [Self; 15] = [
        Self::Basic3d,
        Self::FirstPerson3d,
        Self::ThirdPerson3d,
        Self::PhysicsSandbox,
        Self::Platformer2d,
        Self::Starter,
        Self::Puzzle,
        Self::TopDown,
        Self::Racing,
        Self::TwinStick,
        Self::TowerDefense,
        Self::Roguelike,
        Self::CardGame,
        Self::Rhythm,
        Self::Empty,
    ];

    /// The name `rusting new --template` takes.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Basic3d => "3d",
            Self::Platformer2d => "2d",
            Self::Starter => "starter",
            Self::FirstPerson3d => "first-person",
            Self::ThirdPerson3d => "third-person",
            Self::PhysicsSandbox => "sandbox",
            Self::Empty => "empty",
            Self::Puzzle => "puzzle",
            Self::TopDown => "top-down",
            Self::Racing => "racing",
            Self::TwinStick => "twin-stick",
            Self::TowerDefense => "tower-defense",
            Self::Roguelike => "roguelike",
            Self::CardGame => "card-game",
            Self::Rhythm => "rhythm",
        }
    }

    /// A short label for pickers.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Basic3d => "Empty 3D",
            Self::Platformer2d => "2D platformer",
            Self::Starter => "Coin Run (2D game)",
            Self::FirstPerson3d => "3D first person",
            Self::ThirdPerson3d => "3D third person",
            Self::PhysicsSandbox => "Physics sandbox",
            Self::Empty => "Empty (camera only)",
            Self::Puzzle => "Box Push (grid puzzle)",
            Self::TopDown => "Arena (top-down action)",
            Self::Racing => "Circuit (racing)",
            Self::TwinStick => "Swarm (twin-stick shooter)",
            Self::TowerDefense => "Outpost (tower defense)",
            Self::Roguelike => "Crypt (roguelike)",
            Self::CardGame => "Duel (card game)",
            Self::Rhythm => "Beat (rhythm)",
        }
    }

    /// Inverse of [`Self::name`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|template| template.name() == name)
    }
}

/// [`create_project`] with a chosen [`ProjectTemplate`].
pub fn create_project_from(
    parent: &Path,
    name: &str,
    template: ProjectTemplate,
) -> Result<OpenProject, ProjectError> {
    validate_project_name(name)?;
    if !parent.is_dir() {
        return Err(ProjectError::MissingParent(parent.to_owned()));
    }
    let root = parent.join(name.trim());
    if root.exists() {
        return Err(ProjectError::AlreadyExists(root));
    }

    // A temporary sibling keeps half-written projects out of the chosen path.
    let temporary = parent.join(format!(".rusting-project-{}", Uuid::new_v4()));
    let result = write_project_template(&temporary, name.trim(), template)
        .and_then(|()| {
            std::fs::rename(&temporary, &root).map_err(ProjectError::from)
        });
    if result.is_err() && temporary.exists() {
        // Cleanup is safe because the random folder was created by this call.
        let _ = std::fs::remove_dir_all(&temporary);
    }
    result?;
    open_project(&root)
}

/// Opens and validates an existing RustingEngine project folder.
pub fn open_project(root: &Path) -> Result<OpenProject, ProjectError> {
    let root = root.canonicalize()?;
    let manifest_path = root.join("project.json");
    let cargo_path = root.join("Cargo.toml");
    for required in [&manifest_path, &cargo_path] {
        if !required.is_file() {
            return Err(ProjectError::MissingFile(required.to_path_buf()));
        }
    }
    let manifest_bytes = std::fs::read(&manifest_path)?;
    let mut manifest: ProjectManifest =
        serde_json::from_slice(&manifest_bytes)?;
    let stored_manifest = manifest.clone();
    if manifest.format_version > PROJECT_FORMAT_VERSION {
        return Err(ProjectError::UnsupportedVersion(manifest.format_version));
    }
    if manifest.format_version == 0 {
        // Version 0 is the old unversioned project.json shape.
        let backup = root.join("project.json.v0.backup");
        if !backup.exists() {
            std::fs::copy(&manifest_path, &backup)?;
        }
        manifest.format_version = PROJECT_FORMAT_VERSION;
    }
    if manifest.binary_name.is_empty() {
        manifest.binary_name = cargo_package_name(&manifest.name);
    }
    if manifest.binary_name.is_empty()
        || manifest.binary_name.chars().any(|character| {
            !(character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_'))
        })
    {
        return Err(ProjectError::InvalidBinaryName);
    }
    // Write fields added by migration only after every value was validated.
    if manifest != stored_manifest {
        crate::runtime::write_atomic(
            &manifest_path,
            &serde_json::to_vec_pretty(&manifest)?,
        )?;
    }
    let scene_path = checked_project_path(&root, &manifest.main_scene)?;
    if !scene_path.exists() {
        return Err(ProjectError::MissingFile(scene_path));
    }
    // The cooked file may not exist yet, but its configured destination must
    // still remain inside the project.
    checked_project_path(&root, &manifest.cooked_scene)?;
    let code_path = root.join("src/main.rs");
    if !code_path.is_file() {
        return Err(ProjectError::MissingFile(code_path));
    }
    Ok(OpenProject {
        root,
        manifest,
        scene_path,
        code_path,
    })
}

/// Adds a project to the top of the recent list and saves the list.
pub fn remember_project(
    recent: &mut Vec<RecentProject>,
    project: &OpenProject,
) -> Result<(), ProjectError> {
    recent.retain(|item| item.path != project.root);
    recent.insert(
        0,
        RecentProject {
            name: project.manifest.name.clone(),
            path: project.root.clone(),
        },
    );
    recent.truncate(12);
    save_recent_projects(recent)
}

fn validate_project_name(name: &str) -> Result<(), ProjectError> {
    let name = name.trim();
    let upper_name = name.to_ascii_uppercase();
    let windows_device_name = matches!(
        upper_name.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if name.is_empty()
        || name == "."
        || name == ".."
        || windows_device_name
        || name.chars().any(|character| {
            !(character.is_ascii_alphanumeric()
                || matches!(character, ' ' | '-' | '_'))
        })
    {
        return Err(ProjectError::InvalidName);
    }
    Ok(())
}

fn checked_project_path(
    root: &Path,
    relative: &Path,
) -> Result<PathBuf, ProjectError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(ProjectError::MissingFile(root.join(relative)));
    }
    Ok(root.join(relative))
}

fn write_project_template(
    root: &Path,
    name: &str,
    template: ProjectTemplate,
) -> Result<(), ProjectError> {
    for folder in ["src", "scenes", "assets", "shaders", "build"] {
        std::fs::create_dir_all(root.join(folder))?;
    }

    let package_name = cargo_package_name(name);
    let engine_source = Path::new(env!("CARGO_MANIFEST_DIR"));
    let engine_dependency = if engine_source.join("Cargo.toml").is_file() {
        let engine_path = engine_source.to_string_lossy().replace('\\', "\\\\");
        format!("path = \"{engine_path}\"")
    } else {
        // A downloaded editor has no source checkout beside its executable.
        // Use the matching GitHub release tag for generated game projects.
        format!(
            "git = \"https://github.com/GoingRusting/RustingEngine\", tag = \"v{}\"",
            env!("CARGO_PKG_VERSION")
        )
    };
    let cargo = format!(
        "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nrusting_engine = {{ {engine_dependency}, default-features = false, features = [\"ui\"] }}\n\n[workspace]\n\n# Debug builds of the engine are too slow to play: optimize dependencies\n# and keep the game's own code quick to rebuild.\n[profile.dev.package.\"*\"]\nopt-level = 3\n"
    );
    std::fs::write(root.join("Cargo.toml"), cargo)?;

    let manifest = ProjectManifest {
        format_version: PROJECT_FORMAT_VERSION,
        name: name.into(),
        main_scene: "scenes/main.rscene".into(),
        cooked_scene: "build/main.rscene.bin".into(),
        binary_name: package_name.clone(),
        generators: std::collections::BTreeMap::new(),
        scene_generators: std::collections::BTreeMap::new(),
        determinism: Default::default(),
        window: None,
        ui_base_size: None,
        window_title: None,
        icon: None,
        export_presets: std::collections::BTreeMap::new(),
    };
    std::fs::write(
        root.join("project.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    std::fs::write(
        root.join("src/main.rs"),
        match template {
            ProjectTemplate::Puzzle => include_str!("templates/puzzle.rs"),
            ProjectTemplate::TopDown => include_str!("templates/arena.rs"),
            ProjectTemplate::Racing => include_str!("templates/racing.rs"),
            ProjectTemplate::TwinStick => {
                include_str!("templates/twin_stick.rs")
            }
            ProjectTemplate::TowerDefense => {
                include_str!("templates/tower_defense.rs")
            }
            ProjectTemplate::Roguelike => {
                include_str!("templates/roguelike.rs")
            }
            ProjectTemplate::CardGame => include_str!("templates/card_game.rs"),
            ProjectTemplate::Rhythm => include_str!("templates/rhythm.rs"),
            _ => default_game_source(),
        },
    )?;
    let scenario = match template {
        ProjectTemplate::Puzzle => {
            Some(("solve.json", include_str!("templates/puzzle_solve.json")))
        }
        ProjectTemplate::TopDown => {
            Some(("fight.json", include_str!("templates/arena_fight.json")))
        }
        ProjectTemplate::Racing => {
            Some(("lap.json", include_str!("templates/racing_lap.json")))
        }
        ProjectTemplate::TwinStick => {
            Some(("wave.json", include_str!("templates/twin_stick_wave.json")))
        }
        ProjectTemplate::TowerDefense => Some((
            "defend.json",
            include_str!("templates/tower_defense_defend.json"),
        )),
        ProjectTemplate::Roguelike => Some((
            "descend.json",
            include_str!("templates/roguelike_descend.json"),
        )),
        ProjectTemplate::CardGame => {
            Some(("duel.json", include_str!("templates/card_game_duel.json")))
        }
        ProjectTemplate::Rhythm => {
            Some(("song.json", include_str!("templates/rhythm_song.json")))
        }
        ProjectTemplate::Platformer2d => {
            Some(("run.json", include_str!("templates/platformer_run.json")))
        }
        ProjectTemplate::FirstPerson3d | ProjectTemplate::ThirdPerson3d => {
            Some(("walk.json", include_str!("templates/walk_forward.json")))
        }
        ProjectTemplate::PhysicsSandbox => {
            Some(("drop.json", include_str!("templates/sandbox_drop.json")))
        }
        _ => None,
    };
    if let Some((file, text)) = scenario {
        std::fs::create_dir_all(root.join("tests"))?;
        std::fs::write(root.join("tests").join(file), text)?;
    }
    std::fs::write(root.join("AGENTS.md"), include_str!("project_agents.md"))?;
    std::fs::create_dir_all(root.join("skills/rusting-game"))?;
    std::fs::write(
        root.join("skills/rusting-game/SKILL.md"),
        include_str!("../skills/rusting-game/SKILL.md"),
    )?;
    std::fs::write(root.join(".gitignore"), "/target\n/build\n/tests/shots\n")?;
    // Scene merges go through `rusting merge` once the driver is configured.
    std::fs::write(
        root.join(".gitattributes"),
        "*.rscene merge=rusting-scene\n",
    )?;
    std::fs::write(
        root.join("scenes/main.rscene"),
        serde_json::to_vec_pretty(&match template {
            ProjectTemplate::Basic3d => default_scene(name),
            ProjectTemplate::Empty => {
                let mut scene = default_scene(name);
                scene
                    .entities
                    .retain(|entity| entity.name.as_deref() != Some("Cube"));
                scene
            }
            ProjectTemplate::Platformer2d => platformer_scene(name),
            ProjectTemplate::Starter => starter_scene(name),
            ProjectTemplate::Puzzle => puzzle_scene(name),
            ProjectTemplate::TopDown => arena_scene(name),
            ProjectTemplate::Racing => racing_scene(name),
            ProjectTemplate::TwinStick => twin_stick_scene(name),
            ProjectTemplate::TowerDefense => tower_defense_scene(name),
            ProjectTemplate::Roguelike => roguelike_scene(name),
            ProjectTemplate::CardGame => card_game_scene(name),
            ProjectTemplate::Rhythm => rhythm_scene(name),
            ProjectTemplate::FirstPerson3d
            | ProjectTemplate::ThirdPerson3d
            | ProjectTemplate::PhysicsSandbox => scene_3d(name, template),
        })?,
    )?;
    Ok(())
}

fn cargo_package_name(name: &str) -> String {
    let mut package = String::new();
    let mut previous_separator = false;
    for character in name.trim().chars() {
        if character.is_alphanumeric() {
            package.extend(character.to_lowercase());
            previous_separator = false;
        } else if !previous_separator {
            package.push('_');
            previous_separator = true;
        }
    }
    package.trim_matches('_').to_owned()
}

fn default_game_source() -> &'static str {
    "use rusting_engine::prelude::*;\n\nfn update(_scene: &mut GameScene<'_>, _time: &FrameTime) {\n    // Add game behaviour here.\n}\n\nrusting_game!(update);\n"
}

fn default_scene(name: &str) -> SceneDocument {
    let cube = Uuid::new_v4();
    let camera = Uuid::new_v4();
    SceneDocument {
        format_version: SCENE_FORMAT_VERSION,
        name: format!("{name} Main Scene"),
        entities: vec![
            SceneEntity {
                id: cube,
                parent: None,
                name: Some("Cube".into()),
                classes: Vec::new(),
                transform: Some(SceneTransform {
                    position: [0.0, 0.0, 0.0],
                    rotation: [0.0, 0.0, 0.0],
                    scale: [1.0, 1.0, 1.0],
                }),
                mesh_renderer: Some(SceneMeshRenderer {
                    mesh: SceneMesh::BuiltinCube,
                    material: SceneMaterial::Inline(SceneMaterialData {
                        name: String::new(),
                        model: SceneMaterialModel::Pbr,
                        alpha_mode: SceneAlphaMode::Opaque,
                        base_color: [0.1, 0.45, 0.95, 1.0],
                        emissive: [0.0; 3],
                        metallic: 0.0,
                        roughness: 0.5,
                        transmission: 0.0,
                        ior: crate::assets::MaterialAsset::default().ior,
                        thickness: 0.0,
                        uv_scale: [1.0; 2],
                        uv_offset: [0.0; 2],
                        flat_shading: false,
                        base_color_texture: None,
                        normal_texture: None,
                        metallic_roughness_texture: None,
                        occlusion_texture: None,
                        emissive_texture: None,
                    }),
                    cast_shadows: true,
                    receive_shadows: true,
                }),
                camera: None,
                visible: Some(true),
                physics_body: None,
                rigid_body: None,
                collider: None,
                collision_layers: None,
                gpu_physics_watch: None,
                components: BTreeMap::new(),
                directional_light: None,
                point_light: None,
                spot_light: None,
            },
            SceneEntity {
                id: camera,
                parent: None,
                name: Some("Game Camera".into()),
                classes: Vec::new(),
                transform: Some(SceneTransform {
                    position: [0.0, 3.0, 8.0],
                    rotation: [0.0, 0.0, 0.0],
                    scale: [1.0; 3],
                }),
                mesh_renderer: None,
                camera: Some(SceneCamera {
                    projection: SceneProjection::Perspective {
                        vertical_fov_radians: std::f32::consts::FRAC_PI_3,
                        near: 0.1,
                        far: 1_000.0,
                    },
                    active: true,
                    priority: 10,
                    viewport: None,
                }),
                visible: None,
                physics_body: None,
                rigid_body: None,
                collider: None,
                collision_layers: None,
                gpu_physics_watch: None,
                components: BTreeMap::new(),
                directional_light: None,
                point_light: None,
                spot_light: None,
            },
        ],
        render: Default::default(),
        simulation: Default::default(),
    }
}

fn platformer_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{
        BurstEmitter, HudElement, PlatformerController, TileKind, TileMap,
    };

    let sprite = |color: [f32; 4]| {
        json!({
            "mesh": {"BuiltinPrimitive": "Quad"},
            "material": {"Inline": {
                "model": "Unlit", "alpha_mode": "Opaque", "base_color": color,
                "emissive": [0.0, 0.0, 0.0], "metallic": 0.0, "roughness": 1.0,
                "base_color_texture": null, "normal_texture": null,
                "metallic_roughness_texture": null,
                "occlusion_texture": null, "emissive_texture": null
            }},
            "cast_shadows": false, "receive_shadows": false
        })
    };
    let transform = |position: [f32; 3], scale: [f32; 3]| json!({"position": position, "rotation": [0.0, 0.0, 0.0], "scale": scale});
    let level = TileMap {
        tile_size: 1.0,
        rows: [
            "....................",
            "....................",
            "....................",
            "..............BBB...",
            "........BBB.........",
            "....................",
            "...BB...........###.",
            "#######...##########",
            "####################",
        ]
        .map(String::from)
        .to_vec(),
        tiles: BTreeMap::from([
            (
                "#".into(),
                TileKind {
                    color: [0.25, 0.55, 0.3, 1.0],
                    ..TileKind::default()
                },
            ),
            (
                "B".into(),
                TileKind {
                    color: [0.55, 0.35, 0.2, 1.0],
                    ..TileKind::default()
                },
            ),
        ]),
    };
    let goal_burst = BurstEmitter {
        count: 24,
        speed: 4.0,
        lifetime: 0.8,
        particle_scale: 0.25,
        ..BurstEmitter::default()
    };
    let help = HudElement {
        text: "A/D or arrows run, Space jumps. Reach the gold block.".into(),
        ..HudElement::default()
    };
    let player = Uuid::new_v4();
    let entities = json!([
        {
            "id": Uuid::new_v4(), "parent": null, "name": "Level",
            "transform": transform([-10.0, 6.0, 0.0], [1.0, 1.0, 1.0]),
            "components": {"rusting.tile_map": component(&level)}
        },
        {
            "id": player, "parent": null, "name": "Player",
            "transform": transform([-8.0, 0.5, 0.0], [1.0, 1.0, 1.0]),
            "physics_body": {"simulation": "Cpu", "solver": "Simplified", "custom_shader": null},
            "rigid_body": {"kind": "Kinematic", "mass": 1.0, "linear_velocity": [0.0, 0.0, 0.0], "angular_velocity": [0.0, 0.0, 0.0], "gravity_scale": 1.0},
            "collider": {"shape": {"Box": {"half_extents": [0.3, 0.5, 0.3]}}, "friction": 0.0, "restitution": 0.0, "sensor": false},
            "components": {
                "rusting.platformer_controller":
                    component(&PlatformerController::default())
            }
        },
        {
            "id": Uuid::new_v4(), "parent": player, "name": "Player Sprite",
            "transform": transform([0.0, 0.0, 0.1], [0.6, 1.0, 1.0]),
            "mesh_renderer": sprite([0.95, 0.45, 0.15, 1.0]),
            "visible": true
        },
        {
            "id": Uuid::new_v4(), "parent": player, "name": "Game Camera",
            "transform": transform([0.0, 1.0, 10.0], [1.0, 1.0, 1.0]),
            "camera": {
                "projection": {"Orthographic": {"vertical_size": 12.0, "near": 0.1, "far": 100.0}},
                "active": true, "priority": 10
            }
        },
        {
            "id": Uuid::new_v4(), "parent": null, "name": "Goal",
            "transform": transform([8.5, 0.5, 0.05], [0.6, 1.0, 1.0]),
            "mesh_renderer": sprite([1.0, 0.8, 0.1, 1.0]),
            "visible": true,
            "physics_body": {"simulation": "Cpu", "solver": "Simplified", "custom_shader": null},
            "rigid_body": {"kind": "Fixed", "mass": 1.0, "linear_velocity": [0.0, 0.0, 0.0], "angular_velocity": [0.0, 0.0, 0.0], "gravity_scale": 1.0},
            "collider": {"shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}}, "friction": 0.5, "restitution": 0.0, "sensor": true},
            "components": {"rusting.burst_emitter": component(&goal_burst)}
        },
        {
            "id": Uuid::new_v4(), "parent": null, "name": "Help",
            "components": {"rusting.hud": component(&help)}
        }
    ]);
    SceneDocument {
        format_version: SCENE_FORMAT_VERSION,
        name: format!("{name} Main Scene"),
        entities: serde_json::from_value(entities)
            .expect("the 2D template is a valid scene"),
        render: Default::default(),
        simulation: Default::default(),
    }
}

/// The first-person, third-person, and physics sandbox templates. They
/// share a lit floor; the player templates add crates and a
/// `PlayerController`, the sandbox a pile of dynamic bodies and a fixed
/// camera.
fn scene_3d(name: &str, template: ProjectTemplate) -> SceneDocument {
    use serde_json::{json, Value};

    use crate::runtime::{HudElement, PlayerController};

    let mesh = |shape: &str, color: [f32; 3]| {
        json!({
            "mesh": {"BuiltinPrimitive": shape},
            "material": {"Inline": {
                "model": "Pbr", "alpha_mode": "Opaque",
                "base_color": [color[0], color[1], color[2], 1.0],
                "emissive": [0.0, 0.0, 0.0], "metallic": 0.0, "roughness": 0.7,
                "base_color_texture": null, "normal_texture": null,
                "metallic_roughness_texture": null,
                "occlusion_texture": null, "emissive_texture": null
            }},
            "cast_shadows": true, "receive_shadows": true
        })
    };
    let transform = |position: [f32; 3], scale: [f32; 3]| json!({"position": position, "rotation": [0.0, 0.0, 0.0], "scale": scale});
    let physics = json!({"simulation": "Cpu", "solver": "Simplified", "custom_shader": null});
    let body = |kind: &str| json!({"kind": kind, "mass": 1.0, "linear_velocity": [0.0, 0.0, 0.0], "angular_velocity": [0.0, 0.0, 0.0], "gravity_scale": 1.0});
    let collider = |shape: Value| json!({"shape": shape, "friction": 0.5, "restitution": 0.0, "sensor": false});
    // A box of `size` whose mesh matches its collider.
    let block = |name: &str,
                 kind: &str,
                 position: [f32; 3],
                 size: [f32; 3],
                 color| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": name,
            "transform": transform(position, size),
            "mesh_renderer": mesh("Cube", color), "visible": true,
            "physics_body": physics, "rigid_body": body(kind),
            "collider": collider(json!({"Box": {"half_extents": [0.5, 0.5, 0.5]}}))
        })
    };

    let mut entities = vec![
        // Top face at y = 0.
        block(
            "Floor",
            "Fixed",
            [0.0, -0.5, 0.0],
            [30.0, 1.0, 30.0],
            [0.35, 0.37, 0.4],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
    ];
    let help = if template == ProjectTemplate::PhysicsSandbox {
        for (index, x) in [-1.1_f32, 0.0, 1.1].into_iter().enumerate() {
            for level in 0..3 {
                entities.push(block(
                    &format!("Box {}", level * 3 + index + 1),
                    "Dynamic",
                    [x, 0.5 + 1.05 * level as f32, 0.0],
                    [1.0; 3],
                    [0.9, 0.55 - 0.15 * level as f32, 0.2],
                ));
            }
        }
        // Low walls keep rolling balls on the floor.
        for (index, (position, size)) in [
            ([0.0, 0.5, -15.0], [30.0, 1.0, 0.5]),
            ([0.0, 0.5, 15.0], [30.0, 1.0, 0.5]),
            ([-15.0, 0.5, 0.0], [0.5, 1.0, 30.0]),
            ([15.0, 0.5, 0.0], [0.5, 1.0, 30.0]),
        ]
        .into_iter()
        .enumerate()
        {
            entities.push(block(
                &format!("Wall {}", index + 1),
                "Fixed",
                position,
                size,
                [0.3, 0.32, 0.35],
            ));
        }
        for (index, x) in [-0.6_f32, 0.6].into_iter().enumerate() {
            entities.push(json!({
                "id": Uuid::new_v4(), "parent": null,
                "name": format!("Ball {}", index + 1),
                "transform": transform([x, 6.0 + 2.0 * index as f32, 0.2], [1.0; 3]),
                "mesh_renderer": mesh("Sphere", [0.2, 0.6, 0.95]), "visible": true,
                "physics_body": physics, "rigid_body": body("Dynamic"),
                "collider": collider(json!({"Sphere": {"radius": 0.5}}))
            }));
        }
        entities.push(json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [0.0, 4.0, 10.0], "rotation": [-0.3, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }));
        "Boxes and balls fall and settle. Press Play again to restart."
    } else {
        for (index, (position, size)) in [
            ([-3.0, 0.5, -3.0], [1.0, 1.0, 1.0]),
            ([-1.5, 0.25, -4.0], [1.0, 0.5, 1.0]),
            ([3.0, 0.75, -2.0], [2.0, 1.5, 2.0]),
            ([0.0, 1.5, -12.0], [12.0, 3.0, 1.0]),
        ]
        .into_iter()
        .enumerate()
        {
            entities.push(block(
                &format!("Crate {}", index + 1),
                "Fixed",
                position,
                size,
                [0.6, 0.45, 0.3],
            ));
        }
        let third_person = template == ProjectTemplate::ThirdPerson3d;
        let player = Uuid::new_v4();
        let controller = if third_person {
            // Start looking a little down, over the body's shoulder.
            PlayerController {
                camera_distance: 4.0,
                pitch: -0.3,
                ..PlayerController::default()
            }
        } else {
            PlayerController::default()
        };
        entities.push(json!({
            "id": player, "parent": null, "name": "Player",
            "transform": transform([0.0, 1.0, 4.0], [1.0; 3]),
            "physics_body": physics, "rigid_body": body("Kinematic"),
            "collider": collider(json!({"Capsule": {"half_height": 0.6, "radius": 0.3}})),
            "components": {"rusting.player_controller": component(&controller)}
        }));
        entities.push(json!({
            "id": Uuid::new_v4(), "parent": player, "name": "Game Camera",
            "transform": transform([0.0, 0.7, 0.0], [1.0; 3]),
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.05, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }));
        if third_person {
            entities.push(json!({
                "id": Uuid::new_v4(), "parent": player, "name": "Body",
                "transform": transform([0.0; 3], [0.6, 1.8, 0.6]),
                "mesh_renderer": mesh("Cylinder", [0.95, 0.45, 0.15]),
                "visible": true
            }));
        }
        "Click to look around, Esc frees the mouse. WASD moves, Shift runs, Space jumps."
    };
    let help = HudElement {
        text: help.into(),
        ..HudElement::default()
    };
    entities.push(json!({
        "id": Uuid::new_v4(), "parent": null, "name": "Help",
        "components": {"rusting.hud": component(&help)}
    }));
    SceneDocument {
        format_version: SCENE_FORMAT_VERSION,
        name: format!("{name} Main Scene"),
        entities: serde_json::from_value(Value::Array(entities))
            .expect("the 3D templates are valid scenes"),
        render: Default::default(),
        simulation: Default::default(),
    }
}

/// Number of coins in the starter game.
pub const STARTER_COINS: i32 = 5;

/// The 2D template turned into Coin Run: a flatter level with two blocks,
/// a pit, and a ledge, five coins, a coin counter on the HUD, and a flag
/// that wins once every coin is collected.
fn starter_scene(name: &str) -> SceneDocument {
    use serde_json::{json, Value};

    use crate::runtime::{
        BurstEmitter, Counter, HudAnchor, HudElement, Pickup, SceneBackground,
        SceneEntity, TileKind, TileMap,
    };

    fn find<'a>(
        scene: &'a mut SceneDocument,
        name: &str,
    ) -> &'a mut SceneEntity {
        scene
            .entities
            .iter_mut()
            .find(|entity| entity.name.as_deref() == Some(name))
            .expect("the 2D template has this entity")
    }
    let mut scene = platformer_scene(name);
    let level = find(&mut scene, "Level");
    let mut map: TileMap =
        serde_json::from_str(&level.components["rusting.tile_map"])
            .expect("the 2D template has a tile map");
    map.rows = [
        "......................",
        "......................",
        "......................",
        "#....................#",
        "#....................#",
        "#....................#",
        "#....B.......B....####",
        "#########...##########",
        "######################",
    ]
    .map(String::from)
    .to_vec();
    map.tiles.insert(
        "B".into(),
        TileKind {
            color: [0.55, 0.35, 0.2, 1.0],
            ..TileKind::default()
        },
    );
    level
        .components
        .insert("rusting.tile_map".into(), component(&map));
    level.transform.as_mut().expect("placed").position = [-11.0, 6.0, 0.0];
    find(&mut scene, "Player")
        .transform
        .as_mut()
        .expect("placed")
        .position = [-9.5, 0.5, 0.0];
    // Look ahead of the runner.
    find(&mut scene, "Game Camera")
        .transform
        .as_mut()
        .expect("placed")
        .position = [4.0, 1.0, 10.0];
    let goal = find(&mut scene, "Goal");
    goal.name = Some("Flag".into());
    goal.components.insert(
        "rusting.burst_emitter".into(),
        component(&BurstEmitter {
            count: 32,
            speed: 5.0,
            lifetime: 1.0,
            particle_scale: 0.25,
            on_collision: false,
            ..BurstEmitter::default()
        }),
    );
    goal.components.insert(
        "rusting.pickup".into(),
        component(&Pickup {
            counter: "won".into(),
            requires: Some("coins".into()),
            ..Pickup::default()
        }),
    );
    find(&mut scene, "Help").components.insert(
        "rusting.hud".into(),
        component(&HudElement {
            text: "A/D or arrows run, Space jumps. Grab every coin, then the flag."
                .into(),
            anchor: HudAnchor::BottomLeft,
            // Dark: white text is unreadable on the pale sky.
            color: [0.1, 0.12, 0.18, 1.0],
            ..HudElement::default()
        }),
    );

    let mut extra = Vec::new();
    let coin_burst = component(&BurstEmitter {
        count: 8,
        speed: 2.5,
        lifetime: 0.4,
        particle_scale: 0.5,
        on_collision: false,
        ..BurstEmitter::default()
    });
    let coin = component(&Pickup {
        counter: "coins".into(),
        ..Pickup::default()
    });
    let mut coin_sprite =
        serde_json::to_value(&find(&mut scene, "Flag").mesh_renderer)
            .expect("mesh renderers serialize");
    coin_sprite["material"]["Inline"]["base_color"] =
        json!([1.0, 0.85, 0.2, 1.0]);
    // Ground, over the first block, over the pit, over the second block,
    // ground.
    for (index, [x, y]) in [
        [-8.0, -0.4],
        [-5.5, 1.2],
        [-0.5, 0.6],
        [2.5, 1.2],
        [5.0, -0.4],
    ]
    .into_iter()
    .enumerate()
    {
        extra.push(json!({
            "id": Uuid::new_v4(), "parent": null,
            "name": format!("Coin {}", index + 1), "classes": ["coin"],
            "transform": {"position": [x, y, 0.05], "rotation": [0.0, 0.0, 0.0], "scale": [0.4, 0.4, 1.0]},
            "mesh_renderer": coin_sprite,
            "visible": true,
            "physics_body": {"simulation": "Cpu", "solver": "Simplified", "custom_shader": null},
            "rigid_body": {"kind": "Fixed", "mass": 1.0, "linear_velocity": [0.0, 0.0, 0.0], "angular_velocity": [0.0, 0.0, 0.0], "gravity_scale": 1.0},
            "collider": {"shape": {"Box": {"half_extents": [0.5, 0.5, 0.5]}}, "friction": 0.5, "restitution": 0.0, "sensor": true},
            "components": {"rusting.pickup": coin, "rusting.burst_emitter": coin_burst}
        }));
    }
    let counter = |name: &str, target| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target: Some(target),
        })
    };
    let hud = |text: &str, anchor, font_size, color, requires: Option<&str>| {
        component(&HudElement {
            text: text.into(),
            anchor,
            font_size,
            color,
            requires: requires.map(String::from),
            ..HudElement::default()
        })
    };
    extra.push(json!({
        "id": Uuid::new_v4(), "parent": null, "name": "Score",
        "components": {
            "rusting.counter": counter("coins", STARTER_COINS),
            "rusting.hud": hud(&format!("Coins {{coins}}/{STARTER_COINS}"), HudAnchor::TopLeft, 28.0, [0.45, 0.3, 0.0, 1.0], None),
        }
    }));
    extra.push(json!({
        "id": Uuid::new_v4(), "parent": null, "name": "Win",
        "components": {
            "rusting.counter": counter("won", 1),
            "rusting.hud": hud("You win!", HudAnchor::Center, 56.0, [1.0, 1.0, 1.0, 1.0], Some("won")),
            "rusting.background": component(&SceneBackground {
                color: [0.35, 0.55, 0.8, 1.0],
            }),
        }
    }));
    scene.entities.extend(
        extra
            .into_iter()
            .map(|entity: Value| serde_json::from_value(entity))
            .collect::<Result<Vec<_>, _>>()
            .expect("the starter template is a valid scene"),
    );
    scene
}

/// Box Push: walls, two boxes and two goals laid out from a map on whole
/// X and Z cells, a ball player, a camera looking down at the grid, and the
/// `boxes` counter. The game code is `src/templates/puzzle.rs`.
fn puzzle_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    const MAP: [&str; 6] = [
        "########", "#......#", "#.@.B..#", "#......#", "#.B.GG.#", "########",
    ];
    let object = mesh_object;
    let mut entities = vec![
        // Top face at y = 0, under every cell.
        object(
            "Floor",
            None,
            "Cube",
            [0.3, 0.32, 0.36],
            [-0.5, -0.1, -0.5],
            [8.0, 0.2, 6.0],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [-0.5, 8.0, 5.0], "rotation": [-1.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ];
    let mut count = std::collections::HashMap::new();
    let mut numbered = |kind: &str| {
        let n = count.entry(kind.to_owned()).or_insert(0);
        *n += 1;
        format!("{kind} {n}")
    };
    // Map column x and row z become cell (x - 4, z - 3).
    for (z, row) in MAP.iter().enumerate() {
        for (x, tile) in row.chars().enumerate() {
            let (x, z) = (x as f32 - 4.0, z as f32 - 3.0);
            entities.push(match tile {
                '#' => object(
                    &numbered("Wall"),
                    Some("wall"),
                    "Cube",
                    [0.45, 0.47, 0.52],
                    [x, 0.5, z],
                    [1.0; 3],
                ),
                'B' => object(
                    &numbered("Box"),
                    Some("box"),
                    "Cube",
                    [0.8, 0.55, 0.25],
                    [x, 0.4, z],
                    [0.8; 3],
                ),
                'G' => object(
                    &numbered("Goal"),
                    Some("goal"),
                    "Cube",
                    [0.25, 0.8, 0.35],
                    [x, 0.02, z],
                    [0.9, 0.04, 0.9],
                ),
                '@' => object(
                    "Player",
                    None,
                    "Sphere",
                    [0.25, 0.55, 0.95],
                    [x, 0.4, z],
                    [0.8; 3],
                ),
                _ => continue,
            });
        }
    }
    let hud = hud_component;
    let goals = MAP.concat().matches('G').count() as i32;
    entities.push(json!({
        "id": Uuid::new_v4(), "parent": null, "name": "Score",
        "components": {
            "rusting.counter": component(&Counter { name: "boxes".into(), value: 0, target: Some(goals) }),
            "rusting.hud": hud(&format!("Boxes {{boxes}}/{goals}"), HudAnchor::TopLeft, 28.0, None),
        }
    }));
    entities.push(json!({
        "id": Uuid::new_v4(), "parent": null, "name": "Solved",
        "components": {"rusting.hud": hud("Solved!", HudAnchor::Center, 56.0, Some("boxes"))}
    }));
    entities.push(json!({
        "id": Uuid::new_v4(), "parent": null, "name": "Help",
        "components": {"rusting.hud": hud("Arrows or WASD move. Push every box onto a green goal.", HudAnchor::BottomLeft, 20.0, None)}
    }));
    entities.extend(move_actions());
    json_scene(name, entities)
}

/// Serializes a runtime component for a scene's `components` map.
fn component(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("components serialize")
}

/// A named object with a builtin mesh in a flat PBR color.
fn mesh_object(
    name: &str,
    class: Option<&str>,
    shape: &str,
    color: [f32; 3],
    position: [f32; 3],
    scale: [f32; 3],
) -> serde_json::Value {
    serde_json::json!({
        "id": Uuid::new_v4(), "parent": null, "name": name,
        "classes": class.into_iter().collect::<Vec<_>>(),
        "transform": {"position": position, "rotation": [0.0, 0.0, 0.0], "scale": scale},
        "mesh_renderer": {
            "mesh": {"BuiltinPrimitive": shape},
            "material": {"Inline": {
                "model": "Pbr", "alpha_mode": "Opaque",
                "base_color": [color[0], color[1], color[2], 1.0],
                "emissive": [0.0, 0.0, 0.0], "metallic": 0.0, "roughness": 0.7,
                "base_color_texture": null, "normal_texture": null,
                "metallic_roughness_texture": null,
                "occlusion_texture": null, "emissive_texture": null
            }},
            "cast_shadows": true, "receive_shadows": true
        },
        "visible": true
    })
}

/// A `rusting.hud` component, shown only once `requires` completes.
fn hud_component(
    text: &str,
    anchor: crate::runtime::HudAnchor,
    font_size: f32,
    requires: Option<&str>,
) -> String {
    component(&crate::runtime::HudElement {
        text: text.into(),
        anchor,
        font_size,
        requires: requires.map(String::from),
        ..Default::default()
    })
}

/// The `up`, `down`, `left` and `right` actions on WASD, the arrows and
/// the d-pad.
fn move_actions() -> Vec<serde_json::Value> {
    [
        ("up", ["KeyW", "ArrowUp", "PadDpadUp"]),
        ("down", ["KeyS", "ArrowDown", "PadDpadDown"]),
        ("left", ["KeyA", "ArrowLeft", "PadDpadLeft"]),
        ("right", ["KeyD", "ArrowRight", "PadDpadRight"]),
    ]
    .into_iter()
    .map(|(action, inputs)| {
        serde_json::json!({
            "id": Uuid::new_v4(), "parent": null, "name": format!("Move {action}"),
            "components": {"rusting.input_action": component(&serde_json::json!({"action": action, "inputs": inputs}))}
        })
    })
    .collect()
}

fn json_scene(name: &str, entities: Vec<serde_json::Value>) -> SceneDocument {
    SceneDocument {
        format_version: SCENE_FORMAT_VERSION,
        name: format!("{name} Main Scene"),
        entities: entities
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<_>, _>>()
            .expect("templates are valid scenes"),
        render: Default::default(),
        simulation: Default::default(),
    }
}

/// The arena's floor, low walls, ball player, sun and a camera looking
/// down, shared by the top-down and twin-stick templates.
fn arena_room() -> Vec<serde_json::Value> {
    use serde_json::json;

    let wall = [0.45, 0.47, 0.52];
    vec![
        mesh_object(
            "Floor",
            None,
            "Cube",
            [0.3, 0.32, 0.36],
            [0.0, -0.1, 0.0],
            [16.0, 0.2, 16.0],
        ),
        mesh_object(
            "Wall North",
            None,
            "Cube",
            wall,
            [0.0, 0.3, -8.0],
            [16.0, 0.6, 0.4],
        ),
        mesh_object(
            "Wall South",
            None,
            "Cube",
            wall,
            [0.0, 0.3, 8.0],
            [16.0, 0.6, 0.4],
        ),
        mesh_object(
            "Wall West",
            None,
            "Cube",
            wall,
            [-8.0, 0.3, 0.0],
            [0.4, 0.6, 16.0],
        ),
        mesh_object(
            "Wall East",
            None,
            "Cube",
            wall,
            [8.0, 0.3, 0.0],
            [0.4, 0.6, 16.0],
        ),
        mesh_object(
            "Player",
            None,
            "Sphere",
            [0.25, 0.55, 0.95],
            [0.0, 0.4, 0.0],
            [0.8; 3],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [0.0, 15.0, 9.0], "rotation": [-1.03, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ]
}

/// Arena: a floor with low walls, a ball player, three enemies, a camera
/// looking down, and the `kills` and `hits` counters. The game code is
/// `src/templates/arena.rs`.
fn arena_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    let mut entities = arena_room();
    let enemies = [[3.0, 0.4, 0.0], [-5.0, 0.4, 0.0], [0.0, 0.4, -7.0]];
    for (n, position) in enemies.iter().enumerate() {
        entities.push(mesh_object(
            &format!("Enemy {}", n + 1),
            Some("enemy"),
            "Cube",
            [0.9, 0.3, 0.25],
            *position,
            [0.7; 3],
        ));
    }
    let counter = |name: &str, target: usize| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target: Some(target as i32),
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Kills",
            "components": {
                "rusting.counter": counter("kills", enemies.len()),
                "rusting.hud": hud_component(&format!("Defeated {{kills}}/{}", enemies.len()), HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Hits",
            "components": {
                "rusting.counter": counter("hits", 3),
                "rusting.hud": hud_component("Hits {hits}/3", HudAnchor::TopRight, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Won",
            "components": {"rusting.hud": hud_component("You win! Attack to play again.", HudAnchor::Center, 48.0, Some("kills"))}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Lost",
            "components": {"rusting.hud": hud_component("Game over. Attack to try again.", HudAnchor::Center, 48.0, Some("hits"))}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Help",
            "components": {"rusting.hud": hud_component("Arrows or WASD move. Space attacks enemies in reach.", HudAnchor::BottomLeft, 20.0, None)}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Attack",
            "components": {"rusting.input_action": component(&json!({"action": "attack", "inputs": ["Space", "KeyJ", "PadSouth"]}))}
        }),
    ]);
    entities.extend(move_actions());
    json_scene(name, entities)
}

/// Circuit: grass, four road pieces (class `road`) around a rectangle,
/// four checkpoints with the finish line last, a car, a camera above the
/// track, and the `laps`, `checkpoint` and `speed` counters. The game
/// Swarm: the arena room, hidden enemy and shot templates under the
/// floor, and the `kills` and `hits` counters. The game code is
/// `src/templates/twin_stick.rs`.
fn twin_stick_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    let mut entities = arena_room();
    for (template, color, scale) in [
        ("Enemy Template", [0.9, 0.3, 0.25], [0.7; 3]),
        ("Bullet Template", [1.0, 0.85, 0.3], [0.3; 3]),
    ] {
        let mut object = mesh_object(
            template,
            None,
            "Sphere",
            color,
            [0.0, -20.0, 0.0],
            scale,
        );
        object["visible"] = json!(false);
        entities.push(object);
    }
    let counter = |name: &str, target: i32| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target: Some(target),
        })
    };
    let hud = |name: &str, text: &str, anchor, size, requires: Option<&str>| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": name,
            "components": {"rusting.hud": hud_component(text, anchor, size, requires)}
        })
    };
    let action = |action: &str, inputs: &[&str]| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": format!("Action {action}"),
            "components": {"rusting.input_action": component(&json!({"action": action, "inputs": inputs}))}
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Kills",
            "components": {
                "rusting.counter": counter("kills", 12),
                "rusting.hud": hud_component("Defeated {kills}/12", HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Hits",
            "components": {
                "rusting.counter": counter("hits", 3),
                "rusting.hud": hud_component("Hits {hits}/3", HudAnchor::TopRight, 28.0, None),
            }
        }),
        hud("Won", "You win! R plays again.", HudAnchor::Center, 48.0, Some("kills")),
        hud("Lost", "Game over. R tries again.", HudAnchor::Center, 48.0, Some("hits")),
        hud("Help", "WASD or left stick move. Arrows or right stick aim and fire.", HudAnchor::BottomLeft, 20.0, None),
        action("up", &["KeyW", "PadLeftStickUp"]),
        action("down", &["KeyS", "PadLeftStickDown"]),
        action("left", &["KeyA", "PadLeftStickLeft"]),
        action("right", &["KeyD", "PadLeftStickRight"]),
        action("aim_up", &["ArrowUp", "PadRightStickUp"]),
        action("aim_down", &["ArrowDown", "PadRightStickDown"]),
        action("aim_left", &["ArrowLeft", "PadRightStickLeft"]),
        action("aim_right", &["ArrowRight", "PadRightStickRight"]),
        action("restart", &["KeyR", "PadStart"]),
    ]);
    json_scene(name, entities)
}

/// Beat: three lanes, a hit line with a pad per lane, a hidden note
/// template, and the `score`, `misses` and `played` counters. The game
/// code, `src/templates/rhythm.rs`, holds the chart.
fn rhythm_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    let mut entities = vec![
        mesh_object(
            "Ground",
            None,
            "Cube",
            [0.1, 0.1, 0.14],
            [0.0, -0.2, -8.0],
            [12.0, 0.2, 28.0],
        ),
        mesh_object(
            "Hit Line",
            None,
            "Cube",
            [0.9, 0.9, 0.95],
            [0.0, 0.02, 2.0],
            [5.0, 0.04, 0.15],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.3, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [0.0, 5.0, 7.0], "rotation": [-0.45, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ];
    let colors = [[0.95, 0.35, 0.4], [0.4, 0.85, 0.45], [0.35, 0.55, 0.95]];
    for (lane, color) in colors.into_iter().enumerate() {
        let x = (lane as f32 - 1.0) * 1.5;
        let n = lane + 1;
        entities.push(mesh_object(
            &format!("Lane {n}"),
            None,
            "Cube",
            [0.2, 0.2, 0.26],
            [x, -0.05, -9.0],
            [1.2, 0.1, 24.0],
        ));
        entities.push(mesh_object(
            &format!("Pad {n}"),
            None,
            "Cube",
            color,
            [x, 0.0, 2.0],
            [1.2, 0.08, 0.6],
        ));
    }
    let mut note = mesh_object(
        "Note Template",
        None,
        "Cube",
        [1.0, 0.95, 0.8],
        [0.0, -20.0, 0.0],
        [1.0, 0.3, 0.4],
    );
    note["visible"] = json!(false);
    entities.push(note);
    let counter = |name: &str, target| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target,
        })
    };
    let hud = |name: &str, text: &str, anchor, size, requires: Option<&str>| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": name,
            "components": {"rusting.hud": hud_component(text, anchor, size, requires)}
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Score",
            "components": {
                "rusting.counter": counter("score", None),
                "rusting.hud": hud_component("Score {score}", HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Misses",
            "components": {
                "rusting.counter": counter("misses", Some(8)),
                "rusting.hud": hud_component("Misses {misses}/8", HudAnchor::TopRight, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Played",
            "components": {"rusting.counter": counter("played", Some(16))}
        }),
        hud("Judgement", "", HudAnchor::Top, 40.0, None),
        hud("Won", "Song clear! R plays again.", HudAnchor::Center, 48.0, Some("played")),
        hud("Lost", "Game over. R tries again.", HudAnchor::Center, 48.0, Some("misses")),
        hud("Help", "D, F and J (or the arrows) hit a lane as its note crosses the line.", HudAnchor::BottomLeft, 20.0, None),
    ]);
    for (action, inputs) in [
        ("lane_1", &["KeyD", "ArrowLeft", "PadWest"][..]),
        ("lane_2", &["KeyF", "ArrowDown", "PadSouth"]),
        ("lane_3", &["KeyJ", "ArrowRight", "PadEast"]),
        ("restart", &["KeyR", "PadStart"]),
    ] {
        entities.push(json!({
            "id": Uuid::new_v4(), "parent": null, "name": format!("Action {action}"),
            "components": {"rusting.input_action": component(&json!({"action": action, "inputs": inputs}))}
        }));
    }
    json_scene(name, entities)
}

/// Duel: a table, the foe, three card slots with a HUD button each, and
/// the `foe_damage`, `damage` and `intent` counters. The game code,
/// `src/templates/card_game.rs`, deals the cards.
fn card_game_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor, HudElement};

    let mut entities = vec![
        mesh_object(
            "Table",
            None,
            "Cube",
            [0.35, 0.22, 0.15],
            [0.0, -0.1, 0.0],
            [10.0, 0.2, 7.0],
        ),
        mesh_object(
            "Foe",
            None,
            "Sphere",
            [0.6, 0.25, 0.7],
            [0.0, 1.0, -2.0],
            [1.6; 3],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [0.0, 6.0, 6.0], "rotation": [-0.8, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ];
    for slot in 1..=3 {
        let x = (slot as f32 - 2.0) * 1.8;
        entities.push(mesh_object(
            &format!("Slot {slot}"),
            None,
            "Cube",
            [0.9, 0.9, 0.85],
            [x, 0.05, 1.8],
            [1.4, 0.05, 2.0],
        ));
        entities.push(json!({
            "id": Uuid::new_v4(), "parent": null, "name": format!("Card {slot}"),
            "components": {"rusting.hud": component(&HudElement {
                text: slot.to_string(),
                anchor: HudAnchor::Bottom,
                offset: [(slot as f32 - 2.0) * 220.0, 60.0],
                font_size: 26.0,
                button: true,
                ..Default::default()
            })}
        }));
    }
    let counter = |name: &str, target| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target,
        })
    };
    let hud = |name: &str, text: &str, anchor, size, requires: Option<&str>| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": name,
            "components": {"rusting.hud": hud_component(text, anchor, size, requires)}
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Foe Damage",
            "components": {
                "rusting.counter": counter("foe_damage", Some(30)),
                "rusting.hud": hud_component("Foe {foe_damage}/30", HudAnchor::TopRight, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Damage",
            "components": {
                "rusting.counter": counter("damage", Some(20)),
                "rusting.hud": hud_component("You {damage}/20", HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Intent",
            "components": {
                "rusting.counter": counter("intent", None),
                "rusting.hud": hud_component("Foe attacks for {intent}", HudAnchor::Top, 28.0, None),
            }
        }),
        hud("Won", "You win! R plays again.", HudAnchor::Center, 48.0, Some("foe_damage")),
        hud("Lost", "Game over. R tries again.", HudAnchor::Center, 48.0, Some("damage")),
        hud("Help", "Keys 1 to 3 play a card. Guard blocks the foe's next attack.", HudAnchor::BottomLeft, 20.0, None),
    ]);
    for (action, inputs) in [
        ("play_1", ["Digit1", "PadWest"]),
        ("play_2", ["Digit2", "PadSouth"]),
        ("play_3", ["Digit3", "PadEast"]),
        ("restart", ["KeyR", "PadStart"]),
    ] {
        entities.push(json!({
            "id": Uuid::new_v4(), "parent": null, "name": format!("Action {action}"),
            "components": {"rusting.input_action": component(&json!({"action": action, "inputs": inputs}))}
        }));
    }
    json_scene(name, entities)
}

/// Crypt: a floor inside four walls, the player, the stairs, hidden
/// rubble and enemy templates, and the `depth` and `wounds` counters. The
/// game code, `src/templates/roguelike.rs`, lays out each floor.
fn roguelike_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    // Cells run from 1 to 9 on X and 1 to 7 on Z.
    let stone = [0.45, 0.47, 0.52];
    let mut entities = vec![
        mesh_object(
            "Floor",
            None,
            "Cube",
            [0.3, 0.32, 0.36],
            [5.0, -0.1, 4.0],
            [11.0, 0.2, 9.0],
        ),
        mesh_object(
            "Wall North",
            None,
            "Cube",
            stone,
            [5.0, 0.5, 0.0],
            [11.0, 1.0, 1.0],
        ),
        mesh_object(
            "Wall South",
            None,
            "Cube",
            stone,
            [5.0, 0.5, 8.0],
            [11.0, 1.0, 1.0],
        ),
        mesh_object(
            "Wall West",
            None,
            "Cube",
            stone,
            [0.0, 0.5, 4.0],
            [1.0, 1.0, 7.0],
        ),
        mesh_object(
            "Wall East",
            None,
            "Cube",
            stone,
            [10.0, 0.5, 4.0],
            [1.0, 1.0, 7.0],
        ),
        mesh_object(
            "Player",
            None,
            "Sphere",
            [0.25, 0.55, 0.95],
            [1.0, 0.4, 1.0],
            [0.8; 3],
        ),
        mesh_object(
            "Stairs",
            None,
            "Cube",
            [0.95, 0.8, 0.3],
            [9.0, 0.02, 7.0],
            [0.9, 0.04, 0.9],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [5.0, 11.0, 10.5], "rotation": [-1.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ];
    for (template, shape, color, scale) in [
        ("Rubble Template", "Cube", stone, [1.0; 3]),
        ("Enemy Template", "Sphere", [0.9, 0.3, 0.25], [0.8; 3]),
    ] {
        let mut object =
            mesh_object(template, None, shape, color, [0.0, -20.0, 0.0], scale);
        object["visible"] = json!(false);
        entities.push(object);
    }
    let counter = |name: &str, target| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target: Some(target),
        })
    };
    let hud = |name: &str, text: &str, anchor, size, requires: Option<&str>| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": name,
            "components": {"rusting.hud": hud_component(text, anchor, size, requires)}
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Depth",
            "components": {
                "rusting.counter": counter("depth", 3),
                "rusting.hud": hud_component("Depth {depth}/3", HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Wounds",
            "components": {
                "rusting.counter": counter("wounds", 5),
                "rusting.hud": hud_component("Wounds {wounds}/5", HudAnchor::TopRight, 28.0, None),
            }
        }),
        hud("Won", "You escaped! R plays again.", HudAnchor::Center, 48.0, Some("depth")),
        hud("Lost", "Game over. R tries again.", HudAnchor::Center, 48.0, Some("wounds")),
        hud("Help", "Arrows or WASD move. Walk into an enemy to defeat it. Reach the gold stairs.", HudAnchor::BottomLeft, 20.0, None),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Action restart",
            "components": {"rusting.input_action": component(&json!({"action": "restart", "inputs": ["KeyR", "PadStart"]}))}
        }),
    ]);
    entities.extend(move_actions());
    json_scene(name, entities)
}

/// Outpost: grass, a path of road pieces through six waypoints to a base,
/// four build pads, hidden enemy and tower templates, and the `gold`,
/// `leaks` and `cleared` counters. The game code is
/// `src/templates/tower_defense.rs`.
fn tower_defense_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    let path = [
        [-10.0, -4.0],
        [-2.0, -4.0],
        [-2.0, 4.0],
        [6.0, 4.0],
        [6.0, -1.0],
        [10.0, -1.0],
    ];
    let pads = [[-4.5, -1.5], [0.5, 1.5], [3.5, 1.5], [8.5, 1.5]];
    let mut entities = vec![
        mesh_object(
            "Grass",
            None,
            "Cube",
            [0.25, 0.5, 0.25],
            [0.0, -0.2, 0.0],
            [24.0, 0.2, 14.0],
        ),
        mesh_object(
            "Base",
            None,
            "Cube",
            [0.3, 0.5, 0.9],
            [path[5][0], 0.5, path[5][1]],
            [1.6, 1.0, 1.6],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [0.0, 20.0, 9.0], "rotation": [-1.15, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ];
    for (n, [x, z]) in path.iter().enumerate() {
        let mut waypoint = mesh_object(
            &format!("Waypoint {}", n + 1),
            None,
            "Cube",
            [0.0; 3],
            [*x, 0.0, *z],
            [0.2; 3],
        );
        waypoint["visible"] = json!(false);
        entities.push(waypoint);
    }
    for (n, leg) in path.windows(2).enumerate() {
        let ([x0, z0], [x1, z1]) = (leg[0], leg[1]);
        entities.push(mesh_object(
            &format!("Road {}", n + 1),
            None,
            "Cube",
            [0.55, 0.45, 0.3],
            [(x0 + x1) / 2.0, -0.05, (z0 + z1) / 2.0],
            [(x1 - x0).abs() + 1.2, 0.1, (z1 - z0).abs() + 1.2],
        ));
    }
    for (n, [x, z]) in pads.iter().enumerate() {
        entities.push(mesh_object(
            &format!("Pad {}", n + 1),
            None,
            "Cube",
            [0.6, 0.6, 0.65],
            [*x, 0.05, *z],
            [1.4, 0.1, 1.4],
        ));
    }
    for (template, shape, color, scale) in [
        ("Enemy Template", "Sphere", [0.9, 0.3, 0.25], [0.6; 3]),
        ("Tower Template", "Cube", [0.85, 0.85, 0.9], [0.8, 1.2, 0.8]),
    ] {
        let mut object =
            mesh_object(template, None, shape, color, [0.0, -20.0, 0.0], scale);
        object["visible"] = json!(false);
        entities.push(object);
    }
    let counter = |name: &str, value: i32, target: Option<i32>| {
        component(&Counter {
            name: name.into(),
            value,
            target,
        })
    };
    let hud = |name: &str, text: &str, anchor, size, requires: Option<&str>| {
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": name,
            "components": {"rusting.hud": hud_component(text, anchor, size, requires)}
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Gold",
            "components": {
                "rusting.counter": counter("gold", 100, None),
                "rusting.hud": hud_component("Gold {gold}", HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Leaks",
            "components": {
                "rusting.counter": counter("leaks", 0, Some(5)),
                "rusting.hud": hud_component("Leaks {leaks}/5", HudAnchor::TopRight, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Cleared",
            "components": {"rusting.counter": counter("cleared", 0, Some(10))}
        }),
        hud("Won", "You win! R plays again.", HudAnchor::Center, 48.0, Some("cleared")),
        hud("Lost", "Game over. R tries again.", HudAnchor::Center, 48.0, Some("leaks")),
        hud("Help", "Keys 1 to 4 build a tower on a pad for 50 gold.", HudAnchor::BottomLeft, 20.0, None),
    ]);
    for (action, inputs) in [
        ("build_1", ["Digit1", "PadSouth"]),
        ("build_2", ["Digit2", "PadEast"]),
        ("build_3", ["Digit3", "PadWest"]),
        ("build_4", ["Digit4", "PadNorth"]),
        ("restart", ["KeyR", "PadStart"]),
    ] {
        entities.push(json!({
            "id": Uuid::new_v4(), "parent": null, "name": format!("Action {action}"),
            "components": {"rusting.input_action": component(&json!({"action": action, "inputs": inputs}))}
        }));
    }
    json_scene(name, entities)
}

/// code is `src/templates/racing.rs`.
fn racing_scene(name: &str) -> SceneDocument {
    use serde_json::json;

    use crate::runtime::{Counter, HudAnchor};

    // The road's center line is a rectangle 32 by 20 m; the road is 8 m wide.
    let (half_x, half_z, road) = (16.0, 10.0, 8.0);
    let asphalt = [0.22, 0.22, 0.25];
    let mut entities = vec![
        mesh_object(
            "Grass",
            None,
            "Cube",
            [0.25, 0.5, 0.25],
            [0.0, -0.2, 0.0],
            [60.0, 0.2, 44.0],
        ),
        mesh_object(
            "Road South",
            Some("road"),
            "Cube",
            asphalt,
            [0.0, -0.05, half_z],
            [2.0 * half_x + road, 0.1, road],
        ),
        mesh_object(
            "Road North",
            Some("road"),
            "Cube",
            asphalt,
            [0.0, -0.05, -half_z],
            [2.0 * half_x + road, 0.1, road],
        ),
        mesh_object(
            "Road East",
            Some("road"),
            "Cube",
            asphalt,
            [half_x, -0.05, 0.0],
            [road, 0.1, 2.0 * half_z + road],
        ),
        mesh_object(
            "Road West",
            Some("road"),
            "Cube",
            asphalt,
            [-half_x, -0.05, 0.0],
            [road, 0.1, 2.0 * half_z + road],
        ),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Car",
            "transform": {"position": [0.0, 0.3, half_z], "rotation": [0.0, -std::f32::consts::FRAC_PI_2, 0.0], "scale": [0.9, 0.5, 1.6]},
            "mesh_renderer": mesh_object("", None, "Cube", [0.9, 0.2, 0.15], [0.0; 3], [1.0; 3])["mesh_renderer"],
            "visible": true
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Sun",
            "transform": {"position": [0.0, 10.0, 0.0], "rotation": [-0.9, 0.5, 0.0], "scale": [1.0, 1.0, 1.0]},
            "directional_light": {"color": [1.0, 0.96, 0.9], "illuminance": 100_000.0, "shadows": true}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Game Camera",
            "transform": {"position": [0.0, 34.0, 14.0], "rotation": [-1.18, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
            "camera": {
                "projection": {"Perspective": {"vertical_fov_radians": std::f32::consts::FRAC_PI_3, "near": 0.1, "far": 1000.0}},
                "active": true, "priority": 10
            }
        }),
    ];
    // Checkpoints mid-way along each side, counterclockwise from the
    // start; the last is the finish line under the car.
    let checkpoints =
        [[half_x, 0.0], [0.0, -half_z], [-half_x, 0.0], [0.0, half_z]];
    for (n, [x, z]) in checkpoints.iter().enumerate() {
        let across_x = n % 2 == 1;
        let size = if across_x {
            [0.6, 0.02, road]
        } else {
            [road, 0.02, 0.6]
        };
        entities.push(mesh_object(
            &format!("Checkpoint {}", n + 1),
            Some("checkpoint"),
            "Cube",
            if n + 1 == checkpoints.len() {
                [0.95, 0.95, 0.95]
            } else {
                [0.95, 0.8, 0.2]
            },
            [*x, 0.01, *z],
            size,
        ));
    }
    let counter = |name: &str, target: Option<i32>| {
        component(&Counter {
            name: name.into(),
            value: 0,
            target,
        })
    };
    entities.extend([
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Laps",
            "components": {
                "rusting.counter": counter("laps", Some(3)),
                "rusting.hud": hud_component("Lap {laps}/3", HudAnchor::TopLeft, 28.0, None),
            }
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Next Checkpoint",
            "components": {"rusting.counter": counter("checkpoint", None)}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Speed",
            "components": {"rusting.counter": counter("speed", None)}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Finished",
            "components": {"rusting.hud": hud_component("Finished! R restarts.", HudAnchor::Center, 48.0, Some("laps"))}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Help",
            // Dark: it sits on the light grass.
            "components": {"rusting.hud": component(&crate::runtime::HudElement {
                text: "Up accelerates, down brakes, left and right steer. Pass the yellow checkpoints in order.".into(),
                anchor: HudAnchor::BottomLeft,
                font_size: 20.0,
                color: [0.1, 0.12, 0.18, 1.0],
                ..Default::default()
            })}
        }),
        json!({
            "id": Uuid::new_v4(), "parent": null, "name": "Restart",
            "components": {"rusting.input_action": component(&json!({"action": "restart", "inputs": ["KeyR", "PadStart"]}))}
        }),
    ]);
    entities.extend(move_actions());
    json_scene(name, entities)
}

/// Uses the process folder only for editor settings, never project creation.
fn default_project_parent() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Per-user editor configuration folder.
pub(super) fn user_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        });
    base.unwrap_or_else(default_project_parent)
        .join("rusting_engine")
}

fn recent_projects_path() -> PathBuf {
    user_config_dir().join("recent_projects.json")
}

/// Editor settings that belong to the user, not to a project.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EditorPreferences {
    /// Whole-interface zoom (egui zoom factor); 1.0 is 100%.
    pub ui_scale: f32,
    /// Multiplier on egui's default text sizes; 1.0 is 100%.
    pub font_scale: f32,
}

impl Default for EditorPreferences {
    fn default() -> Self {
        Self {
            ui_scale: 1.0,
            font_scale: 1.0,
        }
    }
}

impl EditorPreferences {
    /// UI scales offered in the View menu.
    pub const UI_SCALES: [f32; 6] = [0.8, 0.9, 1.0, 1.1, 1.25, 1.5];
    /// Text sizes offered in the View menu.
    pub const FONT_SCALES: [f32; 4] = [0.9, 1.0, 1.15, 1.3];

    fn path() -> PathBuf {
        user_config_dir().join("editor_preferences.json")
    }

    /// Reads the user's preferences; a missing or broken file gives defaults.
    #[must_use]
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    fn load_from(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
            .map(|mut preferences| {
                preferences.ui_scale = preferences.ui_scale.clamp(0.5, 3.0);
                preferences.font_scale = preferences.font_scale.clamp(0.5, 3.0);
                preferences
            })
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), ProjectError> {
        self.save_to(&Self::path())
    }

    fn save_to(&self, path: &Path) -> Result<(), ProjectError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::runtime::write_atomic(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

fn load_recent_projects() -> Result<Vec<RecentProject>, ProjectError> {
    let path = recent_projects_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn save_recent_projects(recent: &[RecentProject]) -> Result<(), ProjectError> {
    let path = recent_projects_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::runtime::write_atomic(path, &serde_json::to_vec_pretty(recent)?)?;
    Ok(())
}

/// File name of the game executable built for `target`.
pub fn executable_name(binary_name: &str, target: Option<&str>) -> String {
    let windows = target.map_or(cfg!(target_os = "windows"), |target| {
        target.contains("windows")
    });
    if windows {
        format!("{binary_name}.exe")
    } else {
        binary_name.to_owned()
    }
}

/// Creates a portable folder after Cargo has produced the release binary.
pub fn export_built_game(
    project_root: &std::path::Path,
    manifest: &std::path::Path,
    parent: &std::path::Path,
    project_name: &str,
    binary_name: &str,
    cooked_scene: &std::path::Path,
    target: Option<&str>,
) -> Result<PathBuf, String> {
    if !parent.is_dir() {
        return Err(format!("{} is not a folder", parent.display()));
    }
    let executable =
        built_executable(project_root, manifest, binary_name, target, true)?;
    package_game_files(
        project_root,
        &executable,
        parent,
        project_name,
        binary_name,
        cooked_scene,
        target,
    )
}

/// Copies a verified executable and runtime data into one new export folder.
pub fn package_game_files(
    project_root: &std::path::Path,
    executable: &std::path::Path,
    parent: &std::path::Path,
    project_name: &str,
    binary_name: &str,
    cooked_scene: &std::path::Path,
    target: Option<&str>,
) -> Result<PathBuf, String> {
    let executable_name = executable_name(binary_name, target);
    let cooked_source = project_root.join(cooked_scene);
    if !cooked_source.is_file() {
        return Err(format!(
            "Cooked scene {} is missing",
            cooked_source.display()
        ));
    }

    // A unique final name avoids replacing an older playable export.
    // Cross exports name their system, e.g. `game_windows_export`.
    let safe_name = match target.and_then(|target| target.split('-').nth(2)) {
        Some(system) => format!("{}_{system}", binary_name.replace('-', "_")),
        None => binary_name.replace('-', "_"),
    };
    let mut destination = parent.join(format!("{safe_name}_export"));
    for number in 2.. {
        if !destination.exists() {
            break;
        }
        destination = parent.join(format!("{safe_name}_export_{number}"));
    }
    let temporary =
        parent.join(format!(".rusting-export-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<(), String> {
        std::fs::create_dir_all(&temporary)
            .map_err(|error| error.to_string())?;
        std::fs::copy(executable, temporary.join(&executable_name))
            .map_err(|error| error.to_string())?;
        let cooked_destination = temporary.join(cooked_scene);
        if let Some(folder) = cooked_destination.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|error| error.to_string())?;
        }
        std::fs::copy(&cooked_source, cooked_destination)
            .map_err(|error| error.to_string())?;
        // The game finds its asset root and window size through the
        // manifest; without it, `assets/` is looked up next to the scene.
        let manifest = project_root.join("project.json");
        if manifest.is_file() {
            std::fs::copy(manifest, temporary.join("project.json"))
                .map_err(|error| error.to_string())?;
        }
        let assets = project_root.join("assets");
        if assets.is_dir() {
            copy_directory(&assets, &temporary.join("assets"))?;
        }
        // Scenes a game loads later, next to the cooked main scene.
        let scenes = project_root.join("scenes");
        if scenes.is_dir() {
            copy_directory(&scenes, &temporary.join("scenes"))?;
        }
        let engine_license =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("LICENSE.md");
        if engine_license.is_file() {
            std::fs::copy(
                engine_license,
                temporary.join("RUSTING_ENGINE_LICENSE.md"),
            )
            .map_err(|error| error.to_string())?;
        }
        std::fs::write(
            temporary.join("README.txt"),
            format!(
                "{project_name}\n\nRun {executable_name} to start the game.\nThe system needs a Vulkan-capable graphics driver.\n\nSettings and saves live in the user data folder: ~/.local/share/{binary_name} on Linux,\n~/Library/Application Support/{binary_name} on macOS, %APPDATA%\\{binary_name} on Windows.\nCrash reports go to the crashes folder there.\nSet RUSTING_USER_DATA=<folder> to use another one.\n\nRUSTING_HEADLESS_TICKS=<n> runs n ticks with no window and exits, for smoke tests;\ncommand-line arguments reach the game code through std::env::args().\n"
            ),
        )
        .map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &destination)
            .map_err(|error| error.to_string())?;
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = std::fs::remove_dir_all(&temporary);
    }
    result.map(|()| destination)
}

/// Recursively copies normal files and folders while ignoring symbolic links.
pub fn copy_directory(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), String> {
    std::fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            continue;
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_directory(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), target)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Path of the executable Cargo built for the project, checked to exist.
pub fn built_executable(
    project_root: &std::path::Path,
    manifest: &std::path::Path,
    binary_name: &str,
    target: Option<&str>,
    release: bool,
) -> Result<PathBuf, String> {
    let metadata = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .args(crate::runtime::lease::offline().then_some("--offline"))
        .args(["--manifest-path"])
        .arg(manifest)
        .current_dir(project_root)
        .output()
        .map_err(|error| format!("Could not read Cargo metadata: {error}"))?;
    if !metadata.status.success() {
        return Err(String::from_utf8_lossy(&metadata.stderr).into_owned());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout)
        .map_err(|error| format!("Invalid Cargo metadata: {error}"))?;
    let target_directory = metadata
        .get("target_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Cargo metadata has no target directory".to_owned())?;
    let executable = PathBuf::from(target_directory)
        .join(target.unwrap_or_default())
        .join(if release { "release" } else { "debug" })
        .join(executable_name(binary_name, target));
    if !executable.is_file() {
        return Err(format!(
            "Executable {} was not produced",
            executable.display()
        ));
    }
    Ok(executable)
}

// `rusting_game!` adds a `main` the tests never call.
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/arena.rs"]
mod arena_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/card_game.rs"]
mod card_game_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/puzzle.rs"]
mod puzzle_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/racing.rs"]
mod racing_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/rhythm.rs"]
mod rhythm_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/roguelike.rs"]
mod roguelike_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/tower_defense.rs"]
mod tower_defense_template;
#[cfg(test)]
#[allow(dead_code)]
#[path = "templates/twin_stick.rs"]
mod twin_stick_template;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zipped_export_holds_each_file_with_its_crc_and_mode() {
        assert_eq!(!crc32_update(!0, b"123456789"), 0xCBF4_3926);
        let root = std::env::temp_dir()
            .join(format!("rusting-zip-{}", uuid::Uuid::new_v4()));
        let folder = root.join("My Game");
        std::fs::create_dir_all(folder.join("assets/art")).unwrap();
        std::fs::write(folder.join("game"), b"#!run").unwrap();
        std::fs::write(folder.join("assets/art/a.txt"), b"hello").unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(
            folder.join("game"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let out = root.join("My Game.zip");
        zip_folder(&folder, &out).unwrap();
        let zip = std::fs::read(&out).unwrap();
        let u16_at = |at: usize| u16::from_le_bytes([zip[at], zip[at + 1]]);
        let u32_at =
            |at: usize| u32::from_le_bytes(zip[at..at + 4].try_into().unwrap());
        let end = zip.len() - 22;
        assert_eq!(u32_at(end), 0x0605_4b50);
        assert_eq!(u16_at(end + 10), 2);
        let mut entry = u32_at(end + 16) as usize;
        let mut found = Vec::new();
        for _ in 0..2 {
            assert_eq!(u32_at(entry), 0x0201_4b50);
            let name_len = usize::from(u16_at(entry + 28));
            let name = std::str::from_utf8(&zip[entry + 46..][..name_len])
                .unwrap()
                .to_owned();
            let mode = u32_at(entry + 38) >> 16;
            let local = u32_at(entry + 42) as usize;
            let size = u32_at(local + 22) as usize;
            let data_at = local + 30 + usize::from(u16_at(local + 26));
            let data = &zip[data_at..][..size];
            assert_eq!(!crc32_update(!0, data), u32_at(local + 14));
            found.push((name, data.to_vec(), mode));
            entry += 46 + name_len;
        }
        assert_eq!(found[0].0, "My Game/assets/art/a.txt");
        assert_eq!(found[0].1, b"hello");
        assert_eq!(found[1].0, "My Game/game");
        assert_eq!(found[1].1, b"#!run");
        #[cfg(unix)]
        assert_eq!(found[1].2 & 0o777, 0o755);
        zip_folder(&folder, &root.join("again.zip")).unwrap();
        assert_eq!(std::fs::read(root.join("again.zip")).unwrap(), zip);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn export_excludes_match_paths_and_leave_the_rest() {
        assert!(path_matches("assets/source/**", "assets/source/a/b.psd"));
        assert!(path_matches("**/*.psd", "assets/art/hero.psd"));
        assert!(path_matches("**/*.psd", "hero.psd"));
        assert!(path_matches("assets/*.png", "assets/a.png"));
        assert!(!path_matches("assets/*.png", "assets/sub/a.png"));
        assert!(!path_matches("**/*.psd", "assets/hero.psd.png"));
        assert!(!path_matches("assets/source/**", "assets/sources/a"));

        let export = std::env::temp_dir()
            .join(format!("rusting-exclude-{}", uuid::Uuid::new_v4()));
        for file in [
            "game",
            "build/main.rscene.bin",
            "assets/hero.png",
            "assets/source/hero.psd",
            "scenes/debug.rscene",
            "scenes/level.rscene",
        ] {
            let path = export.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let removed = remove_excluded(
            &export,
            &["assets/source/**".into(), "scenes/debug*".into()],
        )
        .unwrap();
        assert_eq!(removed, ["assets/source/hero.psd", "scenes/debug.rscene"]);
        // `**` matches everything, but only under assets/ and scenes/.
        let removed = remove_excluded(&export, &["**".into()]).unwrap();
        assert_eq!(removed, ["assets/hero.png", "scenes/level.rscene"]);
        assert!(export.join("game").is_file());
        assert!(export.join("build/main.rscene.bin").is_file());

        std::fs::write(export.join("assets/hero.png"), "png").unwrap();
        std::fs::write(export.join("scenes/level.rscene"), "{}").unwrap();
        assert_eq!(
            pack_export(&export).unwrap(),
            ["assets.rpak", "scenes.rpak"]
        );
        assert!(!export.join("assets").exists());
        assert!(!export.join("scenes").exists());
        assert_eq!(
            crate::assets::read_file(&export.join("assets/hero.png")).unwrap(),
            b"png"
        );
        assert_eq!(
            crate::assets::read_file(&export.join("scenes/level.rscene"))
                .unwrap(),
            b"{}"
        );
        let _ = std::fs::remove_dir_all(export);

        let preset: ExportPreset = serde_json::from_str(
            r#"{"target": "x86_64-pc-windows-gnu", "features": ["steam"], "exclude": ["**/*.psd"], "pack": true}"#,
        )
        .unwrap();
        assert_eq!(preset.features, ["steam"]);
        assert!(preset.pack);
        assert!(
            serde_json::from_str::<ExportPreset>(r#"{"exclud": []}"#).is_err(),
            "a misspelt key is an error, not a silent no-op"
        );
    }

    #[test]
    fn a_bench_line_round_trips_its_frame_times() {
        let mut lengths: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        let work: Vec<_> = (0..100).map(|i| [3.0, f64::from(i % 7)]).collect();
        let line = bench_line(&mut lengths, &work);
        assert!(line.starts_with(BENCH_MARKER), "{line}");
        let summary = bench_result(&format!("noise\n{line}\nmore")).unwrap();
        assert_eq!(summary["frames"], 100);
        assert_eq!(summary["mean_ms"], 50.5);
        assert_eq!(summary["p50_ms"], 50.0);
        assert_eq!(summary["p95_ms"], 95.0);
        assert_eq!(summary["p99_ms"], 99.0);
        assert_eq!(summary["max_ms"], 100.0);
        // The GPU median (3) is not above the CPU median (3).
        assert_eq!(summary["gpu_p50_ms"], 3.0);
        assert_eq!(summary["bound"], "cpu");
        let gpu_heavy = [[2.0, 9.0], [2.5, 8.0], [3.0, 10.0]];
        let line = bench_line(&mut [10.0, 11.0, 12.0], &gpu_heavy);
        let summary = bench_result(&line).unwrap();
        assert_eq!(summary["bound"], "gpu");
        assert_eq!(summary["cpu_p50_ms"], 2.5);
        let untimed = bench_line(&mut [10.0], &[[4.0, 0.0]]);
        assert!(bench_result(&untimed).unwrap()["bound"].is_null());
        assert!(bench_result("no bench here").is_none());
    }

    #[test]
    fn reload_output_is_split_into_file_diagnostics_and_first_frame_time() {
        let output = "\
   Compiling game v0.1.0
src/main.rs:12:5: error[E0425]: cannot find value `x` in this scope
src/main.rs:3:1: warning: unused import
/engine/src/shaders/mod.rs:4:1: error: shader.comp:7: error: undeclared identifier
error: linking with `cc` failed
error: could not compile `game` (bin \"game\") due to 2 previous errors
hot reload failed: failed to load `assets/crate.rtexture`: bad header
[rusting] first playable frame after 412 ms
";
        let diagnostics = reload_diagnostics(output);
        let summary: Vec<_> = diagnostics
            .iter()
            .map(|d| (d.kind, d.file.clone(), d.line))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("rust", Some(PathBuf::from("src/main.rs")), Some(12)),
                (
                    "shader",
                    Some(PathBuf::from("/engine/src/shaders/mod.rs")),
                    Some(4)
                ),
                ("rust", None, None),
                ("asset", Some(PathBuf::from("assets/crate.rtexture")), None),
            ]
        );
        assert!(diagnostics[0].message.starts_with("error[E0425]"));
        assert_eq!(first_frame_ms(output), Some(412));
        assert_eq!(first_frame_ms("no marker"), None);
    }

    #[test]
    fn project_creation_writes_a_complete_openable_template() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project = create_project(&parent, "Example Game").unwrap();

        assert_eq!(project.manifest.name, "Example Game");
        assert!(project.root.join("Cargo.toml").is_file());
        assert!(project.root.join("src/main.rs").is_file());
        assert!(project.root.join("AGENTS.md").is_file());
        assert!(project.root.join("skills/rusting-game/SKILL.md").is_file());
        assert!(project.root.join(".gitignore").is_file());
        assert!(project.root.join("scenes/main.rscene").is_file());
        assert!(project.root.join("assets").is_dir());
        assert!(project.root.join("shaders").is_dir());
        assert!(project.root.join("build").is_dir());

        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn project_creation_never_overwrites_an_existing_folder() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        let existing = parent.join("Existing");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("keep.txt"), "keep").unwrap();

        assert!(matches!(
            create_project(&parent, "Existing"),
            Err(ProjectError::AlreadyExists(_))
        ));
        assert_eq!(
            std::fs::read_to_string(existing.join("keep.txt")).unwrap(),
            "keep"
        );

        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn the_2d_template_is_a_playable_side_view_level() {
        use std::time::Duration;

        use crate::assets::AssetPlugin;
        use crate::runtime::{
            load_scene, App, Collider, KeyCode, PlatformerController,
            RuntimeInput, SceneLoadMode, TileOf,
        };

        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project = create_project_from(
            &parent,
            "Jumper",
            ProjectTemplate::Platformer2d,
        )
        .unwrap();
        let cargo =
            std::fs::read_to_string(project.root.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("features = [\"ui\"]"), "{cargo}");

        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        load_scene(
            app.world_mut(),
            project.root.join("scenes/main.rscene"),
            SceneLoadMode::Replace,
        )
        .unwrap();
        let tick = |app: &mut App, ticks| {
            for _ in 0..ticks {
                app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
                app.world_mut()
                    .resource_mut::<RuntimeInput>()
                    .clear_frame_edges();
            }
        };
        let player = |app: &mut crate::runtime::App| {
            let world = app.world_mut();
            let mut query =
                world.query::<(&PlatformerController, &crate::Transform)>();
            let (controller, transform) = query.single(world).unwrap();
            (*controller, transform.position)
        };
        tick(&mut app, 60);
        let (controller, start) = player(&mut app);
        assert!(controller.grounded, "{start:?}");
        // The ground's top edge is y = -1 and the body is 1 m tall.
        assert!((start[1] + 0.5).abs() < 0.02, "{start:?}");
        let world = app.world_mut();
        let tiles = world.query::<&TileOf>().iter(world).count();
        let colliders =
            world.query::<(&TileOf, &Collider)>().iter(world).count();
        assert_eq!(colliders, 7, "one collider per solid run");
        assert!(tiles > 40, "{tiles}");

        // Run right and jump over the two-tile step ahead.
        let mut input = app.world_mut().resource_mut::<RuntimeInput>();
        input.record_key(KeyCode::KeyD, true);
        input.record_key(KeyCode::Space, true);
        tick(&mut app, 20);
        assert!(!player(&mut app).0.grounded, "the jump left the ground");
        let mut frames = 20;
        while !player(&mut app).0.grounded {
            assert!(frames < 120, "never landed: {:?}", player(&mut app).1);
            tick(&mut app, 1);
            frames += 1;
        }
        let (_, moved) = player(&mut app);
        assert!(moved[0] > start[0] + 3.0, "{moved:?} after {frames}");

        std::fs::remove_dir_all(parent).unwrap();
    }

    /// Creates `template` in a temporary folder and loads its main scene.
    /// Returns the app, a ticker at 60 Hz, and the folder to remove.
    fn load_template(
        template: ProjectTemplate,
    ) -> (
        crate::runtime::App,
        impl Fn(&mut crate::runtime::App, u32),
        PathBuf,
    ) {
        use std::time::Duration;

        use crate::assets::AssetPlugin;
        use crate::runtime::{load_scene, App, RuntimeInput, SceneLoadMode};

        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Template", template).unwrap();
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        load_scene(
            app.world_mut(),
            project.root.join("scenes/main.rscene"),
            SceneLoadMode::Replace,
        )
        .unwrap();
        let tick = |app: &mut App, ticks| {
            for _ in 0..ticks {
                app.update(Duration::from_secs_f64(1.0 / 60.0)).unwrap();
                app.world_mut()
                    .resource_mut::<RuntimeInput>()
                    .clear_frame_edges();
            }
        };
        (app, tick, parent)
    }

    #[test]
    fn the_empty_template_has_no_named_objects_to_clash_with() {
        let (mut app, _, parent) = load_template(ProjectTemplate::Empty);
        let world = app.world_mut();
        let names: Vec<String> = world
            .query::<&rusting_core::components::Name>()
            .iter(world)
            .map(|name| name.0.clone())
            .collect();
        assert!(!names.iter().any(|name| name == "Cube"), "{names:?}");
        let cameras = world
            .query_filtered::<(), bevy_ecs::query::With<crate::runtime::Camera>>()
            .iter(world)
            .count();
        assert_eq!(cameras, 1);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn template_names_round_trip() {
        for template in ProjectTemplate::ALL {
            assert_eq!(ProjectTemplate::parse(template.name()), Some(template));
        }
    }

    #[test]
    fn the_player_templates_stand_walk_and_frame_the_camera() {
        use crate::runtime::{Camera, KeyCode, PlayerController, RuntimeInput};

        for template in [
            ProjectTemplate::FirstPerson3d,
            ProjectTemplate::ThirdPerson3d,
        ] {
            let (mut app, tick, parent) = load_template(template);
            let player = |app: &mut crate::runtime::App| {
                let world = app.world_mut();
                let mut query =
                    world.query::<(&PlayerController, &crate::Transform)>();
                let (controller, transform) = query.single(world).unwrap();
                (*controller, transform.position)
            };
            tick(&mut app, 60);
            let (controller, start) = player(&mut app);
            assert!(controller.grounded, "{template:?} {start:?}");
            // The capsule is 1.8 m tall and the floor's top is y = 0.
            assert!((start[1] - 0.9).abs() < 0.05, "{template:?} {start:?}");

            app.world_mut()
                .resource_mut::<RuntimeInput>()
                .record_key(KeyCode::KeyW, true);
            tick(&mut app, 30);
            let (_, moved) = player(&mut app);
            assert!(moved[2] < start[2] - 1.0, "{template:?} {moved:?}");

            let world = app.world_mut();
            let camera = world
                .query_filtered::<&crate::Transform, bevy_ecs::query::With<Camera>>()
                .single(world)
                .unwrap()
                .position;
            if template == ProjectTemplate::ThirdPerson3d {
                assert!(camera[2] > 3.0 && camera[1] > 0.5, "{camera:?}");
            } else {
                assert_eq!(camera, [0.0, 0.7, 0.0]);
            }
            std::fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn the_sandbox_template_settles_its_pile_on_the_floor() {
        use crate::runtime::{Name, RigidBody, RigidBodyKind};

        let (mut app, tick, parent) =
            load_template(ProjectTemplate::PhysicsSandbox);
        let dynamic = |app: &mut crate::runtime::App| {
            let world = app.world_mut();
            let mut bodies: Vec<_> = world
                .query::<(&Name, &RigidBody, &crate::Transform)>()
                .iter(world)
                .filter(|(_, body, _)| body.kind == RigidBodyKind::Dynamic)
                .map(|(name, _, transform)| {
                    (name.0.clone(), transform.position)
                })
                .collect();
            bodies.sort_by(|a, b| a.0.cmp(&b.0));
            bodies
        };
        let start = dynamic(&mut app);
        assert_eq!(start.len(), 11);
        // Long enough for a rolling ball to reach a wall.
        tick(&mut app, 1200);
        let settled = dynamic(&mut app);
        tick(&mut app, 30);
        for ((name, before), (_, after)) in
            settled.iter().zip(dynamic(&mut app))
        {
            // Every body rests on the floor or the pile, not in it, and
            // inside the walls.
            assert!(before[1] > 0.45, "{name} sank: {before:?}");
            assert!(
                after[0].abs() < 15.0 && after[2].abs() < 15.0,
                "{name} left the floor: {after:?}"
            );
            if name.starts_with("Ball") {
                // Balls roll on with no rolling resistance.
                continue;
            }
            let moved = (0..3)
                .map(|axis| (after[axis] - before[axis]).abs())
                .fold(0.0, f32::max);
            assert!(moved < 0.02, "{name} still moves: {before:?} {after:?}");
        }
        let fell = start
            .iter()
            .zip(&settled)
            .any(|(a, b)| b.1[1] < a.1[1] - 1.0);
        assert!(fell, "the balls dropped onto the pile");
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn the_starter_template_wins_its_sample_scenario() {
        use crate::assets::AssetPlugin;
        use crate::runtime::{
            load_scene, App, RenderExtractPlugin, SceneLoadMode,
        };
        use crate::scenario::{run_scenario, Scenario};

        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project = create_project_from(
            &parent,
            "Coin Run",
            ProjectTemplate::parse("starter").unwrap(),
        )
        .unwrap();
        let mut app = App::new();
        app.add_plugin(AssetPlugin).unwrap();
        app.add_plugin(RenderExtractPlugin).unwrap();
        load_scene(
            app.world_mut(),
            project.root.join("scenes/main.rscene"),
            SceneLoadMode::Replace,
        )
        .unwrap();
        let scenario: Scenario = serde_json::from_str(include_str!(
            "../samples/starter_game/win.scenario.json"
        ))
        .unwrap();
        // The capture is skipped without Vulkan.
        let report = run_scenario(&mut app, &scenario, &parent);
        assert!(report.passed, "{:#?}", report.first_failure);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn replays_of_the_starter_template_seek_through_snapshots() {
        use crate::assets::AssetPlugin;
        use crate::runtime::{
            load_scene, App, Counter, Name, RandomSeed, RenderExtractPlugin,
            ReplayError, ReplaySeeker, SceneLoadMode,
        };
        use crate::scenario::{run_scenario, Scenario, StepAction};

        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project = create_project_from(
            &parent,
            "Coin Run",
            ProjectTemplate::parse("starter").unwrap(),
        )
        .unwrap();
        let scene = project.root.join("scenes/main.rscene");
        let make_app = || {
            let mut app = App::new();
            app.add_plugin(AssetPlugin)?;
            app.add_plugin(RenderExtractPlugin)?;
            load_scene(app.world_mut(), &scene, SceneLoadMode::Replace)
                .expect("the starter scene loads");
            Ok(app)
        };
        // Record the sample scenario playing the level.
        let mut scenario: Scenario = serde_json::from_str(include_str!(
            "../samples/starter_game/win.scenario.json"
        ))
        .unwrap();
        scenario
            .steps
            .retain(|step| !matches!(step.action, StepAction::Capture(_)));
        let mut app = make_app().unwrap();
        app.world_mut().resource_mut::<RandomSeed>().0 = scenario.seed;
        app.start_recording();
        assert!(run_scenario(&mut app, &scenario, &parent).passed);
        let replay = app.finish_recording().unwrap();
        let counter = |app: &App, wanted: &str| {
            let world = app.world();
            let mut counters = world.try_query::<(&Name, &Counter)>().unwrap();
            counters
                .iter(world)
                .find(|(name, _)| name.0 == wanted)
                .map(|(_, counter)| counter.value)
                .unwrap()
        };

        let mut seeker =
            ReplaySeeker::new(replay.clone(), 50, make_app).unwrap();
        assert_eq!(seeker.seek(240).unwrap(), 240);
        assert_eq!(seeker.snapshot_count(), 4);
        assert_eq!(counter(seeker.app(), "Win"), 1);
        // Back to before the first checkpoint, then between checkpoints.
        assert_eq!(seeker.seek(10).unwrap(), 10);
        assert_eq!(counter(seeker.app(), "Score"), 0);
        assert_eq!(seeker.seek(190).unwrap(), 190);
        assert_eq!(counter(seeker.app(), "Score"), 5);
        assert_eq!(counter(seeker.app(), "Win"), 0);
        assert_eq!(seeker.seek(60).unwrap(), 60);
        assert_eq!(seeker.seek(240).unwrap(), 240);
        assert_eq!(counter(seeker.app(), "Win"), 1);
        assert_eq!(seeker.snapshot_count(), 4);

        // Past a checkpoint, each tick still checks the recorded hash.
        let mut tampered = replay;
        let index = tampered
            .hashes
            .iter()
            .position(|&(tick, _)| tick == 170)
            .unwrap();
        tampered.hashes[index].1 ^= 1;
        let mut seeker = ReplaySeeker::new(tampered, 50, make_app).unwrap();
        assert_eq!(seeker.seek(160).unwrap(), 160);
        assert!(matches!(
            seeker.seek(240),
            Err(ReplayError::Diverged { tick: 170 })
        ));
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn invalid_project_names_are_rejected() {
        for name in ["", "../game", "game/name", ".", "..", "CON", "game❤"] {
            assert!(matches!(
                validate_project_name(name),
                Err(ProjectError::InvalidName)
            ));
        }
    }

    #[test]
    fn unversioned_project_is_backed_up_and_migrated() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-project-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project = create_project(&parent, "Legacy Game").unwrap();
        let manifest_path = project.root.join("project.json");
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap())
                .unwrap();
        legacy.as_object_mut().unwrap().remove("format_version");
        legacy.as_object_mut().unwrap().remove("binary_name");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let migrated = open_project(&project.root).unwrap();

        assert_eq!(migrated.manifest.format_version, PROJECT_FORMAT_VERSION);
        assert_eq!(migrated.manifest.binary_name, "legacy_game");
        assert!(project.root.join("project.json.v0.backup").is_file());
        let stored: ProjectManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap())
                .unwrap();
        assert_eq!(stored, migrated.manifest);
        std::fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn editor_preferences_round_trip_and_fall_back_to_defaults() {
        let folder = std::env::temp_dir()
            .join(format!("rusting_prefs_{}", Uuid::new_v4()));
        let path = folder.join("editor_preferences.json");
        assert_eq!(
            EditorPreferences::load_from(&path),
            EditorPreferences::default()
        );

        let preferences = EditorPreferences {
            ui_scale: 1.25,
            font_scale: 1.15,
        };
        preferences.save_to(&path).unwrap();
        assert_eq!(EditorPreferences::load_from(&path), preferences);

        // Files saved before a field existed keep their other settings.
        std::fs::write(&path, "{\"ui_scale\": 40.0}").unwrap();
        let old = EditorPreferences::load_from(&path);
        assert_eq!((old.ui_scale, old.font_scale), (3.0, 1.0));
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            EditorPreferences::load_from(&path),
            EditorPreferences::default()
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    /// Runs a template's own `update` as its game binary would.
    struct TemplateUpdate(fn(&mut bevy_ecs::prelude::World));

    impl crate::runtime::Plugin for TemplateUpdate {
        fn build(
            &self,
            app: &mut crate::App,
        ) -> Result<(), crate::runtime::AppError> {
            app.add_system(crate::runtime::ScheduleStage::Update, self.0);
            Ok(())
        }
    }

    #[test]
    fn the_controller_templates_ship_scenarios_that_pass() {
        for (template, file) in [
            (ProjectTemplate::Platformer2d, "run.json"),
            (ProjectTemplate::FirstPerson3d, "walk.json"),
            (ProjectTemplate::ThirdPerson3d, "walk.json"),
            (ProjectTemplate::PhysicsSandbox, "drop.json"),
        ] {
            let parent = std::env::temp_dir()
                .join(format!("rusting-controller-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&parent).unwrap();
            let project =
                create_project_from(&parent, "Walker", template).unwrap();
            // These templates ship no game code; the scene's controllers
            // and physics play the scenario.
            crate::project_runner::run_project_scenario(
                project.scene_path.clone(),
                TemplateUpdate(|_| {}),
                project.root.join("tests").join(file),
                Some(project.root.join("report.json")),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", template.name()));
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn the_puzzle_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-puzzle-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Box Push", ProjectTemplate::Puzzle)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/puzzle.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::puzzle_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/solve.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_top_down_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-arena-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Arena", ProjectTemplate::TopDown)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/arena.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::arena_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/fight.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_racing_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-racing-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Circuit", ProjectTemplate::Racing)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/racing.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::racing_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/lap.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_twin_stick_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-twin-stick-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Swarm", ProjectTemplate::TwinStick)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/twin_stick.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::twin_stick_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/wave.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_tower_defense_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-tower-defense-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project = create_project_from(
            &parent,
            "Outpost",
            ProjectTemplate::TowerDefense,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/tower_defense.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::tower_defense_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/defend.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_roguelike_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-roguelike-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Crypt", ProjectTemplate::Roguelike)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/roguelike.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::roguelike_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/descend.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_card_game_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-card-game-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Duel", ProjectTemplate::CardGame)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/card_game.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::card_game_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/duel.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn the_rhythm_template_ships_code_whose_scenario_passes() {
        let parent = std::env::temp_dir()
            .join(format!("rusting-rhythm-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&parent).unwrap();
        let project =
            create_project_from(&parent, "Beat", ProjectTemplate::Rhythm)
                .unwrap();
        assert_eq!(
            std::fs::read_to_string(&project.code_path).unwrap(),
            include_str!("templates/rhythm.rs")
        );
        crate::project_runner::run_project_scenario(
            project.scene_path.clone(),
            TemplateUpdate(|world| {
                let time = *world.resource::<crate::runtime::FrameTime>();
                super::rhythm_template::update(
                    &mut crate::project_runner::GameScene { world },
                    &time,
                );
            }),
            project.root.join("tests/song.json"),
            Some(project.root.join("report.json")),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(parent);
    }
}
