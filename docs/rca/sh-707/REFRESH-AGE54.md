# SH-707: AGE-54 refresh, ready for native review

The replacement is **staged, not installed**. Use this procedure instead of the
older `greenlight@personal` activation instructions. Source preparation, real
isolated installation, compatibility checks, and preservation checks are complete.
Native trust and selection remain Mikey's action; host validation is still pending.

## Exact candidate

| Item | Value |
|---|---|
| Plugin identity | `greenlight-sh707-age54@personal` |
| Version | `3.9.1+codex.20260913065353` |
| Display name | Greenlight (SH-707 explorer repair) |
| Source commit | `dfd8aebda5c3fa33dec87bc93c4925fff7264809` |
| Greenlight subtree | `c49f3ec2dcbadb8c66780047922aed20d1d5c4a8` |
| Staged source | `/Users/mikey/plugins/greenlight-sh707-age54` |
| Review directory | `/Users/mikey/Enderchest/storyhook/sh707-development-20260913-age54-isolated` |
| Archive | `greenlight-sh707-development.tar.gz`, read-only |
| SHA256 | `c58e1bcafcb38756e1473fd972be896a2f67971fb1e5c3e98a621350792e29be` |
| Lasting source owners | AGE-103 (Codex output), AGE-54 (explorer policy) |
| Live installed / native validated / release certified | No / No / No |

[Provenance](refresh-age54-provenance.json) records every source and payload hash,
mode, helper command, and metadata delta. The sole source addition is the Codex
manifest; all original source bytes and modes, including the Claude manifest,
are preserved. Official plugin-creator scaffold, cachebuster, and validation
helpers passed. The generic generator receipt names AGE-103; this refresh also
includes the two AGE-54 source commits described below.

AGE-54 reproduced unsafe uncertainty approval and writable-redirection bypasses
in the exact Greenlight subtree used by the original SH-707 artifact. Commits
`2945d86c69325d7c301375d448b2273dd8b87154` and
`dfd8aebda5c3fa33dec87bc93c4925fff7264809` repair these at their source. Its
126 focused checks passed. Central verification merged PR196 as
`8d76175c77b7e6bec67ab03784e8f319cceba360` at 2026-09-13 07:00:46 UTC.
AGE-103 is also landed. AGE-52's separate configuration-layering work is not
included. Existing user configuration is preserved, including explicit AI opt-in.

## Why a separate name

Isolated probes of both `codex plugin add` and native app-server `plugin/install`
showed that installation enables the selected plugin, even if it was disabled.
Reinstalling a newer version under the same identity also pruned that identity's
previous cache. Neither interface offered a preserve-disabled install option in
installed Codex 0.154.0. These probes changed only disposable fixtures.

The new alias preserves old cache paths for running sessions. Installation is
part of the operator-present native review because it enables the new producer.
The original remains available until the replacement is trusted and selected.
Do not reinstall either older candidate or alter any installed cache directly.

## Completed evidence

| Check | Result |
|---|---|
| Real isolated installer identity and exact cached bytes | Pass |
| Actual installed-manifest compatibility controls | 20/20 pass |
| Packaged AGE-54 policy/redirection Bats tests | 16/16 pass |
| Fixture config-version conflict refusal | Pass, no mutation |
| Fixture native selection and rollback | Pass; new alias disabled again |
| All preexisting fixture cache files | 43 preserved |
| Live preexisting cache files | 435 preserved |
| Live original source, candidate archive, config, native trust | Preserved; config/trust byte-identical |
| Existing personal marketplace entries | Preserved; one new alias appended |

The [rehearsal receipt](refresh-age54-rehearsal.json) contains synthetic fixture
configuration and trust only. It is not proof of native authorization. The
[staging receipt](refresh-age54-staging.json) contains safe live preservation
results; raw private configuration backups remain outside this repository.
`greenlight@agentics` remains enabled, the original `greenlight@personal` remains
disabled, and the new alias has no installed cache or live enabled setting.

The actual-tree selector returned ALL because certified baseline
`7d8c74f5d3b2de80fe6e44c1db2e89ff4dd98247` has no coverage map. The central
verifier retains the full suite. Direct packaging, preservation, and personal-hook
checks pass **29/29** with warnings treated as errors. The first local invocation
omitted the personal-hook environment input and used macOS's symlinked `/tmp`
fixture path; correcting the invocation and resolving the newly created fixture
root fixed these setup failures without changing production behavior or relaxing
symlink refusal. Python compilation and whitespace checks pass.

```sh
PYTHONDONTWRITEBYTECODE=1 \
PERSONAL_HOOK=/Users/mikey/.codex/hooks/git-readonly-allow.py \
/opt/homebrew/bin/python3 -W error -m unittest discover \
  -s docs/rca/sh-707 -p 'test_*.py'
```

`rehearse_alias_recorded.py` preserves the exact completed fixture experiment and
its original paths. For a new run, use a new fixture output directory and an
Agentics checkout retaining the pinned objects; never rerun against the existing
fixture. The script creates its own HOME and CODEX_HOME and starts no model thread.
The checked-in generator accepts a full immutable `--source-commit` and a separate
`--plugin-name`. `stage_alias.py` refuses existing names, existing sources,
symlinked paths, and mismatched archive identities before registration.

## Mikey: install and review

1. In a terminal, run:

   ```sh
   codex plugin add greenlight-sh707-age54@personal --json
   ```

   Confirm identity `greenlight-sh707-age54@personal` and version
   `3.9.1+codex.20260913065353`. If they differ, stop and report the output.

2. Open a fresh Codex terminal session. Before asking it to run tools, open
   `/hooks`, choose **PreToolUse**, and review only this handler:

   ```text
   greenlight-sh707-age54@personal:hooks/hooks.json:pre_tool_use:0:0
   ```

   Its command is `bash "${PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/greenlight.sh"`.
   Its resolved plugin root must be
   `/Users/mikey/.codex/plugins/cache/personal/greenlight-sh707-age54/3.9.1+codex.20260913065353`.
   Use the handler's native **t** trust control. Do not use trust-all or type a
   trust hash into configuration. If the handler is absent or the identity differs,
   report that observation instead of selecting another entry.

3. Reply **Trusted** to the supervisor. Until selection is complete, do not send
   model tasks in that fresh session. The supervisor will verify the actual trust
   and installed bytes, then use the supported native config API with a fresh
   expectedVersion to select only the new alias. The original and older personal
   candidate will be disabled, with all caches retained. A fresh native thread
   must then demonstrate actual hook execution without unsupported-output errors.

This request exists because native hook trust requires the user's review. A
packaging or subprocess test cannot supply that authorization. Source staging and
reviewable evidence are complete before requesting it.

## Rollback, retirement, and completion

After verifying the retained original identity and trust, use supported native
plugin selection to enable `greenlight@agentics`, then disable the new alias.
Keep `greenlight@personal` disabled. Verify exactly one selected producer and
preserved caches before starting a fresh thread. Do not restore whole config
backups or uninstall a cache referenced by a running session. Rollback restores
the original known Codex output defect; it is recovery, not acceptance.

Retire the workaround only after a release containing both AGE-103 and AGE-54 is
installed through normal Agentics registration and its identity, manifest, and
native-host behavior are verified. Source merge alone is not retirement. Remove
the separate development identities only when no sessions reference their caches.

After native acceptance, record distinct live installation and host receipts,
commit them, and make `story move SH-707 verifying` the last worktree action.
Until then this is a concrete native-review hold, not a source or tooling block.
