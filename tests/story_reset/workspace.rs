//! A real repository with one story worktree, as dispatch leaves it, for reset tests.
use std::path::{Path, PathBuf};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo, StoryReset, WriteOps};
use storyhook_test_support::ServiceFixture;

/// Runs one Git command that must succeed.
pub(crate) fn git(cwd: &Path, args: &[&str]) -> String {
    let output = storyhook::env::git_env::command(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Commits with signing disabled, so fixtures never prompt.
pub(crate) fn commit(cwd: &Path, message: &str) {
    git(
        cwd,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ],
    );
}

/// One story in progress with a worktree, a local branch and a bare origin.
pub(crate) struct Workspace {
    pub(crate) fixture: ServiceFixture,
    _root: tempfile::TempDir,
    pub(crate) repo: PathBuf,
    pub(crate) id: String,
    pub(crate) worktree: PathBuf,
}

impl Workspace {
    /// The base commit is on origin/main; the story branch `worktree-SH-1` is
    /// checked out in `.codex/worktrees/SH-1` and holds nothing else yet.
    pub(crate) fn new(with_origin: bool) -> Self {
        let fixture = ServiceFixture::new();
        let root = storyhook_test_support::scratch_dir();
        let repo = root.path().join("repo");
        let remote = root.path().join("remote.git");
        std::fs::create_dir(&repo).unwrap();
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                remote.to_str().unwrap(),
            ],
        );
        git(&repo, &["init", "--initial-branch=main"]);
        storyhook_test_support::approve_fixture_identity(
            &repo,
            "Reset fixture",
            "reset@example.test",
        );
        commit(&repo, "base");
        if with_origin {
            git(
                &repo,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
            git(&repo, &["push", "origin", "main"]);
        }
        let repo = repo.canonicalize().unwrap();
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
            .unwrap();
        let id = StoryService::new(&fixture.ctx().no_hooks(true))
            .create(&NewStoryInput {
                title: "Discard local work".into(),
                state: Some("in-progress".into()),
                ..Default::default()
            })
            .unwrap()
            .id;
        let worktree = repo.join(".codex/worktrees").join(&id);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "worktree-SH-1",
                worktree.to_str().unwrap(),
            ],
        );
        Self {
            fixture,
            _root: root,
            repo,
            id,
            worktree,
        }
    }

    /// Reserves the reset and pins the story's resources as they are now.
    pub(crate) fn reserve_pinned(&self) -> StoryReset {
        self.reserve_pinned_with(|_| {})
    }

    /// Reserves the reset and pins resources after `edit` adjusts them, as a
    /// resolver that observed something else would have pinned them.
    pub(crate) fn reserve_pinned_with(
        &self,
        edit: impl FnOnce(&mut storyhook::service::resources::ResourceReport),
    ) -> StoryReset {
        let ctx = self.fixture.ctx().no_hooks(true);
        let reset = StoryResetService::new(&ctx)
            .reserve(&self.id, &self.id)
            .unwrap();
        let mut pinned = reset.clone();
        pinned.resources = Some(
            storyhook::service::resources::ResourceService::new(&ctx)
                .resolve(&self.id, &Default::default())
                .unwrap(),
        );
        pinned.paths = pinned_paths(pinned.resources.as_ref().unwrap());
        edit(pinned.resources.as_mut().unwrap());
        self.fixture
            .store()
            .write(|tx| tx.put_story_reset(&pinned))
            .unwrap();
        pinned
    }

    /// Runs the reset to completion from `cwd`; it must never fail.
    pub(crate) fn execute_from(&self, cwd: &Path, reset: &StoryReset) -> StoryReset {
        let ctx = storyhook::service::Ctx::new(
            self.fixture.store(),
            self.fixture.project(),
            cwd.to_path_buf(),
            self.fixture.env().clone(),
        )
        .no_hooks(true);
        let done = StoryResetService::new(&ctx)
            .execute(&self.id, &reset.token, || Ok(()))
            .expect("a reset must finish");
        assert!(done.completed, "{done:?}");
        done
    }

    /// Runs the reset to completion from the fixture's own directory.
    pub(crate) fn execute(&self, reset: &StoryReset) -> StoryReset {
        self.execute_from(self.fixture.cwd(), reset)
    }

    /// The story row after the reset.
    pub(crate) fn story(&self) -> storyhook::store::StoryRow {
        self.fixture
            .store()
            .read(|tx| tx.story(self.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
    }

    /// Whether `branch` exists as a local branch.
    pub(crate) fn branch_exists(&self, branch: &str) -> bool {
        storyhook::service::resources::git::branch_exists(&self.repo, branch).unwrap()
    }

    /// The last comment on the story.
    pub(crate) fn last_comment(&self) -> String {
        let events = self
            .fixture
            .store()
            .read(|tx| tx.events_for(self.fixture.project(), StoryNo::new(1)))
            .unwrap();
        events
            .iter()
            .rev()
            .find_map(|event| match event.known() {
                Some(storyhook::domain::StoryEvent::StoryCommentAdded { text, .. }) => {
                    Some(text.clone())
                }
                _ => None,
            })
            .unwrap()
    }
}

/// The filesystem identities a reset pins with its resources.
pub(crate) fn pinned_paths(
    report: &storyhook::service::resources::ResourceReport,
) -> Vec<storyhook::store::ResetPathIdentity> {
    use std::os::unix::fs::MetadataExt;
    let repo = report.repository.as_ref().unwrap();
    let common = storyhook::service::resources::git::text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .unwrap();
    let worktree = report.worktree.as_ref().unwrap();
    let private =
        storyhook::service::resources::git::text(worktree, &["rev-parse", "--absolute-git-dir"])
            .unwrap();
    [
        (PathBuf::from(common.trim()), false),
        (worktree.clone(), true),
        (PathBuf::from(private.trim()), true),
    ]
    .into_iter()
    .map(|(path, removable)| {
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        storyhook::store::ResetPathIdentity {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            removable,
        }
    })
    .collect()
}
