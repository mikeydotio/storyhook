//! Where storyhook's *global* state lives.
//!
//! Everything here is outside any repository. Story data is moving into one
//! database per machine rather than one directory per checkout, and the
//! artifacts that go with it — pre-migration backups, the dashboard's runtime
//! files — belong beside it rather than inside a repository a user commits.
//!
//! The layout is XDG's, with `STORYHOOK_DATA_DIR` overriding all of it. Both are
//! read from the environment on every call rather than cached: a test harness
//! isolates a child process by setting these variables, and a value latched at
//! first use would leak one test's directory into the next.

use std::path::PathBuf;

use crate::error::AppError;

/// The directory holding the store itself.
///
/// `$STORYHOOK_DATA_DIR`, else `$XDG_DATA_HOME/storyhook`, else
/// `~/.local/share/storyhook`.
pub fn data_dir() -> Result<PathBuf, AppError> {
    if let Some(explicit) = env_path("STORYHOOK_DATA_DIR") {
        return Ok(explicit);
    }
    if let Some(xdg) = env_path("XDG_DATA_HOME") {
        return Ok(xdg.join("storyhook"));
    }
    Ok(home()?.join(".local/share/storyhook"))
}

/// The directory holding state that is regenerable but should survive a reboot:
/// backups, logs, pid files.
///
/// `$XDG_STATE_HOME/storyhook`, else `~/.local/state/storyhook`. Deliberately
/// *not* covered by `STORYHOOK_DATA_DIR`, which names where the data is, so that
/// pointing storyhook at a synced data directory does not also start syncing
/// its runtime scratch.
pub fn state_dir() -> Result<PathBuf, AppError> {
    if let Some(xdg) = env_path("XDG_STATE_HOME") {
        return Ok(xdg.join("storyhook"));
    }
    Ok(home()?.join(".local/state/storyhook"))
}

/// The store's database file.
pub fn store_path() -> Result<PathBuf, AppError> {
    Ok(data_dir()?.join("store.db"))
}

/// One environment variable as a path, ignoring an empty value.
///
/// An empty `XDG_DATA_HOME` is what a shell leaves behind when an export is
/// unset the careless way, and joining `storyhook` onto it would silently make a
/// *relative* path — a store in whatever directory the process happened to
/// start in.
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// The user's home directory.
fn home() -> Result<PathBuf, AppError> {
    env_path("HOME")
        .ok_or_else(|| AppError::Storage("could not determine home directory".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_xdg_variable_is_ignored_rather_than_joined_onto() {
        // Joining onto "" yields a relative path, which would put a user's
        // whole tracker wherever the process happened to start.
        assert_eq!(env_path("STORYHOOK_PATHS_TEST_EMPTY"), None);
    }

    #[test]
    fn the_store_lives_inside_the_data_directory() {
        let dir = data_dir().expect("a data directory");
        let store = store_path().expect("a store path");
        assert_eq!(store.parent(), Some(dir.as_path()));
        assert_eq!(store.file_name().unwrap(), "store.db");
    }
}
