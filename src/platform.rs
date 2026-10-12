//! Platform services: where saves go, achievements and stats, workshop
//! mods and store content, behind one
//! [`PlatformServices`] trait so a storefront SDK (Steam, GOG, Epic) plugs
//! in without game code changing.
//!
//! Game code calls the free functions here. They go to the services set
//! with [`set_platform_services`], or to [`LocalPlatform`] when none is
//! set, which keeps achievements and stats in `platform.json` in the user
//! data folder, so a game without a storefront still remembers them.
//!
//! Never let these calls change simulation state: achievements are
//! presentation, like audio, and must not affect replay hashes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// File in the user data folder where [`LocalPlatform`] keeps achievements
/// and stats.
pub const LOCAL_PLATFORM_FILE: &str = "platform.json";

/// One storefront or platform backend. Implement it over the store's SDK
/// in the game crate and install it with [`set_platform_services`] at
/// start. Calls must not block for long: they run on the game thread.
pub trait PlatformServices: Send {
    /// Short name shown in logs, such as `local` or `steam`.
    fn name(&self) -> &str;

    /// Folder for settings and saves, when the platform has its own (a
    /// cloud-synced folder). `None` keeps the default user data folder.
    fn save_folder(&self) -> Option<PathBuf> {
        None
    }

    /// Unlocks the achievement with this API name. Returns whether the
    /// platform took it; unlocking twice is not an error.
    fn unlock_achievement(&mut self, name: &str) -> bool;

    /// Whether the achievement with this API name is unlocked.
    fn achievement_unlocked(&self, name: &str) -> bool;

    /// Sets an integer stat, such as nights survived. Returns whether the
    /// platform took it.
    fn set_stat(&mut self, name: &str, value: i32) -> bool;

    /// The stat's value, or `None` when it was never set.
    fn stat(&self, name: &str) -> Option<i32>;

    /// Installed workshop items the player subscribed to: mod folders or
    /// `.rmod` files, as [`crate::mods`] reads them. Games load them with
    /// the mods in their `mods/` folder. None by default.
    fn workshop_items(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Uploads a packed mod ([`crate::mods::pack_mod`]) as a workshop item,
    /// a new one or a new version of the item with the same name, and
    /// returns the item's ID. Fails by default: there is no workshop.
    ///
    /// # Errors
    /// Why the platform did not take it.
    fn publish_workshop_item(
        &mut self,
        archive: &Path,
        manifest: &crate::mods::ModManifest,
    ) -> Result<String, String> {
        let _ = (archive, manifest);
        Err(format!("the `{}` platform has no workshop", self.name()))
    }

    /// Whether the player owns the paid content (DLC) with this store ID.
    /// True by default, so a game without a storefront has all of it.
    fn owns_content(&self, id: &str) -> bool {
        let _ = id;
        true
    }

    /// Opens the store page of the game, or of the content with this ID,
    /// in the platform's overlay or browser. Returns whether it opened;
    /// false by default.
    fn open_store_page(&mut self, id: Option<&str>) -> bool {
        let _ = id;
        false
    }
}

/// The default services: achievements and stats in
/// [`LOCAL_PLATFORM_FILE`], saves in the default user data folder.
#[derive(Debug, Default)]
pub struct LocalPlatform {
    /// Folder holding the file; `None` means
    /// [`crate::project::default_user_data_folder`], read at each call so
    /// `RUSTING_USER_DATA` set later still counts.
    folder: Option<PathBuf>,
}

/// What [`LocalPlatform`] stores.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct LocalRecord {
    #[serde(default)]
    achievements: Vec<String>,
    #[serde(default)]
    stats: BTreeMap<String, i32>,
}

impl LocalPlatform {
    /// Local services keeping their file in `folder` instead of the user
    /// data folder.
    #[must_use]
    pub fn in_folder(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: Some(folder.into()),
        }
    }

    fn path(&self) -> PathBuf {
        self.folder
            .clone()
            .unwrap_or_else(crate::project::default_user_data_folder)
            .join(LOCAL_PLATFORM_FILE)
    }

    /// The stored record; a missing or broken file reads as empty.
    fn read(&self) -> LocalRecord {
        std::fs::read_to_string(self.path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write(&self, record: &LocalRecord) -> bool {
        let path = self.path();
        let text = match serde_json::to_string_pretty(record) {
            Ok(text) => text + "\n",
            Err(_) => return false,
        };
        path.parent()
            .is_none_or(|folder| std::fs::create_dir_all(folder).is_ok())
            && crate::runtime::write_atomic_unchecked(path, text.as_bytes())
                .is_ok()
    }
}

impl PlatformServices for LocalPlatform {
    fn name(&self) -> &str {
        "local"
    }

    fn unlock_achievement(&mut self, name: &str) -> bool {
        let mut record = self.read();
        if record.achievements.iter().any(|known| known == name) {
            return true;
        }
        record.achievements.push(name.to_owned());
        record.achievements.sort();
        self.write(&record)
    }

    fn achievement_unlocked(&self, name: &str) -> bool {
        self.read().achievements.iter().any(|known| known == name)
    }

    fn set_stat(&mut self, name: &str, value: i32) -> bool {
        let mut record = self.read();
        if record.stats.get(name) == Some(&value) {
            return true;
        }
        record.stats.insert(name.to_owned(), value);
        self.write(&record)
    }

    fn stat(&self, name: &str) -> Option<i32> {
        self.read().stats.get(name).copied()
    }
}

fn services() -> MutexGuard<'static, Box<dyn PlatformServices>> {
    static SERVICES: OnceLock<Mutex<Box<dyn PlatformServices>>> =
        OnceLock::new();
    SERVICES
        .get_or_init(|| Mutex::new(Box::new(LocalPlatform::default())))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Replaces the platform services for the rest of the process, returning
/// the previous ones. Call it at start, before the first save.
pub fn set_platform_services(
    new: Box<dyn PlatformServices>,
) -> Box<dyn PlatformServices> {
    std::mem::replace(&mut *services(), new)
}

/// Name of the platform services in use, such as `local`.
#[must_use]
pub fn platform_name() -> String {
    services().name().to_owned()
}

/// The platform's own save folder, if it has one; see
/// [`PlatformServices::save_folder`].
#[must_use]
pub fn platform_save_folder() -> Option<PathBuf> {
    services().save_folder()
}

/// Unlocks the achievement with this API name; see
/// [`PlatformServices::unlock_achievement`].
pub fn unlock_achievement(name: &str) -> bool {
    services().unlock_achievement(name)
}

/// Whether the achievement with this API name is unlocked.
#[must_use]
pub fn achievement_unlocked(name: &str) -> bool {
    services().achievement_unlocked(name)
}

/// Sets an integer stat; see [`PlatformServices::set_stat`].
pub fn set_stat(name: &str, value: i32) -> bool {
    services().set_stat(name, value)
}

/// The stat's value, or `None` when it was never set.
#[must_use]
pub fn stat(name: &str) -> Option<i32> {
    services().stat(name)
}

/// Installed workshop items; see [`PlatformServices::workshop_items`].
#[must_use]
pub fn workshop_items() -> Vec<PathBuf> {
    services().workshop_items()
}

/// Uploads a packed mod as a workshop item and returns its ID; see
/// [`PlatformServices::publish_workshop_item`].
///
/// # Errors
/// Why the platform did not take it.
pub fn publish_workshop_item(
    archive: &Path,
    manifest: &crate::mods::ModManifest,
) -> Result<String, String> {
    services().publish_workshop_item(archive, manifest)
}

/// Whether the player owns the paid content with this store ID; see
/// [`PlatformServices::owns_content`].
#[must_use]
pub fn owns_content(id: &str) -> bool {
    services().owns_content(id)
}

/// Opens the store page of the game (`None`) or of some content; see
/// [`PlatformServices::open_store_page`].
pub fn open_store_page(id: Option<&str>) -> bool {
    services().open_store_page(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A storefront stand-in that keeps everything in memory.
    #[derive(Default)]
    struct FakeStore {
        unlocked: Vec<String>,
    }

    impl PlatformServices for FakeStore {
        fn name(&self) -> &str {
            "fake"
        }
        fn unlock_achievement(&mut self, name: &str) -> bool {
            self.unlocked.push(name.to_owned());
            true
        }
        fn achievement_unlocked(&self, name: &str) -> bool {
            self.unlocked.iter().any(|known| known == name)
        }
        fn set_stat(&mut self, _: &str, _: i32) -> bool {
            false
        }
        fn stat(&self, _: &str) -> Option<i32> {
            None
        }
        fn publish_workshop_item(
            &mut self,
            _: &Path,
            manifest: &crate::mods::ModManifest,
        ) -> Result<String, String> {
            Ok(format!("item-{}", manifest.name))
        }
        fn owns_content(&self, id: &str) -> bool {
            id == "soundtrack"
        }
    }

    #[test]
    fn local_services_remember_achievements_and_a_store_can_replace_them() {
        let folder = std::env::temp_dir()
            .join(format!("rusting-platform-{}", uuid::Uuid::new_v4()));
        let mut local = LocalPlatform::in_folder(&folder);
        assert!(!local.achievement_unlocked("night_5"));
        assert!(local.unlock_achievement("night_5"));
        assert!(local.unlock_achievement("night_5"), "twice is fine");
        assert!(local.set_stat("nights_survived", 5));
        assert_eq!(local.stat("missing"), None);
        // A new instance reads the same file, as the next run would.
        let again = LocalPlatform::in_folder(&folder);
        assert!(again.achievement_unlocked("night_5"));
        assert_eq!(again.stat("nights_survived"), Some(5));
        let text =
            std::fs::read_to_string(folder.join(LOCAL_PLATFORM_FILE)).unwrap();
        assert_eq!(text.matches("night_5").count(), 1, "{text}");
        std::fs::write(folder.join(LOCAL_PLATFORM_FILE), "not json").unwrap();
        assert!(!again.achievement_unlocked("night_5"), "broken reads empty");
        let _ = std::fs::remove_dir_all(&folder);

        let previous = set_platform_services(Box::new(FakeStore::default()));
        assert_eq!(platform_name(), "fake");
        assert!(unlock_achievement("first_blood"));
        assert!(achievement_unlocked("first_blood"));
        assert!(!set_stat("kills", 1));
        assert_eq!(platform_save_folder(), None);
        let manifest = crate::mods::ModManifest {
            name: "hats".into(),
            ..Default::default()
        };
        let archive = Path::new("hats.rmod");
        assert_eq!(
            publish_workshop_item(archive, &manifest),
            Ok("item-hats".into())
        );
        assert!(owns_content("soundtrack"));
        assert!(!owns_content("expansion"));
        assert!(!open_store_page(None));
        set_platform_services(previous);
        assert_eq!(platform_name(), "local");
        // Local services: no workshop, and every piece of content owned.
        assert!(workshop_items().is_empty());
        assert!(publish_workshop_item(archive, &manifest)
            .unwrap_err()
            .contains("no workshop"));
        assert!(owns_content("expansion"));
    }
}
