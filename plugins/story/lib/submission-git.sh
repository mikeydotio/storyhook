#!/usr/bin/env bash
# The provider plugin is self-contained; the shared binary owns GitHub policy.
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/github-access.sh"

# Preserve the existing caller shape without allowing arbitrary Git overrides.
submission_git() (
  if [ "${1:-}" = -C ]; then
    cd "$2" || exit 1
    shift 2
  fi
  github_git "$@"
)

# submission_remote_head <worktree> <branch> — exact remote OID, or empty
# when absent. Failed reads retain their diagnostics and cannot mean absent.
submission_remote_head() {
  local worktree="$1" ref="refs/heads/$2" out
  if ! out=$(submission_git -C "$worktree" ls-remote --heads origin "$ref" 2>&1); then
    printf '%s\n' "$out" >&2
    return 1
  fi
  # Git may emit informational stderr on success. Only the requested ref's
  # tab-separated record is evidence, never those diagnostic lines.
  printf '%s\n' "$out" | awk -F '\t' -v ref="$ref" '$2 == ref { print $1 }'
}
