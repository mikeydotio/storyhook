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

/// The hook directory storyhook owns, resolved by **asking git** rather than by
/// assuming a layout.
///
/// `<root>/.git/hooks` was wrong in two ways, and this type exists to stop
/// both. In a *linked* worktree `.git` is a file holding a `gitdir:` pointer,
/// not a directory, so joining `hooks` onto it could not be created at all
/// (SH-314) — and a worktree is where this project does most of its work.
/// Worse, the obvious repair is also wrong: git resolves hooks for **every**
/// worktree in the *common* directory, so an implementation that wrote into
/// the worktree's private git directory would install hooks git never runs,
/// which is the silent class SH-313 is about, one directory over.
///
/// Resolution is two steps, and both are load-bearing:
///
/// 1. `--show-toplevel` names the directory git runs hooks from. Asking for it
///    first makes step 2 independent of the caller's working directory — which
///    matters because since SH-114 this runs *in the daemon*, against the
///    envelope's root rather than any shell's cwd. It also refuses a bare
///    repository for free (`fatal: this operation must be run in a work tree`),
///    preserving the refusal the hand-built path gave by accident.
/// 2. `--git-common-dir` is asked **from that top-level**. It answers
///    *relatively* in an ordinary checkout (`.git`) and *absolutely* in a linked
///    worktree, so joining it onto the top-level is not defensive coding — it is
///    the documented shape of the answer, and `Path::join` discards the
///    left-hand side when the right is absolute, which is exactly the behaviour
///    both cases need.
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
    /// directory storyhook writes a managed hook into.
    managed: PathBuf,
}

impl HookDirs {
    /// Resolves the hook directories for the repository containing `cwd`.
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] when `cwd` is not inside a git working tree, or
    /// when `git` cannot be run to ask. The message keeps the phrase
    /// `not a git repository`, which is the contract two test files pin.
    fn resolve(cwd: &Path) -> Result<Self, AppError> {
        let toplevel = Self::ask(cwd, "--show-toplevel")?;
        let common = Self::ask(&toplevel, "--git-common-dir")?;
        Ok(Self {
            managed: toplevel.join(common).join("hooks"),
        })
    }

    /// One `git rev-parse <flag>`, refusing an empty answer as loudly as a
    /// failed one — a blank line here would otherwise become the current
    /// directory and send three executables somewhere nobody asked for.
    fn ask(cwd: &Path, flag: &str) -> Result<PathBuf, AppError> {
        git_env::output(cwd, &["rev-parse", flag])
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                AppError::Validation(format!(
                    "not a git repository: `git rev-parse {flag}` had no answer for {}",
                    cwd.display()
                ))
            })
    }

    /// The directory storyhook owns and writes into.
    fn managed(&self) -> &Path {
        &self.managed
    }
}

pub fn install_hooks(root: &Path) -> Result<String, AppError> {
    let dirs = HookDirs::resolve(root)?;
    let hooks_dir = dirs.managed();
    fs::create_dir_all(hooks_dir)?;

    let mut results = Vec::new();
    for (name, content) in HOOKS {
        let hook_path = hooks_dir.join(name);
        if hook_path.exists() && !is_storyhook_hook(&hook_path) {
            results.push(format!("  {name} — skipped (existing user hook)"));
            continue;
        }
        fs::write(&hook_path, content)?;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755))?;
        results.push(format!("  {name} — installed"));
    }
    Ok(format!("Git hooks:\n{}", results.join("\n")))
}

pub fn uninstall_hooks(root: &Path) -> Result<String, AppError> {
    let dirs = HookDirs::resolve(root)?;
    let hooks_dir = dirs.managed();

    let mut results = Vec::new();
    for (name, _) in HOOKS {
        let hook_path = hooks_dir.join(name);
        if !hook_path.exists() {
            results.push(format!("  {name} — not present"));
            continue;
        }
        if !is_storyhook_hook(&hook_path) {
            results.push(format!("  {name} — skipped (not a storyhook hook)"));
            continue;
        }
        fs::remove_file(&hook_path)?;
        results.push(format!("  {name} — removed"));
    }
    Ok(format!("Git hooks:\n{}", results.join("\n")))
}
