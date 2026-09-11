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
| D-G | **One completion-state resolver** in `src/service` (first CLOSED state, `STORY_DONE_STATE` override) used by the verifier, the template renderer and the helper. | Three spellings of one fact disagree by construction today; a project whose first CLOSED state is not `done` lands every green story, writes `done`, and then fails reap on every attempt, forever, loudly. | SH-652 | open |
| D-H | **`story cleanup` is subordinated to the verifier.** It may touch only a worktree whose story is CLOSED and carries the verifier's CLEANUP COMPLETE or CLEANUP REQUIRED marker (the retry path, never an independent one); it never deletes a remote branch `land-pr.sh` has not already removed; `--dry-run` says what it declined and why. | Step 4b names one reaper. A second, state-blind one with wider authority and no lease is exactly the kind of "two answers from one fact" this project has paid for (SH-136, SH-263). | SH-653 | done — see "SH-653" under As built for the generation the marker is read from, and for the remote branch leaving cleanup's scope entirely |

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
`record_generation_merged`s the story into the state whose slug is the
literal `done` (refusing, with `run story doctor --fix`, if the project has no
CLOSED state so spelled), comments GREEN, and asks the actuator to `reap`:
`story.sh reap-leased`, which re-checks every postcondition from the lease —
including that the story is CLOSED **and** in the project's completion state,
resolved by `story_closed_state` as `$STORY_DONE_STATE` or else the first
CLOSED state — before removing the tmux window, the worktree, the local branch
and the per-story caches. A failed reap comments CLEANUP REQUIRED and is
retried by `next_cleanup`, queried separately from active verification so a
cleanup fault cannot starve the gate. In a project whose first CLOSED state
is not `done`, every retry fails the same way (SH-652).

`story cleanup` (`workspace-cleanup.md`) is the same reap's retry path by
hand, and on the daemon's daily cadence when `cleanup.auto` allows (a missing
stamp counts as due) — never an independent reaper (SH-653). Before any git
work on a candidate it reads the store: the lease must name a story of this
project, the story must be CLOSED, and the story's **latest verification
generation** must carry the verifier's CLEANUP COMPLETE or CLEANUP REQUIRED
comment, read through the same `latest_generation` the verifier's own retry
uses. Only then do the git gates run (clean, unlocked, window closed, worktree
and local-branch tips reachable from the freshly fetched default branch), and
what it removes is what `reap-leased` removes: the worktree and the local
branch. It neither reads nor writes the remote branch, which `land-pr.sh`
deleted at merge time. Every refusal is a skip with a reason, which is what
`--dry-run` prints.

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
| `workspace-cleanup.md` | `story cleanup`'s own preflight and recovery — the reap's retry path since SH-653 |
| `release-observer.md` | the third lock name and the observer that takes it |
| `activity-log.md` | the verifier's tmux mirror and the daemon journal a stalled verification is diagnosed from |
