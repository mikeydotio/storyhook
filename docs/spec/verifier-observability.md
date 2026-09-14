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

## Validation

Regression coverage exercises admission parity, durable receipts, immediate
worker wake, exact attempt correlation, stale-request settlement, output shapes,
freshness boundaries, hook delivery, and dashboard visibility. Fixtures use
isolated stores and production control/worker paths with external gate doubles.

The adopted halted-hook timeout defect is repaired at configuration validation.
Both supported loaders reject `on_verification_halted.timeout_seconds = 61`
against the existing 60-second ceiling, naming the field, value, and limit.
An independent table-driven regression covers all 13 hook names through both
loaders: omitted overrides inherit the configured default, 60 seconds loads, and
61 seconds rejects the entire configuration. `list_hooks` and `test_hook` expose
the same refusal. No timeout policy or configuration precedence changed.

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

### Continuation validation (2026-09-13)

Merge `78ed02dd5` preserves the primary implementation and incorporates centrally
verified SH-698 commit `d3a01a0e2`, including the impact-manifest, private-origin
reap fixture, and process-teardown repairs. The three new hook regressions failed
before the missing validator entry was added; all pass after the origin repair.

| Direct check | Result |
|---|---|
| Hook units / hook CLI integration | 28 / 11 passed |
| Verifier observability / controls | 12 / 13 passed |
| Process units / selective gate / timing assertions | 9 / 25 / 12 passed |
| Verifier lifecycle wrapper / exact leased-reap regression | 1 / 1 passed |
| Targeted Clippy with warnings denied | Passed |

The initial sandboxed process run had one cancellation assertion failure; the
unchanged nine-test process selection passed with approved execution access.
Compiler-slot access was also denied inside the sandbox; subsequent compilation
used approved access to the existing shared slots without changing the wrapper.
Cargo may replay earlier dependency stderr. These diagnostics remain in the
story's continuation report; they are not attributed to the hook repair.

The selector still returns `ALL` for its missing baseline coverage map. The full
suite and final merge verification remain central-verifier-owned. No adopted
implementation scope remains pending.

## Recovered infrastructure retries — SH-714

An incident records failed attempts and enforces their retry ceiling. Its presence
alone does not establish that the current attempt is still blocked. Deleting it
when a launcher starts would erase the failure budget without proving recovery.

Transactional admission captures the prior incident ID and failed-attempt count
in optional process-local `ActiveVerification.retry_origin`. A shared journal
observation can classify that incident as history only when all conditions hold:

| Evidence | Required condition |
|---|---|
| Incident | Retryable, not halted, unchanged admission ID and attempt count |
| Ownership | Same project, story, and nonempty submission generation as the queued candidate and incident |
| Journal | Exact active attempt UUID and generation |
| Lifecycle | Explicit root merge-preflight Passed, then explicit root release-gate Running, Passed, Failed, or Reused |

This means the retry reached verification; it neither certifies success nor
promises infrastructure cannot fail again. Classification continues through gate
completion and landing while ownership remains current. A new failure recorded
before ownership release immediately restores incident authority.

Missing, stale, legacy generation-only, or malformed lifecycle evidence cannot
override an incident. A complete malformed journal record invalidates recovery
proof until the next valid run record. An unfinished final append is tolerated;
unknown future record kinds remain forward compatible. Output-reference schema
diagnostics remain independent because raw output is not lifecycle authority.

Status, dashboard data, and progress publication share one parsed journal per
snapshot. `VerifierStatus.incident_is_current` is false with no incident or a
proven recovering attempt, and missing fields deserialize conservatively as true.
The retained `incident` still carries its original diagnosis, counts and dates.
Human status identifies it as “Previous infrastructure failure; current retry
running.” Story cards and live comments show Running; waiting cards show ordinary
ownership instead of inheriting the historical prerequisite failure.

Manual stopped, draining, and stopping controls remain independent and still hold
the queue. Halted incidents always retain acknowledgement authority. Publication
rechecks ownership and cancellation under the registry lock, then compares the
incident inside the generation-guarded store transaction, so a prepared Running
body cannot overwrite a later failure or interruption.

Regression coverage includes the production shell verifier failing real local
head-ref convergence, automatically retrying into a held gate, and separately
settling RED and GREEN. Production CLI/load-context, REST data, status, comments,
failure budgets, stale identity, malformed proof, terminal/reused gates and
browser rendering are exercised. Mutation checks must reject restored
incident-first precedence and removed UUID/new-failure guards.

The read-only installed-system inspection workaround remains necessary until a
containing release is installed and an installed recovery regression validates
the transition. A source merge does not retire it.
