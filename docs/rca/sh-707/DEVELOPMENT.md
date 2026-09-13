# SH-707: reviewed development delivery candidate

**Prepared and rehearsed; not installed in the live host.** The operator's
2026-09-13 02:07 UTC remediation authorizes this separate disposable identity.
It expressly reserves live registration changes for supervisor review of the
concrete artifact and commands below. This supersedes the earlier runbook's
missing-development-identity prerequisite. Native trust remains an operator step.
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
| Claims deliberately unset | Release certification, live installation, native activation |

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
proposed below. The [live preservation snapshot](development-live-preservation.json)
confirms the original active baseline and personal-hook hashes still match the
earlier receipt; the personal source and marketplace do not exist in the live home.

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

## Supervisor-reviewed live sequence

These commands are prepared, **not executed against the live home**. Run from the
SH-707 worktree only after the supervisor reviews this exact artifact and sequence.
Coordinate a maintenance interval with no agent tool execution during plugin
replacement. Retain the original trusted producer until the new producer has
passed installed checks and native review. Do not disable hooks, edit trust
hashes, remove the Agentics marketplace, or change unrelated plugins.
Only the supervisor-coordinated native plugin selection changes which reviewed
producer new sessions use. Retain both installed caches and every hook trust entry.

1. Re-read story/operator state and compare the live cache, registration, enabled
   plugins, personal hooks, and trust with the preservation receipts. Capture a
   fresh private snapshot before mutation. Stop on unexplained drift. Confirm the
   original cache still matches the recorded 3.9.1 inventory. Rollback will select
   those retained bytes, without reinstalling from a mutable marketplace source.

2. Stage the exact sealed source using the official scaffold. This refuses an
   existing personal marketplace/source, symlinked ancestors, altered archive,
   and inconsistent provenance before the helper writes anything:

   ```sh
   PYTHONDONTWRITEBYTECODE=1 python3 -W error docs/rca/sh-707/stage_personal.py \
     --artifact-directory /Users/mikey/Enderchest/storyhook/sh707-development-20260913 \
     --home /Users/mikey \
     --expected-sha256 cd5c382dd492cbfa265a84d518d4e1e72392e78dc8accdf5d391fd7c22d9e7f3
   python3 /Users/mikey/.codex/skills/.system/plugin-creator/scripts/read_marketplace_name.py
   ```

   The reader must return `personal`. The helper-created default marketplace is
   implicitly discovered; do not run marketplace add. Source goes to
   `~/plugins/greenlight`; the existing Agentics registration remains untouched.

3. Install through the supported CLI and retain its JSON result:

   ```sh
   codex plugin add greenlight@personal --json
   codex plugin list --marketplace personal --json
   ```

   Require pluginId `greenlight@personal`, the exact development version above,
   enabled true, and the installer-returned path. Compare that path's **entire**
   inventory to `metadata_delta.files` in the approved provenance, then run the
   real installed manifest controls. For the expected standard cache path:

   ```sh
   PYTHONDONTWRITEBYTECODE=1 python3 -W error - <<'PY'
   import json, sys
   from pathlib import Path
   sys.path.insert(0, 'docs/rca/sh-707')
   from packaging_evidence import inventory
   from installed_contract import exercise
   receipt = json.loads(Path('docs/rca/sh-707/development-provenance.json').read_text())
   installed = Path('/Users/mikey/.codex/plugins/cache/personal/greenlight/3.9.1+codex.20260913021137')
   assert inventory(installed) == receipt['metadata_delta']['files'], 'installed artifact differs'
   print(json.dumps({'contracts': exercise(installed, repaired=True),
                     'native_host_validated': False}, indent=2))
   PY
   ```

4. Complete the native trust boundary, without running model tools in the
   duplicate-enabled state. Start a fresh native Codex thread, choose **Review
   hooks**, select the **PreToolUse** handler belonging to `greenlight@personal`,
   and inspect its cache path and command. Expected command:
   `bash "${PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/greenlight.sh"`.
   In the terminal hooks browser, **t on the selected handler** trusts that
   handler; do not use the event-level trust-all action. Verify the selected
   hook is enabled and trusted. Record the native UI's actual identity/hash;
   never write a guessed trust hash. The app may present the equivalent native
   review UI rather than the terminal key binding.

5. Once the replacement's installed checks and native trust are accepted, open
   **`/plugins`** in the new native session. Select the installed Greenlight row
   from **agentics** (not personal), and press **Space** to turn that plugin off.
   The selected-row hint explicitly says `Space to disable`. Keep the personal
   development plugin on. This native plugin-level selection is the replacement
   boundary; do not use hook-level enablement switches or remove/uninstall.
   The supervisor must coordinate this with existing session owners; this lane
   does not change their threads or promise they ignore config notifications.
   Check the supported read-only listings:

   ```sh
   codex plugin list --marketplace personal --json
   codex plugin list --marketplace agentics --json
   ```

   Confirm exactly one enabled Greenlight producer and unchanged unrelated
   plugin inventories, both Greenlight caches, Agentics registration, and
   existing trust entries. Original `greenlight@agentics` is installed but
   disabled for new selection; its cache path must still exist byte-for-byte.
   Start a fresh native thread to pick up the final configuration. Exercise a
   benign request and capture hook-specific execution evidence without the
   unsupported-output diagnostic. Subprocess controls above prove denials with
   inert payloads; never execute a destructive command to test a denial.

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
Until then retain this lane in progress for the supervisor's concrete review
and native activation coordination; do not certify source readiness as completion.

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
