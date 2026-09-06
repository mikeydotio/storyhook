# SH-490 Handoff

## Delivered

- Fresh named dispatches record their intended tmux window in the atomic claim.
- NEXT, forced, and resumed dispatches record tmux window, worktree, and branch
  after confirmed handoff.
- Dispatch rollbacks retain `story unclaim`'s transactional correction comment.
- A failed post-handoff audit write preserves the live claim and resources and
  returns `dispatch-comment-failed` with their identities.

## Reconciliation

- The published branch is additively merged with `origin/main` at `96417b9a3`;
  SH-490 history remains unchanged.
- A second additive reconciliation merges `origin/main` at `29c238e30` after
  SH-573 and SH-506 landed; published history remains unchanged.
- Main's autonomous Codex dispatch changes auto-merge with SH-490's resource
  comments, including their shared story helper and tmux fake.
- AGENTS.md and its canonical template make SH-490 the current verifier item
  while retaining main's forward roadmap.

## Focused verification

- All 11 SH-490 dispatch/unclaim/reset shell files pass on the latest tree.
- Four intersecting Codex dispatch/provider shell files also pass.
- Plugin-skill determinism and scope/scaffold contracts pass: 16 Rust tests.
- Shell syntax, Rust formatting, and staged/unstaged whitespace checks pass.

## Submission boundary

Push one additive merge commit to existing PR #660, comment final evidence,
then move SH-490 to `verifying` as the absolute last action. Central
verification owns the full suite, merge, completion, and worktree cleanup.
