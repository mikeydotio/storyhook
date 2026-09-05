# SH-555 Handoff

## Delivered

- Repaired this repository's legacy persistent verifier worktree under a
  graceful daemon stop; connectivity checks and ordinary fetch now succeed.
- `verify-pr.sh` repairs legacy or unreadable verifier administration before
  fetch, then marks worktrees created under SH-552's private-Git-dir contract.
- Healthy marked worktrees remain persistent and retain build caches.
- A real-Git regression reproduces missing private HEAD/reflog/index objects
  and proves repair, fetch connectivity, and healthy reuse.

## Verification scope

- `bash -n scripts/verify-pr.sh`
- `cargo fmt --all -- --check`
- `cargo clippy --test merge_gate -- -D warnings`
- `cargo test --test merge_gate`
- Repository/generated AGENTS equality regression

## Submission contract

- Branch: `worktree-SH-555`.
- Exactly one open PR references SH-555 and contains SH-555 in its title.
- The centralized verifier owns the complete suite, merge, story completion,
  and worktree cleanup.
