//! Steam achievements and stats for game code.
//!
//! Every call compiles in every build and does nothing yet: Steamworks needs
//! the `steamworks` crate, which waits on the owner's approval. The optional
//! `steam` feature is reserved for that backend, so game code written against
//! these calls today keeps working when it lands. Until then a game runs the
//! same with or without Steam installed, and these calls never fail.
//!
//! Game code should call [`crate::platform`] instead: the Steamworks
//! backend will land as a [`crate::platform::PlatformServices`]
//! implementation, so the same calls reach Steam, another store, or the
//! local file.
//!
//! Never let these calls change simulation state: achievements are
//! presentation, like audio, and must not affect replay hashes.

/// Whether a Steam client is connected. Always `false` until the
/// Steamworks backend lands.
#[must_use]
pub fn is_running() -> bool {
    false
}

/// Unlocks the achievement with this API name. Returns whether Steam took
/// the call; always `false` until the backend lands.
pub fn unlock_achievement(name: &str) -> bool {
    let _ = name;
    false
}

/// Sets an integer stat, such as nights survived. Returns whether Steam
/// took the call; always `false` until the backend lands.
pub fn set_stat(name: &str, value: i32) -> bool {
    let _ = (name, value);
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn calls_do_nothing_without_steam() {
        assert!(!super::is_running());
        assert!(!super::unlock_achievement("night_5"));
        assert!(!super::set_stat("nights_survived", 5));
    }
}
