# SH-579 Handoff

## Implemented

- Independent remote tag audit and repeated real host/Lima preflight observer.
- `make release-watch`, `make release-status`, and configuration-only
  `make release-watch-plist`; no timer installed from this lane.
- Atomic observation records distinguish missing, running, failed, stale and
  successful results. All test tiers print the read-only advisory.
- Historical tag mismatches remain visible; published tags are never repaired.
- Design and optional durable-checkout setup: `docs/spec/release-observer.md`.
- AGENTS.md and its source template remain synchronized. SH-557 owns their
  eventual separation. SH-584 (#668) and SH-585 (#669) are preserved.
- Reconciled origin/main d4b060e6e (SH-581 / #670) with a merge commit.
  The Lima guest builds in the extracted source subshell and preserves the
  caller cwd for export. Both Linux-target regression paths remain intact.
  SH-581 records the original RED evidence and its 59 passing targeted tests.

## Validation and submission

- New contracts use real Git remotes, production locks and status code, with
  only the external preflight represented by a fixture. No Lima starts.
- Targeted runner: `release_observer release_tagging release_targets gate_tiers
  scaffold`; reconciliation outcomes are recorded on SH-579 and PR #671.
- Continue the existing PR: https://github.com/mikeydotio/storyhook/pull/671.
  Preserve published history; no rebase, force-push, or additional PR.
- Central verification owns the full suite, merge, completion and lane cleanup.
- Repair the existing PR without rewriting history if verification returns it.
- Do not run release/version/deployment commands from this worktree.

## Operational limits

- A preflight observation does not prove complete builds or artifact provenance.
- Normal test output reports missing observations until the optional scheduler
  is configured from a durable checkout after merge.
- Do not run manual release assembly concurrently with the observer. Observer
  passes share a machine lock; existing manual release commands do not.
- Preserve `.git/storyhook/verification-recovery-SH-584-20260906`.
