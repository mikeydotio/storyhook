//! Journal directories ignore themselves (SH-771).
//!
//! A project journal lives inside a registered checkout, at
//! [`super::PROJECT_JOURNAL`], and nothing in that repository ignores it.
//! Storyhook does not edit a repository's tracked `.gitignore`: a tracked
//! edit dirties the working tree and needs a commit. So every journal
//! directory carries its own ignore file, whose `*` ignores the journal and
//! the ignore file itself. Git gives a deeper ignore file precedence over
//! every shallower one, so the repository's own rules cannot re-include the
//! journal. Files already in the index stay tracked whatever an ignore file
//! says; [`super::hygiene`] reports those.
//!
//! Every writer calls [`prepare`] before it opens a journal file, so no
//! journal file exists in a directory that git can see. Every writer means
//! both languages: `scripts/activity-run.py` writes the same bytes by the
//! same rule, and `tests/activity_script.rs` compares the two.

use std::fs::{DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

/// The ignore file's name inside a journal directory.
pub const IGNORE_FILE: &str = ".gitignore";

/// The exact bytes of a journal directory's ignore file. ASCII only, so that
/// the Python writer's byte comparison cannot depend on its locale.
pub const JOURNAL_IGNORE: &[u8] =
    b"# Created by storyhook automatically: local activity journal, not source.\n*\n";

/// Creates `directory` privately if it is absent, then makes sure it holds
/// exactly [`JOURNAL_IGNORE`], writing it again when it is missing or
/// different.
///
/// The check is one small read, so writers run it before every journal
/// open: an ignore file deleted during a long gate is back before the next
/// record. A replacement is written beside the target and renamed over it,
/// so a reader sees either the old file or the whole new one.
///
/// # Errors
///
/// Any failure to create the directory or to write the ignore file. The
/// caller must then write no journal file: the record would be visible to
/// git.
pub(crate) fn prepare(directory: &Path) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    if is_current(&directory.join(IGNORE_FILE))? {
        return Ok(());
    }
    replace(directory)
}

/// Whether `path` is a regular file holding exactly [`JOURNAL_IGNORE`].
///
/// A symlink is replaced, never followed: its target is outside this
/// directory's ownership. A FIFO is opened without blocking and then
/// replaced, so a hostile file type cannot wedge a daemon thread.
fn is_current(path: &Path) -> io::Result<bool> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(false),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(false);
    }
    let mut bytes = Vec::with_capacity(JOURNAL_IGNORE.len() + 1);
    // One byte past the expected length is enough to prove a difference.
    file.take(JOURNAL_IGNORE.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes == JOURNAL_IGNORE)
}

/// Writes [`JOURNAL_IGNORE`] beside the target under a unique name, then
/// renames it over [`IGNORE_FILE`]. Concurrent writers each rename their own
/// complete copy of the same bytes.
fn replace(directory: &Path) -> io::Result<()> {
    let mut staged = tempfile::Builder::new()
        .prefix(".gitignore.")
        .tempfile_in(directory)?;
    staged.write_all(JOURNAL_IGNORE)?;
    staged
        .persist(directory.join(IGNORE_FILE))
        .map(drop)
        .map_err(|error| error.error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    /// Every entry name in `directory`, sorted: a leftover temporary file
    /// shows up here.
    fn entries(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn logs(root: &Path) -> PathBuf {
        root.join("checkout with spaces/.storyhook/logs")
    }

    fn ignore_bytes(directory: &Path) -> Vec<u8> {
        std::fs::read(directory.join(IGNORE_FILE)).unwrap()
    }

    #[test]
    fn an_absent_directory_is_created_private_with_only_the_ignore_file() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        prepare(&directory).unwrap();
        assert_eq!(entries(&directory), [IGNORE_FILE]);
        assert_eq!(ignore_bytes(&directory), JOURNAL_IGNORE);
        let mode = std::fs::metadata(&directory).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "a journal directory stays private");
        assert_eq!(
            entries(directory.parent().unwrap()),
            ["logs"],
            "nothing is left beside the journal directory"
        );
    }

    #[test]
    fn an_existing_directory_without_the_file_gets_it_and_keeps_its_journal() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("2026-09-24.jsonl"), "{}\n").unwrap();
        std::fs::write(directory.join(".view.lock"), "").unwrap();
        prepare(&directory).unwrap();
        assert_eq!(
            entries(&directory),
            [IGNORE_FILE, ".view.lock", "2026-09-24.jsonl"]
        );
        assert_eq!(ignore_bytes(&directory), JOURNAL_IGNORE);
        assert_eq!(
            std::fs::read_to_string(directory.join("2026-09-24.jsonl")).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn a_changed_truncated_extended_or_deleted_file_is_written_again() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        prepare(&directory).unwrap();
        let mut extended = JOURNAL_IGNORE.to_vec();
        extended.extend_from_slice(b"!keep.jsonl\n");
        let damage: [&dyn Fn(&Path); 4] = [
            &|path| std::fs::write(path, "# edited by hand\n!*.jsonl\n").unwrap(),
            &|path| std::fs::write(path, &JOURNAL_IGNORE[..JOURNAL_IGNORE.len() - 1]).unwrap(),
            &|path| std::fs::write(path, &extended).unwrap(),
            &|path| std::fs::remove_file(path).unwrap(),
        ];
        for damage in damage {
            damage(&directory.join(IGNORE_FILE));
            prepare(&directory).unwrap();
            assert_eq!(ignore_bytes(&directory), JOURNAL_IGNORE);
            assert_eq!(entries(&directory), [IGNORE_FILE]);
        }
    }

    #[test]
    fn a_symlinked_ignore_file_is_replaced_and_its_target_untouched() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let target = root.path().join("elsewhere.txt");
        std::fs::write(&target, "not storyhook's\n").unwrap();
        std::os::unix::fs::symlink(&target, directory.join(IGNORE_FILE)).unwrap();
        prepare(&directory).unwrap();
        let metadata = std::fs::symlink_metadata(directory.join(IGNORE_FILE)).unwrap();
        assert!(metadata.is_file(), "the symlink itself is replaced");
        assert_eq!(ignore_bytes(&directory), JOURNAL_IGNORE);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "not storyhook's\n"
        );
    }

    #[test]
    fn a_fifo_named_like_the_ignore_file_is_replaced_without_blocking() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        std::fs::create_dir_all(&directory).unwrap();
        let fifo = std::ffi::CString::new(
            directory
                .join(IGNORE_FILE)
                .into_os_string()
                .into_encoded_bytes(),
        )
        .unwrap();
        // SAFETY: a valid NUL-terminated path and a plain mode.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        prepare(&directory).unwrap();
        assert_eq!(ignore_bytes(&directory), JOURNAL_IGNORE);
    }

    #[test]
    fn an_ignore_file_that_is_a_directory_fails_loudly() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        std::fs::create_dir_all(directory.join(IGNORE_FILE)).unwrap();
        assert!(prepare(&directory).is_err());
        assert_eq!(
            entries(&directory),
            [IGNORE_FILE],
            "a failed replace leaves no temporary file"
        );
    }

    #[test]
    fn a_journal_path_that_is_a_file_fails_loudly() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        std::fs::create_dir_all(directory.parent().unwrap()).unwrap();
        std::fs::write(&directory, "not a directory").unwrap();
        assert!(prepare(&directory).is_err());
        assert_eq!(
            std::fs::read_to_string(&directory).unwrap(),
            "not a directory"
        );
    }

    #[test]
    fn concurrent_writers_on_an_absent_directory_agree_and_leave_nothing_behind() {
        let root = storyhook_test_support::scratch_dir();
        let directory = logs(root.path());
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..20 {
                        prepare(&directory).unwrap();
                        assert_eq!(ignore_bytes(&directory), JOURNAL_IGNORE);
                    }
                });
            }
        });
        assert_eq!(entries(&directory), [IGNORE_FILE]);
    }

    #[test]
    fn the_rule_ignores_everything_including_itself_and_names_storyhook() {
        let text = std::str::from_utf8(JOURNAL_IGNORE).unwrap();
        assert!(text.is_ascii());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one comment line, then the rule");
        assert!(lines[0].starts_with('#') && lines[0].contains("storyhook"));
        assert_eq!(lines[1], "*");
    }
}
