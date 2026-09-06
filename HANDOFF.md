# SH-571 Handoff

## Delivered

- Auto and Full Auto Codex dispatch require a protocol-2 SessionStart sentinel
  from the exact plugin root that owns the helper before prompt delivery.
- Missing hooks, legacy identity-free sentinels, and mismatched plugin roots
  refuse with diagnostics and roll back the claim and Git resources.
- Attended Codex remains screen-gated. Claude's sentinel contract is unchanged.
- SessionStart records its canonical hook root; direct CLI calls remain valid
  with a null `plugin_root`.

## Evidence

- Pre-fix live Codex 0.153.2 probe, with no plugins, accepted the charter and
  stalled at a Plan-mode `request_user_input` question.
- Post-fix live probe reached the normal idle Codex TUI, refused with
  `no-sentinel`, delivered no prompt, and restored LIVE-1 to `todo`.
- New regression covers absent, legacy, mismatched, and exact hook identity for
  Auto and Full Auto, plus unchanged attended behavior.
- Focused Rust hook/service and shell dispatch/provider/autonomy suites pass.

## Commits

- `3b49a86b6` — `fix(dispatch): bind autonomous Codex hooks`
- `ceffa4786` — `docs(dispatch): record Codex hook binding`

## Reconciliation

- Existing PR #654 is additively merged with `origin/main` at `ebedde948`;
  published SH-571 history is unchanged.
- One `STORY_PLUGIN_ROOT` now owns Claude's explicit plugin activation and
  Codex's exact sentinel comparison.
- AGENTS.md and its canonical template make SH-571 the current verifier item.
- Main's launch-template fixture now emits the exact Codex identity sentinel.
- Original SH-571 tests, the roadmap contract, and four intersecting SH-564
  dispatch/path regressions pass on the merged tree.

## Submission boundary

Push the additive merge commit to existing PR #654, comment final evidence,
then move SH-571 to `verifying` as the absolute last action. Central
verification owns the full suite, merge, completion, and worktree cleanup.
