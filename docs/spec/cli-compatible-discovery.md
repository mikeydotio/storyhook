# Compatible CLI discovery and explicit JSON input

SH-757, StoryHook v3.0.3. Approved by Mikey on 2026-10-08: implement compatible
package A and defer optional contract-v2 package B. This document is the planning
deliverable; the four implementation stories below own executable changes.
Approval of this design does not make those changes shipped or authorize a
breaking default, production automation, a full-suite experiment, or a PR merge.

## Compatibility boundary

Keep existing command names, runnable aliases, help-only aliases, input forms,
stdout/stderr contracts, exit codes and authorization. In particular, retain
legacy end-of-options/help handling and the zero/one/many `next` response shapes.
Do not introduce `--cli-contract 2`, warnings on machine stdout, new ambient defaults,
a universal output wrapper, or automatic consumer migration in this package.

The SH-755 audit remains the problem statement: [command audit](../reports/SH-755-cli-audit.html)
and [CLI agent usability](cli-agent-usability.md). Its documentation improvements
did not implement machine-readable discovery or separate JSON input selection.
The surface was refreshed against integrated dev
`06a5061c710f8f5281b7f3b3f9f6867982c1d58d` after the reset, manual-mode and recovery
work. SH-752's assignment removal remains removed. SH-756's explicit complexity
(including medium versus unassessed medium), policy inheritance and executable-child
dispatch resolution remain intact. Include atomic `new --blocked-by`, project
`automations.enabled`, `reset --dry-run`, and managed recovery protocols in the model.

## Approved compatible package A

| Change | Concrete contract and migration | Acceptance tests to add |
|---|---|---|
| Authoritative discovery | Add `story describe [command path] --json [--audience task|operator|internal|all]`. Default audience is task; existing `help --all` remains available. Publish a versioned discovery document from one command model that also supplies help syntax and flag validation/dispatch registration. Represent canonical path, aliases, positional/option grammar, enums or dynamic-enum sources, audience, outputs, and effects. Use an exhaustive mapping for early runtime protocols; do not invent a second unchecked registry. | Every runnable path and alias has one descriptor; every descriptor resolves to a parser/early handler. Required/optional/repeated arguments, flags, enums, and invalid examples agree with parsing. Discovery works without a project or store and does not start the daemon. Unknown command paths return exit 2. |
| Task/operator/protocol separation | Classify subcommands individually: e.g. story editing is task work; `verifier status/start/stop` is operator work; `verifier repair-admit` is internal. Group help and filter discovery; do not move executable names or treat visibility as authorization. | Task listing excludes private protocols; operator and all listings are complete; exact private callers retain their syntax and existing authorization refusals. |
| Separate JSON input | Add `set <id> --input-json <object>`. Keep `--json` as output selection and retain legacy `set --json <object>` during migration. Reject simultaneous legacy and new input forms with an actionable exit-2 diagnostic before any mutation. No new stdin/file convention in this increment. | New input plus JSON output, legacy input plus JSON output, malformed/empty JSON, object-shaped text, unknown fields, duplicate input sources, and atomic failure. Existing stdout/stderr and exit contracts are unchanged. |
| Alias and lifecycle clarity | Keep `relate/unrelate` canonical and `link/unlink` as runnable compatibility aliases. Explain separately that `project link` registers origins/checkouts. Describe `context` and `sync-git` aliases. Mark `states`, `is`, `awaits`, and `priority` as help-only aliases, not executable verbs. Document actual distinct effects of close, archive, delete, reset, and unclaim. No renaming/removal. | Runnable aliases parse equivalently; help-only aliases remain help-only; lifecycle fixtures prove archive visibility, deletion, abandonment, reset, and claim release are not interchangeable. |
| Explicit output capabilities | Describe per-subcommand modes as envelope, raw document, JSON Lines stream, delegated helper, or terminal. Record content/schema, success/empty/error shape, stdout/stderr, exit status, and quiet/follow behavior. Keep `--json`, raw exports, hook payloads, and helper streams unchanged. Do not add a universal wrapper or output flag yet. | Consumer fixtures for each output class; raw export round trip; complete JSONL records during follow; helper stream/status forwarding; quiet success versus errors; conflicts and empty responses. |
| Mutation and preview metadata | Declare effects (store, filesystem, processes, remote operations), confirmation/noninteractive rules, actual dry-run support, guarded-write support, and uncertain-outcome guidance per subcommand. Read-only data queries can still start a daemon; distinguish that operational effect. No universal dry-run, idempotency, or cancellation promise. | Descriptor capability checks against real handlers. Supported dry-runs preserve store and external side effects; unsupported flags fail before mutation; confirmation refusals, guarded conflicts, and lost-reply read-before-retry guidance stay correct. |

Example after A: `story --json set SH-42 --input-json '{"complexity":"medium"}'`.


## One model, multiple consumers

Use one typed command model for grammar/help syntax, flag validation and dispatch
registration. Existing hand-written handlers may remain, but each registration
must identify its handler; an exhaustive mapping covers early runtime protocols
that run before ordinary invocation routing. A second manually duplicated discovery
catalog is not sufficient. Grammar tests must compare actual parsing and handler
resolution with metadata, including malformed/unsupported examples.

Discovery is an offline early command. It must neither resolve a project nor open
a store, start a daemon, inspect credentials, dispatch a provider, or execute a
helper merely to describe that helper. Dynamic enum sources are named as sources,
not fetched by default. Describe effects of the eventual command, separately from
discovery's own lack of effects. Do not confuse a read-only query with a guarantee
that its usual execution cannot start the daemon.

The discovery document has an explicit schema version, a canonical command path,
runnable aliases distinct from help-only topic aliases, argument multiplicity,
static enums or dynamic enum-source names, audience, output capabilities and effect
capabilities. Keep a stable documented ordering. Future incompatible discovery
schema changes need a new version; versioning this document does not change the
legacy CLI behavior contract. Unknown paths and invalid audience values fail with
exit 2 before runtime initialization. An audience filters individual subcommands;
`all` includes every class. Filtering never grants or revokes execution permission.

Output metadata must identify envelope, raw document, JSON Lines, delegated helper,
and terminal behavior honestly. Capture empty/error alternatives and channel/exit
semantics instead of pretending every verb returns one uniform JSON envelope.
Effects distinguish store, filesystem, processes and remote operations. Declare
only implemented preview, guarded-write, confirmation and noninteractive contracts.
Uncertain writes require a read/reconciliation instruction, not a blind retry promise.

## Tracked implementation and execution order

| Story | Deliverable | Dependencies |
| --- | --- | --- |
| SH-898 | Unify command definitions and prove parser/help/dispatch parity. | None; approved design is recorded here. |
| SH-899 | Add explicit `set --input-json` with legacy input compatibility and atomic refusal. | None; independent of the registry refactor. |
| SH-900 | Expose offline `describe`, audiences, aliases, output and effect metadata. | SH-898 and SH-899, so the published model includes the new input form. |
| SH-901 | Validate real consumer compatibility, migration examples and the final command audit. | SH-900. |

All four stories relate to SH-757. They are not executable children of a newly
invented epic: SH-757 remains the original planning story. Its acceptance is the
reviewed plan, accepted/deferred decisions and concrete tracked implementation,
not completion of all four implementations. Conversely, landing this plan must
not mark their behavior implemented. Source dependencies above are recorded as
`blocked-by` when the dependent stories are created.

Each implementation story adds meaningful focused tests for its own acceptance.
SH-898 must test against actual parsers/handlers; SH-899 must observe unchanged
store state on rejected input; SH-900 must prove offline behavior and truthful
capabilities; SH-901 must exercise consumer behavior, including raw export round
trip, JSONL framing, delegated streams/status, quiet errors and lifecycle distinctions.
Run only newly added focused tests under the current manual campaign cadence.
Broad verification and release remain separate. Preserve new failure evidence;
do not manufacture an integrated gate receipt from focused tests.

## Deferred package B

Literal-text termination consistency and stable queue result shapes are explicitly
deferred. The reviewed possible design was an opt-in `--cli-contract 2`, with legacy
behavior still the default, but this approval does not authorize implementing it.
Do not file a ready implementation that silently treats the deferred option as
selected. A later explicit decision must settle its exact grammar, response schema,
consumer migration and acceptance tests before implementation.

## Validation of this planning change

This is documentation only. It adds no runtime behavior and no tests. Validate the
local document links, whitespace, current-source references and the four recorded
story IDs/dependency edges. Do not run the existing CLI or full-suite tests merely
to claim a test pass for a planning document.
