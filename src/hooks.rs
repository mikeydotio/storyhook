use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::env::git_env;
use crate::error::AppError;

const HOOK_MARKER: &str = "# storyhook managed hook -- do not edit this line";

/// The merge-arrival shell: links the commits a merge brought, and — on
/// `main` or `master` — closes the stories it says it closes.
///
/// Composed via `concat!` into any hook that needs to react to a commit whose
/// parent count marks it as a merge, rather than duplicated. A second,
/// hand-copied trailer alternation would drift silently past
/// `tests/service_git.rs::every_keyword_the_merge_hook_closes_on_also_claims`,
/// which only checks the alternation is *present* in `src/hooks.rs`, not that
/// it is unique.
///
/// # Its known consumers — keep this table current
///
/// | Caller | `BASE` | `FLOOR` | Why that floor |
/// |---|---|---|---|
/// | `POST_MERGE_HOOK` | `ORIG_HEAD` | `5` | the minutes SH-182's slack alone requires (below) |
/// | `POST_COMMIT_HOOK`, when `HEAD` has two or more parents (SH-341) | `HEAD^1` | `60` | must never derive a window narrower than the flat `1h` this hook already promised for an ordinary commit |
///
/// A story that changes one caller's `--deadline`, guard, or dispatch shape
/// (SH-343 is live doing exactly that as this is written) does not touch this
/// function. A story that changes the *window arithmetic* or the *trailer
/// grammar* changes every caller in this table at once — that is the point of
/// sharing it.
///
/// # `BASE`
///
/// The exclusive start of the arrival range, `$BASE..HEAD`. `ORIG_HEAD` and
/// `HEAD^1` agree for every merge shape measured while building this: a clean
/// `--no-ff` merge, a conflicted merge concluded by `git commit` or
/// `git merge --continue`, and a clean `git merge --no-commit` concluded by
/// `git commit`. They are not the same *kind* of answer, though — `ORIG_HEAD`
/// is mutable global state a stray `git reset` between resolve and commit can
/// clobber; `HEAD^1` is intrinsic to the commit the hook is running for.
/// `post-merge` keeps `ORIG_HEAD` because a fast-forward has no second parent
/// for `HEAD^1` to name.
///
/// # `FLOOR`
///
/// The narrowest window this *caller* will ever ask for, in minutes — not to
/// be confused with the five-minute slack below, the same number by
/// coincidence and a different quantity by origin. `post-merge` has never
/// promised more than what it can derive, so its floor is the slack alone.
/// `post-commit` promised a flat `1h` for an ordinary commit before this
/// function existed for it, and a derived window can come out narrower than
/// that when a merge brings only recent commits — so its floor keeps the
/// derived window from ever regressing coverage it already had.
///
/// # Why the window is derived and not a constant
///
/// `commit-sync` takes a *duration*, and `read_log` turns it into
/// `git log --since`, which filters on **committer date**. A merged commit
/// keeps the date it was originally made — so the obvious spelling, a flat
/// `--since 1h`, would reproduce here exactly the defect SH-330 added this
/// function to fix: the merge runs, the sync runs, and everything older than
/// the window is missed. The window is therefore derived from the oldest
/// committer date in `$BASE..HEAD` — the exact set of commits that arrived.
///
/// # Why the slack is five minutes, and why it must not be tightened
///
/// This script computes `NOW` with `date +%s`; the *daemon* computes the
/// cutoff later, with its own clock, after a spawn and possibly a cold start —
/// up to **150s** (SH-182). Integer truncation of `/ 60` eats up to 59s more.
/// So the slack must satisfy `slack × 60 ≥ 150 + 59`, i.e. at least four
/// minutes; five is that with room. Tightening this toward the arriving
/// commits reads as an obvious optimisation and is a silent reintroduction of
/// SH-330 (or, for `post-commit`, of SH-341).
///
/// The floor does a second job for whichever caller's `FLOOR` is the slack
/// itself: `parse_duration` accepts a **negative** number, and a negative
/// duration puts the cutoff in the *future*, where the scan finds nothing at
/// all. A clock that jumps backwards cannot produce one here.
///
/// Scanning too wide is a no-op — `commit-sync` short-circuits an
/// already-linked commit on a primary-key probe, and moves no story it has
/// already moved. Scanning too narrow is the defect. The asymmetry is what
/// pays for the generosity.
///
/// # Why an empty range does nothing
///
/// An empty `$BASE..HEAD` means no commit arrived, so there is nothing to
/// scan and no age to derive a window from. Falling back to a default window
/// would scan a period this call has no claim on; computing one from the
/// empty string would be arithmetic on nothing. `post-commit`'s caller only
/// ever invokes this on a two-or-more-parent `HEAD`, for which `HEAD^1..HEAD`
/// is non-empty by construction — it always contains `HEAD` itself.
///
/// # The closing half (SH-56)
///
/// Auto-closes stories a merge says it closes.
///
/// `%B`, not `%s` (SH-56). The subject line is reserved for a summary under 72
/// characters by every Conventional Commits guide there is, so a `Closes SH-12`
/// reference lives in the body as a trailer — and this hook once read subjects
/// only, so following the convention guaranteed it never fired.
///
/// The outer `while read` loop does not need one commit per iteration: `%B`
/// emits multi-line records, and its only job is feeding text to `grep -oiE`,
/// which is line-oriented already and emits one match per line.
///
/// `[[:space:]]` rather than `\s`: `\s` is a GNU extension, this runs under
/// `/bin/sh` on whatever the user has, and a hook that silently matches
/// nothing on BSD grep is exactly the failure mode SH-56 already was.
///
/// `tests/hook_execution.rs` runs this script — the real file, installed into
/// a real repository, over a real merge or merge-shaped commit. The defect
/// class here is "hook logic shipped as an untested string literal", and a
/// `&str` nothing executes is how it survived twice.
///
/// # `--deadline 10` (SH-343), and why it leaves the slack above untouched
///
/// Both `story` calls below bound the *client's* wait, not the *daemon's*
/// cutoff computation — a different axis from the five-minute slack above.
/// Shortening that slack toward the arriving commits would silently
/// reintroduce SH-330; nothing here does that. Giving up on the sync call
/// costs nothing (`--quiet 2>/dev/null || true` already discards its output,
/// and expiry does not cancel the request). Giving up on a `story move`
/// inside the loop below is real but bounded: the daemon still completes
/// that move and nothing here reads the answer. `DeadlineExceeded`'s dedicated
/// exit code 12 lets the loop stop after that first give-up without confusing
/// an ordinary per-story or store failure for a persistently unreachable
/// daemon (SH-353). The sync call deliberately does not share the latch: it
/// may outlive this client while warming a daemon that can accept the moves.
macro_rules! merge_arrival_fn {
    () => {
        r#"storyhook_merge_arrival() {
  BASE="$1"
  FLOOR="$2"
  OLDEST="$({ git log --format='%ct' "$BASE..HEAD" | sort -n | head -n 1; } 2>/dev/null)"
  if [ -n "$OLDEST" ]; then
    MINUTES=$(( ($(date +%s) - OLDEST) / 60 + 5 ))
    if [ "$MINUTES" -lt "$FLOOR" ]; then MINUTES="$FLOOR"; fi
    story --deadline 10 commit-sync --since "${MINUTES}m" --quiet 2>/dev/null || true
  fi
  BRANCH="$(git symbolic-ref --short HEAD 2>/dev/null)"
  case "$BRANCH" in main|master) ;; *) return 0 ;; esac
  git log --format='%B' "$BASE..HEAD" 2>/dev/null |
    grep -oiE '(closes?|fixes?|resolves?)[[:space:]]+[A-Z]+-[0-9]+' |
    while IFS= read -r match; do
      STORY_ID="$(echo "$match" | grep -oE '[A-Z]+-[0-9]+' | head -1)"
      if [ -n "$STORY_ID" ]; then
        story --deadline 10 move "$STORY_ID" done "auto-closed by merge" --quiet 2>/dev/null
        STORY_STATUS=$?
        if [ "$STORY_STATUS" -eq 12 ]; then break; fi
      fi
    done
}
"#
    };
}

/// Links a commit's story references, and — for a merge concluded by
/// `git commit` rather than `git merge` (SH-341) — the full merge-arrival
/// shell too.
///
/// A merge that conflicts, or that is finished with `git merge --continue` or
/// `git merge --no-commit` followed by `git commit`, never runs `post-merge`
/// at all — measured on git 2.50.1, all three ways, before this fix was
/// designed. `MERGE_HEAD` and `MERGE_MSG` are both already gone by the time
/// this hook runs in every one of those cases, so neither can be asked; a
/// commit's own parent count is what survives, and two-or-more is the cheap,
/// honest, and — checked against `git merge --squash` and a resolved
/// cherry-pick, both one-parent — exact question.
///
/// `FLOOR` is `60`, not the `5` `merge_arrival_fn!` uses for `post-merge`: see
/// that macro's doc comment for why.
///
/// `--deadline 10` (SH-343) on the ordinary-commit fallback below, for the
/// same reason `merge_arrival_fn!`'s own calls carry it: this hook runs
/// *inside* `git commit`, which imposes no timeout of its own.
///
/// # The trailing `exit 0` (SH-355)
///
/// Behaviour-neutral here — `githooks(5)` says a nonzero `post-commit` cannot
/// affect `git commit`'s outcome — but stated anyway so every managed hook
/// shares one invariant a test can check mechanically instead of a comment a
/// future edit can silently violate: nothing appended after this line may
/// leak its own exit status into the hook's. `tests/hooks.rs` fences it
/// across all three hooks by reading the installed files back.
const POST_COMMIT_HOOK: &str = concat!(
    "#!/bin/sh\n",
    "# storyhook managed hook -- do not edit this line\n",
    "command -v story >/dev/null 2>&1 || exit 0\n",
    merge_arrival_fn!(),
    r#"if git rev-parse -q --verify HEAD^2 >/dev/null 2>&1; then
  BASE="$(git rev-parse HEAD^1 2>/dev/null)" || exit 0
  storyhook_merge_arrival "$BASE" 60
  exit 0
fi
story --deadline 10 commit-sync --since 1h --quiet 2>/dev/null || true
exit 0
"#
);

/// Fires on every merge `git` concludes without a `commit` of its own — see
/// `merge_arrival_fn!` for what it does and why. `ORIG_HEAD` is the base
/// because a fast-forward merge has no second parent for `HEAD^1` to name;
/// `5` is `merge_arrival_fn!`'s slack floor with nothing added on top, since
/// this hook has never promised more than what it can derive.
///
/// The trailing `exit 0` is the same class fence `POST_COMMIT_HOOK` states —
/// see its doc comment.
const POST_MERGE_HOOK: &str = concat!(
    "#!/bin/sh\n",
    "# storyhook managed hook -- do not edit this line\n",
    "command -v story >/dev/null 2>&1 || exit 0\n",
    merge_arrival_fn!(),
    r#"ORIG_HEAD="$(git rev-parse ORIG_HEAD 2>/dev/null)" || exit 0
storyhook_merge_arrival "$ORIG_HEAD" 5
exit 0
"#
);

/// Appends the top-priority ready story as a hint in the commit message editor.
///
/// # `--deadline 10` (SH-343)
///
/// The only one of the three managed hooks whose answer is read rather than
/// discarded — and also the one where the wait is most visible, since it runs
/// *before* the editor opens, ahead of the commit existing at all. Without a
/// bound this call inherited the same 150s exposure as the other two.
/// Abandoning it degrades to "no hint appended", the same shape SH-182 already
/// sanctioned for a caller that cannot wait regardless of whether storyhook
/// could — not a failure, since this hook's entire contract is best-effort.
///
/// # This is also the only one of the three git's verdict listens to (SH-355)
///
/// `githooks(5)`: a nonzero `post-commit`/`post-merge` "cannot affect the
/// outcome of `git commit`", but a nonzero `prepare-commit-msg` **aborts the
/// commit**. Every exit path here ends in an explicit `exit 0` for exactly
/// that reason — including the append itself, an `if` rather than the
/// `[ -n "$STORY_ID" ] && { ... }` this shipped with, whose status *was* the
/// script's own final command. An empty backlog answers `story next` with
/// `{"result":"ok","message":"no ready stories"}` at exit 0 (a real answer,
/// not a refusal — see `src/invoke.rs`'s `Next` arm), so `grep` finding no
/// `"id"` was never the failure a `\|\| exit 0` upstream could catch; only the
/// trailing conditional's own status decided the commit, silently, for as
/// long as this hook has existed. `tests/hook_execution.rs`'s empty-backlog
/// section pins this; `tests/hooks.rs` fences the class across all three hooks.
const PREPARE_COMMIT_MSG_HOOK: &str = r#"#!/bin/sh
# storyhook managed hook -- do not edit this line
command -v story >/dev/null 2>&1 || exit 0
case "$2" in message|merge|squash) exit 0 ;; esac
NEXT="$(story --deadline 10 next --count 1 --json 2>/dev/null)" || exit 0
STORY_ID="$(echo "$NEXT" | grep -o '"id": *"[^"]*"' | head -1 | cut -d'"' -f4)"
if [ -n "$STORY_ID" ]; then
  TITLE="$(echo "$NEXT" | grep -o '"title": *"[^"]*"' | head -1 | cut -d'"' -f4)"
  printf '\n# Top story: %s — %s\n' "$STORY_ID" "$TITLE" >> "$1"
fi
exit 0
"#;

const HOOKS: &[(&str, &str)] = &[
    ("post-commit", POST_COMMIT_HOOK),
    ("post-merge", POST_MERGE_HOOK),
    ("prepare-commit-msg", PREPARE_COMMIT_MSG_HOOK),
];

fn is_storyhook_hook(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|content| content.contains(HOOK_MARKER))
        .unwrap_or(false)
}

/// The delegator a user needs when their own hook holds the name git runs.
///
/// Printed **verbatim** rather than described, so a paste carries no shell the
/// user had to author. `.githooks/post-commit` in this repository is the same
/// line, working, and `tests/push_gate.rs` keeps it that way.
const DELEGATOR: &str = r#"exec "$(git rev-parse --git-common-dir)/hooks/$(basename "$0")""#;

/// Where git looks for this repository's hooks, and where storyhook owns one.
///
/// Both are resolved by **asking git** rather than by assuming a layout, and
/// `<root>/.git/hooks` was wrong in three ways this type exists to stop.
///
/// In a *linked* worktree `.git` is a file holding a `gitdir:` pointer, not a
/// directory, so joining `hooks` onto it could not be created at all (SH-314) —
/// and a worktree is where this project does most of its work. The obvious
/// repair is wrong too: git resolves hooks for **every** worktree in the
/// *common* directory, so writing into the worktree's private git directory
/// would install hooks git never runs. And when `core.hooksPath` is set git
/// stops consulting `$GIT_DIR/hooks` **wholesale** — not as a fallback, not at
/// all — so everything written there is dark (SH-313).
///
/// Resolution is three questions, and each is load-bearing:
///
/// 1. `--show-toplevel` names the directory git runs hooks from. Asking it
///    first makes the rest independent of the caller's working directory —
///    which matters because since SH-114 this runs *in the daemon*, against the
///    envelope's root rather than any shell's cwd. It also refuses a bare
///    repository for free (`fatal: this operation must be run in a work tree`),
///    preserving a refusal the hand-built path gave by accident.
/// 2. `--git-common-dir` gives the directory storyhook owns.
/// 3. `--git-path hooks` gives the directory git will *actually* consult, which
///    is the same one unless `core.hooksPath` is set.
///
/// Both answers come back *relative* in an ordinary checkout (`.git`) and
/// *absolute* in a linked worktree, so joining them onto the top-level is not
/// defensive coding — it is the documented shape of the answer, and
/// `Path::join` discards its left-hand side when the right is absolute, which
/// is exactly what both cases need. Measured on git 2.50.1: a relative
/// `core.hooksPath` answers relative to the *child's* cwd (`../.githooks` from a
/// subdirectory), so asking from the top-level is what makes the join correct
/// from any depth.
///
/// Every spawn goes through [`git_env::command`], the one place in `src/` that
/// constructs a `git` (`tests/spawn_inventory.rs` fails on a second one).
/// Nothing here is cached: the daemon outlives the shell that started it, and
/// `GIT_CONFIG_GLOBAL` and its siblings reach git through that inherited
/// environment, so an answer memoised once could outlive the configuration that
/// produced it.
#[derive(Debug, Clone)]
struct HookDirs {
    /// `--git-common-dir/hooks`: shared by every worktree, and the only
    /// directory storyhook writes into when something else holds the name git
    /// runs.
    managed: PathBuf,
    /// `--git-path hooks`: where git will *actually* look. Equal to `managed`
    /// unless `core.hooksPath` is set.
    effective: PathBuf,
}

impl HookDirs {
    /// Resolves the hook directories for the repository containing `cwd`.
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] when `cwd` is not inside a git working tree,
    /// when `git` cannot be run to ask, or when `core.hooksPath` names
    /// something that is not a directory — in which case git runs no hooks at
    /// all and there is nowhere to install to.
    fn resolve(cwd: &Path) -> Result<Self, AppError> {
        let toplevel = Self::ask(cwd, "--show-toplevel")?;
        let common = Self::ask(&toplevel, "--git-common-dir")?;
        let effective = Self::ask(&toplevel, "--git-path hooks")?;

        let dirs = Self {
            managed: toplevel.join(common).join("hooks"),
            effective: toplevel.join(effective),
        };

        // Generalised on purpose: `core.hooksPath=/dev/null` is the documented
        // way to switch hooks off, but the dangerous member of this class is the
        // *accident* — a typo'd path naming a regular file — which would
        // otherwise get the cheerful "installed" this story is about. Neither
        // `/dev/null` nor any other literal appears here.
        if dirs.effective.exists() && !dirs.effective.is_dir() {
            return Err(AppError::Validation(format!(
                "git runs no hooks in this repository: core.hooksPath resolves to {}, \
                 which is not a directory",
                dirs.effective.display()
            )));
        }
        Ok(dirs)
    }

    /// One `git rev-parse <flag>`, refusing an empty answer as loudly as a
    /// failed one — a blank line here would otherwise become the current
    /// directory and send three executables somewhere nobody asked for.
    fn ask(cwd: &Path, flag: &str) -> Result<PathBuf, AppError> {
        let args: Vec<&str> = std::iter::once("rev-parse")
            .chain(flag.split(' '))
            .collect();
        git_env::output(cwd, &args)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "not a git repository: `git rev-parse {flag}` had no answer for {}",
                    cwd.display()
                ))
            })
    }

    /// The directory storyhook owns.
    fn managed(&self) -> &Path {
        &self.managed
    }

    /// The directory git will actually consult.
    fn effective(&self) -> &Path {
        &self.effective
    }

    /// Whether git looks exactly where storyhook owns — the ordinary case, and
    /// the one whose output must not change by a byte.
    fn effective_is_managed(&self) -> bool {
        self.effective == self.managed
    }

    /// Every directory a managed hook could be sitting in, for uninstall to
    /// sweep. One entry in the ordinary case; never two identical ones.
    fn all(&self) -> Vec<&Path> {
        if self.effective_is_managed() {
            vec![self.managed()]
        } else {
            vec![self.effective(), self.managed()]
        }
    }
}

/// What install did with one hook, and what git will do with it.
///
/// Cases 3 and 4 of SH-313 — a foreign hook holding the name, and *this
/// repository's own chainer* holding it — are deliberately **not**
/// distinguished. Telling them apart means deciding whether a stranger's script
/// delegates, which cannot be read off the file: a substring test on someone
/// else's hook is SH-239's "a process's NAME is not its identity" reopened, and
/// it fails in both directions (a comment mentioning the flag passes, a
/// genuinely chaining hook written differently fails).
#[derive(Debug, Clone)]
enum Installed {
    /// Written where git looks.
    Direct {
        path: PathBuf,
        /// A stale managed copy refreshed in passing, if there was one.
        also_refreshed: Option<PathBuf>,
    },
    /// The name git runs is held by a file storyhook did not write, so the
    /// managed copy went to the directory storyhook owns instead.
    Shadowed { wrote: PathBuf, git_runs: PathBuf },
    /// Nothing was written: a file storyhook did not write holds every name it
    /// could have used.
    Skipped { git_runs: PathBuf },
}

/// What `story hooks install` did, and what git will run.
///
/// A report rather than a `String` because the caveats are **per hook** and a
/// process-wide exit code cannot carry one. They travel as
/// [`warnings`](Self::warnings) in the response envelope's own `warnings`
/// array, so a `--json` caller reads them as data instead of parsing English.
#[derive(Debug, Clone)]
pub struct InstallReport {
    dirs: HookDirs,
    hooks: Vec<(&'static str, Installed)>,
}

impl InstallReport {
    /// The status block, one line per hook.
    #[must_use]
    pub fn message(&self) -> String {
        let lines: Vec<String> = self
            .hooks
            .iter()
            .map(|(name, outcome)| match outcome {
                Installed::Direct { .. } if self.dirs.effective_is_managed() => {
                    format!("  {name} — installed")
                }
                Installed::Direct {
                    path,
                    also_refreshed: None,
                } => format!("  {name} — installed in {}", path.display()),
                Installed::Direct {
                    path,
                    also_refreshed: Some(stale),
                } => format!(
                    "  {name} — installed in {} (and refreshed the stale copy in {})",
                    path.display(),
                    stale.display()
                ),
                Installed::Shadowed { wrote, git_runs } => format!(
                    "  {name} — installed in {}; git runs {}",
                    wrote.display(),
                    git_runs.display()
                ),
                Installed::Skipped { .. } => {
                    format!("  {name} — skipped (existing user hook)")
                }
            })
            .collect();
        format!("Git hooks:\n{}", lines.join("\n"))
    }

    /// One entry per hook git will not run directly, plus one for the
    /// directory itself when `core.hooksPath` sends git somewhere storyhook
    /// does not own.
    ///
    /// Empty for an ordinary repository, which is what keeps that case's output
    /// byte-identical: `MessageWithWarnings(msg, vec![])` renders exactly as
    /// `Message(msg)` does in both the human and the JSON renderer.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if !self.dirs.effective_is_managed()
            && self
                .hooks
                .iter()
                .any(|(_, o)| matches!(o, Installed::Direct { .. }))
        {
            warnings.push(format!(
                "core.hooksPath sends git to {}, which storyhook does not own: hooks \
                 written there are untracked, and a tool that regenerates that \
                 directory — or `git clean -fd` — will remove them",
                self.dirs.effective().display()
            ));
        }

        for (name, outcome) in &self.hooks {
            match outcome {
                Installed::Shadowed { wrote, git_runs } => warnings.push(format!(
                    "{} is not a storyhook hook, so git runs it instead of the managed \
                     {name}. That hook was written to {} and git reaches it only if {} \
                     delegates:\n    {DELEGATOR}",
                    git_runs.display(),
                    wrote.display(),
                    git_runs.display()
                )),
                Installed::Skipped { git_runs } if !self.dirs.effective_is_managed() => {
                    warnings.push(format!(
                        "{name} was not installed anywhere: {} holds the name git runs \
                         and {} holds the managed copy, and storyhook wrote neither",
                        git_runs.display(),
                        self.dirs.managed().join(name).display()
                    ));
                }
                _ => {}
            }
        }
        warnings
    }
}

/// What `story hooks uninstall` removed, and from where.
#[derive(Debug, Clone)]
pub struct UninstallReport {
    dirs: HookDirs,
    hooks: Vec<(&'static str, Swept)>,
}

/// What uninstall found for one hook across every directory it swept.
#[derive(Debug, Clone)]
enum Swept {
    Removed,
    /// Found, but written by someone else — left alone.
    NotOurs,
    NotPresent,
}

impl UninstallReport {
    /// The status block, one line per hook.
    #[must_use]
    pub fn message(&self) -> String {
        let lines: Vec<String> = self
            .hooks
            .iter()
            .map(|(name, swept)| match swept {
                Swept::Removed => format!("  {name} — removed"),
                Swept::NotOurs => format!("  {name} — skipped (not a storyhook hook)"),
                Swept::NotPresent => format!("  {name} — not present"),
            })
            .collect();
        format!("Git hooks:\n{}", lines.join("\n"))
    }

    /// Names the directories swept when there was more than one, so a user who
    /// re-pointed `core.hooksPath` since installing can see what was *not*
    /// looked at.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        if self.dirs.effective_is_managed() {
            return Vec::new();
        }
        vec![format!(
            "swept {} and {}. A hook installed while core.hooksPath named a different \
             directory is not in either, and storyhook keeps no record of where it \
             wrote. {} is shared by every worktree of this repository",
            self.dirs.effective().display(),
            self.dirs.managed().display(),
            self.dirs.managed().display()
        )]
    }
}

/// `true` when `path` is free for storyhook to write — either nothing is there,
/// or what is there is a managed hook of ours to overwrite.
fn is_ours_or_free(path: &Path) -> bool {
    !path.exists() || is_storyhook_hook(path)
}

/// Writes one managed hook, executable.
fn write_hook(path: &Path, content: &str) -> Result<(), AppError> {
    fs::write(path, content)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Installs the managed hooks where git will actually look for them.
///
/// # Errors
///
/// [`AppError::Validation`] when `root` is not inside a git working tree, or
/// when `core.hooksPath` names something that is not a directory. Both mean
/// there is nowhere an installed hook could ever run from, which is the one
/// answer that does not depend on a file storyhook did not write — and so the
/// one that is allowed to fail the command.
pub fn install_hooks(root: &Path) -> Result<InstallReport, AppError> {
    let dirs = HookDirs::resolve(root)?;
    fs::create_dir_all(dirs.effective())?;

    let mut hooks = Vec::new();
    for (name, content) in HOOKS {
        let target = dirs.effective().join(name);

        if is_ours_or_free(&target) {
            write_hook(&target, content)?;

            // A stale managed copy is refreshed, never created. A repository
            // that adopted husky *after* storyhook keeps one, and the bodies
            // are not version-stable — SH-56 changed `%s` to `%B` in the
            // post-merge hook — so leaving it would arm a wrong hook to go
            // live the moment `core.hooksPath` is unset.
            let managed = dirs.managed().join(name);
            let also_refreshed = if !dirs.effective_is_managed() && is_storyhook_hook(&managed) {
                write_hook(&managed, content)?;
                Some(managed)
            } else {
                None
            };

            hooks.push((
                *name,
                Installed::Direct {
                    path: target,
                    also_refreshed,
                },
            ));
            continue;
        }

        // Someone else holds the name git runs. Their hook may delegate to
        // ours — this repository's own chainers do — and storyhook cannot tell
        // without executing it, so the managed copy goes where a delegator
        // would look and the report states the condition rather than assuming
        // either way.
        if dirs.effective_is_managed() {
            hooks.push((*name, Installed::Skipped { git_runs: target }));
            continue;
        }

        let managed = dirs.managed().join(name);
        if is_ours_or_free(&managed) {
            fs::create_dir_all(dirs.managed())?;
            write_hook(&managed, content)?;
            hooks.push((
                *name,
                Installed::Shadowed {
                    wrote: managed,
                    git_runs: target,
                },
            ));
        } else {
            hooks.push((*name, Installed::Skipped { git_runs: target }));
        }
    }
    Ok(InstallReport { dirs, hooks })
}

/// Removes the managed hooks from every directory one could be sitting in.
///
/// # Errors
///
/// As [`install_hooks`].
pub fn uninstall_hooks(root: &Path) -> Result<UninstallReport, AppError> {
    let dirs = HookDirs::resolve(root)?;

    let mut hooks = Vec::new();
    for (name, _) in HOOKS {
        let mut removed = false;
        let mut found_foreign = false;

        for dir in dirs.all() {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            if is_storyhook_hook(&path) {
                fs::remove_file(&path)?;
                removed = true;
            } else {
                found_foreign = true;
            }
        }

        hooks.push((
            *name,
            if removed {
                Swept::Removed
            } else if found_foreign {
                Swept::NotOurs
            } else {
                Swept::NotPresent
            },
        ));
    }
    Ok(UninstallReport { dirs, hooks })
}
