# SH-774: Stop Now could not stop a Full Auto run

## Failure

On 2026-09-25 the operator pressed Stop Now on run `22e4276a` (storyhook, one
Codex lane). The dashboard showed "reset already in progress or lock
unavailable: Resource temporarily unavailable (os error 35)". The run stayed
`draining` until the operator reset story SH-769 by hand.

| Time (UTC) | Event |
|---|---|
| 23:09:38 | The engine claims SH-769; its dispatch runs 58 s |
| 23:09:49 | Stop Now takes the per-run reset lock, writes `draining`, and waits for the dispatch |
| 23:09:49–23:10:36 | The alert shows; Abandon run sends a second Stop Now, which fails on the busy lock |
| 23:10:36 | The dispatch is refused; the lane is quarantined with the story claimed and no cleanup lease |
| 23:10:37–23:12:28 | Every attempt, from the REST call and then back-to-back daemon reconciles, fails: "no cleanup lease; cannot reset legacy lane" |
| 23:12:45 | A card reset of SH-769 frees the lane; the run finishes one second later |

Evidence: the store's `events`, `engine_runs` and `engine_lanes` rows, the
daemon activity journal, and the birth time of the reset lock file.

## Causes

1. **Wedge by design, on a false premise.** SH-706 made the cleanup lease
   the only authority for destructive cleanup. It refused a lane without a
   lease, and a test asserted that refusal. It assumed that only pre-lease
   "legacy" lanes lack a lease. But a refused dispatch creates such a lane
   every time, and the breaker halts a run in that shape. No retry can make a
   lease appear, so the refusal repeated forever. Lanes whose story was
   purged, and lanes whose story another cleanup owned, refused in the same
   way.
2. **A duplicate request was an error.** The non-blocking per-run lock
   rejected a second Stop Now with the raw OS error. The first request writes
   `draining` before it waits, so the dashboard offered Abandon run while the
   first request still ran.
3. **Self-waking retries.** Each attempt rewrote the run's `updated_at`. The
   change watcher compares whole run records, so each failure woke the next
   attempt.
4. **Orphaned dispatches.** A dispatch error, or a failed awaiting write,
   left a lane dispatching forever. Stop Now waited 180 s for such a lane on
   every attempt, and on the daemon's single engine thread that wait blocked
   every project.

## Class killed

"A Stop Now target that no retry can change keeps the run draining."
`reset_now` now maps every occupied lane to exactly one `StopTarget`: Settled,
Reset or Deferred. None of them refuses forever. The lease remains the only
authority for destructive cleanup. A lane without one is released with its
work kept and explained. See `docs/spec/full-auto-engine.md`, "SH-774 — Stop
Now always converges".

## Regression evidence

Each fix was committed with its tests. Each test fails on the pre-fix source
and passes after the fix:

- `tests/engine_reset.rs`: `a_repeated_failing_stop_now_does_not_rewrite_the_run_record`,
  `stop_now_on_a_finished_run_returns_it_unchanged`, the duplicate in
  `stale_probe_and_duplicate_stop_cannot_compete_with_the_reset_owner`, and
  the incident replay `stop_now_during_a_dispatch_that_is_then_refused_finishes_the_run`.
- `tests/engine_run_model.rs`: `immediate_stop_releases_a_leaseless_lane_and_preserves_its_story`
  (reverses the SH-706 test), `immediate_stop_keeps_an_existing_quarantine_diagnosis`,
  `immediate_stop_releases_a_lane_whose_story_was_deleted`,
  `reset_authorization_skips_a_lane_whose_story_is_gone`,
  `stop_now_defers_to_a_card_reset_and_finishes_after_it`,
  `a_deferred_lane_does_not_hide_another_lanes_failure`,
  `stop_now_releases_an_orphaned_dispatching_lane_without_waiting`,
  `stop_now_frees_an_orphaned_dispatch_that_a_card_reset_waits_on`.
- `tests/engine_reconcile.rs`: `stop_now_after_the_breaker_trips_on_refusals_finishes`,
  `a_failed_dispatch_is_quarantined_and_counted_like_a_refusal`,
  `a_dispatch_refused_after_a_card_reset_reserved_its_story_releases_the_lane`,
  `a_dispatch_result_never_overwrites_a_lane_released_meanwhile`.

## Lesson

A safety refusal must name the condition that lets a retry succeed. If no
retry can succeed, the refusal is a wedge: release the work with its evidence
kept instead of refusing it again.
