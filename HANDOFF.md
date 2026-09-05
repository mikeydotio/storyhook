# SH-490 Handoff

## Delivered

- Fresh named dispatches record their intended tmux window in the atomic claim.
- NEXT, forced, and resumed dispatches record tmux window, worktree, and branch
  after confirmed handoff.
- Dispatch rollbacks retain `story unclaim`'s transactional correction comment.
- A failed post-handoff audit write preserves the live claim and resources and
  returns `dispatch-comment-failed` with their identities.

## Focused verification

- Dispatch comment failure, ID mode, NEXT mode/refusals, target/create session,
  active-role, actor-label, resume, unclaim, and reset shell tests pass.
- `bash -n` and `git diff --check` pass.

## Submission boundary

The open SH-490 pull request is the only submission. The centralized verifier
owns the full suite, merge, completion, and worktree cleanup.
