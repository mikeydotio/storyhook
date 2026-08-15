//! Invariants of the system service — the commands that write somewhere other
//! than the store.
//!
//! The differential harness compares *responses*, so it sees a scaffold's
//! output and a hook install's summary but is blind to what landed on disk.
//! These tests cover the other half: the file the hook installer wrote, its
//! mode, the user's own hook it refused to touch, and the project identity the
//! templates are rendered with.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use storyhook::domain::{FieldEdit, StateChanges};
use storyhook::error::AppError;
use storyhook::service::{ConfigService, SystemService, templates};
use storyhook_test_support::ServiceFixture;

// --- helpers ---------------------------------------------------------------

/// `git <args>` in `cwd`, asserting it succeeded.
fn git(cwd: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running `git {}`: {e}", args.join(" ")));
    assert!(
        out.status.success(),
        "`git {}` in {} failed: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Turns the fixture's working directory into a **real** repository, and hands
/// back its `.git/hooks` path.
///
/// This used to `mkdir .git` and call it a repository, which worked only
/// because the installer hand-built `<root>/.git/hooks` and never asked git
/// anything. Now that it resolves `--git-common-dir` (SH-314), a directory
/// named `.git` with nothing in it is what git says it is: not a repository.
/// Initialising one for real is the honest fixture, and it is what lets the
/// worktree test below exist at all.
fn git_repo(fixture: &ServiceFixture) -> PathBuf {
    let root = fixture.cwd();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    root.join(".git").join("hooks")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn message(error: AppError) -> String {
    error.to_string()
}

/// The three hooks storyhook manages.
const MANAGED: [&str; 3] = ["post-commit", "post-merge", "prepare-commit-msg"];

// --- scaffolding -----------------------------------------------------------

#[test]
fn the_agents_template_is_rendered_for_this_project() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let rendered = SystemService::new(&ctx)
        .scaffold("agents-md")
        .expect("scaffolding");
    assert_eq!(rendered, templates::agents_md("SH", "done"));
    assert!(rendered.contains("story show SH-<n>"), "{rendered}");
}

#[test]
fn the_agents_template_follows_the_projects_own_closed_state() {
    // The template names the project's *first* CLOSED state, not the literal
    // `done`. This used to be proven by deleting `done`, which the required
    // floor now refuses (SH-125) — so the same property is proven by putting
    // another CLOSED state ahead of it, which is the other way a project
    // decides what "closed" means to it.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .add_state("shipped", storyhook::domain::SuperState::Closed, None, None)
        .expect("adding a closed state");
    ConfigService::new(&ctx)
        .reorder_states(&[
            "todo".to_string(),
            "in-progress".to_string(),
            "blocked".to_string(),
            "shipped".to_string(),
            "done".to_string(),
        ])
        .expect("putting `shipped` ahead of `done`");

    let rendered = SystemService::new(&ctx)
        .scaffold("agents-md")
        .expect("scaffolding");
    assert!(rendered.contains("story move SH-<n> shipped"), "{rendered}");
    assert!(!rendered.contains("story move SH-<n> done"), "{rendered}");
}

#[test]
fn the_fixed_templates_do_not_depend_on_the_project() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = SystemService::new(&ctx);
    assert_eq!(
        service.scaffold("claude-md").expect("scaffolding"),
        templates::claude_md()
    );
    assert_eq!(
        service.scaffold("cursor-rules").expect("scaffolding"),
        templates::cursor_rules()
    );
}

#[test]
fn an_unknown_scaffold_kind_reports_the_ones_that_exist() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let error = SystemService::new(&ctx).scaffold("vim-rules").unwrap_err();
    assert!(matches!(error, AppError::Usage(_)), "{error:?}");
    assert_eq!(
        message(error),
        "usage: story scaffold agents-md|claude-md|cursor-rules"
    );
}

#[test]
fn scaffolding_writes_nothing() {
    // `story scaffold` prints; the caller decides where it goes. A version
    // that wrote the file itself would be a repository write nobody asked for.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    SystemService::new(&ctx)
        .scaffold("agents-md")
        .expect("scaffolding");
    let entries: Vec<_> = std::fs::read_dir(fixture.cwd())
        .expect("listing")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert!(entries.is_empty(), "scaffold left {entries:?} behind");
}

// --- git hooks -------------------------------------------------------------

#[test]
fn installing_writes_three_executable_hooks_carrying_the_marker() {
    let fixture = ServiceFixture::new();
    let hooks_dir = git_repo(&fixture);
    let ctx = fixture.ctx();

    let summary = SystemService::new(&ctx)
        .install_git_hooks()
        .expect("installing")
        .message();
    for name in MANAGED {
        let path = hooks_dir.join(name);
        let content = read(&path);
        assert!(
            content.starts_with("#!/bin/sh\n"),
            "{name} is not a shell script"
        );
        assert!(
            content.contains("# storyhook managed hook -- do not edit this line"),
            "{name} carries no ownership marker, so uninstall could not tell it apart"
        );
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "{name} is not executable");
        assert!(
            summary.contains(&format!("{name} — installed")),
            "{summary}"
        );
    }
}

#[test]
fn a_users_own_hook_is_never_overwritten() {
    let fixture = ServiceFixture::new();
    let hooks_dir = git_repo(&fixture);
    std::fs::create_dir_all(&hooks_dir).expect("creating hooks/");
    let mine = hooks_dir.join("post-commit");
    std::fs::write(&mine, "#!/bin/sh\necho mine\n").expect("seeding a user hook");

    let ctx = fixture.ctx();
    let summary = SystemService::new(&ctx)
        .install_git_hooks()
        .expect("installing")
        .message();
    assert_eq!(read(&mine), "#!/bin/sh\necho mine\n");
    assert!(
        summary.contains("post-commit — skipped (existing user hook)"),
        "{summary}"
    );
    assert!(summary.contains("post-merge — installed"), "{summary}");
}

#[test]
fn installing_is_idempotent() {
    let fixture = ServiceFixture::new();
    let hooks_dir = git_repo(&fixture);
    let ctx = fixture.ctx();
    let service = SystemService::new(&ctx);
    let first = service
        .install_git_hooks()
        .expect("first install")
        .message();
    let content = read(&hooks_dir.join("post-commit"));
    let second = service
        .install_git_hooks()
        .expect("second install")
        .message();
    assert_eq!(first, second);
    assert_eq!(read(&hooks_dir.join("post-commit")), content);
}

#[test]
fn uninstalling_removes_only_the_hooks_storyhook_wrote() {
    let fixture = ServiceFixture::new();
    let hooks_dir = git_repo(&fixture);
    std::fs::create_dir_all(&hooks_dir).expect("creating hooks/");
    let mine = hooks_dir.join("post-merge");
    std::fs::write(&mine, "#!/bin/sh\necho mine\n").expect("seeding a user hook");

    let ctx = fixture.ctx();
    let service = SystemService::new(&ctx);
    service.install_git_hooks().expect("installing");
    let summary = service
        .uninstall_git_hooks()
        .expect("uninstalling")
        .message();

    assert!(!hooks_dir.join("post-commit").exists());
    assert!(!hooks_dir.join("prepare-commit-msg").exists());
    assert_eq!(read(&mine), "#!/bin/sh\necho mine\n");
    assert!(
        summary.contains("post-merge — skipped (not a storyhook hook)"),
        "{summary}"
    );
}

#[test]
fn uninstalling_reports_hooks_that_were_never_there() {
    let fixture = ServiceFixture::new();
    git_repo(&fixture);
    let ctx = fixture.ctx();
    let summary = SystemService::new(&ctx)
        .uninstall_git_hooks()
        .expect("uninstalling")
        .message();
    for name in MANAGED {
        assert!(
            summary.contains(&format!("{name} — not present")),
            "{summary}"
        );
    }
}

#[test]
fn installing_outside_a_git_repository_is_rejected() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let error = SystemService::new(&ctx).install_git_hooks().unwrap_err();
    assert!(matches!(error, AppError::Validation(_)), "{error:?}");
    assert!(message(error).contains("not a git repository"));
}

/// SH-314. In a linked worktree `.git` is a **file** holding a `gitdir:`
/// pointer, so the old `<root>/.git/hooks` could not be created at all and the
/// install failed with the filesystem's own error.
///
/// This test replaces one that seeded a *fabricated* `.git` file pointing at
/// `/elsewhere` and asserted only that some error came back. Under git-based
/// resolution that fixture is simply "not a git repository", so the old test
/// would have stayed green whether this case were fixed or catastrophically
/// broken — a pass for a reason unrelated to its own name, the shape SH-258 and
/// SH-295 both record. A real `git worktree add` is the only fixture that can
/// tell the difference.
///
/// The assertion that matters is *where* it installs. The obvious repair —
/// write into the worktree's private git directory — would compile, pass a
/// weaker test, and install hooks git never runs, because git resolves hooks
/// for every worktree in the **common** directory.
#[test]
fn installing_from_a_linked_worktree_writes_to_the_shared_common_directory() {
    let fixture = ServiceFixture::new();
    let root = fixture.cwd();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@t"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::write(root.join("f"), "a\n").expect("seeding a tracked file");
    git(root, &["add", "f"]);
    git(root, &["commit", "-qm", "init"]);

    let worktree = root.join("wt");
    git(root, &["worktree", "add", "-q", "wt", "-b", "probe"]);
    assert!(
        worktree.join(".git").is_file(),
        "fixture: a linked worktree's .git must be a file, or this proves nothing"
    );

    // The free function, not the service method: this asks about a *directory*
    // rather than about a project, which is exactly why `system.rs` exposes the
    // project-less half as free functions.
    let summary = storyhook::service::system::install_git_hooks(&worktree)
        .expect("a linked worktree installs into the common directory")
        .message();

    // Where git actually looks, asked of git rather than assumed.
    let common = PathBuf::from(git(&worktree, &["rev-parse", "--git-common-dir"]));
    let common = worktree.join(common).join("hooks");
    for name in MANAGED {
        assert!(
            common.join(name).is_file(),
            "{name} is not in the common hooks directory ({}), so git will never run it",
            common.display()
        );
        assert!(
            summary.contains(&format!("{name} — installed")),
            "{summary}"
        );
    }

    // The private git directory is not a hook directory. Writing there would
    // pass a test that only asked "did a file appear somewhere".
    let private = PathBuf::from(git(&worktree, &["rev-parse", "--git-dir"]));
    let private = worktree.join(private);
    assert_ne!(
        private, common,
        "fixture: the worktree has no private gitdir"
    );
    assert!(
        !private.join("hooks").join("post-commit").is_file(),
        "the installer wrote into the worktree's private git directory, where \
         git does not look for hooks"
    );
    assert!(
        worktree.join(".git").is_file(),
        "the installer replaced the worktree pointer"
    );
}

// --- event hooks -----------------------------------------------------------

#[test]
fn listing_event_hooks_without_a_config_says_so() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    assert!(
        SystemService::new(&ctx)
            .list_event_hooks()
            .contains("no hooks configured")
    );
}

#[test]
fn a_configured_event_hook_is_listed() {
    let fixture = ServiceFixture::new();
    fixture.write_hooks_toml("[on_create]\ncommand = \"echo made\"\n");
    let ctx = fixture.ctx();
    let listed = SystemService::new(&ctx).list_event_hooks();
    assert!(listed.contains("on_create"), "{listed}");
    assert!(listed.contains("echo made"), "{listed}");
}

#[test]
fn testing_an_unknown_event_type_names_the_ones_that_exist() {
    let fixture = ServiceFixture::new();
    fixture.write_hooks_toml("[on_create]\ncommand = \"true\"\n");
    let ctx = fixture.ctx();
    let error = SystemService::new(&ctx)
        .test_event_hook("on_explode")
        .unwrap_err();
    let message = message(error);
    assert!(
        message.contains("unknown event type `on_explode`"),
        "{message}"
    );
    assert!(message.contains("state_change"), "{message}");
}

#[test]
fn testing_an_event_hook_without_a_config_is_not_found() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let error = SystemService::new(&ctx)
        .test_event_hook("create")
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

// --- plugin ----------------------------------------------------------------

#[test]
fn an_unknown_plugin_target_is_rejected_before_anything_is_touched() {
    // Only the rejection path is exercised: installing for real registers a
    // marketplace in the user's own `~/.claude`, which a test has no business
    // doing.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = SystemService::new(&ctx);
    for target in ["vscode", "emacs", ""] {
        let error = service.install_plugin(target).unwrap_err();
        assert!(matches!(error, AppError::Usage(_)), "{error:?}");
        assert!(message(error).contains("Supported: claude-code"));

        let error = service.uninstall_plugin(target).unwrap_err();
        assert!(matches!(error, AppError::Usage(_)), "{error:?}");
    }
}

// --- the system service touches no story -----------------------------------

#[test]
fn nothing_in_this_family_writes_to_the_tracker() {
    let fixture = ServiceFixture::new();
    git_repo(&fixture);
    let ctx = fixture.ctx();
    let service = SystemService::new(&ctx);
    service.scaffold("agents-md").expect("scaffolding");
    service.install_git_hooks().expect("installing");
    service.list_event_hooks();
    service.uninstall_git_hooks().expect("uninstalling");

    // A state edit is the cheapest observable proof the project is untouched:
    // it round-trips through the same tables a stray write would have moved.
    ConfigService::new(&ctx)
        .update_state(
            "todo",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Keep,
                description: FieldEdit::Set("still here".into()),
            },
            None,
        )
        .expect("editing a state");
    fixture.assert_no_drift();
}
