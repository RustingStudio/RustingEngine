//! Mods: folders or `.rmod` archives in a game's `mods/` folder that add
//! or replace the game's assets and scenes.
//!
//! A mod holds a [`MOD_MANIFEST`] and `assets/` and `scenes/` folders laid
//! out like the game's own. `rusting mod pack` packs one into a `.rmod`
//! file, the [`crate::assets::pack_folder`] format. When a game starts it
//! mounts every mod in `mods/` beside its `build/` folder, in load order:
//! a file a mod holds is read in place of the game's, and a later mod wins
//! over an earlier one.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use serde::{Deserialize, Serialize};

use crate::assets::PackArchive;

/// The manifest file at the top of every mod.
pub const MOD_MANIFEST: &str = "mod.json";
/// The extension of a packed mod.
pub const MOD_EXTENSION: &str = "rmod";
/// Optional list of mod names in `mods/`: only these load, in this order
/// where dependencies allow.
pub const LOAD_ORDER_FILE: &str = "load_order.json";
/// The manifest format this engine reads.
pub const MOD_FORMAT_VERSION: u32 = 1;

/// `mod.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModManifest {
    pub format_version: u32,
    /// Unique among the game's mods; other mods depend on it by this name.
    pub name: String,
    /// `major.minor.patch`.
    pub version: String,
    pub description: String,
    /// Mod name to the lowest version it needs, such as `"1.2"`: any
    /// version from 1.2.0 below 2.0.0 (below 0.3.0 for `"0.2"`).
    pub depends: BTreeMap<String, String>,
}

/// Why a mod did not load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModError {
    /// `MOD_MANIFEST_INVALID`, `MOD_DEPENDENCY_UNMET` or
    /// `MOD_DEPENDENCY_CYCLE`.
    pub code: &'static str,
    pub path: PathBuf,
    pub message: String,
}

/// An opened mod: its manifest and files.
#[derive(Debug)]
pub struct Mod {
    pub manifest: ModManifest,
    /// The folder or `.rmod` file.
    pub path: PathBuf,
    pack: Option<PackArchive>,
}

impl Mod {
    /// Opens a mod folder or `.rmod` file and checks its manifest.
    ///
    /// # Errors
    /// `MOD_MANIFEST_INVALID` when the manifest is missing or wrong.
    pub fn open(path: &Path) -> Result<Self, ModError> {
        let invalid = |message: String| ModError {
            code: "MOD_MANIFEST_INVALID",
            path: path.to_owned(),
            message,
        };
        let pack = if path.is_dir() {
            None
        } else {
            Some(PackArchive::open(path).map_err(|error| {
                invalid(format!("cannot open the archive: {error}"))
            })?)
        };
        let mut opened = Self {
            manifest: ModManifest::default(),
            path: path.to_owned(),
            pack,
        };
        let bytes = opened.read(MOD_MANIFEST).map_err(|error| {
            invalid(format!("cannot read {MOD_MANIFEST}: {error}"))
        })?;
        let manifest: ModManifest = serde_json::from_slice(&bytes)
            .map_err(|error| invalid(format!("{MOD_MANIFEST}: {error}")))?;
        if manifest.format_version != MOD_FORMAT_VERSION {
            return Err(invalid(format!(
                "format_version {} is not {MOD_FORMAT_VERSION}",
                manifest.format_version
            )));
        }
        let named = !manifest.name.is_empty()
            && manifest.name.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
            });
        if !named {
            return Err(invalid(format!(
                "name `{}` must be letters, digits, `_`, `-` or `.`",
                manifest.name
            )));
        }
        if version(&manifest.version).is_none() {
            return Err(invalid(format!(
                "version `{}` is not major.minor.patch",
                manifest.version
            )));
        }
        if let Some((name, wanted)) = manifest
            .depends
            .iter()
            .find(|(_, wanted)| version(wanted).is_none())
        {
            return Err(invalid(format!(
                "dependency `{name}` wants `{wanted}`, not a version"
            )));
        }
        opened.manifest = manifest;
        Ok(opened)
    }

    /// Reads a file of the mod by its `/`-separated path, such as
    /// `assets/hero.png`.
    ///
    /// # Errors
    /// Returns the read error, `NotFound` when the mod has no such file.
    pub fn read(&self, entry: &str) -> io::Result<Vec<u8>> {
        match &self.pack {
            Some(pack) => pack.read(entry),
            None => std::fs::read(self.path.join(entry)),
        }
    }

    /// The mod's files under `assets/` and `scenes/`, `/`-separated and
    /// sorted.
    #[must_use]
    pub fn files(&self) -> Vec<String> {
        let mut files: Vec<String> = match &self.pack {
            Some(pack) => pack.paths().map(str::to_owned).collect(),
            None => {
                let mut files = Vec::new();
                let mut pending = vec![self.path.clone()];
                while let Some(dir) = pending.pop() {
                    for entry in
                        std::fs::read_dir(&dir).into_iter().flatten().flatten()
                    {
                        let path = entry.path();
                        // Not followed into linked folders, which can loop.
                        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                            pending.push(path);
                        } else if let Some(entry) = path
                            .strip_prefix(&self.path)
                            .ok()
                            .and_then(Path::to_str)
                        {
                            files.push(entry.replace('\\', "/"));
                        }
                    }
                }
                files
            }
        };
        files.retain(|file| {
            file.starts_with("assets/") || file.starts_with("scenes/")
        });
        files.sort();
        files
    }

    /// The size of one of the mod's files.
    #[must_use]
    pub fn len_of(&self, entry: &str) -> Option<u64> {
        match &self.pack {
            Some(pack) => pack.len_of(entry),
            None => std::fs::metadata(self.path.join(entry))
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|meta| meta.len()),
        }
    }
}

/// `major.minor.patch`, missing parts 0.
fn version(text: &str) -> Option<[u64; 3]> {
    let mut parts = [0; 3];
    let mut count = 0;
    for (slot, part) in parts.iter_mut().zip(text.split('.')) {
        *slot = part.parse().ok()?;
        count += 1;
    }
    (count >= 1 && text.split('.').count() == count).then_some(parts)
}

/// Whether `have` meets the requirement `wanted`: at least it, with the
/// same major version (the same minor for `0.x`).
fn satisfies(have: &str, wanted: &str) -> bool {
    let (Some(have), Some(wanted)) = (version(have), version(wanted)) else {
        return false;
    };
    let same = if wanted[0] == 0 {
        have[..2] == wanted[..2]
    } else {
        have[0] == wanted[0]
    };
    same && have >= wanted
}

/// The mods in `folder` that load, in load order, and why the others did
/// not.
#[derive(Debug, Default)]
pub struct ModSet {
    pub mods: Vec<Mod>,
    pub errors: Vec<ModError>,
}

/// Opens the mods in `folder`: each subfolder with a [`MOD_MANIFEST`] and
/// each `.rmod` file. With a [`LOAD_ORDER_FILE`] only the mods it names
/// load, in its order; otherwise all of them, by name. Every mod comes
/// after the ones it depends on. A mod whose manifest is wrong, whose
/// dependency is missing, disabled or the wrong version, or that is in a
/// dependency cycle is left out with an error, and so is each mod that
/// depends on it. A missing folder gives an empty set.
#[must_use]
pub fn load_mods(folder: &Path) -> ModSet {
    load_mods_with(folder, &[])
}

/// [`load_mods`] with more mods from elsewhere, such as the platform's
/// [`crate::platform::workshop_items`]. They take part in the load order
/// by name like the folder's own; a mod in `folder` wins a name clash.
#[must_use]
pub fn load_mods_with(folder: &Path, extra: &[PathBuf]) -> ModSet {
    let mut set = ModSet::default();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.join(MOD_MANIFEST).is_file()
                || path.extension().is_some_and(|ext| ext == MOD_EXTENSION)
        })
        .collect();
    paths.sort();
    paths.extend_from_slice(extra);
    if paths.is_empty() {
        return set;
    }
    let mut found: BTreeMap<String, Mod> = BTreeMap::new();
    for path in paths {
        match Mod::open(&path) {
            Ok(opened) => {
                if let Some(first) = found.get(&opened.manifest.name) {
                    set.errors.push(ModError {
                        code: "MOD_MANIFEST_INVALID",
                        message: format!(
                            "`{}` is also the name of {}",
                            opened.manifest.name,
                            first.path.display()
                        ),
                        path,
                    });
                } else {
                    found.insert(opened.manifest.name.clone(), opened);
                }
            }
            Err(error) => set.errors.push(error),
        }
    }
    let order_path = folder.join(LOAD_ORDER_FILE);
    let preference: Vec<String> = match std::fs::read(&order_path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(names) => names,
            Err(error) => {
                set.errors.push(ModError {
                    code: "MOD_MANIFEST_INVALID",
                    path: order_path,
                    message: format!("{LOAD_ORDER_FILE}: {error}"),
                });
                return set;
            }
        },
        Err(_) => found.keys().cloned().collect(),
    };
    let mut enabled: Vec<Mod> = preference
        .iter()
        .filter_map(|name| found.remove(name))
        .collect();
    // Drop mods with an unmet dependency until none is left, so a mod
    // that needs a dropped one is dropped too.
    loop {
        let versions: BTreeMap<String, String> = enabled
            .iter()
            .map(|m| (m.manifest.name.clone(), m.manifest.version.clone()))
            .collect();
        let unmet = enabled.iter().position(|m| {
            m.manifest.depends.iter().any(|(name, wanted)| {
                !versions
                    .get(name)
                    .is_some_and(|have| satisfies(have, wanted))
            })
        });
        let Some(index) = unmet else { break };
        let dropped = enabled.remove(index);
        let (name, wanted) = dropped
            .manifest
            .depends
            .iter()
            .find(|(name, wanted)| {
                !versions
                    .get(*name)
                    .is_some_and(|have| satisfies(have, wanted))
            })
            .map(|(name, wanted)| (name.clone(), wanted.clone()))
            .unwrap_or_default();
        let message = match versions.get(&name) {
            Some(have) => format!(
                "`{}` needs `{name}` {wanted}, but {have} is installed",
                dropped.manifest.name
            ),
            None => format!(
                "`{}` needs `{name}` {wanted}, which is not installed or not enabled",
                dropped.manifest.name
            ),
        };
        set.errors.push(ModError {
            code: "MOD_DEPENDENCY_UNMET",
            path: dropped.path,
            message,
        });
    }
    // Stable topological order: each pass takes the first mod, in the
    // preferred order, whose dependencies are all placed.
    let mut placed = BTreeSet::new();
    while let Some(index) = enabled.iter().position(|m| {
        m.manifest.depends.keys().all(|name| placed.contains(name))
    }) {
        let next = enabled.remove(index);
        placed.insert(next.manifest.name.clone());
        set.mods.push(next);
    }
    for left in enabled {
        set.errors.push(ModError {
            code: "MOD_DEPENDENCY_CYCLE",
            message: format!(
                "`{}` is in a dependency cycle",
                left.manifest.name
            ),
            path: left.path,
        });
    }
    set
}

/// A game file that more than one loaded mod holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ModConflict {
    /// `/`-separated, such as `assets/hero.png`.
    pub file: String,
    /// The mods holding it, in load order; the last one's file is read.
    pub mods: Vec<String>,
    /// For a `.rscene`, the fields two of the mods changed from the game's
    /// scene in different ways. Only the last mod's changes take effect.
    pub fields: Vec<FieldConflict>,
}

/// A scene field two mods changed differently.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FieldConflict {
    /// Entity ID, or `None` for a scene-level field.
    pub entity: Option<String>,
    pub name: Option<String>,
    /// Such as `transform`, `components/rusting.light/intensity`, or
    /// `entity` when one mod removed the entity and the other changed it.
    pub field: String,
    /// The earlier and the later mod in load order.
    pub mods: [String; 2],
}

/// The files of the game at `root` that more than one of `mods` (in load
/// order) holds, and for scenes each field that two of them changed
/// differently from the game's own scene (an empty scene when the game
/// has none). A scene file that is not JSON gets no field detail.
#[must_use]
pub fn conflicts(root: &Path, mods: &[Mod]) -> Vec<ModConflict> {
    let scene = |bytes: io::Result<Vec<u8>>| -> Option<serde_json::Value> {
        serde_json::from_slice(&bytes.ok()?).ok()
    };
    let mut found = Vec::new();
    for (file, holding) in shared_files(mods) {
        let mut fields = Vec::new();
        // Only regular files of a known, small size are read: a link to
        // `/dev/zero` or a pipe has no length and is never compared.
        let small = |len: u64| len <= MAX_COMPARED_SCENE;
        let base_len = crate::assets::file_len(&root.join(&file));
        if file.ends_with(".rscene")
            && base_len.is_none_or(small)
            && holding.iter().all(|m| m.len_of(&file).is_some_and(small))
        {
            let base = base_len
                .and_then(|_| {
                    scene(crate::assets::read_unmodded(&root.join(&file)))
                })
                .unwrap_or_else(|| serde_json::json!({}));
            let scenes: Vec<_> =
                holding.iter().map(|m| scene(m.read(&file))).collect();
            for (i, earlier) in holding.iter().enumerate() {
                for (j, later) in holding.iter().enumerate().skip(i + 1) {
                    let (Some(a), Some(b)) = (&scenes[i], &scenes[j]) else {
                        continue;
                    };
                    let pair = [
                        earlier.manifest.name.clone(),
                        later.manifest.name.clone(),
                    ];
                    for conflict in
                        crate::scene_merge::merge_scenes(&base, a, b).1
                    {
                        for field in component_fields(&base, a, b, &conflict) {
                            fields.push(FieldConflict {
                                entity: conflict.entity.clone(),
                                name: conflict.name.clone(),
                                field,
                                mods: pair.clone(),
                            });
                        }
                    }
                }
            }
        }
        found.push(ModConflict {
            file,
            mods: holding.iter().map(|m| m.manifest.name.clone()).collect(),
            fields,
        });
    }
    found
}

/// Scenes larger than this get no field detail in [`conflicts`]: mods
/// are untrusted, and the comparison grows with the square of the entity
/// count.
const MAX_COMPARED_SCENE: u64 = 4 << 20;

/// Each file more than one of `mods` holds, with the mods holding it.
fn shared_files(mods: &[Mod]) -> Vec<(String, Vec<&Mod>)> {
    let mut holders: BTreeMap<String, Vec<&Mod>> = BTreeMap::new();
    for held in mods {
        for file in held.files() {
            holders.entry(file).or_default().push(held);
        }
    }
    holders
        .into_iter()
        .filter(|(_, holding)| holding.len() > 1)
        .collect()
}

/// `text` with control characters, which a mod's file names and manifest
/// can carry to move a terminal's cursor or recolor it, shown as `?`.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Narrows a `components/NAME` conflict to the component's own fields
/// both sides changed differently, none when they changed different
/// fields; other conflicts stay as they are.
fn component_fields(
    base: &serde_json::Value,
    a: &serde_json::Value,
    b: &serde_json::Value,
    conflict: &crate::scene_merge::MergeConflict,
) -> Vec<String> {
    let whole = vec![conflict.field.clone()];
    let (Some(component), Some(id)) =
        (conflict.field.strip_prefix("components/"), &conflict.entity)
    else {
        return whole;
    };
    let value = |scene: &serde_json::Value| {
        scene["entities"]
            .as_array()
            .and_then(|list| list.iter().find(|e| e["id"] == id.as_str()))
            .map(|e| e["components"][component].clone())
            .unwrap_or_default()
    };
    let (base, a, b) = (value(base), value(a), value(b));
    let (Some(a_map), Some(b_map)) = (a.as_object(), b.as_object()) else {
        return whole;
    };
    let null = serde_json::Value::Null;
    let mut keys: Vec<&String> = a_map.keys().chain(b_map.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|key| {
            let (o, t) =
                (a.get(*key).unwrap_or(&null), b.get(*key).unwrap_or(&null));
            let was = base.get(*key).unwrap_or(&null);
            o != was && t != was && o != t
        })
        .map(|key| format!("{}/{key}", conflict.field))
        .collect()
}

/// Checks the mod folder's manifest and packs the folder into `out`.
///
/// # Errors
/// `MOD_MANIFEST_INVALID` for a bad manifest, or the write error.
pub fn pack_mod(folder: &Path, out: &Path) -> Result<ModManifest, ModError> {
    let manifest = Mod::open(folder)?.manifest;
    crate::assets::pack_folder(folder, out).map_err(|error| ModError {
        code: "MOD_MANIFEST_INVALID",
        path: out.to_owned(),
        message: format!("cannot write the archive: {error}"),
    })?;
    Ok(manifest)
}

/// The game folder and its mounted mods, last read first.
type Mounted = (PathBuf, Vec<Arc<Mod>>);

static MOUNTED: RwLock<Option<Mounted>> = RwLock::new(None);

/// Makes `mods` override the files of the game at `root`, in order: for
/// `root/assets/...` and `root/scenes/...`, [`crate::assets::read_file`]
/// reads the last mod that holds the file before the game's own. Replaces
/// any earlier mount; an empty list unmounts.
pub fn mount(root: &Path, mods: Vec<Mod>) {
    let mods = mods.into_iter().rev().map(Arc::new).collect();
    let root = std::path::absolute(root).unwrap_or_else(|_| root.to_owned());
    *MOUNTED.write().unwrap_or_else(PoisonError::into_inner) =
        Some((root, mods));
}

/// Mounts the mods in `mods/` beside the folder that holds `scene` (a
/// game's `build/` or `scenes/` folder) and the platform's subscribed
/// workshop items, reporting each loaded mod, conflict and error on
/// stderr. Games call this as they start.
pub fn mount_beside(scene: &Path) {
    let folder = scene.parent().unwrap_or(Path::new("."));
    let root = match folder.file_name().and_then(|name| name.to_str()) {
        Some("build" | "scenes") => folder.parent().unwrap_or(folder),
        _ => folder,
    };
    let set =
        load_mods_with(&root.join("mods"), &crate::platform::workshop_items());
    for error in &set.errors {
        eprintln!(
            "mods: {} {}: {}",
            error.code,
            printable(&error.path.display().to_string()),
            printable(&error.message)
        );
    }
    for loaded in &set.mods {
        eprintln!(
            "mods: loaded {} {}",
            loaded.manifest.name, loaded.manifest.version
        );
    }
    // Only the files: comparing scenes field by field is for `mod list`.
    for (file, holding) in shared_files(&set.mods) {
        let names: Vec<&str> =
            holding.iter().map(|m| m.manifest.name.as_str()).collect();
        eprintln!(
            "mods: {} is in {}; {} wins",
            printable(&file),
            names.join(", "),
            names.last().unwrap_or(&"")
        );
    }
    if !set.mods.is_empty() {
        mount(root, set.mods);
    }
}

/// The mounted mod that holds `path` and the entry name there.
fn mounted_entry(path: &Path) -> Option<(Arc<Mod>, String)> {
    let mounted = MOUNTED.read().unwrap_or_else(PoisonError::into_inner);
    let (root, mods) = mounted.as_ref()?;
    if mods.is_empty() {
        return None;
    }
    let path = std::path::absolute(path).ok()?;
    let parts: Option<Vec<&str>> = path
        .strip_prefix(root)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    let parts = parts?;
    if !matches!(parts.first(), Some(&("assets" | "scenes"))) {
        return None;
    }
    let entry = parts.join("/");
    mods.iter()
        .find(|m| m.len_of(&entry).is_some())
        .map(|m| (Arc::clone(m), entry))
}

/// The bytes of `path` from a mounted mod, when one holds it.
pub(crate) fn read_mounted(path: &Path) -> Option<io::Result<Vec<u8>>> {
    mounted_entry(path).map(|(holder, entry)| holder.read(&entry))
}

/// The size of `path` in a mounted mod, when one holds it.
pub(crate) fn mounted_len(path: &Path) -> Option<u64> {
    mounted_entry(path).and_then(|(holder, entry)| holder.len_of(&entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn add_mod(mods: &Path, name: &str, version: &str, depends: &str) {
        write(
            &mods.join(name).join(MOD_MANIFEST),
            &format!(
                r#"{{"format_version": 1, "name": "{name}", "version": "{version}", "depends": {{{depends}}}}}"#
            ),
        );
    }

    fn names(set: &ModSet) -> Vec<&str> {
        set.mods.iter().map(|m| m.manifest.name.as_str()).collect()
    }

    fn codes(set: &ModSet) -> Vec<&str> {
        set.errors.iter().map(|e| e.code).collect()
    }

    #[test]
    fn untrusted_mod_text_is_printed_without_control_characters() {
        assert_eq!(
            printable("scenes/\u{1b}[2Jx\n.rscene"),
            "scenes/?[2Jx?.rscene"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_loop_in_a_mod_ends_the_file_walk() {
        let mods = std::env::temp_dir()
            .join(format!("rusting-mod-loop-{}", uuid::Uuid::new_v4()));
        add_mod(&mods, "loop", "1.0.0", "");
        write(&mods.join("loop/assets/a.txt"), "a");
        std::os::unix::fs::symlink(
            mods.join("loop"),
            mods.join("loop/assets/again"),
        )
        .unwrap();
        let set = load_mods(&mods);
        assert_eq!(set.mods[0].files(), ["assets/a.txt", "assets/again"]);
        std::fs::remove_dir_all(mods).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn scenes_linked_to_endless_files_are_not_compared() {
        let root = std::env::temp_dir()
            .join(format!("rusting-mod-zero-{}", uuid::Uuid::new_v4()));
        let mods = root.join("mods");
        std::fs::create_dir_all(root.join("scenes")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", root.join("scenes/z.rscene"))
            .unwrap();
        for name in ["a", "b"] {
            add_mod(&mods, name, "1.0.0", "");
            std::fs::create_dir_all(mods.join(name).join("scenes")).unwrap();
            std::os::unix::fs::symlink(
                "/dev/zero",
                mods.join(name).join("scenes/z.rscene"),
            )
            .unwrap();
        }
        let set = load_mods(&mods);
        let found = conflicts(&root, &set.mods);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].file, "scenes/z.rscene");
        assert!(found[0].fields.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn workshop_items_load_with_the_folders_mods() {
        let root = std::env::temp_dir()
            .join(format!("rusting-workshop-{}", uuid::Uuid::new_v4()));
        let (mods, workshop) = (root.join("mods"), root.join("workshop"));
        add_mod(&mods, "local", "1.0.0", r#""subscribed": "2""#);
        add_mod(&mods, "both", "1.0.0", "");
        add_mod(&workshop, "subscribed", "2.1.0", "");
        add_mod(&workshop, "both", "9.0.0", "");
        let items = [workshop.join("subscribed"), workshop.join("both")];
        let set = load_mods_with(&mods, &items);
        assert_eq!(names(&set), ["both", "subscribed", "local"]);
        assert_eq!(set.mods[0].manifest.version, "1.0.0", "the folder's wins");
        assert_eq!(codes(&set), ["MOD_MANIFEST_INVALID"]);
        // Workshop items alone, with no mods folder.
        assert_eq!(
            names(&load_mods_with(&root.join("none"), &items[..1])),
            ["subscribed"]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn versions_match_by_major_and_minimum() {
        assert!(satisfies("1.4.2", "1.2"));
        assert!(satisfies("1.2.0", "1.2.0"));
        assert!(!satisfies("1.1.9", "1.2"));
        assert!(!satisfies("2.0.0", "1.2"));
        assert!(satisfies("0.2.5", "0.2"));
        assert!(!satisfies("0.3.0", "0.2"));
        assert!(!satisfies("1.x", "1"));
        assert_eq!(version("1.2.3.4"), None);
        assert_eq!(version("3"), Some([3, 0, 0]));
    }

    #[test]
    fn load_order_puts_dependencies_first_and_drops_broken_mods() {
        let mods = std::env::temp_dir()
            .join(format!("rusting-mods-{}", uuid::Uuid::new_v4()))
            .join("mods");
        add_mod(&mods, "base", "1.3.0", "");
        add_mod(&mods, "addon", "0.1.0", r#""base": "1.2""#);
        add_mod(&mods, "tweak", "1.0.0", "");
        add_mod(&mods, "old", "1.0.0", r#""base": "2.0""#);
        add_mod(&mods, "needs-old", "1.0.0", r#""old": "1""#);
        add_mod(&mods, "ping", "1.0.0", r#""pong": "1""#);
        add_mod(&mods, "pong", "1.0.0", r#""ping": "1""#);
        write(&mods.join("broken/mod.json"), "{}");

        let set = load_mods(&mods);
        assert_eq!(names(&set), ["base", "addon", "tweak"]);
        assert_eq!(
            codes(&set),
            [
                "MOD_MANIFEST_INVALID",
                "MOD_DEPENDENCY_UNMET",
                "MOD_DEPENDENCY_UNMET",
                "MOD_DEPENDENCY_CYCLE",
                "MOD_DEPENDENCY_CYCLE",
            ]
        );
        assert!(set.errors[1].message.contains("1.3.0 is installed"));
        assert!(set.errors[2].message.contains("not installed"));

        // A load order picks and orders mods; dependencies still go first.
        write(&mods.join(LOAD_ORDER_FILE), r#"["tweak", "addon", "base"]"#);
        assert_eq!(names(&load_mods(&mods)), ["tweak", "base", "addon"]);
        write(&mods.join(LOAD_ORDER_FILE), r#"["addon"]"#);
        let set = load_mods(&mods);
        assert!(set.mods.is_empty());
        assert_eq!(
            codes(&set),
            ["MOD_MANIFEST_INVALID", "MOD_DEPENDENCY_UNMET"]
        );
        std::fs::remove_dir_all(mods.parent().unwrap()).unwrap();
    }

    #[test]
    fn mounted_mods_replace_game_files_in_load_order() {
        let root = std::env::temp_dir()
            .join(format!("rusting-modded-{}", uuid::Uuid::new_v4()));
        let mods = root.join("mods");
        write(&root.join("assets/hero.txt"), "game hero");
        write(&root.join("assets/sword.txt"), "game sword");
        write(&root.join("scenes/level.rscene"), "game level");
        add_mod(&mods, "first", "1.0.0", "");
        write(&mods.join("first/assets/hero.txt"), "first hero");
        write(&mods.join("first/assets/sword.txt"), "first sword");
        let second = root.join("second-src");
        add_mod(&second, "second", "1.0.0", r#""first": "1""#);
        write(&second.join("second/assets/hero.txt"), "second hero");
        write(&second.join("second/scenes/level.rscene"), "second level");
        write(&second.join("second/scenes/extra.rscene"), "new level");
        let packed = mods.join("second.rmod");
        assert_eq!(
            pack_mod(&second.join("second"), &packed).unwrap().name,
            "second"
        );
        assert_eq!(
            pack_mod(&root.join("assets"), &root.join("x.rmod"))
                .unwrap_err()
                .code,
            "MOD_MANIFEST_INVALID"
        );

        let found = conflicts(&root, &load_mods(&mods).mods);
        let files: Vec<(&str, &[String])> = found
            .iter()
            .map(|c| (c.file.as_str(), c.mods.as_slice()))
            .collect();
        assert_eq!(
            files,
            [(
                "assets/hero.txt",
                &["first".to_owned(), "second".to_owned()][..]
            ),]
        );

        mount_beside(&root.join("build/main.rscene.bin"));
        let read = |path: &str| {
            String::from_utf8(
                crate::assets::read_file(&root.join(path)).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(read("assets/hero.txt"), "second hero");
        assert_eq!(read("assets/sword.txt"), "first sword");
        assert_eq!(read("scenes/level.rscene"), "second level");
        assert_eq!(read("scenes/extra.rscene"), "new level");
        assert_eq!(
            crate::assets::file_len(&root.join("scenes/extra.rscene")),
            Some(9)
        );
        // Only `assets/` and `scenes/` are overlaid.
        assert!(crate::assets::read_file(&root.join("mod.json")).is_err());

        mount(&root, Vec::new());
        assert_eq!(read("assets/hero.txt"), "game hero");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scene_conflicts_name_entity_and_component_fields() {
        let root = std::env::temp_dir()
            .join(format!("rusting-mod-fields-{}", uuid::Uuid::new_v4()));
        let mods = root.join("mods");
        let scene = |intensity: f64, color: [f64; 3], x: f64| {
            serde_json::json!({
                "format_version": 1,
                "entities": [{
                    "id": "lamp", "name": "Lamp",
                    "transform": {"position": [x, 0.0, 0.0]},
                    "components": {"rusting.point_light": {"intensity": intensity, "color": color}},
                }],
            })
            .to_string()
        };
        write(
            &root.join("scenes/main.rscene"),
            &scene(1.0, [1.0, 1.0, 1.0], 0.0),
        );
        for (name, intensity, color, x) in [
            ("bright", 5.0, [1.0, 1.0, 1.0], 2.0),
            ("red", 1.0, [1.0, 0.0, 0.0], 2.0),
            ("dim", 0.5, [0.0, 0.0, 1.0], 0.0),
        ] {
            add_mod(&mods, name, "1.0.0", "");
            write(
                &mods.join(name).join("scenes/main.rscene"),
                &scene(intensity, color, x),
            );
        }
        let found = conflicts(&root, &load_mods(&mods).mods);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].mods, ["bright", "dim", "red"]);
        let fields: Vec<(String, &str)> = found[0]
            .fields
            .iter()
            .map(|f| (f.mods.join("+"), f.field.as_str()))
            .collect();
        // bright and red move the lamp to the same place and change
        // different light fields: no conflict.
        assert_eq!(
            fields,
            [
                (
                    "bright+dim".to_owned(),
                    "components/rusting.point_light/intensity"
                ),
                ("dim+red".to_owned(), "components/rusting.point_light/color"),
            ]
        );
        assert_eq!(found[0].fields[0].name.as_deref(), Some("Lamp"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
