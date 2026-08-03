//! Bringing a project into existence.
//!
//! `story project new` used to be a sequence of independent filesystem writes: make
//! the directories, write `project.toml`, write `states.toml`, write
//! `types.toml`, write `members.jsonl`, write the id counter, open the archive
//! database. Any failure between two of them left a project that existed
//! enough to be found and not enough to be used — a half-initialised tracker
//! whose repair story was "delete the directory and start again".
//!
//! Here the project row, its default states, its default types and its id
//! counter are one transaction: either the project exists completely or it was
//! never created.
//!
//! # What still touches the repository
//!
//! Two artifacts, both of them content a user asked for by running `init`, and
//! neither written by any other command:
//!
//! * `AGENTS.md`, generated for agent discoverability when the repository has
//!   none — the same decision the legacy path made, replicated here.
//! * the pointer file, the repository's copy of *which* project this checkout
//!   belongs to. Every attaching run writes it, or adopts the one already
//!   there, because since SH-119 it is the only thing that identifies a
//!   checkout: the store holds no index of directories to look one up in.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{StateDef, SuperState, TypeDef};
use crate::error::AppError;
use crate::output::DeletePlan;
use crate::store::{
    DeletedProject, NewProject, ProjectId, ProjectRecord, ReadOps, Store, WriteOps,
};

use super::Clock;
use super::state_set::write_states;
use super::templates;

/// The story-id prefix a project gets when `init` is not told one.
pub const DEFAULT_PREFIX: &str = "SH";

/// Set to a non-empty value to permit creating a project under a temporary
/// directory in a store that is not itself temporary. See
/// [`refuse_temp_project_in_real_store`].
pub const ALLOW_TEMP_PROJECT: &str = "STORYHOOK_ALLOW_TEMP_PROJECT";

/// Refuses to create a project at a throwaway path inside a store that is not
/// throwaway (SH-95).
///
/// The invariant is one sentence: **a throwaway project may only be created in
/// a throwaway store.**
///
/// Before the flip, `story project new` wrote a `.storyhook/` directory into the
/// directory it was run in, so a fixture's data lived in the fixture and was
/// deleted with it. Storage isolated itself, and no test suite driving the CLI
/// ever had to think about it — so none of them did. One global store ended
/// that silently: every one of those fixture sites became a permanent write
/// into the developer's real tracker, with no error and nothing to notice. It
/// went unnoticed until a single run of one repository's suite put 394 projects
/// into a real store, 234 of them carrying stories.
///
/// **Why both halves are required.** Refusing every temporary path would be
/// wrong and would break this project's own suite, whose fixtures legitimately
/// live under `/private/tmp` — with a store that lives there too. A temporary
/// project in a temporary store is exactly what a test *should* build. The
/// defect is the mismatch, so the mismatch is what is refused.
///
/// **Why it fails here rather than being cleaned up later.** Hiding these
/// afterwards — in `doctor`, or in the dashboard — treats a symptom while the
/// writes continue. This is the only point where the mistake is still cheap:
/// nothing has been written, and the message can name the one environment
/// variable that fixes the caller for good.
///
/// **Why this still exists after the store-isolation epic.** SH-113 removed
/// this guard's *original* premise — projects were once created only by
/// standing in a directory, and after it, path-based creation is opt-in. It did
/// not remove path-based creation itself: `--attach` still defaults to the
/// client's cwd, and `import-project` and a non-dry-run `migrate` still create
/// at a path (`project_creation_target`, `src/invoke.rs`). So `is_under_temp`
/// still has a live input, and this is still the only thing that fires on the
/// caller SH-122 names as the residual gap — a foreign suite that never opts
/// into `--store-path` at all. Retiring this guard on the strength of SH-113's
/// note would remove the one detection that exists for that caller while its
/// premise still stands.
///
/// **A "state the fact, not the guess" version of the store half was tried and
/// reverted** (SH-95): `store.is_default()` in place of `is_under_temp(store_path)`
/// looks like an improvement — "is this the store every other command reaches
/// for", rather than a path guess. It is not usable here for the same reason
/// [`crate::env`]'s test-build refusal keeps its own path-based check:
/// `is_default()` is relative to whatever `HOME` the process has, and this
/// project's own primary test harness (`TestEnv`, in `storyhook-test-support`)
/// deliberately builds a fake `HOME` and points every store-naming variable at
/// *that* home's own default-shaped subdirectory, so a fixture's layout mirrors
/// a real one while staying disposable. That makes `is_default()` true for
/// essentially every correctly isolated fixture in the suite — indistinguishable
/// by path alone from the real hazard, because both are answered from the same
/// two inputs. Switching the predicate refused project creation in every test
/// built through `TestEnv`, caught by running the suite rather than by review.
pub fn refuse_temp_project_in_real_store(root: &Path, store_path: &Path) -> Result<(), AppError> {
    let allowed = std::env::var(ALLOW_TEMP_PROJECT).is_ok_and(|v| !v.trim().is_empty());
    refuse_temp_project(root, store_path, allowed)
}

/// The decision behind [`refuse_temp_project_in_real_store`], with the
/// environment lifted out into `allowed`.
///
/// Separated so it can be tested without mutating process environment, which
/// two tests running in parallel cannot do safely.
fn refuse_temp_project(root: &Path, store_path: &Path, allowed: bool) -> Result<(), AppError> {
    if allowed || !is_under_temp(root) || is_under_temp(store_path) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "refusing to create a project at `{}`: it is under a temporary directory, and the store \
         at `{}` is not.\n\nNothing has been written. A project created here would outlive the \
         directory it names by a long way — the directory is deleted at the end of the run, and \
         the project stays in the store forever, pointing at nothing.\n\nIf this is a test \
         suite, give it a store of its own: `story store new <path>`, then run with \
         `--store-path <path>` (or its variable, $STORYHOOK_STORE_PATH — either outranks \
         $STORYHOOK_DATA_DIR, which also works but loses if more than one is set). If you \
         really do want a throwaway project in this store, set {ALLOW_TEMP_PROJECT}=1.",
        root.display(),
        store_path.display(),
    )))
}

/// Whether `path` is inside a directory the operating system may reclaim.
///
/// `$TMPDIR` is consulted first because that is where `mktemp` and
/// `tempfile` land by default, and the literals cover the cases it does not:
/// `/tmp` and `/private/tmp` (the same directory on macOS, reached by either
/// name), and `/var/folders`, which is `$TMPDIR`'s real home for a login
/// session other than this process's.
///
/// Every root is compared in both its literal and its canonical spelling, and
/// so is the path — four comparisons rather than one, for a reason the tests
/// pin. `canonical` falls back to its input for a path that does not exist,
/// and the path being judged here usually *does not exist yet*: it is about to
/// be created. So `/tmp/fixture` stays `/tmp/fixture` while the root `/tmp`
/// canonicalizes to `/private/tmp`, and comparing only canonical forms lets
/// exactly the case this guard exists for walk straight through.
pub(crate) fn is_under_temp(path: &Path) -> bool {
    let literal = path.to_path_buf();
    let resolved = canonical(path);
    let mut roots = vec![std::env::temp_dir()];
    roots.extend(
        [
            "/tmp",
            "/private/tmp",
            "/var/folders",
            "/private/var/folders",
        ]
        .into_iter()
        .map(PathBuf::from),
    );
    roots.iter().any(|root| {
        let root_resolved = canonical(root);
        literal.starts_with(root)
            || literal.starts_with(&root_resolved)
            || resolved.starts_with(root)
            || resolved.starts_with(&root_resolved)
    })
}

/// The repository-side file naming the project a checkout belongs to, and
/// carrying the repository's own storyhook configuration.
///
/// This is the *whole* repo footprint of the new design: one small committed
/// file. It holds two different kinds of thing, and the distinction is what
/// keeps the design honest:
///
/// * **Identity** — [`schema`](Self::schema), [`uuid`](Self::uuid) and
///   [`prefix`](Self::prefix), written once by `story project new` so that a fresh
///   clone on another machine knows which project it is looking at before it
///   has any local database row to consult.
/// * **Configuration** — the optional [`plugin`](Self::plugin) and
///   [`hooks`](Self::hooks) tables, which are *user-authored* and which
///   storyhook reads and never writes. They used to be
///   `.storyhook/plugin-config.toml` and `.storyhook/hooks.toml`; they belong
///   in the repository because they are decisions about *this* repository, not
///   data about its stories, and folding them into the pointer means the
///   directory can die without taking a shipped feature with it.
///
/// It deliberately does not carry states, types, members or stories. Those
/// live in the store; a repository that carried its own copy would be a second
/// source of truth, which is the thing this whole rearchitecture exists to
/// delete.
///
/// # Why the two config tables are untyped here
///
/// They are [`toml::Value`], not typed structs, for two reasons that both
/// matter. First, **resolution must not depend on configuration**: a typo in a
/// hook definition would otherwise fail the whole parse and make the repository
/// unresolvable — storyhook would stop knowing which project it was standing
/// in because a timeout was misspelled. Second, storyhook round-trips this file
/// only in the sense of never rewriting it; keeping the tables opaque means a
/// field a newer storyhook understands cannot be silently dropped by an older
/// one that reads the pointer for its identity alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectPointer {
    /// Format version, so a later shape change is detectable rather than
    /// silently misread.
    pub schema: u32,
    /// The project's portable identity — [`crate::store::ProjectRecord::uuid`].
    pub uuid: String,
    /// The project's story-id prefix, duplicated here so a clone can render
    /// ids before it can reach the store.
    pub prefix: String,
    /// The `[plugin]` table, if the repository has one. User-authored;
    /// storyhook never writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<toml::Value>,
    /// The `[hooks]` table, if the repository has one. User-authored;
    /// storyhook never writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<toml::Value>,
}

impl ProjectPointer {
    /// A pointer carrying identity and no configuration — what `story project new`
    /// writes.
    #[must_use]
    pub fn new(uuid: String, prefix: String) -> Self {
        Self {
            schema: POINTER_SCHEMA,
            uuid,
            prefix,
            plugin: None,
            hooks: None,
        }
    }
}

/// The nearest ancestor of `root` (itself included) holding a legacy
/// `.storyhook/` project, if there is one.
///
/// A *project*, not merely a directory: `<dir>/.storyhook/project.toml` is what
/// `storage::ensure_project` looked for, and a repository that has only a
/// `.storyhook/hooks.toml` — configuration in its old home, which is still read
/// — has not been left behind by anything and must not be reported as
/// unmigrated.
#[must_use]
pub fn legacy_project_at(root: &Path) -> Option<PathBuf> {
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    canonical
        .ancestors()
        .find(|dir| dir.join(".storyhook/project.toml").is_file())
        .map(Path::to_path_buf)
}

/// What to tell someone standing in a repository storyhook has not imported.
///
/// Loud and specific, because the alternative is worse in both directions: a
/// bare "not initialized" invites `story project new`, which would mint an *empty*
/// second project beside data the user still has, and a silent fallback to
/// reading the directory is the thing this whole rearchitecture exists to
/// stop.
#[must_use]
pub fn unmigrated_error(tree: &Path) -> AppError {
    AppError::NotFound(format!(
        "`{}` still keeps its stories in a `.storyhook/` directory, and storyhook now \
         reads a single store outside your repositories. Run `story migrate` there to \
         bring them across — it never writes to the directory it reads, so your data \
         stays where it is until you are satisfied. `story migrate --dry-run` shows what \
         it would import.",
        tree.display()
    ))
}

/// This directory's `origin`, reduced to the key project identity is decided by.
///
/// # Why `git config --get`, and not `git remote get-url`
///
/// The two are not the same string. `get-url` applies the caller's
/// `url.<base>.insteadOf` rewrites; `config --get` returns what the repository
/// actually recorded. Those rewrites are **machine-local** — the author's own
/// machine carries a global `url.https://github.com/.insteadOf =
/// git@github.com:`, so the two already disagree there with no `-c` flag — and an
/// identity that moves with them is not an identity: two clones of one
/// repository would key differently on two machines whose users push over
/// different protocols.
///
/// [`RemoteUrl::normalize`](crate::domain::remote::RemoteUrl::normalize) folds
/// scheme and userinfo away, so a scheme-only rewrite happens to survive it. One
/// touching host or path prefix would not, and the resulting failure is silent:
/// the checkout simply stops resolving, and the refusal tells the user to
/// register an origin they already registered.
///
/// Reading `.git/config` directly was rejected for the opposite reason — it
/// re-implements git's own config resolution and misses `include.path`,
/// conditional includes, and worktree indirection. `git` is asked, and asked the
/// question whose answer does not move.
///
/// # Every failure is the same failure
///
/// Not a git repository, no `origin`, no `git` on `PATH`, a url that does not
/// normalize, an origin registered to nobody — all of them answer `None`. A
/// resolution path must not distinguish them, which is what
/// [`normalize_for_lookup`](crate::domain::remote::RemoteUrl::normalize_for_lookup)'s
/// own contract says; the refusal that follows names the origin it found, and
/// that is the whole of what a reader needs.
#[must_use]
pub fn origin_of(cwd: &Path) -> Option<crate::domain::remote::RemoteUrl> {
    let raw = git_output(cwd, &["config", "--get", "remote.origin.url"])?;
    crate::domain::remote::RemoteUrl::normalize_for_lookup(&raw)
}

/// This directory's origin **and its right to claim it** (SH-151).
///
/// [`origin_of`] answers "what origin is reported here?", which every directory
/// in a repository answers identically. This answers the question registration
/// actually has: *may this directory register it?* — and the two are different
/// because `git config --get remote.origin.url` walks up. Before this existed,
/// `story project new` in `monorepo/service-a` registered the monorepo's
/// identity against `service-a`, permanently, and locked out every sibling
/// including the repository's own top level.
///
/// # The predicate, and the two shapes that decide it
///
/// One `git rev-parse --show-toplevel --git-dir --git-common-dir` supplies both
/// clauses:
///
/// 1. **`cwd` is its repository's top level.** A plain subdirectory reports the
///    enclosing origin and owns nothing.
/// 2. **`--git-dir` is `--git-common-dir`.** This is "cwd is the *main* working
///    tree". A linked worktree holds a `.git` **file** and shares the main
///    checkout's config, so the remote recorded there is not its to claim —
///    wherever the worktree sits, inside the checkout or outside it. A
///    **submodule** root passes both clauses, which is right: it has its own
///    `.git` and reports its own origin, not the superproject's.
///
/// Both paths are resolved against `cwd` before comparison, because git answers
/// the second pair relatively in some layouts (`.git` and `../.git`) and
/// absolutely in others. A literal string comparison passes for the main
/// worktree's own root and fails one level down, which is a difference that
/// would not change the verdict but would name the wrong owner in the refusal.
///
/// # Why `git worktree list` is not used
///
/// It looks like the direct question and answers a different one: inside a
/// **submodule** its first `worktree` line is the *gitdir*
/// (`<super>/.git/modules/<name>`), not the checkout. A rule comparing `cwd`
/// against that head calls a submodule root a non-owner and leaves a genuine
/// repository unable to register the identity it holds.
///
/// # Cost
///
/// Two subprocesses when an origin exists, one when it does not, paid only by
/// `story project new` and `story project link origin`. **Resolution pays
/// none** — it still calls [`origin_of`], and still last.
#[must_use]
pub fn origin_at(cwd: &Path) -> crate::domain::remote::RepoOrigin {
    use crate::domain::remote::{OwnedOrigin, RepoOrigin};

    let Some(origin) = origin_of(cwd) else {
        return RepoOrigin::Absent;
    };
    const FACTS: [&str; 4] = [
        "rev-parse",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ];
    let unreadable = || RepoOrigin::Unknown(format!("git {}", FACTS.join(" ")));
    let Some(answer) = git_output(cwd, &FACTS) else {
        return unreadable();
    };
    let mut lines = answer.lines();
    let (Some(toplevel), Some(git_dir), Some(common_dir)) =
        (lines.next(), lines.next(), lines.next())
    else {
        return unreadable();
    };

    let toplevel = canonical(Path::new(toplevel));
    let main_worktree = canonical(&cwd.join(git_dir)) == canonical(&cwd.join(common_dir));
    if main_worktree && canonical(cwd) == toplevel {
        return RepoOrigin::Owned(OwnedOrigin::computed(origin));
    }
    RepoOrigin::Inherited {
        owner: if main_worktree {
            toplevel
        } else {
            main_worktree_of(&canonical(&cwd.join(common_dir)))
        },
        origin,
    }
}

/// The main working tree that owns `common_dir`, for a refusal that names it.
///
/// A linked worktree's common dir is the main checkout's `.git`, so its parent
/// is the directory being named. The one layout that does not fit — a linked
/// worktree *of a submodule*, whose common dir is `<super>/.git/modules/<name>`
/// — names that path instead: not a checkout, but a real path that identifies
/// the repository, which is better than naming nothing.
fn main_worktree_of(common_dir: &Path) -> PathBuf {
    match common_dir.file_name() {
        Some(name) if name == ".git" => common_dir
            .parent()
            .map_or_else(|| common_dir.to_path_buf(), Path::to_path_buf),
        _ => common_dir.to_path_buf(),
    }
}

/// The origin `cwd`'s **own** repository records, or a refusal saying why there
/// is none to take.
///
/// The single constructor for the omitted-URL form of `story project link
/// origin`, and the whole of what "when unambiguous" means there. Two
/// conditions, not one:
///
/// 1. `cwd` is a git repository's own top level, and
/// 2. that repository records an origin that normalizes.
///
/// # Why the first condition exists
///
/// `git config --get remote.origin.url` **walks up the directory tree** (SH-151).
/// A directory that is not itself a repository but sits inside one reports the
/// *enclosing* repository's origin — so without this check, running `story
/// project link origin` with no URL inside `monorepo/service-b` would silently
/// register the monorepo's origin against `service-b`. That is not a wrong
/// answer a user can see and undo: the unique index on
/// `project_remotes.normalized` means one origin belongs to at most one project,
/// so the claim is permanent and cross-project until somebody unlinks it, and
/// every other project in that repository stops being able to register at all.
///
/// [`origin_of`] deliberately keeps the walk, because *resolution* asking "does
/// any project answer for where I am standing?" is a question the enclosing
/// repository can legitimately answer. Registration is the opposite direction
/// and cannot.
///
/// # Why a refusal rather than `None`
///
/// [`origin_of`]'s contract is that every failure is the same failure, because a
/// resolution path must not distinguish them. Here the user typed a command
/// about origins and got nothing, so the reverse is true: the reason is the
/// whole of what they need, and the refusal names the URL they can pass
/// explicitly instead.
pub fn origin_here(cwd: &Path) -> Result<crate::domain::remote::OwnedOrigin, AppError> {
    use crate::domain::remote::RepoOrigin;

    match origin_at(cwd) {
        RepoOrigin::Owned(origin) => Ok(origin),
        RepoOrigin::Inherited { owner, .. } => Err(not_this_directorys_origin(cwd, &owner)),
        RepoOrigin::Unknown(command) => Err(AppError::Validation(format!(
            "no URL was given, and `{command}` failed in `{}`, so storyhook cannot tell whether \
             this directory owns the origin it reports.\n\nName the origin explicitly:\n\n  \
             story project link origin <url>",
            cwd.display()
        ))),
        RepoOrigin::Absent => Err(no_origin_to_take(cwd)),
    }
}

/// A URL the user typed, checked against the one shape it may not be.
///
/// `story project link origin <url>` is deliberately permissive: it exists so a
/// project whose checkout is on another machine — or in no repository at all —
/// can register the origin it answers to. So an unrelated URL is accepted
/// wherever it is typed, and the store's uniqueness index remains the guard
/// against two projects claiming one origin.
///
/// The exception is the case that index cannot see. If the URL *is* the
/// enclosing repository's own origin and this directory does not own it, then
/// nobody may be holding it yet, so there is no collision to refuse — and the
/// first sub-directory to ask takes the repository's identity permanently,
/// locking out every sibling project and the repository's own top level.
///
/// # Errors
///
/// [`AppError::Validation`] naming the directory that does own it.
pub fn claim_stated(
    cwd: &Path,
    url: crate::domain::remote::RemoteUrl,
) -> Result<crate::domain::remote::OwnedOrigin, AppError> {
    let claim = origin_at(cwd);
    if let Some(owner) = claim.forbids(&url) {
        return Err(not_this_directorys_origin(cwd, owner));
    }
    Ok(crate::domain::remote::OwnedOrigin::explicit(url))
}

/// The refusal for taking an origin some other directory owns.
///
/// One constructor because two verbs raise it — the omitted-URL form of `link
/// origin`, and the explicit-URL form when the URL given *is* the enclosing
/// repository's — and they are owed the same sentence: which directory owns it,
/// and why standing here is not enough.
fn not_this_directorys_origin(cwd: &Path, owner: &Path) -> AppError {
    AppError::Validation(format!(
        "`{}` does not own its repository's origin — `{}` does.\n\n`git config --get \
         remote.origin.url` answers the same for every directory in a repository and for every \
         worktree of it, so registering it here would take another checkout's identity, and an \
         origin belongs to at most one project.\n\nRun it in the directory that owns it, or name \
         a different origin explicitly:\n\n  story project link origin <url>",
        canonical(cwd).display(),
        owner.display()
    ))
}

/// The refusal when there is no origin here to take at all, distinguishing the
/// two reasons a user can act on differently.
///
/// [`origin_at`] deliberately folds "not a repository" and "no `origin`" into
/// one answer, because a *resolution* path must not tell them apart. A user who
/// typed a command about origins is owed the opposite, so this asks the one
/// extra question — on a path that is already failing.
fn no_origin_to_take(cwd: &Path) -> AppError {
    if git_output(cwd, &["rev-parse", "--show-toplevel"]).is_none() {
        return AppError::Validation(format!(
            "no URL was given, and `{}` is not inside a git repository.\n\nName the origin \
             explicitly:\n\n  story project link origin <url>",
            cwd.display()
        ));
    }
    let remotes = git_output(cwd, &["remote", "-v"]).unwrap_or_default();
    let seen = if remotes.trim().is_empty() {
        "  remotes    (none)".to_string()
    } else {
        remotes
            .lines()
            .map(|line| format!("  remotes    {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    AppError::Validation(format!(
        "no URL was given, and `{}` records no usable `remote.origin.url`.\n\n{seen}\n\n\
         storyhook reads exactly `git config --get remote.origin.url`. Name the origin \
         explicitly:\n\n  story project link origin <url>",
        cwd.display()
    ))
}

/// The git environment variables every probe here removes.
///
/// **`git config --get remote.origin.url` obeys `$GIT_DIR` over `cwd`, and
/// `git rev-parse --show-toplevel` does not.** With one inherited, the two
/// disagree: the origin is read from whatever repository the variable names
/// while the top level is read from the working directory, so an ownership
/// check comparing them agrees with itself and registers another repository's
/// identity. Measured on git 2.50.1.
///
/// The reachability is not the one it looks like. No git hook sets `GIT_DIR`
/// on that version — `pre-commit`, `post-commit`, `post-checkout` and
/// `pre-push` all run without it. What does reach here is a daemon: it inherits
/// the environment of whichever client started it and keeps it for its whole
/// life, and since SH-114 the daemon is the process that runs these probes.
/// That is SH-160, whose fix is at the spawn; this list is what makes *these*
/// probes correct regardless of it.
///
/// `GIT_CONFIG_*` is deliberately **not** here: the test suite isolates git
/// config through it, and removing it would make every fixture read the
/// developer's real `~/.gitconfig`.
const GIT_ENV_TO_SCRUB: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_NAMESPACE",
];

/// `git <args>` in `cwd`, or `None` if git failed for any reason at all.
///
/// Shared by [`origin_of`], [`origin_at`] and [`origin_here`] so there is one
/// place that knows a missing `git`, a non-repository and a failed command are
/// the same outcome — and one place that scrubs the inherited git environment
/// (see [`GIT_ENV_TO_SCRUB`]), so a probe answers about `cwd` and nothing else.
fn git_output(cwd: &Path, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new("git");
    for name in GIT_ENV_TO_SCRUB {
        command.env_remove(name);
    }
    let output = command.current_dir(cwd).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.trim().to_string())
}

/// Records an origin against a project — **the only place `src/` calls
/// [`link_remote`](crate::store::WriteOps::link_remote)**.
///
/// Pinned by `tests/invoker_seam.rs`, which fails if a second call site appears.
/// The point is not tidiness: the argument is an
/// [`OwnedOrigin`](crate::domain::remote::OwnedOrigin), which cannot be built
/// from a bare [`RemoteUrl`](crate::domain::remote::RemoteUrl) outside the
/// ownership check and one audited escape hatch. One funnel plus that type is
/// what makes "a directory registers only the origin it owns" a property rather
/// than a habit, and a new registration site cannot be added without answering
/// the ownership question.
///
/// # The SH-116 deviation this withdraws
///
/// A collision used to be **skipped** by `init`, on the ground that refusing it
/// would make a monorepo's second project impossible to create: every directory
/// in a repository reports the same origin, so the second `story project new`
/// collided with the first. SH-151 removes that reason — a sub-directory never
/// computes `Owned`, so it never reaches this at all — and a collision now means
/// something else entirely: a genuinely new project taking an origin another
/// project already holds, which is one repository wearing two identities.
/// [`adopt_origin`] refuses it.
pub(crate) fn register_origin(
    tx: &mut impl WriteOps,
    project: ProjectId,
    origin: &crate::domain::remote::OwnedOrigin,
    now: &str,
) -> Result<Registration, crate::store::StoreError> {
    let url = origin.url();
    if let Some(holder) = tx.project_by_remote(url)?
        && holder.id != project
    {
        return Ok(Registration::HeldBy(holder.slug));
    }
    tx.link_remote(project, url, now)?;
    Ok(Registration::Recorded)
}

/// What [`register_origin`] did.
///
/// A value rather than an error, because the two callers owe the user different
/// sentences for the same fact: `story project link origin` names the URL the
/// user typed and how to move it, while `story project new` says that commands
/// here already resolve to the holder. Deciding that here would give one of them
/// the wrong message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Registration {
    /// The project holds the origin — it may already have.
    Recorded,
    /// Another project holds it, named by slug. **Nothing was written.**
    HeldBy(String),
}

/// Registers `claim` against `project` if it is this directory's to register,
/// refusing if another project holds it.
///
/// The refusal is raised **inside the transaction** as
/// [`StoreError::Rejected`](crate::store::StoreError::Rejected), so the project
/// row this call is part of creating rolls back with it. That is what makes
/// "one origin, one project" hold against two `story project new` runs racing:
/// the check and the write are one transaction rather than a read followed by a
/// hopeful write.
fn adopt_origin(
    tx: &mut impl WriteOps,
    project: ProjectId,
    claim: &crate::domain::remote::RepoOrigin,
    now: &str,
) -> Result<(), crate::store::StoreError> {
    let Some(owned) = claim.owned() else {
        return Ok(());
    };
    match register_origin(tx, project, owned, now)? {
        Registration::Recorded => Ok(()),
        Registration::HeldBy(holder) => Err(crate::store::StoreError::Rejected(Box::new(
            AppError::Validation(format!(
                "`{url}` is already registered to project `{holder}`, and a git origin belongs to at \
                 most one project.\n\nCommands run in this checkout already resolve to \
                 `{holder}`, so there is probably nothing to do. If this really is a second \
                 project for the same repository, create it detached and name it \
                 explicitly:\n\n  story project new --prefix <PREFIX> --no-attach\n\nOr move the \
                 origin:\n\n  story --project {holder} project unlink origin {url}",
                url = owned.url().raw(),
            )),
        ))),
    }
}

/// Records `root` as the project's checkout, unless it already has one.
///
/// **Fills a gap; never replaces.** A project's checkout answers where its
/// repo-side work runs, and moving it is `story project link checkout`'s job —
/// a verb that reports the path it displaced, precisely because a silent
/// replacement is how somebody discovers six weeks later that dispatch has been
/// running in the wrong tree. Attaching a *second* clone must not quietly
/// perform that move.
///
/// This is also what makes `story project new` and `story project new
/// --attach` do the same thing, which is the premise the SH-117 fixture sweep
/// rests on: 251 call sites can only be rewritten mechanically if the two verbs
/// mean the same thing at every one of them.
pub(super) fn adopt_checkout(
    tx: &mut impl WriteOps,
    project: ProjectId,
    root: &Path,
) -> Result<(), crate::store::StoreError> {
    if tx.checkout_path(project)?.is_none() {
        tx.set_checkout_path(project, Some(root))?;
    }
    Ok(())
}

/// Forgets the recorded checkout, if it is the directory being forgotten.
///
/// **The other half of [`adopt_checkout`].** A project claiming to have a
/// checkout at a path that no longer exists is a row in `story project list`
/// and a card on the dashboard, indistinguishable from a real repository until
/// somebody clicks it — so `doctor --fix` clears it, through here.
///
/// Conditional on the paths matching, deliberately. A project may have been
/// pointed at some *other* checkout by `story project link checkout` between
/// the report and the repair, and forgetting a stale directory must not
/// silently discard a link somebody made on purpose.
pub(super) fn forget_checkout(
    tx: &mut impl WriteOps,
    project: ProjectId,
    path: &Path,
) -> Result<(), crate::store::StoreError> {
    if tx.checkout_path(project)?.as_deref() == Some(path) {
        tx.set_checkout_path(project, None)?;
    }
    Ok(())
}

/// What to tell someone whose directory names no project.
///
/// **The one constructor for this refusal**, which is what makes "it always
/// names both ways out" a property rather than a habit: there is no other way to
/// build one, so there is no site that can forget.
///
/// `origin` is what [`origin_of`] found, and it is reported either way — a
/// reader who has an origin needs to know it is not registered, and a reader who
/// has none needs to know that is why nothing was found. Reporting it costs
/// nothing here: this is only reached once the lookup has already run.
#[must_use]
pub fn no_project_refusal(
    cwd: &Path,
    origin: Option<&crate::domain::remote::RemoteUrl>,
) -> AppError {
    let origin_line = match origin {
        Some(remote) => format!("{} - not registered to any project", remote.raw()),
        None => "(none) - this directory has no git origin".to_string(),
    };
    AppError::NotFound(format!(
        "storyhook cannot tell which project this is.\n\n  directory  {}\n  origin     \
         {origin_line}\n\nName the project for one command, or for this shell:\n\n  story \
         --project <slug> <command>\n  export STORYHOOK_PROJECT=<slug>\n\nOr make this checkout \
         answer for itself, once:\n\n  story project new --prefix <PREFIX>\n\n`story project \
         list` shows the slugs this machine's store has.",
        cwd.display()
    ))
}

/// What to tell someone whose selector names a project the store does not have.
///
/// The other single constructor, and the reason the selector carries its source
/// rather than just a slug: a bad `--project` is a mistake in one command, and a
/// bad `$STORYHOOK_PROJECT` is a mistake in a *shell* that will repeat on every
/// command run there. The extra paragraph is the whole remedy for the second,
/// and it is the fix for a measured defect — the variable used to be ignored in
/// silence, so a typo in it meant every command quietly answered about a
/// different project.
#[must_use]
pub fn unknown_project_refusal(selector: &crate::api::wire::ProjectSelector) -> AppError {
    use crate::api::wire::ProjectSelector;
    let tail = match selector {
        ProjectSelector::Flag { .. } => String::new(),
        ProjectSelector::Environment { .. } => "\n\nIt is set in this shell, so every storyhook \
             command run here will fail this way until it is changed or unset."
            .to_string(),
    };
    AppError::NotFound(format!(
        "`{} {}` names no project this machine's store has.{tail}\n\n`story project list` shows \
         the slugs it does have.",
        selector.source(),
        selector.slug()
    ))
}

/// The repository's `[hooks]` table, if it has a readable one.
///
/// Fails **open** at every step, and deliberately: a repository whose pointer
/// file cannot be parsed still has to be usable, and a hook nobody can read is
/// a hook that does not fire rather than a command that refuses to run.
#[must_use]
pub fn pointer_hooks(root: &Path) -> Option<toml::Value> {
    read_pointer(root).ok().flatten().and_then(|p| p.hooks)
}

/// The repository's `[plugin]` table, if it has a readable one. See
/// [`pointer_hooks`].
#[must_use]
pub fn pointer_plugin(root: &Path) -> Option<toml::Value> {
    read_pointer(root).ok().flatten().and_then(|p| p.plugin)
}

/// The pointer format this build writes.
pub(crate) const POINTER_SCHEMA: u32 = 1;

/// Where the pointer file lives: `<root>/.storyhook.toml`.
///
/// A file rather than a directory, and beside the legacy `.storyhook/` rather
/// than inside it, so that the wave which deletes the directory does not have
/// to move the pointer at the same time.
#[must_use]
pub fn pointer_path(root: &Path) -> PathBuf {
    root.join(".storyhook.toml")
}

/// Reads a checkout's pointer file, if it has one.
///
/// Every failure names the file. That is not politeness: `.storyhook.toml` is
/// **committed to the repository and hand-authored** — it carries the user's
/// `[plugin]` and `[hooks]` tables beside the project's identity — so a syntax
/// error in it is an ordinary mistake made in an ordinary editor. Left to
/// `toml`'s own words, `story list` reports `TOML parse error at line 1, column
/// 6` and the user has no file to open. Resolution runs this on almost every
/// command, so the unattributed version could surface anywhere.
pub fn read_pointer(root: &Path) -> Result<Option<ProjectPointer>, AppError> {
    let path = pointer_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        AppError::Storage(format!(
            "{} could not be read: {e}. This file names the project this checkout \
             belongs to.",
            path.display()
        ))
    })?;
    let pointer = toml::from_str(&raw).map_err(|e: toml::de::Error| {
        AppError::Storage(format!(
            "{} is not valid: {e}This file names the project this checkout belongs to; \
             it is committed, so `git diff` on it is the fastest way to see what changed.",
            path.display()
        ))
    })?;
    Ok(Some(pointer))
}

/// Writes a checkout's pointer file, replacing any existing one.
pub fn write_pointer(root: &Path, pointer: &ProjectPointer) -> Result<(), AppError> {
    std::fs::write(pointer_path(root), toml::to_string_pretty(pointer)?)?;
    Ok(())
}

/// What `story project new` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOptions {
    /// The story-id prefix, defaulting to [`DEFAULT_PREFIX`].
    pub prefix: Option<String>,
    /// The project's display name, defaulting to the directory's basename.
    ///
    /// Unlike [`prefix`](Self::prefix) this is applied on **every** init, not
    /// only the one that creates the project: renaming is a reversible display
    /// change, whereas a prefix is baked into every story id ever minted, so
    /// re-initializing must leave one alone and may honour the other.
    pub name: Option<String>,
    /// Generate `AGENTS.md` when the repository does not already have one.
    pub agents_md: bool,
    /// Whether the directory this service was pointed at becomes the project's
    /// checkout.
    ///
    /// `true` is everything `story project new` has always done: the path is
    /// recorded for resolution, the origin registered, the checkout adopted,
    /// and the repository-side files written. `false` — `story project new
    /// --no-attach` — writes the store record and nothing else, for the project
    /// whose repository is on another machine or does not exist yet.
    ///
    /// **Idempotence is a property of the attach target, so `false` has none.**
    /// With no directory to match on there is nothing to recognize a second run
    /// by, and each one creates another project. That is the honest answer
    /// rather than a surprising one: `--no-attach` says the project is not
    /// identified by anything on this machine.
    pub attach: bool,
}

impl Default for InitOptions {
    fn default() -> Self {
        Self {
            prefix: None,
            name: None,
            agents_md: true,
            attach: true,
        }
    }
}

/// What `story project new` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitOutcome {
    /// The project this checkout now belongs to.
    pub project: ProjectId,
    /// Whether the project was created, as opposed to already existing.
    ///
    /// `story project new` is idempotent — running it twice re-registers the checkout
    /// and reports success — so this distinguishes the two without changing
    /// what the user is told.
    pub created: bool,
    /// Whether `AGENTS.md` was generated by this run.
    pub agents_md: bool,
    /// Whether the pointer file was written by this run.
    pub pointer: bool,
    /// What became of this directory's git origin.
    pub origin: OriginOutcome,
}

/// What `story project new` did about the origin the directory reports.
///
/// Reported rather than left silent (SH-151). A project created in a
/// subdirectory of a repository is a legitimate arrangement — a monorepo with a
/// project per service — but it is identified by its committed pointer file
/// rather than by an origin, and somebody who does not know that will wonder
/// why `story project list` shows no origin against it, or worse will not
/// wonder at all until a clone stops resolving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OriginOutcome {
    /// Registered: this checkout answers for the project by its origin, on any
    /// machine that clones it.
    Registered(String),
    /// The origin belongs to another directory, which is named. `holder` is the
    /// project that has registered it, if one has.
    Inherited {
        /// The directory entitled to the origin.
        owner: PathBuf,
        /// The project holding it, if any.
        holder: Option<String>,
    },
    /// No origin here: not a git repository, or no `origin` remote.
    Absent,
}

/// The name of the agent-instruction file `init` generates.
pub const AGENTS_MD: &str = "AGENTS.md";

/// What a [`ProjectService::delete`] destroyed.
#[derive(Clone, Debug)]
pub struct DeleteOutcome {
    /// The plan it carried out.
    pub plan: DeletePlan,
    /// What the store deleted, counted inside the deleting transaction.
    pub removed: DeletedProject,
}

/// Creating projects and registering the checkouts that belong to them.
///
/// Unlike every other service this one does *not* take a
/// [`Ctx`](super::Ctx): a context names the project it operates on, and
/// `init`'s whole job is that there is not one yet.
pub struct ProjectService<'a, S: Store> {
    store: &'a S,
    root: PathBuf,
    clock: Clock,
}

impl<'a, S: Store> ProjectService<'a, S> {
    /// A project service for the checkout at `root`.
    pub fn new(store: &'a S, root: impl Into<PathBuf>) -> Self {
        Self {
            store,
            root: root.into(),
            clock: Clock::System,
        }
    }

    /// Sets the clock this service's timestamps come from.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Initialises a project for this checkout.
    ///
    /// The project row, its default states, its default types and its story-id
    /// counter are written in one transaction. The repository-side artifacts
    /// are written afterwards, deliberately: they are content, not identity,
    /// and a failure to write one must not undo a project that already exists
    /// in the store.
    ///
    /// Idempotent. A checkout that already belongs to a project has its
    /// registration refreshed and its `prefix` left alone — the same shape as
    /// the legacy path, which skipped every file it found already present.
    ///
    /// "Already belongs" is asked of the committed pointer file *before* the
    /// path, matching every other resolution in the codebase. It matters on a
    /// fresh clone: the checkout is at a path the store has never seen, but its
    /// pointer names a project the store may well know — and answering by path
    /// alone would mint a second project for a repository that already has one,
    /// leaving the clone pointing at the old identity and storing into the new.
    ///
    /// With [`attach`](InitOptions::attach) off, none of the paragraph above
    /// applies: nothing outside the store is read or written, so there is
    /// nothing to adopt and every call creates a project. See that field.
    pub fn init(&self, options: &InitOptions) -> Result<InitOutcome, AppError> {
        if !options.attach {
            return self.create_detached(options);
        }
        let root = canonical(&self.root);
        let now = self.clock.now();
        let existing_pointer = read_pointer(&root)?;

        // A repository whose stories are still in `.storyhook/` is not an
        // uninitialized one, and treating it as such is how a user ends up with
        // an empty project beside their real data. `story migrate` is the
        // command for it, and this says so before anything is written.
        if existing_pointer.is_none() && root.join(".storyhook/project.toml").is_file() {
            return Err(unmigrated_error(&root));
        }

        // Read *before* the transaction opens. The probe spawns `git`, and a
        // subprocess inside a `BEGIN IMMEDIATE` holds the store's only write
        // lock for however long that process takes — which is 14 ms on a good
        // day and unbounded on a bad one.
        //
        // This is where an origin enters the store at all: SH-116 resolves *by*
        // origin, and without a registration site it would resolve by a key
        // nothing ever writes. Registering here rather than in a new verb keeps
        // the boundary the epic drew — `story project link origin <url>`, for
        // adopting an origin after the fact, is SH-117's.
        //
        // `origin_at` rather than `origin_of` (SH-151): the question here is not
        // "what origin is reported?" — every directory in a repository reports
        // the same one — but "may this directory register it?".
        let claim = origin_at(&root);
        if let crate::domain::remote::RepoOrigin::Unknown(command) = &claim {
            // Fail closed. Reading an unanswerable probe as "not owned" would be
            // safe today and wrong the moment a git too old to answer makes
            // every directory a non-owner; reading it as owned restores the
            // defect silently. Neither, and the user is told which command to
            // try themselves.
            return Err(AppError::Validation(format!(
                "`{command}` failed in `{}`, so storyhook cannot tell whether this directory \
                 owns the origin it reports.\n\nCreate the project without attaching an \
                 origin:\n\n  story project new --prefix <PREFIX> --no-attach",
                root.display()
            )));
        }
        // SH-151's council asked for a refusal here — a run that would leave the
        // project identified by *neither* an owned origin nor a pointer file.
        // There is nothing to refuse: an attaching run writes the pointer file
        // below, or adopts the one already there, so "neither" is not a state
        // this verb can produce. SH-119 deleted the switch that could produce
        // it rather than adding a check for it.

        let (project, created, uuid, prefix) = self.store.write(|tx| {
            if let Some(pointer) = &existing_pointer
                && let Some(existing) = tx.project_by_uuid(&pointer.uuid)?
            {
                adopt_checkout(tx, existing.id, &root)?;
                adopt_origin(tx, existing.id, &claim, &now)?;
                if let Some(name) = &options.name {
                    tx.rename_project(existing.id, name)?;
                }
                return Ok((existing.id, false, existing.uuid, existing.prefix));
            }
            // A checkout that already carries a pointer is a *clone*, not a new
            // repository, and the identity it names is the one to create. The
            // uuid exists precisely so a project survives being copied to
            // another machine; minting a fresh one here would leave the
            // committed file naming a project that exists nowhere, and the
            // repository resolving by path alone from then on — so moving the
            // checkout, or cloning it again, would stop resolving entirely.
            //
            // The prefix comes with it for the same reason. A clone whose
            // history is full of `ZZ-*` ids must not get a project that mints
            // `SH-1`; that is a second tracker wearing the first one's name.
            // `--prefix` is therefore ignored here, which matches the rule that
            // `init` on a project that already exists leaves its prefix alone.
            let (uuid, prefix) = match &existing_pointer {
                Some(pointer) => (pointer.uuid.clone(), pointer.prefix.clone()),
                None => (
                    uuid::Uuid::new_v4().to_string(),
                    options
                        .prefix
                        .clone()
                        .unwrap_or_else(|| DEFAULT_PREFIX.to_string()),
                ),
            };
            let name = options.name.clone().unwrap_or_else(|| display_name(&root));
            let project = tx.create_project(&NewProject {
                uuid: uuid.clone(),
                slug: unique_slug(&*tx, &name)?,
                name,
                prefix: prefix.clone(),
                created_at: now.clone(),
            })?;
            adopt_checkout(tx, project, &root)?;
            adopt_origin(tx, project, &claim, &now)?;
            write_states(tx, project, &default_states())?;
            tx.put_types(project, &default_types())?;
            Ok((project, true, uuid, prefix))
        })?;

        // Never overwritten. The file is user-authored the moment it carries a
        // `[plugin]` or `[hooks]` table, and `story project new` is idempotent — so a
        // second `init` in a repository that already has a pointer must leave
        // the user's configuration exactly where it is rather than replacing
        // the file with a freshly generated identity-only copy.
        //
        // **Not optional (SH-119).** An attaching run either writes this file or
        // adopts the one already here, so it cannot leave a project that no
        // directory identifies. That used to be a switch — `InitOptions::pointer`
        // — whose `false` was reachable from no caller in `src/` and whose only
        // effect was to make an unreachable project expressible. SH-151's council
        // asked for a refusal here; deleting the switch makes the state
        // unrepresentable instead, which is the same answer in a stronger form.
        let pointer = existing_pointer.is_none();
        if pointer {
            write_pointer(&root, &ProjectPointer::new(uuid, prefix.clone()))?;
        }

        let done_state = self.store.read(|tx| Ok(closed_state(tx, project)?))?;
        let agents_md = options.agents_md && self.write_agents_md(&root, &prefix, &done_state)?;

        // Read after the transaction, because the holder is only interesting
        // when this directory did *not* register — and in that case the write
        // did not touch the remotes table, so there is nothing to be stale
        // about.
        let origin = match &claim {
            crate::domain::remote::RepoOrigin::Owned(owned) => {
                OriginOutcome::Registered(owned.url().raw().to_string())
            }
            crate::domain::remote::RepoOrigin::Inherited { origin, owner } => {
                OriginOutcome::Inherited {
                    owner: owner.clone(),
                    holder: self
                        .store
                        .read(|tx| tx.project_by_remote(origin))?
                        .map(|held| held.slug),
                }
            }
            // `Unknown` is refused above, before anything is written.
            crate::domain::remote::RepoOrigin::Unknown(_)
            | crate::domain::remote::RepoOrigin::Absent => OriginOutcome::Absent,
        };

        Ok(InitOutcome {
            project,
            created,
            agents_md,
            pointer,
            origin,
        })
    }

    /// Creates a project attached to nothing — `story project new --no-attach`.
    ///
    /// The store record and nothing else. No directory is read, canonicalized,
    /// registered or written to, which is what makes this the verb for a
    /// project whose repository lives on another machine, or does not exist
    /// yet, or is one of several this store will never see.
    ///
    /// The directory the client ran in still supplies the *default name*, and
    /// only that. It is the one documented default `--name` has, and a caller
    /// who does not want it is one word away from saying so.
    fn create_detached(&self, options: &InitOptions) -> Result<InitOutcome, AppError> {
        let now = self.clock.now();
        let name = options
            .name
            .clone()
            .unwrap_or_else(|| display_name(&canonical(&self.root)));
        let prefix = options
            .prefix
            .clone()
            .unwrap_or_else(|| DEFAULT_PREFIX.to_string());

        let project = self.store.write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: uuid::Uuid::new_v4().to_string(),
                slug: unique_slug(&*tx, &name)?,
                name,
                prefix,
                created_at: now.clone(),
            })?;
            write_states(tx, project, &default_states())?;
            tx.put_types(project, &default_types())?;
            Ok(project)
        })?;

        Ok(InitOutcome {
            project,
            created: true,
            agents_md: false,
            pointer: false,
            // Nothing on disk was read, so there is no directory whose origin
            // this could be about.
            origin: OriginOutcome::Absent,
        })
    }

    /// Everything a [`delete`](Self::delete) would destroy. Writes nothing, and
    /// **reads no filesystem**.
    ///
    /// Separated from the deletion so that the answer can be shown to somebody
    /// before it happens, and so that the CLI's prompt and the dashboard's
    /// modal are looking at the same value rather than each computing their
    /// own.
    ///
    /// The checkout is listed because it is a directory left carrying a
    /// `.storyhook.toml` that names a project which will not exist — something
    /// only the person confirming can decide what to do about. It is not a list
    /// of files about to be destroyed: since SH-117 this verb destroys none.
    ///
    /// One directory rather than every directory storyhook was ever run in.
    /// The list used to come from `project_paths`, which recorded each one; the
    /// project's linked checkout is what is left of that, and it is the one a
    /// user would actually go and look at.
    pub fn delete_plan(&self, project: ProjectId) -> Result<DeletePlan, AppError> {
        let record = self.project_record(project)?;
        let (stories, events, checkouts) = self.store.read(|tx| {
            Ok((
                tx.stories(project, &crate::store::StoryQuery::all())?.len(),
                tx.event_count(project)?,
                tx.checkout_path(project)?
                    .map(|path| path.to_string_lossy().into_owned())
                    .into_iter()
                    .collect::<Vec<_>>(),
            ))
        })?;

        Ok(DeletePlan {
            slug: record.slug,
            name: record.name,
            prefix: record.prefix,
            stories,
            events,
            checkouts,
        })
    }

    /// Destroys the project, and nothing outside the store.
    ///
    /// **Reads and writes no filesystem.** Until SH-117 this also deleted the
    /// `.storyhook.toml` and the `AGENTS.md` that `init` had generated, in
    /// *every* recorded checkout — including directories the caller had never
    /// named, justified by the plan having listed them first. What survives of
    /// that reasoning is the listing: the plan still names the checkouts, so
    /// the person confirming knows which directories are about to be left
    /// carrying a pointer file that names nothing.
    ///
    /// Deleting them was the wrong trade in one direction only, and that is
    /// what decided it. A stale `.storyhook.toml` is a tidiness complaint with
    /// a clear diagnosis; a deleted `AGENTS.md` is work gone. A tracker whose
    /// data lives in a store outside every repository has no business writing
    /// to a repository at all.
    pub fn delete(&self, project: ProjectId) -> Result<DeleteOutcome, AppError> {
        let plan = self.delete_plan(project)?;
        let removed = self.store.write(|tx| tx.delete_project(project))?;
        Ok(DeleteOutcome { plan, removed })
    }

    /// This project's catalog row, or a `NotFound` naming the id.
    ///
    /// The id has already been resolved by the selector, so a miss here means
    /// the project was deleted between resolution and this call — reported
    /// rather than unwrapped, because a race is exactly what a destructive
    /// verb's two-step round trip can lose to.
    fn project_record(&self, project: ProjectId) -> Result<ProjectRecord, AppError> {
        self.store
            .read(|tx| tx.project(project))?
            .ok_or_else(|| AppError::NotFound("that project no longer exists".to_string()))
    }

    /// Writes `AGENTS.md`, reporting whether it did.
    ///
    /// An existing file is never overwritten: it is repository content the
    /// user may have edited, and `init` is idempotent.
    fn write_agents_md(
        &self,
        root: &Path,
        prefix: &str,
        done_state: &str,
    ) -> Result<bool, AppError> {
        let path = root.join(AGENTS_MD);
        if path.exists() {
            return Ok(false);
        }
        std::fs::write(&path, templates::agents_md(prefix, done_state))?;
        Ok(true)
    }
}

/// The state set a new project starts with.
///
/// Exactly [`crate::domain::REQUIRED_STATES`], in that order, with `active` on
/// `in-progress` — the floor and the default are the same set, which is what
/// makes a project conforming the moment it is created.
///
/// **No longer the legacy path's twin.** `storage::init_project` still seeds
/// the original three, deliberately: it writes a `.storyhook/` tree for
/// rollback, and a rollback that invented a state the tree never had would be
/// reconstructing something other than what was there. The difference is
/// recorded in `tests/legacy_reader.rs` rather than left to be discovered.
#[must_use]
pub fn default_states() -> Vec<StateDef> {
    vec![
        StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "in-progress".to_string(),
            super_state: SuperState::Open,
            role: Some("active".to_string()),
            description: None,
        },
        StateDef {
            slug: "blocked".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "done".to_string(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
    ]
}

/// The type set a new project starts with. See [`default_states`].
#[must_use]
pub fn default_types() -> Vec<TypeDef> {
    [
        ("story", "A user story or feature"),
        ("epic", "A large initiative containing child stories"),
        ("bug", "A defect or regression"),
        ("chore", "Maintenance or infrastructure work"),
        ("task", "A discrete unit of work"),
    ]
    .into_iter()
    .map(|(slug, description)| TypeDef {
        slug: slug.to_string(),
        description: Some(description.to_string()),
    })
    .collect()
}

/// The project's first CLOSED state, for the templates that name "done".
///
/// Falls back to the literal `done` for a project that has none — a template
/// is documentation, and documentation that fails to render is worse than
/// documentation naming a state the reader can correct.
pub fn closed_state(tx: &impl ReadOps, project: ProjectId) -> Result<String, AppError> {
    Ok(tx
        .states(project)?
        .into_iter()
        .find(|state| state.super_state == SuperState::Closed)
        .map_or_else(|| "done".to_string(), |state| state.slug))
}

/// A checkout's canonical path, falling back to the path as given.
///
/// The store matches checkouts by the path it was handed, so the caller has to
/// decide what canonical means. A directory that cannot be canonicalized is
/// one that does not exist yet — `init` on a path the user is about to create
/// — and passing it through unchanged is better than failing on it here, where
/// the eventual filesystem error would be far more specific.
fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// The display name for a checkout: its directory's basename.
fn display_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string())
}

/// A slug for `name` that no project in the store is using yet.
///
/// Collisions are expected — every `app` directory on a machine slugs to
/// `app` — and are resolved the way the registry has always resolved them, by
/// appending the first free numeric suffix.
pub(crate) fn unique_slug(tx: &impl ReadOps, name: &str) -> Result<String, AppError> {
    let base = slugify(name);
    if tx.project_by_slug(&base)?.is_none() {
        return Ok(base);
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if tx.project_by_slug(&candidate)?.is_none() {
            return Ok(candidate);
        }
        n += 1;
    }
}

/// Lowercases `value` and collapses every run of non-alphanumerics into a
/// single dash, falling back to `project` when nothing survives.
fn slugify(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "project".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod temp_project_tests {
    use super::*;
    use std::path::Path;

    /// The case that put 394 projects in a real store: a fixture directory
    /// under `$TMPDIR`, a store in the user's data home.
    #[test]
    fn a_temp_project_in_a_real_store_is_refused() {
        let root = std::env::temp_dir().join("tmp.abc123");
        let store = Path::new("/Users/someone/.local/share/storyhook/store.db");
        let error = refuse_temp_project(&root, store, false)
            .expect_err("a temp project in a real store must be refused");
        assert_eq!(error.exit_code(), 2, "it is a usage error, not a crash");
        let message = error.to_string();
        assert!(
            message.contains("--store-path") || message.contains("STORYHOOK_STORE_PATH"),
            "the message must lead with a lever that actually wins the precedence contest: \
             {message}"
        );
        assert!(
            message.contains("STORYHOOK_DATA_DIR"),
            "the message must still name the environment-only route: {message}"
        );
        assert!(
            message.contains(ALLOW_TEMP_PROJECT),
            "the message must name the override: {message}"
        );
    }

    /// What this project's own suite does, and what any correctly isolated
    /// suite does. Refusing it would be the fix breaking its own users.
    #[test]
    fn a_temp_project_in_a_temp_store_is_allowed() {
        let root = Path::new("/private/tmp/storyhook-tests/fixture-1");
        let store = Path::new("/private/tmp/storyhook-gate.XXXX/data/store.db");
        assert!(
            refuse_temp_project(root, store, false).is_ok(),
            "a throwaway project in a throwaway store is exactly what a test builds"
        );
    }

    /// The ordinary case: a real repository, a real store.
    #[test]
    fn a_real_project_in_a_real_store_is_allowed() {
        let root = Path::new("/Volumes/Code/mikeyward/storyhook");
        let store = Path::new("/Users/someone/.local/share/storyhook/store.db");
        assert!(refuse_temp_project(root, store, false).is_ok());
    }

    /// A deliberate override is honoured, so the refusal can never become a
    /// wall someone has no way past.
    #[test]
    fn the_override_permits_what_would_otherwise_be_refused() {
        let root = std::env::temp_dir().join("tmp.abc123");
        let store = Path::new("/Users/someone/.local/share/storyhook/store.db");
        assert!(refuse_temp_project(&root, store, true).is_ok());
    }

    /// `/tmp` and `/private/tmp` are the same directory on macOS reached by two
    /// names. Without canonicalization one of them is not recognised as
    /// temporary, and half the cases walk straight through the guard.
    #[test]
    fn both_spellings_of_the_macos_temp_directory_are_recognised() {
        for spelling in ["/tmp/fixture", "/private/tmp/fixture"] {
            assert!(
                is_under_temp(Path::new(spelling)),
                "`{spelling}` must be recognised as temporary"
            );
        }
        assert!(
            !is_under_temp(Path::new("/Volumes/Code/mikeyward/storyhook")),
            "a real checkout must not be mistaken for a temporary directory"
        );
    }
}
