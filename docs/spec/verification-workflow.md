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
| 1 | storyhook pushes and opens the PR | the **agent** pushes, runs `gh pr create`, `story link-pr`, then `story move <n> verifying` (both charters in `story.sh`, the scaffolded `AGENTS.md`); `VerificationProblem::{MissingPullRequest, MultiplePullRequests, UnregisteredPullRequest}` catch a partial submission **after** the fact and return the story | SH-647 (D-A) |
| 2 | one verifier per project; suites serialize per project | **one global worker** (`VerificationActivity`, a single slot) over a queue spanning every project (`ordered_candidates`); the `gate` and `merge` locks are keyed by **name only**, so two clones and two unrelated repositories serialize together | SH-648 (D-B) |
| 2 | order by priority then queue age | priority → `created_at` → project slug → story id (`sort_candidates`); `verifying_since` is carried on the candidate and never sorted on | SH-651 (D-F) |
| 2 | not mergeable → instruct the agent and hold the queue | as stated, **but only while the dispatched pane is alive**; a dead pane releases the hold and parks the story with `awaiting` — see "The conflict queue-hold" | SH-650 (D-E) |
| 3 | gate configurable per project | `.storyhook.toml` `[verify] gate`, default `make test`; the daemon reads it, `verify-pr.sh` requires it as argv, the GREEN/RED text names it | done, SH-649 (D-D) |
| 4a | red returns to the same window | pasted into the dispatched pane by `story.sh notify`; pane gone → `awaiting` is set, and under Full Auto that is `AgentBlocked` → quarantine → a breaker strike | SH-650 (D-E) |
| 4b | merge, done, reap | `land-pr.sh` merges; the verifier writes the required `done` and `reap-leased` accepts exactly that — one constant, `domain::COMPLETION_STATE_SLUG`, pinned equal to the helper's by `tests/plugin_contract.rs` (SH-652, **deviating from D-G**: see As built) | SH-652 (D-G) |
| 4b | the verifier reaps | `story cleanup` (`workspace-cleanup.md`) is a second reaper with no story-state gate and wider authority (it deletes the remote branch) | SH-653 (D-H) |
| — | (unstated) the verifier serves any project | the verifier script family ships inside the binary and runs from the daemon's own state directory; the checkout contributes its `[verify] gate` and its receipt store | done, SH-654 (filed beside the epic, not in it); landing a foreign project still needs SH-665 |

## Decisions of record

Taken in an unattended session on 2026-09-10, each with one clear best answer
given the stated process or settled by user determination; the reasoning is
repeated here so a reader does not have to open eight stories to see why.

| # | Decision | Why | Child | Status |
|---|---|---|---|---|
| D-A | **The verifier submits.** For a candidate with no linked open close-on-merge PR, the verifier's first step is submission from the cleanup lease's worktree and branch: refuse a dirty worktree (return the story naming the files), push over HTTPS, create the PR against the project's integration branch or **adopt** the one already open for that head, `link-pr`, comment the URL. `MissingPullRequest` becomes a submit step; `MultiplePullRequests` still returns. | Matches the stated order. Submission is derived from store facts (lease + no linked PR), so a daemon restart or a skipped verb cannot lose it; resubmission after remediation needs no extra agent step; and it removes `git push` from the agent's toolchain entirely, which dissolves the push-hook contradiction (D-C) structurally rather than by exemption. Rejected: an agent-invoked submit verb. | SH-647 | open |
| D-B | **Per-project queue and locks; cross-project suites may overlap.** Lock key = the canonical git common dir, hashed into the lock name, so every worktree of one clone still serializes with that clone's verifier and a different repository does not. One verifier worker per project, each with its own ordering, incident halt and conflict hold. | User determination: "project-wide (not machine-wide)". Trade-off stated, not hidden: two projects' suites now contend for CPU on one machine; SH-627's quiesce rule still governs the release tier; no machine-wide cap (YAGNI — D14's lane budget bounds agents). | SH-648 | open |
| D-C | **The user-level push hook delegates** to repositories whose `core.hooksPath` names a tracked `pre-push`. | The PreToolUse hook `~/.claude/hooks/pre-push-tests.sh` ran `make test` on every agent push under an 840s budget and waited on the verifier's own `gate` lock (628s measured on SH-640, budget breached, `SKIP_PREPUSH_TESTS=1` reached for). Deleting the hook was rejected: other projects have no gate of their own. | done, **outside this repository** — the hook lives in no tracked file, its verdict token is `delegated`, and nothing in this suite fences it. Proven both ways on the day: this repository → delegated, exit 0; a plain repository with a red `make test` → blocked. | done |
| D-D | **The gate command lives in `.storyhook.toml` `[verify] gate`**, default `make test` when absent; a value that is not a plain argv is refused by name (the SH-357 rule). | A fact about the checkout, versioned with the Makefile it names, belongs in the repository rather than in store settings. E2E stays off the verification allowlist; a project that wants the browser tier names `make test-full` as its gate. | SH-649 | done — see "SH-649" under As built for the receipt contract this put on the value |
| D-E | **A dead pane triggers a resume re-dispatch, never parking.** On a notify refusal the verifier dispatches the same story with the resume clause into the same window name and worktree, then delivers the diagnosis as the first turn; `awaiting` is set only if the re-dispatch itself is refused. The conflict hold applies unconditionally. | Step 4a says the same window. Parking classifies as `AgentBlocked` under Full Auto and strikes the breaker for what is ordinary remediation. | SH-650 | open |
| D-F | **Queue age is `verifying_since`**: priority → `verifying_since` → project slug → story id. | At equal priority an old story resubmitted repeatedly permanently outranks a newer one that has waited longer; `verifying_since` is already the documented honest queue-wait fact (SH-524) and is the only one that resets on resubmission. | SH-651 | open |
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
| Both charters tell the agent to push, open the PR and link it; nothing deterministic does | `PROMPT_TPL`, `AUTO_PROMPT_TAIL` (`plugins/story/bin/story.sh`) | 441, 504 | SH-647 |
| `link-pr` only records a URL; it never pushes or opens anything | `PrLinkService::link` (`src/service/pr_link.rs`) | module doc | SH-647 |
| The user-level PreToolUse push hook ran `make test` on every agent push and waited on the verifier's own lock | `~/.claude/hooks/pre-push-tests.sh` (untracked) | — | done (D-C) |
| One global worker; a queue spanning every project | `VerificationActivity::acquire` (`src/daemon/verification.rs`); `ordered_candidates` (`src/service/verification.rs`) | 88-110; 650 | SH-648 |
| `gate`/`merge` keyed by name only under `$HOME` | `scripts/machine-lock.sh` lock root | 219-225 | SH-648 |
| Tiebreak is `created_at`; `verifying_since` exists and is not it | `sort_candidates`; `verifying_since`, `verifying_entry` | 742-750; 99-106, 632-644 | SH-651 |
| The conflict hold is released when the paste fails | `wait_for_reconciled_candidate`, `return_for_repair`; `tests/verification_queue.rs::a_failed_conflict_notification_releases_the_reservation` | 1246, 1190-1213; 1439 | SH-650 |
| `make test` is a literal in the gate invocation and in the comment text | `run_verification_gate` call (`scripts/verify-pr.sh`); GREEN and RED format strings (`src/daemon/verification.rs`) | 673; 896, 996 | done (SH-649): `gate_command_for` (`src/service/gate_command.rs`), `verify-pr.sh <pr-url> -- <gate…>`, `{gate}` in both strings |
| `scripts/verify-pr.sh` is a repo-relative literal in the daemon | `ShellVerificationActuator::verify` | 521 | SH-654 |
| RED pastes into the dispatched pane; a dead pane parks the story | `cmd_notify` (`story.sh`); `set_generation_awaiting` fallback in `return_for_repair` | 3182-3215; 1204-1210 | SH-650 |
| The verifier writes the literal `done`; reap requires the first CLOSED state | `record_generation_merged`, `record_merged`; `story_closed_state`, `cmd_reap_leased` (`story.sh`); `{done_state}` in `src/service/templates.rs` | 195, 523; 3435-3441, 4072-4078; 48 | SH-652 |
| `story cleanup` reads every story, gates on no state, and deletes the remote branch | `CleanupService::run` (`src/service/cleanup.rs`); daily trigger in `src/daemon/cleanup.rs` | 97, 482-487; 64-70 | SH-653 |
| `machine-lock.sh`'s header said its callers did not exist; specs counted two lock names; `merge-watch.sh`'s strip list silently depended on not stripping `STORYHOOK_MACHINE_LOCKS` | the header; `full-auto-engine.md` "Central verification and machine locks"; the `env -u` list | 9-12; 574; 283-289 | this document |

## The mechanism as it stands

This section describes the tree **today**, so that a reader who arrives before
SH-647..SH-653 land is told the truth rather than the target. Each child
replaces the paragraph it changes and says so under "As built".

### Submission

The agent commits, pushes its branch, opens exactly one close-on-merge PR
against the integration branch (`dev` here — `development-branch.md`), records
it with `story link-pr`, comments the URL, and runs `story move <n> verifying`
as its last action. `verifying` is a required OPEN state (SH-521); the
transition captures a cleanup lease naming the worktree and branch
(`src/service/story.rs`), which is what the verifier later reaps from.
A submission that is not exactly that shape is diagnosed by
`VerificationProblem` when the candidate is picked, not when it is made, and
returned to the agent with the diagnosis as a comment and a pane paste. The
`.githooks/pre-push` gate reports and never refuses on a feature branch
(SH-429), so the push itself is not gated by the suite. After SH-647 the
agent's last action is unchanged and everything before `story move` except the
commit moves into the verifier.

### The queue

`VerificationQueue::ordered` (`src/service/verification.rs`) folds every
project's stories in `verifying` into candidates and sorts them with
`sort_candidates`: priority rank, then `created_at`, then project slug, then
story id. `verifying_since` is computed by `verifying_entry` from the story's
own `StoryStateChanged` history rather than `updated_at` — the progress
checklist rewrites `updated_at` on every publish (SH-524) — and is reported,
but not sorted on (SH-651). The daemon runs **one** worker
(`poll_verification`, `src/daemon/verification.rs`); `VerificationActivity` is
a single slot and asserts if acquired twice. A halt (an infrastructure incident
recorded for the current generation, `record_generation_incident`) therefore
stalls every project's queue until it is acknowledged or the story's
generation changes. SH-648 makes the worker, the ordering, the halt and the
hold per project.

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
as returned, moves the story back to `in-progress`, comments the diagnosis, and
asks the actuator to `notify` — `story.sh notify`, which finds the story's
tmux window, refuses with `pane-unavailable`, `pane-changed` or
`pane-provider-unknown` when the window is gone or no longer runs the
dispatched agent, and otherwise pastes the diagnosis and presses submit.

**If the paste succeeded, the verifier holds.** `wait_for_reconciled_candidate`
keeps the (today: global) worker reserved for that story and re-observes the
queue on every change-bus wake until the same story presents a **newer**
`verifying_generation` — the agent's resubmission — then transfers the
reservation to it and continues in the same tick. Other arrivals cannot take
the slot; a daemon stop ends the wait without manufacturing a candidate. This
is step 2's "wait, holding the queue", and it is pinned by
`tests/verification_queue.rs` (the reservation, the generation check, and that
a wrong candidate is an error rather than a transfer).

**If the paste failed, the verifier does not hold.** `return_for_repair` falls
back to `set_generation_awaiting`, the reservation and the activity slot are
both released, and the story sits in `in-progress` with an `awaiting` reason
until a person re-dispatches it
(`a_failed_conflict_notification_releases_the_reservation`). Under Full Auto
that `awaiting` classifies as `AgentBlocked`, the lane is quarantined and the
breaker takes a strike (`full-auto-engine.md`, "The verifying handoff"). A
dispatched pane is **normally already dead** at the handoff — the launch
command is exec'd into the pane and exits with the agent — so this is the
common path, not the exception, which is what SH-650 changes: a refused paste
becomes a resume re-dispatch into the same window, and the hold applies
whether or not the first paste landed.

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
`gate` (or `full`) receipt through `gate-receipt.sh postlude`, so the tree the
verifier just certified needs no second run anywhere else
(`selective-testing.md`, `test-tiers.md`).

**A gate must certify the tree it ran on.** Landing asks `merge-preflight.sh`
for a `gate`/`full` receipt before it merges, and the receipt is minted by the
suite itself, so a configured gate that does not end in `gate-receipt.sh
postlude` (`make test-changed`, a bare test runner) exits 0 having certified
nothing. `verify-pr.sh` re-asks `merge-preflight.sh` — the same reader, never
a second parser of the receipt file — immediately after a green gate and
refuses by name (`require_certified_by_gate`), rather than letting the refusal
surface downstream from `reconcile_land_refusal` as "no longer has a
qualifying release-gate receipt" after GitHub had been asked again. What is
**not** configurable, and stays that way: the receipt tier a merge accepts
(`gate` or `full`, never `changed` — a council decision on SH-429), and the
verification allowlist of environment names the daemon lets through to the
gate.

### Red

`VerificationOutcome::TestsFailed` carries the tree, the per-attempt log path
under `<common-dir>/storyhook/verification-logs/`, and a detail excerpt. The
verdict is confirmed against the PR's **current** head immediately before it
is posted (`confirm_judged_head`, SH-637); a head that moved during the run is
retryable, never a verdict about a commit nobody can act on. The worker then
calls `return_for_repair` exactly as for a conflict — comment, pane paste,
`in-progress` — but without the hold: a red story re-enters the queue on
resubmission and waits its turn (step 4a). The dead-pane fallback is the same
as above and is closed by the same child.

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
names are live:

| Name | Taken by | Around |
|---|---|---|
| `gate` | `scripts/run-tests.sh` (every `make test`, re-exec'd under the lock); `scripts/verify-pr.sh` (the whole speculative run) | the suite |
| `merge` | `scripts/land-pr.sh` (asserted by its private phase, never re-taken) | preflight, merge, verify, branch delete |
| `release-observer` | `scripts/release-watch.sh` (`release-observer.md`) | one observer pass |

The key is the name alone, so today every clone and every unrelated repository
on the machine serializes on one `gate` and one `merge`. SH-648 adds a project
component derived from the canonical git common dir.

Reentrancy is by environment: a successful take exports
`STORYHOOK_MACHINE_LOCKS="<held>:<name>"`, and a later `machine-lock.sh` in the
same process tree that finds its name there runs without re-taking. This is
load-bearing for the verifier in a way nothing had written down:
`verify-pr.sh` holds `gate`, `merge-watch.sh` execs `make test` inside it,
`make test` reaches `run-tests.sh`, and `run-tests.sh` re-execs itself under
`machine-lock.sh gate`. That inner take succeeds only because the variable
crossed `merge-watch.sh`'s `env -u` scrub. **`merge-watch.sh` must never strip
`STORYHOOK_MACHINE_LOCKS`** (nor `STORYHOOK_GATE_PROGRESS_ACTIVITY_PATH`, which
the lock consumes to report its own wait): a holder is judged by liveness, not
by a clock, there is no `--max-wait` on the gate, and the outer holder is
provably alive — so the inner take would wait forever and every verification
on the machine would deadlock against itself. `land-pr.sh` reads the same
variable as a proof that its private phase runs under `merge`. The variable's
format is known to `machine-lock.sh` alone (`run-tests.sh` deliberately does
not parse it — the SH-136 rule); the invariant here is only that it
**survives**.

### What the verifier cannot do, stated rather than glossed

- It **lands** only a project whose gate can mint a receipt. Since SH-654 the
  verifier itself runs against any registered checkout (see "The verifier
  runs from the daemon's bundle" under As built), but the receipt contract
  SH-649 put on the gate can only be met by `gate-receipt.sh postlude`, and
  that writer lives in this checkout and enrols this checkout's
  `.githooks` — so a foreign project passes its gate and is then refused, by
  name, at the certifies-nothing check. SH-665 owns that gap.
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

**Limit, stated.** Verified is not landed. The receipt contract (SH-649)
still requires the gate to end in `gate-receipt.sh postlude`, and that writer
is storyhook's alone — it enrols this checkout's `.githooks` and refuses
without an executable `.githooks/pre-push`. A foreign project therefore
passes its gate and is refused at the certifies-nothing check, loudly.
Filed as SH-665 rather than adopted: it is a separate mechanism (how a
project-agnostic receipt writer reaches a foreign gate) with more than one
defensible design.

### SH-655 — D-B's "D14's lane budget bounds agents" was not true

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
