//! Reaching the global store a fixture's `story` processes are using.
//!
//! Before the flip a test that wanted to look at — or corrupt — a project's
//! data opened files under `<project>/.storyhook/`. After it, the data is in
//! one database outside every repository, and the only reason a test can still
//! find it is that [`TestEnv`] told the child process where to put it.
//!
//! So the path is handed *back* here rather than re-derived from the
//! environment. That is not a stylistic choice: an in-process test cannot
//! redirect `STORYHOOK_DATA_DIR` for itself — the variable is applied to child
//! commands, and the current process still sees the developer's real one — so
//! any helper that read the environment would open the wrong database and, on a
//! write, eat real data. Every function here takes its location from the
//! fixture that owns it.

use std::path::Path;

use storyhook::service::project::{ProjectPointer, read_pointer};
use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store, StoryNo};

use crate::env::TestEnv;
use crate::project::Project;

impl TestEnv {
    /// Opens — and migrates — this environment's store.
    ///
    /// Safe to call while `story` processes are running against it: the store
    /// is WAL-mode SQLite with a busy timeout, which is the same concurrency
    /// the CLI itself relies on.
    pub fn open_store(&self) -> SqliteStore {
        let store = SqliteStore::open(self.store_path()).expect("opening the fixture store");
        store.migrate().expect("migrating the fixture store");
        store
    }
}

impl Project<'_> {
    /// Opens the store this project's commands write to.
    pub fn open_store(&self) -> SqliteStore {
        self.env().open_store()
    }

    /// This project's row in `store`, by the identity its checkout carries.
    ///
    /// The committed pointer file, which is what the CLI reads at the working
    /// directory — so a fixture that stops resolving here is a fixture the CLI
    /// has also stopped resolving *there*. It is deliberately not the whole
    /// resolution order: the ancestor climb and the registered-origin lookup
    /// answer for directories a `Project` fixture does not own, and a helper
    /// that reimplemented them would be a second resolver to keep in step.
    pub fn try_project_id(&self, store: &SqliteStore) -> Option<ProjectId> {
        project_id_at(store, self.path())
    }

    /// This project's row in `store`, or a panic naming the checkout.
    pub fn project_id(&self, store: &SqliteStore) -> ProjectId {
        self.try_project_id(store).unwrap_or_else(|| {
            panic!(
                "no project in the store for the checkout at {}",
                self.path().display()
            )
        })
    }

    /// The committed pointer file, if this checkout has one.
    pub fn pointer(&self) -> Option<ProjectPointer> {
        read_pointer(self.path()).expect("reading the pointer file")
    }

    /// Parses a story id (`SH-1`) into the number the store keys on.
    pub fn story_no(&self, store: &SqliteStore, id: &str) -> StoryNo {
        let project = self.project_id(store);
        let prefix = store
            .read(|tx| Ok(tx.project(project)?.expect("the project exists").prefix))
            .expect("reading the project");
        StoryNo::parse_id(&prefix, id)
            .unwrap_or_else(|e| panic!("`{id}` is not a story id in this project: {e}"))
    }
}

/// The project a checkout at `root` belongs to, by its committed pointer file.
///
/// Free rather than a method so a test can ask the question about a directory
/// that is not a [`Project`] fixture — a linked worktree, or a subdirectory
/// several levels down.
///
/// The directory itself is not a key: SH-119 deleted the recorded-path index,
/// so a checkout that carries no pointer file resolves to nothing here exactly
/// as it resolves to nothing in the CLI.
pub fn project_id_at(store: &SqliteStore, root: &Path) -> Option<ProjectId> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let pointer: ProjectPointer = read_pointer(&canonical).expect("reading a pointer file")?;
    store
        .read(|tx| Ok(tx.project_by_uuid(&pointer.uuid)?.map(|project| project.id)))
        .expect("reading the store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use storyhook::service::{InitOptions, ProjectService};

    #[test]
    fn the_store_path_is_inside_the_environments_own_data_directory() {
        let env = TestEnv::isolated();
        assert_eq!(env.store_path().parent(), Some(env.data_dir()));
        assert!(
            !env.store_path()
                .starts_with(std::env::var("HOME").unwrap_or_default()),
            "a fixture store must never resolve inside the developer's real HOME"
        );
    }

    #[test]
    fn a_fresh_environments_store_opens_migrated_and_empty() {
        let env = TestEnv::isolated();
        let store = env.open_store();
        let projects = store.read(|tx| tx.projects()).expect("listing projects");
        assert!(projects.is_empty());
    }

    #[test]
    fn a_project_is_found_by_the_pointer_file_its_checkout_carries() {
        let env = TestEnv::isolated();
        let store = env.open_store();
        let root = crate::scratch::scratch_dir_named("pid-");

        ProjectService::new(&store, root.path())
            .init(&InitOptions::default())
            .expect("initializing a project");

        assert!(
            project_id_at(&store, root.path()).is_some(),
            "an attaching run leaves the checkout carrying its identity"
        );
    }

    #[test]
    fn a_checkout_answers_for_the_project_its_pointer_names_whatever_directory_it_is() {
        let env = TestEnv::isolated();
        let store = env.open_store();
        let one = crate::scratch::scratch_dir_named("pid-a-");
        let two = crate::scratch::scratch_dir_named("pid-b-");

        let a = ProjectService::new(&store, one.path())
            .init(&InitOptions::default())
            .expect("initializing project a")
            .project;
        let b = ProjectService::new(&store, two.path())
            .init(&InitOptions::default())
            .expect("initializing project b")
            .project;
        assert_ne!(a, b);

        // `two` was created as its own project and then given `a`'s committed
        // identity — which is what a clone, a move, or a restored backup looks
        // like from here. The committed uuid is the answer; the directory is
        // not a key at all since SH-119.
        let uuid = store
            .read(|tx| Ok(tx.project(a)?.expect("project a exists").uuid))
            .expect("reading project a");
        storyhook::service::project::write_pointer(
            two.path(),
            &ProjectPointer::new(uuid, "SH".to_string()),
        )
        .expect("writing a pointer file");

        assert_eq!(project_id_at(&store, two.path()), Some(a));
    }

    #[test]
    fn a_checkout_with_no_pointer_file_resolves_to_nothing() {
        let env = TestEnv::isolated();
        let store = env.open_store();
        let root = crate::scratch::scratch_dir_named("pid-c-");

        ProjectService::new(&store, root.path())
            .init(&InitOptions::default())
            .expect("initializing a project");
        std::fs::remove_file(root.path().join(".storyhook.toml")).expect("removing the pointer");

        assert_eq!(
            project_id_at(&store, root.path()),
            None,
            "the directory it was created at is not an identity"
        );
    }

    #[test]
    fn a_checkout_the_store_has_never_seen_resolves_to_nothing() {
        let env = TestEnv::isolated();
        let store = env.open_store();
        let stranger = crate::scratch::scratch_dir_named("stranger-");
        assert_eq!(project_id_at(&store, stranger.path()), None);
    }
}
