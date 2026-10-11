//! Typed, generational CPU asset storage.

use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::marker::PhantomData;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use bevy_ecs::prelude::{Local, Res, ResMut, Resource};
use serde::{Deserialize, Serialize};

use crate::runtime::{
    App, AppError, Camera, DirectionalLight, MeshRenderer, Name, Plugin,
    PointLight, ScheduleStage, SpotLight,
};

mod data;

pub use data::{
    read_data_file, write_data_file, DataAsset, DataAssetType, DataAssetTypes,
    DataAssets, DATA_EXTENSION,
};

/// A compact typed asset identity. Reused slots receive a new generation, so
/// stale handles can never resolve to unrelated assets.
pub struct Handle<T> {
    index: u32,
    generation: u32,
    marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    #[must_use]
    pub const fn generation(self) -> u32 {
        self.generation
    }

    #[must_use]
    pub const fn key(self) -> u64 {
        ((self.generation as u64) << 32) | self.index as u64
    }

    /// The handle whose [`key`](Self::key) is `key`.
    #[must_use]
    pub const fn from_key(key: u64) -> Self {
        Self {
            index: key as u32,
            generation: (key >> 32) as u32,
            marker: PhantomData,
        }
    }
}

/// Serializes as the [`key`](Handle::key), which is only valid in the same
/// `AssetServer`. Scenes save reflected handles as asset paths instead.
impl<T> Serialize for Handle<T> {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.key())
    }
}

impl<'de, T> Deserialize<'de> for Handle<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Self, D::Error> {
        u64::deserialize(deserializer).map(Self::from_key)
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> Debug for Handle<T> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Handle")
            .field("index", &self.index)
            .field("generation", &self.generation)
            .finish()
    }
}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<T> Eq for Handle<T> {}

impl<T> std::hash::Hash for Handle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssetError {
    EmptyPath,
    Missing(AssetKey),
    StillReferenced { key: AssetKey, references: u32 },
    Load { path: PathBuf, message: String },
}

impl Display for AssetError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPath => {
                formatter.write_str("asset path cannot be empty")
            }
            Self::Missing(key) => {
                write!(formatter, "asset {key:?} is missing or stale")
            }
            Self::StillReferenced { key, references } => {
                write!(
                    formatter,
                    "asset {key:?} still has {references} references"
                )
            }
            Self::Load { path, message } => {
                write!(
                    formatter,
                    "failed to load `{}`: {message}",
                    path.display()
                )
            }
        }
    }
}

impl Error for AssetError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AssetKey {
    pub index: u32,
    pub generation: u32,
}

impl<T> From<Handle<T>> for AssetKey {
    fn from(handle: Handle<T>) -> Self {
        Self {
            index: handle.index,
            generation: handle.generation,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadState {
    /// A worker is decoding the asset; `Assets::poll_loads` publishes it.
    Loading,
    Loaded,
    Failed(String),
}

struct Slot<T> {
    generation: u32,
    revision: u64,
    value: Option<T>,
    path: Option<PathBuf>,
    references: u32,
    state: LoadState,
    /// Source file modification time last seen by `changed`.
    modified: Option<SystemTime>,
    /// A worker is decoding a replacement; the old value stays visible.
    reloading: bool,
}

/// Storage for one asset type, including path deduplication and deferred drops.
pub struct Assets<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    paths: HashMap<PathBuf, Handle<T>>,
    deferred: Vec<(u64, T)>,
    /// Results finished by worker threads, drained by `poll_loads`.
    finished: Arc<Mutex<FinishedLoads<T>>>,
    /// Hot reloads that failed to decode, drained by `take_reload_failures`.
    reload_failures: Vec<AssetError>,
    /// Paths whose hot reload published a new value, drained by
    /// `take_reloaded`.
    reloaded: Vec<PathBuf>,
}

type FinishedLoads<T> = Vec<(AssetKey, Result<T, AssetError>)>;

impl<T> Default for Assets<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            paths: HashMap::new(),
            deferred: Vec::new(),
            finished: Arc::default(),
            reload_failures: Vec::new(),
            reloaded: Vec::new(),
        }
    }
}

impl<T> Assets<T> {
    pub fn insert(&mut self, value: T) -> Handle<T> {
        self.insert_slot(Some(value), None, LoadState::Loaded)
    }

    /// Inserts a prepared asset with a source path, or returns the asset that
    /// was already imported from the same path.
    pub fn insert_with_path(
        &mut self,
        path: impl AsRef<Path>,
        value: T,
    ) -> Result<Handle<T>, AssetError> {
        let path = normalize_path(path.as_ref())?;
        if let Some(handle) = self.paths.get(&path).copied() {
            if self.contains(handle) {
                return Ok(handle);
            }
            self.paths.remove(&path);
        }
        Ok(self.insert_slot(Some(value), Some(path), LoadState::Loaded))
    }

    pub fn get_or_insert_with(
        &mut self,
        path: impl AsRef<Path>,
        loader: impl FnOnce(&Path) -> Result<T, AssetError>,
    ) -> Result<Handle<T>, AssetError> {
        let path = normalize_path(path.as_ref())?;
        if let Some(handle) = self.paths.get(&path).copied() {
            if self.contains(handle) {
                return Ok(handle);
            }
            self.paths.remove(&path);
        }
        let value = loader(&path).map_err(|error| AssetError::Load {
            path: path.clone(),
            message: error.to_string(),
        })?;
        Ok(self.insert_slot(Some(value), Some(path), LoadState::Loaded))
    }

    #[must_use]
    pub fn get(&self, handle: Handle<T>) -> Option<&T> {
        self.slot(handle).and_then(|slot| slot.value.as_ref())
    }

    pub fn get_mut(&mut self, handle: Handle<T>) -> Option<&mut T> {
        let slot = self.slot_mut(handle)?;
        let value = slot.value.as_mut()?;
        slot.revision = slot.revision.saturating_add(1);
        Some(value)
    }

    #[must_use]
    pub fn contains(&self, handle: Handle<T>) -> bool {
        self.get(handle).is_some()
    }

    #[must_use]
    pub fn load_state(&self, handle: Handle<T>) -> Option<&LoadState> {
        self.slot(handle).map(|slot| &slot.state)
    }

    #[must_use]
    pub fn path(&self, handle: Handle<T>) -> Option<&Path> {
        self.slot(handle).and_then(|slot| slot.path.as_deref())
    }

    /// Resolves an already-loaded asset by its normalized source path.
    #[must_use]
    pub fn handle_for_path(&self, path: impl AsRef<Path>) -> Option<Handle<T>> {
        let path = normalize_path(path.as_ref()).ok()?;
        self.paths
            .get(&path)
            .copied()
            .filter(|handle| self.contains(*handle))
    }

    #[must_use]
    pub fn revision(&self, handle: Handle<T>) -> Option<u64> {
        self.slot(handle).map(|slot| slot.revision)
    }

    pub fn retain(&mut self, handle: Handle<T>) -> Result<(), AssetError> {
        let slot = self
            .slot_mut(handle)
            .ok_or_else(|| AssetError::Missing(handle.into()))?;
        slot.references = slot.references.saturating_add(1);
        Ok(())
    }

    pub fn release(&mut self, handle: Handle<T>) -> Result<(), AssetError> {
        let slot = self
            .slot_mut(handle)
            .ok_or_else(|| AssetError::Missing(handle.into()))?;
        slot.references = slot.references.saturating_sub(1);
        Ok(())
    }

    pub fn remove(&mut self, handle: Handle<T>) -> Result<T, AssetError> {
        let slot = self
            .slot_mut(handle)
            .ok_or_else(|| AssetError::Missing(handle.into()))?;
        if slot.references > 0 {
            return Err(AssetError::StillReferenced {
                key: handle.into(),
                references: slot.references,
            });
        }
        // Removing a loading or failed asset frees its slot; a late worker
        // result for it is discarded by `poll_loads`.
        let path = slot.path.take();
        let value = slot.value.take();
        slot.state = LoadState::Loaded;
        // A hot reload still in flight must not publish into the freed slot.
        slot.reloading = false;
        if let Some(path) = path {
            self.paths.remove(&path);
        }
        // A slot whose generation would wrap is never reused, so a handle
        // from its first use cannot resolve again.
        if handle.generation < u32::MAX {
            self.free.push(handle.index);
        }
        value.ok_or_else(|| AssetError::Missing(handle.into()))
    }

    /// Publishes every worker result that finished since the last poll and
    /// returns how many handles changed state.
    pub fn poll_loads(&mut self) -> usize {
        let finished = std::mem::take(
            &mut *self.finished.lock().unwrap_or_else(PoisonError::into_inner),
        );
        let mut published = 0;
        for (key, result) in finished {
            let Some(slot) = self.slots.get_mut(key.index as usize) else {
                continue;
            };
            if slot.generation != key.generation
                || (slot.state != LoadState::Loading && !slot.reloading)
            {
                continue;
            }
            let reloading = std::mem::take(&mut slot.reloading);
            match result {
                Ok(value) => {
                    slot.value = Some(value);
                    slot.state = LoadState::Loaded;
                    if reloading {
                        self.reloaded.extend(slot.path.clone());
                    }
                }
                // A failed hot reload keeps serving the last good value.
                Err(error) if reloading => {
                    self.reload_failures.push(error);
                    continue;
                }
                Err(error) => slot.state = LoadState::Failed(error.to_string()),
            }
            slot.revision = slot.revision.saturating_add(1);
            published += 1;
        }
        published
    }

    /// Returns path-backed assets whose file modification time changed since
    /// they were loaded or last reported. Missing files are ignored so an
    /// editor's save-by-rename never drops the current value.
    // ponytail: stats every watched file per call; switch to OS file
    // notifications if projects grow to many thousands of assets.
    pub fn changed(&mut self) -> Vec<(Handle<T>, PathBuf)> {
        let mut changed = Vec::new();
        for (index, slot) in self.slots.iter_mut().enumerate() {
            let Some(path) = slot.path.as_ref() else {
                continue;
            };
            if !slot.is_live() || slot.reloading {
                continue;
            }
            let modified = file_modified(path);
            if modified.is_some() && modified != slot.modified {
                slot.modified = modified;
                changed.push((
                    Handle {
                        index: index as u32,
                        generation: slot.generation,
                        marker: PhantomData,
                    },
                    path.clone(),
                ));
            }
        }
        changed
    }

    /// Drains hot reloads whose decode failed since the last call.
    pub fn take_reload_failures(&mut self) -> Vec<AssetError> {
        std::mem::take(&mut self.reload_failures)
    }

    /// Drains the paths of hot reloads that published since the last call.
    pub fn take_reloaded(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.reloaded)
    }

    pub fn retire(
        &mut self,
        handle: Handle<T>,
        safe_after_frame: u64,
    ) -> Result<(), AssetError> {
        let value = self.remove(handle)?;
        self.deferred.push((safe_after_frame, value));
        Ok(())
    }

    pub fn collect_retired(&mut self, completed_frame: u64) -> usize {
        let before = self.deferred.len();
        self.deferred.retain(|(safe_after_frame, _)| {
            *safe_after_frame > completed_frame
        });
        before - self.deferred.len()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.value.is_some())
            .count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = (Handle<T>, &T)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            slot.value.as_ref().map(|value| {
                (
                    Handle {
                        index: index as u32,
                        generation: slot.generation,
                        marker: PhantomData,
                    },
                    value,
                )
            })
        })
    }

    pub fn paths(&self) -> impl Iterator<Item = (Handle<T>, &Path)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            Some((
                Handle {
                    index: index as u32,
                    generation: slot.generation,
                    marker: PhantomData,
                },
                slot.path.as_deref()?,
            ))
        })
    }

    fn insert_slot(
        &mut self,
        value: Option<T>,
        path: Option<PathBuf>,
        state: LoadState,
    ) -> Handle<T> {
        let modified = path.as_deref().and_then(file_modified);
        let handle = if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.generation += 1;
            slot.revision = slot.revision.saturating_add(1);
            slot.value = value;
            slot.path = path.clone();
            slot.references = 0;
            slot.state = state;
            slot.modified = modified;
            slot.reloading = false;
            Handle {
                index,
                generation: slot.generation,
                marker: PhantomData,
            }
        } else {
            let index = u32::try_from(self.slots.len())
                .expect("asset slot count exceeds u32");
            self.slots.push(Slot {
                generation: 1,
                revision: 1,
                value,
                path: path.clone(),
                references: 0,
                state,
                modified,
                reloading: false,
            });
            Handle {
                index,
                generation: 1,
                marker: PhantomData,
            }
        };
        if let Some(path) = path {
            self.paths.insert(path, handle);
        }
        handle
    }

    fn slot(&self, handle: Handle<T>) -> Option<&Slot<T>> {
        self.slots.get(handle.index as usize).filter(|slot| {
            slot.generation == handle.generation && slot.is_live()
        })
    }

    fn slot_mut(&mut self, handle: Handle<T>) -> Option<&mut Slot<T>> {
        self.slots.get_mut(handle.index as usize).filter(|slot| {
            slot.generation == handle.generation && slot.is_live()
        })
    }
}

impl<T: Send + 'static> Assets<T> {
    /// Starts decoding `path` on a worker thread and returns a handle in
    /// `LoadState::Loading`. The loader must only produce CPU data; GPU
    /// preparation stays on the main thread. Requests for a path that is
    /// already loading or loaded return the existing handle.
    pub fn load_async(
        &mut self,
        path: impl AsRef<Path>,
        loader: impl FnOnce(&Path) -> Result<T, AssetError> + Send + 'static,
    ) -> Result<Handle<T>, AssetError> {
        let path = normalize_path(path.as_ref())?;
        if let Some(handle) = self.paths.get(&path).copied() {
            if self
                .slot(handle)
                .is_some_and(|slot| !matches!(slot.state, LoadState::Failed(_)))
            {
                return Ok(handle);
            }
            self.paths.remove(&path);
        }
        let handle =
            self.insert_slot(None, Some(path.clone()), LoadState::Loading);
        self.spawn_load(handle.into(), path, loader);
        Ok(handle)
    }

    /// Decodes a replacement for a path-backed asset on a worker thread.
    /// The current value stays visible until `poll_loads` publishes the new
    /// one; a failed decode keeps it and is reported by
    /// `take_reload_failures`.
    pub fn reload_async(
        &mut self,
        handle: Handle<T>,
        loader: impl FnOnce(&Path) -> Result<T, AssetError> + Send + 'static,
    ) -> Result<(), AssetError> {
        let slot = self
            .slot_mut(handle)
            .ok_or_else(|| AssetError::Missing(handle.into()))?;
        let path = slot.path.clone().ok_or(AssetError::EmptyPath)?;
        if slot.state == LoadState::Loading || slot.reloading {
            return Ok(());
        }
        slot.reloading = true;
        self.spawn_load(handle.into(), path, loader);
        Ok(())
    }

    fn spawn_load(
        &self,
        key: AssetKey,
        path: PathBuf,
        loader: impl FnOnce(&Path) -> Result<T, AssetError> + Send + 'static,
    ) {
        let finished = Arc::clone(&self.finished);
        // Rayon's pool bounds concurrent decodes to the core count.
        rayon::spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| loader(&path)))
                .unwrap_or_else(|_| {
                    Err(AssetError::Load {
                        path: path.clone(),
                        message: "loader panicked".to_owned(),
                    })
                })
                .map_err(|error| AssetError::Load {
                    path: path.clone(),
                    message: error.to_string(),
                });
            finished
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((key, result));
        });
    }
}

fn read_asset_file(path: &Path) -> Result<Vec<u8>, AssetError> {
    std::fs::read(path).map_err(|error| AssetError::Load {
        path: path.to_owned(),
        message: error.to_string(),
    })
}

/// `bincode::deserialize` with its input length as the size limit, so a
/// corrupt length prefix fails instead of allocating gigabytes.
pub(crate) fn deserialize_bounded<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
) -> bincode::Result<T> {
    use bincode::Options;
    bincode::options()
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .with_limit(bytes.len() as u64)
        .deserialize(bytes)
}

fn decode_cooked<T: serde::de::DeserializeOwned>(
    path: &Path,
) -> Result<T, AssetError> {
    deserialize_bounded(&read_asset_file(path)?).map_err(|error| {
        AssetError::Load {
            path: path.to_owned(),
            message: error.to_string(),
        }
    })
}

fn decode_mesh_file(path: &Path) -> Result<MeshAsset, AssetError> {
    let bytes = read_asset_file(path)?;
    let mesh = match bytes.strip_prefix(MESH_FILE_MAGIC) {
        Some(body) => deserialize_bounded(body),
        None => deserialize_bounded(&bytes).map(|legacy: LegacyMeshAsset| {
            MeshAsset {
                vertices: legacy
                    .vertices
                    .into_iter()
                    .map(|vertex| MeshVertex {
                        position: vertex.position,
                        normal: vertex.normal,
                        uv: vertex.uv,
                        tangent: vertex.tangent,
                        ..MeshVertex::default()
                    })
                    .collect(),
                indices: legacy.indices,
            }
        }),
    }
    .map_err(|error| AssetError::Load {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    check_mesh_indices(path, &mesh.indices, mesh.vertices.len())?;
    Ok(mesh)
}

/// Rejects indices past the vertex list. The renderer draws them unchecked
/// and without `robustBufferAccess`, so one bad index can lose the device.
fn check_mesh_indices(
    path: &Path,
    indices: &[u32],
    vertex_count: usize,
) -> Result<(), AssetError> {
    match indices
        .iter()
        .find(|index| **index as usize >= vertex_count)
    {
        Some(index) => Err(AssetError::Load {
            path: path.to_owned(),
            message: format!(
                "mesh index {index} is out of range for {vertex_count} vertices"
            ),
        }),
        None => Ok(()),
    }
}

/// Decodes a cooked `.rtexture`, or any image file as an sRGB texture.
fn decode_texture_file(path: &Path) -> Result<TextureAsset, AssetError> {
    if path
        .extension()
        .is_some_and(|extension| extension == "rtexture")
    {
        return decode_cooked(path);
    }
    let image = image::open(path).map_err(|error| AssetError::Load {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    let rgba = image.to_rgba8();
    Ok(TextureAsset {
        size: [rgba.width(), rgba.height()],
        rgba8: rgba.into_raw(),
        color_space: TextureColorSpace::Srgb,
        sampler: TextureSampler::default(),
    })
}

fn file_modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

impl<T> Slot<T> {
    /// Loaded, loading, and failed slots are addressable; freed ones are not.
    fn is_live(&self) -> bool {
        self.value.is_some() || self.state != LoadState::Loaded
    }
}

/// Fills per-vertex tangents from triangle UV gradients (Lengyel's method).
/// `w` holds bitangent handedness. Vertices without usable UVs get an
/// arbitrary unit tangent perpendicular to their normal.
pub fn generate_tangents(vertices: &mut [MeshVertex], indices: &[u32]) {
    use nalgebra::Vector3;
    let mut tangents = vec![Vector3::<f32>::zeros(); vertices.len()];
    let mut bitangents = tangents.clone();
    for triangle in indices.chunks_exact(3) {
        let [a, b, c] = [0, 1, 2].map(|i| triangle[i] as usize);
        if a.max(b).max(c) >= vertices.len() {
            continue;
        }
        let position = |i: usize| Vector3::from(vertices[i].position);
        let edge1 = position(b) - position(a);
        let edge2 = position(c) - position(a);
        let [u0, v0] = vertices[a].uv;
        let (du1, dv1) = (vertices[b].uv[0] - u0, vertices[b].uv[1] - v0);
        let (du2, dv2) = (vertices[c].uv[0] - u0, vertices[c].uv[1] - v0);
        let determinant = du1 * dv2 - du2 * dv1;
        if determinant.abs() <= f32::EPSILON {
            continue;
        }
        let tangent = (edge1 * dv2 - edge2 * dv1) / determinant;
        let bitangent = (edge2 * du1 - edge1 * du2) / determinant;
        for i in [a, b, c] {
            tangents[i] += tangent;
            bitangents[i] += bitangent;
        }
    }
    for (index, vertex) in vertices.iter_mut().enumerate() {
        let normal = Vector3::from(vertex.normal)
            .try_normalize(f32::EPSILON)
            .unwrap_or_else(Vector3::y);
        let orthogonal = |t: Vector3<f32>| {
            (t - normal * normal.dot(&t)).try_normalize(1.0e-6)
        };
        let tangent = orthogonal(tangents[index]).unwrap_or_else(|| {
            let axis = if normal.x.abs() < 0.9 {
                Vector3::x()
            } else {
                Vector3::y()
            };
            orthogonal(axis).expect("axis is not parallel to the normal")
        });
        let handedness = if normal.cross(&tangent).dot(&bitangents[index]) < 0.0
        {
            -1.0
        } else {
            1.0
        };
        vertex.tangent = [tangent.x, tangent.y, tangent.z, handedness];
    }
}

/// Maps glTF sampler state; unspecified filters keep the linear default.
#[cfg(feature = "gltf")]
fn gltf_texture_sampler(sampler: &gltf::texture::Sampler) -> TextureSampler {
    use gltf::texture::{MagFilter, MinFilter, WrappingMode};
    use TextureFilter::{Linear, Nearest};
    let wrap = |mode| match mode {
        WrappingMode::Repeat => TextureWrap::Repeat,
        WrappingMode::MirroredRepeat => TextureWrap::MirroredRepeat,
        WrappingMode::ClampToEdge => TextureWrap::ClampToEdge,
    };
    let (min_filter, mipmap_filter) = match sampler.min_filter() {
        Some(MinFilter::Nearest | MinFilter::NearestMipmapNearest) => {
            (Nearest, Nearest)
        }
        Some(MinFilter::NearestMipmapLinear) => (Nearest, Linear),
        Some(MinFilter::LinearMipmapNearest) => (Linear, Nearest),
        Some(MinFilter::Linear | MinFilter::LinearMipmapLinear) | None => {
            (Linear, Linear)
        }
    };
    TextureSampler {
        mag_filter: match sampler.mag_filter() {
            Some(MagFilter::Nearest) => Nearest,
            Some(MagFilter::Linear) | None => Linear,
        },
        min_filter,
        mipmap_filter,
        wrap: [wrap(sampler.wrap_s()), wrap(sampler.wrap_t())],
    }
}

/// Converts a decoded glTF image into tightly packed RGBA8, expanding
/// whichever channel layout the source used. 16-bit and float glTF image
/// formats are rare (most exporters emit 8-bit PNG/JPEG); they are
/// downsampled to 8 bits rather than rejected.
#[cfg(feature = "gltf")]
fn gltf_image_to_rgba8(image: &gltf::image::Data) -> Vec<u8> {
    use gltf::image::Format;
    let pixel_count = (image.width as usize) * (image.height as usize);
    let mut rgba8 = Vec::with_capacity(pixel_count * 4);
    match image.format {
        Format::R8 => {
            for &r in &image.pixels {
                rgba8.extend_from_slice(&[r, r, r, 255]);
            }
        }
        Format::R8G8 => {
            for chunk in image.pixels.chunks_exact(2) {
                rgba8.extend_from_slice(&[chunk[0], chunk[1], 0, 255]);
            }
        }
        Format::R8G8B8 => {
            for chunk in image.pixels.chunks_exact(3) {
                rgba8.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        Format::R8G8B8A8 => rgba8.extend_from_slice(&image.pixels),
        Format::R16 => {
            for chunk in image.pixels.chunks_exact(2) {
                let v = chunk[1];
                rgba8.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Format::R16G16 => {
            for chunk in image.pixels.chunks_exact(4) {
                rgba8.extend_from_slice(&[chunk[1], chunk[3], 0, 255]);
            }
        }
        Format::R16G16B16 => {
            for chunk in image.pixels.chunks_exact(6) {
                rgba8.extend_from_slice(&[chunk[1], chunk[3], chunk[5], 255]);
            }
        }
        Format::R16G16B16A16 => {
            for chunk in image.pixels.chunks_exact(8) {
                rgba8.extend_from_slice(&[
                    chunk[1], chunk[3], chunk[5], chunk[7],
                ]);
            }
        }
        Format::R32G32B32FLOAT => {
            for chunk in image.pixels.chunks_exact(12) {
                let channel = |bytes: &[u8]| {
                    (f32::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3],
                    ])
                    .clamp(0.0, 1.0)
                        * 255.0) as u8
                };
                rgba8.extend_from_slice(&[
                    channel(&chunk[0..4]),
                    channel(&chunk[4..8]),
                    channel(&chunk[8..12]),
                    255,
                ]);
            }
        }
        Format::R32G32B32A32FLOAT => {
            for chunk in image.pixels.chunks_exact(16) {
                let channel = |bytes: &[u8]| {
                    (f32::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3],
                    ])
                    .clamp(0.0, 1.0)
                        * 255.0) as u8
                };
                rgba8.extend_from_slice(&[
                    channel(&chunk[0..4]),
                    channel(&chunk[4..8]),
                    channel(&chunk[8..12]),
                    channel(&chunk[12..16]),
                ]);
            }
        }
    }
    rgba8
}

fn normalize_path(path: &Path) -> Result<PathBuf, AssetError> {
    if path.as_os_str().is_empty() {
        return Err(AssetError::EmptyPath);
    }
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Ok(canonical);
    }

    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| AssetError::Load {
                path: path.to_path_buf(),
                message: error.to_string(),
            })?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub tangent: [f32; 4],
    /// Linear RGBA multiplied into the material's base color; white
    /// leaves the material as it is.
    pub color: [f32; 4],
}

impl Default for MeshVertex {
    fn default() -> Self {
        Self {
            position: [0.0; 3],
            normal: [0.0; 3],
            uv: [0.0; 2],
            tangent: [0.0; 4],
            color: [1.0; 4],
        }
    }
}

/// A vertex as `.rmesh` files stored it before vertex colors.
#[derive(Deserialize)]
struct LegacyMeshVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
}

#[derive(Deserialize)]
struct LegacyMeshAsset {
    vertices: Vec<LegacyMeshVertex>,
    indices: Vec<u32>,
}

/// Starts every `.rmesh` written since vertex colors. Read as a legacy
/// file's vertex count it is trillions, so the two layouts never mix up.
const MESH_FILE_MAGIC: &[u8; 8] = b"RMESH\0\0\x02";

/// Encodes a mesh as a `.rmesh` file.
pub fn encode_mesh_file(mesh: &MeshAsset) -> bincode::Result<Vec<u8>> {
    let mut bytes = MESH_FILE_MAGIC.to_vec();
    bytes.extend(bincode::serialize(mesh)?);
    Ok(bytes)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MeshAsset {
    pub vertices: Vec<MeshVertex>,
    pub indices: Vec<u32>,
}

/// Up to four joints and weights per vertex of a skinned glTF mesh, cooked
/// next to its `.rmesh` as `.rskin`. Joint numbers index the `rusting.skin`
/// joint list of the entity that renders the mesh.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkinWeights {
    pub joints: Vec<[u16; 4]>,
    pub weights: Vec<[f32; 4]>,
}

/// Blend shape offsets of a glTF mesh, cooked next to its `.rmesh` as
/// `.rmorph`: per target, one position and one normal offset per vertex.
/// The `rusting.morph` weights scale each target.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MorphTargets {
    pub positions: Vec<Vec<[f32; 3]>>,
    /// Empty for a target without normal offsets, and for flat-shaded
    /// meshes, which keep their face normals.
    pub normals: Vec<Vec<[f32; 3]>>,
}

/// What a LOD group measures to pick a level.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
pub enum LodMetric {
    /// World distance from the camera to the bounds' center.
    Distance,
    /// Fraction of the view height the bounding sphere's diameter covers,
    /// which tracks projected error across fields of view.
    #[default]
    ScreenSize,
}

/// One mesh of a [`LodGroupAsset`] and where it hands over to the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodLevel {
    pub mesh: Handle<MeshAsset>,
    /// `Distance`: the level draws while the distance is below this.
    /// `ScreenSize`: it draws while the screen size is at least this.
    /// Past the last level's value the object is not drawn; use infinity
    /// or `0.0` to draw it at any range.
    pub until: f32,
}

/// Most levels a LOD group can have: one instanced batch picks between them
/// per instance.
pub const MAX_LOD_LEVELS: usize = 4;

/// Coarser stand-ins for one mesh. Every renderer drawing `levels[0].mesh`
/// picks one level per object and frame instead.
#[derive(Clone, Debug, PartialEq)]
pub struct LodGroupAsset {
    pub metric: LodMetric,
    /// Finest first.
    pub levels: Vec<LodLevel>,
}

impl LodGroupAsset {
    /// Each level's `[start, end)` range of the metric turned into a value
    /// that grows with distance: the distance itself, or the inverse screen
    /// size.
    #[must_use]
    pub fn ranges(&self) -> Vec<[f32; 2]> {
        let mut start = 0.0;
        self.levels
            .iter()
            .map(|level| {
                let end = match self.metric {
                    LodMetric::Distance => level.until,
                    LodMetric::ScreenSize => 1.0 / level.until,
                };
                let range = [start, end];
                start = end;
                range
            })
            .collect()
    }
}

/// `.rlod` file: a [`LodGroupAsset`] whose meshes are paths relative to it.
#[derive(Deserialize)]
struct LodGroupFile {
    #[serde(default)]
    metric: LodMetric,
    levels: Vec<LodLevelFile>,
}

#[derive(Deserialize)]
struct LodLevelFile {
    mesh: PathBuf,
    /// Left out: the level draws at any range.
    until: Option<f32>,
}

/// Procedural meshes shipped with the engine and available in Add Object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrimitiveShape {
    Cube,
    Sphere,
    Triangle,
    Plane,
    Tetrahedron,
    Octahedron,
    Dodecahedron,
    Icosahedron,
    Pyramid,
    Cylinder,
    Cone,
    Torus,
    /// Upright 1×1 square in the XY plane facing +Z, with the whole texture
    /// mapped upright: the sprite shape for 2D scenes.
    Quad,
    /// 1×1×1 box with edges rounded by 0.1, for crates, furniture and any
    /// box that should not look like a placeholder.
    RoundedCube,
    /// Diameter 1 and height 2 with round ends, like Godot's capsule: limbs,
    /// characters and posts. Matches a `Capsule` collider with radius 0.5 and
    /// half height 0.5.
    Capsule,
}

impl PrimitiveShape {
    pub const ALL: [Self; 15] = [
        Self::Cube,
        Self::Sphere,
        Self::Triangle,
        Self::Plane,
        Self::Tetrahedron,
        Self::Octahedron,
        Self::Dodecahedron,
        Self::Icosahedron,
        Self::Pyramid,
        Self::Cylinder,
        Self::Cone,
        Self::Torus,
        Self::Quad,
        Self::RoundedCube,
        Self::Capsule,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cube => "Cube",
            Self::Sphere => "Sphere",
            Self::Triangle => "Triangle",
            Self::Plane => "Plane",
            Self::Tetrahedron => "Tetrahedron",
            Self::Octahedron => "Octahedron",
            Self::Dodecahedron => "Dodecahedron",
            Self::Icosahedron => "Icosahedron",
            Self::Pyramid => "Pyramid",
            Self::Cylinder => "Cylinder",
            Self::Cone => "Cone",
            Self::Torus => "Torus",
            Self::Quad => "Quad",
            Self::RoundedCube => "Rounded Cube",
            Self::Capsule => "Capsule",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextureColorSpace {
    Srgb,
    Linear,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub enum TextureFilter {
    Nearest,
    #[default]
    Linear,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub enum TextureWrap {
    #[default]
    Repeat,
    MirroredRepeat,
    ClampToEdge,
}

/// Sampling state carried with a texture. The default (linear, repeat)
/// matches the renderer's shared sampler.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize,
)]
pub struct TextureSampler {
    pub mag_filter: TextureFilter,
    pub min_filter: TextureFilter,
    pub mipmap_filter: TextureFilter,
    /// Wrap modes along U and V.
    pub wrap: [TextureWrap; 2],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureAsset {
    pub size: [u32; 2],
    pub rgba8: Vec<u8>,
    pub color_space: TextureColorSpace,
    pub sampler: TextureSampler,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaterialModel {
    #[default]
    Pbr,
    Unlit,
}

/// How a material's base-color alpha is interpreted, matching glTF.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AlphaMode {
    #[default]
    Opaque,
    /// Fragments with alpha below `cutoff` are discarded.
    Mask {
        cutoff: f32,
    },
    Blend,
}

/// Surface settings for `GameScene::create_material`. Build it with
/// `..MaterialAsset::default()` (white, roughness 0.5, opaque PBR) and
/// set only the fields you need. Colors are linear RGB(A), 0 to 1.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialAsset {
    /// Editor name, such as "glass"; empty when unnamed. Two materials
    /// that differ only by name stay separate assets.
    pub name: String,
    /// `Pbr` (default) or `Unlit`, which ignores lights.
    pub model: MaterialModel,
    /// `Opaque` (default), `Mask { cutoff }` or `Blend` for see-through.
    pub alpha_mode: AlphaMode,
    /// Surface color and alpha.
    pub base_color: [f32; 4],
    /// Light the surface gives off on its own; above 1 blooms.
    pub emissive: [f32; 3],
    /// 0 is plastic or wood, 1 is bare metal.
    pub metallic: f32,
    /// 0 is a mirror, 1 is fully matte.
    pub roughness: f32,
    /// Share of light that passes through instead of being diffused, 0 to 1.
    /// Above 0 the material draws after opaque objects and shows them
    /// through itself, bent by `ior` and tinted by the base color.
    pub transmission: f32,
    /// Index of refraction: 1.0 does not bend light, glass is about 1.5.
    pub ior: f32,
    /// How far light travels inside, in world units; 0 is a thin sheet
    /// that does not bend what is behind it.
    pub thickness: f32,
    /// How many times the textures repeat across each face of the mesh, per
    /// axis; `[4.0, 8.0]` tiles a long floor instead of stretching it.
    pub uv_scale: [f32; 2],
    /// Shifts the textures across each face, in whole-texture units.
    pub uv_offset: [f32; 2],
    /// Lights each triangle with its own face normal, taken from
    /// screen-space derivatives, for a faceted low-poly look on any mesh.
    pub flat_shading: bool,
    pub base_color_texture: Option<Handle<TextureAsset>>,
    pub normal_texture: Option<Handle<TextureAsset>>,
    pub metallic_roughness_texture: Option<Handle<TextureAsset>>,
    pub occlusion_texture: Option<Handle<TextureAsset>>,
    pub emissive_texture: Option<Handle<TextureAsset>>,
}

impl Default for MaterialAsset {
    fn default() -> Self {
        Self {
            name: String::new(),
            model: MaterialModel::Pbr,
            alpha_mode: AlphaMode::Opaque,
            base_color: [1.0; 4],
            emissive: [0.0; 3],
            metallic: 0.0,
            roughness: 0.5,
            transmission: 0.0,
            ior: 1.5,
            thickness: 0.0,
            uv_scale: [1.0; 2],
            uv_offset: [0.0; 2],
            flat_shading: false,
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            occlusion_texture: None,
            emissive_texture: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneAsset {
    pub source: Option<PathBuf>,
}

/// Canonical CPU asset collections and built-in fallbacks.
#[derive(Resource)]
pub struct AssetServer {
    pub meshes: Assets<MeshAsset>,
    pub textures: Assets<TextureAsset>,
    pub materials: Assets<MaterialAsset>,
    pub scenes: Assets<SceneAsset>,
    /// Scenes loaded for placing at runtime; see [`AssetServer::load_prefab`].
    pub prefabs: Assets<crate::runtime::Prefab>,
    pub lod_groups: Assets<LodGroupAsset>,
    /// Game-defined data assets; see [`DataAsset`].
    pub data: DataAssets,
    pub fallback_mesh: Handle<MeshAsset>,
    /// Shared smooth sphere used by the editor's Add Sphere action.
    pub builtin_sphere: Handle<MeshAsset>,
    /// Every procedural primitive, keyed by its stable serialized identity.
    pub builtin_primitives: HashMap<PrimitiveShape, Handle<MeshAsset>>,
    pub fallback_texture: Handle<TextureAsset>,
    pub fallback_material: Handle<MaterialAsset>,
    /// Local mesh boxes by asset key and revision, so per-frame editor
    /// callers do not rescan every vertex of a large mesh.
    mesh_bounds_cache: Mutex<HashMap<(AssetKey, u64), Option<MeshBox>>>,
    /// Skinned and morphed mesh copies, kept with the meshes they live in.
    pub(crate) deformers: crate::runtime::Deformers,
}

type MeshBox = ([f32; 3], [f32; 3]);

/// One glTF primitive prepared for assignment in the editor.
#[derive(Clone, Debug)]
pub struct ImportedGltfPrimitive {
    /// Readable mesh and primitive name shown in Assets.
    pub name: String,
    /// Typed CPU mesh created from the primitive.
    pub mesh: Handle<MeshAsset>,
    /// PBR material factors created from the glTF material.
    pub material: Handle<MaterialAsset>,
}

impl AssetServer {
    /// The `.rskin` weights cooked next to a mesh file by the glTF importer.
    pub fn skin_weights(
        &self,
        mesh: Handle<MeshAsset>,
    ) -> Result<SkinWeights, AssetError> {
        let path = self.meshes.path(mesh).ok_or_else(|| AssetError::Load {
            path: PathBuf::new(),
            message: "skinned mesh has no file".into(),
        })?;
        decode_cooked(&path.with_extension("rskin"))
    }

    /// The `.rmorph` blend shapes cooked next to a mesh file by the glTF
    /// importer.
    pub fn morph_targets(
        &self,
        mesh: Handle<MeshAsset>,
    ) -> Result<MorphTargets, AssetError> {
        let path = self.meshes.path(mesh).ok_or_else(|| AssetError::Load {
            path: PathBuf::new(),
            message: "morphed mesh has no file".into(),
        })?;
        decode_cooked(&path.with_extension("rmorph"))
    }

    /// Smallest local box around every vertex of `handle`, computed once per
    /// mesh revision.
    #[must_use]
    pub fn mesh_bounds(&self, handle: Handle<MeshAsset>) -> Option<MeshBox> {
        let key = (AssetKey::from(handle), self.meshes.revision(handle)?);
        let mut cache = self
            .mesh_bounds_cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(&bounds) = cache.get(&key) {
            return bounds;
        }
        // ponytail: drops the whole cache when it grows; an LRU is only
        // worth it if scenes cycle through more meshes than this.
        if cache.len() >= 4096 {
            cache.clear();
        }
        let bounds =
            crate::runtime::picking::mesh_bounds(self.meshes.get(handle)?)
                .map(|(min, max)| (min.into(), max.into()));
        cache.insert(key, bounds);
        bounds
    }

    #[must_use]
    pub fn builtin_primitive(
        &self,
        shape: PrimitiveShape,
    ) -> Handle<MeshAsset> {
        self.builtin_primitives[&shape]
    }

    #[must_use]
    pub fn primitive_for_handle(
        &self,
        handle: Handle<MeshAsset>,
    ) -> Option<PrimitiveShape> {
        self.builtin_primitives
            .iter()
            .find_map(|(shape, candidate)| {
                (*candidate == handle).then_some(*shape)
            })
    }

    /// Loads an engine-native mesh created by the glTF importer, and the
    /// LOD group in the `.rlod` file beside it when there is one.
    pub fn load_mesh(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Handle<MeshAsset>, AssetError> {
        let handle = self.meshes.get_or_insert_with(&path, decode_mesh_file)?;
        let lods = path.as_ref().with_extension("rlod");
        if lods.is_file() && self.lod_groups.handle_for_path(&lods).is_none() {
            self.load_lod_group(lods)?;
        }
        Ok(handle)
    }

    /// Loads a `.rlod` JSON file and the meshes it names, which are
    /// relative to the file.
    // ponytail: `.rlod` edits need a reload of the scene; add them to
    // `reload_changed` when groups are tuned live.
    pub fn load_lod_group(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Handle<LodGroupAsset>, AssetError> {
        let path = path.as_ref();
        let error = |message: String| AssetError::Load {
            path: path.to_owned(),
            message,
        };
        let file: LodGroupFile =
            serde_json::from_slice(&read_asset_file(path)?)
                .map_err(|value| error(value.to_string()))?;
        if file.levels.is_empty() {
            return Err(error("a LOD group needs at least one level".into()));
        }
        if file.levels.len() > MAX_LOD_LEVELS {
            return Err(error(format!(
                "a LOD group has at most {MAX_LOD_LEVELS} levels, not {}",
                file.levels.len()
            )));
        }
        let directory = path.parent().unwrap_or(Path::new(""));
        let levels = file
            .levels
            .into_iter()
            .map(|level| {
                Ok(LodLevel {
                    mesh: self.meshes.get_or_insert_with(
                        directory.join(level.mesh),
                        decode_mesh_file,
                    )?,
                    until: level.until.unwrap_or(match file.metric {
                        LodMetric::Distance => f32::INFINITY,
                        LodMetric::ScreenSize => 0.0,
                    }),
                })
            })
            .collect::<Result<_, AssetError>>()?;
        let group = LodGroupAsset {
            metric: file.metric,
            levels,
        };
        if group
            .ranges()
            .iter()
            .any(|[start, end]| start > end || end.is_nan())
        {
            return Err(error(
                "LOD levels must hand over at growing distances or \
                 shrinking screen sizes"
                    .into(),
            ));
        }
        self.lod_groups.insert_with_path(path, group)
    }

    /// Loads an image as an sRGB texture and deduplicates its source path.
    pub fn load_texture(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Handle<TextureAsset>, AssetError> {
        self.textures.get_or_insert_with(path, decode_texture_file)
    }

    /// Starts worker decodes for every mesh and texture whose source file
    /// changed on disk. Returns how many reloads started. Changed scene
    /// paths are left for the caller via `scenes.changed()`.
    // ponytail: glTF-derived `.rmesh`/`.rtexture` reload when rewritten, but
    // editing the source `.gltf` does not re-import it yet.
    pub fn reload_changed(&mut self) -> usize {
        let mut started = 0;
        for (handle, _) in self.meshes.changed() {
            started += usize::from(
                self.meshes.reload_async(handle, decode_mesh_file).is_ok(),
            );
        }
        for (handle, _) in self.textures.changed() {
            started += usize::from(
                self.textures
                    .reload_async(handle, decode_texture_file)
                    .is_ok(),
            );
        }
        started
    }

    /// Drains mesh and texture hot reloads whose decode failed. The previous
    /// value stays loaded; callers only report the error.
    pub fn take_reload_failures(&mut self) -> Vec<AssetError> {
        let mut failures = self.meshes.take_reload_failures();
        failures.extend(self.textures.take_reload_failures());
        failures.append(&mut self.data.failures);
        failures
    }

    /// Drains the mesh and texture paths whose hot reload published.
    pub fn take_reloaded(&mut self) -> Vec<PathBuf> {
        let mut reloaded = self.meshes.take_reloaded();
        reloaded.extend(self.textures.take_reloaded());
        reloaded.append(&mut self.data.reloaded);
        reloaded
    }

    /// Imports one glTF texture slot as an sRGB or linear [`TextureAsset`],
    /// deduplicating repeated references to the same image/color-space pair
    /// by their synthesized asset path.
    #[cfg(feature = "gltf")]
    fn import_gltf_texture(
        &mut self,
        source: &Path,
        images: &[gltf::image::Data],
        texture: &gltf::texture::Texture,
        color_space: TextureColorSpace,
    ) -> Result<Handle<TextureAsset>, AssetError> {
        let image = &images[texture.source().index()];
        let suffix = match color_space {
            TextureColorSpace::Srgb => "srgb",
            TextureColorSpace::Linear => "linear",
        };
        let gltf_sampler = texture.sampler();
        // ponytail: one image used with two samplers is stored twice; split
        // pixels from sampling state if that shows up in real assets.
        let sampler_key = gltf_sampler
            .index()
            .map_or_else(|| "default".to_owned(), |index| index.to_string());
        let texture_key = source.with_extension(format!(
            "gltf-image-{}-sampler-{sampler_key}-{suffix}.rtexture",
            texture.source().index()
        ));
        // Written next to the source like `.rmesh`, so saved scenes that
        // reference this key load without re-importing the glTF.
        self.textures.get_or_insert_with(texture_key, |key| {
            let texture = TextureAsset {
                size: [image.width, image.height],
                rgba8: gltf_image_to_rgba8(image),
                color_space,
                sampler: gltf_texture_sampler(&gltf_sampler),
            };
            write_cooked_asset(key, &texture)?;
            Ok(texture)
        })
    }

    /// Imports every triangle primitive from a glTF or GLB file as typed CPU
    /// mesh and material assets. Rendering uploads them later as usual.
    #[cfg(feature = "gltf")]
    pub fn import_gltf(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<ImportedGltfPrimitive>, AssetError> {
        let source = normalize_path(path.as_ref())?;
        let (document, buffers, images) =
            gltf::import(&source).map_err(|error| AssetError::Load {
                path: source.clone(),
                message: error.to_string(),
            })?;
        let mut imported = Vec::new();
        for mesh in document.meshes() {
            for primitive in mesh.primitives() {
                if primitive.mode() != gltf::mesh::Mode::Triangles {
                    return Err(AssetError::Load {
                        path: source.clone(),
                        message: "only triangle glTF primitives are supported"
                            .into(),
                    });
                }
                let reader =
                    primitive.reader(|buffer| Some(&buffers[buffer.index()]));
                let positions = reader
                    .read_positions()
                    .ok_or_else(|| AssetError::Load {
                        path: source.clone(),
                        message: "glTF primitive has no positions".into(),
                    })?
                    .collect::<Vec<_>>();
                let imported_normals =
                    reader.read_normals().map(Iterator::collect::<Vec<_>>);
                let flat = imported_normals.is_none();
                // Replaced by flat face normals below.
                let normals = imported_normals
                    .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; positions.len()]);
                let uvs = reader
                    .read_tex_coords(0)
                    .map(|values| values.into_f32().collect())
                    .unwrap_or_else(|| vec![[0.0; 2]; positions.len()]);
                // glTF stores COLOR_0 linear, as the shader wants it.
                let colors = reader
                    .read_colors(0)
                    .map(|values| values.into_rgba_f32().collect())
                    .unwrap_or_else(|| vec![[1.0; 4]; positions.len()]);
                let imported_tangents =
                    reader.read_tangents().map(Iterator::collect::<Vec<_>>);
                let generate = imported_tangents.is_none();
                let tangents = imported_tangents.unwrap_or_else(|| {
                    vec![[1.0, 0.0, 0.0, 1.0]; positions.len()]
                });
                let mut skin = reader
                    .read_joints(0)
                    .zip(reader.read_weights(0))
                    .map(|(joints, weights)| SkinWeights {
                        joints: joints.into_u16().collect(),
                        weights: weights.into_f32().collect(),
                    });
                let mut morph = MorphTargets::default();
                for (offsets, normal_offsets, _) in reader.read_morph_targets()
                {
                    morph.positions.push(offsets.map_or_else(
                        || vec![[0.0; 3]; positions.len()],
                        Iterator::collect,
                    ));
                    morph.normals.push(
                        normal_offsets.map_or_else(Vec::new, Iterator::collect),
                    );
                }
                if normals.len() != positions.len()
                    || morph.positions.iter().chain(&morph.normals).any(
                        |offsets| {
                            !offsets.is_empty()
                                && offsets.len() != positions.len()
                        },
                    )
                    || uvs.len() != positions.len()
                    || colors.len() != positions.len()
                    || tangents.len() != positions.len()
                    || skin.as_ref().is_some_and(|skin| {
                        skin.joints.len() != positions.len()
                            || skin.weights.len() != positions.len()
                    })
                {
                    return Err(AssetError::Load {
                        path: source.clone(),
                        message:
                            "glTF vertex attributes have different lengths"
                                .into(),
                    });
                }
                let mut vertices = positions
                    .into_iter()
                    .enumerate()
                    .map(|(index, position)| MeshVertex {
                        position,
                        normal: normals[index],
                        uv: uvs[index],
                        tangent: tangents[index],
                        color: colors[index],
                    })
                    .collect::<Vec<_>>();
                let mut indices: Vec<u32> =
                    if let Some(values) = reader.read_indices() {
                        values.into_u32().collect()
                    } else {
                        let vertex_count = u32::try_from(vertices.len())
                            .map_err(|_| AssetError::Load {
                                path: source.clone(),
                                message: "glTF primitive has too many vertices"
                                    .into(),
                            })?;
                        (0..vertex_count).collect()
                    };
                check_mesh_indices(&source, &indices, vertices.len())?;
                if flat {
                    if let Some(skin) = &mut skin {
                        // flat_shaded gives each triangle corner its own
                        // vertex, in index order.
                        let corners = &indices[..indices.len() / 3 * 3];
                        *skin = SkinWeights {
                            joints: corners
                                .iter()
                                .map(|&index| skin.joints[index as usize])
                                .collect(),
                            weights: corners
                                .iter()
                                .map(|&index| skin.weights[index as usize])
                                .collect(),
                        };
                    }
                    let corners = &indices[..indices.len() / 3 * 3];
                    for offsets in &mut morph.positions {
                        *offsets = corners
                            .iter()
                            .map(|&index| offsets[index as usize])
                            .collect();
                    }
                    morph.normals.iter_mut().for_each(Vec::clear);
                    (vertices, indices) = flat_shaded(&vertices, &indices);
                }
                if generate {
                    generate_tangents(&mut vertices, &indices);
                }
                let mesh_key = source.with_extension(format!(
                    "mesh-{}-{}.rmesh",
                    mesh.index(),
                    primitive.index()
                ));
                if let Some(skin) = &skin {
                    write_cooked_asset(
                        &mesh_key.with_extension("rskin"),
                        skin,
                    )?;
                }
                if !morph.positions.is_empty() {
                    write_cooked_asset(
                        &mesh_key.with_extension("rmorph"),
                        &morph,
                    )?;
                }
                let mesh_asset = MeshAsset { vertices, indices };
                write_cooked_bytes(&mesh_key, encode_mesh_file(&mesh_asset))?;
                let mesh_handle =
                    self.meshes.insert_with_path(mesh_key, mesh_asset)?;
                let gltf_material = primitive.material();
                let pbr = gltf_material.pbr_metallic_roughness();
                let base_color_texture = pbr
                    .base_color_texture()
                    .map(|info| {
                        self.import_gltf_texture(
                            &source,
                            &images,
                            &info.texture(),
                            TextureColorSpace::Srgb,
                        )
                    })
                    .transpose()?;
                let metallic_roughness_texture = pbr
                    .metallic_roughness_texture()
                    .map(|info| {
                        self.import_gltf_texture(
                            &source,
                            &images,
                            &info.texture(),
                            TextureColorSpace::Linear,
                        )
                    })
                    .transpose()?;
                let normal_texture = gltf_material
                    .normal_texture()
                    .map(|info| {
                        self.import_gltf_texture(
                            &source,
                            &images,
                            &info.texture(),
                            TextureColorSpace::Linear,
                        )
                    })
                    .transpose()?;
                let occlusion_texture = gltf_material
                    .occlusion_texture()
                    .map(|info| {
                        self.import_gltf_texture(
                            &source,
                            &images,
                            &info.texture(),
                            TextureColorSpace::Linear,
                        )
                    })
                    .transpose()?;
                let emissive_texture = gltf_material
                    .emissive_texture()
                    .map(|info| {
                        self.import_gltf_texture(
                            &source,
                            &images,
                            &info.texture(),
                            TextureColorSpace::Srgb,
                        )
                    })
                    .transpose()?;
                let material_key = source.with_extension(format!(
                    "gltf-material-{}",
                    gltf_material.index().unwrap_or(usize::MAX)
                ));
                let material = self.materials.insert_with_path(
                    material_key,
                    MaterialAsset {
                        name: gltf_material
                            .name()
                            .map(str::to_owned)
                            .unwrap_or_default(),
                        model: MaterialModel::Pbr,
                        alpha_mode: match gltf_material.alpha_mode() {
                            gltf::material::AlphaMode::Opaque => {
                                AlphaMode::Opaque
                            }
                            gltf::material::AlphaMode::Mask => {
                                AlphaMode::Mask {
                                    cutoff: gltf_material
                                        .alpha_cutoff()
                                        .unwrap_or(0.5),
                                }
                            }
                            gltf::material::AlphaMode::Blend => {
                                AlphaMode::Blend
                            }
                        },
                        base_color: pbr.base_color_factor(),
                        emissive: gltf_material.emissive_factor(),
                        metallic: pbr.metallic_factor(),
                        roughness: pbr.roughness_factor(),
                        base_color_texture,
                        normal_texture,
                        metallic_roughness_texture,
                        occlusion_texture,
                        emissive_texture,
                        // ponytail: KHR_materials_transmission, ior and
                        // volume need gltf crate features; enable them when
                        // an imported glass model needs it.
                        ..MaterialAsset::default()
                    },
                )?;
                imported.push(ImportedGltfPrimitive {
                    name: format!(
                        "{} / Primitive {}",
                        mesh.name().unwrap_or("Mesh"),
                        primitive.index()
                    ),
                    mesh: mesh_handle,
                    material,
                });
            }
        }
        Ok(imported)
    }

    /// Imports a glTF file's node tree: local transforms, parent links, mesh
    /// primitives, cameras, and `KHR_lights_punctual` lights. The returned
    /// list is indexed like the glTF `nodes` array. Light intensities keep
    /// glTF's physical values (lux for directional, candela otherwise).
    #[cfg(feature = "gltf")]
    pub fn import_gltf_scene(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<ImportedGltfNode>, AssetError> {
        let primitives = self.import_gltf(&path)?;
        let source = normalize_path(path.as_ref())?;
        // ponytail: parses the JSON a second time after import_gltf; share
        // the parsed document if import time on large files matters.
        let load_error = |error: gltf::Error| AssetError::Load {
            path: source.clone(),
            message: error.to_string(),
        };
        let gltf::Gltf { document, blob } =
            gltf::Gltf::open(&source).map_err(load_error)?;
        let buffers = gltf::import_buffers(&document, source.parent(), blob)
            .map_err(load_error)?;
        let mut first_primitive = Vec::new();
        let mut offset = 0;
        for mesh in document.meshes() {
            first_primitive.push(offset);
            offset += mesh.primitives().len();
        }
        let mut nodes = document
            .nodes()
            .map(|node| {
                let (position, [x, y, z, w], scale) =
                    node.transform().decomposed();
                let (roll, pitch, yaw) =
                    nalgebra::UnitQuaternion::from_quaternion(
                        nalgebra::Quaternion::new(w, x, y, z),
                    )
                    .euler_angles();
                ImportedGltfNode {
                    name: node.name().map_or_else(
                        || format!("Node {}", node.index()),
                        str::to_owned,
                    ),
                    parent: None,
                    transform: crate::Transform {
                        position,
                        rotation: [roll, pitch, yaw],
                        scale,
                    },
                    skin: node.skin().map(|skin| ImportedGltfSkin {
                        joints: skin
                            .joints()
                            .map(|joint| joint.index())
                            .collect(),
                        inverse_bind: skin
                            .reader(|buffer| Some(&buffers[buffer.index()]))
                            .read_inverse_bind_matrices()
                            .map_or_else(
                                || {
                                    vec![
                                        nalgebra::Matrix4::identity().into();
                                        skin.joints().len()
                                    ]
                                },
                                Iterator::collect,
                            ),
                    }),
                    morph_weights: node.mesh().and_then(|mesh| {
                        let targets =
                            mesh.primitives().next()?.morph_targets().len();
                        (targets > 0).then(|| {
                            let mut weights = node
                                .weights()
                                .or(mesh.weights())
                                .map_or_else(Vec::new, <[f32]>::to_vec);
                            weights.resize(targets, 0.0);
                            weights
                        })
                    }),
                    primitives: node.mesh().map_or_else(Vec::new, |mesh| {
                        let start = first_primitive[mesh.index()];
                        primitives[start..start + mesh.primitives().len()]
                            .to_vec()
                    }),
                    camera: node.camera().map(|camera| Camera {
                        projection: match camera.projection() {
                            gltf::camera::Projection::Perspective(p) => {
                                crate::runtime::Projection::Perspective {
                                    vertical_fov_radians: p.yfov(),
                                    near: p.znear(),
                                    far: p.zfar().unwrap_or(1_000.0),
                                }
                            }
                            gltf::camera::Projection::Orthographic(o) => {
                                crate::runtime::Projection::Orthographic {
                                    vertical_size: o.ymag() * 2.0,
                                    near: o.znear(),
                                    far: o.zfar(),
                                }
                            }
                        },
                        ..Camera::default()
                    }),
                    light: node.light().map(|light| {
                        let color = light.color();
                        let intensity = light.intensity();
                        let range = light
                            .range()
                            .unwrap_or(PointLight::default().range);
                        match light.kind() {
                            gltf::khr_lights_punctual::Kind::Directional => {
                                ImportedGltfLight::Directional(
                                    DirectionalLight {
                                        color,
                                        illuminance: intensity,
                                        ..DirectionalLight::default()
                                    },
                                )
                            }
                            gltf::khr_lights_punctual::Kind::Point => {
                                ImportedGltfLight::Point(PointLight {
                                    color,
                                    intensity,
                                    range,
                                    shadows: false,
                                    radius: 0.0,
                                })
                            }
                            gltf::khr_lights_punctual::Kind::Spot {
                                inner_cone_angle,
                                outer_cone_angle,
                            } => ImportedGltfLight::Spot(SpotLight {
                                color,
                                intensity,
                                range,
                                inner_angle: inner_cone_angle,
                                outer_angle: outer_cone_angle,
                                shadows: false,
                                radius: 0.0,
                            }),
                        }
                    }),
                }
            })
            .collect::<Vec<_>>();
        for node in document.nodes() {
            for child in node.children() {
                nodes[child.index()].parent = Some(node.index());
            }
        }
        Ok(nodes)
    }
}

impl AssetServer {
    /// Reads a glTF file's node animations as `rusting.animation` clips for
    /// a root whose children are the glTF scene's top nodes, as `nodes`
    /// from [`Self::import_gltf_scene`] spawn. Translation and scale become
    /// Position and Scale tracks, rotation an Orientation (quaternion)
    /// track. Cubic spline keys keep their values and play Smooth. Morph
    /// target weights are skipped.
    #[cfg(feature = "gltf")]
    pub fn import_gltf_animations(
        &self,
        path: impl AsRef<Path>,
        nodes: &[ImportedGltfNode],
    ) -> Result<Vec<crate::runtime::AnimationClip>, AssetError> {
        use crate::runtime::{
            AnimationClip, AnimationProperty, AnimationTrack, Interpolation,
            Keyframe, TweenRepeat,
        };
        use gltf::animation::util::ReadOutputs;
        let source = normalize_path(path.as_ref())?;
        let error = |message: String| AssetError::Load {
            path: source.clone(),
            message,
        };
        let gltf = gltf::Gltf::open(&source)
            .map_err(|value| error(value.to_string()))?;
        let buffers =
            gltf::import_buffers(&gltf.document, source.parent(), gltf.blob)
                .map_err(|value| error(value.to_string()))?;
        // ponytail: a name path finds the first child with that name, so
        // siblings that share a name animate the first one.
        let target = |mut node: usize| {
            let mut names = vec![nodes.get(node)?.name.as_str()];
            while let Some(parent) = nodes[node].parent {
                names.push(&nodes[parent].name);
                node = parent;
            }
            names.reverse();
            Some(names.join("/"))
        };
        let mut clips = Vec::new();
        for animation in gltf.document.animations() {
            let mut tracks = Vec::new();
            for channel in animation.channels() {
                let Some(target_path) = target(channel.target().node().index())
                else {
                    continue;
                };
                let reader = channel
                    .reader(|buffer| Some(&buffers[buffer.index()].0[..]));
                let (Some(times), Some(outputs)) =
                    (reader.read_inputs(), reader.read_outputs())
                else {
                    continue;
                };
                let (property, mut values): (_, Vec<Vec<f32>>) = match outputs {
                    ReadOutputs::Translations(values) => (
                        AnimationProperty::Position,
                        values.map(Vec::from).collect(),
                    ),
                    ReadOutputs::Scales(values) => (
                        AnimationProperty::Scale,
                        values.map(Vec::from).collect(),
                    ),
                    ReadOutputs::Rotations(values) => (
                        AnimationProperty::Orientation,
                        values.into_f32().map(Vec::from).collect(),
                    ),
                    ReadOutputs::MorphTargetWeights(values) => {
                        let node = channel.target().node().index();
                        let width = nodes[node]
                            .morph_weights
                            .as_ref()
                            .map_or(0, Vec::len);
                        if width == 0 {
                            continue;
                        }
                        let values = values.into_f32().collect::<Vec<_>>();
                        (
                            AnimationProperty::Field {
                                component: crate::runtime::MORPH_COMPONENT
                                    .into(),
                                path: "/weights".into(),
                            },
                            values.chunks(width).map(<[f32]>::to_vec).collect(),
                        )
                    }
                };
                let interpolation = match channel.sampler().interpolation() {
                    gltf::animation::Interpolation::Step => Interpolation::Step,
                    gltf::animation::Interpolation::Linear => {
                        Interpolation::Linear
                    }
                    gltf::animation::Interpolation::CubicSpline => {
                        // (in-tangent, value, out-tangent) per key.
                        values = values
                            .chunks(3)
                            .filter_map(|key| key.get(1).cloned())
                            .collect();
                        Interpolation::Smooth
                    }
                };
                if property == AnimationProperty::Orientation {
                    // Shortest arc between keys, so the blend never spins
                    // the long way round.
                    for index in 1..values.len() {
                        let dot: f32 = values[index - 1]
                            .iter()
                            .zip(&values[index])
                            .map(|(a, b)| a * b)
                            .sum();
                        if dot < 0.0 {
                            values[index].iter_mut().for_each(|v| *v = -*v);
                        }
                    }
                }
                tracks.push(AnimationTrack {
                    target: target_path,
                    property,
                    interpolation,
                    keys: times
                        .zip(values)
                        .map(|(time, value)| Keyframe { time, value })
                        .collect(),
                });
            }
            if tracks.is_empty() {
                continue;
            }
            clips.push(AnimationClip {
                name: animation.name().map_or_else(
                    || format!("Animation {}", animation.index()),
                    str::to_owned,
                ),
                duration: 0.0,
                repeat: TweenRepeat::Loop,
                tracks,
                ..AnimationClip::default()
            });
        }
        Ok(clips)
    }
}

/// Writes an importer-produced asset as bincode at its synthesized key.
#[cfg(feature = "gltf")]
fn write_cooked_asset(
    path: &Path,
    asset: &impl Serialize,
) -> Result<(), AssetError> {
    write_cooked_bytes(path, bincode::serialize(asset))
}

#[cfg(feature = "gltf")]
fn write_cooked_bytes(
    path: &Path,
    bytes: bincode::Result<Vec<u8>>,
) -> Result<(), AssetError> {
    let error = |message: String| AssetError::Load {
        path: path.to_owned(),
        message,
    };
    let bytes = bytes.map_err(|value| error(value.to_string()))?;
    std::fs::write(path, bytes).map_err(|value| error(value.to_string()))
}

/// Unshares a triangle list's vertices and gives each triangle its face
/// normal. glTF asks for flat normals when a primitive has none.
#[cfg(feature = "gltf")]
fn flat_shaded(
    vertices: &[MeshVertex],
    indices: &[u32],
) -> (Vec<MeshVertex>, Vec<u32>) {
    use nalgebra::Vector3;
    let mut flat = Vec::with_capacity(indices.len());
    for triangle in indices.chunks_exact(3) {
        let corners =
            [0, 1, 2].map(|corner| vertices[triangle[corner] as usize]);
        let [a, b, c] = corners.map(|vertex| Vector3::from(vertex.position));
        let normal = (b - a)
            .cross(&(c - a))
            .try_normalize(f32::EPSILON)
            .unwrap_or_else(Vector3::y);
        flat.extend(corners.map(|vertex| MeshVertex {
            normal: normal.into(),
            ..vertex
        }));
    }
    let count = flat.len() as u32;
    (flat, (0..count).collect())
}

/// Spawns imported glTF nodes as entities with `Name`, `Transform`, parent
/// links, cameras, and lights. A node's first primitive renders on the node
/// itself; extra primitives become child entities. Returns one entity per
/// node, in node order. Each primitive keeps its imported glTF material
/// unless `material_override` is supplied.
pub fn spawn_gltf_nodes(
    app: &mut App,
    nodes: &[ImportedGltfNode],
    material_override: Option<Handle<MaterialAsset>>,
) -> Result<Vec<bevy_ecs::entity::Entity>, AppError> {
    spawn_gltf_nodes_in_world(app.world_mut(), nodes, material_override)
}

/// [`spawn_gltf_nodes`] for code that holds a `World` instead of an `App`.
/// Every spawned entity gets a new `SceneId`, so it saves with the scene.
pub fn spawn_gltf_nodes_in_world(
    world: &mut bevy_ecs::world::World,
    nodes: &[ImportedGltfNode],
    material_override: Option<Handle<MaterialAsset>>,
) -> Result<Vec<bevy_ecs::entity::Entity>, AppError> {
    use crate::runtime::SceneId;
    let renderer = |primitive: &ImportedGltfPrimitive| MeshRenderer {
        mesh: primitive.mesh,
        material: material_override.unwrap_or(primitive.material),
        cast_shadows: true,
        receive_shadows: true,
    };
    let entities = nodes
        .iter()
        .map(|node| {
            let mut world_entity = world.spawn((
                SceneId::new(),
                Name(node.name.clone()),
                node.transform,
            ));
            if let Some(camera) = node.camera {
                world_entity.insert(camera);
            }
            match node.light {
                Some(ImportedGltfLight::Directional(light)) => {
                    world_entity.insert(light);
                }
                Some(ImportedGltfLight::Point(light)) => {
                    world_entity.insert(light);
                }
                Some(ImportedGltfLight::Spot(light)) => {
                    world_entity.insert(light);
                }
                None => {}
            }
            if let Some(first) = node.primitives.first() {
                world_entity.insert(renderer(first));
            }
            if let Some(weights) = &node.morph_weights {
                world_entity.insert(crate::runtime::Morph {
                    weights: weights.clone(),
                });
            }
            world_entity.id()
        })
        .collect::<Vec<_>>();
    for (node, &entity) in nodes.iter().zip(&entities) {
        if let Some(parent) = node.parent {
            crate::runtime::hierarchy::set_parent(
                world,
                entity,
                entities[parent],
            )?;
        }
        for primitive in node.primitives.iter().skip(1) {
            let child = world
                .spawn((
                    SceneId::new(),
                    Name(primitive.name.clone()),
                    crate::Transform::default(),
                    renderer(primitive),
                ))
                .id();
            crate::runtime::hierarchy::set_parent(world, child, entity)?;
        }
    }
    insert_gltf_skins(world, nodes, &entities);
    Ok(entities)
}

/// Inserts each skinned node's `rusting.skin`, with joint paths from the
/// current node names. A node's extra primitives, its children outside
/// `entities`, get the same skin one level further down.
pub fn insert_gltf_skins(
    world: &mut bevy_ecs::world::World,
    nodes: &[ImportedGltfNode],
    entities: &[bevy_ecs::entity::Entity],
) {
    for (index, &entity) in entities.iter().enumerate() {
        let Some(skin) = gltf_node_skin(nodes, index) else {
            continue;
        };
        let parts = world
            .get::<crate::runtime::Children>(entity)
            .map_or_else(Vec::new, |children| children.0.clone());
        for part in parts.into_iter().filter(|part| !entities.contains(part)) {
            let mut part_skin = skin.clone();
            for joint in &mut part_skin.joints {
                *joint = if joint.is_empty() {
                    "..".into()
                } else {
                    format!("../{joint}")
                };
            }
            world.entity_mut(part).insert(part_skin);
        }
        world.entity_mut(entity).insert(skin);
    }
}

/// One glTF node prepared for spawning; see [`spawn_gltf_nodes`].
#[derive(Clone, Debug)]
pub struct ImportedGltfNode {
    pub name: String,
    /// Index of the parent node in the same list.
    pub parent: Option<usize>,
    /// Local transform relative to the parent.
    pub transform: crate::Transform,
    pub primitives: Vec<ImportedGltfPrimitive>,
    pub camera: Option<Camera>,
    pub light: Option<ImportedGltfLight>,
    pub skin: Option<ImportedGltfSkin>,
    /// Starting blend shape weights when the node's mesh has morph targets.
    pub morph_weights: Option<Vec<f32>>,
}

/// A glTF skin on a node: joint node indices in the same list and their
/// inverse bind matrices (column-major).
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedGltfSkin {
    pub joints: Vec<usize>,
    pub inverse_bind: Vec<[[f32; 4]; 4]>,
}

/// The `rusting.skin` for node `index`, with joint paths built from the
/// current node names, or `None` when the node has no skin. Paths are
/// relative to the skinned node; nodes with no shared ancestor meet at the
/// root the nodes are spawned under.
pub fn gltf_node_skin(
    nodes: &[ImportedGltfNode],
    index: usize,
) -> Option<crate::runtime::Skin> {
    let skin = nodes.get(index)?.skin.as_ref()?;
    let chain = |mut node: usize| {
        let mut chain = vec![node];
        while let Some(parent) = nodes[node].parent {
            chain.push(parent);
            node = parent;
        }
        chain.reverse();
        chain
    };
    let from = chain(index);
    let joints = skin
        .joints
        .iter()
        .map(|&joint| {
            let to = chain(joint);
            let shared =
                from.iter().zip(&to).take_while(|(a, b)| a == b).count();
            let mut path = vec![".."; from.len() - shared];
            path.extend(
                to[shared..].iter().map(|&node| nodes[node].name.as_str()),
            );
            path.join("/")
        })
        .collect();
    Some(crate::runtime::Skin {
        joints,
        inverse_bind: skin.inverse_bind.clone(),
    })
}

/// A `KHR_lights_punctual` light attached to a glTF node.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImportedGltfLight {
    Directional(DirectionalLight),
    Point(PointLight),
    Spot(SpotLight),
}

impl Default for AssetServer {
    fn default() -> Self {
        let mut meshes = Assets::default();
        let mut builtin_primitives = HashMap::new();
        for shape in PrimitiveShape::ALL {
            builtin_primitives
                .insert(shape, meshes.insert(procedural_primitive_mesh(shape)));
        }
        let fallback_mesh = builtin_primitives[&PrimitiveShape::Cube];
        let builtin_sphere = builtin_primitives[&PrimitiveShape::Sphere];
        let mut textures = Assets::default();
        let fallback_texture = textures.insert(TextureAsset {
            size: [1, 1],
            rgba8: vec![255; 4],
            color_space: TextureColorSpace::Srgb,
            sampler: TextureSampler::default(),
        });
        let mut materials = Assets::default();
        let fallback_material = materials.insert(MaterialAsset {
            base_color: [1.0, 0.0, 1.0, 1.0],
            base_color_texture: Some(fallback_texture),
            ..MaterialAsset::default()
        });
        Self {
            meshes,
            textures,
            materials,
            scenes: Assets::default(),
            prefabs: Assets::default(),
            lod_groups: Assets::default(),
            data: DataAssets::default(),
            fallback_mesh,
            builtin_sphere,
            builtin_primitives,
            fallback_texture,
            fallback_material,
            mesh_bounds_cache: Mutex::default(),
            deformers: crate::runtime::Deformers::default(),
        }
    }
}

/// Builds a half-size UV sphere for built-in and procedural scene objects.
///
/// The returned CPU mesh can be stored in [`Assets`] and uploaded by any
/// renderer. Keeping this generator in the asset module lets the editor,
/// ECS runtime, and future import tools use the same geometry format.
pub fn procedural_sphere_mesh(subdivisions: u32) -> MeshAsset {
    use std::f32::consts::PI;

    let stacks = subdivisions.clamp(2, 128);
    let sectors = stacks * 2;
    let mut vertices =
        Vec::with_capacity(((stacks + 1) * (sectors + 1)) as usize);
    let mut indices = Vec::with_capacity((stacks * sectors * 6) as usize);
    for stack in 0..=stacks {
        let vertical = stack as f32 / stacks as f32;
        let phi = PI * vertical;
        let ring = 0.5 * phi.sin();
        let y = 0.5 * phi.cos();
        for sector in 0..=sectors {
            let horizontal = sector as f32 / sectors as f32;
            let theta = 2.0 * PI * horizontal;
            let x = ring * theta.cos();
            let z = ring * theta.sin();
            vertices.push(MeshVertex {
                position: [x, y, z],
                normal: [x * 2.0, y * 2.0, z * 2.0],
                uv: [horizontal, vertical],
                tangent: [0.0, 1.0, 0.0, 1.0],
                color: [1.0; 4],
            });
        }
    }
    let row = sectors + 1;
    for stack in 0..stacks {
        for sector in 0..sectors {
            let first = stack * row + sector;
            let second = first + row;
            indices.extend_from_slice(&[
                first,
                first + 1,
                second,
                second,
                first + 1,
                second + 1,
            ]);
        }
    }
    MeshAsset { vertices, indices }
}

/// Builds the CPU mesh used for one built-in editor primitive.
#[must_use]
pub fn procedural_primitive_mesh(shape: PrimitiveShape) -> MeshAsset {
    let mut mesh = match shape {
        PrimitiveShape::Cube => fallback_cube(),
        PrimitiveShape::Sphere => procedural_sphere_mesh(16),
        PrimitiveShape::Triangle => mesh_from_triangles(&[[
            [-0.5, -0.5, 0.0],
            [0.5, -0.5, 0.0],
            [0.0, 0.5, 0.0],
        ]]),
        PrimitiveShape::Plane => plane_mesh(),
        PrimitiveShape::Tetrahedron => polyhedron_mesh(
            &[
                [0.5, 0.5, 0.5],
                [-0.5, -0.5, 0.5],
                [-0.5, 0.5, -0.5],
                [0.5, -0.5, -0.5],
            ],
            &[[0, 1, 2], [0, 3, 1], [0, 2, 3], [1, 3, 2]],
        ),
        PrimitiveShape::Octahedron => polyhedron_mesh(
            &[
                [0.5, 0.0, 0.0],
                [-0.5, 0.0, 0.0],
                [0.0, 0.5, 0.0],
                [0.0, -0.5, 0.0],
                [0.0, 0.0, 0.5],
                [0.0, 0.0, -0.5],
            ],
            &[
                [0, 2, 4],
                [4, 2, 1],
                [1, 2, 5],
                [5, 2, 0],
                [4, 3, 0],
                [1, 3, 4],
                [5, 3, 1],
                [0, 3, 5],
            ],
        ),
        PrimitiveShape::Dodecahedron => dodecahedron_mesh(),
        PrimitiveShape::Icosahedron => icosahedron_mesh(),
        PrimitiveShape::Pyramid => pyramid_mesh(),
        PrimitiveShape::Cylinder => cylinder_mesh(32),
        PrimitiveShape::Cone => cone_mesh(32),
        PrimitiveShape::Torus => torus_mesh(32, 12),
        PrimitiveShape::Quad => quad_mesh(),
        PrimitiveShape::RoundedCube => rounded_cube_mesh(0.1, 4),
        PrimitiveShape::Capsule => capsule_mesh(32, 8),
    };
    // The builders write placeholder tangents; normal maps need real ones.
    generate_tangents(&mut mesh.vertices, &mesh.indices);
    mesh
}

fn mesh_from_triangles(triangles: &[[[f32; 3]; 3]]) -> MeshAsset {
    let mut vertices = Vec::with_capacity(triangles.len() * 3);
    let mut indices = Vec::with_capacity(triangles.len() * 3);
    for triangle in triangles {
        let edge_a = subtract(triangle[1], triangle[0]);
        let edge_b = subtract(triangle[2], triangle[0]);
        let normal = normalize3(cross(edge_a, edge_b));
        let base = vertices.len() as u32;
        for (position, uv) in
            triangle
                .iter()
                .copied()
                .zip([[0.0, 0.0], [1.0, 0.0], [0.5, 1.0]])
        {
            vertices.push(MeshVertex {
                position,
                normal,
                uv,
                tangent: [1.0, 0.0, 0.0, 1.0],
                color: [1.0; 4],
            });
        }
        indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    MeshAsset { vertices, indices }
}

fn polyhedron_mesh(points: &[[f32; 3]], faces: &[[usize; 3]]) -> MeshAsset {
    let triangles = faces
        .iter()
        .map(|face| {
            let mut triangle =
                [points[face[0]], points[face[1]], points[face[2]]];
            let normal = cross(
                subtract(triangle[1], triangle[0]),
                subtract(triangle[2], triangle[0]),
            );
            let center = [
                (triangle[0][0] + triangle[1][0] + triangle[2][0]) / 3.0,
                (triangle[0][1] + triangle[1][1] + triangle[2][1]) / 3.0,
                (triangle[0][2] + triangle[1][2] + triangle[2][2]) / 3.0,
            ];
            if dot(normal, center) < 0.0 {
                triangle.swap(1, 2);
            }
            triangle
        })
        .collect::<Vec<_>>();
    mesh_from_triangles(&triangles)
}

fn icosahedron_mesh() -> MeshAsset {
    let golden = (1.0 + 5.0_f32.sqrt()) * 0.5;
    let raw = [
        [-1.0, golden, 0.0],
        [1.0, golden, 0.0],
        [-1.0, -golden, 0.0],
        [1.0, -golden, 0.0],
        [0.0, -1.0, golden],
        [0.0, 1.0, golden],
        [0.0, -1.0, -golden],
        [0.0, 1.0, -golden],
        [golden, 0.0, -1.0],
        [golden, 0.0, 1.0],
        [-golden, 0.0, -1.0],
        [-golden, 0.0, 1.0],
    ];
    let points = raw.map(|point| {
        let normal = normalize3(point);
        [normal[0] * 0.5, normal[1] * 0.5, normal[2] * 0.5]
    });
    polyhedron_mesh(
        &points,
        &[
            [0, 11, 5],
            [0, 5, 1],
            [0, 1, 7],
            [0, 7, 10],
            [0, 10, 11],
            [1, 5, 9],
            [5, 11, 4],
            [11, 10, 2],
            [10, 7, 6],
            [7, 1, 8],
            [3, 9, 4],
            [3, 4, 2],
            [3, 2, 6],
            [3, 6, 8],
            [3, 8, 9],
            [4, 9, 5],
            [2, 4, 11],
            [6, 2, 10],
            [8, 6, 7],
            [9, 8, 1],
        ],
    )
}

fn dodecahedron_mesh() -> MeshAsset {
    let golden = (1.0 + 5.0_f32.sqrt()) * 0.5;
    let inverse = 1.0 / golden;
    let raw = [
        [-1.0, -1.0, -1.0],
        [-1.0, -1.0, 1.0],
        [-1.0, 1.0, -1.0],
        [-1.0, 1.0, 1.0],
        [1.0, -1.0, -1.0],
        [1.0, -1.0, 1.0],
        [1.0, 1.0, -1.0],
        [1.0, 1.0, 1.0],
        [0.0, -inverse, -golden],
        [0.0, -inverse, golden],
        [0.0, inverse, -golden],
        [0.0, inverse, golden],
        [-inverse, -golden, 0.0],
        [-inverse, golden, 0.0],
        [inverse, -golden, 0.0],
        [inverse, golden, 0.0],
        [-golden, 0.0, -inverse],
        [golden, 0.0, -inverse],
        [-golden, 0.0, inverse],
        [golden, 0.0, inverse],
    ];
    let scale = 0.5 / 3.0_f32.sqrt();
    let points =
        raw.map(|point| [point[0] * scale, point[1] * scale, point[2] * scale]);
    let mut face_sets = Vec::<Vec<usize>>::new();
    for first in 0..points.len() - 2 {
        for second in first + 1..points.len() - 1 {
            for third in second + 1..points.len() {
                let mut normal = cross(
                    subtract(points[second], points[first]),
                    subtract(points[third], points[first]),
                );
                if dot(normal, normal) < 1.0e-8 {
                    continue;
                }
                normal = normalize3(normal);
                let plane = dot(normal, points[first]);
                let distances = points.map(|point| dot(normal, point) - plane);
                let supports_hull = distances
                    .iter()
                    .all(|distance| *distance <= 1.0e-4)
                    || distances.iter().all(|distance| *distance >= -1.0e-4);
                if !supports_hull {
                    continue;
                }
                let face = distances
                    .iter()
                    .enumerate()
                    .filter_map(|(index, distance)| {
                        (distance.abs() <= 1.0e-4).then_some(index)
                    })
                    .collect::<Vec<_>>();
                if face.len() == 5 && !face_sets.contains(&face) {
                    face_sets.push(face);
                }
            }
        }
    }

    let mut triangles = Vec::with_capacity(36);
    for mut face in face_sets {
        let center = face.iter().fold([0.0; 3], |mut sum, index| {
            for axis in 0..3 {
                sum[axis] += points[*index][axis] / 5.0;
            }
            sum
        });
        let normal = normalize3(center);
        let basis_x = normalize3(subtract(points[face[0]], center));
        let basis_y = cross(normal, basis_x);
        face.sort_by(|left, right| {
            let left_delta = subtract(points[*left], center);
            let right_delta = subtract(points[*right], center);
            let left_angle =
                dot(left_delta, basis_y).atan2(dot(left_delta, basis_x));
            let right_angle =
                dot(right_delta, basis_y).atan2(dot(right_delta, basis_x));
            left_angle.total_cmp(&right_angle)
        });
        for corner in 1..face.len() - 1 {
            triangles.push([
                points[face[0]],
                points[face[corner]],
                points[face[corner + 1]],
            ]);
        }
    }
    mesh_from_triangles(&triangles)
}

fn pyramid_mesh() -> MeshAsset {
    polyhedron_mesh(
        &[
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, -0.5, 0.5],
            [-0.5, -0.5, 0.5],
            [0.0, 0.5, 0.0],
        ],
        &[
            [0, 1, 2],
            [0, 2, 3],
            [0, 4, 1],
            [1, 4, 2],
            [2, 4, 3],
            [3, 4, 0],
        ],
    )
}

/// Turns a profile of `(radius, y)` points with `(radial, y)` normals
/// around the Y axis. Consecutive points form a band; repeat a point with a
/// new normal for a hard edge. Bands wrap U once around the axis with V
/// from 0 at the top to 1 at the bottom, by height, as on the sphere; flat caps (radial normal 0) map the texture as a disc,
/// upright when seen from outside with -Z at the top.
fn lathe_mesh(profile: &[([f32; 2], [f32; 2])], segments: u32) -> MeshAsset {
    let mut vertices = Vec::new();
    let (bottom, top) = profile
        .iter()
        .fold((f32::MAX, f32::MIN), |(low, high), ([_, y], _)| {
            (low.min(*y), high.max(*y))
        });
    for step in 0..=segments {
        let angle = std::f32::consts::TAU * step as f32 / segments as f32;
        let (sin, cos) = angle.sin_cos();
        for ([radius, y], [radial, normal_y]) in profile.iter().copied() {
            vertices.push(MeshVertex {
                position: [radius * cos, y, radius * sin],
                normal: normalize3([radial * cos, normal_y, radial * sin]),
                uv: if radial == 0.0 {
                    [0.5 + radius * cos * normal_y, 0.5 + radius * sin]
                } else {
                    [step as f32 / segments as f32, (top - y) / (top - bottom)]
                },
                tangent: [1.0, 0.0, 0.0, 1.0],
                color: [1.0; 4],
            });
        }
    }
    let rows = profile.len() as u32;
    let mut indices = Vec::new();
    for step in 0..segments {
        for index in 0..rows - 1 {
            let [low_a, low_b] =
                [step * rows + index, (step + 1) * rows + index];
            let [high_a, high_b] = [low_a + 1, low_b + 1];
            for triangle in [[low_a, high_b, low_b], [low_a, high_a, high_b]] {
                let [a, b, c] = triangle.map(|i| vertices[i as usize].position);
                if dot3_nonzero(cross(subtract(b, a), subtract(c, a))) {
                    indices.extend_from_slice(&triangle);
                }
            }
        }
    }
    MeshAsset { vertices, indices }
}

fn cylinder_mesh(segments: u32) -> MeshAsset {
    let [down, out, up] = [[0.0, -1.0], [1.0, 0.0], [0.0, 1.0]];
    lathe_mesh(
        &[
            ([0.0, -0.5], down),
            ([0.5, -0.5], down),
            ([0.5, -0.5], out),
            ([0.5, 0.5], out),
            ([0.5, 0.5], up),
            ([0.0, 0.5], up),
        ],
        segments,
    )
}

fn cone_mesh(segments: u32) -> MeshAsset {
    let down = [0.0, -1.0];
    // The side rises 1 over a run of 0.5.
    let side = [1.0, 0.5];
    lathe_mesh(
        &[
            ([0.0, -0.5], down),
            ([0.5, -0.5], down),
            ([0.5, -0.5], side),
            ([0.0, 0.5], side),
        ],
        segments,
    )
}

/// Radius 0.5 hemispheres on a cylinder from y -0.5 to 0.5.
fn capsule_mesh(segments: u32, cap_rings: u32) -> MeshAsset {
    let mut profile = Vec::new();
    for (center, from, to) in [(-0.5, -1.0, 0.0), (0.5, 0.0, 1.0)] {
        for ring in 0..=cap_rings {
            let t = from + (to - from) * ring as f32 / cap_rings as f32;
            let (sin, cos) = (t * std::f32::consts::FRAC_PI_2).sin_cos();
            profile.push(([0.5 * cos, center + 0.5 * sin], [cos, sin]));
        }
    }
    lathe_mesh(&profile, segments)
}

/// Unit box whose edges are rounded by `radius`, with `bevel_steps` bands
/// across each rounded edge.
fn rounded_cube_mesh(radius: f32, bevel_steps: u32) -> MeshAsset {
    let inner = 0.5 - radius;
    let coordinates: Vec<f32> = (0..=bevel_steps)
        .map(|step| -0.5 + radius * step as f32 / bevel_steps as f32)
        .chain(
            (0..=bevel_steps)
                .map(|step| inner + radius * step as f32 / bevel_steps as f32),
        )
        .collect();
    let size = coordinates.len() as u32;
    let axes: [[f32; 3]; 3] =
        [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let scaled = |axis: [f32; 3], by: f32| axis.map(|value| value * by);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    // (normal, u, v) with u x v = normal, so the grid winds outward.
    for (normal, u, v) in [(0, 1, 2), (1, 2, 0), (2, 0, 1)]
        .into_iter()
        .flat_map(|(n, u, v)| {
            [
                (scaled(axes[n], 1.0), axes[u], axes[v]),
                (scaled(axes[n], -1.0), axes[v], axes[u]),
            ]
        })
    {
        let base = vertices.len() as u32;
        for (i, a) in coordinates.iter().enumerate() {
            for (j, b) in coordinates.iter().enumerate() {
                let point: [f32; 3] = std::array::from_fn(|k| {
                    0.5 * normal[k] + a * u[k] + b * v[k]
                });
                let core = point.map(|value| value.clamp(-inner, inner));
                let direction = normalize3(subtract(point, core));
                vertices.push(MeshVertex {
                    position: std::array::from_fn(|k| {
                        core[k] + direction[k] * radius
                    }),
                    normal: direction,
                    uv: [
                        i as f32 / (size - 1) as f32,
                        j as f32 / (size - 1) as f32,
                    ],
                    tangent: [1.0, 0.0, 0.0, 1.0],
                    color: [1.0; 4],
                });
            }
        }
        for i in 0..size - 1 {
            for j in 0..size - 1 {
                let at = |i: u32, j: u32| base + i * size + j;
                let [a, b, c, d] =
                    [at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1)];
                for triangle in [[a, b, c], [a, c, d]] {
                    let [p, q, r] =
                        triangle.map(|i| vertices[i as usize].position);
                    if dot3_nonzero(cross(subtract(q, p), subtract(r, p))) {
                        indices.extend_from_slice(&triangle);
                    }
                }
            }
        }
    }
    MeshAsset { vertices, indices }
}

fn torus_mesh(major_segments: u32, minor_segments: u32) -> MeshAsset {
    let mut vertices =
        Vec::with_capacity((major_segments * minor_segments) as usize);
    for major in 0..major_segments {
        let u = std::f32::consts::TAU * major as f32 / major_segments as f32;
        for minor in 0..minor_segments {
            let v =
                std::f32::consts::TAU * minor as f32 / minor_segments as f32;
            let normal = [u.cos() * v.cos(), v.sin(), u.sin() * v.cos()];
            let ring = 0.36 + 0.14 * v.cos();
            vertices.push(MeshVertex {
                position: [ring * u.cos(), 0.14 * v.sin(), ring * u.sin()],
                normal,
                uv: [
                    major as f32 / major_segments as f32,
                    minor as f32 / minor_segments as f32,
                ],
                tangent: [-u.sin(), 0.0, u.cos(), 1.0],
                color: [1.0; 4],
            });
        }
    }
    let mut indices =
        Vec::with_capacity((major_segments * minor_segments * 6) as usize);
    for major in 0..major_segments {
        for minor in 0..minor_segments {
            let a = major * minor_segments + minor;
            let b = ((major + 1) % major_segments) * minor_segments + minor;
            let c = major * minor_segments + (minor + 1) % minor_segments;
            let d = ((major + 1) % major_segments) * minor_segments
                + (minor + 1) % minor_segments;
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    MeshAsset { vertices, indices }
}

fn subtract(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// False for the zero-area triangles a lathe makes at the poles.
fn dot3_nonzero(value: [f32; 3]) -> bool {
    dot(value, value) > 1e-18
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normalize3(value: [f32; 3]) -> [f32; 3] {
    let length = dot(value, value).sqrt();
    if length > f32::EPSILON {
        [value[0] / length, value[1] / length, value[2] / length]
    } else {
        [0.0, 1.0, 0.0]
    }
}

/// UV (0, 0) is the image's top-left pixel, as in glTF.
/// Flat in XZ facing +Y; the whole texture, -Z at the top.
fn plane_mesh() -> MeshAsset {
    let corners = [
        ([-0.5, 0.0, -0.5], [0.0, 0.0]),
        ([-0.5, 0.0, 0.5], [0.0, 1.0]),
        ([0.5, 0.0, 0.5], [1.0, 1.0]),
        ([0.5, 0.0, -0.5], [1.0, 0.0]),
    ];
    MeshAsset {
        vertices: corners
            .into_iter()
            .map(|(position, uv)| MeshVertex {
                position,
                normal: [0.0, 1.0, 0.0],
                uv,
                tangent: [1.0, 0.0, 0.0, 1.0],
                color: [1.0; 4],
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

fn quad_mesh() -> MeshAsset {
    let corners = [
        ([-0.5, -0.5, 0.0], [0.0, 1.0]),
        ([0.5, -0.5, 0.0], [1.0, 1.0]),
        ([0.5, 0.5, 0.0], [1.0, 0.0]),
        ([-0.5, 0.5, 0.0], [0.0, 0.0]),
    ];
    MeshAsset {
        vertices: corners
            .into_iter()
            .map(|(position, uv)| MeshVertex {
                position,
                normal: [0.0, 0.0, 1.0],
                uv,
                tangent: [1.0, 0.0, 0.0, 1.0],
                color: [1.0; 4],
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
    }
}

fn fallback_cube() -> MeshAsset {
    let faces = [
        (
            [0.0, 0.0, 1.0],
            [
                [-0.5, -0.5, 0.5],
                [0.5, -0.5, 0.5],
                [0.5, 0.5, 0.5],
                [-0.5, 0.5, 0.5],
            ],
        ),
        (
            [0.0, 0.0, -1.0],
            [
                [0.5, -0.5, -0.5],
                [-0.5, -0.5, -0.5],
                [-0.5, 0.5, -0.5],
                [0.5, 0.5, -0.5],
            ],
        ),
        (
            [1.0, 0.0, 0.0],
            [
                [0.5, -0.5, 0.5],
                [0.5, -0.5, -0.5],
                [0.5, 0.5, -0.5],
                [0.5, 0.5, 0.5],
            ],
        ),
        (
            [-1.0, 0.0, 0.0],
            [
                [-0.5, -0.5, -0.5],
                [-0.5, -0.5, 0.5],
                [-0.5, 0.5, 0.5],
                [-0.5, 0.5, -0.5],
            ],
        ),
        (
            [0.0, 1.0, 0.0],
            [
                [-0.5, 0.5, 0.5],
                [0.5, 0.5, 0.5],
                [0.5, 0.5, -0.5],
                [-0.5, 0.5, -0.5],
            ],
        ),
        (
            [0.0, -1.0, 0.0],
            [
                [-0.5, -0.5, -0.5],
                [0.5, -0.5, -0.5],
                [0.5, -0.5, 0.5],
                [-0.5, -0.5, 0.5],
            ],
        ),
    ];
    let mut vertices = Vec::with_capacity(24);
    let mut indices = Vec::with_capacity(36);
    for (normal, positions) in faces {
        let base = vertices.len() as u32;
        // glTF convention: v = 0 is the image's first row, at the top edge
        // of each side face and at the -Z edge of the top and bottom faces.
        for (position, uv) in positions.into_iter().zip([
            [0.0, 1.0],
            [1.0, 1.0],
            [1.0, 0.0],
            [0.0, 0.0],
        ]) {
            vertices.push(MeshVertex {
                position,
                normal,
                uv,
                tangent: [1.0, 0.0, 0.0, 1.0],
                color: [1.0; 4],
            });
        }
        indices.extend_from_slice(&[
            base,
            base + 1,
            base + 2,
            base,
            base + 2,
            base + 3,
        ]);
    }
    MeshAsset { vertices, indices }
}

/// How often `poll_asset_loads` checks watched files for changes.
pub const HOT_RELOAD_SCAN_INTERVAL: Duration = Duration::from_millis(500);

fn poll_asset_loads(
    mut server: ResMut<AssetServer>,
    data_types: Option<Res<DataAssetTypes>>,
    mut last_scan: Local<Option<Instant>>,
) {
    if last_scan.is_none_or(|last| last.elapsed() >= HOT_RELOAD_SCAN_INTERVAL) {
        *last_scan = Some(Instant::now());
        server.reload_changed();
        if let Some(data_types) = data_types {
            data_types.reload_changed(&mut server);
        }
    }
    server.meshes.poll_loads();
    server.textures.poll_loads();
    server.materials.poll_loads();
    server.scenes.poll_loads();
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AssetPlugin;

impl Plugin for AssetPlugin {
    fn build(&self, app: &mut App) -> Result<(), AppError> {
        app.insert_resource(AssetServer::default());
        app.add_systems(ScheduleStage::Update, poll_asset_loads);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// Matches the size table in docs/look-and-feel.md "Mesh kit".
    #[test]
    fn primitive_sizes_match_the_documented_table() {
        let size = |shape| {
            let mesh = procedural_primitive_mesh(shape);
            let mut low = [f32::MAX; 3];
            let mut high = [f32::MIN; 3];
            for vertex in &mesh.vertices {
                for axis in 0..3 {
                    low[axis] = low[axis].min(vertex.position[axis]);
                    high[axis] = high[axis].max(vertex.position[axis]);
                }
            }
            assert!(
                (0..3).all(|axis| (low[axis] + high[axis]).abs() < 1e-5),
                "{shape:?} is not centered"
            );
            [0, 1, 2]
                .map(|axis| ((high[axis] - low[axis]) * 100.0).round() / 100.0)
        };
        use PrimitiveShape::*;
        for (shape, expected) in [
            (Cube, [1.0, 1.0, 1.0]),
            (RoundedCube, [1.0, 1.0, 1.0]),
            (Sphere, [1.0, 1.0, 1.0]),
            (Cylinder, [1.0, 1.0, 1.0]),
            (Cone, [1.0, 1.0, 1.0]),
            (Capsule, [1.0, 2.0, 1.0]),
            (Pyramid, [1.0, 1.0, 1.0]),
            (Torus, [1.0, 0.28, 1.0]),
            (Plane, [1.0, 0.0, 1.0]),
            (Quad, [1.0, 1.0, 0.0]),
            (Triangle, [1.0, 1.0, 0.0]),
            (Tetrahedron, [1.0, 1.0, 1.0]),
            (Octahedron, [1.0, 1.0, 1.0]),
            (Dodecahedron, [0.93, 0.93, 0.93]),
            (Icosahedron, [0.85, 0.85, 0.85]),
        ] {
            assert_eq!(size(shape), expected, "{shape:?}");
        }
    }
    use super::*;

    #[test]
    fn primitives_wind_outward_with_unit_normals() {
        for shape in PrimitiveShape::ALL {
            let mesh = procedural_primitive_mesh(shape);
            for vertex in &mesh.vertices {
                let length = dot(vertex.normal, vertex.normal).sqrt();
                assert!((length - 1.0).abs() < 1e-4, "{shape:?}");
            }
            for triangle in mesh.indices.chunks(3) {
                let [a, b, c] =
                    [0, 1, 2].map(|i| mesh.vertices[triangle[i] as usize]);
                let face = cross(
                    subtract(b.position, a.position),
                    subtract(c.position, a.position),
                );
                let normal =
                    [0, 1, 2].map(|k| a.normal[k] + b.normal[k] + c.normal[k]);
                assert!(
                    !dot3_nonzero(face) || dot(face, normal) > 0.0,
                    "{shape:?} winds inward"
                );
            }
        }
        let capsule = procedural_primitive_mesh(PrimitiveShape::Capsule);
        let top = capsule.vertices.iter().map(|v| v.position[1]);
        assert!((top.fold(f32::MIN, f32::max) - 1.0).abs() < 1e-5);
        let cylinder = procedural_primitive_mesh(PrimitiveShape::Cylinder);
        assert!(cylinder.vertices.iter().any(|v| v.normal[1] == 0.0));
        let rounded = procedural_primitive_mesh(PrimitiveShape::RoundedCube);
        for vertex in &rounded.vertices {
            assert!(vertex.position.iter().all(|v| v.abs() <= 0.5 + 1e-5));
        }
    }

    #[cfg(feature = "gltf")]
    fn gltf_image(
        format: gltf::image::Format,
        pixels: Vec<u8>,
    ) -> gltf::image::Data {
        gltf::image::Data {
            pixels,
            format,
            width: 1,
            height: 1,
        }
    }

    /// Polls until no handle is loading, failing the test after 5 seconds.
    fn poll_until_settled(assets: &mut Assets<u32>, handles: &[Handle<u32>]) {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(5);
        while handles.iter().any(|handle| {
            assets.load_state(*handle) == Some(&LoadState::Loading)
        }) {
            assert!(std::time::Instant::now() < deadline, "load timed out");
            assets.poll_loads();
            std::thread::yield_now();
        }
    }

    #[test]
    fn cooked_mesh_with_out_of_range_index_is_rejected() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-mesh-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("broken.rmesh");
        let mesh = MeshAsset {
            vertices: vec![MeshVertex::default(); 3],
            indices: vec![0, 1, 3],
        };
        std::fs::write(&path, encode_mesh_file(&mesh).unwrap()).unwrap();

        let result = AssetServer::default().load_mesh(&path);

        assert!(matches!(result, Err(AssetError::Load { .. })), "{result:?}");
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn huge_length_prefix_fails_without_allocating() {
        // A 2^62-byte string prefix followed by three bytes.
        let mut bytes = (1u64 << 62).to_le_bytes().to_vec();
        bytes.extend(b"abc");
        assert!(deserialize_bounded::<String>(&bytes).is_err());
        let valid = bincode::serialize(&(7u32, "abc".to_owned())).unwrap();
        assert_eq!(
            deserialize_bounded::<(u32, String)>(&valid).unwrap(),
            (7, "abc".to_owned())
        );
    }

    #[test]
    fn removing_during_a_hot_reload_does_not_resurrect_the_slot() {
        let mut assets = Assets::<u32>::default();
        let handle = assets.load_async("virtual.bin", |_| Ok(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while assets.get(handle).is_none() {
            assert!(Instant::now() < deadline, "load timed out");
            assets.poll_loads();
            std::thread::yield_now();
        }
        let (sender, receiver) = std::sync::mpsc::channel::<()>();
        assets
            .reload_async(handle, move |_| {
                receiver.recv().ok();
                Ok(2)
            })
            .unwrap();
        assert_eq!(assets.remove(handle), Ok(1));
        sender.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            assets.poll_loads();
            std::thread::yield_now();
        }
        assert_eq!(assets.len(), 0);
    }

    #[test]
    fn changed_files_reload_on_workers_and_failures_keep_last_value() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-hot-reload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("tri.rmesh");
        let mesh = |count: u32| MeshAsset {
            vertices: vec![MeshVertex::default(); count as usize],
            indices: (0..count).collect(),
        };
        // Explicit timestamps: coarse filesystem clocks could otherwise hide
        // a rewrite made within the same tick.
        let write = |bytes: Vec<u8>, seconds: u64| {
            std::fs::write(&path, bytes).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(
                    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds),
                )
                .unwrap();
        };
        let settle = |server: &mut AssetServer| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while server.meshes.slots.iter().any(|slot| slot.reloading) {
                assert!(Instant::now() < deadline, "reload timed out");
                server.meshes.poll_loads();
                std::thread::yield_now();
            }
        };
        write(encode_mesh_file(&mesh(3)).unwrap(), 1_000);
        let mut server = AssetServer::default();
        let handle = server.load_mesh(&path).unwrap();
        let first_revision = server.meshes.revision(handle).unwrap();
        assert_eq!(
            server.reload_changed(),
            0,
            "unchanged file is not reloaded"
        );

        write(encode_mesh_file(&mesh(6)).unwrap(), 2_000);
        assert_eq!(server.reload_changed(), 1);
        assert_eq!(
            server.meshes.get(handle).unwrap().indices.len(),
            3,
            "old value stays visible while the worker decodes"
        );
        settle(&mut server);
        assert_eq!(server.meshes.get(handle).unwrap().indices.len(), 6);
        assert!(server.meshes.revision(handle).unwrap() > first_revision);
        assert_eq!(server.take_reloaded().len(), 1);

        write(b"not a mesh".to_vec(), 3_000);
        assert_eq!(server.reload_changed(), 1);
        settle(&mut server);
        assert_eq!(server.meshes.get(handle).unwrap().indices.len(), 6);
        assert_eq!(server.meshes.load_state(handle), Some(&LoadState::Loaded));
        assert_eq!(server.meshes.take_reload_failures().len(), 1);
        assert!(server.take_reloaded().is_empty(), "failed reload");

        std::fs::remove_file(&path).unwrap();
        assert_eq!(server.reload_changed(), 0, "deleted file keeps its value");
        assert!(server.meshes.get(handle).is_some());
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn async_loads_publish_success_failure_panic_and_discard_cancelled() {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let mut assets = Assets::<u32>::default();
        let loaded = assets
            .load_async("async/ok.bin", move |_| {
                gate.recv().unwrap();
                Ok(7)
            })
            .unwrap();
        assert_eq!(assets.load_state(loaded), Some(&LoadState::Loading));
        assert!(!assets.contains(loaded));
        assert_eq!(
            assets.load_async("async/./ok.bin", |_| Ok(99)).unwrap(),
            loaded,
            "a path already loading is deduplicated"
        );
        let failed = assets
            .load_async("async/bad.bin", |path| {
                Err(AssetError::Load {
                    path: path.to_owned(),
                    message: "corrupt".to_owned(),
                })
            })
            .unwrap();
        let panicked = assets
            .load_async("async/panic.bin", |_| panic!("decoder bug"))
            .unwrap();
        let cancelled = assets.load_async("async/gone.bin", |_| Ok(1)).unwrap();
        assert_eq!(
            assets.remove(cancelled),
            Err(AssetError::Missing(cancelled.into()))
        );

        release.send(()).unwrap();
        poll_until_settled(&mut assets, &[loaded, failed, panicked]);

        assert_eq!(assets.get(loaded), Some(&7));
        assert_eq!(assets.handle_for_path("async/ok.bin"), Some(loaded));
        assert!(matches!(
            assets.load_state(failed),
            Some(LoadState::Failed(message)) if message.contains("corrupt")
        ));
        assert!(matches!(
            assets.load_state(panicked),
            Some(LoadState::Failed(message)) if message.contains("panicked")
        ));
        assert_eq!(assets.load_state(cancelled), None);
        assert_eq!(assets.len(), 1);

        let retried = assets.load_async("async/bad.bin", |_| Ok(3)).unwrap();
        assert_ne!(retried, failed, "a failed path can be retried");
        poll_until_settled(&mut assets, &[retried]);
        assert_eq!(assets.get(retried), Some(&3));
    }

    #[test]
    #[cfg(feature = "gltf")]
    fn gltf_image_conversion_expands_every_supported_channel_layout() {
        assert_eq!(
            gltf_image_to_rgba8(&gltf_image(
                gltf::image::Format::R8,
                vec![200]
            )),
            vec![200, 200, 200, 255]
        );
        assert_eq!(
            gltf_image_to_rgba8(&gltf_image(
                gltf::image::Format::R8G8B8,
                vec![10, 20, 30]
            )),
            vec![10, 20, 30, 255]
        );
        assert_eq!(
            gltf_image_to_rgba8(&gltf_image(
                gltf::image::Format::R8G8B8A8,
                vec![10, 20, 30, 40]
            )),
            vec![10, 20, 30, 40]
        );
        assert_eq!(
            gltf_image_to_rgba8(&gltf_image(
                gltf::image::Format::R32G32B32FLOAT,
                [1.0f32, 0.5, 0.0]
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect(),
            )),
            vec![255, 127, 0, 255]
        );
    }

    #[test]
    fn stale_handle_does_not_resolve_reused_slot() {
        let mut assets = Assets::default();
        let stale = assets.insert(String::from("old"));
        assert_eq!(assets.remove(stale).unwrap(), "old");
        let current = assets.insert(String::from("new"));
        assert_eq!(stale.index(), current.index());
        assert_ne!(stale.generation(), current.generation());
        assert!(assets.get(stale).is_none());
        assert_eq!(assets.get(current).map(String::as_str), Some("new"));
    }

    #[test]
    fn handles_stay_unique_across_reuse_keys_and_exhausted_slots() {
        let mut assets = Assets::default();
        let mut seen = std::collections::HashSet::new();
        let mut handle = assets.insert(0);
        for round in 1..100 {
            assert!(seen.insert(handle), "handle {handle:?} came back");
            assets.remove(handle).unwrap();
            handle = assets.insert(round);
            assert_eq!(handle.index(), 0, "the slot is reused");
        }
        // The key round-trips through serde and `from_key`.
        let json = serde_json::to_string(&handle).unwrap();
        let back: Handle<i32> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, handle);
        assert_eq!(Handle::<i32>::from_key(handle.key()), handle);
        assert_eq!(assets.get(back), Some(&99));
        // A handle past the last slot resolves to nothing.
        assert!(assets.get(Handle::from_key(7)).is_none());

        // A slot at the last generation is retired, not wrapped.
        assets.slots[0].generation = u32::MAX;
        let last = Handle::from_key(u64::from(u32::MAX) << 32);
        assert_eq!(assets.remove(last), Ok(99));
        let next = assets.insert(100);
        assert_ne!(next.index(), 0, "an exhausted slot is not reused");
        assert!(assets.get(last).is_none());
    }

    #[test]
    fn equivalent_paths_are_deduplicated() {
        let mut assets = Assets::default();
        let first = assets
            .get_or_insert_with("assets/../assets/cube.mesh", |_| Ok(7_u32))
            .unwrap();
        let second = assets
            .get_or_insert_with("./assets/cube.mesh", |_| Ok(9_u32))
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(assets.get(first), Some(&7));
    }

    #[test]
    fn real_files_dedupe_by_canonical_path_and_reload_after_removal() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-asset-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("meshes")).unwrap();
        std::fs::write(folder.join("meshes/cube.mesh"), "cube").unwrap();
        std::fs::write(folder.join("meshes/ball.mesh"), "ball").unwrap();
        let mut assets = Assets::default();
        let loads = std::cell::Cell::new(0);
        let load = |assets: &mut Assets<i32>, path: &str| {
            assets
                .get_or_insert_with(folder.join(path), |_| {
                    loads.set(loads.get() + 1);
                    Ok(loads.get())
                })
                .unwrap()
        };
        let cube = load(&mut assets, "meshes/cube.mesh");
        assert_eq!(load(&mut assets, "meshes/../meshes/./cube.mesh"), cube);
        assert_eq!(load(&mut assets, "./meshes//cube.mesh"), cube);
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                folder.join("meshes"),
                folder.join("linked"),
            )
            .unwrap();
            assert_eq!(
                load(&mut assets, "linked/cube.mesh"),
                cube,
                "symlinks resolve"
            );
        }
        let ball = load(&mut assets, "meshes/ball.mesh");
        assert_ne!(ball, cube);
        assert_eq!(loads.get(), 2, "each file loads once");

        assets.remove(cube).unwrap();
        let again = load(&mut assets, "meshes/cube.mesh");
        assert_ne!(again, cube, "a removed asset loads into a new handle");
        assert_eq!(assets.get(again), Some(&3));
        assert!(matches!(
            assets.get_or_insert_with("", |_| Ok(0)),
            Err(AssetError::EmptyPath)
        ));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn referenced_assets_cannot_be_removed() {
        let mut assets = Assets::default();
        let handle = assets.insert(42);
        assets.retain(handle).unwrap();
        assert!(matches!(
            assets.remove(handle),
            Err(AssetError::StillReferenced { .. })
        ));
        assets.release(handle).unwrap();
        assert_eq!(assets.remove(handle), Ok(42));
    }

    #[test]
    fn retired_assets_wait_for_safe_frame() {
        let mut assets = Assets::default();
        let handle = assets.insert(42);
        assets.retire(handle, 5).unwrap();
        assert_eq!(assets.collect_retired(4), 0);
        assert_eq!(assets.collect_retired(5), 1);
    }

    #[test]
    fn mutable_access_advances_asset_revision() {
        let mut assets = Assets::default();
        let handle = assets.insert(1_u32);
        let initial = assets.revision(handle).unwrap();
        *assets.get_mut(handle).unwrap() = 2;
        assert!(assets.revision(handle).unwrap() > initial);
        assert_eq!(assets.get(handle), Some(&2));
    }

    #[test]
    fn mesh_bounds_are_cached_until_the_mesh_changes() {
        let mut server = AssetServer::default();
        let vertex = |x| MeshVertex {
            position: [x, 0.0, 0.0],
            ..MeshVertex::default()
        };
        let handle = server.meshes.insert(MeshAsset {
            vertices: vec![vertex(-1.0), vertex(2.0)],
            indices: Vec::new(),
        });
        assert_eq!(
            server.mesh_bounds(handle),
            Some(([-1.0, 0.0, 0.0], [2.0, 0.0, 0.0]))
        );
        server
            .meshes
            .get_mut(handle)
            .unwrap()
            .vertices
            .push(vertex(5.0));
        assert_eq!(server.mesh_bounds(handle).unwrap().1, [5.0, 0.0, 0.0]);
    }

    #[test]
    #[cfg(feature = "gltf")]
    fn gltf_import_carries_sampler_state_and_alpha_mode() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-gltf-sampler-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        image::RgbaImage::from_raw(1, 1, vec![1, 2, 3, 255])
            .unwrap()
            .save(folder.join("pixel.png"))
            .unwrap();
        let positions: Vec<u8> =
            [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
        std::fs::write(folder.join("tri.bin"), &positions).unwrap();
        let path = folder.join("tri.gltf");
        std::fs::write(
            &path,
            r#"{
              "asset": {"version": "2.0"},
              "buffers": [{"uri": "tri.bin", "byteLength": 36}],
              "bufferViews": [{"buffer": 0, "byteLength": 36}],
              "accessors": [{"bufferView": 0, "componentType": 5126,
                "count": 3, "type": "VEC3",
                "min": [0, 0, 0], "max": [1, 1, 0]}],
              "images": [{"uri": "pixel.png"}],
              "samplers": [{"magFilter": 9728, "minFilter": 9986,
                "wrapS": 33071, "wrapT": 33648}],
              "textures": [{"source": 0, "sampler": 0}, {"source": 0}],
              "materials": [{
                "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
                "emissiveTexture": {"index": 1},
                "alphaMode": "MASK", "alphaCutoff": 0.25}],
              "meshes": [{"primitives": [
                {"attributes": {"POSITION": 0}, "material": 0}]}]
            }"#,
        )
        .unwrap();
        let mut server = AssetServer::default();

        let imported = server.import_gltf(&path).unwrap();

        let material = server.materials.get(imported[0].material).unwrap();
        assert_eq!(material.alpha_mode, AlphaMode::Mask { cutoff: 0.25 });
        let sampled = material.base_color_texture.unwrap();
        let default = material.emissive_texture.unwrap();
        assert_ne!(
            sampled, default,
            "sampler state is part of texture identity"
        );
        assert_eq!(
            server.textures.get(sampled).unwrap().sampler,
            TextureSampler {
                mag_filter: TextureFilter::Nearest,
                min_filter: TextureFilter::Nearest,
                mipmap_filter: TextureFilter::Linear,
                wrap: [TextureWrap::ClampToEdge, TextureWrap::MirroredRepeat],
            }
        );
        assert_eq!(
            server.textures.get(default).unwrap().sampler,
            TextureSampler::default()
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    #[cfg(feature = "gltf")]
    fn gltf_import_keeps_vertex_colors_through_the_cooked_file() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-gltf-colors-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let colors = [
            [1.0f32, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 0.5],
            [0.0, 0.0, 1.0, 0.25],
        ];
        let bytes: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .chain(colors.as_flattened())
            .flat_map(|value| value.to_le_bytes())
            .collect();
        std::fs::write(folder.join("tri.bin"), &bytes).unwrap();
        let path = folder.join("tri.gltf");
        std::fs::write(
            &path,
            r#"{
              "asset": {"version": "2.0"},
              "buffers": [{"uri": "tri.bin", "byteLength": 84}],
              "bufferViews": [{"buffer": 0, "byteLength": 36},
                {"buffer": 0, "byteOffset": 36, "byteLength": 48}],
              "accessors": [{"bufferView": 0, "componentType": 5126,
                "count": 3, "type": "VEC3",
                "min": [0, 0, 0], "max": [1, 1, 0]},
                {"bufferView": 1, "componentType": 5126,
                "count": 3, "type": "VEC4"}],
              "meshes": [{"primitives": [
                {"attributes": {"POSITION": 0, "COLOR_0": 1}}]}]
            }"#,
        )
        .unwrap();
        let mut server = AssetServer::default();

        let imported = server.import_gltf(&path).unwrap();
        let mut fresh = AssetServer::default();
        let reloaded =
            fresh.load_mesh(folder.join("tri.mesh-0-0.rmesh")).unwrap();

        let mesh = server.meshes.get(imported[0].mesh).unwrap();
        // Flat shading unshares the vertices, so read them per corner.
        let corner_colors = mesh
            .indices
            .iter()
            .map(|&index| mesh.vertices[index as usize].color)
            .collect::<Vec<_>>();
        assert_eq!(corner_colors, colors);
        assert_eq!(fresh.meshes.get(reloaded), Some(mesh));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[cfg(feature = "gltf")]
    #[test]
    fn gltf_scene_import_spawns_hierarchy_cameras_and_lights() {
        use crate::runtime::Parent;
        use crate::Transform;

        let folder = std::env::temp_dir()
            .join(format!("rusting-gltf-scene-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let positions: Vec<u8> =
            [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]
                .iter()
                .flat_map(|value| value.to_le_bytes())
                .collect();
        std::fs::write(folder.join("tri.bin"), &positions).unwrap();
        let path = folder.join("scene.gltf");
        // Node 0 (rotated 90 degrees about Y) carries a two-primitive mesh
        // and parents node 1 (camera) and node 2 (spot light).
        std::fs::write(
            &path,
            r#"{
              "asset": {"version": "2.0"},
              "extensionsUsed": ["KHR_lights_punctual"],
              "extensions": {"KHR_lights_punctual": {"lights": [
                {"type": "spot", "color": [1, 0.5, 0], "intensity": 40,
                 "range": 7,
                 "spot": {"innerConeAngle": 0.2, "outerConeAngle": 0.6}}]}},
              "buffers": [{"uri": "tri.bin", "byteLength": 36}],
              "bufferViews": [{"buffer": 0, "byteLength": 36}],
              "accessors": [{"bufferView": 0, "componentType": 5126,
                "count": 3, "type": "VEC3",
                "min": [0, 0, 0], "max": [1, 1, 0]}],
              "cameras": [{"type": "orthographic", "orthographic":
                {"xmag": 2, "ymag": 1.5, "znear": 0.5, "zfar": 50}}],
              "meshes": [{"primitives": [
                {"attributes": {"POSITION": 0}},
                {"attributes": {"POSITION": 0}}]}],
              "nodes": [
                {"name": "Root", "mesh": 0, "children": [1, 2],
                 "translation": [1, 2, 3],
                 "rotation": [0, 0.70710677, 0, 0.70710677]},
                {"name": "Eye", "camera": 0, "scale": [2, 2, 2]},
                {"extensions": {"KHR_lights_punctual": {"light": 0}}}],
              "scenes": [{"nodes": [0]}]
            }"#,
        )
        .unwrap();
        let mut server = AssetServer::default();

        let nodes = server.import_gltf_scene(&path).unwrap();

        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].name, "Root");
        assert_eq!(nodes[2].name, "Node 2");
        assert_eq!(
            nodes.iter().map(|node| node.parent).collect::<Vec<_>>(),
            [None, Some(0), Some(0)]
        );
        assert_eq!(nodes[0].transform.position, [1.0, 2.0, 3.0]);
        let [x, y, z] = nodes[0].transform.rotation;
        assert!(x.abs() < 1.0e-5 && z.abs() < 1.0e-5);
        assert!((y - std::f32::consts::FRAC_PI_2).abs() < 1.0e-3);
        assert_eq!(nodes[1].transform.scale, [2.0; 3]);
        assert_eq!(nodes[0].primitives.len(), 2);
        assert_eq!(
            nodes[1].camera.unwrap().projection,
            crate::runtime::Projection::Orthographic {
                vertical_size: 3.0,
                near: 0.5,
                far: 50.0
            }
        );
        assert_eq!(
            nodes[2].light,
            Some(ImportedGltfLight::Spot(SpotLight {
                color: [1.0, 0.5, 0.0],
                intensity: 40.0,
                range: 7.0,
                inner_angle: 0.2,
                outer_angle: 0.6,
                shadows: false,
                radius: 0.0,
            }))
        );

        let mut app = App::new();
        let entities = spawn_gltf_nodes(&mut app, &nodes, None).unwrap();
        let world = app.world_mut();
        let root = entities[0];
        assert_eq!(world.get::<Name>(root).unwrap().0, "Root");
        assert!(world.get::<MeshRenderer>(root).is_some());
        assert_eq!(world.get::<Transform>(root), Some(&nodes[0].transform));
        assert_eq!(world.get::<Parent>(entities[1]), Some(&Parent(root)));
        assert!(world.get::<Camera>(entities[1]).is_some());
        assert!(world.get::<SpotLight>(entities[2]).is_some());
        let extra_primitives = world
            .query::<(&Parent, &MeshRenderer)>()
            .iter(world)
            .filter(|(parent, _)| parent.0 == root)
            .count();
        assert_eq!(extra_primitives, 1, "second primitive is a child entity");
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn cube_side_faces_show_the_first_image_row_at_the_top() {
        let cube = fallback_cube();
        for vertex in &cube.vertices {
            if vertex.normal[1] == 0.0 {
                let top = vertex.position[1] > 0.0;
                assert_eq!(vertex.uv[1], if top { 0.0 } else { 1.0 });
            }
        }
    }

    #[test]
    fn generated_tangents_follow_uvs_handedness_and_degenerate_fallback() {
        let quad = |flip_v: bool, uv_scale: f32| {
            let v = |y: f32| if flip_v { 1.0 - y } else { y } * uv_scale;
            [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
                .map(|[x, y]: [f32; 2]| MeshVertex {
                    position: [x, y, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [x * uv_scale, v(y)],
                    tangent: [0.0; 4],
                    color: [1.0; 4],
                })
                .to_vec()
        };
        let indices = [0, 1, 2, 0, 2, 3];

        let mut right_handed = quad(false, 1.0);
        generate_tangents(&mut right_handed, &indices);
        let mut flipped = quad(true, 1.0);
        generate_tangents(&mut flipped, &indices);
        let mut degenerate = quad(false, 0.0);
        generate_tangents(&mut degenerate, &indices);

        for vertex in &right_handed {
            assert_eq!(vertex.tangent, [1.0, 0.0, 0.0, 1.0]);
        }
        for vertex in &flipped {
            assert_eq!(vertex.tangent, [1.0, 0.0, 0.0, -1.0]);
        }
        for vertex in &degenerate {
            let [x, y, z, _] = vertex.tangent;
            assert!(z.abs() < 1.0e-6, "tangent is perpendicular to normal");
            assert!(((x * x + y * y) - 1.0).abs() < 1.0e-6, "unit length");
        }
    }

    #[test]
    fn texture_loader_decodes_and_deduplicates_image_files() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-texture-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("pixel.png");
        image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 255])
            .unwrap()
            .save(&path)
            .unwrap();
        let mut server = AssetServer::default();

        let first = server.load_texture(&path).unwrap();
        let second = server.load_texture(&path).unwrap();

        assert_eq!(first, second);
        assert_eq!(server.textures.get(first).unwrap().size, [1, 1]);
        assert_eq!(
            server.textures.get(first).unwrap().rgba8,
            [12, 34, 56, 255]
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn native_mesh_loader_restores_imported_mesh_data() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-mesh-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("triangle.rmesh");
        let tinted = MeshVertex {
            color: [0.2, 0.5, 0.9, 0.75],
            ..MeshVertex::default()
        };
        let mesh = MeshAsset {
            vertices: vec![tinted, MeshVertex::default(), tinted],
            indices: vec![0, 1, 2],
        };
        std::fs::write(&path, encode_mesh_file(&mesh).unwrap()).unwrap();
        let mut server = AssetServer::default();

        let handle = server.load_mesh(&path).unwrap();

        assert_eq!(server.meshes.get(handle), Some(&mesh));
        assert_eq!(server.load_mesh(&path).unwrap(), handle);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn rmesh_files_cooked_before_vertex_colors_load_white() {
        #[derive(Serialize)]
        struct OldVertex {
            position: [f32; 3],
            normal: [f32; 3],
            uv: [f32; 2],
            tangent: [f32; 4],
        }
        let folder = std::env::temp_dir()
            .join(format!("rusting-mesh-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("old.rmesh");
        let old = (0..3)
            .map(|index| OldVertex {
                position: [index as f32, 2.0, 3.0],
                normal: [0.0, 1.0, 0.0],
                uv: [0.5, index as f32],
                tangent: [1.0, 0.0, 0.0, -1.0],
            })
            .collect::<Vec<_>>();
        std::fs::write(
            &path,
            bincode::serialize(&(&old, vec![0u32, 1, 2])).unwrap(),
        )
        .unwrap();
        let mut server = AssetServer::default();

        let handle = server.load_mesh(&path).unwrap();

        let mesh = server.meshes.get(handle).unwrap();
        assert_eq!(mesh.indices, [0, 1, 2]);
        for (vertex, old) in mesh.vertices.iter().zip(&old) {
            assert_eq!(vertex.position, old.position);
            assert_eq!(vertex.normal, old.normal);
            assert_eq!(vertex.uv, old.uv);
            assert_eq!(vertex.tangent, old.tangent);
            assert_eq!(vertex.color, [1.0; 4]);
        }
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn every_builtin_primitive_has_valid_triangle_geometry() {
        let server = AssetServer::default();
        for shape in PrimitiveShape::ALL {
            let handle = server.builtin_primitive(shape);
            let mesh = server.meshes.get(handle).unwrap();
            assert!(
                !mesh.vertices.is_empty(),
                "{} has no vertices",
                shape.label()
            );
            assert_eq!(
                mesh.indices.len() % 3,
                0,
                "{} is not triangulated",
                shape.label()
            );
            assert!(
                mesh.indices
                    .iter()
                    .all(|index| (*index as usize) < mesh.vertices.len()),
                "{} contains an invalid index",
                shape.label()
            );
            assert_eq!(server.primitive_for_handle(handle), Some(shape));
        }
    }

    #[test]
    fn lod_group_beside_a_mesh_loads_with_it_and_rejects_bad_hand_overs() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-lod-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(folder.join("lods")).unwrap();
        let mesh = |count: u32| MeshAsset {
            vertices: vec![MeshVertex::default(); count as usize],
            indices: (0..count).collect(),
        };
        let write_mesh = |name: &str, count| {
            std::fs::write(
                folder.join(name),
                encode_mesh_file(&mesh(count)).unwrap(),
            )
            .unwrap();
        };
        write_mesh("rock.rmesh", 6);
        write_mesh("lods/rock_1.rmesh", 3);
        std::fs::write(
            folder.join("rock.rlod"),
            r#"{"metric": "ScreenSize", "levels": [
                {"mesh": "rock.rmesh", "until": 0.5},
                {"mesh": "lods/rock_1.rmesh"}
            ]}"#,
        )
        .unwrap();
        let mut server = AssetServer::default();
        let rock = server.load_mesh(folder.join("rock.rmesh")).unwrap();
        let (_, group) = server.lod_groups.iter().next().unwrap();
        assert_eq!(group.metric, LodMetric::ScreenSize);
        assert_eq!(group.levels[0].mesh, rock, "paths are relative to it");
        let coarse = group.levels[1].mesh;
        assert_eq!(server.meshes.get(coarse).unwrap().indices.len(), 3);
        // A missing `until` draws at any range.
        assert_eq!(group.ranges(), [[0.0, 2.0], [2.0, f32::INFINITY]]);
        // Loading the mesh again does not add the group twice.
        server.load_mesh(folder.join("rock.rmesh")).unwrap();
        assert_eq!(server.lod_groups.len(), 1);

        let distance = LodGroupAsset {
            metric: LodMetric::Distance,
            levels: vec![
                LodLevel {
                    mesh: rock,
                    until: 7.0,
                },
                LodLevel {
                    mesh: coarse,
                    until: 11.0,
                },
            ],
        };
        assert_eq!(distance.ranges(), [[0.0, 7.0], [7.0, 11.0]]);
        std::fs::write(
            folder.join("bad.rlod"),
            r#"{"metric": "Distance", "levels": [
                {"mesh": "rock.rmesh", "until": 10},
                {"mesh": "lods/rock_1.rmesh", "until": 5}
            ]}"#,
        )
        .unwrap();
        assert!(server.load_lod_group(folder.join("bad.rlod")).is_err());
        std::fs::write(folder.join("empty.rlod"), r#"{"levels": []}"#).unwrap();
        assert!(server.load_lod_group(folder.join("empty.rlod")).is_err());
        let level = r#"{"mesh": "rock.rmesh"}"#;
        std::fs::write(
            folder.join("many.rlod"),
            format!(r#"{{"levels": [{}]}}"#, [level; 5].join(",")),
        )
        .unwrap();
        let many = server.load_lod_group(folder.join("many.rlod"));
        assert!(many.unwrap_err().to_string().contains("at most 4 levels"));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn every_primitive_of_a_skinned_node_gets_the_skin() {
        let server = AssetServer::default();
        let primitive = ImportedGltfPrimitive {
            name: "Part".into(),
            mesh: server.fallback_mesh,
            material: server.fallback_material,
        };
        let node = |name: &str, parent| ImportedGltfNode {
            name: name.into(),
            parent,
            transform: crate::Transform::default(),
            primitives: Vec::new(),
            camera: None,
            light: None,
            skin: None,
            morph_weights: None,
        };
        let nodes = vec![
            node("Rig", None),
            ImportedGltfNode {
                primitives: vec![primitive.clone(), primitive],
                skin: Some(ImportedGltfSkin {
                    joints: vec![2],
                    inverse_bind: vec![[[0.0; 4]; 4]],
                }),
                ..node("Body", Some(0))
            },
            node("Bone", Some(0)),
        ];
        let mut world = bevy_ecs::world::World::new();
        let entities =
            spawn_gltf_nodes_in_world(&mut world, &nodes, None).expect("spawn");
        let part = world
            .get::<crate::runtime::Children>(entities[1])
            .unwrap()
            .0[0];
        let joints =
            |entity| world.get::<crate::runtime::Skin>(entity).unwrap();
        assert_eq!(joints(entities[1]).joints, ["../Bone"]);
        assert_eq!(joints(part).joints, ["../../Bone"]);
        assert_eq!(
            crate::runtime::find_target(&world, part, "../../Bone"),
            Some(entities[2])
        );
    }
}
