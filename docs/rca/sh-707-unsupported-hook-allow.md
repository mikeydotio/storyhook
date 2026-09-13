# SH-707: Claude approval output is incompatible with Codex hooks

- **Date**: 2026-09-12
- **Severity/Impact**: One reported Codex `PreToolUse` failure; two independently reproduced incompatible emitters. Duration and wider impact are unknown.
- **Status**: **PARTIAL — candidate installed and disabled; native activation pending**. Personal hook repaired locally; Greenlight source repairs pass 127 tests. The supervisor installed the development candidate and passed 20 actual installed controls; the original producer remains enabled. Mikey's native trust and final selection remain under root-supervisor coordination.

The operator-authorized development artifact is now staged and installed. Its
immutable identity, metadata-only source exception, safe supervisor receipt and
remaining native action are in [DEVELOPMENT.md](sh-707/DEVELOPMENT.md).
Do not rerun staging/install or copy private raw configuration backups.

## Summary

Codex reported `unsupported permissionDecision:allow` during StoryHook work.
Greenlight 3.9.1 and the personal Codex hook each independently emit a Claude-specific approval envelope that Codex rejects.
StoryHook v2.4.2 already has the relevant compatibility guard, and its installed and checkout controls pass.
The personal hook now emits neutral Codex approvals. AGE-103 repaired the five historical failures and completed the source regression matrix. Its development candidate is installed but disabled, with 20 actual installed-manifest controls passing. The original Greenlight cache/trust remain unchanged and its producer stays enabled until Mikey's deliberate native activation sequence.
No hook-specific live trace attributes the original incident to either producer.

## Timeline

| Date | Evidence and event |
|---|---|
| 2026-03-30 | Agentics `1d9d4ab1760ed107bd22c0e8cc430c396e261ab2` introduced bare approval output in deterministic and AI-rationale paths. |
| 2026-04-01 | Agentics `a1b63be9c1a06481f3af59bbe0f98ef9f9237021` changed serialization to `jq`, preserving that output contract. |
| 2026-09-07 | StoryHook `d8a99db18b804f12374b8277c0b3ed5698ff907d` (SH-599) neutralized Codex plan approval output. |
| 2026-09-12 | SH-707 reported the diagnostic; isolated production-entrypoint replay reproduced incompatible output from both external hooks. |
| 2026-09-12 | Independent challenge upheld the interface defect while retaining uncertainty about the original incident. |
| 2026-09-12 | Personal repair installed after sandbox approval and passed five direct tests. Greenlight candidate preserved for continuation; AGE-103 records external ownership. |
| 2026-09-13 | AGE-103 source repairs inspected; 127 Greenlight tests pass again against pinned committed source. Normal Codex installer rehearsal passes baseline, repaired candidate and baseline rollback with exact byte/mode parity. |
| 2026-09-13 | Operator authorizes a disposable development identity. Official Codex helpers produce `3.9.1+codex.20260913021137`; real personal-marketplace replacement/rollback preserves eight unrelated plugins and trust witnesses through five states, with 20 installed-manifest controls passing. Live changes remain reserved for supervisor review. |
| 2026-09-13 02:33–02:36 UTC | Supervisor reviews and stages the exact artifact, installs greenlight@personal, and disables only the candidate through the version-protected supported config API. Original producer remains enabled; all preexisting caches and unrelated config/trust remain unchanged. Twenty actual installed-manifest controls pass. Native activation remains unverified. |

Personal-hook introduction history and a last-known-good Codex interval are unknown. No bisect or first-onset claim is justified.

## Root cause & trigger

**ODC: Interface / Incorrect / configuration trigger.** A matching enabled hook classifies a tool request as safe, serializes Claude approval without adapting to its receiver, and supplies Codex with bare `permissionDecision:allow`.
The [official Codex parser](https://github.com/openai/codex/blob/main/codex-rs/hooks/src/engine/output_parser.rs) rejects that decision without `updatedInput`; neutral and context-only outputs preserve ordinary permission handling.
These hooks do not rewrite tool input. Adding synthetic rewritten input would obscure their purpose.

| Link or alternative | Evidence |
|---|---|
| Producer → incompatible envelope | Invoke each production hook with `Bash`/`pwd`; adding only Codex's `turn_id` still produces bare `allow`. |
| Envelope → rejection | Official parser source establishes rejection; fixture diagnostics are authored assertions, **not captured host stderr**. |
| StoryHook regression | Refuted for the tested checkout: Codex plan control returns `{}`; question controls preserve denial. |
| Stale StoryHook installation | Refuted for the inspected installation: all 12 installed/checkout controls pass and SHA256 hashes match. |

Matching hook registration, an approval-classified request, and the Codex host are required together.
Either external emitter suffices independently; repairing one leaves the other available.
The precise configuration or host change that exposed the latent defect in the reported session is unknown.

## Contributing factors

- Greenlight duplicates approval serialization in its rationale-enabled AI branch; changing only `allow()` misses that branch.
- Existing Greenlight tests omitted Codex's discriminator and successful API responses.
- Valid AI `answer:false` is discarded by `jq '.answer // empty'`, preventing the successful-response tests from reaching either approval serializer.
- The Greenlight manifest invocation exits 127 with only Codex's `PLUGIN_ROOT` environment. Direct script invocation cannot prove installed manifest compatibility.
- Personal-hook comments describe branch-switch denial, but current classification passes those commands through. This repair preserves that classifier; its denial regression tests the serializer directly.

## The fix

The selected correction is **SURGICAL**: adapt output at each producer boundary, retain Claude approvals and denials, and retain optional AI rationale as `additionalContext`.
Codex detection uses presence of the `turn_id` key, matching StoryHook's existing approach.
No StoryHook runtime, trust, sandbox, classifier, or release change is part of the
repair. The separately authorized disposable artifact adds only a distinctly
versioned Codex manifest; it changes no Agentics source file or release identity.

| Component | State and evidence |
|---|---|
| Personal Codex hook | Installed at `~/.codex/hooks/git-readonly-allow.py`; SHA256 `710fbb086454438e3e1f13d130a93529290fb71054ef3a1b0f09b7d4d98fe284`. Original preserved at `~/.codex/hooks/git-readonly-allow.py.sh707-backup-20260912T234300Z`. Separate Claude copy compared unchanged. |
| Greenlight source | AGE-103 candidate `af73549175847d158c5a59de772f6cfee7b256f5`, including repairs `11f2ae9`, `2b5e20c`, and `66fe644`. Original partial patch remains historical evidence only. |
| Greenlight installed | Development `3.9.1+codex.20260913021137` is installed as `greenlight@personal` and disabled. Original `greenlight@agentics` remains enabled, with its 3.9.1 cache and trust preserved. See [live receipt](sh-707/installed-disabled-receipt.json) and [native handoff](sh-707/DEVELOPMENT.md). |
| StoryHook | Documentation/evidence only. Installed/checkout hook SHA256: `e3a5175eb77d9c070772494e12fa2d1252aa8b0d43f9a7995bcd95827def2a54`. |

Durable continuation artifacts: [personal patch](sh-707/personal-hook.patch), [personal regression](sh-707/test_personal_hook.py), [Greenlight candidate and regression patch](sh-707/greenlight-candidate.patch), and [comparison evidence](sh-707/evidence.json).

| Validation | Result |
|---|---|
| Original isolated replay | Two Codex emitter failures; both Claude approval controls and all 12 StoryHook controls pass. Repeated deterministically. |
| Candidate combined replay | 16/16 controls pass. Switching sources back to originals restores both failures. This exercises deterministic approvals, not AI or manifest delivery. |
| Installed personal regression | Five tests pass, including neutral approvals, Claude compatibility, passthrough, malformed JSON/unrelated tools, and denial serialization. |
| Historical Greenlight new regression | 11/16 passed; four valid boolean-false AI cases and one manifest environment case failed. Subsequently fixed by AGE-103. |
| Historical Greenlight directly impacted existing tests | 84/84 passed against the initial partial candidate. |
| Current Greenlight source suite | 127/127 pass against committed AGE-103 candidate exported into isolation. No full repository suite run. |
| Current personal installed regression | 5/5 pass; repaired hash remains exact and both original backup and separate Claude copy retain the original hash. |
| Installer and rollback | 12 baseline controls, 20 repaired installed-manifest controls, 12 rollback controls pass. Each stage verifies all shipped files/modes, identity, registration and enablement. |
| Receipt negative controls | 10 tests pass, covering wrong/missing/extra bytes, modes, roots, identities, registration, unsafe links and immutable source export. Git replacement-view regression failed before explicit original-object reads. |
| Development packaging and staging | Four metadata and five staging tests pass; modified original bytes/modes, release identities, archive/receipt drift, existing destinations and symlinked ancestors are refused. An altered sidecar reached the scaffold before the regression fix; it is now rejected before destination writes. |
| Development installation and rollback | Five real installer transitions preserve all eight unrelated fixture plugins and trust entries; 20 installed-manifest controls pass. Duplicate-enabled intermediate states and required new native trust are explicit, not hidden. |
| Running-session cache preservation | Follow-up rehearsal uses the native plugin toggle's supported config API; selection and rollback retain both Greenlight caches exactly, all unrelated plugins and trust. A stale expectedVersion returns configVersionConflict without mutation. Live handoff uses native plugin selection, not cache removal. |
| Supervisor live installation | Safe receipt records 20/20 actual installed-manifest controls passing, candidate disabled, original enabled, every preexisting cache and unrelated config/trust unchanged. Native-host validation and release certification remain false. |
| Greenlight syntax and patch | `bash -n` and `git apply --reverse --check` on the durable candidate patch pass. |
| Greenlight static analysis | `shellcheck --severity=warning` fails six SC2221/SC2222 warnings in overlapping `wc`, `terraform state`, and `helm repo` arms. Untouched Agentics HEAD reproduces the same six; no new warnings, but baseline remains unclean for AGE-103. |
| StoryHook impacted-test selector | Actual staged tree selects `ALL`: baseline `7d8c74f5d3b2de80fe6e44c1db2e89ff4dd98247` has no coverage map. Full-suite execution remains with the central verifier; only direct regression checks were run here. Agentics has no impacted-test selector. |

### Remaining work and exact resume procedure

1. Read SH-707's newest operator comments and repeat the complete obviation review. Preserve all committed preparation and original evidence.
2. Read AGE-103 and AGE-84 for current combined-source certification. AGE-103 was verifying at `1305100174dbbc3702fc2af1d3b6a5502200eef3` with the prerequisite fixes incorporated; Greenlight still matches the tested pinned candidate. Do not duplicate their work or wait on stale relationships.
3. Follow [DEVELOPMENT.md](sh-707/DEVELOPMENT.md). Supervisor review, staging, installation and 20 actual installed controls are complete; do not repeat them. Preserve the disabled candidate, enabled original, both caches and all trust. Root will ask Mikey for native candidate trust and final selection. No generic source, permission or context block remains.
4. After that deliberate native action, record actual activation in a fresh native host thread. Keep source, installed behavior, release and active-host receipts distinct. Do not version the Agentics checkout, patch installed artifacts or bypass trust.
5. Record final evidence and commit; only after live acceptance, make `story move SH-707 verifying` the last action. Until that concrete prerequisite is available, keep SH-707 open and record the delivery boundary for the supervisor. A context handoff is not a blocker.

The managed helper reader refusal is owned by SH-712. The supported direct CLI
context command worked; retire that workaround after installing SH-712's repair
and verifying the exact managed reader. No installed guard was changed.

## Preventative action — killing the class

Preserve `PersonalHookContract.test_codex_readonly_approval_is_neutral` and its Claude control as the installed personal-hook boundary regression.
AGE-103's `plugins/greenlight/tests/greenlight-codex.bats` covers deterministic approvals, both AI-rationale settings and providers, warnings, denials, unrelated tools, malformed JSON, and the manifest environment.
The invariant is explicit: a Codex approval that does not rewrite input must emit neither `permissionDecision` nor an orphaned `permissionDecisionReason`.
The AI tests must prove a valid boolean response reaches production serialization; neutral fallback alone cannot count as successful AI coverage.
The source guards pass; their active delivery remains pending. SH-707 now also
checks real installer output against full source inventories, so a stale cache,
partial installation or same-version replacement cannot masquerade as acceptance.

## Lessons

Hook JSON is a provider-specific interface, even when event names match.
Test both direct scripts and manifest invocation; each can fail independently.
Passing deterministic replay does not establish coverage of an unreachable AI branch or prove which hook caused an earlier incident.
