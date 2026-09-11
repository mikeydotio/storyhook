//! `story daemon gc` — reclaims the runtime directory of a store that no
//! longer exists, and keeps everything it cannot prove throwaway (SH-638).
//!
//! Every store a daemon ever served gets `daemons/<key>/` under the state
//! home ([`Environment::daemon_state_dir`]) and nothing removed it: 1,207
//! directories on the filing machine, three with a live store — one per
//! browser-tier run, one per Rust-suite fixture, one per sanctioned
//! `story --store-path /tmp/x …` hand run. SH-633 stopped the harness
//! families accruing; the hand-run family keeps growing at the rate agents
//! try builds.
//!
//! # What makes this safe
//!
//! The key is one-way (a SHA-256 prefix of the canonical store path), so
//! the store a directory belongs to is read from inside it — the portfile's
//! `store_path`, or the `holding` line the daemon writes to its log first —
//! and then **proved**: the parsed path must hash back to the directory's
//! own name, or the directory is kept. A mis-parse cannot reap.
//!
//! An absent store may live on a volume that is merely offline (SH-426),
//! which is the reason nothing reaped these before. The answer is the
//! temp-root gate: only a store under a temp root
//! ([`crate::service::project::is_under_temp`]) is reclaimed, because the
//! operator chose that root and SH-426 already treats such a store as
//! session-scoped. Everything else — a store on `/Volumes`, in a home
//! directory, anywhere durable — is kept and named, with the `rm` the
//! operator may run by hand. A non-default store's `backups/` live inside
//! this directory, so reclaiming destroys the last copy of that store's
//! data; the plan says so out loud, and the gate is what makes it right.
//!
//! The rest is ordinary liveness, done without side effects:
//! [`super::lifecycle::is_live`] creates the pidfile and its directory on
//! the way to probing the lock, which on a foreign key would resurrect what
//! was just removed, so the probe here opens with `create(false)`. A
//! directory younger than [`RECLAIM_AGE_FLOOR`] is never probed at all: a
//! spawn creates the directory before it takes the lock.
//!
//! Design of record: `docs/spec/store-isolation.md`, the SH-638 amendment.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use super::lifecycle::{self, SPAWN_LOCK_DEADLINE};
use crate::env::KEY_HEX;
use crate::env::runtime_file::{LOG, LOG_ROTATED, PIDFILE, PORTFILE, SPAWN_LOCK};
use crate::env::{Environment, StoreLocation};
use crate::service::project::is_under_temp;

/// How old a directory must be before its locks are even probed.
///
/// Derived, not picked: [`SPAWN_LOCK_DEADLINE`] is the longest a client is
/// sanctioned to spend between creating this directory and either holding a
/// lock in it or giving up. A directory younger than that with no lock held
/// may be a spawn that has not taken its lock yet; older than that, it is
/// not.
pub const RECLAIM_AGE_FLOOR: Duration = SPAWN_LOCK_DEADLINE;

/// How much of a log is read looking for the daemon's own first line. The
/// line is written before anything else the daemon says, and the log also
/// carries hook stderr, which is unbounded.
const IDENTITY_READ_CAP: u64 = 64 * 1024;

/// The words the daemon writes between its pid and the store it holds
/// (`lifecycle`'s startup line), matched whole so a hook's stderr cannot
/// impersonate it.
const HOLDING: &str = ") holding ";

/// Why a directory was kept. A code rather than a sentence, so a reader
/// keyed on the reason survives the prose being improved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeepReason {
    /// Not a runtime directory at all: the name is not a store key, or the
    /// entry is not a directory. Never touched.
    NotAKey,
    /// The store this invocation is about.
    ThisStore,
    /// The default store for this home — with or without `$XDG_DATA_HOME`.
    DefaultStore,
    /// Nothing inside names a store.
    Unidentified,
    /// What it names does not hash to its own name: the parse is not trusted.
    IdentityMismatch,
    /// The store is not under a temp root; an absent one may be an offline
    /// volume.
    NotUnderTemp,
    /// The store is still there.
    StoreExists,
    /// A launchd login agent still names the store.
    LoginAgent,
    /// Younger than [`RECLAIM_AGE_FLOOR`].
    TooYoung,
    /// The pidfile lock is held: a daemon is serving it.
    DaemonLive,
    /// The spawn lock is held: a client is starting one.
    SpawnInProgress,
    /// A lock file could not be opened for a reason other than absence.
    Unprobeable,
    /// Reclaimable at survey, taken by someone else by the time it was
    /// reclaimed.
    Raced,
}

impl KeepReason {
    /// The snake_case code the plan prints in brackets — the same spelling
    /// serde writes.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NotAKey => "not_a_key",
            Self::ThisStore => "this_store",
            Self::DefaultStore => "default_store",
            Self::Unidentified => "unidentified",
            Self::IdentityMismatch => "identity_mismatch",
            Self::NotUnderTemp => "not_under_temp",
            Self::StoreExists => "store_exists",
            Self::LoginAgent => "login_agent",
            Self::TooYoung => "too_young",
            Self::DaemonLive => "daemon_live",
            Self::SpawnInProgress => "spawn_in_progress",
            Self::Unprobeable => "unprobeable",
            Self::Raced => "raced",
        }
    }
}

/// A directory the survey would remove.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// The directory's name — the store's key.
    pub key: String,
    /// The directory itself.
    pub path: PathBuf,
    /// The store it served, no longer on disk.
    pub store_path: PathBuf,
    /// Everything under it, in bytes.
    pub bytes: u64,
    /// Backup snapshots of the vanished store inside it — the last copies.
    pub snapshots: usize,
}

/// A directory the survey kept, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    /// The entry's name.
    pub key: String,
    /// The entry itself.
    pub path: PathBuf,
    /// The store it names, when one could be read.
    pub store_path: Option<PathBuf>,
    /// Why it stays.
    pub reason: KeepReason,
    /// The one-line explanation a person reads beside the code.
    pub detail: String,
}

/// What a survey found: the payload of
/// [`crate::output::ConfirmationPlan::RuntimeGc`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeGcPlan {
    /// The directory surveyed.
    pub daemons_dir: PathBuf,
    /// What would be removed.
    pub candidates: Vec<Candidate>,
    /// What stays, and why.
    pub kept: Vec<Kept>,
}

impl RuntimeGcPlan {
    /// Bytes across every candidate.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.candidates.iter().map(|c| c.bytes).sum()
    }

    /// How many candidates hold at least one backup snapshot.
    #[must_use]
    pub fn with_snapshots(&self) -> usize {
        self.candidates.iter().filter(|c| c.snapshots > 0).count()
    }

    /// The body every reader of this plan sees: what would go, then what
    /// stays and why. The `[code]` beside a kept entry is
    /// [`KeepReason::code`].
    #[must_use]
    pub fn render(&self) -> String {
        let mut body = String::new();
        if self.candidates.is_empty() {
            body.push_str(&format!(
                "nothing to reclaim under {}\n",
                self.daemons_dir.display()
            ));
        } else {
            body.push_str(&format!(
                "{} runtime director{} ({}) under {}, for stores that no longer exist:\n",
                self.candidates.len(),
                plural_y(self.candidates.len()),
                describe_bytes(self.bytes()),
                self.daemons_dir.display()
            ));
            for candidate in &self.candidates {
                body.push_str(&format!(
                    "  {}  {}  ({}, {} backup snapshot{})\n",
                    candidate.key,
                    candidate.store_path.display(),
                    describe_bytes(candidate.bytes),
                    candidate.snapshots,
                    plural_s(candidate.snapshots)
                ));
            }
            let with_snapshots = self.with_snapshots();
            if with_snapshots > 0 {
                body.push_str(&format!(
                    "{with_snapshots} of these hold backup snapshots of the vanished store — \
                     the last copies of its data; they go too.\n"
                ));
            }
        }
        if !self.kept.is_empty() {
            body.push_str(&format!(
                "kept {} (never reclaimed automatically):\n",
                self.kept.len()
            ));
            for kept in &self.kept {
                body.push_str(&format!(
                    "  {} [{}]: {}\n",
                    kept.key,
                    kept.reason.code(),
                    kept.detail
                ));
            }
        }
        body
    }
}

/// What [`reclaim`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeGcReport {
    /// Removed for real.
    pub removed: Vec<Candidate>,
    /// Kept at survey time, plus anything that was raced away in between.
    pub kept: Vec<Kept>,
    /// Removal was attempted and failed, with the error.
    pub failed: Vec<(Candidate, String)>,
}

impl RuntimeGcReport {
    /// Bytes freed.
    #[must_use]
    pub fn reclaimed_bytes(&self) -> u64 {
        self.removed.iter().map(|c| c.bytes).sum()
    }

    /// The success message.
    #[must_use]
    pub fn message(&self) -> String {
        let mut body = format!(
            "removed {} runtime director{} ({})\n",
            self.removed.len(),
            plural_y(self.removed.len()),
            describe_bytes(self.reclaimed_bytes())
        );
        for candidate in &self.removed {
            body.push_str(&format!(
                "  {}  {}\n",
                candidate.key,
                candidate.store_path.display()
            ));
        }
        if !self.kept.is_empty() {
            body.push_str(&format!(
                "kept {} (never reclaimed automatically):\n",
                self.kept.len()
            ));
            for kept in &self.kept {
                body.push_str(&format!(
                    "  {} [{}]: {}\n",
                    kept.key,
                    kept.reason.code(),
                    kept.detail
                ));
            }
        }
        body.trim_end().to_string()
    }

    /// One warning per failed removal — a directory that is still there and
    /// still counted next time.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.failed
            .iter()
            .map(|(candidate, error)| {
                format!("could not remove {}: {error}", candidate.path.display())
            })
            .collect()
    }
}

/// Surveys `daemons/` under `env`'s state home without changing anything.
#[must_use]
pub fn survey(env: &Environment) -> RuntimeGcPlan {
    let daemons_dir = env.daemons_dir();
    let mut plan = RuntimeGcPlan {
        daemons_dir: daemons_dir.clone(),
        candidates: Vec::new(),
        kept: Vec::new(),
    };
    let Ok(entries) = fs::read_dir(&daemons_dir) else {
        return plan;
    };
    let exclusions = Exclusions::for_env(env);
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        match classify(env, &exclusions, &path) {
            Verdict::Reclaim(candidate) => plan.candidates.push(candidate),
            Verdict::Keep(kept) => plan.kept.push(kept),
        }
    }
    plan
}

/// Removes every candidate in `plan`, re-checking each under the locks its
/// owner would hold.
#[must_use]
pub fn reclaim(plan: RuntimeGcPlan) -> RuntimeGcReport {
    let mut report = RuntimeGcReport {
        removed: Vec::new(),
        kept: plan.kept,
        failed: Vec::new(),
    };
    for candidate in plan.candidates {
        match remove(&candidate) {
            Ok(()) => report.removed.push(candidate),
            Err(Removal::Raced(detail)) => report.kept.push(Kept {
                key: candidate.key.clone(),
                path: candidate.path.clone(),
                store_path: Some(candidate.store_path.clone()),
                reason: KeepReason::Raced,
                detail,
            }),
            Err(Removal::Failed(error)) => report.failed.push((candidate, error)),
        }
    }
    report
}

/// The line `story daemon status` prints when something is reclaimable, or
/// nothing when nothing is — so the sweeper is named at the moment the
/// accumulation is visible, not only when someone thinks to run it
/// (SH-418).
#[must_use]
pub fn describe(env: &Environment) -> String {
    let plan = survey(env);
    if plan.candidates.is_empty() {
        return String::new();
    }
    format!(
        "{} runtime director{} ({}) for stores that no longer exist can be reclaimed: \
         story daemon gc",
        plan.candidates.len(),
        plural_y(plan.candidates.len()),
        describe_bytes(plan.bytes())
    )
}

/// The keys no survey may ever offer for removal, whatever is inside them.
struct Exclusions {
    this_store: String,
    default_stores: Vec<String>,
}

impl Exclusions {
    fn for_env(env: &Environment) -> Self {
        // Both defaults: the one this process's `$XDG_DATA_HOME` selects,
        // and the one a launchd agent — which has no shell environment —
        // would open for the same home.
        let default_stores = vec![
            StoreLocation::key_for_path(env.store().default_path()),
            StoreLocation::for_home(env.home()).key(),
        ];
        Self {
            this_store: env.store().key(),
            default_stores,
        }
    }
}

enum Verdict {
    Reclaim(Candidate),
    Keep(Kept),
}

fn classify(env: &Environment, exclusions: &Exclusions, path: &Path) -> Verdict {
    let key = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let keep = |reason: KeepReason, store_path: Option<PathBuf>, detail: String| {
        Verdict::Keep(Kept {
            key: key.clone(),
            path: path.to_path_buf(),
            store_path,
            reason,
            detail,
        })
    };
    if !is_key(&key) || !path.is_dir() {
        return keep(
            KeepReason::NotAKey,
            None,
            "not a store's runtime directory".to_string(),
        );
    }
    if key == exclusions.this_store {
        return keep(
            KeepReason::ThisStore,
            None,
            "the store this command is about".to_string(),
        );
    }
    if exclusions.default_stores.contains(&key) {
        return keep(
            KeepReason::DefaultStore,
            None,
            "the default store's own runtime directory".to_string(),
        );
    }
    let Some(store) = identity(path) else {
        return keep(
            KeepReason::Unidentified,
            None,
            "nothing inside names the store it served".to_string(),
        );
    };
    if StoreLocation::key_for_path(&store) != key {
        return keep(
            KeepReason::IdentityMismatch,
            Some(store.clone()),
            format!(
                "names {}, which does not key this directory",
                store.display()
            ),
        );
    }
    if !is_under_temp(&store) {
        return keep(
            KeepReason::NotUnderTemp,
            Some(store.clone()),
            format!(
                "{} is not under a temp root; if it is gone for good rather than on an \
                 offline volume, remove the directory by hand: rm -rf {}",
                store.display(),
                path.display()
            ),
        );
    }
    if fs::symlink_metadata(&store).is_ok() {
        return keep(
            KeepReason::StoreExists,
            Some(store.clone()),
            format!("{} still exists", store.display()),
        );
    }
    if let Some(plist) = super::agent::agent_serving(env, &store) {
        return keep(
            KeepReason::LoginAgent,
            Some(store.clone()),
            format!(
                "a login agent still names {} ({}); launchd would recreate this at login — \
                 story --store-path {} daemon uninstall",
                store.display(),
                plist.display(),
                store.display()
            ),
        );
    }
    match age(path) {
        Some(age) if age >= RECLAIM_AGE_FLOOR => {}
        _ => {
            return keep(
                KeepReason::TooYoung,
                Some(store.clone()),
                format!(
                    "changed less than {}s ago; a spawn may still be taking its lock",
                    RECLAIM_AGE_FLOOR.as_secs()
                ),
            );
        }
    }
    match probe(&path.join(PIDFILE)) {
        Probe::Free => {}
        Probe::Held => {
            return keep(
                KeepReason::DaemonLive,
                Some(store.clone()),
                format!(
                    "a daemon holds the pidfile and is serving {}, which no longer exists — \
                     story --store-path {} daemon stop",
                    store.display(),
                    store.display()
                ),
            );
        }
        Probe::Unprobeable(error) => {
            return keep(
                KeepReason::Unprobeable,
                Some(store.clone()),
                format!("could not open the pidfile to ask whether it is held: {error}"),
            );
        }
    }
    match probe(&path.join(SPAWN_LOCK)) {
        Probe::Free => {}
        Probe::Held => {
            return keep(
                KeepReason::SpawnInProgress,
                Some(store.clone()),
                "a client holds the spawn lock: a daemon is starting".to_string(),
            );
        }
        Probe::Unprobeable(error) => {
            return keep(
                KeepReason::Unprobeable,
                Some(store.clone()),
                format!("could not open the spawn lock to ask whether it is held: {error}"),
            );
        }
    }
    Verdict::Reclaim(Candidate {
        key,
        path: path.to_path_buf(),
        store_path: store,
        bytes: bytes_under(path),
        snapshots: snapshots_under(path),
    })
}

/// Whether `name` is the shape [`StoreLocation::key`] produces.
fn is_key(name: &str) -> bool {
    name.len() == KEY_HEX
        && name
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The store this directory served, read from what its daemon left behind:
/// the portfile first (structured, but deleted on a clean exit), then the
/// daemon's own first log line, then the rotated log from the spawn before.
fn identity(dir: &Path) -> Option<PathBuf> {
    if let Some(info) = lifecycle::read_info_at(&dir.join(PORTFILE))
        && !info.store_path.as_os_str().is_empty()
    {
        return Some(info.store_path);
    }
    [LOG, LOG_ROTATED]
        .iter()
        .find_map(|name| holding_path(&read_capped(&dir.join(name))?))
}

/// The first `IDENTITY_READ_CAP` bytes of `path`, lossily decoded.
fn read_capped(path: &Path) -> Option<String> {
    let mut buffer = Vec::new();
    File::open(path)
        .ok()?
        .take(IDENTITY_READ_CAP)
        .read_to_end(&mut buffer)
        .ok()?;
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

/// The store named by the daemon's startup line, read whole to the end of
/// its line — a path may contain a space (SH-493), so the first field is
/// not the path. Matched strictly: the line must be the daemon's own
/// (`storyhook daemon …`) and carry `) holding `, so a hook's stderr on the
/// same stream cannot supply one.
fn holding_path(text: &str) -> Option<PathBuf> {
    text.lines().find_map(|line| {
        let line = line.strip_prefix("storyhook daemon ")?;
        let (_, rest) = line.split_once(HOLDING)?;
        let rest = rest.trim_end();
        (!rest.is_empty()).then(|| PathBuf::from(rest))
    })
}

/// How long ago anything in `dir` last changed: the newest mtime among the
/// directory and its direct children. `None` when nothing can be read, which
/// the caller treats as too young.
fn age(dir: &Path) -> Option<Duration> {
    let mut newest = fs::metadata(dir).and_then(|m| m.modified()).ok()?;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
            newest = newest.max(modified);
        }
    }
    SystemTime::now().duration_since(newest).ok()
}

enum Probe {
    Free,
    Held,
    Unprobeable(std::io::Error),
}

/// Whether the `flock` on `path` is held — without creating the file, which
/// is what [`lifecycle::is_live`] does and must not happen on a foreign key.
/// A missing file is a free lock: nobody can hold what does not exist.
fn probe(path: &Path) -> Probe {
    match File::options().read(true).open(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Probe::Free,
        Err(error) => Probe::Unprobeable(error),
        Ok(file) => match file.try_lock_exclusive() {
            Ok(()) => {
                let _ = FileExt::unlock(&file);
                Probe::Free
            }
            Err(_) => Probe::Held,
        },
    }
}

/// Takes the `flock` on `path` for the caller to hold, or reports why not.
/// A missing file yields no handle and no objection.
fn take(path: &Path) -> Result<Option<File>, String> {
    match File::options().read(true).open(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not open {}: {error}", path.display())),
        Ok(file) => match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(file)),
            Err(_) => Err(format!("{} is held", path.display())),
        },
    }
}

enum Removal {
    Raced(String),
    Failed(String),
}

/// Removes one candidate, holding both of its locks across the check and the
/// removal so a daemon or spawner that arrives in between is refused rather
/// than raced. The locks live on files inside the directory being removed,
/// which POSIX permits: the handles outlive the names.
fn remove(candidate: &Candidate) -> Result<(), Removal> {
    let _pid = take(&candidate.path.join(PIDFILE)).map_err(Removal::Raced)?;
    let _spawn = take(&candidate.path.join(SPAWN_LOCK)).map_err(Removal::Raced)?;
    if fs::symlink_metadata(&candidate.store_path).is_ok() {
        return Err(Removal::Raced(format!(
            "{} reappeared",
            candidate.store_path.display()
        )));
    }
    if !age(&candidate.path).is_some_and(|age| age >= RECLAIM_AGE_FLOOR) {
        return Err(Removal::Raced(
            "changed since the survey; a spawn may be taking its lock".to_string(),
        ));
    }
    fs::remove_dir_all(&candidate.path).map_err(|error| Removal::Failed(error.to_string()))
}

/// Bytes under `dir`, following no symlinks.
fn bytes_under(dir: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Backup snapshots under `dir`: every regular file in the two places
/// [`Environment::per_store_state`] hangs a non-default store's backups.
fn snapshots_under(dir: &Path) -> usize {
    ["backups", "maintenance/backups"]
        .iter()
        .filter_map(|sub| fs::read_dir(dir.join(sub)).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .count()
}

fn plural_y(n: usize) -> &'static str {
    if n == 1 { "y" } else { "ies" }
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// `1.2 MB`-style, for a plan a person reads; the exact byte count travels
/// in the structured plan.
fn describe_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_path_reads_the_rest_of_the_line_whole() {
        let text = "storyhook daemon 2.4.2 on http://127.0.0.1:1 (pid 7) holding /private/tmp/Ada \
                    Lovelace/store.db\nstoryhook daemon: wrote a backup\n";
        assert_eq!(
            holding_path(text),
            Some(PathBuf::from("/private/tmp/Ada Lovelace/store.db"))
        );
    }

    #[test]
    fn holding_path_is_found_below_a_banner() {
        let text = "storyhook daemon: wrote a backup to /x\nStoryhook dashboard (tailnet): \
                    http://100.1.1.1:1\nstoryhook daemon 2.4.2 on http://127.0.0.1:1 (pid 7) \
                    holding /private/tmp/s.db\n";
        assert_eq!(holding_path(text), Some(PathBuf::from("/private/tmp/s.db")));
    }

    #[test]
    fn holding_path_ignores_a_line_the_daemon_did_not_write() {
        let hook = "hook says: (pid 1) holding /etc/passwd\nerror: unable to open database file\n";
        assert_eq!(holding_path(hook), None);
        assert_eq!(
            holding_path("storyhook daemon 2.4.2 (pid 7) holding \n"),
            None
        );
    }

    #[test]
    fn a_key_is_sixteen_lowercase_hex_digits() {
        assert!(is_key("000427cc0cff49bd"));
        assert!(!is_key("000427CC0CFF49BD"));
        assert!(!is_key("000427cc0cff49b"));
        assert!(!is_key("not-a-key"));
        assert!(!is_key(""));
    }

    #[test]
    fn a_planted_identity_hashes_back_to_its_key() {
        let store = PathBuf::from("/private/tmp/storyhook-tests/x/store.db");
        let key = StoreLocation::key_for_path(&store);
        assert!(is_key(&key));
        let text = format!(
            "storyhook daemon 2.4.2 on http://127.0.0.1:1 (pid 7) holding {}\n",
            store.display()
        );
        assert_eq!(
            StoreLocation::key_for_path(&holding_path(&text).unwrap()),
            key
        );
    }

    #[test]
    fn bytes_read_the_way_a_person_does() {
        assert_eq!(describe_bytes(12), "12 B");
        assert_eq!(describe_bytes(12_345), "12.3 KB");
        assert_eq!(describe_bytes(442_000_000), "442.0 MB");
    }

    #[test]
    fn the_age_floor_is_the_spawn_lock_deadline() {
        assert_eq!(RECLAIM_AGE_FLOOR, SPAWN_LOCK_DEADLINE);
    }
}
