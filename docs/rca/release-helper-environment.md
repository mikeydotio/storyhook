# Parent release routing leaked into build and test children

- **Date**: 2026-09-16
- **Severity/Impact**: SH-737's v3.0.2 release gate was blocked during validation; no published-release impact or observed user data loss. Elapsed incident duration was not established.
- **Status**: Fixed in `1e31b97dcacb44c06ac50eebddbc82eb7f47b829`; full gate passed. Release publication is tracked by SH-737.

## Summary

The release gate failed a real submission-receipt test after central verification passed. Child launch boundaries removed GitHub credentials but retained parent binary and repository routing selectors. Those selectors displaced fixture-owned routing, causing existing identity and verification guards to refuse submissions. The fix isolates children at launch while preserving parent authority and allowing explicit child-owned routing.

## Timeline

| Date | Event and evidence |
|---|---|
| 2026-09-16 | `15dc433df` introduced release/observer origin-authority helpers. Earliest latent fixture susceptibility is not established. |
| 2026-09-16 | SH-734 central verification passed at `149e032687435d00e0e4d73a7444acdee41d6938`; the first v3.0.2 release gate subsequently failed under the release environment, recorded in SH-737. |
| 2026-09-16 | Clean/binary-only launch toggles produced exit statuses `0,1,0,1`; independent expected-identity and authority probes also failed. Independent architectural challenge upheld the boundary diagnosis. |
| 2026-09-16 | `1e31b97dc` repaired child isolation. Targeted regression checks and `make test-full` passed; the sibling sweep filed ambiguous terminal ownership separately as SH-738. |

## Root cause & trigger

**ODC: Interface / Missing; trigger: nested build/test launch inheriting parent orchestration environment.**

1. **Defect:** `github_without_credentials` and observer preflight omitted `STORY_BIN`, `STORYHOOK_GITHUB_AUTHORITY`, and `STORYHOOK_GITHUB_EXPECTED` from child isolation. Corrected boundaries are `scripts/github-access.sh:48`, `plugins/story/lib/github-access.sh:48`, and `scripts/release-observer.py:137`.
2. **Infection:** inherited `STORY_BIN` bypassed the fixture's PATH adapter; authority/expected-identity selectors instead pinned the parent checkout. Isolated selector probes and the reversible launch toggle establish this dependency.
3. **Failure:** six bootstrap receipts refused with `not-verifying`; isolated routing probes also exposed origin mismatch. Current v3.0.2 regression RED reached a daemon identity refusal and observer exit `93`.

The bootstrap reproduction used v3.0.1. These different guard outcomes establish the inherited-routing boundary defect, not identical historical provenance. The release invocation exported routing that central verification stripped, explaining the validation difference.

## Contributing factors

Credential isolation did not express the complete orchestration boundary. Nested fixtures intentionally owned their binary adapters and repository identities, making inherited parent routing incompatible. The version bump coincided with exposure but was not established as the cause.

## The fix

**SURGICAL:** `1e31b97dc` expands the existing boundary policy across both shell adapters and observer preflight. All eight credential/routing selectors are removed only from children; parent values, unrelated variables, explicit child assignments, and exact failure statuses survive. No guards were relaxed.

Targeted GREEN covered nine shell tests, 15 Python observer cases, and 48 successful real receipts with independently checked remote heads. `make test-full` exited `0`: 1,317 browser passes and 15 existing platform keyboard-setting skips. Formatting, lint, core Rust, build, and plugin legs reused results only under the existing content-fingerprint policy; the original receipt test passed the prior fresh, unchanged-input core leg. Gate log: `/tmp/SH-737-full-gate-parked-catalog.log`.

Separate release findings fixed in `2ad0586c3` (PR fixtures) and `9b277ab8e` (catalog interaction/fixtures) were not causes of this defect.

## Preventative action — killing the class

| Executable contract | Location |
|---|---|
| `test_children_cannot_inherit_orchestration_selectors` | `tests/github_shell.rs:245` |
| `sanitized_submission_receipts_match_remote_heads_for_all_parent_selectors` — every selector set at once; the child environment does not depend on the subset (SH-783) | `tests/github_shell.rs:309` |
| `plugin_and_verifier_ship_the_same_thin_adapter` | `tests/github_shell.rs:225` |
| `test_preflight_does_not_receive_orchestration_selectors` | `tests/support/release_observer.py:79` |

The independent sweep found no remaining confirmed in-scope leaks. SH-738 owns unresolved terminal routing semantics; no provider-pane defect was demonstrated.

## Lessons

Child isolation must cover routing as well as credentials. Executable boundary contracts capture this constraint; no AGENTS.md addition is necessary.
