//! SH-816: bounded, process-local reuse of authoritative Codex registry probes.
//!
//! No provider runs under the map lock. Same-key callers share one probe and
//! wait no longer than its existing deadline + termination grace. Installation
//! invalidation retires in-flight entries too: an old result cannot repopulate
//! the cache after a mutation. Nothing is written to provider configuration.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::error::{AppError, WireError};

const SUCCESS_TTL: Duration = Duration::from_secs(30);
const NEGATIVE_TTL: Duration = Duration::from_secs(5);
const MAX_ENTRIES: usize = 32;
const WAIT_BOUND: Duration = Duration::from_secs(
    super::provider_cli::PROVIDER_CLI_TIMEOUT.as_secs()
        + super::provider_cli::PROVIDER_TERM_GRACE.as_secs(),
);

type Answer = Result<Option<PathBuf>, AppError>;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Stamp {
    Missing,
    Unreadable,
    Present {
        canonical: PathBuf,
        len: u64,
        modified: Option<SystemTime>,
        device: u64,
        inode: u64,
        mode: u32,
        changed: (i64, i64),
    },
}

fn stamp(path: &Path) -> Stamp {
    match fs::metadata(path) {
        Ok(metadata) => Stamp::Present {
            canonical: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Stamp::Missing,
        Err(_) => Stamp::Unreadable,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct Key {
    home: PathBuf,
    cwd: PathBuf,
    path: Option<OsString>,
    xdg: [Option<OsString>; 3],
    provider: Option<(PathBuf, Stamp)>,
    config: Stamp,
}

impl Key {
    fn ambient(home: &Path) -> Result<Self, AppError> {
        let cwd = std::env::current_dir()?;
        let path = std::env::var_os("PATH");
        let xdg = ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME"].map(std::env::var_os);
        Ok(Self::from_context(home, cwd, path, xdg))
    }

    fn from_context(
        home: &Path,
        cwd: PathBuf,
        path: Option<OsString>,
        xdg: [Option<OsString>; 3],
    ) -> Self {
        // PATH is also part of the key when no executable exists, so a missing
        // provider cannot leak to a different context. Relative/empty entries
        // are resolved against cwd, matching Command's executable search.
        let provider = path.as_ref().and_then(|path| {
            std::env::split_paths(path).find_map(|directory| {
                let candidate = cwd.join(directory).join("codex");
                crate::path_identity::is_executable_file(&candidate)
                    .then(|| (candidate.clone(), stamp(&candidate)))
            })
        });
        Self {
            home: home.to_path_buf(),
            cwd,
            path,
            xdg,
            provider,
            config: stamp(&home.join(".codex/config.toml")),
        }
    }
}

#[derive(Clone)]
struct Cached {
    completed: Instant,
    result: Result<Option<PathBuf>, WireError>,
    files: Option<[Stamp; 2]>,
}

impl Cached {
    fn files(root: &Path) -> [Stamp; 2] {
        [
            stamp(&root.join(".codex-plugin/plugin.json")),
            stamp(&root.join("bin/story.sh")),
        ]
    }

    fn new(result: &Answer, completed: Instant) -> Self {
        Self {
            completed,
            result: result.as_ref().map(Clone::clone).map_err(WireError::from),
            files: result
                .as_ref()
                .ok()
                .and_then(|root| root.as_ref())
                .map(|root| Self::files(root)),
        }
    }

    fn current(&self, now: Instant) -> bool {
        let ttl = if matches!(self.result, Ok(Some(_))) {
            SUCCESS_TTL
        } else {
            NEGATIVE_TTL
        };
        if now.saturating_duration_since(self.completed) >= ttl {
            return false;
        }
        match &self.result {
            Ok(Some(root)) => {
                let current = Self::files(root);
                matches!(&current[0], Stamp::Present { mode, .. }
                    if *mode & libc::S_IFMT as u32 == libc::S_IFREG as u32)
                    && !current.contains(&Stamp::Unreadable)
                    && self.files.as_ref() == Some(&current)
            }
            _ => true,
        }
    }

    fn answer(&self) -> Answer {
        self.result.clone().map_err(AppError::from)
    }
}

#[derive(Default)]
struct State {
    probing: bool,
    retired: bool,
    cached: Option<Cached>,
}

#[derive(Default)]
struct Entry {
    state: Mutex<State>,
    finished: Condvar,
}

#[derive(Default)]
struct Cache {
    entries: Mutex<HashMap<Key, Arc<Entry>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Cache {
    fn entry(&self, key: Key) -> Result<Arc<Entry>, AppError> {
        let mut entries = lock(&self.entries);
        if let Some(entry) = entries.get(&key) {
            return Ok(Arc::clone(entry));
        }
        if entries.len() >= MAX_ENTRIES {
            let idle = entries.iter().find_map(|(key, entry)| {
                (Arc::strong_count(entry) == 1 && !lock(&entry.state).probing).then(|| key.clone())
            });
            if let Some(idle) = idle {
                entries.remove(&idle);
            } else {
                return Err(AppError::Storage("Codex helper resolution cache is busy; retry after an active registry probe finishes".into()));
            }
        }
        let entry = Arc::new(Entry::default());
        entries.insert(key, Arc::clone(&entry));
        Ok(entry)
    }

    fn resolve(
        &self,
        key: Key,
        now: impl Fn() -> Instant,
        unchanged: impl Fn() -> bool,
        probe: impl FnOnce() -> Answer,
    ) -> Answer {
        let entry = self.entry(key)?;
        // The wait clock is deliberately real even when TTL arithmetic is
        // scripted in tests. A stalled owner may never strand its followers.
        let wait_until = Instant::now() + WAIT_BOUND;
        let mut state = lock(&entry.state);
        loop {
            if state.retired || !unchanged() {
                return Err(changed());
            }
            if let Some(cached) = &state.cached
                && cached.current(now())
            {
                return cached.answer();
            }
            if !state.probing {
                state.probing = true;
                break;
            }
            let remaining = wait_until.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AppError::Storage(
                    "timed out waiting for the in-flight Codex plugin registry probe".into(),
                ));
            }
            let (next, _) = entry
                .finished
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
        drop(state);
        let _owner = ProbeOwner(&entry);
        let answer = probe();
        let mut state = lock(&entry.state);
        if state.retired || !unchanged() {
            return Err(changed());
        }
        state.cached = Some(Cached::new(&answer, now()));
        answer
        // ProbeOwner releases probing and wakes followers even if probe panics.
    }

    fn invalidate(&self, home: &Path) {
        lock(&self.entries).retain(|key, entry| {
            if key.home == home {
                let mut state = lock(&entry.state);
                state.retired = true;
                state.cached = None;
                entry.finished.notify_all();
                false
            } else {
                true
            }
        });
    }
}

fn changed() -> AppError {
    AppError::Storage("Codex helper resolution context changed during its registry probe; retry with the current installation".into())
}

struct ProbeOwner<'a>(&'a Entry);
impl Drop for ProbeOwner<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).probing = false;
        self.0.finished.notify_all();
    }
}

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Cache::default)
}

pub(super) fn resolve(home: &Path, probe: impl FnOnce() -> Answer) -> Answer {
    let key = Key::ambient(home)?;
    cache().resolve(
        key.clone(),
        Instant::now,
        || Key::ambient(home).is_ok_and(|now| now == key),
        probe,
    )
}

/// Scope this inside the existing provider mutation guard. Both success and
/// failure (including rollback) invalidate; in-flight reads cannot publish old
/// answers. A separate process is observed through config/file stamps and TTL.
pub(super) struct Mutation {
    home: PathBuf,
}

impl Mutation {
    pub(super) fn begin(home: PathBuf) -> Self {
        cache().invalidate(&home);
        Self { home }
    }
}

impl Drop for Mutation {
    fn drop(&mut self) {
        cache().invalidate(&self.home);
    }
}

#[cfg(test)]
mod tests;
