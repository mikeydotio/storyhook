# SH-577 Handoff

## Diagnosis and repair

- The three reported toggle failures reproduced at baseline 158d4ddcf:
  24 passed / 3 failed across the selected Chromium specs (2.3m).
- All three manual gestures aimed below a 720px viewport: drawer control
  and held-/data case y=727.390625; description-save case y=742.390625.
  A settled bounding box did not establish an actionable click point.
- `settledBoundingBox` now scrolls without focusing or activating, waits for
  surface animations, and validates the exact returned centre hit target.
  Failure diagnostics identify the receiver, box, centre, and viewport.
- New real-dashboard regressions cover an off-scrollport Comments toggle
  with an active description edit, repeated visible presses, and obstruction
  by the real Delete modal. Before the fix: 1 passed / 2 failed, as expected.
- No production toggle or press-gate changes. SH-584 already repaired the
  dispatch failure; all baseline dispatch tests passed, including real Astra.
- AGENTS.md and its source template remain synchronized. SH-557 owns their
  eventual separation; the required roadmap update is not that redesign.

## Evidence and validation

- Local logs: `/tmp/SH-577-baseline-browser.log` and
  `/tmp/SH-577-regression-red.log`; corresponding preserved artifacts:
  `/tmp/SH-577-baseline-artifacts`, `/tmp/SH-577-regression-red-artifacts`.
- Green: new helper, description-edit-mode, drawer-body-click-race, dispatch,
  and card-reposition-click-race specs: 26 Chromium / 26 WebKit cases.
- Original three toggle witnesses repeated three times per engine: 9 Chromium
  / 9 WebKit cases. All 70 browser executions passed with load grace intact.
- Scaffold and scope-rubric contracts: 24 passed, including byte-for-byte
  AGENTS.md/template fidelity. Formatting and diff checks passed.
- Logs: `/tmp/SH-577-green-{chromium,webkit}.log`,
  `/tmp/SH-577-repeat-{chromium,webkit}.log`, `/tmp/SH-577-scaffold.log`.
  Load samples, final commit and PR are recorded on SH-577.
- No full suite was run from this worktree; central verification owns it.

## Reconciled main

- Merged origin/main d4b060e6e (SH-581, PR #670) without rewriting history.
- Preserve its Lima guest fix: Cargo runs in a subshell inside extracted source;
  the caller cwd remains available for export. Both Linux targets are covered
  by the archived-source/cwd, locked arguments, rustc/linker and export regression.
- SH-581 recorded RED before the fix and 59 passing release_targets/scaffold/
  scope_rubric tests afterward. No real Lima build was run; toolchain/Cargo
  shims exercise the real guest script. Its story retains the detailed evidence.
- The three merge conflicts were roadmap/template text and this handoff.
  SH-577 browser changes and SH-581 release code/tests remain unchanged.
- Reconciliation: 59 Rust contracts and 26 browser cases per desktop engine
  passed. Formatting, shell syntax and diff checks passed. Logs are under
  /tmp/SH-577-reconcile-*.log; final details are on SH-577 and PR #672.
- Run same-checkout Rust builds and browser tests sequentially: both use
  target/debug/story, and executable mtime changes invalidate a running daemon.
  The first overlapping run reported dispatch PID overlap, then connection
  refusal; its evidence is in /tmp/SH-577-reconcile-failed-artifacts. The PID
  overlap's cause is unproven. A fresh sequential Chromium run passed 26/26.

## Submission boundary

Exactly one PR on `worktree-SH-577`, with SH-577 in title and body. Link and
comment it on the story, then make `story move SH-577 verifying` the final
action. Central verification owns full-suite validation, merge, completion,
and cleanup. Repair the existing PR if returned; never rewrite history.
No release, version, deployment, or worktree reaping from this lane.

Preserved prior warning: SH-584 kept a dirty verifier at
`.git/storyhook/verification-recovery-SH-584-20260906`; do not remove it.
