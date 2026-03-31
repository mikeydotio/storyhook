use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::AppError;

const HOOK_MARKER: &str = "# storyhook managed hook -- do not edit this line";

const POST_COMMIT_HOOK: &str = r#"#!/bin/sh
# storyhook managed hook -- do not edit this line
command -v story >/dev/null 2>&1 || exit 0
story commit-sync --since 1h --quiet 2>/dev/null || true
"#;

const POST_MERGE_HOOK: &str = r#"#!/bin/sh
# storyhook managed hook -- do not edit this line
command -v story >/dev/null 2>&1 || exit 0
BRANCH="$(git symbolic-ref --short HEAD 2>/dev/null)"
case "$BRANCH" in main|master) ;; *) exit 0 ;; esac
ORIG_HEAD="$(git rev-parse ORIG_HEAD 2>/dev/null)" || exit 0
git log --format='%s' "$ORIG_HEAD..HEAD" 2>/dev/null | while IFS= read -r msg; do
  echo "$msg" | grep -oiE '(closes?|fixes?|resolves?)\s+[A-Z]+-[0-9]+' | while IFS= read -r match; do
    STORY_ID="$(echo "$match" | grep -oE '[A-Z]+-[0-9]+' | head -1)"
    [ -n "$STORY_ID" ] && story move "$STORY_ID" done "auto-closed by merge" --quiet 2>/dev/null || true
  done
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

pub fn install_hooks(root: &Path) -> Result<String, AppError> {
    let git_dir = root.join(".git");
    if !git_dir.exists() {
        return Err(AppError::Validation(
            "not a git repository (no .git directory)".to_string(),
        ));
    }
    let hooks_dir = git_dir.join("hooks");
    fs::create_dir_all(&hooks_dir)?;

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
    let git_dir = root.join(".git");
    if !git_dir.exists() {
        return Err(AppError::Validation(
            "not a git repository (no .git directory)".to_string(),
        ));
    }
    let hooks_dir = git_dir.join("hooks");

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
