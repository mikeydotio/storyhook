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

## Submission boundary

Exactly one PR on `worktree-SH-577`, with SH-577 in title and body. Link and
comment it on the story, then make `story move SH-577 verifying` the final
action. Central verification owns full-suite validation, merge, completion,
and cleanup. Repair the existing PR if returned; never rewrite history.
No release, version, deployment, or worktree reaping from this lane.

Preserved prior warning: SH-584 kept a dirty verifier at
`.git/storyhook/verification-recovery-SH-584-20260906`; do not remove it.
