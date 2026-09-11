//! Files compiled into the binary and projected onto disk in lockstep with it.
//!
//! `build.rs` generates one `&[EmbeddedFile]` table per payload — the provider
//! plugin marketplace (SH-538, consumed by [`crate::plugin`]) and the verifier
//! script family (SH-654, consumed by [`crate::daemon::verifier_bundle`]).
//! Both are projected with the same three operations so there is one opinion
//! about what "the on-disk copy matches this binary" means: [`matches`] is the
//! comparison, [`write`] is the projection, and [`materialize`] is the
//! transaction around them — reuse an exact existing tree, otherwise stage a
//! fresh one beside the destination, verify it, and publish it by rename, with
//! the previous tree restored if publication fails.
//!
//! The release-lockstep rule this serves (`docs/spec/release-lockstep.md`): a
//! projection of a release is owned by the binary that carries the bytes, so
//! it is rewritten from those bytes whenever it disagrees, never edited in
//! place and never trusted because a path of the right name exists.

use std::collections::BTreeSet;
use std::fs;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use fs4::FileExt;

use crate::error::AppError;

/// One file of an embedded payload: where it lands relative to the payload
/// root, the bytes the binary carries for it, and whether it is executable.
///
/// Instances are written by `build.rs` into a generated `include!` table, so
/// the field set here is the build script's output contract.
pub(crate) struct EmbeddedFile {
    /// Destination path relative to the payload root, `/`-separated, with
    /// only normal components (the build script refuses anything else).
    pub(crate) relative_path: &'static str,
    /// The file's exact contents.
    pub(crate) bytes: &'static [u8],
    /// Whether the on-disk copy must carry an execute bit.
    pub(crate) executable: bool,
}

/// Every regular file beneath `root`, relative to it.
///
/// `None` when `root` cannot be read in full or holds anything other than
/// regular files and directories (a symlink, a socket) — a tree that cannot be
/// enumerated exactly cannot be said to match anything.
pub(crate) fn file_set(root: &Path) -> Option<BTreeSet<PathBuf>> {
    fn visit(root: &Path, directory: &Path, found: &mut BTreeSet<PathBuf>) -> Option<()> {
        let mut entries: Vec<_> = fs::read_dir(directory)
            .ok()?
            .collect::<Result<_, _>>()
            .ok()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).ok()?;
            if metadata.is_dir() {
                visit(root, &path, found)?;
            } else if metadata.is_file() {
                found.insert(path.strip_prefix(root).ok()?.to_path_buf());
            } else {
                return None;
            }
        }
        Some(())
    }

    let mut found = BTreeSet::new();
    visit(root, root, &mut found)?;
    Some(found)
}

/// Whether the tree at `root` is exactly `files`: the same set of paths, each
/// with the same bytes and the same executable bit, and nothing else.
pub(crate) fn matches(files: &[EmbeddedFile], root: &Path) -> bool {
    let Some(found) = file_set(root) else {
        return false;
    };
    let expected: BTreeSet<PathBuf> = files
        .iter()
        .map(|file| PathBuf::from(file.relative_path))
        .collect();
    if found != expected {
        return false;
    }
    files.iter().all(|file| {
        let path = root.join(file.relative_path);
        let Ok(metadata) = fs::metadata(&path) else {
            return false;
        };
        let executable = {
            #[cfg(unix)]
            {
                metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            {
                false
            }
        };
        executable == file.executable && fs::read(path).is_ok_and(|bytes| bytes == file.bytes)
    })
}

/// Writes `files` beneath `root`, creating directories as needed and setting
/// `0755`/`0644` by each file's executable bit.
pub(crate) fn write(files: &[EmbeddedFile], root: &Path) -> Result<(), AppError> {
    for file in files {
        let path = root.join(file.relative_path);
        let parent = path.parent().ok_or_else(|| {
            AppError::Storage(format!(
                "embedded path `{}` has no parent",
                file.relative_path
            ))
        })?;
        fs::create_dir_all(parent)?;
        fs::write(&path, file.bytes)?;
        #[cfg(unix)]
        {
            let mode = if file.executable { 0o755 } else { 0o644 };
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

/// Projects `files` onto `destination`, a directory directly beneath `parent`.
///
/// Serialized by an exclusive lock on `parent/<lock_name>` so two concurrent
/// callers never stage over each other. An existing tree that already
/// [`matches`] is reused untouched. Otherwise a fresh tree is staged in a
/// sibling temporary directory (same parent, so the final `rename` is atomic),
/// verified against `files`, and swapped in; a previous tree at `destination`
/// is moved aside first and put back if the swap fails, so a failed publish
/// never leaves the destination empty. `what` names the payload in errors.
pub(crate) fn materialize(
    files: &[EmbeddedFile],
    parent: &Path,
    destination: &Path,
    lock_name: &str,
    what: &str,
) -> Result<PathBuf, AppError> {
    fs::create_dir_all(parent)?;
    let lock_path = parent.join(lock_name);
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|error| {
            AppError::Storage(format!(
                "failed to open {what} lock `{}`: {error}",
                lock_path.display()
            ))
        })?;
    FileExt::lock_exclusive(&lock).map_err(|error| {
        AppError::Storage(format!(
            "failed to lock {what} at `{}`: {error}",
            lock_path.display()
        ))
    })?;

    if matches(files, destination) {
        return Ok(destination.to_path_buf());
    }

    let staged = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(parent)?;
    write(files, staged.path())?;
    if !matches(files, staged.path()) {
        return Err(AppError::Storage(format!(
            "the staged {what} did not match its embedded payload"
        )));
    }

    if destination.exists() || fs::symlink_metadata(destination).is_ok() {
        let backup = tempfile::Builder::new()
            .prefix(".previous-")
            .tempdir_in(parent)?;
        let previous = backup.path().join("previous");
        fs::rename(destination, &previous)?;
        if let Err(error) = fs::rename(staged.path(), destination) {
            let restore = fs::rename(&previous, destination);
            return Err(AppError::Storage(match restore {
                Ok(()) => format!(
                    "failed to publish {what} at `{}`; restored the previous copy: {error}",
                    destination.display()
                ),
                Err(restore_error) => format!(
                    "failed to publish {what} at `{}` ({error}) and failed to restore its previous copy ({restore_error})",
                    destination.display()
                ),
            }));
        }
    } else {
        fs::rename(staged.path(), destination)?;
    }
    Ok(destination.to_path_buf())
}
