# Target accounting rejected Cargo's internal dSYM aliases

- **Date**: 2026-10-10
- **Severity/Impact**: One SH-872 baseline attempt was interrupted during Rust compilation and produced no accepted timing sample.
- **Status**: Fixed in `a22964d8a6b4f0cef9568443affc94f7547223e4` and `f907958632151e18177e674153b5ad224fb3c5ef`; native wrapper and full baseline validation remain pending.

## Summary

The SH-872 storage observer rejected an internal Cargo dSYM alias, causing its supervised gate to stop. Target accounting prohibited every symbolic link, although packed debug output can contain an alias to a physical bundle within the same target. The repair recognizes that specific relationship without traversing the alias and applies it consistently to live accounting and settled disposal. A follow-up repair includes the new helper in the embedded verifier bundle. Output validation must distinguish recognizing an ordinary producer artifact from following an arbitrary link.

## Timeline

- **2026-10-09 02:13 UTC** — `a23a8657b334c5038b6b871319bed1a7e8a905f1` introduced the storage guard with blanket target symlink refusal.
- **2026-10-09 23:57 UTC** — The SH-872 baseline at `4c8f6eaffbf4b5062060c7807f9420e77a766cda` stopped during Rust compilation after formatting and clippy passed. The observer reported `symlink in measurement storage` for `debug/story.dSYM`; the retained alias pointed to a physical bundle under `debug/deps`.
- **2026-10-10** — A real-filesystem regression reproduced the refusal. A separate, minimal Cargo build with packed debug information produced the same alias layout. Independent review supported the rejection mechanism while retaining uncertainty about the historical artifact's creator.
- **2026-10-10 01:49 UTC** — `a22964d8a6b4f0cef9568443affc94f7547223e4` committed the accounting repair and 23 passing new Python tests. An isolated corrected/pre-fix/restored experiment passed, reproduced the exact refusal, then passed again with unchanged test assertions.
- **2026-10-10 01:53 UTC** — A sibling integration review found the new helper missing from the embedded payload. `f907958632151e18177e674153b5ad224fb3c5ef` added it and a regression that first reproduced `ModuleNotFoundError`. All 24 new Python tests passed after the correction.

## Root cause & trigger

The verified chain is:

1. A normal packed Cargo debug layout can include `debug/<stem>.dSYM` pointing to `deps/<stem>-<16 lowercase hex>.dSYM`. The independent native probe demonstrated this producer behavior; the retained failed target contained that relationship.
2. `usage` in [scripts/gate_measurement_storage.py](../../scripts/gate_measurement_storage.py) rejected the alias solely because it was a symlink. The real-filesystem regression and isolated repair/revert experiment establish this branch directly.
3. The refusal escaped `check_storage` through the campaign health callback in [scripts/gate_measurement_campaign.py](../../scripts/gate_measurement_campaign.py).
4. [scripts/gate_measurement_execution.py](../../scripts/gate_measurement_execution.py) interrupted the supervised gate and propagated the error. The attempt remained charged and incomplete.

**ODC classification:** Interface / Incorrect; triggered by normal packed debug output becoming visible during live observation. The deterministic observer mechanism is established; attribution of the historical alias to a particular creating process remains probable, with medium confidence. Its creator was not traced. This is distinct from the previously repaired [descendant disappearance race](storage-scan-race.md).

## Contributing factors

- Packed output, alias presence, and blanket link refusal had to coincide during observation.
- The output policy conflated recognizing an alias with traversing its contents.
- Settled disposal shared the same accounting boundary and therefore needed the same explicit artifact contract.
- Adding a Python helper also required updating the embedded script manifest; source-checkout imports alone did not exercise that packaging boundary.

## The fix

**SURGICAL:** `a22964d8a6b4f0cef9568443affc94f7547223e4` corrects the owning accounting boundary. [scripts/gate_measurement_dsym.py](../../scripts/gate_measurement_dsym.py) recognizes only the demonstrated debug layout, checks the same stem and 16-character lowercase hexadecimal suffix, and validates physical destination directories on the admitted device using directory descriptors and no-follow opens. It rechecks observed identities and link text, then counts only the alias's allocated blocks. The ordinary physical walk counts the destination contents once.

Live `check_storage`, strict `TargetPool.dispose`, and strict `remove_exact` explicitly select this policy. The default policy still refuses links. A live scan tolerates an alias disappearing only when the alias itself is confirmed absent; a persistent dangling alias remains an error. Source-input and evidence policies, quotas, ownership, lifecycle checks, and sensor-error handling retain their existing contracts. This remains a non-atomic observation, not an exact snapshot or a guarantee against every possible mutation.

Commit `f907958632151e18177e674153b5ad224fb3c5ef` adds the helper to `VERIFIER_SCRIPTS` in [build.rs](../../build.rs). Neither repair changes the Cargo profile or retries failed measurements. Four prior failed campaigns remain preserved and charged. The native Rust wrapper and full baseline were not run at this postmortem's validation boundary; focused regression success does not establish performance acceptance.

## Preventative action — killing the class

- [DsymAlias.test_live_target_accounts_internal_dsym_alias_without_following_it](../../scripts/tests/test_gate_measurement_dsym_alias.py) preserves the original real-filesystem reproduction. The isolated repair/revert/restore experiment verified that the accounting change controls its outcome.
- The same 24-test module covers distinct stems, exact allocated-byte accounting, invalid destinations, directory and alias mutation, strict versus live disappearance, descriptor closure, sensor errors, quotas, and actual settled disposal.
- `DsymBundle.test_embedded_bundle_imports_storage_and_accounts_dsym` reconstructs the actual manifest bundle and imports it in isolated Python without checkout imports. It detects the missing-module class and exercises accounting against a real alias and payload.
- [measurement_cargo_dsym_alias_regressions](../../tests/gate_measurement.rs) includes the module in the native integration harness. Its execution remains pending at the stated validation boundary.

## Lessons

Producer output contracts belong at the shared accounting boundary so live observation and disposal agree. Recognize supported aliases without following them, and retain explicit refusal for unsupported layouts. New embedded dependencies need an isolated payload test as well as source-level tests. Preserve interrupted attempts and distinguish a verified repair from an accepted measurement campaign.
