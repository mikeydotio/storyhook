# The verification workflow — SH-645

The design of record for how a story goes from "implemented" to "merged,
done, and reaped": who submits, who tests, who merges, and who cleans up.
Epic `story show SH-645` states the target; this document carries it, the gap
between it and the tree, the decisions taken to close that gap, and — once
each child lands — what was actually built. Read it before touching
`src/service/verification.rs`, `src/daemon/verification.rs`,
`scripts/verify-pr.sh`, `scripts/merge-watch.sh`, `scripts/land-pr.sh`,
`scripts/machine-lock.sh`, or the dispatch charters in
`plugins/story/bin/story.sh`.

Until this document existed, the verifier was described in three places that
each owned a slice — `full-auto-engine.md` (D4, D5, D14 and some fifteen As
built entries), `test-tiers.md` (the merge gate, receipts, the verifier
worktree) and `selective-testing.md` (what the `changed` tier may never do) —
and nothing owned the whole. Six statements had gone stale across them by
2026-09-10, one invariant that every verification depends on was written
down nowhere, and one mechanism (the conflict queue-hold) existed in code
and in a test and in no spec. Those are corrected in place and cross-
referenced from here; the sections below are the source from now on.

## The workflow

Stated by the operator on 2026-09-10 and recorded on SH-645 verbatim as the
target:

1. When a story finishes its implementation, **storyhook** (deterministic,
   not the agent) pushes a PR for it; the story moves to `verifying`, which is
   the verifier's queue.
2. The verifier is the **only** test runner and merge validator, serializing
   all test batteries **per project** (not machine-wide). It first checks
   mergeability: mergeable → step 3; not mergeable → instruct the implementor
   agent to rebase/reconcile and **wait, holding the queue**.
3. Mergeable: run the merge-gate tests, **configurable per project**
   (storyhook: all non-browser tests).
4. a. Red: send the story and its test log back to the implementor agent in
   the **same tmux window**; on completion the story re-enters the queue and
   repeats when its turn comes.
   b. Green: storyhook merges the PR, moves the story to Done, reaps the
   story's tmux window, worktree, and per-story caches and build products.

What each step does today, and which child closes the gap:

| Step | Target | Today | Closed by |
|---|---|---|---|
| 1 | storyhook pushes and opens the PR | **done (SH-647):** the agent commits and moves the story to `verifying` from inside its worktree; the verifier runs `story.sh submit` from the lease — push over HTTPS, open or adopt the PR against the default branch, link it, comment — before it verifies | SH-647 (D-A) |
| 2 | one verifier per project; suites serialize per project | **done** (SH-648): one worker per project (`VerificationActivity` is a per-project map, `ordered_for`), and `gate`/`merge` carry the project's canonical git common dir in their key — see "The queue" and "The locks" | done, SH-648 (D-B) |
| 2 | order by priority then queue age | priority → `verifying_since` → project slug → story id (`sort_candidates`); missing entry timestamps follow known timestamps within equal priority | done, SH-651 (D-F) |
| 2 | not mergeable → instruct the agent and hold the queue | as stated; a dead pane is re-dispatched in place and the hold continues (SH-650) — see "The conflict queue-hold" | done (SH-650, D-E) |
| 3 | gate configurable per project | `.storyhook.toml` `[verify] gate`, default `make test`; the daemon reads it, `verify-pr.sh` requires it as argv, the GREEN/RED text names it | done, SH-649 (D-D) |
| 4a | red returns to the same window | pasted into the dispatched pane by `story.sh notify`; pane gone → `awaiting` is set, and under Full Auto that is `AgentBlocked` → quarantine → a breaker strike | SH-650 (D-E) |
| 4b | merge, done, reap | `land-pr.sh` merges; the verifier writes the required `done` and `reap-leased` accepts exactly that — one constant, `domain::COMPLETION_STATE_SLUG`, pinned equal to the helper's by `tests/plugin_contract.rs` (SH-652, **deviating from D-G**: see As built) | SH-652 (D-G) |
| 4c | the verifier reaps | `story cleanup` (`workspace-cleanup.md`) is a second reaper with no story-state gate and wider authority (it deletes the remote branch) | SH-653 (D-H) |
| — | (unstated) the verifier serves any project | the verifier script family ships inside the binary and runs from the daemon's own state directory; the checkout contributes its `[verify] gate` and its receipt store | done, SH-654 (filed beside the epic, not in it); landing a foreign project still needs SH-665 |

## Decisions of record

Taken in an unattended session on 2026-09-10, each with one clear best answer
given the stated process or settled by user determination; the reasoning is
repeated here so a reader does not have to open eight stories to see why.

| # | Decision | Why | Child | Status |
|---|---|---|---|---|
| D-A | **The verifier submits.** For a candidate with no linked open close-on-merge PR, the verifier's first step is submission from the cleanup lease's worktree and branch: refuse a dirty worktree (return the story naming the files), push over HTTPS, create the PR against the project's integration branch or **adopt** the one already open for that head, `link-pr`, comment the URL. `MissingPullRequest` becomes a submit step; `MultiplePullRequests` still returns. | Matches the stated order. Submission is derived from store facts (lease + no linked PR), so a daemon restart or a skipped verb cannot lose it; resubmission after remediation needs no extra agent step; and it removes `git push` from the agent's toolchain entirely, which dissolves the push-hook contradiction (D-C) structurally rather than by exemption. Rejected: an agent-invoked submit verb. | SH-647 | open |
| D-B | **Per-project queue and locks; cross-project suites may overlap.** Lock key = the canonical git common dir, hashed into the lock name, so every worktree of one clone still serializes with that clone's verifier and a different repository does not. One verifier worker per project, each with its own ordering, incident halt and conflict hold. | User determination: "project-wide (not machine-wide)". Trade-off stated, not hidden: two projects' suites now contend for CPU on one machine; SH-627's quiesce rule still governs the release tier; no machine-wide cap (YAGNI — D14's lane budget bounds agents). | SH-648 | done — see "SH-648" under As built |
| D-C | **The user-level push hook delegates** to repositories whose `core.hooksPath` names a tracked `pre-push`. | The PreToolUse hook `~/.claude/hooks/pre-push-tests.sh` ran `make test` on every agent push under an 840s budget and waited on the verifier's own `gate` lock (628s measured on SH-640, budget breached, `SKIP_PREPUSH_TESTS=1` reached for). Deleting the hook was rejected at that time: other projects had no gate of their own. | SH-681 corrected ownership: canonical source is Agentics `hooks/pre-push-tests.sh`; the original live delegation patch affected Claude only. See the evidence and subsequent retirement under As built. | superseded by SH-682 / AGE-102 retirement; SH-681 repair archived |
| D-D | **The gate command lives in `.storyhook.toml` `[verify] gate`**, default `make test` when absent; a value that is not a plain argv is refused by name (the SH-357 rule). | A fact about the checkout, versioned with the Makefile it names, belongs in the repository rather than in store settings. E2E stays off the verification allowlist; a project that wants the browser tier names `make test-full` as its gate. | SH-649 | done — see "SH-649" under As built for the receipt contract this put on the value |
| D-E | **A dead pane triggers a resume re-dispatch, never parking.** On a notify refusal the verifier dispatches the same story with the resume clause into the same window name and worktree, then delivers the diagnosis as the first turn; `awaiting` is set only if the re-dispatch itself is refused. The conflict hold applies unconditionally. | Step 4a says the same window. Parking classifies as `AgentBlocked` under Full Auto and strikes the breaker for what is ordinary remediation. | SH-650 | done — see "As built — SH-650" for what "a notify refusal" and "unconditionally" turned out to mean |
| D-F | **Queue age is `verifying_since`**: priority → `verifying_since` → project slug → story id. | At equal priority an old story resubmitted repeatedly permanently outranks a newer one that has waited longer; `verifying_since` is already the documented honest queue-wait fact (SH-524) and is the only one that resets on resubmission. | SH-651 | done — see "SH-651" under As built |
| D-G | **One completion-state resolver** in `src/service` (first CLOSED state, `STORY_DONE_STATE` override) used by the verifier, the template renderer and the helper. | Three spellings of one fact disagree by construction today; a project whose first CLOSED state is not `done` lands every green story, writes `done`, and then fails reap on every attempt, forever, loudly. | SH-652 | done — **built with different semantics**: the resolver is `domain::completion_state`, answering the required `done`, never the first CLOSED state, and `STORY_DONE_STATE` is refused rather than honoured; a council decision recorded on the story (`story show SH-652`) and under As built |
| D-H | **`story cleanup` is subordinated to the verifier.** It may touch only a worktree whose story is CLOSED and carries the verifier's CLEANUP COMPLETE or CLEANUP REQUIRED marker (the retry path, never an independent one); it never deletes a remote branch `land-pr.sh` has not already removed; `--dry-run` says what it declined and why. | Step 4b names one reaper. A second, state-blind one with wider authority and no lease is exactly the kind of "two answers from one fact" this project has paid for (SH-136, SH-263). | SH-653 | open |

A child that lands updates its row's status and adds an entry under "As
built" below. A child that deviates from its row records the deviation there
rather than editing the row.

## The gap ledger

Measured 2026-09-10 by three read-only explorations; re-pointed here at
`ade35404d` (the `origin/dev` tip this document was written against) rather
than the SH-640 worktree the epic cites. Line numbers drift; the symbols do
not, and each row names one.

| Gap | Where (symbol) | Line at `ade35404d` | Child |
|---|---|---|---|
| Both charters tell the agent to push, open the PR and link it; nothing deterministic does | `PROMPT_TPL`, `AUTO_PROMPT_TAIL` (`plugins/story/bin/story.sh`) | 441, 504 | SH-647 — **closed** |
| `link-pr` only records a URL; it never pushes or opens anything (the verifier now does, through `story.sh submit` and `record_generation_submitted`) | `PrLinkService::link` (`src/service/pr_link.rs`) | module doc | SH-647 — **closed** |
| The user-level PreToolUse push hook ran `make test` on every agent push and waited on the verifier's own lock | Agentics `hooks/pre-push-tests.sh`; installed Claude and Codex copies (ownership corrected by SH-681) | — | D-C covered Claude only; SH-682 / AGE-102 subsequently retired both live hooks |
| One global worker; a queue spanning every project | `VerificationActivity::acquire` (`src/daemon/verification.rs`); `ordered_candidates` (`src/service/verification.rs`) | 88-110; 650 | done (SH-648): `acquire` asserts per project, `poll_verification` supervises one `poll_project_verification` per project, `ordered_candidates_for` |
| `gate`/`merge` keyed by name only under `$HOME` | `scripts/machine-lock.sh` lock root | 219-225 | done (SH-648): `<name>.<hash of the canonical common dir>.lock`; `--held` |
| Tiebreak is `created_at`; `verifying_since` exists and is not it | `sort_candidates`; `verifying_since`, `verifying_entry` | 742-750; 99-106, 632-644 | done (SH-651): verification sorts by latest entry time; cleanup retains creation order |
| The conflict hold is released when the paste fails | `wait_for_reconciled_candidate`, `return_for_repair`; `tests/verification_queue.rs::a_failed_conflict_notification_releases_the_reservation` | 1246, 1190-1213; 1439 | done (SH-650) — the test is now `a_conflict_returned_to_a_dead_pane_is_redispatched_and_still_holds_the_queue` |
| `make test` is a literal in the gate invocation and in the comment text | `run_verification_gate` call (`scripts/verify-pr.sh`); GREEN and RED format strings (`src/daemon/verification.rs`) | 673; 896, 996 | done (SH-649): `gate_command_for` (`src/service/gate_command.rs`), `verify-pr.sh <pr-url> -- <gate…>`, `{gate}` in both strings |
| `scripts/verify-pr.sh` is a repo-relative literal in the daemon | `ShellVerificationActuator::verify` | 521 | SH-654 |
| RED pastes into the dispatched pane; a dead pane parks the story | `cmd_notify` (`story.sh`); `set_generation_awaiting` fallback in `return_for_repair` | 3182-3215; 1204-1210 | done (SH-650) |
| The verifier writes the literal `done`; reap requires the first CLOSED state | `record_generation_merged`, `record_merged`; `story_closed_state`, `cmd_reap_leased` (`story.sh`); `{done_state}` in `src/service/templates.rs` | 195, 523; 3435-3441, 4072-4078; 48 | SH-652 |
| `story cleanup` reads every story, gates on no state, and deletes the remote branch | `CleanupService::run` (`src/service/cleanup.rs`); daily trigger in `src/daemon/cleanup.rs` | 97, 482-487; 64-70 | SH-653 |
| `machine-lock.sh`'s header said its callers did not exist; specs counted two lock names; `merge-watch.sh`'s strip list silently depended on not stripping `STORYHOOK_MACHINE_LOCKS` | the header; `full-auto-engine.md` "Central verification and machine locks"; the `env -u` list | 9-12; 574; 283-289 | this document |

## The mechanism as it stands

This section describes the tree **today**, so that a reader who arrives before
SH-647..SH-653 land is told the truth rather than the target. Each child
replaces the paragraph it changes and says so under "As built".

### Submission

The agent commits its work and runs `story move <n> verifying` from inside its
worktree as its last action — nothing more (SH-647). `verifying` is a required
OPEN state (SH-521); the transition captures a cleanup lease naming the
worktree and branch (`src/service/story.rs`), which is what the verifier both
submits from and later reaps from.

The verifier's **first** step on a leased candidate is submission, through the
leased, verifier-only helper verb `story.sh submit` (`ShellVerificationActuator::
submit_leased`, spawned like `reap_leased` but under the submission allowlist,
the one helper child that carries the operator's GitHub credential). It
re-proves the lease, requires the story to be in `verifying`, refuses a dirty
worktree naming the files, pushes the leased branch over HTTPS
(`url.https://github.com/.insteadOf=git@github.com:`; no `--force` — a rewritten
branch is returned to the agent as `push-rejected`), then opens one PR against
the repository's default branch (`origin/HEAD`, `dev` here — the same fact
dispatch based the worktree on) or **adopts** the one already open for that
head. The helper records nothing on the story; it answers a typed
`SubmissionReceipt`, and the daemon records the `StoryPrLinked`
(`close_on_merge`) and a marked `CENTRAL VERIFICATION SUBMITTED` comment in one
generation-guarded write (`record_generation_submitted`), then proceeds into
verification in the same tick.

Submission runs on **every** leased generation, linked PR or not: after a RED
or conflict return the agent only commits, so the verifier's push is the one
thing that carries the fix to the remote; push and adopt are idempotent, and an
in-tick guard stops one generation being pushed twice. A daemon restart at any
point re-runs the whole verb and converges — the PR a crashed attempt created
is adopted by the next. After a real push the head may take a moment to
converge on GitHub, which `verify-pr.sh` reports as its retryable outcome
(SH-636); the next tick re-pushes (a no-op) and verifies.

A refusal the helper classes `repair` (dirty worktree, rejected push, more than
one open PR) returns the story to its agent with the helper's own words; a
`infrastructure` refusal (an unreachable GitHub, a failed push) is a retryable
incident and the story stays in `verifying`. A PR on a repository the project
has not registered is a configuration fault no retry fixes, so it halts the
queue. A story that entered `verifying` with no lease — moved from outside its
worktree — is returned naming that cause: there is no branch to push, and the
fix is to commit and re-run `story move <n> verifying` from the worktree.

The other three `VerificationProblem` shapes are unchanged: more than one open
close-on-merge link (`MultiplePullRequests`) and a link whose repository is not
registered (`UnregisteredPullRequest`) still return the story; `MissingCheckout`
is still configuration work. The `.githooks/pre-push` gate reports and never
refuses on a feature branch (SH-429), and the verifier's push is not gated by
the suite either — the gate runs on the speculative merge tree, after
submission.

### The queue

`VerificationQueue::ordered_for(project)` (`src/service/verification.rs`)
folds one project's stories in `verifying` into candidates and sorts them with
`sort_candidates`: priority rank, then `verifying_since`, then project slug, then
story id (`ordered()` concatenates every project's for the cross-project
surfaces). `verifying_since` is computed by `verifying_entry` from the story's
own `StoryStateChanged` history rather than `updated_at` — the progress
checklist rewrites `updated_at` on every publish (SH-524). Resubmission resets
queue age (SH-651). Missing entry timestamps follow known timestamps within
equal priority; project and story identity still resolve ties. Completed-story
cleanup uses a separate sorter that retains priority, creation time, project,
and story order. The daemon runs **one worker per project**
(SH-648): `poll_verification` (`src/daemon/verification.rs`) is a supervisor
that spawns `poll_project_verification` for every registered project, on
start and on every catalog change, and a worker whose project is deleted
retires itself. `VerificationActivity` is a map keyed by project and asserts
if one project is acquired twice; two projects acquired at once is the point.
A halt (an infrastructure incident recorded for the current generation,
`record_generation_incident`, one row per project since schema 35) stalls
**that project's** queue until it is acknowledged through that project's
dashboard or the story's generation changes; every other project keeps
draining. A queued story's position and blocker are its own project's, since
the progress publisher and `/data` build each project's snapshot from that
project's queue, active attempt and incident.

Selection is store-derived on every tick, so a daemon restart loses nothing;
an in-flight file records the candidate the worker holds so a restart can
tell an interrupted verification from a fresh one (SH-547, SH-555).

### Mergeability, and the conflict queue-hold

`scripts/verify-pr.sh <pr-url> -- <gate…>` establishes the persistent verifier worktree,
refreshes the submission's refs (`refresh_submission_refs`: the PR head from
`gh`, `refs/pull/N/head`, and `refs/heads/<branch>` on origin, required to
agree three ways before anything is judged — SH-636), and runs
`scripts/merge-preflight.sh`, which computes the exact tree the merge would
produce with `git merge-tree --write-tree` and checks it against the receipt
store (SH-396). A conflict is `VerificationOutcome::Conflict`.

On `Conflict` the worker calls `return_for_repair`: it records the generation
as returned, moves the story back to `in-progress` (`RETURNED_STATE`),
comments the diagnosis, and asks the actuator to `notify` — `story.sh notify`,
which finds the story's tmux window and either pastes the diagnosis and presses
submit, or refuses by name: `pane-unavailable` (no window), `pane-dead` (the
pane's process has exited under `remain-on-exit`), `pane-changed` (something
else runs there), `pane-provider-unknown` (the window carries no Storyhook
provider tag), `pane-query-failed` (tmux could not be asked) or
`delivery-failed` (a live pane refused the paste). The daemon classifies those
slugs through one exhaustive table, `NOTIFY_REFUSALS`, into **absent** (the
first three) and **not absent** (the last three); `tests/notify_reasons.rs`
derives the helper's slugs from `cmd_notify`'s own literals and demands
set-equality with the table.

**If the paste succeeded, the verifier holds.** `wait_for_reconciled_candidate`
keeps the project's worker reserved for that story — every other project's
worker is unaffected (SH-648) — and re-observes the queue on every change-bus wake until the same story presents a **newer**
`verifying_generation` — the agent's resubmission — then transfers the
reservation to it and continues in the same tick. Other arrivals cannot take
the slot; a daemon stop ends the wait without manufacturing a candidate. This
is step 2's "wait, holding the queue", and it is pinned by
`tests/verification_queue.rs` (the reservation, the generation check, and that
a wrong candidate is an error rather than a transfer).

**If the agent is absent, the verifier re-dispatches and still holds**
(SH-650). A dispatched pane is **normally already dead** at the handoff — the
launch command is exec'd into the pane and exits with the agent — so this is
the common path. `return_for_repair` comments what it is about to do
(`CENTRAL VERIFICATION RESUME — …`), asks the helper for
`dispatch <id> --resume --auto` through the one argv composer the dashboard and
the engine use (`run_shell_dispatch`, with `STORY_TARGET_SESSION` and
`STORY_CREATE_SESSION` as the engine sets them), and pastes the diagnosis
afterwards. `--resume` respawns a dead pane in place under the same pane id,
recreates a missing window under the same name, reuses the `in-progress`
claim, rewrites the lease marker and appends the resume clause to the charter;
the helper relaunches the provider the abandoned dispatch recorded. A story a
live Full Auto lane holds is re-dispatched as that lane (the run's provider
options and `--full-auto`); see `resume_plan`. Remediation then counts as
started, so the reservation is kept exactly as after a delivered paste
(`a_conflict_returned_to_a_dead_pane_is_redispatched_and_still_holds_the_queue`).

**Only a refusal that is not absence, or a refused re-dispatch, parks.**
`return_for_repair` then falls back to `set_generation_awaiting` naming the
refusal, the reservation and the activity slot are both released, and the
story sits in `in-progress` with an `awaiting` reason until a person acts
(`a_refused_resume_redispatch_parks_the_story_and_releases_the_reservation`,
`a_notify_failure_that_is_not_absence_parks_without_redispatching`). Under
Full Auto that `awaiting` classifies as `AgentBlocked` (`full-auto-engine.md`,
"The verifying handoff") — now for a story a person genuinely has to look at.

### The gate

The gate is the project's own (SH-649, D-D): the daemon reads `[verify] gate`
from the registered checkout's committed `.storyhook.toml`
(`service::gate_command::gate_command_for`), `make test` when the file, the
table or the key is absent, and hands it to `verify-pr.sh` as
`<pr-url> -- <gate…>`. The script carries no default of its own —
`GateCommand::DEFAULT` is the one place it lives — and refuses by name to run
without one. The value is a **plain argv**: space-separated words of ASCII
alphanumerics and `_.:/=@+,-`, the first not a flag, because every hop from
the daemon to `merge-watch.sh --speculative-run … -- "$@"` execs it word for
word with no shell, so `make test && echo ok` would reach `make` as three
literal arguments. Anything else — and an unknown key under `[verify]`, which
would otherwise land nowhere and silently run the default — is refused naming
`[verify].gate`, the file, the value and the offending character; an
unreadable pointer surfaces `read_pointer`'s own error rather than failing
open. A refused gate is a **permanent infrastructure failure**: local
configuration needing a person, taken before any journal or process exists,
which halts the queue with a comment and is never returned to the implementor
as a red. The gate named in the GREEN and RED comments is the one the verdict
carries (`VerificationOutcome::{Merged, TestsFailed}.gate`), derived from the
same parsed value the argv was.

A tree with no `gate`/`full` receipt runs `run_verification_gate`, which
takes `machine-lock.sh gate` around the whole run (SH-589) and executes
`merge-watch.sh --speculative-run <tree> <base> <head> <worktree> -- <gate…>`.
`merge-watch.sh` checks the speculative merge out privately (lease-local
per-worktree administration, SH-552/SH-559) and execs the gate command in it
with a scrubbed environment. `make test` is the **gate** tier: the whole Rust
suite over `/api/v1/invoke` and the plugin shell leg, with the browser leg
deferred by design (SH-394); `make test-full` is the release tier, and a
project that wants it names it as its gate. A green run mints an ordinary
`gate` (or `full`) receipt through the portable `tree-receipt.sh postlude`
(or StoryHook's `gate-receipt.sh` wrapper), so the tree the
verifier just certified needs no second run anywhere else
(`selective-testing.md`, `test-tiers.md`).

**A gate must certify the tree it ran on.** Landing asks `merge-preflight.sh`
for a `gate`/`full` receipt before it merges, and the receipt is minted by the
suite itself. A gate with no postlude, or only a `changed` postlude
(`make test-changed`), exits 0 without certifying a merge.
`verify-pr.sh` re-asks `merge-preflight.sh` — the same reader, never
a second parser of the receipt file — immediately after a green gate and
refuses by name (`require_certified_by_gate`), rather than letting the refusal
surface downstream from `reconcile_land_refusal` as "no longer has a
qualifying release-gate receipt" after GitHub had been asked again. What is
**not** configurable, and stays that way: the receipt tier a merge accepts
(`gate` or `full`, never `changed` — a council decision on SH-429), and the
verification allowlist of environment names the daemon lets through to the
gate.

**Foreign project integration (SH-665).** `merge-watch.sh` supplies
`STORYHOOK_GATE_RECEIPT` as the absolute executable path to the portable
writer in this daemon's bundle, overriding any inherited value. The bundle
includes its `tracked-tree.sh` dependency. Neither file needs to exist in the
project. The writer leaves Git hook configuration untouched, including custom
hooks and repositories with no `.githooks` directory.

For example, commit `ci/gate.sh` and configure `[verify] gate = "bash ci/gate.sh"`:

```bash
#!/usr/bin/env bash
set -euo pipefail
: "${STORYHOOK_GATE_RECEIPT:?Run this gate through the StoryHook verifier}"
"$STORYHOOK_GATE_RECEIPT" preflight
cargo test --workspace # Replace with this project's complete required checks.
"$STORYHOOK_GATE_RECEIPT" postlude gate
```

The postlude must be reached only after every required check succeeds. Use
`full` only when the project's full tier actually ran. Shell quoting belongs
inside the script, not in the plain-argv `[verify].gate` value. The path is
provided during verification; standalone local tests do not receive it.
StoryHook's own Makefile continues to call `gate-receipt.sh`, which enforces
its existing hook enrollment before delegating to the same portable core.
Receipt format, shared project storage, private preflight state and objects,
tree-drift refusal, tier ordering, and atomic publication are unchanged.

SH-683 adds shared lifecycle ownership before verifier preflight and speculative
execution. Interrupted recovery preserves the checkout, private index and
objects together; ambiguous writers prevent repair and remain infrastructure
failures. See [Shared verifier lifecycle](verifier-worktree-lifecycle.md) for
the durable journal, canonical registration rules and operator limits.

### Red

`VerificationOutcome::TestsFailed` carries the tree, the per-attempt log path
under `<common-dir>/storyhook/verification-logs/`, and a detail excerpt. The
verdict is confirmed against the PR's **current** head immediately before it
is posted (`confirm_judged_head`, SH-637); a head that moved during the run is
retryable, never a verdict about a commit nobody can act on. The worker then
calls `return_for_repair` exactly as for a conflict — comment, pane paste or
resume re-dispatch, `in-progress` — but without the hold: a red story
re-enters the queue on resubmission and waits its turn (step 4a;
`a_red_story_returned_to_a_dead_pane_is_redispatched_and_reenters_the_queue`).

The detail excerpt lists every failing case the log holds, and since SH-697 a
Rust battery runs to completion after its first red test binary
(`cargo test --no-fail-fast` in `scripts/run-tests.sh`), so one RED carries
every failure of the leg that went red rather than the first binary's; the
legs after it are still not reached (`test-tiers.md`, "a battery finishes
after its first red binary").

### Green: merge, done, reap

`Merged` means `land-pr.sh` ran under `machine-lock.sh merge`, re-read the
branch tip under that lock (SH-637), merged with `gh pr merge --merge`,
verified the merge landed, and deleted the remote branch. The worker then
`record_generation_merged`s the story into the completion state —
`domain::completion_state`, the required `done` while it is CLOSED, refusing
with `run story doctor --fix` on a catalog below the floor — comments GREEN,
and asks the actuator to `reap`: `story.sh reap-leased`, which re-checks every
postcondition from the lease — including that the story is CLOSED **and** in
that same completion state, which the helper spells as the constant
`COMPLETION_STATE` (`tests/plugin_contract.rs` pins it equal to the daemon's)
— before removing the tmux window, the worktree, the local branch and the
per-story caches. A failed reap comments CLEANUP REQUIRED and is retried by
`next_cleanup`, queried separately from active verification so a cleanup
fault cannot starve the gate. Until SH-652 the helper accepted only the
project's *first* CLOSED state or `$STORY_DONE_STATE`, so in a project that
ordered another CLOSED state ahead of `done` every retry failed the same way.

`story cleanup` (`workspace-cleanup.md`) is a second path to the same
resources: daily from the daemon when `cleanup.auto` allows (a missing stamp
counts as due), over every story with a lease, gated on git facts (clean,
unlocked, tips reachable from the default branch, window closed) and on **no
story state**, and deleting the remote branch as well as the local one. SH-653
makes it the verifier's retry path and nothing more.

### The locks, and the one invariant every verification depends on

`scripts/machine-lock.sh <name> -- <command>` is a pid-and-start-time-checked
advisory lock rooted under `$HOME/.local/state/storyhook/locks` (deliberately
not `$XDG_STATE_HOME`, which the test harness redirects per run). **Three**
names are live, and the **scope** of each is a property of the name, declared
in the script (SH-648):

| Name | Scope | Taken by | Around |
|---|---|---|---|
| `gate` | project | `scripts/run-tests.sh` (every `make test`, re-exec'd under the lock); `scripts/verify-pr.sh` (the whole speculative run) | the suite |
| `merge` | project | `scripts/land-pr.sh` (asserted by its private phase through `--held merge`, never re-taken) | preflight, merge, verify, branch delete |
| `release-observer` | machine | `scripts/release-watch.sh` (`release-observer.md`) | one observer pass — it drives the one Lima guest |

A project-scoped key is `<name>.<hash>`, where the hash is git's own object
hash (`git hash-object --stdin`, whole, never truncated) of the **canonical git
common dir** of the working directory — `cd "$(git rev-parse
--git-common-dir)" && pwd -P`, the derivation `verify-pr.sh` already keys its
receipts and logs by. Every worktree of one clone resolves the same directory,
and so does `merge-watch.sh`'s speculative checkout, whose swapped gitlink's
`commondir` names it; a second clone or an unrelated repository resolves a
different one. So an interactive `make test` in any worktree of this clone
still queues behind this clone's verifier, and another project's suite does
not (D-B). A project-scoped name taken from a directory that is not inside a
git repository is **refused by name**, never widened to machine scope
(SH-576). `--plan` prints `scope=`, `project=`, `key=` and `lock=` without
taking anything; `tests/machine_lock.rs` computes the expected key
independently of the script and pins that the three working directories
above agree.

Reentrancy is by environment: a successful take exports
`STORYHOOK_MACHINE_LOCKS="<held>:<key>"` — the full key, so reentrancy is per
(name, project) — and a later `machine-lock.sh` in the same process tree that
finds its key there runs without re-taking. `machine-lock.sh --held <name>`
(exit 0 held, 1 not, 2 refused) answers the same question for a caller, and is
the **only** reader of that variable outside the take. This is
load-bearing for the verifier in a way nothing had written down:
`verify-pr.sh` holds `gate`, `merge-watch.sh` execs `make test` inside it,
`make test` reaches `run-tests.sh`, and `run-tests.sh` re-execs itself under
`machine-lock.sh gate`. That inner take succeeds only because the variable
crossed `merge-watch.sh`'s `env -u` scrub. **`merge-watch.sh` must never strip
`STORYHOOK_MACHINE_LOCKS`** (nor `STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH`, which
the lock consumes to report its own wait): a holder is judged by liveness, not
by a clock, there is no `--max-wait` on the gate, and the outer holder is
provably alive — so the inner take would wait forever and every verification
in that project would deadlock against itself. `land-pr.sh` asks
`machine-lock.sh --held merge` as the proof that its private phase runs under
`merge`. The variable's format is known to `machine-lock.sh` alone, now with
no exceptions (`run-tests.sh` and `land-pr.sh` deliberately do not parse it —
the SH-136 rule); the invariant here is only that it **survives**.

### What the verifier cannot do, stated rather than glossed

- It **lands** only a project whose gate explicitly certifies its tree.
  SH-665 supplies the portable writer to every project, but a bare test
  command that exits zero still certifies nothing. Projects must integrate
  the preflight and successful postlude described above.
- It runs the gate tier, never the release tier, and so cannot find what only
  the browser suite finds (SH-416, SH-418, SH-622 are the precedents); the
  `browser-watch.sh` poller is what runs `make test-full` between releases.
- It does not itself prove a merge that GitHub performs without it — a merge
  from the UI or another machine is the SH-396 case, still covered only by
  `merge-preflight.sh` being run by someone.

## Related specs, and what each keeps

| Spec | Keeps |
|---|---|
| `full-auto-engine.md` | the engine, lanes, the verifying handoff, and the historical D4/D5/D14 rows (superseded in part by SH-645; noted under its decisions table) |
| `test-tiers.md` | the tiers, receipts, `merge-preflight.sh`, the verifier worktree and its private objects, the gate lock's idle ceiling |
| `selective-testing.md` | the `changed` tier and why a merge never accepts it |
| `development-branch.md` | `dev` integrates, `main` releases; what a PR targets |
| `workspace-cleanup.md` | `story cleanup`'s own preflight and recovery — subordinated by SH-653 |
| `release-observer.md` | the third lock name and the observer that takes it |
| `activity-log.md` | the verifier's tmux mirror and the daemon journal a stalled verification is diagnosed from |

## As built

Deviations from this document are recorded here, one entry per child, rather
than in a second file. Each child lands with its own `### SH-N — <what
changed>` entry and a status update in the decisions table above.

### SH-651 — queue age follows the latest submission

Verification uses priority, latest `verifying_since`, project slug, and story
ID, in that order. Resubmission sends a story behind equal-priority peers
that have waited longer. Comments do not change its position. Missing entry
timestamps follow known timestamps within equal priority; identity resolves
the remaining ties. Cleanup retains its separate creation-time order.

`tests/verification_queue_order.rs` exercises reversed creation/submission
order, repeated resubmission, comments, priority, equal-time identity ties,
and cleanup through the store-backed service. Comparator unit tests cover
missing timestamps and project/story ties in both input orders.

### SH-681 — push-hook ownership, publication evidence, and retirement

The D-C completion claim covered a local Claude patch. Agentics owned the
canonical gate and its installer; the installed Codex copy matched the
canonical source and lacked delegation. SH-665's retained logs identify
840-second suites launched before shell execution, including for a diagnostic
comment containing quoted push text. This was a pre-tool gate failure, not
evidence of a GitHub transport failure. The
[SH-681 RCA](../rca/remote-publication-hook-stalls.md) records timestamps,
artifact digests, source commits, and the 140 targeted repair checks.

Before SH-681 installed its tested repair, SH-682 completed a separate
user-approved retirement of both live files and registrations. A unanimous
council retained the repair in closed archival
[Agentics PR #187](https://github.com/mikeydotio/agentics/pull/187), with no
reinstallation. [Retirement PR #186](https://github.com/mikeydotio/agentics/pull/186)
is the active source direction, owned by AGE-102 / SH-682; their central
verification blocker is separate. Only this compatible evidence change enters
SH-681 verification. The installed v2.4.2 dispatch prompt also still asks
agents to publish although tracked source already implements SH-647; the RCA
records that drift without changing the cache or release state.

### SH-649 — `[verify] gate`, and the receipt contract it put on the value

Built as D-D states, with one addition the decision did not name and three
limits stated rather than glossed.

**The addition: a gate that exits 0 but mints no receipt is refused by
name.** D-D said the receipt tier stays non-configurable and left the
consequence implicit — that a gate other than `make test`/`make test-full`
certifies nothing and lands nothing. The story found the failure surfaced two
steps downstream with a diagnosis about the wrong layer (the SH-576/SH-578
shape), so `verify-pr.sh` now asks `merge-preflight.sh` again right after the
gate ("The gate", above). Wrapping an arbitrary gate in
`gate-receipt.sh preflight`/`postlude` ourselves was rejected: it nests when
the gate is `make test`, whose own preflight discards the outer one.

**Limits.**

- The pointer is read from the **registered checkout's working tree**, not
  from the merge tree being verified. The merge tree is only computed inside
  `verify-pr.sh`, and a shell-side TOML parse would be a second reader
  (SH-136). A PR that changes `[verify] gate` is therefore verified under the
  checkout's current value, and takes effect once merged and checked out.
- No project-identity check on the pointer, unlike `[github]`'s reader
  (`pr_link.rs`): the actuator has no store handle, and the checkout is
  already trusted for `scripts/verify-pr.sh` itself — a registered top level,
  read root-only, never climbing.
- A gate that emits no `gate-progress.sh` journal lines runs under
  `VERIFICATION_IDLE_TIMEOUT`'s silence cap alone (derived from the default
  gate's measured contended runtime); `make test` renews it per leg.
- SH-654 ships the verifier scripts with storyhook, so any registered
  checkout is verified; the receipt contract is then the honest boundary of
  "configurable" for a foreign project until SH-665 gives its gate a writer
  to end in.

Tests: `tests/gate_command.rs` (the rule, the reader, the pointer round
trip), `tests/verification_queue.rs` (the shell actuator's argv, the refusal
that spawns nothing, the derived comment text), `tests/merge_gate.rs` (the
public path with a named gate, without one, and the certifies-nothing refusal
with its positive control through the production receipt writer).

### SH-654 — the verifier runs from the daemon's bundle, not the checkout

`ShellVerificationActuator::verify` spawned `bash scripts/verify-pr.sh` with
the registered checkout as its working directory — a repo-relative literal
in a shipped daemon — so the only project that could ever be verified was
storyhook itself; any other registered project halted the queue with
"`scripts/verify-pr.sh` returned invalid JSON". Two leaks inside the family
had the same shape: `verify-pr.sh` sourced `gate-progress.sh` and
`verify-window.sh` from `$root/scripts/` with a silent no-op fallback (so a
foreign project's verification would also have run with no progress
emission, and therefore under `VERIFICATION_IDLE_TIMEOUT`'s silence cap
alone, and no mirror), and `land-pr.sh` reached three siblings through
`$root/scripts/`.

**Built.** `build.rs` writes a second `EmbeddedFile` table,
`EMBEDDED_VERIFIER`, from an explicit list of the ten scripts `verify-pr.sh`
reaches through its own directory (`VERIFIER_SCRIPTS`), with paths relative
to `scripts/`. `src/daemon/verifier_bundle.rs` projects it under the daemon's
store-keyed state directory at `verifier/<payload digest>/` through
`src/embedded.rs` — the SH-538 reuse/stage/verify/rename materializer,
extracted from the plugin installer so both payloads share one opinion about
"the on-disk copy matches this binary" — and sweeps leaves left by earlier
builds. The actuator resolves `verify-pr.sh` from that leaf ahead of the
journal, refusing a bundle it cannot project as a permanent infrastructure
failure; `with_verifier_script` is the injection seam, in `with_paths`'s
shape, and every fixture checkout now holds no `scripts/` tree at all. The
scripts themselves source their siblings from `$script_dir` unconditionally
and refuse a missing one by name. `merge-watch.sh`'s retired sweep body —
unreachable since SH-521 and the only code in the family still spelling
`bash scripts/…` — was deleted rather than exempted.

**Decisions, each with one answer.** *The binary, not the plugin payload*:
the family answers to a daemon↔script wire contract, and the plugin is
provider-scoped and can skew from the daemon (`REQUIRED_DISPATCH_PROTOCOL`
exists because it does); a payload the daemon carries cannot skew from it.
*Content-addressed, not version-keyed*: two builds of one crate version with
different script bytes — any dev build — must never rewrite a directory a
running `verify-pr.sh` is resolving its siblings from, and a different payload
writing a different leaf makes that structural. *The store-keyed state dir,
not the data home*: one store has one daemon (SH-113), so no two daemons ever
contend for a leaf, and a test store's bundle dies with its runtime directory
(`story daemon gc`, SH-638).

**Fences, derived.** `tests/verifier_bundle.rs` reads the table the build
actually wrote: every embedded file is the tracked script byte for byte with
its executable bit; every sibling a bundled script references
(`$script_dir/NAME`, `"$(dirname "${BASH_SOURCE[0]}")/NAME"`, a Python
`from NAME import` naming a tracked sibling) is bundled, so a name missing
from `VERIFIER_SCRIPTS` fails by name; and no bundled script reaches a
sibling through the checkout, comments stripped first.
`tests/verifier_foreign_checkout.rs` is the regression test for the filed
symptom — the production actuator against a repository with no `scripts/`
directory and a fake `gh` answering a closed PR returns the PR's own verdict
from the bundled script — mutation-checked by reverting the spawn to the
checkout-relative literal, which reproduces the filed message exactly.

**Historical limit, resolved by SH-665 below.** Verified is not landed. The receipt contract (SH-649)
still requires the gate to end in `gate-receipt.sh postlude`, and that writer
is storyhook's alone — it enrols this checkout's `.githooks` and refuses
without an executable `.githooks/pre-push`. A foreign project therefore
passes its gate and is refused at the certifies-nothing check, loudly.
Filed as SH-665 rather than adopted: it is a separate mechanism (how a
project-agnostic receipt writer reaches a foreign gate) with more than one
defensible design.

### SH-665 — a portable receipt writer supplied to foreign gates

The approved split keeps hook enrollment in `gate-receipt.sh` and moves its
receipt mechanics into `tree-receipt.sh`. `build.rs` embeds that core and
`tracked-tree.sh`; `merge-watch.sh` sets `STORYHOOK_GATE_RECEIPT` at the gate's
exec boundary. The verifier still neither brackets an arbitrary command nor
promotes its exit status to certification. `merge-preflight.sh` remains the
single merge-certification reader.

`tests/portable_receipt.rs` drives the materialized bundle against foreign
repositories and speculative merge worktrees with spaces in their paths.
It proves gate/full acceptance, no hook enrollment or custom-policy changes,
replacement of an inherited writer path, refusal without a complete bracket,
failed-gate non-certification, changed-tier rejection, tree-drift refusal,
project-local receipts, and successful worktree restoration. Existing push,
merge, and lease tests exercise the same extracted core through its original
wrapper. The bundle dependency fence derives the new transitive dependencies
from the production scripts.

### SH-655 — D-B's "D14's lane budget bounds agents" was not true

**Historical: SH-672 removes the session admission gates described below.**
Engine limits are independent per run; manual concurrency belongs to the
operator. The census remains informational, and the compiler bound remains
machine-wide. See the SH-672 entry in `docs/spec/full-auto-engine.md`.

D-B declined a machine-wide CPU cap as YAGNI on the grounds that D14's lane
budget bounds agents. It bounded *engine* agents: the budget was enforced over
`engine_lanes` rows alone, and a `/story do` typed by hand — the same worktree,
window and cold workspace build — counted for nothing (seven were measured at
load 33 on ten cores, one project active). D-B's row stays as written, per
this document's own rule. What changed: every door that opens an agent session
now measures one census of live agent windows against `ENGINE_LANE_BUDGET`
(`story lane-budget`; `cmd_dispatch` refuses past it, `--over-budget`
overrides; the engine's fill counts it), and compilation itself is bounded
machine-wide by `scripts/rustc-slot.py` through the tracked `.cargo/config.toml`
— so when a second project does appear and its suite overlaps this one's, the
overlap is at most K concurrent rustc processes plus test execution, not
N × 10 compile jobs. D-B's trade-off sentence deserves re-reading against
that: the cap it declined now exists one layer down, on the resource that was
actually saturating. Design of record: `docs/spec/full-auto-engine.md`'s
SH-655 As-built entry and `docs/spec/test-tiers.md`, "The compile bound".

### SH-652 — the completion state is named, never searched for

Built with **different semantics from D-G**, on a council decision recorded
on the story (`story show SH-652` — never the council's own directory,
SH-363). D-G said "first CLOSED state, `STORY_DONE_STATE` override"; what
landed is the required `done`, one pure resolver, no override.

**The resolver is `domain::completion_state`, not a service.** It is a pure
function over the catalog, like `active_state` and
`resting_state_for_closure` beside it, and it answers the catalog's `done`
only while that state is CLOSED — `None` below the SH-125 floor, on which a
writer refuses (`run story doctor --fix`) and the scaffolded `AGENTS.md`,
being documentation, renders the constant. Its consumers: both verifier
writers through one private door, `next_cleanup`'s query, `service::pr_check`,
`service::project::completion_state_slug` (the template, formerly `closed_state`), and the TUI's `>`-key walk in
both components. The helper spells the same constant in its config block, the
way it already spells `verifying`; `tests/plugin_contract.rs` pins the two
equal, so the verifier's write and the helper's reap agree by construction
rather than by two resolvers that happen to match.

**Why not D-G as written.** Three grounds, each measured rather than argued.
(1) An environment override cannot be sound once every service runs inside
one daemon (SH-114): `reap-leased` runs with the *daemon's* environment under
`apply_dispatch_allowlist`, one daemon serves every project so a process
variable cannot carry a per-project fact, and a helper reading its own shell's
value is exactly one client disagreeing with the verifier about one store fact
— the defect this story exists to fix, in the SH-404/SH-411 shape. (2) "The
first CLOSED state" had already failed silently: `service::pr_check` searched
`state_map`, a `BTreeMap`, and so closed every merged story into `closed` —
the abandonment state — on every default catalog since SH-505, while three
comments (the `REQUIRED_STATES` floor and `tests/required_states.rs` twice)
asserted catalog order protected it and `tests/service_pr_check.rs` asserted
only `archived`. Adopted into SH-652 and fixed in its own commit. (3)
Catalog order is documented as layout — the board's columns and where new
stories land (`story help state`) — and SH-521 had already decided, with the
same reasoning, that verification writes the required slug; D-G did not cite
it. A `completion` state role (the council's third candidate) is where a
project would one day name a different completion state; it is a one-function
change inside this resolver, and no project has asked.

**`STORY_DONE_STATE` is refused by name, never dropped** (SH-357): presence is
the request, an empty value included (SH-534), checked ahead of every verb
because the variable reaches the helper from the daemon by `STORY_*` prefix as
well as from a shell. The README, `story help state` and the plugin docs were
corrected in the same change so no surface still promises the knob.

**The class is fenced.** `tests/completion_state_search.rs` scans every
tracked `src/` file, comment-stripped, for a `.find(` whose argument names
`SuperState::Closed` and permits it only inside `domain::completion_state`
and `domain::resting_state_for_closure` — keyed on the enclosing function,
not the file (SH-345). Both permitted sites are positive controls, and it was
mutation-checked in both directions (the pre-fix `pr_check` is flagged in
`run_check`; dropping the resolver's own exemption flags `completion_state`).
Its limit is lexical and stated in its module doc: a search spelled as
`.filter(..).next()` walks past it, which is why every consumer also carries a
behavioural straddle test — a catalog with `shipped` first by position and
`abandoned` first by name — in `tests/verification_queue.rs`,
`tests/service_pr_check.rs`, `tests/service_system.rs`, the TUI component
tests and `plugins/story/tests/test-reap-leased-completion-state.sh`.

**What changes for a user.** A story a person moved into a custom CLOSED
state such as `shipped` is refused by `story.sh reap` as `not-completion-state`
and must be moved to `done` to be reaped — the refusal names that. The
scaffolded `AGENTS.md` now always tells an agent the verifier lands work in
`done`, which is true.

### SH-666 — a halt is the verifier's own, and says so

The incident on 2026-09-10/11 (`docs/rca/verifier-halt-read-as-a-story-block.md`)
was a lockstep failure — an installed daemon older than the registered
checkout's `verify-pr.sh`, refused by name at argument parsing — reported by
this workflow as a story dependency: every waiting candidate carried "Verifier
HALTED since T; blocked by SH-648: …", and the head story's comment said only
that its code "was not classified red". It was filed as a soft block, and the
dashboard's Acknowledge-and-retry was pressed seven times in six seconds
against the same refusal because nothing named the cause's layer or the way
out. The origin is SH-654's (the scripts are embedded in the binary, so the
daemon runs the scripts it was built with — the sixth lockstep component,
`release-lockstep.md`); what this entry settles is the report.

**An infrastructure incident is the verifier's own.** Every permanent
disposition — the script's `die_json` sites and the daemon's own — is a
statement that the verifier could not run, never that a story is wrong; a
story-scoped problem is `InvalidSubmission` and goes back to its implementer.
So the halt policy of SH-573 stands, unchanged: continuing to the next
candidate would have met the identical refusal (SH-627's one-dead-browser
lesson, one tier over). The texts now say what the policy means. The waiting
candidates' line names the incident as "an infrastructure failure of the
verifier itself, first hit while verifying SH-N (SH-N is not at fault)", and
the halted form names the release command; the head comment states that the
halt stops the whole queue, that no story is at fault, and the same command.
`VerificationBlocker` carries `incident_id` (additive on the wire) so both
can print it. "Blocked by" does not appear in either, and
`tests/verification_queue.rs` asserts that absence alongside the words.

**The release path is reachable from where the halt is read.** `story verifier
ack <incident-id>` is the CLI twin of `POST …/verification/ack`. One function,
`service::acknowledge_verification_incident`, serves both doors so their
contract cannot drift (SH-136): the id must be the *current* incident, still
halted rather than retrying, and this project's — a reader of a stale comment
cannot release a newer incident. The id is positional and required for the
same reason; an acknowledgement retries nothing itself, the next tick does.

**A base that moves during the gate is the story's CONFLICT, never a halt.**
The story's own PR met the class a second time: a fetch elsewhere in the shared
repository moved `refs/remotes/origin/dev` while the gate ran, and SH-649's
post-gate certification check re-resolved that ref, computed a different
merge, met a conflict, and halted the queue as "certified nothing" over a tree
it had certified. `verify-pr.sh` now pins `base_commit`/`head_commit` right
after `refresh_submission_refs` and hands those to the preflight, the gate and
`require_certified_by_gate`, so the check asks exactly whether the gate
certified the tree it ran on; a base that has moved is found by `land-pr.sh`
under the merge lock and answered as the story's own conflict.

**The rule this settles, by operator determination (2026-09-11).** A story
under verification whose merge conflicts **holds the queue**: the implementer
is notified, reconciles, and resubmits, and the verifier proceeds with its
remaining steps for that story — the conflict queue-hold above, which exists so
a story with conflicting changes is not bypassed by every later story each time
it comes back up. **Every other story-scoped failure returns the story** to its
implementer and the verifier proceeds to the next candidate (RED, invalid
submission). A halt is reserved for the verifier's own inability to run.

**Stated limits.** The receipt seam (an embedded `merge-preflight.sh` reading
what a merge tree's `gate-receipt.sh` wrote) is the same contract shape one hop
over and stays tolerated; repeated acknowledgements against an unfixed cause
are not rate-limited, the message is the fix; an acknowledgement resets the
incident's attempt count, so the journal is the history.

### SH-647 — the verifier submits

Built as decision D-A describes, with one correction the story as filed did not
carry: submission runs on **every** leased generation, not only when no PR is
linked, because after a RED return the agent only commits and the push is what
reaches the remote (push and adopt are idempotent; an in-tick guard prevents a
double push, and a linked PR whose number differs from the adopted one returns
the story naming both).

The helper verb is `story.sh submit` (leased, verifier-only; refuses
`submit-requires-lease` without the lease). The actuator gained
`VerificationActuator::submit` and `ShellVerificationActuator::submit_leased`,
spawned under a third environment allowlist, `apply_submission_allowlist` — the
dispatch surface (it runs `story`) plus `GITHUB_CREDENTIAL_MAY_SEE`, the three
names now shared with the verification list. The tick records through
`record_generation_submitted` and re-derives the candidate (`refresh_authority`'s
`Current` arm now returns the re-read candidate) so verification runs against
the linked PR as the store folds it, never a `PrLink` built in Rust. The PR
title is `<id>: <title>`, so `land-pr.sh`'s merge commit body carries the id for
`commit-sync`; the Codex charter's old PR-title clause retired with it. The
dead-pane handling of a returned submission is SH-650's, unchanged here.

### SH-648 — per-project verifier and locks

Built as D-B states, with five choices the decision did not name and two
limits stated rather than glossed.

**Scope by name, in the script.** Whether a lock carries the project is
declared inside `machine-lock.sh` (`gate`, `merge` → project; everything else
→ machine), not chosen by a caller flag: a caller that forgot a flag would
silently over-serialize, and the script already applies name-specific policy
(`gate`'s idle ceiling). **One root, hashed key**, over a lock directory
inside the common dir (`coverage-watch.sh`'s shape): one root keeps
`STORYHOOK_LOCK_DIR` a uniform test override, lets a test prove non-collision
under a shared root, and keeps every lock inspectable in one place. **The
whole hash**, never a chosen width (SH-394). **Refusal outside a repository**
for a project-scoped name, never a silent widening to the machine. **The
reentrancy variable carries the full key** and `--held` is its only reader
outside the take; `land-pr.sh`'s own parse of it is gone. The daemon runs a
**supervisor with one thread per project** rather than one thread
multiplexing projects, because overlap is the point; it holds the live-worker
set across "read the catalog, spawn what is missing" and across a worker's
own retirement, so a project deleted and re-registered under the same rowid
never has two workers. Migration 35 rebuilds `verification_incident` keyed by
`project_id`, carrying the singleton row forward; the table is a leaf, so
the rebuild runs under live foreign-key enforcement.

**Limits.**

- The tmux verifier mirror (`scripts/verify-window.sh`) is one fixed pane by
  SH-545's council decision, so two projects verifying at once show whichever
  started last. Best effort and non-fatal; a window per project is filed
  separately because it revisits a council verdict.
- A project registered while the daemon runs gets a worker on the
  `Change::Catalog` wake, and otherwise within one `RECOVERY_WAKE` (30s).
- Two projects' suites now contend for CPU on one machine, as D-B accepts;
  `VERIFICATION_IDLE_TIMEOUT` is silence-based and needs no re-derivation,
  but its measured contended maximum (873s) predates cross-project overlap.
  SH-655 is where the machine's compile bound lives.

Tests: `tests/machine_lock.rs` (the derivation pinned independently from a
repository, a linked worktree and a symlinked path; two repositories do not
serialize; two worktrees of one clone do; the refusal; `release-observer`
stays machine-scoped; `--held`; per-project reentrancy),
`tests/merge_gate.rs` (`--held gate` from inside the speculative checkout —
the proof that the inner `run-tests.sh` take is reentrant with the outer
`verify-pr.sh` hold), `tests/store_migrations.rs` (migration 35),
`tests/verification_queue.rs` (two projects verify concurrently, both visible
as owned; a halt in one leaves the other draining and is acknowledged only
through its own route; a conflict hold in one does not hold the other; queue
position counts one project; the supervisor follows the catalog).

## Manual verifier controls — SH-668

Each project stores admission permission separately from failure incidents.
The default is running; an operator stop survives daemon restart. Stopping
does not change queued stories, agent lanes, or another project's verifier.

| Action | Contract |
|---|---|
| Let inflight verifications finish | Disable new admissions; finish the owned attempt, including its reconciliation hold. |
| Stop inflight verifications | Disable admissions and latch cancellation on the owned attempt. The worker terminates and reaps its subprocess group, and releases reconciliation waits. |
| Start verifier | Enable admission after stopped work has exited. Never acknowledge a failure implicitly. |
| Leave verifier stopped | Validate and acknowledge the exact halted incident and disable admission in one transaction. |
| Acknowledge and retry | Validate and acknowledge the exact halted incident, then permit another attempt. |

The header displays stop while running or draining, allowing drain to escalate
to cancellation. While cancelling it displays stopping; play becomes available
once the worker has released ownership. A halted incident remains visible until
explicit acknowledgement. Legacy CLI acknowledgement preserves manual permission.

The council chose worker-owned cancellation over API-side signalling (decision
and complete reasoning recorded on SH-668). The shared activity registry serializes
admission and control mutations, always locking before store access. Only permission
is durable; draining/stopping derive from live ownership and an attempt-scoped,
monotonic token that survives reconciliation-generation replacement. No registry
lock is held during subprocess work or waits. Cancellation is distinct from
infrastructure failure; uncertain external merges are recovered on restart through
the existing authoritative PR checks, never presumed absent because a child exited.

REST: `POST /api/repos/{project}/verification/control` accepts `action` of
`start`, `drain`, or `stop`. `/data` includes `verification_control` with its
derived `state`. `/verification/ack` accepts optional `action`: `retry` or
`leave-stopped`; omission retains the legacy acknowledgement contract. Mutations
return confirmed state; the UI refreshes after failures or ambiguous transport.

## Withdrawing active verification — SH-686

An operator may move a story out of `verifying` while its gate runs. That
withdraws the exact generation's authority: the verifier cancels its subprocess,
finishes owned cleanup, discards its outcome and proceeds to current queued work.
A rapid departure and resubmission also invalidates the old generation. Ordinary
comments, priority changes and another project's changes do not withdraw it.

Monitoring belongs only around the blocking verification attempt. Subscribe
before checking authority; recheck on project/catalog/resync notifications and
an absolute recovery deadline that unrelated events cannot postpone. Stop and
join the monitor before verifier-owned repair or completion transitions. An
observation error cancels the attempt and reports context after cleanup.

Each attempt has a fresh cancellation signal, separate from manual stop, which
remains irreversible across reconciliation generations. State withdrawal does
not disable project admission. Existing generation-guarded writes still fence
completion races. Preserve the operator's state and any uncertain cleanup
evidence; never classify withdrawal as failed tests or successful verification.

The Python lifecycle owner must install signal handling before releasing its
child handshake, settle its recorded lifecycle and gate sessions before the
outer termination deadline, and retain ownership until cleanup is established.
The design council's complete decision is recorded on SH-686.
