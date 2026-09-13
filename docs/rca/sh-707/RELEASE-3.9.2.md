# SH-707: released Agentics 3.9.2 installed; native review remains

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
| Native discovery | Correct 3.9.2 handler is enabled; `trustStatus: modified` |
| Native active-host acceptance | Pending; subprocess tests do not certify host activation |

The [safe receipt](release-3.9.2-receipt.json) records the complete installed
inventory, native handler metadata, test commands and control outputs. The
[test log](release-3.9.2-bats.log) records all shipped cases, including AGE-54
uncertainty/redirection protections and configuration-layering tests. Source
provenance comes from the immutable release's GitHub Git-tree API. The containing
release was deployed by Mikey; this lane did not run a release gate or installer.

## The one native action

Codex still stores the prior handler's trust hash. Its read-only `hooks/list`
response identifies the released handler as modified. This is Codex's native
review boundary for changed hook code, not a choice between technical approaches.

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

After native review, reread SH-707 and repeat obviation review. Confirm the exact
current handler is trusted and collect fresh-host invocation evidence; do not
infer either from this receipt. Commit final evidence and, only after acceptance,
make `story move SH-707 verifying` the last action. Keep SH-707 open until then.
The central verifier owns the full suite and submission. The actual-tree test
selector returned ALL because its baseline has no coverage map; this lane ran
only Greenlight's directly impacted installed tests and manifest controls.
