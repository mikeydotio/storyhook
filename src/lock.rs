//! # QUARANTINED — legacy-web only, deleted at W5
//!
//! This module is part of the pre-rearchitecture storage path. Story data lives
//! in a single global store now ([`crate::store`]), and no `story` command
//! reaches this code. It survives for exactly one reason: the web dashboard
//! (`src/web.rs`) still reads `.storyhook/` directories directly, and the wave
//! that promotes the daemon is what moves it onto the store and deletes this.
//!
//! **Do not add callers.** `tests/invoker_seam.rs::the_legacy_path_is_reachable_
//! only_from_the_web_dashboard` fails if any module other than `web.rs` reaches
//! it, so an accidental dependency is a failing test rather than a surprise a
//! wave later.
//!
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use fs4::FileExt;

use crate::error::AppError;

pub fn with_project_lock<T>(
    root: &Path,
    action: impl FnOnce() -> Result<T, AppError>,
) -> Result<T, AppError> {
    let lock_dir = root.join(".storyhook");
    fs::create_dir_all(&lock_dir)?;
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_dir.join("lock"))?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match lock_file.try_lock_exclusive() {
            Ok(()) => break,
            Err(error) => {
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    if Instant::now() >= deadline {
                        return Err(AppError::LockTimeout(
                            "timed out waiting for the project write lock".to_string(),
                        ));
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
                return Err(AppError::Storage(format!(
                    "failed to acquire project lock: {error}"
                )));
            }
        }
    }

    let result = action();
    if let Err(error) = lock_file.unlock() {
        return Err(AppError::Storage(format!(
            "failed to release project lock: {error}"
        )));
    }
    result
}
