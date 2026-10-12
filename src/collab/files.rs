//! Project file paths and change polling for a collaboration session.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

/// Folders never sent: build output and version control.
const SKIPPED_DIRS: [&str; 3] = ["target", "build", ".git"];

/// A path received from a peer as a safe relative path, or `None` when it
/// is empty, absolute, has a drive prefix, or climbs with `..`. Joined to a
/// project root, it cannot leave that root except through a symlink, which
/// [`inside_root`] checks.
#[must_use]
pub fn safe_relative(path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

/// [`safe_relative`] for a path a peer may write: also refuses build
/// output, per-user sidecars, and any part starting with `.` (`.git` hooks,
/// `.cargo` config and editor task files run code outside the trust prompt).
#[must_use]
pub fn shareable(path: &str) -> Option<PathBuf> {
    let clean = safe_relative(path)?;
    let blocked = clean.components().any(|part| {
        let part = part.as_os_str().to_string_lossy();
        part.starts_with('.')
            || SKIPPED_DIRS
                .iter()
                .any(|dir| dir.eq_ignore_ascii_case(&part))
    });
    (!blocked && !is_local(&path.to_ascii_lowercase())).then_some(clean)
}

/// `root.join(relative)` when it stays inside `root` after resolving the
/// symlinks of the parts that exist.
#[must_use]
pub fn inside_root(root: &Path, relative: &Path) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let target = root.join(relative);
    let mut existing = target.as_path();
    while !existing.exists() {
        existing = existing.parent()?;
    }
    existing
        .canonicalize()
        .ok()?
        .starts_with(&root)
        .then_some(target)
}

/// True for scene files, which travel as operations instead of whole files.
#[must_use]
pub fn is_scene(path: &str) -> bool {
    path.ends_with(".rscene")
}

/// Per-user files that stay local: editor camera and selection sidecars.
fn is_local(path: &str) -> bool {
    path.ends_with(".editor.json")
}

/// Modification time and size of every shared file under `root`, keyed by
/// its `/`-separated relative path.
#[must_use]
pub fn scan_project(root: &Path) -> BTreeMap<String, (SystemTime, u64)> {
    let mut files = BTreeMap::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let name = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if shareable(&name).is_none() {
                continue;
            }
            if kind.is_dir() {
                folders.push(path);
            } else if kind.is_file() {
                if let Ok(metadata) = entry.metadata() {
                    let modified =
                        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    files.insert(name, (modified, metadata.len()));
                }
            }
        }
    }
    files
}

/// Relative paths that changed between two polls.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

/// Finds files that changed since the last poll.
// ponytail: full directory scan per poll. Use OS file events if large
// projects make the once-a-second scan show up in the profiler.
#[derive(Clone, Debug, Default)]
pub struct FileWatcher {
    known: BTreeMap<String, (SystemTime, u64)>,
}

impl FileWatcher {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            known: scan_project(root),
        }
    }

    /// Files added, modified and removed since the last call.
    pub fn changed(&mut self, root: &Path) -> Changes {
        let now = scan_project(root);
        let mut changes = Changes::default();
        for (path, stamp) in &now {
            match self.known.get(path) {
                None => changes.added.push(path.clone()),
                Some(known) if known != stamp => {
                    changes.modified.push(path.clone());
                }
                Some(_) => {}
            }
        }
        changes.removed = self
            .known
            .keys()
            .filter(|path| !now.contains_key(*path))
            .cloned()
            .collect();
        self.known = now;
        changes
    }

    /// Records a file the session itself wrote or deleted, so it is not
    /// sent back.
    pub fn note(&mut self, root: &Path, path: &str) {
        if let Ok(metadata) = std::fs::metadata(root.join(path)) {
            let modified =
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            self.known
                .insert(path.to_owned(), (modified, metadata.len()));
        } else {
            self.known.remove(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_rejects_escapes() {
        for bad in ["", "../x", "/etc/passwd", "a/../../x", "a/../b", "."] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
        assert_eq!(
            safe_relative("assets/./a.png"),
            Some(PathBuf::from("assets/a.png"))
        );
    }

    #[test]
    fn shareable_refuses_hooks_build_output_and_sidecars() {
        for bad in [
            ".git/hooks/pre-commit",
            ".cargo/config.toml",
            "src/.vscode/tasks.json",
            "Target/debug/game",
            "scenes/main.rscene.editor.json",
            "../x",
        ] {
            assert_eq!(shareable(bad), None, "{bad}");
        }
        assert!(shareable("src/main.rs").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn inside_root_rejects_symlink_escape() {
        let root = std::env::temp_dir()
            .join(format!("rusting-collab-link-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let _ = std::fs::remove_file(root.join("out"));
        std::os::unix::fs::symlink("/tmp", root.join("out")).unwrap();
        assert!(inside_root(&root, Path::new("out/x")).is_none());
        assert!(inside_root(&root, Path::new("new/x")).is_some());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn watcher_reports_changes_once_and_skips_build_output() {
        let root = std::env::temp_dir()
            .join(format!("rusting-collab-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("a.txt"), "1").unwrap();
        let mut watcher = FileWatcher::new(&root);
        assert_eq!(watcher.changed(&root), Changes::default());
        std::fs::write(root.join("b.txt"), "new").unwrap();
        std::fs::write(root.join("a.txt"), "longer").unwrap();
        std::fs::write(root.join("target/x"), "skip").unwrap();
        std::fs::write(root.join("s.rscene.editor.json"), "{}").unwrap();
        let changes = watcher.changed(&root);
        assert_eq!(changes.added, ["b.txt"]);
        assert_eq!(changes.modified, ["a.txt"]);
        assert_eq!(watcher.changed(&root), Changes::default());
        std::fs::remove_file(root.join("a.txt")).unwrap();
        assert_eq!(watcher.changed(&root).removed, ["a.txt"]);
        std::fs::write(root.join("c.txt"), "from peer").unwrap();
        watcher.note(&root, "c.txt");
        std::fs::remove_file(root.join("b.txt")).unwrap();
        watcher.note(&root, "b.txt");
        assert_eq!(watcher.changed(&root), Changes::default());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
