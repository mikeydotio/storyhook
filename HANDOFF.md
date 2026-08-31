# Handoff

## Completed in SH-471

- The dashboard keeps every unacknowledged Full Auto stop as a persistent,
  project-scoped banner outside the timed notice stack.
- Each banner shows run/state/reason/streak diagnostics and the three newest
  recoverable quarantine outcomes with story links.
- Acknowledgement is guarded, ambiguity-safe, stale-read-safe, and reconciled
  against the engine status endpoint.
- Browser coverage pins reload durability, acknowledgement, stale status,
  historical ordering, press safety, and notice-dock separation.

## Next

- Continue the Full Auto epic from `story next`; reconciliation and close-out
  stories remain the source of truth for lane supervision and real-run proof.
- Preserve the banner DTO contract: it derives entirely from the existing run
  and lane fields, with no separate client persistence.
- Keep engine alerts registered in `NOTICE_BAND_TOP`; otherwise the fixed
  notice dock can cover their diagnostics and acknowledgement controls.
