//! The verifier script family, projected from this binary onto disk (SH-654).
//!
//! `scripts/verify-pr.sh` and the scripts it reaches through its own
//! directory — `merge-watch.sh`, `merge-preflight.sh`, `land-pr.sh`,
//! `machine-lock.sh`, `gate-progress.sh`, `verify-window.sh`,
//! `activity-log.sh`, `activity-run.py`, `test_output.py` — are compiled
//! into the binary by `build.rs` (`VERIFIER_SCRIPTS`) and written here before
//! a verification runs. Until SH-654 the daemon spawned
//! `scripts/verify-pr.sh` relative to the **registered project's checkout**,
//! so the only project that could ever be verified was storyhook itself; any
//! other registered project halted the queue with "returned invalid JSON".
//! The checkout now contributes what is genuinely the project's — its
//! `[verify] gate` argv and the receipt store under its git common dir — and
//! the verifier's own mechanics travel with the daemon that invokes them.
//!
//! # Why the binary, not the plugin payload
//!
//! The scripts answer to a daemon↔script wire contract (the JSON result,
//! `<pr-url> -- <gate…>`, `$STORYHOOK_GATE_PROGRESS`). The plugin is
//! provider-scoped, installed only where a provider is, and can skew from the
//! daemon — `REQUIRED_DISPATCH_PROTOCOL` exists because it does. A payload
//! the daemon carries cannot skew from it, which is the SH-530 lockstep rule
//! by construction rather than by check.
//!
//! # Where, and why content-addressed
//!
//! The projection lives under the daemon's store-keyed state directory
//! ([`Environment::daemon_state_dir`]) — one store, one daemon (SH-113), so a
//! test store's bundle dies with its runtime directory and `story daemon gc`
//! (SH-638) reaps it with the rest. The leaf is named by a digest of the
//! payload itself (`verifier/<digest>/`), never by the crate version: two
//! builds of one version with different script bytes (any dev build) must
//! never rewrite a directory a running `verify-pr.sh` is resolving its
//! siblings from, and a different payload writing a different leaf makes
//! that structural. Inside one leaf the bytes are still verified and
//! repaired through [`crate::embedded::materialize`], the same transaction
//! the plugin marketplace uses (SH-538). Leaves left by earlier builds are
//! swept after a successful materialization — best effort, each failure
//! journaled, never fatal to the verification that found them.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::embedded::EmbeddedFile;
use crate::env::Environment;
use crate::error::AppError;

include!(concat!(env!("OUT_DIR"), "/embedded_verifier.rs"));

/// The entry point the daemon runs; the rest of the bundle is its siblings.
pub const VERIFY_SCRIPT: &str = "verify-pr.sh";

/// The directory beneath the daemon state dir that holds every leaf.
const BUNDLE_DIR: &str = "verifier";

/// How many hex characters of the payload digest name a leaf — the same
/// width `StoreLocation::key` uses for the state directory above it.
const LEAF_HEX: usize = 16;

/// Every file in the bundle: its path relative to the bundle root, whether
/// it is executable, and its bytes. Public so `tests/verifier_bundle.rs`
/// can derive its checks from the table the build actually wrote rather
/// than from a second list.
pub fn files() -> impl Iterator<Item = (&'static str, bool, &'static [u8])> {
    EMBEDDED_VERIFIER
        .iter()
        .map(|file| (file.relative_path, file.executable, file.bytes))
}

/// The digest that names this payload's leaf: over every file's path,
/// executable bit and bytes, in table order (the table is sorted by path
/// at build time, so the digest is a function of the content alone).
fn payload_digest() -> String {
    let mut hasher = Sha256::new();
    for file in EMBEDDED_VERIFIER {
        hasher.update(file.relative_path.as_bytes());
        hasher.update([0, u8::from(file.executable)]);
        hasher.update((file.bytes.len() as u64).to_le_bytes());
        hasher.update(file.bytes);
    }
    format!("{:x}", hasher.finalize())[..LEAF_HEX].to_string()
}

/// The parent of every leaf for this daemon's store.
pub fn bundle_root(env: &Environment) -> PathBuf {
    env.daemon_state_dir().join(BUNDLE_DIR)
}

/// The leaf this binary's payload projects to, whether or not it exists yet.
pub fn bundle_dir(env: &Environment) -> PathBuf {
    bundle_root(env).join(payload_digest())
}

/// Projects the bundle for this binary and returns its directory.
///
/// Reuses an exact existing leaf, otherwise stages and publishes one under
/// the bundle root's lock, then sweeps sibling leaves from other builds.
pub fn materialize(env: &Environment) -> Result<PathBuf, AppError> {
    let root = bundle_root(env);
    let leaf = bundle_dir(env);
    let dir = crate::embedded::materialize(
        EMBEDDED_VERIFIER,
        &root,
        &leaf,
        ".materialize.lock",
        "verifier scripts",
    )?;
    sweep_stale_leaves(&root, &leaf);
    Ok(dir)
}

/// The path of `verify-pr.sh` for this binary, materializing the bundle
/// first. What the actuator hands to `bash`.
pub fn verify_script(env: &Environment) -> Result<PathBuf, AppError> {
    Ok(materialize(env)?.join(VERIFY_SCRIPT))
}

/// Removes every directory beneath `root` that is neither `keep` nor one of
/// the materializer's own dot-prefixed staging/backup/lock entries.
///
/// Only a daemon of another build wrote them, and a store has one daemon at
/// a time whose verifications are drained before it exits, so nothing can be
/// running from them. A failure to remove one is reported to the activity
/// journal and otherwise ignored: the leaf in use is already intact, and
/// halting the queue over garbage would be the wrong trade.
fn sweep_stale_leaves(root: &Path, keep: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == keep
            || path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            || !entry.file_type().is_ok_and(|kind| kind.is_dir())
        {
            continue;
        }
        if let Err(error) = fs::remove_dir_all(&path) {
            super::activity::emit(
                "WARN",
                "verifier",
                "bundle",
                "",
                &format!(
                    "could not remove a stale verifier bundle at {}: {error}",
                    path.display()
                ),
            );
        }
    }
}
