# SH-666: A verifier halt read as a story soft-blocking the queue

## Reported failure

Filed 2026-09-11 01:15Z: "A story can wedge the entire verifier by being
soft-blocked on another story" — a comment on SH-648 appeared to say it was
blocked on SH-650 with no relation recorded, and the centralized verifier was
halted with four stories in `verifying` (SH-648, SH-650, SH-653, SH-654). The
filing asked for a full RCA and for the verifier never to be halted on a story.

## What was actually on the stories

No comment on SH-648 names SH-650. The sentence the operator read is the line
the verifier writes on every *other* queued story while the head is stalled
(`publish_once`, `src/daemon/verification_progress.rs`):

    Verifier HALTED since 2026-09-11T01:09:32Z; blocked by SH-648: usage:
    verify-pr.sh <pr-url> -- <gate-command...> (the daemon passes the
    project's [verify] gate)

It sat on SH-650, SH-653 and SH-654. SH-648 was the story the halt was most
recently hit on — the victim, not a blocker — and the cause is in the same
line: `scripts/verify-pr.sh` refused the daemon's own invocation by name.

## Evidence

Sources: the daemon activity journal
(`~/.local/state/storyhook/daemons/eab76ca58d086ca4/activity/2026-09-1{0,1}.jsonl`),
the `verification_incident` row and the `events` table of the production store,
the main checkout's reflog, `story --version`, and git history. Local time is
-0700; the journal is UTC.

| When (local) | Fact |
|---|---|
| 09-10 07:44 | Main checkout `pull: Fast-forward` to `6c5f50a0f` (the SH-646 merge). |
| 09-10 11:30 | Daemon PID 80908 started from the installed binary: `story 2.4.2 (build 89f604316fa5)`. That build id is the tracked-tree oid of `6c5f50a0f` (SH-406's stamp). It is still the running daemon at the time of writing. |
| 09-10 12:31 | SH-649 merged (`bc5c30ae5`, PR #755). `verify-pr.sh` now requires `<pr-url> -- <gate…>` and answers `{"result":"infrastructure-failure","disposition":"permanent","detail":"usage: …"}` without it. The same commit makes the daemon pass `--`; the installed daemon predates it and spawns `bash scripts/verify-pr.sh <url>` with `cwd` = the registered checkout. |
| 09-10 14:22–14:29 | SH-648 verified RED after a full 407 s gate; SH-650 verified CONFLICT. Both normal: the checkout was still at `6c5f50a0f`, whose script accepted the old argv. |
| 09-10 15:43 | Main checkout `pull: Fast-forward` to `a6c48cec2` (dev HEAD, which includes SH-649). Nothing reinstalled the daemon. From this moment the registered checkout's script and the running daemon disagree on their wire contract. |
| 09-10 16:23 (23:23:48Z) | SH-650 resubmitted after its conflict repair. `verify-pr.sh` answers the usage refusal in 50 ms. `record_generation_incident` records incident `2:28879`, permanent, `halted` on attempt 1 — SH-573's designed behaviour for a permanent disposition. The queue stops. |
| 09-10 16:23 → 18:09 | Nothing verifies. Every queued story's progress comment is rewritten each minute with the "blocked by SH-650" line. |
| 09-11 01:09:26–32Z | Seven `verification started` events for SH-648 in six seconds, each answered with the identical usage refusal, each recorded as a fresh "Attempt 1 of 3 … HALTED". The daemon journal shows no `rpc` request between them; the cadence (~0.75 s) and the incident row being replaced each time match the dashboard's **Acknowledge and retry** button pressed repeatedly: an acknowledgement deletes the incident row, the next tick re-runs the identical invocation, and the new incident starts at attempt 1. The first acknowledgement is also what moved the halt from SH-650's generation to SH-648's — the queue's head after SH-650's conflict hold was released — which is why every line then read "blocked by SH-648". |
| 09-11 01:15Z | SH-666 filed. Incident row: story 648, generation 28821, permanent, halted, attempts 1. |

Mutation-free confirmation of the mechanism: the old daemon's spawn is
`command.arg("scripts/verify-pr.sh").arg(&pull_request.url)` at
`6c5f50a0f:src/daemon/verification.rs:521-523`; the new script's refusal is
`scripts/verify-pr.sh:645-646` on `a6c48cec2`; `git merge-base --is-ancestor
bc5c30ae5 HEAD` is true in the main checkout and false for the tree the daemon
was built from.

## Competing explanations

| Explanation | Verdict |
|---|---|
| SH-648 is soft-blocked on SH-650 by a comment with no relation | Refuted. No such comment exists on SH-648; the "blocked by" text is the verifier's own progress line, and it named SH-650 first, SH-648 after the first acknowledgement. |
| Something about SH-648's code or PR wedges the verifier | Refuted. The refusal fires before the PR is read (`verify-pr.sh` exits at argument parsing, 50 ms after spawn), and fired identically for SH-650 first. Any story would have met it. |
| The daemon's incident retry loop re-attempted on its own | Refuted. A permanent disposition halts on attempt 1 and the tick returns `Halted` on every later wake while the incident row matches the head's generation (`tick_with_reconciliation`). Seven attempts with `attempts = 1` each require the row to be deleted between them, which only `POST /verification/ack` does. |
| The installed daemon and the registered checkout's scripts disagree | Supported by every measurement above, and reproduced by the SH-654 mutation check: reverting its spawn to the checkout-relative literal reproduces this class's message verbatim. |

## Mechanism

The release-lockstep spec (`docs/spec/release-lockstep.md`, SH-530) lists five
components that must arrive together — CLI, daemon, plugin, store schema,
hooks/plist — and made the plugin a projection of a release precisely because a
live view of the checkout skews from the daemon. The verifier scripts were a
sixth component it never listed: `scripts/verify-pr.sh` and its siblings were
invoked from the registered checkout's working tree, so a `git pull` there
changed the daemon's own wire contract underneath a daemon that had not moved.
The refusal that resulted was correct in *detecting* the skew (SH-649 refuses a
missing gate by name rather than assuming one) and correct in *halting* (below).
What was wrong was the origin — that the skew could exist — and the report.

## Why the halt was right, and stays

The filing asked for the verifier to continue with other stories, hard-block the
story, or hand it back. None fits this failure: it is verifier-scoped, not
story-scoped. Every `die_json` permanent site in `verify-pr.sh` (not inside a
git worktree, `jq`/`gh` missing, the usage refusal, a gate that certified
nothing) and every daemon-side `Permanent` (an unrunnable `[verify] gate`, an
unwritable progress journal, invalid JSON, no progress) is a statement about the
verifier's own ability to run. Continuing would have produced the identical
refusal on SH-648, then SH-653, then SH-654 — SH-627's lesson one tier over: a
browser that cannot launch is one dead browser, not N failing tests. SH-573
chose the halt for exactly this reason, and story-scoped problems already go
back to the implementer as `InvalidSubmission`. The halt policy is unchanged.

## Corrections

**Origin — by SH-654 (PR #762), filed independently for the "foreign checkout"
symptom and recorded here as the fix for this incident's class.** The verifier
script family is embedded in the binary (`build.rs` table, the SH-538
materializer) and run from `<daemon_state_dir>/verifier/<content digest>/`; the
checkout contributes only the `[verify] gate` argv and the receipt store. A
derived closure fence pins that every sibling a bundled script references is
bundled, a second fence that no bundled script resolves a checkout-relative
path, and `tests/verifier_foreign_checkout.rs` drives the production actuator
against a checkout with no scripts tree. With that landed and installed, the
daemon runs the scripts it was built with, whatever the checkout is at.

**Report — SH-666.** The waiting candidates' line now reads "Verifier HALTED
since T on an infrastructure failure of the verifier itself, first hit while
verifying SH-N (SH-N is not at fault): <detail>" and names the release command;
the head story's comment says the halt stops the whole queue, that no story is
at fault, and the same command. `VerificationBlocker` carries the incident id
so both can print it. The dashboard's queued chip and banner say "first hit
on", not "at".

**Release path — SH-666.** `story verifier ack <incident-id>` is the CLI twin
of the dashboard's acknowledge, sharing one service function
(`acknowledge_verification_incident`) and its exact-id contract, so a reader of
the halt comment can act where they read it — and a stale comment cannot
release a newer incident.

## Adopted: the certification check halted the queue on a moved base

While this story's own PR was under verification, the verifier halted again —
a second incident of the same class, one seam over, adopted into SH-666 on the
operator's determination (2026-09-11).

| When (UTC) | Fact |
|---|---|
| 02:40:35 | PR #763's verification started against `dev` at `12741cf77`; merge tree `f85a0c64…` computed; gate started. |
| 02:52:10–45 | A `/story do` for SH-670 created its worktree in the shared repository; its fetch moved `refs/remotes/origin/dev` to `7dec28cec` (SH-652 and SH-650 had landed, both touching `CLAUDE.md` and `verification-workflow.md`). Reflog entry at 02:52:45. |
| ~03:01 | Gate green. A `tier gate` receipt for `f85a0c64…` exists in the receipt store: the tree WAS certified. |
| 03:01:34 | `require_certified_by_gate "$tree" "$base_ref" "$head_ref"` re-ran `merge-preflight.sh` on the mutable ref, which now resolved to the new tip: a different merge, a real conflict, a nonzero exit, and the message "exited 0 … but certified nothing", disposition permanent. The whole queue halted on incident `2:29325`. |

Two defects in `scripts/verify-pr.sh`. The check SH-649 added between the gate
and landing was handed `$base_ref`, not the commit the gate ran against —
SH-584's correction ("resolve both transaction refs to commit ids before
checkout, preflight, …") missing from the one call added after it. And a base
that moved was classified as a permanent verifier failure, when it is the most
story-scoped outcome there is.

**The required behaviour, by operator determination (2026-09-11):** a story
under verification whose merge conflicts holds the queue while its implementer
is notified, reconciles, and reports back; the verifier then proceeds with its
remaining steps for that story. Every other story-scoped failure returns the
story to its implementer and the verifier proceeds to the next candidate.
Holding on a conflict is what prevents starvation — otherwise a story with
conflicting changes is bypassed by every later story each time it comes back
up. That is the mechanism `Conflict` already has (`return_for_repair`, the
notify, `wait_for_reconciled_candidate`; SH-521, SH-650), and RED/invalid
already return without holding. The fix therefore reclassifies nothing new: it
makes the moved-base case *reach* `Conflict`. The transaction is pinned to two
commits right after the refs converge, and the preflight, the gate and the
certification check all speak about those parents; landing then refreshes the
base under the merge lock and answers CONFLICT (or a fresh tree to verify).
`tests/merge_gate.rs::a_base_that_moves_during_the_gate_is_a_conflict_for_the_story_never_a_halt`
constructs the incident — a certifying gate that moves both the origin branch
and the remote-tracking ref before returning — and was red with the incident's
message verbatim before the fix.

## Stated limits

- The receipt seam survives SH-654: an embedded `merge-preflight.sh` reads
  receipts a merge tree's own `gate-receipt.sh` writes. That is a cross-version
  contract of the same shape, one hop over, tolerated because both usually come
  from the same commit. SH-665 owns the landing half for foreign projects.
- Repeated blind acknowledgements are not guarded against. The seven retries
  cost six seconds and left seven comments; the correction is the message
  saying what to fix first, not a rate limit on a human.
- The incident row records attempts per incident id; an acknowledgement resets
  the count, so "Attempt 1 of 3" after an ack does not mean the first attempt
  ever. The journal and the retracted comments keep the history.

## Operational recovery

1. `make install` from the main checkout on `dev` — `a6c48cec2` already passes
   `--`, so the installed daemon and the checkout's script agree again. (After
   SH-654 lands, reinstalling picks up the embedded scripts and the checkout's
   state stops mattering.)
2. `story verifier ack 2:28821` (or the dashboard's Acknowledge and retry).
   The queue drains SH-648 → SH-650 → SH-653 → SH-654.
