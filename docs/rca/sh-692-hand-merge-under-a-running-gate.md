# SH-692: A terminated verification gate still merged its PR and closed its story, with no verdict recorded

## Reported failure

Filed 2026-09-12 04:09Z: PR #791 (SH-690) merged at 02:18:16Z while its
central-verification gate was at leg 226 of 4058; SH-690 closed `done` at
02:18:27Z; both `pr-791-result.*` files contained `143`; the story carried no
`CENTRAL VERIFICATION GREEN` and no `RED`. The gate's log already showed
`an_armed_daemon_dies_by_sigkill_rather_than_by_its_own_abort ... FAILED`,
and that failure reached `dev` (bisected to `ac7f2aef2`, #791's own merge;
repaired as SH-693). The filing asked, first, WHO merged.

## Evidence

Sources: the daemon activity journal
(`~/.local/state/storyhook/daemons/eab76ca58d086ca4/activity/2026-09-12.jsonl`,
UTC), the story export (event kinds and timestamps), the verifier's artifacts
under `.git/storyhook/` (`verification-logs/`, `gate-receipts/`,
`verifier-lifecycle/`), GitHub's PR timeline, and the session transcripts of
the two agents that were running. Local time is −0700.

| When (UTC) | Fact |
|---|---|
| 02:06:35 | Daemon PID 60718 started: `story 2.4.2 (build 449cda7c5ad9)` — the tree of `dd67d579b` (SH-686 and SH-687 in, SH-690 not). |
| 02:11:01 | SH-690 → `verifying` (generation 33032); the verifier opens #791 and starts its gate (attempt `HRK6k3`). |
| 02:11:26 | The Codex agent runs `story move SH-690 verifying` again (an operator-directed resubmission after a lease repair): generation 33036. The SH-686 observer cancels the 4-second-old gate: `machine-lock: received SIGTERM`, `make: *** [test] Terminated: 15`, `merge-watch.sh process finished status=143`. `pr-791-result.MHFwtb` = `143`. Journal: "verification generation Some(GlobalSeq(33032)) is superseded and has no outcome authority; transferring to Some(GlobalSeq(33036))". |
| 02:11:30 | Attempt 2 (`PbzAyG`) starts; `merge-preflight: not certified — no receipt for tree ad953f39…`. |
| 02:15:57 | `StoryAwaitingCleared` on SH-690, written inside the daemon with no `rpc` line — a dashboard action. |
| 02:17:35 | Last PROGRESS comment: rust-suite 221/4053, running. |
| 02:18:16 | #791 merged by `mikeyward` (GitHub timeline). No `land-pr.sh` ran for #791 that day; the newest receipt in the store is 18:42 local for the base tree. |
| 02:18:27.435 | `StoryStateChanged state=done` + `StoryClosedAndArchived` on SH-690, inside the daemon, no `rpc` line, no `StoryPrMerged`, no GREEN. |
| 02:18:27.586 | The observer cancels the running gate; `status=-15`; `pr-791-result.tf8pbX` = `143`; the owner record is finalised. No "verification outcome" line is ever written for SH-690 — the only run that day without one. |
| 02:19:27 | The supervising Claude session, idle since 02:14:50, notices: "SH-690 shows `done`. Verifying that's real before I act on it, since its last comment still shows a gate running." |
| 02:22:19 | The operator, to that session: "We're golden. Run the full suite one more time…" |

Who merged, by elimination: not the verifier (no receipt, no `land-pr.sh`, no
GREEN, no `StoryPrMerged` — its completion writes all four atomically); not
the GitHub poller (it writes `StoryPrMerged` first); not either agent session
(one stopped at 02:11:31, the other was idle); not `story move` from a
terminal (every CLI move logs `rpc set-state started/completed`; the dashboard
REST door is the one mutation path with no such line). The operator confirmed:
a hand merge on GitHub, then the card dragged to Done.

## Mechanism

Three independent facts stacked:

1. Nothing stopped a hand completion of a story under verification, and
   nothing recorded a reason. The dashboard's drop, like every door, went
   straight to `set_state`.
2. The SH-686 cancellation discards the withdrawn attempt's outcome and, until
   this story, wrote nothing: the story's last word stayed "running".
3. The script layer could misread a killed gate. `merge-watch.sh` published
   the dead child's status, `143`, as a completion record, satisfying
   `verify-pr.sh`'s one guard (record == status); and `verify-pr.sh` carried
   no trap, so when its group was terminated bash's default action ended it
   between merge-watch's return and its `rm -f` of the record — the two
   orphan `143` files are that. The daemon never read the exit status either:
   a verifier killed before answering was "invalid JSON", permanent.

Direction (b) of the filing was already true: `land-pr.sh --certified-run`
merges only after `merge-preflight.sh` certifies the exact tree.

## Corrections

- `057f5307c` — script layer: no completion record for a signalled child; gate
  status ≥ 128 is infrastructure/retryable naming the signal; a TERM/INT/HUP
  trap in `verify-pr.sh` removes the record and emits one JSON verdict naming
  the phase, to a saved copy of stdout.
- `390ed356a` — daemon: `CENTRAL VERIFICATION WITHDRAWN —` on every authority
  loss (naming leg, counts, elapsed, and why), PROGRESS rewritten as
  INTERRUPTED on a stop; a signal death of the verifier is retryable and named.
- service and doors: `verifying → done` requires a reason
  (`CENTRAL VERIFICATION OVERRIDDEN — <why>`), backstopped in
  `append_and_fold`; the poller records an uncertified merge and never
  completes a `verifying` story; an overridden story is reaped once its pull
  request is recorded merged.
- dashboard: a required-reason prompt on the Done drop and the drawer's state
  select.

## Adopted

Fixed here rather than filed, each in its own commit with its own test:
merge-watch's record for a signalled child; the orphaned result files; the
daemon ignoring the verifier's exit status; the reap ineligibility of a
hand-completed story.

## Stated limits

- A hand merge on GitHub cannot be prevented from this repository; a branch
  ruleset requiring a status check would, and is the operator's call.
- The TUI's move carries no comment; a `verifying → done` move there shows
  the refusal. A TUI prompt is a separate surface.
- The out-of-band merge itself is still classified by the verifier's existing
  entry path: a receipt for the merged tree, or a named halt.
