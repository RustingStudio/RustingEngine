//! Packed asset archives: one file holding a folder's files, read back one
//! entry at a time without loading the rest.
//!
//! Layout, all integers little-endian: the 8-byte [`PACK_MAGIC`], a `u32`
//! entry count, then per entry a `u16` path length, the UTF-8 path (relative,
//! `/`-separated), a `u64` offset from the start of the file and a `u64`
//! length; the file bodies follow the index in path order.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The first bytes of every pack file.
pub const PACK_MAGIC: &[u8; 8] = b"RPAK\0\0\0\x01";

/// Writes every file under `folder` into a new pack at `out`, with paths
/// relative to `folder`, in sorted order so the same folder packs to the
/// same bytes. Returns the number of files packed.
///
/// # Errors
/// Returns an error when a file cannot be read, a path is not UTF-8 or
/// longer than 65535 bytes, or `out` cannot be written.
pub fn pack_folder(folder: &Path, out: &Path) -> io::Result<usize> {
    let mut paths = Vec::new();
    let mut pending = vec![folder.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path != out {
                paths.push(path);
            }
        }
    }
    let invalid =
        |message: String| io::Error::new(io::ErrorKind::InvalidInput, message);
    let mut names = Vec::with_capacity(paths.len());
    for path in &paths {
        let relative = path
            .strip_prefix(folder)
            .map_err(|e| invalid(e.to_string()))?;
        let parts: Option<Vec<&str>> = relative
            .components()
            .map(|part| part.as_os_str().to_str())
            .collect();
        let name = parts
            .ok_or_else(|| invalid(format!("{} is not UTF-8", path.display())))?
            .join("/");
        if name.len() > usize::from(u16::MAX) {
            return Err(invalid(format!("{name} is too long")));
        }
        names.push((name, path.clone()));
    }
    names.sort();
    let count =
        u32::try_from(names.len()).map_err(|e| invalid(e.to_string()))?;
    let index_len: u64 =
        names.iter().map(|(name, _)| 18 + name.len() as u64).sum();
    let mut offset = PACK_MAGIC.len() as u64 + 4 + index_len;
    let mut index = Vec::new();
    index.extend_from_slice(PACK_MAGIC);
    index.extend_from_slice(&count.to_le_bytes());
    let mut lengths = Vec::with_capacity(names.len());
    for (name, path) in &names {
        let len = std::fs::metadata(path)?.len();
        index.extend_from_slice(&(name.len() as u16).to_le_bytes());
        index.extend_from_slice(name.as_bytes());
        index.extend_from_slice(&offset.to_le_bytes());
        index.extend_from_slice(&len.to_le_bytes());
        lengths.push(len);
        offset += len;
    }
    let mut writer = io::BufWriter::new(File::create(out)?);
    writer.write_all(&index)?;
    for ((name, path), len) in names.iter().zip(lengths) {
        // A file that changed size since the index was written would shift
        // every later entry.
        let copied = io::copy(&mut File::open(path)?.take(len), &mut writer)?;
        if copied != len {
            return Err(invalid(format!("{name} changed while packing")));
        }
    }
    writer.flush()?;
    Ok(names.len())
}

/// An open pack file. Only the index is held in memory; each read seeks to
/// one entry.
#[derive(Debug)]
pub struct PackArchive {
    path: PathBuf,
    entries: BTreeMap<String, (u64, u64)>,
}

impl PackArchive {
    /// Reads the index of the pack at `path`.
    ///
    /// # Errors
    /// Returns `InvalidData` when the file is not a pack, or an entry lies
    /// outside the file or has a path with `..`, a leading `/` or `\`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let bad = |message: &str| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {message}", path.display()),
            )
        };
        let mut magic = [0; 8];
        reader
            .read_exact(&mut magic)
            .map_err(|_| bad("not a pack"))?;
        if &magic != PACK_MAGIC {
            return Err(bad("not a pack"));
        }
        let count = u32::from_le_bytes(read_array(&mut reader)?);
        // Each index entry takes at least 18 bytes, so a corrupt count
        // fails here instead of reading for ever.
        if u64::from(count) * 18 > size {
            return Err(bad("entry count larger than the file"));
        }
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let len = u16::from_le_bytes(read_array(&mut reader)?);
            let mut name = vec![0; usize::from(len)];
            reader.read_exact(&mut name)?;
            let name = String::from_utf8(name)
                .map_err(|_| bad("path is not UTF-8"))?;
            let offset = u64::from_le_bytes(read_array(&mut reader)?);
            let len = u64::from_le_bytes(read_array(&mut reader)?);
            if offset.checked_add(len).is_none_or(|end| end > size) {
                return Err(bad(&format!("{name} lies outside the file")));
            }
            if name.starts_with('/')
                || name.contains('\\')
                || name.split('/').any(|part| part.is_empty() || part == "..")
            {
                return Err(bad(&format!("unsafe path {name}")));
            }
            entries.insert(name, (offset, len));
        }
        Ok(Self {
            path: path.to_owned(),
            entries,
        })
    }

    /// Every entry's path, sorted.
    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// The size in bytes of the entry at `path`, if there is one.
    #[must_use]
    pub fn len_of(&self, path: &str) -> Option<u64> {
        self.entries.get(path).map(|&(_, len)| len)
    }

    /// A reader over one entry, which streams it from the file instead of
    /// loading it whole.
    ///
    /// # Errors
    /// Returns `NotFound` when the pack has no entry at `path`.
    pub fn reader(&self, path: &str) -> io::Result<io::Take<BufReader<File>>> {
        let &(offset, len) = self.entries.get(path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{path} is not in {}", self.path.display()),
            )
        })?;
        let mut file = BufReader::new(File::open(&self.path)?);
        file.seek(SeekFrom::Start(offset))?;
        Ok(file.take(len))
    }

    /// The whole entry at `path`.
    ///
    /// # Errors
    /// As [`Self::reader`], or when the file cannot be read.
    pub fn read(&self, path: &str) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.reader(path)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    }
}

/// Reads `path` from disk or, when there is no such file, from the pack
/// that stands in for its nearest `assets` or `scenes` folder:
/// `game/assets/sfx/hit.wav` is read as `sfx/hit.wav` from
/// `game/assets.rpak`. Loose files win, so a patch can
/// replace one packed file.
///
/// # Errors
/// Returns the disk error when no pack holds the file either.
pub fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    match std::fs::read(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match packed(path) {
                Some((archive, name)) => archive.read(&name),
                None => Err(error),
            }
        }
        other => other,
    }
}

/// The size of `path` on disk or in the pack [`read_file`] would read it
/// from; `None` when neither has it.
#[must_use]
pub fn file_len(path: &Path) -> Option<u64> {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file().then_some(meta.len()),
        Err(_) => {
            packed(path).and_then(|(archive, name)| archive.len_of(&name))
        }
    }
}

/// The open pack that holds `path`, and the entry name there.
fn packed(path: &Path) -> Option<(std::sync::Arc<PackArchive>, String)> {
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    // ponytail: a pack is opened once per process and its index cached, so a
    // pack rewritten while the game runs is not seen until a restart.
    static OPEN: OnceLock<Mutex<BTreeMap<PathBuf, Option<Arc<PackArchive>>>>> =
        OnceLock::new();
    // Only the nearest `assets/` or `scenes/` folder is looked up, so a
    // stray `.rpak` higher up the tree (`/home.rpak`) is never read.
    let folder = path.ancestors().skip(1).find(|folder| {
        matches!(
            folder.file_name().and_then(|name| name.to_str()),
            Some("assets" | "scenes")
        )
    })?;
    let pack = folder.with_extension("rpak");
    let archive = OPEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(pack.clone())
        .or_insert_with(|| match PackArchive::open(&pack) {
            Ok(archive) => Some(Arc::new(archive)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                eprintln!("assets: cannot open pack: {error}");
                None
            }
        })
        .clone()?;
    let parts: Option<Vec<&str>> = path
        .strip_prefix(folder)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_str())
        .collect();
    Some((archive, parts?.join("/")))
}

fn read_array<const N: usize>(reader: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("rusting-pack-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_packed_folder_reads_back_each_file_and_streams_one() {
        let dir = temp_dir();
        let assets = dir.join("assets");
        std::fs::create_dir_all(assets.join("sfx")).unwrap();
        std::fs::write(assets.join("a.txt"), b"alpha").unwrap();
        std::fs::write(assets.join("sfx/hit.wav"), vec![7u8; 100_000]).unwrap();
        std::fs::write(assets.join("empty"), b"").unwrap();
        let pack = dir.join("assets.rpak");
        assert_eq!(pack_folder(&assets, &pack).unwrap(), 3);
        let archive = PackArchive::open(&pack).unwrap();
        assert_eq!(
            archive.paths().collect::<Vec<_>>(),
            ["a.txt", "empty", "sfx/hit.wav"]
        );
        assert_eq!(archive.read("a.txt").unwrap(), b"alpha");
        assert_eq!(archive.read("empty").unwrap(), b"");
        assert_eq!(archive.len_of("sfx/hit.wav"), Some(100_000));
        let mut head = [0; 4];
        archive
            .reader("sfx/hit.wav")
            .unwrap()
            .read_exact(&mut head)
            .unwrap();
        assert_eq!(head, [7; 4]);
        assert_eq!(archive.read("sfx/hit.wav").unwrap(), vec![7u8; 100_000]);
        let missing = archive.read("b.txt").unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);
        // Same folder, same bytes.
        let again = dir.join("again.rpak");
        pack_folder(&assets, &again).unwrap();
        assert_eq!(
            std::fs::read(&pack).unwrap(),
            std::fs::read(&again).unwrap()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_or_unsafe_pack_is_refused() {
        let dir = temp_dir();
        let file = dir.join("bad.rpak");
        let entry = |name: &str, offset: u64, len: u64| {
            let mut bytes = PACK_MAGIC.to_vec();
            bytes.extend_from_slice(&1u32.to_le_bytes());
            bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(name.as_bytes());
            bytes.extend_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&len.to_le_bytes());
            bytes
        };
        let refused = |bytes: &[u8]| {
            std::fs::write(&file, bytes).unwrap();
            PackArchive::open(&file).unwrap_err().kind()
        };
        assert_eq!(refused(b"not a pack at all"), io::ErrorKind::InvalidData);
        let mut huge = PACK_MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(refused(&huge), io::ErrorKind::InvalidData);
        assert_eq!(
            refused(&entry("a", 0, 1 << 40)),
            io::ErrorKind::InvalidData
        );
        assert_eq!(refused(&entry("../up", 0, 1)), io::ErrorKind::InvalidData);
        assert_eq!(refused(&entry("/abs", 0, 1)), io::ErrorKind::InvalidData);
        std::fs::write(&file, entry("ok", 0, 8)).unwrap();
        assert!(PackArchive::open(&file).is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_read_from_the_pack_beside_its_folder() {
        let dir = temp_dir();
        let assets = dir.join("assets");
        std::fs::create_dir_all(assets.join("ui")).unwrap();
        image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 255])
            .unwrap()
            .save(assets.join("ui/pixel.png"))
            .unwrap();
        std::fs::write(assets.join("note.txt"), b"packed").unwrap();
        pack_folder(&assets, &dir.join("assets.rpak")).unwrap();
        std::fs::remove_dir_all(&assets).unwrap();

        let note = assets.join("note.txt");
        assert_eq!(read_file(&note).unwrap(), b"packed");
        assert_eq!(file_len(&note), Some(6));
        let mut server = crate::assets::AssetServer::default();
        let pixel = server.load_texture(assets.join("ui/pixel.png")).unwrap();
        assert_eq!(
            server.textures.get(pixel).unwrap().rgba8,
            [12, 34, 56, 255]
        );
        let missing = assets.join("ui/none.png");
        assert_eq!(
            read_file(&missing).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(file_len(&missing), None);
        // Only `assets` and `scenes` folders stand in for packs.
        let other = dir.join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("note.txt"), b"x").unwrap();
        pack_folder(&other, &dir.join("other.rpak")).unwrap();
        std::fs::remove_dir_all(&other).unwrap();
        assert!(read_file(&other.join("note.txt")).is_err());
        // A loose file wins over the packed one.
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(&note, b"loose").unwrap();
        assert_eq!(read_file(&note).unwrap(), b"loose");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn packed_scenes_load_in_file_and_folder_form() {
        use crate::runtime::{read_scene_bytes, write_atomic};
        let dir = temp_dir();
        let scenes = dir.join("scenes");
        std::fs::create_dir_all(scenes.join("level.rscene")).unwrap();
        let scene = serde_json::json!({
            "format_version": 6, "name": "Main", "entities": [
                {"id": "00000000-0000-0000-0000-000000000001", "parent": null,
                 "name": "Cube"},
            ]
        });
        std::fs::write(scenes.join("main.rscene"), scene.to_string()).unwrap();
        let single = read_scene_bytes(scenes.join("main.rscene")).unwrap();
        write_atomic(scenes.join("level.rscene"), &single).unwrap();
        let joined = read_scene_bytes(scenes.join("level.rscene")).unwrap();
        assert!(scenes.join("level.rscene/scene.json").is_file());
        pack_folder(&scenes, &dir.join("scenes.rpak")).unwrap();
        std::fs::remove_dir_all(&scenes).unwrap();

        assert_eq!(
            read_scene_bytes(scenes.join("main.rscene")).unwrap(),
            single
        );
        assert_eq!(
            read_scene_bytes(scenes.join("level.rscene")).unwrap(),
            joined
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
