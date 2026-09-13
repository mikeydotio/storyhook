# SH-707: Agentics 3.9.2 installed and native smoke accepted

Mikey deployed Agentics 3.9.2. The installed official Greenlight matches the
tagged release exactly and passes its complete shipped test suite. This replaces
both temporary personal-plugin activation procedures. **Do not install or activate
either personal candidate.** No further installation or plugin selection is needed.

| Evidence | Result |
|---|---|
| Release tag / commit | `v3.9.2` / `da6db5643ef1a36f985078217e36174b17b2478c` |
| Greenlight Git tree | `750c2dba8be5d8e14a871493dadc5197ce51234d` |
| Installed identity | `greenlight@agentics`, version `3.9.2`, enabled |
| Installed provenance | All 21 files match release Git blobs and executable modes; no missing/extra files or links |
| Installed Greenlight suite | 164/164 pass, no skips, disposable HOME |
| Actual installed-manifest compatibility controls | 20/20 pass; both providers, AI responses, neutral Codex approvals, denials and warnings |
| Native discovery | Correct 3.9.2 handler is enabled; `trustStatus: trusted`; stored and current hashes match |
| Native active-host acceptance | Mikey completed review; fresh `sttest` session ran `pwd` successfully without the hook diagnostic |

The [safe receipt](release-3.9.2-receipt.json) records the complete installed
inventory, native handler metadata, test commands and control outputs. The
[test log](release-3.9.2-bats.log) records all shipped cases, including AGE-54
uncertainty/redirection protections and configuration-layering tests. Source
provenance comes from the immutable release's GitHub Git-tree API. The containing
release was deployed by Mikey; this lane did not run a release gate or installer.

## Completed native acceptance

Mikey completed the native review and requested `pwd` in terminal window `sttest`.
The [acceptance receipt](native-acceptance-receipt.json) records independent native
trusted discovery, an unchanged 21-file installed inventory, and the real command
event from fresh thread `01a09d03-16d9-7d90-aab2-0685099d8fba` in Codex 0.154.0.
At 2026-09-13 23:06:14 UTC, `pwd` returned the expected directory with exit 0 and
empty stderr; the pane displayed no hook failure. No further user action remains.

The retained rollout has no separate successful per-handler event, and existing
Greenlight logging is disabled. Native acceptance combines trusted discovery and
the actual-host smoke result with the independently passing installed-manifest
contracts. It does not claim captured per-handler stdout or prove which emitter
caused the original incident. No logging, trust or configuration was changed by
this lane to obtain evidence.

## Historical native review instructions — completed

Before Mikey's review, Codex stored the prior handler's trust hash and reported
the released handler as modified. The following procedure is retained as history;
do not repeat it for the already trusted identity.

1. Start a fresh `codex` session. Open `/hooks`, then **PreToolUse**.
2. Select only `greenlight@agentics:hooks/hooks.json:pre_tool_use:0:0` with source
   `/Users/mikey/.codex/plugins/cache/agentics/greenlight/3.9.2/hooks/hooks.json`.
   Review its command, `bash "${PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/greenlight.sh"`,
   and use the individual handler's native **t** trust control. Do not use
   trust-all or enter hashes into configuration. Stop if the identity differs.
3. Ask that fresh session to run `pwd`. Record the session and native result so
   the supervisor can confirm the trusted handler ran without the unsupported
   approval diagnostic. Then report completion to the supervisor.

Observed current hash:
`sha256:e7cd1e579c65088e3ff4b0fa8c16c5a5b01c8381c2ee486e560c4bb85632044e`.
Previously trusted hash:
`sha256:cd0e38d792650e8c92bde2297bf4e1a86e5c48fa970e3e7c5f63ba586818e570`.
These are evidence for comparison, not configuration-edit instructions.

## Preservation and continuation

`greenlight@personal` remains disabled with its original cache retained.
`greenlight-sh707-age54@personal` remains staged and uninstalled. Their artifacts
and receipts are historical evidence. No cache cleanup is part of this handoff.
The old Agentics 3.9.1 cache was already absent when this lane first inspected the
user-reported update; this lane did not delete or restore it. Preserve all
remaining caches and use a fresh session for acceptance rather than relying on
older sessions' plugin snapshots. No live configuration or trust was changed by
this continuation, and no raw private configuration backup was copied.

The standard release removes the need to use either temporary artifact. Retain
their bytes until the supervisor establishes that no sessions reference them;
do not use the known-defective older personal candidate as rollback. Any failure
in the release requires diagnosis at the owning source and a supported repair.

Native review and fresh-host acceptance are complete. The obviation review was
repeated; all 14 candidate stories remain unchanged from the preceding review
and none obviates this work. Commit final evidence and make
`story move SH-707 verifying` the last action.
The central verifier owns the full suite and submission. The actual-tree test
selector returned ALL because its baseline has no coverage map; this lane ran
only Greenlight's directly impacted installed tests and manifest controls.
