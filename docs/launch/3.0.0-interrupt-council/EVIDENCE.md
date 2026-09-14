# SH-718: Native reset exposes a late Interrupt race

## Finding

A queued block Interrupt can reach a replacement agent after the block-producing reset has completed. The current worker admits Interrupt for any OPEN story. Its target is captured at delivery time, not at enqueue time. Existing helper identity checks prevent replacement after that capture, but cannot distinguish the old blocked lane from a replacement that started before capture.

This is a code-confirmed gap. No new executable reproduction has run; the full gate still owns frozen source and Cargo. Do not treat the proposed native write_stories conversion as complete until this interaction is resolved.

## Actual paths

| Stage | Current implementation | Relevant guarantee or gap |
| --- | --- | --- |
| Producer | src/service/block_delivery.rs:44, Ctx::write_stories | Before/after project snapshots derive Interrupt on unblocked→blocked. enqueue_block_delivery receives only project, story and action. |
| Proposed native producer | src/service/reset.rs:149 and :244; /tmp/sh718-native-block-delivery.patch | Reservation appends StoryAwaitingSet("Reset pending..."); finish restores prior awaiting and state todo. The patch routes these two transactions through write_stories. |
| Persistence | src/store/schema/0038_block_deliveries.sql; src/store/block_delivery.rs:57; src/store/sqlite/block_delivery.rs | Records contain id, project, story, action, status, target and detail. target starts NULL. There is no enqueue-time story revision, lease generation, pane identity or reset operation token. |
| Consumer admission | src/daemon/block_delivery.rs:145–152 | `s.superstate == Open && (action == Interrupt || state == in-progress && !is_blocked(...))`. Interrupt does not revalidate effective blocking. |
| Consumer claim | src/daemon/block_delivery.rs:203 | Pending→Attempting is a status CAS. It prevents duplicate claiming, not generation changes. The database transaction ends before the external helper runs. |
| External command | src/daemon/block_delivery.rs:244–254 | Interrupt sends only `notify <id> --interrupt`. Resume sends an acknowledged target with `--expected-target`. No enqueue-time lease is supplied. |
| Helper resource boundary | plugins/story/bin/story.sh:3571–3624, cmd_notify | Resolves current story resources and current provider process. Supports optional STORYHOOK_NOTIFY_LEASE_V1, but this worker does not set it. The grammar explicitly disallows --expected-target with --interrupt. |
| Helper lifetime checks | plugins/story/lib/interrupt-agent.py, target() and interrupt() | Captures socket, pane, PID, process start and provider at delivery time. Revalidates before native Escape and owned gate cleanup. This protects the captured lifetime only. |
| Acknowledgement | src/daemon/block_delivery.rs:276 onward | Successful Interrupt stores the helper's target after the effect. Resume binds to the preceding acknowledged Interrupt. |
| Verifier revision guard | src/service/verification.rs:1034, block delivery revision lookup | Verifier candidates use durable block history to withdraw their own attempt. This does not fence terminal Interrupt delivery. |

## Deterministic reproduction sequence to implement

1. Use an isolated ServiceFixture with a temporary Git repository and an in-progress ordinary story. Keep the daemon delivery worker paused.
2. Run native reset using the proposed write_stories patch. Reservation adds a pending Interrupt; successful finish returns the story to todo and clears the native owner reservation.
3. Start or represent a newly dispatched replacement agent for that story. The story can now be in-progress again and unblocked.
4. Invoke process_one with a recording provider script. It should refuse/supersede the old effect without calling the helper for the replacement. Current code instead calls the script because the story remains OPEN.
5. A second barrier-based test must cover Attempting claimed before completion, with helper resource capture delayed until after reset completion/replacement. A check only at Pending claim time does not close that interval.
6. Preserve ordinary rapid block→unblock semantics, prior awaiting, duplicate holds, acknowledged resume targeting, and genuine still-current blocked delivery.

The simpler sequence omitting replacement dispatch is already sufficient to prove a stale Interrupt is attempted for completed reset's unblocked todo state. The replacement establishes the user-visible consequence.

## Schema and compatibility constraints

The merge has canonical schema 44. The launch bridge preserves main's older native reset journal in story_reset_reservations and dev's typed card journal in story_resets; it also preserves older main landing reservations. Do not rewrite historical migrations or erase pending operational rows. New persistent delivery identity would need an additive migration, a policy for unbound legacy pending rows, corresponding reader/writer/export support, and lineage regressions. target currently means acknowledged interrupted session and is later consumed by Resume; repurposing it at enqueue would need an explicit contract review.

Protocol 5 currently advertises native interrupt and session-bound resume. Extending interrupt argv must keep old-helper refusal honest and update protocol/grammar/provider tests where required. The native reset operation holds WorkspaceLock through completion, then drops it before transition hooks. Block delivery does not share that lock.

## Minimum choices for council

| Choice | Benefit | Remaining requirement |
| --- | --- | --- |
| Revalidate current effective blocking | Small consumer correction; stale unblocked todo deliveries can be superseded. | Alone insufficient: check-to-helper race remains, and a replacement can itself be blocked. It may alter intentional rapid block→unblock delivery semantics. |
| Bind delivery to enqueue-time revision and lease/session lifetime | Distinguishes the original operational target and supports refusal after replacement, even if current state is blocked. | Needs a trustworthy target capture boundary, atomic persistence, external effect revalidation, and an explicit legacy-row policy. Revision alone may reject legitimate comments and is not a lifetime identity. |
| Reset-specific supersession and attempt coordination | Could bound the immediate native-reset integration without a general delivery redesign. | Pending effects must be superseded atomically at reset completion; already Attempting effects must be fenced/quiesced before replacement dispatch becomes possible. Merely editing status after effect start is insufficient. |

## Uncertainty and scope

No evidence currently binds pending Interrupt to an enqueue-time target. No claim is made that resetting the story is the only trigger; any delayed block followed by resource replacement has the same shape. The generic producer existed on dev before this merge; routing main's native reset through it exposes another trigger. Existing engine/landing/card/native owner guards remain valuable but do not cover a terminal delivery after owner release. The launch task requires a decision and regression-backed repair, not a separate story.
