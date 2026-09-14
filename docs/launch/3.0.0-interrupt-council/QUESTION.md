# Council Question

**Caller:** SH-718 launch chair

## Question

What delivery-authority contract should StoryHook 3.0.0 use so a delayed block Interrupt cannot stop a replacement session, including native reset and rapid block/unblock races, while preserving safe interruption and session-bound resumption?

## Context

Assembled by chair from working-tree evidence. User authorized autonomous3.0.0 launch fixes, no separate defect stories, and council judgment on hard questions. Read the evidence packet at /tmp/sh718-late-interrupt-evidence.md and relevant source in /tmp/storyhook-v3-release.cV4Sva/repo. The full release diagnostic gate is running against frozen source; no Cargo or browser or live provider tests may run concurrently. Read-only analysis and isolated pure measurements only. The uncommitted integration reconciles main and dev using canonical schema44 with a verified atomic lineage bridge. Native reset preserves local branches/awaiting and holds WorkspaceLock; typed card reset discards its owned lane. Proposed /tmp/sh718-native-block-delivery.patch would route native reserve and finish through Ctx::write_stories; it is held because the worker admits Interrupt for every OPEN story and captures its target only at delivery time. An old reset/block delivery can therefore target a replacement lane. A current-block check by itself leaves the check-to-external-helper interval. Existing BlockDelivery.target means acknowledged interrupted session and Resume binds to it; Attempting is not replayed after daemon restart. No executable new race reproduction has yet run, so distinguish observed facts from proposed tests. Decide the minimum complete correction at the real ownership boundary: consider current-state coalescing, persistent block-episode/session identity, reset/dispatch serialization, legacy pending rows, external-side-effect uncertainty and protocol compatibility. Do not presume a schema migration is required if existing authoritative data suffices, and do not weaken process identity, transactional ownership, or no-external-work-under-SQLite-write-lock invariants. Name concrete acceptance tests and implementation boundaries.
