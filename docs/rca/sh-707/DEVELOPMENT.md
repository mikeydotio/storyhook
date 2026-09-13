# SH-707: reviewed development delivery candidate

**Installed and disabled; native activation pending.** The supervisor completed
source staging at 2026-09-13 02:33 UTC and installation at 02:36 UTC after reviewing
the exact artifact and commands. `greenlight@personal` is disabled;
`greenlight@agentics` remains enabled. Both caches and all existing trust remain
preserved. Do not repeat staging or installation. The root supervisor will ask
Mikey for native candidate trust and final selection; this lane changes neither.
The supervisor's 02:17 UTC review additionally requires retaining original cache
bytes for running sessions. Native plugin selection below supersedes the earlier
proposed live remove/reinstall transition; those prior receipts remain historical.

## Artifact and provenance

| Item | Value |
|---|---|
| Review directory | `/Users/mikey/Enderchest/storyhook/sh707-development-20260913` |
| Sealed archive | `greenlight-sh707-development.tar.gz` (read-only) |
| SHA256 | `cd5c382dd492cbfa265a84d518d4e1e72392e78dc8accdf5d391fd7c22d9e7f3` |
| Codex identity | `greenlight@personal`, `3.9.1+codex.20260913021137` |
| Pinned Greenlight source | Agentics `af73549175847d158c5a59de772f6cfee7b256f5` |
| Sole source-tree difference | Added `.codex-plugin/plugin.json`; no original file or executable mode changes |
| Original manifest | `.claude-plugin/plugin.json` retained byte-for-byte as source provenance |
| Lasting source owner | AGE-103 |
| Live installation | Complete; candidate disabled, original enabled |
| Claims deliberately unset | Release certification and native activation |

The official plugin-creator scaffold, marketplace-name reader, default UTC
cachebuster helper, and validator all passed. The validator ran with PyYAML 6.0.3
in an isolated temporary environment; no installed helper was modified. The
[Codex manifest](development-codex-manifest.json) explicitly labels this as an
unreleased development artifact. Codex 0.154.0 selected that manifest over the
unchanged Claude manifest during real installation.

The [provenance](development-provenance.json) records every source and payload
file's SHA256 and executable mode, the source Git tree, helper results, and the
metadata delta. The archive is hash-addressed; do not rebuild it and silently
reuse this checksum or development version. A new build needs a new receipt.

At the latest dependency read, AGE-103 was verifying at
`1305100174dbbc3702fc2af1d3b6a5502200eef3`, with AGE-84 prerequisite repairs
incorporated. Its Greenlight subtree matched the pinned candidate exactly.
There is no remaining source-unavailable dependency. SH-711 owns continuation
automation; SH-712 owns the managed context-reader workaround.

## Evidence and limits

The [cache-preserving receipt](development-native-toggle.json) proves five
transitions using the installed Codex app-server's supported config API. It
contains complete request/response evidence, including config-version conflict
rejection. The [earlier remove/reinstall receipt](development-rehearsal.json) and
[CLI transcript](development-commands.json) retain the first packaging experiment.
Both use a disposable Agentics-shaped marketplace and the exact staging helper
subsequently used by the supervisor. The
[pre-staging snapshot](development-live-preservation.json) is historical evidence;
its absent-source/marketplace observation is superseded by the
[supervisor live receipt](installed-disabled-receipt.json). The live receipt
records the installed candidate, disabled state, original enabled state, preserved
existing caches/config/trust, and 20 passing actual installed-manifest controls.
Raw private configuration backups remain outside this repository.

| Transition | Enabled Greenlight plugin keys |
|---|---|
| Baseline | `greenlight@agentics` |
| Add personal | Both keys |
| Native plugin selection: original off after replacement is accepted | `greenlight@personal` |
| Rollback: original plugin on | Both keys |
| Rollback: personal plugin off after original is accepted | `greenlight@agentics` |

All five fixture states preserve the Agentics registration, eight unrelated
enabled plugins, their complete installed byte/mode inventories, and every seeded
trust entry. Unrelated plugins are preservation witnesses, not behavior mocks.
Trust entries are synthetic test data, not evidence of native authorization.
Twenty cases execute the installed production manifest with inert tool payloads
and only HTTP stubbed. Safe/neutral output, AI boolean false, warnings, context,
destructive denials, malformed input, Claude compatibility, and Codex PLUGIN_ROOT
resolution pass. The pinned source previously passed all 127 Greenlight tests.

The CLI exposes add/remove but no atomic replacement or persisted enable/disable
command. The native `/plugins` UI does provide a Space-to-enable/disable toggle.
Its `config/value/write` API merges only the selected plugin's enabled setting;
the rehearsal calls that same API and additionally supplies `expectedVersion`.
Both caches retain exact inventories through selection and rollback. The API's
`configVersionConflict` negative test leaves the config byte-identical. No loaded
thread is started or changed in the rehearsal; preserving executable paths does
not assert anything about an existing session's configuration-refresh behavior.

**The intermediate duplicate-enabled state is real.** No new trust for
`greenlight@personal` was synthesized by installation. An installer receipt
therefore cannot certify an active safety hook.

## Completed supervisor delivery

The SH-707 comments **SUPERVISOR LIVE REVIEW** (02:34 UTC) and
**SUPERVISOR LIVE INSTALL COMPLETE** (02:38 UTC) are the authoritative handoff.
Their actions are complete; the earlier staging/install commands are retained
in Git history for provenance, not as instructions to run again.

| Completed action | Recorded result |
|---|---|
| Official scaffold and source staging, 02:33 UTC | `/Users/mikey/plugins/greenlight` and `/Users/mikey/.agents/plugins/marketplace.json` created; archive hash unchanged |
| Normal `codex plugin add`, 02:36 UTC | Installed `greenlight@personal`, version `3.9.1+codex.20260913021137` |
| Supported `config/value/write` with fresh expectedVersion | Changed only `plugins.greenlight@personal.enabled` to false |
| Original producer | `greenlight@agentics` remains enabled; original 3.9.1 cache retained |
| Preservation | All nine preexisting Agentics plugin caches and unrelated configuration/trust unchanged |
| Actual installed-manifest validation | All 20 controls passed against the new live cache; inert payloads, only HTTP stubbed |

Installed candidate path:
`/Users/mikey/.codex/plugins/cache/personal/greenlight/3.9.1+codex.20260913021137`.

The safe [installed-disabled receipt](installed-disabled-receipt.json) is copied
unchanged from
`/private/tmp/sh707-supervisor-live-20260913/installed-disabled-receipt.json`.
The supervisor's installer/toggle results and raw private config backups stay in
that private directory. No raw backup is copied into these docs.

## Remaining native action — owned by Mikey and the root supervisor

Keep the candidate disabled and the original enabled until the deliberate native
activation sequence. The root supervisor will ask Mikey for this action; this
lane must not activate either producer, rerun staging/install, or fabricate trust.

1. During the root-coordinated native review, inspect the personal Greenlight
   **PreToolUse** handler against the installed path and exact development
   identity above. Its command is
   `bash "${PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/greenlight.sh"`.
   Use native candidate-specific trust review, not a trust-all action or config
   write. Record the identity/hash reported by the native UI. The supervisor
   owns the native sequence for exposing a disabled plugin's handler; the
   rehearsal does not prove it appears in the startup review while disabled.

2. Once Mikey accepts candidate trust, use supported native plugin enablement to
   enable `greenlight@personal` and then deselect `greenlight@agentics`, leaving
   exactly one enabled producer. The terminal `/plugins` UI offers **Space** on
   the selected installed plugin row. Retain the original producer until the
   candidate is trusted and deliberately selected. Coordinate any intermediate
   duplicate-enabled state without agent tool execution. Do not use hook-level
   enablement switches, uninstall either plugin, or modify existing sessions.

3. In a fresh native thread, confirm the intended candidate handler actually
   runs without the unsupported-output diagnostic. Record native activation
   separately from the already-passing installed controls. Preserve both
   complete caches, all unrelated plugin settings, Agentics registration and
   existing trust records. Safe read-only listings for the supervisor are:

   ```sh
   codex plugin list --marketplace personal --json
   codex plugin list --marketplace agentics --json
   ```

Native-host validation and release certification remain false. The installed
receipt establishes packaging and subprocess behavior, not host activation.

## Rollback and retirement

During a coordinated maintenance interval, verify the retained original cache
against `active-installation.json` and its native trust. In `/plugins`, select
**Greenlight from agentics** and press **Space** to enable it. Once the original
selection/trust is verified, select **Greenlight (SH-707 development) from
personal** and press **Space** to disable it. Do not uninstall either plugin.
Check both listings, exactly one enabled producer, unchanged cache inventories,
registration and trust. Start a fresh native thread. If identity differs, keep
the known replacement selected and report the mismatch; do not patch caches or
restore a stale whole config. Rollback restores the known bare-allow defect; it
is recovery, not SH-707 acceptance. Retain staged source/archive for diagnostics.

Retire this interim delivery after an AGE-103-containing release is installed
through normal Agentics registration and passes exact identity, manifest, and
native-host checks, and only after sessions referencing the old caches have
ended. The supervisor can then coordinate supported plugin removal/retirement;
never delete a cache still referenced by a session. No release/version step
belongs in this worktree.

After actual live acceptance, record separate installed/native evidence, commit
it, and make `story move SH-707 verifying` the absolute last worktree action.
Until then retain this lane in progress for Mikey's native trust and final
selection, coordinated by the root supervisor. Supervisor delivery review and
installation are complete; no generic source, permission, or context block remains.

## Primary support and reproducibility

- [Official scaffold/update/install workflow](https://github.com/openai/codex/blob/main/codex-rs/skills/src/assets/samples/plugin-creator/references/installing-and-updating.md)
- [Native startup hook review](https://github.com/openai/codex/blob/main/codex-rs/tui/src/startup_hooks_review.rs)
- [Individual hook trust controls](https://github.com/openai/codex/blob/main/codex-rs/tui/src/bottom_pane/hooks_browser_view.rs)
- [Native plugin Space toggle](https://github.com/openai/codex/blob/main/codex-rs/tui/src/chatwidget/plugin_catalog.rs)
- [Native toggle RPC implementation](https://github.com/openai/codex/blob/main/codex-rs/tui/src/app/background_requests.rs)

The final fixture command was:

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -W error docs/rca/sh-707/rehearse_development.py \
  --agentics-repo /Volumes/Code/mikeyward/agentics/.codex/worktrees/AGE-103 \
  --artifact-directory /Users/mikey/Enderchest/storyhook/sh707-development-20260913 \
  --output-directory /tmp/sh707-native-toggle-final --preserve-caches
```

Use a new output path for another run. The artifact directory can instead be the
Enderchest review directory. Nineteen directly impacted packaging, metadata and
staging tests pass. The sidecar-tampering regression failed before the staging
preflight comparison and passed after it. No full repository suite was run.
