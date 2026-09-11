# Live-daemon portfile fixtures omitted rewrite isolation

- **Date**: 2026-09-07 PDT / 2026-09-08 UTC
- **Severity/Impact**: PR 701 could not pass centralized verification. No release, user, runtime, or data impact occurred.
- **Status**: Fixed in `3104dc230`

## Summary

Central verification of PR 701 rejected three tests that mutated a live daemon's portfile without disabling the daemon's asynchronous Tailscale reprobe. The latent fixture defect entered in `c505802d2`; SH-597's dashboard-only commit `665a5ea27` exposed it to verification but did not cause it. Commit `3104dc230` applied the established no-Tailscale PATH guard at all three daemon-start origins. The existing repository-wide portfile-fixture hygiene contract remains the class-level regression guard.

## Timeline

- **2026-09-07 16:09:08 PDT** — Commit `c505802d2` added forced-stop coverage, including three fixtures that started daemons before corrupting, removing, or redirecting their live portfiles without `path_without_tailscale`.
- **2026-09-07 18:58:45 PDT** — SH-597 commit `665a5ea27` compacted the dashboard Open PR pill and added its focused UI and tap-target coverage; it did not change daemon lifecycle or portfile fixtures.
- **2026-09-07 PDT / 2026-09-08 UTC** — Central verification of PR 701 on merge tree `72472545eedb6cd00e1558bf6a9e3fcd8a2f17c3` failed `portfile_fixture_hygiene::every_mutation_after_a_daemon_start_is_guarded_against_the_daemons_own_rewrite`. The earlier comparison stopped before this contract ran, so it did not establish a green result for the latent violation.
- **2026-09-07 PDT / 2026-09-08 UTC** — A light RCA reproduced the same three reported locations 4/4, verified that each match represented executable start-before-mutation behavior, and ruled out a textual false positive and force-stop ordering as protections.
- **2026-09-07 19:35:40 PDT / 2026-09-08 02:35:40 UTC** — Commit `3104dc230` added the canonical PATH isolation to the three fixture daemon starts. The exact contract and all three directly affected runtime tests passed.

## Root cause & trigger

Commit `c505802d2` added three forced-stop fixtures that started daemons with the ordinary test PATH and then mutated their portfiles. A reachable Tailscale executable allowed the daemon's independently scheduled tailnet reprobe to call `write_info` after those mutations, potentially undoing a corrupt portfile, recreating a removed one, or replacing a silent-peer redirect before force-stop consumed the fixture state. The static hygiene contract classified each function as a live-daemon mutation without the required `path_without_tailscale` guard and deterministically reported all three sites.

SH-597 did not introduce this chain. Its UI commit caused PR 701's merge tree to enter centralized verification, where the repository-wide contract exposed the pre-existing fixture defect. The verifier's earlier comparison ended before this contract, so the reported delta did not mean that the preceding tree had exercised and passed this invariant.

ODC classification: **Assignment/Init / Missing / configuration + concurrency**. The fixture command lacked required PATH initialization; the trigger combined a reachable Tailscale configuration with an independently scheduled writer.

## Contributing factors

- The affected fixtures depended on mutated portfile state while their daemons remained live.
- Tailnet reprobe and force-stop fixture setup had no ordering edge, so ordinary PATH made the late rewrite reachable.
- The omission existed in three tests from one forced-stop change, while the repository-wide hygiene fence detected the pattern only when centralized verification reached that contract.
- The comparison run stopped before this test, obscuring the distinction between a newly introduced failure and a newly observed latent violation.

## The fix

Commit `3104dc230` imports and retains `path_without_tailscale` at each of the three daemon-start origins in `tests/daemon_lifecycle.rs` and `tests/daemon_timeouts.rs`. Each shim remains alive for the fixture lifetime, preventing asynchronous tailnet discovery from rewriting the deliberately corrupt, missing, or redirected portfile. The repair addresses fixture initialization rather than weakening the scanner or changing daemon production behavior.

The verdict was **SURGICAL**: two test files and three fixture starts changed, with no product code, shared helper, contract logic, or unrelated daemon start modified. The exact contract, the corrupt-portfile fixture, the missing-portfile fixture, and the silent-control-peer fixture all passed after the change.

## Preventative action — killing the class

The existing class-level test `portfile_fixture_hygiene::every_mutation_after_a_daemon_start_is_guarded_against_the_daemons_own_rewrite` remains unchanged. It scans tracked Rust fixtures and fails whenever a function starts a daemon, then mutates its portfile without mentioning `path_without_tailscale`. This repository-wide fence detected all three siblings and will make the same initialization omission loud at verification time.

## Lessons

- A fixture that mutates a live daemon's portfile must disable Tailscale discovery at daemon startup; stopping the daemon later does not order an independently scheduled rewrite.
- Static contract failures can expose latent defects in code unrelated to the submitting feature; merge-tree attribution must distinguish exposure from causation.
- A comparison that stops before a contract runs is not evidence that the earlier tree passed that contract.
- Pattern-level guards are most valuable when repairs preserve the guard and correct every reported origin instead of adding exceptions.
