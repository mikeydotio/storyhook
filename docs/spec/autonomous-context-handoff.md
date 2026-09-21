# Autonomous context handoffs (SH-711)

## Problem and authority

The autonomous charter permits adopting work and deferring it when context
capacity is unknown or below half. In v2.4.2 it offers no continuation route;
the generic hard-stop instruction instead leads to a story block. Session
eligibility then correctly enforces that block. SH-702/703 and the dependent
SH-699 demonstrate the resulting operational deadlock.

A context transfer is neither an implementation approval nor a dependency.
The daemon owns its durable state. Native providers own live context management
and queued input. Only a proven absent process may be replaced automatically.
Real dependency, trust, ownership and human obviation holds remain enforced.

## Native handoff

An autonomous root emits one complete JSON object, with no surrounding prose:

```json
{"type":"storyhook.session-handoff","version":1,"story_id":"SH-711","kind":"context","evidence":{"context":"Why continuation is needed","outstanding_work":"Exact remaining approved work, corrections and tests"}}
```

The five envelope fields are exact. Evidence must contain nonempty context and
outstanding_work; additional evidence should include approved scope, decisions,
commits, tests/results, pending corrections, and prerequisite observations.
Whole-message parsing rejects duplicate keys, foreign story identities, malformed
objects, subagent events and unbound transcripts. A
continued native turn may still report an administrative obviation hold. Parsing runs
before implementation-plan classification and never consumes approval receipts.

The trusted Stop hook sends `story continuation request <id> --stdin --json`
with the envelope and validated provider origin. After durable acceptance, native
`decision: block` feedback continues the current task in its existing mode.
The provider compacts automatically as needed. No daemon `/compact`, Escape,
terminal paste, forced restart or collaboration-mode change occurs.

The receiver reads status, story discussion, Git history/diffs and pending work;
runs `story help obviation-review` and `story load-context --story <id>`; compares
every candidate; reconciles landed prerequisites and pending corrections; and
preserves all real holds. Already adopted work is assigned work: unknown context
does not justify endlessly adopting and deferring it again.

In Plan mode, continue read-only review and planning. Do not acknowledge or write
until ordinary implementation approval changes the session to Default. In Default,
acknowledge before implementation, then refresh the review immediately before
submission if comments, relationships or HEAD changed:

```sh
story continuation status SH-711 --json
story continuation ack SH-711 <request-id> --reviewed-seq <snapshot_seq> --head <HEAD> --provider codex --session-id <native-session-id>
```

Acknowledgement binds the fresh story sequence, Git HEAD and receiving native
session. Submission checks that evidence; a new durable correction invalidates
it. Native queued text is not exposed in Stop payloads or rollout history until
its queued turn executes. **Acknowledgement does not prove that queue is empty.**
Corrections which must fence submission must also be recorded on the story.
The runtime probe proves native continuation preserves and subsequently executes
the queue; it cannot promise the current turn processes queued work first.

## Durable state and recovery

An additive SQLite table retains request ID, immutable originating generation,
current receiving capture, exact cleanup lease, HEAD, dirty fingerprint and
base64-encoded NUL-delimited Git status inventory, provider
session/turn, tmux socket/window/pane, PID/start time, model/effort/speed/autonomy,
handoff evidence, revision, timestamps, attempts and reviewed sequence/HEAD.
Creation and its evidence comment are atomic. Duplicate generations return the
same record; conflicting same-kind payloads refuse. Generations include a native
assistant-message ID, so acknowledged continuations can request another handoff
in the same provider turn. The runtime validates the latest assistant content
against the envelope, current root session, task, cwd and mode. Codex uses its
message ID; Claude uses its root assistant UUID. Stop recursion flags do not
authorize or prohibit admission. Only the creating transaction
admits native feedback. A same-turn administrative hold can coexist with its
pending context transfer. Updates use revision compare-and-swap.
External observation and process work occur outside store transactions.

| State | Meaning |
|---|---|
| pending | Explicit retry accepted for observation. |
| attempting | One absent-provider recovery owns the external-effect gap. |
| awaiting-ack | Native continuation or replacement admitted; review remains required. |
| acknowledged | Receiving root bound current review and HEAD. |
| needs-attention | Observation failed, timed out, repeated without progress, or is uncertain. |
| superseded | The story was explicitly closed; delivery is no longer applicable. |

Live observation timeout is 45 seconds. It reports uncertainty and preserves the
session; it never retries input. A validated late acknowledgement may resolve it.
Three consecutive handoffs with unchanged HEAD and dirty content stop automatic
continuation, including distinct messages within one turn. An unresolved request
refuses a new context request. False native-feedback receipts are validated before
being treated as duplicate delivery: needs-attention and invalid records produce
a visible diagnostic with status and recovery commands. `story continuation retry <id> <request-id>` requires fresh safe
observations; it cannot replay an ambiguous live effect. A daemon restart during
an attempting effect marks uncertainty instead of replaying it.

Engine lane handling distinguishes outstanding continuations from dependency
blocks and ordinary missing/stalled lanes. Generic unblock prompt delivery is
suppressed while continuation owns delivery. Removing a real hold does not create
a second prompt producer.

The private Python runtime checks the canonical linked-worktree marker, retained
branch and repository inventory without reading main's working files. Dispatch
binds its native SessionStart witness to the exact pane process incarnation in
`@storyhook-continuation` before sending the charter. The witness alone does not
prove ownership. Observation never treats inaccessible or changed process evidence
as absence. Dirty fingerprints hash tracked diffs and untracked content, including
symlink targets, without mutating Git or following untracked links.

Automatic replacement requires the exact retained **dead pane**, unchanged
captured Git evidence, eligibility, capability and identity. Guarded dispatch
uses `respawn-pane` without `-k`, so tmux atomically refuses a live process that
appeared after preflight. It does not reconstruct missing branches/worktrees or
create windows from a racy name lookup. Missing/ambiguous windows become visible
needs-attention evidence. Manual supported recovery can inspect them explicitly.

A replacement retains model, effort, speed and autonomy settings and starts in
Plan mode. This is a stricter boundary than restoring a dead Default process.
Ordinary automatic plan approval governs its subsequent writes. No human reply is
required in autonomous sessions. Native PostCompact receipts are informational;
they neither acknowledge review nor schedule more input.

## Administrative obviation stop

SH-710 exposed a second deadlock: a Plan-mode agent can discover likely obviation
but cannot write its required review block. It emits the same strict envelope
with `kind: obviation-review` and evidence containing nonempty `context`, the
exact `original_state`, and a nonempty unique `candidates` list of other story IDs.

The supervisor atomically records evidence, reciprocal obviation relationships
and the guarded blocked transition. It preserves unrelated holds, refuses state
conflicts and never approves implementation, determines obviation, closes the
story or clears the human-review hold. The branch, worktree and session remain.

## Compatibility and verification

The helper advertises the charter only when the installed daemon supports
continuation protocol 1 and exact registration succeeds. Older combinations keep
ordinary dispatch with a visible continuation-unavailable diagnostic. Guarded
recovery refuses missing capability. Source merge alone does not prove an
installed runtime supports the feature; the story's operational bridge remains
until a containing release is installed and verified.

Regression layers cover production hook parsing, durable service and worker
transitions, submission fencing, private tmux/Git ownership and guarded dispatch.
`probe_context_compaction.py --native-stop` uses the actual installed Codex runtime,
test-owned native hook trust, fixture model responses and a fixture StoryHook
endpoint. It verifies Default acknowledgement, Plan read-only continuation,
preserved sandbox/mode, bounded recursion and retained queued correction. It does
not exercise the real daemon store; Rust tests cover that layer separately.

Run the actual-tree selector and only new/directly impacted tests. The central
verifier owns the full suite and submission. No release or installation is part
of this worktree's implementation.

## SH-735 repeated-handoff regression

SH-734 emitted distinct handoff messages at 2026-09-15T22:03:16Z and
22:31:40Z in session `01a0a700-e555-7e53-896b-3ed1f4f9027f`, turn
`01a0a703-a00e-7e71-9a3c-562b73bfd9e8`. The first was acknowledged;
commit `5b02b9a33` preceded the second. The second was followed by
`task_complete` without another recorded request. Its original Stop payload
was not retained, so the transcript alone does not establish that payload.

The isolated real Codex probe reproduced a first Stop with
`stop_hook_active=false`, acknowledgement through a native tool call, and a
second Stop in the same turn with `stop_hook_active=true`. Before the fix,
the production hook returned no continuation and the turn completed. The
separate Rust regression exposed turn-only generation matching; the SQL index
also prohibited multiple same-turn records. Repeated-handoff admission therefore
requires both hook and persistence fixes.

Migration 46 adds message-level uniqueness without rewriting records. A missing
legacy message identity remains a conservative turn-wide match; an identical
request cannot acquire another feedback receipt, and changed evidence refuses
visibly. The current runtime must supply a validated message ID for new records.
The public five-field handoff envelope remains version 1.

Run `python3 plugins/story/tests/probe_context_compaction.py --native-stop --repeated`
for the real-provider regression. Model responses and the external StoryHook
endpoint are fixtures; native Stop handling, message validation, Git progress,
sandbox enforcement and queued correction delivery execute production behavior.
The Default fixture preserves its existing commit while editing assigned work
and retaining a dirty correction between handoffs; the Plan fixture remains read-only without acknowledging. Rust tests
exercise actual store admission, deduplication, progress limits and migration.
Neither probe modifies installed plugins or operator sessions.

The sibling Codex plan-approval guard remains intentional: it applies only after
administrative handling and cannot consume or approve a context handoff. Claude
uses the shared administrative handler and receives the same bounded admission
and explicit refusal behavior.


## Lost request responses — SH-742

The CLI deadline does not cancel daemon execution. A request can be accepted
before its response is lost. Transport failure or undecodable request output
therefore returns native Stop feedback for **status inspection only**. It does
not replay the request, issue an atomic native-feedback receipt, approve a plan,
or authorize implementation. The owning session must match retained provider,
session, message generation, and handoff evidence, perform fresh review, and
acknowledge an accepted request before resuming previously authorized work.
Plan mode keeps its ordinary approval boundary. Real holds remain in force.

Explicit decoded supervisor refusals and invalid input remain diagnostic-only.
Nonzero subprocess errors retain bounded stdout and stderr, so JSON-only
DeadlineExceeded output is not replaced by an empty error. The hook keeps its
bounded deadline; increasing that deadline cannot remove the lost-response
window. The cause of an individual slow request requires separate evidence.

## Accepted after deadline — SH-744

An isolated regression runs the production CLI, Stop adapter, daemon intake,
and persistent continuation service. A fault-injection build delays the reply
for three seconds **after** the request transaction commits; the unchanged
Stop call gives its CLI two seconds. In one isolated run, provider capture was
observed at 218 ms, durable admission at 229 ms, and the Stop hook's lost-reply
feedback at 2,167 ms from launch. The test then proves that the one durable
`awaiting-ack` request survives and an identical request has the same ID with
`native_feedback: false`. These observations measure the client/server timing
mismatch under a controlled delay; they do not establish why the live
SH-742 request took longer than its deadline. The test replaces only the
external provider capture endpoint and does not touch the live SH-742 lane.

After 45 seconds without a receiving review, the supervisor keeps its existing
`needs-attention` status and does not type into a live provider. The shared
story view exposes an actionable continuation alert with the exact request ID,
diagnosis, and read-only status command. CLI list/show and the dashboard card
and detail banner display it. The story remains in its literal state, and the
outstanding continuation still prevents submission. A valid late acknowledgement
or explicit supersession removes the alert; neither the alert nor a status read
grants approval or acknowledges a request. The receiver must use the exact
current session, story sequence, and Git HEAD after reviewing pending comments,
corrections, holds, and the original handoff evidence.

The sibling `codex_stop.py` eligibility lookup and `stop-handoff.sh` handoff
query are reads. `compact_receipt.py` records an informational receipt, but
its failure cannot create a second native continuation or complete the required
receiving acknowledgement. They retain their existing deadlines and ownership.
