# Python input capture rejected certifi's canonical public CA bundle

- **Date**: 2026-10-09
- **Severity/Impact**: SH-872 measurement preparation stopped before a campaign could start. The observed impact was incomplete input evidence, not a production trust failure; elapsed impact duration is unverified.
- **Status**: Source fixed through `d854f4c4dade5073bf0bd844938d0ad4c253c84f`; focused Python regressions passed. Native validation and fresh real input capture remain pending.

## Summary

Fresh measurement input capture refused a valid installed Python dependency because certifi's certificate link resolved outside its Homebrew Cellar. The dependency model represented complete package directories but lacked an explicit declaration for this canonical public file. The fix adds one constrained file dependency, preserving refusal of arbitrary external paths and carrying its restrictions through hashing and the final stability audit. No certificates, system trust settings, or production configuration changed, and this fix does not establish performance acceptance.

## Timeline

- **2026-10-08, UTC−07:00**: Commit `be39c5f1bf1aa852b3afe0462394d54123d69cf3` added installed Python package link closure with a Cellar boundary. No successful capture for the subsequently failing installed layout is established.
- **2026-10-09**: SH-872 fresh capture at staged source `cd7c1008c109a92fbe55edddad15b077b187011e` refused the selected Python 3.14.8_1 dependency topology. This exposed an unsupported layout; it does not establish a source regression.
- **2026-10-09**: A synthetic outside/inside/outside target intervention reproduced refusal, admission, then refusal. Independent challenge verified the boundary as the cause and rejected a broken-link explanation for that fixture.
- **2026-10-09**: Commit `3030f617bc4907bb064c34005169fe191b50396d` added the constrained dependency and regressions. All 16 added CA tests and 37 related focused Python tests passed. Native validation and fresh installed-input capture were still pending when this record was written.

- **2026-10-09**: Installed-layout discovery exposed the free-threaded ABI suffix, shared site-packages, and pip-vendored certifi sources. Follow-up regressions reproduced each refusal; all 21 CA tests then passed, and metadata-only discovery of the actual installed closure succeeded. Native validation and complete byte capture remain separate obligations.

## Root cause & trigger

The verified chain was: a valid dependency source resolved to a regular public file outside Cellar → `python_linked_packages` rejected that target → input inventory remained incomplete → measurement setup correctly refused to start. The missing representation was a narrowly typed external file, not permission to read an arbitrary external directory. The trigger was selection of an installed Python closure containing this certifi layout.

ODC classification: **Checking / Incorrect / configuration topology**. The diagnosis is grounded in the explicit rejection branch and reversible target intervention; earlier success with different installed inputs cannot prove this layout previously worked.

## Contributing factors

- Homebrew stores the public bundle outside the versioned certifi package directory.
- Existing closure tests did not exercise this exact package-to-public-file topology.
- Shared dependency traversal can encounter a target repeatedly and in different orders. A declaration alone must not authorize later links from unrelated sources.

## The fix

The change is **SURGICAL**: one dependency representation and its checks, with no migration or trust-store modification. See [`scripts/gate_measurement_inputs.py`](../../scripts/gate_measurement_inputs.py), especially `ca_source`, `open_ca_bundle`, `snapshot_ca_bundle`, and `check_audit`.

Supported physical sources use `lib/python<major>.<minor>[t]/site-packages/` beneath the same Homebrew prefix. Certifi's `certifi/cacert.pem` may be in the shared library or a versioned `Cellar/certifi/<version>` package. Pip's `_vendor/certifi/cacert.pem` may be in the shared `pip` package or a versioned `Cellar/python@<major>.<minor>/<version>` package with a matching library ABI. Every source must resolve to exactly `<prefix>/etc/ca-certificates/cert.pem`. Versions must start with a digit and use the accepted version characters. Lookalike packages, malformed ABIs and arbitrary external paths refuse. Source provenance is checked before an already-declared-root shortcut. Only the canonical file is declared; its enclosing directory is not admitted. Discovery also refuses if the caller omits the typed-file declaration output.

Descriptor-relative opens require every ancestor to be a directory without following symlinks. The leaf must be a regular file with exactly one hard link before any content read. Identity checks detect substitutions and changes during hashing, reopened ancestry checks, repeated references, and final audit. These constraints also apply when recursive package traversal reaches the bundle first. This does not claim comprehensive confinement for unrelated generic directory handling.

Constrained audit entries are typed JSON dictionaries containing ancestor identities and seven leaf stat fields, including link count. Consumers must preserve that representation. The retired private controller that converted every audit value to a tuple is incompatible and must not be reused unchanged.

Rollback is to revert `d854f4c4dade5073bf0bd844938d0ad4c253c84f`, `24279289` and `3030f617bc4907bb064c34005169fe191b50396d` in reverse order, restoring explicit unsupported-layout refusal. It requires no certificate or system configuration changes.

## Preventative action — killing the class

[`scripts/tests/test_gate_measurement_ca_bundle.py`](../../scripts/tests/test_gate_measurement_ca_bundle.py) adds 21 regressions and is registered in the native `throughput_cohort_and_execution_evidence_regressions` wrapper. In particular:

- `test_certifi_canonical_public_bundle_is_an_exact_dependency` verifies inclusion and fingerprint sensitivity to changed bytes.
- `test_wrong_origins_refuse_before_and_after_valid_source` and `test_wrong_origins_cannot_use_an_already_declared_target` protect traversal-order independence.
- `test_ancestor_swap_before_open_never_reads_external_bytes` and `test_leaf_substitution_between_stat_and_open_refuses_before_read` guard read boundaries.
- `test_serialized_inventory_preserves_constraints_in_observe` and `test_late_byte_mutation_refuses_shared_final_audit` preserve restrictions across serialization and final verification.

These focused checks passed; native-wrapper execution and real input recapture remain separate acceptance obligations.

## Lessons

Dependency closure sometimes needs a typed file exception rather than a larger allowed directory. Such an exception must retain provenance and file constraints at every access, independent of discovery order, and its audit representation must survive consumer serialization. A refused installed layout is evidence of unsupported topology, not automatically evidence of a source regression.
