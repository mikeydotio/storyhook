# SH-473 Handoff

## Scope delivered by this branch

- Adds one real Full Auto browser flow to all four Playwright projects. Each
  project invocation owns a dedicated Engine project/story, daemon, seed, and
  fake-tmux state.
- Drives the production dashboard, engine HTTP controller, daemon reconciler,
  ShellDispatcher, and StoryHook helper: start two lanes, observe the claim,
  stop now, observe the durable outcome banner, and acknowledge it.
- Retains mocked browser coverage for deterministic races, stale responses,
  ordering, and ambiguous failures.
- Adds structural contracts for four-project selection, exact real-dispatch
  detection, and per-project fixture isolation.
- Adds the README operator guide, generated typed-epic guidance, and the
  reconciled Full Auto As-built record.
- Adopts the stale structural-epic wording found during this pass. Commit
  `eb8fb06` fixes README/help guidance separately and adds its regression test.

## Durable workflow record

- SH-473 is the system of record. Read its comments before resuming: they hold
  the exact approved plan, every implementation decision, PR link, gate
  diagnostics or receipts, merge evidence, and acceptance assessment.
- This is autonomous work. Do not ask another question after the recorded plan
  approval.
- Do not version, release, deploy, force-push, or run `/story complete` here.
- The approved preservation order is commit, push, open/link the PR, then run
  `make test` and `make e2e ARGS='engine.spec.ts'`.
- A red test, merge conflict, or failed landing is a hard stop: comment full
  diagnostics on SH-473, block it, and leave the PR and worktree intact.
- A green candidate lands through `bash scripts/land-pr.sh PR-NUMBER`.

## Remaining program acceptance

SH-473 does not close when this code lands. Its final acceptance criterion is
a deployed main-branch Full Auto run that autonomously claims, dispatches,
merges, and closes a real story without operator keystrokes. Record the run id,
story id, PR, merge, and final story state in a comment on SH-473.

If that observation is absent, leave SH-473 open and do not reap this lane.
Only after the evidence is recorded may the story move to exactly `done`; then
run the supplied StoryHook reap helper as the absolute last observable action.
