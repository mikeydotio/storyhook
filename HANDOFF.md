# SH-539 Handoff

## Delivered

- Dispatch protocol 4 returns its versioned creation-time cleanup lease.
- Migration 29 persists that lease on each engine lane across daemon restart,
  after main's pane-identity and quarantine-history migrations 27 and 28.
- Stop-now uses only the persisted lease and requires an echoed exact-window
  absence receipt; legacy unleased lanes remain occupied with a loud error.
- `complete`, `unclaim`, and `reset` now return nonzero `ok:false` when attempted
  tmux or Git cleanup cannot prove its postcondition.
- Failed dispatch rollback reports surviving markers, worktrees, and branches.

## Verification scope

- Rust: engine run model, restart/reconcile, dispatcher, daemon/dispatch API,
  plugin contract, schema migration/fixture.
- Shell: dispatch lease output, dispatch rollback survivor, complete, unclaim,
  and reset cleanup failures.

## Submission contract

- Branch: `worktree-SH-539`.
- Current `origin/main` is merged additively; published history is unchanged.
- The one linked PR must remain open for the centralized verifier.
- The verifier owns the full suite, merge, completion, and worktree cleanup.
