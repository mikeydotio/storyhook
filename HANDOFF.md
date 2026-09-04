# SH-473 Handoff

## Delivered

- Full Auto's real-daemon browser flow runs in all four Playwright projects;
  operator, scaffold, test-tier, and As-built guidance is complete.
- A deployed beta run autonomously claimed, dispatched, merged, and completed
  SH-530. The full acceptance record is in SH-473's comments.
- The adopted cleanup RCA is implemented on Draft PR #615. It persists an exact
  creation-time cleanup lease, requires a typed postcondition receipt, routes
  worktree-local reap through its private marker, and rejects contradictory
  helper exit/JSON success across dispatch, unclaim, capabilities, notify, and
  centralized reap.
- The durable postmortem is
  `docs/rca/reap-reports-cleanup-success-after.md`.
- Larger non-verification cleanup ownership work is intentionally separate in
  high-priority bug SH-539.

## Current preserved state

- Branch: `worktree-SH-473`; Draft PR: #615.
- Last focused green commit at this handoff: `54742da`.
- Focused results: dispatcher 7/7, capabilities 7/7, verification queue 29/29,
  and the strengthened real checkout-switch helper regression passed.
- Build/test artifacts were removed after each run.

## Remaining sequence if interrupted

1. Push the documentation close-out commit while PR #615 remains Draft.
2. From a clean artifact state, run full `make test`; clean artifacts again.
3. Record the exact green head/tree on SH-473, mark PR #615 ready, and merge it
   with `bash scripts/land-pr.sh 615`.
4. In a standalone `/private/tmp` clone of landed `main`, use the semver
   workflow to set `2.2.1-beta.4`. Push/open a Draft release PR, run the full
   clean suite, mark ready, land it, publish the GitHub prerelease, and install
   and verify the released CLI/plugin. Do not version in this linked worktree.
5. Record every merge, release, install, and acceptance result on SH-473. Move
   it to exactly `done` only when all are complete.
6. Run the user-supplied pinned reap command as the absolute last action. Never
   run `/story complete`, and never reap an incomplete story.

On any red test, diagnose and fix the root cause without weakening coverage;
push the corrected Draft head before rerunning its batteries.
