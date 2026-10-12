//! Project asset import: copies an image, glTF, or audio file into a
//! project's `assets` folder and writes a sidecar `<file>.rmeta` with a stable ID,
//! import settings, dependencies, a content hash, and source/license
//! provenance. Scenes keep referencing assets by path; the ID survives
//! reimport and replacement, so tools can track an asset across edits.
//!
//! Generator hooks are optional commands listed in `project.json` that
//! produce a file from a prompt (any tool or model service). Their output
//! goes through the same import, validation, and license checks as a file
//! the user picked; a project without hooks loses nothing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::runtime::{
    read_scene_document, scene_revision, SceneDocument, SceneMaterial,
    SceneMesh,
};

pub const META_EXTENSION: &str = "rmeta";
pub const META_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportedKind {
    Image,
    Gltf,
    Audio,
    Font,
}

impl ImportedKind {
    /// The kind for a file extension, or `None` for unsupported files.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "png" | "jpg" | "jpeg" | "bmp" | "tga" => Some(Self::Image),
            "gltf" | "glb" => Some(Self::Gltf),
            "wav" | "ogg" => Some(Self::Audio),
            "ttf" | "otf" => Some(Self::Font),
            _ => None,
        }
    }
}

/// Where an asset came from and under which terms it may be used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetProvenance {
    /// The file the asset was imported from, as given on import.
    pub original: String,
    pub author: Option<String>,
    /// License name or SPDX identifier, such as `CC0-1.0`.
    pub license: Option<String>,
    /// Web page or store listing the asset came from.
    pub url: Option<String>,
    /// Tool or service that generated the asset, if any.
    pub generator: Option<String>,
    pub notes: Option<String>,
}

impl AssetProvenance {
    /// Overwrites the fields `other` sets, and `original` when not empty.
    fn merge(&mut self, other: &AssetProvenance) {
        if !other.original.is_empty() {
            self.original.clone_from(&other.original);
        }
        for (mine, theirs) in [
            (&mut self.author, &other.author),
            (&mut self.license, &other.license),
            (&mut self.url, &other.url),
            (&mut self.generator, &other.generator),
            (&mut self.notes, &other.notes),
        ] {
            if theirs.is_some() {
                mine.clone_from(theirs);
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportSettings {
    /// Images only: the longest edge is scaled down to this many pixels on
    /// import, keeping the aspect ratio.
    pub max_size: Option<u32>,
}

/// Sidecar metadata written next to each imported asset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetMeta {
    pub format_version: u32,
    pub id: Uuid,
    pub kind: ImportedKind,
    #[serde(default)]
    pub settings: ImportSettings,
    /// Files the asset loads, relative to its folder, such as glTF buffers
    /// and textures.
    #[serde(default)]
    pub dependencies: Vec<PathBuf>,
    /// FNV-1a 64 of the imported file, to spot edits made outside import.
    pub content_hash: String,
    #[serde(default)]
    pub source: AssetProvenance,
}

#[derive(Debug)]
pub enum AssetImportError {
    SourceMissing(PathBuf),
    Unsupported(PathBuf),
    Invalid {
        path: PathBuf,
        message: String,
    },
    Exists(PathBuf),
    NotFound(String),
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    GeneratorUnknown(String),
    /// The hook did not run, exited with an error, or printed no usable
    /// result.
    GeneratorFailed {
        hook: String,
        message: String,
    },
    /// A generated scene was edited by hand since it was generated.
    GeneratedSceneEdited(PathBuf),
    /// A new generated scene failed one of its generator's `checks`.
    GeneratedSceneCheckFailed {
        name: String,
        scenario: PathBuf,
        message: String,
    },
}

impl AssetImportError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SourceMissing(_) => "ASSET_SOURCE_MISSING",
            Self::Unsupported(_) => "ASSET_UNSUPPORTED",
            Self::Invalid { .. } => "ASSET_INVALID",
            Self::Exists(_) => "ASSET_EXISTS",
            Self::NotFound(_) => "ASSET_NOT_FOUND",
            Self::Io { .. } => "ASSET_IO",
            Self::GeneratorUnknown(_) => "GENERATOR_UNKNOWN",
            Self::GeneratorFailed { .. } => "GENERATOR_FAILED",
            Self::GeneratedSceneEdited(_) => "GENERATED_SCENE_EDITED",
            Self::GeneratedSceneCheckFailed { .. } => {
                "GENERATED_SCENE_CHECK_FAILED"
            }
        }
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::SourceMissing(path)
            | Self::Unsupported(path)
            | Self::Exists(path)
            | Self::GeneratedSceneEdited(path)
            | Self::GeneratedSceneCheckFailed { scenario: path, .. }
            | Self::Invalid { path, .. }
            | Self::Io { path, .. } => Some(path),
            Self::NotFound(_)
            | Self::GeneratorUnknown(_)
            | Self::GeneratorFailed { .. } => None,
        }
    }
}

impl Display for AssetImportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceMissing(path) => {
                write!(formatter, "`{}` does not exist", path.display())
            }
            Self::Unsupported(path) => write!(
                formatter,
                "`{}` is not a png, jpeg, bmp, tga, gltf, glb, wav or ogg file",
                path.display()
            ),
            Self::Invalid { path, message } => {
                write!(
                    formatter,
                    "`{}` is not valid: {message}",
                    path.display()
                )
            }
            Self::Exists(path) => write!(
                formatter,
                "`{}` already exists; use `asset reimport` to replace it",
                path.display()
            ),
            Self::NotFound(name) => {
                write!(formatter, "no imported asset matches `{name}`")
            }
            Self::Io { path, error } => {
                write!(formatter, "`{}`: {error}", path.display())
            }
            Self::GeneratorUnknown(hook) => write!(
                formatter,
                "project.json has no generator hook named `{hook}`"
            ),
            Self::GeneratorFailed { hook, message } => {
                write!(formatter, "generator `{hook}`: {message}")
            }
            Self::GeneratedSceneEdited(path) => write!(
                formatter,
                "`{}` was edited since it was generated; pass --force to replace it",
                path.display()
            ),
            Self::GeneratedSceneCheckFailed {
                name,
                scenario,
                message,
            } => write!(
                formatter,
                "the scene from generator `{name}` failed check `{}`, so the old scene stays: {message}",
                scenario.display()
            ),
        }
    }
}

impl std::error::Error for AssetImportError {}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> AssetImportError + '_ {
    move |error| AssetImportError::Io {
        path: path.to_owned(),
        error,
    }
}

/// What an import or reimport wrote.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ImportReport {
    pub id: Uuid,
    pub kind: ImportedKind,
    /// Relative to the project root, with `/` separators.
    pub path: String,
    /// The path a scene in `scenes/` uses to reference the asset.
    pub reference: String,
    pub dependencies: Vec<PathBuf>,
    /// Image size in pixels, glTF primitive count as `[count, 0]`, or WAV
    /// sample rate and length in milliseconds (`[0, 0]` for Ogg).
    pub size: [u32; 2],
    /// WAV channel count: 1 mono, 2 stereo. A mono clip cannot pan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    /// A preview: the checks ran on a staged copy and nothing was written.
    pub dry_run: bool,
    pub settings: ImportSettings,
    pub source: AssetProvenance,
    /// Project scenes that reference the asset.
    pub referenced_by: Vec<String>,
    pub warnings: Vec<String>,
}

#[must_use]
pub fn meta_path(asset: &Path) -> PathBuf {
    let mut name = asset.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(META_EXTENSION);
    asset.with_file_name(name)
}

fn slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn is_plain_relative(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

/// Files a glTF loads through relative URIs; `data:` URIs are embedded.
#[cfg(feature = "gltf")]
fn gltf_dependencies(path: &Path) -> Result<Vec<PathBuf>, AssetImportError> {
    let document =
        gltf::Gltf::open(path).map_err(|error| AssetImportError::Invalid {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let buffers =
        document
            .buffers()
            .filter_map(|buffer| match buffer.source() {
                gltf::buffer::Source::Uri(uri) => Some(uri.to_owned()),
                gltf::buffer::Source::Bin => None,
            });
    let images = document.images().filter_map(|image| match image.source() {
        gltf::image::Source::Uri { uri, .. } => Some(uri.to_owned()),
        gltf::image::Source::View { .. } => None,
    });
    // ponytail: percent-encoded URIs are taken literally; decode them if an
    // exporter writes spaces as %20.
    let mut dependencies = BTreeSet::new();
    for uri in buffers.chain(images) {
        if uri.starts_with("data:") {
            continue;
        }
        let dependency = PathBuf::from(&uri);
        if !is_plain_relative(&dependency) {
            return Err(AssetImportError::Invalid {
                path: path.to_owned(),
                message: format!(
                    "URI `{uri}` must stay inside the glTF's folder"
                ),
            });
        }
        dependencies.insert(dependency);
    }
    Ok(dependencies.into_iter().collect())
}

#[cfg(not(feature = "gltf"))]
fn gltf_dependencies(path: &Path) -> Result<Vec<PathBuf>, AssetImportError> {
    Err(AssetImportError::Invalid {
        path: path.to_owned(),
        message: "this build has no glTF support (feature `gltf`)".into(),
    })
}

/// Loads the file the way the runtime would and returns its size.
fn validate(
    kind: ImportedKind,
    path: &Path,
) -> Result<[u32; 2], AssetImportError> {
    let invalid = |message: String| AssetImportError::Invalid {
        path: path.to_owned(),
        message,
    };
    match kind {
        ImportedKind::Image => image::image_dimensions(path)
            .map(|(width, height)| [width, height])
            .map_err(|error| invalid(error.to_string())),
        ImportedKind::Gltf => {
            #[cfg(feature = "gltf")]
            {
                let mut server = crate::assets::AssetServer::default();
                let primitives = server
                    .import_gltf(path)
                    .map_err(|error| invalid(error.to_string()))?;
                Ok([u32::try_from(primitives.len()).unwrap_or(u32::MAX), 0])
            }
            #[cfg(not(feature = "gltf"))]
            gltf_dependencies(path).map(|_| [0, 0])
        }
        ImportedKind::Audio => {
            let bytes = std::fs::read(path).map_err(io(path))?;
            audio_size(&bytes).map_err(|message| invalid(message.into()))
        }
        ImportedKind::Font => {
            let bytes = std::fs::read(path).map_err(io(path))?;
            let magic = bytes.get(..4).unwrap_or_default();
            if ![&[0, 1, 0, 0][..], b"OTTO", b"true"].contains(&magic) {
                return Err(invalid("not a TrueType or OpenType font".into()));
            }
            // egui is what draws HUD and sign text; make sure it can.
            #[cfg(feature = "ui")]
            if crate::text_texture::text_texture_in_font(
                "Aa",
                crate::text_texture::TextStyle::default(),
                &bytes,
            )
            .is_none()
            {
                return Err(invalid("egui cannot read this font".into()));
            }
            Ok([0, 0])
        }
    }
}

/// WAV sample rate and length in milliseconds. Ogg files only get their
/// magic checked.
// ponytail: Ogg length needs a Vorbis/Opus page walk; add it with the
// audio engine (Milestone 15).
fn audio_size(bytes: &[u8]) -> Result<[u32; 2], &'static str> {
    if bytes.starts_with(b"OggS") {
        return Ok([0, 0]);
    }
    wav_format(bytes).map(|(_, rate, milliseconds)| [rate, milliseconds])
}

/// WAV channel count, sample rate and length in milliseconds.
fn wav_format(bytes: &[u8]) -> Result<(u16, u32, u32), &'static str> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE or Ogg file");
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| {
        u32::from_le_bytes([
            bytes[at],
            bytes[at + 1],
            bytes[at + 2],
            bytes[at + 3],
        ])
    };
    let (mut format, mut data) = (None, None);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let length = u32_at(at + 4) as usize;
        let body = at + 8;
        match &bytes[at..at + 4] {
            b"fmt " if length >= 16 && body + 16 <= bytes.len() => {
                // Channels, sample rate, and bytes per sample frame.
                format = Some((
                    u16_at(body + 2),
                    u32_at(body + 4),
                    u16_at(body + 12),
                ));
            }
            b"data" => data = Some(length.min(bytes.len() - body)),
            _ => {}
        }
        // Chunks are padded to an even length.
        at = body.saturating_add(length + (length & 1));
    }
    match (format, data) {
        (Some((channels, rate, frame)), Some(data))
            if channels > 0 && rate > 0 && frame > 0 =>
        {
            let frames = (data / usize::from(frame)) as u64;
            let milliseconds = frames * 1000 / u64::from(rate);
            Ok((
                channels,
                rate,
                u32::try_from(milliseconds).unwrap_or(u32::MAX),
            ))
        }
        _ => Err("WAV file has no usable `fmt ` and `data` chunks"),
    }
}

/// Scales an image file down in place so its longest edge fits `max_size`.
fn apply_settings(
    kind: ImportedKind,
    path: &Path,
    settings: &ImportSettings,
) -> Result<(), AssetImportError> {
    let (ImportedKind::Image, Some(max_size)) = (kind, settings.max_size)
    else {
        return Ok(());
    };
    let invalid = |error: image::ImageError| AssetImportError::Invalid {
        path: path.to_owned(),
        message: error.to_string(),
    };
    let image = image::open(path).map_err(invalid)?;
    let longest = image.width().max(image.height());
    if longest <= max_size.max(1) {
        return Ok(());
    }
    image
        .resize(max_size, max_size, image::imageops::FilterType::Lanczos3)
        .save(path)
        .map_err(invalid)
}

fn write_meta(asset: &Path, meta: &AssetMeta) -> Result<(), AssetImportError> {
    let path = meta_path(asset);
    let mut bytes =
        serde_json::to_vec_pretty(meta).expect("asset metadata serializes");
    bytes.push(b'\n');
    crate::runtime::write_atomic(&path, &bytes).map_err(io(&path))
}

fn read_meta(path: &Path) -> Result<AssetMeta, AssetImportError> {
    let bytes = std::fs::read(path).map_err(io(path))?;
    serde_json::from_slice(&bytes).map_err(|error| AssetImportError::Invalid {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

/// Copies `source` and its dependencies into `destination`'s folder,
/// validates, applies settings, and returns the finished metadata.
fn copy_in(
    source: &Path,
    destination: &Path,
    settings: &ImportSettings,
) -> Result<(ImportedKind, Vec<PathBuf>, [u32; 2], String), AssetImportError> {
    if !source.is_file() {
        return Err(AssetImportError::SourceMissing(source.to_owned()));
    }
    let kind = ImportedKind::from_path(source)
        .ok_or_else(|| AssetImportError::Unsupported(source.to_owned()))?;
    let dependencies = match kind {
        ImportedKind::Image | ImportedKind::Audio | ImportedKind::Font => {
            Vec::new()
        }
        ImportedKind::Gltf => gltf_dependencies(source)?,
    };
    let size = validate(kind, source)?;
    let from = source.parent().unwrap_or(Path::new("."));
    let to = destination.parent().unwrap_or(Path::new("."));
    // Copying a file onto itself would truncate it.
    let same = |a: &Path, b: &Path| {
        a.canonicalize()
            .ok()
            .is_some_and(|a| b.canonicalize().ok() == Some(a))
    };
    for file in dependencies
        .iter()
        .map(|dependency| (from.join(dependency), to.join(dependency)))
        .chain([(source.to_owned(), destination.to_owned())])
    {
        if same(&file.0, &file.1) {
            continue;
        }
        if let Some(parent) = file.1.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        std::fs::copy(&file.0, &file.1).map_err(io(&file.0))?;
    }
    apply_settings(kind, destination, settings)?;
    let size = if settings.max_size.is_some() {
        validate(kind, destination)?
    } else {
        size
    };
    let bytes = std::fs::read(destination).map_err(io(destination))?;
    Ok((kind, dependencies, size, scene_revision(&bytes)))
}

fn report(
    project_root: &Path,
    asset: &Path,
    meta: AssetMeta,
    size: [u32; 2],
    dry_run: bool,
) -> ImportReport {
    let relative = asset.strip_prefix(project_root).unwrap_or(asset);
    let mut warnings = Vec::new();
    if meta.source.license.is_none() {
        warnings.push(
            "no license recorded; add one with --license before shipping"
                .into(),
        );
    }
    ImportReport {
        id: meta.id,
        kind: meta.kind,
        path: slash(relative),
        reference: format!("../{}", slash(relative)),
        dependencies: meta.dependencies,
        size,
        channels: wav_channels(asset),
        dry_run,
        settings: meta.settings,
        source: meta.source,
        referenced_by: scenes_referencing(project_root, asset),
        warnings,
    }
}

/// A WAV file's channel count; `None` for anything else.
fn wav_channels(path: &Path) -> Option<u16> {
    let bytes = std::fs::read(path).ok()?;
    wav_format(&bytes).ok().map(|format| format.0)
}

/// A fresh folder under the system temp directory, removed on drop.
struct Staging(PathBuf);

impl Staging {
    fn new(purpose: &str) -> Result<Self, AssetImportError> {
        let path = std::env::temp_dir()
            .join(format!("rusting-{purpose}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).map_err(io(&path))?;
        Ok(Self(path))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs [`copy_in`] onto `destination`, or onto a staged copy when
/// `dry_run` is set so the project is not touched.
fn copy_in_or_preview(
    source: &Path,
    destination: &Path,
    settings: &ImportSettings,
    dry_run: bool,
) -> Result<(ImportedKind, Vec<PathBuf>, [u32; 2], String), AssetImportError> {
    if !dry_run {
        return copy_in(source, destination, settings);
    }
    let staging = Staging::new("asset-preview")?;
    copy_in(
        source,
        &staging.0.join(destination.file_name().unwrap_or_default()),
        settings,
    )
}

/// Imports `source` into `<project>/assets/<folder>/`; a `folder` that
/// starts with `assets/` means the same folder. Fails without
/// writing when the file does not load or the destination exists. A
/// non-empty `provenance.original` replaces the recorded source path.
/// `dry_run` runs every check and returns the report without writing.
pub fn import_asset(
    project_root: &Path,
    source: &Path,
    folder: &Path,
    provenance: &AssetProvenance,
    settings: &ImportSettings,
    dry_run: bool,
) -> Result<ImportReport, AssetImportError> {
    if !is_plain_relative(folder) {
        return Err(AssetImportError::Invalid {
            path: folder.to_owned(),
            message: "the folder must be relative to `assets`".into(),
        });
    }
    let folder = folder.strip_prefix("assets").unwrap_or(folder);
    let name = source
        .file_name()
        .ok_or_else(|| AssetImportError::SourceMissing(source.to_owned()))?;
    let destination = project_root.join("assets").join(folder).join(name);
    for path in [destination.clone(), meta_path(&destination)] {
        if path.exists() {
            return Err(AssetImportError::Exists(path));
        }
    }
    let (kind, dependencies, size, content_hash) =
        copy_in_or_preview(source, &destination, settings, dry_run)?;
    let mut recorded = AssetProvenance {
        original: source.display().to_string(),
        ..AssetProvenance::default()
    };
    recorded.merge(provenance);
    let meta = AssetMeta {
        format_version: META_FORMAT_VERSION,
        id: Uuid::new_v4(),
        kind,
        settings: settings.clone(),
        dependencies,
        content_hash,
        source: recorded,
    };
    if !dry_run {
        write_meta(&destination, &meta)?;
    }
    let mut report = report(project_root, &destination, meta, size, dry_run);
    if dry_run {
        report.channels = wav_channels(source);
    }
    Ok(report)
}

/// Replaces an imported asset, found by ID or by its path relative to the
/// project or to `assets/`, and
/// keeps its ID. The new content comes from `from`, else from the recorded
/// original when it still exists, else the asset is revalidated in place.
/// Provenance fields and settings that are given replace the recorded ones.
/// `dry_run` previews the replacement without writing.
pub fn reimport_asset(
    project_root: &Path,
    asset: &str,
    from: Option<&Path>,
    provenance: &AssetProvenance,
    settings: Option<&ImportSettings>,
    dry_run: bool,
) -> Result<ImportReport, AssetImportError> {
    let (path, mut meta) = match find_asset(project_root, asset) {
        Err(AssetImportError::NotFound(_)) => {
            unregistered_asset(project_root, asset)
                .ok_or_else(|| AssetImportError::NotFound(asset.to_owned()))?
        }
        found => found?,
    };
    if let Some(settings) = settings {
        meta.settings = settings.clone();
    }
    let original = PathBuf::from(&meta.source.original);
    let source = match from {
        Some(from) => from.to_owned(),
        None if original.is_file() => original,
        None => path.clone(),
    };
    if ImportedKind::from_path(&source) != Some(meta.kind) {
        return Err(AssetImportError::Invalid {
            path: source,
            message: format!(
                "a {:?} asset must be replaced by the same kind",
                meta.kind
            ),
        });
    }
    let (_, dependencies, size, content_hash) =
        copy_in_or_preview(&source, &path, &meta.settings, dry_run)?;
    if from.is_some() {
        meta.source.original = source.display().to_string();
    }
    meta.source.merge(provenance);
    meta.dependencies = dependencies;
    meta.content_hash = content_hash;
    if !dry_run {
        write_meta(&path, &meta)?;
    }
    let mut report = report(project_root, &path, meta, size, dry_run);
    if dry_run {
        report.channels = wav_channels(&source);
    }
    Ok(report)
}

/// A project command, listed under `generators` in `project.json`, that
/// turns a prompt into an asset file. The engine does not know or need any
/// particular model or service: the command runs from the project root
/// without a shell, gets the prompt in `RUSTING_PROMPT` and an empty folder
/// in `RUSTING_OUTPUT_DIR`, writes its file there, and prints one JSON line
/// such as `{"file": "crate.png", "license": "CC0-1.0"}`. Other keys are
/// the [`AssetProvenance`] fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneratorHook {
    /// Program and arguments.
    pub command: Vec<String>,
    /// License recorded when the output names none. Output without any
    /// license is rejected.
    pub license: Option<String>,
    /// Settings every result is imported with, such as `max_size`.
    pub settings: ImportSettings,
}

/// The last line a generator hook prints.
#[derive(Deserialize)]
struct GeneratedFile {
    /// Relative to `RUSTING_OUTPUT_DIR`, or absolute.
    file: PathBuf,
    #[serde(flatten)]
    provenance: AssetProvenance,
}

/// Where a generated file goes.
#[derive(Clone, Copy, Debug)]
pub enum GenerateTarget<'a> {
    /// A new asset in `<project>/assets/<folder>/`.
    Import { folder: &'a Path },
    /// Replaces an imported asset, found by ID or path, keeping its ID.
    Replace { asset: &'a str },
}

/// Runs the hook named `hook` and imports its file through the normal
/// importer, so it gets the same type, size, dependency, and metadata
/// checks. The recorded original is `generator:<hook>` and the prompt goes
/// into the notes unless the hook sets them.
pub fn generate_asset(
    project_root: &Path,
    hooks: &BTreeMap<String, GeneratorHook>,
    hook: &str,
    prompt: &str,
    target: GenerateTarget<'_>,
    dry_run: bool,
) -> Result<ImportReport, AssetImportError> {
    if BUILTIN_GENERATORS.contains(&hook) && !hooks.contains_key(hook) {
        return generate_builtin(project_root, hook, prompt, target, dry_run);
    }
    let config = hooks
        .get(hook)
        .ok_or_else(|| AssetImportError::GeneratorUnknown(hook.to_owned()))?;
    let failed = |message: String| AssetImportError::GeneratorFailed {
        hook: hook.to_owned(),
        message,
    };
    let (program, arguments) = config
        .command
        .split_first()
        .ok_or_else(|| failed("`command` is empty".into()))?;
    let output_dir = Staging::new("generated")?;
    let output = std::process::Command::new(program)
        .args(arguments)
        .current_dir(project_root)
        .env("RUSTING_PROMPT", prompt)
        .env("RUSTING_OUTPUT_DIR", &output_dir.0)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| {
            failed(format!("could not run `{program}`: {error}"))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(failed(format!(
            "exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let last = stdout.lines().rev().find(|line| !line.trim().is_empty());
    let generated: GeneratedFile = last
        .and_then(|line| serde_json::from_str(line).ok())
        .ok_or_else(|| {
            failed(
                "the last output line must be JSON like {\"file\": \"x.png\"}"
                    .into(),
            )
        })?;
    let mut provenance = generated.provenance;
    if provenance.license.is_none() {
        provenance.license.clone_from(&config.license);
    }
    if provenance.license.is_none() {
        return Err(failed(
            "no license: print one or set `license` on the hook".into(),
        ));
    }
    provenance.generator.get_or_insert_with(|| hook.to_owned());
    provenance
        .notes
        .get_or_insert_with(|| format!("prompt: {prompt}"));
    provenance.original = format!("generator:{hook}");
    let file = output_dir.0.join(generated.file);
    match target {
        GenerateTarget::Import { folder } => import_asset(
            project_root,
            &file,
            folder,
            &provenance,
            &config.settings,
            dry_run,
        ),
        GenerateTarget::Replace { asset } => reimport_asset(
            project_root,
            asset,
            Some(&file),
            &provenance,
            (config.settings != ImportSettings::default())
                .then_some(&config.settings),
            dry_run,
        ),
    }
}

/// A scene made by a project command, listed under `scene_generators` in
/// `project.json`. The command runs from the project root without a shell,
/// gets `RUSTING_SEED`, the `inputs` as JSON in `RUSTING_INPUTS`, and a
/// path to write the scene to in `RUSTING_OUTPUT` (a game binary can read
/// them with [`crate::procgen::GeneratorRequest::from_env`] and write the
/// scene with [`crate::runtime::save_scene`]). The result is a normal scene
/// at `output`, cached by the command, seed, inputs and the contents of the
/// `watch` paths: [`generate_scenes`] runs the command again only when one
/// of them changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneGenerator {
    /// Program and arguments.
    pub command: Vec<String>,
    /// The scene to write, relative to the project root.
    pub output: PathBuf,
    pub seed: u64,
    /// Any JSON the command reads, such as a size or a room count.
    pub inputs: serde_json::Value,
    /// Files and folders, relative to the project root, whose contents
    /// the output depends on, such as the generator's source folder.
    pub watch: Vec<PathBuf>,
    /// Scenario files, relative to the project root, that must pass on the
    /// new scene before it replaces the old one, such as an `explore` run
    /// that reaches every goal or `invariants` the level must keep.
    /// `rusting scene generate` plays them on the staged scene.
    pub checks: Vec<PathBuf>,
}

/// What [`generate_scenes`] did for one generator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GeneratedScene {
    pub name: String,
    pub output: PathBuf,
    /// `generated`, or `cached` when nothing it depends on changed.
    pub status: &'static str,
    /// FNV-1a 64 over the command, seed, inputs and watched contents.
    pub key: String,
    /// The `checks` the new scene passed; empty when cached.
    pub checked: Vec<PathBuf>,
}

/// The cache, `.rusting/generated.json`: the key each output was made
/// with and the output's revision, to spot hand edits.
#[derive(Default, Serialize, Deserialize)]
struct GeneratedCache(BTreeMap<String, (String, String)>);

fn generator_key(project_root: &Path, generator: &SceneGenerator) -> String {
    let mut bytes = serde_json::to_vec(&(
        &generator.command,
        &generator.output,
        generator.seed,
        &generator.inputs,
    ))
    .unwrap_or_default();
    for watched in &generator.watch {
        let mut stack = vec![project_root.join(watched)];
        while let Some(path) = stack.pop() {
            bytes.extend(
                path.strip_prefix(project_root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .bytes(),
            );
            bytes.push(0);
            if let Ok(entries) = std::fs::read_dir(&path) {
                let mut children: Vec<_> =
                    entries.flatten().map(|entry| entry.path()).collect();
                // Reverse order so the stack visits them by name.
                children.sort_by(|a, b| b.cmp(a));
                stack.extend(children);
            } else if let Ok(contents) = std::fs::read(&path) {
                bytes.extend(contents);
            }
            bytes.push(0);
        }
    }
    crate::runtime::scene_revision(&bytes)
}

/// Runs the `scene_generators` named in `names` (all of them when empty)
/// whose key changed or whose output is missing, or every one with
/// `force`. Each output is checked as a scene before it is written, so a
/// broken command never replaces a good scene. An output edited by hand
/// since it was generated is refused with `GENERATED_SCENE_EDITED` unless
/// `force` is set. `dry_run` reports what would run without running it.
///
/// `check` runs one of a generator's `checks` on the staged scene (its
/// path is the second argument) and returns why it failed; any failure
/// keeps the old scene and gives `GENERATED_SCENE_CHECK_FAILED`.
pub fn generate_scenes(
    project_root: &Path,
    generators: &BTreeMap<String, SceneGenerator>,
    names: &[&str],
    force: bool,
    dry_run: bool,
    check: &mut dyn FnMut(&Path, &Path) -> Result<(), String>,
) -> Result<Vec<GeneratedScene>, AssetImportError> {
    for name in names {
        if !generators.contains_key(*name) {
            return Err(AssetImportError::GeneratorUnknown((*name).to_owned()));
        }
    }
    let cache_path = project_root.join(".rusting/generated.json");
    let mut cache: GeneratedCache = std::fs::read(&cache_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut results = Vec::new();
    for (name, generator) in generators {
        if !names.is_empty() && !names.contains(&name.as_str()) {
            continue;
        }
        let failed = |message: String| AssetImportError::GeneratorFailed {
            hook: name.clone(),
            message,
        };
        let output = project_root.join(&generator.output);
        let key = generator_key(project_root, generator);
        let current = std::fs::read(&output)
            .ok()
            .map(|bytes| crate::runtime::scene_revision(&bytes));
        let recorded = cache.0.get(name);
        let cached = recorded.is_some_and(|(old, revision)| {
            *old == key && current.as_ref() == Some(revision)
        });
        if !force {
            if let (Some((_, revision)), Some(current)) = (recorded, &current) {
                if revision != current {
                    return Err(AssetImportError::GeneratedSceneEdited(output));
                }
            }
        }
        let mut result = GeneratedScene {
            name: name.clone(),
            output: generator.output.clone(),
            status: if cached && !force {
                "cached"
            } else {
                "generated"
            },
            key: key.clone(),
            checked: Vec::new(),
        };
        if result.status == "cached" || dry_run {
            results.push(result);
            continue;
        }
        let (program, arguments) = generator
            .command
            .split_first()
            .ok_or_else(|| failed("`command` is empty".into()))?;
        let parent = output.parent().unwrap_or(project_root);
        std::fs::create_dir_all(parent).map_err(io(parent))?;
        // Beside the output, so the scene's relative asset paths hold.
        let staged = TemporaryFile(
            parent
                .join(format!(".{name}-{}.generating.rscene", Uuid::new_v4())),
        );
        let run = std::process::Command::new(program)
            .args(arguments)
            .current_dir(project_root)
            .env("RUSTING_SEED", generator.seed.to_string())
            .env("RUSTING_INPUTS", generator.inputs.to_string())
            .env("RUSTING_OUTPUT", &staged.0)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| {
                failed(format!("could not run `{program}`: {error}"))
            })?;
        if !run.status.success() {
            let stderr = String::from_utf8_lossy(&run.stderr);
            return Err(failed(format!(
                "exited with {}: {}",
                run.status,
                stderr.trim()
            )));
        }
        let bytes = std::fs::read(&staged.0).map_err(|error| {
            failed(format!("wrote no scene to RUSTING_OUTPUT: {error}"))
        })?;
        crate::runtime::parse_scene_document(&bytes).map_err(|error| {
            failed(format!("wrote an invalid scene: {error}"))
        })?;
        for scenario in &generator.checks {
            check(&project_root.join(scenario), &staged.0).map_err(
                |message| AssetImportError::GeneratedSceneCheckFailed {
                    name: name.clone(),
                    scenario: scenario.clone(),
                    message,
                },
            )?;
            result.checked.push(scenario.clone());
        }
        crate::runtime::write_atomic(&output, &bytes).map_err(io(&output))?;
        cache.0.insert(
            name.clone(),
            (key, crate::runtime::scene_revision(&bytes)),
        );
        // Saved after each scene, so a later failure does not leave a
        // written scene looking edited by hand.
        let cache_bytes = serde_json::to_vec_pretty(&cache).unwrap_or_default();
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        std::fs::write(&cache_path, cache_bytes).map_err(io(&cache_path))?;
        results.push(result);
    }
    Ok(results)
}

/// A file removed when dropped.
struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Generator hook names built into the engine; a project hook with the same
/// name wins.
pub const BUILTIN_GENERATORS: [&str; 5] =
    ["sfx", "texture", "sprite", "mesh", "font"];

/// A built-in generator: `prompt` is a preset and an optional seed (`coin`
/// or `coin 7`). `sfx` synthesizes a WAV with [`crate::sfx::synth`];
/// `texture` draws a tileable PNG with [`crate::texgen::synth`], and
/// `sprite` a transparent PNG shape with [`crate::texgen::sprite`],
/// `mesh` a low-poly binary glTF with [`crate::meshgen::synth`], and
/// `font` a pixel TrueType font with [`crate::fontgen::synth`], which
/// takes no seed.
fn generate_builtin(
    project_root: &Path,
    hook: &str,
    prompt: &str,
    target: GenerateTarget<'_>,
    dry_run: bool,
) -> Result<ImportReport, AssetImportError> {
    let failed = |message: String| AssetImportError::GeneratorFailed {
        hook: hook.to_owned(),
        message,
    };
    let mut words = prompt.split_whitespace();
    let preset = words.next().unwrap_or_default();
    let seed = match words.next().map(str::parse::<u64>) {
        None => 1,
        Some(Ok(seed)) => seed,
        Some(Err(_)) => {
            return Err(failed(format!(
                "the seed in `{prompt}` must be a whole number"
            )))
        }
    };
    let unknown = |presets: &[&str]| {
        failed(format!(
            "unknown preset `{preset}`; use one of {}",
            presets.join(", ")
        ))
    };
    let output_dir = Staging::new("generated")?;
    let file = if hook == "font" {
        if prompt.split_whitespace().count() > 1 {
            return Err(failed(format!(
                "`{prompt}`: a font preset takes no seed"
            )));
        }
        let font = crate::fontgen::synth(preset)
            .ok_or_else(|| unknown(&crate::fontgen::PRESETS))?;
        let file = output_dir.0.join(format!("{preset}.ttf"));
        std::fs::write(&file, font).map_err(io(&file))?;
        file
    } else if hook == "sfx" {
        let samples = crate::sfx::synth(preset, seed)
            .ok_or_else(|| unknown(&crate::sfx::PRESETS))?;
        let file = output_dir.0.join(format!("{preset}_{seed}.wav"));
        crate::audio_output::write_wav(&file, &samples).map_err(io(&file))?;
        file
    } else if hook == "mesh" {
        let glb = crate::meshgen::synth(preset, seed)
            .ok_or_else(|| unknown(&crate::meshgen::PRESETS))?;
        let file = output_dir.0.join(format!("{preset}_{seed}.glb"));
        std::fs::write(&file, glb).map_err(io(&file))?;
        file
    } else {
        let image = if hook == "sprite" {
            crate::texgen::sprite(preset, seed)
                .ok_or_else(|| unknown(&crate::texgen::SPRITES))?
        } else {
            crate::texgen::synth(preset, seed)
                .ok_or_else(|| unknown(&crate::texgen::PRESETS))?
        };
        let file = output_dir.0.join(format!("{preset}_{seed}.png"));
        image
            .save(&file)
            .map_err(|error| failed(format!("could not write PNG: {error}")))?;
        file
    };
    let provenance = AssetProvenance {
        original: format!("generator:{hook}"),
        license: Some("CC0-1.0".to_owned()),
        generator: Some(format!("rusting {hook}")),
        notes: Some(if hook == "font" {
            format!("preset {preset}")
        } else {
            format!("preset {preset}, seed {seed}")
        }),
        ..AssetProvenance::default()
    };
    match target {
        GenerateTarget::Import { folder } => import_asset(
            project_root,
            &file,
            folder,
            &provenance,
            &ImportSettings::default(),
            dry_run,
        ),
        GenerateTarget::Replace { asset } => reimport_asset(
            project_root,
            asset,
            Some(&file),
            &provenance,
            None,
            dry_run,
        ),
    }
}

/// Every `.rmeta` under `<project>/assets`, as `(asset path, metadata)`.
fn metas(
    project_root: &Path,
) -> Vec<(PathBuf, Result<AssetMeta, AssetImportError>)> {
    let mut found = Vec::new();
    let mut folders = vec![project_root.join("assets")];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.extension().is_some_and(|e| e == META_EXTENSION) {
                found.push((path.with_extension(""), read_meta(&path)));
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

/// A file under `assets/` that has no `.rmeta` yet, such as a model brought
/// in by `scene add-model`, gets fresh metadata so it can be reimported.
fn unregistered_asset(
    project_root: &Path,
    asset: &str,
) -> Option<(PathBuf, AssetMeta)> {
    let relative = Path::new(asset);
    if !is_plain_relative(relative) || !relative.starts_with("assets") {
        return None;
    }
    let path = project_root.join(relative);
    let kind = ImportedKind::from_path(&path).filter(|_| path.is_file())?;
    Some((
        path,
        AssetMeta {
            format_version: META_FORMAT_VERSION,
            id: Uuid::new_v4(),
            kind,
            settings: ImportSettings::default(),
            dependencies: Vec::new(),
            content_hash: String::new(),
            source: AssetProvenance::default(),
        },
    ))
}

fn find_asset(
    project_root: &Path,
    asset: &str,
) -> Result<(PathBuf, AssetMeta), AssetImportError> {
    let id = Uuid::parse_str(asset).ok();
    // `textures/x.png` works as `import --to textures` names it.
    let by_path = [
        project_root.join(asset),
        project_root.join("assets").join(asset),
    ];
    metas(project_root)
        .into_iter()
        .filter_map(|(path, meta)| Some((path, meta.ok()?)))
        .find(|(path, meta)| Some(meta.id) == id || by_path.contains(path))
        .ok_or_else(|| AssetImportError::NotFound(asset.to_owned()))
}

/// Every mesh and texture path a scene references, with the entity index.
#[must_use]
pub fn asset_references(document: &SceneDocument) -> Vec<(PathBuf, usize)> {
    let mut references = Vec::new();
    for (index, entity) in document.entities.iter().enumerate() {
        if let Some(renderer) = &entity.mesh_renderer {
            if let SceneMesh::AssetPath(path) = &renderer.mesh {
                references.push((path.clone(), index));
            }
            if let SceneMaterial::Inline(material) = &renderer.material {
                for path in [
                    &material.base_color_texture,
                    &material.normal_texture,
                    &material.metallic_roughness_texture,
                    &material.occlusion_texture,
                    &material.emissive_texture,
                ]
                .into_iter()
                .flatten()
                {
                    references.push((path.clone(), index));
                }
            }
        }
    }
    references
}

/// Scenes in `<project>/scenes` whose meshes or textures resolve to
/// `asset`, as project-relative paths.
fn scenes_referencing(project_root: &Path, asset: &Path) -> Vec<String> {
    let Ok(asset) = asset.canonicalize() else {
        return Vec::new();
    };
    let scenes = project_root.join("scenes");
    let Ok(entries) = std::fs::read_dir(&scenes) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "rscene"))
        .filter(|scene| {
            read_scene_document(scene).is_ok_and(|document| {
                asset_references(&document).iter().any(|(path, _)| {
                    scenes.join(path).canonicalize().ok().as_ref()
                        == Some(&asset)
                })
            })
        })
        .map(|scene| slash(scene.strip_prefix(project_root).unwrap_or(&scene)))
        .collect();
    found.sort();
    found
}

/// One problem `list_assets` found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AssetIssue {
    pub code: &'static str,
    /// `error` blocks shipping; `warning` does not.
    pub severity: &'static str,
    pub message: String,
    pub file: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct AssetCatalog {
    pub assets: Vec<ImportReport>,
    pub issues: Vec<AssetIssue>,
}

/// Lists imported assets with their references and reports missing files
/// and dependencies, duplicate IDs, edits made outside import, assets
/// without a license, and asset files that were never imported.
#[must_use]
pub fn list_assets(project_root: &Path) -> AssetCatalog {
    let mut catalog = AssetCatalog::default();
    let mut issue = |code, severity, message: String, file: &Path| {
        catalog.issues.push(AssetIssue {
            code,
            severity,
            message,
            file: file.to_owned(),
        });
    };
    let mut known = BTreeSet::new();
    let mut ids: BTreeMap<Uuid, PathBuf> = BTreeMap::new();
    let mut assets = Vec::new();
    for (path, meta) in metas(project_root) {
        let meta = match meta {
            Ok(meta) => meta,
            Err(error) => {
                issue(
                    "ASSET_META_INVALID",
                    "error",
                    error.to_string(),
                    &meta_path(&path),
                );
                continue;
            }
        };
        known.insert(path.clone());
        let folder = path.parent().unwrap_or(Path::new("."));
        known.extend(meta.dependencies.iter().map(|d| folder.join(d)));
        if let Some(first) = ids.insert(meta.id, path.clone()) {
            issue(
                "ASSET_DUPLICATE_ID",
                "error",
                format!("ID {} is also used by `{}`", meta.id, first.display()),
                &meta_path(&path),
            );
        }
        let Ok(bytes) = std::fs::read(&path) else {
            issue(
                "ASSET_MISSING",
                "error",
                "the imported file is missing".into(),
                &path,
            );
            continue;
        };
        if scene_revision(&bytes) != meta.content_hash {
            issue(
                "ASSET_CHANGED",
                "warning",
                "changed since import; run `asset reimport` to record it"
                    .into(),
                &path,
            );
        }
        for dependency in &meta.dependencies {
            let file = folder.join(dependency);
            if !file.is_file() {
                issue(
                    "ASSET_DEPENDENCY_MISSING",
                    "error",
                    format!("needs `{}`", dependency.display()),
                    &file,
                );
            }
        }
        let size = validate(meta.kind, &path).unwrap_or_else(|error| {
            issue("ASSET_INVALID", "error", error.to_string(), &path);
            [0, 0]
        });
        assets.push((path, meta, size));
    }
    let mut folders = vec![project_root.join("assets")];
    while let Some(folder) = folders.pop() {
        for path in std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
        {
            if path.is_dir() {
                folders.push(path);
            } else if ImportedKind::from_path(&path).is_some()
                && !known.contains(&path)
            {
                issue(
                    "ASSET_NOT_IMPORTED",
                    "warning",
                    "has no .rmeta; import it to record an ID and license"
                        .into(),
                    &path,
                );
            }
        }
    }
    for (path, meta, size) in assets {
        let entry = report(project_root, &path, meta, size, false);
        if entry.source.license.is_none() {
            issue(
                "ASSET_NO_LICENSE",
                "warning",
                "no license recorded".into(),
                &path,
            );
        }
        catalog.assets.push(ImportReport {
            warnings: Vec::new(),
            ..entry
        });
    }
    catalog
        .issues
        .sort_by(|a, b| (&a.file, a.code).cmp(&(&b.file, b.code)));
    catalog
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> PathBuf {
        let root = std::env::temp_dir()
            .join(format!("rusting-asset-import-{}", Uuid::new_v4()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join("scenes")).unwrap();
        root
    }

    fn png(path: &Path, width: u32, height: u32, value: u8) {
        image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([value, 0, 0, 255]),
        )
        .save(path)
        .unwrap();
    }

    #[test]
    fn images_import_with_settings_provenance_and_a_stable_id_on_reimport() {
        let root = project();
        let source = root.join("incoming.png");
        png(&source, 64, 32, 10);
        let provenance = AssetProvenance {
            author: Some("Ada".into()),
            ..AssetProvenance::default()
        };
        let settings = ImportSettings { max_size: Some(16) };

        let imported = import_asset(
            &root,
            &source,
            Path::new("ui"),
            &provenance,
            &settings,
            false,
        )
        .unwrap();

        assert_eq!(imported.path, "assets/ui/incoming.png");
        // Found by the path `--to` took, relative to assets/, as well.
        for name in ["assets/ui/incoming.png", "ui/incoming.png"] {
            let found = find_asset(&root, name).unwrap();
            assert_eq!(found.1.id, imported.id, "{name}");
        }
        assert_eq!(imported.reference, "../assets/ui/incoming.png");
        assert_eq!(imported.size, [16, 8], "longest edge scaled to max_size");
        assert_eq!(imported.source.author.as_deref(), Some("Ada"));
        assert_eq!(
            imported.warnings.len(),
            1,
            "no license: {:?}",
            imported.warnings
        );
        let meta = read_meta(&meta_path(&root.join(&imported.path))).unwrap();
        assert_eq!(meta.id, imported.id);
        assert!(matches!(
            import_asset(
                &root,
                &source,
                Path::new("ui"),
                &provenance,
                &settings,
                false
            ),
            Err(AssetImportError::Exists(_))
        ));

        // A scene that uses it is reported.
        let scene = serde_json::json!({
            "format_version": crate::runtime::SCENE_FORMAT_VERSION,
            "name": "Main",
            "entities": [{
                "id": Uuid::new_v4(), "name": "Sign",
                "transform": {"position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0], "scale": [1.0, 1.0, 1.0]},
                "mesh_renderer": {"mesh": {"BuiltinPrimitive": "Quad"}, "material": {"Inline": {
                    "model": "Unlit", "base_color": [1.0, 1.0, 1.0, 1.0], "emissive": [0.0, 0.0, 0.0],
                    "metallic": 0.0, "roughness": 1.0,
                    "base_color_texture": imported.reference,
                    "normal_texture": null, "metallic_roughness_texture": null,
                    "occlusion_texture": null, "emissive_texture": null}},
                    "cast_shadows": false, "receive_shadows": false}
            }]
        });
        std::fs::write(root.join("scenes/main.rscene"), scene.to_string())
            .unwrap();
        read_scene_document(root.join("scenes/main.rscene")).unwrap();

        // Replace with new art: same ID, new size, license added.
        let replacement = root.join("better.png");
        png(&replacement, 8, 8, 200);
        let license = AssetProvenance {
            license: Some("CC0-1.0".into()),
            ..AssetProvenance::default()
        };
        let replaced = reimport_asset(
            &root,
            &imported.id.to_string(),
            Some(&replacement),
            &license,
            None,
            false,
        )
        .unwrap();
        assert_eq!(replaced.id, imported.id);
        assert_eq!(replaced.size, [8, 8]);
        assert_eq!(replaced.source.author.as_deref(), Some("Ada"));
        assert_eq!(replaced.source.license.as_deref(), Some("CC0-1.0"));
        assert!(replaced.source.original.ends_with("better.png"));
        assert_eq!(replaced.referenced_by, ["scenes/main.rscene"]);
        assert!(replaced.warnings.is_empty());
        let pixel = image::open(root.join(&replaced.path)).unwrap().to_rgba8();
        assert_eq!(pixel.get_pixel(0, 0)[0], 200);

        let catalog = list_assets(&root);
        assert_eq!(catalog.assets.len(), 1);
        assert!(catalog.issues.is_empty(), "{:?}", catalog.issues);

        // Edits outside import, stray files, and a copied sidecar are found.
        png(&root.join(&replaced.path), 8, 8, 99);
        png(&root.join("assets/stray.png"), 2, 2, 1);
        std::fs::copy(
            meta_path(&root.join(&replaced.path)),
            root.join("assets/stray.png.rmeta"),
        )
        .unwrap();
        let codes: Vec<_> = list_assets(&root)
            .issues
            .iter()
            .map(|issue| issue.code)
            .collect();
        assert_eq!(
            codes,
            ["ASSET_CHANGED", "ASSET_CHANGED", "ASSET_DUPLICATE_ID"]
        );

        assert!(matches!(
            import_asset(
                &root,
                &root.join("notes.txt"),
                Path::new(""),
                &provenance,
                &settings,
                false
            ),
            Err(AssetImportError::SourceMissing(_))
        ));
        std::fs::write(root.join("broken.png"), b"not a png").unwrap();
        assert_eq!(
            import_asset(
                &root,
                &root.join("broken.png"),
                Path::new(""),
                &provenance,
                &settings,
                false
            )
            .unwrap_err()
            .code(),
            "ASSET_INVALID"
        );
        assert!(!root.join("assets/broken.png").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A 16-bit mono WAV with `frames` silent samples.
    fn wav(rate: u32, frames: u32) -> Vec<u8> {
        let data = frames * 2;
        let mut bytes = b"RIFF".to_vec();
        bytes.extend((36 + data).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes()); // PCM
        bytes.extend(1u16.to_le_bytes()); // mono
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 2).to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(data.to_le_bytes());
        bytes.resize(bytes.len() + data as usize, 0);
        bytes
    }

    #[test]
    fn audio_imports_and_dry_runs_write_nothing() {
        let root = project();
        let source = root.join("hit.wav");
        std::fs::write(&source, wav(8000, 4000)).unwrap();
        let none = AssetProvenance::default();
        let settings = ImportSettings::default();

        let preview = import_asset(
            &root,
            &source,
            Path::new("sfx"),
            &none,
            &settings,
            true,
        )
        .unwrap();
        assert!(preview.dry_run);
        assert_eq!(preview.kind, ImportedKind::Audio);
        assert_eq!(preview.size, [8000, 500], "sample rate and milliseconds");
        assert_eq!(preview.channels, Some(1), "a preview reads the source");
        assert!(
            !root.join("assets/sfx").exists(),
            "a preview writes nothing"
        );

        let imported = import_asset(
            &root,
            &source,
            Path::new("sfx"),
            &none,
            &settings,
            false,
        )
        .unwrap();
        assert!(!imported.dry_run);
        assert_eq!(imported.channels, Some(1), "mono");
        // A folder written from the project root lands in the same place.
        let rooted = import_asset(
            &root,
            &source,
            Path::new("assets/music"),
            &none,
            &settings,
            false,
        )
        .unwrap();
        assert_eq!(rooted.path, "assets/music/hit.wav");
        std::fs::write(&source, wav(8000, 8000)).unwrap();
        let replace = reimport_asset(
            &root,
            "assets/sfx/hit.wav",
            None,
            &none,
            None,
            true,
        )
        .unwrap();
        assert_eq!((replace.id, replace.size), (imported.id, [8000, 1000]));
        assert_eq!(
            std::fs::read(root.join("assets/sfx/hit.wav"))
                .unwrap()
                .len(),
            44 + 8000,
            "the previewed replacement left the old file"
        );

        std::fs::write(root.join("bad.wav"), b"RIFF\0\0\0\0WAVE").unwrap();
        let bad = import_asset(
            &root,
            &root.join("bad.wav"),
            Path::new(""),
            &none,
            &settings,
            false,
        )
        .unwrap_err();
        assert_eq!(bad.code(), "ASSET_INVALID", "{bad}");
        std::fs::write(root.join("music.ogg"), b"OggS and more").unwrap();
        let ogg = import_asset(
            &root,
            &root.join("music.ogg"),
            Path::new(""),
            &none,
            &settings,
            true,
        )
        .unwrap();
        assert_eq!(ogg.size, [0, 0]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn scene_generators_cache_by_inputs_and_refuse_bad_output() {
        let root = project();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join("gen/rules.txt"), "v1").unwrap();
        // Writes a scene named after the seed and inputs, and counts runs.
        let script = r#"echo run >> runs.txt; echo "$RUSTING_INPUTS" > inputs.txt; printf '{"format_version": 1, "name": "level %s", "entities": []}' "$RUSTING_SEED" > "$RUSTING_OUTPUT""#;
        let generator = |script: &str| SceneGenerator {
            command: vec!["sh".into(), "-c".into(), script.into()],
            output: "scenes/level.rscene".into(),
            seed: 7,
            inputs: serde_json::json!({"rooms": 3}),
            watch: vec!["gen".into()],
            checks: Vec::new(),
        };
        let mut generators =
            BTreeMap::from([("level".to_owned(), generator(script))]);
        let runs = || {
            std::fs::read_to_string(root.join("runs.txt"))
                .unwrap_or_default()
                .lines()
                .count()
        };
        let generate = |generators: &BTreeMap<_, _>, force| {
            generate_scenes(
                &root,
                generators,
                &[],
                force,
                false,
                &mut |_, _| Ok(()),
            )
        };
        let scene = || {
            crate::runtime::read_scene_document(
                root.join("scenes/level.rscene"),
            )
            .unwrap()
            .name
        };

        let preview = generate_scenes(
            &root,
            &generators,
            &["level"],
            false,
            true,
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(preview[0].status, "generated");
        assert_eq!(runs(), 0, "a dry run runs nothing");
        let made = generate(&generators, false).unwrap();
        assert_eq!(made[0].status, "generated");
        assert_eq!(scene(), "level 7");
        assert_eq!(
            std::fs::read_to_string(root.join("inputs.txt")).unwrap(),
            "{\"rooms\":3}\n"
        );
        assert_eq!(generate(&generators, false).unwrap()[0].status, "cached");
        assert_eq!(runs(), 1);

        // A watched file, the seed, or the inputs changing runs it again.
        std::fs::write(root.join("gen/rules.txt"), "v2").unwrap();
        assert_eq!(
            generate(&generators, false).unwrap()[0].status,
            "generated"
        );
        generators.get_mut("level").unwrap().seed = 8;
        assert_eq!(
            generate(&generators, false).unwrap()[0].status,
            "generated"
        );
        assert_eq!(scene(), "level 8");
        assert_eq!(runs(), 3);
        assert_eq!(generate(&generators, true).unwrap()[0].status, "generated");
        assert_eq!(runs(), 4, "force runs it");

        // A hand edit is kept until forced over.
        std::fs::write(
            root.join("scenes/level.rscene"),
            r#"{"format_version": 1, "name": "edited", "entities": []}"#,
        )
        .unwrap();
        let edited = generate(&generators, false).unwrap_err();
        assert_eq!(edited.code(), "GENERATED_SCENE_EDITED");
        assert_eq!(scene(), "edited");
        generate(&generators, true).unwrap();
        assert_eq!(scene(), "level 8");

        // Output that is not a scene, or none, leaves the scene alone.
        for bad in ["echo nope > \"$RUSTING_OUTPUT\"", "true", "exit 3"] {
            let broken = BTreeMap::from([("level".to_owned(), generator(bad))]);
            let error = generate(&broken, true).unwrap_err();
            assert_eq!(error.code(), "GENERATOR_FAILED", "{bad}: {error}");
            assert_eq!(scene(), "level 8");
        }
        assert_eq!(
            generate_scenes(
                &root,
                &generators,
                &["missing"],
                false,
                false,
                &mut |_, _| Ok(())
            )
            .unwrap_err()
            .code(),
            "GENERATOR_UNKNOWN"
        );

        // A failed check keeps the old scene; a passing one is reported.
        generators.get_mut("level").unwrap().seed = 9;
        generators.get_mut("level").unwrap().checks =
            vec!["reach.scenario.json".into()];
        let mut seen = Vec::new();
        let error = generate_scenes(
            &root,
            &generators,
            &[],
            false,
            false,
            &mut |scenario, scene| {
                seen.push(
                    crate::runtime::read_scene_document(scene).unwrap().name,
                );
                assert_eq!(scenario, root.join("reach.scenario.json"));
                Err("goal 2 not reached".into())
            },
        )
        .unwrap_err();
        assert_eq!(error.code(), "GENERATED_SCENE_CHECK_FAILED");
        assert!(error.to_string().contains("goal 2 not reached"), "{error}");
        assert_eq!(seen, ["level 9"], "the check sees the new scene");
        assert_eq!(scene(), "level 8");
        let made = generate_scenes(
            &root,
            &generators,
            &[],
            false,
            false,
            &mut |_, _| Ok(()),
        )
        .unwrap();
        assert_eq!(made[0].checked, [PathBuf::from("reach.scenario.json")]);
        assert_eq!(scene(), "level 9");
        let leftovers = std::fs::read_dir(root.join("scenes")).unwrap().count();
        assert_eq!(leftovers, 1, "no staged file is left behind");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generator_hooks_import_and_replace_through_the_importer() {
        let root = project();
        let fixture = root.join("fixture.png");
        png(&fixture, 32, 32, 7);
        // The hook copies the fixture and names it after the prompt.
        let hook = |print: &str, license: Option<&str>| {
            GeneratorHook {
            command: vec![
                "sh".into(),
                "-c".into(),
                format!(
                    "cp \"$0\" \"$RUSTING_OUTPUT_DIR/$RUSTING_PROMPT.png\" && echo '{print}'"
                ),
                fixture.display().to_string(),
            ],
            license: license.map(Into::into),
            settings: ImportSettings { max_size: Some(16) },
        }
        };
        let hooks = BTreeMap::from([
            (
                "icons".to_owned(),
                hook(r#"{"file": "crate.png"}"#, Some("CC0-1.0")),
            ),
            (
                "unlicensed".to_owned(),
                hook(r#"{"file": "crate.png"}"#, None),
            ),
            ("quiet".to_owned(), hook("done", Some("CC0-1.0"))),
            (
                "broken".to_owned(),
                GeneratorHook {
                    command: vec![
                        "sh".into(),
                        "-c".into(),
                        "echo nope >&2; exit 3".into(),
                    ],
                    ..GeneratorHook::default()
                },
            ),
        ]);
        let folder = Path::new("props");
        let generate = |hook: &str, target, dry_run| {
            generate_asset(&root, &hooks, hook, "crate", target, dry_run)
        };

        let preview =
            generate("icons", GenerateTarget::Import { folder }, true).unwrap();
        assert!(preview.dry_run);
        assert!(!root.join("assets/props").exists());
        let made = generate("icons", GenerateTarget::Import { folder }, false)
            .unwrap();
        assert_eq!(made.path, "assets/props/crate.png");
        assert_eq!(made.size, [16, 16], "hook settings apply");
        assert_eq!(made.source.original, "generator:icons");
        assert_eq!(made.source.license.as_deref(), Some("CC0-1.0"));
        assert_eq!(made.source.generator.as_deref(), Some("icons"));
        assert_eq!(made.source.notes.as_deref(), Some("prompt: crate"));
        assert!(made.warnings.is_empty());

        let id = made.id.to_string();
        let replaced =
            generate("icons", GenerateTarget::Replace { asset: &id }, false)
                .unwrap();
        assert_eq!(replaced.id, made.id);
        assert!(list_assets(&root).issues.is_empty());

        let codes: Vec<_> = ["unlicensed", "quiet", "broken", "missing"]
            .map(|hook| {
                generate(hook, GenerateTarget::Import { folder }, true)
                    .unwrap_err()
                    .code()
            })
            .into();
        assert_eq!(
            codes,
            [
                "GENERATOR_FAILED",
                "GENERATOR_FAILED",
                "GENERATOR_FAILED",
                "GENERATOR_UNKNOWN"
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_builtin_sfx_generator_imports_a_seeded_cc0_wav() {
        let root = project();
        let hooks = BTreeMap::new();
        let folder = Path::new("sounds");
        let generate = |prompt| {
            generate_asset(
                &root,
                &hooks,
                "sfx",
                prompt,
                GenerateTarget::Import { folder },
                false,
            )
        };
        let made = generate("coin 7").unwrap();
        assert_eq!(made.path, "assets/sounds/coin_7.wav");
        assert_eq!(made.source.license.as_deref(), Some("CC0-1.0"));
        assert_eq!(made.source.notes.as_deref(), Some("preset coin, seed 7"));
        let first = std::fs::read(root.join(&made.path)).unwrap();
        std::fs::remove_dir_all(root.join("assets/sounds")).unwrap();
        generate("coin 7").unwrap();
        assert_eq!(std::fs::read(root.join(&made.path)).unwrap(), first);
        for bad in ["moo", "coin seven"] {
            assert_eq!(generate(bad).unwrap_err().code(), "GENERATOR_FAILED");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_builtin_texture_generator_imports_a_seeded_cc0_png() {
        let root = project();
        let generate = |prompt| {
            generate_asset(
                &root,
                &BTreeMap::new(),
                "texture",
                prompt,
                GenerateTarget::Import {
                    folder: Path::new("textures"),
                },
                false,
            )
        };
        let made = generate("bricks 3").unwrap();
        assert_eq!(made.path, "assets/textures/bricks_3.png");
        assert_eq!(made.kind, ImportedKind::Image);
        assert_eq!(made.size, [crate::texgen::SIZE, crate::texgen::SIZE]);
        assert_eq!(made.source.license.as_deref(), Some("CC0-1.0"));
        assert_eq!(made.source.notes.as_deref(), Some("preset bricks, seed 3"));
        let star = generate_asset(
            &root,
            &BTreeMap::new(),
            "sprite",
            "star 2",
            GenerateTarget::Import {
                folder: Path::new("sprites"),
            },
            false,
        )
        .unwrap();
        assert_eq!(star.path, "assets/sprites/star_2.png");
        assert_eq!(star.size, [crate::texgen::SPRITE_SIZE; 2]);
        #[cfg(feature = "gltf")]
        {
            let barrel = generate_asset(
                &root,
                &BTreeMap::new(),
                "mesh",
                "barrel 4",
                GenerateTarget::Import {
                    folder: Path::new("models"),
                },
                false,
            )
            .unwrap();
            assert_eq!(barrel.path, "assets/models/barrel_4.glb");
            assert_eq!(barrel.kind, ImportedKind::Gltf);
            assert_eq!(barrel.source.license.as_deref(), Some("CC0-1.0"));
        }
        for bad in ["marble", "bricks three"] {
            assert_eq!(generate(bad).unwrap_err().code(), "GENERATOR_FAILED");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "gltf")]
    #[test]
    fn gltf_import_copies_and_reports_dependencies() {
        let root = project();
        let incoming = root.join("incoming");
        std::fs::create_dir_all(incoming.join("textures")).unwrap();
        png(&incoming.join("textures/pixel.png"), 1, 1, 5);
        let positions: Vec<u8> =
            [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
        std::fs::write(incoming.join("tri.bin"), &positions).unwrap();
        let gltf = incoming.join("tri.gltf");
        let json = |uri: &str| {
            format!(
                r#"{{"asset": {{"version": "2.0"}},
                  "buffers": [{{"uri": "{uri}", "byteLength": 36}}],
                  "bufferViews": [{{"buffer": 0, "byteLength": 36}}],
                  "accessors": [{{"bufferView": 0, "componentType": 5126, "count": 3,
                    "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}}],
                  "images": [{{"uri": "textures/pixel.png"}}],
                  "textures": [{{"source": 0}}],
                  "materials": [{{"pbrMetallicRoughness": {{"baseColorTexture": {{"index": 0}}}}}}],
                  "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0}}, "material": 0}}]}}]}}"#
            )
        };
        std::fs::write(&gltf, json("tri.bin")).unwrap();

        let imported = import_asset(
            &root,
            &gltf,
            Path::new("models"),
            &AssetProvenance {
                license: Some("CC-BY-4.0".into()),
                ..AssetProvenance::default()
            },
            &ImportSettings::default(),
            false,
        )
        .unwrap();

        assert_eq!(imported.kind, ImportedKind::Gltf);
        assert_eq!(imported.size, [1, 0]);
        assert_eq!(
            imported.dependencies,
            [
                PathBuf::from("textures/pixel.png"),
                PathBuf::from("tri.bin")
            ]
        );
        assert!(root.join("assets/models/textures/pixel.png").is_file());
        assert!(list_assets(&root).issues.is_empty());

        std::fs::remove_file(root.join("assets/models/tri.bin")).unwrap();
        let codes: Vec<_> = list_assets(&root)
            .issues
            .iter()
            .map(|issue| issue.code)
            .collect();
        assert_eq!(codes, ["ASSET_DEPENDENCY_MISSING", "ASSET_INVALID"]);

        std::fs::write(&gltf, json("../outside.bin")).unwrap();
        let escaped = reimport_asset(
            &root,
            "assets/models/tri.gltf",
            None,
            &AssetProvenance::default(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(escaped.code(), "ASSET_INVALID", "{escaped}");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "gltf")]
    #[test]
    fn a_model_without_metadata_is_registered_by_reimport() {
        let root = project();
        let models = root.join("assets/models");
        std::fs::create_dir_all(&models).unwrap();
        let positions: Vec<u8> =
            [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
        std::fs::write(models.join("tri.bin"), &positions).unwrap();
        std::fs::write(
            models.join("tri.gltf"),
            r#"{"asset": {"version": "2.0"},
              "buffers": [{"uri": "tri.bin", "byteLength": 36}],
              "bufferViews": [{"buffer": 0, "byteLength": 36}],
              "accessors": [{"bufferView": 0, "componentType": 5126, "count": 3,
                "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]}],
              "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}]}"#,
        )
        .unwrap();

        let report = reimport_asset(
            &root,
            "assets/models/tri.gltf",
            None,
            &AssetProvenance::default(),
            None,
            false,
        )
        .unwrap();

        assert_eq!(report.kind, ImportedKind::Gltf);
        assert_eq!(report.dependencies, [PathBuf::from("tri.bin")]);
        assert!(models.join("tri.gltf.rmeta").is_file());
        let codes: Vec<_> = list_assets(&root)
            .issues
            .iter()
            .map(|issue| issue.code)
            .collect();
        assert_eq!(codes, ["ASSET_NO_LICENSE"]);
        let missing = reimport_asset(
            &root,
            "assets/models/none.gltf",
            None,
            &AssetProvenance::default(),
            None,
            false,
        )
        .unwrap_err();
        assert_eq!(missing.code(), "ASSET_NOT_FOUND");
        std::fs::remove_dir_all(root).unwrap();
    }
}
