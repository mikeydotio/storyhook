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
objects, subagent events, repeated context feedback and unbound transcripts. A
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
same record; conflicting same-kind payloads refuse. Only the creating transaction
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
continuation. `story continuation retry <id> <request-id>` requires fresh safe
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
