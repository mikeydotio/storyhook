# SH-587 Handoff

## Defect and origin

- SH-577 was returned for PR reconciliation twice. Each conflict moved it to
  `in-progress`, released verifier ownership, and immediately drained another
  story. New merges then invalidated SH-577 again.
- `src/daemon/verification.rs` classified `VerificationOutcome::Conflict`
  through the same return-and-release path used by red tests and invalid
  submissions. The serialized worker had no reservation state.

## Repair

- A conflict still records its durable diagnosis, moves the story to
  `in-progress`, and notifies the exact agent.
- The verifier now retains activity and shutdown-drain guards while an
  event-driven waiter watches for that same project/story to create a newer
  `verifying` generation.
- Queue arrivals, priority changes, heartbeats, and unrelated project events
  cannot transfer the reservation. Resubmission updates the owned generation
  and immediately retries the same story.
- Repeated conflicts preserve the reservation. Merge, red, invalid-submission,
  and infrastructure behavior remain unchanged.
- A notification failure keeps the existing `awaiting` diagnosis and releases
  ownership; an unreachable agent cannot deadlock the machine-wide queue.
- Orderly daemon shutdown wakes and cancels the process-local wait cleanly.
  No persistent lease, schema migration, CLI command, or REST change was added.

## Validation

- RED: the new reservation integration test initially failed to compile because
  no reconciliation-aware verification cycle existed.
- GREEN: `verification_queue` passed 46/46 and
  `verification_shutdown_drain` passed 3/3 with local daemon access.
- The first sandboxed run passed 45/46; its existing real-helper reap case was
  denied local daemon access (`Operation not permitted`). The identical
  escalated run passed.
- Scaffold and scope-rubric contracts passed 24/24. Targeted Clippy passed with
  warnings denied. Formatting, timing-policy, and diff checks passed.
- The full suite is intentionally verifier-owned.

## Submission boundary

Open one PR from `worktree-SH-587`, with SH-587 in title and body. Link and
comment it on SH-587, then make `story move SH-587 verifying` the final action.
If returned, repair that PR with additive history. Do not merge, reap, version,
deploy, release, or run the full suite from this worktree.

Preserved prior warning: SH-584 kept a dirty verifier at
`.git/storyhook/verification-recovery-SH-584-20260906`; do not remove it.
