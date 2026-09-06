# SH-567 Handoff

## Delivered

- Running Full Auto runs expose guarded Pause and Stop controls; paused runs
  expose Resume and Stop; draining runs retain Stop-now escalation.
- The accessible stop modal explains Drain and Stop-now consequences, locks
  duplicate submissions, and restores focus to the logical live control.
- Pending action labels join the engine fingerprint, and ambiguous transport
  failures report uncertainty before reconciling through a fresh status GET.
- The engine specification records the dashboard's as-built lifecycle contract.

## Decisions

- Draining disables the redundant Drain choice but keeps Stop now available as
  an explicit escalation path.
- The stop flow uses the dashboard's shared overlay stack, starts on Cancel,
  and returns focus to Stop or Run Full Auto after state changes.
- Existing global focus-visible styling is sufficient; no SH-567-specific
  focus CSS was added.
- SH-568's durable alert owns the top overlay after a confirmed Stop. The Stop
  confirmation closes and focuses the logical lifecycle control first, so
  acknowledgement returns focus to Stop (draining) or Run Full Auto.

## Evidence

- Branch: `worktree-SH-567`.
- StoryHook version: 2.4.0.
- Server lifecycle endpoints were already complete and remain unchanged.
- The feature commit and PR are linked from SH-567.

## Commits

- `58cc74ac9` — `feat(dashboard): add Full Auto lifecycle controls`
- `f94d1c671` — `docs: update SH-567 handoff and roadmap`

## Reconciliation

- The first reconciliation merged `origin/main` at `a396b3e9a` additively.
- The second reconciliation merges `origin/main` at `64c2b256f` additively;
  published SH-567 history remains unchanged.
- The third reconciliation merges `origin/main` at `2d8dd34d3` additively;
  published SH-567 history remains unchanged.
- Main's verification-incident dashboard and Full Auto specification changes
  merge with SH-567 automatically.
- Main's dispatch-resource persistence changes do not overlap the dashboard
  lifecycle controls.
- Main's SH-568 alert surface and SH-567's lifecycle controls keep independent
  request claims. Each Stop result opens the non-dismissable alert, and the
  combined regression proves durable acknowledgement and focus restoration.
- AGENTS.md and its canonical template keep the SH-567 verifier roadmap in
  sync; this handoff remains scoped to the active story.

## Focused verification

- Third-reconciliation engine and overlay-modality E2E: 29 Chromium, 28 WebKit
  plus 1 documented skip, 23 mobile Chromium, and 22 mobile WebKit plus 1
  documented skip passed (102 passed, 2 skipped total).
- Web template, dashboard error-reporting, focus-coverage, and roadmap-template
  tests: 251 passed.

## Submission boundary

The centralized verifier owns the full suite, merge, completion, and worktree
cleanup after SH-567 moves to `verifying`.
