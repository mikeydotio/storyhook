# SH-555 Handoff

## Delivered

- Repaired this repository's legacy persistent verifier worktree under a
  graceful daemon stop; connectivity checks and ordinary fetch now succeed.
- `verify-pr.sh` repairs legacy or unreadable verifier administration before
  fetch, then marks worktrees created under SH-552's private-Git-dir contract.
- Healthy marked worktrees remain persistent and retain build caches.
- A real-Git regression reproduces missing private HEAD/reflog/index objects
  and proves repair, fetch connectivity, and healthy reuse.
- Central verification then exposed a load-sensitive daemon containment defect:
  the parent watcher treated a reusable PID as process identity.
- Commit `97ab33a6d` pairs every Rust test-parent PID with a native start token
  and rejects a live PID whose incarnation does not match. Empty tokens retain
  compatibility for shell-only harnesses.
- The deterministic live-PID/mismatched-token regression is green 20/20; both
  original parent-death tests are green.

## Verification scope

- `bash -n scripts/verify-pr.sh`
- `cargo fmt --all -- --check`
- `cargo clippy --test merge_gate -- -D warnings`
- `cargo test --test merge_gate`
- Repository/generated AGENTS equality regression
- `cargo test --test daemon_parent_identity ... --exact` (20 runs)
- `cargo test --test daemon_lifecycle parent`
- Daemon lifecycle and test-environment module unit tests
- `cargo test --test test_environment`
- Test-support parent-token propagation and scratch-environment rendering
- Targeted Clippy with warnings denied; rustfmt, Bash syntax, and diff checks

## Known compatibility boundary

- Shell-only `storyhook_isolate` callers export an empty start token because a
  portable shell cannot derive the same native high-resolution identity. They
  retain PID-only containment; Rust tests, including the centralized Rust
  battery that returned RED, use the incarnation-safe contract.

## Submission contract

- Branch: `worktree-SH-555`.
- Exactly one open PR references SH-555 and contains SH-555 in its title.
- The centralized verifier owns the complete suite, merge, story completion,
  and worktree cleanup.
