# Full Auto declared working lanes stalled, blocked their stories and halted the run

- **Date**: 2026-09-10 (first occurrence 2026-09-08)
- **Severity/Impact**: Eight `stalled` verdicts written by the engine since it first observed a
  real lane, **eight false** — every one on an agent that was alive and working. Each verdict
  set `awaiting` on a story a person had not blocked (the dashboard's "hard block"), counted
  toward the breaker, and re-filled the lane with the next story while the "stalled" agent kept
  running, so a four-lane run briefly carried five agents. Two runs halted by breaker
  (`b04b00c1`, 2026-09-08; `a64495a6`, 2026-09-10 — SH-643, SH-647, SH-649 quarantined in one
  pass, SH-650 spared only by having been dispatched 2m20s later). No data lost; every affected
  story kept working and reached `verifying` on its own.
- **Status**: Fixed on `worktree-SH-657`. Council verdict on the one design question — keep the
  hard stop and widen its evidence, versus demote it to advisory — recorded on SH-657
  (`story show SH-657`) and restated below.

## Summary

The engine's stall detector read one progress signal, the story's change-feed position
(`stories.head_global_seq`), and declared a lane stalled when that had not moved for
`STALL_CEILING_SECS` — 288 s, derived as `ENGINE_LANE_BUDGET × GATE_MEDIAN_SECS × STALL_MARGIN`
(4 × 36 × 2). The derivation bounded a lane's *test leg*: the constant's own doc said the
longest legitimate silence was "its own `make test` run queuing behind other lanes". But the
clock never measured test time. It measured time between story events, and an autonomous
agent writes nothing to the store between its dispatch comment and the plan comment its
charter tells it to post *after* plan approval — 267–616 s across thirty dispatches on this
tracker on 2026-09-05 alone, and far longer during implementation, where the next event is a
commit link or the PR link. A store silence has no bounded legitimate span, so no ceiling over
the store alone can be derived from anything.

**The lesson: a ceiling derives from the deadline it disproves, and the deadline has to be one
the clock actually measures. A bound on a test leg says nothing about a clock that reads the
store, and a channel with no bounded legitimate silence cannot be a hard-stop detector on its
own.**

## Timeline

| When | What | Anchor |
|---|---|---|
| 2026-08-30 | The stall clock ships: `last_progress_seq`/`last_progress_at` on `engine_lanes`, `classify()`'s stall row, and the ceiling derived from the suite median. | `520db0998`, `5789f024f` |
| 2026-08-31 | SH-521 moves the full suite out of the lane; the ceiling's doc is rewritten to say the median is now "a generous bound on a lane's much smaller test leg". The number stays. | `9a6e70a75` |
| 2026-09-02 – 09-05 | Three engine runs, days long, zero stalls. The store shows the same 300–600 s post-claim silences on every dispatched story — but every claim in those runs carries `actor = story.sh:dispatch`: the engine's own in-process fill (a blank-actor claim) never happened, so no lane was ever observed. | `events` table, `actor` column |
| 2026-09-08 11:00 local | SH-609 lands: engine fills work. | `2a45cec76`, `de9bddc17` |
| 2026-09-08 15:46:24Z | Run `3c987e92` fills lane 0 with SH-388 — the first blank-actor engine claim in the store. | `events` seq 23313 |
| 2026-09-08 16:03:11Z | SH-388 declared stalled, 329 s after its last event (a commit link). The agent comments 8 s later, clears the block itself at 16:04:22Z, links its PR and moves to `verifying` at 16:04:47Z. SH-389 is dispatched into the same lane at 16:03:11Z. | `events` seq 23401–23417 |
| 2026-09-08 19:24 – 19:37Z | Run `b04b00c1` (one lane): SH-613, SH-391, SH-392 each stalled ~6 min after dispatch; breaker trips; run halted. | `engine_runs.recent_quarantines_json` |
| 2026-09-10 18:30:37Z | Daemon restarts on build `89f604316fa5`. | daemon journal |
| 2026-09-10 18:31:31–37Z | Run `a64495a6` fills lanes 0–2 with SH-643, SH-647, SH-649. Each story's last store event is its "Dispatching…" comment at 18:31:34–39Z. | `engine_lanes` rows |
| 2026-09-10 18:34:59Z | Lane 3 filled with SH-650. | `engine_lanes` |
| 2026-09-10 18:37:19Z | One reconcile pass: lanes 0–2 read `elapsed > 288` on an unmoved seq → three `Stalled` quarantines, `awaiting` set on all three stories, `consecutive_hard_stops = 3`, `stop_reason = breaker-tripped`. All three agents are working in their windows; tmux's activity stamp on each is under one second old. | `engine_runs`, `StoryAwaitingSet` events 27703–27705 |
| 2026-09-10 18:37:37Z | Operator acknowledges the halt; SH-657 filed at 18:41:01Z. | `engine_runs.acknowledged_at` |

The daemon's activity journal carries only `reconciliation started/completed` for the engine.
The verdicts and their evidence live in `engine_runs.recent_quarantines_json`, `engine_lanes`
and the stories' own `StoryAwaitingSet` events — read before the description was trusted, per
this project's SH-630 rule.

## Root cause & trigger

**Root cause.** `classify()` (`src/service/engine.rs`) had one progress channel, and the
ceiling over it was derived from a quantity that channel does not measure. The constant was
built on the model "a lane is silent while its tests run"; the clock actually implements "a
lane is silent while its story has no new event". Those coincide only for an agent that
writes to the store every few minutes, which no autonomous agent does: the charter's first
store write after dispatch is the plan comment, and the plan comes after reading the code.

**Why it was latent for nine days.** The detector shipped on 2026-08-30 but observed no real
lane until SH-609 made engine fills work on 2026-09-08 — before that every run's stories were
dispatched by `story.sh` directly (`actor = story.sh:dispatch` on every claim), and the
reconciler had no occupied lane to judge. The first lane it ever judged past the ceiling was a
false positive, and so were the next seven. Not a regression: `git diff v2.4.0 v2.4.2 --
src/service/engine.rs` touches no stall line.

**Trigger.** Any engine lane whose agent goes 288 s without a store write, which is every
engine lane during planning.

## Contributing factors

- **The derivation doc was rewritten twice without the measurement being re-asked.** SH-521
  changed which run the ceiling described and kept the number, on the reasoning that the
  suite median was "generous" for a smaller leg. Generous for a test leg; irrelevant to the
  channel.
- **A false verdict did more than misreport.** Below the breaker, `reconcile_pass` clears the
  quarantined lane and re-fills it (D10), so a wrongly stalled story's window stayed alive
  while a new agent was dispatched beside it — the lane budget was exceeded by construction,
  the machine-saturation shape SH-655 was filed for the same day.
- **Nothing showed the number the verdict was made on.** `last_progress_at` reached the wire
  but no surface rendered "quiet for N", so a person could not see a healthy lane approaching
  the ceiling. SH-418's shape: a check whose evidence nobody sees.
- **The fences pinned the spelling, not the premise.** `the_stall_ceiling_is_spelled_as_its_
  derivation` and `…_still_derives_from_the_measured_suite_median` proved the constant was
  written as its three factors and that the median matched `timings.md`. Both were green
  throughout; neither could ask whether the median bounded what the clock read.

## The fix

The pane is the second channel. tmux keeps its own timestamp of the last write to a window's
pty, `#{window_activity}`; measured for this story on tmux 3.7c, it was under one second old on
every working agent window and hours old on every idle Claude sitting at a prompt. During a
long *foreground* tool call the pane goes static (70+ s observed), so the longest a live agent
can be silent on **both** channels is one foreground tool call — which the host bounds:
Claude Code's Bash tool ceiling is 600 s (`timeout … max 600000` ms). That is a deadline to
derive from, and the only one in evidence.

- `WINDOW_PROBE_FORMAT` gains `#{window_activity}` as a fourth field; `WindowProbe::Alive`
  carries it as `last_output_at`, `None` when tmux answered it empty (absence states nothing,
  SH-372). The fake tmux and the browser harness's doubles answer the field.
- `LaneObservation` gains `seconds_since_output`; `classify()` declares `Stalled` only when
  the seq is unmoved past the ceiling **and** the pane has been silent past the ceiling. An
  unknown pty channel leaves the store to judge alone, which preserves SH-626's backstop for a
  dead-but-unobservable lane.
- `record_progress()` restarts the clock from the pane's own stamp when the store did not
  move, never rewinding it; `last_progress_at` now means "last observed activity, store or
  terminal", with no schema change.
- `STALL_CEILING_SECS = HOST_TOOL_CALL_CEILING_SECS × STALL_MARGIN` = 600 × 2 = 1200 s; the
  tick stays a quarter of it (300 s). `GATE_MEDIAN_SECS` leaves the engine.
- The engine makes the bound its own rather than a cited host default: every Full Auto dispatch
  carries `STORY_LANE_TOOL_CEILING_MS`, and `story.sh` pins it on the lane's window as
  `BASH_MAX_TIMEOUT_MS`. One constant, two consumers — the environment the agent runs under
  and the ceiling that judges it.
- A stall reason names both channels' measurements and the ceiling; `story engine status` and
  the dashboard lane strip show each lane's quiet time continuously.

**Council** (three seats — architecture, QA, observability — unanimous in round one): keep the
hard stop and widen its evidence. Advisory-only was rejected by every seat as trading a bounded
false positive for an unbounded livelock — four idle lanes holding a run forever, uncounted by
the breaker, in the one product that exists to run unattended. Adding a process-tree activity
signal now was rejected as premature persisted state for an unmeasured case that CPU-time
delta cannot see when the task is I/O-blocked.

## Preventative action — killing the class

- `tests/engine_reconcile.rs`: the taxonomy rows for the second channel, the incident replayed
  through a real store and the fake dispatcher (store silent past the ceiling, pane writing:
  no quarantine, no breaker increment, clock restarted from the stamp), the true stall with its
  evidence, a stale stamp never rewinding the mark. Mutation-checked: disabling the pty clause
  fails two rows, disabling the reseed fails the replay.
- The derivation fences now refuse the old factors by name: a ceiling spelled from
  `ENGINE_LANE_BUDGET` or `GATE_MEDIAN_SECS` fails the build with the reason.
- `HOST_TOOL_CALL_CEILING_SECS` is pinned at 600 with its provenance, so moving it is a
  decision that re-asks the measurement rather than drift.
- The quiet-time surface: the next wrong ceiling is visible as a number climbing on a healthy
  lane before it is a quarantine.

**Stated limit, not glossed.** An agent whose *turn ended* waiting on a background task is
silent on both channels for the task's whole duration, which the host does not bound. Lanes
are forbidden from running `make test` and their own legs are small, so no such wait has been
measured to exceed 1200 s; a process-tree activity signal would cover it and is filed
separately, gated on that measurement. Codex CLI's render cadence during tool execution is
also unmeasured; the Codex arm relies on the pty channel on the same terms as Claude's until
it is measured, as SH-459 measured Codex before its own arm shipped.

## Lessons

- **Derive the ceiling from what the clock reads.** A bound on some other quantity, however
  well measured, is a bare literal wearing a derivation.
- **A channel with no bounded legitimate silence is not a detector on its own.** It can
  withhold a failure verdict (a moved seq is always progress) but cannot issue one.
- **Read the journal and the store before trusting the description** (SH-630). The story
  said "stalled and hard-blocked"; the `engine_lanes` rows said 340 s of store silence on an
  agent whose pane had written one second before the pass.
- **A detector that has never fired has never been proved.** Nine days of green runs were
  nine days of no lane observed; the first real observation was the incident.
