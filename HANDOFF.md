# SH-559 Handoff

## Delivered

- SH-552's private per-worktree Git administration remains the production fix.
- The blocked-gate regression now proves fetch and explicit pull from ordinary
  worktrees, plus GC, fsck, repack, worktree prune, and bisect.
- The test-tier design record distinguishes SH-552 prevention from SH-555
  repair of legacy or stranded verifier administration.
- No production script or public interface changed.

## Verification scope

- `cargo test --test merge_gate`
- `cargo clippy --test merge_gate -- -D warnings`
- `cargo fmt --all -- --check`
- Exact generated-AGENTS scope-rubric contract.

## Submission contract

- Branch: `worktree-SH-559`.
- SH-555 exclusively owns repair of the live legacy verifier registration.
- The one linked PR must remain open for the centralized verifier.
- The verifier owns the full suite, merge, completion, and worktree cleanup.
