# Handoff

## Completed in SH-524

- Every story sitting in `verifying` now carries a self-updating `CENTRAL
  VERIFICATION PROGRESS —` comment, rewritten in place rather than appended.
  The candidate the release gate is actually running shows a live nested
  checklist (legs, suites, per-test counts, elapsed time); every other
  candidate shows its queue position and what is ahead of it.
- A journal file (`$STORYHOOK_GATE_PROGRESS`), not a new RPC: the daemon
  points the gate's already-sanitized subprocess at it, every gate script
  appends NDJSON when it is set and is a byte-for-byte no-op otherwise.
- `PublishBackoff` (1→2→5→10 minute ladder, resetting on any real change)
  keeps a wedged run from paying ~480 comment rewrites over eight hours.
- A sixth data-dir-isolating harness (`scripts/coverage-map.sh`) surfaced
  during containment work that this project's own CLAUDE.md prose (naming
  five) had not caught up to — fixed, and the derived fence now covers it.
- Docs: `docs/spec/full-auto-engine.md`'s As-built section carries the design
  and every deliberate scope narrowing (no free-text per-item note field yet).

## Next

- A free-text "note" field per checklist item (e.g. "merge preflight — tree
  `abc1234` needs the gate") was drafted but not wired — `scripts/gate-
  progress.sh`'s extra fields are bare JSON tokens with no string-escaping
  support today. Follow-up, not a defect.
- No stall ceiling exists on the gate itself (`verify()` has no timeout);
  this story makes a wedge *visible*, it does not fix one. A future story
  could act on `STALE_GATE_THRESHOLD_SECS` staying elevated.
- Continue the Full Auto epic from `story next`.
