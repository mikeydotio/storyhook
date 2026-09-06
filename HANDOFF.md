# SH-585 Handoff

## Current work

- PR #669 on `worktree-SH-585`: exact installer-produced Codex launcher plus
  five narrow reader argv contracts; no general Bash or compound exemption.
- Real launcher/helper/CLI tests preserve story data, global events and
  installed artifact bytes. Unsupported shell safety has an honest diagnosis.
- Verifier repairs synchronize the AGENTS.md source template and use
  `TestEnv::raw_story` for the older reinstall fixture. Actual environment
  assertions cover containment, isolated paths, provider PATH and cwd.
- RCA: `docs/rca/SH-585-installed-launcher-readers.md`.

## Reconciled main

SH-584 merged as PR #668. Its changes are preserved:

- Daemon dispatch and engine monitoring exclude ambient tmux identity while
  interactive and explicit cleanup-lease addressing remain intact.
- Installer verifies the enabled plugin's complete release payload; provider
  fixtures now materialize that payload. Capabilities track helper content.
- Verifier preparation/restoration preserve tracked edits and speculative
  refs; remediation retains tmux diagnostics and provider-specific keys.
- Context: `docs/rca/web-dispatch-tmux-and-stale-catalog.md` and
  `docs/rca/verifier-remediation-context.md`. SH-583 was absorbed into SH-584.
- SH-584 preserved a dirty verifier at
  `.git/storyhook/verification-recovery-SH-584-20260906`; do not remove it.

## Reconciliation validation

- Keep main's provider payload fixtures with SH-585's shared command builder
  and containment regression. Keep generated AGENTS.md equal to its template.
- Run installer/freshness/reinstall, launcher/guard, containment and scaffold
  contracts; targeted Clippy, formatting and diff checks. Final results are
  recorded on SH-585 and PR #669.

## Submission boundary

Merge current origin/main into the existing branch, preserving published
history. Push to PR #669; no new PR, rebase or force-push. Record final context,
then make `story move SH-585 verifying` the absolute final action.

Central verification owns the full suite, merge, completion and cleanup.
No full-suite, release, version or deployment operation from this worktree.
SH-557 remains responsible for separating roadmap data from the template.
