# SH-592 public timeout compatibility alias failed verification

- **Date**: 2026-09-07
- **Severity/Impact**: PR 694 could not pass centralized verification. No release, runtime, user, or data impact occurred.
- **Status**: Fixed in `d7716c841`

## Summary

SH-592 renamed the verifier timeout to `VERIFICATION_IDLE_TIMEOUT` so its name
matched the new idle-silence behavior. Commit `004aa8f7e` also retained the old
name as a deprecated public alias after migrating every tracked caller. The
repository's dead-public-surface contract rejected that unreferenced export on
central-verifier merge tree `1c8feff07156c9a1744c4341ae06ec5eda21e394`.
Commit `d7716c841` removed only the alias; runtime behavior did not change.

## Timeline

- SH-592 introduced progress-sensitive verification supervision and renamed the
  operational timeout for its idle-silence semantics.
- Commit `004aa8f7e` migrated production and test consumers to
  `VERIFICATION_IDLE_TIMEOUT`, then added `VERIFICATION_TIMEOUT` as a deprecated
  public compatibility alias.
- Central verification of PR 694 on merge tree
  `1c8feff07156c9a1744c4341ae06ec5eda21e394` failed
  `dead_public_surface::every_pub_item_has_a_call_site`, naming only the alias.
- A light RCA reproduced the failure, traced the declaration and call sites,
  challenged competing explanations, and toggle-verified the causal chain in a
  disposable worktree.
- Commit `d7716c841` removed the unsupported alias and restored the focused
  contract while retaining SH-592's runtime behavior.

## Root cause & trigger

The defect chain was:

1. `004aa8f7e` declared a bare-public compatibility constant after all tracked
   callers had moved to the new name.
2. The static scanner included that declaration and found no other
   whole-identifier reference in tracked Rust sources.
3. Central verification exercised the public-surface contract and rejected the
   orphaned item.

Removing only the alias changed the focused test from fail to pass; restoring
it reproduced the same failure. The contract predates the alias, and every
operational caller correctly uses the new idle-timeout name.

ODC classification: **Interface / Extraneous / configuration-static-contract
trigger**. The unsupported declaration was extra API surface at the crate
boundary; the repository's static configuration contract exposed it.

## Contributing factors and AND conditions

The failure required all three conditions: the file was tracked source, the
declaration matched the scanner's bare-public-item grammar, and the identifier
had zero tracked call sites. Runtime timing, concurrency, and deprecation did
not contribute.

The compatibility alias was retained conservatively even though the repository
contract defines unreferenced Rust API as unsupported. No crates.io package,
generated use, workspace omission, or actual external consumer was found.
Unknown private git/path consumers remain possible, but they are unsupported
under the enforced contract; this RCA does not claim they cannot exist.

## Fix

Commit `d7716c841` deleted only the deprecated `VERIFICATION_TIMEOUT` alias from
`src/daemon/verification.rs`. It did not change the scanner, manufacture a call
site, or alter the operational constant and runtime paths. This surgical fix
removes the invalid state at its origin and preserves the accurate
`VERIFICATION_IDLE_TIMEOUT` name.

## Preventative action

The existing class-wide test
`dead_public_surface::every_pub_item_has_a_call_site` remains the regression
guard. It rejects future public Rust declarations without tracked consumers,
including unsupported compatibility aliases. The repair keeps that contract
unchanged instead of adding an alias-specific exception that could conceal the
same defect class.

## Lessons

- A deprecated export is still public surface and must satisfy the repository's
  supported-consumer contract.
- Compatibility aliases need evidence of a supported consumer; hypothetical
  external use alone does not override an enforced no-dead-public-API policy.
- For static contract regressions, a single-variable remove-and-restore test can
  distinguish the triggering declaration without involving runtime behavior.
- Directly impacted test selection must include repository-wide architectural
  contracts, not only tests for the changed runtime subsystem.
