//! Where a legacy project keeps its files.

use std::path::{Path, PathBuf};

/// The name of the legacy project directory, relative to a checkout root.
pub const LEGACY_DIR: &str = ".storyhook";

/// The layout of one legacy `.storyhook` tree.
///
/// A deliberate second copy of `storage::ProjectPaths`. The two describe the
/// same bytes and must not be unified: `src/storage.rs` is deleted by the flip
/// and this module outlives it, so a shared type would make the reader's
/// lifetime depend on the writer's.
#[derive(Clone, Debug)]
pub struct LegacyPaths {
    root: PathBuf,
}

impl LegacyPaths {
    /// The layout under `root` — the checkout, not the `.storyhook` directory.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    /// The checkout this layout describes.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/.storyhook`.
    #[must_use]
    pub fn storyhook_dir(&self) -> PathBuf {
        self.root.join(LEGACY_DIR)
    }

    /// `project.toml` — schema, creation time, prefix, and the two settings
    /// tables.
    #[must_use]
    pub fn project_file(&self) -> PathBuf {
        self.storyhook_dir().join("project.toml")
    }

    /// `states.toml`.
    #[must_use]
    pub fn states_file(&self) -> PathBuf {
        self.storyhook_dir().join("states.toml")
    }

    /// `types.toml`.
    #[must_use]
    pub fn types_file(&self) -> PathBuf {
        self.storyhook_dir().join("types.toml")
    }

    /// `members.jsonl`.
    #[must_use]
    pub fn members_file(&self) -> PathBuf {
        self.storyhook_dir().join("members.jsonl")
    }

    /// `next-id` — the story-number counter.
    #[must_use]
    pub fn next_id_file(&self) -> PathBuf {
        self.storyhook_dir().join("next-id")
    }

    /// `open/stories`, one `<id>.jsonl` per open story.
    #[must_use]
    pub fn open_stories_dir(&self) -> PathBuf {
        self.storyhook_dir().join("open/stories")
    }

    /// `archive/archive.db`, the closed stories.
    #[must_use]
    pub fn archive_db(&self) -> PathBuf {
        self.storyhook_dir().join("archive/archive.db")
    }

    /// The write-ahead log beside [`archive_db`](Self::archive_db).
    ///
    /// Read only to *refuse*: an archive database with uncommitted WAL content
    /// cannot be read faithfully without writing to the tree, and a reader that
    /// ignores it silently reports a stale archive.
    #[must_use]
    pub fn archive_wal(&self) -> PathBuf {
        self.storyhook_dir().join("archive/archive.db-wal")
    }

    /// Whether a legacy project exists here.
    ///
    /// `project.toml` is the marker, matching what `storage::ensure_project`
    /// looked for — a directory with a `.storyhook` folder and no project file
    /// is not a project, in either implementation.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.project_file().is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_path_hangs_off_the_storyhook_directory() {
        let paths = LegacyPaths::new(Path::new("/checkout"));
        assert_eq!(
            paths.storyhook_dir(),
            Path::new("/checkout/.storyhook"),
            "the legacy directory is a fixed name beside the checkout root"
        );
        for path in [
            paths.project_file(),
            paths.states_file(),
            paths.types_file(),
            paths.members_file(),
            paths.next_id_file(),
            paths.open_stories_dir(),
            paths.archive_db(),
            paths.archive_wal(),
        ] {
            assert!(
                path.starts_with(paths.storyhook_dir()),
                "{} escapes the legacy directory",
                path.display()
            );
        }
    }

    #[test]
    fn a_directory_without_a_project_file_is_not_a_project() {
        let dir = storyhook_test_support::scratch_dir();
        let paths = LegacyPaths::new(dir.path());
        assert!(!paths.exists());
        std::fs::create_dir_all(paths.storyhook_dir()).unwrap();
        assert!(
            !paths.exists(),
            "an empty `.storyhook` directory is not a project — `ensure_project` \
             looked for the project file, and so does this"
        );
        std::fs::write(paths.project_file(), "schema = 1\ncreated_at = \"now\"\n").unwrap();
        assert!(paths.exists());
    }
}
