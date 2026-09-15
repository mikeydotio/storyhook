#!/usr/bin/env bash
# submission_git <git-args...> — submission's network Git boundary.
# Git does not read GH_TOKEN itself. Reset only GitHub's inherited helper
# chain so an interactive keychain cannot preempt the credentials gh already
# uses for the PR. Command-local settings never alter the operator's config.
# Keep the credential in the helper protocol, never in argv or a remote URL.
submission_git() {
  # Askpass runs before the terminal path, even with terminal prompts off.
  GIT_TERMINAL_PROMPT=0 GH_PROMPT_DISABLED=1 GIT_ASKPASS='' SSH_ASKPASS='' git \
    -c core.askPass= \
    -c 'credential.https://github.com.helper=' \
    -c 'credential.https://github.com.helper=!gh auth git-credential' \
    -c 'url.https://github.com/.insteadOf=git@github.com:' \
    -c 'url.https://github.com/.insteadOf=ssh://git@github.com/' \
    "$@"
}

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
