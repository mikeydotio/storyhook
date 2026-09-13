# Verifier observability — SH-703

## Problem

In v2.4.2, CLI acknowledgement cleared an infrastructure incident while
preserving disabled admission, then promised a retry. A stopped queue was
visible in story comments and the dashboard but absent from ordinary CLI context.

## Controls and evidence

| Command | Effect |
|---|---|
| `story verifier status` | Project admission, incident, ownership, queue, and recovery evidence |
| `story verifier start` | Enable admission; an infrastructure halt still requires acknowledgement |
| `story verifier stop` | Disable admission and cancel the owned attempt |
| `story verifier drain` | Disable admission and finish the owned attempt |
| `story verifier ack ID` | Clear the exact halted incident and enable admission atomically |
| `story verifier ack ID --leave-stopped` | Clear the exact incident and disable admission |

CLI/RPC and REST share the daemon's `VerificationActivity`. Store-only callers
cannot invent an empty ownership registry for runtime commands. The existing REST
acknowledgement with omitted action retains its admission-preserving meaning.

Schema migration 39 stores one typed recovery receipt per project. It retains the
latest acknowledgement separately from the latest request, including explicit
Retry, LeaveStopped, or legacy PreserveAdmission intent and resulting permission. A request is
`scheduled`, `admitted`, or `settled` with a reason and diagnosis. Actual admission
identity remains recorded after settlement. This is operational evidence, not
certification or authority to restore a checkout, merge, or complete a story.

A control commits before publishing the existing project wake event. The normal
worker admits work; no HTTP request executes a gate. Durable pending intent is
checked on startup and the existing recovery cadence. A tick settles the request
ID captured atomically with its admission; a tick that admits nothing may settle
only the pending ID it observed before starting. An idle tick writes and publishes
nothing. Startup marks prior admissions interrupted and preserves scheduled
requests; live ownership remains strictly process-local.

Command responses carry the receipt captured by their own transaction alongside
current status. A subsequent command cannot change which request the first caller
caused. `scheduled` never claims a gate started. Admission names an independently
generated attempt UUID, actual story, generation, and acquisition time.

## Read surfaces

The shared snapshot reports admission independently from the current incident,
its age, first-hit story, failure text, attempts/retries, held stories, actual
owner, last-evidence timestamp and age, latest acknowledgement, and recovery request.

`load-context`, `next`, `summary`, and `engine status` add a single project warning
when unhealthy. Existing JSON result keys and ready-work selection are preserved;
verifier data and warnings are additive. `lane-budget` preserves its local tmux
census and reads verifier notices from an already-running daemon. It never starts
a daemon; missing or incompatible runtime status is explicitly unavailable.

Silence exceeding `PUBLISH_INTERVAL` is overdue. Active work uses the modification
time of a generation-matching journal, including case-only appends. A missing
journal falls back to ownership acquisition. An unowned queue uses its oldest
verification entry; an otherwise empty pending request uses its request time.
Waiting behind a progressing owner is ordinary queueing. Unreadable or mismatched
evidence is unavailable, not healthy. Timestamps remain UTC on the wire and use
the existing local-time helpers in client-rendered prose.

## Notifications and dashboard

`on_verification_halted` carries held stories, the exact acknowledgement command,
and diagnostic commands. `on_verification_resumed` reports cleared incident,
project, first-hit story, resulting admission permission, reason, and recovery
receipt. Clearing a halt while leaving admission stopped is explicitly reported.
Hooks execute after commit and outside store/ownership locks. Existing hook
execution and failure reporting remain in use; no notification service is added.

The dashboard retains its halt controls and adds stopped, draining, stopping,
overdue, and scheduled-recovery banners. A halt-cleared notice lasts until the
next observed verifier transition. Responses remain bound to their originating
project, so a delayed acknowledgement cannot affect another project's board.

## Validation and remaining scope

Regression coverage exercises admission parity, durable receipts, immediate
worker wake, exact attempt correlation, stale-request settlement, output shapes,
freshness boundaries, hook delivery, and dashboard visibility. Fixtures use
isolated stores and production control/worker paths with external gate doubles.

**SH-703 remains open for an adopted defect:** the existing
`on_verification_halted` event is absent from `timeout_ceiling_violation`'s override
roster. The new resumed event is registered correctly. A continuation must add
halted-event validation in its own behavior-fix commit with a regression through
the production loader: a `timeout_seconds` exceeding
`HOOK_TIMEOUT_CEILING_SECS` must be rejected. This additional repair was deferred
under the session's explicit context-limit rule; it must not be mistaken for
completed scope.


### Implementation validation (2026-09-12)

| Direct check | Result |
|---|---|
| New observability / existing control / wire envelope | 12 / 13 / 15 passed |
| CLI grammar / store migrations / lane budget | 43 / 64 / 8 passed |
| Golden CLI / CLI help / dashboard local time | 30 / 4 / 4 passed |
| Queue incident / halt / ownership / dashboard cases | 4 / 2 / 4 / 1 passed |
| Verifier units / hook ceiling units | 22 / 8 passed |
| Chromium / WebKit control, incident and layout specs | 11 / 11 passed |
| Mobile Chromium / mobile WebKit layout specs | 2 / 2 passed |
| Focused Clippy with warnings denied, formatting, whitespace | Passed |

The six golden snapshot changes contain only additive verifier data; removing
those additions reproduces their original content. Random fixture project identity
is narrowly normalized. New receipts distinguish explicit Retry, LeaveStopped,
and legacy PreserveAdmission. Freshness assertions use recorded journal metadata,
including ownership that differs from queue rank and an unreadable-journal case.

The staged-tree impact selector returned `ALL` because certified baseline tree
`7d8c74f5d3b2de80fe6e44c1db2e89ff4dd98247` has no coverage map. Only new and directly
impacted checks ran here; the full suite remains central-verifier-owned. Broad
queue tests with known fixture containment/reap defects were not selected; SH-699
and SH-702 already track those issues. This is no claim that their checks pass.

Continuation: repeat obviation review, reproduce the missing halted-event timeout
validation through `load_hooks_config_result` (a halted-hook override of 61 seconds
currently passes configuration loading although the ceiling is 60), repair that
roster entry in its own commit with its regression, and rerun the directly impacted
hook checks. Resolve any newer verification feedback without rewriting history.
Only after all adopted scope is complete should this worktree be submitted with
`story move SH-703 verifying` as the final action.
