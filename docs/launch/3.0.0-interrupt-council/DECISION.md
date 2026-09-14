# Council Decision

**Question:** What delivery-authority contract should StoryHook 3.0.0 use so a delayed block Interrupt cannot stop a replacement session, including native reset and rapid block/unblock races, while preserving safe interruption and session-bound resumption?

**Decided:** 2026-09-14T05:22:28.298402+00:00

**Method:** unanimous-round-1

## Winning proposal

Use revocable pending authority plus shared workspace exclusion: supersede obsolete block episodes and pending deliveries before every session replacement, and hold inherited ownership from final delivery admission through external-effect quiescence.

src/daemon/block_delivery.rs:145 currently admits every open Interrupt, while plugins/story/bin/story.sh:3612 captures whichever session exists later; StoryCleanupLease at src/domain.rs:524 identifies reusable resources rather than a unique provider lifetime. In src/service/block_delivery.rs:44, atomically supersede pending Interrupts whenever effective blocking ends, including transitions without Resume; admit only the current blocked active episode, and preserve already-started ordered Interrupt/Resume behavior with Resume bound exclusively to an acknowledged target. Acquire the existing cross-process WorkspaceLock before final admission and Pending-to-Attempting CAS, inherit it into the helper as supported by src/service/workspace_lock.rs:67, and require native reset, typed reset, dispatch, adoption, and every other replacement path to invalidate pending authority under that same lock before exposing another session; keep SQLite transactions closed during helper execution. Existing delivery statuses can encode this revocation without changing schema44 or repurposing target, but legacy pending rows lacking provable authority must be explicitly superseded rather than retroactively assigned to a discovered session; Attempting recovery remains Uncertain without replay. Acceptance tests must pause before claim, before helper capture, and before Escape while racing reset/replacement and block/unblock/reblock, cover restored prior awaiting and duplicate holds, prove orphan helpers retain exclusion, and verify unchanged exact PID/start/provider checks, session-bound Resume, lock contention retry, and honest timeout outcomes; these are proposed tests, not executed evidence.

**Risks:** This contract is complete only if every managed session-replacement path shares the lock and revokes pending authority; an uncovered replacement path requires a durable unique session binding rather than relying on reusable lease paths or current blocking alone.

**Confidence:** medium

## Why this won

All three Astra medium seats chose C in round 1. It explicitly retires obsolete pending effects even when no Resume is emitted, preserves acknowledged target semantics, and requires ownership through external quiescence. No retries, abstentions or dissent occurred. The panel ran through Codex native subagent orchestration.

## Dissent

None — unanimous decision.

## Audit trail

- [QUESTION.md](QUESTION.md)
- [PANEL.md](PANEL.md)
- [proposals-round-1.md](proposals-round-1.md)
- [vote-round-1.md](vote-round-1.md)
- [LIVENESS.md](LIVENESS.md)


Liveness and degraded participation: [LIVENESS.md](LIVENESS.md).
