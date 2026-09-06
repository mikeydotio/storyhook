# SH-585 Handoff

## Root cause

The installed-artifact guard classified shell calls by six reader executable
names, so the exact stable Codex launcher with `context` was denied before
execution. SH-584 transferred guard ownership and reproduction evidence here.

## Delivered

- Exact installer-produced launcher identity plus five narrow reader argv
  contracts; no wrapper, compound-command or general Bash exemption.
- Unknown shell safety is distinguished from structured artifact edits.
- Installer-backed classification and real helper/domain immutability tests.

## RCA

- Report: `docs/rca/SH-585-installed-launcher-readers.md`.
- Automated RED on unchanged hook; independent read-only challenge.
- Classification: checking, missing; surgical.

## Focused verification

- Run plugin_install, protect_install_hook, hook_budgets and hook_bounds.
- Run targeted Clippy with warnings denied, rustfmt, Bash/Python syntax and
  diff checks. Exact final results are recorded on SH-585 and its PR.

## Submission boundary

Push `worktree-SH-585`, open and link exactly one PR with SH-585 in its title,
then move SH-585 to `verifying` as the absolute final action. Central
verification owns the full suite, merge, completion and cleanup. Repair a
returned PR with additional commits; never rewrite published history.

No full suite, merge, version, release, or deployment from this worktree.
SH-584 remains the owner of dispatch and plugin freshness work.
