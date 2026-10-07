//! The hygiene sweep against real git repositories (SH-771).
//!
//! Every git call here runs with no system or global configuration and with
//! `core.excludesFile=/dev/null`, so only the journal's own ignore file can
//! hide the journal: a developer's global excludes cannot make a test pass.

use super::*;
use crate::store::{NewProject, SqliteStore, WriteOps};

/// Half the sweep interval: a poll that waited one interval before its
/// first sweep cannot meet it, and a first sweep on a loaded machine still
/// has thirty seconds.
const FIRST_SWEEP_PATIENCE: Duration = Duration::from_secs(SWEEP_INTERVAL.as_secs() / 2);

/// A store whose registered checkouts live under one scratch root.
///
/// Every sweep waits for a real `git ls-files` to answer, so the registry's
/// environment declares patience (SH-836); the `poll` thread a test starts
/// with it applies the same declaration.
struct Registry {
    root: tempfile::TempDir,
    env: Environment,
    store: SqliteStore,
}

impl Registry {
    fn new() -> Self {
        let root = tempfile::tempdir_in("/private/tmp").unwrap();
        let env = Environment::at(root.path().join("home")).with_subprocess_patience();
        std::fs::create_dir_all(env.store_path().parent().unwrap()).unwrap();
        let store = SqliteStore::open(env.store_path()).unwrap();
        store.migrate().unwrap();
        // Production claims its pidfile before starting the hygiene worker.
        std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
        Self { root, env, store }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.path().join(relative)
    }

    fn register(&self, slug: &str, checkout: &Path) -> ProjectId {
        self.store
            .write(|tx| {
                let project = tx.create_project(&NewProject {
                    uuid: format!("uuid-{slug}"),
                    slug: slug.into(),
                    name: slug.into(),
                    prefix: slug.to_uppercase(),
                    created_at: "2026-09-26T00:00:00Z".into(),
                })?;
                tx.set_checkout_path(project, Some(checkout))?;
                Ok(project)
            })
            .unwrap()
    }

    fn sweep(&self) -> Vec<TrackedJournal> {
        sweep(&self.store, &self.env).unwrap()
    }
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = crate::env::git_env::command(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "user.name=SH-771 fixture",
            "-c",
            "user.email=sh-771@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn status(repo: &Path) -> String {
    git(repo, &["status", "--porcelain", "--untracked-files=all"])
}

/// A repository with one commit, so worktrees and index comparisons have a
/// HEAD to compare.
fn repository(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "-q"]);
    std::fs::write(path.join("README.md"), "fixture\n").unwrap();
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-qm", "fixture"]);
    path.to_path_buf()
}

/// One record into `checkout`'s project journal, as the daemon writes it.
fn journal(checkout: &Path, message: &str) {
    super::super::Journal::new(super::super::project_journal(checkout))
        .append(chrono::Utc::now(), "INFO", "fixture", "event", "", message)
        .unwrap();
}

/// A journal directory as every checkout journaled before SH-771 has one:
/// a day file and a reader lock, and no ignore file.
fn unprotected_journal(checkout: &Path) -> PathBuf {
    let directory = super::super::project_journal(checkout);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("2026-09-24.jsonl"), "{}\n").unwrap();
    std::fs::write(directory.join(".view.lock"), "").unwrap();
    directory
}

fn ignore_file(checkout: &Path) -> PathBuf {
    super::super::project_journal(checkout).join(super::super::IGNORE_FILE)
}

#[test]
fn a_new_journal_never_appears_in_git_status() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    journal(&repo, "first");
    assert_eq!(status(&repo), "");
    assert_eq!(registry.sweep(), []);
    assert_eq!(status(&repo), "");
}

#[test]
fn a_journal_from_before_sh771_is_fixed_by_the_sweep_with_no_user_action() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    unprotected_journal(&repo);
    assert_eq!(
        status(&repo),
        "?? .storyhook/logs/.view.lock\n?? .storyhook/logs/2026-09-24.jsonl\n",
        "the defect, reproduced with global excludes disabled"
    );
    assert_eq!(registry.sweep(), []);
    assert_eq!(status(&repo), "");
    assert_eq!(
        std::fs::read(ignore_file(&repo)).unwrap(),
        super::super::JOURNAL_IGNORE
    );
}

#[test]
fn a_changed_or_deleted_ignore_file_is_written_again_by_the_sweep_and_by_a_record() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    journal(&repo, "first");

    std::fs::write(ignore_file(&repo), "# edited by hand\n").unwrap();
    assert_ne!(
        status(&repo),
        "",
        "a changed ignore file exposes the journal"
    );
    registry.sweep();
    assert_eq!(status(&repo), "");

    std::fs::remove_file(ignore_file(&repo)).unwrap();
    assert_ne!(
        status(&repo),
        "",
        "a deleted ignore file exposes the journal"
    );
    registry.sweep();
    assert_eq!(status(&repo), "");

    std::fs::remove_file(ignore_file(&repo)).unwrap();
    journal(&repo, "second");
    assert_eq!(status(&repo), "", "the next record puts it back too");
}

#[test]
fn a_repository_that_keeps_storyhook_trackable_ignores_only_the_journal() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    std::fs::create_dir_all(repo.join(".storyhook")).unwrap();
    // Every way a repository's own rules could try to re-include the journal.
    std::fs::write(
        repo.join(".gitignore"),
        "!.storyhook/\n!.storyhook/logs/\n!.storyhook/logs/*\n!*.jsonl\n",
    )
    .unwrap();
    std::fs::write(repo.join(".storyhook/settings.toml"), "kept = true\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "keep .storyhook trackable"]);
    registry.register("repo", &repo);
    journal(&repo, "first");
    registry.sweep();
    assert_eq!(status(&repo), "");
    std::fs::write(repo.join(".storyhook/notes.md"), "new\n").unwrap();
    assert_eq!(
        status(&repo),
        "?? .storyhook/notes.md\n",
        "other .storyhook/ content stays trackable"
    );
}

#[test]
fn a_registered_linked_worktree_ignores_its_journal_in_both_trees() {
    let registry = Registry::new();
    let main = repository(&registry.path("main"));
    let linked = registry.path("linked");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            linked.to_str().unwrap(),
        ],
    );
    registry.register("linked", &linked);
    unprotected_journal(&linked);
    assert_ne!(status(&linked), "");
    assert_eq!(registry.sweep(), []);
    journal(&linked, "first");
    assert_eq!(status(&linked), "");
    assert_eq!(status(&main), "");
}

#[test]
fn committed_journal_files_are_reported_and_the_index_is_never_touched() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    let project = registry.register("repo", &repo);
    let other = registry.register("other", &repository(&registry.path("other")));
    unprotected_journal(&repo);
    git(&repo, &["add", "-f", ".storyhook/logs/2026-09-24.jsonl"]);
    git(&repo, &["commit", "-qm", "an accidental journal commit"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let index = git(&repo, &["ls-files", "-s"]);

    let found = registry.sweep();
    assert_eq!(
        found,
        [TrackedJournal {
            project_id: project,
            project: "repo".into(),
            checkout: repo.clone(),
            files: 1,
            more: false,
        }]
    );
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]),
        head,
        "nothing committed"
    );
    assert_eq!(git(&repo, &["ls-files", "-s"]), index, "index untouched");
    let warning = warning_for(&registry.env, project).expect("a warning for the project");
    assert!(
        warning.contains("git rm -r --cached .storyhook/logs"),
        "{warning}"
    );
    assert!(warning.contains("1 activity journal file in "), "{warning}");
    assert!(warning.contains(&repo.display().to_string()), "{warning}");
    assert_eq!(warning_for(&registry.env, other), None);
    assert_eq!(warnings(&registry.env), [warning]);

    git(&repo, &["rm", "-r", "-q", "--cached", ".storyhook/logs"]);
    git(&repo, &["commit", "-qm", "untrack the journal"]);
    assert_eq!(registry.sweep(), []);
    assert_eq!(warning_for(&registry.env, project), None);
    assert!(warnings(&registry.env).is_empty());
    assert_eq!(status(&repo), "", "the files stay on disk, ignored");
}

#[test]
fn a_committed_foreign_ignore_file_is_rewritten_and_reported() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    let directory = super::super::project_journal(&repo);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(ignore_file(&repo), "*.jsonl\n").unwrap();
    git(&repo, &["add", "-f", ".storyhook/logs/.gitignore"]);
    git(
        &repo,
        &["commit", "-qm", "a hand-written journal ignore file"],
    );
    let found = registry.sweep();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].files, 1);
    assert_eq!(
        std::fs::read(ignore_file(&repo)).unwrap(),
        super::super::JOURNAL_IGNORE
    );
    assert_eq!(status(&repo), " M .storyhook/logs/.gitignore\n");
}

#[test]
fn a_checkout_that_is_not_a_repository_is_prepared_and_journals_nothing() {
    let registry = Registry::new();
    let plain = registry.path("plain");
    registry.register("plain", &plain);
    let directory = unprotected_journal(&plain);
    assert_eq!(registry.sweep(), []);
    assert_eq!(
        std::fs::read(ignore_file(&plain)).unwrap(),
        super::super::JOURNAL_IGNORE
    );
    assert_eq!(
        super::super::day_files(&directory).unwrap(),
        [directory.join("2026-09-24.jsonl")],
        "no failure was journaled"
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("2026-09-24.jsonl")).unwrap(),
        "{}\n"
    );
}

#[test]
fn a_checkout_without_a_journal_is_left_alone() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    registry.register("relative", Path::new("relative/checkout"));
    registry.register("missing", &registry.path("missing"));
    assert_eq!(registry.sweep(), []);
    assert!(
        !repo.join(".storyhook").exists(),
        "a sweep creates no journal"
    );
    assert!(!registry.path("missing").exists());
    assert_eq!(status(&repo), "");
}

#[test]
fn reset_removes_a_previous_daemons_findings() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    unprotected_journal(&repo);
    git(&repo, &["add", "-f", ".storyhook/logs"]);
    git(&repo, &["commit", "-qm", "tracked"]);
    assert_eq!(registry.sweep().len(), 1);
    assert_eq!(warnings(&registry.env).len(), 1);
    reset(&registry.env).unwrap();
    assert!(!registry.env.journal_hygiene_file().exists());
    assert!(warnings(&registry.env).is_empty());
    reset(&registry.env).unwrap();
}

#[test]
fn findings_publication_does_not_recreate_a_detached_home() {
    let registry = Registry::new();
    assert!(registry.sweep().is_empty());
    assert!(registry.env.journal_hygiene_file().is_file());
    let journal = super::super::Journal::anchored(
        registry.env.daemon_state_dir().join("activity"),
        registry.env.daemon_state_dir(),
    );

    // Keep the store connection open, just as a sweep still finishing after
    // the daemon's HOME was detached. Never clean the original path again:
    // that would erase the recreation this regression must detect.
    std::fs::rename(registry.env.home(), registry.path("retired-home")).unwrap();
    assert!(publish(&registry.env, &Findings::default()).is_err());
    assert!(!registry.env.home().exists(), "findings recreated HOME");
    assert!(
        journal
            .append(
                chrono::Utc::now(),
                "INFO",
                "daemon",
                "event",
                "",
                "daemon stopped"
            )
            .is_err(),
        "failed findings publication must not revive the journal's anchor"
    );
    assert!(
        !registry.env.home().exists(),
        "shutdown logging recreated HOME"
    );
}

#[test]
fn findings_publication_requires_the_existing_daemon_state_directory() {
    let registry = Registry::new();
    assert!(registry.sweep().is_empty());
    let directory = registry.env.daemon_state_dir();
    std::fs::rename(&directory, registry.path("retired-daemon-state")).unwrap();

    assert!(
        registry.env.home().is_dir(),
        "only the daemon anchor was removed"
    );
    assert!(publish(&registry.env, &Findings::default()).is_err());
    assert!(
        !directory.exists(),
        "findings recreated their removed anchor"
    );
    assert!(!registry.env.journal_hygiene_file().exists());
}

#[test]
fn unreadable_findings_are_reported_not_hidden() {
    let registry = Registry::new();
    let path = registry.env.journal_hygiene_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "not json").unwrap();
    let reported = warnings(&registry.env);
    assert_eq!(reported.len(), 1);
    assert!(reported[0].contains("unreadable"), "{reported:?}");
    assert!(reported[0].contains(&path.display().to_string()));
    assert_eq!(
        warning_for(&registry.env, ProjectId::new(1)),
        Some(reported[0].clone())
    );
}

/// Stops a poll however the test body ends. Without it a failed assertion
/// unwinds into the thread scope, which waits forever on a poller that
/// sweeps every minute.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[test]
fn the_poll_sweeps_at_start_and_stops_when_asked() {
    let registry = Registry::new();
    let repo = repository(&registry.path("repo"));
    registry.register("repo", &repo);
    unprotected_journal(&repo);
    let stop = AtomicBool::new(false);
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            poll(&registry.store, &registry.env, &stop);
            done.send(()).unwrap();
        });
        let stopping = StopOnDrop(&stop);
        let deadline = Instant::now() + FIRST_SWEEP_PATIENCE;
        while !registry.env.journal_hygiene_file().exists() {
            assert!(
                Instant::now() < deadline,
                "no sweep at start within {FIRST_SWEEP_PATIENCE:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(status(&repo), "");
        drop(stopping);
        finished
            .recv_timeout(FIRST_SWEEP_PATIENCE)
            .expect("the poll stops when asked");
    });
}
