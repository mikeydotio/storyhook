use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::env::git_env;
use crate::error::AppError;

const HOOK_MARKER: &str = "# storyhook managed hook -- do not edit this line";

const POST_COMMIT_HOOK: &str = r#"#!/bin/sh
# storyhook managed hook -- do not edit this line
command -v story >/dev/null 2>&1 || exit 0
story commit-sync --since 1h --quiet 2>/dev/null || true
"#;

/// Auto-closes stories a merge says it closes.
///
/// `%B`, not `%s` (SH-56). The subject line is reserved for a summary under 72
/// characters by every Conventional Commits guide there is, so a `Closes SH-12`
/// reference lives in the body as a trailer — and this hook read subjects, so
/// following the convention guaranteed it never fired. It had plausibly never
/// worked for a body trailer at all.
///
/// The outer `while read` loop went with the change and did not need replacing:
/// `%B` emits multi-line records, so a per-line loop no longer receives one
/// commit per iteration, and it never had to — its only job was feeding text to
/// `grep -oiE`, which is line-oriented already and emits one match per line.
/// Piping the log straight into `grep` is both simpler and correct.
///
/// `[[:space:]]` rather than `\s`: `\s` is a GNU extension, this runs under
/// `/bin/sh` on whatever the user has, and a hook that silently matches nothing
/// on BSD grep is exactly the failure mode SH-56 already was.
///
/// `tests/hook_execution.rs` runs this script — the real file, installed into a
/// real repository, over a real merge. The defect class here is "hook logic
/// shipped as an untested string literal", and a `&str` nothing executes is how
/// it survived.
const POST_MERGE_HOOK: &str = r#"#!/bin/sh
# storyhook managed hook -- do not edit this line
command -v story >/dev/null 2>&1 || exit 0
BRANCH="$(git symbolic-ref --short HEAD 2>/dev/null)"
case "$BRANCH" in main|master) ;; *) exit 0 ;; esac
ORIG_HEAD="$(git rev-parse ORIG_HEAD 2>/dev/null)" || exit 0
git log --format='%B' "$ORIG_HEAD..HEAD" 2>/dev/null |
  grep -oiE '(closes?|fixes?|resolves?)[[:space:]]+[A-Z]+-[0-9]+' |
  while IFS= read -r match; do
    STORY_ID="$(echo "$match" | grep -oE '[A-Z]+-[0-9]+' | head -1)"
    [ -n "$STORY_ID" ] && story move "$STORY_ID" done "auto-closed by merge" --quiet 2>/dev/null || true
  done
"#;

const PREPARE_COMMIT_MSG_HOOK: &str = r#"#!/bin/sh
# storyhook managed hook -- do not edit this line
command -v story >/dev/null 2>&1 || exit 0
case "$2" in message|merge|squash) exit 0 ;; esac
NEXT="$(story next --count 1 --json 2>/dev/null)" || exit 0
STORY_ID="$(echo "$NEXT" | grep -o '"id": *"[^"]*"' | head -1 | cut -d'"' -f4)"
[ -n "$STORY_ID" ] && {
  TITLE="$(echo "$NEXT" | grep -o '"title": *"[^"]*"' | head -1 | cut -d'"' -f4)"
  printf '\n# Top story: %s — %s\n' "$STORY_ID" "$TITLE" >> "$1"
}
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
